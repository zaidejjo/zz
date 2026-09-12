// ZZ native runtime — dynamic array (vec) and object dictionary
// implementations, plus boxed structs, Option/Result variants, match
// extraction helpers, and higher-order iterators.
#include "runtime.h"

// ---- arrays ------------------------------------------------------------
zz_value zz_array_new(void) {
    zz_array *a = (zz_array *)calloc(1, sizeof(zz_array));
    a->refs = 1;  // ARC: initial reference count
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

// Arena-aware array constructor. When arena is non-NULL, the zz_array
// header is bump-allocated (no malloc, O(1)). The items buffer still uses
// malloc since it may realloc on growth.
zz_value zz_array_new_arena(zz_arena *arena) {
    zz_array *a;
    if (arena) {
        a = (zz_array *)zz_arena_alloc(arena, sizeof(zz_array), 8);
        memset(a, 0, sizeof(zz_array));
        // Arena-allocated objects don't need refcounting — they're freed
        // in bulk at arena reset. Set refs to a sentinel so zz_release
        // knows not to free them individually.
        a->refs = 0;
    } else {
        a = (zz_array *)calloc(1, sizeof(zz_array));
        a->refs = 1;
    }
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

// Arena-allocated array with pre-sized items buffer. Both header and items
// live on the arena (zero malloc on creation). Uses ZZ_ARRAY_ARENA_MAGIC
// sentinel so zz_vec_append can detect arena items and migrate to heap if
// growth is needed.
zz_value zz_array_new_arena_sized(zz_arena *arena, size_t cap) {
    zz_array *a;
    if (arena) {
        a = (zz_array *)zz_arena_alloc(arena, sizeof(zz_array), 8);
        a->refs = ZZ_ARRAY_ARENA_MAGIC;
        a->len = 0;
        a->cap = cap;
        if (cap > 0) {
            a->items = (zz_value *)zz_arena_alloc(arena, cap * sizeof(zz_value), 8);
        } else {
            a->items = NULL;
        }
    } else {
        a = (zz_array *)calloc(1, sizeof(zz_array));
        a->refs = 1;
        a->len = 0;
        a->cap = cap;
        a->items = cap > 0 ? (zz_value *)malloc(cap * sizeof(zz_value)) : NULL;
    }
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

zz_value zz_array_new_lit(zz_arena *arena, size_t n) {
    zz_array *a;
    if (arena) {
        // Header AND items buffer both bump-allocated: zero realloc, zero
        // malloc. refs = LIT_MAGIC sentinel marks the no-op release path.
        a = (zz_array *)zz_arena_alloc(arena, sizeof(zz_array), 8);
        a->refs = ZZ_ARRAY_LIT_MAGIC;
        a->len = 0;
        a->cap = n;
        // NOTE: when n==0 the items buffer would be a zero-size arena
        // allocation. Passing such a pointer to realloc() later (in
        // zz_vec_append) is invalid — realloc only works with malloc-allocated
        // memory. Set items=NULL instead so zz_vec_append correctly calls
        // realloc(NULL, ...) which is defined as malloc.
        a->items = n > 0 ? (zz_value *)zz_arena_alloc(arena, n * sizeof(zz_value), 8) : NULL;
    } else {
        // Escaping literal: single pre-allocated heap block, normal ARC.
        a = (zz_array *)calloc(1, sizeof(zz_array));
        a->refs = 1;
        a->len = 0;
        a->cap = n;
        a->items = (zz_value *)malloc(n * sizeof(zz_value));
    }
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

// Direct store into a fixed-capacity literal array. The caller guarantees:
//   - the array has at least `len + 1` slots (`new_lit(arena, K)` with K
//     equal to the literal's arity),
//   - the item is a scalar (int/float/bool) needing no refcount.
// Element stores intentionally skip zz_clone: the literal never escapes.
void zz_array_push_lit(zz_array *a, zz_value item) {
    a->items[a->len++] = item;
}

void zz_array_push(zz_array *a, zz_value item) {
    if (a->len == a->cap) {
        size_t nc = a->cap == 0 ? 4 : a->cap * 2;
        // Stack/lit/arena arrays need migration to malloc before realloc.
        if (a->refs == ZZ_ARRAY_STACK_MAGIC || a->refs == ZZ_ARRAY_LIT_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(nc * sizeof(zz_value));
            for (size_t i = 0; i < a->len; i++) new_items[i] = a->items[i];
            a->items = new_items;
            a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, nc * sizeof(zz_value));
        }
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

// Duplicate an array with an INDEPENDENT items buffer (shallow element clone,
// like the VM's `vs.clone()`). Unlike `zz_clone` (which shares the buffer and
// bumps the refcount), callers may freely mutate the returned array without
// affecting the original.
zz_value zz_array_dup(const zz_array *a) {
    zz_value out = zz_array_new();
    for (size_t i = 0; i < a->len; i++) {
        zz_array_push(out.arr, zz_clone(a->items[i]));
    }
    return out;
}

// ---- dicts ---------------------------------------------------------------
zz_value zz_dict_new(void) {
    zz_dict *d = (zz_dict *)calloc(1, sizeof(zz_dict));
    d->refs = 1;  // ARC: initial reference count
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
}

// Arena-aware dict constructor.
zz_value zz_dict_new_arena(zz_arena *arena) {
    zz_dict *d;
    if (arena) {
        d = (zz_dict *)zz_arena_alloc(arena, sizeof(zz_dict), 8);
        memset(d, 0, sizeof(zz_dict));
        d->refs = 0;  // sentinel: arena-allocated, don't individually free
    } else {
        d = (zz_dict *)calloc(1, sizeof(zz_dict));
        d->refs = 1;
    }
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
}

// Arena-aware dict constructor with pre-allocated entries buffer.
// When arena is non-NULL, both the header AND the entries buffer are
// bump-allocated with exactly `hint` slots. zz_dict_set must NOT realloc
// these — the buffer is fixed-capacity and dies at arena reset.
zz_value zz_dict_new_arena_sized(zz_arena *arena, size_t hint) {
    zz_dict *d;
    if (arena) {
        d = (zz_dict *)zz_arena_alloc(arena, sizeof(zz_dict), 8);
        memset(d, 0, sizeof(zz_dict));
        d->refs = ZZ_DICT_ARENA_MAGIC;  // sentinel: arena-allocated entries
        d->cap = hint;
        d->len = 0;
        if (hint > 0) {
            d->entries = (zz_dict_entry *)zz_arena_alloc(arena, hint * sizeof(zz_dict_entry), 8);
            memset(d->entries, 0, hint * sizeof(zz_dict_entry));
        } else {
            d->entries = NULL;
        }
    } else {
        d = (zz_dict *)calloc(1, sizeof(zz_dict));
        d->refs = 1;
        if (hint > 0) {
            d->cap = hint;
            d->entries = (zz_dict_entry *)malloc(hint * sizeof(zz_dict_entry));
        }
    }
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
}

// Index-expression dispatchers: `obj[idx]` read and `obj[idx] = v` write.
// Arrays and dicts only; unsupported tags set *err = 1 and return unit.
zz_value zz_index_get(zz_value obj, zz_value idx, int *err) {
    switch (obj.tag) {
    case ZZ_ARRAY:
        return zz_array_get(obj.arr, idx, err);
    case ZZ_DICT:
        return zz_dict_get(obj.dict, idx, err);
    default:
        *err = 1;
        return zz_unit();
    }
}

// Slice a value by byte indices (array elements or ASCII-compatible strings).
// Missing bounds (unit) mean "from 0" / "to end". Matches the VM for ASCII.
zz_value zz_slice_value(zz_value obj, zz_value start, zz_value end, int *err) {
    int64_t n;
    switch (obj.tag) {
    case ZZ_ARRAY:
        n = (int64_t)obj.arr->len;
        {
            zz_value s = start, e = end;
            if (s.tag == ZZ_UNIT) s = zz_int(0);
            if (e.tag == ZZ_UNIT) e = zz_int(n);
            if (s.tag != ZZ_INT || e.tag != ZZ_INT) {
                if (err) *err = 1;
                return zz_unit();
            }
            return zz_array_slice(obj.arr, s, e, err);
        }
    case ZZ_STR:
        n = (int64_t)obj.s->len;
        {
            int64_t si = 0, ei = n;
            if (start.tag == ZZ_INT) si = start.i;
            if (end.tag == ZZ_INT) ei = end.i;
            if (si < 0) si += n;
            if (ei < 0) ei += n;
            if (si < 0) si = 0;
            if (ei > n) ei = n;
            if (si > ei) si = ei;
            zz_value out = zz_str_new(zz_str_cptr(obj.s) + si, (size_t)(ei - si));
            return out;
        }
    default:
        if (err) *err = 1;
        return zz_unit();
    }
}

void zz_index_set(zz_value obj, zz_value idx, zz_value item, int *err) {
    switch (obj.tag) {
    case ZZ_ARRAY:
        zz_array_set(obj.arr, idx, item, err);
        return;
    case ZZ_DICT:
        zz_dict_set(obj.dict, idx, item);
        *err = 0;
        return;
    default:
        *err = 1;
    }
}

void zz_dict_set(zz_dict *d, zz_value key, zz_value val) {
    if (key.tag != ZZ_STR)
        return;
    for (size_t i = 0; i < d->len; i++) {
        zz_dict_entry *e = &d->entries[i];
        if (e->key->len == key.s->len &&
            memcmp(zz_str_cptr(e->key), zz_str_cptr(key.s), key.s->len) == 0) {
            zz_assign(&e->val, val);
            return;
        }
    }
    if (d->len == d->cap) {
        if (d->refs == ZZ_DICT_ARENA_MAGIC) {
            // Arena-allocated entries buffer is fixed-capacity — cannot grow.
            // This should not happen if the codegen pre-sized correctly.
            return;
        }
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
            memcmp(zz_str_cptr(e->key), zz_str_cptr(key.s), key.s->len) == 0) {
            return zz_clone(e->val);
        }
    }
    *err = 1;
    return zz_unit();
}

size_t zz_dict_len(const zz_dict *d) {
    return d ? d->len : 0;
}

// Materialize an array/range value into a new array of item values.
static zz_value zz_iter_items(zz_value v) {
    if (v.tag == ZZ_ARRAY && v.arr) {
        return zz_array_dup(v.arr);
    }
    if (v.tag == ZZ_TUPLE && v.payload && ((zz_value *)v.payload)->tag == ZZ_ARRAY) {
        return zz_array_dup((*((zz_value *)v.payload)).arr);
    }
    return zz_array_new();
}

// map(items, f) → array of f(item)
zz_value zz_iter_map(zz_value items, zz_value f, int *err) {
    (void)err;
    zz_dispatch_fn fn = zz_closure_target(f);
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    if (!fn) return out;
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value a1[] = { arr.arr->items[i] };
        zz_value r = fn(a1, 1);
        zz_array_push(out.arr, r);
    }
    zz_release(&arr);
    return out;
}

// filter(items, f) → array of items where f(item) is truthy
zz_value zz_iter_filter(zz_value items, zz_value f, int *err) {
    (void)err;
    zz_dispatch_fn fn = zz_closure_target(f);
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    if (!fn) return out;
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value a1[] = { arr.arr->items[i] };
        zz_value r = fn(a1, 1);
        if (zz_truthy(r)) {
            zz_array_push(out.arr, zz_clone(arr.arr->items[i]));
        }
    }
    zz_release(&arr);
    return out;
}

// enumerate(items) → array of tuples (idx, item)
zz_value zz_iter_enumerate(zz_value items, int *err) {
    (void)err;
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value pair = zz_tuple((zz_value){ZZ_INT, {.i = (int64_t)i}}, arr.arr->items[i]);
        zz_array_push(out.arr, pair);
    }
    zz_release(&arr);
    return out;
}

// zip(a, b) → array of tuples (x, y)
zz_value zz_iter_zip(zz_value a, zz_value b, int *err) {
    (void)err;
    zz_value ar = zz_iter_items(a);
    zz_value br = zz_iter_items(b);
    zz_value out = zz_array_new();
    size_t n = ar.arr->len < br.arr->len ? ar.arr->len : br.arr->len;
    for (size_t i = 0; i < n; i++) {
        zz_value pair = zz_tuple(ar.arr->items[i], br.arr->items[i]);
        zz_array_push(out.arr, pair);
    }
    zz_release(&ar);
    zz_release(&br);
    return out;
}

// Tuple value: payload = heap array holding the item values.
zz_value zz_tuple(zz_value a, zz_value b) {
    zz_value arr = zz_array_new();
    zz_array_push(arr.arr, zz_clone(a));
    zz_array_push(arr.arr, zz_clone(b));
    zz_value *slot = (zz_value *)malloc(sizeof(zz_value));
    *slot = arr;
    zz_value v;
    v.tag = ZZ_TUPLE;
    v.payload = slot;
    return v;
}

// range(start, stop, step) — materialize to an array of ints.
zz_value zz_range3(zz_value a, zz_value b, zz_value c, int *err) {
    (void)err;
    if (a.tag != ZZ_INT || b.tag != ZZ_INT || c.tag != ZZ_INT) {
        return zz_array_new();
    }
    int64_t start = a.i, end = b.i, step = c.i;
    if (step == 0) return zz_array_new();
    zz_value out = zz_array_new();
    if (step > 0) {
        for (int64_t i = start; i < end; i += step) {
            zz_array_push(out.arr, (zz_value){ZZ_INT, {.i = i}});
        }
    } else {
        for (int64_t i = start; i > end; i += step) {
            zz_array_push(out.arr, (zz_value){ZZ_INT, {.i = i}});
        }
    }
    return out;
}

// ---- variant constructors (Option / Result) ---------------------------
// Store the inner value on the heap so match can extract it via payload pointer.

zz_value zz_variant_some(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_OPTION_SOME, {.payload = p}};
}

