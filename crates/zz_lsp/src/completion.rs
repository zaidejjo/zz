//! Context-aware autocompletion for ZZ.
//!
//! Three completion modes:
//! 1. **Dot access** (`obj.`) — resolves the object's type and returns its fields.
//! 2. **Scope completion** (bare identifier) — returns visible locals, globals,
//!    functions, structs, and keywords filtered by prefix.

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse};
use zz_checker::{CheckResult, FuncSig, StructSig, Type};
use zz_frontend::ast::{Expr, Program, Stmt};

use crate::lookup::resolve_type_of_expr;

// ── ZZ keywords ──────────────────────────────────────────────────────────

const KEYWORDS: &[&str] = &[
    "break", "continue", "defer", "else", "false", "for", "func", "if", "import", "match", "none",
    "return", "struct", "true", "while",
];

// ── Stdlib module names ──────────────────────────────────────────────────

const STDLIB_MODULES: &[&str] = &[
    "io", "str", "vec", "json", "http", "fs", "env", "math", "time",
];

// ── Public API ───────────────────────────────────────────────────────────

/// Build completions for the given cursor position.
pub fn completions_for_position(
    program: &Program,
    source: &str,
    offset: u32,
    check_result: Option<&CheckResult>,
) -> Option<CompletionResponse> {
    let ctx = detect_context(source, offset)?;
    let items = match ctx {
        CompletionContext::DotAccess {
            obj_name,
            partial_prefix,
        } => dot_access_completions(program, check_result, &obj_name, &partial_prefix),
        CompletionContext::Scope { partial_prefix } => {
            scope_completions(program, check_result, &partial_prefix)
        }
    };
    Some(CompletionResponse::Array(items))
}

/// Resolve extra detail for a completion item.
pub fn resolve_completion_detail(item: &mut CompletionItem, check_result: Option<&CheckResult>) {
    let cr = match check_result {
        Some(cr) => cr,
        None => return,
    };
    if let Some(sig) = cr.funcs.get(&item.label) {
        item.detail = Some(format_func_sig(&item.label, sig));
        if let Some(docs) = format_func_docs(sig) {
            item.documentation = Some(tower_lsp::lsp_types::Documentation::String(docs));
        }
    } else if let Some(sig) = cr.structs.get(&item.label) {
        item.detail = Some(format_struct_sig(&item.label, sig));
    }
}

// ── Context detection ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum CompletionContext {
    DotAccess {
        obj_name: String,
        partial_prefix: String,
    },
    Scope {
        partial_prefix: String,
    },
}

fn detect_context(source: &str, offset: u32) -> Option<CompletionContext> {
    let before = &source[..offset as usize];

    let partial = extract_partial_identifier(before);
    let partial_prefix = partial.clone().unwrap_or_default();

    let dot_check_offset = before.len() - partial_prefix.len();
    if dot_check_offset > 0 {
        let ch_before_dot = before.as_bytes().get(dot_check_offset - 1).copied();
        if ch_before_dot == Some(b'.') {
            let before_dot = &before[..dot_check_offset - 1];
            if let Some(obj_name) = extract_partial_identifier(before_dot) {
                return Some(CompletionContext::DotAccess {
                    obj_name,
                    partial_prefix,
                });
            }
        }
    }

    if inside_string_literal(before) {
        return None;
    }
    if inside_line_comment(before) {
        return None;
    }

    Some(CompletionContext::Scope { partial_prefix })
}

fn extract_partial_identifier(text: &str) -> Option<String> {
    let mut end = text.len();
    let bytes = text.as_bytes();
    while end > 0 {
        let ch = bytes[end - 1];
        if ch.is_ascii_alphanumeric() || ch == b'_' {
            end -= 1;
        } else {
            break;
        }
    }
    if end == text.len() {
        return None;
    }
    let word = &text[end..];
    if word.is_empty() {
        None
    } else {
        Some(word.to_string())
    }
}

fn inside_string_literal(text: &str) -> bool {
    let mut in_string = false;
    for ch in text.chars() {
        if ch == '"' {
            in_string = !in_string;
        }
    }
    in_string
}

fn inside_line_comment(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'/' && bytes[i + 1] == b'/' {
            return true;
        }
        i += 1;
    }
    false
}

// ── Dot access completions ───────────────────────────────────────────────

