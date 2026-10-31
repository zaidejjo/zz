// ZZ native runtime — memory allocation and GC/tracking helpers.
//
// Arena bump allocator for non-escaping local allocations plus
// thread-safe atomic reference counting (ARC) for heap objects that
// escape their creating scope.
#include "runtime.h"

// =====================================================================
//  Arena allocator
//
//  A bump allocator for non-escaping local allocations. Each function gets
//  its own arena; allocations are O(1) pointer bumps and the entire arena
//  is freed in O(1) at function exit by resetting the offset to zero.
//
//  The arena uses a stack-allocated primary buffer (fast path) and falls
//  back to heap-allocated chunks when it fills up. Multiple chunks form
//  a singly-linked list; all are freed on destroy.
#define ZZ_ARENA_DEFAULT_CAP (256 * 1024)  // 256 KB primary block

// Thread-local arena for the current function scope.
// Each generated function sets up its own arena on entry and resets on exit.
static __thread zz_arena zz_thread_arena = {0};

void zz_arena_init(zz_arena *a, size_t cap) {
    if (cap < 1024) cap = 1024;
    a->buf = (char *)malloc(cap);
    if (!a->buf) {
        fprintf(stderr, "zz: arena out of memory\n");
        exit(1);
    }
    a->cap = cap;
    for (int i = 0; i < ZZ_ARENA_NCLASSES; i++) a->offsets[i] = 0;
    a->chunks = NULL;
}

void *zz_arena_alloc(zz_arena *a, size_t size, size_t align) {
    // Size-classed allocation: round up to nearest class, bump within that
    // class's region of the buffer. Small objects stay in small regions,
    // large objects in large regions — reducing external fragmentation
    // versus a single monolithic bump pointer.
    int cls = zz_arena_size_class_for(size);
    size_t class_size = ZZ_ARENA_CLASS_SIZES[cls];

    // Per-class region: buffer is divided into NCLASSES equal regions,
    // one per size class. Class `cls` owns [region_start, region_end).
    size_t region_size = a->cap / ZZ_ARENA_NCLASSES;
    size_t region_start = (size_t)cls * region_size;
    size_t region_end = region_start + region_size;

    size_t aligned_offset =
        (a->offsets[cls] + align - 1) & ~(align - 1);
    aligned_offset += region_start;

    if (aligned_offset + class_size <= region_end) {
        void *ptr = a->buf + aligned_offset;
        a->offsets[cls] = aligned_offset + class_size - region_start;
        return ptr;
    }

    // Class region full: adopt current buffer as an overflow chunk, then
    // allocate a fresh block large enough for every class's region.
    // Adopting keeps in-flight pointers valid (callers may hold pointers
    // into this buffer).
    size_t chunk_cap =
        (class_size > region_size) ? class_size * ZZ_ARENA_NCLASSES : region_size;

    // Check if any class region has data worth preserving.
    int has_data = 0;
    for (int i = 0; i < ZZ_ARENA_NCLASSES; i++) {
        if (a->offsets[i] > 0) { has_data = 1; break; }
    }
    if (a->buf && has_data) {
        zz_arena_chunk *old = (zz_arena_chunk *)malloc(sizeof(zz_arena_chunk));
        if (old) {
            old->buf = a->buf;
            old->cap = a->cap;
            old->next = a->chunks;
            a->chunks = old;
            a->buf = NULL;
        } else {
            free(a->buf);
            a->buf = NULL;
        }
    }

    // Drop any previous primary before installing the fresh block.
    free(a->buf);
    a->buf = (char *)malloc(chunk_cap);
    if (!a->buf) {
        fprintf(stderr, "zz: arena chunk out of memory\n");
        exit(1);
    }
    // Recompute region boundaries for the new block size so every class
    // gets a contiguous region starting at cls * region_size.
    a->cap = chunk_cap;
    size_t new_region_size = a->cap / ZZ_ARENA_NCLASSES;
    size_t new_region_start = (size_t)cls * new_region_size;
    for (int i = 0; i < ZZ_ARENA_NCLASSES; i++) a->offsets[i] = 0;
    a->offsets[cls] = class_size;
    return a->buf + new_region_start;
}

