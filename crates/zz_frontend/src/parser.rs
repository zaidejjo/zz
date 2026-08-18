//! Recursive-descent parser with statement-level error recovery.
//!
//! Grammar (Phase 0):
//! ```text
//! program        := stmt* eof
//! stmt           := let_stmt | expr_stmt
//! let_stmt       := 'let' IDENT '=' expr
//! expr           := additive
//! additive       := multiplicative (('+' | '-') multiplicative)*
//! multiplicative := unary (('*' | '/' | '%') unary)*
//! unary          := ('-' | '+') unary | primary
//! primary        := INT | FLOAT | IDENT | '(' expr ')'
//! ```
//!
//! On a statement-level error the parser records a diagnostic and skips to
//! the next `StmtEnd`, so one bad line never hides the rest of the program.

use crate::ast::{BinOp, Expr, Ident, Program, Stmt, UnOp};
use crate::diag::{error_at, RawDiag};
use crate::lexer::lex;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub struct Parsed {
    pub program: Program,
    pub errors: Vec<RawDiag>,
}

pub fn parse(source: &str) -> Parsed {
    let lexed = lex(source);
    let mut parser = Parser {
        toks: lexed.tokens,
        pos: 0,
        errors: lexed.errors,
    };
    let program = parser.parse_program();
    Parsed {
        program,
        errors: parser.errors,
    }
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    errors: Vec<RawDiag>,
}

impl Parser {
    fn parse_program(&mut self) -> Program {
        let mut stmts = Vec::new();
        loop {
            self.skip_stmt_ends();
            if self.at(TokenKind::Eof) {
                break;
            }
            let errors_before = self.errors.len();
            let pos_before = self.pos;
            let stmt = self.parse_stmt();
            if self.errors.len() > errors_before {
                self.skip_to_stmt_end();
            } else if self.pos == pos_before {
                // No progress and no error: force forward to avoid a loop.
                self.advance();
            } else if !self.at(TokenKind::StmtEnd) && !self.at(TokenKind::Eof) {
                // Two statements with no terminator between them.
                self.error_here(format!(
                    "expected end of statement, found {}",
                    self.peek_kind().describe()
                ));
                self.skip_to_stmt_end();
            }
            stmts.push(stmt);
        }
        let span = Span::new(0, self.src_len());
        Program { stmts, span }
    }

    fn src_len(&self) -> u32 {
        // The EOF token's span start is the buffer length.
        self.toks.last().map(|t| t.span.start).unwrap_or(0)
    }

    fn parse_stmt(&mut self) -> Stmt {
        if self.at(TokenKind::Let) {
            self.parse_let()
        } else {
            Stmt::Expr(self.parse_expr())
        }
    }

    fn parse_let(&mut self) -> Stmt {
        let let_tok = self.advance(); // `let`
        let name = match self.expect_ident() {
            Some(id) => id,
            None => {
                return Stmt::Expr(dummy_expr(let_tok.span));
            }
        };
        if !self.eat(TokenKind::Assign) {
            self.error_here("expected `=` in let binding");
            return Stmt::Expr(dummy_expr(let_tok.span));
        }
        let value = self.parse_expr();
        let span = let_tok.span.join(value.span());
        Stmt::Let { name, value, span }
    }

    // --- expressions ------------------------------------------------------

    fn parse_expr(&mut self) -> Expr {
        self.parse_additive()
    }

    fn parse_additive(&mut self) -> Expr {
        let mut left = self.parse_multiplicative();
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative();
            let span = left.span().join(right.span());
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        left
    }

    fn parse_multiplicative(&mut self) -> Expr {
        let mut left = self.parse_unary();
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Rem,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary();
            let span = left.span().join(right.span());
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        left
    }

    fn parse_unary(&mut self) -> Expr {
        let op = match self.peek_kind() {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::Plus => UnOp::Pos,
            _ => return self.parse_primary(),
        };
        let op_tok = self.advance();
        let expr = self.parse_unary();
        let span = op_tok.span.join(expr.span());
        Expr::Unary {
            op,
            expr: Box::new(expr),
            span,
        }
    }

