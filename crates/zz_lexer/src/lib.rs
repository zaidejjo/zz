//! Lossless lexer.
//!
//! Rules:
//! - Whitespace, comments, and newlines *inside* brackets are trivia attached
//!   to the following significant token.
//! - A newline or `;` at bracket depth 0 becomes a significant `StmtEnd`
//!   token (the statement terminator). This gives newline-significant syntax
//!   with optional semicolons, while multi-line expressions inside parens
//!   just work.
//! - A newline after an operator or `=` is dropped (Go-style continuation).
//! - Block comments nest.

mod cursor;
pub mod token;

use std::borrow::Cow;

use crate::token::{Token, TokenKind, Trivia, TriviaKind};
use zz_diag_core::{error_at, FixIt, RawDiag};
use zz_span_ast::Span;

/// Render a fixit replacement for display: backtick-quoted, except a
/// replacement made solely of backticks (e.g. a raw-string closer),
/// which would render ambiguously as ``` — spell those by name (#414).
pub(crate) fn render_replacement(rep: &str) -> String {
    if !rep.is_empty() && rep.chars().all(|c| c == '`') {
        if rep.len() == 1 {
            "a backtick".to_string()
        } else {
            format!("{} backticks", rep.len())
        }
    } else {
        format!("`{rep}`")
    }
}

pub struct Lexed<'a> {
    pub tokens: Vec<Token<'a>>,
    pub errors: Vec<RawDiag>,
}

pub fn lex(source: &str) -> Lexed<'_> {
    Lexer::new(source).run()
}

