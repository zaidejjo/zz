// ZZ native runtime — core string manipulation routines.
//
// String interning, heap/arena string construction, concatenation
// shims, formatting, and the str.* natives.
#include "runtime.h"

// Display name for a boxed struct: bare type without the module
// namespace (`user.User` → `User`), matching the VM's Display.
// Identity (dispatch, conversions) keeps the qualified name;
// only printing shortens.
static const char *zz_object_display_name(const zz_object *o) {
    const char *t = (o && o->type_name) ? o->type_name : "<struct>";
    const char *dot = strrchr(t, '.');
    return dot ? dot + 1 : t;
}


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

// Spawned threads evaluate string literals too, so interning must be
// thread-safe: one mutex around lookup-or-create (first touch per literal
// is rare; steady-state hits are a single locked probe chain).
static pthread_mutex_t zz_intern_lock = PTHREAD_MUTEX_INITIALIZER;

static uint32_t fnv1a(const char *s, size_t len) {
    uint32_t h = 2166136261u;
    for (size_t i = 0; i < len; i++) {
        h ^= (unsigned char)s[i];
        h *= 16777619u;
    }
    return h;
}

static zz_str *intern_lookup_or_create(const char *src, size_t len) {
    pthread_mutex_lock(&zz_intern_lock);
    uint32_t h = fnv1a(src, len);
    uint32_t idx = h % ZZ_INTERN_BUCKETS;
    zz_str *found = NULL;
    for (uint32_t probe = 0; probe < ZZ_INTERN_BUCKETS; probe++) {
        uint32_t i = (idx + probe) % ZZ_INTERN_BUCKETS;
        zz_intern_entry *e = &zz_intern_table[i];
        if (e->src == NULL) {
            break; // absent — fall through to create below
        }
        if (e->len == len && e->src == src) {
            // Same literal pointer: guaranteed match.
            found = e->singleton;
            break;
        }
        if (e->len == len && memcmp(e->src, src, len) == 0) {
            // Same bytes, different .rodata address (e.g., the same literal
            // duplicated by the compiler or by string concatenation in C).
            found = e->singleton;
            break;
        }
    }
    if (!found) {
        // Absent (or table full — then the singleton is simply not cached):
        // build an immortal copy via str_alloc.
        for (uint32_t probe = 0; probe < ZZ_INTERN_BUCKETS; probe++) {
            uint32_t i = (idx + probe) % ZZ_INTERN_BUCKETS;
            zz_intern_entry *e = &zz_intern_table[i];
            if (e->src == NULL) {
                zz_str *s = str_alloc(len);
                s->interned = 1;
                memcpy(zz_str_ptr(s), src, len);
                zz_str_ptr(s)[len] = '\0';
                e->src = src;
                e->len = len;
                e->singleton = s;
                found = s;
                break;
            }
        }
        if (!found) {
            zz_str *s = str_alloc(len);
            s->interned = 1;
            memcpy(zz_str_ptr(s), src, len);
            zz_str_ptr(s)[len] = '\0';
            found = s;
        }
    }
    pthread_mutex_unlock(&zz_intern_lock);
    return found;
}

// ---- string helpers ----------------------------------------------------
// Fast header pool (mimalloc-lite): zz_str headers are a fixed 56 bytes
// and churn heavily in string/alloc benchmarks. A thread-local free-list
// recycles them with O(1) push/pop and zero syscalls on the hot path.
// Heap payload buffers (s->heap) still use malloc/realloc/free.
#define ZZ_STR_SLAB_MAX 256
static __thread zz_str *zz_str_slab = NULL;
static __thread size_t zz_str_slab_len = 0;

zz_str *zz_str_header_alloc(void) {
    if (zz_str_slab) {
        zz_str *s = zz_str_slab;
        zz_str_slab = *(zz_str **)s->sso;  // next pointer stashed in sso
        zz_str_slab_len--;
        return s;
    }
    return (zz_str *)malloc(sizeof(zz_str));
}

void zz_str_header_free(zz_str *s) {
    if (zz_str_slab_len < ZZ_STR_SLAB_MAX) {
        *(zz_str **)s->sso = zz_str_slab;  // stash next pointer in sso
        zz_str_slab = s;
        zz_str_slab_len++;
        return;
    }
    free(s);
}

// Allocate a string with at least `need` bytes of payload capacity.
// SSO: if need <= ZZ_SSO_MAX, store inline in s->sso — zero extra alloc.
// Heap: allocate a separate buffer, store pointer in s->heap.
zz_str *str_alloc(size_t need) {
    zz_str *s = zz_str_header_alloc();
    if (!s) {
        fprintf(stderr, "zz: out of memory\n");
        exit(1);
    }
    s->refs = 1;
    s->interned = 0;
    s->len = need;
    if (need <= ZZ_SSO_MAX) {
        s->cap = 0;   // SSO sentinel
        s->sso[need] = '\0';
    } else {
        size_t cap = need;
        if (cap < 32) cap = 32;
        s->cap = cap;
        s->heap = (char *)malloc(cap + 1);
        if (!s->heap) {
            fprintf(stderr, "zz: out of memory\n");
            exit(1);
        }
        s->heap[need] = '\0';
    }
    return s;
}

// Grow an existing string's buffer to hold at least `new_len` bytes.
// Caller must have already verified new_len > s->cap and refs==1.
// If the string is currently in SSO mode, promotes to heap.
zz_str *str_grow(zz_str *s, size_t new_len) {
    // Small growth that still fits SSO: stay inline, no heap alloc.
    if (new_len <= ZZ_SSO_MAX && s->cap == 0) {
        return s;
    }
    // 2x growth factor (matches Rust `String`): amortized O(1) appends
    // with minimal reallocs on large builds (5k–12k char strings).
    size_t nc = s->cap ? s->cap * 2 : 32;
    if (nc < new_len) nc = new_len;
    if (s->cap == 0) {
        // SSO → heap promotion: allocate fresh buffer, copy inline data.
        char *buf = (char *)malloc(nc + 1);
        if (!buf) {
            fprintf(stderr, "zz: out of memory\n");
            exit(1);
        }
        memcpy(buf, s->sso, s->len);
        buf[s->len] = '\0';
        s->cap = nc;
        s->heap = buf;
    } else {
        // Heap → heap grow: realloc the buffer.
        char *buf = (char *)realloc(s->heap, nc + 1);
        if (!buf) {
            fprintf(stderr, "zz: out of memory\n");
            exit(1);
        }
        s->cap = nc;
        s->heap = buf;
    }
    return s;
}

