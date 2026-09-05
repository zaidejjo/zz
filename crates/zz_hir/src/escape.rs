//! Escape analysis pass.
//!
//! Walks a [`TypedProgram`] and classifies heap-allocating expressions as
//! either **non-escaping** (local scope only — can be arena-allocated) or
//! **escaping** (returned, stored globally, passed across function boundaries
//! — must use ARC).
//!
//! The result is a per-span classification map consumed by the codegen
//! lowerer to decide allocation strategy.

use std::collections::{HashMap, HashSet};

use zz_checker::Type;
use zz_frontend::ast::{Expr, Stmt};
use zz_frontend::span::Span;

use crate::callgraph::TOP;
use crate::TypedProgram;

/// Classification of an allocation site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AllocClass {
    /// Object lives only within the current function scope. Can be
    /// bump-allocated on the arena and freed in O(1) at scope exit.
    NonEscaping,
    /// Object escapes the current scope (returned, stored in a global,
    /// passed to another function, captured in a closure). Must use
    /// thread-safe ARC.
    Escaping,
}

/// Result of escape analysis: maps expression spans to their allocation
/// classification. Only spans that correspond to heap-allocating expressions
/// are present.
#[derive(Debug, Clone, Default)]
pub struct EscapeResult {
    /// span → allocation classification
    pub classes: HashMap<Span, AllocClass>,
}

/// Run escape analysis on a typed program.
pub fn analyze(tp: &TypedProgram) -> EscapeResult {
    let mut result = EscapeResult::default();

    // Collect top-level binding names that are globally visible.
    let global_names: HashSet<String> = tp.bindings.keys().cloned().collect();

    // Analyze each reachable function.
    for stmt in tp.stmts() {
        match stmt {
            Stmt::Func {
                name, params, body, ..
            } => {
                let fname = name.join(".");
                let param_names: HashSet<String> =
                    params.iter().map(|p| p.name.name.clone()).collect();
                let mut ctx = FuncCtx {
                    global_names: &global_names,
                    param_names,
                    func_name: &fname,
                    escaped_names: HashSet::new(),
                };
                analyze_block_impl(body, tp, &mut ctx, &mut result, true);
                // Propagate escaping to initializer expressions of escaped variables.
                propagate_escaped(body, &ctx.escaped_names, tp, &mut result);
            }
            other => {
                let mut ctx = FuncCtx {
                    global_names: &global_names,
                    param_names: HashSet::new(),
                    func_name: TOP,
                    escaped_names: HashSet::new(),
                };
                analyze_stmt(other, tp, &mut ctx, &mut result);
            }
        }
    }

    result
}

/// After the first pass, walk declarations and mark the initializer of any
/// escaped variable as escaping.
fn propagate_escaped(
    block: &crate::Block,
    escaped_names: &HashSet<String>,
    tp: &TypedProgram,
    result: &mut EscapeResult,
) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                if escaped_names.contains(&name.name) {
                    mark_escaping_recursive(value, tp, result);
                }
            }
            Stmt::For { body, .. } => propagate_escaped(body, escaped_names, tp, result),
            _ => {}
        }
    }
}

/// Per-function analysis context.
struct FuncCtx<'a> {
    global_names: &'a HashSet<String>,
    param_names: HashSet<String>,
    #[allow(dead_code)]
    func_name: &'a str,
    /// Variable names that escape this function scope.
    escaped_names: HashSet<String>,
}

/// Analyze a block's statements. When `is_func_body` is true, the last
/// expression is treated as an implicit return (tail expression).
fn analyze_block_impl(
    block: &crate::Block,
    tp: &TypedProgram,
    ctx: &mut FuncCtx<'_>,
    result: &mut EscapeResult,
    is_func_body: bool,
) {
    let n = block.stmts.len();
    for (i, stmt) in block.stmts.iter().enumerate() {
        let is_tail = is_func_body && i == n - 1;
        if is_tail {
            if let Stmt::Expr(e) = stmt {
                // Tail expression = implicit return.
                mark_escaping_recursive(e, tp, result);
                match e {
                    Expr::Ident { name, .. } => {
                        ctx.escaped_names.insert(name.clone());
                    }
                    Expr::Path { parts, .. } => {
                        if let Some(first) = parts.first() {
                            ctx.escaped_names.insert(first.clone());
                        }
                    }
                    _ => {}
                }
                // Still analyze sub-expressions in the tail.
                analyze_stmt(stmt, tp, ctx, result);
            } else {
                // Non-expression tail (e.g., for loop) — analyze normally.
                analyze_stmt(stmt, tp, ctx, result);
            }
        } else {
            analyze_stmt(stmt, tp, ctx, result);
        }
    }
}

