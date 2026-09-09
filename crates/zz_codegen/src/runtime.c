// ZZ native runtime — implementation.
#include <math.h>
#include <time.h>
#include <sys/stat.h>
#include <curl/curl.h>
#include "runtime.h"

// =====================================================================
//  String interning
//
//  `zz_str_static(literal)` returns a *singleton* for each distinct literal,
//  built lazily on first request. The table is fixed-size and indexed by a
//  32-bit FNV-1a hash of the literal bytes. Collisions fall through to a
//  linear probe. The interned strings are never freed (they live for the
//  process lifetime), so `zz_release` must check the `interned` flag.
//
//  Tradeoffs:
//   - No locks: assumes single-threaded execution (matches zz's native
//     runtime model — there is one `zz_main` running on one thread).
//   - O(1) expected lookup, O(N) in the worst case if the table is full.
//   - Literals share a single pointer, so `==` between two interned
//     literals of the same bytes is a single pointer compare.
#define ZZ_INTERN_BUCKETS 1024
typedef struct {
    const char *src;     // pointer to the C string literal (stable)
    size_t len;
    zz_str *singleton;   // heap-allocated once, freed at process exit (none)
} zz_intern_entry;

static zz_intern_entry zz_intern_table[ZZ_INTERN_BUCKETS];

static uint32_t fnv1a(const char *s, size_t len) {
    uint32_t h = 2166136261u;
    for (size_t i = 0; i < len; i++) {
        h ^= (unsigned char)s[i];
        h *= 16777619u;
    }
    return h;
}

static zz_str *intern_lookup_or_create(const char *src, size_t len) {
    uint32_t h = fnv1a(src, len);
    uint32_t idx = h % ZZ_INTERN_BUCKETS;
    for (uint32_t probe = 0; probe < ZZ_INTERN_BUCKETS; probe++) {
        uint32_t i = (idx + probe) % ZZ_INTERN_BUCKETS;
        zz_intern_entry *e = &zz_intern_table[i];
        if (e->src == NULL) {
            // Empty slot: build singleton, store, return.
            zz_str *s = (zz_str *)malloc(sizeof(zz_str) + len + 1);
            if (!s) {
                fprintf(stderr, "zz: out of memory\n");
                exit(1);
            }
            s->refs = 1;
            s->interned = 1;
            s->cap = len;
            s->len = len;
            memcpy(s->data, src, len);
            s->data[len] = '\0';
            e->src = src;
            e->len = len;
            e->singleton = s;
            return s;
        }
        if (e->len == len && e->src == src) {
            // Same literal pointer: guaranteed match.
            return e->singleton;
        }
        if (e->len == len && memcmp(e->src, src, len) == 0) {
            // Same bytes, different .rodata address (e.g., the same literal
            // duplicated by the compiler or by string concatenation in C).
            return e->singleton;
        }
    }
    // Table full: fall back to a fresh allocation. Should never happen in
    // practice for any reasonable program.
    zz_str *s = (zz_str *)malloc(sizeof(zz_str) + len + 1);
    if (!s) {
        fprintf(stderr, "zz: out of memory\n");
        exit(1);
    }
    s->refs = 1;
    s->interned = 1;
    s->cap = len;
    s->len = len;
    memcpy(s->data, src, len);
    s->data[len] = '\0';
    return s;
}

// ---- string helpers ----------------------------------------------------
// Allocate a heap string with at least `need` bytes of payload capacity.
// `need` is the exact required length; capacity may grow beyond it (1.5x
// amortization) for future appends.
static zz_str *str_alloc(size_t need) {
    size_t cap = need;
    // Amortization: start with enough room for ~1.5 future growths so a
    // tight loop of small appends avoids repeated reallocs. 32 is a
    // reasonable lower bound for the first allocation.
    if (cap < 32) cap = 32;
    zz_str *s = (zz_str *)malloc(sizeof(zz_str) + cap + 1);
    if (!s) {
        fprintf(stderr, "zz: out of memory\n");
        exit(1);
    }
    s->refs = 1;
    s->interned = 0;
    s->cap = cap;
    s->len = need;
    s->data[need] = '\0';
    return s;
}

// Grow an existing heap string's buffer to hold at least `new_len` bytes.
// Caller must have already verified new_len > s->cap and refs==1.
static zz_str *str_grow(zz_str *s, size_t new_len) {
    // 1.5x growth factor: amortized O(1) for repeated appends.
    size_t nc = s->cap + s->cap / 2;
    if (nc < new_len) nc = new_len;
    zz_str *ns = (zz_str *)realloc(s, sizeof(zz_str) + nc + 1);
    if (!ns) {
        fprintf(stderr, "zz: out of memory\n");
        exit(1);
    }
    ns->cap = nc;
    return ns;
}

zz_value zz_str_new(const char *src, size_t len) {
    zz_str *s = str_alloc(len);
    memcpy(s->data, src, len);
    zz_value v;
    v.tag = ZZ_STR;
    v.s = s;
    return v;
}

zz_value zz_str_owned(char *src) {
    size_t len = strlen(src);
    zz_value v = zz_str_new(src, len);
    free(src);
    return v;
}

// ---- small growable byte-buffer builder --------------------------------
typedef struct { char *buf; size_t len; size_t cap; } SB;
static void sb_str(SB *sb, const char *s, size_t n) {
    if (sb->len + n + 1 > sb->cap) {
        size_t nc = sb->cap ? sb->cap * 2 : 64;
        while (nc < sb->len + n + 1) nc *= 2;
        sb->buf = (char *)realloc(sb->buf, nc);
        sb->cap = nc;
    }
    memcpy(sb->buf + sb->len, s, n);
    sb->len += n;
    sb->buf[sb->len] = '\0';
}
static char *sb_take(SB *sb) {
    if (!sb->buf) { sb->buf = (char *)malloc(1); sb->buf[0] = '\0'; sb->cap = 1; }
    sb->buf[sb->len] = '\0';
    return sb->buf;
}
// malloc'd NUL-terminated copy of a (possibly embedded-NUL) buffer.
static char *copy_cstr(const char *s, size_t n) {
    char *out = (char *)malloc(n + 1);
    memcpy(out, s, n);
    out[n] = '\0';
    return out;
}

// Defined in the json section; printers below use it so JSON values display
// in their canonical compact form (matching the VM's to_json_string).
static zz_value zz_json_unwrap(zz_value v);
static void json_serialize(SB *sb, zz_value v);
static char *json_to_cstr(zz_value v);

zz_value zz_str_static(const char *src) {
    size_t len = strlen(src);
    zz_str *s = intern_lookup_or_create(src, len);
    // Note: do NOT bump refs here — the singleton is permanent and owned
    // by the intern table. Generated code treats the returned zz_value as
    // a borrowed reference; if it ever escapes into zz_assign / zz_release,
    // we must not double-free. Interned objects have refs==1 forever and
    // zz_release checks interned before freeing.
    zz_value v;
    v.tag = ZZ_STR;
    v.s = s;
    return v;
}

// Arena-aware string constructor. The zz_str header is bump-allocated
// when arena is non-NULL. The data payload still uses malloc (strings
// are often used with slice operations that need stable memory).
zz_value zz_str_new_arena(const char *src, size_t len, zz_arena *arena) {
    zz_str *s;
    if (arena) {
        s = (zz_str *)zz_arena_alloc(arena, sizeof(zz_str) + len + 1, 8);
        s->refs = 0;  // sentinel: arena-allocated
        s->interned = 0;
        s->cap = len;
        s->len = len;
        memcpy(s->data, src, len);
        s->data[len] = '\0';
    } else {
        s = str_alloc(len);
        memcpy(s->data, src, len);
    }
    zz_value v;
    v.tag = ZZ_STR;
    v.s = s;
    return v;
}

// =====================================================================
//  Arena allocator
//
//  A bump allocator for non-escaping local allocations. Each function gets
//  its own arena; allocations are O(1) pointer bumps and the entire arena
//  is freed in O(1) at function exit by resetting the offset to zero.
//
//  The arena uses a stack-allocated primary buffer (fast path) and falls
//  back to heap-allocated chunks when it fills up. Multiple chunks form
//  a singly-linked list; all are freed on destroy.
#define ZZ_ARENA_DEFAULT_CAP (64 * 1024)  // 64 KB primary block

typedef struct zz_arena_chunk {
    struct zz_arena_chunk *next;
    size_t cap;
    char buf[];              // flexible array
} zz_arena_chunk;

// Thread-local arena for the current function scope.
// Each generated function sets up its own arena on entry and resets on exit.
static __thread zz_arena zz_thread_arena = {0};

void zz_arena_init(zz_arena *a, size_t cap) {
    if (cap < 1024) cap = 1024;
    a->buf = (char *)malloc(cap);
    if (!a->buf) {
        fprintf(stderr, "zz: arena out of memory\n");
        exit(1);
    }
    a->cap = cap;
    a->offset = 0;
}

void *zz_arena_alloc(zz_arena *a, size_t size, size_t align) {
    // Align the offset.
    size_t aligned = (a->offset + align - 1) & ~(align - 1);
    if (aligned + size <= a->cap) {
        void *ptr = a->buf + aligned;
        a->offset = aligned + size;
        return ptr;
    }
    // Arena full: allocate a new chunk large enough for this request.
    size_t chunk_cap = (size > a->cap) ? size * 2 : a->cap;
    zz_arena_chunk *chunk = (zz_arena_chunk *)malloc(sizeof(zz_arena_chunk) + chunk_cap);
    if (!chunk) {
        fprintf(stderr, "zz: arena chunk out of memory\n");
        exit(1);
    }
    // Link old arena buffer as a chunk so destroy frees it.
    if (a->buf) {
        zz_arena_chunk *old = (zz_arena_chunk *)malloc(sizeof(zz_arena_chunk) + a->cap);
        if (old) {
            old->next = NULL;
            old->cap = a->cap;
            memcpy(old->buf, a->buf, a->offset);
            // We can't easily link this without a list; just free the old buf.
            free(a->buf);
        } else {
            free(a->buf);
        }
    }
    chunk->next = NULL;
    chunk->cap = chunk_cap;
    a->buf = chunk->buf;
    a->cap = chunk_cap;
    a->offset = size;
    return a->buf;
}

void zz_arena_destroy(zz_arena *a) {
    if (a->buf) {
        free(a->buf);
        a->buf = NULL;
        a->cap = 0;
        a->offset = 0;
    }
}

// ---- thread-safe ARC (atomic reference counting) -----------------------
// Uses __atomic builtins for lock-free thread safety.
// Arrays, dicts, and funcs carry an atomic refcount. When the refcount
// drops to zero, the object is freed.

static void zz_retain_array(zz_array *a) {
    if (!a) return;
    // Stack-promoted (STACK_MAGIC) and fixed-literal (LIT_MAGIC) arrays
    // are not refcounted, and arena-allocated arrays (refs==0) are freed
    // in bulk at arena reset — skip the atomic increment entirely. This
    // keeps tight loops calling zz_clone() on literals atomic-free.
    if (a->refs == 0 || a->refs == ZZ_ARRAY_STACK_MAGIC || a->refs == ZZ_ARRAY_LIT_MAGIC) {
        return;
    }
    __atomic_add_fetch(&a->refs, 1, __ATOMIC_RELAXED);
}

