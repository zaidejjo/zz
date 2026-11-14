// ZZ native runtime — dynamic array (vec) and object dictionary
// implementations, plus boxed structs, Option/Result variants, match
// extraction helpers, and higher-order iterators.
#include "runtime.h"

// ---- arena healing -------------------------------------------------------
// Retaining stores (array/dict/object slots, `zz_assign`, `std.cell`
// pool) must never alias arena-owned values: the per-iteration
// `zz_arena_reset` reuses the buffer, so a stored alias reads back
// garbage on the next iteration — the nested `[[str]]` first-element
// corruption + SIGSEGV class (loop-built inner arrays stored into an
// outer array, then the arena reset overwrote them), and the `std.cell`
// variant (loop-built values stored into cells, then read back across
// iterations — garbage data, garbage `len` → wild free, issue #410).
// Strings already heal via `zz_str_heal_arena`; arrays, dicts and boxed
// structs need the same treatment (deep: nested arena values copied
// recursively, heap elements shared via `zz_clone`).
//
// Two flavors, mirroring the string path:
//   - `zz_clone_for_store` (share convention, callers pass borrowed):
//     arena → fresh heap copy, heap → `zz_clone` (bump).
//   - `zz_heal_for_move` (move convention, callers transfer ownership —
//     `zz_array_push`, `zz_assign`): arena → fresh heap copy, heap →
//     unchanged (adopted, no bump).
//
// Depth-capped at 32 (like ZZ_EMBED_MAX_DEPTH / ZZ_PRINT_MAX_DEPTH):
// pathological nesting falls back to sharing. Arena-fresh values cannot
// be cyclic without an escaping store (which heals), so the cap is a
// backstop, never a behavior.
#define ZZ_HEAL_MAX_DEPTH 32

int zz_array_is_arena(const zz_array *a) {
    if (!a) return 0;
    return a->refs == 0 || a->refs == ZZ_ARRAY_STACK_MAGIC ||
           a->refs == ZZ_ARRAY_LIT_MAGIC || a->refs == ZZ_ARRAY_ARENA_MAGIC;
}

int zz_dict_is_arena(const zz_dict *d) {
    if (!d) return 0;
    return d->refs == 0 || d->refs == ZZ_DICT_ARENA_MAGIC;
}

// Out-of-line external definition (declared in collections.h) so the symbol
// is linkable from the C-ABI Rust FFI (`zz_native_rt::pg::dict_insert`).
// `static inline` would give it internal linkage only.
void zz_index_set(zz_value *obj, zz_value idx, zz_value item, int *err) {
    if (!obj) { if (err) *err = 1; return; }
    if (obj->tag == ZZ_ARRAY && obj->arr && obj->arr->refs != 1) {
        zz_value fresh = zz_array_dup(obj->arr);
        if (!fresh.arr) { if (err) *err = 1; zz_release(&item); return; }
        zz_release_array(obj->arr);
        obj->arr = fresh.arr;
    } else if (obj->tag == ZZ_DICT && obj->dict && obj->dict->refs != 1) {
        // Arena dicts (`refs == ZZ_DICT_ARENA_MAGIC`, e.g. `d := {}`) are
        // fixed-capacity and cannot grow in place. On first write dup them
        // to a growable heap dict so `d[k] = v` persists (matches the VM).
        zz_value fresh = zz_dict_dup_value(obj->dict);
        if (!fresh.dict) { if (err) *err = 1; zz_release(&item); return; }
        zz_release_dict(obj->dict);
        obj->dict = fresh.dict;
    }
    // A container tag with a NULL backing object cannot be indexed — bail
    // before dereferencing (defensive; constructors always set it).
    if ((obj->tag == ZZ_ARRAY && !obj->arr) || (obj->tag == ZZ_DICT && !obj->dict)) {
        if (err) *err = 1;
        zz_release(&item);
        return;
    }
    switch (obj->tag) {
    case ZZ_ARRAY:
        zz_array_set(obj->arr, idx, item, err);
        if (err && *err) { zz_release(&item); return; }
        // `zz_array_set` -> `zz_assign` already retained `item` for the
        // slot (including ZZ_STR since core.h's zz_assign retain list now
        // covers it). Drop the caller's share here so the slot holds
        // exactly one share of every type, strings included.
        zz_release(&item);
        return;
    case ZZ_DICT:
        zz_dict_set(obj->dict, idx, item);
        // Same contract as the array arm: consume `item` for every type.
        zz_release(&item);
        if (err) *err = 0;
        return;
    default:
        if (err) *err = 1;
    }
}

// True when storing `v` must copy instead of share: top-level arena
// ownership, a boxed struct transitively containing one, or a
// variant/tuple wrapper around a value that does.
// Heap-rooted arrays/dicts are guaranteed clean (every ingress path
// heals), so no deep element walk is needed — O(1) for containers,
// O(wrapper depth) otherwise. Objects walk their fields: a struct is
// heap-allocated itself, but its field slots may alias arena values
// (e.g. `Outer{inner: Slot{val: arena_arr}}` or a `TT` struct stored
// into a cell), and the loop-arena reset would corrupt those aliases.
static int value_needs_heal(zz_value v, int depth) {
    if (depth > ZZ_HEAL_MAX_DEPTH) return 0;
    switch (v.tag) {
    case ZZ_STR:
        return v.s && !v.s->interned && v.s->refs == 0;
    case ZZ_ARRAY:
        return v.arr && zz_array_is_arena(v.arr);
    case ZZ_DICT:
        return v.dict && zz_dict_is_arena(v.dict);
    case ZZ_OBJECT: {
        if (!v.obj) return 0;
        for (size_t i = 0; i < v.obj->len * 2; i++) {
            if (value_needs_heal(v.obj->fields[i], depth + 1)) return 1;
        }
        return 0;
    }
    case ZZ_TUPLE:
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        return v.payload && value_needs_heal(*v.payload, depth + 1);
    default:
        return 0;
    }
}

static zz_value heal_deep_copy(zz_value v, int depth) {
    if (depth > ZZ_HEAL_MAX_DEPTH) return zz_clone(v);
    switch (v.tag) {
    case ZZ_STR:
        if (v.s && !v.s->interned && v.s->refs == 0) return zz_str_heal_arena(v);
        return zz_clone(v);
    case ZZ_ARRAY: {
        if (!v.arr || !zz_array_is_arena(v.arr)) return zz_clone(v);
        zz_value out = zz_array_new();
        for (size_t i = 0; i < v.arr->len; i++) {
            // Elements arrive owned (fresh copy or bumped clone) and
            // `zz_array_push` adopts (move convention) — exactly one
            // share lands in the slot. Push also re-heals, which is a
            // no-op here (outputs are heap-owned), doubling only a few
            // tag branches, never correctness.
            zz_array_push(out.arr, heal_deep_copy(v.arr->items[i], depth + 1));
        }
        return out;
    }
    case ZZ_DICT: {
        if (!v.dict || !zz_dict_is_arena(v.dict)) return zz_clone(v);
        zz_value out = zz_dict_new();
        for (size_t i = 0; i < v.dict->len; i++) {
            zz_value k = heal_deep_copy(v.dict->entries[i].key, depth + 1);
            zz_value val = heal_deep_copy(v.dict->entries[i].val, depth + 1);
            // `zz_dict_set` adopts the value as-is (move convention) and
            // retains the key, so drop our key temp only: the dict keeps
            // exactly one share of each.
            zz_dict_set(out.dict, k, val);
            zz_release(&k);
        }
        return out;
    }
    case ZZ_TUPLE: {
        if (!v.payload) return v;
        // Tuple boxes predate the payload refcount: `ZZ_TUPLE` has no
        // clone/release arms (they leak rather than double-free), so they
        // keep the plain allocation here.
        if (!value_needs_heal(*v.payload, depth + 1)) return zz_clone(v);
        zz_value *slot = (zz_value *)malloc(sizeof(zz_value));
        if (!slot) return zz_clone(v);
        *slot = heal_deep_copy(*v.payload, depth + 1);
        zz_value out = v;
        out.payload = slot;
        return out;
    }
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON: {
        if (!v.payload) return v;
        // Clean payloads stay shared (no rebuild churn); only
        // arena-containing payloads get a fresh box + healed inner.
        if (!value_needs_heal(*v.payload, depth + 1)) return zz_clone(v);
        zz_value *slot = zz_vbox_new(heal_deep_copy(*v.payload, depth + 1));
        if (!slot) return zz_clone(v);
        zz_value out = v;
        out.payload = slot;
        return out;
    }
    case ZZ_OBJECT: {
        if (!v.obj) return v;
        // Clean structs stay shared (header bump only). Only structs
        // transitively containing arena values get a fresh header with
        // healed fields — otherwise a struct stored into a cell (or a
        // container slot) would dangle its inner aliases past the reset.
        if (!value_needs_heal(v, depth)) return zz_clone(v);
        size_t n = v.obj->len;
        zz_object *o = (zz_object *)malloc(sizeof(zz_object) + n * 2 * sizeof(zz_value));
        if (!o) return zz_clone(v);
        o->refs = 1;
        o->type_name = v.obj->type_name; // static C string: share
        o->len = n;
        for (size_t i = 0; i < n * 2; i++) {
            o->fields[i] = heal_deep_copy(v.obj->fields[i], depth + 1);
        }
        zz_value out;
        out.tag = ZZ_OBJECT;
        out.obj = o;
        return out;
    }
    default:
        return zz_clone(v);
    }
}

zz_value zz_heal_for_move(zz_value v) {
    if (value_needs_heal(v, 0)) return heal_deep_copy(v, 0);
    return v;
}

// Ownership-sharing store of a value into a container slot: arena-owned
// values (strings, arrays, dicts, arena-containing structs/wrappers)
// are healed to independent heap copies (else the loop-arena reset
// corrupts the stored element); every other value is shared via zz_clone.
zz_value zz_clone_for_store(zz_value v) {
    if (value_needs_heal(v, 0)) return heal_deep_copy(v, 0);
    return zz_clone(v);
}
zz_value zz_array_new(void) {
    zz_array *a = (zz_array *)calloc(1, sizeof(zz_array));
    a->refs = 1;  // ARC: initial reference count
    zz_value v;
    v.tag = ZZ_ARRAY;
    v.arr = a;
    return v;
}