zz_value zz_str_new(const char *src, size_t len) {
    zz_str *s = str_alloc(len);
    memcpy(zz_str_ptr(s), src, len);
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
// `SB` typedef lives in strings.h (shared with the JSON serializer).
void sb_str(SB *sb, const char *s, size_t n) {
    if (sb->len + n + 1 > sb->cap) {
        size_t nc = sb->cap ? sb->cap * 2 : 256;
        while (nc < sb->len + n + 1) nc *= 2;
        sb->buf = (char *)realloc(sb->buf, nc);
        sb->cap = nc;
    }
    memcpy(sb->buf + sb->len, s, n);
    sb->len += n;
    sb->buf[sb->len] = '\0';
}
char *sb_take(SB *sb) {
    if (!sb->buf) { sb->buf = (char *)malloc(1); sb->buf[0] = '\0'; sb->cap = 1; }
    sb->buf[sb->len] = '\0';
    return sb->buf;
}
// malloc'd NUL-terminated copy of a (possibly embedded-NUL) buffer.
char *copy_cstr(const char *s, size_t n) {
    char *out = (char *)malloc(n + 1);
    memcpy(out, s, n);
    out[n] = '\0';
    return out;
}

// FFI bridge (see strings.h): string bytes for Rust staticlib callers.
void zz_str_view(zz_value v, const char **out_ptr, size_t *out_len) {
    if (v.tag != ZZ_STR || v.s == NULL) {
        *out_ptr = NULL;
        *out_len = 0;
        return;
    }
    *out_ptr = zz_str_cptr(v.s);
    *out_len = v.s->len;
}


zz_value zz_str_static(const char *src) {
    // Call-site literal cache (P6): the same .rodata address arrives on
    // every loop iteration, so a tiny MRU keyed on POINTER equality
    // (same address ⇒ same bytes) skips strlen + fnv1a + table probe on
    // hits. Misses fall through to the intern table. Thread-local like
    // the header slab; singletons are never freed so entries stay valid.
#define ZZ_LIT_CACHE_N 8
    static __thread const char *zz_lit_keys[ZZ_LIT_CACHE_N];
    static __thread zz_str *zz_lit_vals[ZZ_LIT_CACHE_N];
    static __thread unsigned zz_lit_victim;
    for (unsigned i = 0; i < ZZ_LIT_CACHE_N; i++) {
        if (zz_lit_keys[i] == src) {
            zz_value v;
            v.tag = ZZ_STR;
            v.s = zz_lit_vals[i];
            return v;
        }
    }
    size_t len = strlen(src);
    zz_str *s = intern_lookup_or_create(src, len);
    unsigned vic = zz_lit_victim;
    zz_lit_keys[vic] = src;
    zz_lit_vals[vic] = s;
    zz_lit_victim = (vic + 1) % ZZ_LIT_CACHE_N;
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

// Heal an arena-owned string into an independent heap-owned copy.
// See strings.h for the full contract (retaining stores must heal).
zz_value zz_str_heal_arena(zz_value v) {
    if (v.tag != ZZ_STR || !v.s || v.s->interned || v.s->refs != 0) {
        return v;
    }
    zz_str *s = str_alloc(v.s->len);
    memcpy(zz_str_ptr(s), zz_str_cptr(v.s), v.s->len);
    zz_str_ptr(s)[v.s->len] = '\0';
    zz_value out;
    out.tag = ZZ_STR;
    out.s = s;
    return out;
}

// Arena-aware string constructor. The zz_str header is bump-allocated
// when arena is non-NULL. The data payload still uses malloc (strings
// are often used with slice operations that need stable memory).
zz_value zz_str_new_arena(const char *src, size_t len, zz_arena *arena) {
    zz_str *s;
    if (arena) {
        // Arena-allocated string: header on arena, data handled per size.
        s = (zz_str *)zz_arena_alloc(arena, sizeof(zz_str), 8);
        s->refs = 0;  // sentinel: arena-allocated
        s->interned = 0;
        s->len = len;
        if (len <= ZZ_SSO_MAX) {
            // Small string: store inline in SSO buffer — zero extra alloc.
            s->cap = 0;
            memcpy(s->sso, src, len);
            s->sso[len] = '\0';
        } else {
            // Large string: allocate data on the arena too.
            s->cap = len;
            s->heap = (char *)zz_arena_alloc(arena, len + 1, 1);
            memcpy(s->heap, src, len);
            s->heap[len] = '\0';
        }
    } else {
        s = str_alloc(len);
        memcpy(zz_str_ptr(s), src, len);
    }
    zz_value v;
    v.tag = ZZ_STR;
    v.s = s;
    return v;
}

// ---- io natives -----------------------------------------------------------

/// Format a double per the canonical rule (IR spec §4): shortest
/// decimal string that round-trips, Rust `Display` semantics, never
/// exponent notation. Produced by the Rust core (`zz_native_rt`
/// `float_fmt`), never formatted in C — this TU must not contain its
/// own float printer. Weak import: programs that can never hold a
/// float-typed value link without the staticlib (the gate scans the
/// typed program for `Float`); the guard below fails closed so a missed
/// gate aborts loudly instead of diverging.
#if defined(__APPLE__)
#define ZZ_WEAK_IMPORT_FLOAT __attribute__((weak_import))
#else
#define ZZ_WEAK_IMPORT_FLOAT __attribute__((weak))
#endif
size_t zz_float_format_raw(double x, char *buf, size_t cap) ZZ_WEAK_IMPORT_FLOAT;

// Stack covers every f64 Display rendering (longest observed: 5e-324
// at 326 bytes); the heap spill below is paranoia, never hot.
#define ZZ_FLOAT_STACK 1024

// Render `x` canonically: `*out_len` bytes at the returned pointer
// (`stack`, or malloc'd `*heap` when the core reports more than fits).
// Callers `free(*heap)` (`free(NULL)` is a no-op).
static const char *zz_canonical_double(double x, char *stack, char **heap, size_t *out_len) {
    if (!zz_float_format_raw) {
        fprintf(stderr,
                "zz error: float formatting needs the Rust core "
                "(float-typed program linked without libzz_native_rt; "
                "rebuild without --static)\n");
        exit(1);
    }
    size_t n = zz_float_format_raw(x, stack, ZZ_FLOAT_STACK);
    if (n < ZZ_FLOAT_STACK) {
        *heap = NULL;
        *out_len = n;
        return stack;
    }
    *heap = (char *)malloc(n + 1);
    if (!*heap) {
        fprintf(stderr, "zz: out of memory (float formatting)\n");
        exit(1);
    }
    *out_len = zz_float_format_raw(x, *heap, n + 1);
    return *heap;
}

/// Print a double canonically (see above).
static void zz_print_double(FILE *out, double x) {
    char stack[ZZ_FLOAT_STACK];
    char *heap = NULL;
    size_t n = 0;
    const char *s = zz_canonical_double(x, stack, &heap, &n);
    fwrite(s, 1, n, out);
    free(heap);
}

// Maximum nesting for printed values. Values are finite trees, so this is
// only a safety bound against reference cycles built through mutation.
#define ZZ_PRINT_MAX_DEPTH 32
static void zz_print_value_depth(FILE *out, const zz_value *v, int depth);
static void zz_print_value_display_depth(FILE *out, const zz_value *v, int depth);

// User-enum constructor form: qualified name minus the module
// namespace + `(payload, ...)` (`Token.IntLit(5)`, `Token.Eof`),
// matching the VM's Display. `display_inner` selects the Display vs
// Debug recursion for payload values, matching the caller.
static void zz_print_enum_shape(FILE *out, const zz_object *o, int depth, int display_inner) {
    // Namespace strips only when really present (3+ segments), matching
    // the VM: bare `Token.Eof` prints whole, `ns.Token.Eof` shortens.
    const char *t = o->type_name;
    const char *dot = strchr(t, '.');
    const char *shown = (dot && strchr(dot + 1, '.')) ? dot + 1 : t;
    fputs(shown, out);
    fputc('(', out);
    for (size_t i = 0; i < o->len; i++) {
        if (i > 0) fputs(", ", out);
        if (display_inner) {
            zz_print_value_display_depth(out, &o->fields[i * 2 + 1], depth + 1);
        } else {
            zz_print_value_depth(out, &o->fields[i * 2 + 1], depth + 1);
        }
    }
    fputc(')', out);
}

void zz_print_value(FILE *out, const zz_value *v) {
    zz_print_value_depth(out, v, 0);
}

// Display printer: auto-unwraps Option for user-facing output
// (interpolation, println nesting). `.some(v)` renders as `v`,
// `.none` renders as `none` (no dot). Only `zz_dbg` / `:?` keep the
// explicit `.some(...)` / `.none` debug form above.
void zz_print_value_display(FILE *out, const zz_value *v) {
    zz_print_value_display_depth(out, v, 0);
}

static void zz_print_value_depth(FILE *out, const zz_value *v, int depth) {
    switch (v->tag) {
    case ZZ_UNIT:
        break;
    case ZZ_INT:
        fprintf(out, "%lld", (long long)v->i);
        break;
    case ZZ_FLOAT:
        // Canonical rendering comes from the Rust core (spec §4):
        // NaN/inf/-0/integrals all handled there, never in C.
        zz_print_double(out, v->f);
        break;
    case ZZ_BOOL:
        fputs(v->b ? "true" : "false", out);
        break;
    case ZZ_STR:
        fwrite(zz_str_ptr(v->s), 1, v->s->len, out);
        break;
    case ZZ_ARRAY:
        fputs("[", out);
        if (v->arr) {
            for (size_t i = 0; i < v->arr->len; i++) {
                if (i > 0) fputs(", ", out);
                zz_print_value_depth(out, &v->arr->items[i], depth + 1);
            }
        }
        fputs("]", out);
        break;
    case ZZ_BYTES: {
        // Same `[104, 105]` shape as an int array (matches the VM).
        fputs("[", out);
        if (v->bytes && v->bytes->buf) {
            for (size_t i = 0; i < v->bytes->len; i++) {
                if (i > 0) fputs(", ", out);
                fprintf(out, "%u", v->bytes->buf->data[v->bytes->off + i]);
            }
        }
        fputs("]", out);
        break;
    }
    case ZZ_DICT:
        fputs("{", out);
        if (v->dict) {
            for (size_t i = 0; i < v->dict->len; i++) {
                if (i > 0) fputs(", ", out);
                fwrite(zz_str_ptr(v->dict->entries[i].key), 1,
                       v->dict->entries[i].key->len, out);
                fputs(": ", out);
                zz_print_value_depth(out, &v->dict->entries[i].val, depth + 1);
            }
        }
        fputs("}", out);
        break;
    case ZZ_OPTION_SOME:
        fputs(".some(", out);
        if (v->payload) zz_print_value_depth(out, v->payload, depth + 1);
        fputs(")", out);
        break;
    case ZZ_OPTION_NONE:
        fputs(".none", out);
        break;
    case ZZ_RESULT_OK:
        fputs(".ok(", out);
        if (v->payload) zz_print_value_depth(out, v->payload, depth + 1);
        fputs(")", out);
        break;
    case ZZ_RESULT_ERR:
        fputs(".err(", out);
        if (v->payload) zz_print_value_depth(out, v->payload, depth + 1);
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
                    zz_print_value_depth(out, &(*arr).arr->items[i], depth + 1);
                }
            }
        }
        fputs(")", out);
        break;
    case ZZ_OBJECT: {
        // Boxed struct: `Type{field: value, ...}`, matching the VM's
        // Display. Embedded fields recurse through the same printer.
        if (!v->obj || depth >= ZZ_PRINT_MAX_DEPTH) {
            fputs("...", out);
            break;
        }
        const zz_object *o = v->obj;
        if (zz_object_is_enum_shape(o)) {
            zz_print_enum_shape(out, o, depth, 0);
            break;
        }
        fputs(zz_object_display_name(o), out);
        fputc('{', out);
        for (size_t i = 0; i < o->len; i++) {
            if (i > 0) fputs(", ", out);
            const zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR && fname->s) {
                fwrite(zz_str_ptr(fname->s), 1, fname->s->len, out);
            } else {
                fputs("?", out);
            }
            fputs(": ", out);
            zz_print_value_depth(out, &o->fields[i * 2 + 1], depth + 1);
        }
        fputc('}', out);
        break;
    }
    case ZZ_TCP_STREAM:
        fputs("<tcp stream>", out);
        break;
    case ZZ_TCP_LISTENER:
        fputs("<tcp listener>", out);
        break;
    case ZZ_FILE:
        fputs("<file>", out);
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

