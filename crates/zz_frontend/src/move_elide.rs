//! Move-on-self-reassign analysis shared by all engines.
//!
//! ## Exact rule
//!
//! For a statement `x = RHS` (or `x := RHS`) where `x` is a plain local,
//! the single occurrence of `x` inside `RHS` may be *moved* (taken out of
//! its slot, leaving `Unit` behind until the final store) instead of
//! cloned when every clause below holds:
//!
//! 1. **Single occurrence.** `x` is referenced exactly once in the whole
//!    `RHS` — as `x`, `x.y`, or `x[i]` (any reference counts). Two reads
//!    (`x = x + x`, `x = f(x, x)`) need the value twice; no move.
//! 2. **No closures, no spawn in `RHS`.** A closure literal captures the
//!    scope and could observe the transient `Unit` when invoked before the
//!    store completes; `spawn` snapshots the scope (same hazard).
//! 3. **No other calls in the moved value's way — for `ThreadCall`.**
//!    For `vec.push` shapes the element argument needs no purity: engines
//!    evaluate it *before* the take, while the slot is still intact, so
//!    even calls/closures/spawns there observe the old value and the
//!    take→push→store window runs no user code at all. For `x = f(x, ...)`
//!    the take comes first, so every argument other than `x` itself must
//!    be call-free, closure-free and spawn-free; the callee itself is
//!    vetted per engine (native: plain local + not captured; VM: runtime
//!    scope-share check).
//! 4. **Loop iterators.** Sequential re-reads (conditions, bounds) are safe
//!    by construction — the store completes before any re-read. The only
//!    loop hazard is a *live borrow* of `x`'s buffer (native `for v in x`
//!    holds `iter_tmp`, a cloned share): the refcount uniqueness check
//!    (`refs == 1`) fails while borrowed and forces the copy fallback.
//! 5. **Field aliases.** Native container stores share (`refs` bump), so an
//!    `x` aliased through a dict/array/struct field fails the uniqueness
//!    check and falls back. VM stores deep-clone on read, so field aliases
//!    cannot exist (closure scope-sharing is the only VM alias — clause 2
//!    plus the scope-share check cover it).
//!
//! ## Shapes
//!
//! - `VecPush(x)`: `x = vec.push(x, e)` / `x = std.vec.push(x, e)` /
//!   `x = x.push(e)` (method spellings). The callee never invokes user
//!   code, so no callee check is needed.
//! - `FieldPush(obj, field)`: `s.f = vec.push(s.f, e)` (same, for one
//!   struct/object field; the container is never moved, only the field).
//! - `ThreadCall(x)`: `x = f(x, ...)` — the argument in `x`'s position is
//!   moved into the call. Per-engine guards apply on top (see clause 3).

use crate::ast::{Block, Expr, Stmt};

/// Classified self-reassignment shape (see module docs).
#[derive(Debug, Clone, PartialEq)]
pub enum MoveKind {
    /// `x = vec.push(x, e)` or `x = x.push(e)`.
    VecPush(String),
    /// `s.f = vec.push(s.f, e)`.
    FieldPush { obj: String, field: String },
    /// `x = f(x, ...)`.
    ThreadCall(String),
}

impl MoveKind {
    /// The moved variable (bare name; for fields, the container).
    pub fn var(&self) -> &str {
        match self {
            MoveKind::VecPush(v) | MoveKind::ThreadCall(v) => v,
            MoveKind::FieldPush { obj, .. } => obj,
        }
    }
}

/// Push-family callee spellings that never invoke user code.
/// Bare `append` is intentionally absent: on native it lowers to the
/// in-place mutator (unit return), so `x = vec.append(x, e)` is not the
/// same value shape there.
fn is_push_callee(callee: &Expr) -> bool {
    match callee {
        Expr::Ident { name, .. } => name == "vec.push" || name == "std.vec.push",
        Expr::Path { parts, .. } => {
            parts.as_slice() == ["vec", "push"] || parts.as_slice() == ["std", "vec", "push"]
        }
        _ => false,
    }
}

/// `is_push_callee` plus the append spellings. Only for engines where
/// append is value-identical to push (the VM: both return the new array).
/// The native backend must keep using `is_push_callee`.
fn is_push_or_append_callee(callee: &Expr) -> bool {
    if is_push_callee(callee) {
        return true;
    }
    match callee {
        Expr::Ident { name, .. } => {
            name == "append" || name == "vec.append" || name == "std.vec.append"
        }
        Expr::Path { parts, .. } => {
            parts.as_slice() == ["vec", "append"] || parts.as_slice() == ["std", "vec", "append"]
        }
        _ => false,
    }
}

/// `spawn` (any spelling whose head or tail is `spawn`) snapshots scope.
fn is_spawn_callee(callee: &Expr) -> bool {
    match callee {
        Expr::Ident { name, .. } => name == "spawn",
        Expr::Path { parts, .. } => {
            parts.first().is_some_and(|p| p == "spawn" || p == "task")
                || parts.last().is_some_and(|p| p == "spawn")
        }
        Expr::Field { name, .. } => name == "spawn",
        _ => false,
    }
}

