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
use crate::printer::{self, Eol};
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
    // Only hard parse errors block formatting; warnings (e.g. redundant
    // semicolon) are reported but must not silently refuse to format.
    if parsed
        .errors
        .iter()
        .any(|d| d.severity == zz_frontend::diag::Severity::Error)
    {
        let summary = parsed
            .errors
            .iter()
            .filter(|d| d.severity == zz_frontend::diag::Severity::Error)
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(FmtError::Parse {
            path: PathBuf::from("<source>"),
            summary,
        });
    }
    let (doc, eol) = ir::lower_program(&parsed.program, source, config);
    let mut out = printer::render(
        &doc,
        config.line_width,
        config.indent_width,
        eol,
        config.use_tabs,
    );
    if config.trailing_newline {
        match eol {
            Eol::Lf => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Eol::Crlf => {
                if !out.ends_with("\r\n") {
                    out.push_str("\r\n");
                }
            }
        }
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

/// Format only the statements whose source spans intersect the given
/// line range (`start_line`..=`end_line`, 1-indexed). Everything outside
/// the range is left byte-identical.
///
/// Each selected statement is formatted independently via
/// [`format_source`], then spliced back into the original source in
/// reverse span order (to preserve byte offsets). The result is
/// guaranteed to be valid ZZ source — each formatted statement passes
/// `format_source`'s strict verification.
///
/// Lines are clamped to the file's extent; an empty or inverted range
/// returns the source unchanged.
pub fn format_range(
    source: &str,
    start_line: usize,
    end_line: usize,
    config: &FmtConfig,
) -> Result<String, FmtError> {
    let total_lines = source.split_inclusive('\n').count();
    if total_lines == 0 || start_line > end_line {
        return Ok(source.to_string());
    }
    let start_line = start_line.min(total_lines);
    let end_line = end_line.min(total_lines).max(start_line);

    // Byte offset where `line` (1-indexed) begins.
    fn line_start_byte(source: &str, line: usize) -> usize {
        let mut bytes = 0;
        for (i, l) in source.split_inclusive('\n').enumerate() {
            if i + 1 == line {
                return bytes;
            }
            bytes += l.len();
        }
        bytes
    }

    let start_byte = line_start_byte(source, start_line);
    // End offset is the start of the line after `end_line` (exclusive),
    // which lands right after the newline terminating `end_line`.
    let end_byte = line_start_byte(source, end_line + 1).min(source.len());

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

    // Collect spans of top-level statements intersecting the range.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for stmt in &parsed.program.stmts {
        let span = stmt.span();
        let s = span.start as usize;
        let e = span.end as usize;
        if s < end_byte && e > start_byte {
            spans.push((s, e));
        }
    }

    if spans.is_empty() {
        return Ok(source.to_string());
    }

    // Format each selected statement and splice back (reverse order so
    // earlier replacements don't shift later spans). Strip the trailing
    // newline that `format_source` adds so the original inter-statement
    // newlines are preserved.
    let mut result = source.to_string();
    for (s, e) in spans.into_iter().rev() {
        let stmt_text = &source[s..e];
        let mut formatted = format_source(stmt_text, config)?;
        if formatted.ends_with('\n') {
            formatted.pop();
        }
        result.replace_range(s..e, &formatted);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_range_empty_source() {
        let cfg = FmtConfig::default();
        let out = format_range("", 1, 1, &cfg).unwrap();
        assert_eq!(out, "");
    }

    #[test]
    fn format_range_inverted_returns_unchanged() {
        let cfg = FmtConfig::default();
        let src = "x := 1\ny := 2\n";
        let out = format_range(src, 5, 1, &cfg).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn format_range_out_of_bounds_clamps() {
        let cfg = FmtConfig::default();
        let src = "x := 1\n";
        let out = format_range(src, 1, 999, &cfg).unwrap();
        // Only line 1 exists; it's already formatted so output is unchanged.
        assert_eq!(out, src);
    }

    #[test]
    fn format_range_formats_selected_statement() {
        let cfg = FmtConfig::default();
        let src = "func add(a:int,b:int)->int{return a+b}\ny := 2\n";
        let out = format_range(src, 1, 1, &cfg).unwrap();
        // Line 1 should be formatted; line 2 untouched.
        assert!(
            out.contains("func add(a: int, b: int) -> int {"),
            "line 1 should be formatted: {out}"
        );
        assert!(
            out.contains("y := 2\n"),
            "line 2 should be untouched: {out}"
        );
    }

    #[test]
    fn format_range_range_spans_multiple_statements() {
        let cfg = FmtConfig::default();
        let src = "a:=1\nb:=2\nc:=3\n";
        let out = format_range(src, 1, 2, &cfg).unwrap();
        assert!(
            out.contains("a := 1\nb := 2"),
            "lines 1-2 should be formatted: {out}"
        );
        assert!(out.contains("c:=3\n"), "line 3 should be untouched: {out}");
    }
}
