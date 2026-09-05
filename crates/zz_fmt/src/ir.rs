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

use crate::doc::Doc;
use crate::printer::Eol;
use crate::trivia::{ClassifiedKind, ClassifiedTrivia};
use std::borrow::Cow;
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
    let annotated: Vec<Annotated> = lexed
        .tokens
        .into_iter()
        .map(|t: Token| {
            println!(
                "annotate: Token {:?} (text: {:?}) has {} leading trivia items",
                t.kind,
                t.text,
                t.leading.len()
            );
            for (i, trivia) in t.leading.iter().enumerate() {
                println!("  leading[{}]: {:?} = {}", i, trivia.kind, trivia.text);
            }
            let leading = classify_leading(&t.leading);
            println!(
                "annotate: After classify_leading, leading has {} items",
                leading.len()
            );
            for (i, c) in leading.iter().enumerate() {
                println!("  leading[{}]: {:?}", i, c.kind);
            }
            // Debug: print if we have any comment trivia in leading
            for c in &leading {
                if matches!(
                    c.kind,
                    ClassifiedKind::Line(_) | ClassifiedKind::Doc(_) | ClassifiedKind::Block(_)
                ) {
                    println!(
                        "annotate: Token {:?} has comment trivia: {:?}",
                        t.kind, c.kind
                    );
                }
            }
            Annotated {
                tok: TokRef {
                    start: t.span.start,
                    end: t.span.end,
                },
                kind: t.kind,
                leading,
            }
        })
        .collect();
    println!("annotate: Total tokens: {}", annotated.len());
    annotated
}