/// Count references to bare name `name`: `Ident`, `Path` heads, and roots
/// of `Field`/`Index`/`Slice` chains. Anything value-position counts;
/// type names (`StructInit` head) do not.
pub fn count_refs(e: &Expr, name: &str) -> usize {
    match e {
        Expr::Ident { name: n, .. } => usize::from(n == name),
        Expr::Path { parts, .. } => usize::from(parts.first().is_some_and(|p| p == name)),
        Expr::Fmt { parts, .. } => parts
            .iter()
            .map(|p| match p {
                crate::ast::FmtPart::Expr(e, _) => count_refs(e, name),
                _ => 0,
            })
            .sum(),
        Expr::Paren { expr, .. } => count_refs(expr, name),
        Expr::Tuple { items, .. } => items.iter().map(|i| count_refs(i, name)).sum(),
        Expr::Unary { expr, .. } => count_refs(expr, name),
        Expr::Binary { left, right, .. } => count_refs(left, name) + count_refs(right, name),
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            count_refs(callee, name)
                + args.iter().map(|a| count_refs(a, name)).sum::<usize>()
                + named
                    .iter()
                    .map(|(_, v)| count_refs(v, name))
                    .sum::<usize>()
        }
        Expr::Closure { .. } => {
            // A closure body mentioning `name` captures it (reads the slot
            // when invoked). Count as a reference: single-occurrence rule
            // then fails, which is exactly the conservative answer.
            count_closure_refs(e, name)
        }
        Expr::If {
            cond, then, els, ..
        } => {
            count_refs(cond, name)
                + count_block_refs(then, name)
                + els.as_ref().map_or(0, |x| count_refs(x, name))
        }
        Expr::While { cond, body, .. } => count_refs(cond, name) + count_block_refs(body, name),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            count_refs(scrutinee, name)
                + arms
                    .iter()
                    .map(|a| {
                        count_refs(&a.body, name)
                            + a.guard.as_ref().map_or(0, |g| count_refs(g, name))
                    })
                    .sum::<usize>()
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            count_refs(value, name)
                + count_block_refs(then, name)
                + els.as_ref().map_or(0, |x| count_refs(x, name))
        }
        Expr::Try { expr, .. } => count_refs(expr, name),
        Expr::Block(b) => count_block_refs(b, name),
        Expr::Variant { arg, .. } => arg.as_ref().map_or(0, |a| count_refs(a, name)),
        Expr::Array { elems, .. } => elems.iter().map(|x| count_refs(x, name)).sum(),
        Expr::Dict { entries, .. } => entries
            .iter()
            .map(|(k, v)| count_refs(k, name) + count_refs(v, name))
            .sum(),
        Expr::Field { obj, .. } => count_refs(obj, name),
        Expr::Range { start, end, .. } => count_refs(start, name) + count_refs(end, name),
        Expr::StructInit { fields, .. } => fields.iter().map(|(_, v)| count_refs(v, name)).sum(),
        Expr::Index { obj, index, .. } => count_refs(obj, name) + count_refs(index, name),
        Expr::Slice {
            obj, start, end, ..
        } => {
            count_refs(obj, name)
                + start.as_ref().map_or(0, |s| count_refs(s, name))
                + end.as_ref().map_or(0, |x| count_refs(x, name))
        }
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            ..
        } => {
            // The comprehension variable shadows `name` inside body/filter.
            let inner = if var.name == name {
                0
            } else {
                count_refs(body, name) + filter.as_ref().map_or(0, |f| count_refs(f, name))
            };
            inner + count_refs(iter, name)
        }
        Expr::Break { .. } | Expr::Continue { .. } => 0,
        _ => 0,
    }
}

fn count_closure_refs(e: &Expr, name: &str) -> usize {
    match e {
        Expr::Closure { params, body, .. } => {
            if params.iter().any(|p| p.name.name == name) {
                0
            } else {
                count_refs(body, name)
            }
        }
        _ => 0,
    }
}

fn count_block_refs(b: &Block, name: &str) -> usize {
    b.stmts.iter().map(|s| count_stmt_refs(s, name)).sum()
}

fn count_stmt_refs(s: &Stmt, name: &str) -> usize {
    match s {
        Stmt::Decl { value, .. } => count_refs(value, name),
        Stmt::Assign { target, value, .. } => count_refs(target, name) + count_refs(value, name),
        // `x OP= e` reads and writes its target like `x = x OP e`.
        Stmt::CompoundAssign { target, value, .. } => {
            count_refs(target, name) + count_refs(value, name)
        }
        Stmt::Return { value, .. } => value.as_ref().map_or(0, |v| count_refs(v, name)),
        Stmt::Expr(e) => count_refs(e, name),
        Stmt::Func { .. } | Stmt::Struct { .. } | Stmt::Impl { .. } | Stmt::Import { .. } => 0,
        Stmt::For { iter, body, .. } => count_refs(iter, name) + count_block_refs(body, name),
        Stmt::Break { .. } | Stmt::Continue { .. } => 0,
        Stmt::Defer { expr, .. } => count_refs(expr, name),
        Stmt::Destructure { value, .. } => count_refs(value, name),
        Stmt::ExternBlock { .. } | Stmt::Link { .. } => 0,
    }
}

