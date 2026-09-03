// ZZ native runtime — implementation.
#include <math.h>
#include "runtime.h"

// ---- string helpers ----------------------------------------------------
static zz_str *str_alloc(size_t len) {
    zz_str *s = (zz_str *)malloc(sizeof(zz_str) + len + 1);
    if (!s) {
        fprintf(stderr, "zz: out of memory\n");
        exit(1);
    }
    s->refs = 1;
    s->len = len;
    s->data[len] = '\0';
    return s;
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
    return zz_str_new(src, strlen(src));
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
        if (v->s && --v->s->refs == 0) {
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

zz_value zz_math_pow(zz_value a, zz_value b, int *err) {
    (void)err;
    if (a.tag == ZZ_INT && b.tag == ZZ_INT)
        return zz_int((int64_t)dpow((double)a.i, (double)b.i));
    double x = a.tag == ZZ_FLOAT ? a.f : (double)a.i;
    double y = b.tag == ZZ_FLOAT ? b.f : (double)b.i;
    return zz_float(dpow(x, y));
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

zz_value zz_binop_cat(zz_value a, zz_value b) {
    if (a.tag == ZZ_STR && b.tag == ZZ_STR) {
        zz_str *out = str_alloc(a.s->len + b.s->len);
        memcpy(out->data, a.s->data, a.s->len);
        memcpy(out->data + a.s->len, b.s->data, b.s->len);
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

zz_value zz_range_build(zz_value start, zz_value end) {
    (void)end;
    // Represent a range inline; used in `for`. Return an int start marker
    // (codegen for `for` emits two-path C loop directly, so this is mostly
    // unused).
    zz_value v = {ZZ_RANGE, {0}};
    v.i = start.tag == ZZ_INT ? start.i : 0;
    return v;
}