static void zz_print_value_display_depth(FILE *out, const zz_value *v, int depth) {
    // Unwrap consecutive `.some` layers; bare `.none` is `none`.
    while (v->tag == ZZ_OPTION_SOME && v->payload) {
        v = v->payload;
        if (depth >= ZZ_PRINT_MAX_DEPTH) {
            fputs("...", out);
            return;
        }
    }
    if (v->tag == ZZ_OPTION_NONE) {
        fputs("none", out);
        return;
    }
    switch (v->tag) {
    case ZZ_UNIT:
        break;
    case ZZ_INT:
        fprintf(out, "%lld", (long long)v->i);
        break;
    case ZZ_FLOAT:
        // Canonical rendering comes from the Rust core (spec §4):
        // NaN/inf/-0/integrals all handled there, never in C.
        zz_print_double(out, v->f);
        break;
    case ZZ_BOOL:
        fputs(v->b ? "true" : "false", out);
        break;
    case ZZ_STR:
        fwrite(zz_str_ptr(v->s), 1, v->s->len, out);
        break;
    case ZZ_ARRAY:
        fputs("[", out);
        if (v->arr) {
            for (size_t i = 0; i < v->arr->len; i++) {
                if (i > 0) fputs(", ", out);
                zz_print_value_display_depth(out, &v->arr->items[i], depth + 1);
            }
        }
        fputs("]", out);
        break;
    case ZZ_BYTES: {
        fputs("[", out);
        if (v->bytes && v->bytes->buf) {
            for (size_t i = 0; i < v->bytes->len; i++) {
                if (i > 0) fputs(", ", out);
                fprintf(out, "%u", v->bytes->buf->data[v->bytes->off + i]);
            }
        }
        fputs("]", out);
        break;
    }
    case ZZ_DICT:
        fputs("{", out);
        if (v->dict) {
            for (size_t i = 0; i < v->dict->len; i++) {
                if (i > 0) fputs(", ", out);
                fwrite(zz_str_ptr(v->dict->entries[i].key), 1,
                       v->dict->entries[i].key->len, out);
                fputs(": ", out);
                zz_print_value_display_depth(out, &v->dict->entries[i].val, depth + 1);
            }
        }
        fputs("}", out);
        break;
    case ZZ_RESULT_OK:
        fputs(".ok(", out);
        if (v->payload) zz_print_value_display_depth(out, v->payload, depth + 1);
        fputs(")", out);
        break;
    case ZZ_RESULT_ERR:
        fputs(".err(", out);
        if (v->payload) zz_print_value_display_depth(out, v->payload, depth + 1);
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
                    zz_print_value_display_depth(out, &(*arr).arr->items[i], depth + 1);
                }
            }
        }
        fputs(")", out);
        break;
    case ZZ_OBJECT: {
        if (!v->obj || depth >= ZZ_PRINT_MAX_DEPTH) {
            fputs("...", out);
            break;
        }
        const zz_object *o = v->obj;
        if (zz_object_is_enum_shape(o)) {
            zz_print_enum_shape(out, o, depth, 1);
            break;
        }
        fputs(zz_object_display_name(o), out);
        fputc('{', out);
        for (size_t i = 0; i < o->len; i++) {
            if (i > 0) fputs(", ", out);
            const zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR && fname->s) {
                fwrite(zz_str_ptr(fname->s), 1, fname->s->len, out);
            } else {
                fputs("?", out);
            }
            fputs(": ", out);
            zz_print_value_display_depth(out, &o->fields[i * 2 + 1], depth + 1);
        }
        fputc('}', out);
        break;
    }
    case ZZ_TCP_STREAM:
        fputs("<tcp stream>", out);
        break;
    case ZZ_TCP_LISTENER:
        fputs("<tcp listener>", out);
        break;
    case ZZ_FILE:
        fputs("<file>", out);
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
// ---- formatting (malloc'd, caller frees) --------------------------------
static char *strdup_len(const char *s, size_t len) {
    char *o = (char *)malloc(len + 1);
    memcpy(o, s, len);
    o[len] = '\0';
    return o;
}

// Simple growable buffer for value_to_string.
typedef struct {
    char  *buf;
    size_t len;
    size_t cap;
} strbuf;

static void sb_init(strbuf *sb) {
    sb->cap  = 256;
    sb->len  = 0;
    sb->buf = (char *)malloc(sb->cap);
    sb->buf[0] = '\0';
}

static void sb_append(strbuf *sb, const char *s, size_t slen) {
    while (sb->len + slen + 1 > sb->cap) {
        sb->cap *= 2;
        sb->buf = (char *)realloc(sb->buf, sb->cap);
    }
    memcpy(sb->buf + sb->len, s, slen);
    sb->len += slen;
    sb->buf[sb->len] = '\0';
}

static void sb_append_str(strbuf *sb, const char *s) {
    sb_append(sb, s, strlen(s));
}

static void sb_append_c(strbuf *sb, char c) {
    sb_append(sb, &c, 1);
}

/// Append a double canonically (see `zz_canonical_double`).
static void zz_append_double(strbuf *sb, double x) {
    char stack[ZZ_FLOAT_STACK];
    char *heap = NULL;
    size_t n = 0;
    const char *s = zz_canonical_double(x, stack, &heap, &n);
    sb_append(sb, s, n);
    free(heap);
}

static void zz_value_to_strbuf_depth(strbuf *sb, const zz_value *v, int depth);
static void zz_value_to_display_strbuf_depth(strbuf *sb, const zz_value *v, int depth);

// `strbuf` twin of `zz_print_enum_shape` for `str()` and interpolation.
static void zz_print_enum_shape_sb(
    strbuf *sb,
    const zz_object *o,
    int depth,
    int display_inner
) {
    const char *t = o->type_name;
    const char *dot = strchr(t, '.');
    sb_append_str(sb, (dot && strchr(dot + 1, '.')) ? dot + 1 : t);
    sb_append_c(sb, '(');
    for (size_t i = 0; i < o->len; i++) {
        if (i > 0) sb_append_str(sb, ", ");
        if (display_inner) {
            zz_value_to_display_strbuf_depth(sb, &o->fields[i * 2 + 1], depth + 1);
        } else {
            zz_value_to_strbuf_depth(sb, &o->fields[i * 2 + 1], depth + 1);
        }
    }
    sb_append_c(sb, ')');
}

static void zz_value_to_strbuf(strbuf *sb, const zz_value *v) {
    zz_value_to_strbuf_depth(sb, v, 0);
}

