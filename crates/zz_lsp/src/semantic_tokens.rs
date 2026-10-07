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
/// Collect with the set of known function names (bare and qualified).
/// Call callees found in `known` tokenize as functions instead of plain
/// variables/namespaces — this is what colors `pow(2, 3)` and
/// `table.render(..)` as calls.
pub(crate) fn collect_semantic_tokens_with(
    program: &Program,
    source: &str,
    known: &std::collections::HashSet<String>,
) -> Vec<RawToken> {
    let mut tokens = Vec::new();
    for stmt in &program.stmts {
        collect_stmt_tokens(stmt, source, known, &mut tokens);
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

fn collect_stmt_tokens(
    stmt: &Stmt,
    source: &str,
    known: &std::collections::HashSet<String>,
    out: &mut Vec<RawToken>,
) {
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
                push_ident_token(&g.name.name, g.span, TokenType::Type, source, out);
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
                    collect_type_tokens(ty, source, known, out);
                }
            }
            // Return type.
            if let Some(ret_ty) = ret {
                collect_type_tokens(ret_ty, source, known, out);
            }
            // Body.
            collect_block_tokens(body, source, known, out);
        }
        Stmt::Struct { name, fields, .. } => {
            push_keyword_token(stmt.span(), "struct", source, out);
            push_name_tokens(name, TokenType::Struct, source, out);
            for (fname, fty) in fields {
                push_ident_token(&fname.name, fname.span, TokenType::Variable, source, out);
                collect_type_tokens(fty, source, known, out);
            }
        }
        Stmt::TypeAlias {
            name,
            generics,
            target,
            ..
        } => {
            // `type` is contextual (lexes as Ident): highlight by span.
            push_keyword_token(stmt.span(), "type", source, out);
            push_name_tokens(name, TokenType::Struct, source, out);
            for g in generics {
                push_ident_token(&g.name, g.span, TokenType::Type, source, out);
            }
            collect_type_tokens(target, source, known, out);
        }
        Stmt::Enum { name, variants, .. } => {
            // `enum` is contextual (lexes as Ident): highlight by span.
            push_keyword_token(stmt.span(), "enum", source, out);
            push_name_tokens(name, TokenType::Struct, source, out);
            for (vname, payload) in variants {
                push_ident_token(&vname.name, vname.span, TokenType::Function, source, out);
                if let Some(pty) = payload {
                    collect_type_tokens(pty, source, known, out);
                }
            }
        }
        Stmt::Impl { name, methods, .. } => {
            push_keyword_token(stmt.span(), "impl", source, out);
            push_name_tokens(name, TokenType::Struct, source, out);
            for method in methods {
                collect_stmt_tokens(method, source, known, out);
            }
        }
        Stmt::Decl {
            ty, name, value, ..
        } => {
            push_ident_token(&name.name, name.span, TokenType::Variable, source, out);
            if let Some(ty) = ty {
                collect_type_tokens(ty, source, known, out);
            }
            collect_expr_tokens(value, source, known, out);
        }
        Stmt::Import { path, items, .. } => {
            push_keyword_token(stmt.span(), "import", source, out);
            let _ = path; // Dotted path parts have no individual spans to tokenize.
                          // Selectively imported names tokenize like their definitions:
                          // known functions read as functions (`pow` in
                          // `import std.math(pow)`), everything else as variables.
            for item in items {
                if let zz_frontend::ast::ImportItem::Named { name, alias, span } = item {
                    let target = alias.as_ref().unwrap_or(name);
                    let kind = if known.contains(target) {
                        TokenType::Function
                    } else {
                        TokenType::Variable
                    };
                    push_ident_token(target, *span, kind, source, out);
                }
            }
        }
        Stmt::Return { value, .. } => {
            push_keyword_token(stmt.span(), "return", source, out);
            if let Some(v) = value {
                collect_expr_tokens(v, source, known, out);
            }
        }
        Stmt::For {
            vars, iter, body, ..
        } => {
            push_keyword_token(stmt.span(), "for", source, out);
            for (i, v) in vars.iter().enumerate() {
                if i > 0 {
                    // push comma token
                    push_keyword_token(stmt.span(), ",", source, out);
                }
                push_ident_token(&v.name, v.span, TokenType::Variable, source, out);
            }
            push_keyword_token_stmt(stmt.span(), "in", source, out);
            collect_expr_tokens(iter, source, known, out);
            collect_block_tokens(body, source, known, out);
        }
        Stmt::Break { .. } => push_keyword_token(stmt.span(), "break", source, out),
        Stmt::Continue { .. } => push_keyword_token(stmt.span(), "continue", source, out),
        Stmt::Defer { expr, .. } => {
            push_keyword_token(stmt.span(), "defer", source, out);
            collect_expr_tokens(expr, source, known, out);
        }
        Stmt::Assign { target, value, .. } => {
            collect_expr_tokens(target, source, known, out);
            collect_expr_tokens(value, source, known, out);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            collect_expr_tokens(target, source, known, out);
            collect_expr_tokens(value, source, known, out);
        }
        Stmt::Destructure { value, .. } => collect_expr_tokens(value, source, known, out),
        Stmt::ExternBlock { items, .. } => {
            for item in items {
                // "func" keyword
                push_keyword_token(item.span, "func", source, out);
                // function name
                push_ident_token(
                    &item.name.name,
                    item.name.span,
                    TokenType::Function,
                    source,
                    out,
                );
                // parameters
                for (i, param) in item.params.iter().enumerate() {
                    if i > 0 {
                        // skip comma
                    }
                    push_ident_token(
                        &param.name.name,
                        param.name.span,
                        TokenType::Parameter,
                        source,
                        out,
                    );
                    if let Some(ty) = &param.ty {
                        collect_type_tokens(ty, source, known, out);
                    }
                }
                // return type
                if let Some(ret) = &item.ret {
                    // skip arrow
                    collect_type_tokens(ret, source, known, out);
                }
                // skip semicolon
            }
        }
        Stmt::Link { .. } => {}
        Stmt::Expr(e) => collect_expr_tokens(e, source, known, out),
    }
}

