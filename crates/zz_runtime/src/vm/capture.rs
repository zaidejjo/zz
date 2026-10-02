use zz_frontend::ast::{Block, Expr, FmtPart, Param, Pattern, Stmt};

/// Whether a pattern binds any names (forcing a scope environment).
pub(crate) fn pattern_binds(pat: &Pattern) -> bool {
    match pat {
        Pattern::Binding { .. } => true,
        Pattern::Variant { arg: Some(p), .. } => pattern_binds(p),
        Pattern::Tuple { pats, .. } => pats.iter().any(pattern_binds),
        Pattern::Or { pats, .. } => pats.iter().any(pattern_binds),
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
        scan_stmt_captured(stmt, &mut defined, &mut free, true);
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
    scan_expr_captured(body, &mut defined, &mut free, true);
    free
}

/// `nested` marks code that executes outside the current frame
/// (function and closure bodies, transitively): only references made
/// there can observe the environment, so only they count toward
/// capture. Top-level statements — including loop bodies and blocks,
/// which run inline in the current frame — resolve through slots and
/// must not force environment promotion.
pub(crate) fn scan_expr_captured(
    expr: &Expr,
    defined: &mut std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
    nested: bool,
) {
    match expr {
        Expr::Ident { name, .. } => {
            if nested && !defined.contains(name) {
                free.insert(name.clone());
            }
        }
        Expr::Closure { params, body, .. } => {
            let mut inner: std::collections::HashSet<String> =
                params.iter().map(|p| p.name.name.clone()).collect();
            scan_expr_captured(body, &mut inner, free, true);
        }
        Expr::Block(block) => {
            let mut inner = defined.clone();
            for stmt in &block.stmts {
                scan_stmt_captured(stmt, &mut inner, free, nested);
            }
        }
        Expr::Paren { expr, .. } => scan_expr_captured(expr, defined, free, nested),
        Expr::Unary { expr, .. } => scan_expr_captured(expr, defined, free, nested),
        Expr::Binary { left, right, .. } => {
            scan_expr_captured(left, defined, free, nested);
            scan_expr_captured(right, defined, free, nested);
        }
        Expr::Call { callee, args, .. } => {
            // Callee position resolves through the environment at
            // runtime (`CallPath`/`CallMethod` never consult slots), so
            // a callee root naming a top-level binding must stay
            // environment-promoted even in inline code (`p.dist()`).
            // Argument and receiver values use slot-aware loads.
            scan_expr_captured(callee, defined, free, true);
            for a in args {
                scan_expr_captured(a, defined, free, nested);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            scan_expr_captured(cond, defined, free, nested);
            let mut inner = defined.clone();
            for stmt in &then.stmts {
                scan_stmt_captured(stmt, &mut inner, free, nested);
            }
            if let Some(e) = els {
                scan_expr_captured(e, defined, free, nested);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, _) = part {
                    scan_expr_captured(e, defined, free, nested);
                }
            }
        }
        Expr::While { cond, body, .. } => {
            scan_expr_captured(cond, defined, free, nested);
            let mut inner = defined.clone();
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free, nested);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            scan_expr_captured(scrutinee, defined, free, nested);
            for arm in arms {
                let mut inner = defined.clone();
                collect_pattern_bindings(&arm.pat, &mut inner);
                scan_expr_captured(&arm.body, &mut inner, free, nested);
            }
        }
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            ..
        } => {
            scan_expr_captured(value, defined, free, nested);
            let mut inner = defined.clone();
            collect_pattern_bindings(pat, &mut inner);
            for stmt in &then.stmts {
                scan_stmt_captured(stmt, &mut inner, free, nested);
            }
            if let Some(e) = els {
                scan_expr_captured(e, defined, free, nested);
            }
        }
        Expr::Try { expr, .. } => scan_expr_captured(expr, defined, free, nested),
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                scan_expr_captured(a, defined, free, nested);
            }
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                scan_expr_captured(e, defined, free, nested);
            }
        }
        Expr::Tuple { items, .. } => {
            for e in items {
                scan_expr_captured(e, defined, free, nested);
            }
        }
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            ..
        } => {
            scan_expr_captured(iter, defined, free, nested);
            let mut inner = defined.clone();
            inner.insert(var.name.clone());
            if let Some(f) = filter {
                scan_expr_captured(f, &mut inner, free, nested);
            }
            scan_expr_captured(body, &mut inner, free, nested);
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                scan_expr_captured(k, defined, free, nested);
                scan_expr_captured(v, defined, free, nested);
            }
        }
        Expr::Field { obj, .. } => scan_expr_captured(obj, defined, free, nested),
        Expr::Range { start, end, .. } => {
            scan_expr_captured(start, defined, free, nested);
            scan_expr_captured(end, defined, free, nested);
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                scan_expr_captured(v, defined, free, nested);
            }
        }
        Expr::Index { obj, index, .. } => {
            scan_expr_captured(obj, defined, free, nested);
            scan_expr_captured(index, defined, free, nested);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            scan_expr_captured(obj, defined, free, nested);
            if let Some(e) = start {
                scan_expr_captured(e, defined, free, nested);
            }
            if let Some(e) = end {
                scan_expr_captured(e, defined, free, nested);
            }
        }
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
        // A dotted path may reference a namespaced top-level binding
        // (`ns.var`), a struct field access (`p.x`), or a nested module
        // path.  The *root* variable (`parts[0]`) is what must be
        // captured from the enclosing scope; the full joined name is
        // kept as a fallback for namespaced top-level bindings stored as
        // single slots.
        Expr::Path { parts, .. } => {
            if !nested {
                return;
            }
            let root = parts[0].clone();
            if !defined.contains(&root) {
                free.insert(root);
            }
            let full = parts.join(".");
            if full != parts[0] && !defined.contains(&full) {
                free.insert(full);
            }
        }
    }
}