// Heap array constructor with a pre-allocated items buffer. Eliminates the
// calloc + realloc cascade when the caller knows the final size up front
// (e.g. `zz_array_dup`): all `cap` inserts fit without growth. The buffer
// stays growable beyond `cap`.
zz_value zz_array_new_sized(size_t cap) {
    zz_array *a = (zz_array *)calloc(1, sizeof(zz_array));
    a->refs = 1;
    a->len = 0;
    a->cap = cap;
    a->items = cap > 0 ? (zz_value *)malloc(cap * sizeof(zz_value)) : NULL;
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

// Ensure the array has capacity for at least `additional` more elements.
// Caller must hold the only share (refs == 1); grows in place with zero
// copies when capacity suffices, realloc when growth is needed. Used by
// list comprehensions over counted ranges to pre-size the result array.
void zz_array_reserve(zz_value arr, size_t additional, int *err) {
    if (arr.tag != ZZ_ARRAY || !arr.arr) { if (err) *err = 1; return; }
    zz_array *a = arr.arr;
    // refs == 1 excludes arena/lit/stack sentinels and any live borrow;
    // the buffer is malloc'd and safe to realloc.
    if (a->refs != 1) { if (err) *err = 1; return; }
    size_t need = a->len + additional;
    if (need <= a->cap) { if (err) *err = 0; return; }
    size_t nc = a->cap == 0 ? 8 : a->cap;
    while (nc < need) nc *= 2;
    zz_value *ni = (zz_value *)realloc(a->items, nc * sizeof(zz_value));
    if (!ni) { if (err) *err = 1; return; }
    a->items = ni;
    a->cap = nc;
    if (err) *err = 0;
}

void zz_array_push(zz_array *a, zz_value item) {
    if (a->len == a->cap) {
        size_t nc;
        if (a->cap == 0) {
            nc = 4;
        } else if (a->cap > SIZE_MAX / 2) {
            fprintf(stderr, "zz error: array capacity overflow\n");
            exit(1);
        } else {
            nc = a->cap * 2;
        }
        // Stack/lit/arena arrays need migration to malloc before realloc
        // (realloc on arena/stack memory corrupts the heap — SIGSEGV
        // class; ARENA_MAGIC was missing here while zz_vec_append
        // handled it).
        if (a->refs == ZZ_ARRAY_STACK_MAGIC || a->refs == ZZ_ARRAY_LIT_MAGIC
            || a->refs == ZZ_ARRAY_ARENA_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(nc * sizeof(zz_value));
            for (size_t i = 0; i < a->len; i++) new_items[i] = a->items[i];
            // Stack/lit/arena items live on the stack or an arena — they must
            // NOT be freed (free() on stack/arena memory corrupts the heap).
            // They're reclaimed when the frame/arena resets. On later grows
            // `a->refs` is 0/1 so we take the realloc branch above.
            a->items = new_items;
            // Preserve an accurate `refs == 1` (fresh heap array claiming
            // its first buffer): it proves single ownership, which the
            // move-aware push relies on. Anything else becomes 0.
            if (a->refs != 1) a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, nc * sizeof(zz_value));
            if (!a->items) {
                fprintf(stderr, "zz: out of memory\n");
                exit(1);
            }
        }
        a->cap = nc;
    }
    // Move convention (callers transfer ownership): arena-owned values
    // heal to heap copies so the stored element survives the loop-arena
    // reset; heap values are adopted as-is.
    a->items[a->len++] = zz_heal_for_move(item);
}

size_t zz_array_len(const zz_array *a) {
    return a ? a->len : 0;
}

// FFI bridge for the Rust static library: positional array read without
// requiring Rust to mirror the zz_array layout. Cloned item, or unit for
// non-arrays and out-of-range indices (with *err set).
zz_value zz_array_at(zz_value arr, size_t i, int *err) {
    *err = 0;
    if (arr.tag != ZZ_ARRAY || arr.arr == NULL || i >= arr.arr->len) {
        *err = 1;
        return zz_unit();
    }
    return zz_clone(arr.arr->items[i]);
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
    zz_value out = zz_array_new_sized(a ? a->len : 0);
    for (size_t i = 0; i < (a ? a->len : 0); i++) {
        zz_array_push(out.arr, zz_clone(a->items[i]));
    }
    return out;
}

// ---- byte buffers ----------------------------------------------------------
// Contiguous `bytes`: a refcounted backing store (`zz_bytes_buf`) plus a
// per-value window (`off`/`len`). Slices share the store (O(1), zero-copy);
// buffers are immutable so sharing is always sound.

zz_bytes_buf *zz_bytes_buf_new(size_t len) {    zz_bytes_buf *buf =
        (zz_bytes_buf *)malloc(sizeof(zz_bytes_buf) + (len > 0 ? len : 1));
    if (!buf) return NULL;
    buf->refs = 1;
    buf->len = len;
    return buf;
}

zz_value zz_bytes_wrap(zz_bytes_buf *buf, size_t off, size_t len) {
    zz_bytes *b = (zz_bytes *)malloc(sizeof(zz_bytes));
    if (!b) {
        if (__atomic_sub_fetch(&buf->refs, 1, __ATOMIC_ACQ_REL) == 0) free(buf);
        return zz_unit();
    }
    b->refs = 1;
    b->buf = buf;
    b->off = off;
    b->len = len;
    return (zz_value){ZZ_BYTES, {.bytes = b}};
}

zz_value zz_bytes_new(const unsigned char *src, size_t len) {
    zz_bytes_buf *buf = zz_bytes_buf_new(len);
    if (!buf) return zz_unit();
    if (len && src) memcpy(buf->data, src, len);
    return zz_bytes_wrap(buf, 0, len);
}

zz_value zz_bytes_take(unsigned char *data, size_t len) {
    zz_bytes_buf *buf = zz_bytes_buf_new(len);
    if (!buf) {
        free(data);
        return zz_unit();
    }
    if (len && data) memcpy(buf->data, data, len);
    free(data);
    return zz_bytes_wrap(buf, 0, len);
}

zz_value zz_bytes_slice(const zz_bytes *b, int64_t s, int64_t e) {
    if (!b || !b->buf) return zz_unit();
    int64_t n = (int64_t)b->len;
    if (s < 0) s += n;
    if (e < 0) e += n;
    if (s < 0) s = 0;
    if (e > n) e = n;
    if (s > e) s = e;
    __atomic_add_fetch(&b->buf->refs, 1, __ATOMIC_RELAXED);
    return zz_bytes_wrap(b->buf, b->off + (size_t)s, (size_t)(e - s));
}

// Borrow the window bytes for FFI readers (mirrors zz_str_view): sets
// (*out_ptr, *out_len); null/0 for non-bytes. The view borrows the
// shared store — copy before retaining beyond the call.
void zz_bytes_view(zz_value v, const unsigned char **out_ptr, size_t *out_len) {
    if (v.tag != ZZ_BYTES || v.bytes == NULL || v.bytes->buf == NULL) {
        *out_ptr = NULL;
        *out_len = 0;
        return;
    }
    *out_ptr = v.bytes->buf->data + v.bytes->off;
    *out_len = v.bytes->len;
}

void zz_retain_bytes(zz_bytes *b) {
    if (!b) return;
    __atomic_add_fetch(&b->refs, 1, __ATOMIC_RELAXED);}

void zz_release_bytes(zz_bytes *b) {
    if (!b) return;
    if (__atomic_sub_fetch(&b->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        if (b->buf && __atomic_sub_fetch(&b->buf->refs, 1, __ATOMIC_ACQ_REL) == 0) {
            free(b->buf);
        }
        free(b);
    }
}

// ---- dict key index ------------------------------------------------------
// The VM keeps a dict as an insertion-ordered entry list plus a lazy
// `HashMap<DictKey, usize>` (int/bool/str keys only). The C twin mirrors
// that shape: `idx` caches entry positions, entries stay authoritative.
// Keys outside that set are never indexed, so they always take the linear
// lane — same lookup result, just without the acceleration.





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
        // The entries buffer is fixed-capacity, so pre-size the key index to
        // the same hint (both live on the arena and die with it).
        if (hint > ZZ_DICT_INDEX_THRESHOLD) {
            zz_dict_idx_build(d, arena, zz_dict_cap_for(hint));
        }
    } else {
        d = (zz_dict *)calloc(1, sizeof(zz_dict));
        d->refs = 1;
        if (hint > 0) {
            d->cap = hint;
            d->entries = (zz_dict_entry *)malloc(hint * sizeof(zz_dict_entry));
            // Zero the slots: `zz_dict_set` releases `entry->val` through
            // `zz_assign`, which reads the tag. Uninitialized slots could
            // carry a stale refcounted tag/pointer from a recycled block
            // and free a live string (heap corruption).
            memset(d->entries, 0, hint * sizeof(zz_dict_entry));
        }
        // Literals and dict copies know their size up front: allocate the
        // index now so the inserts never rebuild it.
        if (hint > ZZ_DICT_INDEX_THRESHOLD) {
            zz_dict_idx_build(d, NULL, zz_dict_cap_for(hint));
        }
    }
    zz_value v;
    v.tag = ZZ_DICT;
    v.dict = d;
    return v;
}

// Heap dict constructor with pre-allocated entries buffer. Eliminates
// the calloc + realloc cascade for dict literals that escape the loop
// arena (wave 2/4 of memory_alloc when heap-allocated): all `n` inserts
// fit without growth. Unlike the arena path the buffer stays growable.
zz_value zz_dict_new_sized(size_t hint) {
    return zz_dict_new_arena_sized(NULL, hint);
}

// Index-expression dispatchers: `obj[idx]` read and `obj[idx] = v` write.
// Arrays, bytes, strings and dicts; unsupported tags set *err = 1 and
// return unit.
// (zz_index_get lives inline in collections.h; only the setter, which is
// never loop-hot, stays out-of-line here.)

