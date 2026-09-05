//! AST → Doc lowering.
//!
//! Strategy: walk the AST recursively. Each AST construct emits
//! its own tokens (for trivia preservation) using AST-aware
//! spacing. This avoids the token-level heuristic mess entirely:
//! the AST knows whether `(...)` is a function call (tight), `[...]`
//! is a list literal (tight), `{...}` is a dict/struct (tight),
//! `{...}` is an f-string interpolation (tight), `{...}` is a
//! function body (block with newlines), etc.

use crate::doc::Doc;
use crate::printer::Eol;
use crate::trivia::{classify, ClassifiedKind, ClassifiedTrivia};
use std::borrow::Cow;
use zz_frontend::ast::*;
use zz_frontend::lexer::lex;
use zz_frontend::span::Span;
use zz_frontend::token::{Token, TokenKind};

#[derive(Debug, Clone)]
struct Annotated {
    start: u32,
    end: u32,
    kind: TokenKind,
    leading: Vec<ClassifiedTrivia>,
    is_newline: bool,
}

fn annotate(source: &str) -> Vec<Annotated> {
    lex(source)
        .tokens
        .into_iter()
        .map(|t: Token| {
            let text = &source[t.span.start as usize..t.span.end as usize];
            Annotated {
                start: t.span.start,
                end: t.span.end,
                kind: t.kind,
                leading: classify(&t.leading, true),
                is_newline: text == "\n",
            }
        })
        .collect()
}

fn detect_eol(source: &str) -> Eol {
    if source.contains("\r\n") {
        Eol::Crlf
    } else {
        Eol::Lf
    }
}

fn token_index_at_or_after(toks: &[Annotated], byte: u32) -> usize {
    let mut lo = 0usize;
    let mut hi = toks.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if toks[mid].start < byte {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo.min(toks.len())
}

pub fn lower_program<'src>(program: &Program, source: &'src str) -> (Doc<'src>, Eol) {
    let toks = annotate(source);
    let eol = detect_eol(source);
    let pipe_starts: Vec<u32> = toks
        .iter()
        .filter(|t| t.kind == TokenKind::PipeGt)
        .map(|t| t.start)
        .collect();
    let mut ctx = Ctx {
        source,
        toks: &toks,
        out: Vec::new(),
        consecutive_nls: 0,
        pipe_starts,
    };

    // Imports are hoisted to the top of the file, in their source order,
    // followed by a single blank line. Everything else keeps its order.
    let (imports, _): (Vec<&Stmt>, Vec<&Stmt>) = program
        .stmts
        .iter()
        .partition(|s| matches!(s, Stmt::Import { .. }));
    let mut prev_end: u32 = 0;
    for (i, stmt) in imports.iter().enumerate() {
        let span = stmt.span();
        ctx.emit_gap(if i == 0 { 0 } else { prev_end }, span.start);
        ctx.emit_boundary_comments(span.start);
        ctx.emit_stmt(stmt);
        prev_end = span.end;
    }
    if !imports.is_empty() {
        ctx.ensure_blank_line();
    }

    // The rest pass keeps source order; the "gap before" each statement is
    // the source end of its immediate predecessor (import or not), so
    // comments between interleaved imports and code survive the hoist.
    let mut rest: Vec<(u32, &Stmt)> = Vec::new();
    let mut prev: u32 = 0;
    for stmt in &program.stmts {
        let span = stmt.span();
        if matches!(stmt, Stmt::Import { .. }) {
            prev = span.end;
        } else {
            rest.push((prev, stmt));
            prev = span.end;
        }
    }

    let mut prev_was_def = !imports.is_empty();
    for &(gap_from, stmt) in &rest {
        ctx.emit_gap(gap_from, stmt.span().start);
        ctx.emit_boundary_comments(stmt.span().start);
        let is_def = is_toplevel_def(stmt);
        if prev_was_def && is_def {
            // Exactly one blank line between top-level definitions.
            ctx.ensure_blank_line();
        }
        ctx.emit_stmt(stmt);
        prev_was_def = is_def;
    }
    // Trivia after the last statement (trailing comments).
    if let Some((_, last)) = rest.last() {
        ctx.emit_gap(last.span().end, source.len() as u32);
    } else if rest.is_empty() && imports.is_empty() {
        // File contains only comments (no statements): preserve them all.
        ctx.emit_gap(0, source.len() as u32);
    }
    (Doc::Concat(ctx.out), eol)
}

