//! Call-graph construction and dead-code elimination.
//!
//! The analyzer works over a [`TypedProgram`]:
//! - builds the call graph (caller → callees, including method dispatch and
//!   stdlib natives) plus struct-instantiation and function-as-value edges,
//! - computes the reachable closure from entry points (`main` + all
//!   top-level statements),
//! - and prunes the program to only the reachable items.

use std::collections::{HashMap, HashSet, VecDeque};

use zz_checker::Type;
use zz_frontend::ast::{Expr, Stmt};

use crate::TypedProgram;

/// The pseudo-caller owning all top-level (non-function) statements.
pub const TOP: &str = "<top>";

/// A resolved method-dispatch edge for a receiver expression.
///
/// When the receiver type is known, dispatches to the type's method
/// namespace. When unknown (an unresolved inference variable), conservatively
/// emits an edge to every known `*.{method}` function so DCE never drops a
/// potential target.
fn resolve_methods(tp: &TypedProgram, recv: &Expr, method: &str) -> Vec<String> {
    match tp.type_at(recv.span()) {
        Some(Type::Str) => vec![format!("str.{method}")],
        Some(Type::Array(_)) => vec![format!("vec.{method}")],
        Some(Type::Option(_)) => vec![format!("option.{method}")],
        Some(Type::Result(_, _)) => vec![format!("result.{method}")],
        Some(Type::Response) => vec![format!("http.{method}")],
        Some(Type::TcpStream) | Some(Type::TcpListener) => vec![format!("net.{method}")],
        Some(Type::HttpServer) => vec![format!("http.{method}")],
        Some(Type::Json) => vec![format!("json.{method}")],
        Some(Type::Struct(s)) => {
            let fq = format!("{s}.{method}");
            if tp.funcs.contains_key(&fq) {
                vec![fq]
            } else {
                vec![method.to_string()]
            }
        }
        // Unknown/Var receiver: keep every candidate method conservatively.
        _ => {
            let mut out: Vec<String> = tp
                .funcs
                .keys()
                .filter(|k| k.ends_with(&format!(".{method}")))
                .cloned()
                .collect();
            if out.is_empty() {
                out.push(method.to_string());
            }
            out
        }
    }
}

/// The call graph: edges from each defined function (or [`TOP`]) to every
/// resolved callee name, plus struct instantiations and function-as-value
/// uses (escaping closures).
#[derive(Debug, Clone, Default)]
pub struct CallGraph {
    /// caller → resolved callee names.
    pub edges: HashMap<String, Vec<String>>,
    /// caller → struct names constructed (`Point{..}`).
    pub struct_uses: HashMap<String, Vec<String>>,
    /// caller → function names used as values (first-class funcs).
    pub value_uses: HashMap<String, Vec<String>>,
    /// All function/method names defined by this program (incl. stdlib
    /// seeded into `tp.funcs`).
    pub defined: HashSet<String>,
    /// Only the function/method/struct names declared in the AST itself
    /// (excludes seeded stdlib). Used to separate natives from user code.
    pub program_defined: HashSet<String>,
    /// Struct names defined by this program.
    pub defined_structs: HashSet<String>,
}

impl CallGraph {
    fn edge(&mut self, from: &str, to: &str) {
        self.edges
            .entry(from.to_string())
            .or_default()
            .push(to.to_string());
    }
}

/// A reachable set after DCE: only the transitively-reachable program items
/// and stdlib natives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachableSet {
    /// Reachable function/method names (program-defined + stdlib).
    pub funcs: HashSet<String>,
    /// Reachable struct names.
    pub structs: HashSet<String>,
    /// Reachable stdlib native names (`stdio.io.println`-style qualified).
    pub natives: HashSet<String>,
}

/// Build the call graph for a typed program.
pub fn build_callgraph(tp: &TypedProgram) -> CallGraph {
    let mut cg = CallGraph::default();

    // Program-defined names come from the AST (Func/Impl/Struct statements),
    // so seeded stdlib funcs are NOT treated as program-defined. This lets
    // `reachable.natives` separate stdlib deps from user code.
    let mut program_defined: HashSet<String> = HashSet::new();
    for stmt in tp.stmts() {
        match stmt {
            Stmt::Func { name, .. } => {
                program_defined.insert(name.join("."));
            }
            Stmt::Impl { name, methods, .. } => {
                let tname = name.join(".");
                for m in methods {
                    if let Stmt::Func { name: mname, .. } = m {
                        program_defined.insert(format!("{tname}.{}", mname.join(".")));
                    }
                }
            }
            Stmt::Struct { name, .. } => {
                program_defined.insert(name.join("."));
            }
            _ => {}
        }
    }

    // Collect defined names.
    for name in tp.funcs.keys() {
        cg.defined.insert(name.clone());
    }
    for name in tp.structs.keys() {
        cg.defined_structs.insert(name.clone());
    }
    cg.program_defined = program_defined;

    // Walk all top-level statements, with TOP as the initial caller.
    for stmt in tp.stmts() {
        walk_stmt_for_graph(tp, stmt, TOP, &mut cg);
    }
    cg
}

