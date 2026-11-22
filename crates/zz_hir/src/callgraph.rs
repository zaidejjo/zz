//! Call-graph construction and dead-code elimination.
//!
//! The analyzer works over a [`TypedProgram`]:
//! - builds the call graph (caller → callees, including method dispatch and
//!   stdlib natives) plus struct-instantiation and function-as-value edges,
//! - computes the reachable closure from entry points (`main` + all
//!   top-level statements),
//! - and prunes the program to only the reachable items.
#![allow(clippy::collapsible_match, clippy::single_match)]

use std::collections::{HashMap, HashSet, VecDeque};

use zz_checker::Type;
use zz_diag_core::ErrorCode;
use zz_frontend::ast::{Expr, Stmt};
use zz_span_ast::span::Span;

use crate::TypedProgram;

/// The pseudo-caller owning all top-level (non-function) statements.
pub const TOP: &str = "<top>";

/// A resolved method-dispatch edge for a receiver expression.
///
/// When the receiver type is known, dispatches to the type's method
/// namespace. When unknown (an unresolved inference variable), conservatively
/// emits an edge to every known `*.{method}` function so DCE never drops a
/// potential target.
/// Canonicalize a constructed `ns.method` name to its `std.*` seed.
/// Seeds hold canonical names only; orphans (`option.unwrap`) and user
/// namespaces pass through unchanged.
fn canon(name: String) -> String {
    zz_checker::canonical_std_name(&name).unwrap_or(name)
}

