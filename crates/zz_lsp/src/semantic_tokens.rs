//! Semantic tokens: walk the AST to produce token highlights for
//! keywords, functions, types, parameters, variables, strings, and
//! numbers.

use tower_lsp::lsp_types::*;
use zz_frontend::ast::*;
use zz_frontend::span::Span;

/// Semantic token types we emit.
/// These map to the standard LSP semantic token type legend.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub(crate) enum TokenType {
    Keyword,
    Function,
    Struct,
    Type,
    Parameter,
    Variable,
    String,
    Number,
    Operator,
    Comment,
    Namespace,
    Decorator,
}

impl TokenType {
    /// Index into the token type legend.
    pub(crate) fn index(self) -> u32 {
        match self {
            TokenType::Keyword => 0,
            TokenType::Function => 1,
            TokenType::Struct => 2,
            TokenType::Type => 3,
            TokenType::Parameter => 4,
            TokenType::Variable => 5,
            TokenType::String => 6,
            TokenType::Number => 7,
            TokenType::Operator => 8,
            TokenType::Comment => 9,
            TokenType::Namespace => 10,
            TokenType::Decorator => 11,
        }
    }
}

/// The standard LSP semantic token type legend.
pub(crate) fn token_type_legend() -> Vec<SemanticTokenType> {
    vec![
        SemanticTokenType::KEYWORD,
        SemanticTokenType::FUNCTION,
        SemanticTokenType::STRUCT,
        SemanticTokenType::TYPE,
        SemanticTokenType::PARAMETER,
        SemanticTokenType::VARIABLE,
        SemanticTokenType::STRING,
        SemanticTokenType::NUMBER,
        SemanticTokenType::OPERATOR,
        SemanticTokenType::COMMENT,
        SemanticTokenType::NAMESPACE,
        SemanticTokenType::DECORATOR,
    ]
}

/// A raw semantic token (line, col, len, type, modifiers).
#[derive(Debug, Clone)]
pub(crate) struct RawToken {
    pub line: u32,
    pub col: u32,
    pub len: u32,
    pub token_type: TokenType,
}

/// Collect all semantic tokens from the program.
pub(crate) fn collect_semantic_tokens(program: &Program, source: &str) -> Vec<RawToken> {
    let mut tokens = Vec::new();
    for stmt in &program.stmts {
        collect_stmt_tokens(stmt, source, &mut tokens);
    }
    // Sort by (line, col) for LSP encoding.
    tokens.sort_by_key(|t| (t.line, t.col));
    tokens
}

/// Encode tokens into LSP SemanticToken (delta encoding).
pub(crate) fn encode_tokens(tokens: &[RawToken], source: &str) -> Vec<SemanticToken> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_col = 0u32;

    for token in tokens {
        let pos = crate::convert::offset_to_position(source, token.line);
        let line = pos.line;
        let col = pos.character;

        let delta_line = line.saturating_sub(prev_line);
        let delta_start = if delta_line == 0 {
            col.saturating_sub(prev_col)
        } else {
            col
        };

        result.push(SemanticToken {
            delta_line,
            delta_start,
            length: token.len,
            token_type: token.token_type.index(),
            token_modifiers_bitset: 0,
        });

        prev_line = line;
        prev_col = col;
    }
    result
}

fn collect_stmt_tokens(stmt: &Stmt, source: &str, out: &mut Vec<RawToken>) {
    match stmt {
        Stmt::Func {
            name,
            generics,
            params,
            ret,
            body,
            ..
        } => {
            // "func" keyword.
            push_keyword_token(stmt.span(), "func", source, out);
            // Function name.
            push_name_tokens(name, TokenType::Function, source, out);
            // Generics.
            for g in generics {
                push_ident_token(&g.name, g.span, TokenType::Type, source, out);
            }
            // Parameters.
            for param in params {
                push_ident_token(
                    &param.name.name,
                    param.name.span,
                    TokenType::Parameter,
                    source,
                    out,
                );
                if let Some(ty) = &param.ty {
                    collect_type_tokens(ty, source, out);
                }
            }
            // Return type.
            if let Some(ret_ty) = ret {
                collect_type_tokens(ret_ty, source, out);
            }
            // Body.
            collect_block_tokens(body, source, out);
        }
        Stmt::Struct { name, fields, .. } => {
            push_keyword_token(stmt.span(), "struct", source, out);
            push_name_tokens(name, TokenType::Struct, source, out);
            for (fname, fty) in fields {
                push_ident_token(&fname.name, fname.span, TokenType::Variable, source, out);
                collect_type_tokens(fty, source, out);
            }
        }
        Stmt::Decl {
            ty, name, value, ..
        } => {
            push_ident_token(&name.name, name.span, TokenType::Variable, source, out);
            if let Some(ty) = ty {
                collect_type_tokens(ty, source, out);
            }
            collect_expr_tokens(value, source, out);
        }
        Stmt::Import { path, .. } => {
            push_keyword_token(stmt.span(), "import", source, out);
            let _ = path; // Dotted path parts have no individual spans to tokenize.
        }
        Stmt::Return { value, .. } => {
            push_keyword_token(stmt.span(), "return", source, out);
            if let Some(v) = value {
                collect_expr_tokens(v, source, out);
            }
        }
        Stmt::For {
            var, iter, body, ..
        } => {
            push_keyword_token(stmt.span(), "for", source, out);
            push_ident_token(&var.name, var.span, TokenType::Variable, source, out);
            push_keyword_token_stmt(stmt.span(), "in", source, out);
            collect_expr_tokens(iter, source, out);
            collect_block_tokens(body, source, out);
        }
        Stmt::Break { .. } => push_keyword_token(stmt.span(), "break", source, out),
        Stmt::Continue { .. } => push_keyword_token(stmt.span(), "continue", source, out),
        Stmt::Defer { expr, .. } => {
            push_keyword_token(stmt.span(), "defer", source, out);
            collect_expr_tokens(expr, source, out);
        }
        Stmt::Assign { target, value, .. } => {
            collect_expr_tokens(target, source, out);
            collect_expr_tokens(value, source, out);
        }
        Stmt::Expr(e) => collect_expr_tokens(e, source, out),
    }
}

