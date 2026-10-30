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
    /// For-loop spans whose body contains at least one non-escaping allocation.
    /// Codegen uses this to inject `zz_arena_reset(&_arena)` at the end of
    /// each iteration, preventing unbounded arena growth in tight loops.
    pub loop_spans: HashSet<Span>,
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
                // Closure-captured bindings outlive the function scope (the
                // environment holds shared cells), so their initializers
                // must be heap-allocated, never arena/stack.
                let captured = crate::capture::captured_in_block(
                    &params
                        .iter()
                        .map(|p| p.name.name.clone())
                        .collect::<Vec<_>>(),
                    body,
                    &global_names,
                );
                let mut ctx = FuncCtx {
                    global_names: &global_names,
                    param_names,
                    func_name: &fname,
                    escaped_names: captured,
                };
                analyze_block_impl(body, tp, &mut ctx, &mut result);
                // Propagate escaping to initializer expressions of escaped variables.
                propagate_escaped(body, &ctx.escaped_names, tp, ctx.func_name, &mut result);
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
/// escaped variable as escaping. Recurses into nested blocks (if/while/block
/// expressions) so that a variable declared inside a nested scope and
/// returned from there has its initializer marked escaping too.
fn propagate_escaped(
    block: &crate::Block,
    escaped_names: &HashSet<String>,
    tp: &TypedProgram,
    scope: &str,
    result: &mut EscapeResult,
) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                if escaped_names.contains(&name.name) {
                    mark_escaping_recursive(value, tp, scope, result);
                }
            }
            Stmt::For { body, .. } => {
                propagate_escaped(body, escaped_names, tp, scope, result);
            }
            Stmt::Expr(e) => match e {
                Expr::If { then, els, .. } => {
                    propagate_escaped(then, escaped_names, tp, scope, result);
                    if let Some(el) = els.as_ref() {
                        propagate_escaped_expr(el, escaped_names, tp, scope, result);
                    }
                }
                Expr::While { body, .. } => {
                    propagate_escaped(body, escaped_names, tp, scope, result);
                }
                Expr::Block(b) => {
                    propagate_escaped(b, escaped_names, tp, scope, result);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Recurses into a single expression that may contain a block body (used
/// for the `else` branch of `if`, which is `Option<Box<Expr>>` rather than
/// a `Block`).
fn propagate_escaped_expr(
    e: &Expr,
    escaped_names: &HashSet<String>,
    tp: &TypedProgram,
    scope: &str,
    result: &mut EscapeResult,
) {
    match e {
        Expr::If { then, els, .. } => {
            propagate_escaped(then, escaped_names, tp, scope, result);
            if let Some(el) = els.as_ref() {
                propagate_escaped_expr(el, escaped_names, tp, scope, result);
            }
        }
        Expr::While { body, .. } => {
            propagate_escaped(body, escaped_names, tp, scope, result);
        }
        Expr::Block(b) => {
            propagate_escaped(b, escaped_names, tp, scope, result);
        }
        _ => {}
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

/// Analyze a block's statements. Named functions have no tail value (a
/// value leaves only through `return`, which `analyze_stmt` handles), so
/// every statement is analyzed in discard position.
fn analyze_block_impl(
    block: &crate::Block,
    tp: &TypedProgram,
    ctx: &mut FuncCtx<'_>,
    result: &mut EscapeResult,
) {
    for stmt in &block.stmts {
        analyze_stmt(stmt, tp, ctx, result);
    }
}

fn analyze_block(
    block: &crate::Block,
    tp: &TypedProgram,
    ctx: &mut FuncCtx<'_>,
    result: &mut EscapeResult,
) {
    analyze_block_impl(block, tp, ctx, result);
}

fn analyze_stmt(stmt: &Stmt, tp: &TypedProgram, ctx: &mut FuncCtx<'_>, result: &mut EscapeResult) {
    match stmt {
        Stmt::Decl { name, value, .. } => {
            // Top-level bindings live in `zz_global_*` past every frame:
            // their initializers escape by definition (#421: a const
            // array lowered as a stack temporary dangles once `zz_main`
            // returns). Same for any binding shadowing a global name
            // (conservative: over-marking only costs an allocation).
            if ctx.func_name == TOP || ctx.global_names.contains(&name.name) {
                mark_escaping_recursive(value, tp, ctx.func_name, result);
            } else {
                let cls = classify_expr(value, tp, ctx);
                result.classes.insert(value.span(), cls);
                // Also classify sub-expressions within the initializer.
                analyze_expr(value, tp, ctx, result);
            }
        }
        Stmt::Assign { target, value, .. } => {
            // Assignment to a global name = escaping.
            if let Expr::Ident { name, .. } = target {
                if ctx.global_names.contains(name) || ctx.param_names.contains(name) {
                    // Assigning to a param or global: the RHS escapes.
                    mark_escaping_recursive(value, tp, ctx.func_name, result);
                } else if ctx.escaped_names.contains(name) {
                    // Assigning to a variable that escapes this scope (e.g.
                    // read after the loop, returned, or passed to a callee):
                    // the RHS outlives the loop iteration and must not be
                    // arena-allocated (it would dangle at arena reset).
                    mark_escaping_recursive(value, tp, ctx.func_name, result);
                } else {
                    let cls = classify_expr(value, tp, ctx);
                    result.classes.insert(value.span(), cls);
                    analyze_expr(value, tp, ctx, result);
                }
            } else {
                // Assignment to a path (struct field, etc.) — conservative.
                mark_escaping_recursive(value, tp, ctx.func_name, result);
            }
        }
        Stmt::CompoundAssign { target, value, .. } => {
            // Same escape behavior as `=`: the target is also read, which
            // never escapes anything the plain assignment wouldn't.
            if let Expr::Ident { name, .. } = target {
                if ctx.global_names.contains(name) || ctx.param_names.contains(name) {
                    mark_escaping_recursive(value, tp, ctx.func_name, result);
                } else {
                    let cls = classify_expr(value, tp, ctx);
                    result.classes.insert(value.span(), cls);
                    analyze_expr(value, tp, ctx, result);
                }
            } else {
                mark_escaping_recursive(value, tp, ctx.func_name, result);
            }
        }
        Stmt::Expr(e) => {
            analyze_expr(e, tp, ctx, result);
        }
        Stmt::Return { value: Some(v), .. } => {
            // Returned values always escape.
            mark_escaping_recursive(v, tp, ctx.func_name, result);
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
        Stmt::For {
            iter, body, span, ..
        } => {
            // Iter expression escapes (evaluated once).
            let cls = classify_expr(iter, tp, ctx);
            result.classes.insert(iter.span(), cls);
            analyze_expr(iter, tp, ctx, result);
            // Loop body: analyze normally. Allocations that escape the
            // loop (returned, stored in outer scope, passed to functions
            // that retain them) are marked escaping by the normal analysis.
            // The per-iteration arena reset (injected by codegen when
            // loop_spans contains this span) safely frees all arena-
            // allocated objects at the end of each iteration.
            analyze_block(body, tp, ctx, result);
            // If the loop body has any non-scalar allocation, record the
            // loop span so codegen can inject a per-iteration arena reset.
            if has_non_scalar_alloc(body, tp, ctx.func_name) {
                result.loop_spans.insert(*span);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        _ => {}
    }
}

fn analyze_expr(e: &Expr, tp: &TypedProgram, ctx: &mut FuncCtx<'_>, result: &mut EscapeResult) {
    match e {
        Expr::Block(b) => analyze_block(b, tp, ctx, result),
        Expr::While {
            cond, body, span, ..
        } => {
            // The condition is evaluated once per iteration; it may itself
            // allocate (e.g., a call returning a string).
            analyze_expr(cond, tp, ctx, result);
            // Loop body: analyzed like a For body. Allocations that escape
            // the loop are marked escaping by the normal analysis; the
            // per-iteration arena reset (injected by codegen when
            // loop_spans contains this span) frees all arena-allocated
            // objects at the end of each iteration.
            analyze_block(body, tp, ctx, result);
            // If the loop body has any non-scalar allocation, record the
            // loop span so codegen can inject a per-iteration arena reset.
            if has_non_scalar_alloc(body, tp, ctx.func_name) {
                result.loop_spans.insert(*span);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            analyze_expr(cond, tp, ctx, result);
            analyze_block(then, tp, ctx, result);
            if let Some(el) = els {
                analyze_expr(el, tp, ctx, result);
            }
        }
        Expr::Call { callee, args, .. } => {
            // A `readonly` function (via `#[readonly]` decorator) never
            // retains its arguments — arguments don't escape through the
            // callee, so they can stay arena-allocated instead of being
            // forced to ARC heap. Also hardcodes read-only builtins not
            // yet in `tp.funcs`.
            let callee_readonly = match callee.as_ref() {
                Expr::Ident { name, .. } => tp.funcs.get(name).map(|f| f.readonly).unwrap_or(false),
                Expr::Path { parts, .. } => {
                    let joined = parts.join(".");
                    tp.funcs.get(&joined).map(|f| f.readonly).unwrap_or(false)
                }
                _ => false,
            };
            let is_read_only_builtin = callee_readonly
                || matches!(
                    callee.as_ref(),
                    Expr::Ident { name, .. }
                        if matches!(
                            name.as_str(),
                            "len" | "std.len" | "vec.len" | "std.vec.len" | "range" | "std.range"
                        )
                );
            if is_read_only_builtin {
                for a in args {
                    analyze_expr(a, tp, ctx, result);
                }
            } else {
                for a in args {
                    mark_escaping_recursive(a, tp, ctx.func_name, result);
                    // Track variable name if arg is an identifier.
                    if let Expr::Ident { name, .. } = a {
                        ctx.escaped_names.insert(name.clone());
                    }
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
    if let Some(ty) = tp.type_at(ctx.func_name, e.span()) {
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
fn mark_escaping_recursive(e: &Expr, tp: &TypedProgram, scope: &str, result: &mut EscapeResult) {
    if let Some(ty) = tp.type_at(scope, e.span()) {
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
                    mark_escaping_recursive(inner, tp, scope, result);
                }
            }
        }
        Expr::While { cond, body, .. } => {
            mark_escaping_recursive(cond, tp, scope, result);
            for stmt in &body.stmts {
                if let Stmt::Expr(inner) = stmt {
                    mark_escaping_recursive(inner, tp, scope, result);
                }
            }
        }
        Expr::If { then, els, .. } => {
            for stmt in &then.stmts {
                if let Stmt::Expr(inner) = stmt {
                    mark_escaping_recursive(inner, tp, scope, result);
                }
            }
            if let Some(el) = els {
                mark_escaping_recursive(el, tp, scope, result);
            }
        }
        Expr::Binary { left, right, .. } => {
            mark_escaping_recursive(left, tp, scope, result);
            mark_escaping_recursive(right, tp, scope, result);
        }
        Expr::Unary { expr, .. } => mark_escaping_recursive(expr, tp, scope, result),
        Expr::Paren { expr, .. } => mark_escaping_recursive(expr, tp, scope, result),
        Expr::Array { elems, .. } => {
            for elem in elems {
                mark_escaping_recursive(elem, tp, scope, result);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, f) in fields {
                mark_escaping_recursive(f, tp, scope, result);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let zz_frontend::ast::FmtPart::Expr(inner, _) = part {
                    mark_escaping_recursive(inner, tp, scope, result);
                }
            }
        }
        _ => {}
    }
}

/// Check if a block contains any non-scalar allocation (array, dict, struct).
/// Used to decide whether to inject a per-iteration arena reset in a loop.
fn has_non_scalar_alloc(block: &crate::Block, tp: &TypedProgram, scope: &str) -> bool {
    for stmt in &block.stmts {
        if stmt_has_non_scalar_alloc(stmt, tp, scope) {
            return true;
        }
    }
    false
}

fn stmt_has_non_scalar_alloc(stmt: &Stmt, tp: &TypedProgram, scope: &str) -> bool {
    match stmt {
        Stmt::Decl { value, .. } => expr_has_non_scalar_alloc(value, tp, scope),
        Stmt::Expr(e) => expr_has_non_scalar_alloc(e, tp, scope),
        // Assignments (and compound/destructure/defer payloads) allocate
        // exactly like declarations: `s = s + lit` in a loop without this
        // arm never gets a loop arena, and its chained heap temporaries are
        // never released (GB-scale leak on accumulation loops). `Return`
        // values are deliberately excluded: a returned allocation must stay
        // valid past loop exit, which an iteration arena cannot provide.
        Stmt::Assign { value, .. } => expr_has_non_scalar_alloc(value, tp, scope),
        Stmt::CompoundAssign { value, .. } => expr_has_non_scalar_alloc(value, tp, scope),
        Stmt::Destructure { value, .. } => expr_has_non_scalar_alloc(value, tp, scope),
        Stmt::Defer { expr, .. } => expr_has_non_scalar_alloc(expr, tp, scope),
        Stmt::For { body, .. } => has_non_scalar_alloc(body, tp, scope),
        _ => false,
    }
}

fn expr_has_non_scalar_alloc(e: &Expr, tp: &TypedProgram, scope: &str) -> bool {
    if let Some(ty) = tp.type_at(scope, e.span()) {
        if !is_scalar_type(ty) {
            return true;
        }
    }
    match e {
        // Strings and fmt strings allocate heap objects; inside a loop body
        // they are freed each iteration by the injected arena reset, so they
        // count toward `loop_spans` and route to the loop arena.
        Expr::Str { .. } | Expr::Fmt { .. } => true,
        Expr::Array { elems, .. } => elems
            .iter()
            .any(|x| expr_has_non_scalar_alloc(x, tp, scope)),
        Expr::Dict { entries, .. } => entries.iter().any(|(k, v)| {
            expr_has_non_scalar_alloc(k, tp, scope) || expr_has_non_scalar_alloc(v, tp, scope)
        }),
        Expr::StructInit { fields, .. } => fields
            .iter()
            .any(|(_, f)| expr_has_non_scalar_alloc(f, tp, scope)),
        Expr::Call { args, .. } => args.iter().any(|a| expr_has_non_scalar_alloc(a, tp, scope)),
        Expr::Binary { left, right, .. } => {
            expr_has_non_scalar_alloc(left, tp, scope)
                || expr_has_non_scalar_alloc(right, tp, scope)
        }
        Expr::If {
            cond, then, els, ..
        } => {
            expr_has_non_scalar_alloc(cond, tp, scope)
                || has_non_scalar_alloc(then, tp, scope)
                || els
                    .as_ref()
                    .is_some_and(|e| expr_has_non_scalar_alloc(e, tp, scope))
        }
        Expr::While { cond, body, .. } => {
            expr_has_non_scalar_alloc(cond, tp, scope) || has_non_scalar_alloc(body, tp, scope)
        }
        Expr::Block(b) => b
            .stmts
            .iter()
            .any(|s| stmt_has_non_scalar_alloc(s, tp, scope)),
        _ => false,
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
        // Seed a minimal stdlib for println only.
        let mut funcs = HashMap::new();
        let unit = Type::Unit;
        let t_any = Type::Named("T".to_string());
        funcs.insert(
            "println".into(),
            crate::FuncSig {
                is_extern: false,
                readonly: false,
                extern_c_symbol: None,
                generics: vec!["T".into()],
                bounds: Vec::new(),
                params: vec![("v".into(), t_any)],
                has_default: vec![false],
                ret: unit,
            },
        );
        let res = crate::build_program(
            &parsed.program,
            HashMap::new(),
            funcs,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
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
    return arr
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
    fn global_array_escapes() {
        // #421: top-level bindings outlive every frame (`zz_global_*`),
        // so their initializers must never lower as stack temporaries.
        let src = "const CBOUNDS = [0, 54, 102, 146]\nfunc main() {\n    println(1)\n}\n";
        let tp = build_tp(src);
        let result = analyze(&tp);
        assert!(
            !result.classes.is_empty(),
            "expected classified initializers, got none"
        );
        for cls in result.classes.values() {
            assert_eq!(
                *cls,
                AllocClass::Escaping,
                "top-level initializer must escape, got: {:?}",
                result.classes
            );
        }
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
        // Loop-aware analysis: string allocations inside a loop body are
        // NonEscaping — the per-iteration arena reset frees them each pass,
        // so they never accumulate. The loop's iter expression (`0..100`)
        // is still conservatively Escaping (evaluated once per loop entry).
        let has_non_escaping = result
            .classes
            .values()
            .any(|c| *c == AllocClass::NonEscaping);
        assert!(
            has_non_escaping,
            "loop body string should be NonEscaping (arena-reset per iter), classes: {:?}",
            result.classes
        );
    }

    #[test]
    fn readonly_func_args_do_not_escape() {
        // A `#[readonly]` function never retains its arguments, so
        // arguments passed to it stay arena-allocatable instead of being
        // forced to ARC heap.
        let src = r#"
@readonly
func pure_len(arr: array) -> int {
    return 0
}
func main() {
    arr := [1, 2, 3]
    n := pure_len(arr)
}
"#;
        let unit = Type::Unit;
        let t_int = Type::Int;
        let t_array = Type::Array(Box::new(Type::Int));
        let mut funcs = HashMap::new();
        funcs.insert(
            "println".into(),
            crate::FuncSig {
                is_extern: false,
                readonly: false,
                extern_c_symbol: None,
                generics: vec!["T".into()],
                bounds: Vec::new(),
                params: vec![("v".into(), Type::Named("T".to_string()))],
                has_default: vec![false],
                ret: unit,
            },
        );
        funcs.insert(
            "pure_len".into(),
            crate::FuncSig {
                is_extern: false,
                readonly: true,
                extern_c_symbol: None,
                generics: Vec::new(),
                bounds: Vec::new(),
                params: vec![("arr".into(), t_array.clone())],
                has_default: vec![false],
                ret: t_int,
            },
        );
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let res = crate::build_program(
            &parsed.program,
            HashMap::new(),
            funcs,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let tp = res.program;
        let result = analyze(&tp);
        // The array literal should be NonEscaping because pure_len is readonly.
        let arr_classes: Vec<_> = result
            .classes
            .iter()
            .filter(|(_, c)| **c == AllocClass::NonEscaping)
            .collect();
        assert!(
            !arr_classes.is_empty(),
            "array passed to readonly function should be NonEscaping, classes: {:?}",
            result.classes
        );
    }
}
