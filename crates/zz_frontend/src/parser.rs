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
//! expr           := pipe
//! pipe           := range ('|>' range)*                    // pipeline
//! range          := or ('..' or)?                          // integer range
//! or             := and ('||' and)*
//! and            := equality ('&&' equality)*
//! equality       := relational (('=='|'!=') relational)*
//! relational     := additive (('<'|'>'|'<='|'>=') additive)*
//! additive       := multiplicative (('+'|'-') multiplicative)*
//! multiplicative := unary (('*'|'/'|'%') unary)*
//! unary          := ('-'|'+'|'!') unary | postfix
//! postfix        := primary (call | '?' | '.' IDENT | '[' expr (':' expr)? ']')*
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
    BinOp, Block, Expr, FmtPart, Ident, Lit, MatchArm, Param, Pattern, Program, Stmt, Ty, TyKind,
    UnOp,
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
        delim_stack: Vec::new(),
    };
    let program = parser.parse_program();
    Parsed {
        program,
        errors: parser.errors,
    }
}

/// A tracked open delimiter for mismatched-delimiter diagnostics.
#[derive(Debug, Clone)]
struct DelimEntry {
    open: TokenKind,
    span: Span,
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    errors: Vec<RawDiag>,
    /// Stack of open delimiters for mismatched-delimiter diagnostics.
    delim_stack: Vec<DelimEntry>,
}

impl Parser {
    fn parse_program(&mut self) -> Program {
        let stmts = self.parse_stmt_list(TokenKind::Eof);
        self.check_unclosed_delims();
        let span = Span::new(0, self.src_len());
        Program { stmts, span }
    }

    fn src_len(&self) -> u32 {
        self.toks.last().map(|t| t.span.start).unwrap_or(0)
    }

    // --- delimiter tracking ------------------------------------------------

    fn push_delim(&mut self, open: TokenKind, span: Span) {
        self.delim_stack.push(DelimEntry { open, span });
    }

    fn pop_delim(&mut self, close: TokenKind, close_span: Span) {
        let expected = match close {
            TokenKind::RParen => Some(TokenKind::LParen),
            TokenKind::RBrace => Some(TokenKind::LBrace),
            TokenKind::RBracket => Some(TokenKind::LBracket),
            _ => None,
        };
        if expected.is_none() {
            return;
        }
        let expected = expected.unwrap();

        // Find matching opener, reporting any mismatches in between.
        match self.delim_stack.iter().rposition(|e| e.open == expected) {
            Some(idx) => {
                // Pop everything above the match (mismatched delimiters).
                for entry in self.delim_stack.drain(idx + 1..) {
                    self.errors.push(error_at(
                        format!("unclosed `{}` (opened here)", entry.open.describe()),
                        entry.span,
                    ));
                }
                self.delim_stack.pop(); // Remove the matching opener.
            }
            None => {
                self.errors.push(error_at(
                    format!(
                        "unexpected `{}` with no matching opening `{}`",
                        close.describe(),
                        expected.describe()
                    ),
                    close_span,
                ));
            }
        }
    }