fn dot_access_completions(
    program: &Program,
    check_result: Option<&CheckResult>,
    obj_name: &str,
    partial_prefix: &str,
) -> Vec<CompletionItem> {
    let cr = match check_result {
        Some(cr) => cr,
        None => return Vec::new(),
    };

    // 1. Try struct field access (existing logic).
    let obj_type = resolve_obj_type(program, cr, obj_name);
    let struct_name = match obj_type {
        Some(Type::Struct(name)) => name,
        _ => {
            // 2. Not a struct — check if obj_name is an imported stdlib module
            //    alias (e.g. `math` after `import std.math as math`).
            return stdlib_module_completions(program, cr, obj_name, partial_prefix);
        }
    };

    let sig = match cr.structs.get(&struct_name) {
        Some(s) => s,
        None => return Vec::new(),
    };

    sig.fields
        .iter()
        .filter(|(fname, _)| fname.starts_with(partial_prefix))
        .map(|(fname, fty)| CompletionItem {
            label: fname.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("{fname}: {fty}")),
            insert_text: Some(fname.clone()),
            ..Default::default()
        })
        .collect()
}

/// Provide completions for stdlib module access (`math.`, `str.`, etc.).
///
/// When the user types `math.`, look up the import alias in the AST and
/// provide all functions from the corresponding stdlib module.
fn stdlib_module_completions(
    program: &Program,
    cr: &CheckResult,
    obj_name: &str,
    partial_prefix: &str,
) -> Vec<CompletionItem> {
    // Find the stdlib module for this alias.
    let module = find_stdlib_module_for_alias(program, obj_name);
    let module = match module {
        Some(m) => m,
        None => return Vec::new(),
    };

    // The checker registers functions as `math.abs`, `math.floor`, etc.
    // using the module's own name.  When the user uses an alias like
    // `import std.math as m`, we need to match against `math.*` keys and
    // present them as `m.*` completions.
    let module_prefix = format!("{module}.");
    let _user_prefix = format!("{obj_name}.");
    let mut items: Vec<CompletionItem> = cr
        .funcs
        .keys()
        .filter(|k| k.starts_with(&module_prefix))
        .map(|k| {
            let func_name = &k[module_prefix.len()..];
            CompletionItem {
                label: func_name.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(format!("{obj_name}.{func_name}")),
                documentation: Some(tower_lsp::lsp_types::Documentation::String(format!(
                    "std.{module}.{func_name}"
                ))),
                insert_text: Some(func_name.to_string()),
                ..Default::default()
            }
        })
        .filter(|item| item.label.starts_with(partial_prefix))
        .collect();

    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

/// Walk the AST import statements to find which stdlib module an alias
/// maps to.  For example, `import std.math as math` maps alias `math`
/// to module `math`.
fn find_stdlib_module_for_alias(program: &Program, alias: &str) -> Option<String> {
    for stmt in &program.stmts {
        if let Stmt::Import {
            path,
            alias: import_alias,
            ..
        } = stmt
        {
            // The effective namespace is the import alias, or the last
            // path segment if no explicit alias.
            let ns = import_alias
                .as_ref()
                .cloned()
                .or_else(|| path.last().cloned())
                .unwrap_or_default();
            if ns == alias {
                // Extract the module name from the path (e.g. `std.math` → `math`).
                if path.len() >= 2 && path[0] == "std" {
                    return Some(path[1].clone());
                }
                // Direct import like `import math` — assume it's a stdlib module.
                if path.len() == 1 && STDLIB_MODULES.contains(&path[0].as_str()) {
                    return Some(path[0].clone());
                }
            }
        }
    }
    None
}

fn resolve_obj_type(program: &Program, cr: &CheckResult, name: &str) -> Option<Type> {
    // 1. Check bindings.
    if let Some(ty) = cr.bindings.get(name) {
        return Some(ty.clone());
    }
    // 2. Check function name.
    if let Some(sig) = cr.funcs.get(name) {
        return Some(func_sig_to_type(sig));
    }
    // 3. Check struct name.
    if cr.structs.contains_key(name) {
        return Some(Type::Struct(name.to_string()));
    }
    // 4. Walk AST for Decl.
    for stmt in &program.stmts {
        if let Stmt::Decl {
            name: ident, value, ..
        } = stmt
        {
            if ident.name == name {
                return resolve_type_of_expr(program, cr, value);
            }
        }
    }
    // 5. Check function params.
    for stmt in &program.stmts {
        if let Stmt::Func { params, body, .. } = stmt {
            for param in params {
                if param.name.name == name {
                    if let Some(ref ty) = param.ty {
                        return Some(type_from_annotation(ty));
                    }
                    return Some(Type::Unit);
                }
            }
            if let Some(ty) = find_local_in_block(body, name, cr) {
                return Some(ty);
            }
        }
    }
    None
}

fn find_local_in_block(
    block: &zz_frontend::ast::Block,
    name: &str,
    cr: &CheckResult,
) -> Option<Type> {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl {
                name: ident, value, ..
            } => {
                if ident.name == name {
                    return resolve_type_of_expr(
                        &Program {
                            stmts: vec![],
                            span: zz_frontend::span::Span::new(0, 0),
                        },
                        cr,
                        value,
                    );
                }
            }
            Stmt::For { vars, body, .. } => {
                for v in vars {
                    if v.name == name {
                        return Some(Type::Unit);
                    }
                }
                if let Some(ty) = find_local_in_block(body, name, cr) {
                    return Some(ty);
                }
            }
            Stmt::Expr(Expr::If { then, els, .. }) => {
                if let Some(ty) = find_local_in_block(then, name, cr) {
                    return Some(ty);
                }
                if let Some(Expr::Block(b)) = els.as_deref() {
                    if let Some(ty) = find_local_in_block(b, name, cr) {
                        return Some(ty);
                    }
                }
            }
            Stmt::Expr(Expr::While { body, .. }) => {
                if let Some(ty) = find_local_in_block(body, name, cr) {
                    return Some(ty);
                }
            }
            Stmt::Expr(Expr::Match { arms, .. }) => {
                for arm in arms {
                    if let Expr::Block(b) = &arm.body {
                        if let Some(ty) = find_local_in_block(b, name, cr) {
                            return Some(ty);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ── Scope completions ────────────────────────────────────────────────────

fn scope_completions(
    program: &Program,
    check_result: Option<&CheckResult>,
    partial_prefix: &str,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Keywords.
    for kw in KEYWORDS {
        if kw.starts_with(partial_prefix) {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("keyword".to_string()),
                ..Default::default()
            });
        }
    }

    if let Some(cr) = check_result {
        // Global functions.
        for name in cr.funcs.keys() {
            if name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(format!("func {name}")),
                    ..Default::default()
                });
            }
        }
        // Global structs.
        for name in cr.structs.keys() {
            if name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("struct {name}")),
                    ..Default::default()
                });
            }
        }
        // Global bindings.
        for (name, ty) in &cr.bindings {
            if name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("{name}: {ty}")),
                    ..Default::default()
                });
            }
        }
    }

    // Stdlib module names (as imported aliases).
    for stmt in &program.stmts {
        if let Stmt::Import {
            path,
            alias: import_alias,
            ..
        } = stmt
        {
            let ns = import_alias
                .as_ref()
                .cloned()
                .or_else(|| path.last().cloned())
                .unwrap_or_default();
            if ns.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: ns.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some(format!("import {}", path.join("."))),
                    ..Default::default()
                });
            }
        }
    }

    // Local names from AST.
    collect_local_names(program, &mut items, partial_prefix);

    // Deduplicate by label.
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    items
}