    fn parse_primary(&mut self) -> Expr {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Int => {
                self.advance();
                let cleaned = tok.text.replace('_', "");
                match cleaned.parse::<i64>() {
                    Ok(value) => Expr::Int {
                        value,
                        span: tok.span,
                    },
                    Err(_) => {
                        self.errors
                            .push(error_at("integer literal out of range", tok.span));
                        Expr::Int {
                            value: 0,
                            span: tok.span,
                        }
                    }
                }
            }
            TokenKind::Float => {
                self.advance();
                let cleaned = tok.text.replace('_', "");
                let value = cleaned.parse::<f64>().unwrap_or(f64::NAN);
                Expr::Float {
                    value,
                    span: tok.span,
                }
            }
            TokenKind::Ident => {
                self.advance();
                Expr::Ident {
                    name: tok.text,
                    span: tok.span,
                }
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_expr();
                let end = if self.eat(TokenKind::RParen) {
                    self.previous().span
                } else {
                    self.error_here("expected `)` to close parenthesized expression");
                    inner.span()
                };
                let span = tok.span.join(end);
                Expr::Paren {
                    expr: Box::new(inner),
                    span,
                }
            }
            _ => {
                self.error_here(format!(
                    "expected expression, found {}",
                    tok.kind.describe()
                ));
                dummy_expr(tok.span)
            }
        }
    }

    // --- helpers ----------------------------------------------------------

    fn expect_ident(&mut self) -> Option<Ident> {
        if self.at(TokenKind::Ident) {
            let tok = self.advance();
            Some(Ident {
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

    fn error_here(&mut self, msg: impl Into<String>) -> Span {
        let span = self.peek().span;
        self.errors.push(error_at(msg, span));
        span
    }

    fn skip_stmt_ends(&mut self) {
        while self.at(TokenKind::StmtEnd) {
            self.advance();
        }
    }

    fn skip_to_stmt_end(&mut self) {
        while !self.at(TokenKind::StmtEnd) && !self.at(TokenKind::Eof) {
            self.advance();
        }
    }

    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn peek_kind(&self) -> TokenKind {
        self.toks[self.pos].kind
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek_kind() == kind
    }

    fn advance(&mut self) -> Token {
        let tok = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        tok
    }

    fn previous(&self) -> &Token {
        &self.toks[self.pos.saturating_sub(1)]
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }
}

fn dummy_expr(span: Span) -> Expr {
    Expr::Int { value: 0, span }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BinOp::*;
    use crate::ast::Expr as E;

    fn parse_ok(src: &str) -> Program {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
        parsed.program
    }

    #[test]
    fn parses_let_binding() {
        let p = parse_ok("let x = 1 + 2");
        assert_eq!(p.stmts.len(), 1);
        match &p.stmts[0] {
            Stmt::Let { name, value, .. } => {
                assert_eq!(name.name, "x");
                assert!(matches!(value, E::Binary { op: Add, .. }));
            }
            other => panic!("expected let, got {other:?}"),
        }
    }

    #[test]
    fn precedence_multiplication_over_addition() {
        let p = parse_ok("1 + 2 * 3");
        match &p.stmts[0] {
            Stmt::Expr(E::Binary {
                op: Add,
                left,
                right,
                ..
            }) => {
                assert!(matches!(**left, E::Int { value: 1, .. }));
                assert!(matches!(**right, E::Binary { op: Mul, .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parens_override_precedence() {
        let p = parse_ok("(1 + 2) * 3");
        match &p.stmts[0] {
            Stmt::Expr(E::Binary { op: Mul, left, .. }) => {
                assert!(matches!(**left, E::Paren { .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unary_minus() {
        let p = parse_ok("-5");
        match &p.stmts[0] {
            Stmt::Expr(E::Unary { op: UnOp::Neg, .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn multiple_statements() {
        let p = parse_ok("let a = 1\nlet b = 2\nlet c = a + b");
        assert_eq!(p.stmts.len(), 3);
    }

    #[test]
    fn missing_equals_reports_error_and_recovers() {
        let parsed = parse("let x 1\nlet y = 2");
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.program.stmts.len(), 2);
    }

    #[test]
    fn missing_expression_reports_error() {
        let parsed = parse("let x =");
        assert_eq!(parsed.errors.len(), 1);
    }

    #[test]
    fn missing_close_paren_reports_error() {
        let parsed = parse("(1 + 2");
        assert_eq!(parsed.errors.len(), 1);
    }

    #[test]
    fn missing_stmt_end_reports_error() {
        let parsed = parse("let x = 1 let y = 2");
        assert_eq!(parsed.errors.len(), 1);
    }

    #[test]
    fn empty_program_ok() {
        let p = parse_ok("");
        assert!(p.stmts.is_empty());
    }
}
