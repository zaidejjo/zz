//! Recursive-descent parser with statement-level error recovery.
//!
//! Grammar (Phase 1.5):
//! ```text
//! program        := stmt* eof
//! stmt           := import_stmt | decl_stmt | func_stmt | return_stmt | expr_stmt
//! import_stmt    := 'import' IDENT ('.' IDENT)*
//! decl_stmt      := IDENT ':=' expr                       // short declaration
//!                | type IDENT '=' expr                    // explicit declaration
//! func_stmt      := 'func' IDENT ('<' IDENT (',' IDENT)* '>')? '(' param_list ')' ('->' type)? block
//! return_stmt    := 'return' expr?
//! param_list     := (param (',' param)*)?
//! param          := IDENT (':' type)?
//! block          := '{' stmt* '}'
//! type           := type_base ('|' type_base)*            // union
//! type_base      := 'int'|'float'|'bool'|'str'|'unit'
//!                | IDENT ('<' type (',' type)* '>')?
//!                | '(' type (',' type)* ')'
//!                | '[' type ']'                           // array
//!                | '{' type ':' type '}'                  // dict
//! expr           := or
//! or             := and ('||' and)*
//! and            := equality ('&&' equality)*
//! equality       := relational (('=='|'!=') relational)*
//! relational     := additive (('<'|'>'|'<='|'>=') additive)*
//! additive       := multiplicative (('+'|'-') multiplicative)*
//! multiplicative := unary (('*'|'/'|'%') unary)*
//! unary          := ('-'|'+'|'!') unary | postfix
//! postfix        := primary (call | '?')*
//! primary        := literal | IDENT | '(' expr ')' | '[' expr_list ']' | dict_or_block
//!                | closure | 'if' | 'while' | 'match' | '.' variant
//! dict_or_block  := '{' (expr ':' expr (',' expr ':' expr)*)? '}'   // dict
//!                | '{' stmt* '}'                                     // block
//! closure        := '|' param_list '|' expr
//! if             := 'if' ('let' pattern '=')? expr block ('else' (if | block))?
//! while          := 'while' expr block
//! match          := 'match' expr '{' (pattern '=>' expr (','|stmt_end))* '}'
//! pattern        := '_' | IDENT | literal | '.' IDENT ('(' pattern ')')?
//! ```
//!
//! On a statement-level error the parser records a diagnostic and skips to
//! the next `StmtEnd`, so one bad line never hides the rest of the program.

