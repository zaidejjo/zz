//! Statement parsing.

use crate::ast::{
    Block, ExternFunc, Ident, ImportItem, Param, Pattern, Stmt, TraitBound, TypeParam,
};
use crate::diag::error_at;
use crate::span::Span;
use crate::token::TokenKind;

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
            TokenKind::Pub => {
                let pub_tok = self.advance();
                // `pub` must be followed by func, struct, import, or a declaration.
                let stmt = match self.peek_kind() {
                    TokenKind::Func => self.parse_func(true),
                    TokenKind::Struct => self.parse_struct(true),
                    TokenKind::Impl => {
                        // `pub impl` is not allowed: impl methods are always
                        // public. Recover by treating the impl as non-pub.
                        self.error_here(
                            "cannot use `pub` on `impl`\n\
                             hint: impl methods are always public, remove the `pub` keyword",
                        );
                        self.parse_impl(false)
                    }
                    TokenKind::Import => self.parse_import(true),
                    // `pub const x = expr` / `pub const x: type = expr`
                    TokenKind::Const => self.parse_const_decl(true),
                    // `pub x := expr` or `pub x: type = expr`
                    TokenKind::Ident if self.peek_kind_at(1) == TokenKind::ColonEq => {
                        self.parse_short_decl(true, false)
                    }
                    TokenKind::Ident => {
                        // Try `pub x: type = expr`
                        let save_pos = self.pos;
                        let save_errs = self.errors.len();
                        if let Some(decl) = self.try_parse_explicit_decl(true, false) {
                            decl
                        } else {
                            self.pos = save_pos;
                            self.errors.truncate(save_errs);
                            self.error_here(
                                "expected `func`, `struct`, `import`, or declaration after `pub`",
                            );
                            // Recover by parsing the next statement as if `pub` wasn't there.
                            self.parse_stmt()
                        }
                    }
                    _ => {
                        self.error_here(
                            "expected `func`, `struct`, `impl`, `import`, or declaration after `pub`",
                        );
                        self.parse_stmt()
                    }
                };
                pub_started(stmt, pub_tok.span)
            }
            TokenKind::Import => self.parse_import(false),
            TokenKind::Func => self.parse_func(false),
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
                self.parse_short_decl(false, false)
            }
            // `const x = expr` / `const x: type = expr` — immutable binding.
            TokenKind::Const => self.parse_const_decl(false),
            // `(a, b) := expr` — tuple destructuring declaration.
            TokenKind::LParen
                if self.peek_kind_at(1) == TokenKind::Ident
                    && self.peek_kind_at(2) == TokenKind::Comma =>
            {
                self.parse_destructure_decl(false)
            }
            TokenKind::Struct => self.parse_struct(false),
            TokenKind::Impl => self.parse_impl(false),
            TokenKind::At => self.parse_link(),
            TokenKind::Extern => self.parse_extern_block(),
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
                if let Some(decl) = self.try_parse_explicit_decl(false, false) {
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
                            pub_: false,
                            is_const: false,
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

    pub(crate) fn parse_link(&mut self) -> Stmt {
        let at_tok = self.advance(); // `@`
        if self.at(TokenKind::Ident) && self.peek().text == "link" {
            self.advance();
        } else {
            self.error_here("expected `link` after `@` (e.g. `@link(\"sqlite3\")`)");
        }
        let lib = if self.eat(TokenKind::LParen) {
            let lib_tok = self.peek().clone();
            let lib = if self.at(TokenKind::Str) {
                self.advance().text
            } else {
                self.error_here("expected string literal in `@link(\"lib\")`");
                String::new()
            };
            if !self.eat(TokenKind::RParen) {
                self.error_here("expected `)` to close `@link(...)`");
            }
            let _ = lib_tok;
            lib
        } else if self.at(TokenKind::Str) {
            self.advance().text
        } else {
            self.error_here("expected `(\"lib\")` after `@link`");
            String::new()
        };
        if lib.is_empty() {
            self.error_here("`@link` requires a non-empty library name");
        }
        let span = at_tok.span.join(self.previous().span);
        Stmt::Link { lib, span }
    }

    pub(crate) fn parse_extern_block(&mut self) -> Stmt {
        let extern_tok = self.advance(); // `extern`
        let abi_tok = self.peek().clone();
        let abi = if self.at(TokenKind::Str) {
            self.advance().text
        } else {
            self.error_here("expected ABI string after `extern` (e.g. `extern \"C\"`)");
            String::new()
        };
        if abi != "C" {
            self.errors.push(error_at(
                format!("unsupported extern ABI `{abi}` (expected `\"C\"`)"),
                abi_tok.span,
            ));
        }
        if !self.eat(TokenKind::LBrace) {
            self.error_here("expected `{` to start extern block");
            self.skip_to_rbrace();
        }
        let mut items = Vec::new();
        loop {
            self.skip_stmt_ends();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            if !self.at(TokenKind::Func) {
                self.error_here("expected `func` signature in extern block");
                self.skip_to_stmt_end();
                continue;
            }
            let func_tok = self.advance(); // `func`
            let name = self
                .expect_ident()
                .unwrap_or_else(|| dummy_ident(self.peek().span));
            if self.at(TokenKind::Lt) {
                self.error_here("extern functions cannot have generic parameters");
            }
            if !self.eat(TokenKind::LParen) {
                self.error_here("expected `(` after extern function name");
            } else {
                self.push_delim(TokenKind::LParen, self.previous().span);
            }
            let params = self.parse_param_list();
            if !self.eat(TokenKind::RParen) {
                self.error_here("expected `)` after extern parameters");
            } else {
                self.pop_delim(TokenKind::RParen, self.previous().span);
            }
            let ret = if self.eat(TokenKind::Arrow) {
                Some(self.parse_type())
            } else {
                None
            };
            // Extern signatures have no body — must end the statement here.
            let span = func_tok.span.join(
                ret.as_ref()
                    .map(|t| t.span)
                    .unwrap_or_else(|| params.last().map(|p| p.span).unwrap_or(name.span)),
            );
            items.push(ExternFunc {
                name,
                params,
                ret,
                span,
            });
            // A trailing `{` means the user wrote a body — reject it.
            if self.at(TokenKind::LBrace) {
                self.error_here("extern function signatures must not have a body");
                // Skip the block to recover.
                self.advance();
                self.skip_to_rbrace();
                self.eat(TokenKind::RBrace);
            }
        }
        let end = if self.eat(TokenKind::RBrace) {
            self.previous().span
        } else {
            self.error_here("expected `}` to close extern block");
            self.peek().span
        };
        let span = extern_tok.span.join(end);
        Stmt::ExternBlock {
            abi: if abi.is_empty() { "C".to_string() } else { abi },
            items,
            span,
        }
    }

    pub(crate) fn parse_struct(&mut self, pub_: bool) -> Stmt {
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
        Stmt::Struct {
            name,
            fields,
            span,
            pub_,
        }
    }

    pub(crate) fn parse_impl(&mut self, pub_: bool) -> Stmt {
        let impl_tok = self.advance();
        let name = self.parse_dotted_ident();
        if !self.eat(TokenKind::LBrace) {
            self.error_here("expected `{` to start impl body");
            self.skip_to_rbrace();
        }
        let mut methods = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            self.skip_stmt_ends();
            if self.at(TokenKind::RBrace) {
                break;
            }
            match self.peek_kind() {
                TokenKind::Func => {
                    methods.push(self.parse_func(false));
                }
                TokenKind::Pub => {
                    let _pub_tok = self.advance();
                    if self.peek_kind() == TokenKind::Func {
                        methods.push(self.parse_func(true));
                    } else {
                        self.error_here("expected `func` after `pub` in impl block");
                    }
                }
                _ => {
                    self.error_here("expected `func` in impl block");
                    // Skip to next statement or end of block
                    if self.pos < self.toks.len() {
                        self.advance();
                    }
                }
            }
        }
        let end = if self.eat(TokenKind::RBrace) {
            self.previous().span
        } else {
            self.error_here("expected `}` to close impl body");
            self.peek().span
        };
        let span = impl_tok.span.join(end);
        Stmt::Impl {
            name,
            methods,
            span,
            pub_,
        }
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

    pub(crate) fn parse_import(&mut self, pub_: bool) -> Stmt {
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
        // Parse optional selective import list: `import module(A, B, *)`
        let items = if self.eat(TokenKind::LParen) {
            let mut items = Vec::new();
            while self.peek_kind() != TokenKind::RParen {
                if self.peek_kind() == TokenKind::Star {
                    let tok = self.advance();
                    items.push(ImportItem::Wildcard { span: tok.span });
                } else if let Some(id) = self.expect_ident() {
                    let item_alias = if self.eat(TokenKind::As) {
                        self.expect_ident().map(|id| id.name)
                    } else {
                        None
                    };
                    items.push(ImportItem::Named {
                        name: id.name,
                        alias: item_alias,
                        span: id.span,
                    });
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.eat(TokenKind::RParen);
            items
        } else {
            Vec::new()
        };
        let span = import_tok.span.join(self.previous().span);
        Stmt::Import {
            path,
            alias,
            items,
            span,
            pub_,
        }
    }

    pub(crate) fn parse_short_decl(&mut self, pub_: bool, is_const: bool) -> Stmt {
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
            pub_,
            is_const,
        }
    }

    /// Parse `const x = expr` or `const x: Type = expr` — an immutable
    /// binding. Unlike plain declarations, `const` uses `=` for both the
    /// inferred and the annotated form.
    pub(crate) fn parse_const_decl(&mut self, pub_: bool) -> Stmt {
        let const_tok = self.advance(); // `const`
        let name = self.advance(); // identifier
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        if !self.eat(TokenKind::Assign) {
            self.error_here("expected `=` after const declaration");
        }
        let value = self.parse_expr();
        let span = const_tok.span.join(value.span());
        Stmt::Decl {
            ty,
            name: Ident {
                name: name.text,
                span: name.span,
            },
            value,
            span,
            pub_,
            is_const: true,
        }
    }

    /// Parse `(a, b) := expr` — tuple destructuring declaration.
    pub(crate) fn parse_destructure_decl(&mut self, _pub_: bool) -> Stmt {
        let lparen = self.advance(); // `(`
        let mut pats = Vec::new();
        // Parse first pattern
        pats.push(self.parse_pattern());
        // Parse remaining patterns
        while self.eat(TokenKind::Comma) {
            if self.at(TokenKind::RParen) {
                break;
            }
            pats.push(self.parse_pattern());
        }
        let rparen = if self.eat(TokenKind::RParen) {
            self.previous().span
        } else {
            self.error_here("expected `)` to close destructuring pattern");
            self.peek().span
        };
        // Parse `:=`
        if !self.eat(TokenKind::ColonEq) {
            self.error_here("expected `:=` after destructuring pattern");
        }
        let value = self.parse_expr();
        let span = lparen.span.join(value.span());
        Stmt::Destructure {
            pat: Pattern::Tuple {
                pats,
                span: lparen.span.join(rparen),
            },
            value,
            span,
        }
    }

    /// Parse `IDENT: TYPE = expr`; returns `None` (with position restored by
    /// the caller) when the statement is not an explicit declaration.
    pub(crate) fn try_parse_explicit_decl(&mut self, pub_: bool, is_const: bool) -> Option<Stmt> {
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
            pub_,
            is_const,
        })
    }

    pub(crate) fn parse_func(&mut self, pub_: bool) -> Stmt {
        let func_tok = self.advance();
        let name = self.parse_dotted_ident();
        let generics = if self.eat(TokenKind::Lt) {
            let mut gs = Vec::new();
            loop {
                let name = self
                    .expect_ident()
                    .unwrap_or_else(|| dummy_ident(self.peek().span));
                let start = name.span;
                let mut bounds = Vec::new();
                if self.eat(TokenKind::Colon) {
                    loop {
                        if let Some(id) = self.expect_ident() {
                            match id.name.as_str() {
                                "Num" => bounds.push(TraitBound::Num),
                                "Ord" => bounds.push(TraitBound::Ord),
                                "Eq" => bounds.push(TraitBound::Eq),
                                "Display" => bounds.push(TraitBound::Display),
                                other => self.errors.push(error_at(
                                    format!("unknown trait bound `{other}` (expected `Num`, `Ord`, `Eq`, or `Display`)"),
                                    id.span,
                                )),
                            }
                        }
                        if self.eat(TokenKind::Plus) {
                            continue;
                        }
                        break;
                    }
                }
                let end = self.previous().span;
                let span = start.join(end);
                gs.push(TypeParam { name, bounds, span });
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
            pub_,
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

/// Extend a statement's span to include the leading `pub` keyword so the
/// `pub_` flag and the span always agree (the formatter relies on it when
/// re-emitting `pub`).
fn pub_started(stmt: Stmt, pub_span: Span) -> Stmt {
    let span = |s: &mut Span| {
        if s.start > pub_span.start {
            s.start = pub_span.start;
        }
    };
    match stmt {
        Stmt::Decl {
            ty,
            name,
            value,
            span: mut sp,
            pub_,
            is_const,
        } => {
            span(&mut sp);
            Stmt::Decl {
                ty,
                name,
                value,
                span: sp,
                pub_,
                is_const,
            }
        }
        Stmt::Import {
            path,
            alias,
            items,
            span: mut sp,
            pub_,
        } => {
            span(&mut sp);
            Stmt::Import {
                path,
                alias,
                items,
                span: sp,
                pub_,
            }
        }
        Stmt::Func {
            name,
            generics,
            params,
            ret,
            body,
            span: mut sp,
            pub_,
        } => {
            span(&mut sp);
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                body,
                span: sp,
                pub_,
            }
        }
        Stmt::Struct {
            name,
            fields,
            span: mut sp,
            pub_,
        } => {
            span(&mut sp);
            Stmt::Struct {
                name,
                fields,
                span: sp,
                pub_,
            }
        }
        Stmt::Impl {
            name,
            methods,
            span: mut sp,
            pub_,
        } => {
            span(&mut sp);
            Stmt::Impl {
                name,
                methods,
                span: sp,
                pub_,
            }
        }
        other => other,
    }
}
