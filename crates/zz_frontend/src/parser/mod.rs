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

pub mod decl;
pub mod expr;
pub mod recovery;
pub mod stmt;

use crate::ast::{Program, Stmt};
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
}