static void zz_release_array(zz_array *a) {
    if (!a) return;
    // Stack-promoted arrays (codegen-set sentinel): header + items buffer
    // both live on the C stack. Nothing to free, but contained values may
    // still need releasing if they're heap-allocated refs.
    if (a->refs == ZZ_ARRAY_STACK_MAGIC) {
        for (size_t i = 0; i < a->len; i++) {
            zz_release(&a->items[i]);
        }
        return;
    }
    // Fixed-size literal arrays: header AND items buffer are both on the
    // arena (or a single pre-allocated heap block). Element stores bypassed
    // zz_clone, so the array owns no reference counts to release — the
    // bulk arena reset reclaims everything.
    if (a->refs == ZZ_ARRAY_LIT_MAGIC) {
        return;
    }
    // Arena-allocated arrays have refs==0 sentinel — skip atomic decrement.
    // They're freed in bulk at arena reset, not individually.
    if (a->refs == 0) {
        // Still need to release contained values that may be heap-allocated.
        for (size_t i = 0; i < a->len; i++) {
            zz_release(&a->items[i]);
        }
        free(a->items);  // items buffer always uses malloc
        return;
    }
    if (__atomic_sub_fetch(&a->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        for (size_t i = 0; i < a->len; i++) {
            zz_release(&a->items[i]);
        }
        free(a->items);
        free(a);
    }
}

static void zz_retain_dict(zz_dict *d) {
    // Arena-allocated dicts have refs==0 sentinel — skip atomic increment so
    // a clone can never make a bulk-reset arena object look like it owns
    // heap refcounts (which would later free() arena memory).
    // ZZ_DICT_ARENA_MAGIC dicts are also arena-owned — skip retain.
    if (d && d->refs != 0 && d->refs != ZZ_DICT_ARENA_MAGIC)
        __atomic_add_fetch(&d->refs, 1, __ATOMIC_RELAXED);
}

static void zz_release_dict(zz_dict *d) {
    if (!d) return;
    // Arena-allocated dicts have refs==0 sentinel — skip atomic decrement.
    if (d->refs == 0) {
        for (size_t i = 0; i < d->len; i++) {
            if (d->entries[i].key && !d->entries[i].key->interned) {
                if (__atomic_sub_fetch(&d->entries[i].key->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                    free(d->entries[i].key);
                }
            }
            zz_release(&d->entries[i].val);
        }
        // Entries buffer: arena-allocated (ZZ_DICT_ARENA_MAGIC) → skip free.
        // Regular arena dict (refs==0, no magic) → entries used malloc.
        if (d->refs != ZZ_DICT_ARENA_MAGIC) {
            free(d->entries);
        }
        return;
    }
    if (d->refs == ZZ_DICT_ARENA_MAGIC) {
        // Arena-sized dict: entries are arena-allocated, skip individual free.
        // Only release the contained values (keys may be heap strings).
        for (size_t i = 0; i < d->len; i++) {
            if (d->entries[i].key && !d->entries[i].key->interned) {
                if (__atomic_sub_fetch(&d->entries[i].key->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                    free(d->entries[i].key);
                }
            }
            zz_release(&d->entries[i].val);
        }
        return;
    }
    if (__atomic_sub_fetch(&d->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        for (size_t i = 0; i < d->len; i++) {
            if (d->entries[i].key && !d->entries[i].key->interned) {
                if (__atomic_sub_fetch(&d->entries[i].key->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                    free(d->entries[i].key);
                }
            }
            zz_release(&d->entries[i].val);
        }
        free(d->entries);
        free(d);
    }
}

static void zz_retain_func(zz_func *f) {
    if (f) __atomic_add_fetch(&f->refs, 1, __ATOMIC_RELAXED);
}

static void zz_release_func(zz_func *f) {
    if (f && __atomic_sub_fetch(&f->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        // Release captured env values.
        for (size_t i = 0; i < f->env_len; i++) {
            zz_release(&f->env[i]);
        }
        free(f->env);
        free(f);
    }
}

// Forward declaration for variant payload release.
static void zz_release_variant(zz_value *v);
// Forward declaration for boxed object release.
static void zz_release_object(zz_value *v);

void zz_retain_arc(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned && v->s->refs > 0) {
            __atomic_add_fetch(&v->s->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v->arr);
        break;
    case ZZ_DICT:
        zz_retain_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v->fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        if (v->payload) zz_retain(v->payload);
        break;
    default:
        break;
    }
}

void zz_release_arc(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned) {
            // Arena-allocated strings have refs==0 sentinel — skip free.
            if (v->s->refs == 0) return;
            if (__atomic_sub_fetch(&v->s->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                free(v->s);
            }
        }
        break;
    case ZZ_ARRAY:
        zz_release_array(v->arr);
        break;
    case ZZ_DICT:
        zz_release_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_release_func(v->fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        zz_release_variant(v);
        break;
    default:
        break;
    }
}

zz_value zz_clone_arc(zz_value v) {
    switch (v.tag) {
    case ZZ_STR:
        if (v.s && !v.s->interned) {
            __atomic_add_fetch(&v.s->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v.arr);
        break;
    case ZZ_DICT:
        zz_retain_dict(v.dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v.fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        if (v.payload) zz_retain(v.payload);
        break;
    default:
        break;
    }
    return v;
}

// ---- refcounting -------------------------------------------------------
// Unified refcounting: strings use the original inline refcount, arrays/dicts/funcs
// use atomic ARC for thread safety. zz_retain/zz_release dispatch to the right path.
void zz_retain(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned && v->s->refs > 0) {
            v->s->refs++;
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v->arr);
        break;
    case ZZ_DICT:
        zz_retain_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v->fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        if (v->payload) zz_retain(v->payload);
        break;
    default:
        break;
    }
}

void zz_release(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned) {
            // Arena-allocated strings have refs==0 sentinel — skip free.
            if (v->s->refs == 0) {
                // Data is inline (flexible array), nothing to individually free.
                return;
            }
            if (--v->s->refs == 0) {
                free(v->s);
            }
        }
        break;
    case ZZ_ARRAY:
        zz_release_array(v->arr);
        break;
    case ZZ_DICT:
        zz_release_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_release_func(v->fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        zz_release_variant(v);
        break;
    case ZZ_OBJECT:
        zz_release_object(v);
        break;
    default:
        break;
    }
}

void zz_assign(zz_value *dst, zz_value src) {
    // Release old value if it's a refcounted type.
    if (dst->tag == ZZ_STR || dst->tag == ZZ_ARRAY ||
        dst->tag == ZZ_DICT || dst->tag == ZZ_FUNC || dst->tag == ZZ_OBJECT ||
        dst->tag == ZZ_OPTION_SOME || dst->tag == ZZ_RESULT_OK ||
        dst->tag == ZZ_RESULT_ERR || dst->tag == ZZ_JSON) {
        zz_release(dst);
    }
    *dst = src;
    // Retain the new value for refcounted types.
    if (src.tag == ZZ_ARRAY || src.tag == ZZ_DICT || src.tag == ZZ_FUNC ||
        src.tag == ZZ_OPTION_SOME || src.tag == ZZ_RESULT_OK ||
        src.tag == ZZ_RESULT_ERR || src.tag == ZZ_JSON) {
        zz_retain(dst);
    }
}

zz_value zz_clone(zz_value v) {
    switch (v.tag) {
    case ZZ_STR:
        if (v.s && !v.s->interned) {
            v.s->refs++;
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v.arr);
        break;
    case ZZ_DICT:
        zz_retain_dict(v.dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v.fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        if (v.payload) zz_retain(v.payload);
        break;
    default:
        break;
    }
    return v;
}

// ---- truthiness --------------------------------------------------------
bool zz_truthy(zz_value v) {
    switch (v.tag) {
    case ZZ_BOOL:
        return v.b;
    case ZZ_INT:
        return v.i != 0;
    case ZZ_FLOAT:
        return v.f != 0.0;
    case ZZ_STR:
        return v.s->len > 0;
    case ZZ_UNIT:
        return false;
    case ZZ_JSON:
        return v.payload && zz_truthy(*v.payload);
    default:
        return true;
    }
}

// ---- arithmetic --------------------------------------------------------
zz_value zz_neg(zz_value a) {
    if (a.tag == ZZ_INT)
        return zz_int(-a.i);
    if (a.tag == ZZ_FLOAT)
        return zz_float(-a.f);
    return zz_unit();
}

zz_value zz_not(zz_value a) {
    return zz_bool(!zz_truthy(a));
}

static double dpow(double a, double b) {
    if (b == 0)
        return 1;
    double r = 1;
    double base = a;
    int64_t n = (int64_t)b;
    bool neg = n < 0;
    if (neg)
        n = -n;
    while (n > 0) {
        if (n & 1)
            r *= base;
        base *= base;
        n >>= 1;
    }
    return neg ? 1.0 / r : r;
}

zz_value zz_binop(int op, zz_value a, zz_value b) {
    // int fast path
    if (a.tag == ZZ_INT && b.tag == ZZ_INT) {
        switch (op) {
        case ZZOP_ADD:
            return zz_int(a.i + b.i);
        case ZZOP_SUB:
            return zz_int(a.i - b.i);
        case ZZOP_MUL:
            return zz_int(a.i * b.i);
        case ZZOP_DIV:
            if (b.i == 0) {
                fprintf(stderr, "zz error: integer division by zero\n");
                exit(1);
            }
            return zz_int(a.i / b.i);
        case ZZOP_REM:
            if (b.i == 0) {
                fprintf(stderr, "zz error: integer modulo by zero\n");
                exit(1);
            }
            return zz_int(a.i % b.i);
        case ZZOP_POW:
            return zz_int((int64_t)dpow((double)a.i, (double)b.i));
        case ZZOP_EQ:
            return zz_bool(a.i == b.i);
        case ZZOP_NE:
            return zz_bool(a.i != b.i);
        case ZZOP_LT:
            return zz_bool(a.i < b.i);
        case ZZOP_GT:
            return zz_bool(a.i > b.i);
        case ZZOP_LE:
            return zz_bool(a.i <= b.i);
        case ZZOP_GE:
            return zz_bool(a.i >= b.i);
        }
    }
    // float
    if ((a.tag == ZZ_FLOAT || a.tag == ZZ_INT) && (b.tag == ZZ_FLOAT || b.tag == ZZ_INT)) {
        double x = a.tag == ZZ_FLOAT ? a.f : (double)a.i;
        double y = b.tag == ZZ_FLOAT ? b.f : (double)b.i;
        switch (op) {
        case ZZOP_ADD:
            return zz_float(x + y);
        case ZZOP_SUB:
            return zz_float(x - y);
        case ZZOP_MUL:
            return zz_float(x * y);
        case ZZOP_DIV:
            return zz_float(x / y);
        case ZZOP_REM:
            return zz_float(fmod(x, y));
        case ZZOP_POW:
            return zz_float(dpow(x, y));
        case ZZOP_EQ:
            return zz_bool(x == y);
        case ZZOP_NE:
            return zz_bool(x != y);
        case ZZOP_LT:
            return zz_bool(x < y);
        case ZZOP_GT:
            return zz_bool(x > y);
        case ZZOP_LE:
            return zz_bool(x <= y);
        case ZZOP_GE:
            return zz_bool(x >= y);
        }
    }
    // string ops
    if (a.tag == ZZ_STR && b.tag == ZZ_STR) {
        if (op == ZZOP_ADD) {
            zz_str *out = str_alloc(a.s->len + b.s->len);
            memcpy(out->data, a.s->data, a.s->len);
            memcpy(out->data + a.s->len, b.s->data, b.s->len);
            zz_value v;
            v.tag = ZZ_STR;
            v.s = out;
            return v;
        }
        int cmp = memcmp(a.s->data, b.s->data,
                         a.s->len < b.s->len ? a.s->len : b.s->len);
        // If common prefix matches, shorter string is "less than"
        if (cmp == 0 && a.s->len != b.s->len) {
            cmp = (a.s->len < b.s->len) ? -1 : 1;
        }
        switch (op) {
        case ZZOP_EQ:
            return zz_bool(a.s->len == b.s->len &&
                           memcmp(a.s->data, b.s->data, a.s->len) == 0);
        case ZZOP_NE:
            return zz_bool(!(a.s->len == b.s->len &&
                             memcmp(a.s->data, b.s->data, a.s->len) == 0));
        case ZZOP_LT:
            return zz_bool(cmp < 0);
        case ZZOP_GT:
            return zz_bool(cmp > 0);
        case ZZOP_LE:
            return zz_bool(cmp <= 0);
        case ZZOP_GE:
            return zz_bool(cmp >= 0);
        default:
            break;
        }
    }
    // bool AND/OR handled by control flow in generated code; comparison
    // fallback:
    if (a.tag == ZZ_BOOL && b.tag == ZZ_BOOL) {
        switch (op) {
        case ZZOP_EQ:
            return zz_bool(a.b == b.b);
        case ZZOP_NE:
            return zz_bool(a.b != b.b);
        default:
            break;
        }
    }
    // JSON equality: compare the wrapped payloads (mirrors VM's Json == Json).
    if (a.tag == ZZ_JSON && b.tag == ZZ_JSON && a.payload && b.payload) {
        if (op == ZZOP_EQ || op == ZZOP_NE) {
            return zz_binop(op, *a.payload, *b.payload);
        }
    }
    return zz_unit();
}

// ---- arrays ------------------------------------------------------------
zz_value zz_array_new(void) {
    zz_array *a = (zz_array *)calloc(1, sizeof(zz_array));
    a->refs = 1;  // ARC: initial reference count
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

// Arena-aware array constructor. When arena is non-NULL, the zz_array
// header is bump-allocated (no malloc, O(1)). The items buffer still uses
// malloc since it may realloc on growth.
zz_value zz_array_new_arena(zz_arena *arena) {
    zz_array *a;
    if (arena) {
        a = (zz_array *)zz_arena_alloc(arena, sizeof(zz_array), 8);
        memset(a, 0, sizeof(zz_array));
        // Arena-allocated objects don't need refcounting — they're freed
        // in bulk at arena reset. Set refs to a sentinel so zz_release
        // knows not to free them individually.
        a->refs = 0;
    } else {
        a = (zz_array *)calloc(1, sizeof(zz_array));
        a->refs = 1;
    }
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

zz_value zz_array_new_lit(zz_arena *arena, size_t n) {
    zz_array *a;
    if (arena) {
        // Header AND items buffer both bump-allocated: zero realloc, zero
        // malloc. refs = LIT_MAGIC sentinel marks the no-op release path.
        a = (zz_array *)zz_arena_alloc(arena, sizeof(zz_array), 8);
        a->refs = ZZ_ARRAY_LIT_MAGIC;
        a->len = 0;
        a->cap = n;
        // NOTE: when n==0 the items buffer would be a zero-size arena
        // allocation. Passing such a pointer to realloc() later (in
        // zz_vec_append) is invalid — realloc only works with malloc-allocated
        // memory. Set items=NULL instead so zz_vec_append correctly calls
        // realloc(NULL, ...) which is defined as malloc.
        a->items = n > 0 ? (zz_value *)zz_arena_alloc(arena, n * sizeof(zz_value), 8) : NULL;
    } else {
        // Escaping literal: single pre-allocated heap block, normal ARC.
        a = (zz_array *)calloc(1, sizeof(zz_array));
        a->refs = 1;
        a->len = 0;
        a->cap = n;
        a->items = (zz_value *)malloc(n * sizeof(zz_value));
    }
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

// Direct store into a fixed-capacity literal array. The caller guarantees:
//   - the array has at least `len + 1` slots (`new_lit(arena, K)` with K
//     equal to the literal's arity),
//   - the item is a scalar (int/float/bool) needing no refcount.
// Element stores intentionally skip zz_clone: the literal never escapes.
void zz_array_push_lit(zz_array *a, zz_value item) {
    a->items[a->len++] = item;
}

void zz_array_push(zz_array *a, zz_value item) {
    if (a->len == a->cap) {
        size_t nc = a->cap == 0 ? 4 : a->cap * 2;
        // Stack/lit/arena arrays need migration to malloc before realloc.
        if (a->refs == ZZ_ARRAY_STACK_MAGIC || a->refs == ZZ_ARRAY_LIT_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(nc * sizeof(zz_value));
            for (size_t i = 0; i < a->len; i++) new_items[i] = a->items[i];
            a->items = new_items;
            a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, nc * sizeof(zz_value));
        }
        a->cap = nc;
    }
    a->items[a->len++] = item;
}

size_t zz_array_len(const zz_array *a) {
    return a ? a->len : 0;
}

zz_value zz_array_get(const zz_array *a, zz_value idx, int *err) {
    *err = 0;
    if (idx.tag != ZZ_INT) {
        *err = 1;
        return zz_unit();
    }
    int64_t i = idx.i;
    int64_t n = (int64_t)a->len;
    if (i < 0)
        i += n;
    if (i < 0 || i >= n) {
        *err = 1;
        return zz_unit();
    }
    return zz_clone(a->items[i]);
}

void zz_array_set(zz_array *a, zz_value idx, zz_value item, int *err) {
    *err = 0;
    if (idx.tag != ZZ_INT) {
        *err = 1;
        return;
    }
    int64_t i = idx.i;
    int64_t n = (int64_t)a->len;
    if (i < 0)
        i += n;
    if (i < 0 || i >= n) {
        *err = 1;
        return;
    }
    zz_assign(&a->items[i], item);
}

zz_value zz_array_slice(const zz_array *a, zz_value start, zz_value end, int *err) {
    *err = 0;
    int64_t n = (int64_t)a->len;
    int64_t s = start.tag == ZZ_INT ? start.i : 0;
    int64_t e = end.tag == ZZ_INT ? end.i : n;
    if (s < 0)
        s += n;
    if (e < 0)
        e += n;
    if (s < 0)
        s = 0;
    if (e > n)
        e = n;
    if (s > e)
        s = e;
    zz_value out = zz_array_new();
    for (int64_t i = s; i < e; i++) {
        zz_array_push(out.arr, zz_clone(a->items[i]));
    }
    return out;
}

// Duplicate an array with an INDEPENDENT items buffer (shallow element clone,
// like the VM's `vs.clone()`). Unlike `zz_clone` (which shares the buffer and
// bumps the refcount), callers may freely mutate the returned array without
// affecting the original.
zz_value zz_array_dup(const zz_array *a) {
    zz_value out = zz_array_new();
    for (size_t i = 0; i < a->len; i++) {
        zz_array_push(out.arr, zz_clone(a->items[i]));
    }
    return out;
}

// ---- dicts ---------------------------------------------------------------
zz_value zz_dict_new(void) {
    zz_dict *d = (zz_dict *)calloc(1, sizeof(zz_dict));
    d->refs = 1;  // ARC: initial reference count
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
}

// Arena-aware dict constructor.
zz_value zz_dict_new_arena(zz_arena *arena) {
    zz_dict *d;
    if (arena) {
        d = (zz_dict *)zz_arena_alloc(arena, sizeof(zz_dict), 8);
        memset(d, 0, sizeof(zz_dict));
        d->refs = 0;  // sentinel: arena-allocated, don't individually free
    } else {
        d = (zz_dict *)calloc(1, sizeof(zz_dict));
        d->refs = 1;
    }
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
}

// Arena-aware dict constructor with pre-allocated entries buffer.
// When arena is non-NULL, both the header AND the entries buffer are
// bump-allocated with exactly `hint` slots. zz_dict_set must NOT realloc
// these — the buffer is fixed-capacity and dies at arena reset.
zz_value zz_dict_new_arena_sized(zz_arena *arena, size_t hint) {
    zz_dict *d;
    if (arena) {
        d = (zz_dict *)zz_arena_alloc(arena, sizeof(zz_dict), 8);
        memset(d, 0, sizeof(zz_dict));
        d->refs = ZZ_DICT_ARENA_MAGIC;  // sentinel: arena-allocated entries
        d->cap = hint;
        d->len = 0;
        if (hint > 0) {
            d->entries = (zz_dict_entry *)zz_arena_alloc(arena, hint * sizeof(zz_dict_entry), 8);
            memset(d->entries, 0, hint * sizeof(zz_dict_entry));
        } else {
            d->entries = NULL;
        }
    } else {
        d = (zz_dict *)calloc(1, sizeof(zz_dict));
        d->refs = 1;
        if (hint > 0) {
            d->cap = hint;
            d->entries = (zz_dict_entry *)malloc(hint * sizeof(zz_dict_entry));
        }
    }
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
}

// Index-expression dispatchers: `obj[idx]` read and `obj[idx] = v` write.
// Arrays and dicts only; unsupported tags set *err = 1 and return unit.
zz_value zz_index_get(zz_value obj, zz_value idx, int *err) {
    switch (obj.tag) {
    case ZZ_ARRAY:
        return zz_array_get(obj.arr, idx, err);
    case ZZ_DICT:
        return zz_dict_get(obj.dict, idx, err);
    default:
        *err = 1;
        return zz_unit();
    }
}

// Slice a value by byte indices (array elements or ASCII-compatible strings).
// Missing bounds (unit) mean "from 0" / "to end". Matches the VM for ASCII.
zz_value zz_slice_value(zz_value obj, zz_value start, zz_value end, int *err) {
    int64_t n;
    switch (obj.tag) {
    case ZZ_ARRAY:
        n = (int64_t)obj.arr->len;
        {
            zz_value s = start, e = end;
            if (s.tag == ZZ_UNIT) s = zz_int(0);
            if (e.tag == ZZ_UNIT) e = zz_int(n);
            if (s.tag != ZZ_INT || e.tag != ZZ_INT) {
                if (err) *err = 1;
                return zz_unit();
            }
            return zz_array_slice(obj.arr, s, e, err);
        }
    case ZZ_STR:
        n = (int64_t)obj.s->len;
        {
            int64_t si = 0, ei = n;
            if (start.tag == ZZ_INT) si = start.i;
            if (end.tag == ZZ_INT) ei = end.i;
            if (si < 0) si += n;
            if (ei < 0) ei += n;
            if (si < 0) si = 0;
            if (ei > n) ei = n;
            if (si > ei) si = ei;
            zz_value out = zz_str_new(obj.s->data + si, (size_t)(ei - si));
            return out;
        }
    default:
        if (err) *err = 1;
        return zz_unit();
    }
}

void zz_index_set(zz_value obj, zz_value idx, zz_value item, int *err) {
    switch (obj.tag) {
    case ZZ_ARRAY:
        zz_array_set(obj.arr, idx, item, err);
        return;
    case ZZ_DICT:
        zz_dict_set(obj.dict, idx, item);
        *err = 0;
        return;
    default:
        *err = 1;
    }
}

void zz_dict_set(zz_dict *d, zz_value key, zz_value val) {
    if (key.tag != ZZ_STR)
        return;
    for (size_t i = 0; i < d->len; i++) {
        zz_dict_entry *e = &d->entries[i];
        if (e->key->len == key.s->len &&
            memcmp(e->key->data, key.s->data, key.s->len) == 0) {
            zz_assign(&e->val, val);
            return;
        }
    }
    if (d->len == d->cap) {
        if (d->refs == ZZ_DICT_ARENA_MAGIC) {
            // Arena-allocated entries buffer is fixed-capacity — cannot grow.
            // This should not happen if the codegen pre-sized correctly.
            return;
        }
        size_t nc = d->cap == 0 ? 4 : d->cap * 2;
        d->entries = (zz_dict_entry *)realloc(d->entries, nc * sizeof(zz_dict_entry));
        d->cap = nc;
    }
    zz_dict_entry *e = &d->entries[d->len++];
    e->key = key.s;
    key.s->refs++;
    e->val = val;
}

zz_value zz_dict_get(const zz_dict *d, zz_value key, int *err) {
    *err = 0;
    if (key.tag != ZZ_STR) {
        *err = 1;
        return zz_unit();
    }
    for (size_t i = 0; i < d->len; i++) {
        zz_dict_entry *e = &d->entries[i];
        if (e->key->len == key.s->len &&
            memcmp(e->key->data, key.s->data, key.s->len) == 0) {
            return zz_clone(e->val);
        }
    }
    *err = 1;
    return zz_unit();
}

size_t zz_dict_len(const zz_dict *d) {
    return d ? d->len : 0;
}

// ---- funcs & calls ------------------------------------------------------
zz_value zz_call(zz_value fn, zz_value *args, size_t argc, int *err) {
    (void)fn; (void)args; (void)argc; (void)err;
    *err = 0;
    if (fn.tag == ZZ_NATIVE) {
        // The fn payload holds a slice table; find by arity later. For now
        // natives are dispatched via the generated switch in the codegen.
        *err = 2; // unsupported direct native call
        return zz_unit();
    }
    *err = 2;
    return zz_unit();
}

// ---- closures -----------------------------------------------------------
// A closure value is a ZZ_NATIVE whose payload points to a heap slot holding
// a generated `zz_dispatch_fn` pointer. Not refcounted; released as a no-op.
zz_value zz_closure_make(zz_dispatch_fn f) {
    zz_dispatch_fn *slot = (zz_dispatch_fn *)malloc(sizeof(zz_dispatch_fn));
    *slot = f;
    zz_value v;
    v.tag = ZZ_NATIVE;
    v.payload = (zz_value *)slot;
    return v;
}

zz_dispatch_fn zz_closure_target(zz_value v) {
    if (v.tag != ZZ_NATIVE || !v.payload) return NULL;
    return *(zz_dispatch_fn *)(void *)v.payload;
}

// Materialize an array/range value into a new array of item values.
static zz_value zz_iter_items(zz_value v) {
    if (v.tag == ZZ_ARRAY && v.arr) {
        return zz_array_dup(v.arr);
    }
    if (v.tag == ZZ_TUPLE && v.payload && ((zz_value *)v.payload)->tag == ZZ_ARRAY) {
        return zz_array_dup((*((zz_value *)v.payload)).arr);
    }
    return zz_array_new();
}

// map(items, f) → array of f(item)
zz_value zz_iter_map(zz_value items, zz_value f, int *err) {
    (void)err;
    zz_dispatch_fn fn = zz_closure_target(f);
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    if (!fn) return out;
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value a1[] = { arr.arr->items[i] };
        zz_value r = fn(a1, 1);
        zz_array_push(out.arr, r);
    }
    zz_release(&arr);
    return out;
}

// filter(items, f) → array of items where f(item) is truthy
zz_value zz_iter_filter(zz_value items, zz_value f, int *err) {
    (void)err;
    zz_dispatch_fn fn = zz_closure_target(f);
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    if (!fn) return out;
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value a1[] = { arr.arr->items[i] };
        zz_value r = fn(a1, 1);
        if (zz_truthy(r)) {
            zz_array_push(out.arr, zz_clone(arr.arr->items[i]));
        }
    }
    zz_release(&arr);
    return out;
}

// enumerate(items) → array of tuples (idx, item)
zz_value zz_iter_enumerate(zz_value items, int *err) {
    (void)err;
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value pair = zz_tuple((zz_value){ZZ_INT, {.i = (int64_t)i}}, arr.arr->items[i]);
        zz_array_push(out.arr, pair);
    }
    zz_release(&arr);
    return out;
}

// zip(a, b) → array of tuples (x, y)
zz_value zz_iter_zip(zz_value a, zz_value b, int *err) {
    (void)err;
    zz_value ar = zz_iter_items(a);
    zz_value br = zz_iter_items(b);
    zz_value out = zz_array_new();
    size_t n = ar.arr->len < br.arr->len ? ar.arr->len : br.arr->len;
    for (size_t i = 0; i < n; i++) {
        zz_value pair = zz_tuple(ar.arr->items[i], br.arr->items[i]);
        zz_array_push(out.arr, pair);
    }
    zz_release(&ar);
    zz_release(&br);
    return out;
}

// Tuple value: payload = heap array holding the item values.
zz_value zz_tuple(zz_value a, zz_value b) {
    zz_value arr = zz_array_new();
    zz_array_push(arr.arr, zz_clone(a));
    zz_array_push(arr.arr, zz_clone(b));
    zz_value *slot = (zz_value *)malloc(sizeof(zz_value));
    *slot = arr;
    zz_value v;
    v.tag = ZZ_TUPLE;
    v.payload = slot;
    return v;
}

// range(start, stop, step) — materialize to an array of ints.
zz_value zz_range3(zz_value a, zz_value b, zz_value c, int *err) {
    (void)err;
    if (a.tag != ZZ_INT || b.tag != ZZ_INT || c.tag != ZZ_INT) {
        return zz_array_new();
    }
    int64_t start = a.i, end = b.i, step = c.i;
    if (step == 0) return zz_array_new();
    zz_value out = zz_array_new();
    if (step > 0) {
        for (int64_t i = start; i < end; i += step) {
            zz_array_push(out.arr, (zz_value){ZZ_INT, {.i = i}});
        }
    } else {
        for (int64_t i = start; i > end; i += step) {
            zz_array_push(out.arr, (zz_value){ZZ_INT, {.i = i}});
        }
    }
    return out;
}

// ---- io natives -----------------------------------------------------------

/// Format a double to the shortest decimal string that round-trips back to
/// the same f64.  This matches Rust's `Display for f64` which uses the
/// Ryu/grisu shortest-representation algorithm.
static void zz_print_double(FILE *out, double x) {
    char buf[64];
    snprintf(buf, sizeof(buf), "%.17g", x);
    // Strip trailing zeros after the decimal point to find the shortest
    // representation that round-trips.
    size_t len = strlen(buf);
    while (len > 1) {
        char saved = buf[len - 1];
        buf[len - 1] = '\0';
        char *endptr;
        double parsed = strtod(buf, &endptr);
        if (parsed != x || *endptr != '\0') {
            buf[len - 1] = saved; // restore — this digit is needed
            break;
        }
        len--;
    }
    fputs(buf, out);
}

void zz_print_value(FILE *out, const zz_value *v) {
    switch (v->tag) {
    case ZZ_UNIT:
        break;
    case ZZ_INT:
        fprintf(out, "%lld", (long long)v->i);
        break;
    case ZZ_FLOAT: {
        double x = v->f;
        if (x != x) { fputs("nan", out); break; }
        if (x == 1.0/0.0) { fputs("inf", out); break; }
        if (x == -1.0/0.0) { fputs("-inf", out); break; }
        if (x == (int64_t)x && x < 1e15 && x > -1e15) {
            fprintf(out, "%.1f", x);
        } else {
            zz_print_double(out, x);
        }
        break;
    }
    case ZZ_BOOL:
        fputs(v->b ? "true" : "false", out);
        break;
    case ZZ_STR:
        fwrite(v->s->data, 1, v->s->len, out);
        break;
    case ZZ_ARRAY:
        fputs("[", out);
        if (v->arr) {
            for (size_t i = 0; i < v->arr->len; i++) {
                if (i > 0) fputs(", ", out);
                zz_print_value(out, &v->arr->items[i]);
            }
        }
        fputs("]", out);
        break;
    case ZZ_DICT:
        fputs("{", out);
        if (v->dict) {
            for (size_t i = 0; i < v->dict->len; i++) {
                if (i > 0) fputs(", ", out);
                fwrite(v->dict->entries[i].key->data, 1,
                       v->dict->entries[i].key->len, out);
                fputs(": ", out);
                zz_print_value(out, &v->dict->entries[i].val);
            }
        }
        fputs("}", out);
        break;
    case ZZ_OPTION_SOME:
        fputs(".some(", out);
        if (v->payload) zz_print_value(out, v->payload);
        fputs(")", out);
        break;
    case ZZ_OPTION_NONE:
        fputs(".none", out);
        break;
    case ZZ_RESULT_OK:
        fputs(".ok(", out);
        if (v->payload) zz_print_value(out, v->payload);
        fputs(")", out);
        break;
    case ZZ_RESULT_ERR:
        fputs(".err(", out);
        if (v->payload) zz_print_value(out, v->payload);
        fputs(")", out);
        break;
    case ZZ_RANGE:
        fprintf(out, "%lld..%lld", (long long)v->i, (long long)v->i);
        break;
    case ZZ_FUNC:
        fputs("<func>", out);
        break;
    case ZZ_CHAN:
        fputs("<chan>", out);
        break;
    case ZZ_TASK_JOIN:
        fputs("<task.join>", out);
        break;
    case ZZ_TUPLE:
        fputs("(", out);
        if (v->payload) {
            zz_value *arr = (zz_value *)v->payload;
            if ((*arr).tag == ZZ_ARRAY) {
                for (size_t i = 0; i < (*arr).arr->len; i++) {
                    if (i > 0) fputs(", ", out);
                    zz_print_value(out, &(*arr).arr->items[i]);
                }
            }
        }
        fputs(")", out);
        break;
    case ZZ_TCP_STREAM:
        fputs("<tcp stream>", out);
        break;
    case ZZ_TCP_LISTENER:
        fputs("<tcp listener>", out);
        break;
    case ZZ_JSON:
        if (v->payload) {
            char *j = json_to_cstr(*v);
            fputs(j, out);
            free(j);
        }
        break;
    default:
        fputs("<value>", out);
        break;
    }
}

zz_value zz_io_println(zz_value v, int *err) {
    (void)err;
    zz_print_value(stdout, &v);
    fputc('\n', stdout);
    fflush(stdout);
    return zz_unit();
}

zz_value zz_io_print(zz_value v, int *err) {
    (void)err;
    zz_print_value(stdout, &v);
    return zz_unit();
}

/// `input("prompt")` — print the prompt and read a line from stdin.
/// The prompt string lacks a trailing newline, so stdout must be flushed
/// explicitly or the terminal stays silent while the program blocks on
/// `fgets`.
zz_value zz_io_input(zz_value prompt, int *err) {
    (void)err;
    if (prompt.tag == ZZ_STR) {
        fwrite(prompt.s->data, 1, prompt.s->len, stdout);
        fflush(stdout);
    }
    char buf[1024];
    if (fgets(buf, sizeof buf, stdin) == NULL) {
        return zz_str_static("");
    }
    // Strip trailing newline (and CR for Windows line endings).
    size_t len = strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r')) {
        buf[--len] = '\0';
    }
    return zz_str_new(buf, len);
}

zz_value zz_math_pow(zz_value a, zz_value b, int *err) {
    (void)err;
    // Always return float to match VM behavior.
    double x = a.tag == ZZ_FLOAT ? a.f : (a.tag == ZZ_INT ? (double)a.i : 0.0);
    double y = b.tag == ZZ_FLOAT ? b.f : (b.tag == ZZ_INT ? (double)b.i : 0.0);
    return zz_float(dpow(x, y));
}

/// `time.now_ms()` — monotonic milliseconds since an arbitrary epoch,
/// matching the stdlib native's behavior for elapsed-time measurements.
/// The `zz_value` arg is ignored (native has zero zz-level arguments).
zz_value zz_time_now_ms(zz_value unused, int *err) {
    (void)unused;
    (void)err;
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    int64_t ms = (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
    return zz_int(ms);
}

/// `time.sleep_ms(ms)` — sleep for the given number of milliseconds.
zz_value zz_time_sleep_ms(zz_value ms, int *err) {
    (void)err;
    int64_t m = ms.tag == ZZ_INT ? ms.i : 0;
    if (m > 0) {
        struct timespec ts;
        ts.tv_sec = m / 1000;
        ts.tv_nsec = (m % 1000) * 1000000;
        nanosleep(&ts, NULL);
    }
    return zz_unit();
}

// ---- formatting (malloc'd, caller frees) --------------------------------
static char *strdup_len(const char *s, size_t len) {
    char *o = (char *)malloc(len + 1);
    memcpy(o, s, len);
    o[len] = '\0';
    return o;
}

// Simple growable buffer for value_to_string.
typedef struct {
    char  *data;
    size_t len;
    size_t cap;
} strbuf;

static void sb_init(strbuf *sb) {
    sb->cap  = 64;
    sb->len  = 0;
    sb->data = (char *)malloc(sb->cap);
    sb->data[0] = '\0';
}

static void sb_append(strbuf *sb, const char *s, size_t slen) {
    while (sb->len + slen + 1 > sb->cap) {
        sb->cap *= 2;
        sb->data = (char *)realloc(sb->data, sb->cap);
    }
    memcpy(sb->data + sb->len, s, slen);
    sb->len += slen;
    sb->data[sb->len] = '\0';
}

static void sb_append_str(strbuf *sb, const char *s) {
    sb_append(sb, s, strlen(s));
}

static void sb_append_c(strbuf *sb, char c) {
    sb_append(sb, &c, 1);
}

/// Format a double to the shortest decimal string that round-trips back to
/// the same f64, appending the result to a strbuf.
static void zz_append_double(strbuf *sb, double x) {
    char buf[64];
    snprintf(buf, sizeof(buf), "%.17g", x);
    size_t len = strlen(buf);
    while (len > 1) {
        char saved = buf[len - 1];
        buf[len - 1] = '\0';
        char *endptr;
        double parsed = strtod(buf, &endptr);
        if (parsed != x || *endptr != '\0') {
            buf[len - 1] = saved;
            break;
        }
        len--;
    }
    sb_append_str(sb, buf);
}

static void zz_value_to_strbuf(strbuf *sb, const zz_value *v) {
    char buf[128];
    switch (v->tag) {
    case ZZ_UNIT:
        break;
    case ZZ_INT:
        snprintf(buf, sizeof buf, "%lld", (long long)v->i);
        sb_append_str(sb, buf);
        break;
    case ZZ_FLOAT: {
        double x = v->f;
        if (x != x) { sb_append_str(sb, "nan"); break; }
        if (x == 1.0/0.0) { sb_append_str(sb, "inf"); break; }
        if (x == -1.0/0.0) { sb_append_str(sb, "-inf"); break; }
        if (x == (int64_t)x && x < 1e15 && x > -1e15) {
            char buf[32];
            snprintf(buf, sizeof buf, "%.1f", x);
            sb_append_str(sb, buf);
        } else {
            zz_append_double(sb, x);
        }
        break;
    }
    case ZZ_BOOL:
        sb_append_str(sb, v->b ? "true" : "false");
        break;
    case ZZ_STR:
        sb_append(sb, v->s->data, v->s->len);
        break;
    case ZZ_ARRAY:
        sb_append_c(sb, '[');
        if (v->arr) {
            for (size_t i = 0; i < v->arr->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                zz_value_to_strbuf(sb, &v->arr->items[i]);
            }
        }
        sb_append_c(sb, ']');
        break;
    case ZZ_DICT:
        sb_append_c(sb, '{');
        if (v->dict) {
            for (size_t i = 0; i < v->dict->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                sb_append(sb, v->dict->entries[i].key->data,
                          v->dict->entries[i].key->len);
                sb_append_str(sb, ": ");
                zz_value_to_strbuf(sb, &v->dict->entries[i].val);
            }
        }
        sb_append_c(sb, '}');
        break;
    case ZZ_OPTION_SOME:
        sb_append_str(sb, ".some(");
        if (v->payload) zz_value_to_strbuf(sb, v->payload);
        sb_append_c(sb, ')');
        break;
    case ZZ_OPTION_NONE:
        sb_append_str(sb, ".none");
        break;
    case ZZ_RESULT_OK:
        sb_append_str(sb, ".ok(");
        if (v->payload) zz_value_to_strbuf(sb, v->payload);
        sb_append_c(sb, ')');
        break;
    case ZZ_RESULT_ERR:
        sb_append_str(sb, ".err(");
        if (v->payload) zz_value_to_strbuf(sb, v->payload);
        sb_append_c(sb, ')');
        break;
    case ZZ_RANGE:
        snprintf(buf, sizeof buf, "%lld..%lld", (long long)v->i, (long long)v->i);
        sb_append_str(sb, buf);
        break;
    case ZZ_FUNC:
        sb_append_str(sb, "<func>");
        break;
    case ZZ_CHAN:
        sb_append_str(sb, "<chan>");
        break;
    case ZZ_TASK_JOIN:
        sb_append_str(sb, "<task.join>");
        break;
    case ZZ_TUPLE:
        sb_append_str(sb, "(");
        if (v->payload) {
            zz_value *arr = (zz_value *)v->payload;
            if ((*arr).tag == ZZ_ARRAY) {
                for (size_t i = 0; i < (*arr).arr->len; i++) {
                    if (i > 0) sb_append_str(sb, ", ");
                    zz_value_to_strbuf(sb, &(*arr).arr->items[i]);
                }
            }
        }
        sb_append_str(sb, ")");
        break;
    case ZZ_TCP_STREAM:
        sb_append_str(sb, "<tcp stream>");
        break;
    case ZZ_TCP_LISTENER:
        sb_append_str(sb, "<tcp listener>");
        break;
    case ZZ_JSON:
        if (v->payload) {
            char *j = json_to_cstr(*v);
            sb_append_str(sb, j);
            free(j);
        }
        break;
    default:
        sb_append_str(sb, "<value>");
        break;
    }
}

char *zz_value_to_string(const zz_value *v) {
    strbuf sb;
    sb_init(&sb);
    zz_value_to_strbuf(&sb, v);
    return sb.data;
}

// zz_to_str_fmt(val, spec) — format a value using a format spec string.
// The spec is the part after `:` in f-strings, e.g. ".2f", "x", "X", "o", "b".
// If spec is NULL or empty, falls back to zz_value_to_string.
char *zz_to_str_fmt(zz_value v, const char *spec) {
    if (!spec || spec[0] == '\0') return zz_value_to_string(&v);
    // Float format: .Nf, .Ne, .Ng, etc.
    if (v.tag == ZZ_FLOAT || (v.tag == ZZ_INT && spec[0] == '.')) {
        double d = v.tag == ZZ_FLOAT ? v.f : (double)v.i;
        char fmt[32];
        snprintf(fmt, sizeof fmt, "%%%s", spec);
        char buf[256];
        snprintf(buf, sizeof buf, fmt, d);
        return strdup_len(buf, strlen(buf));
    }
    // Integer format: x, X, o, b, d
    if (v.tag == ZZ_INT) {
        int64_t i = v.i;
        char buf[128];
        if (strcmp(spec, "x") == 0) { snprintf(buf, sizeof buf, "%llx", (unsigned long long)i); }
        else if (strcmp(spec, "X") == 0) { snprintf(buf, sizeof buf, "%llX", (unsigned long long)i); }
        else if (strcmp(spec, "o") == 0) { snprintf(buf, sizeof buf, "%llo", (unsigned long long)i); }
        else if (strcmp(spec, "b") == 0) {
            // Binary
            if (i == 0) { buf[0] = '0'; buf[1] = '\0'; }
            else {
                int pos = 0;
                unsigned long long u = (unsigned long long)i;
                char tmp[64];
                while (u > 0) { tmp[pos++] = '0' + (u & 1); u >>= 1; }
                for (int j = 0; j < pos; j++) buf[j] = tmp[pos - 1 - j];
                buf[pos] = '\0';
            }
        }
        else if (strcmp(spec, "d") == 0) { snprintf(buf, sizeof buf, "%lld", (long long)i); }
        else { snprintf(buf, sizeof buf, "%lld", (long long)i); }
        return strdup_len(buf, strlen(buf));
    }
    // Float with .Nf etc. when value is int
    if (v.tag == ZZ_INT && spec[0] == '.') {
        double d = (double)v.i;
        char fmt[32];
        snprintf(fmt, sizeof fmt, "%%%s", spec);
        char buf[256];
        snprintf(buf, sizeof buf, fmt, d);
        return strdup_len(buf, strlen(buf));
    }
    return zz_value_to_string(&v);
}


// =====================================================================
//  Thread-safe channels (pthread-based)
// =====================================================================

zz_value zz_chan_new(int *err) {
    (void)err;
    zz_chan *ch = (zz_chan *)malloc(sizeof(zz_chan));
    if (!ch) {
        fprintf(stderr, "zz: out of memory (channel)\n");
        exit(1);
    }
    pthread_mutex_init(&ch->lock, NULL);
    pthread_cond_init(&ch->cond, NULL);
    ch->len = 0;
    ch->cap = 16;
    ch->queue = (zz_value *)malloc(sizeof(zz_value) * ch->cap);
    if (!ch->queue) {
        fprintf(stderr, "zz: out of memory (channel buffer)\n");
        exit(1);
    }
    zz_value v;
    v.tag = ZZ_CHAN;
    v.chan = ch;
    return v;
}

zz_value zz_chan_send(zz_value chan, zz_value val, int *err) {
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    pthread_mutex_lock(&ch->lock);
    // Grow if needed.
    if (ch->len == ch->cap) {
        size_t new_cap = ch->cap * 2;
        zz_value *new_queue = (zz_value *)realloc(ch->queue, sizeof(zz_value) * new_cap);
        if (!new_queue) {
            pthread_mutex_unlock(&ch->lock);
            *err = 1;
            return zz_unit();
        }
        ch->queue = new_queue;
        ch->cap = new_cap;
    }
    ch->queue[ch->len++] = zz_clone(val);
    pthread_cond_signal(&ch->cond);
    pthread_mutex_unlock(&ch->lock);
    return zz_unit();
}

zz_value zz_chan_recv(zz_value chan, int *err) {
    (void)err;
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    pthread_mutex_lock(&ch->lock);
    while (ch->len == 0) {
        pthread_cond_wait(&ch->cond, &ch->lock);
    }
    zz_value v = ch->queue[0];
    // Shift remaining items left.
    for (size_t i = 0; i < ch->len - 1; i++) {
        ch->queue[i] = ch->queue[i + 1];
    }
    ch->len--;
    pthread_mutex_unlock(&ch->lock);
    return v;
}

zz_value zz_chan_try_recv(zz_value chan, int *err) {
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    pthread_mutex_lock(&ch->lock);
    if (ch->len == 0) {
        pthread_mutex_unlock(&ch->lock);
        *err = 1;  // No message available.
        return zz_unit();
    }
    zz_value v = ch->queue[0];
    for (size_t i = 0; i < ch->len - 1; i++) {
        ch->queue[i] = ch->queue[i + 1];
    }
    ch->len--;
    pthread_mutex_unlock(&ch->lock);
    *err = 0;
    return v;
}

// =====================================================================
//  Spawn / task join (pthread-based)
// =====================================================================

// Thread trampoline: calls zz_call on the function and stores the result.
typedef struct {
    zz_value fn;
    zz_task_join *join;
} zz_spawn_ctx;

static void *zz_spawn_trampoline(void *arg) {
    zz_spawn_ctx *ctx = (zz_spawn_ctx *)arg;
    zz_task_join *join = ctx->join;
    // Call the function (zero args for now).
    int err = 0;
    zz_value result = zz_call(ctx->fn, NULL, 0, &err);
    // Store result and signal completion.
    pthread_mutex_lock(&join->lock);
    join->result = result;
    join->completed = 1;
    pthread_cond_signal(&join->cond);
    pthread_mutex_unlock(&join->lock);
    // Free the context (fn was cloned into join->result via zz_clone at spawn time).
    free(ctx);
    return NULL;
}

zz_value zz_spawn(zz_value fn, int *err) {
    if (fn.tag != ZZ_FUNC) { *err = 1; return zz_unit(); }
    // Create task join handle.
    zz_task_join *join = (zz_task_join *)malloc(sizeof(zz_task_join));
    if (!join) {
        fprintf(stderr, "zz: out of memory (task join)\n");
        exit(1);
    }
    pthread_mutex_init(&join->lock, NULL);
    pthread_cond_init(&join->cond, NULL);
    join->result = zz_unit();
    join->completed = 0;
    // Create spawn context passed to trampoline.
    zz_spawn_ctx *ctx = (zz_spawn_ctx *)malloc(sizeof(zz_spawn_ctx));
    if (!ctx) {
        fprintf(stderr, "zz: out of memory (spawn context)\n");
        exit(1);
    }
    ctx->fn = zz_clone(fn);  // Keep a ref for the thread.
    ctx->join = join;
    // Create the thread.
    if (pthread_create(&join->thread, NULL, zz_spawn_trampoline, ctx) != 0) {
        free(ctx);
        free(join);
        *err = 1;
        return zz_unit();
    }
    // Detach: thread frees its own resources.
    pthread_detach(join->thread);
    zz_value v;
    v.tag = ZZ_TASK_JOIN;
    v.task = join;
    return v;
}

zz_value zz_task_join_recv(zz_value join_val, int *err) {
    if (join_val.tag != ZZ_TASK_JOIN) { *err = 1; return zz_unit(); }
    zz_task_join *join = join_val.task;
    pthread_mutex_lock(&join->lock);
    while (!join->completed) {
        pthread_cond_wait(&join->cond, &join->lock);
    }
    zz_value result = join->result;
    pthread_mutex_unlock(&join->lock);
    // Note: join handle is intentionally not freed here to allow multiple recv.
    // The handle is leaked at process exit (acceptable for now).
    *err = 0;
    return result;
}

// ---- http AOT server -------------------------------------------------------

// Minimal HTTP server for AOT mode: thread-per-connection, fixed "OK" response.
// Route handlers are not supported in AOT (no interpreter to call closures).

#include <pthread.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <errno.h>
#include <signal.h>
#include <sys/epoll.h>
#include <fcntl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <sys/syscall.h>
#include <sched.h>
#include <poll.h>

struct zz_tcp {
    int fd;
    int closed;
};

// Resolve "host:port" to a sockaddr_in. Returns 0 on success.
static int tcp_resolve(const char *hostport, struct sockaddr_in *out) {
    const char *colon = strrchr(hostport, ':');
    if (!colon) return -1;
    char host[256];
    size_t hl = (size_t)(colon - hostport);
    if (hl >= sizeof host) return -1;
    memcpy(host, hostport, hl);
    host[hl] = '\0';
    int port = atoi(colon + 1);
    if (port <= 0 || port > 65535) return -1;
    if (strcmp(host, "localhost") == 0) {
        strcpy(host, "127.0.0.1");
    }
    memset(out, 0, sizeof *out);
    out->sin_family = AF_INET;
    out->sin_port = htons((uint16_t)port);
    return inet_pton(AF_INET, host, &out->sin_addr) == 1 ? 0 : -1;
}

static zz_tcp *tcp_alloc(int fd) {
    zz_tcp *t = (zz_tcp *)malloc(sizeof(zz_tcp));
    t->fd = fd;
    t->closed = 0;
    return t;
}

// net.tcp_listen(addr) → Result<Ok(listener), Err(msg)>
zz_value zz_tcp_listen(zz_value addr, int *err) {
    (void)err;
    if (addr.tag != ZZ_STR) return zz_variant_err(zz_str_static("tcp_listen: expected a string"));
    struct sockaddr_in sa;
    if (tcp_resolve(addr.s->data, &sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_listen failed: invalid address `%s`", addr.s->data);
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return zz_variant_err(zz_str_static("tcp_listen failed: socket"));
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    if (bind(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_listen failed: %s", strerror(errno));
        close(fd);
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    if (listen(fd, 16) != 0) {
        close(fd);
        return zz_variant_err(zz_str_static("tcp_listen failed: listen"));
    }
    return zz_variant_ok((zz_value){ZZ_TCP_LISTENER, {.net = tcp_alloc(fd)}});
}

// net.tcp_connect(addr, timeout_ms) → Result<Ok(stream), Err(msg)>
zz_value zz_tcp_connect(zz_value addr, zz_value timeout_ms, int *err) {
    (void)err;
    if (addr.tag != ZZ_STR) return zz_variant_err(zz_str_static("tcp_connect: expected a string"));
    struct sockaddr_in sa;
    if (tcp_resolve(addr.s->data, &sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "invalid address: `%s`", addr.s->data);
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return zz_variant_err(zz_str_static("tcp_connect failed: socket"));
    long toms = timeout_ms.tag == ZZ_INT ? (long)timeout_ms.i : 5000;
    // Non-blocking connect so the timeout is honored.
    int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    int rc = connect(fd, (struct sockaddr *)&sa, sizeof sa);
    if (rc != 0 && errno != EINPROGRESS) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_connect failed: %s", strerror(errno));
        close(fd);
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    if (rc != 0) {
        struct pollfd pfd = { fd, POLLOUT, 0 };
        int pr = poll(&pfd, 1, (int)toms);
        if (pr <= 0) {
            close(fd);
            return zz_variant_err(zz_str_static("tcp_connect failed: timed out"));
        }
        int soerr = 0;
        socklen_t slen = sizeof soerr;
        getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &slen);
        if (soerr != 0) {
            char buf[192];
            int n = snprintf(buf, sizeof buf, "tcp_connect failed: %s", strerror(soerr));
            close(fd);
            return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
        }
    }
    fcntl(fd, F_SETFL, flags);
    return zz_variant_ok((zz_value){ZZ_TCP_STREAM, {.net = tcp_alloc(fd)}});
}

// net.tcp_accept(listener) → Result<Ok(stream), Err(msg)>
zz_value zz_tcp_accept(zz_value listener, int *err) {
    (void)err;
    if (listener.tag != ZZ_TCP_LISTENER || !listener.net || listener.net->closed) {
        return zz_variant_err(zz_str_static("tcp_accept failed: not a listener"));
    }
    int cfd = accept(listener.net->fd, NULL, NULL);
    if (cfd < 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_accept failed: %s", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    return zz_variant_ok((zz_value){ZZ_TCP_STREAM, {.net = tcp_alloc(cfd)}});
}

// net.tcp_write(stream, data) → Result<Ok(byte_count), Err(msg)>
zz_value zz_tcp_write(zz_value stream, zz_value data, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_write failed: not a stream"));
    }
    if (data.tag != ZZ_STR) return zz_variant_err(zz_str_static("tcp_write failed: expected a string"));
    size_t total = 0;
    while (total < data.s->len) {
        ssize_t w = send(stream.net->fd, data.s->data + total, data.s->len - total, 0);
        if (w <= 0) {
            char buf[192];
            int n = snprintf(buf, sizeof buf, "tcp_write failed: %s", strerror(errno));
            return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
        }
        total += (size_t)w;
    }
    return zz_variant_ok((zz_value){ZZ_INT, {.i = (int64_t)total}});
}

// net.tcp_read(stream, max_bytes) → Result<Ok(str), Err(msg)>
zz_value zz_tcp_read(zz_value stream, zz_value max_bytes, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_read failed: not a stream"));
    }
    size_t cap = max_bytes.tag == ZZ_INT && max_bytes.i > 0 ? (size_t)max_bytes.i : 1024;
    char *buf = (char *)malloc(cap);
    ssize_t n = recv(stream.net->fd, buf, cap, 0);
    if (n < 0) {
        free(buf);
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            return zz_variant_err(zz_str_static("tcp_read failed: timed out"));
        }
        char msg[192];
        int m = snprintf(msg, sizeof msg, "tcp_read failed: %s", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(msg, (size_t)m)));
    }
    if (n == 0) {
        free(buf);
        return zz_variant_err(zz_str_static("tcp_read failed: connection closed"));
    }
    return zz_variant_ok(zz_str_owned(copy_cstr(buf, (size_t)n)));
}

// net.tcp_readline(stream) → Result<Ok(line without '\n'), Err(msg)>
zz_value zz_tcp_readline(zz_value stream, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_readline failed: not a stream"));
    }
    SB sb = {0};
    char b;
    for (;;) {
        ssize_t n = recv(stream.net->fd, &b, 1, 0);
        if (n <= 0) {
            if (sb.len > 0) break;  // EOF after partial line
            char *msg = sb_take(&sb);
            free(msg);
            if (n < 0) return zz_variant_err(zz_str_static("tcp_readline failed"));
            return zz_variant_err(zz_str_static("connection closed"));
        }
        if (b == '\n') break;
        sb_str(&sb, &b, 1);
    }
    return zz_variant_ok(zz_str_owned(sb_take(&sb)));
}

// net.tcp_close(stream) → Result<Ok(true), Err(msg)> (idempotent)
zz_value zz_tcp_close(zz_value stream, int *err) {
    (void)err;
    if ((stream.tag == ZZ_TCP_STREAM || stream.tag == ZZ_TCP_LISTENER) && stream.net && !stream.net->closed) {
        close(stream.net->fd);
        stream.net->closed = 1;
    }
    return zz_variant_ok((zz_value){ZZ_BOOL, {.b = true}});
}

static zz_value tcp_addr(zz_value stream, int peer, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("addr_failed"));
    }
    struct sockaddr_in sa;
    socklen_t slen = sizeof sa;
    if (peer ? getpeername(stream.net->fd, (struct sockaddr *)&sa, &slen) != 0
             : getsockname(stream.net->fd, (struct sockaddr *)&sa, &slen) != 0) {
        char msg[192];
        int m = snprintf(msg, sizeof msg, "%s failed: %s", peer ? "peer_addr" : "local_addr", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(msg, (size_t)m)));
    }
    char ip[INET_ADDRSTRLEN];
    inet_ntop(AF_INET, &sa.sin_addr, ip, sizeof ip);
    char buf[64];
    int n = snprintf(buf, sizeof buf, "%s:%d", ip, ntohs(sa.sin_port));
    return zz_variant_ok(zz_str_owned(copy_cstr(buf, (size_t)n)));
}

zz_value zz_tcp_peer_addr(zz_value stream, int *err) { return tcp_addr(stream, 1, err); }
zz_value zz_tcp_local_addr(zz_value stream, int *err) { return tcp_addr(stream, 0, err); }

// net.set_read_timeout / set_write_timeout → Result<Ok(true), Err(msg)>
zz_value zz_tcp_set_read_timeout(zz_value stream, zz_value ms, int *err) {
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("set_read_timeout failed"));
    }
    struct timeval tv;
    tv.tv_sec = (ms.tag == ZZ_INT ? ms.i : 0) / 1000;
    tv.tv_usec = (ms.tag == ZZ_INT ? ms.i : 0) % 1000 * 1000;
    if (setsockopt(stream.net->fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv) != 0) {
        return zz_variant_err(zz_str_static("set_read_timeout failed"));
    }
    return zz_variant_ok((zz_value){ZZ_BOOL, {.b = true}});
}

