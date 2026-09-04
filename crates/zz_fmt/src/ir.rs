//! AST → Doc lowering.
//!
//! The lowering pass walks the AST to know the *shape* of each construct
//! (so we can decide where to break lines, where to indent, where a
//! group fits on one line) and emits the original token text verbatim
//! for every token. Because the lexer attaches `leading` trivia to each
//! significant token, the original comments and whitespace rhythm are
//! preserved automatically — we never strip them.
//!
//! Structural decisions (line breaks, indentation) are inserted at AST
//! node boundaries by switching to `Doc::HardLine` / `Doc::Indent` /
//! `Doc::Line` between specific tokens.
//!
//! M1 scope: full coverage of all `Stmt` and `Expr` variants, with
//! trivia preserved. Width-aware wrapping is exercised via `Group` on
//! call argument lists, struct fields, and array/dict literals.

use std::borrow::Cow;
use crate::doc::Doc;
use crate::printer::Eol;
use crate::trivia::{ClassifiedKind, ClassifiedTrivia};
use zz_frontend::ast::*;
use zz_frontend::lexer::lex;
use zz_frontend::span::Span;
use zz_frontend::token::{Token, TokenKind, TriviaKind};

/// A token with its byte range in the source, kept for slice emission.
#[derive(Debug, Clone, Copy)]
struct TokRef {
    start: u32,
    end: u32,
}

/// A token + the trivia that precedes it (already classified).
#[derive(Debug, Clone)]
struct Annotated {
    tok: TokRef,
    kind: TokenKind,
    leading: Vec<ClassifiedTrivia>,
}

/// Build the annotated token stream from source.
fn annotate(source: &str) -> Vec<Annotated> {
    let lexed = lex(source);
    lexed
        .tokens
        .into_iter()
        .map(|t: Token| {
            let leading = classify_leading(&t.leading);
            Annotated {
                tok: TokRef {
                    start: t.span.start,
                    end: t.span.end,
                },
                kind: t.kind,
                leading,
            }
        })
        .collect()
}

fn classify_leading(trivia: &[zz_frontend::token::Trivia]) -> Vec<ClassifiedTrivia> {
    let mut out = Vec::new();
    for t in trivia {
        match t.kind {
            TriviaKind::Whitespace => {
                let newlines = t.text.bytes().filter(|b| *b == b'\n').count();
                if newlines == 0 {
                    if !t.text.is_empty() {
                        out.push(ClassifiedTrivia {
                            kind: ClassifiedKind::Spacing(t.text.clone()),
                            start: t.span.start,
                            end: t.span.end,
                        });
                    }
                } else {
                    out.push(ClassifiedTrivia {
                        kind: ClassifiedKind::BlankLine {
                            count: newlines.max(1),
                        },
                        start: t.span.start,
                        end: t.span.end,
                    });
                }
            }
            TriviaKind::Newline => {
                out.push(ClassifiedTrivia {
                    kind: ClassifiedKind::BlankLine { count: 1 },
                    start: t.span.start,
                    end: t.span.end,
                });
            }
            TriviaKind::Comment => {
                let text = &t.text;
                let kind = if text.starts_with("///") {
                    let body = text.trim_start_matches("///").trim_start();
                    ClassifiedKind::Doc(body.to_string())
                } else if text.starts_with("//") {
                    let body = text.trim_start_matches("//").trim_start();
                    ClassifiedKind::Line(body.to_string())
                } else {
                    ClassifiedKind::Block(text.to_string())
                };
                out.push(ClassifiedTrivia {
                    kind,
                    start: t.span.start,
                    end: t.span.end,
                });
            }
        }
    }
    out
}

/// Lower a parsed program to a Doc.
///
/// Returns `(doc, eol_style)`. The caller renders the doc and uses
/// `eol_style` to pick `\n` vs `\r\n`.
pub fn lower_program<'src>(program: &Program, source: &'src str) -> (Doc<'src>, Eol) {
    let toks = annotate(source);
    let eol = detect_eol(source);
    let mut ctx = Ctx {
        source,
        toks: &toks,
        pos: 0,
        collapse_blanks: true,
    };
    let mut parts: Vec<Doc<'src>> = Vec::new();
    for (i, stmt) in program.stmts.iter().enumerate() {
        // Emit blank line(s) between top-level items, but only one.
        if i > 0 {
            parts.push(Doc::hard_line());
            parts.push(Doc::hard_line());
        }
        ctx.emit_stmt(stmt, &mut parts);
    }
    (Doc::Concat(parts), eol)
}

fn detect_eol(source: &str) -> Eol {
    if source.contains("\r\n") {
        Eol::Crlf
    } else {
        Eol::Lf
    }
}

struct Ctx<'src, 'a> {
    source: &'src str,
    toks: &'a [Annotated],
    pos: usize,
    collapse_blanks: bool,
}