fn collect_local_names(program: &Program, items: &mut Vec<CompletionItem>, prefix: &str) {
    for stmt in &program.stmts {
        collect_locals_in_stmt(stmt, items, prefix);
    }
}

fn collect_locals_in_stmt(stmt: &Stmt, items: &mut Vec<CompletionItem>, prefix: &str) {
    match stmt {
        Stmt::Func { params, body, .. } => {
            for param in params {
                if param.name.name.starts_with(prefix) {
                    let detail = match &param.ty {
                        Some(ty) => format!("param: {ty:?}"),
                        None => "param".to_string(),
                    };
                    items.push(CompletionItem {
                        label: param.name.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some(detail),
                        insert_text: Some(param.name.name.clone()),
                        ..Default::default()
                    });
                }
            }
            collect_locals_in_block(body, items, prefix);
        }
        Stmt::Decl { name, .. } => {
            if name.name.starts_with(prefix) {
                items.push(CompletionItem {
                    label: name.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("let {}", name.name)),
                    insert_text: Some(name.name.clone()),
                    ..Default::default()
                });
            }
        }
        Stmt::For { vars, body, .. } => {
            for v in vars {
                if v.name.starts_with(prefix) {
                    items.push(CompletionItem {
                        label: v.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some(format!("for var {}", v.name)),
                        insert_text: Some(v.name.clone()),
                        ..Default::default()
                    });
                }
            }
            collect_locals_in_block(body, items, prefix);
        }
        Stmt::Expr(expr) => collect_locals_in_expr(expr, items, prefix),
        _ => {}
    }
}

fn collect_locals_in_block(
    block: &zz_frontend::ast::Block,
    items: &mut Vec<CompletionItem>,
    prefix: &str,
) {
    for stmt in &block.stmts {
        collect_locals_in_stmt(stmt, items, prefix);
    }
}