/// True when `e` contains any closure literal or `spawn` call.
pub fn has_closure_or_spawn(e: &Expr) -> bool {
    match e {
        Expr::Closure { .. } => true,
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            if is_spawn_callee(callee) {
                return true;
            }
            has_closure_or_spawn(callee)
                || args.iter().any(has_closure_or_spawn)
                || named.iter().any(|(_, v)| has_closure_or_spawn(v))
        }
        Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
            crate::ast::FmtPart::Expr(x, _) => has_closure_or_spawn(x),
            _ => false,
        }),
        Expr::Paren { expr, .. } => has_closure_or_spawn(expr),
        Expr::Tuple { items, .. } => items.iter().any(has_closure_or_spawn),
        Expr::Unary { expr, .. } => has_closure_or_spawn(expr),
        Expr::Binary { left, right, .. } => {
            has_closure_or_spawn(left) || has_closure_or_spawn(right)
        }
        Expr::If {
            cond, then, els, ..
        } => {
            has_closure_or_spawn(cond)
                || block_has_closure_or_spawn(then)
                || els
                    .as_ref()
                    .is_some_and(|x| has_closure_or_spawn(x.as_ref()))
        }
        Expr::While { cond, body, .. } => {
            has_closure_or_spawn(cond) || block_has_closure_or_spawn(body)
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            has_closure_or_spawn(scrutinee)
                || arms.iter().any(|a| {
                    has_closure_or_spawn(&a.body)
                        || a.guard.as_ref().is_some_and(has_closure_or_spawn)
                })
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            has_closure_or_spawn(value)
                || block_has_closure_or_spawn(then)
                || els
                    .as_ref()
                    .is_some_and(|x| has_closure_or_spawn(x.as_ref()))
        }
        Expr::Try { expr, .. } => has_closure_or_spawn(expr),
        Expr::Block(b) => block_has_closure_or_spawn(b),
        Expr::Variant { arg, .. } => arg.as_ref().is_some_and(|a| has_closure_or_spawn(a)),
        Expr::Array { elems, .. } => elems.iter().any(has_closure_or_spawn),
        Expr::Dict { entries, .. } => entries
            .iter()
            .any(|(k, v)| has_closure_or_spawn(k) || has_closure_or_spawn(v)),
        Expr::Field { obj, .. } => has_closure_or_spawn(obj),
        Expr::Range { start, end, .. } => has_closure_or_spawn(start) || has_closure_or_spawn(end),
        Expr::StructInit { fields, .. } => fields.iter().any(|(_, v)| has_closure_or_spawn(v)),
        Expr::Index { obj, index, .. } => has_closure_or_spawn(obj) || has_closure_or_spawn(index),
        Expr::Slice {
            obj, start, end, ..
        } => {
            has_closure_or_spawn(obj)
                || start.as_ref().is_some_and(|s| has_closure_or_spawn(s))
                || end
                    .as_ref()
                    .is_some_and(|x| has_closure_or_spawn(x.as_ref()))
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            has_closure_or_spawn(body)
                || has_closure_or_spawn(iter)
                || filter
                    .as_ref()
                    .is_some_and(|x| has_closure_or_spawn(x.as_ref()))
        }
        _ => false,
    }
}

fn block_has_closure_or_spawn(b: &Block) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Decl { value, .. } => has_closure_or_spawn(value),
        Stmt::Assign { target, value, .. } => {
            has_closure_or_spawn(target) || has_closure_or_spawn(value)
        }
        Stmt::Return { value, .. } => value.as_ref().is_some_and(has_closure_or_spawn),
        Stmt::Expr(e) => has_closure_or_spawn(e),
        Stmt::For { iter, body, .. } => {
            has_closure_or_spawn(iter) || block_has_closure_or_spawn(body)
        }
        Stmt::Defer { expr, .. } => has_closure_or_spawn(expr),
        Stmt::Destructure { value, .. } => has_closure_or_spawn(value),
        _ => false,
    })
}

/// True when `e` contains any call node (user or native, any callee form).
pub fn has_call(e: &Expr) -> bool {
    match e {
        Expr::Call { .. } => true,
        Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
            crate::ast::FmtPart::Expr(x, _) => has_call(x),
            _ => false,
        }),
        Expr::Paren { expr, .. } => has_call(expr),
        Expr::Tuple { items, .. } => items.iter().any(has_call),
        Expr::Unary { expr, .. } => has_call(expr),
        Expr::Binary { left, right, .. } => has_call(left) || has_call(right),
        Expr::Closure { body, .. } => has_call(body),
        Expr::If {
            cond, then, els, ..
        } => {
            has_call(cond)
                || block_has_call(then)
                || els.as_ref().is_some_and(|x| has_call(x.as_ref()))
        }
        Expr::While { cond, body, .. } => has_call(cond) || block_has_call(body),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            has_call(scrutinee)
                || arms
                    .iter()
                    .any(|a| has_call(&a.body) || a.guard.as_ref().is_some_and(has_call))
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            has_call(value)
                || block_has_call(then)
                || els.as_ref().is_some_and(|x| has_call(x.as_ref()))
        }
        Expr::Try { expr, .. } => has_call(expr),
        Expr::Block(b) => block_has_call(b),
        Expr::Variant { arg, .. } => arg.as_ref().is_some_and(|a| has_call(a)),
        Expr::Array { elems, .. } => elems.iter().any(has_call),
        Expr::Dict { entries, .. } => entries.iter().any(|(k, v)| has_call(k) || has_call(v)),
        Expr::Field { obj, .. } => has_call(obj),
        Expr::Range { start, end, .. } => has_call(start) || has_call(end),
        Expr::StructInit { fields, .. } => fields.iter().any(|(_, v)| has_call(v)),
        Expr::Index { obj, index, .. } => has_call(obj) || has_call(index),
        Expr::Slice {
            obj, start, end, ..
        } => {
            has_call(obj)
                || start.as_ref().is_some_and(|s| has_call(s))
                || end.as_ref().is_some_and(|x| has_call(x.as_ref()))
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            has_call(body)
                || has_call(iter)
                || filter.as_ref().is_some_and(|x| has_call(x.as_ref()))
        }
        _ => false,
    }
}