fn collect_block_tokens(block: &Block, source: &str, out: &mut Vec<RawToken>) {
    for stmt in &block.stmts {
        collect_stmt_tokens(stmt, source, out);
    }
}

fn collect_expr_tokens(expr: &Expr, source: &str, out: &mut Vec<RawToken>) {
    match expr {
        Expr::Int { span, .. } => {
            push_token("int", *span, TokenType::Number, source, out);
        }
        Expr::Float { span, .. } => {
            push_token("float", *span, TokenType::Number, source, out);
        }
        Expr::Str { value, span } => {
            push_token(
                &format!("\"{}\"", value),
                *span,
                TokenType::String,
                source,
                out,
            );
        }
        Expr::Bool { value, span } => {
            let s = if *value { "true" } else { "false" };
            push_token(s, *span, TokenType::Keyword, source, out);
        }
        Expr::Ident { name, span } => {
            push_token(name, *span, TokenType::Variable, source, out);
        }
        Expr::Path { parts, span } => {
            push_token(&parts.join("."), *span, TokenType::Namespace, source, out);
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, _) = part {
                    collect_expr_tokens(e, source, out);
                }
            }
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_expr_tokens(callee, source, out);
            for arg in args {
                collect_expr_tokens(arg, source, out);
            }
            for (_, arg) in named {
                collect_expr_tokens(arg, source, out);
            }
        }
        Expr::Binary {
            op, left, right, ..
        } => {
            collect_expr_tokens(left, source, out);
            collect_expr_tokens(right, source, out);
            let _ = op;
        }
        Expr::Unary { expr, .. } => collect_expr_tokens(expr, source, out),
        Expr::If {
            cond, then, els, ..
        } => {
            push_keyword_token(expr.span(), "if", source, out);
            collect_expr_tokens(cond, source, out);
            collect_block_tokens(then, source, out);
            if let Some(e) = els {
                push_keyword_token(e.span(), "else", source, out);
                collect_expr_tokens(e, source, out);
            }
        }
        Expr::While { cond, body, .. } => {
            push_keyword_token(expr.span(), "while", source, out);
            collect_expr_tokens(cond, source, out);
            collect_block_tokens(body, source, out);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            push_keyword_token(expr.span(), "match", source, out);
            collect_expr_tokens(scrutinee, source, out);
            for arm in arms {
                collect_pattern_tokens(&arm.pat, source, out);
                collect_expr_tokens(&arm.body, source, out);
            }
        }
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            ..
        } => {
            push_keyword_token(expr.span(), "if", source, out);
            push_keyword_token_stmt(expr.span(), "let", source, out);
            collect_pattern_tokens(pat, source, out);
            collect_expr_tokens(value, source, out);
            collect_block_tokens(then, source, out);
            if let Some(e) = els {
                push_keyword_token(e.span(), "else", source, out);
                collect_expr_tokens(e, source, out);
            }
        }
        Expr::Try { expr, .. } => {
            collect_expr_tokens(expr, source, out);
        }
        Expr::Block(b) => collect_block_tokens(b, source, out),
        Expr::Array { elems, .. } => {
            for e in elems {
                collect_expr_tokens(e, source, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_expr_tokens(k, source, out);
                collect_expr_tokens(v, source, out);
            }
        }
        Expr::Field { obj, .. } => collect_expr_tokens(obj, source, out),
        Expr::Index { obj, index, .. } => {
            collect_expr_tokens(obj, source, out);
            collect_expr_tokens(index, source, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_expr_tokens(obj, source, out);
            if let Some(s) = start {
                collect_expr_tokens(s, source, out);
            }
            if let Some(e) = end {
                collect_expr_tokens(e, source, out);
            }
        }
        Expr::Range { start, end, .. } => {
            collect_expr_tokens(start, source, out);
            collect_expr_tokens(end, source, out);
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_expr_tokens(body, source, out);
            collect_expr_tokens(iter, source, out);
            if let Some(f) = filter {
                collect_expr_tokens(f, source, out);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_expr_tokens(v, source, out);
            }
        }
        Expr::Closure { params, body, .. } => {
            for param in params {
                push_ident_token(
                    &param.name.name,
                    param.name.span,
                    TokenType::Parameter,
                    source,
                    out,
                );
            }
            collect_expr_tokens(body, source, out);
        }
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                collect_expr_tokens(a, source, out);
            }
        }
        Expr::Paren { expr, .. } => collect_expr_tokens(expr, source, out),
    }
}