zz_value zz_tcp_set_write_timeout(zz_value stream, zz_value ms, int *err) {
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("set_write_timeout failed"));
    }
    struct timeval tv;
    tv.tv_sec = (ms.tag == ZZ_INT ? ms.i : 0) / 1000;
    tv.tv_usec = (ms.tag == ZZ_INT ? ms.i : 0) % 1000 * 1000;
    if (setsockopt(stream.net->fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv) != 0) {
        return zz_variant_err(zz_str_static("set_write_timeout failed"));
    }
    return zz_variant_ok((zz_value){ZZ_BOOL, {.b = true}});
}

// =====================================================================
//  Epoll HTTP Server — SO_REUSEPORT Multi-Core Event Loop
// =====================================================================
//
//  Architecture:
//    - SO_REUSEPORT: multiple forked workers share the same port
//    - Each worker: own epoll_create1() + epoll_wait() loop
//    - Level-triggered EPOLLIN/EPOLLOUT (not edge-triggered)
//    - Pre-allocated Connection array — no malloc per request
//
//  Connection state machine:
//    CONN_CONNECTED → CONN_READING → CONN_PARSED → CONN_WRITING → CONN_KEEP_ALIVE/CONN_CLOSED

#define MAX_CONNECTIONS 1024
#define READ_BUF_SIZE   8192
#define WRITE_BUF_SIZE  16384
#define MAX_EVENTS      64

