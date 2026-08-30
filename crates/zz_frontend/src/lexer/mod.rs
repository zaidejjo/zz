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
                self.lex_string_cont();
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
                    | TokenKind::Question
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

    /// Begin a fresh string literal: consume the opening quote and enter
    /// string mode. The main loop then feeds characters through
    /// [`Lexer::lex_string_cont`].
    fn lex_string(&mut self) {
        let start = self.pos;
        self.bump_char(); // opening quote
        let is_nested = matches!(self.contexts.last(), Some(LexContext::Interp { .. }));
        self.contexts.push(LexContext::Str {
            start,
            value: String::new(),
            is_nested,
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
            }) => (start, value, is_nested),
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
                });
            }
            None => {
                let span = Span::new(start as u32, self.src.len() as u32);
                self.errors
                    .push(error_at("unterminated string literal", span));
            }
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

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: returns true if the token stream contains a `StmtEnd` between
    /// the first `Int` and the first `PipeGt`.
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