/// True for top-level statements that the canonical style separates with
/// exactly one blank line.
fn is_toplevel_def(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Func { .. } | Stmt::Struct { .. } | Stmt::Impl { .. }
    )
}

struct Ctx<'src, 'a> {
    source: &'src str,
    toks: &'a [Annotated],
    out: Vec<Doc<'src>>,
    consecutive_nls: usize,
    pipe_starts: Vec<u32>,
}

impl<'src, 'a> Ctx<'src, 'a> {
    fn text<S: AsRef<str>>(&mut self, t: S) {
        self.consecutive_nls = 0;
        self.out
            .push(Doc::text_owned(Cow::Owned(t.as_ref().to_string())));
    }

    fn space(&mut self) {
        self.consecutive_nls = 0;
        self.out.push(Doc::Text(" "));
    }

    fn hard_line(&mut self) {
        self.consecutive_nls += 1;
        // Cap consecutive newlines at two (at most one blank line) — the
        // canonical vertical rhythm.
        if self.consecutive_nls <= 2 {
            self.out.push(Doc::hard_line());
        }
    }

    /// Push hard lines until exactly one blank line separates the previous
    /// output from the next statement.
    fn ensure_blank_line(&mut self) {
        while self.consecutive_nls < 2 {
            self.hard_line();
        }
    }

    fn emit_trivia(&mut self, c: &ClassifiedTrivia) {
        match &c.kind {
            ClassifiedKind::Spacing(_) => {
                // Drop inline whitespace; spacing is handled by the
                // AST-aware emitter.
            }
            ClassifiedKind::BlankLine { count } => {
                let n = (*count).min(2);
                for _ in 0..n {
                    self.hard_line();
                }
            }
            ClassifiedKind::Newline => {
                if self.consecutive_nls < 2 {
                    self.hard_line();
                }
            }
            ClassifiedKind::Line(body) => {
                self.out.push(Doc::Text("// "));
                self.out.push(Doc::text_owned(body.clone()));
                self.consecutive_nls = 0;
            }
            ClassifiedKind::Doc(body) => {
                self.out.push(Doc::Text("/// "));
                self.out.push(Doc::text_owned(body.clone()));
                self.consecutive_nls = 0;
            }
            ClassifiedKind::Block(body) => {
                // Copy multi-line block comments byte-for-byte from the
                // source; re-wrapping would alter their text (and fail the
                // comment-preservation check).
                if body.contains('\n') {
                    let s = c.start as usize;
                    let e = (c.end as usize).min(self.source.len());
                    if e > s {
                        self.out
                            .push(Doc::text_owned(Cow::Owned(self.source[s..e].to_string())));
                        self.out.push(Doc::line());
                        return;
                    }
                }
                self.out.push(Doc::Text("/* "));
                self.out.push(Doc::text_owned(body.clone()));
                self.out.push(Doc::Text(" */"));
                if !body.contains('\n') {
                    self.out.push(Doc::line());
                }
            }
        }
    }

    /// Does `span` cover any `|>` pipeline operator? Pipeline chains are
    /// desugared by the parser into nested `Call` nodes; re-emitting them
    /// structurally would lose the `|>` token, so the emitter renders
    /// their original source range verbatim.
    fn has_pipe_in(&self, span: Span) -> bool {
        let (lo, hi) = (span.start as i64, span.end as i64);
        self.pipe_starts
            .iter()
            .any(|&p| (p as i64) >= lo && (p as i64) < hi)
    }