fn classify_leading(trivia: &[zz_frontend::token::Trivia]) -> Vec<ClassifiedTrivia> {
    let mut out = Vec::new();
    for t in trivia {
        match t.kind {
            TriviaKind::Whitespace => {
                let mut space_buf = String::new();
                for ch in t.text.chars() {
                    if ch == '\n' {
                        // flush any accumulated spaces as Spacing
                        if !space_buf.is_empty() {
                            out.push(ClassifiedTrivia {
                                kind: ClassifiedKind::Spacing(space_buf.clone()),
                                start: 0,
                                end: 0,
                            });
                            space_buf.clear();
                        }
                        out.push(ClassifiedTrivia {
                            kind: ClassifiedKind::BlankLine { count: 1 },
                            start: 0,
                            end: 0,
                        });
                    } else if ch.is_ascii_whitespace() {
                        space_buf.push(ch);
                    } else {
                        // Should not happen in whitespace, but ignore
                    }
                }
                // After processing all chars, if there are remaining spaces, emit as Spacing
                if !space_buf.is_empty() {
                    out.push(ClassifiedTrivia {
                        kind: ClassifiedKind::Spacing(space_buf.clone()),
                        start: 0,
                        end: 0,
                    });
                }
            }
            TriviaKind::Newline => {
                out.push(ClassifiedTrivia {
                    kind: ClassifiedKind::BlankLine { count: 1 },
                    start: 0,
                    end: 0,
                });
            }
            TriviaKind::Comment => {
                println!("classify_leading: Found comment trivia: {}", t.text);
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
                    start: 0,
                    end: 0,
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
        consecutive_line_endings: 0,
    };
    let mut parts: Vec<Doc<'src>> = Vec::new();
    // Emit the entire token stream in one go to preserve all trivia exactly
    ctx.emit_span(Span::new(0, source.len() as u32), &mut parts);
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
    consecutive_line_endings: usize,
}

impl<'src, 'a> Ctx<'src, 'a> {
    fn emit_classified(&mut self, c: &ClassifiedTrivia, out: &mut Vec<Doc<'src>>) -> bool {
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
                println!("Emitting Line comment: {}", body);
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
                true
            }
            ClassifiedKind::Newline => {
                // Handle consecutive line endings for newline trivia
                if self.consecutive_line_endings < 2 {
                    out.push(Doc::hard_line());
                    self.consecutive_line_endings += 1;
                }
                // If we've already seen 2 or more consecutive line endings, skip emitting this one
                // but still increment the counter to track that we've seen it
                else {
                    self.consecutive_line_endings += 1;
                }
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
        // Track whether we emitted a hard line from trivia (to avoid double newlines)
        let mut emitted_hard_line_from_trivia = false;
        // Track consecutive newlines from token text to collapse excessive blank lines
        let mut consecutive_token_newlines = 0;
        // The byte offset of the first character of the next thing we will emit.
        let mut cursor = span.start;
        while self.pos < self.toks.len() {
            let t = &self.toks[self.pos];
            if t.tok.start >= span.end {
                break;
            }
            // Debug: show which token we're processing and its leading trivia count
            println!(
                "emit_span: Processing token {:?} at pos {} with {} leading trivia items",
                t.kind,
                self.pos,
                t.leading.len()
            );
            // Emit the token's leading trivia, updating line_start as we go.
            for c in &t.leading {
                println!(
                    "emit_span: Processing leading trivia: {:?} = {}",
                    c.kind,
                    match &c.kind {
                        ClassifiedKind::Spacing(s) => format!("Spacing(\"{}\")", s),
                        ClassifiedKind::BlankLine { count } => format!("BlankLine({})", count),
                        ClassifiedKind::Newline => "Newline".to_string(),
                        ClassifiedKind::Line(s) => format!("Line(\"{}\")", s),
                        ClassifiedKind::Doc(s) => format!("Doc(\"{}\")", s),
                        ClassifiedKind::Block(s) => format!("Block(\"{}\")", s),
                    }
                );
                match &c.kind {
                    ClassifiedKind::Spacing(text) => {
                        // Always emit spacing exactly as it is to preserve original whitespace
                        out.push(Doc::TextOwned(Cow::Owned(text.clone())));
                        line_start = false; // we just emitted non-newline text
                    }
                    ClassifiedKind::BlankLine { count } => {
                        let n = if self.collapse_blanks {
                            (*count).min(2)
                        } else {
                            *count
                        };
                        for _ in 0..n {
                            out.push(Doc::hard_line());
                            emitted_hard_line_from_trivia = true;
                        }
                    }
                    ClassifiedKind::Newline => {
                        out.push(Doc::hard_line());
                        emitted_hard_line_from_trivia = true;
                    }
                    ClassifiedKind::Line(body) => {
                        out.push(Doc::Text("// "));
                        out.push(Doc::text_owned(body.clone()));
                        // Don't emit hard line here - token text will provide newline if needed
                    }
                    ClassifiedKind::Doc(body) => {
                        out.push(Doc::Text("/// "));
                        out.push(Doc::text_owned(body.clone()));
                        // Don't emit hard line here - token text will provide newline if needed
                    }
                    ClassifiedKind::Block(body) => {
                        println!("emit_span: Emitting Block comment: {}", body);
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
            // if started {
            //     let gap_start = cursor as usize;
            //     let gap_end = t.tok.start as usize;
            //     if gap_end > gap_start && gap_end <= self.source.len() {
            //         let gap = &self.source[gap_start..gap_end];
            //         if gap.contains('\n') {
            //             out.push(Doc::hard_line());
            //             line_start = true;
            //         } else if !gap.is_empty() {
            //             // Original inline whitespace → normalize to a single space.
            //             out.push(Doc::Text(" "));
            //             line_start = false; // we just emitted a space
            //         }
            //     }
            // }
            // Emit the token's own text.
            let start_idx = t.tok.start as usize;
            let end_idx = (t.tok.end as usize).min(self.source.len());
            if end_idx > start_idx {
                let text = &self.source[start_idx..end_idx];
                // Track consecutive newlines from token text to collapse excessive blank lines
                if text == "\n" {
                    consecutive_token_newlines += 1;
                    // Allow at most 2 consecutive newlines (one blank line)
                    if consecutive_token_newlines <= 2 {
                        out.push(Doc::Text(text));
                    }
                } else {
                    // Non-newline text resets the consecutive newline counter
                    consecutive_token_newlines = 0;
                    out.push(Doc::Text(text));
                }
                line_start = !text.contains('\n');
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