fn collect_locals_in_expr(expr: &Expr, items: &mut Vec<CompletionItem>, prefix: &str) {
    match expr {
        Expr::Block(b) => collect_locals_in_block(b, items, prefix),
        Expr::If { then, els, .. } => {
            collect_locals_in_block(then, items, prefix);
            if let Some(Expr::Block(b)) = els.as_deref() {
                collect_locals_in_block(b, items, prefix);
            }
        }
        Expr::While { body, .. } => collect_locals_in_block(body, items, prefix),
        Expr::Match { arms, .. } => {
            for arm in arms {
                if let Expr::Block(b) = &arm.body {
                    collect_locals_in_block(b, items, prefix);
                }
            }
        }
        Expr::IfLet { then, els, .. } => {
            collect_locals_in_block(then, items, prefix);
            if let Some(Expr::Block(b)) = els.as_deref() {
                collect_locals_in_block(b, items, prefix);
            }
        }
        _ => {}
    }
}

// ── Formatting helpers ───────────────────────────────────────────────────

fn format_func_sig(name: &str, sig: &FuncSig) -> String {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect();
    format!("func {name}({}) -> {}", params.join(", "), sig.ret)
}

fn format_func_docs(sig: &FuncSig) -> Option<String> {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect();
    Some(format!(
        "```zz\nfunc({}) -> {}\n```",
        params.join(", "),
        sig.ret
    ))
}

fn format_struct_sig(name: &str, sig: &StructSig) -> String {
    let fields: Vec<String> = sig
        .fields
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect();
    format!("struct {name} {{ {} }}", fields.join(", "))
}

fn func_sig_to_type(sig: &FuncSig) -> Type {
    let params: Vec<Type> = sig.params.iter().map(|(_, t)| t.clone()).collect();
    Type::Func(params, Box::new(sig.ret.clone()))
}

