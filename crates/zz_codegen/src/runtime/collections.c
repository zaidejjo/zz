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
        // Stack/lit/arena arrays need migration to malloc before realloc
        // (realloc on arena/stack memory corrupts the heap — SIGSEGV
        // class; ARENA_MAGIC was missing here while zz_vec_append
        // handled it).
        if (a->refs == ZZ_ARRAY_STACK_MAGIC || a->refs == ZZ_ARRAY_LIT_MAGIC
            || a->refs == ZZ_ARRAY_ARENA_MAGIC || a->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(nc * sizeof(zz_value));
            for (size_t i = 0; i < a->len; i++) new_items[i] = a->items[i];
            a->items = new_items;
            // Preserve an accurate `refs == 1` (fresh heap array claiming
            // its first buffer): it proves single ownership, which the
            // move-aware push relies on. Anything else becomes 0.
            if (a->refs != 1) a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, nc * sizeof(zz_value));
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
// non-arrays and out-of-range indices.
zz_value zz_array_at(zz_value arr, size_t i) {
    if (arr.tag != ZZ_ARRAY || arr.arr == NULL || i >= arr.arr->len) {
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
    zz_value out = zz_array_new();
    for (size_t i = 0; i < a->len; i++) {
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

static size_t zz_dict_cap_for(size_t len) {
    size_t cap = 16;
    // Load factor <= 3/4 keeps linear probing short.
    while (cap - (cap >> 2) < len) {
        cap <<= 1;
    }
    return cap;
}

// 1 when the key can be indexed (VM `DictKey`: int, bool, str), 0 otherwise.
static int zz_dict_key_hash(zz_value key, size_t *out) {
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
        const unsigned char *p = (const unsigned char *)zz_str_cptr(key.s);
        size_t n = key.s->len;
        h = 1469598103934665603ULL;  // FNV-1a
        for (size_t i = 0; i < n; i++) {
            h ^= p[i];
            h *= 1099511628211ULL;
        }
        break;
    }
    default:
        return 0;
    }
    // splitmix64 finalizer: sequential int keys must not collide on the
    // low bits used for probing.
    h ^= h >> 30;
    h *= 0xbf58476d1ce4e5b9ULL;
    h ^= h >> 27;
    h *= 0x94d049bb133111ebULL;
    h ^= h >> 31;
    *out = (size_t)h;
    return 1;
}

// Place entry `j` in the index (no-op for a non-indexed key or no index).
// The first occurrence of a key wins, matching the linear scan's `find`.
static void zz_dict_idx_place(zz_dict *d, size_t j) {
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

// Allocate an index of `cap` slots from the arena (dies at reset) or the
// heap, then index every entry placed so far.
static void zz_dict_idx_build(zz_dict *d, zz_arena *arena, size_t cap) {
    if (cap == 0) return;
    size_t bytes = cap * sizeof(size_t);
    size_t *slots = arena ? (size_t *)zz_arena_alloc(arena, bytes, 8)
                          : (size_t *)calloc(1, bytes);
    if (!slots) return;
    if (arena) memset(slots, 0, bytes);
    d->idx = slots;
    d->idx_cap = cap;
    for (size_t j = 0; j < d->len; j++) {
        zz_dict_idx_place(d, j);
    }
}

// Keep the cache in step with a freshly appended entry.
static void zz_dict_idx_insert(zz_dict *d, size_t i) {
    size_t h;
    if (!zz_dict_key_hash(d->entries[i].key, &h)) return;  // unindexed key
    if (!d->idx) {
        // Linear lane until the dict outgrows the VM's threshold.
        if (d->len <= ZZ_DICT_INDEX_THRESHOLD) return;
        zz_dict_idx_build(d, NULL, zz_dict_cap_for(d->len));
        return;
    }
    if (d->len * 4 > d->idx_cap * 3) {
        // Arena-sized caches are fixed-capacity (bump-allocated): drop the
        // cache instead of growing it. Entries remain authoritative, so the
        // dict just falls back to the linear scan.
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
        for (size_t j = 0; j < d->len; j++) {
            zz_dict_idx_place(d, j);
        }
        return;
    }
    zz_dict_idx_place(d, i);
}

// First entry whose key equals `key`. Indexed keys probe the cache, every
// other key (and every small dict) takes the linear scan — the same result
// either way, since only int/bool/str keys ever enter the cache.
int zz_dict_find(const zz_dict *d, zz_value key, size_t *out) {
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
        size_t byte_lo, byte_hi;
        if (si >= nchars) {
            byte_lo = obj.s->len;
        } else {
            size_t cl = 1;
            byte_lo = zz_str_char_byte_off(obj.s, (size_t)si, &cl);
        }
        if (ei >= nchars) {
            byte_hi = obj.s->len;
        } else if (ei <= si) {
            byte_hi = byte_lo;
        } else {
            size_t cl = 1;
            byte_hi = zz_str_char_byte_off(obj.s, (size_t)ei, &cl);
        }
        zz_value out = zz_str_new(zz_str_cptr(obj.s) + byte_lo, byte_hi - byte_lo);
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
    }
    return out;
}

void zz_index_set(zz_value *obj, zz_value idx, zz_value item, int *err) {
    if (!obj) {
        if (err) *err = 1;
        return;
    }
    // Detach shared/sentinel buffers so the write below cannot leak
    // into co-owners (`a2 := a; a2[0] = x` leaves `a` untouched),
    // literals, or arenas. Uniquely-owned heap buffers (`refs == 1`,
    // the only exact state) write in place with zero copies;
    // arena/sentinel buffers heal to heap; shared heap buffers dup.
    if (obj->tag == ZZ_ARRAY && obj->arr && obj->arr->refs != 1) {
        zz_value fresh;
        if (zz_array_is_arena(obj->arr)) {
            fresh = zz_heal_for_move((zz_value){ZZ_ARRAY, {.arr = obj->arr}});
        } else {
            fresh = zz_array_dup(obj->arr);
        }
        if (!fresh.arr) {
            if (err) *err = 1;
            return;
        }
        zz_release_array(obj->arr);
        obj->arr = fresh.arr;
    } else if (obj->tag == ZZ_DICT && obj->dict && obj->dict->refs != 1) {
        zz_value fresh;
        if (zz_dict_is_arena(obj->dict)) {
            fresh = zz_heal_for_move((zz_value){ZZ_DICT, {.dict = obj->dict}});
        } else {
            fresh = zz_dict_dup_value(obj->dict);
        }
        if (!fresh.dict) {
            if (err) *err = 1;
            return;
        }
        zz_release_dict(obj->dict);
        obj->dict = fresh.dict;
    }
    switch (obj->tag) {
    case ZZ_ARRAY:
        zz_array_set(obj->arr, idx, item, err);
        return;
    case ZZ_DICT:
        zz_dict_set(obj->dict, idx, item);
        if (err) *err = 0;
        return;
    default:
        if (err) *err = 1;
    }
}

void zz_dict_set(zz_dict *d, zz_value key, zz_value val) {
    if (!d) return;
    size_t pos;
    if (zz_dict_find(d, key, &pos)) {
        // First occurrence wins on the key, the value is replaced
        // (mirrors the VM's `DictData::insert`).
        zz_assign(&d->entries[pos].val, val);
        return;
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
    // Keys use the share convention: callers keep their own share (borrowed
    // keys are common — interned field names, another dict's entry), so store
    // an independent share. `zz_clone_for_store` also heals arena-backed
    // keys to heap copies, else the loop-arena reset would corrupt the slot.
    e->key = zz_clone_for_store(key);
    // Values use the move convention (callers transfer ownership): arena-owned
    // values heal to heap copies so the stored element survives the loop-arena
    // reset; heap values are adopted as-is (no bump — same as the old
    // `zz_str_heal_arena` passthrough, extended to containers).
    e->val = zz_heal_for_move(val);
    zz_dict_idx_insert(d, d->len - 1);
}

zz_value zz_dict_get(const zz_dict *d, zz_value key, int *err) {
    *err = 0;
    if (!d) {
        *err = 1;
        return zz_unit();
    }
    size_t pos;
    if (zz_dict_find(d, key, &pos)) {
        return zz_clone(d->entries[pos].val);
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
    size_t nenv = 0;
    void **env = zz_closure_env(f, &nenv);
    zz_value arr = zz_iter_items(items);
    zz_value out = zz_array_new();
    if (!fn) return out;
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
    for (size_t i = 0; i < o->len; i++) {
        const zz_value *fname = &o->fields[i * 2];
        if (fname->tag == ZZ_STR && strcmp(zz_str_cptr(fname->s), name) == 0) {
            *out = zz_clone(o->fields[i * 2 + 1]);
            return 1;
        }
    }
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

// User-enum tag test for generated `match` guards: true only for boxed
// objects whose qualified type name matches exactly (`Token.IntLit`).
// Everything else (scalars, options, results, other objects) is false.
int zz_enum_is(const zz_value *v, const char *qualified) {
    if (!v || !qualified || v->tag != ZZ_OBJECT || !v->obj || !v->obj->type_name) return 0;
    return strcmp(v->obj->type_name, qualified) == 0;
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
    zz_release(&left);
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
            // Preserve `refs == 1` (see `zz_array_push`): only untracked
            // shares (sentinels) collapse to 0.
            if (a->refs != 1) a->refs = 0;  // Arena-allocated header, malloc'd items
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    // Share convention (was zz_clone): heal arena-owned values to heap so the
    // element survives the loop-arena reset.
    a->items[a->len++] = zz_clone_for_store(item);
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
            // Preserve `refs == 1` (see `zz_array_push`): dup results stay
            // provably single-owner, so a later move-aware push can go
            // in place instead of copying again.
            if (a->refs != 1) a->refs = 0;
        } else {
            a->items = (zz_value *)realloc(a->items, new_cap * sizeof(zz_value));
        }
        a->cap = new_cap;
    }
    // Share convention (was zz_clone): heal arena-owned values to heap so the
    // element survives the loop-arena reset.
    a->items[a->len++] = zz_clone_for_store(item);
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
        size_t new_cap = o->cap ? o->cap * 2 : 8;
        if (o->refs == ZZ_ARRAY_STACK_MAGIC || o->refs == ZZ_ARRAY_LIT_MAGIC
            || o->refs == ZZ_ARRAY_ARENA_MAGIC || o->items == NULL) {
            zz_value *new_items = (zz_value *)malloc(new_cap * sizeof(zz_value));
            for (size_t j = 0; j < o->len; j++) new_items[j] = o->items[j];
            o->items = new_items;
            // Preserve `refs == 1` (see `zz_array_push`).
            if (o->refs != 1) o->refs = 0;
        } else {
            o->items = (zz_value *)realloc(o->items, new_cap * sizeof(zz_value));
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
    for (size_t i = 0; i < arr.arr->len; i++) {
        if (zz_truthy(zz_binop(ZZOP_EQ, zz_clone(arr.arr->items[i]), zz_clone(item))))
            return (zz_value){ZZ_BOOL, {.b = true}};
    }
    return (zz_value){ZZ_BOOL, {.b = false}};
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

static int zz_sort_cmp_i64(const void *pa, const void *pb) {
    int64_t a = *(const int64_t *)pa, b = *(const int64_t *)pb;
    return (a > b) - (a < b);
}

static int zz_sort_cmp_f64(const void *pa, const void *pb) {
    // NaN compares equal to everything, like the VM's
    // `partial_cmp(..).unwrap_or(Ordering::Equal)`.
    double a = *(const double *)pa, b = *(const double *)pb;
    return (a > b) - (a < b);
}

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

// vec.sort(arr) → sorted array (new array, original unchanged)
zz_value zz_vec_sort(zz_value arr, int *err) {
    (void)err;
    if (arr.tag != ZZ_ARRAY || !arr.arr) return arr;
    size_t n = arr.arr->len;
    if (n <= 1) return zz_clone(arr);
    // Fresh array with one share of each element: the caller's array is
    // untouched (value semantics). `zz_array_push` shares (it stores its own
    // clone), so the elements stay borrowed here.
    zz_value sorted = zz_array_new();
    if (sorted.tag != ZZ_ARRAY || !sorted.arr) return zz_unit();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(sorted.arr, arr.arr->items[i]);
    }
    zz_value *items = sorted.arr->items;

    // Scalar lanes: no pair array and no key boxing. Homogeneity is the
    // common case (typed arrays) and removes every type check from the
    // comparator.
    unsigned char tag = items[0].tag;
    int homo = tag == ZZ_INT || tag == ZZ_FLOAT || tag == ZZ_BOOL || tag == ZZ_STR;
    for (size_t i = 1; homo && i < n; i++) {
        if (items[i].tag != tag) homo = 0;
    }
    if (homo && tag == ZZ_INT) {
        int64_t *keys = (int64_t *)malloc(n * sizeof *keys);
        if (!keys) return sorted;
        for (size_t i = 0; i < n; i++) keys[i] = items[i].i;
        qsort(keys, n, sizeof *keys, zz_sort_cmp_i64);
        for (size_t i = 0; i < n; i++) items[i].i = keys[i];
        free(keys);
        return sorted;
    }
    if (homo && tag == ZZ_FLOAT) {
        double *keys = (double *)malloc(n * sizeof *keys);
        if (!keys) return sorted;
        for (size_t i = 0; i < n; i++) keys[i] = items[i].f;
        qsort(keys, n, sizeof *keys, zz_sort_cmp_f64);
        for (size_t i = 0; i < n; i++) items[i].f = keys[i];
        free(keys);
        return sorted;
    }
    if (homo && tag == ZZ_BOOL) {
        // Only two values: counting beats every comparison sort.
        size_t falses = 0;
        for (size_t i = 0; i < n; i++) {
            if (!items[i].b) falses++;
        }
        for (size_t i = 0; i < n; i++) items[i].b = i >= falses;
        return sorted;
    }
    if (homo && tag == ZZ_STR) {
        qsort(items, n, sizeof *items, zz_sort_cmp_str);
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
// Insertion sort (stable) with a closure comparator called via the C ABI.
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

// Insertion sort over zz_sort_pair using a comparator with context.
// Stable by construction (only swaps when strictly greater).
static void insertion_sort_ctx(zz_sort_pair *pairs, size_t n,
                               int (*cmp)(void *, const zz_sort_pair *, const zz_sort_pair *),
                               void *thunk) {
    for (size_t i = 1; i < n; i++) {
        zz_sort_pair key = pairs[i];
        size_t j = i;
        while (j > 0 && cmp(thunk, &pairs[j - 1], &key) > 0) {
            pairs[j] = pairs[j - 1];
            j--;
        }
        pairs[j] = key;
    }
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
    if (!pairs) return zz_clone(arr);
    for (size_t i = 0; i < n; i++) {
        pairs[i].v = zz_clone(arr.arr->items[i]);
        pairs[i].k = (zz_value){ZZ_UNIT, {0}};
        pairs[i].idx = i;
    }
    // Sync-call hygiene: closures run outside the trampoline.
    zz_task_frame *saved_fr = zz_green_frame();
    zz_green_frame_set(NULL);
    sort_by_ctx ctx = { fn, env, nenv };
    insertion_sort_ctx(pairs, n, sort_by_cmp, &ctx);
    zz_green_frame_set(saved_fr);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(out.arr, pairs[i].v);
        zz_release(&pairs[i].v);
        zz_release(&pairs[i].k);
    }
    free(pairs);
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
    if (!pairs) return zz_clone(arr);
    for (size_t i = 0; i < n; i++) {
        pairs[i].v = zz_clone(arr.arr->items[i]);
        pairs[i].k = (zz_value){ZZ_UNIT, {0}};
        pairs[i].idx = i;
    }
    zz_task_frame *saved_fr = zz_green_frame();
    zz_green_frame_set(NULL);
    sort_by_desc_ctx ctx = { fn, env, nenv };
    insertion_sort_ctx(pairs, n, sort_by_desc_cmp, &ctx);
    zz_green_frame_set(saved_fr);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(out.arr, pairs[i].v);
        zz_release(&pairs[i].v);
        zz_release(&pairs[i].k);
    }
    free(pairs);
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

// Insertion sort using sort_key_cmp (no context).
static void insertion_sort_key(zz_sort_pair *pairs, size_t n) {
    for (size_t i = 1; i < n; i++) {
        zz_sort_pair key = pairs[i];
        size_t j = i;
        while (j > 0 && sort_key_cmp(&pairs[j - 1], &key) > 0) {
            pairs[j] = pairs[j - 1];
            j--;
        }
        pairs[j] = key;
    }
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
    if (!pairs) return zz_clone(arr);
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
    insertion_sort_key(pairs, n);
    zz_green_frame_set(saved_fr);
    zz_value out = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(out.arr, pairs[i].v);
        zz_release(&pairs[i].v);
        zz_release(&pairs[i].k);
    }
    free(pairs);
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

// fs.read(path)
