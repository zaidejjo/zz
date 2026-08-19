//! Token definitions for the lossless lexer.
//!
//! The lexer produces a stream of significant tokens, each carrying its
//! `leading` trivia (whitespace, newlines inside brackets, comments). This
//! preserves the original source text exactly — the foundation for the
//! formatter later.

use crate::span::Span;

/// Trivia: text that carries no syntax meaning but must be preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriviaKind {
    Whitespace,
    Newline,
    Comment,
}

/// A significant token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub span: Span,
    pub leading: Vec<Trivia>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    // Keywords
    Import,
    As,
    Func,
    Return,
    If,
    Else,
    While,
    Match,
    True,
    False,
    Struct,
    For,
    In,
    Break,
    Continue,
    // Literals
    Int,
    Float,
    Str,
    Ident,
    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    Question,
    Colon,
    ColonEq,
    Comma,
    Dot,
    DotDot,
    Pipe,
    PipeGt,
    Arrow,
    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    // Statement terminator: `;` or a newline at bracket depth 0
    StmtEnd,
    Eof,
}

impl TokenKind {
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Import => "`import`",
            TokenKind::As => "`as`",
            TokenKind::Func => "`func`",
            TokenKind::Return => "`return`",
            TokenKind::If => "`if`",
            TokenKind::Else => "`else`",
            TokenKind::While => "`while`",
            TokenKind::Match => "`match`",
            TokenKind::True => "`true`",
            TokenKind::False => "`false`",
            TokenKind::Struct => "`struct`",
            TokenKind::For => "`for`",
            TokenKind::In => "`in`",
            TokenKind::Break => "`break`",
            TokenKind::Continue => "`continue`",
            TokenKind::Int => "integer literal",
            TokenKind::Float => "float literal",
            TokenKind::Str => "string literal",
            TokenKind::Ident => "identifier",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::Percent => "`%`",
            TokenKind::Assign => "`=`",
            TokenKind::Eq => "`==`",
            TokenKind::Ne => "`!=`",
            TokenKind::Lt => "`<`",
            TokenKind::Gt => "`>`",
            TokenKind::Le => "`<=`",
            TokenKind::Ge => "`>=`",
            TokenKind::AndAnd => "`&&`",
            TokenKind::OrOr => "`||`",
            TokenKind::Bang => "`!`",
            TokenKind::Question => "`?`",
            TokenKind::Colon => "`:`",
            TokenKind::ColonEq => "`:=`",
            TokenKind::Comma => "`,`",
            TokenKind::Dot => "`.`",
            TokenKind::DotDot => "`..`",
            TokenKind::Pipe => "`|`",
            TokenKind::PipeGt => "`|>`",
            TokenKind::Arrow => "`->`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::StmtEnd => "end of statement",
            TokenKind::Eof => "end of input",
        }
    }
}
