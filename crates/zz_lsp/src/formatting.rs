//! Document formatting — thin wrapper around `zz_frontend::fmt` that
//! adds an LSP-specific `format_as_edit` helper.

pub use zz_frontend::fmt::{format_program, is_formatted, FormatConfig};

/// Format a program and return a full-document TextEdit (old → new).
pub fn format_as_edit(
    program: &zz_frontend::ast::Program,
    source: &str,
    config: &FormatConfig,
) -> Option<tower_lsp::lsp_types::TextEdit> {
    let formatted = format_program(program, source, config);
    if formatted == source {
        return None; // No changes needed.
    }
    let last_line = source.lines().count().saturating_sub(1);
    let last_col = source.lines().last().map(|l| l.len()).unwrap_or(0);
    Some(tower_lsp::lsp_types::TextEdit {
        range: tower_lsp::lsp_types::Range {
            start: tower_lsp::lsp_types::Position::new(0, 0),
            end: tower_lsp::lsp_types::Position::new(last_line as u32, last_col as u32),
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
        let parsed = zz_frontend::parse(src);
        let edit = format_as_edit(&parsed.program, src, &FormatConfig::default());
        assert!(
            edit.is_none(),
            "already-formatted source should return None"
        );
    }

    #[test]
    fn format_change_returns_edit() {
        let src = "x:=1";
        let parsed = zz_frontend::parse(src);
        let edit = format_as_edit(&parsed.program, src, &FormatConfig::default());
        assert!(edit.is_some(), "unformatted source should return Some");
        let edit = edit.unwrap();
        assert!(edit.new_text.contains("x := 1"));
    }
}