zz_value zz_variant_ok(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_RESULT_OK, {.payload = p}};
}

zz_value zz_variant_err(zz_value inner) {
    zz_value *p = (zz_value *)malloc(sizeof(zz_value));
    *p = inner;
    return (zz_value){ZZ_RESULT_ERR, {.payload = p}};
}

// Release helper for variant payloads.
void zz_release_variant(zz_value *v) {
    if (v->payload) {
        zz_release(v->payload);
        free(v->payload);
        v->payload = NULL;
    }
}

// ---- boxed struct (object) constructors and accessors --------------------

zz_value zz_object_new(const char *type_name, zz_value *field_names, size_t n) {
    // Allocate: refs + type_name + len + (n * 2 fields: name, value pairs)
    zz_object *obj = (zz_object *)malloc(sizeof(zz_object) + n * 2 * sizeof(zz_value));
    obj->refs = 1;
    obj->type_name = type_name;
    obj->len = n;
    // Initialize all field slots to unit
    for (size_t i = 0; i < n * 2; i++) {
        obj->fields[i] = (zz_value){ZZ_UNIT, {.i = 0}};
    }
    // Store field names in the alternating slots
    for (size_t i = 0; i < n; i++) {
        obj->fields[i * 2] = field_names[i]; // name (zz_value, should be str)
        // fields[i * 2 + 1] is the value slot (initialized to unit above)
    }
    return (zz_value){ZZ_OBJECT, {.obj = obj}};
}

