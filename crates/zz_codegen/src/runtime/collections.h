// ZZ native runtime — dynamic array (vec) and object dictionary
// implementations, plus boxed structs, Option/Result variants, match
// extraction helpers, and higher-order iterators.

#ifndef ZZ_RUNTIME_COLLECTIONS_H
#define ZZ_RUNTIME_COLLECTIONS_H

#include "core.h"

#ifdef __cplusplus
extern "C" {
#endif

// ---- array / dict constructors -----------------------------------------
zz_value zz_array_new(void);
zz_value zz_dict_new(void);
zz_value zz_range(int64_t start, int64_t end, int64_t step);

// ---- arena-aware constructors -------------------------------------------
// When arena is non-NULL, the object header is bump-allocated on the arena
// (O(1) alloc, freed in bulk at arena reset). When arena is NULL, falls
// back to the standard malloc path (thread-safe ARC).
//
// The items/data buffers ALWAYS use malloc (they may realloc on growth).
// Only the *header structs* (zz_array, zz_dict, zz_str) are arena-eligible.
zz_value zz_array_new_arena(zz_arena *arena);
zz_value zz_array_new_arena_sized(zz_arena *arena, size_t cap);
zz_value zz_dict_new_arena(zz_arena *arena);
zz_value zz_dict_new_arena_sized(zz_arena *arena, size_t hint);

// Fixed-size array literal constructor. When `arena` is non-NULL, both the
// header and the items buffer (exactly `n` slots) are bump-allocated on the
// arena: `a->items = zz_arena_alloc(...)` pre-allocates capacity so appends
// never hit realloc, and `zz_array_push_lit` stores elements directly
// without `zz_clone` atomic refcount bumps. When `arena` is NULL, the
// standard ARC heap path is used (calloc + one malloc, refs=1).
zz_value zz_array_new_lit(zz_arena *arena, size_t n);

// Store `item` into the next free slot of a fixed-capacity literal array.
// No clone, no realloc, no bounds branch. Only valid on arrays built by
// `zz_array_new_lit` (or newly heap-allocated literals) with spare capacity.
void zz_array_push_lit(zz_array *a, zz_value item);

// ---- containers --------------------------------------------------------
void zz_array_push(zz_array *a, zz_value v);
zz_value zz_array_get(const zz_array *a, zz_value idx, int *err);
void zz_array_set(zz_array *a, zz_value idx, zz_value v, int *err);
size_t zz_array_len(const zz_array *a);
zz_value zz_array_slice(const zz_array *a, zz_value start, zz_value end, int *err);
zz_value zz_array_dup(const zz_array *a);

// Index expression support (lowered from `obj[idx]`). Dispatch on the
// object tag at runtime: arrays and dicts. Returns unit + *err=1 on unsupported.
zz_value zz_index_get(zz_value obj, zz_value idx, int *err);
void zz_index_set(zz_value obj, zz_value idx, zz_value item, int *err);

// Slice expression (`obj[a:b]`): arrays (items) and strings (bytes).
zz_value zz_slice_value(zz_value obj, zz_value start, zz_value end, int *err);

void zz_dict_set(zz_dict *d, zz_value key, zz_value val);
zz_value zz_dict_get(const zz_dict *d, zz_value key, int *err);
size_t zz_dict_len(const zz_dict *d);

// ---- higher-order iterators --------------------------------------------
// map/filter/enumerate/zip — call closures per item.
zz_value zz_iter_map(zz_value items, zz_value f, int *err);
zz_value zz_iter_filter(zz_value items, zz_value f, int *err);
zz_value zz_iter_enumerate(zz_value items, int *err);
zz_value zz_iter_zip(zz_value a, zz_value b, int *err);
zz_value zz_range3(zz_value a, zz_value b, zz_value c, int *err);
// Tuples: display as `(a, b)` (distinct from arrays' `[a, b]`).
zz_value zz_tuple(zz_value a, zz_value b);

// ---- vec natives -------------------------------------------------------
zz_value zz_len(zz_value v, int *err);
zz_value zz_vec_len(zz_value v, int *err);
zz_value zz_vec_append(zz_value arr, zz_value item, int *err);
zz_value zz_vec_push(zz_value arr, zz_value item, int *err);
zz_value zz_vec_pop(zz_value arr, int *err);
zz_value zz_vec_remove(zz_value arr, zz_value idx, int *err);
zz_value zz_vec_insert(zz_value arr, zz_value idx, zz_value item, int *err);
zz_value zz_vec_contains(zz_value arr, zz_value item, int *err);
zz_value zz_vec_sort(zz_value arr, int *err);
zz_value zz_vec_reverse(zz_value arr, int *err);

// ---- dict natives ------------------------------------------------------
zz_value zz_dict_len_val(zz_value d, int *err);
zz_value zz_dict_keys(zz_value d, int *err);
zz_value zz_dict_has(zz_value d, zz_value key, int *err);

// ---- variant constructors (Option / Result) ---------------------------
// Store the inner value on the heap so match can extract it via payload pointer.
zz_value zz_variant_some(zz_value inner);
zz_value zz_variant_ok(zz_value inner);
zz_value zz_variant_err(zz_value inner);

// Release helper for variant payloads (called by the ARC dispatcher).
void zz_release_variant(zz_value *v);

// ---- boxed struct (object) constructors and accessors --------------------
zz_value zz_object_new(const char *type_name, zz_value *field_names, size_t n);
void zz_object_set_field(zz_value *obj, const char *name, zz_value val);
zz_value zz_object_get_field(zz_value *obj, const char *name);

// Release helper for boxed objects (called by the ARC dispatcher).
void zz_release_object(zz_value *v);

// ---- match extraction helpers ------------------------------------------
// Returns the payload of a variant, or unit if tag doesn't match.
zz_value zz_match_ok(zz_value v);
zz_value zz_match_err(zz_value v);
zz_value zz_match_some(zz_value v);

// ---- codegen shims -----------------------------------------------------
zz_value zz_range_build(zz_value start, zz_value end);
zz_value zz_elvis(zz_value left, zz_value right);

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_COLLECTIONS_H