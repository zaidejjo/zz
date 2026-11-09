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
    /// Stable error code (e.g. `Z0001`), or `None` for diagnostics that have
    /// not yet been assigned a code.
    pub code: Option<ErrorCode>,
    pub message: String,
    pub span: Option<Span>,
    pub notes: Vec<String>,
    pub fixits: Vec<FixIt>,
    /// Secondary source labels (rendered above the primary span).
    /// Multiple labels are supported for multi-span diagnostics
    /// (e.g. "expected `Box<str>` here" + "found `Box<int>` here").
    pub secondary: Vec<SecondaryLabel>,
}

// --- Stable error codes ----------------------------------------------------

/// A stable, machine-readable error code, e.g. `Z0001`.
///
/// Codes are assigned per diagnostic *kind*, not per occurrence, so the
/// same code always means the same thing. They appear in text and JSON
/// output and are looked up by `zz explain <code>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(pub u32);

impl ErrorCode {
    /// Create a new error code from a u32, rendered as `Z{:04}`.
    pub const fn new(n: u32) -> Self {
        Self(n)
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Z{:04}", self.0)
    }
}

impl serde::Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("Z{:04}", self.0))
    }
}

impl<'de> serde::Deserialize<'de> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.strip_prefix('Z').unwrap_or(&s);
        let n: u32 = s
            .parse()
            .map_err(|e| serde::de::Error::custom(format!("invalid error code: {e}")))?;
        Ok(ErrorCode(n))
    }
}

/// One entry in the error-code catalog: the code, a human-readable title,
/// a description, and a before/after example pair.
#[derive(Debug, Clone)]
pub struct ErrorCatalogEntry {
    pub code: ErrorCode,
    pub title: &'static str,
    pub description: &'static str,
    /// Example of broken code that triggers this code.
    pub bad_example: &'static str,
    /// Example of corrected code.
    pub fixed_example: &'static str,
}

/// The catalog of all error codes, indexed by code number.
///
/// Every diagnostic that carries a code must have an entry here; the
/// `error_codes_have_explanations` test enforces this in both directions.
/// All error codes, sorted by number.
pub static ALL_ERROR_CODES: &[ErrorCatalogEntry] = &[
    ErrorCatalogEntry {
        code: ErrorCode(1),
        title: "undefined variable",
        description: "A name was used that has not been declared in scope.",
        bad_example: "println(foo)",
        fixed_example: "let foo = 42\nprintln(foo)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(2),
        title: "type mismatch",
        description: "An expression was used where a different type was expected.",
        bad_example: "let x: int = \"hello\"",
        fixed_example: "let x: int = 42",
    },
    ErrorCatalogEntry {
        code: ErrorCode(3),
        title: "generic bound violation",
        description: "A type argument does not satisfy a trait bound (`Num`, `Ord`, ...).",
        bad_example: "func f<T: Num>(x: T) { ... }\nf(\"nope\")",
        fixed_example: "f(42)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(4),
        title: "non-exhaustive match",
        description: "A `match` does not cover every possible value of its scrutinee.",
        bad_example: "match opt { .some(v) => v }",
        fixed_example: "match opt { .some(v) => v, .none => 0 }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(5),
        title: "unreachable match arm",
        description: "A match arm can never be reached because a previous arm covers it.",
        bad_example: "match x { _ => 1, .some(v) => v }",
        fixed_example: "match x { .some(v) => v, _ => 1 }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(6),
        title: "missing return",
        description: "A non-`unit` function can fall through its body without returning.",
        bad_example: "func f() -> int { if c { 1 } }",
        fixed_example: "func f() -> int { if c { 1 } else { 0 } }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(7),
        title: "use before initialization",
        description: "A local variable may be read on some path before being assigned.",
        bad_example: "let x: int\nif c { x = 1 }\nprintln(x)",
        fixed_example: "let x: int = 0\nif c { x = 1 }\nprintln(x)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(8),
        title: "index out of bounds",
        description: "An array/tuple index is outside the valid range.",
        bad_example: "let a = [1, 2]\nprintln(a[5])",
        fixed_example: "let a = [1, 2]\nprintln(a[0])",
    },
    ErrorCatalogEntry {
        code: ErrorCode(9),
        title: "division by zero",
        description: "A division or remainder by a provably-zero divisor.",
        bad_example: "let x = 1 / 0",
        fixed_example: "let x = 1 / 2",
    },
    ErrorCatalogEntry {
        code: ErrorCode(10),
        title: "unknown field",
        description: "A struct field access uses a name that is not a field.",
        bad_example: "struct Point { x: int }\nlet p = Point { x: 1 }\nprintln(p.y)",
        fixed_example:
            "struct Point { x: int, y: int }\nlet p = Point { x: 1, y: 2 }\nprintln(p.y)",
    },
];

/// Look up a catalog entry by code. Returns `None` for unknown codes.
pub fn error_code_lookup(code: ErrorCode) -> Option<&'static ErrorCatalogEntry> {
    ALL_ERROR_CODES.iter().find(|e| e.code == code)
}

/// Render `Z{:04}` for display.
pub fn error_code_display(code: ErrorCode) -> String {
    format!("Z{:04}", code.0)
}

// --- constructors ----------------------------------------------------------

pub fn error(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Error,
        code: None,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: Vec::new(),
    }
}

pub fn error_at(message: impl Into<String>, span: Span) -> RawDiag {
    RawDiag {
        severity: Severity::Error,
        code: None,
        message: message.into(),
        span: Some(span),
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: Vec::new(),
    }
}

pub fn warning(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Warning,
        code: None,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: Vec::new(),
    }
}

pub fn warning_at(message: impl Into<String>, span: Span) -> RawDiag {
    RawDiag {
        severity: Severity::Warning,
        code: None,
        message: message.into(),
        span: Some(span),
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: Vec::new(),
    }
}

pub fn note(message: impl Into<String>) -> RawDiag {
    RawDiag {
        severity: Severity::Help,
        code: None,
        message: message.into(),
        span: None,
        notes: Vec::new(),
        fixits: Vec::new(),
        secondary: Vec::new(),
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
        self.secondary.push(label);
        self
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.code = Some(code);
        self
    }
}