// Slice a value by char/element indices (arrays count elements, strings
// count chars like the VM). Missing bounds (unit) mean "from 0" / "to end".
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
    case ZZ_BYTES: {
        int64_t n = obj.bytes ? (int64_t)obj.bytes->len : 0;
        int64_t s = start.tag == ZZ_INT ? start.i : 0;
        int64_t e = end.tag == ZZ_INT ? end.i : n;
        return zz_bytes_slice(obj.bytes, s, e);
    }
    case ZZ_STR: {
        int64_t nchars = (int64_t)zz_str_char_len(obj.s);
        int64_t si = 0, ei = nchars;
        if (start.tag == ZZ_INT) si = start.i;
        if (end.tag == ZZ_INT) ei = end.i;
        if (si < 0) si += nchars;
        if (ei < 0) ei += nchars;
        if (si < 0) si = 0;
        if (ei > nchars) ei = nchars;
        if (si > ei) si = ei;
        // Single UTF-8 pass locating both bounds instead of two separate
        // O(n) scans (si then ei). Common case: scan to max(si, ei) once.
        size_t byte_lo = obj.s->len, byte_hi = obj.s->len;
        const unsigned char *p = (const unsigned char *)zz_str_cptr(obj.s);
        size_t n = obj.s->len, i = 0;
        int64_t c = 0;
        int pending = 0;
        if (si < nchars) pending |= 1;    // need lo
        if (ei < nchars && ei > si) pending |= 2;  // need hi (ei==si ⇒ empty)
        int got_lo = 0, got_hi = 0;
        while (i < n && pending) {
            size_t l = zz_utf8_seq_len(p + i, n - i);
            if ((pending & 1) && !got_lo && c == si) { byte_lo = i; got_lo = 1; pending &= ~1; }
            if ((pending & 2) && !got_hi && c == ei) { byte_hi = i; got_hi = 1; pending &= ~2; }
            i += l;
            c++;
        }
        if (ei <= si) byte_hi = byte_lo;
        zz_value out = zz_str_new((const char *)p + byte_lo, byte_hi - byte_lo);
        return out;
    }
    default:
        if (err) *err = 1;
        return zz_unit();
    }
}

// Duplicate a dict with an INDEPENDENT entries buffer (shallow value
// clone, like `zz_array_dup`): callers may mutate the result without
// affecting the original. Arena-owned keys/values heal to heap via
// `zz_dict_set`'s own conventions (keys bumped-or-healed, values
// adopted from a prior `zz_clone`), so each side owns exactly one
// share of everything stored.
zz_value zz_dict_dup_value(const zz_dict *d) {
    if (!d) return zz_unit();
    zz_value out = zz_dict_new_sized(d->len);
    if (!out.dict) return zz_unit();
    for (size_t i = 0; i < d->len; i++) {
        // Keys are borrowed (the setter takes its own share); values are
        // cloned shares handed over per the setter's move convention. The
        // setter also heals arena-backed keys/values to heap, so the copy
        // outlives the arena that built the original.
        zz_value v = zz_clone(d->entries[i].val);
        zz_dict_set(out.dict, d->entries[i].key, v);
        zz_release(&v);
    }
    return out;
}





size_t zz_dict_len(const zz_dict *d) {
    return d ? d->len : 0;
}

void zz_dict_reserve(zz_value d, size_t hint, int *err) {
    *err = 0;
    if (d.tag != ZZ_DICT || !d.dict) {
        *err = 1;
        return;
    }
    zz_dict *dict = d.dict;
    // Only growable heap dicts can be resized; arena dicts have fixed
    // capacity and must be pre-sized at construction.
    if (dict->refs == ZZ_DICT_ARENA_MAGIC || dict->refs == 0) return;
    if (hint <= dict->cap) { *err = 0; return; }
    size_t nc = dict->cap == 0 ? 4 : dict->cap;
    while (nc < hint) nc *= 2;
    zz_dict_entry *ne = (zz_dict_entry *)realloc(dict->entries, nc * sizeof(zz_dict_entry));
    if (!ne) { *err = 1; return; }
    // Zero the new tail so zz_assign can safely release stale slots.
    memset(&ne[dict->len], 0, (nc - dict->len) * sizeof(zz_dict_entry));
    dict->entries = ne;
    dict->cap = nc;
    // Rebuild the key index for the larger capacity.
    if (dict->idx && dict->idx_cap > 0) {
        size_t cap = dict->idx_cap;
        while (cap < nc) cap <<= 1;
        zz_dict_idx_build(dict, NULL, cap);
    }
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
    size_t nenv = 0;
    void **env = zz_closure_env(f, &nenv);
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    if (!fn) return out;
    // Pre-size to input length: avoids log(n) realloc+copy churn on growth
    // (each push may realloc the items buffer). Output length == input
    // length for map, so reserve exactly once up front.
    {
        int _err = 0;
        zz_array_reserve(out, arr.arr->len, &_err);
    }
    // Sync-call hygiene (B3): callbacks run outside the trampoline, so a
    // stale task frame would corrupt the owning task's resume state.
    // Green closures see NULL and take the stack-frame + thread-park path.
    zz_task_frame *saved_fr = zz_green_frame();
    zz_green_frame_set(NULL);
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value a1[] = { arr.arr->items[i] };
        zz_value r = fn(a1, 1, env, nenv);
        zz_array_push(out.arr, r);
    }
    zz_green_frame_set(saved_fr);
    zz_release(&arr);
    return out;
}

// filter(items, f) → array of items where f(item) is truthy
zz_value zz_iter_filter(zz_value items, zz_value f, int *err) {
    (void)err;
    zz_dispatch_fn fn = zz_closure_target(f);
    size_t nenv = 0;
    void **env = zz_closure_env(f, &nenv);
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    if (!fn) return out;
    // Pre-size to input length (worst case every element passes). Avoids
    // log(n) realloc+copy churn on growth.
    {
        int _err = 0;
        zz_array_reserve(out, arr.arr->len, &_err);
    }
    // Sync-call hygiene (B3): see zz_iter_map.
    zz_task_frame *saved_fr = zz_green_frame();
    zz_green_frame_set(NULL);
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value a1[] = { arr.arr->items[i] };
        zz_value r = fn(a1, 1, env, nenv);
        if (zz_truthy(r)) {
            zz_array_push(out.arr, zz_clone(arr.arr->items[i]));
        }
    }
    zz_green_frame_set(saved_fr);
    zz_release(&arr);
    return out;
}