fn resolve_methods(tp: &TypedProgram, caller: &str, recv: &Expr, method: &str) -> Vec<String> {
    match tp.type_at(caller, recv.span()) {
        Some(Type::Str) => vec![canon(format!("str.{method}"))],
        Some(Type::Array(_)) => vec![canon(format!("vec.{method}"))],
        Some(Type::Bytes) => vec![canon(format!("bytes.{method}"))],
        Some(Type::Option(_)) => vec![canon(format!("option.{method}"))],
        Some(Type::Result(_, _)) => vec![canon(format!("result.{method}"))],
        Some(Type::Response) => vec![canon(format!("http.{method}"))],
        Some(Type::HttpRequest) => vec![canon(format!("http.{method}"))],
        Some(Type::TcpStream) | Some(Type::TcpListener) => vec![canon(format!("net.{method}"))],
        Some(Type::HttpServer) => vec![canon(format!("http.{method}"))],
        Some(Type::Db) => vec![canon(format!("sqlz.{method}"))],
        Some(Type::Json) => vec![canon(format!("json.{method}"))],
        // Opaque handles dispatch on their module tag (`regex.*`, …).
        Some(Type::Opaque(tag)) => vec![canon(format!("{tag}.{method}"))],
        Some(Type::Struct(s, _)) => {
            let fq = format!("{s}.{method}");
            if tp.funcs.contains_key(&fq) {
                vec![fq]
            } else if let Some(promoted) = find_promoted_method(tp, s, method) {
                // Embedded promotion: `u.area()` on `User` (embedding
                // `Base`) dispatches to `Base.area`.
                vec![promoted]
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

/// True when a struct field is an embedded (anonymous) field: its type is
/// a struct whose last name segment equals the field name. Mirrors the
/// checker's rule (see `zz_checker::checker::structs`).
fn is_embedded_field(fname: &str, fty: &Type) -> bool {
    matches!(fty, Type::Struct(s, _) if s.rsplit('.').next().unwrap_or(s) == fname)
}

/// Search structs embedded in `root` (transitively, breadth-first) for a
/// definition of `{inner}.{method}`. Returns the defining method name.
fn find_promoted_method(tp: &TypedProgram, root: &str, method: &str) -> Option<String> {
    let mut visited = vec![root.to_string()];
    let mut queue = vec![root.to_string()];
    while let Some(cur) = queue.first().cloned() {
        queue.remove(0);
        let sig = tp.structs.get(&cur)?;
        for (fname, fty) in &sig.fields {
            if let Type::Struct(inner, _) = fty {
                if is_embedded_field(fname, fty) && !visited.contains(inner) {
                    let fq = format!("{inner}.{method}");
                    if tp.funcs.contains_key(&fq) {
                        return Some(fq);
                    }
                    visited.push(inner.clone());
                    queue.push(inner.clone());
                }
            }
        }
    }
    None
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
    /// Import alias head → dotted module path (`fspath` → `std.path`,
    /// from `import std.path as fspath`). Used to edge qualified alias
    /// calls (`fspath.join`) to their canonical definitions, which is
    /// where function bodies actually live.
    pub import_aliases: HashMap<String, String>,
    /// All function/method names defined by this program (incl. stdlib
    /// seeded into `tp.funcs`).
    pub defined: HashSet<String>,
    /// Only the function/method/struct names declared in the AST itself
    /// (excludes seeded stdlib). Used to separate natives from user code.
    pub program_defined: HashSet<String>,
    /// Struct names defined by this program.
    pub defined_structs: HashSet<String>,
    /// All functions whose value is taken anywhere in the program
    /// (first-class references: `g := helper`, `arr = [helper]`, `f(helper)`,
    /// struct field `s.f = helper`, etc.). Used to conservatively edge
    /// unresolved indirect calls so DCE never prunes a function that might
    /// be called through a function value.
    pub address_taken: HashSet<String>,
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
    /// Reachable stdlib native names (`std.str.length`-style qualified).
    pub natives: HashSet<String>,
}

/// Canonical targets for a call through an import alias head
/// (`fspath.join` with `fspath` → `std.path`): the full dotted form plus
/// the `std.`-stripped form, filtered to names that actually define a
/// body. Bodies live under canonical names (seed copies under the alias
/// have none), so edging these retains the real definition. Filtering
/// to definitions (not just signatures) matters: edging a signature-only
/// name would misclassify it as a native downstream.
fn alias_canonical_targets(cg: &CallGraph, parts: &[String]) -> Vec<String> {
    if parts.len() < 2 {
        return Vec::new();
    }
    let Some(head) = cg.import_aliases.get(&parts[0]) else {
        return Vec::new();
    };
    let method = &parts[parts.len() - 1];
    let mut cands = vec![format!("{head}.{method}")];
    if let Some(stripped) = head.strip_prefix("std.") {
        cands.push(format!("{stripped}.{method}"));
    }
    cands
        .into_iter()
        .filter(|c| cg.program_defined.contains(c))
        .collect()
}

/// Collect all functions whose value is taken anywhere in the program
/// (first-class references). Used to conservatively edge unresolved
/// indirect calls so DCE never prunes a function that might be called
/// through a function value.
fn collect_address_taken(tp: &TypedProgram) -> HashSet<String> {
    let mut taken = HashSet::new();
    for stmt in tp.stmts() {
        collect_address_taken_in_stmt(tp, stmt, &mut taken);
    }
    taken
}

fn collect_address_taken_in_stmt(tp: &TypedProgram, stmt: &Stmt, taken: &mut HashSet<String>) {
    match stmt {
        Stmt::Func { body, .. } => {
            for s in &body.stmts {
                collect_address_taken_in_stmt(tp, s, taken);
            }
        }
        Stmt::Impl { methods, .. } => {
            for m in methods {
                if let Stmt::Func { body, .. } = m {
                    for s in &body.stmts {
                        collect_address_taken_in_stmt(tp, s, taken);
                    }
                }
            }
        }
        Stmt::Decl { value, .. } => {
            collect_address_taken_in_expr(tp, value, taken);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                collect_address_taken_in_expr(tp, v, taken);
            }
        }
        Stmt::For { iter, body, .. } => {
            collect_address_taken_in_expr(tp, iter, taken);
            for s in &body.stmts {
                collect_address_taken_in_stmt(tp, s, taken);
            }
        }
        Stmt::Defer { expr, .. } => {
            collect_address_taken_in_expr(tp, expr, taken);
        }
        Stmt::Atomic { body, .. } => {
            for s in &body.stmts {
                collect_address_taken_in_stmt(tp, s, taken);
            }
        }
        Stmt::Assign { target, value, .. } => {
            collect_address_taken_in_expr(tp, target, taken);
            collect_address_taken_in_expr(tp, value, taken);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            collect_address_taken_in_expr(tp, target, taken);
            collect_address_taken_in_expr(tp, value, taken);
        }
        Stmt::Destructure { value, .. } => {
            collect_address_taken_in_expr(tp, value, taken);
        }
        Stmt::Expr(e) => {
            collect_address_taken_in_expr(tp, e, taken);
        }
        _ => {}
    }
}

fn collect_address_taken_in_block(
    tp: &TypedProgram,
    b: &zz_span_ast::ast::Block,
    taken: &mut HashSet<String>,
) {
    for s in &b.stmts {
        collect_address_taken_in_stmt(tp, s, taken);
    }
}

fn collect_address_taken_in_expr(tp: &TypedProgram, e: &Expr, taken: &mut HashSet<String>) {
    match e {
        Expr::Ident { name, .. } => {
            if tp.funcs.contains_key(name) {
                taken.insert(name.clone());
            }
        }
        Expr::Path { parts, .. } => {
            let joined = parts.join(".");
            if tp.funcs.contains_key(&joined) {
                taken.insert(joined);
            }
        }
        Expr::Closure { body, .. } => {
            // Closures are anonymous; walk the body for address-taken refs.
            collect_address_taken_in_expr(tp, body, taken);
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_address_taken_in_expr(tp, callee, taken);
            for a in args {
                collect_address_taken_in_expr(tp, a, taken);
            }
            for (_, a) in named {
                collect_address_taken_in_expr(tp, a, taken);
            }
        }
        Expr::Block(b) => {
            for s in &b.stmts {
                collect_address_taken_in_stmt(tp, s, taken);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_address_taken_in_expr(tp, cond, taken);
            collect_address_taken_in_block(tp, then, taken);
            if let Some(el) = els {
                collect_address_taken_in_expr(tp, el, taken);
            }
        }
        Expr::While { cond, body, .. } => {
            collect_address_taken_in_expr(tp, cond, taken);
            collect_address_taken_in_block(tp, body, taken);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_address_taken_in_expr(tp, scrutinee, taken);
            for a in arms {
                if let Some(g) = &a.guard {
                    collect_address_taken_in_expr(tp, g, taken);
                }
                collect_address_taken_in_expr(tp, &a.body, taken);
            }
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            collect_address_taken_in_expr(tp, value, taken);
            collect_address_taken_in_block(tp, then, taken);
            if let Some(el) = els {
                collect_address_taken_in_expr(tp, el, taken);
            }
        }
        Expr::Try { expr, .. } => {
            collect_address_taken_in_expr(tp, expr, taken);
        }
        Expr::Array { elems, .. } => {
            for el in elems {
                collect_address_taken_in_expr(tp, el, taken);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_address_taken_in_expr(tp, k, taken);
                collect_address_taken_in_expr(tp, v, taken);
            }
        }
        Expr::Field { obj, .. } => {
            collect_address_taken_in_expr(tp, obj, taken);
        }
        Expr::Range { start, end, .. } => {
            collect_address_taken_in_expr(tp, start, taken);
            collect_address_taken_in_expr(tp, end, taken);
        }
        Expr::Index { obj, index, .. } => {
            collect_address_taken_in_expr(tp, obj, taken);
            collect_address_taken_in_expr(tp, index, taken);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_address_taken_in_expr(tp, obj, taken);
            if let Some(s) = start {
                collect_address_taken_in_expr(tp, s, taken);
            }
            if let Some(e) = end {
                collect_address_taken_in_expr(tp, e, taken);
            }
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_address_taken_in_expr(tp, iter, taken);
            if let Some(flt) = filter {
                collect_address_taken_in_expr(tp, flt, taken);
            }
            collect_address_taken_in_expr(tp, body, taken);
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_address_taken_in_expr(tp, v, taken);
            }
        }
        Expr::Paren { expr, .. } => {
            collect_address_taken_in_expr(tp, expr, taken);
        }
        Expr::Unary { expr, .. } => {
            collect_address_taken_in_expr(tp, expr, taken);
        }
        Expr::Binary { left, right, .. } => {
            collect_address_taken_in_expr(tp, left, taken);
            collect_address_taken_in_expr(tp, right, taken);
        }
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                collect_address_taken_in_expr(tp, a, taken);
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_frontend::ast::FmtPart::Expr(inner, _) = p {
                    collect_address_taken_in_expr(tp, inner, taken);
                }
            }
        }
        Expr::Is { expr, .. } => {
            collect_address_taken_in_expr(tp, expr, taken);
        }
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. }
        | Expr::Tuple { .. } => {}
    }
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
            // Enum type names count as program-defined (like structs);
            // variant construction references them.
            Stmt::Enum { name, .. } => {
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

    // Collect the address-taken set (all functions whose value is taken
    // anywhere as a first-class value) before walking edges, so unresolved
    // indirect calls can edge to the complete set.
    cg.address_taken = collect_address_taken(tp);

    // Import alias heads (`fspath` from `import std.path as fspath`):
    // qualified calls through them (`fspath.join`) resolve against
    // seed copies, but bodies live under canonical names — record the
    // mapping so call/value edges can also retain the canonical target.
    for stmt in tp.stmts() {
        if let Stmt::Import {
            path,
            alias: Some(a),
            ..
        } = stmt
        {
            cg.import_aliases
                .entry(a.clone())
                .or_insert_with(|| path.join("."));
        }
    }

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
        // Trait declarations carry no bodies: nothing to walk.
        Stmt::Trait { .. } => {}
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
        Stmt::Atomic { body, .. } => {
            for s in &body.stmts {
                walk_stmt_for_graph(tp, s, caller, cg);
            }
        }
        Stmt::Assign { target, value, .. } => {
            walk_expr_for_graph(tp, target, caller, cg);
            walk_expr_for_graph(tp, value, caller, cg);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            walk_expr_for_graph(tp, target, caller, cg);
            walk_expr_for_graph(tp, value, caller, cg);
        }
        Stmt::Destructure { value, .. } => walk_expr_for_graph(tp, value, caller, cg),
        Stmt::Expr(e) => walk_expr_for_graph(tp, e, caller, cg),
        Stmt::Struct { .. }
        | Stmt::TypeAlias { .. }
        | Stmt::Enum { .. }
        | Stmt::Import { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::ExternBlock { .. }
        | Stmt::Link { .. } => {}
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
            // Resolve the callee. Statically-resolved callees (known funcs /
            // paths / method dispatch) get a precise edge. Anything else — a
            // local first-class func value, a direct closure invocation, or
            // any other unresolved callee — conservatively edges to EVERY
            // function whose value is taken anywhere in the program
            // (`cg.address_taken`), so DCE never prunes a reachable target.
            let mut resolved = false;
            match callee.as_ref() {
                Expr::Ident { name, .. } => {
                    if tp.funcs.contains_key(name) || tp.bindings.contains_key(name) {
                        cg.edge(caller, name);
                        resolved = true;
                    }
                }
                Expr::Path { parts, .. } => {
                    let joined = parts.join(".");
                    if tp.funcs.contains_key(&joined) || tp.bindings.contains_key(&joined) {
                        cg.edge(caller, &joined);
                        resolved = true;
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
                        let mut any_known = false;
                        for c in &candidates {
                            if tp.funcs.contains_key(c) || tp.bindings.contains_key(c) {
                                cg.edge(caller, c);
                                any_known = true;
                            }
                        }
                        // Only mark resolved if we found a known target;
                        // otherwise the call is through an unknown receiver
                        // or a func-typed field/path, and we fall through
                        // to the address-taken conservative edge below.
                        if any_known {
                            resolved = true;
                        }
                    }
                    // Qualified alias calls (`fspath.join`) edge only the
                    // alias spelling above, whose seed copy has no body —
                    // also edge the canonical definition so it survives.
                    for c in alias_canonical_targets(cg, parts) {
                        cg.edge(caller, &c);
                    }
                }
                Expr::Field { obj, name, .. } => {
                    let methods = resolve_methods(tp, caller, obj, name);
                    let mut any_known = false;
                    for m in &methods {
                        if tp.funcs.contains_key(m) || tp.bindings.contains_key(m) {
                            cg.edge(caller, m);
                            any_known = true;
                        }
                    }
                    // If no resolved method is a known function, the field
                    // access may be calling a func-typed field value — treat
                    // as unresolved indirect call below.
                    resolved = any_known;
                    // Also descend into the object expression (it may hold
                    // a func used as a value / further calls).
                    walk_expr_for_graph(tp, obj, caller, cg);
                }
                Expr::Closure { body, .. } => {
                    // Direct closure invocation `{|x| x + 1}(5)`: walk the
                    // body with the enclosing caller so any functions called
                    // inside the closure body are retained by DCE.
                    walk_expr_for_graph(tp, body, caller, cg);
                    resolved = true;
                }
                _ => {}
            }
            if !resolved {
                // Unresolved indirect call: callee is a local first-class
                // func value or otherwise not statically known. Conservatively
                // edge to every address-taken function so DCE cannot prune a
                // reachable target. Clone first to avoid borrow conflict.
                let taken: Vec<String> = cg.address_taken.iter().cloned().collect();
                for t in taken {
                    cg.edge(caller, &t);
                }
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
        Expr::Is { expr, .. } => walk_expr_for_graph(tp, expr, caller, cg),
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
            // function, mark it reachable conservatively. Bare selective
            // names (`join` for `path.join`) additionally retain their
            // canonical definitions, which is where bodies live.
            if tp.funcs.contains_key(name) {
                cg.value_uses
                    .entry(caller.to_string())
                    .or_default()
                    .push(name.clone());
                if !name.contains('.') && !cg.program_defined.contains(name) {
                    let suffix = format!(".{name}");
                    let mut hits: Vec<String> = cg
                        .program_defined
                        .iter()
                        .filter(|pd| pd.ends_with(suffix.as_str()))
                        .cloned()
                        .collect();
                    hits.sort();
                    for h in hits {
                        cg.value_uses.entry(caller.to_string()).or_default().push(h);
                    }
                }
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
            // Same canonical retention as calls above (the use site
            // itself still lowers through the value path).
            for c in alias_canonical_targets(cg, parts) {
                cg.value_uses.entry(caller.to_string()).or_default().push(c);
            }
        }
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
    }
}

/// Compute the reachable set from the given roots.
///
/// `roots` are function names (or [`TOP`]) that are always executed. The
/// engine follows call edges, method dispatches, function-as-value uses, and
/// struct instantiations, transitively.
pub fn reachable_from(tp: &TypedProgram, cg: &CallGraph, roots: &[&str]) -> ReachableSet {
    let mut reach = ReachableSet::default();
    let mut queue: VecDeque<String> = VecDeque::new();

    for r in roots {
        if !reach.funcs.contains(*r) {
            reach.funcs.insert(r.to_string());
            queue.push_back(r.to_string());
        }
    }

    // Selective-import bare aliases (`double` for `math_utils.double`)
    // edge only the bare name; the canonical body would otherwise be
    // pruned while the bare alias is misclassified as a native below.
    // Resolve bare names to their program-defined canonicals here so
    // both stay reachable.
    let mut alias_extra: Vec<String> = Vec::new();
    while let Some(caller) = queue.pop_front() {
        if let Some(callees) = cg.edges.get(&caller) {
            for c in callees {
                if reach.funcs.insert(c.clone()) {
                    queue.push_back(c.clone());
                }
                // Bare alias → queue canonical program-defined targets.
                if !c.contains('.') && !cg.program_defined.contains(c) {
                    let suffix = format!(".{c}");
                    for pd in &cg.program_defined {
                        if pd.ends_with(suffix.as_str()) && !reach.funcs.contains(pd) {
                            alias_extra.push(pd.clone());
                        }
                    }
                }
                // Canonical → queue short program-defined targets.
                // Pure-ZZ bodies are declared short (`path.join`) while
                // callers may edge the canonical spelling (`std.path.join`
                // via selective imports or qualified calls). Without the
                // short edge, pruning drops the body while the call
                // still references it (dangling `zz_fn_*` in AOT).
                if let Some(stripped) = c.strip_prefix("std.") {
                    if cg.program_defined.contains(stripped) && !reach.funcs.contains(stripped) {
                        alias_extra.push(stripped.to_string());
                    }
                }
            }
        }
        if let Some(vals) = cg.value_uses.get(&caller) {
            for v in vals {
                if reach.funcs.insert(v.clone()) {
                    queue.push_back(v.clone());
                }
                // Same canonical→short mirror as calls above: a
                // first-class reference to the canonical spelling must
                // also retain the short program definition.
                if let Some(stripped) = v.strip_prefix("std.") {
                    if cg.program_defined.contains(stripped) && !reach.funcs.contains(stripped) {
                        alias_extra.push(stripped.to_string());
                    }
                }
            }
        }
        if let Some(structs) = cg.struct_uses.get(&caller) {
            for s in structs {
                reach.structs.insert(s.clone());
            }
        }
    }
    // Drain alias canonicals + their transitive callees.
    let mut extra_q: VecDeque<String> = alias_extra.into_iter().collect();
    while let Some(c) = extra_q.pop_front() {
        if !reach.funcs.insert(c.clone()) {
            continue;
        }
        if let Some(callees) = cg.edges.get(&c) {
            for n in callees {
                if !reach.funcs.contains(n) {
                    extra_q.push_back(n.clone());
                }
            }
        }
        if let Some(vals) = cg.value_uses.get(&c) {
            for v in vals {
                if !reach.funcs.contains(v) {
                    extra_q.push_back(v.clone());
                }
            }
        }
    }

    // stdlib natives = reachable funcs that were seeded (not defined by the
    // program). Binding-only values (top-level closure vars like
    // `function_types.transform`, in `tp.bindings` but not `tp.funcs`)
    // are values, not natives — they lower via `zz_call_closure`.
    // Bare selective aliases resolved above are likewise not natives.
    for f in &reach.funcs {
        if cg.program_defined.contains(f) {
            continue;
        }
        // Binding-only: never a native.
        if tp.bindings.contains_key(f) && !tp.funcs.contains_key(f) {
            continue;
        }
        // Bare alias of a program-defined canonical: not a native.
        if !f.contains('.') {
            let suffix = format!(".{f}");
            if cg
                .program_defined
                .iter()
                .any(|pd| pd.ends_with(suffix.as_str()))
            {
                continue;
            }
        }
        reach.natives.insert(f.clone());
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
    // Keep implicit `try` conversion edges alive here too (see `dce`).
    let converts: Vec<String> = tp.try_converts.values().filter_map(|o| o.clone()).collect();
    for c in &converts {
        if !roots.contains(&c.as_str()) {
            roots.push(c.as_str());
        }
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
            // Aliases erase at check time: no code to keep.
            Stmt::TypeAlias { .. } => {}
            // Enums register their variants at runtime (all engines read
            // the declaration to resolve `Enum.Variant` construction), so
            // they are always kept — declarations are cheap, one per type.
            Stmt::Enum { .. } => {
                stmts.push(stmt.clone());
            }
            Stmt::Impl {
                name,
                generics,
                methods,
                trait_name,
                ..
            } => {
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
                        generics: generics.clone(),
                        methods: keep,
                        span: stmt.span(),
                        pub_: false,
                        trait_name: trait_name.clone(),
                    });
                }
            }
            Stmt::Import {
                path, alias, items, ..
            } => {
                // Keep the import if any reachable func lives under the
                // imported namespace.
                let ns = alias
                    .clone()
                    .unwrap_or_else(|| path.last().cloned().unwrap_or_default());
                let prefix = format!("{ns}.");
                // Selective/wildcard imports (`import std.fs(read_to_string
                // as rts)`) are always kept: they carry alias info the AOT
                // backend needs to resolve bare names to canonical natives.
                // Namespace aliases (`import std.fs as f`) are likewise
                // always kept: reachability is computed on canonical names
                // (`fs.walk_dir`), which never match the alias prefix
                // (`f.`), so the prefix check below would wrongly drop
                // them and every `f.*` call would lower to unit.
                // They emit no code (the lowerer ignores Import statements).
                if !items.is_empty()
                    || alias.is_some()
                    || reach.funcs.iter().any(|f| f.starts_with(&prefix))
                {
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
        enums: tp.enums.clone(),
        // Trait tables are tiny (names + signatures) and codegen reads them
        // through the pruned program, so they pass through unchanged.
        traits: tp.traits.clone(),
        trait_impls: tp.trait_impls.clone(),
        try_converts: tp.try_converts.clone(),
        module_origins: tp.module_origins.clone(),
        symbol_map: tp.symbol_map.clone(),
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
    // `try` error conversions are implicit call edges (no `Call` node in
    // the AST): seed every conversion target as a root so DCE keeps the
    // `convert_to_` method and its transitive callees.
    let converts: Vec<String> = tp.try_converts.values().filter_map(|o| o.clone()).collect();
    for c in &converts {
        if !roots.contains(&c.as_str()) {
            roots.push(c.as_str());
        }
    }
    let reach = reachable_from(tp, &cg, &roots);
    let pruned = prune_program(tp, &reach);
    (pruned, reach)
}

// --- D5: R2 plain globals vs tasks --------------------------------------

/// A normal mutable top-level global accessed by code reachable from a
/// `task.spawn` closure is a compile error.
/// Runs as a whole-program post-pass on the merged `TypedProgram`.
pub fn check_r2_globals_vs_tasks(
    tp: &TypedProgram,
    cg: &CallGraph,
    errors: &mut Vec<zz_frontend::diag::RawDiag>,
) {
    // Find all plain mutable globals assigned somewhere in the program.
    let mut assigned_globals: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Span of each global's declaration (for diagnostics).
    let mut decl_spans: std::collections::HashMap<String, Span> = std::collections::HashMap::new();
    collect_assigned_globals(tp, &mut assigned_globals, &mut decl_spans);

    // Globals textually referenced inside spawn closures (direct task access).
    let mut spawn_globals: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_spawn_globals(tp, &mut spawn_globals);

    // Functions whose closures run in tasks: every top-level function whose
    // body directly contains a `task.spawn(...)` call. Because closure bodies
    // are walked with the enclosing function as `caller` when building the
    // call graph (`Expr::Closure => walk_expr_for_graph(..., caller, cg)`),
    // `cg.edges[enclosing]` already lists every function called inside its
    // spawn closures. Walking `cg.edges` transitively from these roots yields
    // every function whose code can execute inside a task.
    let mut task_roots: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    collect_spawn_enclosing_funcs(tp, &mut task_roots);

    // `task_roots` doubles as the BFS queue: seed it with the spawn-enclosing
    // functions, then drain it while discovering callees.
    let mut task_funcs: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(f) = task_roots.pop_front() {
        if !task_funcs.insert(f.clone()) {
            continue;
        }
        for callee in cg.edges.get(&f).cloned().unwrap_or_default() {
            if task_funcs.insert(callee.clone()) {
                task_roots.push_back(callee.clone());
            }
        }
    }

    // Globals accessed (read or written) by any task-reachable function,
    // plus those textually inside spawn closures. Deep-scan includes
    // closures/nested functions because all code in a task-reachable
    // function runs inside the task.
    let mut task_globals: std::collections::HashSet<String> = spawn_globals.clone();
    for f in &task_funcs {
        collect_globals_in_func(tp, f, &mut task_globals);
    }

    // Report errors for plain mutable globals reachable from a task.
    // Exception: a global never assigned after initialization (not in
    // `assigned_globals`) is effectively immutable and may be read from
    // tasks.
    for g in &task_globals {
        if assigned_globals.contains(g) && !is_state_or_const(tp, g, &assigned_globals) {
            let span = decl_spans.get(g).copied().unwrap_or_default();
            errors.push(
                zz_frontend::diag::error_at(
                    format!(
                        "global `{g}` is reachable from a task; declare it `state`\n\
                         hint: change `{g} := ...` to `state {g} := ...`",
                        g = g
                    ),
                    span,
                )
                .with_code(ErrorCode(17)),
            );
        }
    }
}

/// Collect the dotted names of top-level functions whose body directly
/// contains a `task.spawn(...)` call. Their closures run inside tasks, so
/// they are the BFS roots for task-reachability. Does not descend into
/// nested `fn` items (a nested function's spawn is that nested function's
/// own concern and is reached via the call graph if that nested function
/// itself is task-reachable).
fn collect_spawn_enclosing_funcs(tp: &TypedProgram, out: &mut std::collections::VecDeque<String>) {
    for stmt in &tp.program.stmts {
        if let zz_span_ast::ast::Stmt::Func { name, body, .. } = stmt {
            if block_contains_task_spawn(body) {
                out.push_back(name.join("."));
            }
        }
    }
}

/// True if `block` (or any nested block/loop/conditional/closure) directly
/// contains a `task.spawn(...)` call. Stops at nested `fn` items.
fn block_contains_task_spawn(block: &zz_span_ast::ast::Block) -> bool {
    for s in &block.stmts {
        if stmt_contains_task_spawn(s) {
            return true;
        }
    }
    false
}

fn stmt_contains_task_spawn(s: &zz_span_ast::ast::Stmt) -> bool {
    match s {
        zz_span_ast::ast::Stmt::Expr(e) => expr_contains_task_spawn(e),
        zz_span_ast::ast::Stmt::Decl { value, .. } => expr_contains_task_spawn(value),
        zz_span_ast::ast::Stmt::For { body, .. } => block_contains_task_spawn(body),
        zz_span_ast::ast::Stmt::Atomic { body, .. } => block_contains_task_spawn(body),
        // Control flow (`if`/`while`/`match`/`block`) arrives here wrapped
        // as `Stmt::Expr(Expr::If/While/Match/Block)` and is handled by the
        // expression recursion above.
        _ => false,
    }
}

fn expr_contains_task_spawn(e: &zz_span_ast::ast::Expr) -> bool {
    match e {
        zz_span_ast::ast::Expr::Call { callee, .. } => {
            callee_name(callee).as_deref() == Some("task.spawn")
        }
        zz_span_ast::ast::Expr::Block(b) => block_contains_task_spawn(b),
        zz_span_ast::ast::Expr::If {
            cond, then, els, ..
        } => {
            expr_contains_task_spawn(cond)
                || block_contains_task_spawn(then)
                || els.as_ref().is_some_and(|e| expr_contains_task_spawn(e))
        }
        zz_span_ast::ast::Expr::While { cond, body, .. } => {
            expr_contains_task_spawn(cond) || block_contains_task_spawn(body)
        }
        zz_span_ast::ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            expr_contains_task_spawn(scrutinee)
                || arms.iter().any(|a| expr_contains_task_spawn(&a.body))
        }
        zz_span_ast::ast::Expr::Closure { body, .. } => expr_contains_task_spawn(body),
        zz_span_ast::ast::Expr::Try { expr, .. } => expr_contains_task_spawn(expr),
        _ => false,
    }
}

/// Collect every global variable name read or written inside function
/// `dotted_name` (a `cg.edges` / `tp.funcs` key such as `ns.func` or `func`),
/// including code inside closures and nested functions, since all of it runs
/// inside the task when the function is task-reachable.
fn collect_globals_in_func(
    tp: &TypedProgram,
    dotted_name: &str,
    out: &mut std::collections::HashSet<String>,
) {
    for stmt in &tp.program.stmts {
        if let zz_span_ast::ast::Stmt::Func { name, body, .. } = stmt {
            if name.join(".") == dotted_name {
                collect_globals_from_block_deep(tp, body, out);
                return;
            }
        }
    }
}

/// Like [`collect_globals_from_block`] but also descends into closure and
/// nested-function bodies, because all code in a task-reachable function
/// executes inside the task.
fn collect_globals_from_block_deep(
    tp: &TypedProgram,
    b: &zz_span_ast::ast::Block,
    out: &mut std::collections::HashSet<String>,
) {
    for s in &b.stmts {
        collect_globals_from_stmt_deep(tp, s, out);
    }
}

fn collect_globals_from_stmt_deep(
    tp: &TypedProgram,
    s: &zz_span_ast::ast::Stmt,
    out: &mut std::collections::HashSet<String>,
) {
    match s {
        zz_span_ast::ast::Stmt::Expr(e) => collect_globals_from_expr_deep(tp, e, out),
        zz_span_ast::ast::Stmt::Decl { value, .. } => {
            collect_globals_from_expr_deep(tp, value, out);
        }
        zz_span_ast::ast::Stmt::Assign { target, value, .. } => {
            collect_globals_from_expr_deep(tp, target, out);
            collect_globals_from_expr_deep(tp, value, out);
        }
        zz_span_ast::ast::Stmt::CompoundAssign { target, value, .. } => {
            collect_globals_from_expr_deep(tp, target, out);
            collect_globals_from_expr_deep(tp, value, out);
        }
        zz_span_ast::ast::Stmt::Func { body, .. } => collect_globals_from_block_deep(tp, body, out),
        zz_span_ast::ast::Stmt::For { body, .. } => collect_globals_from_block_deep(tp, body, out),
        zz_span_ast::ast::Stmt::Atomic { body, .. } => {
            collect_globals_from_block_deep(tp, body, out)
        }
        zz_span_ast::ast::Stmt::Return { value, .. } => {
            if let Some(v) = value {
                collect_globals_from_expr_deep(tp, v, out);
            }
        }
        zz_span_ast::ast::Stmt::Defer { expr, .. } => {
            collect_globals_from_expr_deep(tp, expr, out);
        }
        zz_span_ast::ast::Stmt::Destructure { value, .. } => {
            collect_globals_from_expr_deep(tp, value, out);
        }
        // Break, Continue, Decl, Import, Struct, Trait, Impl, TypeAlias,
        // Enum, ExternBlock, Link: no global accesses.
        _ => {}
    }
}

fn collect_globals_from_expr_deep(
    tp: &TypedProgram,
    e: &zz_span_ast::ast::Expr,
    out: &mut std::collections::HashSet<String>,
) {
    match e {
        zz_span_ast::ast::Expr::Ident { name, .. } => {
            if tp.bindings.contains_key(name) {
                out.insert(name.clone());
            }
        }
        zz_span_ast::ast::Expr::Path { parts, .. } => {
            if let Some(bare) = binding_basename(tp, parts) {
                out.insert(bare);
            }
        }
        zz_span_ast::ast::Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_globals_from_expr_deep(tp, callee, out);
            for a in args {
                collect_globals_from_expr_deep(tp, a, out);
            }
            for (_, v) in named {
                collect_globals_from_expr_deep(tp, v, out);
            }
        }
        zz_span_ast::ast::Expr::Block(b) => collect_globals_from_block_deep(tp, b, out),
        zz_span_ast::ast::Expr::If {
            cond, then, els, ..
        } => {
            collect_globals_from_expr_deep(tp, cond, out);
            collect_globals_from_block_deep(tp, then, out);
            if let Some(e) = els {
                collect_globals_from_expr_deep(tp, e, out);
            }
        }
        zz_span_ast::ast::Expr::While { cond, body, .. } => {
            collect_globals_from_expr_deep(tp, cond, out);
            collect_globals_from_block_deep(tp, body, out);
        }
        zz_span_ast::ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_globals_from_expr_deep(tp, scrutinee, out);
            for a in arms {
                collect_globals_from_expr_deep(tp, &a.body, out);
            }
        }
        zz_span_ast::ast::Expr::Closure { body, .. } => {
            collect_globals_from_expr_deep(tp, body, out);
        }
        zz_span_ast::ast::Expr::Try { expr, .. } => collect_globals_from_expr_deep(tp, expr, out),
        zz_span_ast::ast::Expr::Array { elems, .. } => {
            for e in elems {
                collect_globals_from_expr_deep(tp, e, out);
            }
        }
        zz_span_ast::ast::Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_globals_from_expr_deep(tp, k, out);
                collect_globals_from_expr_deep(tp, v, out);
            }
        }
        zz_span_ast::ast::Expr::Paren { expr, .. } => collect_globals_from_expr_deep(tp, expr, out),
        zz_span_ast::ast::Expr::Tuple { items, .. } => {
            for e in items {
                collect_globals_from_expr_deep(tp, e, out);
            }
        }
        _ => {}
    }
}

