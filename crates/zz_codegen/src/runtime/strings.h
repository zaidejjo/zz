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

// ---- string concatenation shims ----------------------------------------
zz_value zz_binop_cat(zz_value a, zz_value b);       // str concat
zz_value zz_binop_cat_arena(zz_value a, zz_value b, zz_arena *arena); // arena str concat
zz_value zz_binop_cat_str(zz_value a, zz_value b);   // str + Display(b)
// In-place append: reuses *a->s buffer if refs==1 and capacity allows.
// Returns void; *a is mutated. Generated for hot `s = s + literal` loops.
void zz_str_append_str(zz_value *a, zz_value b);
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
zz_value zz_str_cast(zz_value v, int *err);
zz_value zz_to_str(zz_value v, int *err);

// ---- formatting --------------------------------------------------------
void zz_print_value(FILE *out, const zz_value *v);
char *zz_value_to_string(const zz_value *v);  // malloc'd
char *zz_to_str_fmt(zz_value v, const char *spec);  // malloc'd

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_STRINGS_H