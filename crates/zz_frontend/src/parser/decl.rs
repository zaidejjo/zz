//! Type declaration parsing.

use crate::ast::{Ty, TyKind};
use crate::diag::error_at;
use crate::span::Span;
use crate::token::TokenKind;

use super::Parser;

impl Parser {
    // --- types ------------------------------------------------------------

    pub(crate) fn parse_type(&mut self) -> Ty {
        let first = self.parse_type_base();
        if !self.at(TokenKind::Pipe) {
            return first;
        }
        let mut members = vec![first];
        while self.eat(TokenKind::Pipe) {
            members.push(self.parse_type_base());
        }
        let span = members
            .first()
            .unwrap()
            .span
            .join(members.last().unwrap().span);
        Ty {
            kind: TyKind::Union(members),
            span,
        }
    }

    pub(crate) fn parse_type_base(&mut self) -> Ty {
        let tok = self.peek().clone();
        // `*const T` / `*mut T` / `*T` — raw C pointer. Bare `*T` means `*const T`.
        if tok.kind == TokenKind::Star {
            self.advance();
            let mutable = if self.at(TokenKind::Const) {
                self.advance();
                false
            } else if self.at(TokenKind::Mut) {
                self.advance();
                true
            } else {
                false
            };
            let inner = self.parse_type_base();
            let span = tok.span.join(inner.span);
            return Ty {
                kind: TyKind::Ptr {
                    mutable,
                    inner: Box::new(inner),
                },
                span,
            };
        }
        match tok.kind {
            // func(int) -> int  — function type keyword
            TokenKind::Func => {
                self.advance();
                if !self.eat(TokenKind::LParen) {
                    self.error_here("expected `(` after `func` in function type");
                } else {
                    self.push_delim(TokenKind::LParen, self.previous().span);
                }
                let params = if self.at(TokenKind::RParen) {
                    Vec::new()
                } else {
                    let mut ts = vec![self.parse_type()];
                    while self.eat(TokenKind::Comma) {
                        ts.push(self.parse_type());
                    }
                    ts
                };
                if !self.eat(TokenKind::RParen) {
                    self.error_here("expected `)` to close function type parameters");
                } else {
                    self.pop_delim(TokenKind::RParen, self.previous().span);
                }
                if !self.eat(TokenKind::Arrow) {
                    self.error_here("expected `->` in function type");
                }
                let ret = self.parse_type();
                let span = tok.span.join(ret.span);
                Ty {
                    kind: TyKind::Func(params, Box::new(ret)),
                    span,
                }
            }
            TokenKind::Ident => {
                self.advance();
                // Consume dotted type names: `shapes.Point`, `a.b.c`, etc.
                let mut full_name = tok.text.clone();
                let mut end_span = tok.span;
                while self.eat(TokenKind::Dot) {
                    if let Some(id) = self.expect_ident() {
                        full_name.push('.');
                        full_name.push_str(&id.name);
                        end_span = id.span;
                    } else {
                        break;
                    }
                }
                let args = if self.eat(TokenKind::Lt) {
                    let mut ts = vec![self.parse_type()];
                    while self.eat(TokenKind::Comma) {
                        ts.push(self.parse_type());
                    }
                    if !self.eat(TokenKind::Gt) {
                        self.error_here("expected `>` to close type arguments");
                    }
                    ts
                } else {
                    Vec::new()
                };
                let kind = match full_name.as_str() {
                    "int" => {
                        if !args.is_empty() {
                            self.errors.push(error_at(
                                "type `int` does not take type arguments",
                                tok.span,
                            ));
                        }
                        TyKind::Int
                    }
                    "float" => {
                        if !args.is_empty() {
                            self.errors.push(error_at(
                                "type `float` does not take type arguments",
                                tok.span,
                            ));
                        }
                        TyKind::Float
                    }
                    "bool" => {
                        if !args.is_empty() {
                            self.errors.push(error_at(
                                "type `bool` does not take type arguments",
                                tok.span,
                            ));
                        }
                        TyKind::Bool
                    }
                    "str" => {
                        if !args.is_empty() {
                            self.errors.push(error_at(
                                "type `str` does not take type arguments",
                                tok.span,
                            ));
                        }
                        TyKind::Str
                    }
                    "unit" => {
                        if !args.is_empty() {
                            self.errors.push(error_at(
                                "type `unit` does not take type arguments",
                                tok.span,
                            ));
                        }
                        TyKind::Unit
                    }
                    "void" => {
                        if !args.is_empty() {
                            self.errors.push(error_at(
                                "type `void` does not take type arguments",
                                tok.span,
                            ));
                        }
                        TyKind::Void
                    }
                    "Option" => match args.len() {
                        1 => TyKind::Option(Box::new(args.into_iter().next().unwrap())),
                        _ => {
                            self.errors.push(error_at(
                                "`Option` requires exactly one type argument",
                                tok.span,
                            ));
                            TyKind::Option(Box::new(Ty {
                                kind: TyKind::Unit,
                                span: tok.span,
                            }))
                        }
                    },
                    "Result" => {
                        let mut it = args.into_iter();
                        match (it.next(), it.next()) {
                            (Some(t), Some(e)) => TyKind::Result(Box::new(t), Box::new(e)),
                            _ => {
                                self.errors.push(error_at(
                                    "`Result` requires two type arguments",
                                    tok.span,
                                ));
                                TyKind::Result(
                                    Box::new(Ty {
                                        kind: TyKind::Unit,
                                        span: tok.span,
                                    }),
                                    Box::new(Ty {
                                        kind: TyKind::Unit,
                                        span: tok.span,
                                    }),
                                )
                            }
                        }
                    }
                    _ => TyKind::Named(full_name, args),
                };
                Ty {
                    kind,
                    span: tok.span.join(end_span),
                }
            }
            TokenKind::LBracket => {
                self.advance();
                let inner = self.parse_type();
                let end = if self.eat_close(TokenKind::RBracket) {
                    self.previous().span
                } else {
                    self.error_here("expected `]` to close array type");
                    tok.span
                };
                Ty {
                    kind: TyKind::Array(Box::new(inner)),
                    span: tok.span.join(end),
                }
            }
            TokenKind::LBrace => {
                self.advance();
                let key = self.parse_type();
                if !self.eat(TokenKind::Colon) {
                    self.error_here("expected `:` in dict type");
                }
                let value = self.parse_type();
                let end = if self.eat_close(TokenKind::RBrace) {
                    self.previous().span
                } else {
                    self.error_here("expected `}` to close dict type");
                    tok.span
                };
                Ty {
                    kind: TyKind::Dict(Box::new(key), Box::new(value)),
                    span: tok.span.join(end),
                }
            }
            TokenKind::LParen => {
                self.advance();
                if self.eat_close(TokenKind::RParen) {
                    return Ty {
                        kind: TyKind::Unit,
                        span: tok.span,
                    };
                }
                let first = self.parse_type();
                let first_span = first.span;
                if self.eat(TokenKind::Comma) {
                    let mut ts = vec![first];
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        ts.push(self.parse_type());
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    // Save position before consuming `)` to detect `(A, B) -> Ret`.
                    let save_pos = self.pos;
                    if self.eat_close(TokenKind::RParen) {
                        if self.eat(TokenKind::Arrow) {
                            let ret = self.parse_type();
                            let span = tok.span.join(ret.span);
                            Ty {
                                kind: TyKind::Func(ts, Box::new(ret)),
                                span,
                            }
                        } else {
                            // Not a function type — restore and re-consume `)`.
                            self.pos = save_pos;
                            self.eat_close(TokenKind::RParen);
                            Ty {
                                kind: TyKind::Tuple(ts),
                                span: tok.span.join(self.previous().span),
                            }
                        }
                    } else {
                        self.error_here("expected `)` to close tuple type");
                        Ty {
                            kind: TyKind::Tuple(ts),
                            span: tok.span.join(first_span),
                        }
                    }
                } else {
                    // Save position before consuming `)` to detect `(A) -> Ret`.
                    let save_pos = self.pos;
                    if self.eat_close(TokenKind::RParen) {
                        if self.eat(TokenKind::Arrow) {
                            let ret = self.parse_type();
                            let span = tok.span.join(ret.span);
                            Ty {
                                kind: TyKind::Func(vec![first], Box::new(ret)),
                                span,
                            }
                        } else {
                            // Not a function type — restore and re-consume `)`.
                            self.pos = save_pos;
                            self.eat_close(TokenKind::RParen);
                            Ty {
                                kind: first.kind,
                                span: tok.span.join(self.previous().span),
                            }
                        }
                    } else {
                        self.error_here("expected `)` to close type");
                        Ty {
                            kind: first.kind,
                            span: tok.span.join(first_span),
                        }
                    }
                }
            }
            _ => {
                self.error_here(format!("expected type, found {}", tok.kind.describe()));
                Ty {
                    kind: TyKind::Unit,
                    span: tok.span,
                }
            }
        }
    }

    /// Parse a dotted identifier: `a`, `a.b`, `a.b.c`, ...
    /// Returns `Vec<String>` of parts.
    pub(crate) fn parse_dotted_ident(&mut self) -> Vec<String> {
        let first = self
            .expect_ident()
            .unwrap_or_else(|| dummy_ident(self.peek().span));
        let mut parts = vec![first.name];
        while self.eat(TokenKind::Dot) {
            match self.expect_ident() {
                Some(id) => parts.push(id.name),
                None => break,
            }
        }
        parts
    }
}

fn dummy_ident(span: Span) -> crate::ast::Ident {
    crate::ast::Ident {
        name: String::new(),
        span,
    }
}