fn block_has_call(b: &Block) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Decl { value, .. } => has_call(value),
        Stmt::Assign { target, value, .. } => has_call(target) || has_call(value),
        Stmt::Return { value, .. } => value.as_ref().is_some_and(has_call),
        Stmt::Expr(e) => has_call(e),
        Stmt::For { iter, body, .. } => has_call(iter) || block_has_call(body),
        Stmt::Defer { expr, .. } => has_call(expr),
        Stmt::Destructure { value, .. } => has_call(value),
        _ => false,
    })
}

/// Copy `e`, rewriting every two-part `Path [m, x]` for which
/// `is_global("m.x")` holds into `Ident(x)`. Module loaders qualify
/// top-level references (`b` → `push_int.b`) before every engine sees
/// them; the classifier and counters below only understand bare names,
/// so consumers normalize first. Returns the copy when at least one
/// rewrite fired (else `None`, and the original can be used directly).
///
/// Coverage mirrors [`count_refs`] exactly (a missed occurrence would
/// under-count and wrongly allow a move), including statements nested in
/// blocks.
pub fn unqualify_expr(e: &Expr, is_global: &dyn Fn(&str) -> bool) -> Option<Expr> {
    fn rw(e: &Expr, is_global: &dyn Fn(&str) -> bool, changed: &mut bool) -> Expr {
        match e {
            Expr::Path { parts, span } if parts.len() == 2 => {
                let key = format!("{}.{}", parts[0], parts[1]);
                if is_global(&key) {
                    *changed = true;
                    return Expr::Ident {
                        name: parts[1].clone(),
                        span: *span,
                    };
                }
                e.clone()
            }
            Expr::Fmt { parts, span } => Expr::Fmt {
                parts: parts
                    .iter()
                    .map(|p| match p {
                        crate::ast::FmtPart::Expr(x, s) => crate::ast::FmtPart::Expr(
                            Box::new(rw(x, is_global, changed)),
                            s.clone(),
                        ),
                        other => other.clone(),
                    })
                    .collect(),
                span: *span,
            },
            Expr::Paren { expr, span } => Expr::Paren {
                expr: Box::new(rw(expr, is_global, changed)),
                span: *span,
            },
            Expr::Tuple { items, span } => Expr::Tuple {
                items: items.iter().map(|i| rw(i, is_global, changed)).collect(),
                span: *span,
            },
            Expr::Unary { op, expr, span } => Expr::Unary {
                op: *op,
                expr: Box::new(rw(expr, is_global, changed)),
                span: *span,
            },
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => Expr::Binary {
                op: *op,
                left: Box::new(rw(left, is_global, changed)),
                right: Box::new(rw(right, is_global, changed)),
                span: *span,
            },
            Expr::Call {
                callee,
                args,
                named,
                span,
            } => Expr::Call {
                callee: Box::new(rw(callee, is_global, changed)),
                args: args.iter().map(|a| rw(a, is_global, changed)).collect(),
                named: named
                    .iter()
                    .map(|(n, v)| (n.clone(), rw(v, is_global, changed)))
                    .collect(),
                span: *span,
            },
            Expr::Closure {
                params,
                ret_ty,
                body,
                span,
            } => Expr::Closure {
                params: params.clone(),
                ret_ty: ret_ty.clone(),
                body: Box::new(rw(body, is_global, changed)),
                span: *span,
            },
            Expr::If {
                cond,
                then,
                els,
                span,
            } => Expr::If {
                cond: Box::new(rw(cond, is_global, changed)),
                then: rw_block(then, is_global, changed),
                els: els.as_ref().map(|x| Box::new(rw(x, is_global, changed))),
                span: *span,
            },
            Expr::While { cond, body, span } => Expr::While {
                cond: Box::new(rw(cond, is_global, changed)),
                body: rw_block(body, is_global, changed),
                span: *span,
            },
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => Expr::Match {
                scrutinee: Box::new(rw(scrutinee, is_global, changed)),
                arms: arms
                    .iter()
                    .map(|a| crate::ast::MatchArm {
                        pat: a.pat.clone(),
                        guard: a.guard.as_ref().map(|g| rw(g, is_global, changed)),
                        body: rw(&a.body, is_global, changed),
                        span: a.span,
                    })
                    .collect(),
                span: *span,
            },
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                span,
            } => Expr::IfLet {
                pat: pat.clone(),
                value: Box::new(rw(value, is_global, changed)),
                then: rw_block(then, is_global, changed),
                els: els.as_ref().map(|x| Box::new(rw(x, is_global, changed))),
                span: *span,
            },
            Expr::Try { expr, span } => Expr::Try {
                expr: Box::new(rw(expr, is_global, changed)),
                span: *span,
            },
            Expr::Block(b) => Expr::Block(rw_block(b, is_global, changed)),
            Expr::Variant { name, arg, span } => Expr::Variant {
                name: name.clone(),
                arg: arg.as_ref().map(|a| Box::new(rw(a, is_global, changed))),
                span: *span,
            },
            Expr::Array { elems, span } => Expr::Array {
                elems: elems.iter().map(|x| rw(x, is_global, changed)).collect(),
                span: *span,
            },
            Expr::Dict { entries, span } => Expr::Dict {
                entries: entries
                    .iter()
                    .map(|(k, v)| (rw(k, is_global, changed), rw(v, is_global, changed)))
                    .collect(),
                span: *span,
            },
            Expr::Field { obj, name, span } => Expr::Field {
                obj: Box::new(rw(obj, is_global, changed)),
                name: name.clone(),
                span: *span,
            },
            Expr::Range { start, end, span } => Expr::Range {
                start: Box::new(rw(start, is_global, changed)),
                end: Box::new(rw(end, is_global, changed)),
                span: *span,
            },
            Expr::StructInit { name, fields, span } => Expr::StructInit {
                name: name.clone(),
                fields: fields
                    .iter()
                    .map(|(n, v)| (n.clone(), rw(v, is_global, changed)))
                    .collect(),
                span: *span,
            },
            Expr::Index { obj, index, span } => Expr::Index {
                obj: Box::new(rw(obj, is_global, changed)),
                index: Box::new(rw(index, is_global, changed)),
                span: *span,
            },
            Expr::Slice {
                obj,
                start,
                end,
                span,
            } => Expr::Slice {
                obj: Box::new(rw(obj, is_global, changed)),
                start: start.as_ref().map(|s| Box::new(rw(s, is_global, changed))),
                end: end.as_ref().map(|x| Box::new(rw(x, is_global, changed))),
                span: *span,
            },
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                span,
            } => Expr::ListComp {
                body: Box::new(rw(body, is_global, changed)),
                var: var.clone(),
                iter: Box::new(rw(iter, is_global, changed)),
                filter: filter.as_ref().map(|f| Box::new(rw(f, is_global, changed))),
                span: *span,
            },
            _ => e.clone(),
        }
    }
    fn rw_block(b: &Block, is_global: &dyn Fn(&str) -> bool, changed: &mut bool) -> Block {
        Block {
            stmts: b
                .stmts
                .iter()
                .map(|s| match s {
                    Stmt::Decl {
                        ty,
                        name,
                        value,
                        span,
                        pub_,
                        is_const,
                    } => Stmt::Decl {
                        ty: ty.clone(),
                        name: name.clone(),
                        value: rw(value, is_global, changed),
                        span: *span,
                        pub_: *pub_,
                        is_const: *is_const,
                    },
                    Stmt::Assign {
                        target,
                        value,
                        span,
                    } => Stmt::Assign {
                        target: rw(target, is_global, changed),
                        value: rw(value, is_global, changed),
                        span: *span,
                    },
                    Stmt::Return { value, span } => Stmt::Return {
                        value: value.as_ref().map(|v| rw(v, is_global, changed)),
                        span: *span,
                    },
                    Stmt::Expr(x) => Stmt::Expr(rw(x, is_global, changed)),
                    Stmt::For {
                        vars,
                        iter,
                        body,
                        span,
                    } => Stmt::For {
                        vars: vars.clone(),
                        iter: Box::new(rw(iter, is_global, changed)),
                        body: rw_block(body, is_global, changed),
                        span: *span,
                    },
                    Stmt::Defer { expr, span } => Stmt::Defer {
                        expr: Box::new(rw(expr, is_global, changed)),
                        span: *span,
                    },
                    Stmt::Destructure { pat, value, span } => Stmt::Destructure {
                        pat: pat.clone(),
                        value: rw(value, is_global, changed),
                        span: *span,
                    },
                    other => other.clone(),
                })
                .collect(),
            span: b.span,
        }
    }
    let mut changed = false;
    let out = rw(e, is_global, &mut changed);
    changed.then_some(out)
}

