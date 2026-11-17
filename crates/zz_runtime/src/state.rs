//! Shared mutable state registry for the VM engine.
//!
//! One process-global mutex per registry (D7). The registry is owned by
//! the program run and shared via `Arc` with worker interpreters
//! (spawned tasks). Reset per `Interp`/test for isolation
//! (see `docs/test-isolation.md`).
//!
//! # Locking and reentrancy
//!
//! State access is mutually exclusive between OS threads via `mutex`.
//! Within a single thread (green task), `state_lock_depth` on the VM
//! tracks nesting: `atomic { }` increments it on entry and decrements on
//! exit. When `state_lock_depth > 0`, the current thread holds the mutex
//! and state ops access the map directly (no re-lock, no deadlock).
//! Because green tasks never yield while `state_lock_depth > 0`
//! (D3 runtime guard), no other green task on the same thread can
//! interleave.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::value::Value;

/// A process-global registry of `state` bindings, shared by all
/// interpreters running the same program (including worker tasks).
#[derive(Debug)]
pub struct StateRegistry {
    /// The state map. Direct access via `map_mut()` is only safe when the
    /// caller holds `mutex` (tracked by `state_lock_depth` on the VM).
    states: UnsafeCell<HashMap<String, Value>>,
    /// Mutex for mutual exclusion between OS threads.
    mutex: Mutex<()>,
    /// Shared reentrancy depth counter (informational; the authoritative
    /// per-thread depth lives on the `Vm`).
    depth: AtomicUsize,
}

unsafe impl Send for StateRegistry {}
unsafe impl Sync for StateRegistry {}

impl StateRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            states: UnsafeCell::new(HashMap::new()),
            mutex: Mutex::new(()),
            depth: AtomicUsize::new(0),
        })
    }

    /// Acquire the state lock. Returns a guard that releases on drop.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.mutex.lock().unwrap()
    }

    /// Execute `f` while holding the state lock.
    pub fn with_lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = self.lock();
        self.depth.fetch_add(1, Ordering::SeqCst);
        let result = f();
        self.depth.fetch_sub(1, Ordering::SeqCst);
        result
    }

    /// Unsafe: return `&mut` to the state map. Caller MUST hold `mutex`.
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

impl Default for StateRegistry {
    fn default() -> Self {
        Self {
            states: UnsafeCell::new(HashMap::new()),
            mutex: Mutex::new(()),
            depth: AtomicUsize::new(0),
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
}
