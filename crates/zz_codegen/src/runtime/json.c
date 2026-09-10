// ZZ native runtime — native JSON parsing, stringifying, and utilities.
//
// Parses recursive JSON into plain zz_values matching the VM grammar
// and serializes zz_values back to compact JSON text.
#include "runtime.h"

// Wrap a raw value as a JSON value (mirrors the VM's `Value::Json`).
// The payload is heap-allocated and refcounted via the same path as
// Option/Result variants.
zz_value zz_json_wrap(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_JSON, {.payload = p}};
}

// Unwrap a ZZ_JSON payload (or pass through plain values).
zz_value zz_json_unwrap(zz_value v) {
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

// Convert byte position to 1-based line and column.
static void json_line_col(json_parser *p, size_t pos, int *line, int *col) {
    int l = 1, c = 1;
    for (size_t i = 0; i < pos && i < p->len; i++) {
        if (p->s[i] == '\n') { l++; c = 1; } else { c++; }
    }
    *line = l; *col = c;
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
        int line, col; json_line_col(p, p->pos, &line, &col);
        snprintf(p->err, sizeof(p->err), "expected `\"` at line %d, col %d", line, col);
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
                        else { int line, col; json_line_col(p, p->pos, &line, &col); snprintf(p->err, sizeof(p->err), "invalid \\u escape at line %d, col %d", line, col); goto fail; }
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
                    { int line, col; json_line_col(p, p->pos, &line, &col); snprintf(p->err, sizeof(p->err), "invalid escape `\\%c` at line %d, col %d", e, line, col); }
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
        int line, col; json_line_col(p, p->pos, &line, &col);
        snprintf(p->err, sizeof(p->err), "invalid literal at line %d, col %d", line, col);
        return -1;
    }
    if (c == 't') {
        if (p->pos + 4 <= p->len && memcmp(p->s + p->pos, "true", 4) == 0) { p->pos += 4; *out = (zz_value){ZZ_BOOL, {.b = true}}; return 0; }
        int line, col; json_line_col(p, p->pos, &line, &col);
        snprintf(p->err, sizeof(p->err), "invalid literal at line %d, col %d", line, col);
        return -1;
    }
    if (c == 'f') {
        if (p->pos + 5 <= p->len && memcmp(p->s + p->pos, "false", 5) == 0) { p->pos += 5; *out = (zz_value){ZZ_BOOL, {.b = false}}; return 0; }
        int line, col; json_line_col(p, p->pos, &line, &col);
        snprintf(p->err, sizeof(p->err), "invalid literal at line %d, col %d", line, col);
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
            { int line, col; json_line_col(p, p->pos, &line, &col);
            snprintf(p->err, sizeof(p->err), "expected `]` at line %d, col %d", line, col); }
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
                int line, col; json_line_col(p, p->pos, &line, &col);
                snprintf(p->err, sizeof(p->err), "expected `:` at line %d, col %d", line, col);
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
            { int line, col; json_line_col(p, p->pos, &line, &col);
            snprintf(p->err, sizeof(p->err), "expected `}` at line %d, col %d", line, col); }
            zz_release(&dict);
            return -1;
        }
    }
    if (c == '-' || (c >= '0' && c <= '9')) return json_parse_number(p, out);
    { int line, col; json_line_col(p, p->pos, &line, &col);
    snprintf(p->err, sizeof(p->err), "unexpected character `%c` at line %d, col %d", c, line, col); }
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
        int line, col; json_line_col(&p, p.pos, &line, &col);
        snprintf(p.err, sizeof(p.err), "unexpected trailing characters at line %d, col %d", line, col);
        return zz_variant_err(zz_str_owned(text_invalid_json(p.err)));
    }
    return zz_variant_ok(zz_json_wrap(raw));
}

// ---- compact JSON serializer (matches VM to_json_string) -----------------
static void json_append_str_sb(SB *sb, const char *s, size_t len);
void json_serialize(SB *sb, zz_value v) {
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
char *json_to_cstr(zz_value v) {
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
