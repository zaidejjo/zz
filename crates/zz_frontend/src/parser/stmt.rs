//! Statement parsing.

use crate::ast::{
    BinOp, Block, Decorator, ExternFunc, Ident, ImportItem, Param, Pattern, Stmt, TraitBound,
    TypeParam,
};
use crate::diag::{error_at, FixIt};
use crate::levenshtein::suggest_all;
use crate::span::Span;
use crate::token::TokenKind;

use super::Parser;

impl<'a> Parser<'a> {
    // --- statements -------------------------------------------------------

    pub(crate) fn parse_stmt_list(&mut self, term: TokenKind) -> Vec<Stmt> {
        // Top-level list pre-sizes from the token stream (~1 stmt per ~10
        // tokens); nested blocks stay small — sizing every block from the
        // whole stream would over-reserve gigabytes on large files.
        let cap = if self.block_depth == 0 {
            self.toks.len() / 10 + 4
        } else {
            4
        };
        let mut stmts = Vec::with_capacity(cap);
        loop {
            self.skip_stmt_ends();
            if self.at(term) || self.at(TokenKind::Eof) {
                break;
            }
            // Stray closing bracket with no opener in this list: tell the
            // user to remove it and skip it so parsing continues.
            if self.at_close() {
                self.error_stray_close(self.peek_kind());
                continue;
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
                // A stray closing bracket after a complete statement (e.g.
                // the second `)` in `f(a))`): remove it, don't end the file.
                if self.at_close() {
                    self.error_stray_close(self.peek_kind());
                } else {
                    // Two statements with no terminator between them.
                    self.error_here(format!(
                        "expected end of statement, found {}",
                        self.peek_kind().describe()
                    ));
                    self.skip_to_stmt_end();
                }
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
                    // `pub type X = ...` — contextual alias (see parse_stmt).
                    TokenKind::Ident if self.at_type_alias_start() => self.parse_type_alias(true),
                    // `pub enum X { ... }` — contextual enum (see parse_stmt).
                    TokenKind::Ident if self.at_enum_start() => self.parse_enum(true),
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
                    // `pub @dec func ...` — decorators after `pub`.
                    TokenKind::At => {
                        if self.is_link_directive() {
                            self.error_here("cannot use `pub` on `@link`");
                            self.parse_link()
                        } else {
                            self.parse_decorated_func(true)
                        }
                    }
                    _ => {
                        self.error_here(
                            "expected `func`, `struct`, `impl`, `import`, decorator, or declaration after `pub`",
                        );
                        self.parse_stmt()
                    }
                };
                pub_started(stmt, pub_tok.span)
            }
            TokenKind::Import => self.parse_import(false),
            TokenKind::Func => self.parse_func(false),
            // `type X = ...` — contextual type alias. `type` stays a plain
            // identifier everywhere else (`json.type(x)`, `type := 1` keep
            // working); only `type` + name + `=`/`<>` declares an alias.
            TokenKind::Ident if self.at_type_alias_start() => self.parse_type_alias(false),
            // `enum X { ... }` — contextual user enum. Same rule as
            // aliases: `enum` stays a plain identifier everywhere else.
            TokenKind::Ident if self.at_enum_start() => self.parse_enum(false),
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
            // `a, b := expr` — bare tuple destructuring declaration
            // (no parens). Same AST as `(a, b) := expr`.
            TokenKind::Ident if self.at_bare_destructure() => self.parse_bare_destructure_decl(),
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
            TokenKind::At => {
                if self.is_link_directive() {
                    self.parse_link()
                } else {
                    self.parse_decorated_func(false)
                }
            }
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
                    && !matches!(&*self.peek().text, "true" | "false")
                    && self.peek_kind_at(1) == TokenKind::Ident
                    && self.peek_kind_at(2) == TokenKind::ColonEq
                {
                    let type_tok = self.peek().clone();
                    let type_name = type_tok.text.clone().into_owned();
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
                                name: name.text.into_owned(),
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
                // `expr OP= value` — compound assignment (`x += 1`).
                // Same targets as `=` (validated by the checker); chaining
                // (`x += y += z`) is rejected with a hint.
                if let Some(op) = self.peek_compound_op() {
                    self.advance();
                    let value = self.parse_expr();
                    if self.peek_compound_op().is_some() {
                        self.error_here(
                            "cannot chain compound assignment\n\
                             hint: split into separate statements",
                        );
                    }
                    let span = expr.span().join(value.span());
                    return Stmt::CompoundAssign {
                        target: expr,
                        op,
                        value,
                        span,
                    };
                }
                Stmt::Expr(expr)
            }
        }
    }

    /// True when the upcoming tokens form a `@link` directive rather than a
    /// function decorator: `@` `link` followed by `(` or a string literal.
    /// A bare `@link` followed by `func` is a decorator named `link`.
    pub(crate) fn is_link_directive(&self) -> bool {
        if self.peek_kind() != TokenKind::At {
            return false;
        }
        let next = self.toks.get(self.pos + 1);
        let after = self.toks.get(self.pos + 2);
        matches!((next, after), (Some(n), Some(a)) if n.kind == TokenKind::Ident
            && n.text == "link"
            && (a.kind == TokenKind::LParen || a.kind == TokenKind::Str))
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
                self.advance().text.into_owned()
            } else {
                self.error_here("expected string literal in `@link(\"lib\")`");
                String::new()
            };
            if !self.eat(TokenKind::RParen) {
                self.error_missing_close(")", "expected `)` to close `@link(...)`");
            }
            let _ = lib_tok;
            lib
        } else if self.at(TokenKind::Str) {
            self.advance().text.into_owned()
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
            self.advance().text.into_owned()
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
            let mut name = self
                .expect_ident()
                .unwrap_or_else(|| dummy_ident(self.peek().span));
            // Dotted ZZ-visible names for namespaced plugins
            // (`func zimg.resize(...)`).
            while self.eat(TokenKind::Dot) {
                let part = self
                    .expect_ident()
                    .unwrap_or_else(|| dummy_ident(self.peek().span));
                name.name.push('.');
                name.name.push_str(&part.name);
                name.span = name.span.join(part.span);
            }
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
                self.error_missing_close(")", "expected `)` after extern parameters");
            } else {
                self.pop_delim(TokenKind::RParen, self.previous().span);
            }
            let ret = if self.eat(TokenKind::Arrow) {
                Some(self.parse_type())
            } else {
                None
            };
            // Optional explicit C symbol override:
            // `func zimg.resize(...) -> int = "zimg_resize_impl";`
            // Absent = derive from the ZZ name (`.` → `_`).
            // Note: `=` lexes as Assign (Eq is `==`).
            let c_symbol = if self.eat(TokenKind::Assign) {
                if self.at(TokenKind::Str) {
                    Some(self.advance().text.into_owned())
                } else {
                    self.error_here("expected string literal for C symbol name");
                    None
                }
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
                c_symbol,
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
            self.error_missing_close("}", "expected `}` to close extern block");
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
        let generics = self.parse_struct_generics();
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
            // Embedded (anonymous) field: a bare type name with no `:`,
            // e.g. `Base,` in `struct User { Base, age: int }`. The field
            // name defaults to the type's last segment (`pkg.Base` → `Base`).
            if self.at(TokenKind::Ident) && self.is_embedded_field_start() {
                let fty = self.parse_type_base();
                match &fty.kind {
                    crate::ast::TyKind::Named(full, _) => {
                        let base = full.rsplit('.').next().unwrap_or(full).to_string();
                        let fname = crate::ast::Ident {
                            name: base,
                            span: fty.span,
                        };
                        fields.push((fname, fty));
                    }
                    _ => {
                        self.errors.push(error_at(
                            "embedded struct field must be a struct type",
                            fty.span,
                        ));
                    }
                }
                if self.eat(TokenKind::Comma) {
                    continue;
                }
                self.skip_stmt_ends();
                if !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    self.error_here("expected `,` or `}` after field");
                }
                if self.pos == start_pos {
                    self.advance();
                }
                continue;
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
            self.error_missing_close("}", "expected `}` to close struct body");
            self.peek().span
        };
        let span = struct_tok.span.join(end);
        Stmt::Struct {
            name,
            generics,
            fields,
            span,
            pub_,
        }
    }

    /// Parse `<T, U>` after a struct/impl name. Plain identifiers only:
    /// storage and receivers need no trait bounds (`<T: Num>` is rejected
    /// with a hint to bound at the function instead).
    pub(crate) fn parse_struct_generics(&mut self) -> Vec<crate::ast::Ident> {
        if !self.eat(TokenKind::Lt) {
            // Rust-style `struct Box[T]`: ZZ declares type parameters with
            // `<T>` (`[T]` applies a generic type or makes an array type).
            if self.at(TokenKind::LBracket) {
                return self.parse_misplaced_bracket_struct_generics();
            }
            return Vec::new();
        }
        let mut gs = Vec::new();
        loop {
            let name = self
                .expect_ident()
                .unwrap_or_else(|| dummy_ident(self.peek().span));
            if self.eat(TokenKind::Colon) {
                self.error_here(
                    "struct type parameters do not take bounds\n\
                     hint: bound the generic function instead (e.g. `func get<T: Num>(b: Box[T])`)",
                );
                // Skip the bound list so recovery lands on `,`/`>`.
                while !self.at(TokenKind::Comma)
                    && !self.at(TokenKind::Gt)
                    && !self.at(TokenKind::Eof)
                {
                    self.advance();
                }
            }
            gs.push(name);
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        if !self.eat_gt_close() {
            self.error_here("expected `>` to close generic parameters");
        }
        gs
    }

    pub(crate) fn parse_impl(&mut self, pub_: bool) -> Stmt {
        let impl_tok = self.advance();
        let name = self.parse_dotted_ident();
        let generics = self.parse_struct_generics();
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
                    } else if self.peek_kind() == TokenKind::At && !self.is_link_directive() {
                        methods.push(self.parse_decorated_func(true));
                    } else {
                        self.error_here("expected `func` after `pub` in impl block");
                    }
                }
                TokenKind::At => {
                    if self.is_link_directive() {
                        self.error_here("`@link` is not allowed inside `impl` blocks");
                        self.parse_link();
                    } else {
                        methods.push(self.parse_decorated_func(false));
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
            self.error_missing_close("}", "expected `}` to close impl body");
            self.peek().span
        };
        let span = impl_tok.span.join(end);
        Stmt::Impl {
            name,
            generics,
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
                name: name.text.into_owned(),
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
                name: name.text.into_owned(),
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
            self.error_missing_close(")", "expected `)` to close destructuring pattern");
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

    /// Map a compound-assignment token (`+=`, `<<=`, …) to its binary
    /// operator, or `None` when the next token isn't one.
    pub(crate) fn peek_compound_op(&self) -> Option<BinOp> {
        match self.peek_kind() {
            TokenKind::PlusEq => Some(BinOp::Add),
            TokenKind::MinusEq => Some(BinOp::Sub),
            TokenKind::StarEq => Some(BinOp::Mul),
            TokenKind::SlashEq => Some(BinOp::Div),
            TokenKind::PercentEq => Some(BinOp::Rem),
            TokenKind::StarStarEq => Some(BinOp::Pow),
            TokenKind::AmpEq => Some(BinOp::BitAnd),
            TokenKind::PipeEq => Some(BinOp::BitOr),
            TokenKind::CaretEq => Some(BinOp::BitXor),
            TokenKind::ShlEq => Some(BinOp::Shl),
            TokenKind::ShrEq => Some(BinOp::Shr),
            _ => None,
        }
    }

    /// True when the upcoming tokens form a bare destructuring head:
    /// `Ident (, Ident)+ :=`. (`_` lexes as `Ident`, so wildcards are
    /// included.) Anything else starting with `Ident` is a short decl,
    /// call, or expression — never a bare destructure.
    pub(crate) fn at_bare_destructure(&self) -> bool {
        if self.peek_kind_at(0) != TokenKind::Ident {
            return false;
        }
        let mut i = 1usize;
        // Require at least one `, Ident` pair (a lone `x := ...` is a
        // short declaration, handled elsewhere).
        let mut pairs = 0u32;
        while self.peek_kind_at(i) == TokenKind::Comma
            && self.peek_kind_at(i + 1) == TokenKind::Ident
        {
            pairs += 1;
            i += 2;
        }
        pairs > 0 && self.peek_kind_at(i) == TokenKind::ColonEq
    }

    /// True when the upcoming tokens declare a type alias: `type` +
    /// name. `type` is contextual — anywhere else it lexes and parses
    /// as a plain identifier (`json.type(x)`, `type := 1`, struct
    /// fields named `type` all keep working). Two adjacent identifiers
    /// never form a valid statement, so every `type Name` parses as an
    /// alias; a missing `=` reports from `parse_type_alias` instead of
    /// a bare "expected end of statement".
    pub(crate) fn at_type_alias_start(&self) -> bool {
        if self.peek_kind_at(0) != TokenKind::Ident {
            return false;
        }
        let is_type = self
            .toks
            .get(self.pos)
            .map(|t| &*t.text == "type")
            .unwrap_or(false);
        if !is_type {
            return false;
        }
        self.peek_kind_at(1) == TokenKind::Ident
    }

    /// Parse `type Name = Type` / `type Name<T> = Type` (dotted names
    /// allowed, mirroring `struct shapes.Point`). Generics are plain
    /// identifiers, same rule as structs.
    pub(crate) fn parse_type_alias(&mut self, pub_: bool) -> Stmt {
        let type_tok = self.advance(); // `type`
        let name = self.parse_dotted_ident();
        let generics = self.parse_struct_generics();
        if !self.eat(TokenKind::Assign) {
            self.error_here("expected `=` after type alias name (e.g. `type Tokens = [Token]`)");
        }
        let target = self.parse_type();
        let span = type_tok.span.join(target.span);
        Stmt::TypeAlias {
            name,
            generics,
            target,
            span,
            pub_,
        }
    }

    /// True when the upcoming tokens declare a user enum: `enum` + name.
    /// Same contextual rule as type aliases — anywhere else `enum`
    /// lexes and parses as a plain identifier.
    pub(crate) fn at_enum_start(&self) -> bool {
        if self.peek_kind_at(0) != TokenKind::Ident {
            return false;
        }
        let is_enum = self
            .toks
            .get(self.pos)
            .map(|t| &*t.text == "enum")
            .unwrap_or(false);
        if !is_enum {
            return false;
        }
        self.peek_kind_at(1) == TokenKind::Ident
    }

    /// Parse `enum Name { Variant, Other(Payload) }` /
    /// `enum Name[T] { ... }` (dotted names allowed, mirroring structs).
    /// Variants are comma- or newline-separated; each takes an optional
    /// single parenthesized payload type. Generic parameters are plain
    /// identifiers, same rule as structs.
    pub(crate) fn parse_enum(&mut self, pub_: bool) -> Stmt {
        let enum_tok = self.advance(); // `enum`
        let name = self.parse_dotted_ident();
        let generics = self.parse_struct_generics();
        if !self.eat(TokenKind::LBrace) {
            self.error_here(
                "expected `{` to start enum body (e.g. `enum Token { Eof, IntLit(int) }`)",
            );
            self.skip_to_rbrace();
        }
        let mut variants = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let start_pos = self.pos;
            self.skip_stmt_ends();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            let Some(vname) = self.expect_ident() else {
                self.skip_to_stmt_end();
                continue;
            };
            // Duplicate variant names report here (clearer than a
            // checker "already defined" — the enum is the context).
            // Compare names only: `Ident` equality includes spans.
            if variants
                .iter()
                .any(|(v, _): &(Ident, _)| v.name == vname.name)
            {
                self.errors.push(error_at(
                    format!(
                        "duplicate variant `{}` in enum `{}`",
                        vname.name,
                        name.join(".")
                    ),
                    vname.span,
                ));
            }
            let payload = if self.eat(TokenKind::LParen) {
                let ty = self.parse_type();
                if !self.eat(TokenKind::RParen) {
                    self.error_here(format!(
                        "expected `)` after payload type of variant `{}`",
                        vname.name
                    ));
                }
                Some(ty)
            } else {
                None
            };
            variants.push((vname, payload));
            if self.eat(TokenKind::Comma) {
                continue;
            }
            self.skip_stmt_ends();
            if !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                self.error_here("expected `,` or `}` after variant");
            }
            if self.pos == start_pos {
                self.advance();
            }
        }
        if variants.is_empty() {
            self.errors.push(error_at(
                "enum must declare at least one variant (e.g. `enum Token { Eof }`)",
                enum_tok.span,
            ));
        }
        let end = if self.eat(TokenKind::RBrace) {
            self.previous().span
        } else {
            self.error_missing_close("}", "expected `}` to close enum body");
            self.peek().span
        };
        let span = enum_tok.span.join(end);
        Stmt::Enum {
            name,
            generics,
            variants,
            span,
            pub_,
        }
    }

    /// Parse `a, b := expr` — bare tuple destructuring declaration.
    /// Same AST as `(a, b) := expr`: elements parse as full patterns
    /// (bindings and `_` wildcards), checked and evaluated by the
    /// shared `Stmt::Destructure` paths on all engines.
    pub(crate) fn parse_bare_destructure_decl(&mut self) -> Stmt {
        let start = self.peek().span;
        let mut pats = Vec::new();
        loop {
            pats.push(self.parse_pattern());
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        let pat_end = self.previous().span;
        if !self.eat(TokenKind::ColonEq) {
            self.error_here("expected `:=` after destructuring pattern");
        }
        let value = self.parse_expr();
        let span = start.join(value.span());
        Stmt::Destructure {
            pat: Pattern::Tuple {
                pats,
                span: start.join(pat_end),
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
                name: name.text.into_owned(),
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
        let mut generics = if self.eat(TokenKind::Lt) {
            let mut gs = Vec::new();
            loop {
                let name = self
                    .expect_ident()
                    .unwrap_or_else(|| dummy_ident(self.peek().span));
                let start = name.span;
                let bounds = self.parse_trait_bounds();
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
        // Rust-style `func first[T](...)`: ZZ declares generics with `<T>`
        // (`[T]` applies a generic type, e.g. `Box[T]`). Parse the bracketed
        // list for recovery and show the right way with a fix.
        if generics.is_empty() && self.at(TokenKind::LBracket) {
            generics = self.parse_misplaced_bracket_generics();
        }
        if !self.eat(TokenKind::LParen) {
            self.error_here("expected `(` after function name");
        } else {
            self.push_delim(TokenKind::LParen, self.previous().span);
        }
        let params = self.parse_param_list();
        if !self.eat(TokenKind::RParen) {
            self.error_missing_close(")", "expected `)` after parameters");
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
            decorators: Vec::new(),
        }
    }

    /// Parse `: Bound(+Bound)*` after a type parameter name. Shared by the
    /// `<T: Num>` path and the misplaced-`[T: Num]` recovery path.
    pub(crate) fn parse_trait_bounds(&mut self) -> Vec<TraitBound> {
        let mut bounds = Vec::new();
        if self.eat(TokenKind::Colon) {
            loop {
                if let Some(id) = self.expect_ident() {
                    match id.name.as_str() {
                        "Num" => bounds.push(TraitBound::Num),
                        "Ord" => bounds.push(TraitBound::Ord),
                        "Eq" => bounds.push(TraitBound::Eq),
                        "Display" => bounds.push(TraitBound::Display),
                        other => {
                            // Sherlock: suggest the closest known bound
                            // (`Number` → `Num`) with a fix.
                            let known = ["Num", "Ord", "Eq", "Display"];
                            let mut diag = error_at(
                                format!("unknown trait bound `{other}` (expected `Num`, `Ord`, `Eq`, or `Display`)"),
                                id.span,
                            );
                            if let Some((suggestion, _)) = suggest_all(other, &known).first() {
                                diag = diag
                                    .with_note(format!("did you mean `{suggestion}`?"))
                                    .with_fixit(FixIt::safe(
                                        id.span,
                                        suggestion.to_string(),
                                        "replace bound",
                                    ));
                            } else {
                                // Prefix fallback for longer typos beyond
                                // edit-distance threshold (`Number` → `Num`).
                                let lower = other.to_lowercase();
                                if let Some(prefix) = known
                                    .iter()
                                    .filter(|k| {
                                        lower.starts_with(&k.to_lowercase())
                                            || k.to_lowercase().starts_with(&lower)
                                    })
                                    .copied()
                                    .max_by_key(|k| k.len())
                                {
                                    diag = diag
                                        .with_note(format!("did you mean `{prefix}`?"))
                                        .with_fixit(FixIt::safe(
                                            id.span,
                                            prefix.to_string(),
                                            "replace bound",
                                        ));
                                }
                            }
                            self.errors.push(diag);
                        }
                    }
                }
                if self.eat(TokenKind::Plus) {
                    continue;
                }
                break;
            }
        }
        bounds
    }

    /// Recover `func first[T](...)`: parse the bracketed names (with optional
    /// bounds) into real type parameters, then report the right syntax with
    /// an `<...>` fix. The body and call sites check normally afterwards,
    /// so one typo never cascades.
    pub(crate) fn parse_misplaced_bracket_generics(&mut self) -> Vec<TypeParam> {
        let open = self.advance(); // `[`
        let mut gs = Vec::new();
        loop {
            if self.at(TokenKind::RBracket) || self.at(TokenKind::Eof) {
                break;
            }
            // Bail out of garbage (e.g. `func f[0]`) without looping forever.
            if !matches!(self.peek_kind(), TokenKind::Ident) {
                self.error_here(format!(
                    "expected type parameter name, found {}",
                    self.peek_kind().describe()
                ));
                self.skip_to_stmt_end();
                break;
            }
            let name = self
                .expect_ident()
                .unwrap_or_else(|| dummy_ident(self.peek().span));
            let start = name.span;
            let bounds = self.parse_trait_bounds();
            let end = self.previous().span;
            let span = start.join(end);
            gs.push(TypeParam { name, bounds, span });
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        let end = if self.eat(TokenKind::RBracket) {
            self.previous().span
        } else {
            self.error_missing_close("]", "expected `]` to close generic parameters");
            self.peek().span
        };
        let bracket_span = open.span.join(end);
        let fixed = format!(
            "<{}>",
            gs.iter()
                .map(|p| {
                    if p.bounds.is_empty() {
                        p.name.name.clone()
                    } else {
                        format!(
                            "{}: {}",
                            p.name.name,
                            p.bounds
                                .iter()
                                .map(|b| b.name())
                                .collect::<Vec<_>>()
                                .join(" + ")
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.errors.push(
            error_at(
                "generic parameters are declared with `<T>`, not `[T]`",
                bracket_span,
            )
            .with_note(format!(
                "write `{fixed}` instead (`[T]` applies a generic type, e.g. `Box[T]`, or makes an array type)"
            ))
            .with_fixit(FixIt::safe(bracket_span, fixed, "use `<...>`")),
        );
        gs
    }

    /// Recover `struct Box[T]`: parse the bracketed names into real type
    /// parameters, then report the right syntax with an `<...>` fix.
    pub(crate) fn parse_misplaced_bracket_struct_generics(&mut self) -> Vec<crate::ast::Ident> {
        let open = self.advance(); // `[`
        let mut gs = Vec::new();
        loop {
            if self.at(TokenKind::RBracket) || self.at(TokenKind::Eof) {
                break;
            }
            if !matches!(self.peek_kind(), TokenKind::Ident) {
                self.error_here(format!(
                    "expected type parameter name, found {}",
                    self.peek_kind().describe()
                ));
                self.skip_to_stmt_end();
                break;
            }
            gs.push(
                self.expect_ident()
                    .unwrap_or_else(|| dummy_ident(self.peek().span)),
            );
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        let end = if self.eat(TokenKind::RBracket) {
            self.previous().span
        } else {
            self.error_missing_close("]", "expected `]` to close generic parameters");
            self.peek().span
        };
        let bracket_span = open.span.join(end);
        let fixed = format!(
            "<{}>",
            gs.iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.errors.push(
            error_at(
                "type parameters are declared with `<T>`, not `[T]`",
                bracket_span,
            )
            .with_note(format!(
                "write `{fixed}` instead (`[T]` applies a generic type, e.g. `Box[T]`, or makes an array type)"
            ))
            .with_fixit(FixIt::safe(bracket_span, fixed, "use `<...>`")),
        );
        gs
    }

    /// Parse `@name` / `@name(args)` decorators followed by `func`.
    ///
    /// Grammar: (`@` dotted_ident (`(` call_args `)`)? StmtEnd*)+ (`pub`)? `func`.
    /// `@link(...)` is NOT a decorator — it is handled before calling here.
    pub(crate) fn parse_decorated_func(&mut self, pub_: bool) -> Stmt {
        let mut decorators = self.parse_decorator_list();
        self.skip_stmt_ends();
        // `pub` may appear after the decorators: `@dec pub func f()`.
        let is_pub = if self.at(TokenKind::Pub) {
            self.advance();
            true
        } else {
            pub_
        };
        if self.peek_kind() != TokenKind::Func {
            self.error_here("expected `func` after decorator (e.g. `@dec func foo() { ... }`)");
            // Recover with an empty function so later passes terminate.
            let span = decorators
                .first()
                .map(|d: &Decorator| d.span)
                .unwrap_or_else(|| self.peek().span);
            return Stmt::Func {
                name: vec![String::new()],
                generics: Vec::new(),
                params: Vec::new(),
                ret: None,
                body: Block {
                    stmts: Vec::new(),
                    span,
                },
                span,
                pub_: is_pub,
                decorators: Vec::new(),
            };
        }
        let mut stmt = self.parse_func(is_pub);
        if let Stmt::Func {
            decorators: ref mut slot,
            span: ref mut func_span,
            ..
        } = stmt
        {
            if !decorators.is_empty() {
                let first = decorators.first().unwrap().span;
                func_span.start = func_span.start.min(first.start);
            }
            *slot = std::mem::take(&mut decorators);
        }
        stmt
    }

    /// Parse one or more `@path` / `@path(args)` lines. The caller must have
    /// excluded `@link(...)`. Each decorator must start at `At`; blank lines
    /// (StmtEnd) between decorators are skipped.
    fn parse_decorator_list(&mut self) -> Vec<Decorator> {
        let mut out = Vec::new();
        while self.at(TokenKind::At) {
            let at_tok = self.advance();
            // `parse_dotted_ident` emits its own diagnostic on failure.
            let path = self.parse_dotted_ident();
            let (args, named, end) = if self.eat(TokenKind::LParen) {
                let (args, named) = self.parse_call_args();
                let end = if self.eat_close(TokenKind::RParen) {
                    self.previous().span
                } else {
                    self.error_missing_close(")", "expected `)` to close decorator arguments");
                    self.peek().span
                };
                (args, named, end)
            } else {
                (Vec::new(), Vec::new(), self.previous().span)
            };
            out.push(Decorator {
                path,
                args,
                named,
                span: at_tok.span.join(end),
            });
            self.skip_stmt_ends();
            // A second `@` continues the list; anything else ends it.
            if !self.at(TokenKind::At) {
                break;
            }
        }
        out
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
                // Trailing comma: `func f(a: int, b: int,)` ends the list.
                if self.at(TokenKind::RParen) {
                    break;
                }
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
        self.block_depth += 1;
        let stmts = self.parse_stmt_list(TokenKind::RBrace);
        self.block_depth -= 1;
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
            decorators,
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
                decorators,
            }
        }
        Stmt::Struct {
            name,
            generics,
            fields,
            span: mut sp,
            pub_,
        } => {
            span(&mut sp);
            Stmt::Struct {
                name,
                generics,
                fields,
                span: sp,
                pub_,
            }
        }
        Stmt::Impl {
            name,
            generics,
            methods,
            span: mut sp,
            pub_,
        } => {
            span(&mut sp);
            Stmt::Impl {
                name,
                generics,
                methods,
                span: sp,
                pub_,
            }
        }
        other => other,
    }
}
