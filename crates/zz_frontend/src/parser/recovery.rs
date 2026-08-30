//! Error recovery and helper functions.

use crate::diag::{error_at, RawDiag};
use crate::span::Span;
use crate::token::{Token, TokenKind, TriviaKind};

use super::Parser;

impl Parser {
    // --- helpers ----------------------------------------------------------

    pub(crate) fn expect_ident(&mut self) -> Option<crate::ast::Ident> {
        if self.at(TokenKind::Ident) {
            let tok = self.advance();
            Some(crate::ast::Ident {
                name: tok.text,
                span: tok.span,
            })
        } else {
            self.error_here(format!(
                "expected identifier, found {}",
                self.peek_kind().describe()
            ));
            None
        }
    }

    pub(crate) fn error_here(&mut self, msg: impl Into<String>) -> Span {
        let span = self.peek().span;
        self.errors.push(error_at(msg, span));
        span
    }

    pub(crate) fn skip_stmt_ends(&mut self) {
        while self.at(TokenKind::StmtEnd) {
            self.advance();
        }
    }

    pub(crate) fn skip_to_stmt_end(&mut self) {
        while !self.at(TokenKind::StmtEnd) && !self.at(TokenKind::Eof) {
            self.advance();
        }
    }

    pub(crate) fn skip_to_rbrace(&mut self) {
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            self.advance();
        }
    }

    pub(crate) fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    pub(crate) fn peek_kind(&self) -> TokenKind {
        self.toks[self.pos].kind
    }

    pub(crate) fn peek_kind_at(&self, offset: usize) -> TokenKind {
        self.toks
            .get(self.pos + offset)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    pub(crate) fn at_ident(&self, name: &str) -> bool {
        self.at(TokenKind::Ident) && self.peek().text == name
    }

    pub(crate) fn at(&self, kind: TokenKind) -> bool {
        self.peek_kind() == kind
    }

    pub(crate) fn advance(&mut self) -> Token {
        let tok = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        tok
    }

    pub(crate) fn previous(&self) -> &Token {
        &self.toks[self.pos.saturating_sub(1)]
    }

    pub(crate) fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume a closing delimiter, skipping statement terminators first so
    /// multi-line calls/expressions like `f(\n  a\n)` parse.
    pub(crate) fn eat_close(&mut self, kind: TokenKind) -> bool {
        self.skip_stmt_ends();
        self.eat(kind)
    }

    /// Peek ahead after a `{` to see if it starts a match arm block.
    /// Returns true if the content looks like patterns followed by `=>`.
    pub(crate) fn looks_like_match_arm_block(&self) -> bool {
        let mut idx = self.pos + 1; // skip the LBrace token
        while idx < self.toks.len() {
            let tok = &self.toks[idx];
            match tok.kind {
                TokenKind::StmtEnd => {
                    idx += 1;
                    continue;
                }
                TokenKind::Ident if tok.text == "_" => return true, // wildcard pattern
                TokenKind::Arrow => return true,                    // => directly
                TokenKind::Str
                | TokenKind::Int
                | TokenKind::Float
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Ident
                | TokenKind::Dot
                | TokenKind::LBrace => {
                    // Could be a pattern; scan ahead for `=>`.
                    let mut j = idx;
                    while j < self.toks.len() {
                        let t = &self.toks[j];
                        match t.kind {
                            TokenKind::StmtEnd => j += 1,
                            TokenKind::Arrow => return true,
                            TokenKind::Str
                            | TokenKind::Int
                            | TokenKind::Float
                            | TokenKind::True
                            | TokenKind::False
                            | TokenKind::Ident
                            | TokenKind::Dot
                            | TokenKind::LBrace
                            | TokenKind::RBrace
                            | TokenKind::Pipe
                            | TokenKind::Comma => j += 1,
                            _ => break,
                        }
                    }
                    return false;
                }
                _ => return false,
            }
        }
        false
    }

    /// Check whether the current `LBrace` token is part of a string
    /// interpolation (e.g. `"hello {name}"`) rather than a block delimiter
    /// (e.g. `if x == "zaid" { ... }`).
    ///
    /// The lexer emits `Str` tokens for both cases, but their spans differ:
    /// - **Complete string** (`"zaid"`): span covers both quotes,
    ///   so `span_length - text_length >= 2`.
    /// - **Interpolation segment** (`"hello "` before `{name}`): span ends
    ///   at the `{` (no closing quote yet), so
    ///   `span_length - text_length < 2`.
    ///
    /// We also keep the trivia check as a fast path for the common case
    /// where there IS whitespace between the string and `{`.
    pub(crate) fn lbrace_is_interpolation(&self) -> bool {
        // Fast path: whitespace between string and `{` means it's a block.
        if self
            .peek()
            .leading
            .iter()
            .any(|t| t.kind == TriviaKind::Whitespace || t.kind == TriviaKind::Newline)
        {
            return false;
        }
        // Check the previous token (should be Str).
        let prev = self.previous();
        if prev.kind != TokenKind::Str {
            return false;
        }
        let span_len = (prev.span.end - prev.span.start) as usize;
        let text_len = prev.text.len();
        // For a complete string the span includes both quote characters,
        // so span_len - text_len >= 2.  For an interpolation segment the
        // span ends at the `{` (only the opening quote is inside), so the
        // difference is < 2.
        span_len - text_len < 2
    }
}