/// Recursively collect call/value/struct edges from a statement.
fn walk_stmt_for_graph(tp: &TypedProgram, stmt: &Stmt, caller: &str, cg: &mut CallGraph) {
    match stmt {
        Stmt::Func { name, body, .. } => {
            let fname = name.join(".");
            for s in &body.stmts {
                walk_stmt_for_graph(tp, s, &fname, cg);
            }
        }
        Stmt::Impl { name, methods, .. } => {
            let tname = name.join(".");
            for m in methods {
                if let Stmt::Func {
                    name: mname, body, ..
                } = m
                {
                    let fname = format!("{tname}.{}", mname.join("."));
                    for s in &body.stmts {
                        walk_stmt_for_graph(tp, s, &fname, cg);
                    }
                }
            }
        }
        Stmt::Decl { value, .. } => walk_expr_for_graph(tp, value, caller, cg),
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                walk_expr_for_graph(tp, v, caller, cg);
            }
        }
        Stmt::For { iter, body, .. } => {
            walk_expr_for_graph(tp, iter, caller, cg);
            for s in &body.stmts {
                walk_stmt_for_graph(tp, s, caller, cg);
            }
        }
        Stmt::Defer { expr, .. } => walk_expr_for_graph(tp, expr, caller, cg),
        Stmt::Assign { target, value, .. } => {
            walk_expr_for_graph(tp, target, caller, cg);
            walk_expr_for_graph(tp, value, caller, cg);
        }
        Stmt::Destructure { value, .. } => walk_expr_for_graph(tp, value, caller, cg),
        Stmt::Expr(e) => walk_expr_for_graph(tp, e, caller, cg),
        Stmt::Struct { .. } | Stmt::Import { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

/// Collect call/value/struct edges from an expression, descending into
/// closures/blocks with the same caller (closures are attributed to the
/// enclosing function).
fn walk_expr_for_graph(tp: &TypedProgram, e: &Expr, caller: &str, cg: &mut CallGraph) {
    match e {
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            // Resolve the callee.
            match callee.as_ref() {
                Expr::Ident { name, .. } => {
                    if tp.funcs.contains_key(name) || tp.bindings.contains_key(name) {
                        cg.edge(caller, name);
                    }
                }
                Expr::Path { parts, .. } => {
                    let joined = parts.join(".");
                    if tp.funcs.contains_key(&joined) || tp.bindings.contains_key(&joined) {
                        cg.edge(caller, &joined);
                    } else if parts.len() >= 2 {
                        // `s.len()` parses as a path call when the receiver
                        // is a plain identifier. If the first part isn't a
                        // known module/function, treat the tail as a method
                        // dispatch and keep every candidate conservatively.
                        let method = parts.last().unwrap();
                        let mut candidates: Vec<String> = tp
                            .funcs
                            .keys()
                            .filter(|k| k.ends_with(&format!(".{method}")) || *k == method.as_str())
                            .cloned()
                            .collect();
                        if candidates.is_empty() {
                            candidates.push(method.clone());
                        }
                        for c in candidates {
                            cg.edge(caller, &c);
                        }
                    }
                }
                Expr::Field { obj, name, .. } => {
                    for m in resolve_methods(tp, obj, name) {
                        cg.edge(caller, &m);
                    }
                    // Also descend into the object expression (it may hold
                    // a func used as a value / further calls).
                    walk_expr_for_graph(tp, obj, caller, cg);
                }
                _ => {}
            }
            for a in args {
                walk_expr_for_graph(tp, a, caller, cg);
            }
            for (_, a) in named {
                walk_expr_for_graph(tp, a, caller, cg);
            }
        }
        Expr::StructInit { name, fields, .. } => {
            cg.struct_uses
                .entry(caller.to_string())
                .or_default()
                .push(name.clone());
            for (_, v) in fields {
                walk_expr_for_graph(tp, v, caller, cg);
            }
        }
        Expr::Closure { body, .. } => walk_expr_for_graph(tp, body, caller, cg),
        Expr::If {
            cond, then, els, ..
        } => {
            walk_expr_for_graph(tp, cond, caller, cg);
            for s in &then.stmts {
                walk_stmt_for_graph(tp, s, caller, cg);
            }
            if let Some(el) = els {
                walk_expr_for_graph(tp, el, caller, cg);
            }
        }
        Expr::While { cond, body, .. } => {
            walk_expr_for_graph(tp, cond, caller, cg);
            for s in &body.stmts {
                walk_stmt_for_graph(tp, s, caller, cg);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr_for_graph(tp, scrutinee, caller, cg);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr_for_graph(tp, g, caller, cg);
                }
                walk_expr_for_graph(tp, &arm.body, caller, cg);
            }
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            walk_expr_for_graph(tp, value, caller, cg);
            for s in &then.stmts {
                walk_stmt_for_graph(tp, s, caller, cg);
            }
            if let Some(el) = els {
                walk_expr_for_graph(tp, el, caller, cg);
            }
        }
        Expr::Try { expr, .. } => walk_expr_for_graph(tp, expr, caller, cg),
        Expr::Block(b) => {
            for s in &b.stmts {
                walk_stmt_for_graph(tp, s, caller, cg);
            }
        }
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                walk_expr_for_graph(tp, a, caller, cg);
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_frontend::ast::FmtPart::Expr(inner, _) = p {
                    walk_expr_for_graph(tp, inner, caller, cg);
                }
            }
        }
        Expr::Paren { expr, .. } => walk_expr_for_graph(tp, expr, caller, cg),
        Expr::Tuple { items, .. } => {
            for it in items {
                walk_expr_for_graph(tp, it, caller, cg);
            }
        }
        Expr::Unary { expr, .. } => walk_expr_for_graph(tp, expr, caller, cg),
        Expr::Binary { left, right, .. } => {
            walk_expr_for_graph(tp, left, caller, cg);
            walk_expr_for_graph(tp, right, caller, cg);
        }
        Expr::Array { elems, .. } => {
            for el in elems {
                walk_expr_for_graph(tp, el, caller, cg);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                walk_expr_for_graph(tp, k, caller, cg);
                walk_expr_for_graph(tp, v, caller, cg);
            }
        }
        Expr::Field { obj, .. } => walk_expr_for_graph(tp, obj, caller, cg),
        Expr::Range { start, end, .. } => {
            walk_expr_for_graph(tp, start, caller, cg);
            walk_expr_for_graph(tp, end, caller, cg);
        }
        Expr::Index { obj, index, .. } => {
            walk_expr_for_graph(tp, obj, caller, cg);
            walk_expr_for_graph(tp, index, caller, cg);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            walk_expr_for_graph(tp, obj, caller, cg);
            if let Some(s) = start {
                walk_expr_for_graph(tp, s, caller, cg);
            }
            if let Some(e) = end {
                walk_expr_for_graph(tp, e, caller, cg);
            }
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            walk_expr_for_graph(tp, iter, caller, cg);
            if let Some(flt) = filter {
                walk_expr_for_graph(tp, flt, caller, cg);
            }
            walk_expr_for_graph(tp, body, caller, cg);
        }
        Expr::Ident { name, .. } => {
            // A function used as a value (first-class): if it names a known
            // function, mark it reachable conservatively.
            if tp.funcs.contains_key(name) {
                cg.value_uses
                    .entry(caller.to_string())
                    .or_default()
                    .push(name.clone());
            }
        }
        Expr::Path { parts, .. } => {
            let joined = parts.join(".");
            if tp.funcs.contains_key(&joined) {
                cg.value_uses
                    .entry(caller.to_string())
                    .or_default()
                    .push(joined);
            }
        }
        Expr::Int { .. } | Expr::Float { .. } | Expr::Str { .. } | Expr::Bool { .. } => {}
    }
}

