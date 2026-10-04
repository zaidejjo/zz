//! Error recovery and helper functions.

use crate::diag::error_at;
use crate::span::Span;
use crate::token::{Token, TokenKind};

use super::Parser;

impl<'a> Parser<'a> {
    // --- helpers ----------------------------------------------------------

    pub(crate) fn expect_ident(&mut self) -> Option<crate::ast::Ident> {
        if self.at(TokenKind::Ident) {
            let tok = self.advance();
            Some(crate::ast::Ident {
                name: tok.text.into_owned(),
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

    pub(crate) fn peek(&self) -> &Token<'a> {
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

    pub(crate) fn advance(&mut self) -> Token<'a> {
        // Clone without allocating: `text` is usually a borrowed source
        // slice (`Cow::Borrowed` clones as a pointer copy) and the parser
        // never reads `leading` trivia, so it is dropped instead of cloned
        // (saves one Vec allocation per consumed token).
        let src = &self.toks[self.pos];
        let tok = Token {
            kind: src.kind,
            text: src.text.clone(),
            span: src.span,
            leading: Vec::new(),
        };
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        tok
    }

    pub(crate) fn previous(&self) -> &Token<'a> {
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

    /// Consume a `>` closing a `<...>` type-argument list, splitting a
    /// `>>` (`Shr`) token when nested generics close together:
    /// `Option<Option<int>>` lexes the end as one `Shr`, which closes
    /// the inner list and banks one owed `>` (via `pending_gt`) for the
    /// outer list. Arbitrary depth works: each split banks one close.
    pub(crate) fn eat_gt_close(&mut self) -> bool {
        if self.pending_gt > 0 {
            self.pending_gt -= 1;
            return true;
        }
        if self.eat(TokenKind::Gt) {
            return true;
        }
        if self.eat(TokenKind::Shr) {
            self.pending_gt += 1;
            return true;
        }
        false
    }

    /// Consume a closing delimiter, skipping statement terminators first so
    /// multi-line calls/expressions like `f(\n  a\n)` parse.
    pub(crate) fn eat_close(&mut self, kind: TokenKind) -> bool {
        self.skip_stmt_ends();
        self.eat(kind)
    }
}