/// True when `e` can unwind past the enclosing statement: `break` /
/// `continue` expressions, `return` statements in nested blocks, or `?`
/// (`Try`). A `ThreadCall` take must not precede these — the store would
/// never run while the slot already holds `Unit`. (Push takes evaluate
/// siblings *before* taking, so they need no such exclusion.)
pub fn has_early_exit(e: &Expr) -> bool {
    match e {
        Expr::Break { .. } | Expr::Continue { .. } | Expr::Try { .. } => true,
        Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
            crate::ast::FmtPart::Expr(x, _) => has_early_exit(x),
            _ => false,
        }),
        Expr::Paren { expr, .. } => has_early_exit(expr),
        Expr::Tuple { items, .. } => items.iter().any(has_early_exit),
        Expr::Unary { expr, .. } => has_early_exit(expr),
        Expr::Binary { left, right, .. } => has_early_exit(left) || has_early_exit(right),
        Expr::Closure { body, .. } => has_early_exit(body),
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            has_early_exit(callee)
                || args.iter().any(has_early_exit)
                || named.iter().any(|(_, v)| has_early_exit(v))
        }
        Expr::If {
            cond, then, els, ..
        } => {
            has_early_exit(cond)
                || block_has_early_exit(then)
                || els.as_ref().is_some_and(|x| has_early_exit(x.as_ref()))
        }
        Expr::While { cond, body, .. } => has_early_exit(cond) || block_has_early_exit(body),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            has_early_exit(scrutinee)
                || arms.iter().any(|a| {
                    has_early_exit(&a.body) || a.guard.as_ref().is_some_and(has_early_exit)
                })
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            has_early_exit(value)
                || block_has_early_exit(then)
                || els.as_ref().is_some_and(|x| has_early_exit(x.as_ref()))
        }
        Expr::Block(b) => block_has_early_exit(b),
        Expr::Variant { arg, .. } => arg.as_ref().is_some_and(|a| has_early_exit(a.as_ref())),
        Expr::Array { elems, .. } => elems.iter().any(has_early_exit),
        Expr::Dict { entries, .. } => entries
            .iter()
            .any(|(k, v)| has_early_exit(k) || has_early_exit(v)),
        Expr::Field { obj, .. } => has_early_exit(obj),
        Expr::Range { start, end, .. } => has_early_exit(start) || has_early_exit(end),
        Expr::StructInit { fields, .. } => fields.iter().any(|(_, v)| has_early_exit(v)),
        Expr::Index { obj, index, .. } => has_early_exit(obj) || has_early_exit(index),
        Expr::Slice {
            obj, start, end, ..
        } => {
            has_early_exit(obj)
                || start.as_ref().is_some_and(|s| has_early_exit(s.as_ref()))
                || end.as_ref().is_some_and(|x| has_early_exit(x.as_ref()))
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            has_early_exit(body)
                || has_early_exit(iter)
                || filter.as_ref().is_some_and(|f| has_early_exit(f.as_ref()))
        }
        _ => false,
    }
}