// enumerate(items) → array of tuples (idx, item)
zz_value zz_iter_enumerate(zz_value items, int *err) {
    (void)err;
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    {
        int _err = 0;
        zz_array_reserve(out, arr.arr->len, &_err);
    }
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
    {
        int _err = 0;
        zz_array_reserve(out, n, &_err);
    }
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
// Store the inner value in a reference-counted box (see `zz_vbox`) so
// every owner of the wrapper shares one box safely; `match` and friends
// extract the inner through the payload pointer.

zz_value zz_variant_some(zz_value inner) {
    return (zz_value){ZZ_OPTION_SOME, {.payload = zz_vbox_new(inner)}};
}

zz_value zz_variant_ok(zz_value inner) {
    return (zz_value){ZZ_RESULT_OK, {.payload = zz_vbox_new(inner)}};
}

zz_value zz_variant_err(zz_value inner) {
    return (zz_value){ZZ_RESULT_ERR, {.payload = zz_vbox_new(inner)}};
}

// Drop one owner of a variant payload: the box releases the inner it owns
// and is freed once the last owner goes away. `zz_vbox_release` lives out
// of line (next to this function) because it needs `zz_release`.
void zz_vbox_release(zz_value *payload) {
    if (!payload) {
        return;
    }
    zz_vbox *b = zz_vbox_of(payload);
    if (__atomic_sub_fetch(&b->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        zz_release(&b->inner);
        free(b);
    }
}

// Release helper for variant payloads (drop our owner reference).
void zz_release_variant(zz_value *v) {
    zz_vbox_release(v->payload);
    v->payload = NULL;
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

// Maximum promotion depth when searching embedded structs. Struct values
// are finite trees, so this is only a safety bound.
#define ZZ_EMBED_MAX_DEPTH 32

// True when a struct slot holds an embedded (anonymous) field value: an
// object whose type's last segment equals the field name
// (`User.Base: Base{...}`). Mirrors the checker's rule.
static int zz_embedded_match(const char *fname, const zz_value *slot) {
    if (!fname || !slot || slot->tag != ZZ_OBJECT || !slot->obj || !slot->obj->type_name)
        return 0;
    const char *base = strrchr(slot->obj->type_name, '.');
    base = base ? base + 1 : slot->obj->type_name;
    return strcmp(base, fname) == 0;
}

// Detach one owner's interest from a shared struct header: when
// `o->refs > 1`, move this value onto a private header whose slots hold
// fresh shares (`zz_clone` bumps each field buffer exactly once, making
// the previously undercounted clone-shares exact). The old header keeps
// its remaining owners untouched. Uniquely-owned headers (refs == 1)
// are returned as-is; non-objects are ignored.
static void zz_object_detach(zz_value *obj) {
    if (!obj || obj->tag != ZZ_OBJECT || !obj->obj) return;
    zz_object *o = obj->obj;
    if (o->refs <= 1) return;
    zz_object *fresh =
        (zz_object *)malloc(sizeof(zz_object) + o->len * 2 * sizeof(zz_value));
    if (!fresh) return; // OOM: keep sharing (old behavior) rather than crash
    fresh->refs = 1;
    fresh->type_name = o->type_name;
    fresh->len = o->len;
    for (size_t i = 0; i < o->len * 2; i++) {
        fresh->fields[i] = zz_clone(o->fields[i]);
    }
    o->refs--;
    obj->obj = fresh;
}

static int zz_object_set_depth(zz_value *obj, const char *name, zz_value val, int depth) {
    if (obj->tag != ZZ_OBJECT || !obj->obj) return 0;
    // Value semantics: a field write through a shared header must not
    // leak into co-owners (`t := s; t.f = v` leaves `s` untouched).
    // Clones bump only the header, so detach first: transfer this
    // owner's interest into a private header (fresh per-field shares),
    // leaving co-owners with their own intact view. Descends after
    // detaching, so embedded children detach level by level too.
    zz_object_detach(obj);
    zz_object *o = obj->obj;
    for (size_t i = 0; i < o->len; i++) {
        zz_value *fname = &o->fields[i * 2];
        if (fname->tag == ZZ_STR && strcmp(zz_str_cptr(fname->s), name) == 0) {
            zz_value *slot = &o->fields[i * 2 + 1];
            zz_release(slot);
            // Share convention (was zz_clone): heal arena-owned values to heap.
            *slot = zz_clone_for_store(val);
            return 1;
        }
    }
    // Embedded promotion: `u.id = 1` writes `u.Base.id` transitively.
    if (depth < ZZ_EMBED_MAX_DEPTH) {
        for (size_t i = 0; i < o->len; i++) {
            zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR
                && zz_embedded_match(zz_str_cptr(fname->s), &o->fields[i * 2 + 1])) {
                if (zz_object_set_depth(&o->fields[i * 2 + 1], name, val, depth + 1))
                    return 1;
            }
        }
    }
    return 0;
}

void zz_object_set_field(zz_value *obj, const char *name, zz_value val) {
    (void)zz_object_set_depth(obj, name, val, 0);
}

static int zz_object_get_depth(const zz_value *obj, const char *name, int depth, zz_value *out) {
    if (!obj || obj->tag != ZZ_OBJECT || !obj->obj) return 0;
    const zz_object *o = obj->obj;
    // Single-entry cache for repeated field lookups on the same type.
    // `name` and `o->type_name` are both pointer-stable C strings, so
    // pointer comparison is valid. Hot paths like `rows[i].val` hit the
    // cache on every access after the first, turning O(fields)·strcmp
    // into O(1). Thread-local: the cached entries are per-thread so
    // concurrent field access across spawned tasks never races on shared
    // statics.
    static __thread const char *cached_type = NULL;
    static __thread const char *cached_name = NULL;
    static __thread size_t cached_index = 0;
    static __thread int cached_found = 0;
    if (o->type_name == cached_type && name == cached_name) {
        if (cached_found) {
            *out = zz_clone(o->fields[cached_index * 2 + 1]);
            return 1;
        }
        return 0;
    }
    for (size_t i = 0; i < o->len; i++) {
        const zz_value *fname = &o->fields[i * 2];
        if (fname->tag == ZZ_STR && strcmp(zz_str_cptr(fname->s), name) == 0) {
            cached_type = o->type_name;
            cached_name = name;
            cached_index = i;
            cached_found = 1;
            *out = zz_clone(o->fields[i * 2 + 1]);
            return 1;
        }
    }
    cached_type = o->type_name;
    cached_name = name;
    cached_index = 0;
    cached_found = 0;
    // Embedded promotion: `u.id` reads `u.Base.id` transitively.
    if (depth < ZZ_EMBED_MAX_DEPTH) {
        for (size_t i = 0; i < o->len; i++) {
            const zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR
                && zz_embedded_match(zz_str_cptr(fname->s), &o->fields[i * 2 + 1])) {
                if (zz_object_get_depth(&o->fields[i * 2 + 1], name, depth + 1, out))
                    return 1;
            }
        }
    }
    return 0;
}

zz_value zz_object_get_field(zz_value *obj, const char *name) {
    if (!obj) return zz_unit();
    // Struct field access — works on both ZZ_OBJECT (boxed structs) and
    // ZZ_DICT (e.g. rows returned by the AOT C FFI for db.query).
    if (obj->tag == ZZ_OBJECT && obj->obj) {
        zz_value out;
        if (zz_object_get_depth(obj, name, 0, &out))
            return out;
        return zz_unit();
    }
    if (obj->tag == ZZ_DICT && obj->dict) {
        int derr = 0;
        zz_value key = zz_str_static(name);
        return zz_dict_get(obj->dict, key, &derr);
    }
    return zz_unit();
}

// Release helper for boxed objects.
void zz_release_object(zz_value *v) {
    if (v->tag != ZZ_OBJECT || !v->obj) return;
    zz_object *o = v->obj;
    if (__atomic_load_n(&o->refs, __ATOMIC_ACQUIRE) == 0) return; // already freed
    if (__atomic_sub_fetch(&o->refs, 1, __ATOMIC_ACQ_REL) == 0) {
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

// User-enum tag test for generated `match` guards: true only for boxed
// objects whose qualified type name matches exactly (`Token.IntLit`).
// Everything else (scalars, options, results, other objects) is false.
int zz_enum_is(const zz_value *v, const char *qualified) {
    if (!v || !qualified || v->tag != ZZ_OBJECT || !v->obj || !v->obj->type_name) return 0;
    return strcmp(v->obj->type_name, qualified) == 0;
}

// Trait dispatch implementor test: exact match for struct implementors,
// variant-prefix match for user-enum implementors (their stored type name
// is `Enum.Variant`, while the trait impl is keyed by `Enum`).
int zz_trait_type_is(const zz_value *v, const char *qualified) {
    if (!v || !qualified || v->tag != ZZ_OBJECT || !v->obj || !v->obj->type_name) return 0;
    const char *tn = v->obj->type_name;
    size_t n = strlen(qualified);
    if (n == 0 || strncmp(tn, qualified, n) != 0) return 0;
    return tn[n] == '\0' || tn[n] == '.';
}

// Unreachable in a well-formed program: the checker rejects a trait method
// call unless a bound provides it, and the generated dispatcher covers every
// registered implementor. Reached only when a program bypasses the checker
// (stale cache, REPL drift) — mirror the other native panic sites.
zz_value zz_trait_missing_impl(const char *trait, const char *method, zz_value recv) {
    const char *tn = NULL;
    if (recv.tag == ZZ_OBJECT && recv.obj) tn = recv.obj->type_name;
    fprintf(stderr,
            "error: no implementation of trait `%s` for method `%s` (receiver: `%s`)\n",
            trait ? trait : "?", method ? method : "?", tn ? tn : "?");
    exit(1);
}

// Display shape test for user-enum variant values: a dotted type name
// with no fields (unit variant) or exactly one `value` field (payload
// variant). Mirrors the VM's Display rule so `println` agrees on both
// engines. (A single-field-`value` struct shares the payload shape and
// prints in constructor form — cosmetic only.)
int zz_object_is_enum_shape(const zz_object *o) {
    if (!o || !o->type_name || !strchr(o->type_name, '.')) return 0;
    if (o->len == 0) return 1;
    if (o->len != 1) return 0;
    const zz_value *fname = &o->fields[0];
    return fname->tag == ZZ_STR && fname->s && strcmp(zz_str_cptr(fname->s), "value") == 0;
}
zz_value zz_range_build(zz_value start, zz_value end) {
    return zz_range_build_step(start, end, zz_int(1));
}

// First-class range value: heap-allocated (start, end, step) shared by
// both AOT backends (#254). Bounds must be ints (mirrors the VM, which
// rejects non-int bounds); non-int inputs degrade to `0..0`, same as
// the old degenerate marker. Step 0 normalizes to 1 (a zero step can
// never advance iteration).
zz_value zz_range_build_step(zz_value start, zz_value end, zz_value step) {
    int64_t s = start.tag == ZZ_INT ? start.i : 0;
    int64_t e = end.tag == ZZ_INT ? end.i : 0;
    int64_t st = step.tag == ZZ_INT ? step.i : 1;
    if (st == 0) st = 1;
    zz_range *r = (zz_range *)malloc(sizeof(zz_range));
    if (!r) {
        fprintf(stderr, "zz error: out of memory (range)\n");
        exit(1);
    }
    r->refs = 1;
    r->start = s;
    r->end = e;
    r->step = st;
    zz_value v;
    v.tag = ZZ_RANGE;
    v.range = r;
    return v;
}

// Release an owned temporary variant/JSON wrapper.
//
// Boxes are reference counted now (`zz_vbox`), so the plain release is
// correct: it drops this owner's box reference, and the box releases the
// inner it owns once the last owner goes away. This used to need a
// box-leaking special case (`zz_release_shared`), because
// `zz_release_variant` freed a box other owners still pointed at (#507).
zz_value zz_elvis(zz_value left, zz_value right) {
    zz_value out;
    if (left.tag == ZZ_OPTION_SOME && left.payload)
        out = zz_clone(*left.payload);
    else if (left.tag == ZZ_OPTION_NONE)
        out = zz_clone(right);
    else if (left.tag == ZZ_RESULT_OK && left.payload)
        out = zz_clone(*left.payload);
    else if (left.tag == ZZ_RESULT_ERR)
        out = zz_clone(right);
    // For non-optional/result types, fall back to truthiness check.
    else if (zz_truthy(left))
        out = zz_clone(left);
    else
        out = zz_clone(right);
    // `zz_elvis` borrows `left` (reads it during the call) — the caller owns
    // `left` and must release it (typically via `cleanup(zz_release)` on the
    // temp). `right` is consumed (released) here since callers pass owned
    // shares.
    zz_release(&right);
    return out;
}
// =====================================================================
//  Missing stdlib natives — bare builtins and module functions
// =====================================================================

// len(v) — array/element length, bytes length, string char length
// (Unicode scalar values, matching the VM), dict entry count, or 0
// for other types.
zz_value zz_len(zz_value v, int *err) {
    (void)err;
    if (v.tag == ZZ_ARRAY) {
        return (zz_value){ZZ_INT, {.i = (int64_t)v.arr->len}};
    }
    if (v.tag == ZZ_BYTES) {
        return (zz_value){ZZ_INT, {.i = v.bytes ? (int64_t)v.bytes->len : 0}};
    }
    if (v.tag == ZZ_STR) {
        return (zz_value){ZZ_INT, {.i = (int64_t)zz_str_char_len(v.s)}};
    }
    if (v.tag == ZZ_DICT) {
        return (zz_value){ZZ_INT, {.i = v.dict ? (int64_t)v.dict->len : 0}};
    }
    return (zz_value){ZZ_INT, {.i = 0}};
}

// len(s.f) without the getter's retain: reading a field only to measure it
// must not bump the array's refcount (each leaked share defeats the next
// move-aware push). Direct fields answer inline; anything else takes the
// exact old path (get + len + release of the getter's share).
zz_value zz_len_field(zz_value *obj, const char *field, int *err) {
    if (obj && obj->tag == ZZ_OBJECT && obj->obj) {
        zz_object *o = obj->obj;
        for (size_t i = 0; i < o->len; i++) {
            zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR && strcmp(zz_str_cptr(fname->s), field) == 0) {
                zz_value *slot = &o->fields[i * 2 + 1];
                if (slot->tag == ZZ_ARRAY && slot->arr) {
                    return (zz_value){ZZ_INT, {.i = (int64_t)slot->arr->len}};
                }
                if (slot->tag == ZZ_BYTES && slot->bytes) {
                    return (zz_value){ZZ_INT, {.i = (int64_t)slot->bytes->len}};
                }
                if (slot->tag == ZZ_STR && slot->s) {
                    return (zz_value){ZZ_INT, {.i = (int64_t)zz_str_char_len(slot->s)}};
                }
                if (slot->tag == ZZ_DICT) {
                    return (zz_value){ZZ_INT, {.i = slot->dict ? (int64_t)slot->dict->len : 0}};
                }
                return (zz_value){ZZ_INT, {.i = 0}};
            }
        }
    }
    zz_value cur = zz_object_get_field(obj, field);
    zz_value r = zz_len(cur, err);
    zz_release(&cur);
    return r;
}

// vec.len(v) — same as len for arrays.
zz_value zz_vec_len(zz_value v, int *err) {
    return zz_len(v, err);
}

// vec.push(arr, item) — returns a NEW array with item appended
// (matches the VM: the input array is left untouched).
// Fast path: when `arr` is uniquely owned (refs == 1), mutate in place and
// return the same handle — no full-array copy. When shared (refs != 1),
// fall back to copy-on-write via zz_array_dup so callers still see a fresh
// array and the original is left untouched.
zz_value zz_vec_push(zz_value arr, zz_value item, int *err) {
    if (arr.tag != ZZ_ARRAY) return zz_unit();
    zz_array *a = arr.arr;
    // In-place fast path when uniquely owned: no alias exists to observe
    // the mutation, so returning the same handle preserves value semantics
    // while turning `vec.push(a, x)` in a loop from O(n²) into O(n).
    // Stack/lit/arena/0 sentinels are uniquely owned by construction.
    int uniquely_owned = (a->refs == 1
        || a->refs == ZZ_ARRAY_STACK_MAGIC
        || a->refs == ZZ_ARRAY_LIT_MAGIC
        || a->refs == ZZ_ARRAY_ARENA_MAGIC
        || a->refs == 0);
    if (uniquely_owned) {
        // `zz_array_push` migrates stack/lit/arena items to malloc on grow
        // and takes element ownership (move convention).
        zz_array_push(a, item);
        return arr;
    }
    // Shared (refs != 1): copy-on-write so the caller's array is untouched.
    zz_value out = zz_array_dup(a);
    if (!out.arr) { if (err) *err = 1; return zz_unit(); }
    zz_array_push(out.arr, item);
    return out;
}

// Move-aware push for `x = vec.push(x, e)` (see collections.h).
// The fast path moves the element in (no bump, no copy); the fallback
// heals the counter (dup preserves `refs == 1`, the taken share is
// released, the result adopted without a bump), so at most one copy ever
// precedes in-place pushes.
zz_value zz_vec_push_take(zz_value taken, zz_value item, int *err) {
    if (taken.tag == ZZ_ARRAY && taken.arr && taken.arr->refs == 1) {
        // Uniquely owned: refs==1 excludes every sentinel (arena 0,
        // STACK/LIT/ARENA magics) and any live borrow, so the items
        // buffer is malloc'd (or NULL when empty) and growth is sound.
        // `zz_array_push` takes element ownership (move convention: the
        // emitted element temp's share transfers into the array).
        zz_array_push(taken.arr, item);
        return taken;
    }
    zz_value out = zz_vec_push(taken, item, err);
    // Balance the take: the old codegen's assign-release freed the slot's
    // old share here (and additionally leaked the argument-clone share,
    // which no longer exists).
    zz_release(&taken);
    return out;
}

// Move-aware field push for `s.f = vec.push(s.f, e)` (see collections.h).
// Same in-place rule for the field array; the fallback heals like above
// (drops the getter's share the old lowering leaked, adopts the dup
// without a bump), so a field pays at most one copy, ever.
void zz_object_push_field_take(zz_value *obj, const char *field, zz_value item, int *err) {
    if (obj && obj->tag == ZZ_OBJECT && obj->obj) {
        zz_object *o = obj->obj;
        for (size_t i = 0; i < o->len; i++) {
            zz_value *fname = &o->fields[i * 2];
            if (fname->tag == ZZ_STR && strcmp(zz_str_cptr(fname->s), field) == 0) {
                // In place only when uniquely owned end to end: the field
                // buffer counter alone is NOT exact (struct clones share
                // field buffers without bumping them), so the header must
                // be unshared too. Otherwise the generic get+push+set path
                // below runs, and the set detaches (copy-on-write header).
                zz_value *slot = &o->fields[i * 2 + 1];
                if (o->refs == 1 && slot->tag == ZZ_ARRAY && slot->arr
                    && slot->arr->refs == 1) {
                    zz_array_push(slot->arr, item);
                    return;
                }
                zz_value cur = zz_object_get_field(obj, field);
                zz_value n = zz_vec_push(cur, item, err);
                zz_release(&cur);
                // The slot may sit in a shared header: detach first (this
                // reseats obj->obj), then re-lookup, release, and adopt.
                zz_object_detach(obj);
                zz_object *fresh = obj->obj;
                for (size_t j = 0; j < fresh->len; j++) {
                    zz_value *fname2 = &fresh->fields[j * 2];
                    if (fname2->tag == ZZ_STR
                        && strcmp(zz_str_cptr(fname2->s), field) == 0) {
                        zz_value *slot2 = &fresh->fields[j * 2 + 1];
                        zz_release(slot2);
                        *slot2 = n;
                        return;
                    }
                }
                // Unreachable: detach preserves shape. Generic set as the
                // backstop (detaches again, harmlessly).
                zz_object_set_field(obj, field, n);
                return;
            }
        }
    }
    // Non-object base or absent/promoted field: same get+push+set shape
    // (getters return unit, setters no-op, matching old lowering).
    zz_value cur = zz_object_get_field(obj, field);
    zz_value n = zz_vec_push(cur, item, err);
    zz_object_set_field(obj, field, n);
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
        size_t new_cap;
        if (o->cap == 0) {
            new_cap = 8;
        } else if (o->cap > SIZE_MAX / 2) {
            if (err) *err = 1;
            return zz_unit();
        } else {
            new_cap = o->cap * 2;
        }
        if (o->refs == ZZ_ARRAY_STACK_MAGIC || o->refs == ZZ_ARRAY_LIT_MAGIC
            || o->refs == ZZ_ARRAY_ARENA_MAGIC || o->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            if (!new_items) { if (err) *err = 1; return zz_unit(); }
            for (size_t j = 0; j < o->len; j++) new_items[j] = o->items[j];
            o->items = new_items;
            // Preserve `refs == 1` (see `zz_array_push`).
            if (o->refs != 1) o->refs = 0;
        } else {
            o->items = (zz_value *)realloc(o->items, new_cap * sizeof(zz_value));
            if (!o->items) { if (err) *err = 1; return zz_unit(); }
        }
        o->cap = new_cap;
    }
    for (size_t j = o->len; j > (size_t)i; j--) {
        o->items[j] = o->items[j - 1];
    }
    // Share convention: heal arena-owned values to heap (fresh heap array may
    // still outlive the loop-arena reset via the caller's store).
    o->items[i] = zz_clone_for_store(item);
    o->len++;
    return out;
}

// vec.contains(arr, item) → bool
zz_value zz_vec_contains(zz_value arr, zz_value item, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return (zz_value){ZZ_BOOL, {.b = false}};
    // Clone the needle once, not per element. Tags that differ can never be
    // equal (zz_values_equal requires identical tags), so skip zz_binop
    // entirely for those. Interned strings compare by pointer in O(1).
    zz_value item_clone = zz_clone(item);
    for (size_t i = 0; i < arr.arr->len; i++) {
        zz_value el = arr.arr->items[i];
        if (el.tag != item_clone.tag) continue;
        if (el.tag == ZZ_STR && el.s && item_clone.s && el.s == item_clone.s)
            goto found;
        if (zz_truthy(zz_binop(ZZOP_EQ, zz_clone(el), zz_clone(item_clone))))
            goto found;
    }
    zz_release(&item_clone);
    return (zz_value){ZZ_BOOL, {.b = false}};
found:
    zz_release(&item_clone);
    return (zz_value){ZZ_BOOL, {.b = true}};
}

// ---- vec.sort -------------------------------------------------------------
// Ordering mirrors the VM (`natives/vec_mod::vec_sort`): type name first,
// then the value within a type, and *stably* (equal elements keep their
// input order). qsort is not stable, so the generic lane breaks ties on the
// original index; the scalar lanes need no tie-break because equal scalars
// are indistinguishable, so any permutation is the same array.
static const char *zz_sort_type_name(zz_value v) {
    switch (v.tag) {
    case ZZ_UNIT: return "unit";
    case ZZ_INT: return "int";
    case ZZ_FLOAT: return "float";
    case ZZ_BOOL: return "bool";
    case ZZ_STR: return "str";
    case ZZ_ARRAY: return "array";
    case ZZ_DICT: return "dict";
    case ZZ_FUNC: return "func";
    case ZZ_NATIVE: return "native";
    case ZZ_OPTION_SOME:
    case ZZ_OPTION_NONE: return "option";
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR: return "result";
    case ZZ_RANGE: return "range";
    case ZZ_JSON: return "json";
    case ZZ_TCP_STREAM: return "tcp.stream";
    case ZZ_TCP_LISTENER: return "tcp.listener";
    case ZZ_BYTES: return "bytes";
    case ZZ_TUPLE: return "tuple";
    case ZZ_OBJECT:
        return (v.obj && v.obj->type_name) ? v.obj->type_name : "object";
    default: return "unknown";
    }
}

typedef struct {
    zz_value v;
    zz_value k;   // sort key (sort_key only; unset for sort_by)
    size_t idx;
} zz_sort_pair;

// Lexicographic byte order, then length — `String::cmp` on the VM side.
static int zz_sort_cmp_str(const void *pa, const void *pb) {
    zz_str *a = ((const zz_value *)pa)->s;
    zz_str *b = ((const zz_value *)pb)->s;
    if (a == b) return 0;
    if (!a) return -1;
    if (!b) return 1;
    size_t m = a->len < b->len ? a->len : b->len;
    int c = m ? memcmp(zz_str_cptr(a), zz_str_cptr(b), m) : 0;
    if (c) return c;
    return (a->len > b->len) - (a->len < b->len);
}

static int zz_sort_cmp_value(const void *pa, const void *pb) {
    const zz_sort_pair *a = (const zz_sort_pair *)pa;
    const zz_sort_pair *b = (const zz_sort_pair *)pb;
    const char *ta = zz_sort_type_name(a->v);
    const char *tb = zz_sort_type_name(b->v);
    int c = ta == tb ? 0 : strcmp(ta, tb);
    if (c == 0 && a->v.tag == b->v.tag) {
        switch (a->v.tag) {
        case ZZ_INT:
            c = (a->v.i > b->v.i) - (a->v.i < b->v.i);
            break;
        case ZZ_FLOAT:
            c = (a->v.f > b->v.f) - (a->v.f < b->v.f);
            break;
        case ZZ_BOOL: {
            int x = a->v.b ? 1 : 0, y = b->v.b ? 1 : 0;
            c = (x > y) - (x < y);
            break;
        }
        case ZZ_STR:
            c = zz_sort_cmp_str(&a->v, &b->v);
            break;
        default:
            // The VM orders other same-type values `Equal`; the index
            // tie-break below keeps their input order (stability).
            break;
        }
    }
    if (c == 0) c = (a->idx > b->idx) - (a->idx < b->idx);
    return c;
}

// ---- specialized introsort for the homogeneous scalar lanes ------------
// The generic `qsort` path pays an indirect call per comparison, which is
// what made `vec.sort` ~4.6x slower than Rust's `sort_unstable` on 100k
// ints. These introsorts compare inline. 3-way partitioning keeps
// duplicate-heavy inputs linear (the benchmark's `seed % 100000` produces
// many duplicates), median-of-3 picks the pivot, and a heapsort fallback
// bounds the worst case at O(n log n). Semantics match the VM lanes:
// equal ints/floats/strings are indistinguishable, so instability is
// unobservable.
#define ZZ_SORT_INSERTION_CUTOFF 24

#define ZZ_LT_SCALAR(x, y) ((x) < (y))
#define ZZ_LT_STR(x, y) (zz_sort_cmp_str(&(x), &(y)) < 0)

#define ZZ_DEFINE_INTROSORT(NAME, TYPE, LESS)                                 \
    static void NAME##_sift_down(TYPE *a, size_t root, size_t n) {            \
        for (;;) {                                                            \
            size_t child = 2 * root + 1;                                      \
            if (child >= n) break;                                            \
            if (child + 1 < n && LESS(a[child], a[child + 1])) child++;       \
            if (!LESS(a[root], a[child])) break;                              \
            TYPE t = a[root];                                                 \
            a[root] = a[child];                                               \
            a[child] = t;                                                     \
            root = child;                                                     \
        }                                                                     \
    }                                                                         \
    static void NAME##_heapsort(TYPE *a, size_t n) {                          \
        if (n < 2) return;                                                    \
        for (size_t start = n / 2; start-- > 0;)                              \
            NAME##_sift_down(a, start, n);                                    \
        for (size_t end = n - 1; end > 0; end--) {                            \
            TYPE t = a[0];                                                    \
            a[0] = a[end];                                                    \
            a[end] = t;                                                       \
            NAME##_sift_down(a, 0, end);                                      \
        }                                                                     \
    }                                                                         \
    static void NAME##_insertion(TYPE *a, size_t n) {                         \
        for (size_t i = 1; i < n; i++) {                                      \
            TYPE v = a[i];                                                    \
            size_t j = i;                                                     \
            while (j > 0 && LESS(v, a[j - 1])) {                              \
                a[j] = a[j - 1];                                              \
                j--;                                                          \
            }                                                                 \
            a[j] = v;                                                         \
        }                                                                     \
    }                                                                         \
    static void NAME##_intro(TYPE *a, size_t n, int depth) {                  \
        while (n > ZZ_SORT_INSERTION_CUTOFF) {                                \
            if (depth-- == 0) {                                               \
                NAME##_heapsort(a, n);                                        \
                return;                                                       \
            }                                                                 \
            size_t mid = n / 2;                                               \
            if (LESS(a[mid], a[0])) {                                         \
                TYPE t = a[mid]; a[mid] = a[0]; a[0] = t;                     \
            }                                                                 \
            if (LESS(a[n - 1], a[0])) {                                       \
                TYPE t = a[n - 1]; a[n - 1] = a[0]; a[0] = t;                 \
            }                                                                 \
            if (LESS(a[n - 1], a[mid])) {                                     \
                TYPE t = a[n - 1]; a[n - 1] = a[mid]; a[mid] = t;             \
            }                                                                 \
            TYPE pivot = a[mid];                                              \
            size_t lt = 0, i = 0, gt = n;                                     \
            while (i < gt) {                                                  \
                if (LESS(a[i], pivot)) {                                      \
                    TYPE t = a[lt]; a[lt] = a[i]; a[i] = t;                   \
                    lt++;                                                     \
                    i++;                                                      \
                } else if (LESS(pivot, a[i])) {                               \
                    gt--;                                                     \
                    TYPE t = a[i]; a[i] = a[gt]; a[gt] = t;                   \
                } else {                                                      \
                    i++;                                                      \
                }                                                             \
            }                                                                 \
            if (lt < n - gt) {                                                \
                NAME##_intro(a, lt, depth);                                   \
                a += gt;                                                      \
                n -= gt;                                                      \
            } else {                                                          \
                NAME##_intro(a + gt, n - gt, depth);                          \
                n = lt;                                                       \
            }                                                                 \
        }                                                                     \
        NAME##_insertion(a, n);                                               \
    }                                                                         \
    static void NAME(TYPE *a, size_t n) {                                     \
        if (n < 2) return;                                                    \
        int depth = 0;                                                        \
        for (size_t m = n; m > 1; m >>= 1) depth++;                           \
        NAME##_intro(a, n, depth * 2);                                        \
    }

