//! Conversion between ZZ byte-offset spans and LSP line-column positions.

use tower_lsp::lsp_types::{DiagnosticSeverity, Position, Range};
use zz_frontend::diag::Severity;
use zz_frontend::span::Span;

/// Convert a byte offset to an LSP `Position` (0-based line, 0-based character).
pub fn offset_to_position(source: &str, offset: u32) -> Position {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in source.char_indices() {
        if (i as u32) >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Position {
        line,
        character: col,
    }
}

/// Convert a ZZ `Span` to an LSP `Range`.
pub fn span_to_range(source: &str, span: Span) -> Range {
    Range {
        start: offset_to_position(source, span.start),
        end: offset_to_position(source, span.end),
    }
}

/// Convert an LSP `Position` to a byte offset.
pub fn position_to_offset(source: &str, pos: Position) -> u32 {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in source.char_indices() {
        if line == pos.line && col == pos.character {
            return i as u32;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    source.len() as u32
}

/// Map a ZZ diagnostic severity to an LSP diagnostic severity.
pub fn severity_to_lsp(sev: Severity) -> DiagnosticSeverity {
    match sev {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Help => DiagnosticSeverity::HINT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_simple() {
        let src = "hello\nworld";
        assert_eq!(
            offset_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position(src, 5),
            Position {
                line: 0,
                character: 5
            }
        );
        assert_eq!(
            offset_to_position(src, 6),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position(src, 11),
            Position {
                line: 1,
                character: 5
            }
        );
    }

    #[test]
    fn range_simple() {
        let src = "abc\ndef";
        let span = Span::new(0, 3);
        let range = span_to_range(src, span);
        assert_eq!(
            range.start,
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            range.end,
            Position {
                line: 0,
                character: 3
            }
        );
    }

    #[test]
    fn range_cross_line() {
        let src = "abc\ndef";
        let span = Span::new(2, 5);
        let range = span_to_range(src, span);
        assert_eq!(
            range.start,
            Position {
                line: 0,
                character: 2
            }
        );
        assert_eq!(
            range.end,
            Position {
                line: 1,
                character: 1
            }
        );
    }

    #[test]
    fn roundtrip() {
        let src = "aaa\nbbb\nccc";
        let pos = Position {
            line: 2,
            character: 1,
        };
        let offset = position_to_offset(src, pos);
        let back = offset_to_position(src, offset);
        assert_eq!(back, pos);
    }
}
