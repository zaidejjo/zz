//! AST-based symbol resolution for go-to-definition and hover.
//!
//! `find_definition_at` walks the AST to locate the definition span for the
//! symbol under the cursor. `collect_definitions` builds a map of all
//! names → definition spans for the entire program.

use std::collections::HashMap;

use zz_checker::{CheckResult, FuncSig, Type};
use zz_frontend::ast::{Block, Expr, Ident, Param, Pattern, Program, Stmt};
use zz_frontend::span::Span;

// ── Definition map ───────────────────────────────────────────────────────

/// A single resolved definition: name, definition span, and optional type.
#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub span: Span,
    pub kind: DefKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DefKind {
    Func,
    Struct,
    Param,
    Var,
    Field,
    Import,
}

/// Collect every name defined in the program into a map of
/// (source offset → Definition). Only the *start* byte of the definition
/// span is used as the key.
pub fn collect_definitions(program: &Program, source: &str) -> HashMap<u32, Definition> {
    let mut defs = HashMap::new();
    for stmt in &program.stmts {
        collect_stmt_defs(stmt, source, &mut defs);
    }
    defs
}

fn collect_stmt_defs(stmt: &Stmt, source: &str, defs: &mut HashMap<u32, Definition>) {
    match stmt {
        Stmt::Func {
            name, params, body, ..
        } => {
            let joined = name.join(".");
            if let Some(span) = find_name_in_source(source, &joined) {
                defs.insert(
                    span.start,
                    Definition {
                        name: joined,
                        span,
                        kind: DefKind::Func,
                    },
                );
            }
            // Params are local to the function — collect for hover.
            for param in params {
                collect_param_defs(param, defs);
            }
            collect_block_defs(body, source, defs);
        }
        Stmt::Struct { name, fields, .. } => {
            let joined = name.join(".");
            if let Some(span) = find_name_in_source(source, &joined) {
                defs.insert(
                    span.start,
                    Definition {
                        name: joined.clone(),
                        span,
                        kind: DefKind::Struct,
                    },
                );
            }
            // Fields are accessible as `s.field` — store them too.
            for (fname, _fty) in fields {
                let full = format!("{joined}.{}", fname.name);
                defs.insert(
                    fname.span.start,
                    Definition {
                        name: full,
                        span: fname.span,
                        kind: DefKind::Field,
                    },
                );
            }
        }
        Stmt::Decl { name, value, .. } => {
            defs.insert(
                name.span.start,
                Definition {
                    name: name.name.clone(),
                    span: name.span,
                    kind: DefKind::Var,
                },
            );
            collect_expr_defs(value, source, defs);
        }
        Stmt::For {
            var, iter, body, ..
        } => {
            defs.insert(
                var.span.start,
                Definition {
                    name: var.name.clone(),
                    span: var.span,
                    kind: DefKind::Var,
                },
            );
            collect_expr_defs(iter, source, defs);
            collect_block_defs(body, source, defs);
        }
        Stmt::Return { value, .. } => {
            if let Some(expr) = value {
                collect_expr_defs(expr, source, defs);
            }
        }
        Stmt::Assign { target, value, .. } => {
            collect_expr_defs(target, source, defs);
            collect_expr_defs(value, source, defs);
        }
        Stmt::Defer { expr, .. } => {
            collect_expr_defs(expr, source, defs);
        }
        Stmt::Import { path, .. } => {
            let joined = path.join(".");
            if let Some(span) = find_name_in_source(source, &joined) {
                defs.insert(
                    span.start,
                    Definition {
                        name: joined,
                        span,
                        kind: DefKind::Import,
                    },
                );
            }
        }
        Stmt::Expr(e) => collect_expr_defs(e, source, defs),
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

fn collect_block_defs(block: &Block, source: &str, defs: &mut HashMap<u32, Definition>) {
    for stmt in &block.stmts {
        collect_stmt_defs(stmt, source, defs);
    }
}

fn collect_expr_defs(expr: &Expr, source: &str, defs: &mut HashMap<u32, Definition>) {
    match expr {
        Expr::If {
            cond, then, els, ..
        } => {
            collect_expr_defs(cond, source, defs);
            collect_block_defs(then, source, defs);
            if let Some(els) = els {
                collect_expr_defs(els, source, defs);
            }
        }
        Expr::While { cond, body, .. } => {
            collect_expr_defs(cond, source, defs);
            collect_block_defs(body, source, defs);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_expr_defs(scrutinee, source, defs);
            for arm in arms {
                collect_pattern_defs(&arm.pat, defs);
                collect_expr_defs(&arm.body, source, defs);
            }
        }
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            ..
        } => {
            collect_pattern_defs(pat, defs);
            collect_expr_defs(value, source, defs);
            collect_block_defs(then, source, defs);
            if let Some(els) = els {
                collect_expr_defs(els, source, defs);
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_defs(callee, source, defs);
            for arg in args {
                collect_expr_defs(arg, source, defs);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_defs(left, source, defs);
            collect_expr_defs(right, source, defs);
        }
        Expr::Unary { expr, .. } => collect_expr_defs(expr, source, defs),
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_expr_defs(body, source, defs);
            collect_expr_defs(iter, source, defs);
            if let Some(f) = filter {
                collect_expr_defs(f, source, defs);
            }
        }
        _ => {}
    }
}

fn collect_pattern_defs(pat: &Pattern, defs: &mut HashMap<u32, Definition>) {
    match pat {
        Pattern::Binding { name } => {
            defs.insert(
                name.span.start,
                Definition {
                    name: name.name.clone(),
                    span: name.span,
                    kind: DefKind::Var,
                },
            );
        }
        Pattern::Variant {
            arg: Some(inner), ..
        } => collect_pattern_defs(inner, defs),
        _ => {}
    }
}

fn collect_param_defs(param: &Param, defs: &mut HashMap<u32, Definition>) {
    defs.insert(
        param.name.span.start,
        Definition {
            name: param.name.name.clone(),
            span: param.name.span,
            kind: DefKind::Param,
        },
    );
}

// ── Node-at-offset ───────────────────────────────────────────────────────

/// Information about the AST node at a byte offset.
#[derive(Debug, Clone)]
pub struct NodeAtOffset<'a> {
    /// The expression node at the offset (if any).
    pub expr: Option<&'a Expr>,
    /// The statement node at the offset (if any).
    pub stmt: Option<&'a Stmt>,
    /// The name string at the offset (ident or last part of a path).
    pub name: Option<String>,
    /// Span of the name token.
    pub name_span: Option<Span>,
}

/// Walk the AST and find the innermost node that contains `offset`.
pub fn find_node_at<'a>(program: &'a Program, source: &'a str, offset: u32) -> NodeAtOffset<'a> {
    let mut result = NodeAtOffset {
        expr: None,
        stmt: None,
        name: None,
        name_span: None,
    };
    for stmt in &program.stmts {
        walk_stmt(stmt, source, offset, &mut result);
    }
    result
}

fn walk_stmt<'a>(stmt: &'a Stmt, source: &str, offset: u32, result: &mut NodeAtOffset<'a>) {
    let span = stmt.span();
    if offset < span.start || offset >= span.end {
        return;
    }
    result.stmt = Some(stmt);

    match stmt {
        Stmt::Decl {
            ty: _, name, value, ..
        } => {
            check_ident(name, offset, result);
            walk_expr(value, source, offset, result);
        }
        Stmt::Func {
            name, params, body, ..
        } => {
            // Check if cursor is on the function name.
            let joined = name.join(".");
            if let Some(name_span) = find_name_in_source(source, &joined) {
                if offset >= name_span.start && offset < name_span.end {
                    result.name = Some(joined);
                    result.name_span = Some(name_span);
                }
            }
            for param in params {
                check_ident(&param.name, offset, result);
            }
            walk_block(body, source, offset, result);
        }
        Stmt::For {
            var, iter, body, ..
        } => {
            check_ident(var, offset, result);
            walk_expr(iter, source, offset, result);
            walk_block(body, source, offset, result);
        }
        Stmt::Return { value, .. } => {
            if let Some(expr) = value {
                walk_expr(expr, source, offset, result);
            }
        }
        Stmt::Struct { name, .. } => {
            let joined = name.join(".");
            if let Some(name_span) = find_name_in_source(source, &joined) {
                if offset >= name_span.start && offset < name_span.end {
                    result.name = Some(joined);
                    result.name_span = Some(name_span);
                }
            }
        }
        Stmt::Assign { target, value, .. } => {
            walk_expr(target, source, offset, result);
            walk_expr(value, source, offset, result);
        }
        Stmt::Defer { expr, .. } => walk_expr(expr, source, offset, result),
        Stmt::Import { .. } => {}
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Expr(e) => walk_expr(e, source, offset, result),
    }
}