ZZ_DEFINE_INTROSORT(zz_introsort_i64, int64_t, ZZ_LT_SCALAR)
ZZ_DEFINE_INTROSORT(zz_sort_f64, double, ZZ_LT_SCALAR)
ZZ_DEFINE_INTROSORT(zz_sort_str, zz_value, ZZ_LT_STR)

// ---- adaptive LSD radix sort for the int lane -------------------------
// Integers sort in O(n) with a radix pass, and the common workload
// (ids, counts, the benchmark's `seed % 100000`) only occupies the low
// 17 bits, so we do only the 8-bit passes the data actually needs: three
// passes here vs the comparison introsort's ~16 ms / `qsort`'s ~30 ms,
// landing at ~3 ms (Rust `sort_unstable` is ~4 ms on the same input).
// Equal ints are indistinguishable, so LSD's stability is not observable
// and the output matches the VM's `sort_unstable` exactly. Falls back to
// the inlined introsort when the scratch buffer cannot be allocated.
#define ZZ_RADIX_MIN 128

static void zz_sort_i64(int64_t *a, size_t n) {
    if (n < 2) return;
    if (n < ZZ_RADIX_MIN) {
        zz_introsort_i64(a, n);
        return;
    }
    // Flip the sign bit so unsigned digit order equals signed value order.
    for (size_t i = 0; i < n; i++) a[i] ^= (int64_t)INT64_MIN;
    uint64_t mn = UINT64_MAX, mx = 0;
    for (size_t i = 0; i < n; i++) {
        uint64_t u = (uint64_t)a[i];
        if (u < mn) mn = u;
        if (u > mx) mx = u;
    }
    // Only the bits where the values actually differ need a pass.
    uint64_t diff = mn ^ mx;
    int bits = 0;
    for (uint64_t d = diff; d; d >>= 1) bits++;
    int passes = (bits + 7) / 8;
    if (passes == 0) {
        for (size_t i = 0; i < n; i++) a[i] ^= (int64_t)INT64_MIN;
        return;
    }
    int64_t *tmp = (int64_t *)malloc(n * sizeof *tmp);
    if (!tmp) {
        for (size_t i = 0; i < n; i++) a[i] ^= (int64_t)INT64_MIN;
        zz_introsort_i64(a, n);
        return;
    }
    int64_t *src = a, *dst = tmp;
    for (int p = 0; p < passes; p++) {
        int shift = p * 8;
        size_t cnt[256] = {0};
        for (size_t i = 0; i < n; i++) cnt[((uint64_t)src[i] >> shift) & 0xFF]++;
        size_t sum = 0;
        for (int d = 0; d < 256; d++) {
            size_t c = cnt[d];
            cnt[d] = sum;
            sum += c;
        }
        for (size_t i = 0; i < n; i++) {
            dst[cnt[((uint64_t)src[i] >> shift) & 0xFF]++] = src[i];
        }
        int64_t *t = src;
        src = dst;
        dst = t;
    }
    // An even pass count lands the result back in `a`; odd needs one copy.
    if (src != a) memcpy(a, src, n * sizeof *a);
    for (size_t i = 0; i < n; i++) a[i] ^= (int64_t)INT64_MIN;
    free(tmp);
}