void zz_arena_destroy(zz_arena *a) {
    // Free the current primary block.
    if (a->buf) {
        free(a->buf);
        a->buf = NULL;
    }
    // Walk the overflow chunk list and free each adopted buffer + node.
    zz_arena_chunk *chunk = a->chunks;
    while (chunk) {
        zz_arena_chunk *next = chunk->next;
        free(chunk->buf);
        free(chunk);
        chunk = next;
    }
    a->chunks = NULL;
    a->cap = 0;
    for (int i = 0; i < ZZ_ARENA_NCLASSES; i++) a->offsets[i] = 0;
}

// ---- thread-safe ARC (atomic reference counting) -----------------------
// Uses __atomic builtins for lock-free thread safety.
// Arrays, dicts, and funcs carry an atomic refcount. When the refcount
// drops to zero, the object is freed.

void zz_retain_array(zz_array *a) {
    if (!a) return;
    // Stack-promoted (STACK_MAGIC) and fixed-literal (LIT_MAGIC) arrays
    // are not refcounted, and arena-allocated arrays (refs==0) are freed
    // in bulk at arena reset — skip the atomic increment entirely. This
    // keeps tight loops calling zz_clone() on literals atomic-free.
    if (a->refs == 0 || a->refs == ZZ_ARRAY_STACK_MAGIC || a->refs == ZZ_ARRAY_LIT_MAGIC
        || a->refs == ZZ_ARRAY_ARENA_MAGIC) {
        return;
    }
    __atomic_add_fetch(&a->refs, 1, __ATOMIC_RELAXED);
}

void zz_value_release(zz_value v) {
    zz_release(&v);
}