/// Lexer state for interpolated strings.
///
/// A string literal stays "open" across interpolations: the stack holds a
/// `Str` context while consuming text, an `Interp` context inside each
/// `{ expr }` (nested braces push more), and any strings inside the
/// expression (which may themselves be interpolated) push their own `Str`.
#[derive(Debug)]
enum LexContext {
    Str {
        /// Byte offset of the opening quote, for token spans.
        start: usize,
        /// Text accumulated so far, across interpolation segments.
        value: String,
        /// True if this string was opened inside an interpolation expression
        /// (e.g., `f("inner")` inside `"{f("inner")}"`). Its closing `"`
        /// always emits `Str`, not `StrFmt`, because it terminates a
        /// completely separate string literal, not a continuation segment.
        is_nested: bool,
        /// True for triple-quoted (`"""..."""`) multiline strings. These
        /// allow unescaped `"`/`""` and raw newlines inside; the closing
        /// delimiter is `"""`. Interpolation uses the same `{ident}` trigger
        /// as single-line strings, plus `(`- and digit-led expressions.
        triple: bool,
        /// Token indices (into `tokens`) of already-emitted segments of the
        /// current triple-quoted string. On close, all segments are dedented
        /// together based on the closing delimiter's indentation.
        segs: Vec<usize>,
        /// Unclosed `{{` escapes seen in this string. A `}}` pair collapses
        /// to a single literal `}` only while this is nonzero; an unmatched
        /// `}}` (no preceding `{{` — the nested-JSON `...}}` case, #402)
        /// stays two literal braces. Carried across interpolation
        /// continuations so `"{{a{x}b}}"` still collapses.
        brace_depth: u32,
        /// True for backtick raw strings (`` `...` `` / triple-backtick).
        /// Raw content is fully literal: no interpolation, no `\` escapes,
        /// no brace collapsing. A doubled backtick (``` `` ```) is the only
        /// escape, yielding one literal backtick (#414).
        raw: bool,
        /// True when the accumulated `value` is byte-identical to the source
        /// slice (no escapes processed, no interpolation split). Clean
        /// single-line segments borrow the source instead of allocating.
        /// Set at context creation; any escape clears it. Triple-quoted
        /// strings are never clean (post-hoc dedent rewrites them).
        clean: bool,
    },
    /// Inside an interpolation `{ expr }`. `depth` counts nested braces
    /// beyond the interpolation's own opening brace (dicts, blocks, ...).
    /// The interpolation closes when the depth drops to zero.
    Interp { depth: u32 },
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    prev_sig: Option<TokenKind>,
    pending: Vec<Trivia<'a>>,
    tokens: Vec<Token<'a>>,
    errors: Vec<RawDiag>,
    contexts: Vec<LexContext>,
    /// True when the previous string segment ended right before an
    /// interpolation `{`; the next `{` opens the interpolation context.
    pending_interp: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        // Pre-size from source length (Q1 arena-next): ~1 significant token
        // per ~4 bytes measured on the bench corpus, so the token Vec never
        // reallocs on the way up; pending trivia drains per token (small).
        // Over-reserve is bounded and transient (freed with `Lexed`).
        Lexer {
            src,
            pos: 0,
            prev_sig: None,
            pending: Vec::with_capacity(src.len() / 64 + 8),
            tokens: Vec::with_capacity(src.len() / 4 + 16),
            errors: Vec::new(),
            contexts: Vec::new(),
            pending_interp: false,
        }
    }

    /// Unterminated string/comment error with an add-the-closer fix.
    /// `start` is the opening quote offset (the error points there, so the
    /// user sees where the string began swallowing code).
    /// Single-line strings close at end of their first line (the classic
    /// forgotten-quote typo); triple-quoted strings and block comments
    /// close at end of file.
    fn unterminated(&mut self, start: usize, closer: &str, first_line: bool) {
        let end = self.src.len() as u32;
        let span = Span::new(start as u32, end);
        let at = if first_line {
            self.src[start..]
                .find('\n')
                .map(|i| start as u32 + i as u32)
                .unwrap_or(end)
        } else {
            end
        };
        let place = if first_line {
            "at end of line"
        } else {
            "at end of file"
        };
        self.errors.push(
            error_at(
                format!(
                    "unterminated string literal — add closing {} {place}",
                    render_replacement(closer)
                ),
                span,
            )
            .with_fixit(FixIt::safe(
                Span::new(at, at),
                closer,
                format!("add closing {}", render_replacement(closer)),
            )),
        );
    }

    fn run(mut self) -> Lexed<'a> {
        while self.pos < self.src.len() {
            let c = self.peek_char().unwrap();
            // Inside a string literal (including a continuation segment after
            // an interpolation), consume characters as string content. When an
            // interpolation start is pending, the `{` must be dispatched below.
            if matches!(self.contexts.last(), Some(LexContext::Str { .. })) && !self.pending_interp
            {
                let triple = matches!(
                    self.contexts.last(),
                    Some(LexContext::Str { triple: true, .. })
                );
                if triple {
                    self.lex_triple_cont();
                } else {
                    self.lex_string_cont();
                }
                continue;
            }
            match c {
                ' ' | '\t' | '\r' => self.push_trivia(TriviaKind::Whitespace),
                '\n' => {
                    if !self.line_continues() && !self.next_is_pipe_arrow() {
                        // A newline terminates a statement unless the previous
                        // token implies the expression continues (Go-style),
                        // or the next line starts with `|>` (multi-line pipe).
                        // This applies inside braces too, which is what makes
                        // match arms and block statements parse.
                        self.emit_significant(TokenKind::StmtEnd, self.pos, self.pos + 1);
                    } else {
                        self.push_trivia(TriviaKind::Newline);
                    }
                }
                ';' => self.emit_significant(TokenKind::StmtEnd, self.pos, self.pos + 1),
                '/' if self.peek_char_at(1) == Some('/') => self.lex_line_comment(),
                '/' if self.peek_char_at(1) == Some('*') => self.lex_block_comment(),
                '/' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::SlashEq, self.pos, self.pos + 2)
                }
                '/' => self.emit_significant(TokenKind::Slash, self.pos, self.pos + 1),
                '(' => self.emit_significant(TokenKind::LParen, self.pos, self.pos + 1),
                ')' => self.emit_significant(TokenKind::RParen, self.pos, self.pos + 1),
                '{' => {
                    if self.pending_interp {
                        // Heuristic: if the `{` looks like it starts a match arm
                        // block (followed by `_` or `=>`), treat it as a block
                        // delimiter, not string interpolation.
                        if self.looks_like_match_arm_after_brace() {
                            self.pending_interp = false;
                        } else {
                            // This is the interpolation's own opening brace; no
                            // nested braces have been seen yet.
                            self.pending_interp = false;
                            self.contexts.push(LexContext::Interp { depth: 0 });
                        }
                    } else if let Some(LexContext::Interp { depth }) = self.contexts.last_mut() {
                        // A nested brace inside the interpolation (dict
                        // literal, block, closure, ...).
                        *depth += 1;
                    }
                    self.emit_significant(TokenKind::LBrace, self.pos, self.pos + 1);
                }
                '}' => {
                    if let Some(LexContext::Interp { depth }) = self.contexts.last_mut() {
                        if *depth == 0 {
                            // Closing the interpolation: resume string mode.
                            self.contexts.pop();
                            // Reset the Str context start to after this `}`
                            // so the next Str segment's span begins here,
                            // not at the original opening quote.
                            if let Some(LexContext::Str { start, .. }) = self.contexts.last_mut() {
                                *start = self.pos + '}'.len_utf8();
                            }
                        } else {
                            *depth -= 1;
                        }
                    }
                    self.emit_significant(TokenKind::RBrace, self.pos, self.pos + 1);
                }
                '[' => self.emit_significant(TokenKind::LBracket, self.pos, self.pos + 1),
                ']' => self.emit_significant(TokenKind::RBracket, self.pos, self.pos + 1),
                '+' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::PlusEq, self.pos, self.pos + 2)
                }
                '+' => self.emit_significant(TokenKind::Plus, self.pos, self.pos + 1),
                '-' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::Arrow, self.pos, self.pos + 2)
                }
                '-' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::MinusEq, self.pos, self.pos + 2)
                }
                '-' => self.emit_significant(TokenKind::Minus, self.pos, self.pos + 1),
                '*' if self.peek_char_at(1) == Some('*') && self.peek_char_at(2) == Some('=') => {
                    self.emit_significant(TokenKind::StarStarEq, self.pos, self.pos + 3)
                }
                '*' if self.peek_char_at(1) == Some('*') => {
                    self.emit_significant(TokenKind::StarStar, self.pos, self.pos + 2)
                }
                '*' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::StarEq, self.pos, self.pos + 2)
                }
                '*' => self.emit_significant(TokenKind::Star, self.pos, self.pos + 1),
                '%' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::PercentEq, self.pos, self.pos + 2)
                }
                '%' => self.emit_significant(TokenKind::Percent, self.pos, self.pos + 1),
                '=' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Eq, self.pos, self.pos + 2)
                }
                '=' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::Arrow, self.pos, self.pos + 2)
                }
                '=' => self.emit_significant(TokenKind::Assign, self.pos, self.pos + 1),
                '!' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Ne, self.pos, self.pos + 2)
                }
                '!' => self.emit_significant(TokenKind::Bang, self.pos, self.pos + 1),
                '<' if self.peek_char_at(1) == Some('<') && self.peek_char_at(2) == Some('=') => {
                    self.emit_significant(TokenKind::ShlEq, self.pos, self.pos + 3)
                }
                '<' if self.peek_char_at(1) == Some('<') => {
                    self.emit_significant(TokenKind::Shl, self.pos, self.pos + 2)
                }
                '<' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Le, self.pos, self.pos + 2)
                }
                '<' => self.emit_significant(TokenKind::Lt, self.pos, self.pos + 1),
                '>' if self.peek_char_at(1) == Some('>') && self.peek_char_at(2) == Some('=') => {
                    self.emit_significant(TokenKind::ShrEq, self.pos, self.pos + 3)
                }
                '>' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::Shr, self.pos, self.pos + 2)
                }
                '>' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Ge, self.pos, self.pos + 2)
                }
                '>' => self.emit_significant(TokenKind::Gt, self.pos, self.pos + 1),
                '&' if self.peek_char_at(1) == Some('&') => {
                    self.emit_significant(TokenKind::AndAnd, self.pos, self.pos + 2)
                }
                '&' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::AmpEq, self.pos, self.pos + 2)
                }
                '&' => self.emit_significant(TokenKind::Amp, self.pos, self.pos + 1),
                '^' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::CaretEq, self.pos, self.pos + 2)
                }
                '^' => self.emit_significant(TokenKind::Caret, self.pos, self.pos + 1),
                '~' => self.emit_significant(TokenKind::Tilde, self.pos, self.pos + 1),
                '|' if self.peek_char_at(1) == Some('|') => {
                    // Zero-arg closure `|| body` vs boolean-or (#261): after
                    // an operand `||` is `||`; anywhere an expression can
                    // start it opens a closure, so emit two pipes and let
                    // `parse_closure` take the empty-param path.
                    if Self::ends_operand(self.prev_sig) {
                        self.emit_significant(TokenKind::OrOr, self.pos, self.pos + 2)
                    } else {
                        self.emit_significant(TokenKind::Pipe, self.pos, self.pos + 1);
                        self.emit_significant(TokenKind::Pipe, self.pos, self.pos + 1)
                    }
                }
                '|' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::PipeGt, self.pos, self.pos + 2)
                }
                '|' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::PipeEq, self.pos, self.pos + 2)
                }
                '|' => self.emit_significant(TokenKind::Pipe, self.pos, self.pos + 1),
                '?' if self.peek_char_at(1) == Some('?') => {
                    self.emit_significant(TokenKind::QuestionQuestion, self.pos, self.pos + 2)
                }
                '?' => self.emit_significant(TokenKind::Question, self.pos, self.pos + 1),
                ':' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::ColonEq, self.pos, self.pos + 2)
                }
                ':' => self.emit_significant(TokenKind::Colon, self.pos, self.pos + 1),
                ',' => self.emit_significant(TokenKind::Comma, self.pos, self.pos + 1),
                '@' => self.emit_significant(TokenKind::At, self.pos, self.pos + 1),
                '.' if self.peek_char_at(1) == Some('.') => {
                    self.emit_significant(TokenKind::DotDot, self.pos, self.pos + 2)
                }
                '.' => self.emit_significant(TokenKind::Dot, self.pos, self.pos + 1),
                '"' => self.lex_string(),
                // Backtick raw strings (`` `...` `` / triple-backtick, #414).
                '`' => self.lex_string(),
                c if c.is_ascii_digit() => self.lex_number(),
                c if is_ident_start(c) => self.lex_ident(),
                _ => {
                    let start = self.pos;
                    self.bump_char();
                    let span = Span::new(start as u32, self.pos as u32);
                    self.errors
                        .push(error_at(format!("unexpected character `{c}`"), span));
                }
            }
        }
        if !self.contexts.is_empty() {
            // A string (or interpolation) was left open at end of input.
            match self.contexts.last() {
                Some(LexContext::Str {
                    start, triple, raw, ..
                }) => {
                    let (closer, first_line) = if *raw {
                        (if *triple { "```" } else { "`" }, !*triple)
                    } else if *triple {
                        ("\"\"\"", false)
                    } else {
                        ("\"", true)
                    };
                    self.unterminated(*start, closer, first_line);
                }
                _ => {
                    let end = self.src.len() as u32;
                    self.errors.push(
                        error_at(
                            "unterminated string interpolation — add closing `}` at end of file",
                            Span::new(self.pos as u32, end),
                        )
                        .with_fixit(FixIt::safe(
                            Span::new(end, end),
                            "}",
                            "add closing `}`",
                        )),
                    );
                }
            }
        }
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            text: Cow::Borrowed(""),
            span: Span::new(self.src.len() as u32, self.src.len() as u32),
            leading: Vec::new(),
        });
        Lexed {
            tokens: self.tokens,
            errors: self.errors,
        }
    }

    // --- trivia -----------------------------------------------------------

    fn push_trivia(&mut self, kind: TriviaKind) {
        let start = self.pos;
        self.bump_char();
        let span = Span::new(start as u32, self.pos as u32);
        // Borrow the source slice — trivia is never rewritten.
        let src = self.src;
        self.pending.push(Trivia {
            kind,
            text: &src[span.to_range()],
            span,
        });
    }

    fn lex_line_comment(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if c == '\n' {
                break;
            }
            self.bump_char();
        }
        let span = Span::new(start as u32, self.pos as u32);
        let src = self.src;
        self.pending.push(Trivia {
            kind: TriviaKind::Comment,
            text: &src[span.to_range()],
            span,
        });
    }

    fn lex_block_comment(&mut self) {
        let start = self.pos;
        let mut nest = 0u32;
        loop {
            match (self.peek_char(), self.peek_char_at(1)) {
                (Some('/'), Some('*')) => {
                    nest += 1;
                    self.bump_char();
                    self.bump_char();
                }
                (Some('*'), Some('/')) => {
                    nest -= 1;
                    self.bump_char();
                    self.bump_char();
                    if nest == 0 {
                        break;
                    }
                }
                (Some(_), _) => {
                    self.bump_char();
                }
                (None, _) => {
                    let start = start as u32;
                    let end = self.src.len() as u32;
                    self.errors.push(
                        error_at(
                            "unterminated block comment — add closing `*/` at end of file",
                            Span::new(start, end),
                        )
                        .with_fixit(FixIt::safe(
                            Span::new(end, end),
                            "*/",
                            "add closing `*/`",
                        )),
                    );
                    return;
                }
            }
        }
        let span = Span::new(start as u32, self.pos as u32);
        let src = self.src;
        self.pending.push(Trivia {
            kind: TriviaKind::Comment,
            text: &src[span.to_range()],
            span,
        });
    }

    // --- significant tokens ----------------------------------------------

    /// True if the previous significant token ends an operand, so a following
    /// `||` is boolean-or rather than a zero-arg closure opener (#261).
    /// Literals, names, and closing delimiters end operands; everything else
    /// (operators, openers, separators, keywords, statement starts) leaves
    /// the lexer in operand position. Spacing is irrelevant: `a||b` and
    /// `a || b` are both or-ops, `|| body` and `f(|| x)` both closures.
    fn ends_operand(prev: Option<TokenKind>) -> bool {
        matches!(
            prev,
            Some(
                TokenKind::Ident
                    | TokenKind::Int
                    | TokenKind::Float
                    | TokenKind::Str
                    | TokenKind::StrFmt
                    | TokenKind::True
                    | TokenKind::False
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
            )
        )
    }

    /// True if the previous significant token implies the current line
    /// continues (Go-style: newline after an operator or `=` is dropped).
    fn line_continues(&self) -> bool {
        matches!(
            self.prev_sig,
            Some(
                TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::StarStar
                    | TokenKind::Slash
                    | TokenKind::Percent
                    | TokenKind::Assign
                    | TokenKind::Eq
                    | TokenKind::Ne
                    | TokenKind::Lt
                    | TokenKind::Gt
                    | TokenKind::Le
                    | TokenKind::Ge
                    | TokenKind::AndAnd
                    | TokenKind::OrOr
                    | TokenKind::Bang
                    | TokenKind::Amp
                    | TokenKind::Caret
                    | TokenKind::Tilde
                    | TokenKind::Shl
                    | TokenKind::Shr
                    | TokenKind::PlusEq
                    | TokenKind::MinusEq
                    | TokenKind::StarEq
                    | TokenKind::SlashEq
                    | TokenKind::PercentEq
                    | TokenKind::StarStarEq
                    | TokenKind::AmpEq
                    | TokenKind::PipeEq
                    | TokenKind::CaretEq
                    | TokenKind::ShlEq
                    | TokenKind::ShrEq
                    | TokenKind::QuestionQuestion
                    | TokenKind::Colon
                    | TokenKind::Comma
                    | TokenKind::Dot
                    | TokenKind::DotDot
                    | TokenKind::Pipe
                    | TokenKind::PipeGt
                    | TokenKind::Arrow
                    | TokenKind::ColonEq
                    | TokenKind::LParen
                    | TokenKind::LBrace
                    | TokenKind::LBracket
            )
        )
    }

    /// Returns true if the next non-trivia in the source is `|>`. Used to
    /// allow multi-line pipe chains:
    /// ```zz
    /// val
    ///   |> f
    ///   |> g
    /// ```
    /// Comments are trivia too: a comment-only line between chain links
    /// must not terminate the pipeline (#489).
    fn next_is_pipe_arrow(&self) -> bool {
        // Scan the raw source ahead of the newline just seen (`self.pos`
        // is at the `\n`, so `+ 1` skips it). Byte-slice based, so
        // multibyte chars in skipped trivia can't split a boundary.
        let mut rest = self.src.get(self.pos + 1..).unwrap_or("");
        loop {
            rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
            if let Some(after) = rest.strip_prefix("//") {
                // Line comment: skip to its newline and re-trim. A
                // comment running to EOF means no pipe can follow.
                match after.find('\n') {
                    Some(i) => rest = &after[i..],
                    None => return false,
                }
            } else if rest.starts_with("/*") {
                // Block comments nest: track depth until the matching
                // closer. Unterminated means a real lex error surfaces
                // later; stay conservative here and end the statement.
                let mut depth = 1usize;
                let mut scan = &rest[2..];
                loop {
                    let closer = scan.find("*/");
                    let opener = scan.find("/*");
                    match (closer, opener) {
                        (Some(c), Some(o)) if o < c => {
                            depth += 1;
                            scan = &scan[o + 2..];
                        }
                        (Some(c), _) => {
                            depth -= 1;
                            scan = &scan[c + 2..];
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => return false,
                    }
                }
                rest = scan;
            } else {
                return rest.starts_with("|>");
            }
        }
    }

    fn emit_significant(&mut self, kind: TokenKind, start: usize, end: usize) {
        let span = Span::new(start as u32, end as u32);
        let src = self.src;
        let text = Cow::Borrowed(&src[span.to_range()]);
        self.pos = end;
        self.push_token(kind, span, text);
    }

    fn push_token(&mut self, kind: TokenKind, span: Span, text: Cow<'a, str>) {
        self.prev_sig = Some(kind);
        self.tokens.push(Token {
            kind,
            text,
            span,
            leading: std::mem::take(&mut self.pending),
        });
    }

    fn lex_ident(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if is_ident_continue(c) {
                self.bump_char();
            } else {
                break;
            }
        }
        let span = Span::new(start as u32, self.pos as u32);
        let src = self.src;
        let text = Cow::Borrowed(&src[span.to_range()]);
        let kind = match &*text {
            "import" => TokenKind::Import,
            "as" => TokenKind::As,
            "func" => TokenKind::Func,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "match" => TokenKind::Match,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "struct" => TokenKind::Struct,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "defer" => TokenKind::Defer,
            "pub" => TokenKind::Pub,
            "impl" => TokenKind::Impl,
            "const" => TokenKind::Const,
            "extern" => TokenKind::Extern,
            "mut" => TokenKind::Mut,
            _ => TokenKind::Ident,
        };
        self.push_token(kind, span, text);
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        // Detect a base prefix: `0x`/`0X` hex, `0b`/`0B` binary, `0o`/`0O`
        // octal. The entry point only fires on an ascii digit, so a prefixed
        // literal always begins with `0` here.
        let mut base = 10u32;
        let c0 = self.peek_char();
        let is_prefixed = matches!(c0, Some('0'))
            && matches!(
                self.peek_char_at(1),
                Some('x') | Some('X') | Some('b') | Some('B') | Some('o') | Some('O')
            );
        if is_prefixed {
            let prefix = self.src[self.pos + 1..self.pos + 2].to_ascii_lowercase();
            base = match prefix.as_str() {
                "x" => 16,
                "b" => 2,
                "o" => 8,
                _ => unreachable!(),
            };
            // Consume `0` and the prefix letter.
            self.bump_char();
            self.bump_char();
        }
        // Consume digits valid for the base (plus `_` separators).
        while let Some(c) = self.peek_char() {
            if Self::is_digit_for_base(c, base) || c == '_' {
                self.bump_char();
            } else {
                break;
            }
        }
        let mut is_float = false;
        // Floats are decimal-only; hex/binary/octal literals are integers.
        if base == 10
            && self.peek_char() == Some('.')
            && self.peek_char_at(1).is_some_and(|c| c.is_ascii_digit())
        {
            is_float = true;
            self.bump_char(); // '.'
            while let Some(c) = self.peek_char() {
                if c.is_ascii_digit() || c == '_' {
                    self.bump_char();
                } else {
                    break;
                }
            }
        } else if base == 10 && self.peek_char() == Some('.') && self.peek_char_at(1) != Some('.') {
            // `1.` — a dot with no digits after it is not a float (and not a
            // range start, which would be `1..`).
            let span = Span::new(start as u32, (self.pos + '.'.len_utf8()) as u32);
            self.errors
                .push(error_at("expected digit after decimal point", span));
        }
        // `123abc` / `0xFFabc` is a single invalid token, not two.
        // Exception: inside interpolation `{val:.2f}`, allow number+ident
        // sequences so format specs like `.2f` lex as separate tokens.
        if self.peek_char().is_some_and(is_ident_continue)
            && !self
                .contexts
                .iter()
                .any(|c| matches!(c, LexContext::Interp { .. }))
        {
            while let Some(c) = self.peek_char() {
                if is_ident_continue(c) {
                    self.bump_char();
                } else {
                    break;
                }
            }
            let span = Span::new(start as u32, self.pos as u32);
            self.errors.push(error_at("invalid number literal", span));
            return;
        }
        let span = Span::new(start as u32, self.pos as u32);
        let src = self.src;
        let text = Cow::Borrowed(&src[span.to_range()]);
        let kind = if is_float {
            TokenKind::Float
        } else {
            TokenKind::Int
        };
        self.push_token(kind, span, text);
    }

    /// Returns `true` when `c` is a digit valid in the given base.
    /// `_` separators are handled by the caller.
    fn is_digit_for_base(c: char, base: u32) -> bool {
        match base {
            16 => c.is_ascii_hexdigit(),
            8 => matches!(c, '0'..='7'),
            2 => matches!(c, '0' | '1'),
            _ => c.is_ascii_digit(),
        }
    }

    /// Begin a fresh string literal: consume the opening quote(s) and enter
    /// string mode. A `"""` opener starts a triple-quoted multiline string;
    /// otherwise a regular single-line string. The main loop then feeds
    /// characters through [`Lexer::lex_string_cont`] or
    /// [`Lexer::lex_triple_cont`].
    fn lex_string(&mut self) {
        let start = self.pos;
        let backtick = self.src[start..].starts_with('`');
        let triple = if backtick {
            self.src[start..].starts_with("```")
        } else {
            self.src[start..].starts_with("\"\"\"")
        };
        let raw = backtick;
        if triple {
            self.pos += 3; // opening `"""` or ` ``` `
        } else {
            self.bump_char(); // opening quote
        }
        let is_nested = matches!(self.contexts.last(), Some(LexContext::Interp { .. }));
        self.contexts.push(LexContext::Str {
            start,
            value: String::new(),
            is_nested,
            triple,
            segs: Vec::new(),
            brace_depth: 0,
            raw,
            // Fresh strings are clean until an escape proves otherwise.
            // Triple-quoted strings are never clean: post-hoc dedent
            // rewrites their text, so they always take the owned path.
            clean: !triple,
        });
    }

    /// Consume one unit of single-line raw string (`` `...` ``) content:
    /// a doubled backtick, the closing backtick, or a literal character.
    /// Everything else — `{`, `}`, `\`, `"` — stays verbatim (#414).
    fn lex_raw_single(&mut self, start: usize, mut value: String, is_nested: bool, clean: bool) {
        match self.peek_char() {
            // Doubled backtick: the only escape, yielding one literal
            // backtick (mirrors the `{{` / `}}` doubling philosophy).
            Some('`') if self.src[self.pos..].starts_with("``") => {
                value.push('`');
                self.bump_char();
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
                    brace_depth: 0,
                    raw: true,
                    // Rewritten text: never borrow afterwards.
                    clean: false,
                });
            }
            // Lone backtick closes. Same segment logic as `"`: a nested
            // raw string always terminates; otherwise an open `Interp`
            // means a continuation segment, else the string is complete.
            Some('`') => {
                let end = self.pos + '`'.len_utf8();
                self.bump_char();
                let span = Span::new(start as u32, end as u32);
                if is_nested {
                    self.push_token(
                        TokenKind::Str,
                        span,
                        clean_str_token(self.src, start, end, value, clean),
                    );
                } else if self
                    .contexts
                    .iter()
                    .any(|c| matches!(c, LexContext::Interp { .. }))
                {
                    self.push_token(TokenKind::StrFmt, span, Cow::Owned(value));
                    self.contexts.push(LexContext::Str {
                        start: self.pos,
                        value: String::new(),
                        is_nested: false,
                        triple: false,
                        segs: Vec::new(),
                        brace_depth: 0,
                        raw: false,
                        // Continuation segment (no opening quote at `start`).
                        clean: false,
                    });
                } else {
                    self.push_token(
                        TokenKind::Str,
                        span,
                        clean_str_token(self.src, start, end, value, clean),
                    );
                }
            }
            // Fully literal: interpolation, backslash escapes, and brace
            // collapsing never fire inside raw strings.
            Some(c) => {
                value.push(c);
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
                    brace_depth: 0,
                    raw: true,
                    // Verbatim char: cleanliness unchanged.
                    clean,
                });
            }
            None => {
                self.unterminated(start, "`", true);
            }
        }
    }

    /// Consume one unit of triple-backtick raw string content: the closing
    /// delimiter, or a literal character (single/double backticks included).
    /// Like `"""`, lone backticks are content; only ``` closes. No
    /// interpolation, no escapes, no dedent surprises — segments dedent
    /// exactly like triple-quoted strings (#414).
    fn lex_raw_triple(&mut self, start: usize, value: String, is_nested: bool, segs: Vec<usize>) {
        // Closing delimiter.
        if self.src[self.pos..].starts_with("```") {
            let indent = self.closing_triple_indent();
            self.pos += 3;
            let span = Span::new(start as u32, self.pos as u32);
            if is_nested {
                let idx = self.tokens.len();
                self.push_token(TokenKind::Str, span, Cow::Owned(value));
                self.dedent_triple_segments(&segs, idx, indent);
            } else if self
                .contexts
                .iter()
                .any(|c| matches!(c, LexContext::Interp { .. }))
            {
                let idx = self.tokens.len();
                self.push_token(TokenKind::StrFmt, span, Cow::Owned(value));
                let mut next_segs = segs;
                next_segs.push(idx);
                self.dedent_triple_segments(&next_segs, usize::MAX, indent);
                self.contexts.push(LexContext::Str {
                    start: self.pos,
                    value: String::new(),
                    is_nested: false,
                    triple: true,
                    segs: next_segs,
                    brace_depth: 0,
                    raw: false,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                });
            } else {
                let idx = self.tokens.len();
                self.push_token(TokenKind::Str, span, Cow::Owned(value));
                self.dedent_triple_segments(&segs, idx, indent);
            }
            return;
        }
        match self.peek_char() {
            Some(c) => {
                let mut value = value;
                value.push(c);
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: true,
                    segs,
                    brace_depth: 0,
                    raw: true,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                });
            }
            None => {
                self.unterminated(start, "```", false);
            }
        }
    }

    /// Consume one unit of string content (a quote, an escape, an
    /// interpolation start, or a plain character).
    fn lex_string_cont(&mut self) {
        // Pop the current string context; continuation arms re-push it with
        // the updated value.
        let (start, value, is_nested, clean, brace_depth, raw) = match self.contexts.pop() {
            Some(LexContext::Str {
                start,
                value,
                is_nested,
                triple: false,
                segs,
                clean,
                brace_depth,
                raw,
            }) => {
                debug_assert!(segs.is_empty());
                (start, value, is_nested, clean, brace_depth, raw)
            }
            Some(LexContext::Str { triple: true, .. }) => {
                unreachable!("triple-quoted string dispatched to lex_string_cont")
            }
            _ => unreachable!("lex_string_cont called outside string mode"),
        };
        // Raw strings (`` `...` ``, #414): everything is literal. The only
        // escape is a doubled backtick (``` `` ``` → `` ` ``); a lone
        // backtick closes. No interpolation, no `\` escapes, no brace
        // collapsing — braces, quotes, and backslashes stay verbatim.
        if raw {
            self.lex_raw_single(start, value, is_nested, clean);
            return;
        }
        match self.peek_char() {
            Some('"') => {
                let end = self.pos + '"'.len_utf8();
                self.bump_char();
                let span = Span::new(start as u32, end as u32);
                // A nested string (opened inside an interpolation expression
                // like `f("inner")`) always emits Str — its `"` is a real
                // string terminator, not a segment boundary.
                //
                // A non-nested string with an Interp context on the stack is
                // a continuation segment — emit StrFmt and keep the Str
                // context alive for text after `}`.
                if is_nested {
                    self.push_token(
                        TokenKind::Str,
                        span,
                        clean_str_token(self.src, start, end, value, clean),
                    );
                } else if self
                    .contexts
                    .iter()
                    .any(|c| matches!(c, LexContext::Interp { .. }))
                {
                    self.push_token(TokenKind::StrFmt, span, Cow::Owned(value));
                    self.contexts.push(LexContext::Str {
                        start: self.pos,
                        value: String::new(),
                        is_nested: false,
                        triple: false,
                        segs: Vec::new(),
                        // Continuation segment (no opening quote at `start`).
                        clean: false,
                        brace_depth,
                        raw,
                    });
                } else {
                    // Final closing quote — emit Str (complete string).
                    self.push_token(
                        TokenKind::Str,
                        span,
                        clean_str_token(self.src, start, end, value, clean),
                    );
                }
            }
            // Doubled-brace escape `{{` emits a single literal brace and
            // never opens an interpolation. Checked before the
            // interpolation arm so `{{name}}` stays literal text while
            // `{{{name}}}` is literal `{` + interpolation + literal `}`.
            // Each `{{` opens one escape pair (tracked in `brace_depth`).
            // Uses byte-prefix checks (`starts_with`) for exact two-byte
            // consumes; `peek_char_at` is byte-offset based and would be
            // wrong for multi-byte lookahead.
            Some('{') if self.src[self.pos..].starts_with("{{") => {
                let mut value = value;
                value.push('{');
                self.bump_char();
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
                    // Rewritten text: never borrow afterwards.
                    clean: false,
                    brace_depth: brace_depth.saturating_add(1),
                    raw: false,
                });
            }
            // Doubled-brace escape `}}` closes one `{{` pair. An unmatched
            // `}}` (no preceding `{{` — the nested-JSON `...}}` case, #402)
            // is NOT an escape: the guard fails and the braces lex as two
            // literal characters via the default arm below.
            Some('}') if self.src[self.pos..].starts_with("}}") && brace_depth > 0 => {
                let mut value = value;
                value.push('}');
                self.bump_char();
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
                    // Rewritten text: never borrow afterwards.
                    clean: false,
                    brace_depth: brace_depth - 1,
                    raw: false,
                });
            }
            // String interpolation: `{ident...`, `{1...`, `{(...` start an
            // embedded expression — the same trigger set as triple-quoted
            // strings, so `{1 + 2}` prints `3`. Spaces and tabs straight
            // after `{` are skipped before that test (#516), so `{ n}` is
            // `{n}`. `{{`, `{}`, and JSON-like `{"key"...` stay literal
            // text (checked above/below).
            // Emit the accumulated text as StrFmt and enter interpolation
            // mode (leaving the string context underneath); the main loop
            // lexes `{` as LBrace, the expression, and `}` as RBrace, popping
            // back into string mode for the continuation.
            Some('{') if self.interp_starts_after_brace() => {
                let span = Span::new(start as u32, self.pos as u32);
                // Clean prefix (unbroken from the opening quote, no escapes)
                // borrows `src[start+1..pos]`; anything else keeps the
                // accumulated owned text.
                let src = self.src;
                if clean {
                    debug_assert_eq!(value, src[start + 1..self.pos]);
                    self.push_token(
                        TokenKind::StrFmt,
                        span,
                        Cow::Borrowed(&src[start + 1..self.pos]),
                    );
                } else {
                    self.push_token(TokenKind::StrFmt, span, Cow::Owned(value));
                }
                self.contexts.push(LexContext::Str {
                    start: self.pos,
                    value: String::new(),
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
                    // Post-`{` continuation: no opening quote at `start`.
                    clean: false,
                    // Pairs span interpolations: `"{{a{x}b}}"` still closes.
                    brace_depth,
                    raw,
                });
                self.pending_interp = true;
            }
            Some('\\') => {
                self.bump_char();
                let mut value = value;
                match self.peek_char() {
                    Some('n') => {
                        value.push('\n');
                        self.bump_char();
                    }
                    Some('t') => {
                        value.push('\t');
                        self.bump_char();
                    }
                    Some('r') => {
                        value.push('\r');
                        self.bump_char();
                    }
                    Some('\\') => {
                        value.push('\\');
                        self.bump_char();
                    }
                    Some('"') => {
                        value.push('"');
                        self.bump_char();
                    }
                    // Escaped literal braces: `\{` / `\}` stay text and never
                    // open an interpolation.
                    Some('{') => {
                        value.push('{');
                        self.bump_char();
                    }
                    Some('}') => {
                        value.push('}');
                        self.bump_char();
                    }
                    // ESC (ASCII 27): `"\e[31m"` for ANSI terminal colors.
                    Some('e') => {
                        value.push('\x1b');
                        self.bump_char();
                    }
                    // Hex byte: `\xHH` (e.g. `\x1b` == ESC). Exactly two
                    // hex digits; anything else is a lex error.
                    Some('x') => {
                        let hi = self.peek_char_at(1);
                        let lo = self.peek_char_at(2);
                        match (hi, lo) {
                            (Some(h), Some(l))
                                if h.is_ascii_hexdigit() && l.is_ascii_hexdigit() =>
                            {
                                let byte =
                                    h.to_digit(16).unwrap_or(0) * 16 + l.to_digit(16).unwrap_or(0);
                                value.push(char::from_u32(byte).unwrap_or('\u{FFFD}'));
                                self.bump_char();
                                self.bump_char();
                                self.bump_char();
                            }
                            _ => {
                                let span = Span::new((self.pos - 1) as u32, (self.pos + 1) as u32);
                                self.errors.push(error_at(
                                    "invalid hex escape `\\x`: expected two hex digits (e.g. `\\x1b`)",
                                    span,
                                ));
                                self.bump_char();
                            }
                        }
                    }
                    Some(other) => {
                        let span =
                            Span::new((self.pos - 1) as u32, (self.pos + other.len_utf8()) as u32);
                        self.errors
                            .push(error_at(format!("unknown escape `\\{other}`"), span));
                        self.bump_char();
                    }
                    None => {
                        self.unterminated(start, "\"", true);
                        return;
                    }
                }
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
                    // Any `\` escape rewrites text: never borrow afterwards.
                    clean: false,
                    brace_depth,
                    raw,
                });
            }
            Some(c) => {
                let mut value = value;
                value.push(c);
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
                    // Verbatim char: cleanliness unchanged.
                    clean,
                    // Unmatched `}}` lands here one brace at a time (#402).
                    brace_depth,
                    raw,
                });
            }
            None => {
                self.unterminated(start, "\"", true);
            }
        }
    }

    /// Consume one unit of triple-quoted (`"""..."""`) string content.
    ///
    /// Differences from single-line strings:
    /// - the string only closes on `"""`; lone `"`/`""` and raw newlines are
    ///   literal content,
    /// - interpolation also triggers on digit- and `(`-led expressions
    ///   (`{1 + 2}`, `{(a)}`) so `{expr}` works for arbitrary expressions,
    ///   while `{"k": v}` (JSON-like) stays literal text,
    /// - on close, all emitted text segments are dedented together based on
    ///   the closing delimiter's indentation.
    fn lex_triple_cont(&mut self) {
        let (start, value, is_nested, segs, brace_depth, raw) = match self.contexts.pop() {
            Some(LexContext::Str {
                start,
                value,
                is_nested,
                triple: true,
                segs,
                // Triple strings never borrow (dedent rewrites); ignored.
                clean: _,
                brace_depth,
                raw,
            }) => (start, value, is_nested, segs, brace_depth, raw),
            Some(LexContext::Str { triple: false, .. }) => {
                unreachable!("single-line string dispatched to lex_triple_cont")
            }
            _ => unreachable!("lex_triple_cont called outside string mode"),
        };
        // Triple-backtick raw strings (#414): fully literal content.
        if raw {
            self.lex_raw_triple(start, value, is_nested, segs);
            return;
        }
        // Closing delimiter.
        if self.src[self.pos..].starts_with("\"\"\"") {
            let indent = self.closing_triple_indent();
            self.pos += 3;
            let span = Span::new(start as u32, self.pos as u32);
            if is_nested {
                let idx = self.tokens.len();
                self.push_token(TokenKind::Str, span, Cow::Owned(value));
                self.dedent_triple_segments(&segs, idx, indent);
            } else if self
                .contexts
                .iter()
                .any(|c| matches!(c, LexContext::Interp { .. }))
            {
                let idx = self.tokens.len();
                self.push_token(TokenKind::StrFmt, span, Cow::Owned(value));
                let mut next_segs = segs;
                next_segs.push(idx);
                // Dedent will run when the final `"""` closes; segments stay
                // tracked so the whole logical string is dedented together.
                self.dedent_triple_segments(&next_segs, usize::MAX, indent);
                self.contexts.push(LexContext::Str {
                    start: self.pos,
                    value: String::new(),
                    is_nested: false,
                    triple: true,
                    segs: next_segs,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                    brace_depth,
                    raw,
                });
            } else {
                let idx = self.tokens.len();
                self.push_token(TokenKind::Str, span, Cow::Owned(value));
                self.dedent_triple_segments(&segs, idx, indent);
            }
            return;
        }
        match self.peek_char() {
            // Doubled-brace escape `{{` emits a single literal brace and
            // never opens an interpolation. Same semantics as single-line
            // strings; dedent sees only the collapsed value. Each `{{`
            // opens one escape pair (tracked in `brace_depth`).
            Some('{') if self.src[self.pos..].starts_with("{{") => {
                let mut value = value;
                value.push('{');
                self.bump_char();
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: true,
                    segs,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                    brace_depth: brace_depth.saturating_add(1),
                    raw: false,
                });
            }
            // Doubled-brace escape `}}` closes one `{{` pair. An unmatched
            // `}}` (no preceding `{{` — the nested-JSON `...}}` case, #402)
            // stays two literal braces via the default arm below.
            Some('}') if self.src[self.pos..].starts_with("}}") && brace_depth > 0 => {
                let mut value = value;
                value.push('}');
                self.bump_char();
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: true,
                    segs,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                    brace_depth: brace_depth - 1,
                    raw: false,
                });
            }
            // Interpolation: `{ident...`, `{1...`, `{(...` start an embedded
            // expression, also when spaces/tabs follow the brace (#516).
            // `{{`, `{}` and `{"...` (JSON-like) stay literal.
            Some('{') if self.interp_starts_after_brace() => {
                let span = Span::new(start as u32, self.pos as u32);
                let idx = self.tokens.len();
                self.push_token(TokenKind::StrFmt, span, Cow::Owned(value));
                let mut next_segs = segs;
                next_segs.push(idx);
                self.contexts.push(LexContext::Str {
                    start: self.pos,
                    value: String::new(),
                    is_nested,
                    triple: true,
                    segs: next_segs,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                    // Pairs span interpolations, as in single-line strings.
                    brace_depth,
                    raw,
                });
                self.pending_interp = true;
            }
            Some('\\') => {
                self.bump_char();
                let mut value = value;
                match self.peek_char() {
                    Some('n') => {
                        value.push('\n');
                        self.bump_char();
                    }
                    Some('t') => {
                        value.push('\t');
                        self.bump_char();
                    }
                    Some('r') => {
                        value.push('\r');
                        self.bump_char();
                    }
                    Some('\\') => {
                        value.push('\\');
                        self.bump_char();
                    }
                    Some('"') => {
                        value.push('"');
                        self.bump_char();
                    }
                    // Escaped literal braces: `\{` / `\}` stay text and never
                    // open an interpolation.
                    Some('{') => {
                        value.push('{');
                        self.bump_char();
                    }
                    Some('}') => {
                        value.push('}');
                        self.bump_char();
                    }
                    // ESC (ASCII 27): `"\e[31m"` for ANSI terminal colors.
                    Some('e') => {
                        value.push('\x1b');
                        self.bump_char();
                    }
                    // Hex byte: `\xHH` (e.g. `\x1b` == ESC). Exactly two
                    // hex digits; anything else is a lex error.
                    Some('x') => {
                        let hi = self.peek_char_at(1);
                        let lo = self.peek_char_at(2);
                        match (hi, lo) {
                            (Some(h), Some(l))
                                if h.is_ascii_hexdigit() && l.is_ascii_hexdigit() =>
                            {
                                let byte =
                                    h.to_digit(16).unwrap_or(0) * 16 + l.to_digit(16).unwrap_or(0);
                                value.push(char::from_u32(byte).unwrap_or('\u{FFFD}'));
                                self.bump_char();
                                self.bump_char();
                                self.bump_char();
                            }
                            _ => {
                                let span = Span::new((self.pos - 1) as u32, (self.pos + 1) as u32);
                                self.errors.push(error_at(
                                    "invalid hex escape `\\x`: expected two hex digits (e.g. `\\x1b`)",
                                    span,
                                ));
                                self.bump_char();
                            }
                        }
                    }
                    Some(other) => {
                        let span =
                            Span::new((self.pos - 1) as u32, (self.pos + other.len_utf8()) as u32);
                        self.errors
                            .push(error_at(format!("unknown escape `\\{other}`"), span));
                        self.bump_char();
                    }
                    None => {
                        self.unterminated(start, "\"\"\"", false);
                        return;
                    }
                }
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: true,
                    segs,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                    brace_depth,
                    raw,
                });
            }
            Some(c) => {
                let mut value = value;
                value.push(c);
                self.bump_char();
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: true,
                    segs,
                    // Triple strings never borrow (dedent rewrites).
                    clean: false,
                    // Unmatched `}}` lands here one brace at a time (#402).
                    brace_depth,
                    raw,
                });
            }
            None => {
                self.unterminated(start, "\"\"\"", false);
            }
        }
    }

    /// Width (in ` `/`\t` chars) of the whitespace between the start of the
    /// closing delimiter's line and the closing `"""` itself. Used as the
    /// dedent width for the whole triple-quoted string.
    fn closing_triple_indent(&self) -> usize {
        let mut j = self.pos;
        while j > 0 && (self.src.as_bytes()[j - 1] == b' ' || self.src.as_bytes()[j - 1] == b'\t') {
            j -= 1;
        }
        self.pos - j
    }

    /// Dedent the text segments of a closed triple-quoted string in place.
    ///
    /// `segs` holds token indices of previously emitted `StrFmt` segments and
    /// `final_idx` is the closing segment's index (`usize::MAX` when the
    /// string continues after an interpolation and dedent must be deferred —
    /// in that case this is a no-op and the stored `segs` are dedented at the
    /// final close).
    ///
    /// Rules (Swift/Kotlin-style `trimIndent`):
    /// - one leading newline right after the opening `"""` is formatting, not
    ///   content, and is stripped;
    /// - the line break + whitespace before the closing `"""` is formatting
    ///   and is stripped;
    /// - every other line that begins after a newline has up to `indent`
    ///   leading spaces/tabs removed; the first line (on the opener's line)
    ///   is never dedented;
    /// - whitespace-only lines collapse to empty.
    fn dedent_triple_segments(&mut self, segs: &[usize], final_idx: usize, indent: usize) {
        if final_idx == usize::MAX {
            return; // string continues; dedent once at the final close
        }
        let mut idxs: Vec<usize> = segs.to_vec();
        idxs.push(final_idx);
        let mut texts: Vec<String> = idxs
            .iter()
            .map(|&i| self.tokens[i].text.clone().into_owned())
            .collect();
        if texts.is_empty() {
            return;
        }
        // Strip one leading newline (formatting, not content).
        let mut at_line_start = false;
        if let Some(first) = texts.first_mut() {
            if first.starts_with("\r\n") {
                first.drain(..2);
                at_line_start = true;
            } else if first.starts_with('\n') {
                first.drain(..1);
                at_line_start = true;
            }
        }
        // Strip the closer line: a trailing `\n` followed only by spaces/tabs
        // is the line break before the closing delimiter (formatting).
        if let Some(last) = texts.last_mut() {
            if let Some(nl) = last.rfind('\n') {
                if last[nl + 1..].chars().all(|c| c == ' ' || c == '\t') {
                    last.truncate(nl);
                }
            }
        }
        // Dedent line by line, carrying `at_line_start` across interpolation
        // boundaries so a segment that continues a line is left alone.
        for text in texts.iter_mut() {
            let mut out = String::with_capacity(text.len());
            let mut first_line = true;
            for line in text.split('\n') {
                if !first_line {
                    out.push('\n');
                    at_line_start = true;
                }
                first_line = false;
                if at_line_start {
                    let stripped = strip_up_to_indent(line, indent);
                    out.push_str(&stripped);
                    // A line with real content ends the line-start state;
                    // blank lines stay "at start" for the next line.
                    if !stripped.is_empty() {
                        at_line_start = false;
                    }
                } else {
                    out.push_str(line);
                }
            }
            // A segment ending with `\n` leaves the next segment at a line
            // start (its first line gets dedented).
            if text.ends_with('\n') {
                at_line_start = true;
            } else if !text.is_empty() {
                // `split` bookkeeping above already updated the flag for the
                // last line; nothing more to do.
            }
            *text = out;
        }
        for (tok_idx, new_text) in idxs.iter().zip(texts) {
            self.tokens[*tok_idx].text = Cow::Owned(new_text);
        }
    }

    // --- char helpers -----------------------------------------------------

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_char_at(&self, offset: usize) -> Option<char> {
        let idx = self.pos + offset;
        if idx >= self.src.len() || !self.src.is_char_boundary(idx) {
            return None;
        }
        self.src[idx..].chars().next()
    }

    /// Whether the `{` at the cursor opens an interpolation (#516).
    ///
    /// Spaces and tabs immediately after the brace are skipped before the
    /// trigger test, so `{ n }` behaves exactly like `{n}`. The body is
    /// lexed by the main loop, where whitespace between tokens is skipped
    /// like anywhere else, so only the trigger had to become
    /// whitespace-aware. `{{` stays the documented literal escape.
    fn interp_starts_after_brace(&self) -> bool {
        let mut offset = 1;
        loop {
            match self.peek_char_at(offset) {
                Some(' ') | Some('\t') => offset += 1,
                c => return is_interp_start(c),
            }
        }
    }

    fn bump_char(&mut self) -> char {
        let c = self.peek_char().expect("bump past end of input");
        self.pos += c.len_utf8();
        c
    }

    /// Peek ahead after a `{` to see if it starts a match arm block.
    /// Returns true if the content looks like a pattern => body (e.g., `_ =>`,
    /// `literal =>`, `ident =>`).
    fn looks_like_match_arm_after_brace(&self) -> bool {
        let mut idx = self.pos + 1; // skip the `{`
                                    // Skip whitespace and newlines.
        while idx < self.src.len() {
            let Some(c) = self.src[idx..].chars().next() else {
                break;
            };
            if c.is_whitespace() {
                idx += c.len_utf8();
                continue;
            }
            if c == '_' {
                return true; // wildcard pattern
            }
            if c == '=' && self.src.get(idx + 1..idx + 2) == Some(">") {
                return true; // =>
            }
            if c.is_ascii_digit() || c.is_ascii_alphabetic() || c == '"' {
                // Could be a literal or ident pattern; scan for `=>` after it.
                let mut j = idx;
                while j < self.src.len() {
                    let Some(c2) = self.src[j..].chars().next() else {
                        break;
                    };
                    if c2.is_whitespace() {
                        j += c2.len_utf8();
                        continue;
                    }
                    if c2 == '=' && self.src.get(j + 1..j + 2) == Some(">") {
                        return true;
                    }
                    if c2.is_ascii_alphanumeric() || c2 == '_' || c2 == '"' || c2 == '.' {
                        j += c2.len_utf8();
                        continue;
                    }
                    break;
                }
                return false;
            }
            return false;
        }
        false
    }
}

