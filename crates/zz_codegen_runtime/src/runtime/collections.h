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
zz_value zz_array_new_sized(size_t cap);
zz_value zz_dict_new(void);
zz_value zz_dict_new_sized(size_t hint);

// ---- arena healing -----------------------------------------------------
// True when the container header lives on an arena (or the C stack), so a
// retaining store into an escaping container must copy it to the heap
// first — the per-iteration `zz_arena_reset` would otherwise corrupt the
// stored alias (nested-array first-element garbage + SIGSEGV class).
int zz_array_is_arena(const zz_array *a);
int zz_dict_is_arena(const zz_dict *d);
// Heal for move paths (callers transfer ownership: `zz_array_push`,
// `zz_assign`): arena-owned values (deep: nested arena values copied
// recursively, heap elements shared via clone) become independent
// heap-owned copies; heap values pass through unchanged (adopted).
zz_value zz_heal_for_move(zz_value v);
// Heal for share paths (callers pass borrowed: `std.cell` pool,
// container slots): arena-owned values become independent heap-owned
// copies; heap values are shared via `zz_clone` (one extra share for
// the store). Using `zz_heal_for_move(zz_clone(v))` here instead would
// leak one header share for dirty boxed structs (the intermediate clone
// bumps the header, but the deep copy drops that share unreleased).
zz_value zz_clone_for_store(zz_value v);

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

// Pre-reserve capacity for at least `additional` more elements.
// Caller must hold the only share (refs == 1). No-op when capacity suffices.
void zz_array_reserve(zz_value arr, size_t additional, int *err);