/// Compute the reachable set from the given roots.
///
/// `roots` are function names (or [`TOP`]) that are always executed. The
/// engine follows call edges, method dispatches, function-as-value uses, and
/// struct instantiations, transitively.
pub fn reachable_from(_tp: &TypedProgram, cg: &CallGraph, roots: &[&str]) -> ReachableSet {
    let mut reach = ReachableSet::default();
    let mut queue: VecDeque<String> = VecDeque::new();

    for r in roots {
        if !reach.funcs.contains(*r) {
            reach.funcs.insert(r.to_string());
            queue.push_back(r.to_string());
        }
    }

    while let Some(caller) = queue.pop_front() {
        if let Some(callees) = cg.edges.get(&caller) {
            for c in callees {
                if reach.funcs.insert(c.clone()) {
                    queue.push_back(c.clone());
                }
            }
        }
        if let Some(vals) = cg.value_uses.get(&caller) {
            for v in vals {
                if reach.funcs.insert(v.clone()) {
                    queue.push_back(v.clone());
                }
            }
        }
        if let Some(structs) = cg.struct_uses.get(&caller) {
            for s in structs {
                reach.structs.insert(s.clone());
            }
        }
    }

    // stdlib natives = reachable funcs that were seeded (not defined by the
    // program). Everything in reach.funcs that isn't defined in the AST is
    // an external/native dependency.
    for f in &reach.funcs {
        if !cg.program_defined.contains(f) {
            reach.natives.insert(f.clone());
        }
    }

    reach
}

