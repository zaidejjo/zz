//! Diagnostic data shared by the frontend and the checker.
//!
//! This crate holds only the transport types — no rendering, no file store,
//! no `codespan-reporting` dependency — so every pipeline crate can emit
//! diagnostics without depending on the frontend renderer.

use zz_span_ast::Span;

/// Diagnostic severity level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Severity {
    Error,
    Warning,
    Help,
}

/// Safety classification for auto-fix suggestions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum FixSafety {
    /// Single unambiguous candidate — safe to auto-apply.
    Safe,
    /// Multiple candidates — requires user choice or --hard.
    Ambiguous,
}

/// A structured machine-readable suggestion for auto-fix / LSP integration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// A secondary source label: a second span with its own message, rendered
/// above the primary span (e.g. "variable defined as immutable here").
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecondaryLabel {
    pub span: Span,
    pub message: String,
}

/// A frontend diagnostic, decoupled from any file store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RawDiag {
    pub severity: Severity,
    pub message: String,
    pub span: Option<Span>,
    pub notes: Vec<String>,
    pub fixits: Vec<FixIt>,
    /// Optional secondary source label (rendered above the primary span).
    pub secondary: Option<SecondaryLabel>,
}

// --- constructors ----------------------------------------------------------

pub fn error(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Error,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: None,
    }
}

pub fn error_at(message: impl Into<String>, span: Span) -> RawDiag {
    RawDiag {
        severity: Severity::Error,
        message: message.into(),
        span: Some(span),
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: None,
    }
}

pub fn warning(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Warning,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: None,
    }
}

pub fn warning_at(message: impl Into<String>, span: Span) -> RawDiag {
    RawDiag {
        severity: Severity::Warning,
        message: message.into(),
        span: Some(span),
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: None,
    }
}

pub fn note(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Help,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: None,
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

    /// Attach a secondary source label (rendered above the primary span).
    pub fn with_secondary(mut self, label: SecondaryLabel) -> Self {
        self.secondary = Some(label);
        self
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}
