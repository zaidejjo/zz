//! Recursive-descent parser with statement-level error recovery.
//!
//! Grammar (Phase 1.5):
//! ```text
//! program        := stmt* eof
//! stmt           := import_stmt | decl_stmt | func_stmt | return_stmt | expr_stmt
//! import_stmt    := 'import' IDENT ('.' IDENT)*
//! decl_stmt      := IDENT ':=' expr                       // short declaration
//!                | IDENT ':' type '=' expr                // explicit declaration
//! func_stmt      := 'func' IDENT ('<' IDENT (',' IDENT)* '>')? '(' param_list ')' ('->' type)? block
//! return_stmt    := 'return' expr?
//! param_list     := (param (',' param)*)?
//! param          := IDENT (':' type)?
//! block          := '{' stmt* '}'
//! type           := type_base ('|' type_base)*            // union
//! type_base      := 'int'|'float'|'bool'|'str'|'unit'
//!                | 'func' '(' type_list ')' '->' type       // function type
//!                | IDENT ('<' type (',' type)* '>')?
//!                | '(' type (',' type)* ')'                // tuple or grouped
//!                | '(' type ')' '->' type                  // function type (shorthand)
//!                | '[' type ']'                            // array
//!                | '{' type ':' type '}'                   // dict
//! expr           := elvis
//! elvis          := pipe ('??' pipe)*                        // unwrap or fallback
//! pipe           := range ('|>' range)*                    // pipeline
//! range          := or ('..' or)?                          // integer range
//! or             := and ('||' and)*
//! and            := equality ('&&' equality)*
//! equality       := relational (('=='|'!=') relational)*
//! relational     := additive (('<'|'>'|'<='|'>=') additive)*
//! additive       := multiplicative (('+'|'-') multiplicative)*
//! multiplicative := unary (('*'|'/'|'%') unary)*
//! unary          := 'try' unary | ('-'|'+'|'!') unary | postfix
//!                // 'try' binds one postfix chain:
//!                // `try a.b().c()` = try(a.b().c())
//!                // `try a.b() + c` = try(a.b()) + c
//!                // `try f()?` = try(try(f())) — postfix `?` binds tighter,
//!                // so redundant double-unwrap is a *type* error, not special syntax
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

pub mod decl;
pub mod expr;
pub mod recovery;
pub mod stmt;

use crate::ast::Program;
use crate::diag::{error_at, FixIt, RawDiag};
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
        block_depth: 0,
        delim_stack: Vec::new(),
        pending_gt: 0,
    };
    let program = parser.parse_program();
    let mut errors = parser.errors;
    // An unterminated string/comment swallows the rest of the file, so every
    // later diagnostic is fallout of the same root cause. Keep the root
    // cause plus anything before it (one bad quote never hides earlier real
    // errors); drop the cascade, including orphaned "unclosed" delimiters
    // whose closers became string content.
    if let Some(cut) = errors
        .iter()
        .filter(|e| e.message.contains("unterminated"))
        .filter_map(|e| e.span.map(|s| s.start))
        .min()
    {
        errors.retain(|e| {
            e.message.contains("unterminated")
                || (!e.message.starts_with("unclosed")
                    && e.span.map(|s| s.start < cut).unwrap_or(true))
        });
    }
    Parsed { program, errors }
}

/// A tracked open delimiter for mismatched-delimiter diagnostics.
#[derive(Debug, Clone)]
struct DelimEntry {
    open: TokenKind,
    span: Span,
}

/// The closing bracket matching an opener (`(` → `)`), for missing-closer
/// hints and insert fixes.
fn closer_for(open: TokenKind) -> &'static str {
    match open {
        TokenKind::LParen => ")",
        TokenKind::LBracket => "]",
        _ => "}",
    }
}

struct Parser<'a> {
    toks: Vec<Token<'a>>,
    pos: usize,
    errors: Vec<RawDiag>,
    /// Block nesting depth: only the top-level statement list pre-sizes
    /// from the token stream (see `parse_stmt_list`).
    block_depth: usize,
    /// Stack of open delimiters for mismatched-delimiter diagnostics.
    delim_stack: Vec<DelimEntry>,
    /// Owed `>` closes from split `>>` tokens. Nested generic type args
    /// (`Option<Option<int>>`) lex the adjacent closes as one `Shr`;
    /// each split banks one `>` for the enclosing argument list.
    pending_gt: u32,
}

impl<'a> Parser<'a> {
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
                    let want = closer_for(entry.open);
                    let at = Span::new(close_span.start, close_span.start);
                    self.errors.push(
                        error_at(
                            format!(
                                "unclosed `{}` (opened here) — add `{want}` before `{}`",
                                entry.open.describe(),
                                close.describe()
                            ),
                            entry.span,
                        )
                        .with_fixit(FixIt::safe(
                            at,
                            want,
                            format!("add missing `{want}`"),
                        )),
                    );
                }
                self.delim_stack.pop(); // Remove the matching opener.
            }
            None => {
                self.errors.push(
                    error_at(
                        format!(
                            "unexpected `{}` with no matching opening `{}` — remove it",
                            close.describe(),
                            expected.describe()
                        ),
                        close_span,
                    )
                    .with_fixit(FixIt::safe(
                        close_span,
                        "",
                        "remove this bracket",
                    )),
                );
            }
        }
    }

    fn check_unclosed_delims(&mut self) {
        let at = Span::new(self.src_len(), self.src_len());
        for entry in self.delim_stack.drain(..) {
            let want = closer_for(entry.open);
            self.errors.push(
                error_at(
                    format!(
                        "unclosed `{}` at end of file (opened here) — add `{want}` at end of file",
                        entry.open.describe()
                    ),
                    entry.span,
                )
                .with_fixit(FixIt::safe(
                    at,
                    want,
                    format!("add `{want}` at end of file"),
                )),
            );
        }
    }
}
