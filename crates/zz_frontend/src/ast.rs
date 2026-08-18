//! Abstract syntax tree.
//!
//! Every node carries a `Span` covering its exact source text. The printer
//! re-emits those slices verbatim, which makes parsing → printing a perfect
//! round-trip (lossless).

use crate::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
    /// Covers the entire source buffer, so printing a program reproduces the
    /// input exactly.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: Ident,
        ty: Option<Ty>,
        value: Expr,
        span: Span,
    },
    Func {
        name: Ident,
        generics: Vec<Ident>,
        params: Vec<Param>,
        ret: Option<Ty>,
        body: Block,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: Ident,
    pub ty: Option<Ty>,
    pub span: Span,
}

/// A `{ ... }` block. Its value is the last expression statement, or unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

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
    /// Parenthesized expression. Kept in the AST so the printer preserves the
    /// parens (round-trip fidelity); the checker/interpreter just unwrap it.
    Paren {
        expr: Box<Expr>,
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
        span: Span,
    },
    Closure {
        params: Vec<Param>,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pat: Pattern,
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Pos,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
}

impl UnOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Pos => "+",
            UnOp::Not => "!",
        }
    }
}

/// A type annotation as written in source.
#[derive(Debug, Clone, PartialEq)]
pub struct Ty {
    pub kind: TyKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TyKind {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Tuple(Vec<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Func(Vec<Ty>, Box<Ty>),
    /// Named type: a generic parameter or a future struct/alias.
    Named(String, Vec<Ty>),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::Str { span, .. }
            | Expr::Bool { span, .. }
            | Expr::Ident { span, .. }
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
            | Expr::Variant { span, .. } => *span,
            Expr::Block(b) => b.span,
        }
    }
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. } | Stmt::Func { span, .. } | Stmt::Return { span, .. } => *span,
            Stmt::Expr(e) => e.span(),
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
        }
    }
}