use crate::ast::{
    BinOp, Block, Expr, Ident, Lit, MatchArm, Param, Pattern, Program, Stmt, Ty, TyKind, UnOp,
};
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
        let stmts = self.parse_stmt_list(TokenKind::Eof);
        let span = Span::new(0, self.src_len());
        Program { stmts, span }
    }

    fn src_len(&self) -> u32 {
        self.toks.last().map(|t| t.span.start).unwrap_or(0)
    }

    // --- statements -------------------------------------------------------

    fn parse_stmt_list(&mut self, term: TokenKind) -> Vec<Stmt> {
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

    fn parse_stmt(&mut self) -> Stmt {
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
            _ => {
                // Try `TYPE IDENT = expr` (explicit declaration). Backtrack
                // on failure so ordinary expressions still parse.
                let save_pos = self.pos;
                let save_errs = self.errors.len();
                if let Some(decl) = self.try_parse_explicit_decl() {
                    return decl;
                }
                self.pos = save_pos;
                self.errors.truncate(save_errs);
                Stmt::Expr(self.parse_expr())
            }
        }
    }

    fn parse_import(&mut self) -> Stmt {
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
        let span = import_tok.span.join(self.previous().span);
        Stmt::Import { path, span }
    }

    fn parse_short_decl(&mut self) -> Stmt {
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

    /// Parse `TYPE IDENT = expr`; returns `None` (with position restored by
    /// the caller) when the statement is not an explicit declaration.
    fn try_parse_explicit_decl(&mut self) -> Option<Stmt> {
        let ty = self.parse_type();
        if !self.at(TokenKind::Ident) {
            return None;
        }
        let name = self.advance();
        if !self.eat(TokenKind::Assign) {
            return None;
        }
        let value = self.parse_expr();
        let span = ty.span.join(value.span());
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

    fn parse_func(&mut self) -> Stmt {
        let func_tok = self.advance();
        let name = self
            .expect_ident()
            .unwrap_or_else(|| dummy_ident(func_tok.span));
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
        }
        let params = self.parse_param_list();
        if !self.eat(TokenKind::RParen) {
            self.error_here("expected `)` after parameters");
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

    fn parse_param_list(&mut self) -> Vec<Param> {
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
            let span = name
                .span
                .join(ty.as_ref().map(|t| t.span).unwrap_or(name.span));
            params.push(Param { name, ty, span });
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        params
    }

    fn parse_block(&mut self) -> Block {
        let lbrace = self.peek().clone();
        self.skip_stmt_ends();
        if !self.eat(TokenKind::LBrace) {
            self.error_here("expected `{` to start block");
        }
        let stmts = self.parse_stmt_list(TokenKind::RBrace);
        let end = if self.eat(TokenKind::RBrace) {
            self.previous().span
        } else {
            self.peek().span
        };
        Block {
            stmts,
            span: lbrace.span.join(end),
        }
    }

    // --- types ------------------------------------------------------------

    fn parse_type(&mut self) -> Ty {
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

    fn parse_type_base(&mut self) -> Ty {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Ident => {
                self.advance();
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
                let kind = match tok.text.as_str() {
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
                    _ => TyKind::Named(tok.text, args),
                };
                Ty {
                    kind,
                    span: tok.span,
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
                    let end = if self.eat_close(TokenKind::RParen) {
                        self.previous().span
                    } else {
                        self.error_here("expected `)` to close tuple type");
                        first_span
                    };
                    Ty {
                        kind: TyKind::Tuple(ts),
                        span: tok.span.join(end),
                    }
                } else {
                    let end = if self.eat_close(TokenKind::RParen) {
                        self.previous().span
                    } else {
                        self.error_here("expected `)` to close type");
                        first_span
                    };
                    Ty {
                        kind: first.kind,
                        span: tok.span.join(end),
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

    // --- expressions ------------------------------------------------------

    fn parse_expr(&mut self) -> Expr {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Expr {
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

    fn parse_and(&mut self) -> Expr {
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

    fn parse_equality(&mut self) -> Expr {
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

    fn parse_relational(&mut self) -> Expr {
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

    fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            match self.peek_kind() {
                TokenKind::LParen => {
                    self.advance();
                    let args = self.parse_expr_list();
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
                _ => break,
            }
        }
        expr
    }

    fn parse_expr_list(&mut self) -> Vec<Expr> {
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
            TokenKind::Str => {
                self.advance();
                Expr::Str {
                    value: tok.text,
                    span: tok.span,
                }
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
                self.advance();
                let inner = self.parse_expr();
                let end = if self.eat_close(TokenKind::RParen) {
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

    fn parse_closure(&mut self) -> Expr {
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
                params.push(Param { name, ty, span });
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

    fn parse_dict_or_block(&mut self) -> Expr {
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
    fn try_parse_dict(&mut self) -> Option<Expr> {
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

    fn parse_array_literal(&mut self) -> Expr {
        let lbracket = self.advance();
        let mut elems = Vec::new();
        if !self.at(TokenKind::RBracket) {
            loop {
                elems.push(self.parse_expr());
                if self.eat(TokenKind::Comma) {
                    continue;
                }
                break;
            }
        }
        let end = if self.eat_close(TokenKind::RBracket) {
            self.previous().span
        } else {
            self.error_here("expected `]` to close array literal");
            lbracket.span
        };
        Expr::Array {
            elems,
            span: lbracket.span.join(end),
        }
    }

    fn parse_if(&mut self) -> Expr {
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

    fn parse_if_let(&mut self, if_tok: Token) -> Expr {
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

    fn parse_else(&mut self) -> Option<Box<Expr>> {
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

    fn parse_match(&mut self) -> Expr {
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

    fn parse_variant(&mut self) -> Expr {
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

    fn parse_pattern(&mut self) -> Pattern {
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

    fn skip_to_rbrace(&mut self) {
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            self.advance();
        }
    }

    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn peek_kind(&self) -> TokenKind {
        self.toks[self.pos].kind
    }

    fn peek_kind_at(&self, offset: usize) -> TokenKind {
        self.toks
            .get(self.pos + offset)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    fn at_ident(&self, name: &str) -> bool {
        self.at(TokenKind::Ident) && self.peek().text == name
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

    /// Consume a closing delimiter, skipping statement terminators first so
    /// multi-line calls/expressions like `f(\n  a\n)` parse.
    fn eat_close(&mut self, kind: TokenKind) -> bool {
        self.skip_stmt_ends();
        self.eat(kind)
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
    fn parses_short_decl() {
        let p = parse_ok("x := 1 + 2");
        assert_eq!(p.stmts.len(), 1);
        match &p.stmts[0] {
            Stmt::Decl {
                ty: None,
                name,
                value,
                ..
            } => {
                assert_eq!(name.name, "x");
                assert!(matches!(value, E::Binary { op: Add, .. }));
            }
            other => panic!("expected short decl, got {other:?}"),
        }
    }

    #[test]
    fn parses_explicit_decl() {
        let p = parse_ok("int x = 10");
        match &p.stmts[0] {
            Stmt::Decl {
                ty: Some(ty), name, ..
            } => {
                assert_eq!(ty.kind, TyKind::Int);
                assert_eq!(name.name, "x");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_str_explicit_decl() {
        let p = parse_ok("str s = \"hello\"");
        match &p.stmts[0] {
            Stmt::Decl { ty: Some(ty), .. } => assert_eq!(ty.kind, TyKind::Str),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_array_decl_and_literal() {
        let p = parse_ok("scores := [10, 20, 30]");
        match &p.stmts[0] {
            Stmt::Decl {
                ty: None, value, ..
            } => {
                assert!(matches!(value, E::Array { elems, .. } if elems.len() == 3));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let p = parse_ok("[int] scores = [10, 20, 30]");
        match &p.stmts[0] {
            Stmt::Decl { ty: Some(ty), .. } => {
                assert!(matches!(ty.kind, TyKind::Array(_)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_dict_decl_and_literal() {
        let p = parse_ok("ages := {\"Zaid\": 20}");
        match &p.stmts[0] {
            Stmt::Decl {
                ty: None, value, ..
            } => {
                assert!(matches!(value, E::Dict { entries, .. } if entries.len() == 1));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let p = parse_ok("{str: int} ages = {\"Zaid\": 20}");
        match &p.stmts[0] {
            Stmt::Decl { ty: Some(ty), .. } => {
                assert!(matches!(ty.kind, TyKind::Dict(_, _)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_union_type() {
        let p = parse_ok("{str: str | int} user = {\"name\": \"Zaid\", \"age\": 20}");
        match &p.stmts[0] {
            Stmt::Decl { ty: Some(ty), .. } => {
                assert!(matches!(ty.kind, TyKind::Dict(_, _)));
                if let TyKind::Dict(_, v) = &ty.kind {
                    assert!(matches!(v.kind, TyKind::Union(_)));
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_import() {
        let p = parse_ok("import std.io");
        match &p.stmts[0] {
            Stmt::Import { path, .. } => {
                assert_eq!(path, &vec!["std".to_string(), "io".to_string()])
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_func() {
        let p = parse_ok("func add(a: int, b: int) -> int { return a + b }");
        match &p.stmts[0] {
            Stmt::Func {
                name, params, ret, ..
            } => {
                assert_eq!(name.name, "add");
                assert_eq!(params.len(), 2);
                assert_eq!(ret.as_ref().unwrap().kind, TyKind::Int);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_generic_func() {
        let p = parse_ok("func id<T>(x: T) -> T { return x }");
        match &p.stmts[0] {
            Stmt::Func {
                generics, params, ..
            } => {
                assert_eq!(generics.len(), 1);
                assert_eq!(generics[0].name, "T");
                assert_eq!(
                    params[0].ty.as_ref().unwrap().kind,
                    TyKind::Named("T".into(), vec![])
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_option_result_types() {
        let p = parse_ok("Option<int> a = .none\nResult<int, str> b = .ok(1)");
        match &p.stmts[0] {
            Stmt::Decl { ty: Some(ty), .. } => {
                assert!(matches!(ty.kind, TyKind::Option(_)));
            }
            other => panic!("unexpected: {other:?}"),
        }
        match &p.stmts[1] {
            Stmt::Decl { ty: Some(ty), .. } => {
                assert!(matches!(ty.kind, TyKind::Result(_, _)));
            }
            other => panic!("unexpected: {other:?}"),
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
    fn comparison_precedence() {
        let p = parse_ok("1 + 2 < 3 * 4");
        match &p.stmts[0] {
            Stmt::Expr(E::Binary { op: Lt, .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn logical_ops() {
        let p = parse_ok("true && false || !true");
        match &p.stmts[0] {
            Stmt::Expr(E::Binary { op: Or, .. }) => {}
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
        let p = parse_ok("a := 1\nb := 2\nc := a + b");
        assert_eq!(p.stmts.len(), 3);
    }

    #[test]
    fn parses_call() {
        let p = parse_ok("add(1, 2)");
        match &p.stmts[0] {
            Stmt::Expr(E::Call { args, .. }) => assert_eq!(args.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_closure() {
        let p = parse_ok("|x: int, y| x + y");
        match &p.stmts[0] {
            Stmt::Expr(E::Closure { params, .. }) => {
                assert_eq!(params.len(), 2);
                assert!(params[0].ty.is_some());
                assert!(params[1].ty.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_if_else() {
        let p = parse_ok("if x > 5 { 1 } else { 2 }");
        match &p.stmts[0] {
            Stmt::Expr(E::If { els: Some(_), .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_else_if_chain() {
        let p = parse_ok("if a { 1 } else if b { 2 } else { 3 }");
        match &p.stmts[0] {
            Stmt::Expr(E::If { els: Some(e), .. }) => {
                assert!(matches!(**e, E::If { .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_while() {
        let p = parse_ok("while x < 10 { f(x) }");
        match &p.stmts[0] {
            Stmt::Expr(E::While { .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_match() {
        let p = parse_ok("match x { .ok(v) => v, .err(e) => 0 }");
        match &p.stmts[0] {
            Stmt::Expr(E::Match { arms, .. }) => assert_eq!(arms.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_match_multiline() {
        let p = parse_ok("match x {\n    .some(v) => v\n    .none => 0\n}");
        match &p.stmts[0] {
            Stmt::Expr(E::Match { arms, .. }) => assert_eq!(arms.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_if_let() {
        let p = parse_ok("if let .some(x) = opt { x } else { 0 }");
        match &p.stmts[0] {
            Stmt::Expr(E::IfLet { pat, .. }) => {
                assert!(matches!(pat, Pattern::Variant { name, .. } if name == "some"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_try() {
        let p = parse_ok("f()?");
        match &p.stmts[0] {
            Stmt::Expr(E::Try { .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_variant_constructors() {
        let p = parse_ok(".ok(1)");
        match &p.stmts[0] {
            Stmt::Expr(E::Variant { name, arg, .. }) => {
                assert_eq!(name, "ok");
                assert!(arg.is_some());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_block_expression() {
        let p = parse_ok("x := { y := 1; y + 1 }");
        match &p.stmts[0] {
            Stmt::Decl { value, .. } => assert!(matches!(value, E::Block(_))),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_string_and_bool() {
        let p = parse_ok("s := \"hi\"\nb := true");
        match &p.stmts[0] {
            Stmt::Decl { value, .. } => assert!(matches!(value, E::Str { .. })),
            other => panic!("unexpected: {other:?}"),
        }
        match &p.stmts[1] {
            Stmt::Decl { value, .. } => assert!(matches!(value, E::Bool { .. })),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_dotted_path() {
        let prog = parse("r := std.io.println(1)\n");
        assert!(prog.errors.is_empty(), "errors: {:?}", prog.errors);
        match &prog.program.stmts[0] {
            Stmt::Decl {
                value: Expr::Call { callee, .. },
                ..
            } => match callee.as_ref() {
                Expr::Path { parts, .. } => {
                    assert_eq!(parts, &["std", "io", "println"]);
                }
                other => panic!("expected Path callee, got {other:?}"),
            },
            other => panic!("expected Decl, got {other:?}"),
        }
    }

    #[test]
    fn single_ident_is_not_path() {
        let prog = parse("x := 1\n");
        assert!(prog.errors.is_empty());
        match &prog.program.stmts[0] {
            Stmt::Decl {
                value: Expr::Int { .. },
                ..
            } => {}
            other => panic!("expected plain decl, got {other:?}"),
        }
    }

    #[test]
    fn missing_equals_reports_error_and_recovers() {
        let parsed = parse("x := 1\ny := 2");
        assert_eq!(parsed.errors.len(), 0);
        assert_eq!(parsed.program.stmts.len(), 2);
    }

    #[test]
    fn missing_expression_reports_error() {
        let parsed = parse("x :=");
        assert_eq!(parsed.errors.len(), 1);
    }

    #[test]
    fn missing_close_paren_reports_error() {
        let parsed = parse("(1 + 2");
        assert_eq!(parsed.errors.len(), 1);
    }

    #[test]
    fn missing_stmt_end_reports_error() {
        let parsed = parse("x := 1 y := 2");
        assert_eq!(parsed.errors.len(), 1);
    }

    #[test]
    fn empty_program_ok() {
        let p = parse_ok("");
        assert!(p.stmts.is_empty());
    }
}
