//! Conversion between ZZ byte-offset spans and LSP line-column positions.
//!
//! LSP specifies that character positions are **UTF-16 code unit offsets**
//! from the line start. This module handles the mapping correctly for
//! multi-byte UTF-8 characters (CJK, emoji, etc.) and surrogate pairs.

use tower_lsp::lsp_types::{DiagnosticSeverity, Position, Range};
use zz_frontend::diag::Severity;
use zz_frontend::span::Span;

// ── Line index (cached) ──────────────────────────────────────────────────

/// Precomputed index for O(log n) line lookups and O(k) UTF-16 position conversion.
///
/// Caches line start byte offsets so repeated position lookups on the same
/// document don't re-scan the entire source.
#[derive(Clone, Debug)]
pub struct LineIndex {
    /// Byte offset of the start of each line.
    line_starts: Vec<usize>,
}

impl LineIndex {
    /// Build the index from a source string.
    ///
    /// Handles both `\n` and `\r\n` line endings. For `\r\n`, the line start
    /// is placed after both characters so the `\r` is part of the previous line.
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        let bytes = source.as_bytes();
        let len = bytes.len();
        let mut i = 0;
        while i < len {
            if bytes[i] == b'\n' {
                let next = i + 1;
                if next <= len {
                    line_starts.push(next);
                }
                i = next;
            } else {
                i += 1;
            }
        }
        Self { line_starts }
    }

    /// Number of lines in the source.
    #[allow(dead_code)]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Find the line number (0-based) for a byte offset.
    fn line_for_offset(&self, offset: u32) -> usize {
        match self.line_starts.binary_search(&(offset as usize)) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// Convert a byte offset to an LSP `Position` using UTF-16 columns.
    pub fn offset_to_position(&self, source: &str, offset: u32) -> Position {
        let line = self.line_for_offset(offset);
        let line_start = self.line_starts[line];
        let col = utf16_column(source, line_start, offset as usize);
        Position {
            line: line as u32,
            character: col,
        }
    }

    /// Convert an LSP `Position` (UTF-16 column) to a byte offset.
    pub fn position_to_offset(&self, source: &str, pos: Position) -> u32 {
        let line = pos.line as usize;
        if line >= self.line_starts.len() {
            return source.len() as u32;
        }
        let line_start = self.line_starts[line];
        byte_offset_for_utf16_col(source, line_start, pos.character)
    }

    /// Convert a `Span` to an LSP `Range`.
    pub fn span_to_range(&self, source: &str, span: Span) -> Range {
        Range {
            start: self.offset_to_position(source, span.start),
            end: self.offset_to_position(source, span.end),
        }
    }
}

// ── UTF-16 helpers ───────────────────────────────────────────────────────

/// Compute the UTF-16 column for byte offset `end` within a line starting at `line_start`.
fn utf16_column(source: &str, line_start: usize, end: usize) -> u32 {
    if end <= line_start {
        return 0;
    }
    let mut col = 0u32;
    for ch in source[line_start..end].chars() {
        col += ch.len_utf16() as u32;
    }
    col
}

/// Find the byte offset within a line for a given UTF-16 column.
///
/// If the column falls in the middle of a surrogate pair, the result is the
/// start of that character (rounding down).
fn byte_offset_for_utf16_col(source: &str, line_start: usize, utf16_col: u32) -> u32 {
    let mut col = 0u32;
    for (i, ch) in source[line_start..].char_indices() {
        let char_len = ch.len_utf16() as u32;
        if col + char_len > utf16_col {
            return (line_start + i) as u32;
        }
        col += char_len;
    }
    source.len() as u32
}

// ── Convenience wrappers (build temp index) ──────────────────────────────

/// Convert a byte offset to an LSP `Position` (UTF-16 columns).
pub fn offset_to_position(source: &str, offset: u32) -> Position {
    let index = LineIndex::new(source);
    index.offset_to_position(source, offset)
}

/// Convert a `Span` to an LSP `Range`.
pub fn span_to_range(source: &str, span: Span) -> Range {
    let index = LineIndex::new(source);
    index.span_to_range(source, span)
}

/// Convert an LSP `Position` to a byte offset.
pub fn position_to_offset(source: &str, pos: Position) -> u32 {
    let index = LineIndex::new(source);
    index.position_to_offset(source, pos)
}

