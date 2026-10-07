//! Deterministic string interner: each distinct string gets one `Symbol`.
//!
//! Single-map design (no sharding) so ids are stable for a given intern
//! order — required for deterministic build-cache keys. Single-threaded
//! compile interns in source order; identical sources always agree.
//!
//! Memory is bounded by distinct identifiers, not by occurrences: a 10k-line
//! file mentioning `self` 5k times stores it once.

use std::collections::HashMap;

/// Compact identifier handle. `u32` keeps AST nodes small; 4B identifiers
/// support every real-world codebase (4B distinct strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Symbol(pub u32);

impl Symbol {
    /// Raw id (for serialization / FFI).
    pub fn id(self) -> u32 {
        self.0
    }
}

/// Owned-string interner with deterministic ids.
#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<String, u32>,
    strings: Vec<Box<str>>,
}

impl Interner {
    /// Empty interner.
    pub fn new() -> Self {
        Interner::default()
    }

    /// Pre-size for an expected distinct-string count (e.g. ident estimate).
    pub fn with_capacity(distinct: usize) -> Self {
        Interner {
            map: HashMap::with_capacity(distinct),
            strings: Vec::with_capacity(distinct),
        }
    }

    /// Intern `s`, returning its stable id. Re-interning returns the same id.
    pub fn intern(&mut self, s: &str) -> Symbol {
        if let Some(&id) = self.map.get(s) {
            return Symbol(id);
        }
        let id = self.strings.len() as u32;
        // `u32` range is unreachable in practice; fail loudly, not silently.
        assert!(
            (id as usize) < u32::MAX as usize,
            "interner exhausted: too many distinct strings"
        );
        let owned: Box<str> = s.into();
        // SAFETY of the borrow: we clone the key for the map, so the map
        // never borrows from `strings` (no self-reference).
        self.map.insert(owned.to_string(), id);
        self.strings.push(owned);
        Symbol(id)
    }

    /// Resolve an id back to its string. Panics on unknown id (caller bug).
    pub fn resolve(&self, sym: Symbol) -> &str {
        self.strings
            .get(sym.0 as usize)
            .map(|s| &**s)
            .expect("unknown Symbol id")
    }

    /// Distinct string count.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Whether nothing is interned.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// Drop everything (e.g. per-file interner in long-lived daemons).
    pub fn clear(&mut self) {
        self.map.clear();
        self.strings.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedups_and_stable() {
        let mut it = Interner::new();
        let a = it.intern("self");
        let b = it.intern("self");
        let c = it.intern("other");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(it.resolve(a), "self");
        assert_eq!(it.resolve(c), "other");
        assert_eq!(it.len(), 2);
    }

    #[test]
    fn deterministic_order() {
        let mut i1 = Interner::new();
        let mut i2 = Interner::new();
        for s in ["a", "b", "c", "a", "b"] {
            i1.intern(s);
            i2.intern(s);
        }
        assert_eq!(i1.intern("b"), i2.intern("b"));
        assert_eq!(i1.len(), 3);
    }

    #[test]
    fn clear_resets() {
        let mut it = Interner::new();
        it.intern("x");
        it.clear();
        assert!(it.is_empty());
        // Ids restart deterministically after clear.
        assert_eq!(it.intern("x"), Symbol(0));
    }
}