fn block_has_early_exit(b: &Block) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::Decl { value, .. } => has_early_exit(value),
        Stmt::Assign { target, value, .. } => has_early_exit(target) || has_early_exit(value),
        Stmt::Expr(e) => has_early_exit(e),
        Stmt::For { iter, body, .. } => has_early_exit(iter) || block_has_early_exit(body),
        Stmt::Defer { expr, .. } => has_early_exit(expr),
        Stmt::Destructure { value, .. } => has_early_exit(value),
        _ => false,
    })
}

/// Receiver of a `.push(elem)` method call that is exactly the name `var`:
fn is_push_method_on(callee: &Expr, var: &str) -> bool {
    match callee {
        Expr::Path { parts, .. } => parts.len() == 2 && parts[0] == var && parts[1] == "push",
        Expr::Field { obj, name, .. } => {
            name == "push" && matches!(obj.as_ref(), Expr::Ident { name: n, .. } if n == var)
        }
        _ => false,
    }
}

/// Field receiver `s.f` as `Path [s, f]` or `Field { Ident s, f }`.
fn as_field_path(e: &Expr) -> Option<(&str, &str)> {
    match e {
        Expr::Path { parts, .. } if parts.len() == 2 => {
            Some((parts[0].as_str(), parts[1].as_str()))
        }
        Expr::Field { obj, name, .. } => match obj.as_ref() {
            Expr::Ident { name: o, .. } => Some((o.as_str(), name.as_str())),
            _ => None,
        },
        _ => None,
    }
}

/// True when `e` is exactly the field path `(obj, field)`.
fn is_field_ref(e: &Expr, obj: &str, field: &str) -> bool {
    match e {
        Expr::Path { parts, .. } => parts.len() == 2 && parts[0] == obj && parts[1] == field,
        Expr::Field { obj: o, name, .. } => {
            name == field
                && match o.as_ref() {
                    Expr::Ident { name: n, .. } => n == obj,
                    Expr::Path { parts, .. } => parts.join(".") == obj,
                    _ => false,
                }
        }
        _ => false,
    }
}

