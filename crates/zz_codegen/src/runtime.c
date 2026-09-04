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

// ---- refcounting -------------------------------------------------------
void zz_retain(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        v->s->refs++;
        break;
    default:
        break;
    }
}

void zz_release(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned && --v->s->refs == 0) {
            free(v->s);
        }
        break;
    default:
        break;
    }
}

void zz_assign(zz_value *dst, zz_value src) {
    if (dst->tag == ZZ_STR) {
        zz_release(dst);
    }
    *dst = src;
}

zz_value zz_clone(zz_value v) {
    if (v.tag == ZZ_STR) {
        v.s->refs++;
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
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

void zz_array_push(zz_array *a, zz_value item) {
    if (a->len == a->cap) {
        size_t nc = a->cap == 0 ? 4 : a->cap * 2;
        a->items = (zz_value *)realloc(a->items, nc * sizeof(zz_value));
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
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
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
        if (x == (int64_t)x && x < 1e15 && x > -1e15) {
            fprintf(out, "%.1f", x);
        } else {
            fprintf(out, "%.17g", x);
        }
        break;
    }
    case ZZ_BOOL:
        fputs(v->b ? "true" : "false", out);
        break;
    case ZZ_STR:
        fwrite(v->s->data, 1, v->s->len, out);
        break;
    case ZZ_OPTION_SOME: {
        // payload printed as .some(...) — not supported inline; fallthrough
        fputs(".? ", out);
        break;
    }
    default:
        fputs("<value>", out);
        break;
    }
}

zz_value zz_io_println(zz_value v, int *err) {
    (void)err;
    zz_print_value(stdout, &v);
    fputc('\n', stdout);
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
    if (a.tag == ZZ_INT && b.tag == ZZ_INT)
        return zz_int((int64_t)dpow((double)a.i, (double)b.i));
    double x = a.tag == ZZ_FLOAT ? a.f : (double)a.i;
    double y = b.tag == ZZ_FLOAT ? b.f : (double)b.i;
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

// ---- formatting (malloc'd, caller frees) --------------------------------
static char *strdup_len(const char *s, size_t len) {
    char *o = (char *)malloc(len + 1);
    memcpy(o, s, len);
    o[len] = '\0';
    return o;
}

char *zz_value_to_string(const zz_value *v) {
    char buf[128];
    switch (v->tag) {
    case ZZ_INT:
        snprintf(buf, sizeof buf, "%lld", (long long)v->i);
        return strdup_len(buf, strlen(buf));
    case ZZ_FLOAT: {
        double x = v->f;
        if (x == (int64_t)x && x < 1e15 && x > -1e15)
            snprintf(buf, sizeof buf, "%.1f", x);
        else
            snprintf(buf, sizeof buf, "%.17g", x);
        return strdup_len(buf, strlen(buf));
    }
    case ZZ_BOOL:
        return strdup_len(v->b ? "true" : "false", v->b ? 4 : 5);
    case ZZ_STR:
        return strdup_len(v->s->data, v->s->len);
    case ZZ_UNIT:
        return strdup_len("", 0);
    default:
        return strdup_len("<value>", 7);
    }
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
        a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
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
        a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        a->cap = new_cap;
    }
    for (size_t j = a->len; j > (size_t)i; j--) {
        a->items[j] = a->items[j - 1];
    }
    a->items[i] = zz_clone(item);
    a->len++;
    return zz_unit();
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

// math.sqrt(v)
zz_value zz_math_sqrt(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = sqrt(d)}};
}

// env.get(name)
zz_value zz_env_get(zz_value name, int *err) {
    (void)err;
    if (name.tag != ZZ_STR) return zz_unit();
    const char *val = getenv(name.s->data);
    if (!val) { *err = 1; return zz_unit(); }
    return zz_str_static(val);
}

// env.args()
zz_value zz_env_args(zz_value unused, int *err) {
    (void)unused; (void)err;
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