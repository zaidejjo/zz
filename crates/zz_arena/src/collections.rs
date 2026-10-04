//! Arena-backed helpers: stage-then-freeze collections without realloc churn.
//!
//! Pattern: push into [`ArenaVec`] (inline-staged), then [`ArenaVec::into_slice`]
//! copies once into the arena. Long-lived data costs exactly one copy and
//! zero reallocs after freeze — the common AST-build shape.

use crate::Bump;

/// Staging vector that freezes into arena memory once.
#[derive(Debug, Default)]
pub struct ArenaVec<T> {
    items: Vec<T>,
}

impl<T> ArenaVec<T> {
    /// Empty staging vec.
    pub fn new() -> Self {
        ArenaVec { items: Vec::new() }
    }

    /// Pre-size staging (avoids even staging reallocs when known).
    pub fn with_capacity(cap: usize) -> Self {
        ArenaVec {
            items: Vec::with_capacity(cap),
        }
    }

    /// Push one item.
    pub fn push(&mut self, item: T) {
        self.items.push(item);
    }

    /// Staged length.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing is staged.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Freeze `Copy` items into the arena with a single copy.
    pub fn into_slice_copy(self, bump: &mut Bump) -> &mut [T]
    where
        T: Copy,
    {
        bump.alloc_slice_copy(&self.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_and_freeze() {
        let mut bump = Bump::new();
        let mut v = ArenaVec::new();
        for i in 0..100u64 {
            v.push(i);
        }
        let s = v.into_slice_copy(&mut bump);
        assert_eq!(s.len(), 100);
        assert_eq!(s[99], 99);
    }

    #[test]
    fn empty_freeze() {
        let mut bump = Bump::new();
        let v: ArenaVec<u64> = ArenaVec::new();
        assert!(v.is_empty());
        let s = v.into_slice_copy(&mut bump);
        assert!(s.is_empty());
    }
}