pub(crate) fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Interpolation trigger for both single-line and triple-quoted strings:
/// `{` opens an embedded expression when followed by an identifier start,
/// a digit, or `(`. `{{`, `{}` and JSON-like `{"key"...` stay literal text.
/// Callers test this on the first char after `{`, spaces and tabs skipped
/// (#516 — see [`Lexer::interp_starts_after_brace`]).
pub fn is_interp_start(c: Option<char>) -> bool {
    match c {
        Some(ch) if is_ident_start(ch) => true,
        Some(ch) if ch.is_ascii_digit() => true,
        Some('(') => true,
        _ => false,
    }
}

/// Remove up to `n` leading spaces/tabs from `line`. Whitespace-only lines
/// collapse to empty so indentation never pollutes the value with trailing
/// spaces.
fn strip_up_to_indent(line: &str, n: usize) -> String {
    if line.trim().is_empty() {
        return String::new();
    }
    let mut rest = line;
    let mut removed = 0;
    while removed < n {
        if rest.starts_with(' ') || rest.starts_with('\t') {
            rest = &rest[1..];
            removed += 1;
        } else {
            break;
        }
    }
    rest.to_string()
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Token text for a closed single-line string segment `[start, end)`.
///
/// When `clean` (unbroken from the opening quote, zero escapes), the value
/// is byte-identical to `src[start+1..end-1]` (quotes stripped) and borrows
/// it — one fewer allocation per plain string literal. Otherwise the
/// accumulated owned text is used. The debug assert self-verifies the
/// cleanliness invariant wherever tests exercise strings.
fn clean_str_token<'a>(
    src: &'a str,
    start: usize,
    end: usize,
    value: String,
    clean: bool,
) -> std::borrow::Cow<'a, str> {
    if clean {
        debug_assert_eq!(value, src[start + 1..end - 1]);
        std::borrow::Cow::Borrowed(&src[start + 1..end - 1])
    } else {
        std::borrow::Cow::Owned(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: returns true if the token stream contains a `StmtEnd` between
    /// the first `Int` and the first `PipeGt`.
    #[allow(dead_code)]
    fn has_stmt_end_before_pipe(src: &str) -> bool {
        let tokens = lex(src).tokens;
        let mut saw_int = false;
        for t in &tokens {
            match t.kind {
                TokenKind::Int => saw_int = true,
                TokenKind::PipeGt if saw_int => {
                    // Check if any StmtEnd appeared between Int and PipeGt
                    return false; // We already passed without finding StmtEnd
                }
                TokenKind::StmtEnd if saw_int => return true,
                _ => {}
            }
        }
        false
    }

    /// Token-kind sequence for `src`, assertion shorthand for the tests below.
    fn kinds_of(src: &str) -> Vec<TokenKind> {
        lex(src).tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn double_pipe_splits_after_non_operand() {
        // #261: `|| body` opens a zero-arg closure — two pipes, no OrOr.
        for src in ["f := || 42", "f :=|| 42", "retry(|| fetch())", "|| 42"] {
            let kinds = kinds_of(src);
            assert!(
                !kinds.contains(&TokenKind::OrOr),
                "{src:?} must not lex OrOr: {kinds:?}"
            );
            assert!(
                kinds.iter().filter(|k| **k == TokenKind::Pipe).count() >= 2,
                "{src:?} must lex two pipes: {kinds:?}"
            );
        }
    }

    #[test]
    fn double_pipe_stays_or_or_after_operand() {
        // #261: every spacing of `a || b` stays boolean-or.
        for src in ["a||b", "a ||b", "a|| b", "a || b", "x := a||b"] {
            let kinds = kinds_of(src);
            assert_eq!(
                kinds.iter().filter(|k| **k == TokenKind::OrOr).count(),
                1,
                "{src:?} must lex one OrOr: {kinds:?}"
            );
            assert!(
                !kinds.contains(&TokenKind::Pipe),
                "{src:?} must not lex Pipe: {kinds:?}"
            );
        }
    }

    #[test]
    fn hash_is_not_a_comment() {
        // `#` is not comment syntax — only `//` and `/* */` are.
        // A leading `#` must error instead of lexing silently.
        let lexed = lex("# not a comment\nx := 1\n");
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| e.message.contains("unexpected character")),
            "`#` must produce an unexpected-character error: {:?}",
            lexed.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        // The surviving comment forms still lex clean.
        for src in ["// line\nx := 1\n", "x := 1 /* block */ + 2\n"] {
            let lexed = lex(src);
            assert!(
                lexed.errors.is_empty(),
                "{src:?} must lex without errors: {:?}",
                lexed.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn single_line_pipe_no_stmt_end() {
        // "5 |> f" — no StmtEnd between 5 and |>
        let tokens = lex("5 |> f").tokens;
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert!(!kinds.contains(&TokenKind::StmtEnd));
    }

    #[test]
    fn multi_line_pipe_no_stmt_end() {
        // "5\n  |> f" — newline before |> should NOT emit StmtEnd
        let tokens = lex("5\n  |> f").tokens;
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        // There should be no StmtEnd between Int(5) and PipeGt
        let mut found_int = false;
        for k in &kinds {
            if *k == TokenKind::Int {
                found_int = true;
            }
            if *k == TokenKind::StmtEnd && found_int {
                panic!(
                    "StmtEnd found before PipeGt in multi-line pipe: {:?}",
                    kinds
                );
            }
            if *k == TokenKind::PipeGt {
                assert!(found_int, "PipeGt should appear after Int");
                break;
            }
        }
    }

    #[test]
    fn multi_line_pipe_triple_chain() {
        let tokens = lex("5\n  |> f\n  |> g").tokens;
        let pipe_count = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::PipeGt)
            .count();
        assert_eq!(pipe_count, 2, "should have two PipeGt tokens");
    }

    #[test]
    fn pipe_chain_with_line_comment_no_stmt_end() {
        // #489: a `//` comment line between chain links must not end
        // the statement.
        let tokens = lex("5\n  // double it\n  |> f").tokens;
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        let mut found_int = false;
        for k in &kinds {
            if *k == TokenKind::Int {
                found_int = true;
            }
            if *k == TokenKind::StmtEnd && found_int {
                panic!(
                    "StmtEnd found before PipeGt with comment in chain: {:?}",
                    kinds
                );
            }
            if *k == TokenKind::PipeGt {
                assert!(found_int, "PipeGt should appear after Int");
                break;
            }
        }
    }

    #[test]
    fn pipe_chain_with_block_comment_no_stmt_end() {
        // #489: block comments (incl. nested) are trivia in the lookahead.
        let tokens = lex("5\n  /* why /* nested */ still why */  |> f").tokens;
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        let int_idx = kinds.iter().position(|k| *k == TokenKind::Int).unwrap();
        let pipe_idx = kinds.iter().position(|k| *k == TokenKind::PipeGt).unwrap();
        assert!(
            !kinds[int_idx..pipe_idx].contains(&TokenKind::StmtEnd),
            "no StmtEnd between Int and PipeGt: {:?}",
            kinds
        );
    }

    #[test]
    fn pipe_chain_trailing_comment_then_link_no_stmt_end() {
        // #489: trailing comment after a link, next link on its own line.
        let tokens = lex("5 |> f // first step\n  |> g").tokens;
        let pipe_count = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::PipeGt)
            .count();
        assert_eq!(pipe_count, 2, "should have two PipeGt tokens");
    }

    #[test]
    fn comment_then_non_pipe_still_emits_stmt_end() {
        // A comment followed by a non-pipe line must still end the
        // statement — the lookahead only skips trivia, it never joins.
        let tokens = lex("5\n  // just a comment\n  x").tokens;
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        let int_idx = kinds.iter().position(|k| *k == TokenKind::Int).unwrap();
        let ident_idx = kinds.iter().position(|k| *k == TokenKind::Ident).unwrap();
        assert!(
            kinds[int_idx..ident_idx].contains(&TokenKind::StmtEnd),
            "StmtEnd expected before non-pipe line: {:?}",
            kinds
        );
    }

    // --- triple-quoted (multiline) strings ---------------------------------

    /// Significant tokens, excluding StmtEnd trivia and Eof.
    fn sig(src: &str) -> Vec<(TokenKind, String)> {
        let lexed = lex(src);
        assert!(
            lexed.errors.is_empty(),
            "lex errors for {src:?}: {:?}",
            lexed.errors
        );
        lexed
            .tokens
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::StmtEnd | TokenKind::Eof))
            .map(|t| (t.kind, t.text.into_owned()))
            .collect()
    }

    #[test]
    fn triple_empty() {
        assert_eq!(sig(r#""""""""#), vec![(TokenKind::Str, String::new())]);
    }

    #[test]
    fn triple_single_line() {
        assert_eq!(
            sig(r#""""hello""""#),
            vec![(TokenKind::Str, "hello".to_string())]
        );
    }

    #[test]
    fn triple_dedents_to_closer_indent() {
        let toks = sig("\"\"\"\n    line1\n    line2\n    \"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "line1\nline2".to_string())]);
    }

    #[test]
    fn triple_dedent_keeps_relative_indent() {
        let toks = sig("\"\"\"\n    outer\n        inner\n    \"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "outer\n    inner".to_string())]);
    }

    #[test]
    fn triple_first_line_never_dedented() {
        // Content on the opener's line is kept verbatim.
        let toks = sig("\"\"\"SELECT *\n    FROM t\n    \"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "SELECT *\nFROM t".to_string())]);
    }

    #[test]
    fn triple_unescaped_quotes_inside() {
        let toks = sig("\"\"\"a \"quoted\" word\"\"\"");
        assert_eq!(
            toks,
            vec![(TokenKind::Str, "a \"quoted\" word".to_string())]
        );
    }

    #[test]
    fn triple_interpolation() {
        let toks = sig("\"\"\"hello {name}!\"\"\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, "hello ".to_string()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::Ident, "name".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, "!".to_string()),
            ]
        );
    }

    #[test]
    fn triple_digit_led_interpolation() {
        // `{expr}` works for arbitrary expressions, not just identifiers.
        let toks = sig("\"\"\"{1}\n\"\"\"");
        assert_eq!(toks[0], (TokenKind::StrFmt, String::new()));
        assert_eq!(toks[1], (TokenKind::LBrace, "{".to_string()));
        assert_eq!(toks[2], (TokenKind::Int, "1".to_string()));
    }

    #[test]
    fn triple_json_braces_stay_literal() {
        // `{"k"...` must not open an interpolation (nested-brace safety).
        let toks = sig("\"\"\"{\"k\": 1}\"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{\"k\": 1}".to_string())]);
    }

    #[test]
    fn triple_double_brace_stays_literal() {
        // `{{` collapses to a single literal `{` (escape, not two chars).
        let toks = sig("\"\"\"a {{ b\"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "a { b".to_string())]);
    }

    #[test]
    fn double_brace_escapes() {
        // Single-line `{{` / `}}` collapse to single braces.
        let toks = sig("\"a{{b}}c\"");
        assert_eq!(toks, vec![(TokenKind::Str, "a{b}c".to_string())]);
        // `{{name}}` stays literal (no interpolation).
        let toks = sig("\"{{name}}\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{name}".to_string())]);
        // `{{{x}}}` is literal `{` + interpolation + literal `}`.
        let toks = sig("\"{{{x}}}\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, "{".to_string()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::Ident, "x".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, "}".to_string()),
            ]
        );
        // Quadruple braces collapse pairwise.
        let toks = sig("\"{{{{}}\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{{}".to_string())]);
        // Lone braces stay literal.
        let toks = sig("\"a}b\"");
        assert_eq!(toks, vec![(TokenKind::Str, "a}b".to_string())]);
    }

    #[test]
    fn raw_single_backtick_is_fully_literal() {
        // #414: braces, quotes, backslashes, and `{interp}`-lookalikes
        // all stay verbatim inside backticks.
        let toks = sig("`{\"a\": {x}} C:\\new`");
        assert_eq!(
            toks,
            vec![(TokenKind::Str, "{\"a\": {x}} C:\\new".to_string())]
        );
        // Interpolation never fires, even for identifiers/digits/parens.
        let toks = sig("`{name} {1} {(a)}`");
        assert_eq!(toks, vec![(TokenKind::Str, "{name} {1} {(a)}".to_string())]);
        // Doubled backticks collapse to one literal backtick.
        let toks = sig("`a``b`");
        assert_eq!(toks, vec![(TokenKind::Str, "a`b".to_string())]);
    }

    #[test]
    fn raw_triple_backtick_dedents_like_triple() {
        // Multiline raw keeps newlines, dedents on the closing fence,
        // and never interpolates.
        let toks = sig("```\n    {\"a\": 1}\n    {x}\n    ```");
        assert_eq!(toks, vec![(TokenKind::Str, "{\"a\": 1}\n{x}".to_string())]);
        // Lone and doubled backticks are content; only ``` closes.
        let toks = sig("```a`b``c```");
        assert_eq!(toks, vec![(TokenKind::Str, "a`b``c".to_string())]);
    }

    #[test]
    fn raw_unterminated_is_error() {
        let lexed = lex("`abc");
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| e.message.contains("a backtick")),
            "unterminated raw string must name the closer: {:?}",
            lexed.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        // Triple raw names its triple-backtick closer the same way.
        let lexed = lex("```abc");
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| e.message.contains("3 backticks")),
            "unterminated triple raw must name the closer: {:?}",
            lexed.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unmatched_double_close_stays_literal() {
        // #402: nested JSON closes (`...}}`) have no preceding `{{`, so
        // both braces stay literal instead of collapsing to one.
        let toks = sig("\"{\\\"a\\\": {}}\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{\"a\": {}}".to_string())]);
        let toks = sig("\"x}}y\"");
        assert_eq!(toks, vec![(TokenKind::Str, "x}}y".to_string())]);
        // Matched pairs still collapse, even across an interpolation.
        let toks = sig("\"{{a{x}b}}\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, "{a".to_string()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::Ident, "x".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, "b}".to_string()),
            ]
        );
    }

    #[test]
    fn triple_double_brace_escapes() {
        let toks = sig("\"\"\"a{{b}}c\"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "a{b}c".to_string())]);
        let toks = sig("\"\"\"{{{x}}}\"\"\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, "{".to_string()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::Ident, "x".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, "}".to_string()),
            ]
        );
    }

    #[test]
    fn triple_escaped_braces() {
        let toks = sig("\"\"\"\\{x\\}\"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{x}".to_string())]);
    }

    #[test]
    fn single_line_escaped_braces() {
        let toks = sig("\"\\{x\\}\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{x}".to_string())]);
    }

    #[test]
    fn single_line_digit_led_interpolation() {
        // `{1 + 2}` interpolates in single-line strings, like triple-quoted.
        let toks = sig("\"{1 + 2}\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, String::new()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::Int, "1".to_string()),
                (TokenKind::Plus, "+".to_string()),
                (TokenKind::Int, "2".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, String::new()),
            ]
        );
        // `{{7}}` stays literal (escape precedence over digit trigger).
        let toks = sig("\"{{7}}\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{7}".to_string())]);
    }

    #[test]
    fn single_line_paren_led_interpolation() {
        let toks = sig("\"{(a)}\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, String::new()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::LParen, "(".to_string()),
                (TokenKind::Ident, "a".to_string()),
                (TokenKind::RParen, ")".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, String::new()),
            ]
        );
    }

    #[test]
    fn single_line_interpolation_leading_space() {
        // #516: `{ n}` is the same interpolation as `{n}` — a leading space
        // used to be silently literal.
        let toks = sig("\"a { n} b\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, "a ".to_string()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::Ident, "n".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, " b".to_string()),
            ]
        );
        // Tabs, digits and parens skip the same way.
        let toks = sig("\"{\t1 + 2}\"");
        assert_eq!(toks[0], (TokenKind::StrFmt, String::new()));
        assert_eq!(toks[1], (TokenKind::LBrace, "{".to_string()));
        assert_eq!(toks[2], (TokenKind::Int, "1".to_string()));
        let toks = sig("\"{ (a)}\"");
        assert_eq!(toks[2], (TokenKind::LParen, "(".to_string()));
    }

    #[test]
    fn single_line_space_before_literal_braces() {
        // A space does not turn a deliberate literal into an interpolation:
        // JSON-like `{ "k": 1}` and `{ }` stay text (#516).
        let toks = sig("\"{ \\\"k\\\": 1}\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{ \"k\": 1}".to_string())]);
        let toks = sig("\"{ }\"");
        assert_eq!(toks, vec![(TokenKind::Str, "{ }".to_string())]);
    }

    #[test]
    fn triple_interpolation_leading_space() {
        let toks = sig("\"\"\"a { n} b\"\"\"");
        assert_eq!(toks[0], (TokenKind::StrFmt, "a ".to_string()));
        assert_eq!(toks[1], (TokenKind::LBrace, "{".to_string()));
        assert_eq!(toks[2], (TokenKind::Ident, "n".to_string()));
    }

    #[test]
    fn triple_dedent_across_interpolation() {
        // Segments before/after `{expr}` dedent as one logical string.
        let toks = sig("\"\"\"\n    a {x} b\n    c\n    \"\"\"");
        assert_eq!(
            toks,
            vec![
                (TokenKind::StrFmt, "a ".to_string()),
                (TokenKind::LBrace, "{".to_string()),
                (TokenKind::Ident, "x".to_string()),
                (TokenKind::RBrace, "}".to_string()),
                (TokenKind::Str, " b\nc".to_string()),
            ]
        );
    }

    #[test]
    fn triple_unterminated_is_error() {
        let lexed = lex("\"\"\"never closed");
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| e.message.contains("unterminated")),
            "expected unterminated error, got {:?}",
            lexed.errors
        );
    }

    #[test]
    fn newline_before_non_pipe_still_emits_stmt_end() {
        // "5\n  x" — plain newline without pipe → StmtEnd
        let tokens = lex("5\n  x").tokens;
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&TokenKind::StmtEnd));
    }

    #[test]
    fn multi_line_pipe_with_blank_lines() {
        // "5\n\n  |> f" — blank line between, still a pipe
        let tokens = lex("5\n\n  |> f").tokens;
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        let mut found_int = false;
        for k in &kinds {
            if *k == TokenKind::Int {
                found_int = true;
            }
            if *k == TokenKind::StmtEnd && found_int {
                panic!("StmtEnd found before PipeGt: {:?}", kinds);
            }
            if *k == TokenKind::PipeGt {
                assert!(found_int);
                break;
            }
        }
    }
}
