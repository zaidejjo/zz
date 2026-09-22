//! Top-level formatting pipeline.
//!
//! `format_source` is the single entry point: lex → parse → classify
//! trivia → lower to Doc → render → verify. Returns the formatted
//! string on success; `FmtError` on failure.
//!
//! `format_file` wraps `format_source` with file IO and a final write
//! step (caller chooses whether to write or only check).

use crate::config::FmtConfig;
use crate::error::FmtError;
use crate::ir;
use crate::printer;
use crate::verify;
use std::path::{Path, PathBuf};
use zz_frontend::parse;

/// Result of formatting one file.
#[derive(Debug, Clone)]
pub struct FormattedFile {
    pub path: PathBuf,
    pub original: String,
    pub formatted: String,
    pub changed: bool,
}

/// Per-file trivia summary (debug/observability).
#[derive(Debug, Clone, Default)]
pub struct TriviaReport {
    pub line_comments: usize,
    pub doc_comments: usize,
    pub block_comments: usize,
    pub blank_lines: usize,
}

/// Format a single source string. Returns the formatted output.
///
/// On parse errors, returns `FmtError::Parse` with the diagnostic
/// summary. On verification mismatch (AST shape, token sequence, or
/// dropped comment), returns `FmtError::Verify`.
///
/// Formatting is strict: the AST-aware pretty printer's output is only
/// returned when verification proves it structurally equivalent to the
/// input. There is no raw-whitespace fallback — if the emitter cannot
/// prove equivalence, the error is surfaced so formatter bugs cannot be
/// silently masked.
pub fn format_source(source: &str, config: &FmtConfig) -> Result<String, FmtError> {
    let parsed = parse(source);
    if !parsed.errors.is_empty() {
        let summary = parsed
            .errors
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(FmtError::Parse {
            path: PathBuf::from("<source>"),
            summary,
        });
    }
    let (doc, eol) = ir::lower_program(&parsed.program, source);
    let mut out = printer::render(&doc, config.line_width, config.indent_width, eol);
    if config.trailing_newline && !out.ends_with('\n') {
        out.push('\n');
    }
    verify::verify(Path::new("<source>"), &parsed.program, source, &out)?;
    Ok(out)
}

/// Format a file in place. Reads source from disk, formats, verifies,
/// and writes back. Returns the `FormattedFile` describing the change.
pub fn format_file(path: &Path, config: &FmtConfig) -> Result<FormattedFile, FmtError> {
    let original = std::fs::read_to_string(path).map_err(|e| FmtError::io(path, e))?;
    let formatted = format_source(&original, config)?;
    let changed = formatted != original;
    if changed {
        std::fs::write(path, &formatted).map_err(|e| FmtError::io(path, e))?;
    }
    Ok(FormattedFile {
        path: path.to_path_buf(),
        original,
        formatted,
        changed,
    })
}
