// ZZ native runtime — memory allocation and GC/tracking helpers.
//
// Arena bump allocator for non-escaping local allocations plus thread-safe
// atomic reference counting (ARC) for heap objects that escape their
// creating scope. `zz_retain`/`zz_release`/`zz_assign`/`zz_clone` are the
// unified refcounting entry points used by generated code.

#ifndef ZZ_RUNTIME_MEMORY_H
#define ZZ_RUNTIME_MEMORY_H

#include "core.h"

#ifdef __cplusplus
extern "C" {
#endif

// Initialize an arena with a pre-allocated buffer of `cap` bytes.
// The buffer must outlive the arena (typically stack or a single malloc).
void zz_arena_init(zz_arena *a, size_t cap);

// Bump-allocate `size` bytes with `align` alignment from the arena.
// Returns NULL only if the arena is full (caller should fall back to ARC).
void *zz_arena_alloc(zz_arena *a, size_t size, size_t align);

// Destroy the arena, freeing its buffer.
void zz_arena_destroy(zz_arena *a);

// ---- thread-safe ARC ---------------------------------------------------
// Atomic reference counting for heap-objects that escape their creating
// scope. Thread-safe via __atomic builtins (no mutex overhead).
//
// All heap-allocated containers (arrays, dicts, funcs) carry an atomic
// refcount. zz_retain/zz_release use atomic increments/decrements.
// When the refcount drops to zero, the object is freed.

// Thread-safe retain: atomically increment the reference count.
void zz_retain_arc(zz_value *v);

// Thread-safe release: atomically decrement the reference count.
// If it reaches zero, free the object and recursively release contained values.
void zz_release_arc(zz_value *v);

// Clone for ARC objects: atomically bump refcount and return a copy.
zz_value zz_clone_arc(zz_value v);

// ---- refcounting -------------------------------------------------------
void zz_retain(zz_value *v);
void zz_release(zz_value *v);
void zz_assign(zz_value *dst, zz_value src);  // release dst, move src in
zz_value zz_clone(zz_value v);

// Release helpers for variant payloads and boxed objects. Defined in
// collections.c; forward-declared here so the ARC dispatchers in memory.c
// can call them.
void zz_release_variant(zz_value *v);
void zz_release_object(zz_value *v);

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_MEMORY_H