//! Expression AST nodes.

use crate::ast::types::Ty;
use crate::span::Span;

/// One piece of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum FmtPart {
    /// Literal text (escapes already processed).
    Text(String),
    /// An embedded expression, rendered via its Display form.
    /// Optional format spec: `{val:.2f}`, `{val:x}`, etc.
    Expr(Box<Expr>, Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard {
        span: Span,
    },
    Binding {
        name: Ident,
    },
    Literal {
        value: Lit,
        span: Span,
    },
    Variant {
        name: String,
        arg: Option<Box<Pattern>>,
        span: Span,
    },
    Tuple {
        pats: Vec<Pattern>,
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

/// An identifier with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// Expression AST.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int {
        value: i64,
        span: Span,
    },
    Float {
        value: f64,
        span: Span,
    },
    Str {
        value: String,
        span: Span,
    },
    Bool {
        value: bool,
        span: Span,
    },
    Ident {
        name: String,
        span: Span,
    },
    /// Dotted path: `std.io.println`. Resolved as a single qualified name.
    Path {
        parts: Vec<String>,
        span: Span,
    },
    /// Interpolated string: `"Hello {name}"`. Parts alternate between
    /// literal text and embedded expressions.
    Fmt {
        parts: Vec<FmtPart>,
        span: Span,
    },
    /// Parenthesized expression. Kept in the AST so the printer preserves the
    /// parens (round-trip fidelity); the checker/interpreter just unwrap it.
    Paren {
        expr: Box<Expr>,
        span: Span,
    },
    /// Tuple literal: `(a, b, c)`.
    Tuple {
        items: Vec<Expr>,
        span: Span,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        named: Vec<(String, Expr)>,
        span: Span,
    },
    Closure {
        params: Vec<Param>,
        ret_ty: Option<Ty>,
        body: Box<Expr>,
        span: Span,
    },
    If {
        cond: Box<Expr>,
        then: Block,
        els: Option<Box<Expr>>,
        span: Span,
    },
    While {
        cond: Box<Expr>,
        body: Block,
        span: Span,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    IfLet {
        pat: Pattern,
        value: Box<Expr>,
        then: Block,
        els: Option<Box<Expr>>,
        span: Span,
    },
    /// Postfix `?`: unwrap Option/Result or propagate.
    Try {
        expr: Box<Expr>,
        span: Span,
    },
    Block(Block),
    /// Variant constructor: `.ok(x)`, `.err(e)`, `.some(x)`, `.none`.
    Variant {
        name: String,
        arg: Option<Box<Expr>>,
        span: Span,
    },
    /// Array literal: `[10, 20, 30]`.
    Array {
        elems: Vec<Expr>,
        span: Span,
    },
    /// Dictionary literal: `{"name": "Zaid", "age": 20}`. Entries are
    /// `(key, value)` pairs.
    Dict {
        entries: Vec<(Expr, Expr)>,
        span: Span,
    },
    /// Field access on a non-trivial base: `makePoint().x`. Pure identifier
    /// chains (`p.x`) parse as [`Expr::Path`] instead.
    Field {
        obj: Box<Expr>,
        name: String,
        span: Span,
    },
    /// `a..b` — an integer range (used by `for` loops).
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        span: Span,
    },
    /// `Point{ x: 1, y: 2 }` — struct construction with named fields.
    StructInit {
        name: String,
        fields: Vec<(String, Expr)>,
        span: Span,
    },
    /// `arr[0]`, `dict["key"]`, `str[0]` — element access.
    Index {
        obj: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    /// `s[1:3]`, `s[:2]`, `s[1:]` — array/string slicing.
    Slice {
        obj: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        span: Span,
    },
    /// List comprehension: `[expr for x in iter]` or
    /// `[expr for x in iter if cond]`.
    ListComp {
        body: Box<Expr>,
        var: Ident,
        iter: Box<Expr>,
        filter: Option<Box<Expr>>,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::Str { span, .. }
            | Expr::Bool { span, .. }
            | Expr::Ident { span, .. }
            | Expr::Path { span, .. }
            | Expr::Fmt { span, .. }
            | Expr::Paren { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Closure { span, .. }
            | Expr::If { span, .. }
            | Expr::While { span, .. }
            | Expr::Match { span, .. }
            | Expr::IfLet { span, .. }
            | Expr::Try { span, .. }
            | Expr::Variant { span, .. }
            | Expr::Array { span, .. }
            | Expr::Dict { span, .. }
            | Expr::Field { span, .. }
            | Expr::Range { span, .. }
            | Expr::StructInit { span, .. }
            | Expr::Index { span, .. }
            | Expr::Slice { span, .. }
            | Expr::ListComp { span, .. }
            | Expr::Tuple { span, .. } => *span,
            Expr::Block(b) => b.span,
        }
    }
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard { span } => *span,
            Pattern::Binding { name } => name.span,
            Pattern::Literal { span, .. } => *span,
            Pattern::Variant { span, .. } => *span,
            Pattern::Tuple { span, .. } => *span,
        }
    }
}

// Re-export from stmt.rs to avoid circular dependency
use crate::ast::stmt::{Block, Param};

use crate::ast::types::{BinOp, UnOp};
