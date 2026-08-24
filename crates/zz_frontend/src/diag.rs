//! Diagnostics infrastructure.
//!
//! The frontend produces `RawDiag`s (message + span + severity + fixits,
//! decoupled from any file store). Rendering produces Rust-quality colored
//! output with source context, carets, and inline suggestions.

use std::io::Write;

use codespan_reporting::diagnostic::{Diagnostic as CsDiagnostic, Label, Severity as CsSeverity};
use codespan_reporting::files::SimpleFiles;
use colored::Colorize;

use crate::span::Span;

/// `SimpleFiles` uses `usize` as its file id.
pub type FileId = usize;

/// Diagnostic severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Help,
}

/// Safety classification for auto-fix suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FixSafety {
    /// Single unambiguous candidate — safe to auto-apply.
    Safe,
    /// Multiple candidates — requires user choice or --hard.
    Ambiguous,
}

/// A structured machine-readable suggestion for auto-fix / LSP integration.
#[derive(Debug, Clone)]
pub struct FixIt {
    /// The exact byte span to replace in the source.
    pub span: Span,
    /// The replacement string (empty string = deletion).
    pub replacement: String,
    /// Human-readable explanation of the fix.
    pub message: String,
    /// Safety classification for tiered auto-fix.
    pub safety: FixSafety,
    /// All candidate replacements for ambiguous fixes (empty for safe fixes).
    pub alternatives: Vec<String>,
}

impl FixIt {
    /// Create a safe fix (single unambiguous candidate).
    pub fn safe(span: Span, replacement: impl Into<String>, message: impl Into<String>) -> Self {
        FixIt {
            span,
            replacement: replacement.into(),
            message: message.into(),
            safety: FixSafety::Safe,
            alternatives: Vec::new(),
        }
    }

    /// Create an ambiguous fix with multiple candidates.
    /// `replacement` is the best/first candidate; `alternatives` is all candidates
    /// (including the best one). The first alternative is pre-selected.
    pub fn ambiguous(
        span: Span,
        replacement: impl Into<String>,
        message: impl Into<String>,
        alternatives: Vec<String>,
    ) -> Self {
        FixIt {
            span,
            replacement: replacement.into(),
            message: message.into(),
            safety: FixSafety::Ambiguous,
            alternatives,
        }
    }
}

/// A frontend diagnostic, decoupled from any file store.
#[derive(Debug, Clone)]
pub struct RawDiag {
    pub severity: Severity,
    pub message: String,
    pub span: Option<Span>,
    pub notes: Vec<String>,
    pub fixits: Vec<FixIt>,
}

/// A rendered diagnostic bound to a file.
pub type Diag = CsDiagnostic<FileId>;

/// Source files known to the diagnostic renderer.
pub type Files = SimpleFiles<String, String>;

// --- constructors ----------------------------------------------------------

pub fn error(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Error,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
    }
}

pub fn error_at(message: impl Into<String>, span: Span) -> RawDiag {
    RawDiag {
        severity: Severity::Error,
        message: message.into(),
        span: Some(span),
        notes: Vec::new(),
        fixits: Vec::new(),
    }
}

pub fn warning(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Warning,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
    }
}

pub fn warning_at(message: impl Into<String>, span: Span) -> RawDiag {
    RawDiag {
        severity: Severity::Warning,
        message: message.into(),
        span: Some(span),
        notes: Vec::new(),
        fixits: Vec::new(),
    }
}

pub fn note(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Help,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
    }
}

