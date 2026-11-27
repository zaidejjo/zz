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
        fixed_example: "foo := 42\nprintln(foo)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(2),
        title: "type mismatch",
        description: "An expression was used where a different type was expected.",
        bad_example: "x: str = \"hello\"",
        fixed_example: "x: int = 42",
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
        bad_example: "x: int\nif c { x = 1 }\nprintln(x)",
        fixed_example: "x: int = 0\nif c { x = 1 }\nprintln(x)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(8),
        title: "index out of bounds",
        description: "An array/tuple index is outside the valid range.",
        bad_example: "a: [int] = [1, 2]\nprintln(a[5])",
        fixed_example: "a: [int] = [1, 2]\nprintln(a[0])",
    },
    ErrorCatalogEntry {
        code: ErrorCode(9),
        title: "division by zero",
        description: "A division or remainder by a provably-zero divisor.",
        bad_example: "x: int = 1 / 0",
        fixed_example: "x: int = 1 / 2",
    },
    ErrorCatalogEntry {
        code: ErrorCode(10),
        title: "unknown field",
        description: "A struct field access uses a name that is not a field.",
        bad_example: "struct Point { x: int }\np: Point = Point { x: 1 }\nprintln(p.y)",
        fixed_example:
            "struct Point { x: int, y: int }\np: Point = Point { x: 1, y: 2 }\nprintln(p.y)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(11),
        title: "`state` used outside module level",
        description: "`state` declarations are only allowed at module level in v1.",
        bad_example: "func f() { state x := 1 }",
        fixed_example: "state x := 1\nfunc f() { x = 2 }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(12),
        title: "`state` holds closure/function type",
        description: "A `state` may not hold a closure or function value, because closures capture by reference through shared heap cells.",
        bad_example: "state f := |x| x + 1",
        fixed_example: "state data := 1\nlet f = |x| x + data",
    },
    ErrorCatalogEntry {
        code: ErrorCode(13),
        title: "read of `state` before init",
        description: "A `state` was read before its initializer ran.",
        bad_example: "state x := 1\nfoo(x)",
        fixed_example: "state x := 1\nfoo(x)  // after init, guaranteed",
    },
    ErrorCatalogEntry {
        code: ErrorCode(14),
        title: "non-whitelisted native or user call in auto-atomic statement",
        description: "A statement touching a `state` may only use literals, operators, and whitelisted pure natives.",
        bad_example: "state n := 0\nn = foo()",
        fixed_example: "atomic { n = foo() }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(15),
        title: "blocking call inside `atomic { }`",
        description: "Blocking calls (chan.recv, task.join, sleep, …) are forbidden inside `atomic { }`.",
        bad_example: "atomic { ch.recv() }",
        fixed_example: "let v = ch.recv()\natomic { n = v }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(16),
        title: "check-then-act on `state`",
        description: "A `state` was read in one statement and written in a different reachable statement outside `atomic { }`.",
        bad_example: "if n > 0 { n = n - 1 }",
        fixed_example: "atomic { if n > 0 { n = n - 1 } }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(17),
        title: "plain global reachable from a task",
        description: "A mutable global accessed from a `task.spawn` closure must be declared `state`.",
        bad_example: "n := 0\ntask.spawn(|_| { n = 1 })",
        fixed_example: "state n := 0\ntask.spawn(|_| { n = 1 })",
    },
    ErrorCatalogEntry {
        code: ErrorCode(18),
        title: "yield/block while atomic depth > 0",
        description: "A task yielded or blocked while holding the state lock.",
        bad_example: "atomic { time.sleep_ms(10) }",
        fixed_example: "let _ = time.sleep_ms(10)\natomic { n = 1 }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(19),
        title: "state reads in one statement separated by a call",
        description: "One statement reads two different `state` bindings in separate pure regions (separated by a call), so it can observe two different snapshots.",
        bad_example: "state a := 0\nstate b := 0\nx := pick(a, f(), b)",
        fixed_example: "atomic { x := pick(a, f(), b) }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(28),
        title: "index operation on incompatible type",
        description: "An index or assignment-by-index is used on a type that does not support it (string, bytes, or non-indexable type).",
        bad_example: "s: str = \"hello\"\ns[0] = 'x'",
        fixed_example: "a: [int] = [1, 2]\na[0] = 3",
    },
    ErrorCatalogEntry {
        code: ErrorCode(29),
        title: "unknown struct",
        description: "A struct initialization or field access references a struct that has not been declared.",
        bad_example: "p: Nope = Nope { x: 1 }",
        fixed_example: "struct Point { x: int }\np: Point = Point { x: 1 }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(30),
        title: "unknown variant",
        description: "A match arm or pattern references a variant that does not exist on the enum.",
        bad_example: "match opt { .nope(v) => v }",
        fixed_example: "match opt { .some(v) => v, .none => 0 }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(31),
        title: "control flow outside of context",
        description: "`break`, `continue`, `return`, or `defer` is used outside the construct that allows it (loop, function, etc.).",
        bad_example: "func f() { break }",
        fixed_example: "func f() { loop { break } }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(32),
        title: "expected boolean",
        description: "A condition or context requires a `bool` but a different type was found.",
        bad_example: "if 1 { }",
        fixed_example: "if true { }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(33),
        title: "http.request has no field",
        description: "An `http.request` field access uses a name that is not a valid request field.",
        bad_example: "r: http.request = ...\nr.mothod",
        fixed_example: "r: http.request = ...\nr.method",
    },
    ErrorCatalogEntry {
        code: ErrorCode(34),
        title: "cannot assign to immutable variable",
        description: "An assignment targets a variable declared with `const` or `let`.",
        bad_example: "const x: int = 1\nx = 2",
        fixed_example: "var x: int = 1\nx = 2",
    },
    ErrorCatalogEntry {
        code: ErrorCode(35),
        title: "cannot assign to field of non-struct",
        description: "An assignment-by-field targets a value that is not a struct.",
        bad_example: "x: int = 5\nx.y = 3",
        fixed_example: "struct Point { y: int }\np: Point = Point { y: 0 }\np.y = 3",
    },
    ErrorCatalogEntry {
        code: ErrorCode(36),
        title: "unknown parameter or named argument",
        description: "A function call uses a named argument that does not match any parameter, or a named argument conflicts with a positional one.",
        bad_example: "func f(x: int) {}\nf(y: 1)",
        fixed_example: "func f(x: int) {}\nf(x: 1)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(37),
        title: "unknown function or method",
        description: "A function call references a function or method that has not been declared.",
        bad_example: "dont_exist()",
        fixed_example: "func exist() {}\nexist()",
    },
    ErrorCatalogEntry {
        code: ErrorCode(38),
        title: "call arity error",
        description: "A function call provides the wrong number of arguments.",
        bad_example: "func f(x: int, y: int) {}\nf(1)",
        fixed_example: "func f(x: int, y: int) {}\nf(1, 2)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(39),
        title: "pattern argument error",
        description: "A `.ok`, `.err`, `.some`, or `.none` pattern is used with the wrong number of arguments.",
        bad_example: "match opt { .some => v }",
        fixed_example: "match opt { .some(v) => v }",
    },
    ErrorCatalogEntry {
        code: ErrorCode(40),
        title: "link directive error",
        description: "An `@link` directive is missing its library name or is otherwise malformed.",
        bad_example: "@link",
        fixed_example: "@link(\"sqlite3\")",
    },
    ErrorCatalogEntry {
        code: ErrorCode(41),
        title: "question mark on non-Result type",
        description: "The `?` operator is used on a value that is not `Result` or `Option`.",
        bad_example: "x: int = 5\ny: int = x?",
        fixed_example: "r: Result<int, str] = Ok(42)\ny: int = r?",
    },
    ErrorCatalogEntry {
        code: ErrorCode(42),
        title: "duplicate definition",
        description: "A struct, type alias, function, or variable is defined more than once in the same scope.",
        bad_example: "func f() {}\nfunc f() {}",
        fixed_example: "func f() {}",
    },
    ErrorCatalogEntry {
        code: ErrorCode(43),
        title: "generic bound violation",
        description: "A generic parameter or type argument does not satisfy a required trait bound.",
        bad_example: "func f<T: Num>(x: T) {}\nf(\"nope\")",
        fixed_example: "func f<T: Num>(x: T) {}\nf(42)",
    },
    ErrorCatalogEntry {
        code: ErrorCode(44),
        title: "enum or struct initialization error",
        description: "An enum variant or struct is initialized with incorrect arguments or fields.",
        bad_example: "struct Point { x: int }\np: Point = Point { y: 1 }",
        fixed_example: "struct Point { x: int }\np: Point = Point { x: 1 }",
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