fn walk_block<'a>(block: &'a Block, source: &str, offset: u32, result: &mut NodeAtOffset<'a>) {
    for stmt in &block.stmts {
        walk_stmt(stmt, source, offset, result);
    }
}

fn walk_expr<'a>(expr: &'a Expr, _source: &str, offset: u32, result: &mut NodeAtOffset<'a>) {
    let span = expr.span();
    if offset < span.start || offset >= span.end {
        return;
    }
    result.expr = Some(expr);

    match expr {
        Expr::Ident { name, span } => {
            result.name = Some(name.clone());
            result.name_span = Some(*span);
        }
        Expr::Path { parts, span } => {
            // Cursor might be on any part of the path.
            let name = pick_path_part(parts, *span, offset);
            result.name = Some(name);
            result.name_span = Some(*span);
        }
        Expr::Call { callee, args, .. } => {
            walk_expr(callee, _source, offset, result);
            for arg in args {
                walk_expr(arg, _source, offset, result);
            }
        }
        Expr::Binary { left, right, .. } => {
            walk_expr(left, _source, offset, result);
            walk_expr(right, _source, offset, result);
        }
        Expr::Unary { expr, .. } => walk_expr(expr, _source, offset, result),
        Expr::If {
            cond, then, els, ..
        } => {
            walk_expr(cond, _source, offset, result);
            walk_block(then, _source, offset, result);
            if let Some(e) = els {
                walk_expr(e, _source, offset, result);
            }
        }
        Expr::While { cond, body, .. } => {
            walk_expr(cond, _source, offset, result);
            walk_block(body, _source, offset, result);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, _source, offset, result);
            for arm in arms {
                walk_expr(&arm.body, _source, offset, result);
            }
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            walk_expr(value, _source, offset, result);
            walk_block(then, _source, offset, result);
            if let Some(e) = els {
                walk_expr(e, _source, offset, result);
            }
        }
        Expr::Try { expr, .. } => walk_expr(expr, _source, offset, result),
        Expr::Field { obj, .. } => walk_expr(obj, _source, offset, result),
        Expr::Index { obj, index, .. } => {
            walk_expr(obj, _source, offset, result);
            walk_expr(index, _source, offset, result);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            walk_expr(obj, _source, offset, result);
            if let Some(s) = start {
                walk_expr(s, _source, offset, result);
            }
            if let Some(e) = end {
                walk_expr(e, _source, offset, result);
            }
        }
        Expr::Range { start, end, .. } => {
            walk_expr(start, _source, offset, result);
            walk_expr(end, _source, offset, result);
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            walk_expr(body, _source, offset, result);
            walk_expr(iter, _source, offset, result);
            if let Some(f) = filter {
                walk_expr(f, _source, offset, result);
            }
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                walk_expr(e, _source, offset, result);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                walk_expr(k, _source, offset, result);
                walk_expr(v, _source, offset, result);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let zz_frontend::ast::FmtPart::Expr(e, _) = part {
                    walk_expr(e, _source, offset, result);
                }
            }
        }
        Expr::Block(b) => walk_block(b, _source, offset, result),
        _ => {}
    }
}