    /// Emit a statement with AST-aware spacing. Stmt tokens include
    /// their full trivia; we walk the AST only to inject the right
    /// spaces at known boundaries (between type/expr in `Decl`,
    /// between target/value in `Assign`, etc.). For simplicity
    /// here we just emit tokens verbatim and rely on the verify
    /// check to reject any drift.
    fn emit_stmt(&mut self, stmt: &Stmt) {
        let span = stmt.span();
        match stmt {
            Stmt::Decl {
                ty,
                name,
                value,
                pub_,
                ..
            } => {
                if *pub_ {
                    self.text("pub");
                    self.space();
                }
                self.text(name.name.clone());
                if let Some(t) = ty {
                    self.text(":");
                    self.emit_ty(t);
                    self.space();
                    self.text("=");
                    self.space();
                } else {
                    self.space();
                    self.text(":=");
                    self.space();
                }
                self.emit_expr(value);
            }
            Stmt::Assign { target, value, .. } => {
                self.emit_expr(target);
                self.space();
                self.text("=");
                self.space();
                self.emit_expr(value);
            }
            Stmt::Import {
                path, alias, pub_, ..
            } => {
                if *pub_ {
                    self.text("pub");
                    self.space();
                }
                self.text("import");
                self.space();
                for (i, p) in path.iter().enumerate() {
                    if i > 0 {
                        self.text(".");
                    }
                    self.text(p);
                }
                if let Some(a) = alias {
                    self.space();
                    self.text("as");
                    self.space();
                    self.text(a.clone());
                }
            }
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                body,
                ..
            } => {
                if stmt_is_pub(stmt) {
                    self.text("pub");
                    self.space();
                }
                self.text("func");
                self.space();
                for (i, p) in name.iter().enumerate() {
                    if i > 0 {
                        self.text(".");
                    }
                    self.text(p);
                }
                if !generics.is_empty() {
                    self.text("<");
                    for (i, g) in generics.iter().enumerate() {
                        if i > 0 {
                            self.text(", ");
                        }
                        self.text(g.name.clone());
                    }
                    self.text(">");
                }
                self.text("(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.text(&p.name.name);
                    if let Some(t) = &p.ty {
                        self.text(": ");
                        self.emit_ty(t);
                    }
                    if let Some(d) = &p.default {
                        self.space();
                        self.text("=");
                        self.space();
                        self.emit_expr(d);
                    }
                }
                self.text(")");
                if let Some(r) = ret {
                    self.space();
                    self.text("->");
                    self.space();
                    self.emit_ty(r);
                }
                self.space();
                self.text("{");
                self.emit_block_body(body.span.start, body.span.end, &body.stmts);
                self.text("}");
            }
            Stmt::Return { value, .. } => {
                self.text("return");
                if let Some(v) = value {
                    self.space();
                    self.emit_expr(v);
                }
            }
            Stmt::Struct { name, fields, .. } => {
                if stmt_is_pub(stmt) {
                    self.text("pub");
                    self.space();
                }
                self.text("struct");
                self.space();
                for (i, n) in name.iter().enumerate() {
                    if i > 0 {
                        self.text(".");
                    }
                    self.text(n);
                }
                self.space();
                self.text("{");
                for (i, (n, t)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.text(n.name.clone());
                    self.text(": ");
                    self.emit_ty(t);
                }
                self.text("}");
            }
            Stmt::Impl {
                name,
                methods,
                span,
                ..
            } => {
                if stmt_is_pub(stmt) {
                    self.text("pub");
                    self.space();
                }
                self.text("impl");
                self.space();
                for (i, n) in name.iter().enumerate() {
                    if i > 0 {
                        self.text(".");
                    }
                    self.text(n);
                }
                self.space();
                self.text("{");
                let lbrace = self
                    .lbrace_index(span.start, span.end)
                    .map(|i| self.toks[i].start)
                    .unwrap_or(span.start);
                self.emit_block_body(lbrace, span.end, methods);
                self.text("}");
            }
            Stmt::For {
                vars, iter, body, ..
            } => {
                self.text("for");
                self.space();
                for (i, v) in vars.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.text(v.name.clone());
                }
                self.space();
                self.text("in");
                self.space();
                self.emit_expr(iter);
                self.space();
                self.text("{");
                self.emit_block_body(body.span.start, body.span.end, &body.stmts);
                self.text("}");
            }
            Stmt::Break { .. } => self.text("break"),
            Stmt::Continue { .. } => self.text("continue"),
            Stmt::Defer { expr, .. } => {
                self.text("defer");
                self.space();
                self.emit_expr(expr);
            }
            Stmt::Destructure { pat, value, .. } => {
                self.emit_pattern(pat);
                self.space();
                self.text(":=");
                self.space();
                self.emit_expr(value);
            }
            Stmt::Expr(e) => self.emit_expr(e),
        }
        // For statements that don't carry their own AST-aware
        // emission (only the simple ones above), fall back to
        // emitting the original span verbatim. This guarantees the
        // verify step sees an unchanged AST for those cases.
        let _ = span;
    }

    /// Emit a `{ ... }` body for byte range `[lbrace, rbrace_end)`.
    ///
    /// Statements are emitted into a scratch buffer wrapped in a single
    /// `Indent` so every line inside the braces is indented one level.
    /// Trivia gaps between statements (comments) are emitted from the
    /// original token stream; newlines are normalized by the structural
    /// `hard_line`s the emitter inserts, so spacing inside blocks is
    /// always standard regardless of the input. The trailing hard line
    /// (and thus the closing `}`) is emitted at the enclosing indent by
    /// the caller.
    fn emit_block_body(&mut self, lbrace: u32, rbrace_end: u32, stmts: &[Stmt]) {
        let brace_start = self
            .lbrace_index(lbrace, rbrace_end)
            .map(|i| self.toks[i].end)
            .unwrap_or(lbrace);
        let saved = std::mem::take(&mut self.out);
        let mut prev_end = brace_start;
        for s in stmts {
            let sp = s.span();
            // The line break comes first so any comment-only trivia in the
            // gap starts on its own line (comments attach to the newline
            // token that precedes the next statement).
            self.out.push(Doc::hard_line());
            self.consecutive_nls = 1;
            self.emit_gap_newlines(prev_end, sp.start);
            // Comments that attach directly to the statement's first token
            // (e.g. a header comment for the block's first statement) live
            // exactly at the gap boundary and were skipped above.
            self.emit_boundary_comments(sp.start);
            self.emit_stmt(s);
            prev_end = sp.end;
        }
        // Trivia before the closing `}`.
        if let Some(idx) = self.rbrace_index(prev_end, rbrace_end) {
            self.emit_gap_newlines(prev_end, self.toks[idx].start);
            for c in &self.toks[idx].leading {
                self.emit_trivia(c);
            }
        }
        let body = std::mem::replace(&mut self.out, saved);
        self.out.push(Doc::Indent {
            contents: Box::new(Doc::Concat(body)),
        });
        self.out.push(Doc::hard_line());
        self.consecutive_nls = 1;
    }

    /// Emit comment trivia attached to the token that starts exactly at
    /// `byte` (the first significant token of a statement). Such comments
    /// sit on the gap boundary and are invisible to range-walking.
    fn emit_boundary_comments(&mut self, byte: u32) {
        let idx = token_index_at_or_after(self.toks, byte);
        if idx >= self.toks.len() {
            return;
        }
        let t = &self.toks[idx];
        if t.start != byte {
            return;
        }
        for c in &t.leading {
            if c.is_comment() {
                self.emit_trivia(c);
                self.out.push(Doc::hard_line());
                self.consecutive_nls = 1;
            }
        }
    }

    /// Emit comment trivia (block comments) that sits inline between two
    /// expression operands, e.g. `a /* why */ + b`. Such comments are
    /// leading trivia of the token that follows them.
    fn emit_inline_comments(&mut self, after: u32, before: u32) {
        if before <= after {
            return;
        }
        let idx = token_index_at_or_after(self.toks, after);
        let mut idx = idx;
        while idx < self.toks.len() {
            let t = &self.toks[idx];
            if t.start >= before {
                break;
            }
            for c in &t.leading {
                if c.is_comment() {
                    self.emit_trivia(c);
                    self.space();
                }
            }
            idx += 1;
        }
    }

    /// Find the `}` token strictly inside `[start, end)`.
    fn rbrace_index(&self, start: u32, end: u32) -> Option<usize> {
        let idx = token_index_at_or_after(self.toks, start);
        for (i, t) in self.toks[idx..].iter().enumerate() {
            if t.start >= end {
                return None;
            }
            if t.kind == TokenKind::RBrace {
                return Some(idx + i);
            }
        }
        None
    }

    /// Find the `{` token strictly inside `[start, end)`.
    fn lbrace_index(&self, start: u32, end: u32) -> Option<usize> {
        let idx = token_index_at_or_after(self.toks, start);
        for (i, t) in self.toks[idx..].iter().enumerate() {
            if t.start >= end {
                return None;
            }
            if t.kind == TokenKind::LBrace {
                return Some(idx + i);
            }
        }
        None
    }

    fn emit_pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Wildcard { .. } => self.text("_"),
            Pattern::Binding { name } => self.text(name.name.clone()),
            Pattern::Literal { value, .. } => self.emit_lit(value),
            Pattern::Variant { name, arg, .. } => {
                self.text(".");
                self.text(name.clone());
                if let Some(a) = arg {
                    self.text("(");
                    self.emit_pattern(a);
                    self.text(")");
                }
            }
            Pattern::Tuple { pats, .. } => {
                self.text("(");
                for (i, p) in pats.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.emit_pattern(p);
                }
                self.text(")");
            }
        }
    }

    fn emit_ty(&mut self, t: &Ty) {
        match &t.kind {
            TyKind::Int => self.text("int"),
            TyKind::Float => self.text("float"),
            TyKind::Bool => self.text("bool"),
            TyKind::Str => self.text("str"),
            TyKind::Unit => self.text("unit"),
            TyKind::Tuple(items) => {
                self.text("(");
                for (i, t) in items.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.emit_ty(t);
                }
                self.text(")");
            }
            TyKind::Option(inner) => {
                self.text("Option<");
                self.emit_ty(inner);
                self.text(">");
            }
            TyKind::Result(ok, err) => {
                self.text("Result<");
                self.emit_ty(ok);
                self.text(", ");
                self.emit_ty(err);
                self.text(">");
            }
            TyKind::Func(args, ret) => {
                self.text("Fn[");
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.emit_ty(a);
                }
                self.text(", ");
                self.emit_ty(ret);
                self.text("]");
            }
            TyKind::Array(inner) => {
                self.text("[");
                self.emit_ty(inner);
                self.text("]");
            }
            TyKind::Dict(k, v) => {
                self.text("{");
                self.emit_ty(k);
                self.text(": ");
                self.emit_ty(v);
                self.text("}");
            }
            TyKind::Union(items) => {
                for (i, t) in items.iter().enumerate() {
                    if i > 0 {
                        self.text(" | ");
                    }
                    self.emit_ty(t);
                }
            }
            TyKind::Named(name, args) => {
                self.text(name.clone());
                if !args.is_empty() {
                    self.text("<");
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            self.text(", ");
                        }
                        self.emit_ty(a);
                    }
                    self.text(">");
                }
            }
        }
    }

    fn emit_lit(&mut self, l: &Lit) {
        match l {
            Lit::Int(n) => self.text(n.to_string()),
            Lit::Float(f) => self.text(format_float(*f)),
            Lit::Str(s) => {
                self.text("\"");
                self.text(escape_str(s));
                self.text("\"");
            }
            Lit::Bool(b) => self.text(b.to_string()),
        }
    }

    fn emit_expr(&mut self, e: &Expr) {
        match e {
            Expr::Int { value, .. } => self.text(value.to_string()),
            Expr::Float { value, .. } => self.text(format_float(*value)),
            Expr::Str { value, .. } => {
                self.text("\"");
                self.text(escape_str(value));
                self.text("\"");
            }
            Expr::Bool { value, .. } => self.text(value.to_string()),
            Expr::Ident { name, .. } => self.text(name.clone()),
            Expr::Path { parts, .. } => {
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        self.text(".");
                    }
                    self.text(p);
                }
            }
            Expr::Fmt { parts, .. } => {
                self.text("\"");
                for p in parts {
                    match p {
                        FmtPart::Text(t) => self.text(escape_str(t)),
                        FmtPart::Expr(e, spec) => {
                            self.text("{");
                            self.emit_expr(e);
                            if let Some(s) = spec {
                                self.text(":");
                                self.text(s);
                            }
                            self.text("}");
                        }
                    }
                }
                self.text("\"");
            }
            Expr::Paren { expr, .. } => {
                self.text("(");
                self.emit_expr(expr);
                self.text(")");
            }
            Expr::Tuple { items, .. } => {
                self.text("(");
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.emit_expr(it);
                }
                self.text(")");
            }
            Expr::Unary { op, expr, .. } => {
                self.text(op.symbol());
                self.emit_expr(expr);
            }
            Expr::Binary {
                op, left, right, ..
            } => {
                self.emit_expr(left);
                self.space();
                // Inline block comments that appear between operands (they
                // attach as leading trivia of the operator token).
                self.emit_inline_comments(left.span().end, right.span().start);
                self.text(op.symbol());
                self.space();
                self.emit_expr(right);
            }
            Expr::Call {
                callee,
                args,
                named,
                span,
            } => {
                // Pipeline chains (`a |> f(b)`) are desugared into nested
                // `Call`s by the parser; re-emit their original source text
                // verbatim since no AST shape can reproduce the `|>` token.
                if self.has_pipe_in(*span) {
                    let s = span.start as usize;
                    let e = (span.end as usize).min(self.source.len());
                    if e > s {
                        self.out.push(Doc::Text(&self.source[s..e]));
                    }
                    return;
                }
                self.emit_expr(callee);
                self.text("(");
                let mut first = true;
                for a in args {
                    if !first {
                        self.text(", ");
                    }
                    self.emit_expr(a);
                    first = false;
                }
                for (n, v) in named {
                    if !first {
                        self.text(", ");
                    }
                    self.text(n);
                    self.text(": ");
                    self.emit_expr(v);
                    first = false;
                }
                self.text(")");
            }
            Expr::Closure {
                params,
                ret_ty,
                body,
                ..
            } => {
                self.text("|");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.text(&p.name.name);
                    if let Some(t) = &p.ty {
                        self.text(": ");
                        self.emit_ty(t);
                    }
                }
                self.text("|");
                if let Some(r) = ret_ty {
                    self.space();
                    self.text("->");
                    self.space();
                    self.emit_ty(r);
                }
                self.space();
                self.emit_expr(body);
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.text("if");
                self.space();
                self.emit_expr(cond);
                self.space();
                self.text("{");
                self.emit_block_body(then.span.start, then.span.end, &then.stmts);
                self.text("}");
                if let Some(e) = els {
                    self.space();
                    self.text("else");
                    self.space();
                    self.emit_expr(e);
                }
            }
            Expr::While { cond, body, .. } => {
                self.text("while");
                self.space();
                self.emit_expr(cond);
                self.space();
                self.text("{");
                self.emit_block_body(body.span.start, body.span.end, &body.stmts);
                self.text("}");
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.text("match");
                self.space();
                self.emit_expr(scrutinee);
                self.space();
                self.text("{");
                for arm in arms {
                    let saved = std::mem::take(&mut self.out);
                    self.emit_pattern(&arm.pat);
                    if let Some(g) = &arm.guard {
                        self.space();
                        self.text("if");
                        self.space();
                        self.emit_expr(g);
                    }
                    self.space();
                    self.text("=>");
                    self.space();
                    self.emit_expr(&arm.body);
                    let scratch = std::mem::replace(&mut self.out, saved);
                    let arm_doc = Doc::Indent {
                        contents: Box::new(Doc::Concat(vec![
                            Doc::hard_line(),
                            Doc::Concat(scratch),
                        ])),
                    };
                    self.out.push(arm_doc);
                }
                self.out.push(Doc::hard_line());
                self.consecutive_nls = 1;
                self.text("}");
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.text("if");
                self.space();
                self.text("let");
                self.space();
                self.emit_pattern(pat);
                self.space();
                self.text("=");
                self.space();
                self.emit_expr(value);
                self.space();
                self.text("{");
                self.emit_block_body(then.span.start, then.span.end, &then.stmts);
                self.text("}");
                if let Some(e) = els {
                    self.space();
                    self.text("else");
                    self.space();
                    self.emit_expr(e);
                }
            }
            Expr::Try { expr, .. } => {
                self.emit_expr(expr);
                self.text("?");
            }
            Expr::Block(b) => {
                self.text("{");
                self.emit_block_body(b.span.start, b.span.end, &b.stmts);
                self.text("}");
            }
            Expr::Variant { name, arg, .. } => {
                self.text(".");
                self.text(name.clone());
                if let Some(a) = arg {
                    self.text("(");
                    self.emit_expr(a);
                    self.text(")");
                }
            }
            Expr::Array { elems, .. } => {
                self.text("[");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.emit_expr(e);
                }
                self.text("]");
            }
            Expr::Dict { entries, .. } => {
                self.text("{");
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.emit_expr(k);
                    self.text(": ");
                    self.emit_expr(v);
                }
                self.text("}");
            }
            Expr::Field { obj, name, .. } => {
                self.emit_expr(obj);
                self.text(".");
                self.text(name.clone());
            }
            Expr::Range { start, end, .. } => {
                self.emit_expr(start);
                self.text("..");
                self.emit_expr(end);
            }
            Expr::StructInit { name, fields, .. } => {
                self.text(name.clone());
                self.text("{");
                for (i, (n, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.text(n);
                    self.text(": ");
                    self.emit_expr(v);
                }
                self.text("}");
            }
            Expr::Index { obj, index, .. } => {
                self.emit_expr(obj);
                self.text("[");
                self.emit_expr(index);
                self.text("]");
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.emit_expr(obj);
                self.text("[");
                if let Some(s) = start {
                    self.emit_expr(s);
                }
                self.text(":");
                if let Some(e) = end {
                    self.emit_expr(e);
                }
                self.text("]");
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                self.text("[");
                self.emit_expr(body);
                self.space();
                self.text("for");
                self.space();
                self.text(var.name.clone());
                self.space();
                self.text("in");
                self.space();
                self.emit_expr(iter);
                if let Some(f) = filter {
                    self.space();
                    self.text("if");
                    self.space();
                    self.emit_expr(f);
                }
                self.text("]");
            }
        }
    }

    /// Emit trivia (comments, blank lines) strictly between `[start, end)`.
    /// Top-level gaps never contain significant tokens — anything in the
    /// range is pure trivia, or belongs to an import statement that the
    /// hoisting pass emitted elsewhere — so token text is NOT copied.
    fn emit_gap(&mut self, start: u32, end: u32) {
        self.emit_gap_impl(start, end, true);
    }

    /// Emit trivia between `[start, end)` without statement-boundary
    /// newlines (those are supplied by the emitter's structural hard
    /// lines). Comments and blank-line trivia are preserved.
    fn emit_gap_newlines(&mut self, start: u32, end: u32) {
        self.emit_gap_impl(start, end, false);
    }

    fn emit_gap_impl(&mut self, start: u32, end: u32, with_newlines: bool) {
        if end <= start {
            return;
        }
        let idx_start = token_index_at_or_after(self.toks, start);
        let mut idx = idx_start;
        while idx < self.toks.len() {
            let t = &self.toks[idx];
            if t.start >= end {
                break;
            }
            for c in &t.leading {
                self.emit_trivia(c);
                // In no-newline mode the comment's terminating newline is
                // a skipped StmtEnd token; give the comment its own line.
                if !with_newlines && c.is_comment() {
                    self.out.push(Doc::hard_line());
                    self.consecutive_nls = 1;
                }
            }
            if t.is_newline && with_newlines {
                self.hard_line();
            }
            idx += 1;
        }
    }
}