static void zz_value_to_strbuf_depth(strbuf *sb, const zz_value *v, int depth) {
    char buf[128];
    switch (v->tag) {
    case ZZ_UNIT:
        break;
    case ZZ_INT:
        snprintf(buf, sizeof buf, "%lld", (long long)v->i);
        sb_append_str(sb, buf);
        break;
    case ZZ_FLOAT:
        // Canonical rendering comes from the Rust core (spec §4).
        zz_append_double(sb, v->f);
        break;
    case ZZ_BOOL:
        sb_append_str(sb, v->b ? "true" : "false");
        break;
    case ZZ_STR:
        sb_append(sb, zz_str_ptr(v->s), v->s->len);
        break;
    case ZZ_ARRAY:
        sb_append_c(sb, '[');
        if (v->arr) {
            for (size_t i = 0; i < v->arr->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                zz_value_to_strbuf_depth(sb, &v->arr->items[i], depth + 1);
            }
        }
        sb_append_c(sb, ']');
        break;
    case ZZ_BYTES: {
        sb_append_c(sb, '[');
        if (v->bytes && v->bytes->buf) {
            for (size_t i = 0; i < v->bytes->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                char num[4];
                snprintf(num, sizeof num, "%u",
                         v->bytes->buf->data[v->bytes->off + i]);
                sb_append_str(sb, num);
            }
        }
        sb_append_c(sb, ']');
        break;
    }
    case ZZ_DICT:
        sb_append_c(sb, '{');
        if (v->dict) {
            for (size_t i = 0; i < v->dict->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                sb_append(sb, zz_str_ptr(v->dict->entries[i].key),
                          v->dict->entries[i].key->len);
                sb_append_str(sb, ": ");
                zz_value_to_strbuf_depth(sb, &v->dict->entries[i].val, depth + 1);
            }
        }
        sb_append_c(sb, '}');
        break;
    case ZZ_OPTION_SOME:
        sb_append_str(sb, ".some(");
        if (v->payload) zz_value_to_strbuf_depth(sb, v->payload, depth + 1);
        sb_append_c(sb, ')');
        break;
    case ZZ_OPTION_NONE:
        sb_append_str(sb, ".none");
        break;
    case ZZ_RESULT_OK:
        sb_append_str(sb, ".ok(");
        if (v->payload) zz_value_to_strbuf_depth(sb, v->payload, depth + 1);
        sb_append_c(sb, ')');
        break;
    case ZZ_RESULT_ERR:
        sb_append_str(sb, ".err(");
        if (v->payload) zz_value_to_strbuf_depth(sb, v->payload, depth + 1);
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
                    zz_value_to_strbuf_depth(sb, &(*arr).arr->items[i], depth + 1);
                }
            }
        }
        sb_append_str(sb, ")");
        break;
    case ZZ_OBJECT: {
        // Boxed struct: `Type{field: value, ...}`, matching the VM's
        // Display. Embedded fields recurse through the same printer.
        if (!v->obj || depth >= ZZ_PRINT_MAX_DEPTH) {
            sb_append_str(sb, "...");
            break;
        }
        const zz_object *o = v->obj;
        if (zz_object_is_enum_shape(o)) {
            zz_print_enum_shape_sb(sb, o, depth, 0);
            break;
        }
        sb_append_str(sb, zz_object_display_name(o));
        sb_append_c(sb, '{');
        for (size_t i = 0; i < o->len; i++) {
            if (i > 0) sb_append_str(sb, ", ");
            const zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR && fname->s) {
                sb_append(sb, zz_str_ptr(fname->s), fname->s->len);
            } else {
                sb_append_c(sb, '?');
            }
            sb_append_str(sb, ": ");
            zz_value_to_strbuf_depth(sb, &o->fields[i * 2 + 1], depth + 1);
        }
        sb_append_c(sb, '}');
        break;
    }
    case ZZ_TCP_STREAM:
        sb_append_str(sb, "<tcp stream>");
        break;
    case ZZ_TCP_LISTENER:
        sb_append_str(sb, "<tcp listener>");
        break;
    case ZZ_FILE:
        sb_append_str(sb, "<file>");
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
    return sb.buf;
}

// Display variant: auto-unwraps Option (`.some(v)` → `v`, `.none` →
// `none`) recursively, so interpolation / `str()` / `println` nesting
// never leak the raw variant structure. Only `zz_dbg` / `:?` keep it.
static void zz_value_to_display_strbuf_depth(strbuf *sb, const zz_value *v, int depth);

static void zz_value_to_display_strbuf(strbuf *sb, const zz_value *v) {
    zz_value_to_display_strbuf_depth(sb, v, 0);
}

static void zz_value_to_display_strbuf_depth(strbuf *sb, const zz_value *v, int depth) {
    if (depth >= ZZ_PRINT_MAX_DEPTH) {
        sb_append_str(sb, "...");
        return;
    }
    // Unwrap consecutive `.some` layers; bare `.none` is `none`.
    while (v->tag == ZZ_OPTION_SOME && v->payload) {
        v = v->payload;
    }
    if (v->tag == ZZ_OPTION_NONE) {
        sb_append_str(sb, "none");
        return;
    }
    char buf[128];
    switch (v->tag) {
    case ZZ_UNIT:
        break;
    case ZZ_INT:
        snprintf(buf, sizeof buf, "%lld", (long long)v->i);
        sb_append_str(sb, buf);
        break;
    case ZZ_FLOAT:
        // Canonical rendering comes from the Rust core (spec §4).
        zz_append_double(sb, v->f);
        break;
    case ZZ_BOOL:
        sb_append_str(sb, v->b ? "true" : "false");
        break;
    case ZZ_STR:
        sb_append(sb, zz_str_ptr(v->s), v->s->len);
        break;
    case ZZ_ARRAY:
        sb_append_c(sb, '[');
        if (v->arr) {
            for (size_t i = 0; i < v->arr->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                zz_value_to_display_strbuf_depth(sb, &v->arr->items[i], depth + 1);
            }
        }
        sb_append_c(sb, ']');
        break;
    case ZZ_BYTES: {
        sb_append_c(sb, '[');
        if (v->bytes && v->bytes->buf) {
            for (size_t i = 0; i < v->bytes->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                char num[4];
                snprintf(num, sizeof num, "%u",
                         v->bytes->buf->data[v->bytes->off + i]);
                sb_append_str(sb, num);
            }
        }
        sb_append_c(sb, ']');
        break;
    }
    case ZZ_DICT:
        sb_append_c(sb, '{');
        if (v->dict) {
            for (size_t i = 0; i < v->dict->len; i++) {
                if (i > 0) sb_append_str(sb, ", ");
                sb_append(sb, zz_str_ptr(v->dict->entries[i].key),
                          v->dict->entries[i].key->len);
                sb_append_str(sb, ": ");
                zz_value_to_display_strbuf_depth(sb, &v->dict->entries[i].val, depth + 1);
            }
        }
        sb_append_c(sb, '}');
        break;
    case ZZ_RESULT_OK:
        sb_append_str(sb, ".ok(");
        if (v->payload) zz_value_to_display_strbuf_depth(sb, v->payload, depth + 1);
        sb_append_c(sb, ')');
        break;
    case ZZ_RESULT_ERR:
        sb_append_str(sb, ".err(");
        if (v->payload) zz_value_to_display_strbuf_depth(sb, v->payload, depth + 1);
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
                    zz_value_to_display_strbuf_depth(sb, &(*arr).arr->items[i], depth + 1);
                }
            }
        }
        sb_append_str(sb, ")");
        break;
    case ZZ_OBJECT: {
        if (!v->obj || depth >= ZZ_PRINT_MAX_DEPTH) {
            sb_append_str(sb, "...");
            break;
        }
        const zz_object *o = v->obj;
        if (zz_object_is_enum_shape(o)) {
            zz_print_enum_shape_sb(sb, o, depth, 1);
            break;
        }
        sb_append_str(sb, zz_object_display_name(o));
        sb_append_c(sb, '{');
        for (size_t i = 0; i < o->len; i++) {
            if (i > 0) sb_append_str(sb, ", ");
            const zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR && fname->s) {
                sb_append(sb, zz_str_ptr(fname->s), fname->s->len);
            } else {
                sb_append_c(sb, '?');
            }
            sb_append_str(sb, ": ");
            zz_value_to_display_strbuf_depth(sb, &o->fields[i * 2 + 1], depth + 1);
        }
        sb_append_c(sb, '}');
        break;
    }
    case ZZ_TCP_STREAM:
        sb_append_str(sb, "<tcp stream>");
        break;
    case ZZ_TCP_LISTENER:
        sb_append_str(sb, "<tcp listener>");
        break;
    case ZZ_FILE:
        sb_append_str(sb, "<file>");
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