fn check_ident(ident: &Ident, offset: u32, result: &mut NodeAtOffset<'_>) {
    if offset >= ident.span.start && offset < ident.span.end {
        result.name = Some(ident.name.clone());
        result.name_span = Some(ident.span);
    }
}

/// For a dotted path like `std.io.println`, if the cursor is on `println`
/// (the last part), return `"println"`. Otherwise return the matched part.
fn pick_path_part(parts: &[String], span: Span, offset: u32) -> String {
    if parts.len() == 1 {
        return parts[0].clone();
    }
    // Heuristic: walk parts and find which one contains the offset.
    let mut pos = span.start;
    for part in parts {
        let part_end = pos + part.len() as u32;
        if offset >= pos && offset < part_end {
            return part.clone();
        }
        pos = part_end + 1; // +1 for the '.'
    }
    // Fallback: return the last part.
    parts.last().cloned().unwrap_or_default()
}

// ── Source-based name search ─────────────────────────────────────────────

/// Find a name token in source text within a span range.
///
/// For simple identifiers this is trivial. For dotted paths like
/// `shapes.Point`, this finds the first occurrence of the full name
/// within the statement's span.
fn find_name_in_source(source: &str, name: &str) -> Option<Span> {
    let bytes = source.as_bytes();
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len() as u32;

    // Search for the name as a standalone token (preceded by non-alnum,
    // followed by non-alnum or EOF).
    for i in 0..bytes.len() {
        if bytes[i..].starts_with(name_bytes) {
            let start = i as u32;
            let end = start + name_len;
            // Check word boundaries.
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

// ── Type resolution ──────────────────────────────────────────────────────

/// Resolve the type of a name at a given byte offset.
///
/// Strategy:
/// 1. Find the name under the cursor via `find_node_at`.
/// 2. Look up the name in the CheckResult (funcs, structs, bindings).
/// 3. For struct field access (`s.field`), look up the struct and find the field type.
pub fn resolve_type_at(
    program: &Program,
    source: &str,
    check_result: &CheckResult,
    offset: u32,
) -> Option<Type> {
    let node = find_node_at(program, source, offset);
    let name = node.name?;

    // Check function signatures.
    if let Some(sig) = check_result.funcs.get(&name) {
        return Some(func_sig_to_type(&name, sig));
    }

    // Check struct definitions.
    if let Some(_sig) = check_result.structs.get(&name) {
        return Some(Type::Struct(name));
    }

    // Check top-level bindings.
    if let Some(ty) = check_result.bindings.get(&name) {
        return Some(ty.clone());
    }

    // Check struct field access: `expr.field`
    if let Some(Expr::Field {
        obj, name: field, ..
    }) = node.expr
    {
        // Resolve the object type, then look up the field.
        if let Some(obj_type) = resolve_type_of_expr(program, check_result, obj) {
            if let Type::Struct(struct_name) = obj_type {
                if let Some(sig) = check_result.structs.get(&struct_name) {
                    for (fname, fty) in &sig.fields {
                        if fname == field {
                            return Some(fty.clone());
                        }
                    }
                }
            }
        }
    }

    None
}

/// Resolve the type of an arbitrary expression node.
pub fn resolve_type_of_expr(
    program: &Program,
    check_result: &CheckResult,
    expr: &Expr,
) -> Option<Type> {
    match expr {
        Expr::Ident { name, .. } => {
            // Same lookup as resolve_type_at but without cursor context.
            if let Some(sig) = check_result.funcs.get(name) {
                return Some(func_sig_to_type(name, sig));
            }
            if let Some(_sig) = check_result.structs.get(name) {
                return Some(Type::Struct(name.clone()));
            }
            check_result.bindings.get(name).cloned()
        }
        Expr::Path { parts, .. } => {
            let joined = parts.join(".");
            if let Some(sig) = check_result.funcs.get(&joined) {
                return Some(func_sig_to_type(&joined, sig));
            }
            if let Some(_sig) = check_result.structs.get(&joined) {
                return Some(Type::Struct(joined));
            }
            check_result.bindings.get(&joined).cloned()
        }
        Expr::Field { obj, name, .. } => {
            let obj_type = resolve_type_of_expr(program, check_result, obj)?;
            if let Type::Struct(struct_name) = obj_type {
                if let Some(sig) = check_result.structs.get(&struct_name) {
                    for (fname, fty) in &sig.fields {
                        if fname == name {
                            return Some(fty.clone());
                        }
                    }
                }
            }
            None
        }
        Expr::Call { callee, .. } => {
            // Call returns the function's return type.
            let callee_type = resolve_type_of_expr(program, check_result, callee)?;
            if let Type::Func(_, ret) = callee_type {
                Some(*ret)
            } else {
                None
            }
        }
        Expr::Int { .. } => Some(Type::Int),
        Expr::Float { .. } => Some(Type::Float),
        Expr::Str { .. } => Some(Type::Str),
        Expr::Bool { .. } => Some(Type::Bool),
        Expr::Array { elems, .. } => {
            if let Some(first) = elems.first() {
                let inner = resolve_type_of_expr(program, check_result, first)?;
                Some(Type::Array(Box::new(inner)))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn func_sig_to_type(_name: &str, sig: &FuncSig) -> Type {
    let params: Vec<Type> = sig.params.iter().map(|(_, t)| t.clone()).collect();
    Type::Func(params, Box::new(sig.ret.clone()))
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn check(source: &str) -> CheckResult {
        let parsed = parse(source);
        check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[test]
    fn find_func_definition() {
        let source =
            "func add(x: int, y: int) -> int {\n  return x + y\n}\nlet result = add(1, 2)\n";
        let parsed = parse(source);
        let defs = collect_definitions(&parsed.program, source);
        // Should find `add` as a function definition.
        let add_def = defs
            .values()
            .find(|d| d.name == "add" && d.kind == DefKind::Func);
        assert!(add_def.is_some(), "expected func `add` in defs");
    }

    #[test]
    fn find_var_definition() {
        let source = "let x = 10\nlet y = x\n";
        let parsed = parse(source);
        let defs = collect_definitions(&parsed.program, source);
        let x_def = defs
            .values()
            .find(|d| d.name == "x" && d.kind == DefKind::Var);
        assert!(x_def.is_some(), "expected var `x` in defs");
    }

    #[test]
    fn find_struct_definition() {
        let source = "struct Point { x: int, y: int }\n";
        let parsed = parse(source);
        let defs = collect_definitions(&parsed.program, source);
        let point_def = defs
            .values()
            .find(|d| d.name == "Point" && d.kind == DefKind::Struct);
        assert!(point_def.is_some(), "expected struct `Point` in defs");
    }

    #[test]
    fn find_param_definition() {
        let source = "func f(n: int) -> int {\n  return n\n}\n";
        let parsed = parse(source);
        let defs = collect_definitions(&parsed.program, source);
        let n_def = defs
            .values()
            .find(|d| d.name == "n" && d.kind == DefKind::Param);
        assert!(n_def.is_some(), "expected param `n` in defs");
    }

    #[test]
    fn resolve_func_type() {
        let source =
            "func add(x: int, y: int) -> int {\n  return x + y\n}\nlet result = add(1, 2)\n";
        let cr = check(source);
        let parsed = parse(source);
        // Offset 5 is inside the `add` function name.
        let ty = resolve_type_at(&parsed.program, source, &cr, 5);
        assert!(ty.is_some(), "expected a type at offset 5");
    }

    #[test]
    fn find_node_at_ident() {
        let source = "let x = 10\n";
        let parsed = parse(source);
        // Offset 4 is inside the `x` identifier.
        let node = find_node_at(&parsed.program, source, 4);
        assert_eq!(node.name.as_deref(), Some("x"));
    }

    #[test]
    fn find_node_at_path() {
        let source = "import std.io\nstd.io.println(\"hi\")\n";
        let parsed = parse(source);
        // Offset 20 should be somewhere in the import or the call.
        let node = find_node_at(&parsed.program, source, 20);
        // Should find something — either the import path or the println call.
        assert!(node.stmt.is_some() || node.expr.is_some());
    }
}
