//! Document formatting — routes formatting requests to `zz_fmt::format_source`
//! for the full verification pipeline (lex → parse → format → verify).
//!
//! Provides an LSP-specific `format_as_edit` helper that returns a
//! full-document `TextEdit`.

use tower_lsp::lsp_types;

/// Format source via `zz_fmt::format_source` and return a full-document
/// `TextEdit` (old → new). Returns `None` when the source is already
/// formatted.
pub fn format_as_edit(source: &str) -> Option<lsp_types::TextEdit> {
    let config = zz_fmt::FmtConfig::default();
    let formatted = match zz_fmt::format_source(source, &config) {
        Ok(s) => s,
        Err(_) => return None,
    };
    if formatted == source {
        return None;
    }
    let last_line = source.lines().count().saturating_sub(1);
    let last_col = source.lines().last().map(|l| l.len()).unwrap_or(0);
    Some(lsp_types::TextEdit {
        range: lsp_types::Range {
            start: lsp_types::Position::new(0, 0),
            end: lsp_types::Position::new(last_line as u32, last_col as u32),
        },
        new_text: formatted,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_no_change_returns_none() {
        let src = "x := 1\n";
        let edit = format_as_edit(src);
        assert!(
            edit.is_none(),
            "already-formatted source should return None"
        );
    }

    #[test]
    fn format_change_returns_edit() {
        let src = "x:=1";
        let edit = format_as_edit(src);
        assert!(edit.is_some(), "unformatted source should return Some");
        let edit = edit.unwrap();
        assert!(edit.new_text.contains("x := 1"));
    }

    #[test]
    fn format_edit_covers_full_document() {
        let src = "x:=1\ny:=2\n";
        let edit = format_as_edit(src).unwrap();
        assert_eq!(edit.range.start, lsp_types::Position::new(0, 0));
        // Full replacement.
        assert!(edit.new_text.contains("x := 1"));
        assert!(edit.new_text.contains("y := 2"));
    }
}
