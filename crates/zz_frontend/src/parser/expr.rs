//! Expression parsing.

use crate::ast::{BinOp, Expr, FmtPart, Ident, Lit, MatchArm, Param, Pattern, Ty, UnOp};
use crate::diag::{error_at, RawDiag};
use crate::span::Span;
use crate::token::{Token, TokenKind};

use super::Parser;

impl Parser {
    // --- expressions ------------------------------------------------------

    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_pipe()
    }

    /// `a |> f(b)` — pipeline. Lowest precedence. The right side must be a
    /// function call or name; it receives the left side as its first
    /// argument (`a |> f(b)` desugars to `f(a, b)`).
    pub(crate) fn parse_pipe(&mut self) -> Expr {
        let mut left = self.parse_range();
        while self.at(TokenKind::PipeGt) {
            self.advance();
            let rhs = self.parse_range();
            left = self.desugar_pipe(left, rhs);
        }
        left
    }

    pub(crate) fn desugar_pipe(&mut self, lhs: Expr, rhs: Expr) -> Expr {
        let span = lhs.span().join(rhs.span());
        match rhs {
            Expr::Call {
                callee,
                mut args,
                named,
                ..
            } => {
                args.insert(0, lhs);
                Expr::Call {
                    callee,
                    args,
                    named,
                    span,
                }
            }
            Expr::Ident { name, span } => Expr::Call {
                callee: Box::new(Expr::Ident { name, span }),
                args: vec![lhs],
                named: vec![],
                span,
            },
            Expr::Path { parts, span } => Expr::Call {
                callee: Box::new(Expr::Path { parts, span }),
                args: vec![lhs],
                named: vec![],
                span,
            },
            other => {
                self.error_here("right side of `|>` must be a function call or name");
                other
            }
        }
    }

    /// `a..b` — integer range. Lowest precedence so `for i in 0..n` parses
    /// the bounds as full expressions.
    pub(crate) fn parse_range(&mut self) -> Expr {
        let start = self.parse_elvis();
        if !self.at(TokenKind::DotDot) {
            return start;
        }
        self.advance();
        let end = self.parse_elvis();
        let span = start.span().join(end.span());
        Expr::Range {
            start: Box::new(start),
            end: Box::new(end),
            span,
        }
    }

    /// `left ?? right` — Elvis operator. Unwraps Option or falls back.
    pub(crate) fn parse_elvis(&mut self) -> Expr {
        let mut left = self.parse_or();
        while self.at(TokenKind::QuestionQuestion) {
            self.advance();
            let right = self.parse_or();
            let span = left.span().join(right.span());
            left = Expr::Binary {
                op: BinOp::Elvis,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        left
    }

    pub(crate) fn parse_or(&mut self) -> Expr {
        let mut left = self.parse_and();
        while self.at(TokenKind::OrOr) {
            self.advance();
            let right = self.parse_and();
            let span = left.span().join(right.span());
            left = Expr::Binary {
                op: BinOp::Or,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        left
    }

    pub(crate) fn parse_and(&mut self) -> Expr {
        let mut left = self.parse_equality();
        while self.at(TokenKind::AndAnd) {
            self.advance();
            let right = self.parse_equality();
            let span = left.span().join(right.span());
            left = Expr::Binary {
                op: BinOp::And,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        left
    }

    pub(crate) fn parse_equality(&mut self) -> Expr {
        let mut left = self.parse_relational();
        loop {
            let op = match self.peek_kind() {
                TokenKind::Eq => BinOp::Eq,
                TokenKind::Ne => BinOp::Ne,
                _ => break,
            };
            self.advance();
            let right = self.parse_relational();
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

    pub(crate) fn parse_relational(&mut self) -> Expr {
        let mut left = self.parse_additive();
        loop {
            let op = match self.peek_kind() {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_additive();
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

    pub(crate) fn parse_additive(&mut self) -> Expr {
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

    pub(crate) fn parse_power(&mut self) -> Expr {
        let base = self.parse_unary();
        if self.at(TokenKind::StarStar) {
            self.advance();
            let exp = self.parse_power(); // right-associative
            let span = base.span().join(exp.span());
            Expr::Binary {
                op: BinOp::Pow,
                left: Box::new(base),
                right: Box::new(exp),
                span,
            }
        } else {
            base
        }
    }

    pub(crate) fn parse_multiplicative(&mut self) -> Expr {
        let mut left = self.parse_power();
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

    pub(crate) fn parse_unary(&mut self) -> Expr {
        let op = match self.peek_kind() {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::Plus => UnOp::Pos,
            TokenKind::Bang => UnOp::Not,
            _ => return self.parse_postfix(),
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

    pub(crate) fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            match self.peek_kind() {
                TokenKind::LParen => {
                    self.advance();
                    let (args, named) = self.parse_call_args();
                    let end = if self.eat_close(TokenKind::RParen) {
                        self.previous().span
                    } else {
                        self.error_here("expected `)` to close call");
                        expr.span()
                    };
                    let span = expr.span().join(end);
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        named,
                        span,
                    };
                }
                TokenKind::Question => {
                    let q = self.advance();
                    let span = expr.span().join(q.span);
                    expr = Expr::Try {
                        expr: Box::new(expr),
                        span,
                    };
                }
                // Field access on a non-trivial base: `makePoint().x`. Pure
                // identifier chains are consumed as `Path` in `parse_primary`.
                TokenKind::Dot if self.peek_kind_at(1) == TokenKind::Ident => {
                    self.advance(); // `.`
                    let member = self.advance(); // identifier
                    let span = expr.span().join(member.span);
                    expr = Expr::Field {
                        obj: Box::new(expr),
                        name: member.text,
                        span,
                    };
                }
                // Indexing and slicing: `arr[0]`, `dict["k"]`, `s[1:3]`.
                TokenKind::LBracket => {
                    self.advance(); // `[`
                    let mut start = None;
                    let mut end = None;
                    let mut index = None;
                    let mut is_slice = false;
                    if self.at(TokenKind::Colon) {
                        // `[:end]` — start omitted.
                        is_slice = true;
                        self.advance();
                        if !self.at(TokenKind::RBracket) {
                            end = Some(Box::new(self.parse_expr()));
                        }
                    } else {
                        let first = self.parse_expr();
                        if self.eat(TokenKind::Colon) {
                            // `[start:end]` or `[start:]`.
                            is_slice = true;
                            start = Some(Box::new(first));
                            if !self.at(TokenKind::RBracket) {
                                end = Some(Box::new(self.parse_expr()));
                            }
                        } else {
                            index = Some(first);
                        }
                    }
                    let close = if self.eat_close(TokenKind::RBracket) {
                        self.previous().span
                    } else {
                        self.error_here("expected `]` to close index");
                        expr.span()
                    };
                    let span = expr.span().join(close);
                    expr = if is_slice {
                        Expr::Slice {
                            obj: Box::new(expr),
                            start,
                            end,
                            span,
                        }
                    } else {
                        Expr::Index {
                            obj: Box::new(expr),
                            index: Box::new(index.unwrap()),
                            span,
                        }
                    };
                }
                _ => break,
            }
        }
        expr
    }

    pub(crate) fn parse_expr_list(&mut self) -> Vec<Expr> {
        let mut args = Vec::new();
        if self.at(TokenKind::RParen) {
            return args;
        }
        loop {
            args.push(self.parse_expr());
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        args
    }

    /// Parse call arguments: `(pos1, pos2, name1: val1, name2: val2)`.
    /// Returns `(positional_args, named_args)`.
    pub(crate) fn parse_call_args(&mut self) -> (Vec<Expr>, Vec<(String, Expr)>) {
        let mut args = Vec::new();
        let mut named = Vec::new();
        if self.at(TokenKind::RParen) {
            return (args, named);
        }
        loop {
            // Peek for `IDENT : expr` pattern (named argument).
            // Only treat as named arg if current token is ident AND next is colon.
            if self.at(TokenKind::Ident) && self.peek_kind_at(1) == TokenKind::Colon {
                let name = self.advance().text;
                self.advance(); // consume `:`
                let value = self.parse_expr();
                named.push((name, value));
            } else {
                args.push(self.parse_expr());
            }
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        (args, named)
    }

    /// Assemble an interpolated string from the token sequence produced by
    /// the lexer: `StrFmt (LBrace expr [: fmt_spec] RBrace StrFmt)* [Str]`.
    pub(crate) fn parse_fmt_string(&mut self, first: Token) -> Expr {
        let mut parts = vec![FmtPart::Text(first.text)];
        let mut end = first.span;
        loop {
            if !self.at(TokenKind::LBrace) {
                break;
            }
            self.advance();
            let expr = self.parse_expr();
            // Optional format spec: `{val:.2f}`, `{val:x}`, etc.
            let fmt_spec = if self.eat(TokenKind::Colon) {
                // Consume everything up to `}` as the format spec.
                // The spec is a simple identifier or dot-prefixed like `.2f`.
                let mut spec = String::new();
                if self.at(TokenKind::Dot) {
                    spec.push('.');
                    self.advance();
                    // Consume digits after the dot.
                    while self.at(TokenKind::Int) {
                        let t = self.advance();
                        spec.push_str(&t.text);
                    }
                    // Consume trailing letter like 'f', 'e', etc.
                    if self.at(TokenKind::Ident) {
                        let t = self.advance();
                        spec.push_str(&t.text);
                    }
                } else if self.at(TokenKind::Ident) {
                    let t = self.advance();
                    spec.push_str(&t.text);
                }
                Some(spec)
            } else {
                None
            };
            if !self.eat(TokenKind::RBrace) {
                self.error_here("expected `}` to close interpolation");
            }
            parts.push(FmtPart::Expr(Box::new(expr), fmt_spec));
            // After `}`, the next token is either StrFmt (more interpolation
            // segments follow) or Str (final segment, end of string).
            match self.peek_kind() {
                TokenKind::StrFmt => {
                    let t = self.advance();
                    parts.push(FmtPart::Text(t.text));
                    end = t.span;
                }
                TokenKind::Str => {
                    let t = self.advance();
                    parts.push(FmtPart::Text(t.text));
                    end = t.span;
                    break;
                }
                _ => {
                    self.error_here("expected string continuation after interpolation");
                    break;
                }
            }
        }
        Expr::Fmt {
            parts,
            span: first.span.join(end),
        }
    }

    pub(crate) fn parse_primary(&mut self) -> Expr {
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
            TokenKind::Str => {
                self.advance();
                Expr::Str {
                    value: tok.text,
                    span: tok.span,
                }
            }
            TokenKind::StrFmt => {
                self.advance();
                self.parse_fmt_string(tok)
            }
            TokenKind::True => {
                self.advance();
                Expr::Bool {
                    value: true,
                    span: tok.span,
                }
            }
            TokenKind::False => {
                self.advance();
                Expr::Bool {
                    value: false,
                    span: tok.span,
                }
            }
            TokenKind::Ident if tok.text == "_" => {
                self.errors
                    .push(error_at("`_` is only valid in patterns", tok.span));
                self.advance();
                dummy_expr(tok.span)
            }
            TokenKind::Ident => {
                self.advance();
                let mut parts = vec![tok.text];
                let mut end = tok.span;
                // Member access: `a.b.c` becomes a dotted path. Only chains
                // of identifiers are supported (no arbitrary member access).
                while self.at(TokenKind::Dot) && self.peek_kind_at(1) == TokenKind::Ident {
                    self.advance(); // `.`
                    let member = self.advance(); // identifier
                    parts.push(member.text);
                    end = member.span;
                }
                // Struct construction: `Point{ x: 1 }` or `Point { x: 1 }`.
                // A `{` is treated as a struct literal when its contents look
                // like fields (`ident : ...`). Adjacency alone is not enough
                // because `if x == y{ ... }` would be misparsed as struct init.
                if self.at(TokenKind::LBrace)
                    && self.peek_kind_at(1) == TokenKind::Ident
                    && self.peek_kind_at(2) == TokenKind::Colon
                {
                    return self.parse_struct_init(parts, tok.span.join(end));
                }
                if parts.len() == 1 {
                    Expr::Ident {
                        name: parts.pop().unwrap(),
                        span: tok.span,
                    }
                } else {
                    Expr::Path {
                        parts,
                        span: tok.span.join(end),
                    }
                }
            }
            TokenKind::LParen => {
                let lparen = self.advance();
                self.push_delim(TokenKind::LParen, lparen.span);
                let inner = self.parse_expr();
                let end = if self.eat_close(TokenKind::RParen) {
                    let rparen = self.previous().span;
                    self.pop_delim(TokenKind::RParen, rparen);
                    rparen
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
            TokenKind::LBrace => self.parse_dict_or_block(),
            TokenKind::LBracket => self.parse_array_literal(),
            TokenKind::Pipe => self.parse_closure(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => {
                let w = self.advance();
                let cond = self.parse_expr();
                let body = self.parse_block();
                let span = w.span.join(body.span);
                Expr::While {
                    cond: Box::new(cond),
                    body,
                    span,
                }
            }
            TokenKind::Match => self.parse_match(),
            TokenKind::Dot => self.parse_variant(),
            _ => {
                self.error_here(format!(
                    "expected expression, found {}",
                    tok.kind.describe()
                ));
                dummy_expr(tok.span)
            }
        }
    }

    pub(crate) fn parse_struct_init(&mut self, parts: Vec<String>, start: Span) -> Expr {
        self.advance(); // `{`
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
            let value = self.parse_expr();
            fields.push((fname.name, value));
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
            self.error_here("expected `}` to close struct literal");
            self.peek().span
        };
        Expr::StructInit {
            name: parts.join("."),
            fields,
            span: start.join(end),
        }
    }

    pub(crate) fn parse_closure(&mut self) -> Expr {
        let pipe_tok = self.advance(); // `|`
        let mut params = Vec::new();
        if !self.at(TokenKind::Pipe) {
            loop {
                let name = self
                    .expect_ident()
                    .unwrap_or_else(|| dummy_ident(self.peek().span));
                // Base type only: `|` closes the parameter list, so unions
                // are not allowed in closure parameter types.
                let ty = if self.eat(TokenKind::Colon) {
                    Some(self.parse_type_base())
                } else {
                    None
                };
                let span = name
                    .span
                    .join(ty.as_ref().map(|t| t.span).unwrap_or(name.span));
                params.push(Param {
                    name,
                    ty,
                    default: None,
                    span,
                });

                if self.eat(TokenKind::Comma) {
                    continue;
                }
                break;
            }
        }
        if !self.eat(TokenKind::Pipe) {
            self.error_here("expected `|` to close closure parameters");
        }
        let body = self.parse_expr();
        let span = pipe_tok.span.join(body.span());
        Expr::Closure {
            params,
            body: Box::new(body),
            span,
        }
    }

    pub(crate) fn parse_dict_or_block(&mut self) -> Expr {
        let save_pos = self.pos;
        let save_errs = self.errors.len();
        if let Some(dict) = self.try_parse_dict() {
            return dict;
        }
        self.pos = save_pos;
        self.errors.truncate(save_errs);
        let block = self.parse_block();
        Expr::Block(block)
    }

    /// Parse a dict literal `{ key: value, ... }`. Returns `None` (leaving
    /// the position at the `{`) when the braces are actually a block.
    pub(crate) fn try_parse_dict(&mut self) -> Option<Expr> {
        let lbrace = self.peek().clone();
        if !self.eat(TokenKind::LBrace) {
            return None;
        }
        let mut entries = Vec::new();
        if self.eat_close(TokenKind::RBrace) {
            let span = lbrace.span.join(self.previous().span);
            return Some(Expr::Dict { entries, span });
        }
        let key = self.parse_expr();
        if !self.eat(TokenKind::Colon) {
            return None;
        }
        let value = self.parse_expr();
        entries.push((key, value));
        while self.eat(TokenKind::Comma) {
            if self.at(TokenKind::RBrace) {
                break; // trailing comma
            }
            let k = self.parse_expr();
            if !self.eat(TokenKind::Colon) {
                self.error_here("expected `:` after dict key");
                break;
            }
            let v = self.parse_expr();
            entries.push((k, v));
        }
        let end = if self.eat_close(TokenKind::RBrace) {
            self.previous().span
        } else {
            self.error_here("expected `}` to close dict literal");
            lbrace.span
        };
        Some(Expr::Dict {
            entries,
            span: lbrace.span.join(end),
        })
    }

    pub(crate) fn parse_array_literal(&mut self) -> Expr {
        let lbracket = self.advance();
        self.push_delim(TokenKind::LBracket, lbracket.span);
        // Empty array: `[]`.
        if self.eat(TokenKind::RBracket) {
            let rbracket = self.previous().span;
            self.pop_delim(TokenKind::RBracket, rbracket);
            let span = lbracket.span.join(rbracket);
            return Expr::Array {
                elems: Vec::new(),
                span,
            };
        }
        // Parse first expression — could be a comprehension body or array element.
        let first = self.parse_expr();
        // List comprehension: `[expr for x in iter]` / `[expr for x in iter if cond]`.
        if self.at(TokenKind::For) {
            return self.parse_list_comp_body(lbracket, first);
        }
        // Normal array literal: `[a, b, c]`.
        let mut elems = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at(TokenKind::RBracket) {
                break; // trailing comma
            }
            elems.push(self.parse_expr());
        }
        let end = if self.eat_close(TokenKind::RBracket) {
            let rbracket = self.previous().span;
            self.pop_delim(TokenKind::RBracket, rbracket);
            rbracket
        } else {
            self.error_here("expected `]` to close array literal");
            lbracket.span
        };
        Expr::Array {
            elems,
            span: lbracket.span.join(end),
        }
    }

    /// Parse the `for x in iter [if cond]` part of a list comprehension.
    /// `first` is the already-parsed body expression.
    pub(crate) fn parse_list_comp_body(&mut self, lbracket: Token, first: Expr) -> Expr {
        self.advance(); // `for`
        let var = self
            .expect_ident()
            .unwrap_or_else(|| dummy_ident(self.peek().span));
        if !self.eat(TokenKind::In) {
            self.error_here("expected `in` after loop variable");
        }
        let iter = self.parse_expr();
        let filter = if self.at(TokenKind::If) {
            self.advance();
            Some(Box::new(self.parse_expr()))
        } else {
            None
        };
        let end = if self.eat_close(TokenKind::RBracket) {
            let rbracket = self.previous().span;
            self.pop_delim(TokenKind::RBracket, rbracket);
            rbracket
        } else {
            self.error_here("expected `]` to close list comprehension");
            lbracket.span
        };
        Expr::ListComp {
            body: Box::new(first),
            var,
            iter: Box::new(iter),
            filter,
            span: lbracket.span.join(end),
        }
    }

    pub(crate) fn parse_if(&mut self) -> Expr {
        let if_tok = self.advance();
        if self.at_ident("let") {
            return self.parse_if_let(if_tok);
        }
        let cond = self.parse_expr();
        let then = self.parse_block();
        let els = self.parse_else();
        let span = if_tok
            .span
            .join(els.as_ref().map(|e| e.span()).unwrap_or(then.span));
        Expr::If {
            cond: Box::new(cond),
            then,
            els,
            span,
        }
    }

    pub(crate) fn parse_if_let(&mut self, if_tok: Token) -> Expr {
        self.advance(); // `let`
        let pat = self.parse_pattern();
        if !self.eat(TokenKind::Assign) {
            self.error_here("expected `=` in if-let");
        }
        let value = self.parse_expr();
        let then = self.parse_block();
        let els = self.parse_else();
        let span = if_tok
            .span
            .join(els.as_ref().map(|e| e.span()).unwrap_or(then.span));
        Expr::IfLet {
            pat,
            value: Box::new(value),
            then,
            els,
            span,
        }
    }

    pub(crate) fn parse_else(&mut self) -> Option<Box<Expr>> {
        if !self.eat(TokenKind::Else) {
            return None;
        }
        if self.at(TokenKind::If) {
            Some(Box::new(self.parse_if()))
        } else {
            let block = self.parse_block();
            Some(Box::new(Expr::Block(block)))
        }
    }

    pub(crate) fn parse_match(&mut self) -> Expr {
        let match_tok = self.advance();
        let scrutinee = self.parse_expr();
        self.skip_stmt_ends();
        if !self.eat(TokenKind::LBrace) {
            self.error_here("expected `{` after match scrutinee");
        }
        let mut arms = Vec::new();
        loop {
            self.skip_stmt_ends();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            let pat = self.parse_pattern();
            if !self.eat(TokenKind::Arrow) {
                self.error_here("expected `=>` after match pattern");
            }
            let body = self.parse_expr();
            let span = pat.span().join(body.span());
            arms.push(MatchArm { pat, body, span });
            if self.eat(TokenKind::Comma) {
                continue;
            }
            if self.at(TokenKind::StmtEnd) {
                self.advance();
                continue;
            }
            if self.at(TokenKind::RBrace) {
                break;
            }
            self.error_here("expected `,` or `}` after match arm");
            self.skip_to_rbrace();
        }
        let end = if self.eat(TokenKind::RBrace) {
            self.previous().span
        } else {
            self.peek().span
        };
        let span = match_tok.span.join(end);
        Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span,
        }
    }

    pub(crate) fn parse_variant(&mut self) -> Expr {
        let dot = self.advance();
        let name_tok = self.peek().clone();
        if name_tok.kind != TokenKind::Ident {
            self.error_here("expected variant name after `.`");
            return dummy_expr(dot.span);
        }
        self.advance();
        let arg = if self.eat(TokenKind::LParen) {
            let e = self.parse_expr();
            if !self.eat_close(TokenKind::RParen) {
                self.error_here("expected `)` to close variant argument");
            }
            Some(Box::new(e))
        } else {
            None
        };
        let span = dot
            .span
            .join(arg.as_ref().map(|e| e.span()).unwrap_or(name_tok.span));
        Expr::Variant {
            name: name_tok.text,
            arg,
            span,
        }
    }

    // --- patterns ---------------------------------------------------------

    pub(crate) fn parse_pattern(&mut self) -> Pattern {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Ident if tok.text == "_" => {
                self.advance();
                Pattern::Wildcard { span: tok.span }
            }
            TokenKind::Ident => {
                self.advance();
                Pattern::Binding {
                    name: Ident {
                        name: tok.text,
                        span: tok.span,
                    },
                }
            }
            TokenKind::Int => {
                self.advance();
                let cleaned = tok.text.replace('_', "");
                let value = cleaned.parse::<i64>().unwrap_or(0);
                Pattern::Literal {
                    value: Lit::Int(value),
                    span: tok.span,
                }
            }
            TokenKind::Float => {
                self.advance();
                let cleaned = tok.text.replace('_', "");
                let value = cleaned.parse::<f64>().unwrap_or(f64::NAN);
                Pattern::Literal {
                    value: Lit::Float(value),
                    span: tok.span,
                }
            }
            TokenKind::Str => {
                self.advance();
                Pattern::Literal {
                    value: Lit::Str(tok.text),
                    span: tok.span,
                }
            }
            TokenKind::True => {
                self.advance();
                Pattern::Literal {
                    value: Lit::Bool(true),
                    span: tok.span,
                }
            }
            TokenKind::False => {
                self.advance();
                Pattern::Literal {
                    value: Lit::Bool(false),
                    span: tok.span,
                }
            }
            TokenKind::Dot => {
                self.advance();
                let name_tok = self.peek().clone();
                if name_tok.kind != TokenKind::Ident {
                    self.error_here("expected variant name after `.`");
                    return Pattern::Wildcard { span: tok.span };
                }
                self.advance();
                let arg = if self.eat(TokenKind::LParen) {
                    let p = self.parse_pattern();
                    if !self.eat_close(TokenKind::RParen) {
                        self.error_here("expected `)` to close pattern");
                    }
                    Some(Box::new(p))
                } else {
                    None
                };
                let span = tok
                    .span
                    .join(arg.as_ref().map(|p| p.span()).unwrap_or(name_tok.span));
                Pattern::Variant {
                    name: name_tok.text,
                    arg,
                    span,
                }
            }
            _ => {
                self.error_here(format!("expected pattern, found {}", tok.kind.describe()));
                Pattern::Wildcard { span: tok.span }
            }
        }
    }
}

fn dummy_expr(span: Span) -> Expr {
    Expr::Int { value: 0, span }
}

fn dummy_ident(span: Span) -> Ident {
    Ident {
        name: String::new(),
        span,
    }
}
