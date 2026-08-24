//! Diagnostics infrastructure.
//!
//! The frontend produces `RawDiag`s (message + span + severity + fixits,
//! decoupled from any file store). Rendering attaches a `FileId` and
//! delegates to `codespan-reporting` for rustc-quality output.

use std::io::Write;

use codespan_reporting::diagnostic::{Diagnostic as CsDiagnostic, Label, Severity as CsSeverity};
use codespan_reporting::files::SimpleFiles;
use codespan_reporting::term::termcolor::{Buffer, StandardStream};
use codespan_reporting::term::{self, Config};

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

/// A structured machine-readable suggestion for auto-fix / LSP integration.
#[derive(Debug, Clone)]
pub struct FixIt {
    /// The exact byte span to replace in the source.
    pub span: Span,
    /// The replacement string (empty string = deletion).
    pub replacement: String,
    /// Human-readable explanation of the fix.
    pub message: String,
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
            let style = match self.severity {
                Severity::Error => LabelStyle::Primary,
                Severity::Warning => LabelStyle::Primary,
                Severity::Help => LabelStyle::Primary,
            };
            labels.push(match style {
                LabelStyle::Primary => Label::primary(file_id, span.to_range()),
            });
        }

        // Attach fix-it suggestions as secondary labels.
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

        // Append fix-it messages as notes for non-LSP consumers.
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

/// Render diagnostics to an in-memory buffer (tests, REPL capture).
pub fn render_to_string(files: &Files, file_id: FileId, diags: &[RawDiag]) -> String {
    let mut buf = Buffer::no_color();
    for raw in diags {
        let d = raw.bind(file_id);
        if term::emit(&mut buf, &Config::default(), files, &d).is_err() {
            buf.write_all(d.message.as_bytes()).ok();
            buf.write_all(b"\n").ok();
        }
    }
    String::from_utf8_lossy(buf.as_slice()).into_owned()
}

/// Render diagnostics to stderr, colored when stderr is a tty.
pub fn render_to_stderr(files: &Files, file_id: FileId, diags: &[RawDiag]) {
    let writer = StandardStream::stderr(if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        codespan_reporting::term::termcolor::ColorChoice::Auto
    } else {
        codespan_reporting::term::termcolor::ColorChoice::Never
    });
    let mut writer = writer.lock();
    for raw in diags {
        let d = raw.bind(file_id);
        let _ = term::emit(&mut writer, &Config::default(), files, &d);
    }
    let _ = writer.write_all(b"\n");
}

// --- helpers used by bind() ------------------------------------------------

/// Label style (currently only Primary exists in codespan-reporting 0.11).
enum LabelStyle {
    Primary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_caret_diagnostic() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "let x = 1 +\n".to_string());
        let diag = error_at("expected expression", Span::new(10, 11));
        let out = render_to_string(&files, id, &[diag]);
        assert!(
            out.contains("expected expression"),
            "missing message: {out}"
        );
        assert!(out.contains("^"), "missing caret: {out}");
    }

    #[test]
    fn renders_warning_with_fixit() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "let _x = 1\nlet y = 2\n".to_string());
        let diag = warning_at("unused variable `y`", Span::new(17, 18))
            .with_note("consider prefixing with `_`")
            .with_fixit(FixIt {
                span: Span::new(17, 18),
                replacement: "_y".to_string(),
                message: "rename to".to_string(),
            });
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("unused variable"), "missing warning: {out}");
        assert!(out.contains("_y"), "missing fixit: {out}");
    }

    #[test]
    fn error_severity_includes_keyword() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x\n".to_string());
        let diag = error_at("undefined variable `x`", Span::new(0, 1));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("error"), "missing error keyword: {out}");
    }

    #[test]
    fn warning_severity_includes_keyword() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x\n".to_string());
        let diag = warning_at("unused variable `x`", Span::new(0, 1));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("warning"), "missing warning keyword: {out}");
    }
}