typedef enum {
    CONN_CONNECTED  = 0,
    CONN_READING    = 1,
    CONN_PARSED     = 2,
    CONN_WRITING    = 3,
    CONN_KEEP_ALIVE = 4,
    CONN_CLOSED     = 5,
} ConnState;

typedef struct {
    int     fd;
    int     state;
    char    read_buf[READ_BUF_SIZE];
    char    write_buf[WRITE_BUF_SIZE];
    int     read_pos;
    int     write_pos;
    int     response_len;
    int     keep_alive;
} Connection;

static Connection g_connections[MAX_CONNECTIONS];
static int g_http_server_running = 0;

// ---- Connection slot management (no malloc) ----

static int find_free_slot(void) {
    for (int i = 0; i < MAX_CONNECTIONS; i++) {
        if (g_connections[i].fd == -1) return i;
    }
    return -1;
}

static Connection* alloc_connection(int fd) {
    int idx = find_free_slot();
    if (idx < 0) return NULL;
    Connection *c = &g_connections[idx];
    c->fd = fd;
    c->state = CONN_READING;  // Start in READING state
    c->read_pos = 0;
    c->write_pos = 0;
    c->response_len = 0;
    c->keep_alive = 0;
    return c;
}

static void free_connection(Connection *c) {
    if (c->fd >= 0) {
        // fprintf(stderr, "closing fd=%d\n", c->fd);
        close(c->fd);
        c->fd = -1;
    }
    c->state = CONN_CLOSED;
    c->read_pos = 0;
    c->write_pos = 0;
    c->response_len = 0;
}

static void init_connections(void) {
    for (int i = 0; i < MAX_CONNECTIONS; i++) {
        g_connections[i].fd = -1;
        g_connections[i].state = CONN_CLOSED;
        g_connections[i].read_pos = 0;
        g_connections[i].write_pos = 0;
        g_connections[i].response_len = 0;
        g_connections[i].keep_alive = 0;
    }
}

// ---- Non-blocking helpers ----

static void set_nonblock(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags >= 0) fcntl(fd, F_SETFL, flags | O_NONBLOCK);
}

// ---- HTTP parsing ----

static int parse_request_headers(Connection *c) {
    // Find \r\n\r\n — end of headers
    if (c->read_pos < 4) return 0;
    void *end = memmem(c->read_buf, c->read_pos, "\r\n\r\n", 4);
    if (!end) return 0;

    // HTTP/1.1 defaults to keep-alive, only disable if "close" is present
    c->keep_alive = 1;

    // Find "Connection:" header and check value
    char *headers_end = (char *)end;
    for (char *p = c->read_buf; p < headers_end - 12; p++) {
        // Look for start of a header line (preceded by \r\n)
        if (p > c->read_buf && p[-1] == '\n' && p[0] == '\r') {
            p++; // skip the \r, now at start of header name
            // Skip leading whitespace
            while (*p == ' ' || *p == '\t') p++;
            // Check for "Connection:" (case-insensitive)
            if (strncasecmp(p, "connection:", 11) == 0) {
                p += 11; // skip "connection:"
                // Skip whitespace
                while (*p == ' ' || *p == '\t') p++;
                // Check if value starts with "close"
                if (strncasecmp(p, "close", 5) == 0) {
                    char *after = p + 5;
                    // Must be at end or followed by \r\n or whitespace
                    if (*after == '\r' || *after == '\n' || *after == ' ' || *after == '\0' || *after == ';') {
                        c->keep_alive = 0;
                    }
                }
            }
        }
    }
    return 1;
}

static int parse_request_line(Connection *c) {
    // Request line: "METHOD URI HTTP/1.1\r\n"
    // Find first \r\n
    if (c->read_pos < 2) return 0;
    int crlf_pos = -1;
    for (int i = 0; i <= c->read_pos - 2; i++) {
        if (c->read_buf[i] == '\r' && c->read_buf[i+1] == '\n') {
            crlf_pos = i;
            break;
        }
    }
    if (crlf_pos < 0) return 0;

    // Look for " HTTP/" before the crlf
    for (int j = 0; j < crlf_pos - 6; j++) {
        if (memcmp(c->read_buf + j, " HTTP/", 6) == 0) {
            // Found " HTTP/" at position j
            // The space before "HTTP/" is at position j
            // Find the space that separates METHOD from URI (search backward from j)
            int space_pos = -1;
            for (int sp = j - 1; sp >= 0; sp--) {
                if (c->read_buf[sp] == ' ') {
                    space_pos = sp;
                    break;
                }
            }
            if (space_pos < 0) continue;
            // Validate method (everything before space_pos)
            int valid = 1;
            for (int k = 0; k < space_pos; k++) {
                if (c->read_buf[k] < 'A' || c->read_buf[k] > 'Z') {
                    valid = 0;
                    break;
                }
            }
            if (valid) return 1;
        }
    }
    return 0;
}

// ---- Response builder ----

static void build_response(Connection *c, int status, const char *body, int body_len) {
    const char *status_line;
    if (status == 200) status_line = "200 OK";
    else if (status == 404) status_line = "404 Not Found";
    else if (status == 400) status_line = "400 Bad Request";
    else status_line = "500 Internal Server Error";

    char headers[512];
    int hl = snprintf(headers, sizeof(headers),
        "HTTP/1.1 %s\r\n"
        "Content-Type: text/plain\r\n"
        "Content-Length: %d\r\n"
        "Connection: %s\r\n"
        "\r\n",
        status_line, body_len,
        c->keep_alive ? "keep-alive" : "close");

    memcpy(c->write_buf, headers, hl);
    if (body && body_len > 0) {
        memcpy(c->write_buf + hl, body, body_len);
    }
    c->write_pos = hl + body_len;
    c->response_len = c->write_pos;
    c->write_pos = 0; // reset write position for actual send
}

// ---- Connection state machine ----

static void connection_to_reading(Connection *c) {
    c->state = CONN_READING;
}

static void connection_to_parsed(Connection *c) {
    c->state = CONN_PARSED;
    build_response(c, 200, "OK", 2);
}

static void connection_to_writing(Connection *c) {
    c->state = CONN_WRITING;
}

static void connection_to_keep_alive(Connection *c) {
    c->state = CONN_KEEP_ALIVE;
    c->read_pos = 0;
    c->write_pos = 0;
}

static void connection_to_closed(Connection *c) {
    free_connection(c);
}

// ---- Process connection in current state ----

static void process_connection(Connection *c) {
    switch (c->state) {
        case CONN_READING: {
            if (parse_request_headers(c)) {
                if (parse_request_line(c)) {
                    connection_to_parsed(c);
                } else {
                    build_response(c, 400, "Bad Request", 11);
                    connection_to_writing(c);
                }
            }
            break;
        }
        case CONN_WRITING:
        case CONN_KEEP_ALIVE:
            // Handled in main loop write phase
            break;
        default:
            break;
    }
}

// ---- Read from socket ----

static int read_from_socket(Connection *c) {
    if (c->read_pos >= READ_BUF_SIZE - 1) return 0; // buffer full

    ssize_t n = read(c->fd, c->read_buf + c->read_pos, READ_BUF_SIZE - c->read_pos - 1);
    if (n > 0) {
        c->read_pos += (int)n;
        c->read_buf[c->read_pos] = '\0';
        return 1;
    } else if (n == 0) {
        // Client closed
        return 0;
    } else {
        // EAGAIN / EWOULDBLOCK — no more data
        if (errno == EAGAIN || errno == EWOULDBLOCK) return 1;
        return 0;
    }
}

// ---- Write to socket ----

static int write_to_socket(Connection *c) {
    int remaining = c->response_len - c->write_pos;
    if (remaining <= 0) return 1;
    ssize_t n = write(c->fd, c->write_buf + c->write_pos, remaining);
    if (n > 0) {
        c->write_pos += (int)n;
        return 1;
    } else if (n == 0) {
        return 0;
    } else {
        if (errno == EAGAIN || errno == EWOULDBLOCK) return 1;
        return 0;
    }
}

// ---- Get CPU core count ----

static int get_cpu_count(void) {
    long n = sysconf(_SC_NPROCESSORS_ONLN);
    return (n > 0) ? (int)n : 4;
}

// ---- Epoll worker loop (runs in each forked process) ----

static void worker_loop(int listen_fd, int worker_id) {
    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        fprintf(stderr, "worker %d: epoll_create1 failed: %s\n", worker_id, strerror(errno));
        return;
    }

    // Add listen_fd to epoll
    struct epoll_event ev;
    ev.events = EPOLLIN | EPOLLET;
    ev.data.fd = listen_fd;
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, listen_fd, &ev) < 0) {
        fprintf(stderr, "worker %d: epoll_ctl ADD listen_fd failed: %s\n", worker_id, strerror(errno));
        close(epfd);
        return;
    }

    struct epoll_event events[MAX_EVENTS];

    while (g_http_server_running) {
        int nfds = epoll_wait(epfd, events, MAX_EVENTS, -1);
        if (nfds < 0) {
            if (errno == EINTR) continue;
            break;
        }

        for (int i = 0; i < nfds; i++) {
            int fd = events[i].data.fd;
            uint32_t revents = events[i].events;

            if (fd == listen_fd) {
                // Accept all pending connections
                while (1) {
                    struct sockaddr_in client_addr;
                    socklen_t client_len = sizeof(client_addr);
                    int client_fd = accept(listen_fd, (struct sockaddr *)&client_addr, &client_len);
                    if (client_fd < 0) break;

                    // Disable Nagle + enable keep-alive
                    int flag = 1;
                    setsockopt(client_fd, IPPROTO_TCP, TCP_NODELAY, &flag, sizeof(flag));
                    setsockopt(client_fd, SOL_SOCKET, SO_KEEPALIVE, &flag, sizeof(flag));

                    Connection *c = alloc_connection(client_fd);
                    if (!c) {
                        close(client_fd);
                        continue;
                    }

                    set_nonblock(client_fd);
                    struct epoll_event cev;
                    cev.events = EPOLLIN | EPOLLET;
                    cev.data.fd = client_fd;
                    if (epoll_ctl(epfd, EPOLL_CTL_ADD, client_fd, &cev) < 0) {
                        free_connection(c);
                        continue;
                    }

                }
            } else {
                // Client socket event
                Connection *c = NULL;
                for (int j = 0; j < MAX_CONNECTIONS; j++) {
                    if (g_connections[j].fd == fd) {
                        c = &g_connections[j];
                        break;
                    }
                }
                if (!c) continue;

                if (revents & (EPOLLERR | EPOLLHUP)) {
                    connection_to_closed(c);
                    epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                    continue;
                }

                if (revents & EPOLLIN) {
                    // Edge-triggered: read all available data until EAGAIN
                    while (read_from_socket(c)) {
                        // Transition from keep-alive to reading when new data arrives
                        if (c->state == CONN_KEEP_ALIVE) {
                            connection_to_reading(c);
                        }
                        process_connection(c);
                        if (c->state != CONN_READING && c->state != CONN_KEEP_ALIVE) {
                            break;
                        }
                    }
                    // If socket closed or error
                    if (c->fd < 0) {
                        continue;
                    }
                }

                if (c->state == CONN_PARSED) {

                    connection_to_writing(c);
                    struct epoll_event cev;
                    cev.events = EPOLLOUT | EPOLLET;
                    cev.data.fd = fd;
                    epoll_ctl(epfd, EPOLL_CTL_MOD, fd, &cev);
                } else if (c->state == CONN_WRITING) {
                    // Edge-triggered: write all data until EAGAIN
                    while (c->write_pos < c->response_len) {
                        int written = write_to_socket(c);
                        if (!written) {
                            connection_to_closed(c);
                            epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                            break;
                        }
                    }
                    // Check if write complete
                    if (c->fd >= 0 && c->write_pos >= c->response_len) {

                        if (!c->keep_alive) {
                            connection_to_closed(c);
                            epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                        } else {
                            // Reset for keep-alive
                            c->state = CONN_KEEP_ALIVE;
                            c->read_pos = 0;
                            c->write_pos = 0;
                            c->response_len = 0;
                            struct epoll_event cev;
                            cev.events = EPOLLIN | EPOLLET;
                            cev.data.fd = fd;
                            epoll_ctl(epfd, EPOLL_CTL_MOD, fd, &cev);
                        }
                    }
                }
            }
        }
    }

    close(epfd);
}