char *zz_value_to_display_string(const zz_value *v) {
    strbuf sb;
    sb_init(&sb);
    zz_value_to_display_strbuf(&sb, v);
    return sb.buf;
}

// zz_to_str_fmt(val, spec) — format a value using a format spec string.
// The spec is the part after `:` in f-strings, e.g. ".2f", "x", "X", "o", "b".
// Display semantics: Option auto-unwraps (`.some(v)` → `v`, `.none` →
// `none`); explicit `?` / `debug` specs preserve the debug form.
// If spec is NULL or empty, falls back to display string.
char *zz_to_str_fmt(zz_value v, const char *spec) {
    if (!spec || spec[0] == '\0') return zz_value_to_display_string(&v);
    // Explicit debug formatting preserves wrappers.
    if (strcmp(spec, "?") == 0 || strcmp(spec, "debug") == 0) {
        return zz_value_to_string(&v);
    }
    // Auto-unwrap Option layers for display before applying the spec.
    while (v.tag == ZZ_OPTION_SOME && v.payload) {
        v = *v.payload;
    }
    if (v.tag == ZZ_OPTION_NONE) {
        return strdup_len("none", 4);
    }
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
    return zz_value_to_display_string(&v);
}
zz_value zz_binop_cat(zz_value a, zz_value b) {
    if (a.tag == ZZ_STR && b.tag == ZZ_STR) {
        size_t la = a.s->len, lb = b.s->len;
        size_t need = la + lb;
        zz_str *out;
        // In-place fast path: a is uniquely owned (refs==1) and is NOT
        // interned (we must never mutate an interned singleton) and has
        // capacity for the result. SSO strings (cap==0) are reusable when
        // the result still fits inline.
        // Consume semantics: the caller transfers ownership of both inputs.
        // Generated code only ever passes owned temporaries (zz_clone bumps,
        // call results, literals) that are never read again, so releasing
        // what we don't reuse keeps `s = s + x` chains leak-free. Releases
        // are no-ops for interned singletons and arena strings (refs==0).
        if (a.s->refs == 1 && !a.s->interned
            && (a.s->cap >= need || (a.s->cap == 0 && need <= ZZ_SSO_MAX))) {
            out = a.s;
            memcpy(zz_str_ptr(out) + la, zz_str_ptr(b.s), lb);
            out->len = need;
            zz_str_ptr(out)[need] = '\0';
            zz_release(&b);
            zz_value v;
            v.tag = ZZ_STR;
            v.s = out;
            return v;
        }
        out = str_alloc(need);
        memcpy(zz_str_ptr(out), zz_str_ptr(a.s), la);
        memcpy(zz_str_ptr(out) + la, zz_str_ptr(b.s), lb);
        zz_release(&a);
        zz_release(&b);
        zz_value v;
        v.tag = ZZ_STR;
        v.s = out;
        return v;
    }
    return zz_binop(ZZOP_ADD, a, b);
}

// Arena-aware string concatenation. Allocates the result zz_str on the
// given arena (refs=0 sentinel), so zz_release skips it and the bulk
// arena reset reclaims everything at scope exit. Zero heap malloc for
// the string header+data.
// Consume semantics (mirrors zz_binop_cat): both inputs are owned
// temporaries and are released when heap-owned. Arena/interned inputs
// are no-ops under zz_release, so chains like
// cat_arena(cat_arena(clone(s), lit), call) stay leak-free.
zz_value zz_binop_cat_arena(zz_value a, zz_value b, zz_arena *arena) {
    if (a.tag == ZZ_STR && b.tag == ZZ_STR && arena) {
        size_t la = a.s->len, lb = b.s->len;
        size_t need = la + lb;
        // Copy the payload bytes first: `a` may live in this same arena
        // block, and the header alloc below can overflow-adopt that block.
        // Reading la/lb bytes off the adopted (but still mapped) chunk
        // stays valid, but copying up front keeps the logic independent
        // of the allocator's growth strategy.
        const char *pa = zz_str_cptr(a.s);
        const char *pb = zz_str_cptr(b.s);
        zz_str *out = (zz_str *)zz_arena_alloc(arena, sizeof(zz_str), 8);
        out->refs = 0;      // arena sentinel
        out->interned = 0;
        out->len = need;
        if (need <= ZZ_SSO_MAX) {
            out->cap = 0;
            memcpy(out->sso, pa, la);
            memcpy(out->sso + la, pb, lb);
            out->sso[need] = '\0';
        } else {
            out->cap = need;
            out->heap = (char *)zz_arena_alloc(arena, need + 1, 1);
            memcpy(out->heap, pa, la);
            memcpy(out->heap + la, pb, lb);
            out->heap[need] = '\0';
        }
        zz_release(&a);
        zz_release(&b);
        zz_value v;
        v.tag = ZZ_STR;
        v.s = out;
        return v;
    }
    // Fallback: non-string or no arena — use heap path
    return zz_binop_cat(a, b);
}


zz_value zz_binop_cat_str(zz_value a, zz_value b) {
    char *sv = zz_value_to_display_string(&b);
    zz_value sb = zz_str_owned(sv);
    // zz_binop_cat consumes both inputs, so `sb` ownership transfers —
    // no extra release here (it would double-free sb's heap buffer).
    return zz_binop_cat(a, sb);
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
        memcpy(zz_str_ptr(a->s) + la, zz_str_ptr(b.s), lb);
        a->s->len = need;
        zz_str_ptr(a->s)[need] = '\0';
        return;
    }
    // Buffer not reusable: replace with a fresh allocation. Release the
    // old ref first so we don't leak (and don't double-free if the old
    // buffer happened to be interned — refs==1 interned strings stay put).
    // Arena-owned source (refs==0 sentinel) is abandoned, never released
    // or mutated: the arena reclaims it at reset.
    zz_str *fresh = str_alloc(need);
    memcpy(zz_str_ptr(fresh), zz_str_ptr(a->s), la);
    memcpy(zz_str_ptr(fresh) + la, zz_str_ptr(b.s), lb);
    if (!a->s->interned && a->s->refs != 0 && --a->s->refs == 0) {
        if (a->s->cap > 0) free(a->s->heap);
        zz_str_header_free(a->s);
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
        memcpy(zz_str_ptr(a->s) + la, lit, lit_len);
        a->s->len = need;
        zz_str_ptr(a->s)[need] = '\0';
        return;
    }
    zz_str *fresh = str_alloc(need);
    memcpy(zz_str_ptr(fresh), zz_str_ptr(a->s), la);
    memcpy(zz_str_ptr(fresh) + la, lit, lit_len);
    // Arena-owned source (refs==0 sentinel): abandon, never release.
    if (!a->s->interned && a->s->refs != 0 && --a->s->refs == 0) {
        if (a->s->cap > 0) free(a->s->heap);
        zz_str_header_free(a->s);
    }
    a->s = fresh;
}
// str.length(s) — string length in chars (Unicode scalar values),
// matching the VM (`s.chars().count()`). Byte length stays in `s->len`
// for storage; only this user-visible measure counts chars.
zz_value zz_str_length(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return (zz_value){ZZ_INT, {.i = 0}};
    return (zz_value){ZZ_INT, {.i = (int64_t)zz_str_char_len(s.s)}};
}

// str.lower(s) — lowercase copy.
zz_value zz_str_lower(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    size_t len = s.s->len;
    zz_str *out = str_alloc(len);
    for (size_t i = 0; i < len; i++) {
        char c = zz_str_ptr(s.s)[i];
        zz_str_ptr(out)[i] = (c >= 'A' && c <= 'Z') ? c + 32 : c;
    }
    zz_str_ptr(out)[len] = '\0';
    return (zz_value){ZZ_STR, {.s = out}};
}

// str.upper(s) — uppercase copy.
zz_value zz_str_upper(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    size_t len = s.s->len;
    zz_str *out = str_alloc(len);
    for (size_t i = 0; i < len; i++) {
        char c = zz_str_ptr(s.s)[i];
        zz_str_ptr(out)[i] = (c >= 'a' && c <= 'z') ? c - 32 : c;
    }
    zz_str_ptr(out)[len] = '\0';
    return (zz_value){ZZ_STR, {.s = out}};
}

// str.replace(s, old, new) — replace all occurrences.
zz_value zz_str_replace(zz_value s, zz_value old_s, zz_value new_s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || old_s.tag != ZZ_STR || new_s.tag != ZZ_STR) return s;
    const char *src = zz_str_ptr(s.s);
    size_t src_len = s.s->len;
    const char *old_str = zz_str_ptr(old_s.s);
    size_t old_len = old_s.s->len;
    const char *new_str = zz_str_ptr(new_s.s);
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
            memcpy(zz_str_ptr(out) + pos, new_str, new_len);
            pos += new_len;
            i += old_len;
        } else {
            zz_str_ptr(out)[pos++] = src[i++];
        }
    }
    zz_str_ptr(out)[out_len] = '\0';
    return (zz_value){ZZ_STR, {.s = out}};
}

