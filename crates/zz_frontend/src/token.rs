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
    Let,
    // Literals
    Int,
    Float,
    Ident,
    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    // Delimiters
    LParen,
    RParen,
    // Statement terminator: `;` or a newline at bracket depth 0
    StmtEnd,
    Eof,
}

impl TokenKind {
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Let => "`let`",
            TokenKind::Int => "integer literal",
            TokenKind::Float => "float literal",
            TokenKind::Ident => "identifier",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::Percent => "`%`",
            TokenKind::Assign => "`=`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::StmtEnd => "end of statement",
            TokenKind::Eof => "end of input",
        }
    }
}
