//! The interactive REPL.
//!
//! Line-based with multi-line continuation: if the accumulated buffer ends on
//! an operator, open paren, or `let ... =`, the prompt continues on the next
//! line. Newlines inside parens are handled by the lexer (they're trivia at
//! depth > 0).

use std::io::{self, BufRead, Write};

use zz_frontend::lexer::lex;
use zz_frontend::token::TokenKind;

use crate::session::Session;

const PROMPT: &str = "zz> ";
const CONTINUATION: &str = "  | ";

pub fn run() -> io::Result<()> {
    let mut session = Session::new("<repl>");
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();

    print!("{PROMPT}");
    stdout.flush()?;

    for line in stdin.lock().lines() {
        let line = line?;
        buffer.push_str(&line);
        buffer.push('\n');

        if needs_more_input(&buffer) {
            print!("{CONTINUATION}");
            stdout.flush()?;
            continue;
        }

        let trimmed = buffer.trim();
        if !trimmed.is_empty() {
            let output = session.eval_to_console(&buffer);
            if !output.is_empty() {
                println!("{output}");
            }
        }
        buffer.clear();
        print!("{PROMPT}");
        stdout.flush()?;
    }
    // EOF (Ctrl-D): exit cleanly.
    println!();
    Ok(())
}

/// Does the accumulated buffer need another line? True when it ends on a
/// token that cannot complete a statement.
fn needs_more_input(src: &str) -> bool {
    let lexed = lex(src);
    // Unterminated block comment: keep going.
    if lexed
        .errors
        .iter()
        .any(|e| e.message.contains("unterminated block comment"))
    {
        return true;
    }
    let last = lexed
        .tokens
        .iter()
        .rev()
        .find(|t| t.kind != TokenKind::Eof && t.kind != TokenKind::StmtEnd);
    matches!(
        last.map(|t| t.kind),
        Some(
            TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::Assign
                | TokenKind::ColonEq
                | TokenKind::LParen
                | TokenKind::LBracket
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_on_trailing_operator() {
        assert!(needs_more_input("x := 1 +\n"));
    }

    #[test]
    fn continuation_on_open_paren() {
        assert!(needs_more_input("(1 +\n"));
    }

    #[test]
    fn continuation_on_let_equals() {
        assert!(needs_more_input("let x =\n"));
    }

    #[test]
    fn no_continuation_on_complete_line() {
        assert!(!needs_more_input("let x = 1 + 2\n"));
    }

    #[test]
    fn no_continuation_on_multiline_parens_complete() {
        assert!(!needs_more_input("(1 +\n2)\n"));
    }

    #[test]
    fn continuation_on_unterminated_comment() {
        assert!(needs_more_input("1 /* oops\n"));
    }
}
