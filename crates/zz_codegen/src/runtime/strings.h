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

// Index a string by byte offset: `s[i]` → 1-char string, negative counts
// from the end. Out of bounds (or non-int index) → unit + *err, mirroring
// zz_bytes_get. Byte-based like zz_slice_value ("ASCII-compatible");
// the VM counts Unicode chars instead — established engine difference
// for non-ASCII, same as slicing.
static inline zz_value zz_str_get(const zz_str *s, zz_value idx, int *err) {
    *err = 0;
    if (idx.tag != ZZ_INT || !s) {
        *err = 1;
        return zz_unit();
    }
    int64_t i = idx.i;
    int64_t n = (int64_t)s->len;
    if (i < 0)
        i += n;
    if (i < 0 || i >= n) {
        *err = 1;
        return zz_unit();
    }
    return zz_str_new(zz_str_cptr(s) + (size_t)i, 1);
}
void zz_str_append_lit(zz_value *a, const char *lit, size_t len);

// ---- str natives -------------------------------------------------------
zz_value zz_str_length(zz_value s, int *err);
zz_value zz_str_lower(zz_value s, int *err);
zz_value zz_str_upper(zz_value s, int *err);
zz_value zz_str_replace(zz_value s, zz_value old_s, zz_value new_s, int *err);
zz_value zz_str_contains(zz_value s, zz_value sub, int *err);
zz_value zz_str_startswith(zz_value s, zz_value prefix, int *err);
zz_value zz_str_endswith(zz_value s, zz_value suffix, int *err);
zz_value zz_str_trim(zz_value s, int *err);
zz_value zz_str_trim_start(zz_value s, int *err);
zz_value zz_str_trim_end(zz_value s, int *err);
zz_value zz_str_join(zz_value items, zz_value sep, int *err);
zz_value zz_str_split(zz_value s, zz_value sep, int *err);

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