/// Compute reachability for a standalone binary: the entry `main` (whatever
/// its namespace) plus all top-level statements (which execute at startup).
pub fn reachable(tp: &TypedProgram, entry_main: &str) -> ReachableSet {
    let cg = build_callgraph(tp);
    let mut roots: Vec<&str> = vec![TOP];
    if tp.bindings.contains_key(entry_main) || tp.funcs.contains_key(entry_main) {
        roots.push(entry_main);
    }
    reachable_from(tp, &cg, &roots)
}

/// Prune the program to only reachable items.
///
/// Drops: unused `func` statements, uninstantiated `struct` definitions,
/// `impl` blocks whose methods are all unreachable, and `import` statements
/// whose namespace contributes no reachable function.
pub fn prune_program(tp: &TypedProgram, reach: &ReachableSet) -> TypedProgram {
    let mut stmts = Vec::new();
    for stmt in tp.stmts() {
        match stmt {
            Stmt::Func { name, .. } => {
                let fname = name.join(".");
                if reach.funcs.contains(&fname) {
                    stmts.push(stmt.clone());
                }
            }
            Stmt::Struct { name, .. } => {
                let sname = name.join(".");
                if reach.structs.contains(&sname) {
                    stmts.push(stmt.clone());
                }
            }
            Stmt::Impl { name, methods, .. } => {
                let tname = name.join(".");
                let keep: Vec<Stmt> = methods
                    .iter()
                    .filter(|m| {
                        if let Stmt::Func { name: mname, .. } = m {
                            let full = format!("{tname}.{}", mname.join("."));
                            reach.funcs.contains(&full)
                        } else {
                            false
                        }
                    })
                    .cloned()
                    .collect();
                if !keep.is_empty() {
                    stmts.push(Stmt::Impl {
                        name: name.clone(),
                        methods: keep,
                        span: stmt.span(),
                        pub_: false,
                    });
                }
            }
            Stmt::Import { path, alias, .. } => {
                // Keep the import only if any reachable func lives under the
                // imported namespace.
                let ns = alias
                    .clone()
                    .unwrap_or_else(|| path.last().cloned().unwrap_or_default());
                let prefix = format!("{ns}.");
                if reach.funcs.iter().any(|f| f.starts_with(&prefix)) {
                    stmts.push(stmt.clone());
                }
            }
            // Top-level statements execute at startup; keep all of them.
            other => stmts.push(other.clone()),
        }
    }
    TypedProgram {
        program: zz_frontend::ast::Program {
            stmts,
            span: tp.program.span,
        },
        types: tp.types.clone(),
        bindings: tp.bindings.clone(),
        funcs: tp.funcs.clone(),
        structs: tp.structs.clone(),
    }
}

/// Convenience: build call graph, compute reachability from `main`, and
/// prune the program — all in one shot.
pub fn dce(tp: &TypedProgram, entry_main: &str) -> (TypedProgram, ReachableSet) {
    let cg = build_callgraph(tp);
    let mut roots: Vec<&str> = vec![TOP];
    if tp.bindings.contains_key(entry_main) || tp.funcs.contains_key(entry_main) {
        roots.push(entry_main);
    }
    let reach = reachable_from(tp, &cg, &roots);
    let pruned = prune_program(tp, &reach);
    (pruned, reach)
}