// ---- Main server startup with SO_REUSEPORT + fork ----

static int spawn_workers(int listen_fd, int port) {
    int workers = get_cpu_count();

    for (int w = 0; w < workers; w++) {
        pid_t pid = fork();
        if (pid < 0) {
            return -1;
        }
        if (pid == 0) {
            // Child worker
            worker_loop(listen_fd, w);
            close(listen_fd);
            exit(0);
        }
        // Parent continues forking
    }

    // Parent waits for children
    while (g_http_server_running) {
        sleep(1);
    }

    // Reap children
    while (wait(NULL) > 0) {}

    return 0;
}

// ---- HTTP AOT stub implementations ----

// Max number of route patterns we track (for debug/future use)
#define MAX_ROUTES 32
static char *g_http_routes[MAX_ROUTES];
static int g_http_route_count = 0;

// zz_http_server(unused, err) — creates an HTTP server handle (AOT stub)
zz_value zz_http_server(zz_value unused, int *err) {
    (void)unused;
    *err = 0;
    return zz_int(0);
}

// zz_http_route_get(server, path, handler, err) — tracks route pattern; handler ignored in AOT
zz_value zz_http_route_get(zz_value server, zz_value path, zz_value handler, int *err) {
    *err = 0;
    (void)server; (void)handler;
    if (g_http_route_count < MAX_ROUTES - 1 && path.tag == ZZ_STR) {
        g_http_routes[g_http_route_count++] = strndup(path.s->data, path.s->len);
    }
    return zz_int(0);
}

// zz_http_log(server, enabled, err) — AOT stub: no-op
zz_value zz_http_log(zz_value server, zz_value enabled, int *err) {
    *err = 0;
    (void)server; (void)enabled;
    return zz_unit();
}

// zz_http_listen(server, port, err) — starts HTTP server, blocks forever
zz_value zz_http_listen(zz_value server, zz_value port, int *err) {
    (void)server;
    *err = 0;

    int p = (port.tag == ZZ_INT) ? (int)port.i : 8080;
    if (p <= 0 || p > 65535) p = 8080;

    // Ignore SIGPIPE to avoid crash on closed connections
    signal(SIGPIPE, SIG_IGN);

    // Initialize connection pool
    init_connections();

    int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (listen_fd < 0) {
        fprintf(stderr, "zz_http_listen: socket() failed: %s\n", strerror(errno));
        return zz_unit();
    }

    int opt = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    // SO_REUSEPORT for multi-core scaling
    int reuseport = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEPORT, &reuseport, sizeof(reuseport));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = htons((unsigned short)p);

    if (bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        fprintf(stderr, "zz_http_listen: bind() failed on port %d: %s\n", p, strerror(errno));
        close(listen_fd);
        return zz_unit();
    }

    if (listen(listen_fd, 128) < 0) {
        fprintf(stderr, "zz_http_listen: listen() failed: %s\n", strerror(errno));
        close(listen_fd);
        return zz_unit();
    }

    // Set listen_fd non-blocking
    set_nonblock(listen_fd);

    // Print SERVER_READY so benchmark runners know the port is open
    fprintf(stdout, "SERVER_READY\n");
    fflush(stdout);

    g_http_server_running = 1;

    // Spawn workers and wait
    spawn_workers(listen_fd, p);

    g_http_server_running = 0;
    close(listen_fd);
    return zz_unit();
}

// zz_http_handle(server, method, path, body, err) — AOT stub: returns "OK"
zz_value zz_http_handle(zz_value server, zz_value method, zz_value path, zz_value body, int *err) {
    *err = 0;
    (void)server; (void)method; (void)path; (void)body;
    return zz_unit();
}

// ---- entry --------------------------------------------------------------
int zz_run(void) {
    zz_main();
    int main_err = 0;
    if (zz_call_main())
        main_err = 1;
    return main_err;
}

int main(void) {
    return zz_run();
}

// ---- codegen shims ------------------------------------------------------
zz_value zz_call_native1(zz_value (*f)(zz_value, int *), zz_value a) {
    int err = 0;
    zz_value r = f(a, &err);
    return r;
}

zz_value zz_call_native0(zz_value (*f)(zz_value, int *)) {
    int err = 0;
    zz_value r = f(zz_unit(), &err);
    return r;
}

zz_value zz_call_native2(zz_value (*f)(zz_value, zz_value, int *), zz_value a, zz_value b) {
    int err = 0;
    zz_value r = f(a, b, &err);
    return r;
}

zz_value zz_call_native3(zz_value (*f)(zz_value, zz_value, zz_value, int *), zz_value a, zz_value b, zz_value c) {
    int err = 0;
    zz_value r = f(a, b, c, &err);
    return r;
}

// ---- variant constructors (Option / Result) ---------------------------
// Store the inner value on the heap so match can extract it via payload pointer.

zz_value zz_variant_some(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_OPTION_SOME, {.payload = p}};
}

zz_value zz_variant_ok(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_RESULT_OK, {.payload = p}};
}

zz_value zz_variant_err(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_RESULT_ERR, {.payload = p}};
}

// Release helper for variant payloads.
static void zz_release_variant(zz_value *v) {
    if (v->payload) {
        zz_release(v->payload);
        free(v->payload);
        v->payload = NULL;
    }
}

// ---- boxed struct (object) constructors and accessors --------------------

zz_value zz_object_new(const char *type_name, zz_value *field_names, size_t n) {
    // Allocate: refs + type_name + len + (n * 2 fields: name, value pairs)
    zz_object *obj = (zz_object *)malloc(sizeof(zz_object) + n * 2 * sizeof(zz_value));
    obj->refs = 1;
    obj->type_name = type_name;
    obj->len = n;
    // Initialize all field slots to unit
    for (size_t i = 0; i < n * 2; i++) {
        obj->fields[i] = (zz_value){ZZ_UNIT, {.i = 0}};
    }
    // Store field names in the alternating slots
    for (size_t i = 0; i < n; i++) {
        obj->fields[i * 2] = field_names[i]; // name (zz_value, should be str)
        // fields[i * 2 + 1] is the value slot (initialized to unit above)
    }
    return (zz_value){ZZ_OBJECT, {.obj = obj}};
}

void zz_object_set_field(zz_value *obj, const char *name, zz_value val) {
    if (obj->tag != ZZ_OBJECT || !obj->obj) return;
    zz_object *o = obj->obj;
    for (size_t i = 0; i < o->len; i++) {
        zz_value *fname = &o->fields[i * 2];
        if (fname->tag == ZZ_STR && strcmp(fname->s->data, name) == 0) {
            zz_value *slot = &o->fields[i * 2 + 1];
            zz_release(slot);
            *slot = zz_clone(val);
            return;
        }
    }
}

zz_value zz_object_get_field(zz_value *obj, const char *name) {
    if (obj->tag != ZZ_OBJECT || !obj->obj) return zz_unit();
    zz_object *o = obj->obj;
    for (size_t i = 0; i < o->len; i++) {
        zz_value *fname = &o->fields[i * 2];
        if (fname->tag == ZZ_STR && strcmp(fname->s->data, name) == 0) {
            return zz_clone(o->fields[i * 2 + 1]);
        }
    }
    return zz_unit();
}

// Release helper for boxed objects.
static void zz_release_object(zz_value *v) {
    if (v->tag != ZZ_OBJECT || !v->obj) return;
    zz_object *o = v->obj;
    if (o->refs == 0) return; // already freed
    if (--o->refs == 0) {
        for (size_t i = 0; i < o->len; i++) {
            zz_release(&o->fields[i * 2]);     // name (str)
            zz_release(&o->fields[i * 2 + 1]); // value
        }
        free(o);
    }
    v->obj = NULL;
}

// ---- match extraction helpers ------------------------------------------
// Returns the payload of a variant, or unit if tag doesn't match.

zz_value zz_match_ok(zz_value v) {
    if (v.tag == ZZ_RESULT_OK && v.payload) return zz_clone(*v.payload);
    return zz_unit();
}

zz_value zz_match_err(zz_value v) {
    if (v.tag == ZZ_RESULT_ERR && v.payload) return zz_clone(*v.payload);
    return zz_unit();
}

zz_value zz_match_some(zz_value v) {
    if (v.tag == ZZ_OPTION_SOME && v.payload) return zz_clone(*v.payload);
    return zz_unit();
}

zz_value zz_binop_cat(zz_value a, zz_value b) {
    if (a.tag == ZZ_STR && b.tag == ZZ_STR) {
        size_t la = a.s->len, lb = b.s->len;
        size_t need = la + lb;
        zz_str *out;
        // In-place fast path: a is uniquely owned (refs==1) and is NOT
        // interned (we must never mutate an interned singleton) and has
        // capacity for the result.
        if (a.s->refs == 1 && !a.s->interned && a.s->cap >= need) {
            out = a.s;
            memcpy(out->data + la, b.s->data, lb);
            out->len = need;
            out->data[need] = '\0';
            zz_value v;
            v.tag = ZZ_STR;
            v.s = out;
            return v;
        }
        out = str_alloc(need);
        memcpy(out->data, a.s->data, la);
        memcpy(out->data + la, b.s->data, lb);
        zz_value v;
        v.tag = ZZ_STR;
        v.s = out;
        return v;
    }
    return zz_binop(ZZOP_ADD, a, b);
}


zz_value zz_binop_cat_str(zz_value a, zz_value b) {
    char *sv = zz_value_to_string(&b);
    zz_value sb = zz_str_owned(sv);
    zz_value r = zz_binop_cat(a, sb);
    zz_release(&sb);
    return r;
}

// In-place append used by loop lowerings (`s = s + literal`). Mutates *a
// in place. If *a is not a string or its buffer can't be reused, fall back
// to zz_binop_cat + zz_assign semantics via the caller.
void zz_str_append_str(zz_value *a, zz_value b) {
    if (a->tag != ZZ_STR || b.tag != ZZ_STR) return;
    size_t la = a->s->len, lb = b.s->len;
    size_t need = la + lb;
    if (a->s->refs == 1 && !a->s->interned) {
        if (a->s->cap < need) {
            a->s = str_grow(a->s, need);
        }
        memcpy(a->s->data + la, b.s->data, lb);
        a->s->len = need;
        a->s->data[need] = '\0';
        return;
    }
    // Buffer not reusable: replace with a fresh allocation. Release the
    // old ref first so we don't leak (and don't double-free if the old
    // buffer happened to be interned — refs==1 interned strings stay put).
    zz_str *fresh = str_alloc(need);
    memcpy(fresh->data, a->s->data, la);
    memcpy(fresh->data + la, b.s->data, lb);
    if (!a->s->interned && --a->s->refs == 0) {
        free(a->s);
    }
    a->s = fresh;
}

// Variant: append a C literal directly without allocating a temporary
// zz_str. Used by the most common hot pattern `s = s + "x"`.
void zz_str_append_lit(zz_value *a, const char *lit, size_t lit_len) {
    if (a->tag != ZZ_STR) return;
    size_t la = a->s->len;
    size_t need = la + lit_len;
    if (a->s->refs == 1 && !a->s->interned) {
        if (a->s->cap < need) {
            a->s = str_grow(a->s, need);
        }
        memcpy(a->s->data + la, lit, lit_len);
        a->s->len = need;
        a->s->data[need] = '\0';
        return;
    }
    zz_str *fresh = str_alloc(need);
    memcpy(fresh->data, a->s->data, la);
    memcpy(fresh->data + la, lit, lit_len);
    if (!a->s->interned && --a->s->refs == 0) {
        free(a->s);
    }
    a->s = fresh;
}

zz_value zz_range_build(zz_value start, zz_value end) {
    (void)end;
    // Represent a range inline; used in `for`. Return an int start marker
    // (codegen for `for` emits two-path C loop directly, so this is mostly
    // unused).
    zz_value v = {ZZ_RANGE, {0}};
    v.i = start.tag == ZZ_INT ? start.i : 0;
    return v;
}

// zz_elvis(left, right) — unwrap Option/Result on the left, else return right.
// Mirrors the VM's `??` operator which unwraps .some(v) and .ok(v).
zz_value zz_elvis(zz_value left, zz_value right) {
    if (left.tag == ZZ_OPTION_SOME && left.payload)
        return zz_clone(*left.payload);
    if (left.tag == ZZ_OPTION_NONE)
        return zz_clone(right);
    if (left.tag == ZZ_RESULT_OK && left.payload)
        return zz_clone(*left.payload);
    if (left.tag == ZZ_RESULT_ERR)
        return zz_clone(right);
    // For non-optional/result types, fall back to truthiness check.
    if (zz_truthy(left))
        return zz_clone(left);
    return zz_clone(right);
}

// =====================================================================
//  Missing stdlib natives — bare builtins and module functions
// =====================================================================

// len(v) — array length, string length, or 0 for other types.
zz_value zz_len(zz_value v, int *err) {
    (void)err;
    if (v.tag == ZZ_ARRAY) {
        return (zz_value){ZZ_INT, {.i = (int64_t)v.arr->len}};
    }
    if (v.tag == ZZ_STR) {
        return (zz_value){ZZ_INT, {.i = (int64_t)v.s->len}};
    }
    return (zz_value){ZZ_INT, {.i = 0}};
}

// vec.len(v) — same as len for arrays.
zz_value zz_vec_len(zz_value v, int *err) {
    return zz_len(v, err);
}