/// Count references to exactly the field `(obj, field)`. Reads of the bare
/// container or of sibling fields do not count: the take moves only the
/// field, so sibling reads (evaluated pre-take) are sound — and a shared
/// container merely forces the runtime fallback via the refcount check.
pub fn count_field_refs(e: &Expr, obj: &str, field: &str) -> usize {
    if is_field_ref(e, obj, field) {
        return 1;
    }
    match e {
        Expr::Fmt { parts, .. } => parts
            .iter()
            .map(|p| match p {
                crate::ast::FmtPart::Expr(x, _) => count_field_refs(x, obj, field),
                _ => 0,
            })
            .sum(),
        Expr::Paren { expr, .. } => count_field_refs(expr, obj, field),
        Expr::Tuple { items, .. } => items.iter().map(|i| count_field_refs(i, obj, field)).sum(),
        Expr::Unary { expr, .. } => count_field_refs(expr, obj, field),
        Expr::Binary { left, right, .. } => {
            count_field_refs(left, obj, field) + count_field_refs(right, obj, field)
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            count_field_refs(callee, obj, field)
                + args
                    .iter()
                    .map(|a| count_field_refs(a, obj, field))
                    .sum::<usize>()
                + named
                    .iter()
                    .map(|(_, v)| count_field_refs(v, obj, field))
                    .sum::<usize>()
        }
        Expr::Closure { params, body, .. } => {
            // A captured field read observes the slot when invoked: count
            // it (fails single-occurrence, the conservative answer), unless
            // shadowed by a closure param of the same container name.
            if params.iter().any(|p| p.name.name == obj) {
                0
            } else {
                count_field_refs(body, obj, field)
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            count_field_refs(cond, obj, field)
                + count_block_field_refs(then, obj, field)
                + els.as_ref().map_or(0, |x| count_field_refs(x, obj, field))
        }
        Expr::While { cond, body, .. } => {
            count_field_refs(cond, obj, field) + count_block_field_refs(body, obj, field)
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            count_field_refs(scrutinee, obj, field)
                + arms
                    .iter()
                    .map(|a| {
                        count_field_refs(&a.body, obj, field)
                            + a.guard
                                .as_ref()
                                .map_or(0, |g| count_field_refs(g, obj, field))
                    })
                    .sum::<usize>()
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            count_field_refs(value, obj, field)
                + count_block_field_refs(then, obj, field)
                + els.as_ref().map_or(0, |x| count_field_refs(x, obj, field))
        }
        Expr::Try { expr, .. } => count_field_refs(expr, obj, field),
        Expr::Block(b) => count_block_field_refs(b, obj, field),
        Expr::Variant { arg, .. } => arg.as_ref().map_or(0, |a| count_field_refs(a, obj, field)),
        Expr::Array { elems, .. } => elems.iter().map(|x| count_field_refs(x, obj, field)).sum(),
        Expr::Dict { entries, .. } => entries
            .iter()
            .map(|(k, v)| count_field_refs(k, obj, field) + count_field_refs(v, obj, field))
            .sum(),
        Expr::Field { obj: o, .. } => count_field_refs(o, obj, field),
        Expr::Range { start, end, .. } => {
            count_field_refs(start, obj, field) + count_field_refs(end, obj, field)
        }
        Expr::StructInit { fields, .. } => fields
            .iter()
            .map(|(_, v)| count_field_refs(v, obj, field))
            .sum(),
        Expr::Index { obj: o, index, .. } => {
            count_field_refs(o, obj, field) + count_field_refs(index, obj, field)
        }
        Expr::Slice {
            obj: o, start, end, ..
        } => {
            count_field_refs(o, obj, field)
                + start
                    .as_ref()
                    .map_or(0, |s| count_field_refs(s, obj, field))
                + end.as_ref().map_or(0, |x| count_field_refs(x, obj, field))
        }
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            ..
        } => {
            // The comprehension variable shadows same-named containers.
            if var.name == obj {
                count_field_refs(iter, obj, field)
            } else {
                count_field_refs(body, obj, field)
                    + count_field_refs(iter, obj, field)
                    + filter
                        .as_ref()
                        .map_or(0, |f| count_field_refs(f, obj, field))
            }
        }
        _ => 0,
    }
}

fn count_block_field_refs(b: &Block, obj: &str, field: &str) -> usize {
    b.stmts
        .iter()
        .map(|s| count_stmt_field_refs(s, obj, field))
        .sum()
}