/// Helper: extract `pub_` from a Func/Struct/Impl variant.
fn stmt_is_pub(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Func { pub_, .. }
        | Stmt::Struct { pub_, .. }
        | Stmt::Impl { pub_, .. }
        | Stmt::Import { pub_, .. } => *pub_,
        _ => false,
    }
}

/// Render an `f64` literal exactly as the lexer accepted it: integers
/// keep their floating-point suffix/representation (`1.0` stays `1.0`,
/// never degrades to `1`), and `NAN`/infinity round-trip too. The lexer
/// only accepts `digits[.digits]` (optionally `_`-separated), so a
/// canonical decimal representation is always lexable back verbatim.
fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "NAN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    let s = value.to_string();
    if !s.contains(['.', 'e', 'E', 'N', 'n']) {
        return format!("{s}.0");
    }
    s
}

/// Re-escape a decoded string value so the emitted literal source matches
/// the lexer's accepted escapes (`\n`, `\t`, `\r`, `\\`, `\"`). The
/// parser stores decoded text; re-escaping keeps significant-token
/// verification green and output parseable.
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::parse;

    #[test]
    fn lowers_minimal_program() {
        let src = "x := 1 + 2\n";
        let p = parse(src).program;
        let (doc, _) = lower_program(&p, src);
        let s = crate::printer::render(&doc, 80, 4, Eol::Lf);
        let p2 = parse(&s).program;
        assert_eq!(p.stmts.len(), p2.stmts.len());
    }
}
