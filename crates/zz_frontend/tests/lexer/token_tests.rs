//! Lexer token tests.

use zz_frontend::tests::common::{lex_kinds, parse_ok};
use zz_frontend::token::TokenKind as K;

#[test]
fn basic_expression() {
    assert_eq!(
        lex_kinds("x := 1 + 2"),
        vec![K::Ident, K::ColonEq, K::Int, K::Plus, K::Int, K::Eof]
    );
}

#[test]
fn newline_is_stmt_end_at_depth_zero() {
    assert_eq!(lex_kinds("1\n2"), vec![K::Int, K::StmtEnd, K::Int, K::Eof]);
}

#[test]
fn newline_after_operator_is_trivia() {
    assert_eq!(lex_kinds("1 +\n2"), vec![K::Int, K::Plus, K::Int, K::Eof]);
}

#[test]
fn newline_after_assign_is_trivia() {
    assert_eq!(
        lex_kinds("x :=\n1"),
        vec![K::Ident, K::ColonEq, K::Int, K::Eof]
    );
}

#[test]
fn newline_inside_parens_is_trivia() {
    assert_eq!(
        lex_kinds("(1 +\n2)"),
        vec![K::LParen, K::Int, K::Plus, K::Int, K::RParen, K::Eof]
    );
}

#[test]
fn semicolon_is_stmt_end() {
    assert_eq!(lex_kinds("1;2"), vec![K::Int, K::StmtEnd, K::Int, K::Eof]);
}

#[test]
fn comments_are_trivia() {
    let lexed = zz_frontend::lex("1 // comment\n2");
    assert_eq!(
        lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
        vec![K::Int, K::StmtEnd, K::Int, K::Eof]
    );
    // The comment rides as leading trivia on the StmtEnd that follows it.
    let stmt_end = &lexed.tokens[1];
    assert_eq!(stmt_end.leading.len(), 2);
    assert_eq!(
        stmt_end.leading[0].kind,
        zz_frontend::token::TriviaKind::Whitespace
    );
    assert_eq!(
        stmt_end.leading[1].kind,
        zz_frontend::token::TriviaKind::Comment
    );
}

#[test]
fn hash_line_comment() {
    let lexed = zz_frontend::lex("1 # comment\n2");
    assert_eq!(
        lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
        vec![K::Int, K::StmtEnd, K::Int, K::Eof]
    );
    let stmt_end = &lexed.tokens[1];
    assert!(stmt_end
        .leading
        .iter()
        .any(|t| t.kind == zz_frontend::token::TriviaKind::Comment));
}

#[test]
fn nested_block_comments() {
    let lexed = zz_frontend::lex("1 /* a /* b */ c */ 2");
    assert!(lexed.errors.is_empty(), "errors: {:?}", lexed.errors);
    assert_eq!(
        lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
        vec![K::Int, K::Int, K::Eof]
    );
}

#[test]
fn unterminated_block_comment_errors() {
    let lexed = zz_frontend::lex("1 /* never closed");
    assert_eq!(lexed.errors.len(), 1);
}

#[test]
fn invalid_number_literal() {
    let lexed = zz_frontend::lex("123abc");
    assert_eq!(lexed.errors.len(), 1);
}

#[test]
fn float_literals() {
    assert_eq!(
        lex_kinds("1.5 + 2.0"),
        vec![K::Float, K::Plus, K::Float, K::Eof]
    );
}

#[test]
fn underscores_in_numbers() {
    assert_eq!(lex_kinds("1_000"), vec![K::Int, K::Eof]);
}

#[test]
fn unexpected_character_errors() {
    let lexed = zz_frontend::lex("1 @ 2");
    assert_eq!(lexed.errors.len(), 1);
}

#[test]
fn keywords() {
    assert_eq!(
        lex_kinds("import func return if else while match true false struct for in break continue"),
        vec![
            K::Import,
            K::Func,
            K::Return,
            K::If,
            K::Else,
            K::While,
            K::Match,
            K::True,
            K::False,
            K::Struct,
            K::For,
            K::In,
            K::Break,
            K::Continue,
            K::Eof
        ]
    );
}

#[test]
fn multi_char_operators() {
    assert_eq!(
        lex_kinds("a == b != c <= d >= e && f || g -> h"),
        vec![
            K::Ident,
            K::Eq,
            K::Ident,
            K::Ne,
            K::Ident,
            K::Le,
            K::Ident,
            K::Ge,
            K::Ident,
            K::AndAnd,
            K::Ident,
            K::OrOr,
            K::Ident,
            K::Arrow,
            K::Ident,
            K::Eof
        ]
    );
}

#[test]
fn string_literals() {
    let lexed = zz_frontend::lex(r#""hello\nworld""#);
    assert!(lexed.errors.is_empty(), "errors: {:?}", lexed.errors);
    assert_eq!(lexed.tokens[0].kind, K::Str);
    assert_eq!(lexed.tokens[0].text, "hello\nworld");
}

#[test]
fn unterminated_string_errors() {
    let lexed = zz_frontend::lex("\"oops");
    assert_eq!(lexed.errors.len(), 1);
}

#[test]
fn unknown_escape_errors() {
    let lexed = zz_frontend::lex(r#""\q""#);
    assert_eq!(lexed.errors.len(), 1);
}

#[test]
fn braces_and_pipe() {
    assert_eq!(
        lex_kinds("|x| x + 1"),
        vec![
            K::Pipe,
            K::Ident,
            K::Pipe,
            K::Ident,
            K::Plus,
            K::Int,
            K::Eof
        ]
    );
    assert_eq!(
        lex_kinds("a || b"),
        vec![K::Ident, K::OrOr, K::Ident, K::Eof]
    );
}

#[test]
fn brackets_and_colon_eq() {
    assert_eq!(
        lex_kinds("[1, 2]"),
        vec![K::LBracket, K::Int, K::Comma, K::Int, K::RBracket, K::Eof]
    );
    assert_eq!(
        lex_kinds("x := 1"),
        vec![K::Ident, K::ColonEq, K::Int, K::Eof]
    );
}

#[test]
fn pipeline_operator() {
    assert_eq!(
        lex_kinds("a |> f(b)"),
        vec![
            K::Ident,
            K::PipeGt,
            K::Ident,
            K::LParen,
            K::Ident,
            K::RParen,
            K::Eof
        ]
    );
    // `|>` must not swallow closure pipes or `||`.
    assert_eq!(
        lex_kinds("|x| x"),
        vec![K::Pipe, K::Ident, K::Pipe, K::Ident, K::Eof]
    );
    assert_eq!(
        lex_kinds("a || b"),
        vec![K::Ident, K::OrOr, K::Ident, K::Eof]
    );
}