// memchr-skip search core: jump to the next first-needle-byte, then
// verify with memcmp. Portable C89 + memchr, near-memmem speed for
// short needles (the common case).
static const char *scan_skip(const char *h, const char *hend, char first) {
    const char *p = h;
    while (p < hend) {
        const char *hit = (const char *)memchr(p, first, (size_t)(hend - p));
        if (!hit) return hend;
        p = hit;
        return p;
    }
    return hend;
}

// str.count(s, sub) — non-overlapping occurrences, no allocation.
// Empty sub counts chars+1 (matches the split-based version it replaces).
zz_value zz_str_count(zz_value s, zz_value sub, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || sub.tag != ZZ_STR) return (zz_value){ZZ_INT, {.i = 0}};
    const char *src = zz_str_ptr(s.s);
    size_t src_len = s.s->len;
    const char *needle = zz_str_ptr(sub.s);
    size_t needle_len = sub.s->len;
    if (needle_len == 0) return (zz_value){ZZ_INT, {.i = (int64_t)zz_str_char_len(s.s) + 1}};
    int64_t n = 0;
    const char *p = src;
    const char *end = src + src_len;
    while (p + needle_len <= end) {
        p = scan_skip(p, end, needle[0]);
        if (p + needle_len > end) break;
        if (memcmp(p, needle, needle_len) == 0) {
            n++;
            p += needle_len;
        } else {
            p++;
        }
    }
    return (zz_value){ZZ_INT, {.i = n}};
}

// str.contains(s, sub) — check if s contains sub.
zz_value zz_str_contains(zz_value s, zz_value sub, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || sub.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    const char *src = zz_str_ptr(s.s);
    size_t src_len = s.s->len;
    const char *needle = zz_str_ptr(sub.s);
    size_t needle_len = sub.s->len;
    if (needle_len == 0) return (zz_value){ZZ_BOOL, {.b = true}};
    const char *end = src + src_len;
    const char *p = src;
    while (p + needle_len <= end) {
        p = scan_skip(p, end, needle[0]);
        if (p + needle_len > end) break;
        if (memcmp(p, needle, needle_len) == 0) return (zz_value){ZZ_BOOL, {.b = true}};
        p++;
    }
    return (zz_value){ZZ_BOOL, {.b = false}};
}

// Byte-offset search helpers. Contract is byte offsets (O(1) per call,
// backend-identical on every input); matches can only start at char
// boundaries, so non-boundary positions snap (ceil for find, floor for
// rfind/tail checks) and empty patterns return the clamped position.
static size_t snap_fwd(const char *p, size_t len, size_t pos) {
    while (pos < len && ((unsigned char)p[pos] & 0xC0) == 0x80) pos++;
    return pos;
}

static size_t snap_bwd(const char *p, size_t pos) {
    while (pos > 0 && ((unsigned char)p[pos] & 0xC0) == 0x80) pos--;
    return pos;
}

static int64_t find_from(zz_value s, zz_value sub, int64_t from) {
    const char *src = zz_str_ptr(s.s);
    size_t src_len = s.s->len;
    const char *needle = zz_str_ptr(sub.s);
    size_t needle_len = sub.s->len;
    int64_t start = from < 0 ? 0 : from;
    if ((uint64_t)start > src_len) start = (int64_t)src_len;
    size_t base = snap_fwd(src, src_len, (size_t)start);
    if (needle_len == 0) return (int64_t)base;
    if (base >= src_len) return -1;
    const char *end = src + src_len;
    const char *p = src + base;
    while (p + needle_len <= end) {
        p = scan_skip(p, end, needle[0]);
        if (p + needle_len > end) break;
        if (memcmp(p, needle, needle_len) == 0) return (int64_t)(p - src);
        p++;
    }
    return -1;
}

static int64_t rfind_from(zz_value s, zz_value sub, int64_t from) {
    const char *src = zz_str_ptr(s.s);
    size_t src_len = s.s->len;
    const char *needle = zz_str_ptr(sub.s);
    size_t needle_len = sub.s->len;
    int64_t end_c = from < 0 ? 0 : from;
    if ((uint64_t)end_c > src_len) end_c = (int64_t)src_len;
    size_t end = snap_bwd(src, (size_t)end_c);
    if (needle_len == 0) return (int64_t)end;
    int64_t best = -1;
    const char *fin = src + src_len;
    const char *p = src;
    while (p + needle_len <= fin) {
        p = scan_skip(p, fin, needle[0]);
        if (p + needle_len > fin) break;
        if ((size_t)(p - src) > end) break;
        if (memcmp(p, needle, needle_len) == 0) best = (int64_t)(p - src);
        p++;
    }
    return best;
}

// str.find(s, sub, from) — first match at/after byte offset `from`.
zz_value zz_str_find(zz_value s, zz_value sub, zz_value from, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || sub.tag != ZZ_STR) return (zz_value){ZZ_INT, {.i = -1}};
    int64_t f = from.tag == ZZ_INT ? from.i : 0;
    return (zz_value){ZZ_INT, {.i = find_from(s, sub, f)}};
}

// str.rfind(s, sub, from) — last match starting at/before `from`.
zz_value zz_str_rfind(zz_value s, zz_value sub, zz_value from, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || sub.tag != ZZ_STR) return (zz_value){ZZ_INT, {.i = -1}};
    int64_t f = from.tag == ZZ_INT ? from.i : 0;
    return (zz_value){ZZ_INT, {.i = rfind_from(s, sub, f)}};
}

// str.starts_with_at(s, sub, pos) — match at byte offset, else false.
// Both window edges must sit on char boundaries (a partial char can
// never equal a valid pattern's bytes).
zz_value zz_str_starts_with_at(zz_value s, zz_value sub, zz_value pos, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || sub.tag != ZZ_STR || pos.tag != ZZ_INT) return (zz_value){ZZ_BOOL, {.b = false}};
    size_t n = sub.s->len;
    if (n == 0) return (zz_value){ZZ_BOOL, {.b = false}};
    if (pos.i < 0 || (uint64_t)pos.i + n > s.s->len) return (zz_value){ZZ_BOOL, {.b = false}};
    size_t base = (size_t)pos.i;
    const char *src = zz_str_ptr(s.s);
    if (snap_bwd(src, base) != base || snap_bwd(src, base + n) != base + n)
        return (zz_value){ZZ_BOOL, {.b = false}};
    return (zz_value){ZZ_BOOL, {.b = memcmp(src + base, zz_str_ptr(sub.s), n) == 0}};
}

// str.ends_with_at(s, sub, pos) — match ending at byte offset `pos`.
zz_value zz_str_ends_with_at(zz_value s, zz_value sub, zz_value pos, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || sub.tag != ZZ_STR || pos.tag != ZZ_INT) return (zz_value){ZZ_BOOL, {.b = false}};
    size_t n = sub.s->len;
    if (n == 0) return (zz_value){ZZ_BOOL, {.b = false}};
    if (pos.i < 0 || (uint64_t)pos.i > s.s->len || (uint64_t)pos.i < n) return (zz_value){ZZ_BOOL, {.b = false}};
    size_t base = (size_t)pos.i - n;
    const char *src = zz_str_ptr(s.s);
    if (snap_bwd(src, base) != base || snap_bwd(src, (size_t)pos.i) != (size_t)pos.i)
        return (zz_value){ZZ_BOOL, {.b = false}};
    return (zz_value){ZZ_BOOL, {.b = memcmp(src + base, zz_str_ptr(sub.s), n) == 0}};
}

// Width of whitespace at d[pos] (0 if none): ASCII ws plus the
// Unicode White_Space sequences. Explicit table so both backends agree
// (never the host trim).
static size_t ws_width_fwd(const unsigned char *d, size_t pos, size_t end) {
    if (pos >= end) return 0;
    unsigned char c = d[pos];
    if (c == ' ' || c == '\t' || c == '\n' || c == '\x0b' || c == '\x0c' || c == '\r') return 1;
    if (c == 0xC2 && pos + 1 < end && (d[pos + 1] == 0x85 || d[pos + 1] == 0xA0)) return 2;
    if (c == 0xE1 && pos + 2 < end && d[pos + 1] == 0x9A && d[pos + 2] == 0x80) return 3;
    if (c == 0xE2 && pos + 2 < end && d[pos + 1] == 0x80) {
        unsigned char e = d[pos + 2];
        if ((e >= 0x80 && e <= 0x8A) || e == 0xA8 || e == 0xA9 || e == 0xAF) return 3;
    }
    if (c == 0xE2 && pos + 2 < end && d[pos + 1] == 0x81 && d[pos + 2] == 0x9F) return 3;
    if (c == 0xE3 && pos + 2 < end && d[pos + 1] == 0x80 && d[pos + 2] == 0x80) return 3;
    return 0;
}