    fn check_unclosed_delims(&mut self) {
        for entry in self.delim_stack.drain(..) {
            self.errors.push(error_at(
                format!(
                    "unclosed `{}` at end of file (opened here)",
                    entry.open.describe()
                ),
                entry.span,
            ));
        }
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
                // Try `TYPE IDENT = expr` (explicit declaration). Backtrack
                // on failure so ordinary expressions still parse.
                let save_pos = self.pos;
                let save_errs = self.errors.len();
                if let Some(decl) = self.try_parse_explicit_decl() {
                    return decl;
                }
                self.pos = save_pos;
                self.errors.truncate(save_errs);

                // Recovery: `TYPE IDENT := expr` is invalid ZZ (should use
                // `IDENT := expr` or `IDENT: TYPE = expr`). Detect the
                // pattern and produce a usable Decl instead of degrading to
                // `Stmt::Expr(Ident("int"))` which the formatter would
                // garble.
                if self.peek_kind_at(0) == TokenKind::Ident
                    && !matches!(self.peek().text.as_str(), "true" | "false")
                    && self.peek_kind_at(1) == TokenKind::Ident
                    && self.peek_kind_at(2) == TokenKind::ColonEq
                {
                    let type_tok = self.peek().clone();
                    let type_name = type_tok.text.clone();
                    // Only treat as a type if the first identifier is a
                    // known primitive or a user-defined type name.  Primitive
                    // type keywords are lexed as Ident, so check for the
                    // common ones.
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

    fn parse_struct(&mut self) -> Stmt {
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

    fn parse_for(&mut self) -> Stmt {
        let for_tok = self.advance();
        let var = self
            .expect_ident()
            .unwrap_or_else(|| dummy_ident(for_tok.span));
        if !self.eat(TokenKind::In) {
            self.error_here("expected `in` after loop variable");
        }
        let iter = self.parse_expr();
        let body = self.parse_block();
        let span = for_tok.span.join(body.span);
        Stmt::For {
            var,
            iter: Box::new(iter),
            body,
            span,
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
        let alias = if self.eat(TokenKind::As) {
            self.expect_ident().map(|id| id.name)
        } else {
            None
        };
        let span = import_tok.span.join(self.previous().span);
        Stmt::Import { path, alias, span }
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

    fn parse_block(&mut self) -> Block {
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
        self.parse_pipe()
    }

    /// `a |> f(b)` — pipeline. Lowest precedence. The right side must be a
    /// function call or name; it receives the left side as its first
    /// argument (`a |> f(b)` desugars to `f(a, b)`).
    fn parse_pipe(&mut self) -> Expr {
        let mut left = self.parse_range();
        while self.at(TokenKind::PipeGt) {
            self.advance();
            let rhs = self.parse_range();
            left = self.desugar_pipe(left, rhs);
        }
        left
    }

    fn desugar_pipe(&mut self, lhs: Expr, rhs: Expr) -> Expr {
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
    fn parse_range(&mut self) -> Expr {
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
    fn parse_elvis(&mut self) -> Expr {
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

    fn parse_power(&mut self) -> Expr {
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

    fn parse_multiplicative(&mut self) -> Expr {
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

    /// Parse call arguments: `(pos1, pos2, name1: val1, name2: val2)`.
    /// Returns `(positional_args, named_args)`.
    fn parse_call_args(&mut self) -> (Vec<Expr>, Vec<(String, Expr)>) {
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
    /// the lexer: `Str (LBrace expr [: fmt_spec] RBrace Str)*`.
    fn parse_fmt_string(&mut self, first: Token) -> Expr {
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
            match self.peek_kind() {
                TokenKind::Str => {
                    let t = self.advance();
                    parts.push(FmtPart::Text(t.text));
                    end = t.span;
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
                // A string followed by `{` is an interpolated string:
                // `"Hello {name}"` lexes as Str, LBrace, expr, RBrace, Str...
                // But if the `{` starts a match arm block (followed by patterns
                // and `=>`), treat it as a plain string.
                if self.at(TokenKind::LBrace) && !self.looks_like_match_arm_block() {
                    self.parse_fmt_string(tok)
                } else {
                    Expr::Str {
                        value: tok.text,
                        span: tok.span,
                    }
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
                // Struct construction: `Point{ x: 1 }` or `Point { x: 1 }`.
                // A `{` is treated as a struct literal when it is adjacent
                // (no leading trivia) or when its contents look like fields
                // (`ident : ...`). Otherwise the `{` belongs to an enclosing
                // block (`if x { ... }`, `while x { ... }`, if-let values).
                if self.at(TokenKind::LBrace)
                    && (self.peek().leading.is_empty()
                        || (self.peek_kind_at(1) == TokenKind::Ident
                            && self.peek_kind_at(2) == TokenKind::Colon))
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

    fn parse_struct_init(&mut self, parts: Vec<String>, start: Span) -> Expr {
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
    fn parse_list_comp_body(&mut self, lbracket: Token, first: Expr) -> Expr {
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

    /// Parse a dotted identifier: `a`, `a.b`, `a.b.c`, ...
    /// Returns `Vec<String>` of parts.
    fn parse_dotted_ident(&mut self) -> Vec<String> {
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

    /// Peek ahead after a `{` to see if it starts a match arm block.
    /// Returns true if the content looks like patterns followed by `=>`.
    fn looks_like_match_arm_block(&self) -> bool {
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
                assert_eq!(name, &vec!["add".to_string()]);
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
        assert!(
            parsed.errors.len() >= 1,
            "expected at least 1 error for unclosed paren, got {}",
            parsed.errors.len()
        );
        // The diagnostic message should mention the missing `)`.
        let msgs: Vec<_> = parsed.errors.iter().map(|e| e.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains(')')),
            "expected a message mentioning `)`, got: {msgs:?}"
        );
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

    // --- indexing & slicing -------------------------------------------------

    #[test]
    fn parses_index() {
        let p = parse_ok("x := arr[0]");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Index { obj, index, .. },
                ..
            } => {
                assert!(matches!(obj.as_ref(), E::Ident { name, .. } if name == "arr"));
                assert!(matches!(index.as_ref(), E::Int { value: 0, .. }));
            }
            other => panic!("expected Index, got {other:?}"),
        }
    }

    #[test]
    fn parses_dict_index() {
        let p = parse_ok("x := ages[\"key\"]");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Index { index, .. },
                ..
            } => assert!(matches!(index.as_ref(), E::Str { value, .. } if value == "key")),
            other => panic!("expected Index, got {other:?}"),
        }
    }

    #[test]
    fn parses_slice_variants() {
        for (src, start, end) in [
            ("x := s[1:3]", Some(1), Some(3)),
            ("x := s[:3]", None, Some(3)),
            ("x := s[1:]", Some(1), None),
            ("x := s[:]", None, None),
        ] {
            let p = parse_ok(src);
            match &p.stmts[0] {
                Stmt::Decl {
                    value:
                        E::Slice {
                            start: st, end: en, ..
                        },
                    ..
                } => {
                    let st_v = st.as_ref().map(|e| match e.as_ref() {
                        E::Int { value, .. } => *value,
                        other => panic!("expected int start, got {other:?}"),
                    });
                    let en_v = en.as_ref().map(|e| match e.as_ref() {
                        E::Int { value, .. } => *value,
                        other => panic!("expected int end, got {other:?}"),
                    });
                    assert_eq!(st_v, start, "start of `{src}`");
                    assert_eq!(en_v, end, "end of `{src}`");
                }
                other => panic!("expected Slice for `{src}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn parses_index_on_path_and_call() {
        let p = parse_ok("x := ns.arr[0]");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Index { obj, .. },
                ..
            } => assert!(matches!(obj.as_ref(), E::Path { parts, .. } if parts == &["ns", "arr"])),
            other => panic!("expected Index, got {other:?}"),
        }
        let p = parse_ok("x := make()[0]");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Index { obj, .. },
                ..
            } => assert!(matches!(obj.as_ref(), E::Call { .. })),
            other => panic!("expected Index, got {other:?}"),
        }
    }

    #[test]
    fn parses_index_assign() {
        let p = parse_ok("arr[0] = 5");
        match &p.stmts[0] {
            Stmt::Assign { target, value, .. } => {
                assert!(matches!(target, E::Index { .. }));
                assert!(matches!(value, E::Int { value: 5, .. }));
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn missing_close_bracket_reports_error() {
        let parsed = parse("x := arr[0");
        assert_eq!(parsed.errors.len(), 1);
    }

    // --- pipeline -----------------------------------------------------------

    #[test]
    fn pipe_desugars_to_call_with_lhs_first() {
        let p = parse_ok("x := a |> f(b)");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { callee, args, .. },
                ..
            } => {
                assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], E::Ident { name, .. } if name == "a"));
                assert!(matches!(&args[1], E::Ident { name, .. } if name == "b"));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn pipe_bare_name_becomes_call() {
        let p = parse_ok("x := a |> f");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { callee, args, .. },
                ..
            } => {
                assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn pipe_empty_call_becomes_call() {
        let p = parse_ok("x := a |> f()");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { callee, args, .. },
                ..
            } => {
                assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn pipe_chains_left_assoc() {
        let p = parse_ok("x := a |> f(b) |> g(c)");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { callee, args, .. },
                ..
            } => {
                assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "g"));
                assert_eq!(args.len(), 2);
                // First arg is the previous pipe result: f(a, b).
                match &args[0] {
                    E::Call { callee, args, .. } => {
                        assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
                        assert_eq!(args.len(), 2);
                    }
                    other => panic!("expected nested Call, got {other:?}"),
                }
                assert!(matches!(&args[1], E::Ident { name, .. } if name == "c"));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn pipe_path_callee() {
        let p = parse_ok("x := a |> ns.f(b)");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { callee, args, .. },
                ..
            } => {
                assert!(matches!(callee.as_ref(), E::Path { parts, .. } if parts == &["ns", "f"]));
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn pipe_non_call_rhs_errors() {
        let parsed = parse("x := a |> 5");
        assert_eq!(parsed.errors.len(), 1);
    }

    #[test]
    fn pipe_lowest_precedence() {
        // `a + b |> f` pipes the whole sum.
        let p = parse_ok("x := a + b |> f");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { args, .. },
                ..
            } => {
                assert!(matches!(&args[0], E::Binary { .. }));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    // --- struct init with space --------------------------------------------

    #[test]
    fn struct_init_with_space() {
        let p = parse_ok("p := Point { x: 1, y: 2 }");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::StructInit { name, fields, .. },
                ..
            } => {
                assert_eq!(name, "Point");
                assert_eq!(fields.len(), 2);
            }
            other => panic!("expected StructInit, got {other:?}"),
        }
    }

    #[test]
    fn struct_init_no_space_still_works() {
        let p = parse_ok("p := Point{ x: 1 }");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::StructInit { name, .. },
                ..
            } => assert_eq!(name, "Point"),
            other => panic!("expected StructInit, got {other:?}"),
        }
    }

    #[test]
    fn if_block_not_struct_init() {
        // `if x { ... }` must stay a block, not become `x{...}`.
        let p = parse_ok("if x { y := 1 }");
        match &p.stmts[0] {
            Stmt::Expr(E::If { cond, then, .. }) => {
                assert!(matches!(cond.as_ref(), E::Ident { name, .. } if name == "x"));
                assert_eq!(then.stmts.len(), 1);
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn while_block_not_struct_init() {
        let p = parse_ok("while x { y := 1 }");
        match &p.stmts[0] {
            Stmt::Expr(E::While { cond, .. }) => {
                assert!(matches!(cond.as_ref(), E::Ident { name, .. } if name == "x"));
            }
            other => panic!("expected While, got {other:?}"),
        }
    }

    #[test]
    fn if_let_value_block_not_struct_init() {
        // `if let .some(v) = x { ... }` — the `{` after the value is the
        // then-block, not a struct literal.
        let p = parse_ok("if let .some(v) = x { v }");
        match &p.stmts[0] {
            Stmt::Expr(E::IfLet { value, then, .. }) => {
                assert!(matches!(value.as_ref(), E::Ident { name, .. } if name == "x"));
                assert_eq!(then.stmts.len(), 1);
            }
            other => panic!("expected IfLet, got {other:?}"),
        }
    }

    #[test]
    fn struct_init_in_if_cond() {
        // Struct literal directly in a condition still parses.
        let p = parse_ok("if Point { x: 1 } == p { y := 1 }");
        match &p.stmts[0] {
            Stmt::Expr(E::If { cond, .. }) => {
                assert!(matches!(cond.as_ref(), E::Binary { .. }));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn malformed_struct_recovers_without_hanging() {
        // A dotted struct name is now VALID (cross-module struct).
        // The parser should accept it and not spin.
        let parsed = parse("struct shapes.Point { x: int }\nz := 1");
        assert!(
            parsed.errors.is_empty(),
            "dotted struct name should be valid: {:?}",
            parsed.errors
        );
        assert_eq!(parsed.program.stmts.len(), 2);
    }

    #[test]
    fn malformed_struct_init_recovers_without_hanging() {
        // Garbage inside a struct literal must not spin the parser.
        let parsed = parse("p := Point{ 123 }\nz := 1");
        assert!(!parsed.errors.is_empty(), "expected parse errors");
        assert_eq!(parsed.program.stmts.len(), 2);
    }

    // --- method calls -------------------------------------------------------

    #[test]
    fn method_call_parses_as_path_callee() {
        let p = parse_ok("z := p.dist()");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { callee, args, .. },
                ..
            } => {
                assert!(matches!(
                    callee.as_ref(),
                    E::Path { parts, .. } if parts == &["p", "dist"]
                ));
                assert!(args.is_empty());
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn method_call_with_args() {
        let p = parse_ok("z := p.move(1, 2)");
        match &p.stmts[0] {
            Stmt::Decl {
                value: E::Call { args, .. },
                ..
            } => assert_eq!(args.len(), 2),
            other => panic!("expected Call, got {other:?}"),
        }
    }
}
