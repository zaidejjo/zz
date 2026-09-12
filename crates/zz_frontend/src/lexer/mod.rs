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

pub mod cursor;

use crate::diag::{error_at, RawDiag};
use crate::span::Span;
use crate::token::{Token, TokenKind, Trivia, TriviaKind};

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<RawDiag>,
}

pub fn lex(source: &str) -> Lexed {
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
    pending: Vec<Trivia>,
    tokens: Vec<Token>,
    errors: Vec<RawDiag>,
    contexts: Vec<LexContext>,
    /// True when the previous string segment ended right before an
    /// interpolation `{`; the next `{` opens the interpolation context.
    pending_interp: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer {
            src,
            pos: 0,
            prev_sig: None,
            pending: Vec::new(),
            tokens: Vec::new(),
            errors: Vec::new(),
            contexts: Vec::new(),
            pending_interp: false,
        }
    }

    fn run(mut self) -> Lexed {
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
                '/' => self.emit_significant(TokenKind::Slash, self.pos, self.pos + 1),
                '#' => self.lex_line_comment(),
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
                '+' => self.emit_significant(TokenKind::Plus, self.pos, self.pos + 1),
                '-' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::Arrow, self.pos, self.pos + 2)
                }
                '-' => self.emit_significant(TokenKind::Minus, self.pos, self.pos + 1),
                '*' if self.peek_char_at(1) == Some('*') => {
                    self.emit_significant(TokenKind::StarStar, self.pos, self.pos + 2)
                }
                '*' => self.emit_significant(TokenKind::Star, self.pos, self.pos + 1),
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
                '<' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Le, self.pos, self.pos + 2)
                }
                '<' => self.emit_significant(TokenKind::Lt, self.pos, self.pos + 1),
                '>' if self.peek_char_at(1) == Some('=') => {
                    self.emit_significant(TokenKind::Ge, self.pos, self.pos + 2)
                }
                '>' => self.emit_significant(TokenKind::Gt, self.pos, self.pos + 1),
                '&' if self.peek_char_at(1) == Some('&') => {
                    self.emit_significant(TokenKind::AndAnd, self.pos, self.pos + 2)
                }
                '|' if self.peek_char_at(1) == Some('|') => {
                    self.emit_significant(TokenKind::OrOr, self.pos, self.pos + 2)
                }
                '|' if self.peek_char_at(1) == Some('>') => {
                    self.emit_significant(TokenKind::PipeGt, self.pos, self.pos + 2)
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
                '.' if self.peek_char_at(1) == Some('.') => {
                    self.emit_significant(TokenKind::DotDot, self.pos, self.pos + 2)
                }
                '.' => self.emit_significant(TokenKind::Dot, self.pos, self.pos + 1),
                '"' => self.lex_string(),
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
            let span = Span::new(self.pos as u32, self.src.len() as u32);
            self.errors
                .push(error_at("unterminated string literal", span));
        }
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            text: String::new(),
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
        let c = self.bump_char();
        let span = Span::new(start as u32, self.pos as u32);
        self.pending.push(Trivia {
            kind,
            text: c.to_string(),
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
        self.pending.push(Trivia {
            kind: TriviaKind::Comment,
            text: self.src[span.to_range()].to_string(),
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
                    let span = Span::new(start as u32, self.src.len() as u32);
                    self.errors
                        .push(error_at("unterminated block comment", span));
                    return;
                }
            }
        }
        let span = Span::new(start as u32, self.pos as u32);
        self.pending.push(Trivia {
            kind: TriviaKind::Comment,
            text: self.src[span.to_range()].to_string(),
            span,
        });
    }

    // --- significant tokens ----------------------------------------------

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

    /// Returns true if the next non-whitespace/non-newline in the source is
    /// `|>`. Used to allow multi-line pipe chains:
    /// ```zz
    /// val
    ///   |> f
    ///   |> g
    /// ```
    fn next_is_pipe_arrow(&self) -> bool {
        let mut offset = 1; // skip the newline we just saw
        loop {
            match self.peek_char_at(offset) {
                Some(' ' | '\t' | '\r') => offset += 1,
                Some('\n') => offset += 1,
                Some('|') => {
                    return self.peek_char_at(offset + 1) == Some('>');
                }
                _ => return false,
            }
        }
    }

    fn emit_significant(&mut self, kind: TokenKind, start: usize, end: usize) {
        let span = Span::new(start as u32, end as u32);
        let text = self.src[span.to_range()].to_string();
        self.pos = end;
        self.push_token(kind, span, text);
    }

    fn push_token(&mut self, kind: TokenKind, span: Span, text: String) {
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
        let text = self.src[span.to_range()].to_string();
        let kind = match text.as_str() {
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
            _ => TokenKind::Ident,
        };
        self.push_token(kind, span, text);
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() || c == '_' {
                self.bump_char();
            } else {
                break;
            }
        }
        let mut is_float = false;
        if self.peek_char() == Some('.') && self.peek_char_at(1).is_some_and(|c| c.is_ascii_digit())
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
        } else if self.peek_char() == Some('.') && self.peek_char_at(1) != Some('.') {
            // `1.` — a dot with no digits after it is not a float (and not a
            // range start, which would be `1..`).
            let span = Span::new(start as u32, (self.pos + '.'.len_utf8()) as u32);
            self.errors
                .push(error_at("expected digit after decimal point", span));
        }
        // `123abc` is a single invalid token, not two.
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
        let text = self.src[span.to_range()].to_string();
        let kind = if is_float {
            TokenKind::Float
        } else {
            TokenKind::Int
        };
        self.push_token(kind, span, text);
    }

    /// Begin a fresh string literal: consume the opening quote(s) and enter
    /// string mode. A `"""` opener starts a triple-quoted multiline string;
    /// otherwise a regular single-line string. The main loop then feeds
    /// characters through [`Lexer::lex_string_cont`] or
    /// [`Lexer::lex_triple_cont`].
    fn lex_string(&mut self) {
        let start = self.pos;
        let triple = self.src[start..].starts_with("\"\"\"");
        if triple {
            self.pos += 3; // opening `"""`
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
        });
    }

    /// Consume one unit of string content (a quote, an escape, an
    /// interpolation start, or a plain character).
    fn lex_string_cont(&mut self) {
        // Pop the current string context; continuation arms re-push it with
        // the updated value.
        let (start, value, is_nested) = match self.contexts.pop() {
            Some(LexContext::Str {
                start,
                value,
                is_nested,
                triple: false,
                segs,
            }) => {
                debug_assert!(segs.is_empty());
                (start, value, is_nested)
            }
            Some(LexContext::Str { triple: true, .. }) => {
                unreachable!("triple-quoted string dispatched to lex_string_cont")
            }
            _ => unreachable!("lex_string_cont called outside string mode"),
        };
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
                    self.push_token(TokenKind::Str, span, value);
                } else if self
                    .contexts
                    .iter()
                    .any(|c| matches!(c, LexContext::Interp { .. }))
                {
                    self.push_token(TokenKind::StrFmt, span, value);
                    self.contexts.push(LexContext::Str {
                        start: self.pos,
                        value: String::new(),
                        is_nested: false,
                        triple: false,
                        segs: Vec::new(),
                    });
                } else {
                    // Final closing quote — emit Str (complete string).
                    self.push_token(TokenKind::Str, span, value);
                }
            }
            // String interpolation: `{ident...` starts an embedded expression.
            // Emit the accumulated text as StrFmt and enter interpolation
            // mode (leaving the string context underneath); the main loop
            // lexes `{` as LBrace, the expression, and `}` as RBrace, popping
            // back into string mode for the continuation.
            Some('{') if self.peek_char_at(1).is_some_and(is_ident_start) => {
                let span = Span::new(start as u32, self.pos as u32);
                self.push_token(TokenKind::StrFmt, span, value);
                self.contexts.push(LexContext::Str {
                    start: self.pos,
                    value: String::new(),
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
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
                    Some(other) => {
                        let span =
                            Span::new((self.pos - 1) as u32, (self.pos + other.len_utf8()) as u32);
                        self.errors
                            .push(error_at(format!("unknown escape `\\{other}`"), span));
                        self.bump_char();
                    }
                    None => {
                        let span = Span::new(start as u32, self.src.len() as u32);
                        self.errors
                            .push(error_at("unterminated string literal", span));
                        return;
                    }
                }
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: false,
                    segs: Vec::new(),
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
                });
            }
            None => {
                let span = Span::new(start as u32, self.src.len() as u32);
                self.errors
                    .push(error_at("unterminated string literal", span));
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
        let (start, value, is_nested, segs) = match self.contexts.pop() {
            Some(LexContext::Str {
                start,
                value,
                is_nested,
                triple: true,
                segs,
            }) => (start, value, is_nested, segs),
            Some(LexContext::Str { triple: false, .. }) => {
                unreachable!("single-line string dispatched to lex_triple_cont")
            }
            _ => unreachable!("lex_triple_cont called outside string mode"),
        };
        // Closing delimiter.
        if self.src[self.pos..].starts_with("\"\"\"") {
            let indent = self.closing_triple_indent();
            self.pos += 3;
            let span = Span::new(start as u32, self.pos as u32);
            if is_nested {
                let idx = self.tokens.len();
                self.push_token(TokenKind::Str, span, value);
                self.dedent_triple_segments(&segs, idx, indent);
            } else if self
                .contexts
                .iter()
                .any(|c| matches!(c, LexContext::Interp { .. }))
            {
                let idx = self.tokens.len();
                self.push_token(TokenKind::StrFmt, span, value);
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
                });
            } else {
                let idx = self.tokens.len();
                self.push_token(TokenKind::Str, span, value);
                self.dedent_triple_segments(&segs, idx, indent);
            }
            return;
        }
        match self.peek_char() {
            // Interpolation: `{ident...`, `{1...`, `{(...` start an embedded
            // expression. `{{`, `{}` and `{"...` (JSON-like) stay literal.
            Some('{') if is_triple_interp_start(self.peek_char_at(1)) => {
                let span = Span::new(start as u32, self.pos as u32);
                let idx = self.tokens.len();
                self.push_token(TokenKind::StrFmt, span, value);
                let mut next_segs = segs;
                next_segs.push(idx);
                self.contexts.push(LexContext::Str {
                    start: self.pos,
                    value: String::new(),
                    is_nested,
                    triple: true,
                    segs: next_segs,
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
                    Some(other) => {
                        let span =
                            Span::new((self.pos - 1) as u32, (self.pos + other.len_utf8()) as u32);
                        self.errors
                            .push(error_at(format!("unknown escape `\\{other}`"), span));
                        self.bump_char();
                    }
                    None => {
                        let span = Span::new(start as u32, self.src.len() as u32);
                        self.errors
                            .push(error_at("unterminated string literal", span));
                        return;
                    }
                }
                self.contexts.push(LexContext::Str {
                    start,
                    value,
                    is_nested,
                    triple: true,
                    segs,
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
                });
            }
            None => {
                let span = Span::new(start as u32, self.src.len() as u32);
                self.errors
                    .push(error_at("unterminated string literal", span));
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
        let mut texts: Vec<String> = idxs.iter().map(|&i| self.tokens[i].text.clone()).collect();
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
            self.tokens[*tok_idx].text = new_text;
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

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Interpolation trigger inside triple-quoted strings: `{` opens an embedded
/// expression when followed by an identifier start, a digit, or `(`.
/// `{{`, `{}` and JSON-like `{"key"...` stay literal text.
fn is_triple_interp_start(c: Option<char>) -> bool {
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
            .map(|t| (t.kind, t.text))
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
        let toks = sig("\"\"\"a {{ b\"\"\"");
        assert_eq!(toks, vec![(TokenKind::Str, "a {{ b".to_string())]);
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