fn count_stmt_field_refs(s: &Stmt, obj: &str, field: &str) -> usize {
    match s {
        Stmt::Decl { value, .. } => count_field_refs(value, obj, field),
        Stmt::Assign { target, value, .. } => {
            count_field_refs(target, obj, field) + count_field_refs(value, obj, field)
        }
        Stmt::Return { value, .. } => value
            .as_ref()
            .map_or(0, |v| count_field_refs(v, obj, field)),
        Stmt::Expr(e) => count_field_refs(e, obj, field),
        Stmt::For { iter, body, .. } => {
            count_field_refs(iter, obj, field) + count_block_field_refs(body, obj, field)
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => 0,
        Stmt::Defer { expr, .. } => count_field_refs(expr, obj, field),
        Stmt::Destructure { value, .. } => count_field_refs(value, obj, field),
        Stmt::ExternBlock { .. } | Stmt::Link { .. } => 0,
        _ => 0,
    }
}

/// Classify `target = rhs` per the module rule. Returns the move shape when
/// every static clause holds; per-engine guards (capture sets, callee
/// scope-share, slot kinds) apply on top.
/// Native-engine classification: push spellings only (see `is_push_callee`).
pub fn classify_self_assign(target: &Expr, rhs: &Expr) -> Option<MoveKind> {
    classify_with(target, rhs, &is_push_callee)
}

/// VM-engine classification: push plus append spellings (see
/// `is_push_or_append_callee`; append is value-identical to push there).
pub fn classify_self_assign_vm(target: &Expr, rhs: &Expr) -> Option<MoveKind> {
    classify_with(target, rhs, &is_push_or_append_callee)
}

fn classify_with(target: &Expr, rhs: &Expr, is_push: &dyn Fn(&Expr) -> bool) -> Option<MoveKind> {
    // `s.f = ...` field shape first (target is not a bare Ident).
    if let Some((obj, field)) = as_field_path(target) {
        if let Expr::Call {
            callee,
            args,
            named,
            ..
        } = rhs
        {
            // Single occurrence of the exact field only (see
            // `count_field_refs`): the take moves just the field. Sibling
            // args need no purity: engines evaluate them *before* the take,
            // while the slot is intact.
            if named.is_empty()
                && is_push(callee)
                && args.len() == 2
                && as_field_path(&args[0]) == Some((obj, field))
                && count_field_refs(rhs, obj, field) == 1
            {
                return Some(MoveKind::FieldPush {
                    obj: obj.to_string(),
                    field: field.to_string(),
                });
            }
        }
        return None;
    }
    let var: &String = match target {
        Expr::Ident { name, .. } => name,
        _ => return None,
    };
    let (callee, args, named): (&Expr, &Vec<Expr>, &Vec<(String, Expr)>) = match rhs {
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => (callee, args, named),
        _ => return None,
    };
    if !named.is_empty() || count_refs(rhs, var) != 1 {
        return None;
    }
    // `x = vec.push(x, e)`: x is args[0]. Sibling args need no purity (see
    // the field shape above for why).
    if is_push(callee) && args.len() == 2 {
        if matches!(&args[0], Expr::Ident { name: n, .. } if n == var) {
            return Some(MoveKind::VecPush(var.clone()));
        }
        return None;
    }
    // `x = x.push(e)`: receiver is the callee, element is args[0].
    if is_push_method_on(callee, var) && args.len() == 1 {
        return Some(MoveKind::VecPush(var.clone()));
    }
    // `x = f(x, ...)` (but not a second push spelling): the callee is a
    // plain reference — bare or dotted (same-module calls arrive qualified
    // as `ns.f`, methods carry their receiver) — with no calls/closures of
    // its own, so engines can vet it; every non-x argument is call-free.
    // Closure literals as callees are out: creating one shares the scope.
    let callee_ok = match callee {
        Expr::Ident { .. } | Expr::Path { .. } | Expr::Field { .. } => {
            !has_call(callee) && !has_closure_or_spawn(callee)
        }
        _ => false,
    };
    // No early exits anywhere in the RHS: the take precedes user code, so
    // the store must be guaranteed to run.
    if callee_ok
        && !has_early_exit(rhs)
        && args
            .iter()
            .any(|a| matches!(a, Expr::Ident { name: n, .. } if n == var))
        && args.iter().all(|a| {
            matches!(a, Expr::Ident { name: n, .. } if n == var)
                || (!has_call(a) && !has_closure_or_spawn(a))
        })
    {
        return Some(MoveKind::ThreadCall(var.clone()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Stmt;

    fn assign_of(src: &str) -> (crate::ast::Expr, crate::ast::Expr) {
        let parsed = crate::parse(src);
        assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
        for stmt in &parsed.program.stmts {
            if let Stmt::Assign { target, value, .. } = stmt {
                return (target.clone(), value.clone());
            }
        }
        panic!("no Assign in {src:?}");
    }

    fn kind(src: &str) -> Option<MoveKind> {
        let (t, v) = assign_of(src);
        classify_self_assign(&t, &v)
    }

    #[test]
    fn push_shapes() {
        assert_eq!(
            kind("b = vec.push(b, 1)\n"),
            Some(MoveKind::VecPush("b".into()))
        );
        assert_eq!(
            kind("b = std.vec.push(b, x)\n"),
            Some(MoveKind::VecPush("b".into()))
        );
        assert_eq!(kind("b = b.push(1)\n"), Some(MoveKind::VecPush("b".into())));
    }

    #[test]
    fn field_shape() {
        assert_eq!(
            kind("s.f = vec.push(s.f, 1)\n"),
            Some(MoveKind::FieldPush {
                obj: "s".into(),
                field: "f".into()
            })
        );
    }

    #[test]
    fn thread_shape() {
        assert_eq!(
            kind("doc = push_node(doc, 1)\n"),
            Some(MoveKind::ThreadCall("doc".into()))
        );
        // Moved arg need not be first.
        assert_eq!(
            kind("doc = combine(1, doc)\n"),
            Some(MoveKind::ThreadCall("doc".into()))
        );
        // Dotted callees (qualified same-module calls, methods) qualify.
        assert_eq!(kind("x = m.f(x)\n"), Some(MoveKind::ThreadCall("x".into())));
    }

    #[test]
    fn must_not_double_read() {
        assert_eq!(kind("b = vec.push(b, b)\n"), None);
        assert_eq!(kind("x = x + x\n"), None);
        assert_eq!(kind("x = f(x, x)\n"), None);
    }

    #[test]
    fn must_not_closure_or_spawn() {
        // ThreadCall: closures/spawns anywhere are out (take comes first).
        assert_eq!(kind("x = f(x, |v| v)\n"), None);
        assert_eq!(kind("x = apply(|v| x)\n"), None);
        assert_eq!(kind("x = f(x, spawn(g))\n"), None);
        // Push shapes evaluate siblings pre-take: allowed.
        assert_eq!(
            kind("b = vec.push(b, spawn(g))\n"),
            Some(MoveKind::VecPush("b".into()))
        );
    }

    #[test]
    fn push_elem_needs_no_purity() {
        // Engines evaluate the element before the take (slot still
        // intact), so calls/closures/spawns there are sound.
        assert_eq!(
            kind("b = vec.push(b, f())\n"),
            Some(MoveKind::VecPush("b".into()))
        );
        assert_eq!(
            kind("s.f = vec.push(s.f, len(s.g))\n"),
            Some(MoveKind::FieldPush {
                obj: "s".into(),
                field: "f".into()
            })
        );
    }

    #[test]
    fn must_not_early_exit() {
        // The take precedes user code: any unwind would skip the store.
        assert_eq!(kind("x = f(x, y?)\n"), None);
        assert_eq!(kind("x = f(x, if c { break } else { 1 })\n"), None);
    }

    #[test]
    fn must_not_nested_calls() {
        // ThreadCall sibling args must be call-free (take comes first).
        assert_eq!(kind("x = f(x, g(1))\n"), None);
        // Callees running code of their own are out.
        assert_eq!(kind("x = f()(x)\n"), None);
    }

    #[test]
    fn must_not_other_shapes() {
        assert_eq!(kind("x = 1\n"), None);
        assert_eq!(kind("x = y\n"), None);
        assert_eq!(kind("d[0] = vec.push(d[0], 1)\n"), None);
    }
}
