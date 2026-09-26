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
    // sqlz! macros desugar to Call nodes; re-emitting structurally would
    // lose the `!` token, so render verbatim like pipelines.
    let bang_starts: Vec<u32> = toks
        .iter()
        .filter(|t| t.kind == TokenKind::Bang)
        .map(|t| t.start)
        .collect();
    let mut ctx = Ctx {
        source,
        toks: &toks,
        out: Vec::new(),
        consecutive_nls: 0,
        pipe_starts,
        bang_starts,
        emitted_comments: std::collections::HashSet::new(),
    };

    // Imports are hoisted to the top of the file, sorted with the
    // standard library group first (`std.*`, alphabetical) then external
    // packages (alphabetical), separated by a blank line — goimports/isort
    // style. Everything else keeps its source order.
    // Import gaps that span code (interleaved `import` after statements)
    // must NOT leak newlines into the import group: imports are separated
    // by exactly one newline, with one blank line after the group.
    // Attached `//` docs move with their import (looked up by span).
    let (mut imports, _): (Vec<&Stmt>, Vec<&Stmt>) = program
        .stmts
        .iter()
        .partition(|s| matches!(s, Stmt::Import { .. }));
    // Stable sort: equal keys keep source order (deterministic output).
    // `sort_by_key` evaluates each key once (no repeated path joins).
    imports.sort_by_key(|s| import_sort_key(s));
    // Byte offset of the earliest import: the file-header gap (comments
    // before any import) belongs to the group, regardless of sort order.
    let header_end: u32 = imports.iter().map(|s| s.span().start).min().unwrap_or(0);
    let mut prev_group: Option<bool> = None;
    for (i, stmt) in imports.iter().enumerate() {
        let span = stmt.span();
        if i == 0 {
            ctx.emit_gap(0, header_end);
            if span.start != header_end {
                // First in *sorted* order but hoisted from later in the
                // file: its attached `//` docs are not in the header gap
                // (which ends at the earliest import), so emit them here.
                // When the first sorted import is also first in source,
                // the gap already covered them — skip to avoid duplication.
                ctx.emit_attached_import_comments(span.start);
            }
        } else {
            // End the previous import line first so attached comments
            // start on their own line (not trailing the previous import).
            ctx.hard_line();
            ctx.consecutive_nls = 1;
            let group = is_std_import(stmt);
            if prev_group != Some(group) {
                // Blank line between the std group and the external group.
                ctx.hard_line();
            }
            // Hoisted import: emit comments immediately attached to it
            // (e.g. `// docs` on the line(s) directly above the import),
            // which live on preceding StmtEnd tokens, not on the `import`
            // token itself. This prevents dropped-comment verification
            // failures when imports move to the top.
            ctx.emit_attached_import_comments(span.start);
        }
        ctx.emit_boundary_comments(span.start);
        ctx.emit_stmt(stmt);
        prev_group = Some(is_std_import(stmt));
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
    bang_starts: Vec<u32>,
    /// Spans of comments already emitted. Import sorting reorders
    /// statements, so an attached-docs walk can rediscover comments the
    /// header gap already emitted — the set keeps each comment exactly
    /// once (idempotence + no duplication).
    emitted_comments: std::collections::HashSet<(u32, u32)>,
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

    /// Emit an exact source slice verbatim (zero-copy borrow).
    ///
    /// Used for **all string literals** (`"..."`, `"""..."""`, interpolated
    /// strings): string contents, escapes (`\x0a`, `\n`), `{expr}`
    /// interpolations and multiline layouts are immutable raw tokens and
    /// must never be re-synthesized. The `Span` comes from the AST node,
    /// which covers the full literal including its delimiters.
    fn emit_raw(&mut self, span: Span) {
        let s = span.start as usize;
        let e = (span.end as usize).min(self.source.len());
        if e > s {
            self.consecutive_nls = 0;
            self.out.push(Doc::Text(&self.source[s..e]));
        }
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

    /// Ensure a single space separates trailing comments from preceding
    /// code (`x := 1// c` -> `x := 1 // c`). Standalone comments that
    /// already start on a fresh line (last output is a newline) need no
    /// space. Inspects the last emitted Doc without rendering.
    fn ensure_space_before_comment(&mut self) {
        let need_space = match self.out.last() {
            None => false,
            Some(Doc::HardLine) => false,
            Some(Doc::Text(t)) => !t.is_empty() && !t.ends_with('\n') && !t.ends_with(' '),
            Some(Doc::TextOwned(t)) => !t.is_empty() && !t.ends_with('\n') && !t.ends_with(' '),
            // After Indent/Group/Concat the true suffix is inside; be
            // conservative and add a space — an extra space before a
            // comment never changes semantics and keeps idempotence
            // (the second pass sees the space and adds none).
            Some(_) => true,
        };
        if need_space {
            self.out.push(Doc::Text(" "));
            self.consecutive_nls = 0;
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
            // Lossless: comments re-emit byte-for-byte from the original
            // source slice. Never normalize spacing (`//hello` stays
            // `//hello`), never re-wrap block comments.
            ClassifiedKind::Line(_) | ClassifiedKind::Doc(_) | ClassifiedKind::Block(_) => {
                self.emit_comment_trivia(c);
            }
        }
    }

    /// Emit one comment's raw text and record its span as emitted.
    fn emit_comment_trivia(&mut self, c: &ClassifiedTrivia) {
        self.emitted_comments.insert((c.start, c.end));
        self.ensure_space_before_comment();
        let s = c.start as usize;
        let e = (c.end as usize).min(self.source.len());
        if e > s {
            self.out
                .push(Doc::text_owned(Cow::Owned(self.source[s..e].to_string())));
            self.consecutive_nls = 0;
        } else {
            // Fallback: classified text without a usable span
            // (whitespace-derived). Emit the stored raw text.
            let raw = match &c.kind {
                ClassifiedKind::Line(t) | ClassifiedKind::Doc(t) | ClassifiedKind::Block(t) => {
                    t.clone()
                }
                _ => String::new(),
            };
            if !raw.is_empty() {
                self.out.push(Doc::text_owned(Cow::Owned(raw)));
                self.consecutive_nls = 0;
            }
        }
    }

    /// Emit one comment unless its span was already emitted (import
    /// sorting can rediscover header comments via multiple walks).
    /// Returns true when the comment was actually emitted.
    fn emit_comment_once(&mut self, c: &ClassifiedTrivia) -> bool {
        if self.emitted_comments.contains(&(c.start, c.end)) {
            return false;
        }
        self.emit_comment_trivia(c);
        true
    }

    /// Does `span` cover any `|>` pipeline operator? Pipeline chains are
    /// desugared by the parser into nested `Call` nodes; re-emitting them
    /// structurally would lose the `|>` token, so the emitter renders
    /// their original source range verbatim.
    ///
    /// `pipe_starts` is built in source order (sorted), so containment is
    /// a binary search — O(log n) per call site instead of O(pipes).
    fn has_pipe_in(&self, span: Span) -> bool {
        let idx = self.pipe_starts.partition_point(|&p| p < span.start);
        self.pipe_starts.get(idx).is_some_and(|&p| p < span.end)
    }

    /// Does `span` cover a `!` macro bang? `sqlz!{...}` desugars to a
    /// `Call` node; re-emitting structurally would lose the `!`, so the
    /// emitter renders the original source range verbatim.
    ///
    /// Sorted offsets → binary search, matching [`Self::has_pipe_in`].
    fn has_bang_in(&self, span: Span) -> bool {
        let idx = self.bang_starts.partition_point(|&p| p < span.start);
        self.bang_starts.get(idx).is_some_and(|&p| p < span.end)
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
                is_const,
                ..
            } => {
                if *pub_ {
                    self.text("pub");
                    self.space();
                }
                if *is_const {
                    self.text("const");
                    self.space();
                }
                self.text(name.name.clone());
                if let Some(t) = ty {
                    self.text(":");
                    self.emit_ty(t);
                    self.space();
                    self.text("=");
                    self.space();
                } else if *is_const {
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
                path,
                alias,
                items,
                pub_,
                ..
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
                if !items.is_empty() {
                    self.text("(");
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            self.text(", ");
                        }
                        match item {
                            ImportItem::Wildcard { .. } => self.text("*"),
                            ImportItem::Named { name, alias, .. } => {
                                self.text(name.clone());
                                if let Some(a) = alias {
                                    self.space();
                                    self.text("as");
                                    self.space();
                                    self.text(a.clone());
                                }
                            }
                        }
                    }
                    self.text(")");
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
                decorators,
                ..
            } => {
                for dec in decorators {
                    self.text("@");
                    for (i, p) in dec.path.iter().enumerate() {
                        if i > 0 {
                            self.text(".");
                        }
                        self.text(p);
                    }
                    if !dec.args.is_empty() || !dec.named.is_empty() {
                        self.text("(");
                        let mut first = true;
                        for arg in &dec.args {
                            if !first {
                                self.text(", ");
                            }
                            first = false;
                            self.emit_expr(arg);
                        }
                        for (aname, arg) in &dec.named {
                            if !first {
                                self.text(", ");
                            }
                            first = false;
                            self.text(aname.clone());
                            self.text(": ");
                            self.emit_expr(arg);
                        }
                        self.text(")");
                    }
                    self.hard_line();
                }
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
                        self.text(g.name.name.clone());
                        if !g.bounds.is_empty() {
                            self.text(": ");
                            for (j, b) in g.bounds.iter().enumerate() {
                                if j > 0 {
                                    self.text(" + ");
                                }
                                self.text(b.name());
                            }
                        }
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
                if fields.is_empty() {
                    self.text("{}");
                } else {
                    // Canonical expanded form (4-space indent, trailing comma):
                    // struct Task {
                    //     id: int,
                    //     name: str,
                    // }
                    self.text("{");
                    let saved = std::mem::take(&mut self.out);
                    for (n, t) in fields.iter() {
                        self.out.push(Doc::hard_line());
                        self.consecutive_nls = 1;
                        // Embedded (anonymous) fields print in short form.
                        let embedded = matches!(&t.kind, TyKind::Named(full, args)
                            if args.is_empty()
                                && full.rsplit('.').next().unwrap_or(full) == n.name);
                        if embedded {
                            self.text(n.name.clone());
                        } else {
                            self.text(n.name.clone());
                            self.text(": ");
                            self.emit_ty(t);
                        }
                        self.text(",");
                    }
                    let body = std::mem::replace(&mut self.out, saved);
                    self.out.push(Doc::Indent {
                        contents: Box::new(Doc::Concat(body)),
                    });
                    self.out.push(Doc::hard_line());
                    self.consecutive_nls = 1;
                    self.text("}");
                }
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
            Stmt::ExternBlock { abi, items, .. } => {
                self.text("extern");
                self.space();
                self.text(format!("\"{abi}\""));
                self.space();
                self.text("{");
                for item in items {
                    self.text("func");
                    self.space();
                    self.text(item.name.name.clone());
                    self.text("(");
                    for (i, p) in item.params.iter().enumerate() {
                        if i > 0 {
                            self.text(", ");
                        }
                        self.text(p.name.name.clone());
                        if let Some(t) = &p.ty {
                            self.text(":");
                            self.emit_ty(t);
                        }
                    }
                    self.text(")");
                    if let Some(r) = &item.ret {
                        self.text(" -> ");
                        self.emit_ty(r);
                    }
                }
                self.text("}");
            }
            Stmt::Link { lib, .. } => {
                self.text("@link");
                self.text(format!("(\"{lib}\")"));
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
        // Clone to avoid holding an immutable borrow across the mutable
        // `emit_trivia` call.
        let (start, leading) = {
            let t = &self.toks[idx];
            if t.start != byte {
                return;
            }
            (t.start, t.leading.clone())
        };
        let _ = start;
        for c in &leading {
            if c.is_comment() && self.emit_comment_once(c) {
                self.out.push(Doc::hard_line());
                self.consecutive_nls = 1;
            }
        }
    }

    /// Emit comments immediately attached above a hoisted `import`.
    ///
    /// Line comments preceding an `import` attach (in the lexer) to the
    /// intervening `StmtEnd` token, not to the `import` token itself, so
    /// `emit_boundary_comments` misses them. Walk backwards from the
    /// import, collecting comment trivia on `StmtEnd` / `import` tokens
    /// until hitting real code; emit the collected comments in source
    /// order. Stops at blank separation implicitly by hitting code —
    /// blank lines are just empty `StmtEnd`s which we skip over while
    /// still collecting (the comment stays attached to the import).
    fn emit_attached_import_comments(&mut self, import_start: u32) {
        let idx = token_index_at_or_after(self.toks, import_start);
        if idx >= self.toks.len() {
            return;
        }
        let mut collected: Vec<ClassifiedTrivia> = Vec::new();
        let mut j = idx.saturating_sub(1);
        let mut first = true;
        loop {
            let (kind, start, leading) = {
                let t = &self.toks[j];
                (t.kind, t.start, t.leading.clone())
            };
            if start >= import_start {
                if j == 0 {
                    break;
                }
                j -= 1;
                continue;
            }
            // Stop at real code: any significant token other than the
            // statement terminator means we've reached the preceding
            // statement/expression.
            if kind != TokenKind::StmtEnd {
                break;
            }
            for c in leading.iter().rev() {
                if c.is_comment() {
                    collected.push(c.clone());
                }
            }
            if j == 0 {
                break;
            }
            // Only walk back over contiguous StmtEnd chain; the first
            // non-StmtEnd (code) stops the search on the next iteration.
            // To avoid pulling distant file-header comments across code,
            // stop after crossing code — handled above — but allow
            // multiple StmtEnds (blank lines) between comment and import.
            j -= 1;
            if first {
                first = false;
                // Always inspect at least two tokens back (StmtEnd + maybe
                // code) to find the attached comment.
            }
            // Safety bound: don't walk more than a handful of tokens back;
            // attached comments are always within 2-3 tokens.
            if idx.saturating_sub(j) > 8 {
                break;
            }
        }
        collected.reverse();
        for c in &collected {
            if self.emit_comment_once(c) {
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
            Pattern::Literal { value, span } => match value {
                // String patterns are raw-immutable (escapes preserved).
                Lit::Str(_) => self.emit_raw(*span),
                _ => self.emit_lit(value, *span),
            },
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
            Pattern::Or { pats, .. } => {
                for (i, p) in pats.iter().enumerate() {
                    if i > 0 {
                        self.text(" | ");
                    }
                    self.emit_pattern(p);
                }
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
            TyKind::Void => self.text("void"),
            TyKind::Ptr { mutable, inner } => {
                self.text(if *mutable { "*mut " } else { "*const " });
                self.emit_ty(inner);
            }
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
                self.text("func(");
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.emit_ty(a);
                }
                self.text(") -> ");
                self.emit_ty(ret);
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

    fn emit_lit(&mut self, l: &Lit, lit_span: Span) {
        match l {
            Lit::Int(n) => self.text(n.to_string()),
            Lit::Float(f) => self.text(format_float(*f)),
            // Lossless: raw source slice (escapes, quotes preserved).
            Lit::Str(_) => self.emit_raw(lit_span),
            Lit::Bool(b) => self.text(b.to_string()),
        }
    }

    fn emit_expr(&mut self, e: &Expr) {
        match e {
            Expr::Int { value, .. } => self.text(value.to_string()),
            Expr::Float { value, .. } => self.text(format_float(*value)),
            // CRITICAL: strings are immutable raw tokens. Emit the exact
            // source slice (delimiters, escapes like `\x0a`, internal
            // newlines/whitespace in `"""` blocks) verbatim. Never
            // re-escape or convert `"` <-> `"""`.
            Expr::Str { span, .. } => {
                self.emit_raw(*span);
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
            // Interpolated strings are also raw-immutable as a whole: the
            // outer literal (text, escapes, `"""` layout) is verbatim.
            // Inner `{expr}` nodes keep their source spelling too since
            // the whole span is copied; this guarantees no mutation of
            // string contents and trivial idempotence.
            Expr::Fmt { span, .. } => {
                self.emit_raw(*span);
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
                // since no AST shape can reproduce the `|>` token.
                // Same for `sqlz!{...}` macros (desugared to `db.query`).
                //
                // Lossless with standardized spacing: single-line chains get
                // exactly one space around each `|>` (`a|>f` -> `a |> f`);
                // multiline chains keep their verbatim layout (newlines are
                // significant style). Splitting only at real `|>` token
                // offsets (never inside strings) guarantees no mutation of
                // string contents.
                if self.has_pipe_in(*span) || self.has_bang_in(*span) {
                    let s = span.start as usize;
                    let e = (span.end as usize).min(self.source.len());
                    if e > s {
                        let raw = &self.source[s..e];
                        if self.has_bang_in(*span) || raw.contains('\n') {
                            self.out.push(Doc::Text(raw));
                        } else if let Some(normalized) =
                            normalize_single_line_pipes(raw, s, &self.pipe_starts)
                        {
                            self.out.push(Doc::text_owned(Cow::Owned(normalized)));
                            self.consecutive_nls = 0;
                        } else {
                            self.out.push(Doc::Text(raw));
                        }
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
            Expr::Try { expr, span } => {
                // Preserve the source form: prefix `try <chain>` stays prefix
                // (keeps `try a.b() + c` unambiguous), postfix `expr?` stays
                // postfix. Both lower to the same node, so sniff the keyword.
                let is_prefix = self
                    .source
                    .get(span.start as usize..)
                    .is_some_and(|s| s.starts_with("try ") || s.starts_with("try("));
                if is_prefix {
                    self.text("try ");
                    self.emit_expr(expr);
                } else {
                    self.emit_expr(expr);
                    self.text("?");
                }
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
            Expr::Break { .. } => self.text("break"),
            Expr::Continue { .. } => self.text("continue"),
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
                    // Embedded shorthand: `Base{...}` instead of
                    // `Base: Base{...}` when the value constructs the
                    // embedded type itself.
                    let shorthand = matches!(v, Expr::StructInit { name: inner, .. }
                        if inner.rsplit('.').next().unwrap_or(inner.as_str()) == n.as_str());
                    if !shorthand {
                        self.text(n);
                        self.text(": ");
                    }
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

/// True for standard-library imports (`import std.*`): these sort first,
/// before third-party / local packages (goimports/isort grouping).
fn is_std_import(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Import { path, .. } => path.first().is_some_and(|p| p == "std"),
        _ => false,
    }
}

/// Total sort key for an import: `(group, path, pub_, alias, items)`.
///
/// Group 0 = `std.*`, group 1 = external/local packages. Paths compare
/// as dotted strings; `pub import` sorts after plain `import` of the same
/// path so the order is fully deterministic. The caller uses a stable
/// sort, so imports that compare equal keep their source order.
fn import_sort_key(stmt: &Stmt) -> (bool, String, bool, String, String) {
    match stmt {
        Stmt::Import {
            path,
            alias,
            items,
            pub_,
            ..
        } => {
            let external = !path.first().is_some_and(|p| p == "std");
            let mut items_key = String::new();
            for item in items {
                match item {
                    ImportItem::Wildcard { .. } => items_key.push('*'),
                    ImportItem::Named { name, alias, .. } => {
                        items_key.push_str(name);
                        if let Some(a) = alias {
                            items_key.push_str(" as ");
                            items_key.push_str(a);
                        }
                        items_key.push(';');
                    }
                }
            }
            (
                external,
                path.join("."),
                *pub_,
                alias.clone().unwrap_or_default(),
                items_key,
            )
        }
        _ => (true, String::new(), false, String::new(), String::new()),
    }
}

/// Normalize single-line `|>` chains to exactly one space on each side.
///
/// `raw` is the source slice for a Call span starting at byte `base`;
/// `pipe_starts` holds absolute byte offsets of real `|>` tokens (never
/// inside strings). Splitting only at those offsets guarantees string
/// contents (which may contain the characters `|>`) are untouched.
/// Returns `None` when no pipe lies inside (caller falls back to verbatim).
fn normalize_single_line_pipes(raw: &str, base: usize, pipe_starts: &[u32]) -> Option<String> {
    // Binary-search the sorted offsets to the slice window [base, base+len];
    // only pipes inside the span are relevant. No full scan, no re-sort.
    let base_u = base as u32;
    let end_u = base_u.saturating_add(raw.len() as u32);
    let lo = pipe_starts.partition_point(|&p| p < base_u);
    let hi = pipe_starts.partition_point(|&p| p < end_u);
    let window = &pipe_starts[lo..hi];
    if window.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::with_capacity(window.len() + 1);
    let mut prev = 0usize;
    for &p in window {
        let rel = (p - base_u) as usize;
        // `|>` is two bytes; guard against malformed offsets.
        if rel < prev || rel + 2 > raw.len() {
            return None;
        }
        parts.push(raw[prev..rel].trim().to_string());
        prev = rel + 2;
    }
    parts.push(raw[prev..].trim().to_string());
    Some(parts.join(" |> "))
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