impl RawDiag {
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_fixit(mut self, fixit: FixIt) -> Self {
        self.fixits.push(fixit);
        self
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// Bind this diagnostic to a file, producing a renderable `Diag`.
    pub fn bind(&self, file_id: FileId) -> Diag {
        let cs_severity = match self.severity {
            Severity::Error => CsSeverity::Error,
            Severity::Warning => CsSeverity::Warning,
            Severity::Help => CsSeverity::Help,
        };

        let mut d = CsDiagnostic::new(cs_severity).with_message(self.message.clone());

        let mut labels = Vec::new();
        if let Some(span) = self.span {
            labels.push(Label::primary(file_id, span.to_range()));
        }

        for fixit in &self.fixits {
            labels.push(
                Label::secondary(file_id, fixit.span.to_range())
                    .with_message(format!("help: {} → `{}`", fixit.message, fixit.replacement)),
            );
        }

        if !labels.is_empty() {
            d = d.with_labels(labels);
        }

        let mut notes = self.notes.clone();
        for fixit in &self.fixits {
            notes.push(format!(
                "{}: replace with `{}`",
                fixit.message, fixit.replacement
            ));
        }

        if !notes.is_empty() {
            d = d.with_notes(notes);
        }

        d
    }
}

// --- Rust-quality colorized renderer ----------------------------------------

/// Compute line number (1-based) and column (0-based) for a byte offset.
fn line_col_for(source: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 0;
    for (i, ch) in source.char_indices() {
        if i >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Find the start byte of the line containing `offset`.
fn line_start_for(source: &str, offset: usize) -> usize {
    match source[..offset].rfind('\n') {
        Some(p) => p + 1,
        None => 0,
    }
}

/// Find the end byte (exclusive) of the line containing `offset`.
fn line_end_for(source: &str, offset: usize) -> usize {
    source[offset..]
        .find('\n')
        .map(|p| offset + p)
        .unwrap_or(source.len())
}

/// Render a single diagnostic with source context and ANSI colors.
///
/// Output format (matches rustc):
/// ```text
/// error: undefined variable `prntlnn`
///   --> file.zz:5:1
///    |
///  5 | prntlnn("hello")
///    | ^^^^^^^ help: replace with `println`
///    |
///    = did you mean `println`?
/// ```
fn render_one_colored(files: &Files, file_id: FileId, raw: &RawDiag) -> String {
    use std::fmt::Write;

    let mut out = String::new();

    // --- Severity tag ---
    let tag = match raw.severity {
        Severity::Error => "error".red().bold(),
        Severity::Warning => "warning".yellow().bold(),
        Severity::Help => "help".cyan().bold(),
    };
    let _ = write!(out, "{}: {}", tag, raw.message.bold());

    // --- Location + source context ---
    if let Some(span) = raw.span {
        if let Ok(file) = files.get(file_id) {
            let name = file.name();
            let source: &str = file.source().as_ref();
            let start = span.start as usize;
            let end = span.end as usize;
            let (line_num, col) = line_col_for(source, start);
            let _ = write!(out, "\n  --> {}:{line_num}:{}", name, col + 1);

            // Source line
            let lstart = line_start_for(source, start);
            let lend = line_end_for(source, start);
            let line_text = &source[lstart..lend];
            let pad = format!("{line_num:>4}");
            let _ = write!(out, "\n    |");
            let _ = write!(out, "\n {pad} | {line_text}");

            // Carets under the span + inline fixit hint
            let col_offset = start - lstart;
            let len = (end - start).max(1);
            let spaces = " ".repeat(col_offset);
            let carets = "^".repeat(len);
            let colored_carets = match raw.severity {
                Severity::Error => carets.red().bold(),
                Severity::Warning => carets.yellow().bold(),
                Severity::Help => carets.cyan().bold(),
            };
            // Inline the first fixit hint directly under the carets
            if let Some(fixit) = raw.fixits.first() {
                let hint = format!(" help: replace with `{}`", fixit.replacement);
                let _ = write!(out, "\n    | {spaces}{colored_carets}{}", hint.cyan());
            } else {
                let _ = write!(out, "\n    | {spaces}{colored_carets}");
            }
        }
    }

    // --- Notes ---
    for note_text in &raw.notes {
        let _ = write!(out, "\n    {} {}", "=".bold(), note_text.cyan());
    }

    // --- Additional FixIt suggestions (beyond the first, which is inlined) ---
    for fixit in raw.fixits.iter().skip(1) {
        let _ = write!(
            out,
            "\n    {} {}: replace with `{}`",
            "=".bold(),
            "help".cyan().bold(),
            fixit.replacement.green()
        );
    }

    out
}

/// Render diagnostics to an in-memory string (tests, REPL capture).
/// Returns ANSI-colored output.
pub fn render_to_string(files: &Files, file_id: FileId, diags: &[RawDiag]) -> String {
    let mut out = String::new();
    for raw in diags {
        out.push_str(&render_one_colored(files, file_id, raw));
        out.push('\n');
    }
    out
}

/// Render diagnostics to stderr with ANSI colors (when stderr is a tty).
pub fn render_to_stderr(files: &Files, file_id: FileId, diags: &[RawDiag]) {
    let use_color = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let mut writer = std::io::stderr();

    for raw in diags {
        if use_color {
            let rendered = render_one_colored(files, file_id, raw);
            let _ = writer.write_all(rendered.as_bytes());
        } else {
            // Fallback: plain text rendering without ANSI codes.
            let mut line = String::new();
            let prefix = match raw.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Help => "help",
            };
            use std::fmt::Write;
            let _ = write!(line, "{prefix}: {}", raw.message);
            if let Some(span) = raw.span {
                if let Ok(file) = files.get(file_id) {
                    let name = file.name();
                    let source: &str = file.source().as_ref();
                    let (ln, col) = line_col_for(source, span.start as usize);
                    let _ = write!(line, "\n  --> {name}:{ln}:{}", col + 1);
                    let lstart = line_start_for(source, span.start as usize);
                    let lend = line_end_for(source, span.start as usize);
                    let line_text = &source[lstart..lend];
                    let _ = write!(line, "\n    |");
                    let _ = write!(line, "\n {:>4} | {line_text}", ln);
                    let col_off = span.start as usize - lstart;
                    let len = ((span.end - span.start).max(1)) as usize;
                    let _ = write!(line, "\n    | {}{}", " ".repeat(col_off), "^".repeat(len));
                }
            }
            for note_text in &raw.notes {
                let _ = write!(line, "\n    = {note_text}");
            }
            let _ = writer.write_all(line.as_bytes());
        }
        let _ = writer.write_all(b"\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_caret_diagnostic() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x := 1 +\n".to_string());
        let diag = error_at("expected expression", Span::new(8, 9));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("error"), "missing error keyword: {out}");
        assert!(
            out.contains("expected expression"),
            "missing message: {out}"
        );
        assert!(out.contains("^"), "missing caret: {out}");
    }

    #[test]
    fn renders_warning_with_fixit() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x := 1\ny := 2\n".to_string());
        let diag = warning_at("unused variable `y`", Span::new(7, 8))
            .with_note("consider prefixing with `_`")
            .with_fixit(FixIt::safe(Span::new(7, 8), "_y", "rename to"));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("warning"), "missing warning keyword: {out}");
        assert!(out.contains("unused variable"), "missing message: {out}");
        // Fixit should be inlined under the caret
        assert!(
            out.contains("help: replace with `_y`"),
            "missing inline fixit: {out}"
        );
    }

    #[test]
    fn warning_severity_includes_keyword() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x\n".to_string());
        let diag = warning_at("unused variable `x`", Span::new(0, 1));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("warning"), "missing warning keyword: {out}");
    }

    #[test]
    fn help_severity_includes_keyword() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x\n".to_string());
        let diag = note("did you mean `y`?");
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("help"), "missing help keyword: {out}");
    }

    #[test]
    fn shows_source_context_with_line_numbers() {
        let mut files = Files::new();
        let id = files.add(
            "test.zz".to_string(),
            "line 1\nline 2\nx := bad\nline 4\n".to_string(),
        );
        let diag = error_at("undefined variable", Span::new(14, 17));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("3"), "missing line number: {out}");
        assert!(out.contains("bad"), "missing source text: {out}");
    }

    #[test]
    fn fixit_safe_constructor() {
        let fixit = FixIt::safe(Span::new(0, 3), "_x".to_string(), "prefix with _");
        assert_eq!(fixit.replacement, "_x");
        assert!(matches!(fixit.safety, FixSafety::Safe));
    }

    #[test]
    fn fixit_ambiguous_constructor() {
        let fixit = FixIt::ambiguous(
            Span::new(5, 10),
            "println",
            "replace function",
            vec!["println".to_string(), "println!".to_string()],
        );
        assert_eq!(fixit.replacement, "println");
        assert_eq!(fixit.alternatives.len(), 2);
        assert!(matches!(fixit.safety, FixSafety::Ambiguous));
    }
}
