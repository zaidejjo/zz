//! Trivia classification: comments, blank lines, and doc-comments.
//!
//! The lexer's lossless `leading` field gives us trivia attached to each
//! significant token. We re-classify it here so the IR layer can make
//! policy decisions (e.g., "this blank line should be preserved" vs
//! "this is whitespace inside a group that we can normalize").
//!
//! Classification rules (mirrors the lexer's `TriviaKind` but adds
//! `BlankLine` and `DocComment`):
//!
//! - Whitespace run that contains at least one newline and no comments →
//!   `BlankLine` (count of newlines = blank-line count + 1).
//! - `//` or `///` → `Line` or `Doc` comment.
//! - `/* ... */` → `Block` comment.
//! - Whitespace run without newlines → `Spacing` (carry the literal
//!   text so we can reproduce original spacing inside lossless sections).

use zz_frontend::token::{Trivia, TriviaKind};

/// What kind of trivia a chunk represents.
#[derive(Debug, Clone, PartialEq, Eq)]
    /// A single newline (hard line).
pub enum ClassifiedKind {
    /// Whitespace run that may contain a single newline (infix space).
    /// Carry the original text for fidelity.
    Spacing(String),
    /// One or more blank lines between tokens.
    Newline,
    BlankLine { count: usize },
    /// `//` line comment (without the trailing newline).
    Line(String),
    /// `///` doc comment.
    Doc(String),
    /// `/* ... */` block comment.
    Block(String),
}

/// A classified trivia chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedTrivia {
    pub kind: ClassifiedKind,
    /// Byte span back into the original source.
    pub start: u32,
    pub end: u32,
}

impl ClassifiedTrivia {
    pub fn is_comment(&self) -> bool {
        matches!(
            self.kind,
            ClassifiedKind::Line(_) | ClassifiedKind::Doc(_) | ClassifiedKind::Block(_)
        )
    }
}

/// Classify the `leading` trivia of a single token.
///
/// `keep_spacing` controls whether the original intra-group whitespace
/// is carried verbatim. For lossless printing (Phase: source-equal
/// output) we keep it; for pretty-printing we drop it.
pub fn classify(trivia: &[Trivia], keep_spacing: bool) -> Vec<ClassifiedTrivia> {
    let mut out = Vec::new();
    for t in trivia {
        match t.kind {
            TriviaKind::Whitespace => {
                if keep_spacing {
                    if let Some(c) = classify_whitespace(&t.text) {
                        out.push(c);
                    }
                }
            }
            TriviaKind::Newline => {
                out.push(ClassifiedTrivia {
                    kind: ClassifiedKind::BlankLine { count: 1 },
                    start: t.span.start,
                    end: t.span.end,
                });
            }
            TriviaKind::Comment => {
                if let Some(c) = classify_comment(&t.text) {
                    out.push(ClassifiedTrivia {
                        kind: c,
                        start: t.span.start,
                        end: t.span.end,
                    });
                }
            }
        }
    }
    out
}

fn classify_whitespace(text: &str) -> Option<ClassifiedTrivia> {
    // Count newlines; if any present, the trivia ends a line.
    let newlines = text.bytes().filter(|b| *b == b'\n').count();
    if newlines == 0 {
        // Pure inline spacing — preserve as-is for fidelity.
        if text.is_empty() {
            return None;
        }
        return Some(ClassifiedTrivia {
            kind: ClassifiedKind::Spacing(text.to_string()),
            start: 0,
            end: 0,
        });
    }
    Some(ClassifiedTrivia {
        kind: ClassifiedKind::BlankLine {
            count: newlines.max(1),
        },
        start: 0,
        end: 0,
    })
}

fn classify_comment(text: &str) -> Option<ClassifiedKind> {
    let stripped = text
        .strip_prefix("///")
        .unwrap_or_else(|| text.strip_prefix("//").unwrap_or(text));
    let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
    if text.starts_with("///") {
        Some(ClassifiedKind::Doc(stripped.to_string()))
    } else if text.starts_with("//") {
        Some(ClassifiedKind::Line(stripped.to_string()))
    } else if text.starts_with("/*") && text.ends_with("*/") {
        let inner = &text[2..text.len() - 2];
        let inner = inner.strip_prefix('*').unwrap_or(inner);
        let inner = inner
            .strip_suffix('/')
            .map(|s| &s[..s.len() - 1])
            .unwrap_or(inner);
        Some(ClassifiedKind::Block(inner.trim().to_string()))
    } else {
        // Unknown comment shape — treat as a block.
        Some(ClassifiedKind::Block(text.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_line_comment() {
        let k = classify_comment("// hello").unwrap();
        assert_eq!(k, ClassifiedKind::Line("hello".to_string()));
    }

    #[test]
    fn classify_doc_comment() {
        let k = classify_comment("/// doc").unwrap();
        assert_eq!(k, ClassifiedKind::Doc("doc".to_string()));
    }

    #[test]
    fn classify_block_comment() {
        let k = classify_comment("/* hi */").unwrap();
        assert_eq!(k, ClassifiedKind::Block("hi".to_string()));
    }

    #[test]
    fn classify_nested_block() {
        let k = classify_comment("/** hi */").unwrap();
        assert!(matches!(k, ClassifiedKind::Block(_)));
    }
}