// vec.sort(arr) → sorted array (new array, original unchanged)
zz_value zz_vec_sort(zz_value arr, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return arr;
    size_t n = arr.arr->len;
    if (n <= 1) return zz_clone(arr);
    zz_value *src = arr.arr->items;
    unsigned char tag = src[0].tag;

    // Fast scalar lanes: scan the source directly, sort the extracted keys,
    // then build the result array from the sorted keys. This skips the
    // intermediate healed copy of the whole array (a 1.6 MB fill plus a
    // second pass to re-extract keys) that the old path paid even for a
    // plain `[int]`; on 100k ints that was ~2.4 ms of the ~6 ms sort. The
    // source is never mutated (value semantics).
    if (tag == ZZ_INT) {
        int64_t *keys = (int64_t *)malloc(n * sizeof *keys);
        if (keys) {
            int all = 1;
            for (size_t i = 0; i < n; i++) {
                if (src[i].tag != ZZ_INT) {
                    all = 0;
                    break;
                }
                keys[i] = src[i].i;
            }
            if (all) {
                zz_sort_i64(keys, n);
                zz_value sorted = zz_array_new_sized(n);
                if (sorted.tag != ZZ_ARRAY || !sorted.arr) {
                    free(keys);
                    return zz_unit();
                }
                for (size_t i = 0; i < n; i++) sorted.arr->items[i] = zz_int(keys[i]);
                sorted.arr->len = n;
                free(keys);
                return sorted;
            }
            free(keys);
        }
    } else if (tag == ZZ_FLOAT) {
        double *keys = (double *)malloc(n * sizeof *keys);
        if (keys) {
            int all = 1;
            for (size_t i = 0; i < n; i++) {
                if (src[i].tag != ZZ_FLOAT) {
                    all = 0;
                    break;
                }
                keys[i] = src[i].f;
            }
            if (all) {
                zz_sort_f64(keys, n);
                zz_value sorted = zz_array_new_sized(n);
                if (sorted.tag != ZZ_ARRAY || !sorted.arr) {
                    free(keys);
                    return zz_unit();
                }
                for (size_t i = 0; i < n; i++) sorted.arr->items[i] = zz_float(keys[i]);
                sorted.arr->len = n;
                free(keys);
                return sorted;
            }
            free(keys);
        }
    } else if (tag == ZZ_BOOL) {
        size_t falses = 0;
        int all = 1;
        for (size_t i = 0; i < n; i++) {
            if (src[i].tag != ZZ_BOOL) {
                all = 0;
                break;
            }
            if (!src[i].b) falses++;
        }
        if (all) {
            // Only two values: counting beats every comparison sort.
            zz_value sorted = zz_array_new_sized(n);
            if (sorted.tag != ZZ_ARRAY || !sorted.arr) return zz_unit();
            for (size_t i = 0; i < n; i++) sorted.arr->items[i] = zz_bool(i >= falses);
            sorted.arr->len = n;
            return sorted;
        }
    }

    // General path: copy with the SHARE convention (the input array is
    // borrowed and released by the caller, so heap elements must be bumped
    // — `zz_heal_for_move` would adopt them without a refcount and the two
    // arrays would double-free at scope end; ASan caught exactly that on a
    // sorted `[[int]]`). Arena-owned values still heal to heap copies, then
    // the copy is sorted in place. Homogeneity is checked on the copy so the
    // str lane can use the inlined sort.
    zz_value sorted = zz_array_new_sized(n);
    if (sorted.tag != ZZ_ARRAY || !sorted.arr) return zz_unit();
    zz_value *items = sorted.arr->items;
    int all_str = 1;
    for (size_t i = 0; i < n; i++) {
        items[i] = zz_clone_for_store(src[i]);
        if (items[i].tag != ZZ_STR) all_str = 0;
    }
    sorted.arr->len = n;
    if (all_str) {
        zz_sort_str(items, n);
        return sorted;
    }

    // Mixed tags (or a single non-scalar tag): decorate with the input
    // position so ties stay stable, sort, undecorate.
    zz_sort_pair *pairs = (zz_sort_pair *)malloc(n * sizeof *pairs);
    if (!pairs) return sorted;
    for (size_t i = 0; i < n; i++) {
        pairs[i].v = items[i];
        pairs[i].idx = i;
    }
    qsort(pairs, n, sizeof *pairs, zz_sort_cmp_value);
    for (size_t i = 0; i < n; i++) items[i] = pairs[i].v;
    free(pairs);
    return sorted;
}