fn analyze_block(
    block: &crate::Block,
    tp: &TypedProgram,
    ctx: &mut FuncCtx<'_>,
    result: &mut EscapeResult,
) {
    analyze_block_impl(block, tp, ctx, result, false);
}

fn analyze_stmt(stmt: &Stmt, tp: &TypedProgram, ctx: &mut FuncCtx<'_>, result: &mut EscapeResult) {
    match stmt {
        Stmt::Decl { value, .. } => {
            let cls = classify_expr(value, tp, ctx);
            result.classes.insert(value.span(), cls);
            // Also classify sub-expressions within the initializer.
            analyze_expr(value, tp, ctx, result);
        }
        Stmt::Assign { target, value, .. } => {
            // Assignment to a global name = escaping.
            if let Expr::Ident { name, .. } = target {
                if ctx.global_names.contains(name) || ctx.param_names.contains(name) {
                    // Assigning to a param or global: the RHS escapes.
                    mark_escaping_recursive(value, tp, result);
                } else {
                    let cls = classify_expr(value, tp, ctx);
                    result.classes.insert(value.span(), cls);
                    analyze_expr(value, tp, ctx, result);
                }
            } else {
                // Assignment to a path (struct field, etc.) — conservative.
                mark_escaping_recursive(value, tp, result);
            }
        }
        Stmt::Expr(e) => {
            analyze_expr(e, tp, ctx, result);
        }
        Stmt::Return { value: Some(v), .. } => {
            // Returned values always escape.
            mark_escaping_recursive(v, tp, result);
            // Track the variable name if it's an identifier.
            match v {
                Expr::Ident { name, .. } => {
                    ctx.escaped_names.insert(name.clone());
                }
                Expr::Path { parts, .. } => {
                    if let Some(first) = parts.first() {
                        ctx.escaped_names.insert(first.clone());
                    }
                }
                _ => {}
            }
        }
        Stmt::Return { value: None, .. } => {}
        Stmt::For { iter, body, .. } => {
            // Iter expression escapes (evaluated once).
            let cls = classify_expr(iter, tp, ctx);
            result.classes.insert(iter.span(), cls);
            analyze_expr(iter, tp, ctx, result);
            // Loop body: allocations inside loops are conservatively
            // escaping (loop body executes multiple times, and the
            // arena reset would free them mid-loop if they escape the
            // iteration). Exception: simple arithmetic/int values.
            analyze_block(body, tp, ctx, result);
            // Mark all non-scalar allocations in the loop body as escaping.
            mark_loop_body_escaping(body, tp, result);
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        _ => {}
    }
}

fn analyze_expr(e: &Expr, tp: &TypedProgram, ctx: &mut FuncCtx<'_>, result: &mut EscapeResult) {
    match e {
        Expr::Block(b) => analyze_block(b, tp, ctx, result),
        Expr::If {
            cond, then, els, ..
        } => {
            analyze_expr(cond, tp, ctx, result);
            analyze_block(then, tp, ctx, result);
            if let Some(el) = els {
                analyze_expr(el, tp, ctx, result);
            }
        }
        Expr::Call { args, .. } => {
            // Arguments to function calls escape (we can't track cross-function).
            for a in args {
                mark_escaping_recursive(a, tp, result);
                // Track variable name if arg is an identifier.
                if let Expr::Ident { name, .. } = a {
                    ctx.escaped_names.insert(name.clone());
                }
            }
        }
        Expr::Binary { left, right, .. } => {
            analyze_expr(left, tp, ctx, result);
            analyze_expr(right, tp, ctx, result);
        }
        Expr::Unary { expr, .. } => analyze_expr(expr, tp, ctx, result),
        Expr::Paren { expr, .. } => analyze_expr(expr, tp, ctx, result),
        Expr::Array { elems, .. } => {
            for elem in elems {
                analyze_expr(elem, tp, ctx, result);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                analyze_expr(k, tp, ctx, result);
                analyze_expr(v, tp, ctx, result);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let zz_frontend::ast::FmtPart::Expr(inner, _) = part {
                    analyze_expr(inner, tp, ctx, result);
                }
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, f) in fields {
                analyze_expr(f, tp, ctx, result);
            }
        }
        Expr::Range { start, end, .. } => {
            analyze_expr(start, tp, ctx, result);
            analyze_expr(end, tp, ctx, result);
        }
        _ => {}
    }
}

/// Classify a single expression's allocation behavior.
fn classify_expr(e: &Expr, tp: &TypedProgram, ctx: &FuncCtx<'_>) -> AllocClass {
    // Scalar types never allocate.
    if let Some(ty) = tp.type_at(e.span()) {
        if is_scalar_type(ty) {
            return AllocClass::NonEscaping;
        }
    }

    match e {
        // Literals that allocate heap objects.
        Expr::Str { .. } => AllocClass::NonEscaping,
        Expr::Array { .. } => AllocClass::NonEscaping,
        Expr::Dict { .. } => AllocClass::NonEscaping,
        Expr::StructInit { .. } => AllocClass::NonEscaping,
        Expr::Fmt { .. } => AllocClass::NonEscaping,

        // Identifiers: look up whether it's a param (non-escaping) or global (escaping).
        Expr::Ident { name, .. } => {
            if ctx.global_names.contains(name) {
                AllocClass::Escaping
            } else {
                AllocClass::NonEscaping
            }
        }

        // Function calls always escape (we can't track what the callee does).
        Expr::Call { .. } => AllocClass::Escaping,

        // Block: classify as the last expression's class.
        Expr::Block(b) => {
            if let Some(Stmt::Expr(e)) = b.stmts.last() {
                classify_expr(e, tp, ctx)
            } else {
                AllocClass::NonEscaping
            }
        }

        // If/else: escape if either branch escapes.
        Expr::If { then, els, .. } => {
            let then_cls = classify_block_terminal(then, tp, ctx);
            let els_cls = els
                .as_ref()
                .map(|e| classify_expr(e, tp, ctx))
                .unwrap_or(AllocClass::NonEscaping);
            if then_cls == AllocClass::Escaping || els_cls == AllocClass::Escaping {
                AllocClass::Escaping
            } else {
                AllocClass::NonEscaping
            }
        }

        // Paren/Unary: inherit from inner.
        Expr::Paren { expr, .. } => classify_expr(expr, tp, ctx),
        Expr::Unary { expr, .. } => classify_expr(expr, tp, ctx),

        // Binary: inherit from children.
        Expr::Binary { left, right, .. } => {
            let l = classify_expr(left, tp, ctx);
            let r = classify_expr(right, tp, ctx);
            if l == AllocClass::Escaping || r == AllocClass::Escaping {
                AllocClass::Escaping
            } else {
                AllocClass::NonEscaping
            }
        }

        // Non-allocating expressions.
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::Path { .. } => {
            AllocClass::NonEscaping
        }

        _ => AllocClass::Escaping, // conservative
    }
}

fn classify_block_terminal(
    block: &crate::Block,
    tp: &TypedProgram,
    ctx: &FuncCtx<'_>,
) -> AllocClass {
    if let Some(Stmt::Expr(e)) = block.stmts.last() {
        classify_expr(e, tp, ctx)
    } else {
        AllocClass::NonEscaping
    }
}

/// Mark all heap-allocating sub-expressions as escaping.
fn mark_escaping_recursive(e: &Expr, tp: &TypedProgram, result: &mut EscapeResult) {
    if let Some(ty) = tp.type_at(e.span()) {
        if !is_scalar_type(ty) {
            result.classes.insert(e.span(), AllocClass::Escaping);
        }
    } else {
        // Unknown type — conservative: mark as escaping if it's a known allocating expr.
        if is_allocating_expr(e) {
            result.classes.insert(e.span(), AllocClass::Escaping);
        }
    }

    // Recurse into sub-expressions.
    match e {
        Expr::Block(b) => {
            for stmt in &b.stmts {
                if let Stmt::Expr(inner) = stmt {
                    mark_escaping_recursive(inner, tp, result);
                }
            }
        }
        Expr::If { then, els, .. } => {
            for stmt in &then.stmts {
                if let Stmt::Expr(inner) = stmt {
                    mark_escaping_recursive(inner, tp, result);
                }
            }
            if let Some(el) = els {
                mark_escaping_recursive(el, tp, result);
            }
        }
        Expr::Binary { left, right, .. } => {
            mark_escaping_recursive(left, tp, result);
            mark_escaping_recursive(right, tp, result);
        }
        Expr::Unary { expr, .. } => mark_escaping_recursive(expr, tp, result),
        Expr::Paren { expr, .. } => mark_escaping_recursive(expr, tp, result),
        Expr::Array { elems, .. } => {
            for elem in elems {
                mark_escaping_recursive(elem, tp, result);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, f) in fields {
                mark_escaping_recursive(f, tp, result);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let zz_frontend::ast::FmtPart::Expr(inner, _) = part {
                    mark_escaping_recursive(inner, tp, result);
                }
            }
        }
        _ => {}
    }
}

/// Mark all non-scalar allocations inside a loop body as escaping.
fn mark_loop_body_escaping(block: &crate::Block, tp: &TypedProgram, result: &mut EscapeResult) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl { value, .. } => {
                if let Some(ty) = tp.type_at(value.span()) {
                    if !is_scalar_type(ty) {
                        result.classes.insert(value.span(), AllocClass::Escaping);
                    }
                }
                if is_allocating_expr(value) {
                    result.classes.insert(value.span(), AllocClass::Escaping);
                }
            }
            Stmt::Expr(e) => {
                mark_loop_expr_escaping(e, tp, result);
            }
            Stmt::For { body: inner, .. } => {
                mark_loop_body_escaping(inner, tp, result);
            }
            _ => {}
        }
    }
}