fn collect_block_tokens(
    block: &Block,
    source: &str,
    known: &std::collections::HashSet<String>,
    out: &mut Vec<RawToken>,
) {
    for stmt in &block.stmts {
        collect_stmt_tokens(stmt, source, known, out);
    }
}

fn collect_expr_tokens(
    expr: &Expr,
    source: &str,
    known: &std::collections::HashSet<String>,
    out: &mut Vec<RawToken>,
) {
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
                    collect_expr_tokens(e, source, known, out);
                }
            }
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            // Callees known to the checker tokenize as functions — this is
            // what colors `pow(2, 3)` and `table.render(..)` as calls
            // instead of plain variables/namespaces.
            let classified = match callee.as_ref() {
                Expr::Ident { name, span } if known.contains(name) => {
                    push_ident_token(name, *span, TokenType::Function, source, out);
                    true
                }
                Expr::Path { parts, span } if known.contains(&parts.join(".")) => {
                    push_token(&parts.join("."), *span, TokenType::Function, source, out);
                    true
                }
                _ => false,
            };
            if !classified {
                collect_expr_tokens(callee, source, known, out);
            }
            for arg in args {
                collect_expr_tokens(arg, source, known, out);
            }
            for (_, arg) in named {
                collect_expr_tokens(arg, source, known, out);
            }
        }
        Expr::Binary {
            op, left, right, ..
        } => {
            collect_expr_tokens(left, source, known, out);
            collect_expr_tokens(right, source, known, out);
            let _ = op;
        }
        Expr::Unary { expr, .. } => collect_expr_tokens(expr, source, known, out),
        Expr::If {
            cond, then, els, ..
        } => {
            push_keyword_token(expr.span(), "if", source, out);
            collect_expr_tokens(cond, source, known, out);
            collect_block_tokens(then, source, known, out);
            if let Some(e) = els {
                push_keyword_token(e.span(), "else", source, out);
                collect_expr_tokens(e, source, known, out);
            }
        }
        Expr::While { cond, body, .. } => {
            push_keyword_token(expr.span(), "while", source, out);
            collect_expr_tokens(cond, source, known, out);
            collect_block_tokens(body, source, known, out);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            push_keyword_token(expr.span(), "match", source, out);
            collect_expr_tokens(scrutinee, source, known, out);
            for arm in arms {
                collect_pattern_tokens(&arm.pat, source, known, out);
                collect_expr_tokens(&arm.body, source, known, out);
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
            collect_pattern_tokens(pat, source, known, out);
            collect_expr_tokens(value, source, known, out);
            collect_block_tokens(then, source, known, out);
            if let Some(e) = els {
                push_keyword_token(e.span(), "else", source, out);
                collect_expr_tokens(e, source, known, out);
            }
        }
        Expr::Try { expr, .. } => {
            collect_expr_tokens(expr, source, known, out);
        }
        Expr::Block(b) => collect_block_tokens(b, source, known, out),
        Expr::Array { elems, .. } => {
            for e in elems {
                collect_expr_tokens(e, source, known, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_expr_tokens(k, source, known, out);
                collect_expr_tokens(v, source, known, out);
            }
        }
        Expr::Field { obj, .. } => collect_expr_tokens(obj, source, known, out),
        Expr::Index { obj, index, .. } => {
            collect_expr_tokens(obj, source, known, out);
            collect_expr_tokens(index, source, known, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_expr_tokens(obj, source, known, out);
            if let Some(s) = start {
                collect_expr_tokens(s, source, known, out);
            }
            if let Some(e) = end {
                collect_expr_tokens(e, source, known, out);
            }
        }
        Expr::Range { start, end, .. } => {
            collect_expr_tokens(start, source, known, out);
            collect_expr_tokens(end, source, known, out);
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_expr_tokens(body, source, known, out);
            collect_expr_tokens(iter, source, known, out);
            if let Some(f) = filter {
                collect_expr_tokens(f, source, known, out);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_expr_tokens(v, source, known, out);
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
            collect_expr_tokens(body, source, known, out);
        }
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                collect_expr_tokens(a, source, known, out);
            }
        }
        Expr::Paren { expr, .. } => collect_expr_tokens(expr, source, known, out),
        Expr::Tuple { items, .. } => {
            for e in items {
                collect_expr_tokens(e, source, known, out);
            }
        }
        Expr::Break { span } => {
            push_token("break", *span, TokenType::Keyword, source, out);
        }
        Expr::Continue { span } => {
            push_token("continue", *span, TokenType::Keyword, source, out);
        }
    }
}