fn collect_type_tokens(ty: &Ty, source: &str, out: &mut Vec<RawToken>) {
    match &ty.kind {
        TyKind::Named(name, generics) => {
            push_token(name, ty.span, TokenType::Type, source, out);
            for g in generics {
                collect_type_tokens(g, source, out);
            }
        }
        TyKind::Array(inner) => collect_type_tokens(inner, source, out),
        TyKind::Dict(key, val) => {
            collect_type_tokens(key, source, out);
            collect_type_tokens(val, source, out);
        }
        TyKind::Option(inner) => collect_type_tokens(inner, source, out),
        TyKind::Result(ok, err) => {
            collect_type_tokens(ok, source, out);
            collect_type_tokens(err, source, out);
        }
        TyKind::Tuple(elems) => {
            for e in elems {
                collect_type_tokens(e, source, out);
            }
        }
        TyKind::Func(params, ret) => {
            for p in params {
                collect_type_tokens(p, source, out);
            }
            collect_type_tokens(ret, source, out);
        }
        TyKind::Union(variants) => {
            for v in variants {
                collect_type_tokens(v, source, out);
            }
        }
        _ => {} // Primitive types (int, float, bool, str, unit) — no span to emit.
    }
}

fn collect_pattern_tokens(pat: &zz_frontend::ast::Pattern, source: &str, out: &mut Vec<RawToken>) {
    match pat {
        zz_frontend::ast::Pattern::Binding { name } => {
            push_ident_token(&name.name, name.span, TokenType::Variable, source, out);
        }
        zz_frontend::ast::Pattern::Variant { arg: Some(a), .. } => {
            collect_pattern_tokens(a, source, out);
        }
        zz_frontend::ast::Pattern::Variant { .. } => {}
        _ => {}
    }
}

// ── Token pushing helpers ────────────────────────────────────────────────

/// Push a keyword token found by searching within a span.
fn push_keyword_token(span: Span, keyword: &str, source: &str, out: &mut Vec<RawToken>) {
    if let Some(kw_span) = find_keyword_in_span(source, span, keyword) {
        let pos = crate::convert::offset_to_position(source, kw_span.start);
        out.push(RawToken {
            line: kw_span.start,
            col: pos.character,
            len: keyword.len() as u32,
            token_type: TokenType::Keyword,
        });
    }
}

/// Push a keyword token for a statement — search the entire statement span.
fn push_keyword_token_stmt(span: Span, keyword: &str, source: &str, out: &mut Vec<RawToken>) {
    push_keyword_token(span, keyword, source, out);
}

/// Push a token for a named identifier (function, struct name).
fn push_name_tokens(name: &[String], token_type: TokenType, source: &str, out: &mut Vec<RawToken>) {
    let joined = name.join(".");
    if let Some(span) = find_name_in_source(source, &joined) {
        let pos = crate::convert::offset_to_position(source, span.start);
        out.push(RawToken {
            line: span.start,
            col: pos.character,
            len: span.end - span.start,
            token_type,
        });
    }
}

/// Push a token for an identifier at a specific span.
fn push_ident_token(
    _name: &str,
    span: Span,
    token_type: TokenType,
    source: &str,
    out: &mut Vec<RawToken>,
) {
    let pos = crate::convert::offset_to_position(source, span.start);
    out.push(RawToken {
        line: span.start,
        col: pos.character,
        len: span.end - span.start,
        token_type,
    });
}

/// Push a token for a literal or matched text.
fn push_token(
    _text: &str,
    span: Span,
    token_type: TokenType,
    source: &str,
    out: &mut Vec<RawToken>,
) {
    let pos = crate::convert::offset_to_position(source, span.start);
    out.push(RawToken {
        line: span.start,
        col: pos.character,
        len: span.end - span.start,
        token_type,
    });
}

