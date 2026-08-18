//! Lossless lexer.
//!
//! Rules:
//! - Whitespace, comments, and newlines *inside* brackets are trivia attached
//!   to the following significant token.
//! - A newline or `;` at bracket depth 0 becomes a significant `StmtEnd`
//!   token (the statement terminator). This gives newline-significant syntax
//!   with optional semicolons, while multi-line expressions inside parens
//!   just work.
//! - Block comments nest.

use crate::diag::{error_at, RawDiag};
use crate::span::Span;
use crate::token::{Token, TokenKind, Trivia, TriviaKind};

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<RawDiag>,
}

pub fn lex(source: &str) -> Lexed {
    Lexer::new(source).run()
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    depth: u32,
    prev_sig: Option<TokenKind>,
    pending: Vec<Trivia>,
    tokens: Vec<Token>,
    errors: Vec<RawDiag>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer {
            src,
            pos: 0,
            depth: 0,
            prev_sig: None,
            pending: Vec::new(),
            tokens: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn run(mut self) -> Lexed {
        while self.pos < self.src.len() {
            let c = self.peek_char().unwrap();
            match c {
                ' ' | '\t' | '\r' => self.push_trivia(TriviaKind::Whitespace),
                '\n' => {
                    if self.depth == 0 && !self.line_continues() {
                        self.emit_significant(TokenKind::StmtEnd, self.pos, self.pos + 1);
                    } else {
                        // Either inside brackets or continuing an expression:
                        // the newline is trivia, not a statement terminator.
                        self.push_trivia(TriviaKind::Newline);
                    }
                }
                ';' => self.emit_significant(TokenKind::StmtEnd, self.pos, self.pos + 1),
                '/' if self.peek_char_at(1) == Some('/') => self.lex_line_comment(),
                '/' if self.peek_char_at(1) == Some('*') => self.lex_block_comment(),
                '/' => self.emit_significant(TokenKind::Slash, self.pos, self.pos + 1),
                '(' => {
                    self.depth += 1;
                    self.emit_significant(TokenKind::LParen, self.pos, self.pos + 1);
                }
                ')' => {
                    self.depth = self.depth.saturating_sub(1);
                    self.emit_significant(TokenKind::RParen, self.pos, self.pos + 1);
                }
                '+' => self.emit_significant(TokenKind::Plus, self.pos, self.pos + 1),
                '-' => self.emit_significant(TokenKind::Minus, self.pos, self.pos + 1),
                '*' => self.emit_significant(TokenKind::Star, self.pos, self.pos + 1),
                '%' => self.emit_significant(TokenKind::Percent, self.pos, self.pos + 1),
                '=' => self.emit_significant(TokenKind::Assign, self.pos, self.pos + 1),
                c if c.is_ascii_digit() => self.lex_number(),
                c if is_ident_start(c) => self.lex_ident(),
                _ => {
                    let start = self.pos;
                    self.bump_char();
                    let span = Span::new(start as u32, self.pos as u32);
                    self.errors
                        .push(error_at(format!("unexpected character `{c}`"), span));
                }
            }
        }
        // Trailing trivia before EOF is dropped from the token stream but the
        // program span still covers the full buffer, so round-trips stay exact.
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            text: String::new(),
            span: Span::new(self.src.len() as u32, self.src.len() as u32),
            leading: Vec::new(),
        });
        Lexed {
            tokens: self.tokens,
            errors: self.errors,
        }
    }

    // --- trivia -----------------------------------------------------------

    fn push_trivia(&mut self, kind: TriviaKind) {
        let start = self.pos;
        let c = self.bump_char();
        let span = Span::new(start as u32, self.pos as u32);
        self.pending.push(Trivia {
            kind,
            text: c.to_string(),
            span,
        });
    }

    fn lex_line_comment(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if c == '\n' {
                break;
            }
            self.bump_char();
        }
        let span = Span::new(start as u32, self.pos as u32);
        self.pending.push(Trivia {
            kind: TriviaKind::Comment,
            text: self.src[span.to_range()].to_string(),
            span,
        });
    }

    fn lex_block_comment(&mut self) {
        let start = self.pos;
        let mut nest = 0u32;
        loop {
            match (self.peek_char(), self.peek_char_at(1)) {
                (Some('/'), Some('*')) => {
                    nest += 1;
                    self.bump_char();
                    self.bump_char();
                }
                (Some('*'), Some('/')) => {
                    nest -= 1;
                    self.bump_char();
                    self.bump_char();
                    if nest == 0 {
                        break;
                    }
                }
                (Some(_), _) => {
                    self.bump_char();
                }
                (None, _) => {
                    let span = Span::new(start as u32, self.src.len() as u32);
                    self.errors
                        .push(error_at("unterminated block comment", span));
                    return;
                }
            }
        }
        let span = Span::new(start as u32, self.pos as u32);
        self.pending.push(Trivia {
            kind: TriviaKind::Comment,
            text: self.src[span.to_range()].to_string(),
            span,
        });
    }

    // --- significant tokens ----------------------------------------------

    /// True if the previous significant token implies the current line
    /// continues (Go-style: newline after an operator or `=` is dropped).
    fn line_continues(&self) -> bool {
        matches!(
            self.prev_sig,
            Some(
                TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::Slash
                    | TokenKind::Percent
                    | TokenKind::Assign
                    | TokenKind::LParen
            )
        )
    }

    fn emit_significant(&mut self, kind: TokenKind, start: usize, end: usize) {
        let span = Span::new(start as u32, end as u32);
        let text = self.src[span.to_range()].to_string();
        self.pos = end;
        self.push_token(kind, span, text);
    }

    fn push_token(&mut self, kind: TokenKind, span: Span, text: String) {
        self.prev_sig = Some(kind);
        self.tokens.push(Token {
            kind,
            text,
            span,
            leading: std::mem::take(&mut self.pending),
        });
    }

    fn lex_ident(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if is_ident_continue(c) {
                self.bump_char();
            } else {
                break;
            }
        }
        let span = Span::new(start as u32, self.pos as u32);
        let text = self.src[span.to_range()].to_string();
        let kind = match text.as_str() {
            "let" => TokenKind::Let,
            _ => TokenKind::Ident,
        };
        self.push_token(kind, span, text);
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() || c == '_' {
                self.bump_char();
            } else {
                break;
            }
        }
        let mut is_float = false;
        if self.peek_char() == Some('.') && self.peek_char_at(1).is_some_and(|c| c.is_ascii_digit())
        {
            is_float = true;
            self.bump_char(); // '.'
            while let Some(c) = self.peek_char() {
                if c.is_ascii_digit() || c == '_' {
                    self.bump_char();
                } else {
                    break;
                }
            }
        } else if self.peek_char() == Some('.') {
            // `1.` — a dot with no digits after it is not a float.
            let span = Span::new(start as u32, (self.pos + '.'.len_utf8()) as u32);
            self.errors
                .push(error_at("expected digit after decimal point", span));
        }
        // `123abc` is a single invalid token, not two.
        if self.peek_char().is_some_and(is_ident_continue) {
            while let Some(c) = self.peek_char() {
                if is_ident_continue(c) {
                    self.bump_char();
                } else {
                    break;
                }
            }
            let span = Span::new(start as u32, self.pos as u32);
            self.errors.push(error_at("invalid number literal", span));
            return;
        }
        let span = Span::new(start as u32, self.pos as u32);
        let text = self.src[span.to_range()].to_string();
        let kind = if is_float {
            TokenKind::Float
        } else {
            TokenKind::Int
        };
        self.push_token(kind, span, text);
    }

    // --- char helpers -----------------------------------------------------

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_char_at(&self, offset: usize) -> Option<char> {
        let idx = self.pos + offset;
        if idx >= self.src.len() || !self.src.is_char_boundary(idx) {
            return None;
        }
        self.src[idx..].chars().next()
    }

    fn bump_char(&mut self) -> char {
        let c = self.peek_char().expect("bump past end of input");
        self.pos += c.len_utf8();
        c
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind as K;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn basic_expression() {
        assert_eq!(
            kinds("let x = 1 + 2"),
            vec![K::Let, K::Ident, K::Assign, K::Int, K::Plus, K::Int, K::Eof]
        );
    }

    #[test]
    fn newline_is_stmt_end_at_depth_zero() {
        assert_eq!(kinds("1\n2"), vec![K::Int, K::StmtEnd, K::Int, K::Eof]);
    }

    #[test]
    fn newline_after_operator_is_trivia() {
        assert_eq!(kinds("1 +\n2"), vec![K::Int, K::Plus, K::Int, K::Eof]);
    }

    #[test]
    fn newline_after_assign_is_trivia() {
        assert_eq!(
            kinds("let x =\n1"),
            vec![K::Let, K::Ident, K::Assign, K::Int, K::Eof]
        );
    }

    #[test]
    fn semicolon_is_stmt_end() {
        assert_eq!(kinds("1;2"), vec![K::Int, K::StmtEnd, K::Int, K::Eof]);
    }

    #[test]
    fn comments_are_trivia() {
        let lexed = lex("1 // comment\n2");
        assert_eq!(
            lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![K::Int, K::StmtEnd, K::Int, K::Eof]
        );
        // The comment rides as leading trivia on the StmtEnd that follows it.
        let stmt_end = &lexed.tokens[1];
        assert_eq!(stmt_end.leading.len(), 2);
        assert_eq!(stmt_end.leading[0].kind, TriviaKind::Whitespace);
        assert_eq!(stmt_end.leading[1].kind, TriviaKind::Comment);
    }

    #[test]
    fn nested_block_comments() {
        let lexed = lex("1 /* a /* b */ c */ 2");
        assert!(lexed.errors.is_empty(), "errors: {:?}", lexed.errors);
        assert_eq!(
            lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![K::Int, K::Int, K::Eof]
        );
    }

    #[test]
    fn unterminated_block_comment_errors() {
        let lexed = lex("1 /* never closed");
        assert_eq!(lexed.errors.len(), 1);
    }

    #[test]
    fn invalid_number_literal() {
        let lexed = lex("123abc");
        assert_eq!(lexed.errors.len(), 1);
    }

    #[test]
    fn float_literals() {
        assert_eq!(
            kinds("1.5 + 2.0"),
            vec![K::Float, K::Plus, K::Float, K::Eof]
        );
    }

    #[test]
    fn underscores_in_numbers() {
        assert_eq!(kinds("1_000"), vec![K::Int, K::Eof]);
    }

    #[test]
    fn unexpected_character_errors() {
        let lexed = lex("1 @ 2");
        assert_eq!(lexed.errors.len(), 1);
    }
}