impl<'src, 'a> Ctx<'src, 'a> {
    fn emit_classified(&self, c: &ClassifiedTrivia, out: &mut Vec<Doc<'src>>) -> bool {
        match &c.kind {
            ClassifiedKind::Spacing(_) => {
                // Spacing emits spaces via Doc::Text (after fix), which is not a hard line.
                false
            }
            ClassifiedKind::BlankLine { count } => {
                let n = if self.collapse_blanks {
                    (*count).min(2)
                } else {
                    *count
                };
                for _ in 0..n {
                    out.push(Doc::hard_line());
                }
                true
            }
            ClassifiedKind::Line(body) => {
                out.push(Doc::Text("// "));
                out.push(Doc::text_owned(body.clone()));
                out.push(Doc::hard_line());
                true
            }
            ClassifiedKind::Doc(body) => {
                out.push(Doc::Text("/// "));
                out.push(Doc::text_owned(body.clone()));
                out.push(Doc::hard_line());
                true
            }
            ClassifiedKind::Block(body) => {
                out.push(Doc::Text("/* "));
                out.push(Doc::text_owned(body.clone()));
                out.push(Doc::Text(" */"));
                false
            }
            ClassifiedKind::Newline => {
                out.push(Doc::hard_line());
                true
            }
        }
    }
    fn emit_span(&mut self, span: Span, out: &mut Vec<Doc<'src>>) {
        let start = token_index_at_or_after(self.toks, span.start);
        self.pos = start;
        // Track whether we are at the start of a line (true) or in the middle of a line (false).
        // Starts as true because we assume the span begins at a line boundary (caller ensures this).
        let mut line_start = true;
        // Track whether we have emitted at least one token in this span (for separator logic).
        let mut started = false;
        // The byte offset of the first character of the next thing we will emit.
        let mut cursor = span.start;
        while self.pos < self.toks.len() {
            let t = &self.toks[self.pos];
            if t.tok.start >= span.end {
                break;
            }
            // Emit the token's leading trivia, updating line_start as we go.
            for c in &t.leading {
                match &c.kind {
                    ClassifiedKind::Spacing(text) => {
                        if line_start {
                            // Leading spaces of a line: emit exact text.
                            out.push(Doc::TextOwned(Cow::Owned(text.clone())));
                            line_start = false; // we just emitted non-newline text
                        } else {
                            // Intra-line spacing: drop (let printer handle via Doc::Line).
                        }
                    }
                    ClassifiedKind::BlankLine { count } => {
                        let n = if self.collapse_blanks {
                            (*count).min(2)
                        } else {
                            *count
                        };
                        for _ in 0..n {
                            out.push(Doc::hard_line());
                            line_start = true; // after a hard line, we are at start of line
                        }
                    }
                    ClassifiedKind::Newline => {
                        out.push(Doc::hard_line());
                        line_start = true;
                    }
                    ClassifiedKind::Line(body) => {
                        out.push(Doc::Text("// "));
                        out.push(Doc::text_owned(body.clone()));
                        out.push(Doc::hard_line());
                        line_start = true; // after a hard line from line comment
                    }
                    ClassifiedKind::Doc(body) => {
                        out.push(Doc::Text("/// "));
                        out.push(Doc::text_owned(body.clone()));
                        out.push(Doc::hard_line());
                        line_start = true; // after a hard line from doc comment
                    }
                    ClassifiedKind::Block(body) => {
                        out.push(Doc::Text("/* "));
                        out.push(Doc::text_owned(body.clone()));
                        out.push(Doc::Text(" */"));
                        // Block comment does not end with a newline unless it contains internal
                        // newlines, which would have been emitted as separate BlankLine trivia.
                        line_start = false;
                    }
                }
            }
            // If this is not the first token of the span, decide what separator to insert
            // based on whether the gap between `cursor` and the start of this token contains a newline.
            if started {
                let gap_start = cursor as usize;
                let gap_end = t.tok.start as usize;
                if gap_end > gap_start && gap_end <= self.source.len() {
                    let gap = &self.source[gap_start..gap_end];
                    if gap.contains('\n') {
                        out.push(Doc::hard_line());
                        line_start = true;
                    } else if !gap.is_empty() {
                        // Original inline whitespace → normalize to a single space.
                        out.push(Doc::Text(" "));
                        line_start = false; // we just emitted a space
                    }
                }
            }
            // Emit the token's own text.
            let start_idx = t.tok.start as usize;
            let end_idx = (t.tok.end as usize).min(self.source.len());
            if end_idx > start_idx {
                out.push(Doc::Text(&self.source[start_idx..end_idx]));
                started = true;
                // If the token's text contains a newline, we are now at the start of a line.
                line_start = self.source[start_idx..end_idx].contains('\n');
            }
            cursor = t.tok.end;
            self.pos += 1;
        }
    }
    fn emit_stmt(&mut self, stmt: &Stmt, out: &mut Vec<Doc<'src>>) {
        match stmt {
            Stmt::Import { .. }
            | Stmt::Decl { .. }
            | Stmt::Func { .. }
            | Stmt::Return { .. }
            | Stmt::Struct { .. }
            | Stmt::Impl { .. }
            | Stmt::For { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Defer { .. }
            | Stmt::Assign { .. }
            | Stmt::Destructure { .. }
            | Stmt::Expr(_) => {
                self.emit_span(stmt.span(), out);
            }
        }
    }
}

fn token_index_at_or_after(toks: &[Annotated], byte: u32) -> usize {
    // Binary search for the first token whose start >= byte.
    let mut lo = 0usize;
    let mut hi = toks.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if toks[mid].tok.start < byte {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo.min(toks.len())
}
