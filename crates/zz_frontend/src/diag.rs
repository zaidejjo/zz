//! Diagnostics infrastructure.
//!
//! The frontend produces `RawDiag`s (message + span, no file knowledge).
//! Rendering attaches a `FileId` and delegates to `codespan-reporting` for
//! rustc-quality output (source lines, carets, colors).

use std::io::Write;

use codespan_reporting::diagnostic::{Diagnostic as CsDiagnostic, Label};
use codespan_reporting::files::SimpleFiles;
use codespan_reporting::term::termcolor::{Buffer, StandardStream};
use codespan_reporting::term::{self, Config};

use crate::span::Span;

/// `SimpleFiles` uses `usize` as its file id.
pub type FileId = usize;

/// A frontend diagnostic, decoupled from any file store.
#[derive(Debug, Clone)]
pub struct RawDiag {
    pub message: String,
    pub span: Option<Span>,
    pub notes: Vec<String>,
}

/// A rendered diagnostic bound to a file.
pub type Diag = CsDiagnostic<FileId>;

/// Source files known to the diagnostic renderer.
pub type Files = SimpleFiles<String, String>;

pub fn error(message: impl Into<String>) -> RawDiag {
    RawDiag {
        message: message.into(),
        span: None,
        notes: Vec::new(),
    }
}

pub fn error_at(message: impl Into<String>, span: Span) -> RawDiag {
    RawDiag {
        message: message.into(),
        span: Some(span),
        notes: Vec::new(),
    }
}

pub fn note(message: impl Into<String>) -> RawDiag {
    RawDiag {
        message: message.into(),
        span: None,
        notes: Vec::new(),
    }
}

impl RawDiag {
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Bind this diagnostic to a file, producing a renderable `Diag`.
    pub fn bind(&self, file_id: FileId) -> Diag {
        let mut d = CsDiagnostic::error().with_message(self.message.clone());
        if let Some(span) = self.span {
            d = d.with_labels(vec![Label::primary(file_id, span.to_range())]);
        }
        if !self.notes.is_empty() {
            d = d.with_notes(self.notes.clone());
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
}
