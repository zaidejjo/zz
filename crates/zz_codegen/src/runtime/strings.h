// ZZ native runtime — core string manipulation routines.
//
// String interning, heap/arena string construction, concatenation shims,
// and the str.* natives. Also exposes the small growable byte-buffer
// builder (`SB`) shared with the JSON serializer.

#ifndef ZZ_RUNTIME_STRINGS_H
#define ZZ_RUNTIME_STRINGS_H

#include "core.h"

#ifdef __cplusplus
extern "C" {
#endif

// ---- internal helpers (shared across runtime modules) ------------------
// Allocate a heap string with at least `need` bytes of payload capacity.
zz_str *str_alloc(size_t need);
// Grow an existing heap string's buffer to hold at least `new_len` bytes.
zz_str *str_grow(zz_str *s, size_t new_len);
// Fast header pool (mimalloc-lite): thread-local free-list of zz_str
// headers. Short-lived strings in tight loops recycle headers without
// touching the system allocator. Arena headers never enter the pool.
zz_str *zz_str_header_alloc(void);
void zz_str_header_free(zz_str *s);

// Small growable byte-buffer builder (malloc'd, caller frees via sb_take).
typedef struct { char *buf; size_t len; size_t cap; } SB;
void sb_str(SB *sb, const char *s, size_t n);
char *sb_take(SB *sb);
// malloc'd NUL-terminated copy of a (possibly embedded-NUL) buffer.
char *copy_cstr(const char *s, size_t n);

// FFI bridge for the Rust static library: expose string bytes without
// requiring Rust to mirror the zz_str layout. Sets (*out_ptr, *out_len);
// (NULL, 0) for non-strings. The bytes stay owned by the runtime — the
// caller must copy synchronously, never retain.
void zz_str_view(zz_value v, const char **out_ptr, size_t *out_len);

// JSON helpers defined in json.c; printers in this module use them so JSON
// values display in their canonical compact form (matching the VM's
// to_json_string).
zz_value zz_json_unwrap(zz_value v);
void json_serialize(SB *sb, zz_value v);
char *json_to_cstr(zz_value v);

// ---- string constructors ------------------------------------------------
zz_value zz_str_new(const char *s, size_t len);
zz_value zz_str_owned(char *s);            // takes ownership
zz_value zz_str_static(const char *s);     // copy of a C literal
zz_value zz_str_new_arena(const char *s, size_t len, zz_arena *arena);
zz_value zz_str_cast_arena(zz_value v, int *err, zz_arena *arena); // arena str cast
// Heal an arena-owned string (refs==0 sentinel) into an independent
// heap-owned copy (refs==1). All other values pass through unchanged.
// Every boundary that retains a value beyond the current loop iteration
// (variable assignment, array/dict/object stores) must heal: the
// per-iteration `zz_arena_reset` reuses the buffer, so aliasing it past
// the reset reads back garbage (NUL bytes), and aliasing it past
// `zz_arena_destroy` is use-after-free. Mirrors the string path of
// `zz_value_dup` used at thread crossings.
zz_value zz_str_heal_arena(zz_value v);

// ---- string concatenation shims ----------------------------------------
zz_value zz_binop_cat(zz_value a, zz_value b);       // str concat
zz_value zz_binop_cat_arena(zz_value a, zz_value b, zz_arena *arena); // arena str concat
zz_value zz_binop_cat_str(zz_value a, zz_value b);   // str + Display(b)
// In-place append: reuses *a->s buffer if refs==1 and capacity allows.
// Returns void; *a is mutated. Generated for hot `s = s + literal` loops.
void zz_str_append_str(zz_value *a, zz_value b);

// ---- UTF-8 character helpers -------------------------------------------
// `str` is UTF-8; `len` / `str.length` / indexing / slicing all count
// Unicode scalar values (chars), matching the VM (`s.chars().count()`).
// Byte length stays in `s->len` for storage/concat/compare/print/hash.
//
// Invalid bytes (lone continuation, truncated sequence, bad continuation)
// count as one single-byte char each: never crash, never loop forever,
// always terminate. Pure ASCII is unaffected (bytes == chars).
static inline size_t zz_utf8_seq_len(const unsigned char *p, size_t remain) {
    unsigned char c = p[0];
    size_t want;
    if (c < 0x80) return 1;
    if ((c & 0xE0) == 0xC0) want = 2;
    else if ((c & 0xF0) == 0xE0) want = 3;
    else if ((c & 0xF8) == 0xF0) want = 4;
    else return 1; // lone continuation or 0xF8+ : one byte char
    if (want > remain) return 1; // truncated: one byte char
    for (size_t k = 1; k < want; k++) {
        if ((p[k] & 0xC0) != 0x80) return 1; // bad continuation: resync
    }
    return want;
}

// Number of Unicode scalar values in `s` (chars, not bytes).
static inline size_t zz_str_char_len(const zz_str *s) {
    if (!s) return 0;
    const unsigned char *p = (const unsigned char *)zz_str_cptr(s);
    size_t n = s->len;
    // ASCII fast path: bytes without the high bit are single-byte
    // chars, so a pure-ASCII string's char count is its byte length.
    // Word-at-a-time high-bit test (~n/8 steps); only strings with
    // actual multibyte sequences pay for the precise UTF-8 walk.
    size_t i = 0;
    const size_t WS = sizeof(size_t);
    const size_t LO = ((size_t)-1) / (size_t)0xFF;
    const size_t HI = LO * (size_t)0x80;
    int ascii = 1;
    while (i < n && (((uintptr_t)(p + i)) & (WS - 1)) != 0) {
        if (p[i] >= 0x80) {
            ascii = 0;
            break;
        }
        i++;
    }
    if (ascii) {
        for (; i + WS <= n; i += WS) {
            size_t w;
            memcpy(&w, p + i, WS);
            if ((w & HI) != 0) {
                ascii = 0;
                break;
            }
        }
    }
    if (ascii) {
        for (; i < n; i++) {
            if (p[i] >= 0x80) {
                ascii = 0;
                break;
            }
        }
    }
    if (ascii) {
        return n;
    }
    size_t count = 0;
    i = 0;
    while (i < n) {
        i += zz_utf8_seq_len(p + i, n - i);
        count++;
    }
    return count;
}

// Byte offset of the `char_idx`-th char (0-based) plus its byte length in
// `*out_clen`. Returns `(size_t)-1` when out of range. Caller normalizes
// negatives against `zz_str_char_len` first.
static inline size_t zz_str_char_byte_off(const zz_str *s, size_t char_idx, size_t *out_clen) {
    const unsigned char *p = (const unsigned char *)zz_str_cptr(s);
    size_t n = s->len, i = 0;
    for (size_t c = 0; i < n; c++) {
        size_t l = zz_utf8_seq_len(p + i, n - i);
        if (c == char_idx) {
            if (out_clen) *out_clen = l;
            return i;
        }
        i += l;
    }
    return (size_t)-1;
}

// Index a string by char: `s[i]` → 1-char string, negative counts
// from the end (in chars). Out of bounds (or non-int index) → unit + *err,
// mirroring zz_bytes_get. Char-based like the VM; slicing below agrees.
static inline zz_value zz_str_get(const zz_str *s, zz_value idx, int *err) {
    *err = 0;
    if (idx.tag != ZZ_INT || !s) {
        *err = 1;
        return zz_unit();
    }
    int64_t n = (int64_t)zz_str_char_len(s);
    int64_t i = idx.i;
    if (i < 0)
        i += n;
    if (i < 0 || i >= n) {
        *err = 1;
        return zz_unit();
    }
    size_t clen = 1;
    size_t off = zz_str_char_byte_off(s, (size_t)i, &clen);
    return zz_str_new(zz_str_cptr(s) + off, clen);
}
void zz_str_append_lit(zz_value *a, const char *lit, size_t len);
// Append a formatted int / bool directly (no temp, no release).
// Fast path for `str(i)` terms in `s = s + ...` append chains.
void zz_str_append_int(zz_value *a, int64_t n);
void zz_str_append_bool(zz_value *a, bool b);

// ---- str natives -------------------------------------------------------
zz_value zz_str_length(zz_value s, int *err);
zz_value zz_str_lower(zz_value s, int *err);
zz_value zz_str_upper(zz_value s, int *err);
zz_value zz_str_replace(zz_value s, zz_value old_s, zz_value new_s, int *err);
zz_value zz_str_count(zz_value s, zz_value sub, int *err);
zz_value zz_str_contains(zz_value s, zz_value sub, int *err);
zz_value zz_str_startswith(zz_value s, zz_value prefix, int *err);
zz_value zz_str_endswith(zz_value s, zz_value suffix, int *err);
zz_value zz_str_trim(zz_value s, int *err);
zz_value zz_str_trim_start(zz_value s, int *err);
zz_value zz_str_trim_end(zz_value s, int *err);
zz_value zz_str_join(zz_value items, zz_value sep, int *err);
zz_value zz_str_split(zz_value s, zz_value sep, int *err);
zz_value zz_str_find(zz_value s, zz_value sub, zz_value from, int *err);
zz_value zz_str_rfind(zz_value s, zz_value sub, zz_value from, int *err);
zz_value zz_str_starts_with_at(zz_value s, zz_value sub, zz_value pos, int *err);
zz_value zz_str_ends_with_at(zz_value s, zz_value sub, zz_value pos, int *err);
zz_value zz_str_bytes(zz_value s, int *err);
zz_value zz_bytes_to_str(zz_value vs, int *err);
zz_value zz_bytes_to_ints(zz_value b, int *err);
zz_value zz_str_trim_span(zz_value s, zz_value start, zz_value end, int *err);

// ---- string casts ------------------------------------------------------
zz_value zz_str_from_int(int64_t n);
zz_value zz_str_cast(zz_value v, int *err);
zz_value zz_to_str(zz_value v, int *err);

// ---- formatting --------------------------------------------------------
void zz_print_value(FILE *out, const zz_value *v);
void zz_print_value_display(FILE *out, const zz_value *v);
char *zz_value_to_string(const zz_value *v);  // malloc'd (debug: keeps wrappers)
char *zz_value_to_display_string(const zz_value *v);  // malloc'd (display: unwraps Option)
char *zz_to_str_fmt(zz_value v, const char *spec);  // malloc'd

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_STRINGS_H