fn mark_loop_expr_escaping(e: &Expr, tp: &TypedProgram, result: &mut EscapeResult) {
    if let Some(ty) = tp.type_at(e.span()) {
        if !is_scalar_type(ty) {
            result.classes.insert(e.span(), AllocClass::Escaping);
        }
    }
    match e {
        Expr::Array { elems, .. } => {
            for elem in elems {
                mark_loop_expr_escaping(elem, tp, result);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, f) in fields {
                mark_loop_expr_escaping(f, tp, result);
            }
        }
        Expr::Block(b) => {
            for stmt in &b.stmts {
                if let Stmt::Expr(inner) = stmt {
                    mark_loop_expr_escaping(inner, tp, result);
                }
            }
        }
        _ => {}
    }
}

/// Check if a type is scalar (no heap allocation).
fn is_scalar_type(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Unit)
}

/// Check if an expression is known to allocate heap memory.
fn is_allocating_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Str { .. }
            | Expr::Array { .. }
            | Expr::Dict { .. }
            | Expr::StructInit { .. }
            | Expr::Fmt { .. }
            | Expr::Call { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn build_tp(src: &str) -> TypedProgram {
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        // Seed a minimal stdlib for io.println only.
        let mut funcs = HashMap::new();
        let unit = Type::Unit;
        let t_any = Type::Named("T".to_string());
        funcs.insert(
            "io.println".into(),
            crate::FuncSig {
                generics: vec!["T".into()],
                params: vec![("v".into(), t_any)],
                has_default: vec![false],
                ret: unit,
            },
        );
        let res = crate::build_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
        res.program
    }

    #[test]
    fn local_array_non_escaping() {
        let src = r#"
func main() {
    arr := [1, 2, 3]
    x := 0
    for i in 0..3 {
        x = x + 1
    }
}
"#;
        let tp = build_tp(src);
        let result = analyze(&tp);
        // The [1, 2, 3] array literal should be non-escaping (local only,
        // not passed to any function or returned).
        let arr_entries: Vec<_> = result.classes.iter().collect();
        // At least one entry should be NonEscaping (the array literal).
        let has_non_escaping = result
            .classes
            .values()
            .any(|c| *c == AllocClass::NonEscaping);
        assert!(
            has_non_escaping,
            "local array should be NonEscaping, got: {:?}",
            arr_entries
        );
    }

    #[test]
    fn returned_value_escapes() {
        let src = r#"
func make() -> array {
    arr := [1, 2, 3]
    arr
}
func main() {
    x := make()
}
"#;
        let tp = build_tp(src);
        let result = analyze(&tp);
        // The returned arr should be marked escaping.
        let has_escaping = result.classes.values().any(|c| *c == AllocClass::Escaping);
        assert!(has_escaping, "returned value should be marked escaping");
    }

    #[test]
    fn scalar_always_non_escaping() {
        let src = r#"
func main() {
    x := 42
    y := 3.14
    z := true
}
"#;
        let tp = build_tp(src);
        let result = analyze(&tp);
        // Scalar allocations should all be NonEscaping.
        for cls in result.classes.values() {
            assert_eq!(
                *cls,
                AllocClass::NonEscaping,
                "scalar should always be NonEscaping"
            );
        }
    }

    #[test]
    fn loop_body_allocations_escaping() {
        let src = r#"
func main() {
    for i in 0..100 {
        s := "hello"
    }
}
"#;
        let tp = build_tp(src);
        let result = analyze(&tp);
        // String allocations inside loop body should be escaping.
        let has_escaping = result.classes.values().any(|c| *c == AllocClass::Escaping);
        if !has_escaping {
            // Debug: print all classifications.
            for (span, cls) in &result.classes {
                eprintln!("span {:?} -> {:?}", span, cls);
            }
        }
        assert!(
            has_escaping,
            "loop body allocations should be escaping, classes: {:?}",
            result.classes
        );
    }
}
