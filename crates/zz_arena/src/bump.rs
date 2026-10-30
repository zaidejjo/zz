//! `ScopedBump` — RAII guard that resets a [`Bump`] when its scope ends.
//!
//! Compilation phases never free individual nodes; instead a whole unit of
//! work (one `zz run`, one `zz check`, one `zz test`, one LSP request, one
//! per-file module load) is allocated into a bump arena and reclaimed in O(1)
//! when the phase ends. [`ScopedBump`] owns the [`Bump`] and resets it on
//! [`Drop`], so transient compiler allocations can never outlive their phase
//! and RSS stays flat in long-running processes (REPL, LSP, daemon).
//!
//! ```rust
//! # use zz_arena::{Bump, ScopedBump};
//! let mut work = ScopedBump::new();
//! let s = work.alloc_str("transient");
//! // `s` and everything else allocated in `work` are reclaimed here.
//! // O(1) reset; hot blocks are retained for the next unit.
//! drop(work);
//! ```
//!
//! All allocation goes through the guard ([`Deref`]/[`DerefMut`] to the inner
//! [`Bump`]), so nothing can accidentally bypass the reset.

use crate::Bump;

/// RAII wrapper around a [`Bump`] that resets it on drop.
///
/// The arena is reset (O(1), retaining the hot blocks for reuse) when the
/// guard goes out of scope, unless [`ScopedBump::shrink`] was used, in which
/// case all memory is returned to the OS.
///
/// [`Deref`]/[`DerefMut`] are implemented so you can call [`Bump`] methods
/// directly on the guard (`work.alloc(...)`, `work.checkpoint()`, …).
#[derive(Debug)]
pub struct ScopedBump {
    bump: Bump,
    /// Return all memory to the OS on drop instead of retaining hot blocks.
    shrink_on_drop: bool,
}

impl ScopedBump {
    /// Empty arena with default limits, owned by this guard.
    pub fn new() -> Self {
        Self {
            bump: Bump::new(),
            shrink_on_drop: false,
        }
    }

    /// Arena with custom limits, owned by this guard.
    pub fn with_config(config: crate::Config) -> Self {
        Self {
            bump: Bump::with_config(config),
            shrink_on_drop: false,
        }
    }

    /// Arena pre-sized from an expected byte count, owned by this guard.
    pub fn with_hint(expected_bytes: usize) -> Self {
        Self {
            bump: Bump::with_hint(expected_bytes),
            shrink_on_drop: false,
        }
    }

    /// Current limits.
    pub fn config(&self) -> &crate::Config {
        self.bump.config()
    }

    /// Copy of live counters.
    pub fn stats(&self) -> crate::Stats {
        self.bump.stats()
    }

    /// Return all memory to the OS when this guard drops (no retained
    /// blocks). Use at daemon/LSP idle or shutdown, not per unit — per-unit
    /// shrinking defeats the block-reuse amortization.
    pub fn shrink(mut self) -> Self {
        self.shrink_on_drop = true;
        self
    }

    /// Manually reset the arena before the guard drops. The drop then
    /// resets again (harmless idempotent O(1)).
    pub fn reset(&mut self) {
        self.bump.reset();
    }

    /// Manually return all memory to the OS before the guard drops.
    pub fn shrink_now(&mut self) {
        self.bump.shrink();
    }

    /// Detach the inner [`Bump`] without resetting it. The caller takes full
    /// ownership and is responsible for calling [`Bump::reset`] /
    /// [`Bump::shrink`] when done.
    pub fn into_inner(mut self) -> Bump {
        // Move the live bump out, leaving a fresh empty arena behind so the
        // upcoming `forget` makes `Drop` a harmless no-op reset of nothing.
        let bump = std::mem::take(&mut self.bump);
        std::mem::forget(self);
        bump
    }
}

impl Drop for ScopedBump {
    fn drop(&mut self) {
        if self.shrink_on_drop {
            self.bump.shrink();
        } else {
            self.bump.reset();
        }
    }
}

impl std::ops::Deref for ScopedBump {
    type Target = Bump;

    fn deref(&self) -> &Self::Target {
        &self.bump
    }
}

impl std::ops::DerefMut for ScopedBump {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bump
    }
}

impl Default for ScopedBump {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_bump_alloc_through_deref() {
        let mut work = ScopedBump::new();
        let s = work.alloc_str("hello");
        assert_eq!(s, "hello");
        let x = work.alloc(7u64);
        assert_eq!(*x, 7);
        assert!(work.stats().bytes_used > 0);
    }

    #[test]
    fn scoped_bump_checkpoint_rewind() {
        let mut work = ScopedBump::new();
        work.alloc(1u64);
        let cp = work.checkpoint();
        work.alloc(2u64);
        work.rewind(cp);
        assert_eq!(work.stats().bytes_used, 8);
    }

    #[test]
    fn scoped_bump_manual_reset_then_drop_is_idempotent() {
        let mut work = ScopedBump::new();
        work.alloc(42u64);
        assert!(work.stats().bytes_used > 0);
        work.reset();
        assert_eq!(work.stats().bytes_used, 0);
        drop(work); // must not panic or double-free
    }

    #[test]
    fn scoped_bump_into_inner_preserves_without_reset() {
        let mut work = ScopedBump::new();
        work.alloc(42u64);
        assert!(work.stats().bytes_used > 0);
        let mut bump = work.into_inner();
        // `into_inner` does NOT reset: the payload is still there.
        assert_eq!(bump.stats().bytes_used, 8);
        // Caller is now responsible for reset/shrink.
        bump.reset();
        assert_eq!(bump.stats().bytes_used, 0);
    }

    #[test]
    fn scoped_bump_shrink_guard_drops_cleanly() {
        let mut work = ScopedBump::with_hint(1024 * 1024).shrink();
        let _big = work.alloc_slice_copy(&[0u8; 64 * 1024]);
        assert!(work.stats().bytes_used > 0);
        drop(work); // returns memory to OS, must not panic
                    // A fresh arena starts empty.
        let after = ScopedBump::new();
        assert_eq!(after.stats().bytes_used, 0);
    }

    #[test]
    fn scoped_bump_default_is_empty() {
        let work = ScopedBump::default();
        assert_eq!(work.stats().bytes_used, 0);
    }
}
