//! Source spans: byte ranges into the original source text.
//!
//! Every AST node carries a `Span` covering its exact source text. This is
//! what makes the printer lossless (it re-emits source slices verbatim) and
//! gives diagnostics precise locations.

use std::fmt;
use std::ops::Range;

/// A half-open byte range `[start, end)` into a source string.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Union with another span, covering both.
    pub fn join(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn to_range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_covers_both() {
        let a = Span::new(2, 5);
        let b = Span::new(0, 3);
        assert_eq!(a.join(b), Span::new(0, 5));
    }
}
