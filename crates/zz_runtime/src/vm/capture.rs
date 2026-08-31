use zz_frontend::ast::{Block, Expr, FmtPart, Param, Pattern, Stmt};

/// Whether a pattern binds any names (forcing a scope environment).
pub(crate) fn pattern_binds(pat: &Pattern) -> bool {
    match pat {
        Pattern::Binding { .. } => true,
        Pattern::Variant { arg: Some(p), .. } => pattern_binds(p),
        _ => false,
    }
}

/// Collect the names referenced by nested closures but not defined within
/// them. These must live in the environment so closures can capture them.
pub(crate) fn scan_block_captured(
    block: &Block,
    params: &[Param],
) -> std::collections::HashSet<String> {
    let mut defined: std::collections::HashSet<String> =
        params.iter().map(|p| p.name.name.clone()).collect();
    let mut free = std::collections::HashSet::new();
    for stmt in &block.stmts {
        scan_stmt_captured(stmt, &mut defined, &mut free);
    }
    free
}

/// Like [`scan_block_captured`] but for a closure body (an expression).
pub(crate) fn scan_closure_captured(
    body: &Expr,
    params: &[Param],
) -> std::collections::HashSet<String> {
    let mut defined: std::collections::HashSet<String> =
        params.iter().map(|p| p.name.name.clone()).collect();
    let mut free = std::collections::HashSet::new();
    scan_expr_captured(body, &mut defined, &mut free);
    free
}

pub(crate) fn scan_expr_captured(
    expr: &Expr,
    defined: &mut std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
) {
    match expr {
        Expr::Ident { name, .. } => {
            if !defined.contains(name) {
                free.insert(name.clone());
            }
        }
        Expr::Closure { params, body, .. } => {
            let mut inner: std::collections::HashSet<String> =
                params.iter().map(|p| p.name.name.clone()).collect();
            scan_expr_captured(body, &mut inner, free);
        }
        Expr::Block(block) => {
            let mut inner = defined.clone();
            for stmt in &block.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
        }
        Expr::Paren { expr, .. } => scan_expr_captured(expr, defined, free),
        Expr::Unary { expr, .. } => scan_expr_captured(expr, defined, free),
        Expr::Binary { left, right, .. } => {
            scan_expr_captured(left, defined, free);
            scan_expr_captured(right, defined, free);
        }
        Expr::Call { callee, args, .. } => {
            scan_expr_captured(callee, defined, free);
            for a in args {
                scan_expr_captured(a, defined, free);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            scan_expr_captured(cond, defined, free);
            let mut inner = defined.clone();
            for stmt in &then.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
            if let Some(e) = els {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, _) = part {
                    scan_expr_captured(e, defined, free);
                }
            }
        }
        Expr::While { cond, body, .. } => {
            scan_expr_captured(cond, defined, free);
            let mut inner = defined.clone();
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            scan_expr_captured(scrutinee, defined, free);
            for arm in arms {
                let mut inner = defined.clone();
                collect_pattern_bindings(&arm.pat, &mut inner);
                scan_expr_captured(&arm.body, &mut inner, free);
            }
        }
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            ..
        } => {
            scan_expr_captured(value, defined, free);
            let mut inner = defined.clone();
            collect_pattern_bindings(pat, &mut inner);
            for stmt in &then.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
            if let Some(e) = els {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::Try { expr, .. } => scan_expr_captured(expr, defined, free),
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                scan_expr_captured(a, defined, free);
            }
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            ..
        } => {
            scan_expr_captured(iter, defined, free);
            let mut inner = defined.clone();
            inner.insert(var.name.clone());
            if let Some(f) = filter {
                scan_expr_captured(f, &mut inner, free);
            }
            scan_expr_captured(body, &mut inner, free);
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                scan_expr_captured(k, defined, free);
                scan_expr_captured(v, defined, free);
            }
        }
        Expr::Field { obj, .. } => scan_expr_captured(obj, defined, free),
        Expr::Range { start, end, .. } => {
            scan_expr_captured(start, defined, free);
            scan_expr_captured(end, defined, free);
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                scan_expr_captured(v, defined, free);
            }
        }
        Expr::Index { obj, index, .. } => {
            scan_expr_captured(obj, defined, free);
            scan_expr_captured(index, defined, free);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            scan_expr_captured(obj, defined, free);
            if let Some(e) = start {
                scan_expr_captured(e, defined, free);
            }
            if let Some(e) = end {
                scan_expr_captured(e, defined, free);
            }
        }
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Path { .. } => {}
    }
}

pub(crate) fn scan_stmt_captured(
    stmt: &Stmt,
    defined: &mut std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Stmt::Decl { name, value, .. } => {
            scan_expr_captured(value, defined, free);
            defined.insert(name.name.clone());
        }
        Stmt::Import { .. } => {}
        Stmt::Func {
            name, params, body, ..
        } => {
            let mut inner: std::collections::HashSet<String> =
                params.iter().map(|p| p.name.name.clone()).collect();
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
            defined.insert(name.join("."));
        }
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                scan_expr_captured(e, defined, free);
            }
        }
        Stmt::Struct { .. } => {}
        Stmt::For {
            vars, iter, body, ..
        } => {
            scan_expr_captured(iter, defined, free);
            let mut inner = defined.clone();
            for v in vars {
                inner.insert(v.name.clone());
            }
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Defer { expr, .. } => {
            scan_expr_captured(expr, defined, free);
        }
        Stmt::Assign { target, value, .. } => {
            scan_expr_captured(value, defined, free);
            scan_expr_captured(target, defined, free);
        }
        Stmt::Expr(e) => scan_expr_captured(e, defined, free),
    }
}

/// Add a pattern's binding names to `defined`.
pub(crate) fn collect_pattern_bindings(
    pat: &Pattern,
    defined: &mut std::collections::HashSet<String>,
) {
    match pat {
        Pattern::Binding { name } => {
            defined.insert(name.name.clone());
        }
        Pattern::Variant { arg: Some(p), .. } => collect_pattern_bindings(p, defined),
        _ => {}
    }
}