static size_t ws_width_bwd(const unsigned char *d, size_t s, size_t end) {
    if (end <= s) return 0;
    unsigned char c = d[end - 1];
    if (c == ' ' || c == '\t' || c == '\n' || c == '\x0b' || c == '\x0c' || c == '\r') return 1;
    if (end - s >= 2 && d[end - 2] == 0xC2 && (c == 0x85 || c == 0xA0)) return 2;
    if (end - s >= 3 && d[end - 3] == 0xE1 && d[end - 2] == 0x9A && c == 0x80) return 3;
    if (end - s >= 3 && d[end - 3] == 0xE2 && d[end - 2] == 0x80) {
        if ((c >= 0x80 && c <= 0x8A) || c == 0xA8 || c == 0xA9 || c == 0xAF) return 3;
    }
    if (end - s >= 3 && d[end - 3] == 0xE2 && d[end - 2] == 0x81 && c == 0x9F) return 3;
    if (end - s >= 3 && d[end - 3] == 0xE3 && d[end - 2] == 0x80 && c == 0x80) return 3;
    return 0;
}

// str.bytes(s) — UTF-8 bytes as plain ints (one copy).
zz_value zz_str_bytes(zz_value s, int *err) {
    (void)err;
    zz_value arr = zz_array_new();
    if (s.tag != ZZ_STR) return arr;
    const unsigned char *d = (const unsigned char *)zz_str_ptr(s.s);
    int sub_err = 0;
    for (size_t i = 0; i < s.s->len; i++) {
        zz_vec_append(arr, (zz_value){ZZ_INT, {.i = (int64_t)d[i]}}, &sub_err);
    }
    return arr;
}

// bytes.to_str(vs) — strict UTF-8 decode; out-of-range values and
// invalid sequences are .err, identically on VM and AOT.
zz_value zz_bytes_to_str(zz_value vs, int *err) {
    if (vs.tag != ZZ_ARRAY) {
        if (err) *err = 1;
        return zz_unit();
    }
    size_t n = vs.arr->len;
    unsigned char *buf = (unsigned char *)malloc(n > 0 ? n : 1);
    if (!buf) {
        return zz_variant_err(zz_str_static("bytes.to_str: out of memory"));
    }
    for (size_t i = 0; i < n; i++) {
        zz_value v = vs.arr->items[i];
        if (v.tag != ZZ_INT) {
            free(buf);
            if (err) *err = 1;
            return zz_unit();
        }
        if (v.i < 0 || v.i > 255) {
            free(buf);
            char msg[96];
            snprintf(msg, sizeof(msg), "bytes.to_str: value %lld out of range 0-255", (long long)v.i);
            return zz_variant_err(zz_str_new(msg, strlen(msg)));
        }
        buf[i] = (unsigned char)v.i;
    }
    // Strict validation: reject overlongs, surrogates, > U+10FFFF.
    size_t i = 0;
    int ok = 1;
    while (i < n) {
        unsigned char c = buf[i];
        size_t want = 1;
        if (c < 0x80) want = 1;
        else if (c >= 0xC2 && c <= 0xDF) want = 2;
        else if (c >= 0xE0 && c <= 0xEF) want = 3;
        else if (c >= 0xF0 && c <= 0xF4) want = 4;
        else { ok = 0; break; }
        if (i + want > n) { ok = 0; break; }
        for (size_t k = 1; k < want; k++) {
            if ((buf[i + k] & 0xC0) != 0x80) { ok = 0; break; }
        }
        if (!ok) break;
        if (want == 3) {
            if (c == 0xE0 && buf[i + 1] < 0xA0) { ok = 0; break; }
            if (c == 0xED && buf[i + 1] > 0x9F) { ok = 0; break; }
        }
        if (want == 4) {
            if (c == 0xF0 && buf[i + 1] < 0x90) { ok = 0; break; }
            if (c == 0xF4 && buf[i + 1] > 0x8F) { ok = 0; break; }
        }
        i += want;
    }
    if (!ok) {
        free(buf);
        return zz_variant_err(zz_str_static("bytes.to_str: invalid UTF-8"));
    }
    zz_value out = zz_str_new((const char *)buf, n);
    free(buf);
    return zz_variant_ok(out);
}

// bytes.to_ints(b) — opaque byte buffer as plain ints.
zz_value zz_bytes_to_ints(zz_value b, int *err) {
    (void)err;
    zz_value arr = zz_array_new();
    if (b.tag != ZZ_BYTES) return arr;
    const unsigned char *d;
    size_t n;
    zz_bytes_view(b, &d, &n);
    int sub_err = 0;
    for (size_t i = 0; i < n; i++) {
        zz_vec_append(arr, (zz_value){ZZ_INT, {.i = (int64_t)d[i]}}, &sub_err);
    }
    return arr;
}

// str.trim_span(s, start, end) — trimmed [lo, hi] byte offsets. Unicode
// White_Space on both backends (explicit table, never the host trim).
zz_value zz_str_trim_span(zz_value s, zz_value start, zz_value end, int *err) {
    (void)err;
    zz_value arr = zz_array_new();
    int sub_err = 0;
    int64_t lo = 0, hi = 0;
    if (s.tag == ZZ_STR) {
        const unsigned char *d = (const unsigned char *)zz_str_ptr(s.s);
        size_t n = s.s->len;
        int64_t a = start.tag == ZZ_INT ? start.i : 0;
        int64_t b = end.tag == ZZ_INT ? end.i : 0;
        if (a < 0) a = 0;
        if ((uint64_t)a > n) a = (int64_t)n;
        if (b < 0) b = 0;
        if ((uint64_t)b > n) b = (int64_t)n;
        lo = a;
        hi = b > a ? b : a;
        size_t w;
        while ((size_t)lo < (size_t)hi && (w = ws_width_fwd(d, (size_t)lo, (size_t)hi)) != 0) lo += (int64_t)w;
        while (hi > lo && (w = ws_width_bwd(d, (size_t)lo, (size_t)hi)) != 0) hi -= (int64_t)w;
    }
    zz_vec_append(arr, (zz_value){ZZ_INT, {.i = lo}}, &sub_err);
    zz_vec_append(arr, (zz_value){ZZ_INT, {.i = hi}}, &sub_err);
    return arr;
}

// str.startswith(s, prefix)// str.startswith(s, prefix)
zz_value zz_str_startswith(zz_value s, zz_value prefix, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || prefix.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    if (prefix.s->len > s.s->len) return (zz_value){ZZ_BOOL, {.b = false}};
    return (zz_value){ZZ_BOOL, {.b = memcmp(zz_str_ptr(s.s), zz_str_ptr(prefix.s), prefix.s->len) == 0}};
}