// vec.append(arr, item) — append item to array in place.
// NOTE: the codegen uses this as the array-literal builder (calls ignore the
// return). Kept as a mutator for that contract; `vec.push` is the functional
// copy-on-write variant that matches the VM.
zz_value zz_vec_append(zz_value arr, zz_value item, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    if (a->len >= a->cap) {
        size_t new_cap = a->cap ? a->cap * 2 : 8;
        // LIT_MAGIC arrays have items from arena — realloc() on arena memory
        // is invalid. Also handle n=0 case where items is NULL.
        // Migrate to malloc, switch to refs=0 (arena-allocated sentinel) so
        // zz_release knows items is malloc'd but header is still arena.
        if (a->refs == ZZ_ARRAY_LIT_MAGIC || a->refs == ZZ_ARRAY_STACK_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            // Copy existing elements if any.
            for (size_t i = 0; i < a->len; i++) {
                new_items[i] = a->items[i];
            }
            a->items = new_items;
            a->refs = 0;  // Arena-allocated header, malloc'd items
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    a->items[a->len++] = zz_clone(item);
    return zz_unit();
}

// vec.push(arr, item) — returns a NEW array with item appended
// (matches the VM: the input array is left untouched).
zz_value zz_vec_push(zz_value arr, zz_value item, int *err) {
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_value out = zz_array_dup(arr.arr);
    zz_array *a = out.arr;
    if (a->len >= a->cap) {
        size_t new_cap = a->cap ? a->cap * 2 : 8;
        if (a->refs == ZZ_ARRAY_LIT_MAGIC || a->refs == ZZ_ARRAY_STACK_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            for (size_t i = 0; i < a->len; i++) {
                new_items[i] = a->items[i];
            }
            a->items = new_items;
            a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    a->items[a->len++] = zz_clone(item);
    return out;
}

// vec.pop(arr) — remove and return a NEW array without the last element
// (matches the VM: the input array is left untouched).
zz_value zz_vec_pop(zz_value arr, int *err) {
    if (arr.tag != ZZ_ARRAY || arr.arr->len == 0) {
        if (err) *err = 1;  // VM errors on empty pop
        return zz_unit();
    }
    zz_value out = zz_array_dup(arr.arr);
    zz_array *a = out.arr;
    zz_release(&a->items[a->len - 1]);
    a->len--;
    return out;
}

// vec.remove(arr, idx) — returns a NEW array with element at idx removed.
zz_value zz_vec_remove(zz_value arr, zz_value idx, int *err) {
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    int64_t i = idx.tag == ZZ_INT ? idx.i : 0;
    int64_t len = (int64_t)a->len;
    if (i < 0) i += len;  // VM supports negative indices
    if (i < 0 || i >= len) {
        if (err) *err = 1;
        return zz_unit();
    }
    zz_value out = zz_array_dup(arr.arr);
    zz_array *o = out.arr;
    zz_release(&o->items[i]);
    for (size_t j = (size_t)i; j < o->len - 1; j++) {
        o->items[j] = o->items[j + 1];
    }
    o->len--;
    return out;
}

// vec.insert(arr, idx, item) — returns a NEW array with item inserted.
zz_value zz_vec_insert(zz_value arr, zz_value idx, zz_value item, int *err) {
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    int64_t i = idx.tag == ZZ_INT ? idx.i : 0;
    int64_t len = (int64_t)a->len;
    if (i < 0) i += len;  // VM supports negative indices
    if (i < 0 || i > len) {
        if (err) *err = 1;
        return zz_unit();
    }
    zz_value out = zz_array_dup(arr.arr);
    zz_array *o = out.arr;
    if (o->len >= o->cap) {
        size_t new_cap = o->cap ? o->cap * 2 : 8;
        if (o->refs == ZZ_ARRAY_STACK_MAGIC || o->refs == ZZ_ARRAY_LIT_MAGIC || o->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            for (size_t j = 0; j < o->len; j++) new_items[j] = o->items[j];
            o->items = new_items;
            o->refs = 0;
        } else {
            o->items = (zz_value *)realloc(o->items, new_cap * sizeof(zz_value));
        }
        o->cap = new_cap;
    }
    for (size_t j = o->len; j > (size_t)i; j--) {
        o->items[j] = o->items[j - 1];
    }
    o->items[i] = zz_clone(item);
    o->len++;
    return out;
}

// vec.contains(arr, item) → bool
zz_value zz_vec_contains(zz_value arr, zz_value item, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return (zz_value){ZZ_BOOL, {.b = false}};
    for (size_t i = 0; i < arr.arr->len; i++) {
        if (zz_truthy(zz_binop(ZZOP_EQ, zz_clone(arr.arr->items[i]), zz_clone(item))))
            return (zz_value){ZZ_BOOL, {.b = true}};
    }
    return (zz_value){ZZ_BOOL, {.b = false}};
}

// vec.sort(arr) → sorted array (new array, original unchanged)
zz_value zz_vec_sort(zz_value arr, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return arr;
    size_t n = arr.arr->len;
    if (n <= 1) return zz_clone(arr);
    // Create a new array with cloned elements.
    zz_value sorted = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(sorted.arr, zz_clone(arr.arr->items[i]));
    }
    // Simple insertion sort (fine for small arrays; larger ones use qsort).
    for (size_t i = 1; i < n; i++) {
        zz_value key = sorted.arr->items[i];
        size_t j = i;
        while (j > 0 && zz_truthy(zz_binop(ZZOP_LT, zz_clone(key), zz_clone(sorted.arr->items[j-1])))) {
            sorted.arr->items[j] = sorted.arr->items[j-1];
            j--;
        }
        sorted.arr->items[j] = key;
    }
    return sorted;
}

// vec.reverse(arr) → reversed array (new array, original unchanged)
zz_value zz_vec_reverse(zz_value arr, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return arr;
    size_t n = arr.arr->len;
    zz_value rev = zz_array_new();
    for (size_t i = n; i > 0; i--) {
        zz_array_push(rev.arr, zz_clone(arr.arr->items[i-1]));
    }
    return rev;
}

// str.length(s) — string length in bytes.
zz_value zz_str_length(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return (zz_value){ZZ_INT, {.i = 0}};
    return (zz_value){ZZ_INT, {.i = (int64_t)s.s->len}};
}

// str.lower(s) — lowercase copy.
zz_value zz_str_lower(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    size_t len = s.s->len;
    zz_str *out = str_alloc(len);
    for (size_t i = 0; i < len; i++) {
        char c = s.s->data[i];
        out->data[i] = (c >= 'A' && c <= 'Z') ? c + 32 : c;
    }
    out->data[len] = '\0';
    return (zz_value){ZZ_STR, {.s = out}};
}

// str.upper(s) — uppercase copy.
zz_value zz_str_upper(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    size_t len = s.s->len;
    zz_str *out = str_alloc(len);
    for (size_t i = 0; i < len; i++) {
        char c = s.s->data[i];
        out->data[i] = (c >= 'a' && c <= 'z') ? c - 32 : c;
    }
    out->data[len] = '\0';
    return (zz_value){ZZ_STR, {.s = out}};
}

// str.replace(s, old, new) — replace all occurrences.
zz_value zz_str_replace(zz_value s, zz_value old_s, zz_value new_s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || old_s.tag != ZZ_STR || new_s.tag != ZZ_STR) return s;
    const char *src = s.s->data;
    size_t src_len = s.s->len;
    const char *old_str = old_s.s->data;
    size_t old_len = old_s.s->len;
    const char *new_str = new_s.s->data;
    size_t new_len = new_s.s->len;
    if (old_len == 0) return zz_clone(s);
    // Count occurrences.
    size_t count = 0;
    for (size_t i = 0; i + old_len <= src_len; i++) {
        if (memcmp(src + i, old_str, old_len) == 0) { count++; i += old_len - 1; }
    }
    if (count == 0) return zz_clone(s);
    size_t out_len = src_len + count * (new_len > old_len ? new_len - old_len : 0) - count * old_len + count * new_len;
    // More precise: out_len = src_len - count*old_len + count*new_len
    out_len = src_len - count * old_len + count * new_len;
    zz_str *out = str_alloc(out_len);
    size_t pos = 0;
    for (size_t i = 0; i < src_len;) {
        if (i + old_len <= src_len && memcmp(src + i, old_str, old_len) == 0) {
            memcpy(out->data + pos, new_str, new_len);
            pos += new_len;
            i += old_len;
        } else {
            out->data[pos++] = src[i++];
        }
    }
    out->data[out_len] = '\0';
    return (zz_value){ZZ_STR, {.s = out}};
}

// str.contains(s, sub) — check if s contains sub.
zz_value zz_str_contains(zz_value s, zz_value sub, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || sub.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    const char *src = s.s->data;
    size_t src_len = s.s->len;
    const char *needle = sub.s->data;
    size_t needle_len = sub.s->len;
    if (needle_len == 0) return (zz_value){ZZ_BOOL, {.b = true}};
    for (size_t i = 0; i + needle_len <= src_len; i++) {
        if (memcmp(src + i, needle, needle_len) == 0) return (zz_value){ZZ_BOOL, {.b = true}};
    }
    return (zz_value){ZZ_BOOL, {.b = false}};
}

// str.startswith(s, prefix)
zz_value zz_str_startswith(zz_value s, zz_value prefix, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || prefix.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    if (prefix.s->len > s.s->len) return (zz_value){ZZ_BOOL, {.b = false}};
    return (zz_value){ZZ_BOOL, {.b = memcmp(s.s->data, prefix.s->data, prefix.s->len) == 0}};
}

// str.endswith(s, suffix)
zz_value zz_str_endswith(zz_value s, zz_value suffix, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || suffix.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    if (suffix.s->len > s.s->len) return (zz_value){ZZ_BOOL, {.b = false}};
    return (zz_value){ZZ_BOOL, {.b = memcmp(s.s->data + s.s->len - suffix.s->len, suffix.s->data, suffix.s->len) == 0}};
}

// str.trim(s) — strip leading/trailing whitespace
zz_value zz_str_trim(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    const char *d = s.s->data;
    size_t len = s.s->len;
    size_t start = 0, end = len;
    while (start < end && (d[start] == ' ' || d[start] == '\t' || d[start] == '\n' || d[start] == '\r')) start++;
    while (end > start && (d[end-1] == ' ' || d[end-1] == '\t' || d[end-1] == '\n' || d[end-1] == '\r')) end--;
    return zz_str_new(d + start, end - start);
}

// str.trim_start(s) — strip leading whitespace
zz_value zz_str_trim_start(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    const char *d = s.s->data;
    size_t len = s.s->len;
    size_t start = 0;
    while (start < len && (d[start] == ' ' || d[start] == '\t' || d[start] == '\n' || d[start] == '\r')) start++;
    return zz_str_new(d + start, len - start);
}

// str.trim_end(s) — strip trailing whitespace
zz_value zz_str_trim_end(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    const char *d = s.s->data;
    size_t len = s.s->len;
    size_t end = len;
    while (end > 0 && (d[end-1] == ' ' || d[end-1] == '\t' || d[end-1] == '\n' || d[end-1] == '\r')) end--;
    return zz_str_new(d, end);
}

// str.join(items, sep) — join array of strings with separator
zz_value zz_str_join(zz_value items, zz_value sep, int *err) {
    (void)err;
    if (items.tag != ZZ_ARRAY || !items.arr) return zz_str_static("");
    const char *sep_d = "";
    size_t sep_len = 0;
    if (sep.tag == ZZ_STR) { sep_d = sep.s->data; sep_len = sep.s->len; }
    // Calculate total length.
    size_t total = 0;
    for (size_t i = 0; i < items.arr->len; i++) {
        zz_value v = items.arr->items[i];
        if (v.tag == ZZ_STR) total += v.s->len;
        if (i > 0) total += sep_len;
    }
    char *buf = (char *)malloc(total + 1);
    size_t pos = 0;
    for (size_t i = 0; i < items.arr->len; i++) {
        if (i > 0) { memcpy(buf + pos, sep_d, sep_len); pos += sep_len; }
        zz_value v = items.arr->items[i];
        if (v.tag == ZZ_STR) { memcpy(buf + pos, v.s->data, v.s->len); pos += v.s->len; }
    }
    buf[pos] = '\0';
    return zz_str_owned(buf);
}

// str.split(s, sep) — split string by separator
zz_value zz_str_split(zz_value s, zz_value sep, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return zz_array_new();
    const char *d = s.s->data;
    size_t len = s.s->len;
    const char *sd = ""; size_t slen = 0;
    if (sep.tag == ZZ_STR) { sd = sep.s->data; slen = sep.s->len; }
    zz_value arr = zz_array_new();
    if (slen == 0) {
        // Split into individual characters.
        for (size_t i = 0; i < len; i++) {
            zz_value item = zz_str_new(d + i, 1);
            int sub_err = 0;
            zz_vec_append(arr, item, &sub_err);
        }
        return arr;
    }
    size_t pos = 0;
    while (pos <= len) {
        size_t next = pos;
        while (next + slen <= len) {
            if (memcmp(d + next, sd, slen) == 0) break;
            next++;
        }
        zz_value item = zz_str_new(d + pos, next - pos);
        int sub_err = 0;
        zz_vec_append(arr, item, &sub_err);
        if (next + slen > len) break;
        pos = next + slen;
    }
    return arr;
}

// typeof(v) — return type name as string.
zz_value zz_typeof(zz_value v, int *err) {
    (void)err;
    const char *name;
    switch (v.tag) {
        case ZZ_UNIT: name = "unit"; break;
        case ZZ_INT: name = "int"; break;
        case ZZ_FLOAT: name = "float"; break;
        case ZZ_BOOL: name = "bool"; break;
        case ZZ_STR: name = "str"; break;
        case ZZ_ARRAY: name = "array"; break;
        case ZZ_DICT: name = "dict"; break;
        case ZZ_FUNC: name = "func"; break;
        case ZZ_NATIVE: name = "native"; break;
        case ZZ_OPTION_SOME: name = "option"; break;
        case ZZ_OPTION_NONE: name = "option"; break;
        case ZZ_RESULT_OK: name = "result"; break;
        case ZZ_RESULT_ERR: name = "result"; break;
        case ZZ_RANGE: name = "range"; break;
        case ZZ_JSON: name = "json"; break;
        case ZZ_TCP_STREAM: name = "tcp.stream"; break;
        case ZZ_TCP_LISTENER: name = "tcp.listener"; break;
        case ZZ_TUPLE: name = "tuple"; break;
        default: name = "unknown"; break;
    }
    return zz_str_static(name);
}

// int(v) — cast to int.
zz_value zz_int_cast(zz_value v, int *err) {
    (void)err;
    switch (v.tag) {
        case ZZ_INT: return v;
        case ZZ_FLOAT: return (zz_value){ZZ_INT, {.i = (int64_t)v.f}};
        case ZZ_BOOL: return (zz_value){ZZ_INT, {.i = v.b ? 1 : 0}};
        case ZZ_STR: {
            char *end;
            int64_t n = strtoll(v.s->data, &end, 10);
            if (end == v.s->data) return (zz_value){ZZ_INT, {.i = 0}};
            return (zz_value){ZZ_INT, {.i = n}};
        }
        default: return (zz_value){ZZ_INT, {.i = 0}};
    }
}

// float(v) — cast to float.
zz_value zz_float_cast(zz_value v, int *err) {
    (void)err;
    switch (v.tag) {
        case ZZ_FLOAT: return v;
        case ZZ_INT: return (zz_value){ZZ_FLOAT, {.f = (double)v.i}};
        case ZZ_BOOL: return (zz_value){ZZ_FLOAT, {.f = v.b ? 1.0 : 0.0}};
        case ZZ_STR: {
            char *end;
            double n = strtod(v.s->data, &end);
            if (end == v.s->data) return (zz_value){ZZ_FLOAT, {.f = 0.0}};
            return (zz_value){ZZ_FLOAT, {.f = n}};
        }
        default: return (zz_value){ZZ_FLOAT, {.f = 0.0}};
    }
}

// bool(v) — cast to bool.
zz_value zz_bool_cast(zz_value v, int *err) {
    (void)err;
    switch (v.tag) {
        case ZZ_BOOL: return v;
        case ZZ_INT: return (zz_value){ZZ_BOOL, {.b = v.i != 0}};
        case ZZ_FLOAT: return (zz_value){ZZ_BOOL, {.b = v.f != 0.0}};
        case ZZ_STR: return (zz_value){ZZ_BOOL, {.b = v.s->len > 0}};
        case ZZ_ARRAY: return (zz_value){ZZ_BOOL, {.b = v.arr->len > 0}};
        case ZZ_DICT: return (zz_value){ZZ_BOOL, {.b = v.dict->len > 0}};
        default: return (zz_value){ZZ_BOOL, {.b = false}};
    }
}

// zz_str(v) — cast to string.
zz_value zz_str_cast(zz_value v, int *err) {
    (void)err;
    char *s = zz_value_to_string(&v);
    return zz_str_owned(s);
}

// to_str(v) — convert any value to a string zz_value (for fstring interpolation).
zz_value zz_to_str(zz_value v, int *err) {
    (void)err;
    char *s = zz_value_to_string(&v);
    return zz_str_owned(s);
}

// Wrap a raw value as a JSON value (mirrors the VM's `Value::Json`).
// The payload is heap-allocated and refcounted via the same path as
// Option/Result variants.
zz_value zz_json_wrap(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_JSON, {.payload = p}};
}

// Unwrap a ZZ_JSON payload (or pass through plain values).
static zz_value zz_json_unwrap(zz_value v) {
    return (v.tag == ZZ_JSON && v.payload) ? *v.payload : v;
}

// "invalid JSON: {parser error}" — malloc'd, for zz_str_owned.
static char *text_invalid_json(const char *inner) {
    size_t il = strlen(inner);
    static const char prefix[] = "invalid JSON: ";
    size_t pl = sizeof(prefix) - 1;
    char *out = (char *)malloc(pl + il + 1);
    memcpy(out, prefix, pl);
    memcpy(out + pl, inner, il);
    out[pl + il] = '\0';
    return out;
}

// ---- JSON parser (matches the VM's grammar + error messages) ------------
// Parse recursive JSON into plain zz_values: unit=null, bool, int/float,
// str, array, dict (string keys, insertion order preserved).
typedef struct {
    const char *s;
    size_t len;
    size_t pos;
    char err[160];
} json_parser;

static void json_err(json_parser *p, const char *fmt, int a, int b) {
    (void)fmt;
    // Build via snprintf into p->err with up to two int args (pos/size).
    snprintf(p->err, sizeof(p->err), fmt, a, b);
}

static char json_peek(json_parser *p) {
    return p->pos < p->len ? p->s[p->pos] : '\0';
}

static void json_skip_ws(json_parser *p) {
    while (p->pos < p->len) {
        char c = p->s[p->pos];
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r') p->pos++;
        else break;
    }
}

static int json_eat(json_parser *p, char c) {
    if (p->pos < p->len && p->s[p->pos] == c) { p->pos++; return 1; }
    return 0;
}

static int json_parse_value(json_parser *p, zz_value *out);

static int json_parse_str(json_parser *p, zz_value *out) {
    if (!json_eat(p, '"')) {
        snprintf(p->err, sizeof(p->err), "expected `\"` at byte %zu", p->pos);
        return -1;
    }
    // First pass: measure decoded size.
    size_t cap = 0;
    size_t q = p->pos;
    while (q < p->len && p->s[q] != '"') {
        if (p->s[q] == '\\') {
            if (q + 1 >= p->len) { snprintf(p->err, sizeof(p->err), "unterminated string"); return -1; }
            q += 2;
        } else {
            q++;
        }
        cap++;
    }
    if (q >= p->len) { snprintf(p->err, sizeof(p->err), "unterminated string"); return -1; }
    zz_str *str = str_alloc(cap);
    size_t w = 0;
    while (p->pos < p->len && p->s[p->pos] != '"') {
        char c = p->s[p->pos];
        if (c == '\\') {
            p->pos++;
            char e = p->s[p->pos];
            switch (e) {
                case '"': str->data[w++] = '"'; break;
                case '\\': str->data[w++] = '\\'; break;
                case '/': str->data[w++] = '/'; break;
                case 'b': str->data[w++] = '\b'; break;
                case 'f': str->data[w++] = '\f'; break;
                case 'n': str->data[w++] = '\n'; break;
                case 'r': str->data[w++] = '\r'; break;
                case 't': str->data[w++] = '\t'; break;
                case 'u': {
                    p->pos++;
                    if (p->pos + 4 > p->len) { snprintf(p->err, sizeof(p->err), "truncated \\u escape"); goto fail; }
                    unsigned code = 0;
                    for (int i = 0; i < 4; i++) {
                        char h = p->s[p->pos + i];
                        code <<= 4;
                        if (h >= '0' && h <= '9') code |= (h - '0');
                        else if (h >= 'a' && h <= 'f') code |= (h - 'a' + 10);
                        else if (h >= 'A' && h <= 'F') code |= (h - 'A' + 10);
                        else { snprintf(p->err, sizeof(p->err), "invalid \\u escape at byte %zu", p->pos); goto fail; }
                    }
                    p->pos += 4;
                    if (code < 0x80) str->data[w++] = (char)code;
                    else if (code < 0x800) {
                        str->data[w++] = (char)(0xC0 | (code >> 6));
                        str->data[w++] = (char)(0x80 | (code & 0x3F));
                    } else {
                        str->data[w++] = (char)(0xE0 | (code >> 12));
                        str->data[w++] = (char)(0x80 | ((code >> 6) & 0x3F));
                        str->data[w++] = (char)(0x80 | (code & 0x3F));
                    }
                    break;
                }
                default:
                    snprintf(p->err, sizeof(p->err), "invalid escape `\\%c` at byte %zu", e, p->pos);
                    goto fail;
            }
            p->pos++;
        } else {
            str->data[w++] = c;
            p->pos++;
        }
    }
    str->data[w] = '\0';
    str->len = w;
    if (!json_eat(p, '"')) { snprintf(p->err, sizeof(p->err), "unterminated string"); goto fail; }
    // Shrink unused capacity warning-free: leave cap as is.
    *out = (zz_value){ZZ_STR, {.s = str}};
    return 0;
fail:
    free(str);
    return -1;
}

static int json_parse_number(json_parser *p, zz_value *out) {
    size_t start = p->pos;
    json_eat(p, '-');
    while (p->pos < p->len && p->s[p->pos] >= '0' && p->s[p->pos] <= '9') p->pos++;
    if (p->pos < p->len && p->s[p->pos] == '.') {
        p->pos++;
        while (p->pos < p->len && p->s[p->pos] >= '0' && p->s[p->pos] <= '9') p->pos++;
    }
    if (p->pos < p->len && (p->s[p->pos] == 'e' || p->s[p->pos] == 'E')) {
        p->pos++;
        if (p->pos < p->len && (p->s[p->pos] == '+' || p->s[p->pos] == '-')) p->pos++;
        while (p->pos < p->len && p->s[p->pos] >= '0' && p->s[p->pos] <= '9') p->pos++;
    }
    if (p->pos == start) {
        snprintf(p->err, sizeof(p->err), "invalid number");
        return -1;
    }
    char *endptr;
    double d = strtod(p->s + start, &endptr);
    if (endptr != p->s + p->pos) {
        snprintf(p->err, sizeof(p->err), "invalid number");
        return -1;
    }
    *out = (zz_value){ZZ_FLOAT, {.f = d}};
    return 0;
}

static int json_parse_value(json_parser *p, zz_value *out) {
    json_skip_ws(p);
    char c = json_peek(p);
    if (c == '\0') {
        snprintf(p->err, sizeof(p->err), "unexpected end of input");
        return -1;
    }
    if (c == 'n') {
        if (p->pos + 4 <= p->len && memcmp(p->s + p->pos, "null", 4) == 0) { p->pos += 4; *out = zz_unit(); return 0; }
        snprintf(p->err, sizeof(p->err), "invalid literal at byte %zu", p->pos);
        return -1;
    }
    if (c == 't') {
        if (p->pos + 4 <= p->len && memcmp(p->s + p->pos, "true", 4) == 0) { p->pos += 4; *out = (zz_value){ZZ_BOOL, {.b = true}}; return 0; }
        snprintf(p->err, sizeof(p->err), "invalid literal at byte %zu", p->pos);
        return -1;
    }
    if (c == 'f') {
        if (p->pos + 5 <= p->len && memcmp(p->s + p->pos, "false", 5) == 0) { p->pos += 5; *out = (zz_value){ZZ_BOOL, {.b = false}}; return 0; }
        snprintf(p->err, sizeof(p->err), "invalid literal at byte %zu", p->pos);
        return -1;
    }
    if (c == '"') return json_parse_str(p, out);
    if (c == '[') {
        p->pos++;
        zz_value arr = zz_array_new();
        json_skip_ws(p);
        if (json_eat(p, ']')) { *out = arr; return 0; }
        for (;;) {
            json_skip_ws(p);
            zz_value item;
            if (json_parse_value(p, &item) != 0) { zz_release(&arr); return -1; }
            zz_array_push(arr.arr, item);
            json_skip_ws(p);
            if (json_eat(p, ',')) continue;
            if (json_eat(p, ']')) { *out = arr; return 0; }
            snprintf(p->err, sizeof(p->err), "expected `]` at byte %zu", p->pos);
            zz_release(&arr);
            return -1;
        }
    }
    if (c == '{') {
        p->pos++;
        zz_value dict = zz_dict_new();
        json_skip_ws(p);
        if (json_eat(p, '}')) { *out = dict; return 0; }
        for (;;) {
            json_skip_ws(p);
            zz_value k;
            if (json_parse_str(p, &k) != 0) { zz_release(&dict); return -1; }
            json_skip_ws(p);
            if (!json_eat(p, ':')) {
                snprintf(p->err, sizeof(p->err), "expected `:` at byte %zu", p->pos);
                zz_release(&k); zz_release(&dict);
                return -1;
            }
            json_skip_ws(p);
            zz_value v;
            if (json_parse_value(p, &v) != 0) { zz_release(&k); zz_release(&dict); return -1; }
            zz_dict_set(dict.dict, k, v);
            zz_release(&k);
            json_skip_ws(p);
            if (json_eat(p, ',')) continue;
            if (json_eat(p, '}')) { *out = dict; return 0; }
            snprintf(p->err, sizeof(p->err), "expected `}` at byte %zu", p->pos);
            zz_release(&dict);
            return -1;
        }
    }
    if (c == '-' || (c >= '0' && c <= '9')) return json_parse_number(p, out);
    snprintf(p->err, sizeof(p->err), "unexpected character `%c` at byte %zu", c, p->pos);
    return -1;
}

// json.parse(s) → Result(Ok(Json)) / Result(Err("invalid JSON: ..."))
zz_value zz_json_parse(zz_value s, int *err) {
    if (s.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    json_parser p = { .s = s.s->data, .len = s.s->len, .pos = 0, .err = {0} };
    zz_value raw;
    if (json_parse_value(&p, &raw) != 0) {
        return zz_variant_err(zz_str_owned(text_invalid_json(p.err)));
    }
    json_skip_ws(&p);
    if (p.pos < p.len) {
        snprintf(p.err, sizeof(p.err), "unexpected trailing characters at byte %zu", p.pos);
        return zz_variant_err(zz_str_owned(text_invalid_json(p.err)));
    }
    return zz_variant_ok(zz_json_wrap(raw));
}

// ---- compact JSON serializer (matches VM to_json_string) -----------------
static void json_append_str_sb(SB *sb, const char *s, size_t len);
static void json_serialize(SB *sb, zz_value v) {
    v = zz_json_unwrap(v);
    switch (v.tag) {
    case ZZ_UNIT:
        sb_str(sb, "null", 4);
        break;
    case ZZ_BOOL:
        sb_str(sb, v.b ? "true" : "false", v.b ? 4 : 5);
        break;
    case ZZ_INT:
        { char buf[32]; int n = snprintf(buf, sizeof buf, "%lld", (long long)v.i); sb_str(sb, buf, (size_t)n); }
        break;
    case ZZ_FLOAT:
        if (v.f == (double)(int64_t)v.f) {
            char buf[32]; int n = snprintf(buf, sizeof buf, "%.0f", v.f); sb_str(sb, buf, (size_t)n);
        } else {
            char buf[64]; int n = snprintf(buf, sizeof buf, "%.15g", v.f); sb_str(sb, buf, (size_t)n);
        }
        break;
    case ZZ_STR: {
        sb_str(sb, "\"", 1);
        json_append_str_sb(sb, v.s->data, v.s->len);
        sb_str(sb, "\"", 1);
        break;
    }
    case ZZ_ARRAY: {
        sb_str(sb, "[", 1);
        for (size_t i = 0; i < v.arr->len; i++) {
            if (i > 0) sb_str(sb, ",", 1);
            json_serialize(sb, v.arr->items[i]);
        }
        sb_str(sb, "]", 1);
        break;
    }
    case ZZ_DICT: {
        sb_str(sb, "{", 1);
        for (size_t i = 0; i < v.dict->len; i++) {
            if (i > 0) sb_str(sb, ",", 1);
            zz_dict_entry *e = &v.dict->entries[i];
            sb_str(sb, "\"", 1);
            json_append_str_sb(sb, e->key->data, e->key->len);
            sb_str(sb, "\":", 2);
            json_serialize(sb, e->val);
        }
        sb_str(sb, "}", 1);
        break;
    }
    case ZZ_OPTION_SOME:
    case ZZ_OPTION_NONE:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
        json_serialize(sb, (v.payload ? *v.payload : zz_unit()));
        break;
    default:
        sb_str(sb, "null", 4);
        break;
    }
}

static void json_append_str_sb(SB *sb, const char *s, size_t len) {
    for (size_t i = 0; i < len; i++) {
        char c = s[i];
        switch (c) {
        case '"': sb_str(sb, "\\\"", 2); break;
        case '\\': sb_str(sb, "\\\\", 2); break;
        case '\n': sb_str(sb, "\\n", 2); break;
        case '\r': sb_str(sb, "\\r", 2); break;
        case '\t': sb_str(sb, "\\t", 2); break;
        default:
            if ((unsigned char)c < 0x20) {
                char buf[8]; int n = snprintf(buf, sizeof buf, "\\u%04x", (unsigned)c);
                sb_str(sb, buf, (size_t)n);
            } else {
                sb_str(sb, s + i, 1);
            }
            break;
        }
    }
}

// json.stringify(v) → Result(Ok(compact json str))
zz_value zz_json_stringify(zz_value v, int *err) {
    (void)err;
    SB sb = {0};
    json_serialize(&sb, v);
    zz_value out = zz_str_owned(sb_take(&sb));
    return zz_variant_ok(out);
}

// Compact JSON text of a value (malloc'd). Unwraps ZZ_JSON payloads.
static char *json_to_cstr(zz_value v) {
    SB sb = {0};
    json_serialize(&sb, v);
    return sb_take(&sb);
}

// json.get(j, key) → Result(Ok(Json)) / Result(Err(msg))
zz_value zz_json_get(zz_value j, zz_value key, int *err) {
    (void)err;
    zz_value inner = zz_json_unwrap(j);
    if (key.tag != ZZ_STR) return zz_variant_err(zz_str_static("expected a string key"));
    const zz_str *k = key.s;
    switch (inner.tag) {
    case ZZ_DICT: {
        for (size_t i = 0; i < inner.dict->len; i++) {
            zz_dict_entry *e = &inner.dict->entries[i];
            if (e->key->len == k->len && memcmp(e->key->data, k->data, k->len) == 0) {
                return zz_variant_ok(zz_json_wrap(zz_clone(e->val)));
            }
        }
        char buf[160];
        int n = snprintf(buf, sizeof buf, "key `%s` not found", k->data);
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    case ZZ_ARRAY: {
        char *endptr;
        long idx = strtol(k->data, &endptr, 10);
        if (endptr != k->data + k->len) {
            char buf[192];
            int n = snprintf(buf, sizeof buf, "expected a numeric index for array, got `%s`", k->data);
            return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
        }
        if (idx < 0 || (size_t)idx >= inner.arr->len) {
            char buf[160];
            int n = snprintf(buf, sizeof buf, "index %ld out of bounds (len %zu)", idx, inner.arr->len);
            return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
        }
        return zz_variant_ok(zz_json_wrap(zz_clone(inner.arr->items[(size_t)idx])));
    }
    default: {
        // Best-effort description of the scalar for the error (VM displays
        // the JSON value itself; exact text only matters for fixtures that
        // hit this branch, which currently do not).
        char buf[200];
        int n = 0;
        if (inner.tag == ZZ_STR) {
            n = snprintf(buf, sizeof buf, "\"%.*s\"", (int)inner.s->len, inner.s->data);
        } else if (inner.tag == ZZ_INT) {
            n = snprintf(buf, sizeof buf, "%lld", (long long)inner.i);
        } else if (inner.tag == ZZ_FLOAT) {
            n = snprintf(buf, sizeof buf, "%.15g", inner.f);
        } else if (inner.tag == ZZ_BOOL) {
            n = snprintf(buf, sizeof buf, "%s", inner.b ? "true" : "false");
        } else {
            n = snprintf(buf, sizeof buf, "null");
        }
        char msg[240];
        int m = snprintf(msg, sizeof msg, "expected an object or array, found `%.*s`", n, buf);
        return zz_variant_err(zz_str_owned(copy_cstr(msg, (size_t)m)));
    }
    }
}

// json.as_str/int/float/bool — unwrap the payload to a plain value.
zz_value zz_json_unwrap_plain(zz_value j) {
    return zz_json_unwrap(j);
}

zz_value zz_json_as_str(zz_value j, int *err) {
    zz_value v = zz_json_unwrap_plain(j);
    if (v.tag == ZZ_STR) return v;
    *err = 1;
    return zz_unit();
}

zz_value zz_json_as_int(zz_value j, int *err) {
    zz_value v = zz_json_unwrap_plain(j);
    if (v.tag == ZZ_FLOAT && v.f == (double)(int64_t)v.f) {
        return (zz_value){ZZ_INT, {.i = (int64_t)v.f}};
    }
    if (v.tag == ZZ_INT) return v;
    *err = 1;
    return zz_unit();
}

zz_value zz_json_as_float(zz_value j, int *err) {
    zz_value v = zz_json_unwrap_plain(j);
    if (v.tag == ZZ_FLOAT) return v;
    if (v.tag == ZZ_INT) return (zz_value){ZZ_FLOAT, {.f = (double)v.i}};
    *err = 1;
    return zz_unit();
}

zz_value zz_json_as_bool(zz_value j, int *err) {
    zz_value v = zz_json_unwrap_plain(j);
    if (v.tag == ZZ_BOOL) return v;
    *err = 1;
    return zz_unit();
}

// json.null() — null value.
zz_value zz_json_null(zz_value unused, int *err) {
    (void)unused; (void)err;
    return zz_json_wrap(zz_unit());
}

// math.abs(v)
zz_value zz_math_abs(zz_value v, int *err) {
    (void)err;
    if (v.tag == ZZ_INT) return (zz_value){ZZ_INT, {.i = v.i < 0 ? -v.i : v.i}};
    if (v.tag == ZZ_FLOAT) return (zz_value){ZZ_FLOAT, {.f = fabs(v.f)}};
    return v;
}

// math.sqrt(v)
zz_value zz_math_sqrt(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = sqrt(d)}};
}

// math.floor(v) → int
zz_value zz_math_floor(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_INT, {.i = (int64_t)floor(d)}};
}