fn collect_type_tokens(
    ty: &Ty,
    source: &str,
    _known: &std::collections::HashSet<String>,
    out: &mut Vec<RawToken>,
) {
    match &ty.kind {
        TyKind::Named(name, generics) => {
            push_token(name, ty.span, TokenType::Type, source, out);
            for g in generics {
                collect_type_tokens(g, source, _known, out);
            }
        }
        TyKind::Array(inner) => collect_type_tokens(inner, source, _known, out),
        TyKind::Dict(key, val) => {
            collect_type_tokens(key, source, _known, out);
            collect_type_tokens(val, source, _known, out);
        }
        TyKind::Option(inner) => collect_type_tokens(inner, source, _known, out),
        TyKind::Result(ok, err) => {
            collect_type_tokens(ok, source, _known, out);
            collect_type_tokens(err, source, _known, out);
        }
        TyKind::Tuple(elems) => {
            for e in elems {
                collect_type_tokens(e, source, _known, out);
            }
        }
        TyKind::Func(params, ret) => {
            for p in params {
                collect_type_tokens(p, source, _known, out);
            }
            collect_type_tokens(ret, source, _known, out);
        }
        TyKind::Union(variants) => {
            for v in variants {
                collect_type_tokens(v, source, _known, out);
            }
        }
        _ => {} // Primitive types (int, float, bool, str, unit) — no span to emit.
    }
}

fn collect_pattern_tokens(
    pat: &zz_frontend::ast::Pattern,
    source: &str,
    _known: &std::collections::HashSet<String>,
    out: &mut Vec<RawToken>,
) {
    match pat {
        zz_frontend::ast::Pattern::Binding { name } => {
            push_ident_token(&name.name, name.span, TokenType::Variable, source, out);
        }
        zz_frontend::ast::Pattern::Variant { arg: Some(a), .. } => {
            collect_pattern_tokens(a, source, _known, out);
        }
        zz_frontend::ast::Pattern::Variant { .. } => {}
        zz_frontend::ast::Pattern::Tuple { pats, .. }
        | zz_frontend::ast::Pattern::Or { pats, .. } => {
            for p in pats {
                collect_pattern_tokens(p, source, _known, out);
            }
        }
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
        let tokens =
            collect_semantic_tokens_with(&parsed.program, src, &std::collections::HashSet::new());
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
        let tokens =
            collect_semantic_tokens_with(&parsed.program, src, &std::collections::HashSet::new());
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
        let tokens =
            collect_semantic_tokens_with(&parsed.program, src, &std::collections::HashSet::new());
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
        let tokens =
            collect_semantic_tokens_with(&parsed.program, src, &std::collections::HashSet::new());
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
        let tokens =
            collect_semantic_tokens_with(&parsed.program, src, &std::collections::HashSet::new());
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
        let tokens =
            collect_semantic_tokens_with(&parsed.program, src, &std::collections::HashSet::new());
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
        let tokens =
            collect_semantic_tokens_with(&parsed.program, src, &std::collections::HashSet::new());
        let encoded = encode_tokens(&tokens, src);
        // First token should have delta_line = 0.
        if let Some(first) = encoded.first() {
            assert_eq!(first.delta_line, 0);
        }
    }

    #[test]
    fn empty_program_has_no_tokens() {
        let parsed = parse("");
        let tokens =
            collect_semantic_tokens_with(&parsed.program, "", &std::collections::HashSet::new());
        assert!(tokens.is_empty());
    }
}