/// Collect names of globals that are assigned somewhere in the program,
/// plus the span of each global's declaration.
fn collect_assigned_globals(
    tp: &TypedProgram,
    out: &mut std::collections::HashSet<String>,
    decl_spans: &mut std::collections::HashMap<String, Span>,
) {
    let mut state_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Build a set of bare names from namespaced bindings (e.g., "state_r2_pos.plain" -> "plain")
    // so that bare identifiers inside closures can be matched to their bindings.
    let bare_names: std::collections::HashSet<&str> = tp
        .bindings
        .keys()
        .filter_map(|k| k.rsplit('.').next())
        .collect();
    for stmt in &tp.program.stmts {
        collect_assigned_in_stmt(tp, stmt, out, decl_spans, &mut state_names, &bare_names);
    }
}

fn collect_assigned_in_stmt(
    tp: &TypedProgram,
    stmt: &zz_span_ast::ast::Stmt,
    out: &mut std::collections::HashSet<String>,
    decl_spans: &mut std::collections::HashMap<String, Span>,
    state_names: &mut std::collections::HashSet<String>,
    bare_names: &std::collections::HashSet<&str>,
) {
    match stmt {
        zz_span_ast::ast::Stmt::Decl {
            name,
            span,
            value,
            is_state,
            ..
        } => {
            decl_spans.insert(name.name.clone(), *span);
            if *is_state {
                state_names.insert(name.name.clone());
            }
            collect_assigned_in_expr(tp, value, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Stmt::Assign { target, .. } => match target {
            zz_span_ast::ast::Expr::Ident { name, .. } => {
                if bare_names.contains(name.as_str()) && !state_names.contains(name) {
                    out.insert(name.clone());
                }
            }
            zz_span_ast::ast::Expr::Path { parts, .. } => {
                // After namespace rewriting, a top-level global target is a
                // namespaced path (`module.counter`). Match it against
                // `tp.bindings` and record the bare name.
                if let Some(bare) = binding_basename(tp, parts) {
                    if !state_names.contains(&bare) {
                        out.insert(bare);
                    }
                }
            }
            _ => {}
        },
        zz_span_ast::ast::Stmt::CompoundAssign { target, .. } => match target {
            zz_span_ast::ast::Expr::Ident { name, .. } => {
                if bare_names.contains(name.as_str()) && !state_names.contains(name) {
                    out.insert(name.clone());
                }
            }
            zz_span_ast::ast::Expr::Path { parts, .. } => {
                if let Some(bare) = binding_basename(tp, parts) {
                    if !state_names.contains(&bare) {
                        out.insert(bare);
                    }
                }
            }
            _ => {}
        },
        zz_span_ast::ast::Stmt::Func { body, .. } => {
            collect_assigned_in_block(tp, body, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Stmt::For { body, .. } => {
            collect_assigned_in_block(tp, body, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Stmt::Expr(e) => {
            collect_assigned_in_expr(tp, e, out, decl_spans, state_names, bare_names);
        }
        _ => {}
    }
}

fn collect_assigned_in_block(
    tp: &TypedProgram,
    block: &zz_span_ast::ast::Block,
    out: &mut std::collections::HashSet<String>,
    decl_spans: &mut std::collections::HashMap<String, Span>,
    state_names: &mut std::collections::HashSet<String>,
    bare_names: &std::collections::HashSet<&str>,
) {
    for s in &block.stmts {
        collect_assigned_in_stmt(tp, s, out, decl_spans, state_names, bare_names);
    }
}

/// Recursively collect globals assigned (written) inside an expression,
/// descending into blocks, conditionals, loops, match arms and closures.
/// Assignments inside closures count because that code may run in a task.
fn collect_assigned_in_expr(
    tp: &TypedProgram,
    e: &zz_span_ast::ast::Expr,
    out: &mut std::collections::HashSet<String>,
    decl_spans: &mut std::collections::HashMap<String, Span>,
    state_names: &mut std::collections::HashSet<String>,
    bare_names: &std::collections::HashSet<&str>,
) {
    match e {
        zz_span_ast::ast::Expr::Block(b) => {
            collect_assigned_in_block(tp, b, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Expr::If { then, els, .. } => {
            collect_assigned_in_block(tp, then, out, decl_spans, state_names, bare_names);
            if let Some(el) = els {
                collect_assigned_in_expr(tp, el, out, decl_spans, state_names, bare_names);
            }
        }
        zz_span_ast::ast::Expr::While { body, .. } => {
            collect_assigned_in_block(tp, body, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Expr::Match { arms, .. } => {
            for a in arms {
                collect_assigned_in_expr(tp, &a.body, out, decl_spans, state_names, bare_names);
            }
        }
        zz_span_ast::ast::Expr::Closure { body, .. } => {
            collect_assigned_in_expr(tp, body, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Expr::Try { expr, .. } => {
            collect_assigned_in_expr(tp, expr, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Expr::Call { args, named, .. } => {
            for a in args {
                collect_assigned_in_expr(tp, a, out, decl_spans, state_names, bare_names);
            }
            for (_, v) in named {
                collect_assigned_in_expr(tp, v, out, decl_spans, state_names, bare_names);
            }
        }
        zz_span_ast::ast::Expr::Array { elems, .. } => {
            for e in elems {
                collect_assigned_in_expr(tp, e, out, decl_spans, state_names, bare_names);
            }
        }
        zz_span_ast::ast::Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_assigned_in_expr(tp, k, out, decl_spans, state_names, bare_names);
                collect_assigned_in_expr(tp, v, out, decl_spans, state_names, bare_names);
            }
        }
        zz_span_ast::ast::Expr::Paren { expr, .. } => {
            collect_assigned_in_expr(tp, expr, out, decl_spans, state_names, bare_names);
        }
        zz_span_ast::ast::Expr::Tuple { items, .. } => {
            for e in items {
                collect_assigned_in_expr(tp, e, out, decl_spans, state_names, bare_names);
            }
        }
        _ => {}
    }
}

/// Collect global names referenced directly inside `task.spawn` closures.
fn collect_spawn_globals(tp: &TypedProgram, out: &mut std::collections::HashSet<String>) {
    for stmt in &tp.program.stmts {
        collect_spawn_globals_in_stmt(tp, stmt, out);
    }
}

fn collect_spawn_globals_in_stmt(
    tp: &TypedProgram,
    stmt: &zz_span_ast::ast::Stmt,
    out: &mut std::collections::HashSet<String>,
) {
    match stmt {
        zz_span_ast::ast::Stmt::Func { body, .. } => {
            for s in &body.stmts {
                collect_spawn_globals_in_stmt(tp, s, out);
            }
        }
        zz_span_ast::ast::Stmt::For { body, .. } => {
            for s in &body.stmts {
                collect_spawn_globals_in_stmt(tp, s, out);
            }
        }
        zz_span_ast::ast::Stmt::Expr(e) => {
            collect_spawn_globals_in_expr(tp, e, out);
        }
        _ => {}
    }
}

fn collect_spawn_globals_in_expr(
    tp: &TypedProgram,
    e: &zz_span_ast::ast::Expr,
    out: &mut std::collections::HashSet<String>,
) {
    match e {
        zz_span_ast::ast::Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            // Check if this is `task.spawn(closure)`
            if let Some(name) = callee_name(callee) {
                if name == "task.spawn" {
                    if let Some(last) = args.last() {
                        collect_globals_from_expr(tp, last, out);
                    }
                }
            }
            for a in args {
                collect_spawn_globals_in_expr(tp, a, out);
            }
            for (_, v) in named {
                collect_spawn_globals_in_expr(tp, v, out);
            }
        }
        zz_span_ast::ast::Expr::Closure { body, .. } => {
            collect_globals_from_expr(tp, body, out);
        }
        _ => {}
    }
}

/// Collect global variable names referenced from an expression.
fn collect_globals_from_expr(
    tp: &TypedProgram,
    e: &zz_span_ast::ast::Expr,
    out: &mut std::collections::HashSet<String>,
) {
    match e {
        zz_span_ast::ast::Expr::Ident { name, .. } => {
            if tp.bindings.contains_key(name) {
                out.insert(name.clone());
            }
        }
        zz_span_ast::ast::Expr::Path { parts, .. } => {
            if let Some(bare) = binding_basename(tp, parts) {
                out.insert(bare);
            }
        }
        zz_span_ast::ast::Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_globals_from_expr(tp, callee, out);
            for a in args {
                collect_globals_from_expr(tp, a, out);
            }
            for (_, v) in named {
                collect_globals_from_expr(tp, v, out);
            }
        }
        zz_span_ast::ast::Expr::Block(b) => {
            for s in &b.stmts {
                collect_globals_from_stmt(tp, s, out);
            }
        }
        zz_span_ast::ast::Expr::If {
            cond, then, els, ..
        } => {
            collect_globals_from_expr(tp, cond, out);
            collect_globals_from_block(tp, then, out);
            if let Some(e) = els {
                collect_globals_from_expr(tp, e, out);
            }
        }
        zz_span_ast::ast::Expr::While { cond, body, .. } => {
            collect_globals_from_expr(tp, cond, out);
            collect_globals_from_block(tp, body, out);
        }
        zz_span_ast::ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_globals_from_expr(tp, scrutinee, out);
            for a in arms {
                collect_globals_from_expr(tp, &a.body, out);
            }
        }
        _ => {}
    }
}

fn collect_globals_from_block(
    tp: &TypedProgram,
    b: &zz_span_ast::ast::Block,
    out: &mut std::collections::HashSet<String>,
) {
    for s in &b.stmts {
        collect_globals_from_stmt(tp, s, out);
    }
}

fn collect_globals_from_stmt(
    tp: &TypedProgram,
    s: &zz_span_ast::ast::Stmt,
    out: &mut std::collections::HashSet<String>,
) {
    match s {
        zz_span_ast::ast::Stmt::Expr(e) => collect_globals_from_expr(tp, e, out),
        zz_span_ast::ast::Stmt::Assign { target, value, .. } => {
            collect_globals_from_expr(tp, target, out);
            collect_globals_from_expr(tp, value, out);
        }
        zz_span_ast::ast::Stmt::CompoundAssign { target, value, .. } => {
            collect_globals_from_expr(tp, target, out);
            collect_globals_from_expr(tp, value, out);
        }
        _ => {}
    }
}

/// Extract callee name from a call expression.
fn callee_name(callee: &zz_span_ast::ast::Expr) -> Option<String> {
    match callee {
        zz_span_ast::ast::Expr::Ident { name, .. } => Some(name.clone()),
        zz_span_ast::ast::Expr::Path { parts, .. } => Some(parts.join(".")),
        _ => None,
    }
}

/// Join a dotted path and, if it names a binding in `tp.bindings`, return
/// the bare (last-segment) name. After namespace rewriting, globals are
/// stored under their full dotted key (`module.counter`) while the R2
/// bookkeeping uses bare names.
fn binding_basename(tp: &TypedProgram, parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    let full = parts.join(".");
    if tp.bindings.contains_key(&full) {
        parts.last().cloned()
    } else {
        None
    }
}

/// True if `name` is a state or const global (immutable): never assigned
/// anywhere in the program after its initializer. Such globals may be read
/// from tasks without becoming `state`.
fn is_state_or_const(
    _tp: &TypedProgram,
    name: &str,
    assigned_globals: &std::collections::HashSet<String>,
) -> bool {
    // A global that is never assigned after its initializer is effectively
    // immutable, so reading it from a task is safe and does not require
    // declaring it `state`.
    !assigned_globals.contains(name)
}