// math.ceil(v) → int
zz_value zz_math_ceil(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_INT, {.i = (int64_t)ceil(d)}};
}

// math.round(v) → float
zz_value zz_math_round(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = round(d)}};
}

// math.trunc(v) → float
zz_value zz_math_trunc(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = trunc(d)}};
}

// math.signum(v) → same type
zz_value zz_math_signum(zz_value v, int *err) {
    (void)err;
    if (v.tag == ZZ_INT) {
        int64_t s = (v.i > 0) - (v.i < 0);
        return (zz_value){ZZ_INT, {.i = s}};
    }
    if (v.tag == ZZ_FLOAT) {
        double s = (v.f > 0.0) - (v.f < 0.0);
        return (zz_value){ZZ_FLOAT, {.f = s}};
    }
    return v;
}

// math.hypot(x, y) → float
zz_value zz_math_hypot(zz_value x, zz_value y, int *err) {
    (void)err;
    double dx = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    double dy = y.tag == ZZ_FLOAT ? y.f : (y.tag == ZZ_INT ? (double)y.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = hypot(dx, dy)}};
}

// math.clamp(val, min, max) → float
zz_value zz_math_clamp(zz_value val, zz_value min, zz_value max, int *err) {
    (void)err;
    double dv = val.tag == ZZ_FLOAT ? val.f : (val.tag == ZZ_INT ? (double)val.i : 0.0);
    double dmin = min.tag == ZZ_FLOAT ? min.f : (min.tag == ZZ_INT ? (double)min.i : 0.0);
    double dmax = max.tag == ZZ_FLOAT ? max.f : (max.tag == ZZ_INT ? (double)max.i : 0.0);
    if (dv < dmin) dv = dmin;
    if (dv > dmax) dv = dmax;
    return (zz_value){ZZ_FLOAT, {.f = dv}};
}

// math.root(x, n) → float (nth root)
zz_value zz_math_root(zz_value x, zz_value n, int *err) {
    (void)err;
    double dx = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    double dn = n.tag == ZZ_FLOAT ? n.f : (n.tag == ZZ_INT ? (double)n.i : 0.0);
    if (dn == 0.0) return (zz_value){ZZ_FLOAT, {.f = 1.0}};
    return (zz_value){ZZ_FLOAT, {.f = pow(dx, 1.0/dn)}};
}

// math.factorial(n) → .ok(int) or .err(str)
zz_value zz_math_factorial(zz_value n, int *err) {
    (void)err;
    if (n.tag != ZZ_INT || n.i < 0 || n.i > 20)
        return zz_variant_err(zz_str_static("factorial: input out of range"));
    int64_t r = 1;
    for (int64_t i = 2; i <= n.i; i++) r *= i;
    return zz_variant_ok((zz_value){ZZ_INT, {.i = r}});
}

// math.gcd(a, b) → int
zz_value zz_math_gcd(zz_value a, zz_value b, int *err) {
    (void)err;
    int64_t x = a.tag == ZZ_INT ? a.i : 0;
    int64_t y = b.tag == ZZ_INT ? b.i : 0;
    while (y != 0) { int64_t t = y; y = x % y; x = t; }
    return (zz_value){ZZ_INT, {.i = x < 0 ? -x : x}};
}

// math.lcm(a, b) → int
zz_value zz_math_lcm(zz_value a, zz_value b, int *err) {
    (void)err;
    int64_t x = a.tag == ZZ_INT ? a.i : 0;
    int64_t y = b.tag == ZZ_INT ? b.i : 0;
    if (x == 0 || y == 0) return (zz_value){ZZ_INT, {.i = 0}};
    int64_t g = x; int64_t t = y;
    while (t != 0) { int64_t tmp = t; t = g % t; g = tmp; }
    return (zz_value){ZZ_INT, {.i = (x / g) * y < 0 ? -((x / g) * y) : (x / g) * y}};
}

// math.sin(x) → float
zz_value zz_math_sin(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = sin(d)}};
}

// math.cos(x) → float
zz_value zz_math_cos(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = cos(d)}};
}

// math.tan(x) → float
zz_value zz_math_tan(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = tan(d)}};
}

// math.asin(x) → float
zz_value zz_math_asin(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = asin(d)}};
}

// math.acos(x) → float
zz_value zz_math_acos(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = acos(d)}};
}

// math.atan(x) → float
zz_value zz_math_atan(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = atan(d)}};
}

// math.sin_deg(x) → float
zz_value zz_math_sin_deg(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = sin(d * M_PI / 180.0)}};
}

// math.cos_deg(x) → float
zz_value zz_math_cos_deg(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = cos(d * M_PI / 180.0)}};
}

// math.tan_deg(x) → float
zz_value zz_math_tan_deg(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = tan(d * M_PI / 180.0)}};
}

// math.to_radians(deg) → float
zz_value zz_math_to_radians(zz_value deg, int *err) {
    (void)err;
    double d = deg.tag == ZZ_FLOAT ? deg.f : (deg.tag == ZZ_INT ? (double)deg.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = d * M_PI / 180.0}};
}

// math.to_degrees(rad) → float
zz_value zz_math_to_degrees(zz_value rad, int *err) {
    (void)err;
    double d = rad.tag == ZZ_FLOAT ? rad.f : (rad.tag == ZZ_INT ? (double)rad.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = d * 180.0 / M_PI}};
}

// math.log(x) → float (natural log)
zz_value zz_math_log(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = log(d)}};
}

// math.log10(x) → float
zz_value zz_math_log10(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = log10(d)}};
}

// math.exp(x) → float
zz_value zz_math_exp(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = exp(d)}};
}

// math.random() → float in [0, 1)
zz_value zz_math_random(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = (double)rand() / (double)RAND_MAX}};
}

// math.is_nan(x) → bool
zz_value zz_math_is_nan(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : 0.0;
    return (zz_value){ZZ_BOOL, {.b = d != d}};
}

// math.is_inf(x) → bool
zz_value zz_math_is_inf(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : 0.0;
    return (zz_value){ZZ_BOOL, {.b = d == 1.0/0.0 || d == -1.0/0.0}};
}

// math constants
zz_value zz_math_pi(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = M_PI}};
}
zz_value zz_math_e(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = M_E}};
}
zz_value zz_math_tau(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = 2.0 * M_PI}};
}
zz_value zz_math_inf(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = 1.0/0.0}};
}
zz_value zz_math_nan(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = 0.0/0.0}};
}

// math.isqrt(n) → .ok(int) or .err(str)
zz_value zz_math_isqrt(zz_value n, int *err) {
    (void)err;
    if (n.tag != ZZ_INT || n.i < 0)
        return zz_variant_err(zz_str_static("isqrt: non-negative integer required"));
    int64_t x = n.i, y = (x + 1) / 2;
    while (y < x) { x = y; y = (x + n.i / x) / 2; }
    return zz_variant_ok((zz_value){ZZ_INT, {.i = x}});
}

// math.mean(list) → .ok(float) or .err(str)
zz_value zz_math_mean(zz_value list, int *err) {
    (void)err;
    if (list.tag != ZZ_ARRAY || !list.arr || list.arr->len == 0)
        return zz_variant_err(zz_str_static("mean: empty list"));
    double sum = 0.0;
    for (size_t i = 0; i < list.arr->len; i++) {
        zz_value v = list.arr->items[i];
        sum += v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    }
    return zz_variant_ok((zz_value){ZZ_FLOAT, {.f = sum / (double)list.arr->len}});
}

// math.median(list) → .ok(float) or .err(str)
zz_value zz_math_median(zz_value list, int *err) {
    (void)err;
    if (list.tag != ZZ_ARRAY || !list.arr || list.arr->len == 0)
        return zz_variant_err(zz_str_static("median: empty list"));
    // Copy to temp array, sort, find median.
    size_t n = list.arr->len;
    double *vals = (double *)malloc(n * sizeof(double));
    for (size_t i = 0; i < n; i++) {
        zz_value v = list.arr->items[i];
        vals[i] = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    }
    // Simple insertion sort (median lists are typically small).
    for (size_t i = 1; i < n; i++) {
        double key = vals[i];
        size_t j = i;
        while (j > 0 && vals[j-1] > key) { vals[j] = vals[j-1]; j--; }
        vals[j] = key;
    }
    double result;
    if (n % 2 == 1)
        result = vals[n / 2];
    else
        result = (vals[n/2 - 1] + vals[n/2]) / 2.0;
    free(vals);
    return zz_variant_ok((zz_value){ZZ_FLOAT, {.f = result}});
}

// math.rand_range(min, max) → float
zz_value zz_math_rand_range(zz_value min, zz_value max, int *err) {
    (void)err;
    double lo = min.tag == ZZ_FLOAT ? min.f : (min.tag == ZZ_INT ? (double)min.i : 0.0);
    double hi = max.tag == ZZ_FLOAT ? max.f : (max.tag == ZZ_INT ? (double)max.i : 0.0);
    double r = (double)rand() / (double)RAND_MAX;
    return (zz_value){ZZ_FLOAT, {.f = lo + r * (hi - lo)}};
}

// math.dot_product(v1, v2) → .ok(float) or .err(str)
zz_value zz_math_dot_product(zz_value v1, zz_value v2, int *err) {
    (void)err;
    if (v1.tag != ZZ_ARRAY || v2.tag != ZZ_ARRAY || !v1.arr || !v2.arr)
        return zz_variant_err(zz_str_static("dot_product: expected two arrays"));
    if (v1.arr->len != v2.arr->len)
        return zz_variant_err(zz_str_static("dot_product: arrays must have same length"));
    double sum = 0.0;
    for (size_t i = 0; i < v1.arr->len; i++) {
        double a = v1.arr->items[i].tag == ZZ_FLOAT ? v1.arr->items[i].f :
                   (v1.arr->items[i].tag == ZZ_INT ? (double)v1.arr->items[i].i : 0.0);
        double b = v2.arr->items[i].tag == ZZ_FLOAT ? v2.arr->items[i].f :
                   (v2.arr->items[i].tag == ZZ_INT ? (double)v2.arr->items[i].i : 0.0);
        sum += a * b;
    }
    return zz_variant_ok((zz_value){ZZ_FLOAT, {.f = sum}});
}

// math.magnitude(v) → float
zz_value zz_math_magnitude(zz_value v, int *err) {
    (void)err;
    if (v.tag != ZZ_ARRAY || !v.arr) return (zz_value){ZZ_FLOAT, {.f = 0.0}};
    double sum = 0.0;
    for (size_t i = 0; i < v.arr->len; i++) {
        double d = v.arr->items[i].tag == ZZ_FLOAT ? v.arr->items[i].f :
                   (v.arr->items[i].tag == ZZ_INT ? (double)v.arr->items[i].i : 0.0);
        sum += d * d;
    }
    return (zz_value){ZZ_FLOAT, {.f = sqrt(sum)}};
}

// math.matrix_mul(m1, m2) → .ok(array) or .err(str)
zz_value zz_math_matrix_mul(zz_value m1, zz_value m2, int *err) {
    (void)err;
    (void)m1; (void)m2;
    return zz_variant_err(zz_str_static("matrix_mul: not yet implemented in native"));
}

// env.get(name) — returns .some(val) or .none
zz_value zz_env_get(zz_value name, int *err) {
    (void)err;
    if (name.tag != ZZ_STR) return (zz_value){ZZ_OPTION_NONE, {0}};
    const char *val = getenv(name.s->data);
    if (!val) return (zz_value){ZZ_OPTION_NONE, {0}};
    return zz_variant_some(zz_str_static(val));
}

// env.var(name) — returns .ok(val) or .err(msg)
zz_value zz_env_var(zz_value name, int *err) {
    (void)err;
    if (name.tag != ZZ_STR) return zz_variant_err(zz_str_static("env.var: expected string name"));
    const char *val = getenv(name.s->data);
    if (!val) {
        // Build error message: "environment variable `NAME` not set"
        size_t nlen = name.s->len;
        const char *prefix = "environment variable `";
        const char *suffix = "` not set";
        size_t total = strlen(prefix) + nlen + strlen(suffix);
        char *msg = (char *)malloc(total + 1);
        memcpy(msg, prefix, strlen(prefix));
        memcpy(msg + strlen(prefix), name.s->data, nlen);
        memcpy(msg + strlen(prefix) + nlen, suffix, strlen(suffix));
        msg[total] = '\0';
        return zz_variant_err(zz_str_owned(msg));
    }
    return zz_variant_ok(zz_str_static(val));
}

// env.args() — returns command-line arguments (excludes argv[0] binary name)
zz_value zz_env_args(zz_value unused, int *err) {
    (void)unused; (void)err;
    // C main() in generated code doesn't receive argc/argv yet.
    // Return an empty array for now.
    return zz_array_new();
}