/// Map a ZZ diagnostic severity to an LSP diagnostic severity.
pub fn severity_to_lsp(sev: Severity) -> DiagnosticSeverity {
    match sev {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Help => DiagnosticSeverity::HINT,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_simple_ascii() {
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
    fn roundtrip_ascii() {
        let src = "aaa\nbbb\nccc";
        let pos = Position {
            line: 2,
            character: 1,
        };
        let offset = position_to_offset(src, pos);
        let back = offset_to_position(src, offset);
        assert_eq!(back, pos);
    }

    // ── UTF-16 tests ─────────────────────────────────────────────────────

    #[test]
    fn utf16_cjk_two_byte_utf8_one_unit() {
        // "中" = U+4E2D, 3 bytes UTF-8, 1 UTF-16 code unit.
        let src = "中a";
        // '中' at byte 0, 'a' at byte 3.
        assert_eq!(
            offset_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position(src, 3),
            Position {
                line: 0,
                character: 1
            }
        );
        // Roundtrip.
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 0
                }
            ),
            0
        );
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 1
                }
            ),
            3
        );
    }

    #[test]
    fn utf16_emoji_surrogate_pair() {
        // "😀" = U+1F600, 4 bytes UTF-8, 2 UTF-16 code units (surrogate pair).
        let src = "😀a";
        // '😀' at byte 0, 'a' at byte 4.
        assert_eq!(
            offset_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position(src, 4),
            Position {
                line: 0,
                character: 2
            }
        );
        // Roundtrip.
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 0
                }
            ),
            0
        );
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 2
                }
            ),
            4
        );
    }

    #[test]
    fn utf16_surrogate_mid_pair_rounds_down() {
        // UTF-16 col 1 falls in the middle of the surrogate pair.
        // Should return the start of the emoji (byte 0), not byte 4.
        let src = "😀a";
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 1
                }
            ),
            0
        );
    }

    #[test]
    fn utf16_mixed_ascii_cjk_emoji() {
        // "a中😀b" = 1 + 3 + 4 + 1 = 9 bytes
        // UTF-16: 1 + 1 + 2 + 1 = 5 code units
        let src = "a中😀b";
        assert_eq!(
            offset_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        ); // 'a'
        assert_eq!(
            offset_to_position(src, 1),
            Position {
                line: 0,
                character: 1
            }
        ); // '中'
        assert_eq!(
            offset_to_position(src, 4),
            Position {
                line: 0,
                character: 2
            }
        ); // '😀'
        assert_eq!(
            offset_to_position(src, 8),
            Position {
                line: 0,
                character: 4
            }
        ); // 'b'
           // Roundtrip for each.
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 0
                }
            ),
            0
        );
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 1
                }
            ),
            1
        );
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 2
                }
            ),
            4
        );
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 4
                }
            ),
            8
        );
    }

    #[test]
    fn utf16_multiline_cjk() {
        let src = "中\n文";
        // line 0: '中' at byte 0 (UTF-16 col 0), newline at byte 3
        // line 1: '文' at byte 4 (UTF-16 col 0)
        assert_eq!(
            offset_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position(src, 4),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 0,
                    character: 0
                }
            ),
            0
        );
        assert_eq!(
            position_to_offset(
                src,
                Position {
                    line: 1,
                    character: 0
                }
            ),
            4
        );
    }

    #[test]
    fn utf16_position_beyond_line_end_clamps() {
        let src = "ab";
        // Requesting col 100 should clamp to end of source.
        let offset = position_to_offset(
            src,
            Position {
                line: 0,
                character: 100,
            },
        );
        assert_eq!(offset, 2);
    }

    #[test]
    fn utf16_line_beyond_source_clamps() {
        let src = "ab";
        let offset = position_to_offset(
            src,
            Position {
                line: 99,
                character: 0,
            },
        );
        assert_eq!(offset, 2);
    }

    #[test]
    fn line_index_direct() {
        let src = "aaa\nbbbbc\nc";
        let index = LineIndex::new(src);
        assert_eq!(index.line_count(), 3);
        assert_eq!(
            index.offset_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            index.offset_to_position(src, 4),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            index.offset_to_position(src, 10),
            Position {
                line: 2,
                character: 0
            }
        );
    }

    #[test]
    fn utf16_emoji_multiline_roundtrip() {
        let src = "😀\n文";
        let pos0 = Position {
            line: 0,
            character: 0,
        };
        let pos1 = Position {
            line: 1,
            character: 0,
        };
        assert_eq!(position_to_offset(src, pos0), 0);
        assert_eq!(position_to_offset(src, pos1), 5); // 4 bytes emoji + 1 newline
        assert_eq!(offset_to_position(src, 0), pos0);
        assert_eq!(offset_to_position(src, 5), pos1);
    }

    // ── CRLF tests ──────────────────────────────────────────────────────

    #[test]
    fn crlf_line_count() {
        let src = "line1\r\nline2\r\nline3";
        let index = LineIndex::new(src);
        assert_eq!(index.line_count(), 3);
    }

    #[test]
    fn crlf_offset_to_position() {
        let src = "abc\r\ndef";
        // 'a' at byte 0 → line 0, col 0
        assert_eq!(
            offset_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        // '\r' at byte 3 → line 0, col 3
        assert_eq!(
            offset_to_position(src, 3),
            Position {
                line: 0,
                character: 3
            }
        );
        // 'd' at byte 5 (after \r\n) → line 1, col 0
        assert_eq!(
            offset_to_position(src, 5),
            Position {
                line: 1,
                character: 0
            }
        );
    }

    #[test]
    fn crlf_position_to_offset() {
        let src = "abc\r\ndef";
        let pos = Position {
            line: 1,
            character: 0,
        };
        assert_eq!(position_to_offset(src, pos), 5);
    }

    #[test]
    fn crlf_roundtrip() {
        let src = "abc\r\ndef\r\nghi";
        let pos = Position {
            line: 2,
            character: 2,
        };
        let offset = position_to_offset(src, pos);
        let back = offset_to_position(src, offset);
        assert_eq!(back, pos);
    }

    // ── Severity mapping ─────────────────────────────────────────────────

    #[test]
    fn severity_mapping() {
        assert_eq!(severity_to_lsp(Severity::Error), DiagnosticSeverity::ERROR);
        assert_eq!(
            severity_to_lsp(Severity::Warning),
            DiagnosticSeverity::WARNING
        );
        assert_eq!(severity_to_lsp(Severity::Help), DiagnosticSeverity::HINT);
    }
}