#[cfg(test)]
mod call_classification_tests {
    use super::*;
    use std::collections::HashSet;
    use zz_checker::CheckResult;
    use zz_frontend::parse;

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn token_types_for(src: &str, known: &HashSet<String>) -> Vec<(String, TokenType)> {
        let parsed = parse(src);
        collect_semantic_tokens_with(&parsed.program, src, known)
            .into_iter()
            .map(|t| {
                // RawToken.line carries the byte offset; col is line-relative.
                let start = t.line as usize;
                let end = (start + t.len as usize).min(src.len());
                (src[start..end].to_string(), t.token_type)
            })
            .collect()
    }

    fn check(src: &str) -> (zz_frontend::ast::Program, Option<CheckResult>) {
        use zz_checker::check_program;
        let parsed = parse(src);
        let state = crate::state::GlobalState::new();
        let (ib, ifunc, is, ia, ie) = state.checker_seed_for(&parsed.program);
        let cr = check_program(&parsed.program, ib, ifunc, is, ia, ie);
        (parsed.program, Some(cr))
    }

    #[test]
    fn selective_call_callee_is_function() {
        // `pow` from `import std.math(pow)`: seeded bare, must read as a call.
        let src = "import std.math(pow)\nfunc main() {\n    println(pow(2, 3))\n}\n";
        let (_program, cr) = check(src);
        let cr = cr.unwrap();
        let known: HashSet<String> = cr.funcs.keys().cloned().collect();
        assert!(known.contains("pow"), "seed carries bare pow");
        let toks = token_types_for(src, &known);
        let pow = toks
            .iter()
            .find(|(text, _)| text == "pow" && !src.starts_with("import"));
        let _ = pow;
        // The call-site `pow` (line 2), not the import item (line 0).
        let call_pow = toks
            .iter()
            .filter(|(text, _)| text == "pow")
            .nth(1)
            .expect("two pow tokens (import + call)");
        assert_eq!(
            call_pow.1,
            TokenType::Function,
            "call callee is function, got {:?} in {toks:?}",
            call_pow.1
        );
    }

    #[test]
    fn unknown_call_stays_variable() {
        let src = "func main() {\n    println(nope(1))\n}\n";
        let toks = token_types_for(src, &HashSet::new());
        let callee = toks
            .iter()
            .find(|(text, _)| text == "nope")
            .expect("nope token");
        assert_eq!(callee.1, TokenType::Variable);
    }

    #[test]
    fn qualified_call_is_function() {
        let src = "import table\nfunc main() {\n    t := table.render(table.new([\"A\"]))\n}\n";
        let toks = token_types_for(src, &known(&["table.render", "table.new"]));
        assert!(
            toks.iter()
                .any(|(text, ty)| text == "table.render" && *ty == TokenType::Function),
            "qualified call is function: {toks:?}"
        );
    }

    #[test]
    fn value_path_stays_namespace() {
        // `math.PI` as a value (not called) keeps its namespace color.
        let src = "import std.math\nfunc main() {\n    x := math.PI\n}\n";
        let toks = token_types_for(src, &known(&["math.PI"]));
        assert!(
            toks.iter()
                .any(|(text, ty)| text == "math.PI" && *ty == TokenType::Namespace),
            "value path stays namespace: {toks:?}"
        );
    }

    #[test]
    fn import_item_known_is_function() {
        let src = "import std.math(pow)\nfunc main() {\n    println(pow(2, 3))\n}\n";
        let (_program, cr) = check(src);
        let cr = cr.unwrap();
        let known: HashSet<String> = cr.funcs.keys().cloned().collect();
        let toks = token_types_for(src, &known);
        // First `pow` token = the import item.
        let first = toks
            .iter()
            .find(|(text, _)| text == "pow")
            .expect("import item token");
        assert_eq!(first.1, TokenType::Function, "import item known: {toks:?}");
    }
}