void zz_object_set_field(zz_value *obj, const char *name, zz_value val) {
    if (obj->tag != ZZ_OBJECT || !obj->obj) return;
    zz_object *o = obj->obj;
    for (size_t i = 0; i < o->len; i++) {
        zz_value *fname = &o->fields[i * 2];
        if (fname->tag == ZZ_STR && strcmp(zz_str_cptr(fname->s), name) == 0) {
            zz_value *slot = &o->fields[i * 2 + 1];
            zz_release(slot);
            *slot = zz_clone(val);
            return;
        }
    }
}

zz_value zz_object_get_field(zz_value *obj, const char *name) {
    if (obj->tag != ZZ_OBJECT || !obj->obj) return zz_unit();
    zz_object *o = obj->obj;
    for (size_t i = 0; i < o->len; i++) {
        zz_value *fname = &o->fields[i * 2];
        if (fname->tag == ZZ_STR && strcmp(zz_str_cptr(fname->s), name) == 0) {
            return zz_clone(o->fields[i * 2 + 1]);
        }
    }
    return zz_unit();
}

// Release helper for boxed objects.
void zz_release_object(zz_value *v) {
    if (v->tag != ZZ_OBJECT || !v->obj) return;
    zz_object *o = v->obj;
    if (o->refs == 0) return; // already freed
    if (--o->refs == 0) {
        for (size_t i = 0; i < o->len; i++) {
            zz_release(&o->fields[i * 2]);     // name (str)
            zz_release(&o->fields[i * 2 + 1]); // value
        }
        free(o);
    }
    v->obj = NULL;
}