/// Find a keyword within a statement span.
fn find_keyword_in_span(source: &str, span: Span, keyword: &str) -> Option<Span> {
    let slice = &source[span.to_range()];
    let kw_bytes = keyword.as_bytes();
    let slice_bytes = slice.as_bytes();
    for i in 0..slice.len() {
        if slice_bytes[i..].starts_with(kw_bytes) {
            let start = span.start + i as u32;
            let end = start + keyword.len() as u32;
            let prev_ok = i == 0 || !slice.as_bytes()[i - 1].is_ascii_alphanumeric();
            let next_ok = i + keyword.len() >= slice.len()
                || !slice.as_bytes()[i + keyword.len()].is_ascii_alphanumeric();
            if prev_ok && next_ok {
                return Some(Span::new(start, end));
            }
        }
    }
    None
}

/// Find a name as a standalone token in source.
fn find_name_in_source(source: &str, name: &str) -> Option<Span> {
    let bytes = source.as_bytes();
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len() as u32;

    for i in 0..bytes.len() {
        if bytes[i..].starts_with(name_bytes) {
            let start = i as u32;
            let end = start + name_len;
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let next_ok =
                end as usize >= bytes.len() || !bytes[end as usize].is_ascii_alphanumeric();
            if prev_ok && next_ok {
                return Some(Span::new(start, end));
            }
        }
    }
    None
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::parse;

    #[test]
    fn token_type_legend_length() {
        let legend = token_type_legend();
        assert_eq!(legend.len(), 12);
    }

    #[test]
    fn keywords_are_highlighted() {
        let src = "func f() { if true { return } while false { break } }\n";
        let parsed = parse(src);
        let tokens = collect_semantic_tokens(&parsed.program, src);
        let keywords: Vec<_> = tokens
            .iter()
            .filter(|t| t.token_type == TokenType::Keyword)
            .collect();
        // Should find: func, if, return, while, break
        assert!(
            keywords.len() >= 4,
            "expected >= 4 keyword tokens, got {}",
            keywords.len()
        );
    }

    #[test]
    fn functions_are_highlighted() {
        let src = "func add(a: int, b: int) -> int { return a + b }\n";
        let parsed = parse(src);
        let tokens = collect_semantic_tokens(&parsed.program, src);
        let funcs: Vec<_> = tokens
            .iter()
            .filter(|t| t.token_type == TokenType::Function)
            .collect();
        assert_eq!(funcs.len(), 1, "expected 1 function token");
    }

    #[test]
    fn variables_are_highlighted() {
        let src = "x := 10\n";
        let parsed = parse(src);
        let tokens = collect_semantic_tokens(&parsed.program, src);
        let vars: Vec<_> = tokens
            .iter()
            .filter(|t| t.token_type == TokenType::Variable)
            .collect();
        assert!(!vars.is_empty(), "expected variable tokens");
    }

    #[test]
    fn numbers_are_highlighted() {
        let src = "x := 42\ny := 3.14\n";
        let parsed = parse(src);
        let tokens = collect_semantic_tokens(&parsed.program, src);
        let nums: Vec<_> = tokens
            .iter()
            .filter(|t| t.token_type == TokenType::Number)
            .collect();
        assert_eq!(nums.len(), 2, "expected 2 number tokens");
    }

    #[test]
    fn strings_are_highlighted() {
        let src = "s := \"hello\"\n";
        let parsed = parse(src);
        let tokens = collect_semantic_tokens(&parsed.program, src);
        let strs: Vec<_> = tokens
            .iter()
            .filter(|t| t.token_type == TokenType::String)
            .collect();
        assert_eq!(strs.len(), 1, "expected 1 string token");
    }

    #[test]
    fn struct_keyword_highlighted() {
        let src = "struct Point { x: int, y: int }\n";
        let parsed = parse(src);
        let tokens = collect_semantic_tokens(&parsed.program, src);
        let keywords: Vec<_> = tokens
            .iter()
            .filter(|t| t.token_type == TokenType::Keyword)
            .collect();
        assert!(
            keywords.iter().any(|t| {
                let pos = crate::convert::offset_to_position(src, t.line);
                // "struct" is at line 0
                pos.line == 0
            }),
            "expected struct keyword"
        );
    }

    #[test]
    fn encode_produces_delta_encoding() {
        let src = "x := 1\ny := 2\n";
        let parsed = parse(src);
        let tokens = collect_semantic_tokens(&parsed.program, src);
        let encoded = encode_tokens(&tokens, src);
        // First token should have delta_line = 0.
        if let Some(first) = encoded.first() {
            assert_eq!(first.delta_line, 0);
        }
    }

    #[test]
    fn empty_program_has_no_tokens() {
        let parsed = parse("");
        let tokens = collect_semantic_tokens(&parsed.program, "");
        assert!(tokens.is_empty());
    }
}
