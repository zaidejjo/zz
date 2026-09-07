// ZZ native runtime — implementation.
#include <math.h>
#include <time.h>
#include <sys/stat.h>
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
    if (d && d->refs != 0) __atomic_add_fetch(&d->refs, 1, __ATOMIC_RELAXED);
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
        free(d->entries);  // entries buffer always uses malloc
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
        dst->tag == ZZ_DICT || dst->tag == ZZ_FUNC || dst->tag == ZZ_OBJECT) {
        zz_release(dst);
    }
    *dst = src;
    // Retain the new value for refcounted types.
    if (src.tag == ZZ_ARRAY || src.tag == ZZ_DICT || src.tag == ZZ_FUNC) {
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
    // string concat
    if ((op == ZZOP_ADD || op == ZZOP_EQ || op == ZZOP_NE) && a.tag == ZZ_STR &&
        b.tag == ZZ_STR) {
        if (op == ZZOP_ADD) {
            zz_str *out = str_alloc(a.s->len + b.s->len);
            memcpy(out->data, a.s->data, a.s->len);
            memcpy(out->data + a.s->len, b.s->data, b.s->len);
            zz_value v;
            v.tag = ZZ_STR;
            v.s = out;
            return v;
        } else if (op == ZZOP_EQ) {
            return zz_bool(a.s->len == b.s->len &&
                           memcmp(a.s->data, b.s->data, a.s->len) == 0);
        } else {
            return zz_bool(!(a.s->len == b.s->len &&
                             memcmp(a.s->data, b.s->data, a.s->len) == 0));
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

// ---- io natives -----------------------------------------------------------
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
            fprintf(out, "%.15g", x);
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
        if (x == (int64_t)x && x < 1e15 && x > -1e15)
            snprintf(buf, sizeof buf, "%.1f", x);
        else
            snprintf(buf, sizeof buf, "%.15g", x);
        sb_append_str(sb, buf);
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

// vec.append(arr, item) — append item to array.
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

// vec.push(arr, item) — alias for append.
zz_value zz_vec_push(zz_value arr, zz_value item, int *err) {
    return zz_vec_append(arr, item, err);
}

// vec.pop(arr) — remove and return last element, or unit.
zz_value zz_vec_pop(zz_value arr, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || arr.arr->len == 0) return zz_unit();
    zz_array *a = arr.arr;
    zz_value item = a->items[a->len - 1];
    a->len--;
    return item;
}

// vec.remove(arr, idx) — remove element at index, shift left.
zz_value zz_vec_remove(zz_value arr, zz_value idx, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    int64_t i = idx.tag == ZZ_INT ? idx.i : 0;
    if (i < 0 || (size_t)i >= a->len) return zz_unit();
    zz_release(&a->items[i]);
    for (size_t j = (size_t)i; j < a->len - 1; j++) {
        a->items[j] = a->items[j + 1];
    }
    a->len--;
    return zz_unit();
}

// vec.insert(arr, idx, item) — insert item at index, shift right.
zz_value zz_vec_insert(zz_value arr, zz_value idx, zz_value item, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    int64_t i = idx.tag == ZZ_INT ? idx.i : 0;
    if (i < 0 || (size_t)i > a->len) return zz_unit();
    if (a->len >= a->cap) {
        size_t new_cap = a->cap ? a->cap * 2 : 8;
        if (a->refs == ZZ_ARRAY_STACK_MAGIC || a->refs == ZZ_ARRAY_LIT_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            for (size_t j = 0; j < a->len; j++) new_items[j] = a->items[j];
            a->items = new_items;
            a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    for (size_t j = a->len; j > (size_t)i; j--) {
        a->items[j] = a->items[j - 1];
    }
    a->items[i] = zz_clone(item);
    a->len++;
    return zz_unit();
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
        int sub_err = 0;
        zz_vec_append(sorted, zz_clone(arr.arr->items[i]), &sub_err);
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
        int sub_err = 0;
        zz_vec_append(rev, zz_clone(arr.arr->items[i-1]), &sub_err);
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

// json.parse(s) — parse JSON string to value (simplified).
zz_value zz_json_parse(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    // Minimal JSON parser: support null, bool, int, float, string, array, object.
    const char *p = s.s->data;
    const char *end = p + s.s->len;
    // Skip whitespace.
    while (p < end && (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')) p++;
    if (p >= end) { *err = 1; return zz_unit(); }
    if (*p == 'n') { return zz_unit(); } // null
    if (*p == 't') { return (zz_value){ZZ_BOOL, {.b = true}}; }
    if (*p == 'f') { return (zz_value){ZZ_BOOL, {.b = false}}; }
    if (*p == '"') {
        p++;
        const char *start = p;
        while (p < end && *p != '"') p++;
        size_t len = p - start;
        zz_str *out = str_alloc(len);
        memcpy(out->data, start, len);
        out->data[len] = '\0';
        return (zz_value){ZZ_STR, {.s = out}};
    }
    if (*p == '-' || (*p >= '0' && *p <= '9')) {
        char *fend;
        double d = strtod(s.s->data + (p - s.s->data), &fend);
        if (fend > p && *fend != '.') {
            return (zz_value){ZZ_INT, {.i = (int64_t)d}};
        }
        return (zz_value){ZZ_FLOAT, {.f = d}};
    }
    if (*p == '[') {
        p++;
        zz_value arr = zz_array_new();
        while (p < end && *p != ']') {
            while (p < end && (*p == ' ' || *p == ',' || *p == '\t')) p++;
            if (p >= end || *p == ']') break;
            // Parse sub-value: create a temporary str wrapping remaining input.
            size_t remain = end - p;
            zz_str tmp = {0};
            tmp.len = remain;
            tmp.refs = 999; // won't be freed
            tmp.interned = 1;
            // We need a mutable copy for the sub-parser.
            char *buf = (char *)malloc(remain + 1);
            memcpy(buf, p, remain);
            buf[remain] = '\0';
            zz_str *tmps = str_alloc(remain);
            memcpy(tmps->data, p, remain);
            tmps->data[remain] = '\0';
            zz_value sub = {ZZ_STR, {.s = tmps}};
            int sub_err = 0;
            zz_value item = zz_json_parse(sub, &sub_err);
            { zz_value to_release = {ZZ_STR, {.s = tmps}}; zz_release(&to_release); }
            // Advance past parsed value.
            if (item.tag == ZZ_STR) {
                // Skip: "content"
                while (p < end && *p != '"') p++;
                if (p < end) p++; // skip closing quote
            } else if (item.tag == ZZ_INT) {
                while (p < end && *p != ',' && *p != ']') p++;
            } else if (item.tag == ZZ_FLOAT) {
                while (p < end && *p != ',' && *p != ']') p++;
            } else if (item.tag == ZZ_BOOL) {
                if (p[0] == 't') p += 4; else if (p[0] == 'f') p += 5;
            } else {
                p++;
            }
            int aerr = 0;
            zz_vec_append(arr, item, &aerr);
            zz_release(&item);
            free(buf);
        }
        return arr;
    }
    if (*p == '{') {
        p++;
        zz_value dict = zz_dict_new();
        while (p < end && *p != '}') {
            while (p < end && (*p == ' ' || *p == ',' || *p == '\t')) p++;
            if (p >= end || *p == '}') break;
            // Parse key.
            if (*p != '"') break;
            p++;
            const char *key_start = p;
            while (p < end && *p != '"') p++;
            size_t klen = p - key_start;
            p++; // skip closing quote.
            while (p < end && *p != ':') p++;
            p++; // skip colon.
            while (p < end && (*p == ' ' || *p == '\t')) p++;
            // Parse value (primitive only).
            size_t remain = end - p;
            zz_str *tmps = str_alloc(remain);
            memcpy(tmps->data, p, remain);
            tmps->data[remain] = '\0';
            zz_value sub = {ZZ_STR, {.s = tmps}};
            int sub_err = 0;
            zz_value val = zz_json_parse(sub, &sub_err);
            { zz_value to_release = {ZZ_STR, {.s = tmps}}; zz_release(&to_release); }
            if (val.tag == ZZ_STR) {
                while (p < end && *p != '"') p++;
                if (p < end) p++;
            } else if (val.tag == ZZ_INT || val.tag == ZZ_FLOAT) {
                while (p < end && *p != ',' && *p != '}') p++;
            } else if (val.tag == ZZ_BOOL) {
                if (p[0] == 't') p += 4; else if (p[0] == 'f') p += 5;
            } else {
                p++;
            }
            // Insert into dict.
            zz_str *ks = str_alloc(klen);
            memcpy(ks->data, key_start, klen);
            ks->data[klen] = '\0';
            if (dict.tag == ZZ_DICT) {
                zz_dict *d = dict.dict;
                if (d->len >= d->cap) {
                    size_t nc = d->cap ? d->cap * 2 : 8;
                    d->entries = (zz_dict_entry *)realloc(d->entries, nc * sizeof(zz_dict_entry));
                    d->cap = nc;
                }
                d->entries[d->len].key = ks;
                d->entries[d->len].val = val;
                d->len++;
            } else {
                zz_release(&val);
                free(ks);
            }
        }
        return dict;
    }
    *err = 1;
    return zz_unit();
}

// json.stringify(v) — value to JSON string (simplified).
zz_value zz_json_stringify(zz_value v, int *err) {
    (void)err;
    char *s = zz_value_to_string(&v);
    return zz_str_owned(s);
}

// json.null() — null value.
zz_value zz_json_null(zz_value unused, int *err) {
    (void)unused; (void)err;
    return zz_unit();
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
    if (!f) { *err = 1; return zz_unit(); }
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    zz_str *out = str_alloc(sz);
    size_t n = fread(out->data, 1, sz, f);
    fclose(f);
    out->data[n] = '\0';
    out->len = n;
    return (zz_value){ZZ_STR, {.s = out}};
}

// fs.write(path, data)
zz_value zz_fs_write(zz_value path, zz_value data, int *err) {
    if (path.tag != ZZ_STR || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    FILE *f = fopen(path.s->data, "wb");
    if (!f) { *err = 1; return zz_unit(); }
    fwrite(data.s->data, 1, data.s->len, f);
    fclose(f);
    return zz_unit();
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
    if (r != 0) { *err = 1; return zz_unit(); }
    return zz_unit();
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

// encoding.url_decode(s)
zz_value zz_encoding_url_decode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
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
    return (zz_value){ZZ_STR, {.s = out}};
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

// encoding.base64_decode(s)
zz_value zz_encoding_base64_decode(zz_value s, int *err) {
    if (s.tag != ZZ_STR) { *err = 1; return zz_unit(); }
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
    return (zz_value){ZZ_STR, {.s = out}};
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
        return zz_variant_err(zz_str_static("hex_decode: expected string"));
    size_t len = s.s->len;
    if (len % 2 != 0)
        return zz_variant_err(zz_str_static("hex_decode: odd-length hex string"));
    char *out = (char *)malloc(len / 2 + 1);
    for (size_t i = 0; i < len; i += 2) {
        char byte_str[3] = { s.s->data[i], s.s->data[i+1], '\0' };
        unsigned long val = strtoul(byte_str, NULL, 16);
        out[i/2] = (char)val;
    }
    out[len/2] = '\0';
    return zz_variant_ok(zz_str_new(out, len / 2));
}

// =====================================================================
//  Missing stdlib natives — bare builtins and module functions
// =====================================================================