// ---- containers --------------------------------------------------------
void zz_array_push(zz_array *a, zz_value v);
// Inlined: every `arr[i]` in a loop pays no call overhead. The tag/bounds
// branches predict perfectly for monomorphic loops; element clone folds
// to a plain increment (str) or nothing (int) via core.h inlines.
static inline zz_value zz_array_get(const zz_array *a, zz_value idx, int *err) {
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

// Bounds-eliminated read: caller guarantees 0 <= i < len. Skips the tag
// check and bounds branch entirely. Returns a cloned value (refcount
// bumped) so the caller's release discipline is unchanged.
static inline zz_value zz_array_get_raw(const zz_array *a, int64_t i) {
    return zz_clone(a->items[(size_t)i]);
}

// Forward declaration: defined below at `zz_array_dup`.
zz_value zz_array_dup(const zz_array *a);

// Bounds-eliminated write: caller guarantees 0 <= i < len. Skips the bounds
// branch but still handles copy-on-write when the buffer is shared
// (refs != 1). Takes a `zz_value *obj` so the container pointer can be
// reseated on COW. `err` is set on OOM only, never on bounds.
static inline void zz_array_set_raw(zz_value *obj, int64_t i, zz_value v, int *err) {
    if (!obj || obj->tag != ZZ_ARRAY || !obj->arr) { if (err) *err = 1; return; }
    zz_array *a = obj->arr;
    if (a->refs != 1) {
        zz_value fresh = zz_array_dup(a);
        if (!fresh.arr) { if (err) *err = 1; return; }
        zz_release_array(a);
        obj->arr = fresh.arr;
        a = obj->arr;
    }
    zz_release(&a->items[(size_t)i]);
    a->items[(size_t)i] = zz_clone(v);
    if (err) *err = 0;
}
void zz_array_set(zz_array *a, zz_value idx, zz_value v, int *err);
size_t zz_array_len(const zz_array *a);
// FFI bridge (see collections.c): positional read for Rust callers.
zz_value zz_array_at(zz_value arr, size_t i, int *err);
zz_value zz_array_slice(const zz_array *a, zz_value start, zz_value end, int *err);
zz_value zz_array_dup(const zz_array *a);

// ---- byte buffers --------------------------------------------------------
// Contiguous `bytes` (ZZ_BYTES): refcounted shared backing store plus a
// window, so slices share without copying (mirrors the VM's BytesData).
zz_value zz_bytes_new(const unsigned char *src, size_t len);
// Adopt an already-malloc'd buffer of exactly `len` bytes (no copy).
zz_value zz_bytes_take(unsigned char *data, size_t len);
// Backing-store constructor + window wrapper (lets file reads land
// directly in the store: one allocation, zero copies).
zz_bytes_buf *zz_bytes_buf_new(size_t len);
zz_value zz_bytes_wrap(zz_bytes_buf *buf, size_t off, size_t len);
zz_value zz_bytes_slice(const zz_bytes *b, int64_t s, int64_t e);
// Borrow the window bytes for FFI readers (mirrors zz_str_view).
void zz_bytes_view(zz_value v, const unsigned char **out_ptr, size_t *out_len);
static inline zz_value zz_bytes_get(const zz_bytes *b, zz_value idx, int *err) {
    *err = 0;
    if (idx.tag != ZZ_INT || !b) {
        *err = 1;
        return zz_unit();
    }
    int64_t i = idx.i;
    int64_t n = (int64_t)b->len;
    if (i < 0)
        i += n;
    if (i < 0 || i >= n) {
        *err = 1;
        return zz_unit();
    }
    return (zz_value){ZZ_INT, {.i = (int64_t)b->buf->data[b->off + (size_t)i]}};
}
void zz_retain_bytes(zz_bytes *b);
void zz_release_bytes(zz_bytes *b);

// ---- dict helpers (inline for hot loop paths) --------------------------

static inline size_t zz_dict_cap_for(size_t len) {
    size_t cap = 16;
    while (cap - (cap >> 2) < len) cap <<= 1;
    return cap;
}

static inline int zz_dict_key_hash(zz_value key, size_t *out) {
    uint64_t h;
    switch (key.tag) {
    case ZZ_INT:
        h = (uint64_t)key.i;
        break;
    case ZZ_BOOL:
        h = (uint64_t)(key.b ? 1 : 0);
        break;
    case ZZ_STR: {
        if (!key.s) return 0;
        if (key.s->interned && key.s->hash != 0) {
            h = key.s->hash;
            break;
        }
        const unsigned char *p = (const unsigned char *)zz_str_cptr(key.s);
        size_t n = key.s->len;
        h = 1469598103934665603ULL;
        for (size_t i = 0; i < n; i++) {
            h ^= p[i];
            h *= 1099511628211ULL;
        }
        break;
    }
    default:
        return 0;
    }
    h ^= h >> 30;
    h *= 0xbf58476d1ce4e5b9ULL;
    h ^= h >> 27;
    h *= 0x94d049bb133111ebULL;
    h ^= h >> 31;
    *out = (size_t)h;
    return 1;
}

static inline void zz_dict_idx_place(zz_dict *d, size_t j) {
    size_t h;
    if (!d->idx || !zz_dict_key_hash(d->entries[j].key, &h)) return;
    size_t mask = d->idx_cap - 1;
    size_t p = h & mask;
    while (d->idx[p] != 0) {
        if (zz_key_eq(d->entries[d->idx[p] - 1].key, d->entries[j].key)) return;
        p = (p + 1) & mask;
    }
    d->idx[p] = j + 1;
}

static inline void zz_dict_idx_build(zz_dict *d, zz_arena *arena, size_t cap) {
    if (cap == 0) return;
    size_t bytes = cap * sizeof(size_t);
    size_t *slots = arena ? (size_t *)zz_arena_alloc(arena, bytes, 8)
                          : (size_t *)calloc(1, bytes);
    if (!slots) return;
    if (arena) memset(slots, 0, bytes);
    d->idx = slots;
    d->idx_cap = cap;
    for (size_t j = 0; j < d->len; j++) zz_dict_idx_place(d, j);
}

static inline void zz_dict_idx_insert(zz_dict *d, size_t i) {
    size_t h;
    if (!zz_dict_key_hash(d->entries[i].key, &h)) return;
    if (!d->idx) {
        if (d->len <= ZZ_DICT_INDEX_THRESHOLD) return;
        zz_dict_idx_build(d, NULL, zz_dict_cap_for(d->len));
        return;
    }
    if (d->len * 4 > d->idx_cap * 3) {
        if (d->refs == ZZ_DICT_ARENA_MAGIC) {
            d->idx = NULL;
            d->idx_cap = 0;
            return;
        }
        size_t cap = d->idx_cap << 1;
        size_t *fresh = (size_t *)calloc(1, cap * sizeof(size_t));
        if (!fresh) return;
        free(d->idx);
        d->idx = fresh;
        d->idx_cap = cap;
        for (size_t j = 0; j < d->len; j++) zz_dict_idx_place(d, j);
        return;
    }
    zz_dict_idx_place(d, i);
}

static inline int zz_dict_find(const zz_dict *d, zz_value key, size_t *out) {
    size_t h;
    if (d->idx && zz_dict_key_hash(key, &h)) {
        size_t mask = d->idx_cap - 1;
        size_t p = h & mask;
        while (d->idx[p] != 0) {
            size_t j = d->idx[p] - 1;
            if (zz_key_eq(d->entries[j].key, key)) {
                *out = j;
                return 1;
            }
            p = (p + 1) & mask;
        }
        return 0;
    }
    for (size_t i = 0; i < d->len; i++) {
        if (zz_key_eq(d->entries[i].key, key)) {
            *out = i;
            return 1;
        }
    }
    return 0;
}

static inline zz_value zz_dict_get(const zz_dict *d, zz_value key, int *err) {
    *err = 0;
    if (!d) { *err = 1; return zz_unit(); }
    size_t pos;
    if (zz_dict_find(d, key, &pos)) return zz_clone(d->entries[pos].val);
    *err = 1;
    return zz_unit();
}

static inline zz_value zz_index_get(zz_value obj, zz_value idx, int *err) {
    switch (obj.tag) {
    case ZZ_ARRAY:
        return zz_array_get(obj.arr, idx, err);
    case ZZ_BYTES:
        return zz_bytes_get(obj.bytes, idx, err);
    case ZZ_STR:
        return zz_str_get(obj.s, idx, err);
    case ZZ_DICT:
        return zz_dict_get(obj.dict, idx, err);
    default:
        *err = 1;
        return zz_unit();
    }
}
// Duplicate a dict with an INDEPENDENT entries buffer (shallow value
// clone, like `zz_array_dup`): callers may mutate the result without
// affecting the original. Arena-owned keys/values heal to heap.
zz_value zz_dict_dup_value(const zz_dict *d);

// Detach-on-mutation for index stores (value semantics): when the
// container buffer is shared (or a non-owned sentinel), replace this
// slot's buffer with a private dup first. Uniquely-owned buffers
// (`refs == 1`) write in place with zero copies.


// Bounds-check trap with operands and enclosing function (#251).
void zz_index_trap(zz_value obj, zz_value idx, const char *func);

// Same trap for flat (unboxed) array buffers, where the container is a raw
// `T *` + tracked length instead of a boxed `zz_value` array. Message and
// abort semantics mirror `zz_index_trap` so VM and native agree.
void zz_index_trap_flat(long long i, long long n, const char *func);

// Normalize a flat-array index (negatives count from the end, like the
// VM's `normalize_index`) and trap when it is out of range. Kept inline so
// the hot read stays two predictable branches.
static inline int64_t zz_flat_index(int64_t i, int64_t n, const char *func) {
    int64_t idx = i < 0 ? i + n : i;
    if (idx < 0 || idx >= n)
        zz_index_trap_flat((long long)i, (long long)n, func);
    return idx;
}

// ---- flat rows (unboxed nested arrays) ---------------------------------
// `a := []` whose elements are themselves flat scalar arrays lowers to
// `zz_flat_row *`: each element is a *snapshot* of a row buffer, taken from
// the function arena when the row is pushed. Copy-on-push gives the same
// value semantics as the boxed layout (which detaches on write), so mutating
// the source row afterwards can never be observed through the outer array.
typedef struct {
    void *data;
    int64_t len;
} zz_flat_row;

// ---- flat rows (unboxed nested arrays) ---------------------------------
// `a := []` whose elements are themselves flat scalar arrays lowers to
// `zz_flat_row *`: each element is a *snapshot* of a row buffer, taken from
// the function arena when the row is pushed. Copy-on-push gives the same
// value semantics as the boxed layout (which detaches on write), so mutating
// the source row afterwards can never be observed through the outer array.
typedef struct {
    void *data;
    int64_t len;
} zz_flat_row;

// Slice expression (`obj[a:b]`): arrays (items) and strings (bytes).
zz_value zz_slice_value(zz_value obj, zz_value start, zz_value end, int *err);

static inline void zz_dict_set(zz_dict *d, zz_value key, zz_value val) {
    if (!d) return;
    size_t pos;
    if (zz_dict_find(d, key, &pos)) {
        zz_assign(&d->entries[pos].val, val);
        return;
    }
    if (d->len == d->cap) {
        if (d->refs == ZZ_DICT_ARENA_MAGIC) return;
        size_t nc = d->cap == 0 ? 4 : d->cap * 2;
        size_t old_cap = d->cap;
        d->entries = (zz_dict_entry *)realloc(d->entries, nc * sizeof(zz_dict_entry));
        memset(&d->entries[old_cap], 0, (nc - old_cap) * sizeof(zz_dict_entry));
        d->cap = nc;
    }
    zz_dict_entry *e = &d->entries[d->len++];
    e->key = zz_clone_for_store(key);
    zz_assign(&e->val, zz_heal_for_move(val));
    if (val.tag != ZZ_INT && val.tag != ZZ_FLOAT && val.tag != ZZ_BOOL
        && val.tag != ZZ_UNIT)
        d->has_heap_values = 1;
    if (!(key.tag == ZZ_STR && key.s && key.s->interned))
        d->has_heap_keys = 1;
    zz_dict_idx_insert(d, d->len - 1);
}

// Index expression support (lowered from `obj[idx]`). Dispatch on the
// object tag at runtime: arrays and dicts. Arena buffers write in place.
//
// Defined out-of-line in collections.c (not `static inline`) because it is
// called through the C-ABI from Rust (`zz_native_rt::pg::dict_insert`) and
// therefore needs an external symbol to link against — `static inline` gives
// it internal linkage only.
void zz_index_set(zz_value *obj, zz_value idx, zz_value item, int *err);

size_t zz_dict_len(const zz_dict *d);

// Grow dict entries buffer to `hint` if currently smaller. Called before a
// counted insertion loop to avoid incremental reallocations (dict_lookup).
void zz_dict_reserve(zz_value d, size_t hint, int *err);

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
zz_value zz_len_field(zz_value *obj, const char *field, int *err);
zz_value zz_vec_len(zz_value v, int *err);
// vec.append(arr, item) — append item to array in place.
// NOTE: the codegen uses this as the array-literal builder (calls ignore the
// return). Kept as a mutator for that contract; `vec.push` is the functional
// copy-on-write variant that matches the VM.
// static inline so the 1M+ append calls in tight loops pay zero call overhead.
static inline zz_value zz_vec_append(zz_value arr, zz_value item, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    // Copy-on-write: only mutate in place when uniquely owned (refs == 1).
    // Stack/arena/lit arrays are uniquely owned (nothing else can point to
    // stack or arena memory), so treat them like refs == 1.
    int uniquely_owned = (a->refs == 1
        || a->refs == ZZ_ARRAY_STACK_MAGIC
        || a->refs == ZZ_ARRAY_LIT_MAGIC
        || a->refs == ZZ_ARRAY_ARENA_MAGIC
        || a->refs == 0);   // arena array already migrated to heap (see below)
    if (!uniquely_owned) return zz_unit();
    if (a->len >= a->cap) {
        size_t new_cap = a->cap ? a->cap * 2 : 8;
        if (a->refs == ZZ_ARRAY_LIT_MAGIC || a->refs == ZZ_ARRAY_STACK_MAGIC
            || a->refs == ZZ_ARRAY_ARENA_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            for (size_t i = 0; i < a->len; i++)
                new_items[i] = a->items[i];
            // Stack/lit/arena items live on the stack or an arena — they must
            // NOT be freed (free() on stack/arena memory corrupts the heap).
            // They're reclaimed when the frame/arena resets. On later grows
            // `a->refs` is 0/1 so we take the realloc branch above.
            a->items = new_items;
            // Mark migration to heap so `zz_release_array` frees the items
            // buffer when this array is released (refs==0 path). Applies to
            // every sentinel type whose items buffer just moved to heap
            // malloc; without this, `zz_release_array` returns early for
            // LIT_MAGIC/STACK_MAGIC/ARENA_MAGIC and leaks the items buffer.
            if (a->refs == ZZ_ARRAY_LIT_MAGIC || a->refs == ZZ_ARRAY_STACK_MAGIC
                || a->refs == ZZ_ARRAY_ARENA_MAGIC) {
                a->refs = 0;
            }
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    a->items[a->len++] = zz_clone_for_store(item);
    return zz_unit();
}
zz_value zz_vec_push(zz_value arr, zz_value item, int *err);
// Move-aware push for `x = vec.push(x, e)`: `taken` is owned (the caller
// moved it out of its slot, which now holds unit). A uniquely-owned heap
// array (refs==1) grows in place — no dup, no copy; anything else takes
// the copy-on-write fallback (`zz_vec_push` + release of the taken share,
// which balances the take exactly like the old assign-release did).
zz_value zz_vec_push_take(zz_value taken, zz_value item, int *err);
// Move-aware field push for `s.f = vec.push(s.f, e)` on boxed structs:
// same in-place rule for the field array, copy fallback otherwise.
void zz_object_push_field_take(zz_value *obj, const char *field, zz_value item, int *err);
zz_value zz_vec_pop(zz_value arr, int *err);
zz_value zz_vec_remove(zz_value arr, zz_value idx, int *err);
zz_value zz_vec_insert(zz_value arr, zz_value idx, zz_value item, int *err);
zz_value zz_vec_contains(zz_value arr, zz_value item, int *err);
zz_value zz_vec_sort(zz_value arr, int *err);
zz_value zz_vec_sort_by(zz_value arr, zz_value cmp, int *err);
zz_value zz_vec_sort_by_desc(zz_value arr, zz_value cmp, int *err);
zz_value zz_vec_sort_key(zz_value arr, zz_value key, int *err);
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

// ---- user enum helpers ------------------------------------------------
// True when `v` is a boxed object whose qualified type name equals
// `qualified` (`Token.IntLit`). Used for `.Variant` pattern guards in
// generated `match` code.
int zz_enum_is(const zz_value *v, const char *qualified);
// True for user-enum variant values (unit or single-`value`-payload
// objects with a dotted type name). See `zz_object_is_enum_shape`.
int zz_object_is_enum_shape(const struct zz_object *o);

// ---- match extraction helpers ------------------------------------------
// Returns the payload of a variant, or unit if tag doesn't match.
zz_value zz_match_ok(zz_value v);
zz_value zz_match_err(zz_value v);
zz_value zz_match_some(zz_value v);

// ---- codegen shims -----------------------------------------------------
zz_value zz_range_build(zz_value start, zz_value end);
zz_value zz_range_build_step(zz_value start, zz_value end, zz_value step);
zz_value zz_elvis(zz_value left, zz_value right);

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_COLLECTIONS_H