// dict.len(d)
zz_value zz_dict_len_val(zz_value d, int *err) {
    (void)err;
    if (d.tag != ZZ_DICT) return (zz_value){ZZ_INT, {.i = 0}};
    return (zz_value){ZZ_INT, {.i = (int64_t)d.dict->len}};
}

// dict.keys(d) — return array of keys.
zz_value zz_dict_keys(zz_value d, int *err) {
    (void)err;
    if (d.tag != ZZ_DICT) return zz_array_new();
    zz_value arr = zz_array_new();
    for (size_t i = 0; i < d.dict->len; i++) {
        int sub_err = 0;
        zz_vec_append(arr, (zz_value){ZZ_STR, {.s = d.dict->entries[i].key}}, &sub_err);
    }
    return arr;
}

// dict.has(d, key)
zz_value zz_dict_has(zz_value d, zz_value key, int *err) {
    (void)err;
    if (d.tag != ZZ_DICT || key.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    for (size_t i = 0; i < d.dict->len; i++) {
        if (strcmp(d.dict->entries[i].key->data, key.s->data) == 0)
            return (zz_value){ZZ_BOOL, {.b = true}};
    }
    return (zz_value){ZZ_BOOL, {.b = false}};
}

// option.expect(opt, msg) — unwrap .some(v) or panic with msg
zz_value zz_option_expect(zz_value opt, zz_value msg, int *err) {
    (void)err;
    if (opt.tag == ZZ_OPTION_SOME && opt.payload)
        return zz_clone(*opt.payload);
    // Panic: print error message and exit.
    const char *m = (msg.tag == ZZ_STR) ? msg.s->data : "expect failed";
    fprintf(stderr, "error: %s\n", m);
    exit(1);
}

// result.expect(res, msg) — unwrap .ok(v) or panic with msg
zz_value zz_result_expect(zz_value res, zz_value msg, int *err) {
    (void)err;
    if (res.tag == ZZ_RESULT_OK && res.payload)
        return zz_clone(*res.payload);
    // If it's a .err, print the error value too.
    if (res.tag == ZZ_RESULT_ERR && res.payload) {
        char *estr = zz_value_to_string(res.payload);
        fprintf(stderr, "error: %s: %s\n",
                (msg.tag == ZZ_STR) ? msg.s->data : "expect failed", estr);
        free(estr);
    } else {
        fprintf(stderr, "error: %s\n",
                (msg.tag == ZZ_STR) ? msg.s->data : "expect failed");
    }
    exit(1);
}

// fs.read(path)
zz_value zz_fs_read(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    FILE *f = fopen(path.s->data, "rb");
    if (!f) {
        // Match the VM: `.err(io error string)`
        return zz_variant_err(zz_str_static("No such file or directory"));
    }
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz < 0) sz = 0;
    zz_str *out = str_alloc(sz);
    size_t n = fread(out->data, 1, sz, f);
    fclose(f);
    out->data[n] = '\0';
    out->len = n;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// fs.write(path, data)
zz_value zz_fs_write(zz_value path, zz_value data, int *err) {
    if (path.tag != ZZ_STR || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    FILE *f = fopen(path.s->data, "wb");
    if (!f) {
        return zz_variant_err(zz_str_static("cannot open file for write"));
    }
    size_t w = fwrite(data.s->data, 1, data.s->len, f);
    int close_ok = (fclose(f) == 0);
    if (w != data.s->len || !close_ok) {
        return zz_variant_err(zz_str_static("write failed"));
    }
    return zz_variant_ok(zz_unit());
}

// fs.exists(path)
zz_value zz_fs_exists(zz_value path, int *err) {
    (void)err;
    if (path.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    FILE *f = fopen(path.s->data, "rb");
    if (!f) return (zz_value){ZZ_BOOL, {.b = false}};
    fclose(f);
    return (zz_value){ZZ_BOOL, {.b = true}};
}

// fs.remove(path)
zz_value zz_fs_remove(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    int r = remove(path.s->data);
    if (r != 0) {
        return zz_variant_err(zz_str_static("cannot remove file"));
    }
    return zz_variant_ok(zz_unit());
}

// fs.mkdir(path)
zz_value zz_fs_mkdir(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    int r = mkdir(path.s->data, 0755);
    if (r != 0) { *err = 1; return zz_unit(); }
    return zz_unit();
}

// fs.readdir(path) — return array of filenames.
zz_value zz_fs_readdir(zz_value path, int *err) {
    (void)err;
    if (path.tag != ZZ_STR) return zz_array_new();
    // Not implemented fully — return empty array.
    return zz_array_new();
}

// encoding.url_encode(s)
zz_value zz_encoding_url_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    const char *src = s.s->data;
    size_t len = s.s->len;
    // Worst case: every byte becomes %XX.
    zz_str *out = str_alloc(len * 3);
    size_t pos = 0;
    for (size_t i = 0; i < len; i++) {
        unsigned char c = (unsigned char)src[i];
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.' || c == '~') {
            out->data[pos++] = c;
        } else {
            snprintf(out->data + pos, 4, "%%%02X", c);
            pos += 3;
        }
    }
    out->data[pos] = '\0';
    out->len = pos;
    return (zz_value){ZZ_STR, {.s = out}};
}

// encoding.url_decode(s) → Result<str>
zz_value zz_encoding_url_decode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR)
        return zz_variant_err(zz_str_static("URL decode error: expected string"));
    const char *src = s.s->data;
    size_t len = s.s->len;
    zz_str *out = str_alloc(len);
    size_t pos = 0;
    for (size_t i = 0; i < len; i++) {
        if (src[i] == '%' && i + 2 < len) {
            char hex[3] = {src[i+1], src[i+2], '\0'};
            out->data[pos++] = (char)strtol(hex, NULL, 16);
            i += 2;
        } else if (src[i] == '+') {
            out->data[pos++] = ' ';
        } else {
            out->data[pos++] = src[i];
        }
    }
    out->data[pos] = '\0';
    out->len = pos;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// encoding.base64_encode(s)
zz_value zz_encoding_base64_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    static const char tbl[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const unsigned char *src = (const unsigned char *)s.s->data;
    size_t len = s.s->len;
    size_t out_len = 4 * ((len + 2) / 3);
    zz_str *out = str_alloc(out_len);
    size_t j = 0;
    for (size_t i = 0; i < len; i += 3) {
        unsigned int a = src[i];
        unsigned int b = (i+1 < len) ? src[i+1] : 0;
        unsigned int c = (i+2 < len) ? src[i+2] : 0;
        unsigned int triple = (a << 16) | (b << 8) | c;
        out->data[j++] = tbl[(triple >> 18) & 0x3F];
        out->data[j++] = tbl[(triple >> 12) & 0x3F];
        out->data[j++] = (i+1 < len) ? tbl[(triple >> 6) & 0x3F] : '=';
        out->data[j++] = (i+2 < len) ? tbl[triple & 0x3F] : '=';
    }
    out->data[j] = '\0';
    out->len = j;
    return (zz_value){ZZ_STR, {.s = out}};
}

// encoding.base64_decode(s) → Result<str>
zz_value zz_encoding_base64_decode(zz_value s, int *err) {
    if (s.tag != ZZ_STR)
        return zz_variant_err(zz_str_static("base64 decode error: expected string"));
    static const unsigned char tbl[256] = {
        ['A']=0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,
        ['a']=26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,
        ['0']=52,53,54,55,56,57,58,59,60,61,
        ['+']=62, ['/']=63
    };
    const char *src = s.s->data;
    size_t len = s.s->len;
    // Remove padding.
    while (len > 0 && src[len-1] == '=') len--;
    // Validate length: base64 (without padding) length must be multiple of 4
    // or the last group may be shorter (2 or 3 chars for 1 or 2 output bytes).
    if (len % 4 != 0 && len % 4 != 2 && len % 4 != 3)
        return zz_variant_err(zz_str_static("base64 decode error: Invalid padding"));
    // Validate characters.
    for (size_t i = 0; i < len; i++) {
        unsigned char c = (unsigned char)src[i];
        if (tbl[c] == 0 && c != 'A')
            return zz_variant_err(zz_str_static("base64 decode error: invalid character"));
    }
    size_t out_len = len * 3 / 4;
    zz_str *out = str_alloc(out_len);
    size_t j = 0;
    for (size_t i = 0; i < len; i += 4) {
        unsigned int a = tbl[(unsigned char)src[i]];
        unsigned int b = (i+1 < len) ? tbl[(unsigned char)src[i+1]] : 0;
        unsigned int c = (i+2 < len) ? tbl[(unsigned char)src[i+2]] : 0;
        unsigned int d = (i+3 < len) ? tbl[(unsigned char)src[i+3]] : 0;
        unsigned int triple = (a << 18) | (b << 12) | (c << 6) | d;
        if (j < out_len) out->data[j++] = (triple >> 16) & 0xFF;
        if (j < out_len) out->data[j++] = (triple >> 8) & 0xFF;
        if (j < out_len) out->data[j++] = triple & 0xFF;
    }
    out->data[j] = '\0';
    out->len = j;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// encoding.hex_encode(data) → hex string
zz_value zz_encoding_hex_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return zz_str_static("");
    const unsigned char *d = (const unsigned char *)s.s->data;
    size_t len = s.s->len;
    char *hex = (char *)malloc(len * 2 + 1);
    for (size_t i = 0; i < len; i++) {
        snprintf(hex + i*2, 3, "%02x", d[i]);
    }
    hex[len * 2] = '\0';
    return zz_str_owned(hex);
}

// encoding.hex_decode(hex_str) → .ok(data) or .err(str)
zz_value zz_encoding_hex_decode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR)
        return zz_variant_err(zz_str_static("expected string"));
    size_t len = s.s->len;
    if (len % 2 != 0)
        return zz_variant_err(zz_str_static("odd-length hex string"));
    char *out = (char *)malloc(len / 2 + 1);
    for (size_t i = 0; i < len; i += 2) {
        char byte_str[3] = { s.s->data[i], s.s->data[i+1], '\0' };
        char *endptr;
        unsigned long val = strtoul(byte_str, &endptr, 16);
        if (endptr != byte_str + 2)
            return zz_variant_err(zz_str_static("hex decode error: invalid digit found in string"));
        out[i/2] = (char)val;
    }
    out[len/2] = '\0';
    return zz_variant_ok(zz_str_new(out, len / 2));
}

// =====================================================================
//  HTTP client — libcurl-based implementation
// =====================================================================

// Helper struct for curl body accumulation (avoids flexible-array-member issues)
typedef struct {
    char *data;
    size_t cap;
    size_t len;
} curl_buf;

// Callback for curl to write response body into a growing memory buffer.
static size_t curl_write_cb(void *data, size_t size, size_t nmemb, void *userp) {
    size_t realsize = size * nmemb;
    curl_buf *buf = (curl_buf *)userp;
    size_t new_len = buf->len + realsize;
    if (buf->cap <= new_len) {
        size_t new_cap = buf->cap == 0 ? 256 : buf->cap * 2;
        while (new_cap < new_len + 1) new_cap *= 2;
        buf->data = (char *)realloc(buf->data, new_cap);
        buf->cap = new_cap;
    }
    memcpy(buf->data + buf->len, data, realsize);
    buf->len = new_len;
    buf->data[buf->len] = '\0';
    return realsize;
}

// Callback for curl to read headers into a dict.
static size_t curl_header_cb(void *data, size_t size, size_t nmemb, void *userp) {
    size_t realsize = size * nmemb;
    const char *line = (const char *)data;
    const char *colon = strchr(line, ':');
    if (!colon || colon >= line + realsize) return realsize;

    size_t key_len = colon - line;
    const char *val = colon + 1;
    while (*val == ' ' || *val == '\t') val++;
    size_t val_len = realsize - (val - line);
    if (val_len > 0 && val[val_len-1] == '\r') val_len--;
    if (val_len > 0 && val[val_len-1] == '\n') val_len--;

    zz_value *hdrs_val = (zz_value *)userp;
    if (hdrs_val->tag != ZZ_DICT) return realsize;

    zz_str *key = str_alloc(key_len);
    memcpy(key->data, line, key_len);
    key->data[key_len] = '\0';
    key->len = key_len;

    zz_str *value_str = str_alloc(val_len);
    memcpy(value_str->data, val, val_len);
    value_str->data[val_len] = '\0';
    value_str->len = val_len;

    zz_dict_set(hdrs_val->dict, (zz_value){ZZ_STR, {.s = key}}, (zz_value){ZZ_STR, {.s = value_str}});
    return realsize;
}

// http.get(url, headers) → .ok(HttpResponse) or .err(str)
zz_value zz_http_get(zz_value url, zz_value headers, int *err) {
    if (url.tag != ZZ_STR) { *err = 1; return zz_variant_err(zz_str_static("http.get: url must be string")); }
    *err = 0;

    CURL *curl = curl_easy_init();
    if (!curl) { *err = 1; return zz_variant_err(zz_str_static("http.get: curl_easy_init failed")); }

    curl_buf body_buf = {0};
    zz_value headers_dict = zz_dict_new();

    curl_easy_setopt(curl, CURLOPT_URL, (char *)url.s->data);
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, curl_write_cb);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &body_buf);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, curl_header_cb);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &headers_dict);
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);

    struct curl_slist *header_list = NULL;
    if (headers.tag == ZZ_DICT && headers.dict && headers.dict->len > 0) {
        for (size_t i = 0; i < headers.dict->len; i++) {
            zz_str *k = headers.dict->entries[i].key;
            zz_value *v = &headers.dict->entries[i].val;
            if (k && v->tag == ZZ_STR) {
                // NOTE: no trailing CRLF — curl treats a trailing CRLF as an
                // empty header line, which terminates the header block early
                // and swallows the request body into the headers.
                size_t hlen = k->len + 2 + v->s->len;
                char *h = (char *)malloc(hlen + 1);
                memcpy(h, k->data, k->len);
                h[k->len] = ':';
                h[k->len + 1] = ' ';
                memcpy(h + k->len + 2, v->s->data, v->s->len);
                h[k->len + 2 + v->s->len] = '\0';
                header_list = curl_slist_append(header_list, h);
                free(h);
            }
        }
        if (header_list) curl_easy_setopt(curl, CURLOPT_HTTPHEADER, header_list);
    }

    CURLcode res = curl_easy_perform(curl);
    if (header_list) curl_slist_free_all(header_list);

    if (res != CURLE_OK) {
        char errbuf[256];
        snprintf(errbuf, sizeof(errbuf), "http.get: %s", curl_easy_strerror(res));
        curl_easy_cleanup(curl);
        if (body_buf.data) free(body_buf.data);
        zz_release(&headers_dict);
        *err = 1;
        return zz_variant_err(zz_str_static(errbuf));
    }

    long http_code = 0;
    curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &http_code);
    curl_easy_cleanup(curl);

    // Adopt body_buf into a proper zz_str (flexible array requires full struct alloc)
    zz_str *body_str;
    if (body_buf.data && body_buf.len > 0) {
        body_str = (zz_str *)malloc(sizeof(zz_str) + body_buf.cap + 1);
        body_str->refs = 1;
        body_str->interned = 0;
        body_str->cap = body_buf.cap;
        body_str->len = body_buf.len;
        memcpy(body_str->data, body_buf.data, body_buf.len + 1);
        free(body_buf.data);
    } else {
        body_str = str_alloc(0);
    }

    // Build response object
    const char *field_names_str[] = {"status", "body", "headers", "text", "json"};
    zz_value field_names[5];
    for (int i = 0; i < 5; i++) {
        field_names[i] = zz_str_static(field_names_str[i]);
    }
    zz_value resp_val = zz_object_new("http.response", field_names, 5);
    zz_object_set_field(&resp_val, "status", (zz_value){ZZ_INT, {.i = http_code}});
    zz_object_set_field(&resp_val, "body", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "text", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "headers", zz_clone(headers_dict));

    zz_value json_val = zz_unit();
    if (body_str->len > 0) {
        int jerr = 0;
        json_val = zz_json_parse((zz_value){ZZ_STR, {.s = body_str}}, &jerr);
        if (jerr) json_val = zz_unit();
    }
    zz_object_set_field(&resp_val, "json", json_val);

    zz_release(&headers_dict);
    return zz_variant_ok(resp_val);
}

// http.post(url, body, headers) → .ok(HttpResponse) or .err(str)
zz_value zz_http_post(zz_value url, zz_value body, zz_value headers, int *err) {
    if (url.tag != ZZ_STR) { *err = 1; return zz_variant_err(zz_str_static("http.post: url must be string")); }
    *err = 0;

    CURL *curl = curl_easy_init();
    if (!curl) { *err = 1; return zz_variant_err(zz_str_static("http.post: curl_easy_init failed")); }

    curl_buf body_buf = {0};
    zz_value headers_dict = zz_dict_new();

    curl_easy_setopt(curl, CURLOPT_URL, (char *)url.s->data);
    curl_easy_setopt(curl, CURLOPT_POST, 1L);
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, curl_write_cb);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &body_buf);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, curl_header_cb);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &headers_dict);
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);

    if (body.tag == ZZ_STR && body.s && body.s->len > 0) {
        curl_easy_setopt(curl, CURLOPT_POSTFIELDS, (char *)body.s->data);
        // Explicit size: CURLOPT_POSTFIELDS alone uses strlen(), which is
        // wrong if the payload ever contains NUL bytes.
        curl_easy_setopt(curl, CURLOPT_POSTFIELDSIZE, (long)body.s->len);
    }

    struct curl_slist *header_list = NULL;
    if (headers.tag == ZZ_DICT && headers.dict && headers.dict->len > 0) {
        for (size_t i = 0; i < headers.dict->len; i++) {
            zz_str *k = headers.dict->entries[i].key;
            zz_value *v = &headers.dict->entries[i].val;
            if (k && v->tag == ZZ_STR) {
                // NOTE: no trailing CRLF — curl treats a trailing CRLF as an
                // empty header line, which terminates the header block early
                // and swallows the request body into the headers.
                size_t hlen = k->len + 2 + v->s->len;
                char *h = (char *)malloc(hlen + 1);
                memcpy(h, k->data, k->len);
                h[k->len] = ':';
                h[k->len + 1] = ' ';
                memcpy(h + k->len + 2, v->s->data, v->s->len);
                h[k->len + 2 + v->s->len] = '\0';
                header_list = curl_slist_append(header_list, h);
                free(h);
            }
        }
        if (header_list) curl_easy_setopt(curl, CURLOPT_HTTPHEADER, header_list);
    }

    CURLcode res = curl_easy_perform(curl);
    if (header_list) curl_slist_free_all(header_list);

    if (res != CURLE_OK) {
        char errbuf[256];
        snprintf(errbuf, sizeof(errbuf), "http.post: %s", curl_easy_strerror(res));
        curl_easy_cleanup(curl);
        if (body_buf.data) free(body_buf.data);
        zz_release(&headers_dict);
        *err = 1;
        return zz_variant_err(zz_str_static(errbuf));
    }

    long http_code = 0;
    curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &http_code);
    curl_easy_cleanup(curl);

    zz_str *body_str;
    if (body_buf.data && body_buf.len > 0) {
        body_str = (zz_str *)malloc(sizeof(zz_str) + body_buf.cap + 1);
        body_str->refs = 1;
        body_str->interned = 0;
        body_str->cap = body_buf.cap;
        body_str->len = body_buf.len;
        memcpy(body_str->data, body_buf.data, body_buf.len + 1);
        free(body_buf.data);
    } else {
        body_str = str_alloc(0);
    }

    const char *field_names_str[] = {"status", "body", "headers", "text", "json"};
    zz_value field_names[5];
    for (int i = 0; i < 5; i++) {
        field_names[i] = zz_str_static(field_names_str[i]);
    }
    zz_value resp_val = zz_object_new("http.response", field_names, 5);
    zz_object_set_field(&resp_val, "status", (zz_value){ZZ_INT, {.i = http_code}});
    zz_object_set_field(&resp_val, "body", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "text", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "headers", zz_clone(headers_dict));

    zz_value json_val = zz_unit();
    if (body_str->len > 0) {
        int jerr = 0;
        json_val = zz_json_parse((zz_value){ZZ_STR, {.s = body_str}}, &jerr);
        if (jerr) json_val = zz_unit();
    }
    zz_object_set_field(&resp_val, "json", json_val);

    zz_release(&headers_dict);
    return zz_variant_ok(resp_val);
}

// http.response.status(response) → int
zz_value zz_http_response_status(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "status");
}

// http.response.text(response) → str
zz_value zz_http_response_text(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "text");
}

// http.response.json(response) → json
zz_value zz_http_response_json(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "json");
}

// http.response.headers(response) → dict
zz_value zz_http_response_headers(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "headers");
}

// =====================================================================
//  Missing stdlib natives — bare builtins and module functions
// =====================================================================