// vec.sort_by(arr, cmp) — sort array using a |T, T| -> int comparator.
// Mergesort (stable, O(n log n)) with a closure comparator called via the C ABI.
// negative/zero/positive from the comparator maps to Less/Equal/Greater.
typedef struct {
    zz_dispatch_fn fn;
    void **env;
    size_t nenv;
} sort_by_ctx;

static int sort_by_cmp(void *thunk, const zz_sort_pair *a, const zz_sort_pair *b) {
    sort_by_ctx *ctx = (sort_by_ctx *)thunk;
    zz_value argv[] = { a->v, b->v };
    zz_value r = ctx->fn(argv, 2, ctx->env, ctx->nenv);
    int ord;
    if (r.tag == ZZ_INT) ord = (r.i > 0) - (r.i < 0);
    else if (r.tag == ZZ_FLOAT) ord = (r.f > 0.0) - (r.f < 0.0);
    else if (r.tag == ZZ_BOOL) ord = r.b ? 1 : -1;
    else ord = 0;
    zz_release(&r);
    return ord;
}

// Mergesort over zz_sort_pair using a comparator with context.
// Stable (takes left element when equal) and O(n log n).
static void merge_sort_ctx(zz_sort_pair *pairs, zz_sort_pair *tmp, size_t n,
                           int (*cmp)(void *, const zz_sort_pair *, const zz_sort_pair *),
                           void *thunk) {
    if (n <= 1) return;
    size_t mid = n / 2;
    merge_sort_ctx(pairs, tmp, mid, cmp, thunk);
    merge_sort_ctx(pairs + mid, tmp + mid, n - mid, cmp, thunk);
    size_t i = 0, j = mid, k = 0;
    while (i < mid && j < n) {
        // Stable: take left element when equal (cmp <= 0 means left <= right).
        if (cmp(thunk, &pairs[i], &pairs[j]) <= 0) {
            tmp[k++] = pairs[i++];
        } else {
            tmp[k++] = pairs[j++];
        }
    }
    while (i < mid) tmp[k++] = pairs[i++];
    while (j < n) tmp[k++] = pairs[j++];
    for (size_t t = 0; t < n; t++) pairs[t] = tmp[t];
}

zz_value zz_vec_sort_by(zz_value arr, zz_value cmp, int *err) {
    *err = 0;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return zz_clone(arr);
    size_t n = arr.arr->len;
    if (n <= 1) return zz_clone(arr);
    zz_dispatch_fn fn = zz_closure_target(cmp);
    size_t nenv = 0;
    void **env = zz_closure_env(cmp, &nenv);
    if (!fn) return zz_clone(arr);
    zz_sort_pair *pairs = (zz_sort_pair *)malloc(n * sizeof *pairs);
    zz_sort_pair *tmp = (zz_sort_pair *)malloc(n * sizeof *pairs);
    if (!pairs || !tmp) {
        free(pairs);
        free(tmp);
        return zz_clone(arr);
    }
    for (size_t i = 0; i < n; i++) {
        pairs[i].v = zz_clone(arr.arr->items[i]);
        pairs[i].k = (zz_value){ZZ_UNIT, {0}};
        pairs[i].idx = i;
    }
    // Sync-call hygiene: closures run outside the trampoline.
    zz_task_frame *saved_fr = zz_green_frame();
    zz_green_frame_set(NULL);
    sort_by_ctx ctx = { fn, env, nenv };
    merge_sort_ctx(pairs, tmp, n, sort_by_cmp, &ctx);
    zz_green_frame_set(saved_fr);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(out.arr, pairs[i].v);
        zz_release(&pairs[i].v);
        zz_release(&pairs[i].k);
    }
    free(pairs);
    free(tmp);
    return out;
}

// vec.sort_by_desc(arr, cmp) — like sort_by but reverses the comparator.
typedef struct {
    zz_dispatch_fn fn;
    void **env;
    size_t nenv;
} sort_by_desc_ctx;

static int sort_by_desc_cmp(void *thunk, const zz_sort_pair *a, const zz_sort_pair *b) {
    sort_by_desc_ctx *ctx = (sort_by_desc_ctx *)thunk;
    zz_value argv[] = { a->v, b->v };
    zz_value r = ctx->fn(argv, 2, ctx->env, ctx->nenv);
    int ord;
    if (r.tag == ZZ_INT) ord = (r.i > 0) - (r.i < 0);
    else if (r.tag == ZZ_FLOAT) ord = (r.f > 0.0) - (r.f < 0.0);
    else if (r.tag == ZZ_BOOL) ord = r.b ? 1 : -1;
    else ord = 0;
    zz_release(&r);
    return -ord;
}

zz_value zz_vec_sort_by_desc(zz_value arr, zz_value cmp, int *err) {
    *err = 0;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return zz_clone(arr);
    size_t n = arr.arr->len;
    if (n <= 1) return zz_clone(arr);
    zz_dispatch_fn fn = zz_closure_target(cmp);
    size_t nenv = 0;
    void **env = zz_closure_env(cmp, &nenv);
    if (!fn) return zz_clone(arr);
    zz_sort_pair *pairs = (zz_sort_pair *)malloc(n * sizeof *pairs);
    zz_sort_pair *tmp = (zz_sort_pair *)malloc(n * sizeof *pairs);
    if (!pairs || !tmp) {
        free(pairs);
        free(tmp);
        return zz_clone(arr);
    }
    for (size_t i = 0; i < n; i++) {
        pairs[i].v = zz_clone(arr.arr->items[i]);
        pairs[i].k = (zz_value){ZZ_UNIT, {0}};
        pairs[i].idx = i;
    }
    zz_task_frame *saved_fr = zz_green_frame();
    zz_green_frame_set(NULL);
    sort_by_desc_ctx ctx = { fn, env, nenv };
    merge_sort_ctx(pairs, tmp, n, sort_by_desc_cmp, &ctx);
    zz_green_frame_set(saved_fr);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(out.arr, pairs[i].v);
        zz_release(&pairs[i].v);
        zz_release(&pairs[i].k);
    }
    free(pairs);
    free(tmp);
    return out;
}