fn type_from_annotation(ty: &zz_frontend::ast::Ty) -> Type {
    use zz_frontend::ast::TyKind;
    match &ty.kind {
        TyKind::Named(name, _generics) => {
            let name = name.clone();
            match name.as_str() {
                "int" => Type::Int,
                "float" => Type::Float,
                "bool" => Type::Bool,
                "str" => Type::Str,
                "void" | "unit" => Type::Unit,
                _ => Type::Struct(name),
            }
        }
        TyKind::Int => Type::Int,
        TyKind::Float => Type::Float,
        TyKind::Bool => Type::Bool,
        TyKind::Str => Type::Str,
        TyKind::Unit => Type::Unit,
        TyKind::Array(inner) => Type::Array(Box::new(type_from_annotation(inner))),
        _ => Type::Unit,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn check(source: &str) -> (Program, Option<CheckResult>) {
        let parsed = parse(source);
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        (parsed.program, Some(cr))
    }

    /// Check with stdlib functions registered (for stdlib module tests).
    fn check_with_stdlib(source: &str) -> (Program, Option<CheckResult>) {
        let parsed = parse(source);
        let mut funcs = zz_stdlib::stdlib_funcs();
        // Register each stdlib module under its own name (e.g. math.abs).
        for module in zz_stdlib::STDLIB_MODULES {
            let _ = zz_stdlib::register_module_namespace(
                module,
                module,
                &mut funcs,
                &mut std::collections::HashMap::new(),
            );
        }
        let cr = check_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
        (parsed.program, Some(cr))
    }

    // ── Context detection ─────────────────────────────────────────────

    #[test]
    fn detect_scope_empty_prefix() {
        let src = "x := 1\n";
        let ctx = detect_context(src, src.len() as u32);
        assert_eq!(
            ctx,
            Some(CompletionContext::Scope {
                partial_prefix: "".into()
            })
        );
    }

    #[test]
    fn detect_scope_partial() {
        let src = "xy := 1\nxz := 2\n";
        let ctx = detect_context(src, 2);
        assert_eq!(
            ctx,
            Some(CompletionContext::Scope {
                partial_prefix: "xy".into()
            })
        );
    }

    #[test]
    fn detect_dot_access() {
        let src = "struct Point { x: int }\np := Point{ x: 1 }\np.";
        let ctx = detect_context(src, src.len() as u32);
        assert_eq!(
            ctx,
            Some(CompletionContext::DotAccess {
                obj_name: "p".into(),
                partial_prefix: "".into(),
            })
        );
    }

    #[test]
    fn detect_dot_access_with_partial() {
        let src = "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x";
        let ctx = detect_context(src, src.len() as u32);
        assert_eq!(
            ctx,
            Some(CompletionContext::DotAccess {
                obj_name: "p".into(),
                partial_prefix: "x".into(),
            })
        );
    }

    #[test]
    fn detect_inside_string_returns_none() {
        let src = r#"x := "hello world""#;
        let ctx = detect_context(src, 12);
        assert_eq!(ctx, None);
    }

    // ── Scope completions ─────────────────────────────────────────────

    #[test]
    fn scope_keywords() {
        let src = "x := 1\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "ret");
        assert!(items.iter().any(|i| i.label == "return"));
        assert!(!items.iter().any(|i| i.label == "func"));
    }

    #[test]
    fn scope_globals() {
        let src = "func add(a: int, b: int) -> int { return a + b }\nx := 1\nxy := 2\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "x");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"));
        assert!(labels.contains(&"xy"));
        assert!(!labels.contains(&"add"));
    }

    #[test]
    fn scope_local_params() {
        let src = "func f(param_a: int) -> int {\n  let local_b = param_a\n  return local_b\n}\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "par");
        assert!(items.iter().any(|i| i.label == "param_a"));
    }

    #[test]
    fn scope_local_let() {
        let src = "alpha := 1\nbeta := 2\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "al");
        assert!(items.iter().any(|i| i.label == "alpha"));
        assert!(!items.iter().any(|i| i.label == "beta"));
    }

    // ── Dot access completions ────────────────────────────────────────

    #[test]
    fn dot_access_struct_fields() {
        let src = "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.";
        let (program, cr) = check(src);
        let items = dot_access_completions(&program, cr.as_ref(), "p", "");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"));
        assert!(labels.contains(&"y"));
        assert!(items
            .iter()
            .all(|i| i.kind == Some(CompletionItemKind::FIELD)));
    }

    #[test]
    fn dot_access_partial_filter() {
        let src = "struct Point { x: int, y: int, z: int }\np := Point{ x: 1, y: 2, z: 3 }\np.x";
        let (program, cr) = check(src);
        let items = dot_access_completions(&program, cr.as_ref(), "p", "x");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "x");
    }

    #[test]
    fn dot_access_non_struct_returns_empty() {
        let src = "x := 42\nx.";
        let (program, cr) = check(src);
        let items = dot_access_completions(&program, cr.as_ref(), "x", "");
        assert!(items.is_empty());
    }

    // ── Full completion pipeline ──────────────────────────────────────

    #[test]
    fn completions_for_position_scope() {
        let src = "myvar := 1\n";
        let (program, cr) = check(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref());
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        assert!(items.iter().any(|i| i.label == "myvar"));
    }

    #[test]
    fn completions_for_position_dot() {
        let src = "struct Foo { bar: int }\nf := Foo{ bar: 1 }\nf.";
        let (program, cr) = check(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref());
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        assert!(items.iter().any(|i| i.label == "bar"));
    }

    // ── Prefix matching ───────────────────────────────────────────────

    #[test]
    fn prefix_filtering() {
        let src =
            "struct Abc { a1: int, b1: int, a2: int }\nobj := Abc{ a1: 1, b1: 2, a2: 3 }\nobj.a";
        let (program, cr) = check(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref());
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"a1"));
        assert!(labels.contains(&"a2"));
        assert!(!labels.contains(&"b1"));
    }

    // ── Stdlib module completions ─────────────────────────────────────

    #[test]
    fn stdlib_module_dot_access() {
        let src = "import std.math\nmath.";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref());
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"abs"), "expected 'abs' in {labels:?}");
        assert!(labels.contains(&"floor"), "expected 'floor' in {labels:?}");
        assert!(labels.contains(&"ceil"), "expected 'ceil' in {labels:?}");
        assert!(labels.contains(&"sqrt"), "expected 'sqrt' in {labels:?}");
        assert!(labels.contains(&"pow"), "expected 'pow' in {labels:?}");
    }

    #[test]
    fn stdlib_module_dot_access_partial() {
        let src = "import std.math\nmath.f";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref());
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"floor"), "expected 'floor' in {labels:?}");
        assert!(!labels.contains(&"abs"), "should not contain 'abs'");
    }

    #[test]
    fn stdlib_module_aliased_dot_access() {
        let src = "import std.math as m\nm.";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref());
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"abs"), "expected 'abs' in {labels:?}");
        assert!(labels.contains(&"pow"), "expected 'pow' in {labels:?}");
    }

    #[test]
    fn stdlib_module_name_in_scope() {
        let src = "import std.math as math\nmath";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref());
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"math"), "expected 'math' in {labels:?}");
    }
}