// str.endswith(s, suffix)
zz_value zz_str_endswith(zz_value s, zz_value suffix, int *err) {
    (void)err;
    if (s.tag != ZZ_STR || suffix.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    if (suffix.s->len > s.s->len) return (zz_value){ZZ_BOOL, {.b = false}};
    return (zz_value){ZZ_BOOL, {.b = memcmp(zz_str_ptr(s.s) + s.s->len - suffix.s->len, zz_str_ptr(suffix.s), suffix.s->len) == 0}};
}

// str.trim(s) — strip leading/trailing whitespace
zz_value zz_str_trim(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    const char *d = zz_str_ptr(s.s);
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
    const char *d = zz_str_ptr(s.s);
    size_t len = s.s->len;
    size_t start = 0;
    while (start < len && (d[start] == ' ' || d[start] == '\t' || d[start] == '\n' || d[start] == '\r')) start++;
    return zz_str_new(d + start, len - start);
}

// str.trim_end(s) — strip trailing whitespace
zz_value zz_str_trim_end(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    const char *d = zz_str_ptr(s.s);
    size_t len = s.s->len;
    size_t end = len;
    while (end > 0 && (d[end-1] == ' ' || d[end-1] == '\t' || d[end-1] == '\n' || d[end-1] == '\r')) end--;
    return zz_str_new(d, end);
}

// str.join(items, sep) — join array of strings with separator.
// Display semantics: non-string items stringify via display conversion
// (Options unwrap), matching the VM's `vec.join` / `str.join`.
zz_value zz_str_join(zz_value items, zz_value sep, int *err) {
    (void)err;
    if (items.tag != ZZ_ARRAY || !items.arr) return zz_str_static("");
    const char *sep_d = "";
    size_t sep_len = 0;
    if (sep.tag == ZZ_STR) { sep_d = zz_str_ptr(sep.s); sep_len = sep.s->len; }
    size_t n = items.arr->len;
    if (n == 0) return zz_str_static("");
    // Materialize each item as bytes (borrowed for STR, owned display
    // string otherwise), then join.
    const char **parts = (const char **)malloc(n * sizeof(char *));
    size_t *lens = (size_t *)malloc(n * sizeof(size_t));
    char **owned = (char **)malloc(n * sizeof(char *));
    if (!parts || !lens || !owned) {
        free(parts); free(lens); free(owned);
        return zz_str_static("");
    }
    size_t total = 0;
    for (size_t i = 0; i < n; i++) {
        zz_value v = items.arr->items[i];
        owned[i] = NULL;
        if (v.tag == ZZ_STR && v.s) {
            parts[i] = zz_str_cptr(v.s);
            lens[i] = v.s->len;
        } else {
            char *s = zz_value_to_display_string(&v);
            owned[i] = s;
            parts[i] = s;
            lens[i] = strlen(s);
        }
        total += lens[i];
        if (i > 0) total += sep_len;
    }
    char *buf = (char *)malloc(total + 1);
    size_t pos = 0;
    for (size_t i = 0; i < n; i++) {
        if (i > 0) { memcpy(buf + pos, sep_d, sep_len); pos += sep_len; }
        memcpy(buf + pos, parts[i], lens[i]);
        pos += lens[i];
        free(owned[i]);
    }
    buf[pos] = '\0';
    free(parts);
    free(lens);
    free(owned);
    return zz_str_owned(buf);
}

// str.split(s, sep) — split string by separator
zz_value zz_str_split(zz_value s, zz_value sep, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return zz_array_new();
    const char *d = zz_str_ptr(s.s);
    size_t len = s.s->len;
    const char *sd = ""; size_t slen = 0;
    if (sep.tag == ZZ_STR) { sd = zz_str_ptr(sep.s); slen = sep.s->len; }
    zz_value arr = zz_array_new();
    if (slen == 0) {
        // Empty separator: leading "" + one item per char (Unicode scalar
        // values, like the VM) + trailing "" — matches Rust `split("")`
        // exactly (`"ab"` → `["", "a", "b", ""]`, `""` → `["", ""]`).
        {
            int sub_err = 0;
            zz_vec_append(arr, zz_str_static(""), &sub_err);
        }
        size_t i = 0;
        while (i < len) {
            size_t l = zz_utf8_seq_len((const unsigned char *)(d + i), len - i);
            zz_value item = zz_str_new(d + i, l);
            int sub_err = 0;
            zz_vec_append(arr, item, &sub_err);
            i += l;
        }
        {
            int sub_err = 0;
            zz_vec_append(arr, zz_str_static(""), &sub_err);
        }
        return arr;
    }
    size_t pos = 0;
    for (;;) {
        size_t next = pos;
        int found = 0;
        while (next + slen <= len) {
            if (memcmp(d + next, sd, slen) == 0) { found = 1; break; }
            next++;
        }
        if (!found) {
            // No more separators: remainder runs to end of string.
            // (Previously emitted d[pos..next] where next stalls at
            // len-slen+1, silently dropping up to slen-1 tail chars —
            // invisible for single-char seps where next always reaches len.)
            zz_value item = zz_str_new(d + pos, len - pos);
            int sub_err = 0;
            zz_vec_append(arr, item, &sub_err);
            break;
        }
        zz_value item = zz_str_new(d + pos, next - pos);
        int sub_err = 0;
        zz_vec_append(arr, item, &sub_err);
        pos = next + slen;
    }
    return arr;
}

// Fast int64 → decimal (P7): two-digit lookup table writes pairs per
// division, halving (expensive) divisions vs one-digit loops and skipping
// snprintf's format/varargs/locale machinery entirely. Writes backwards
// from `end` (one past the buffer); buffer needs >= 22 bytes (20 digits
// + sign + slack). Returns pointer to the first char; length via *len.
static const char *zz_fmt_i64(char *end, int64_t n, size_t *len) {
    static const char pairs[] =
        "00010203040506070809"
        "10111213141516171819"
        "20212223242526272829"
        "30313233343536373839"
        "40414243444546474849"
        "50515253545556575859"
        "60616263646566676869"
        "70717273747576777879"
        "80818283848586878889"
        "90919293949596979899";
    uint64_t u;
    int neg = 0;
    if (n < 0) {
        neg = 1;
        u = 0u - (uint64_t)n;  // exact even for INT64_MIN
    } else {
        u = (uint64_t)n;
    }
    char *p = end;
    while (u >= 100) {
        unsigned r = (unsigned)(u % 100);
        u /= 100;
        *--p = pairs[r * 2 + 1];
        *--p = pairs[r * 2];
    }
    // Final 1–2 digits: never emit a leading zero.
    unsigned last = (unsigned)u;
    *--p = pairs[last * 2 + 1];
    if (last >= 10) {
        *--p = pairs[last * 2];
    }
    if (neg) {
        *--p = '-';
    }
    *len = (size_t)(end - p);
    return p;
}

// Fast int → string: format into a 24-byte stack buffer, then SSO.
// Avoids the zz_value_to_string strbuf path (malloc 256 + free) for the
// most common cast in loops (`str(i)`). int64 min is 20 chars, always SSO.
zz_value zz_str_from_int(int64_t n) {
    char buf[24];
    size_t len;
    const char *p = zz_fmt_i64(buf + sizeof buf, n, &len);
    return zz_str_new(p, len);
}

// Append a formatted int directly into the string buffer: one grow +
// memcpy, no zz_value temp, no arena staging, no release. Used by the
// `s = s + ... + str(i) + ...` append-chain fast path.
void zz_str_append_int(zz_value *a, int64_t n) {
    char buf[24];
    size_t len;
    const char *p = zz_fmt_i64(buf + sizeof buf, n, &len);
    zz_str_append_lit(a, p, len);
}

// Append a bool in display form (`true`/`false`, matching
// zz_print_value_display and the VM).
void zz_str_append_bool(zz_value *a, bool b) {
    if (b) zz_str_append_lit(a, "true", 4);
    else zz_str_append_lit(a, "false", 5);
}

// typeof(v) — return type name as string.
// zz_str(v) — cast to string (display semantics: unwraps Option).
zz_value zz_str_cast(zz_value v, int *err) {
    (void)err;
    if (v.tag == ZZ_INT) return zz_str_from_int(v.i);
    char *s = zz_value_to_display_string(&v);
    return zz_str_owned(s);
}

// Arena-aware str cast: converts v to string and allocates the result on
// the arena (refs=0 sentinel). The intermediate char* from display
// conversion is freed after copying to the arena.
zz_value zz_str_cast_arena(zz_value v, int *err, zz_arena *arena) {
    (void)err;
    if (!arena) return zz_str_cast(v, err);
    // Int fast path: LUT-format, then arena-allocate (SSO, zero heap).
    // Skips the zz_value_to_string strbuf malloc/free entirely.
    if (v.tag == ZZ_INT) {
        char ibuf[24];
        size_t ilen;
        const char *p = zz_fmt_i64(ibuf + sizeof ibuf, v.i, &ilen);
        return zz_str_new_arena(p, ilen, arena);
    }
    char *s = zz_value_to_display_string(&v);
    size_t len = strlen(s);
    zz_str *str = (zz_str *)zz_arena_alloc(arena, sizeof(zz_str), 8);
    str->refs = 0;
    str->interned = 0;
    str->len = len;
    if (len <= ZZ_SSO_MAX) {
        str->cap = 0;
        memcpy(str->sso, s, len);
        str->sso[len] = '\0';
    } else {
        str->cap = len;
        str->heap = (char *)zz_arena_alloc(arena, len + 1, 1);
        memcpy(str->heap, s, len);
        str->heap[len] = '\0';
    }
    free(s);
    zz_value rv;
    rv.tag = ZZ_STR;
    rv.s = str;
    return rv;
}

// to_str(v) — convert any value to a string zz_value (for fstring interpolation).
// Display semantics: auto-unwraps Option; only `zz_dbg` / `:?` keep wrappers.
zz_value zz_to_str(zz_value v, int *err) {
    (void)err;
    char *s = zz_value_to_display_string(&v);
    return zz_str_owned(s);
}