void zz_release_array(zz_array *a) {
    if (!a) return;
    // Stack-promoted arrays (codegen-set sentinel): header + items buffer
    // both live on the C stack. Nothing to free, but contained values may
    // still need releasing if they're heap-allocated refs.
    if (a->refs == ZZ_ARRAY_STACK_MAGIC) {
        for (size_t i = 0; i < a->len; i++) {
            zz_release(&a->items[i]);
        }
        return;
    }
    // Fixed-size literal arrays: header AND items buffer are both on the
    // arena (or a single pre-allocated heap block). Element stores bypassed
    // zz_clone, so the array owns no reference counts to release — the
    // bulk arena reset reclaims everything.
    if (a->refs == ZZ_ARRAY_LIT_MAGIC) {
        return;
    }
    // Arena-allocated arrays with pre-sized items buffer. If items were
    // never migrated to heap (still on arena), just release contained
    // values and let arena bulk-reset handle the rest. If migrated (refs
    // was reset to 0 by zz_vec_append), fall through to the refs==0 path.
    if (a->refs == ZZ_ARRAY_ARENA_MAGIC) {
        for (size_t i = 0; i < a->len; i++) {
            zz_release(&a->items[i]);
        }
        return;  // items still on arena — no individual free
    }
    // Arena-allocated arrays have refs==0 sentinel — skip atomic decrement.
    // They're freed in bulk at arena reset, not individually.
    if (a->refs == 0) {
        // Still need to release contained values that may be heap-allocated.
        for (size_t i = 0; i < a->len; i++) {
            zz_release(&a->items[i]);
        }
        free(a->items);  // items buffer always uses malloc
        return;
    }
    if (__atomic_sub_fetch(&a->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        for (size_t i = 0; i < a->len; i++) {
            zz_release(&a->items[i]);
        }
        free(a->items);
        free(a);
    }
}

void zz_retain_dict(zz_dict *d) {
    // Arena-allocated dicts have refs==0 sentinel — skip atomic increment so
    // a clone can never make a bulk-reset arena object look like it owns
    // heap refcounts (which would later free() arena memory).
    // ZZ_DICT_ARENA_MAGIC dicts are also arena-owned — skip retain.
    if (d && d->refs != 0 && d->refs != ZZ_DICT_ARENA_MAGIC)
        __atomic_add_fetch(&d->refs, 1, __ATOMIC_RELAXED);
}

void zz_release_dict(zz_dict *d) {
    if (!d) return;
    // Arena-allocated dicts have refs==0 sentinel — skip atomic decrement.
    if (d->refs == 0) {
        for (size_t i = 0; i < d->len; i++) {
            zz_release(&d->entries[i].key);
            zz_release(&d->entries[i].val);
        }
        // Entries buffer: arena-allocated (ZZ_DICT_ARENA_MAGIC) → skip free.
        // Regular arena dict (refs==0, no magic) → entries used malloc.
        if (d->refs != ZZ_DICT_ARENA_MAGIC) {
            free(d->entries);
        }
        // The key index is heap-built for refs==0 dicts (the arena-sized
        // constructor uses the magic sentinel and bump-allocates instead).
        free(d->idx);
        return;
    }
    if (d->refs == ZZ_DICT_ARENA_MAGIC) {
        // Arena-sized dict: entries are arena-allocated, skip individual free.
        // Only release the contained values (keys may be heap strings).
        // The key index lives on the same arena — nothing to free either.
        for (size_t i = 0; i < d->len; i++) {
            zz_release(&d->entries[i].key);
            zz_release(&d->entries[i].val);
        }
        return;
    }
    if (__atomic_sub_fetch(&d->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        for (size_t i = 0; i < d->len; i++) {
            zz_release(&d->entries[i].key);
            zz_release(&d->entries[i].val);
        }
        free(d->entries);
        free(d->idx);
        free(d);
    }
}

void zz_retain_func(zz_func *f) {
    if (f) __atomic_add_fetch(&f->refs, 1, __ATOMIC_RELAXED);
}

void zz_release_func(zz_func *f) {
    if (f && __atomic_sub_fetch(&f->refs, 1, __ATOMIC_ACQ_REL) == 0) {
        // Release captured env values.
        for (size_t i = 0; i < f->env_len; i++) {
            zz_release(&f->env[i]);
        }
        free(f->env);
        free(f);
    }
}

void zz_retain_arc(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned && v->s->refs > 0) {
            __atomic_add_fetch(&v->s->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v->arr);
        break;
    case ZZ_BYTES:
        zz_retain_bytes(v->bytes);
        break;
    case ZZ_DICT:
        zz_retain_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v->fn);
        break;
    case ZZ_RANGE:
        if (v->range) {
            __atomic_add_fetch(&v->range->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        zz_vbox_retain(v->payload);
        break;
    case ZZ_DB:
        if (v->db) zz_db_retain_handle(v->db);
        break;
    default:
        break;
    }
}

void zz_release_arc(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned) {
            // Arena-allocated strings have refs==0 sentinel — skip free.
            if (v->s->refs == 0) return;
            if (__atomic_sub_fetch(&v->s->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                // SSO strings (cap==0) have no separate heap buffer.
                // Heap strings (cap>0) store data in a separate malloc'd buffer.
                if (v->s->cap > 0) free(v->s->heap);
                zz_str_header_free(v->s);
            }
        }
        break;
    case ZZ_ARRAY:
        zz_release_array(v->arr);
        break;
    case ZZ_BYTES:
        zz_release_bytes(v->bytes);
        break;
    case ZZ_DICT:
        zz_release_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_release_func(v->fn);
        break;
    case ZZ_RANGE:
        if (v->range) {
            if (__atomic_sub_fetch(&v->range->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                free(v->range);
            }
        }
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        zz_release_variant(v);
        break;
    case ZZ_DB:
        if (v->db) {
            void *db = v->db;
            v->db = NULL;
            zz_db_release_handle(db);
        }
        break;
    case ZZ_WEAK:
        if (v->weak) {
            size_t w = __atomic_sub_fetch(&v->weak->weak, 1, __ATOMIC_ACQ_REL);
            if (w == 0) {
                // Last weak handle: release the referent and free the block.
                zz_release(&v->weak->value);
                free(v->weak);
                v->weak = NULL;
            }
        }
        break;
    default:
        break;
    }
}

zz_value zz_clone_arc(zz_value v) {
    switch (v.tag) {
    case ZZ_STR:
        if (v.s && !v.s->interned) {
            __atomic_add_fetch(&v.s->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v.arr);
        break;
    case ZZ_BYTES:
        zz_retain_bytes(v.bytes);
        break;
    case ZZ_DICT:
        zz_retain_dict(v.dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v.fn);
        break;
    case ZZ_RANGE:
        if (v.range) {
            __atomic_add_fetch(&v.range->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        zz_vbox_retain(v.payload);
        break;
    case ZZ_DB:
        if (v.db) zz_db_retain_handle(v.db);
        break;
    case ZZ_WEAK:
        if (v.weak) {
            __atomic_add_fetch(&v.weak->weak, 1, __ATOMIC_RELAXED);
        }
        break;
    default:
        break;
    }
    return v;
}

// ---- refcounting -------------------------------------------------------
// Unified refcounting entry points (zz_retain/zz_release/zz_assign/
// zz_clone) live as `static inline` in core.h so hot call sites pay no
// call + switch dispatch. The atomic ARC helpers above stay out-of-line.
// The `_arc` (all-atomic) variants below are kept for explicit use.