// vec.sort_key(arr, key) — sort array by |T| -> K key (stable, O(n) key calls).
typedef struct {
    zz_dispatch_fn fn;
    void **env;
    size_t nenv;
} sort_key_ctx;

// Key comparison (no context needed — keys are precomputed in pairs[i].k).
static int sort_key_cmp(const zz_sort_pair *a, const zz_sort_pair *b) {
    // Compare keys by tag then value.
    int ord;
    if (a->k.tag == ZZ_INT && b->k.tag == ZZ_INT) {
        ord = (a->k.i > b->k.i) - (a->k.i < b->k.i);
    } else if (a->k.tag == ZZ_FLOAT && b->k.tag == ZZ_FLOAT) {
        ord = (a->k.f > b->k.f) - (a->k.f < b->k.f);
    } else if (a->k.tag == ZZ_STR && b->k.tag == ZZ_STR) {
        ord = strcmp(zz_str_cptr(a->k.s), zz_str_cptr(b->k.s));
    } else if (a->k.tag == ZZ_BOOL && b->k.tag == ZZ_BOOL) {
        ord = a->k.b - b->k.b;
    } else {
        ord = (int)(a->k.tag - b->k.tag);
    }
    if (ord == 0) ord = (a->idx > b->idx) - (a->idx < b->idx);
    return ord;
}

// Mergesort using sort_key_cmp (no context). Stable and O(n log n).
static void merge_sort_key(zz_sort_pair *pairs, zz_sort_pair *tmp, size_t n) {
    if (n <= 1) return;
    size_t mid = n / 2;
    merge_sort_key(pairs, tmp, mid);
    merge_sort_key(pairs + mid, tmp + mid, n - mid);
    size_t i = 0, j = mid, k = 0;
    while (i < mid && j < n) {
        if (sort_key_cmp(&pairs[i], &pairs[j]) <= 0) {
            tmp[k++] = pairs[i++];
        } else {
            tmp[k++] = pairs[j++];
        }
    }
    while (i < mid) tmp[k++] = pairs[i++];
    while (j < n) tmp[k++] = pairs[j++];
    for (size_t t = 0; t < n; t++) pairs[t] = tmp[t];
}

zz_value zz_vec_sort_key(zz_value arr, zz_value key, int *err) {
    *err = 0;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return zz_clone(arr);
    size_t n = arr.arr->len;
    if (n <= 1) return zz_clone(arr);
    zz_dispatch_fn fn = zz_closure_target(key);
    size_t nenv = 0;
    void **env = zz_closure_env(key, &nenv);
    if (!fn) return zz_clone(arr);
    zz_sort_pair *pairs = (zz_sort_pair *)malloc(n * sizeof *pairs);
    zz_sort_pair *tmp = (zz_sort_pair *)malloc(n * sizeof *pairs);
    if (!pairs || !tmp) {
        free(pairs);
        free(tmp);
        return zz_clone(arr);
    }
    // Decorate: compute key for each element once.
    for (size_t i = 0; i < n; i++) {
        pairs[i].v = zz_clone(arr.arr->items[i]);
        pairs[i].idx = i;
        zz_value argv[] = { pairs[i].v };
        zz_value k = fn(argv, 1, env, nenv);
        pairs[i].k = zz_clone(k);
        zz_release(&k);
    }
    zz_task_frame *saved_fr = zz_green_frame();
    zz_green_frame_set(NULL);
    merge_sort_key(pairs, tmp, n);
    zz_green_frame_set(saved_fr);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(out.arr, pairs[i].v);
        zz_release(&pairs[i].v);
        zz_release(&pairs[i].k);
    }
    free(pairs);
    free(tmp);
    return out;
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
        zz_vec_append(arr, d.dict->entries[i].key, &sub_err);
    }
    return arr;
}

// dict.has(d, key)
zz_value zz_dict_has(zz_value d, zz_value key, int *err) {
    (void)err;
    if (d.tag != ZZ_DICT) return (zz_value){ZZ_BOOL, {.b = false}};
    size_t pos;
    return (zz_value){ZZ_BOOL, {.b = zz_dict_find(d.dict, key, &pos) != 0}};
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

// option.unwrap(opt) — unwrap .some(v) or abort like the VM's
// `called unwrap on .none` EvalError (native: `error:` + exit 1,
// matching `zz_option_expect`).
zz_value zz_option_unwrap(zz_value opt, int *err) {
    (void)err;
    if (opt.tag == ZZ_OPTION_SOME && opt.payload)
        return zz_clone(*opt.payload);
    if (opt.tag == ZZ_OPTION_NONE) {
        fprintf(stderr, "error: called unwrap on .none\n");
        exit(1);
    }
    fprintf(stderr, "error: option.unwrap: expected an option\n");
    exit(1);
}

// option.unwrap_or(opt, def) — `.some(v)` yields v, `.none` yields def.
zz_value zz_option_unwrap_or(zz_value opt, zz_value def, int *err) {
    (void)err;
    if (opt.tag == ZZ_OPTION_SOME && opt.payload)
        return zz_clone(*opt.payload);
    if (opt.tag == ZZ_OPTION_NONE)
        return zz_clone(def);
    fprintf(stderr, "error: option.unwrap_or: expected an option\n");
    exit(1);
}

// result.unwrap(res) — unwrap .ok(v) or abort like the VM's
// `result.unwrap: called unwrap on .err(e)` EvalError.
zz_value zz_result_unwrap(zz_value res, int *err) {
    (void)err;
    if (res.tag == ZZ_RESULT_OK && res.payload)
        return zz_clone(*res.payload);
    if (res.tag == ZZ_RESULT_ERR && res.payload) {
        char *estr = zz_value_to_string(res.payload);
        fprintf(stderr, "error: result.unwrap: called unwrap on .err(%s)\n", estr);
        free(estr);
    } else {
        fprintf(stderr, "error: result.unwrap: expected a result\n");
    }
    exit(1);
}

// result.unwrap_or(res, def) — `.ok(v)` yields v, `.err(_)` yields def.
zz_value zz_result_unwrap_or(zz_value res, zz_value def, int *err) {
    (void)err;
    if (res.tag == ZZ_RESULT_OK && res.payload)
        return zz_clone(*res.payload);
    if (res.tag == ZZ_RESULT_ERR)
        return zz_clone(def);
    fprintf(stderr, "error: result.unwrap_or: expected a result\n");
    exit(1);
}

// Index-trap with operands and function name (#251): called from lowered
// bounds-check failures instead of a bare "index out of bounds". Mirrors
// the VM wording (`index {i} out of bounds for length {len}`) plus the
// enclosing function, so native failures locate themselves without a
// VM bisection run. `func` is a ZZ identifier (safe for %s).
//
// Direct `obj[i]` indexing is NOT catchable in ZZ: the VM raises a hard
// runtime error (`normalize_index`), and `try`/`match` do not turn it into
// a Result. So the native trap must abort too, or the AOT program would
// keep running with a bogus unit. Catchable OOB reads go through
// `zz_array_at`/`vec.get`, which set *err and return unit without calling
// this. (#691 made this a no-op by mistake — that silently dropped the
// bounds trap and broke VM/native parity.)
void zz_index_trap(zz_value obj, zz_value idx, const char *func) {
    if (idx.tag == ZZ_INT) {
        long long i = (long long)idx.i;
        long long n = -1;
        if (obj.tag == ZZ_ARRAY && obj.arr)
            n = (long long)obj.arr->len;
        else if (obj.tag == ZZ_BYTES && obj.bytes)
            n = (long long)obj.bytes->len;
        else if (obj.tag == ZZ_STR && obj.s)
            n = (long long)obj.s->len;
        if (n >= 0) {
            fprintf(stderr, "zz error: index %lld out of bounds for length %lld in %s\n",
                    i, n, func);
            // Mirror the VM's `normalize_index` note so native and VM runs
            // explain the same failure the same way.
            if (n == 0)
                fprintf(stderr,
                        "hint: the value is empty, so no index is valid (check `len(...)` first)\n");
            else
                fprintf(stderr,
                        "hint: valid indices are 0..=%lld (or -1..=-%lld, counting from the end)\n",
                        n - 1, n);
            exit(1);
        }
        fprintf(stderr, "zz error: index %lld out of bounds in %s\n", i, func);
        exit(1);
    }
    if (idx.tag == ZZ_STR && obj.tag == ZZ_DICT && idx.s) {
        fprintf(stderr, "zz error: key `");
        size_t klen = idx.s->len < 64 ? idx.s->len : 64;
        fwrite(zz_str_cptr(idx.s), 1, klen, stderr);
        fprintf(stderr, "` not found in dict in %s\n", func);
        exit(1);
    }
    fprintf(stderr, "zz error: index out of bounds in %s\n", func);
    exit(1);
}

// Flat-array twin of `zz_index_trap`: the buffer is a raw `T *` with a
// tracked length, so the message is built from the operands directly.
void zz_index_trap_flat(long long i, long long n, const char *func) {
    fprintf(stderr, "zz error: index %lld out of bounds for length %lld in %s\n", i, n, func);
    if (n == 0)
        fprintf(stderr,
                "hint: the value is empty, so no index is valid (check `len(...)` first)\n");
    else
        fprintf(stderr,
                "hint: valid indices are 0..=%lld (or -1..=-%lld, counting from the end)\n",
                n - 1, n);
    exit(1);
}

// fs.read(path)