// ---- match extraction helpers ------------------------------------------
// Returns the payload of a variant, or unit if tag doesn't match.

zz_value zz_match_ok(zz_value v) {
    if (v.tag == ZZ_RESULT_OK && v.payload) return zz_clone(*v.payload);
    return zz_unit();
}

zz_value zz_match_err(zz_value v) {
    if (v.tag == ZZ_RESULT_ERR && v.payload) return zz_clone(*v.payload);
    return zz_unit();
}

zz_value zz_match_some(zz_value v) {
    if (v.tag == ZZ_OPTION_SOME && v.payload) return zz_clone(*v.payload);
    return zz_unit();
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

// zz_elvis(left, right) — unwrap Option/Result on the left, else return right.
// Mirrors the VM's `??` operator which unwraps .some(v) and .ok(v).
zz_value zz_elvis(zz_value left, zz_value right) {
    if (left.tag == ZZ_OPTION_SOME && left.payload)
        return zz_clone(*left.payload);
    if (left.tag == ZZ_OPTION_NONE)
        return zz_clone(right);
    if (left.tag == ZZ_RESULT_OK && left.payload)
        return zz_clone(*left.payload);
    if (left.tag == ZZ_RESULT_ERR)
        return zz_clone(right);
    // For non-optional/result types, fall back to truthiness check.
    if (zz_truthy(left))
        return zz_clone(left);
    return zz_clone(right);
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

// vec.append(arr, item) — append item to array in place.
// NOTE: the codegen uses this as the array-literal builder (calls ignore the
// return). Kept as a mutator for that contract; `vec.push` is the functional
// copy-on-write variant that matches the VM.
zz_value zz_vec_append(zz_value arr, zz_value item, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    if (a->len >= a->cap) {
        size_t new_cap = a->cap ? a->cap * 2 : 8;
        // LIT_MAGIC / ARENA_MAGIC arrays have items from arena — realloc() on
        // arena memory is invalid. Also handle n=0 case where items is NULL.
        // Migrate to malloc, switch to refs=0 (arena-allocated header sentinel)
        // so zz_release knows items is malloc'd but header is still arena.
        if (a->refs == ZZ_ARRAY_LIT_MAGIC || a->refs == ZZ_ARRAY_STACK_MAGIC
            || a->refs == ZZ_ARRAY_ARENA_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            // Copy existing elements if any.
            for (size_t i = 0; i < a->len; i++) {
                new_items[i] = a->items[i];
            }
            a->items = new_items;
            a->refs = 0;  // Arena-allocated header, malloc'd items
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    a->items[a->len++] = zz_clone(item);
    return zz_unit();
}

// vec.push(arr, item) — returns a NEW array with item appended
// (matches the VM: the input array is left untouched).
zz_value zz_vec_push(zz_value arr, zz_value item, int *err) {
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_value out = zz_array_dup(arr.arr);
    zz_array *a = out.arr;
    if (a->len >= a->cap) {
        size_t new_cap = a->cap ? a->cap * 2 : 8;
        if (a->refs == ZZ_ARRAY_LIT_MAGIC || a->refs == ZZ_ARRAY_STACK_MAGIC
            || a->refs == ZZ_ARRAY_ARENA_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            for (size_t i = 0; i < a->len; i++) {
                new_items[i] = a->items[i];
            }
            a->items = new_items;
            a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    a->items[a->len++] = zz_clone(item);
    return out;
}

// vec.pop(arr) — remove and return a NEW array without the last element
// (matches the VM: the input array is left untouched).
zz_value zz_vec_pop(zz_value arr, int *err) {
    if (arr.tag != ZZ_ARRAY || arr.arr->len == 0) {
        if (err) *err = 1;  // VM errors on empty pop
        return zz_unit();
    }
    zz_value out = zz_array_dup(arr.arr);
    zz_array *a = out.arr;
    zz_release(&a->items[a->len - 1]);
    a->len--;
    return out;
}

// vec.remove(arr, idx) — returns a NEW array with element at idx removed.
zz_value zz_vec_remove(zz_value arr, zz_value idx, int *err) {
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    int64_t i = idx.tag == ZZ_INT ? idx.i : 0;
    int64_t len = (int64_t)a->len;
    if (i < 0) i += len;  // VM supports negative indices
    if (i < 0 || i >= len) {
        if (err) *err = 1;
        return zz_unit();
    }
    zz_value out = zz_array_dup(arr.arr);
    zz_array *o = out.arr;
    zz_release(&o->items[i]);
    for (size_t j = (size_t)i; j < o->len - 1; j++) {
        o->items[j] = o->items[j + 1];
    }
    o->len--;
    return out;
}

// vec.insert(arr, idx, item) — returns a NEW array with item inserted.
zz_value zz_vec_insert(zz_value arr, zz_value idx, zz_value item, int *err) {
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    int64_t i = idx.tag == ZZ_INT ? idx.i : 0;
    int64_t len = (int64_t)a->len;
    if (i < 0) i += len;  // VM supports negative indices
    if (i < 0 || i > len) {
        if (err) *err = 1;
        return zz_unit();
    }
    zz_value out = zz_array_dup(arr.arr);
    zz_array *o = out.arr;
    if (o->len >= o->cap) {
        size_t new_cap = o->cap ? o->cap * 2 : 8;
        if (o->refs == ZZ_ARRAY_STACK_MAGIC || o->refs == ZZ_ARRAY_LIT_MAGIC || o->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            for (size_t j = 0; j < o->len; j++) new_items[j] = o->items[j];
            o->items = new_items;
            o->refs = 0;
        } else {
            o->items = (zz_value *)realloc(o->items, new_cap * sizeof(zz_value));
        }
        o->cap = new_cap;
    }
    for (size_t j = o->len; j > (size_t)i; j--) {
        o->items[j] = o->items[j - 1];
    }
    o->items[i] = zz_clone(item);
    o->len++;
    return out;
}

// vec.contains(arr, item) → bool
zz_value zz_vec_contains(zz_value arr, zz_value item, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return (zz_value){ZZ_BOOL, {.b = false}};
    for (size_t i = 0; i < arr.arr->len; i++) {
        if (zz_truthy(zz_binop(ZZOP_EQ, zz_clone(arr.arr->items[i]), zz_clone(item))))
            return (zz_value){ZZ_BOOL, {.b = true}};
    }
    return (zz_value){ZZ_BOOL, {.b = false}};
}

// vec.sort(arr) → sorted array (new array, original unchanged)
zz_value zz_vec_sort(zz_value arr, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return arr;
    size_t n = arr.arr->len;
    if (n <= 1) return zz_clone(arr);
    // Create a new array with cloned elements.
    zz_value sorted = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(sorted.arr, zz_clone(arr.arr->items[i]));
    }
    // Simple insertion sort (fine for small arrays; larger ones use qsort).
    for (size_t i = 1; i < n; i++) {
        zz_value key = sorted.arr->items[i];
        size_t j = i;
        while (j > 0 && zz_truthy(zz_binop(ZZOP_LT, zz_clone(key), zz_clone(sorted.arr->items[j-1])))) {
            sorted.arr->items[j] = sorted.arr->items[j-1];
            j--;
        }
        sorted.arr->items[j] = key;
    }
    return sorted;
}

// vec.reverse(arr) → reversed array (new array, original unchanged)
zz_value zz_vec_reverse(zz_value arr, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return arr;
    size_t n = arr.arr->len;
    zz_value rev = zz_array_new();
    for (size_t i = n; i > 0; i--) {
        zz_array_push(rev.arr, zz_clone(arr.arr->items[i-1]));
    }
    return rev;
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
        if (strcmp(zz_str_cptr(d.dict->entries[i].key), zz_str_cptr(key.s)) == 0)
            return (zz_value){ZZ_BOOL, {.b = true}};
    }
    return (zz_value){ZZ_BOOL, {.b = false}};
}

// option.expect(opt, msg) — unwrap .some(v) or panic with msg
zz_value zz_option_expect(zz_value opt, zz_value msg, int *err) {
    (void)err;
    if (opt.tag == ZZ_OPTION_SOME && opt.payload)
        return zz_clone(*opt.payload);
    // Panic: print error message and exit.
    const char *m = (msg.tag == ZZ_STR) ? zz_str_cptr(msg.s) : "expect failed";
    fprintf(stderr, "error: %s\n", m);
    exit(1);
}

// result.expect(res, msg) — unwrap .ok(v) or panic with msg
zz_value zz_result_expect(zz_value res, zz_value msg, int *err) {
    (void)err;
    if (res.tag == ZZ_RESULT_OK && res.payload)
        return zz_clone(*res.payload);
    // If it's a .err, print the error value too.
    if (res.tag == ZZ_RESULT_ERR && res.payload) {
        char *estr = zz_value_to_string(res.payload);
        fprintf(stderr, "error: %s: %s\n",
                (msg.tag == ZZ_STR) ? zz_str_cptr(msg.s) : "expect failed", estr);
        free(estr);
    } else {
        fprintf(stderr, "error: %s\n",
                (msg.tag == ZZ_STR) ? zz_str_cptr(msg.s) : "expect failed");
    }
    exit(1);
}

// fs.read(path)
