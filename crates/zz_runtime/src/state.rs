//! Shared mutable state registry for the VM engine.
//!
//! One process-global reentrant lock per registry (D7). The registry is
//! owned by the program run and shared via `Arc` with worker interpreters
//! (spawned tasks). Reset per `Interp`/test for isolation
//! (see `docs/test-isolation.md`).
//!
//! # Locking and reentrancy
//!
//! State access is mutually exclusive between OS threads via the reentrant
//! state lock (`acquire`/`release`). Within one thread, `state_lock_depth`
//! on the VM tracks `atomic { }` nesting: the `Op::Atomic` arm acquires the
//! lock for the whole body and the frame exit releases it. When
//! `state_lock_depth > 0`, the current thread already owns the lock, so
//! state ops access the map directly (no re-lock, no deadlock).
//! Because tasks never yield while `state_lock_depth > 0` (D3 runtime
//! guard), the lock is never held across a suspension.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;

use crate::value::Value;

/// A process-global registry of `state` bindings, shared by all
/// interpreters running the same program (including worker tasks).
#[derive(Debug)]
pub struct StateRegistry {
    /// The state map. Direct access via `map_mut()` is only safe when the
    /// caller owns the state lock (tracked by `state_lock_depth` on the VM).
    states: UnsafeCell<HashMap<String, Value>>,
    /// Reentrant lock ownership: `(owning thread, acquisition depth)`.
    /// Guarded only for the bookkeeping itself — the state map is protected
    /// by the ownership protocol, not by holding this mutex.
    owner: Mutex<Option<(ThreadId, usize)>>,
    /// Wakes threads blocked in [`StateRegistry::acquire`].
    released: Condvar,
}

unsafe impl Send for StateRegistry {}
unsafe impl Sync for StateRegistry {}

impl StateRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Acquire the reentrant state lock on the current thread. A nested
    /// acquisition by the owner bumps the depth; another thread blocks
    /// until every level held by the owner is released.
    pub fn acquire(&self) {
        let me = std::thread::current().id();
        let mut owner = self.owner.lock().unwrap();
        loop {
            match owner.as_mut() {
                None => {
                    *owner = Some((me, 1));
                    return;
                }
                Some((id, depth)) if *id == me => {
                    *depth += 1;
                    return;
                }
                Some(_) => owner = self.released.wait(owner).unwrap(),
            }
        }
    }

    /// Release one level of the state lock held by the current thread.
    pub fn release(&self) {
        let me = std::thread::current().id();
        let mut owner = self.owner.lock().unwrap();
        if let Some((id, depth)) = owner.as_mut() {
            if *id == me {
                *depth -= 1;
                if *depth == 0 {
                    *owner = None;
                    drop(owner);
                    self.released.notify_one();
                }
                return;
            }
        }
        debug_assert!(
            false,
            "state lock released by a thread that does not hold it"
        );
    }

    /// RAII guard for short critical sections (`init`/`get`/`set`/`rmw` and
    /// a single state op). `atomic { }` bodies use `acquire`/`release`
    /// explicitly because the hold spans many VM ops.
    pub fn lock(&self) -> StateGuard<'_> {
        self.acquire();
        StateGuard { reg: self }
    }

    /// Execute `f` while holding the state lock.
    pub fn with_lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = self.lock();
        f()
    }

    /// Unsafe: return `&mut` to the state map. Caller MUST hold the lock.
    /// Used by the VM when `state_lock_depth > 0` (inside `atomic { }`).
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn map_mut(&self) -> &mut HashMap<String, Value> {
        unsafe { &mut *self.states.get() }
    }

    /// Initialize a state binding (module-level declaration).
    pub fn init(&self, name: &str, value: Value) {
        let _guard = self.lock();
        unsafe {
            let map = &mut *self.states.get();
            map.insert(name.to_string(), value);
        }
    }

    /// Get a state value by name. Locks per-access.
    pub fn get(&self, name: &str) -> Option<Value> {
        let _guard = self.lock();
        unsafe { self.map_mut().get(name).cloned() }
    }

    /// Set a state value by name. Locks per-access.
    pub fn set(&self, name: &str, value: Value) {
        let _guard = self.lock();
        unsafe {
            let map = &mut *self.states.get();
            map.insert(name.to_string(), value);
        }
    }

    /// Read-modify-write on a state value. One lock acquisition.
    pub fn rmw<F>(&self, name: &str, op: F) -> Value
    where
        F: FnOnce(Value) -> Value,
    {
        let _guard = self.lock();
        unsafe {
            let map = &mut *self.states.get();
            let cur = map.get(name).cloned().unwrap_or(Value::Unit);
            let new = op(cur);
            map.insert(name.to_string(), new.clone());
            new
        }
    }
}

/// RAII handle for one level of the reentrant state lock.
pub struct StateGuard<'a> {
    reg: &'a StateRegistry,
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        self.reg.release();
    }
}

impl Default for StateRegistry {
    fn default() -> Self {
        Self {
            states: UnsafeCell::new(HashMap::new()),
            owner: Mutex::new(None),
            released: Condvar::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_get_set() {
        let reg = StateRegistry::new();
        reg.init("hits", Value::Int(0));
        assert_eq!(reg.get("hits"), Some(Value::Int(0)));
        reg.set("hits", Value::Int(1));
        assert_eq!(reg.get("hits"), Some(Value::Int(1)));
    }

    #[test]
    fn state_rmw() {
        let reg = StateRegistry::new();
        reg.init("hits", Value::Int(5));
        let new = reg.rmw("hits", |v| match v {
            Value::Int(n) => Value::Int(n + 1),
            other => other,
        });
        assert_eq!(new, Value::Int(6));
        assert_eq!(reg.get("hits"), Some(Value::Int(6)));
    }

    #[test]
    fn state_with_lock() {
        let reg = StateRegistry::new();
        reg.init("n", Value::Int(0));
        reg.with_lock(|| unsafe {
            let map = reg.map_mut();
            let v = map.get("n").cloned().unwrap_or(Value::Unit);
            map.insert(
                "n".to_string(),
                match v {
                    Value::Int(x) => Value::Int(x + 1),
                    _ => v,
                },
            );
        });
        assert_eq!(reg.get("n"), Some(Value::Int(1)));
    }

    #[test]
    fn reentrant_acquire_release() {
        let reg = StateRegistry::new();
        reg.acquire();
        reg.acquire();
        reg.release();
        // Still owned after the inner release: a per-op lock must not block.
        reg.set("n", Value::Int(1));
        reg.release();
        assert_eq!(reg.get("n"), Some(Value::Int(1)));
    }

    #[test]
    fn acquire_blocks_other_threads() {
        let reg = StateRegistry::new();
        reg.acquire();
        let clone = Arc::clone(&reg);
        let handle = std::thread::spawn(move || clone.get("n"));
        // Give the worker a chance to block, then release.
        std::thread::sleep(std::time::Duration::from_millis(20));
        reg.release();
        assert_eq!(handle.join().unwrap(), None);
    }
}
