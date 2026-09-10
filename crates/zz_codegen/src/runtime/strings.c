// ZZ native runtime — core string manipulation routines.
//
// String interning, heap/arena string construction, concatenation
// shims, formatting, and the str.* natives.
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
zz_str *str_alloc(size_t need) {
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
zz_str *str_grow(zz_str *s, size_t new_len) {
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
// `SB` typedef lives in strings.h (shared with the JSON serializer).
void sb_str(SB *sb, const char *s, size_t n) {
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

// Arena-aware string concatenation. Allocates the result zz_str on the
// given arena (refs=0 sentinel), so zz_release skips it and the bulk
// arena reset reclaims everything at scope exit. Zero heap malloc for
// the string header+data.
zz_value zz_binop_cat_arena(zz_value a, zz_value b, zz_arena *arena) {
    if (a.tag == ZZ_STR && b.tag == ZZ_STR && arena) {
        size_t la = a.s->len, lb = b.s->len;
        size_t need = la + lb;
        zz_str *out = (zz_str *)zz_arena_alloc(arena, sizeof(zz_str) + need + 1, 8);
        out->refs = 0;      // arena sentinel
        out->interned = 0;
        out->cap = need;
        out->len = need;
        memcpy(out->data, a.s->data, la);
        memcpy(out->data + la, b.s->data, lb);
        out->data[need] = '\0';
        zz_value v;
        v.tag = ZZ_STR;
        v.s = out;
        return v;
    }
    // Fallback: non-string or no arena — use heap path
    return zz_binop_cat(a, b);
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
// zz_str(v) — cast to string.
zz_value zz_str_cast(zz_value v, int *err) {
    (void)err;
    char *s = zz_value_to_string(&v);
    return zz_str_owned(s);
}

// Arena-aware str cast: converts v to string and allocates the result on
// the arena (refs=0 sentinel). The intermediate char* from zz_value_to_string
// is freed after copying to the arena.
zz_value zz_str_cast_arena(zz_value v, int *err, zz_arena *arena) {
    (void)err;
    if (!arena) return zz_str_cast(v, err);
    char *s = zz_value_to_string(&v);
    size_t len = strlen(s);
    zz_str *str = (zz_str *)zz_arena_alloc(arena, sizeof(zz_str) + len + 1, 8);
    str->refs = 0;
    str->interned = 0;
    str->cap = len;
    str->len = len;
    memcpy(str->data, s, len);
    str->data[len] = '\0';
    free(s);
    zz_value rv;
    rv.tag = ZZ_STR;
    rv.s = str;
    return rv;
}

// to_str(v) — convert any value to a string zz_value (for fstring interpolation).
zz_value zz_to_str(zz_value v, int *err) {
    (void)err;
    char *s = zz_value_to_string(&v);
    return zz_str_owned(s);
}