pub(crate) fn scan_stmt_captured(
    stmt: &Stmt,
    defined: &mut std::collections::HashSet<String>,
    free: &mut std::collections::HashSet<String>,
    nested: bool,
) {
    match stmt {
        Stmt::Decl { name, value, .. } => {
            scan_expr_captured(value, defined, free, nested);
            defined.insert(name.name.clone());
        }
        Stmt::Import { .. } => {}
        Stmt::ExternBlock { .. } | Stmt::Link { .. } => {}
        Stmt::Func {
            name, params, body, ..
        } => {
            let mut inner: std::collections::HashSet<String> =
                params.iter().map(|p| p.name.name.clone()).collect();
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free, true);
            }
            defined.insert(name.join("."));
        }
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                scan_expr_captured(e, defined, free, nested);
            }
        }
        Stmt::Struct { .. } => {}
        Stmt::Impl { methods, .. } => {
            for method in methods {
                scan_stmt_captured(method, defined, free, nested);
            }
        }
        Stmt::For {
            vars, iter, body, ..
        } => {
            scan_expr_captured(iter, defined, free, nested);
            let mut inner = defined.clone();
            for v in vars {
                inner.insert(v.name.clone());
            }
            for stmt in &body.stmts {
                scan_stmt_captured(stmt, &mut inner, free, nested);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Defer { expr, .. } => {
            scan_expr_captured(expr, defined, free, nested);
        }
        Stmt::Assign { target, value, .. } => {
            scan_expr_captured(value, defined, free, nested);
            scan_expr_captured(target, defined, free, nested);
        }
        Stmt::Destructure { pat, value, .. } => {
            scan_expr_captured(value, defined, free, nested);
            collect_pattern_bindings(pat, defined);
        }
        Stmt::Expr(e) => scan_expr_captured(e, defined, free, nested),
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
        Pattern::Tuple { pats, .. } => {
            for p in pats {
                collect_pattern_bindings(p, defined);
            }
        }
        Pattern::Or { pats, .. } => {
            for p in pats {
                collect_pattern_bindings(p, defined);
            }
        }
        _ => {}
    }
}
