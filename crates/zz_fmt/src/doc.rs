//! Doc IR: intermediate representation for the Wadler pretty-printer.
//!
//! `Doc<'src>` carries borrowed string slices whenever possible, so the
//! lowering pass is allocation-free for source text it copies verbatim.
//! Synthesized punctuation gets `Cow`/`String` via the `text` helper.
//!
//! Variants mirror the canonical Wadler set with a few additions:
//! - `SoftLine`: empty when flat, newline when broken (used inside groups
//!   for inner separation that disappears when everything fits).
//! - `HardLine`: always a newline (used for top-level statement breaks).
//! - `IfBreak`: pick one of two sub-docs based on whether the enclosing
//!   group broke. Used for trailing commas and other context-sensitive
//!   punctuation.

use std::borrow::Cow;

/// A piece of the formatter IR.
#[derive(Debug, Clone, Default)]
pub enum Doc<'src> {
    #[default]
    Nil,

    /// Borrowed source text — zero allocation.
    Text(&'src str),

    /// Owned text — used for synthesized punctuation/spaces.
    TextOwned(Cow<'src, str>),

    /// A line that becomes a single space when the enclosing group fits,
    /// or a newline+indent when it breaks. Use for inter-token spacing.
    Line,

    /// A line that is always a newline, with the enclosing indent applied.
    /// Use at statement-level and after top-level constructs.
    HardLine,

    /// A line that is empty when the enclosing group fits, or a
    /// newline+indent when it breaks. Use for inner separators that
    /// disappear on one line (e.g., args, fields).
    SoftLine,

    /// Concatenation.
    Concat(Vec<Doc<'src>>),

    /// A group: try to render `contents` flat; if it exceeds the
    /// remaining line width, break it and re-render. Line/SoftLine
    /// inside switch to newline mode.
    Group { contents: Box<Doc<'src>> },

    /// Increase the indent level for the duration of `contents`.
    Indent { contents: Box<Doc<'src>> },

    /// Pick `broken` if the enclosing group is broken, else `flat`.
    /// Does not affect width measurement.
    IfBreak {
        flat: Box<Doc<'src>>,
        broken: Box<Doc<'src>>,
    },
}

impl<'src> Doc<'src> {
    pub fn nil() -> Self {
        Doc::Nil
    }

    pub fn text(s: &'src str) -> Self {
        Doc::Text(s)
    }

    pub fn text_owned<S: Into<Cow<'src, str>>>(s: S) -> Self {
        Doc::TextOwned(s.into())
    }

    pub fn line() -> Self {
        Doc::Line
    }

    pub fn hard_line() -> Self {
        Doc::HardLine
    }

    pub fn soft_line() -> Self {
        Doc::SoftLine
    }

    pub fn group(contents: Doc<'src>) -> Self {
        Doc::Group {
            contents: Box::new(contents),
        }
    }

    pub fn indent(contents: Doc<'src>) -> Self {
        Doc::Indent {
            contents: Box::new(contents),
        }
    }

    pub fn if_break(flat: Doc<'src>, broken: Doc<'src>) -> Self {
        Doc::IfBreak {
            flat: Box::new(flat),
            broken: Box::new(broken),
        }
    }

    /// Concatenate two docs, flattening nested Concat where possible.
    pub fn concat(self, other: Doc<'src>) -> Self {
        match (self, other) {
            (Doc::Nil, b) => b,
            (a, Doc::Nil) => a,
            (Doc::Concat(mut a), Doc::Concat(mut b)) => {
                a.append(&mut b);
                Doc::Concat(a)
            }
            (Doc::Concat(mut a), b) => {
                a.push(b);
                Doc::Concat(a)
            }
            (a, Doc::Concat(mut b)) => {
                b.insert(0, a);
                Doc::Concat(b)
            }
            (a, b) => Doc::Concat(vec![a, b]),
        }
    }

    pub fn append(self, other: Doc<'src>) -> Self {
        self.concat(other)
    }
}

impl<'src> From<&'src str> for Doc<'src> {
    fn from(s: &'src str) -> Self {
        Doc::Text(s)
    }
}

impl<'src> From<String> for Doc<'src> {
    fn from(s: String) -> Self {
        Doc::TextOwned(Cow::Owned(s))
    }
}
