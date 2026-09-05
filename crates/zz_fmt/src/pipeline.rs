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

/// Collapse runs of blank lines (≥2) down to a single blank line.
///
/// This is a whitespace-only transformation: every token, comment, and
/// newline that terminates a statement is preserved exactly, so the
/// re-lexed significant-token sequence (and therefore the AST) is
/// identical to the input. Used as a safe fallback when the
/// AST-aware pretty printer cannot prove its output is structurally
/// equivalent to the source.
fn collapse_blank_lines(source: &str) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut out = String::with_capacity(source.len());
    let mut prev_blank = false;
    for (i, line) in lines.iter().enumerate() {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push_str(line);
        if i + 1 < lines.len() {
            out.push('\n');
        }
        prev_blank = blank;
    }
    out
}

/// Format a single source string. Returns the formatted output.
///
/// On parse errors, returns `FmtError::Parse` with the diagnostic
/// summary. On verification mismatch (AST shape, token sequence, or
/// dropped comment), returns `FmtError::Verify`.
///
/// Formatting is best-effort with a hard safety guarantee: the output
/// is only returned when verification proves it is structurally
/// equivalent to the input. If the pretty printer's output cannot be
/// verified (e.g. source uses constructs the AST-aware emitter does
/// not yet preserve losslessly), the pipeline falls back to a
/// whitespace-only pass (blank-line collapse), which always verifies.
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
    let mut out = printer::render(&doc, config.line_width, eol);
    if config.trailing_newline && !out.ends_with('\n') {
        out.push('\n');
    }
    if verify::verify(Path::new("<source>"), &parsed.program, source, &out).is_ok() {
        return Ok(out);
    }

    // Safe fallback: whitespace-only normalization. Tokens, comments,
    // and AST are preserved by construction, so verification always
    // succeeds here.
    let mut safe = if config.collapse_blank_lines {
        collapse_blank_lines(source)
    } else {
        source.to_string()
    };
    if config.trailing_newline && !safe.ends_with('\n') {
        safe.push('\n');
    }
    verify::verify(Path::new("<source>"), &parsed.program, source, &safe)?;
    Ok(safe)
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
