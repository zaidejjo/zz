//! Statement parsing.

use crate::ast::{Block, Expr, Ident, Param, Program, Stmt, Ty};
use crate::diag::{error_at, RawDiag};
use crate::span::Span;
use crate::token::{Token, TokenKind};

use super::Parser;

impl Parser {
    // --- statements -------------------------------------------------------

    pub(crate) fn parse_stmt_list(&mut self, term: TokenKind) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        loop {
            self.skip_stmt_ends();
            if self.at(term) || self.at(TokenKind::Eof) {
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
            } else if !self.at(TokenKind::StmtEnd) && !self.at(term) && !self.at(TokenKind::Eof) {
                // Two statements with no terminator between them.
                self.error_here(format!(
                    "expected end of statement, found {}",
                    self.peek_kind().describe()
                ));
                self.skip_to_stmt_end();
            }
            stmts.push(stmt);
        }
        stmts
    }

    pub(crate) fn parse_stmt(&mut self) -> Stmt {
        match self.peek_kind() {
            TokenKind::Import => self.parse_import(),
            TokenKind::Func => self.parse_func(),
            TokenKind::Return => {
                let ret_tok = self.advance();
                let value = if self.at(TokenKind::StmtEnd)
                    || self.at(TokenKind::Eof)
                    || self.at(TokenKind::RBrace)
                {
                    None
                } else {
                    Some(self.parse_expr())
                };
                let span = ret_tok
                    .span
                    .join(value.as_ref().map(|e| e.span()).unwrap_or(ret_tok.span));
                Stmt::Return { value, span }
            }
            // `x := expr` — short declaration with inference.
            TokenKind::Ident if self.peek_kind_at(1) == TokenKind::ColonEq => {
                self.parse_short_decl()
            }
            TokenKind::Struct => self.parse_struct(),
            TokenKind::For => self.parse_for(),
            TokenKind::Break => {
                let tok = self.advance();
                Stmt::Break { span: tok.span }
            }
            TokenKind::Continue => {
                let tok = self.advance();
                Stmt::Continue { span: tok.span }
            }
            TokenKind::Defer => {
                let tok = self.advance();
                let expr = self.parse_expr();
                let span = tok.span.join(expr.span());
                Stmt::Defer {
                    expr: Box::new(expr),
                    span,
                }
            }
            _ => {
                // Try `IDENT: TYPE = expr` (explicit declaration). Backtrack
                // on failure so ordinary expressions still parse.
                let save_pos = self.pos;
                let save_errs = self.errors.len();
                if let Some(decl) = self.try_parse_explicit_decl() {
                    return decl;
                }
                self.pos = save_pos;
                self.errors.truncate(save_errs);

                // Recovery: `TYPE IDENT := expr` is the OLD syntax which is
                // no longer valid (now `IDENT: TYPE = expr`). Detect the
                // pattern and produce a usable Decl instead of degrading to
                // `Stmt::Expr(Ident("int"))` which the formatter would garble.
                if self.peek_kind_at(0) == TokenKind::Ident
                    && !matches!(self.peek().text.as_str(), "true" | "false")
                    && self.peek_kind_at(1) == TokenKind::Ident
                    && self.peek_kind_at(2) == TokenKind::ColonEq
                {
                    let type_tok = self.peek().clone();
                    let type_name = type_tok.text.clone();
                    let is_type_kw = matches!(
                        type_name.as_str(),
                        "int" | "float" | "bool" | "str" | "Option" | "Result"
                    );
                    if is_type_kw {
                        let ty = self.parse_type();
                        let name = self.advance(); // identifier
                        self.advance(); // `:=`
                        let value = self.parse_expr();
                        let span = ty.span.join(value.span());
                        self.errors.push(error_at(
                            format!(
                                "invalid declaration: use `{}: {} = expr` (explicit type) or `{} := expr` (inferred type)",
                                name.text, type_name, name.text,
                            ),
                            span,
                        ));
                        return Stmt::Decl {
                            ty: Some(ty),
                            name: Ident {
                                name: name.text,
                                span: name.span,
                            },
                            value,
                            span,
                        };
                    }
                }

                let expr = self.parse_expr();
                // `expr = expr` — assignment statement.
                if self.at(TokenKind::Assign) {
                    self.advance();
                    let value = self.parse_expr();
                    let span = expr.span().join(value.span());
                    return Stmt::Assign {
                        target: expr,
                        value,
                        span,
                    };
                }
                Stmt::Expr(expr)
            }
        }
    }

    pub(crate) fn parse_struct(&mut self) -> Stmt {
        let struct_tok = self.advance();
        let name = self.parse_dotted_ident();
        if !self.eat(TokenKind::LBrace) {
            self.error_here("expected `{` to start struct body");
            // Recovery: skip to the closing brace so the field loop below
            // terminates instead of spinning on an unconsumable token.
            self.skip_to_rbrace();
        }
        let mut fields = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let start_pos = self.pos;
            self.skip_stmt_ends();
            if self.at(TokenKind::RBrace) {
                break;
            }
            let fname = self
                .expect_ident()
                .unwrap_or_else(|| dummy_ident(self.peek().span));
            if !self.eat(TokenKind::Colon) {
                self.error_here("expected `:` after field name");
            }
            let fty = self.parse_type();
            fields.push((fname, fty));
            if self.eat(TokenKind::Comma) {
                continue;
            }
            self.skip_stmt_ends();
            if !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                self.error_here("expected `,` or `}` after field");
            }
            // Progress guard: if an error path failed to consume anything,
            // force-advance so recovery always terminates.
            if self.pos == start_pos {
                self.advance();
            }
        }
        let end = if self.eat(TokenKind::RBrace) {
            self.previous().span
        } else {
            self.error_here("expected `}` to close struct body");
            self.peek().span
        };
        let span = struct_tok.span.join(end);
        Stmt::Struct { name, fields, span }
    }

    pub(crate) fn parse_for(&mut self) -> Stmt {
        let for_tok = self.advance();
        let first = self
            .expect_ident()
            .unwrap_or_else(|| dummy_ident(for_tok.span));
        let mut vars = vec![first];
        // for k, v in dict
        while self.eat(TokenKind::Comma) {
            let v = self
                .expect_ident()
                .unwrap_or_else(|| dummy_ident(for_tok.span));
            vars.push(v);
        }
        if !self.eat(TokenKind::In) {
            self.error_here("expected `in` after loop variable");
        }
        let iter = self.parse_expr();
        let body = self.parse_block();
        let span = for_tok.span.join(body.span);
        Stmt::For {
            vars,
            iter: Box::new(iter),
            body,
            span,
        }
    }

    pub(crate) fn parse_import(&mut self) -> Stmt {
        let import_tok = self.advance();
        let mut path = Vec::new();
        if let Some(id) = self.expect_ident() {
            path.push(id.name);
        }
        while self.eat(TokenKind::Dot) {
            if let Some(id) = self.expect_ident() {
                path.push(id.name);
            }
        }
        let alias = if self.eat(TokenKind::As) {
            self.expect_ident().map(|id| id.name)
        } else {
            None
        };
        let span = import_tok.span.join(self.previous().span);
        Stmt::Import { path, alias, span }
    }

    pub(crate) fn parse_short_decl(&mut self) -> Stmt {
        let name = self.advance(); // identifier
        self.advance(); // `:=`
        let value = self.parse_expr();
        let span = name.span.join(value.span());
        Stmt::Decl {
            ty: None,
            name: Ident {
                name: name.text,
                span: name.span,
            },
            value,
            span,
        }
    }

    /// Parse `IDENT: TYPE = expr`; returns `None` (with position restored by
    /// the caller) when the statement is not an explicit declaration.
    pub(crate) fn try_parse_explicit_decl(&mut self) -> Option<Stmt> {
        // Must start with an identifier.
        if !self.at(TokenKind::Ident) {
            return None;
        }
        let name = self.advance();
        // Then a colon.
        if !self.eat(TokenKind::Colon) {
            return None;
        }
        // Then a type.
        let ty = self.parse_type();
        // Then `=`.
        if !self.eat(TokenKind::Assign) {
            return None;
        }
        let value = self.parse_expr();
        let span = name.span.join(value.span());
        Some(Stmt::Decl {
            ty: Some(ty),
            name: Ident {
                name: name.text,
                span: name.span,
            },
            value,
            span,
        })
    }

    pub(crate) fn parse_func(&mut self) -> Stmt {
        let func_tok = self.advance();
        let name = self.parse_dotted_ident();
        let generics = if self.eat(TokenKind::Lt) {
            let mut gs = Vec::new();
            loop {
                if let Some(id) = self.expect_ident() {
                    gs.push(id);
                }
                if self.eat(TokenKind::Comma) {
                    continue;
                }
                break;
            }
            if !self.eat(TokenKind::Gt) {
                self.error_here("expected `>` to close generic parameters");
            }
            gs
        } else {
            Vec::new()
        };
        if !self.eat(TokenKind::LParen) {
            self.error_here("expected `(` after function name");
        } else {
            self.push_delim(TokenKind::LParen, self.previous().span);
        }
        let params = self.parse_param_list();
        if !self.eat(TokenKind::RParen) {
            self.error_here("expected `)` after parameters");
        } else {
            self.pop_delim(TokenKind::RParen, self.previous().span);
        }
        let ret = if self.eat(TokenKind::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };
        let body = self.parse_block();
        let span = func_tok.span.join(body.span);
        Stmt::Func {
            name,
            generics,
            params,
            ret,
            body,
            span,
        }
    }

    pub(crate) fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        if self.at(TokenKind::RParen) || self.at(TokenKind::Pipe) {
            return params;
        }
        loop {
            let name = self
                .expect_ident()
                .unwrap_or_else(|| dummy_ident(self.peek().span));
            let ty = if self.eat(TokenKind::Colon) {
                Some(self.parse_type())
            } else {
                None
            };
            // Default parameter value: `param: type = expr` or `param = expr`.
            let default = if self.eat(TokenKind::Assign) {
                Some(Box::new(self.parse_expr()))
            } else {
                None
            };
            let mut span = name
                .span
                .join(ty.as_ref().map(|t| t.span).unwrap_or(name.span));
            if let Some(ref d) = default {
                span = span.join(d.span());
            }
            params.push(Param {
                name,
                ty,
                default,
                span,
            });
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        params
    }

    pub(crate) fn parse_block(&mut self) -> Block {
        let lbrace = self.peek().clone();
        self.skip_stmt_ends();
        if !self.eat(TokenKind::LBrace) {
            self.error_here("expected `{` to start block");
        } else {
            self.push_delim(TokenKind::LBrace, lbrace.span);
        }
        let stmts = self.parse_stmt_list(TokenKind::RBrace);
        let end = if self.eat(TokenKind::RBrace) {
            let rbrace = self.previous().span;
            self.pop_delim(TokenKind::RBrace, rbrace);
            rbrace
        } else {
            self.peek().span
        };
        Block {
            stmts,
            span: lbrace.span.join(end),
        }
    }
}

fn dummy_ident(span: Span) -> Ident {
    Ident {
        name: String::new(),
        span,
    }
}
