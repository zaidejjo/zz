//! Lossless lexer.
//!
//! Rules:
//! - Whitespace, comments, and newlines *inside* brackets are trivia attached
//!   to the following significant token.
//! - A newline or `;` at bracket depth 0 becomes a significant `StmtEnd`
//!   token (the statement terminator). This gives newline-significant syntax
//!   with optional semicolons, while multi-line expressions inside parens
//!   just work.
//! - A newline after an operator or `=` is dropped (Go-style continuation).
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
                    if !self.line_continues() {
                        // A newline terminates a statement unless the previous
                        // token implies the expression continues (Go-style).
                        // This applies inside braces too, which is what makes
                        // match arms and block statements parse.
                        self.emit_significant(TokenKind::StmtEnd, self.pos, self.pos + 1);
                    } else {
                        self.push_trivia(TriviaKind::Newline);
                    }
                }
                ';' => self.emit_significant(TokenKind::StmtEnd, self.pos, self.pos + 1),
                '/' if self.peek_char_at(1) == Some('/') => self.lex_line_comment(),
                '/' if self.peek_char_at(1) == Some('*') => self.lex_block_comment(),
                '/' => self.emit_significant(TokenKind::Slash, self.pos, self.pos + 1),
                '(' => self.emit_significant(TokenKind::LParen, self.pos, self.pos + 1),
                ')' => self.emit_significant(TokenKind::RParen, self.pos, self.pos + 1),
                '{' => self.emit_significant(TokenKind::LBrace, self.pos, self.pos + 1),
                '}' => self.emit_significant(TokenKind::RBrace, self.pos, self.pos + 1),
                '+' => self.emit_significant(TokenKind::Plus, self.pos, self.pos + 1),
                '-' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::Arrow, self.pos, self.pos + 2)
                }
                '-' => self.emit_significant(TokenKind::Minus, self.pos, self.pos + 1),
                '*' => self.emit_significant(TokenKind::Star, self.pos, self.pos + 1),
                '%' => self.emit_significant(TokenKind::Percent, self.pos, self.pos + 1),
                '=' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Eq, self.pos, self.pos + 2)
                }
                '=' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::Arrow, self.pos, self.pos + 2)
                }
                '=' => self.emit_significant(TokenKind::Assign, self.pos, self.pos + 1),
                '!' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Ne, self.pos, self.pos + 2)
                }
                '!' => self.emit_significant(TokenKind::Bang, self.pos, self.pos + 1),
                '<' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Le, self.pos, self.pos + 2)
                }
                '<' => self.emit_significant(TokenKind::Lt, self.pos, self.pos + 1),
                '>' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Ge, self.pos, self.pos + 2)
                }
                '>' => self.emit_significant(TokenKind::Gt, self.pos, self.pos + 1),
                '&' if self.peek_char_at(1) == Some('&') => {
                    self.emit_significant(TokenKind::AndAnd, self.pos, self.pos + 2)
                }
                '|' if self.peek_char_at(1) == Some('|') => {
                    self.emit_significant(TokenKind::OrOr, self.pos, self.pos + 2)
                }
                '|' => self.emit_significant(TokenKind::Pipe, self.pos, self.pos + 1),
                '?' => self.emit_significant(TokenKind::Question, self.pos, self.pos + 1),
                ':' => self.emit_significant(TokenKind::Colon, self.pos, self.pos + 1),
                ',' => self.emit_significant(TokenKind::Comma, self.pos, self.pos + 1),
                '.' => self.emit_significant(TokenKind::Dot, self.pos, self.pos + 1),
                '"' => self.lex_string(),
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
                    | TokenKind::Eq
                    | TokenKind::Ne
                    | TokenKind::Lt
                    | TokenKind::Gt
                    | TokenKind::Le
                    | TokenKind::Ge
                    | TokenKind::AndAnd
                    | TokenKind::OrOr
                    | TokenKind::Bang
                    | TokenKind::Question
                    | TokenKind::Colon
                    | TokenKind::Comma
                    | TokenKind::Dot
                    | TokenKind::Pipe
                    | TokenKind::Arrow
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
            "func" => TokenKind::Func,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "match" => TokenKind::Match,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
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

    fn lex_string(&mut self) {
        let start = self.pos;
        self.bump_char(); // opening quote
        let mut value = String::new();
        loop {
            match self.peek_char() {
                Some('"') => {
                    self.bump_char();
                    break;
                }
                Some('\\') => {
                    self.bump_char();
                    match self.peek_char() {
                        Some('n') => {
                            value.push('\n');
                            self.bump_char();
                        }
                        Some('t') => {
                            value.push('\t');
                            self.bump_char();
                        }
                        Some('r') => {
                            value.push('\r');
                            self.bump_char();
                        }
                        Some('\\') => {
                            value.push('\\');
                            self.bump_char();
                        }
                        Some('"') => {
                            value.push('"');
                            self.bump_char();
                        }
                        Some(other) => {
                            let span = Span::new(
                                (self.pos - 1) as u32,
                                (self.pos + other.len_utf8()) as u32,
                            );
                            self.errors
                                .push(error_at(format!("unknown escape `\\{other}`"), span));
                            self.bump_char();
                        }
                        None => break,
                    }
                }
                Some(c) => {
                    value.push(c);
                    self.bump_char();
                }
                None => {
                    let span = Span::new(start as u32, self.src.len() as u32);
                    self.errors
                        .push(error_at("unterminated string literal", span));
                    return;
                }
            }
        }
        let span = Span::new(start as u32, self.pos as u32);
        self.push_token(TokenKind::Str, span, value);
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
    fn newline_inside_parens_is_trivia() {
        assert_eq!(
            kinds("(1 +\n2)"),
            vec![K::LParen, K::Int, K::Plus, K::Int, K::RParen, K::Eof]
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

    #[test]
    fn keywords() {
        assert_eq!(
            kinds("func return if else while match true false"),
            vec![
                K::Func,
                K::Return,
                K::If,
                K::Else,
                K::While,
                K::Match,
                K::True,
                K::False,
                K::Eof
            ]
        );
    }

    #[test]
    fn multi_char_operators() {
        assert_eq!(
            kinds("a == b != c <= d >= e && f || g -> h"),
            vec![
                K::Ident,
                K::Eq,
                K::Ident,
                K::Ne,
                K::Ident,
                K::Le,
                K::Ident,
                K::Ge,
                K::Ident,
                K::AndAnd,
                K::Ident,
                K::OrOr,
                K::Ident,
                K::Arrow,
                K::Ident,
                K::Eof
            ]
        );
    }

    #[test]
    fn string_literals() {
        let lexed = lex(r#""hello\nworld""#);
        assert!(lexed.errors.is_empty(), "errors: {:?}", lexed.errors);
        assert_eq!(lexed.tokens[0].kind, K::Str);
        assert_eq!(lexed.tokens[0].text, "hello\nworld");
    }

    #[test]
    fn unterminated_string_errors() {
        let lexed = lex("\"oops");
        assert_eq!(lexed.errors.len(), 1);
    }

    #[test]
    fn unknown_escape_errors() {
        let lexed = lex(r#""\q""#);
        assert_eq!(lexed.errors.len(), 1);
    }

    #[test]
    fn braces_and_pipe() {
        assert_eq!(
            kinds("|x| x + 1"),
            vec![
                K::Pipe,
                K::Ident,
                K::Pipe,
                K::Ident,
                K::Plus,
                K::Int,
                K::Eof
            ]
        );
        assert_eq!(kinds("a || b"), vec![K::Ident, K::OrOr, K::Ident, K::Eof]);
    }
}
