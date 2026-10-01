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
//! 3. **No other calls in the moved value's way.** For `vec.push` shapes
//!    the element argument must be call-free (a nested call could re-enter
//!    user code that reads `x`). For `x = f(x, ...)` every argument other
//!    than `x` itself must be call-free, closure-free and spawn-free; the
//!    callee itself is vetted per engine (native: plain local + not
//!    captured; VM: runtime scope-share check).
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
                || els.as_ref().is_some_and(|x| has_closure_or_spawn(x))
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
                        || a.guard.as_ref().is_some_and(|x| has_closure_or_spawn(x))
                })
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            has_closure_or_spawn(value)
                || block_has_closure_or_spawn(then)
                || els.as_ref().is_some_and(|x| has_closure_or_spawn(x))
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
                || end.as_ref().is_some_and(|x| has_closure_or_spawn(x))
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            has_closure_or_spawn(body)
                || has_closure_or_spawn(iter)
                || filter.as_ref().is_some_and(|x| has_closure_or_spawn(x))
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
        Stmt::Return { value, .. } => value.as_ref().is_some_and(|x| has_closure_or_spawn(x)),
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
        } => has_call(cond) || block_has_call(then) || els.as_ref().is_some_and(|x| has_call(x)),
        Expr::While { cond, body, .. } => has_call(cond) || block_has_call(body),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            has_call(scrutinee)
                || arms
                    .iter()
                    .any(|a| has_call(&a.body) || a.guard.as_ref().is_some_and(|x| has_call(x)))
        }
        Expr::IfLet {
            value, then, els, ..
        } => has_call(value) || block_has_call(then) || els.as_ref().is_some_and(|x| has_call(x)),
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
                || end.as_ref().is_some_and(|x| has_call(x))
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => has_call(body) || has_call(iter) || filter.as_ref().is_some_and(|x| has_call(x)),
        _ => false,
    }
}

fn block_has_call(b: &Block) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Decl { value, .. } => has_call(value),
        Stmt::Assign { target, value, .. } => has_call(target) || has_call(value),
        Stmt::Return { value, .. } => value.as_ref().is_some_and(|x| has_call(x)),
        Stmt::Expr(e) => has_call(e),
        Stmt::For { iter, body, .. } => has_call(iter) || block_has_call(body),
        Stmt::Defer { expr, .. } => has_call(expr),
        Stmt::Destructure { value, .. } => has_call(value),
        _ => false,
    })
}

/// Receiver of a `.push(elem)` method call that is exactly the name `var`:
/// `x.push(e)` as `Path [x, push]` or `Field { Ident x, push }`.
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

/// Classify `target = rhs` per the module rule. Returns the move shape when
/// every static clause holds; per-engine guards (capture sets, callee
/// scope-share, slot kinds) apply on top.
pub fn classify_self_assign(target: &Expr, rhs: &Expr) -> Option<MoveKind> {
    // `s.f = ...` field shape first (target is not a bare Ident).
    if let Some((obj, field)) = as_field_path(target) {
        if let Expr::Call {
            callee,
            args,
            named,
            ..
        } = rhs
        {
            if named.is_empty() && is_push_callee(callee) && args.len() == 2 {
                if as_field_path(&args[0]) == Some((obj, field))
                    && count_refs(rhs, obj) == 1
                    && !has_closure_or_spawn(rhs)
                    && !has_call(&args[1])
                    && !has_closure_or_spawn(&args[1])
                {
                    return Some(MoveKind::FieldPush {
                        obj: obj.to_string(),
                        field: field.to_string(),
                    });
                }
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
    if !named.is_empty() || has_closure_or_spawn(rhs) || count_refs(rhs, var) != 1 {
        return None;
    }
    // `x = vec.push(x, e)`: x is args[0], element pure.
    if is_push_callee(callee) && args.len() == 2 {
        if matches!(&args[0], Expr::Ident { name: n, .. } if n == var)
            && !has_call(&args[1])
            && !has_closure_or_spawn(&args[1])
        {
            return Some(MoveKind::VecPush(var.clone()));
        }
        return None;
    }
    // `x = x.push(e)`: receiver is the callee, element is args[0].
    if is_push_method_on(callee, var) && args.len() == 1 {
        if !has_call(&args[0]) && !has_closure_or_spawn(&args[0]) {
            return Some(MoveKind::VecPush(var.clone()));
        }
        return None;
    }
    // `x = f(x, ...)` (but not a second push spelling): callee must be a
    // bare name so engines can vet it; every non-x argument call-free.
    if matches!(callee, Expr::Ident { .. })
        && args
            .iter()
            .any(|a| matches!(a, Expr::Ident { name: n, .. } if n == var))
        && args.iter().all(|a| {
            matches!(a, Expr::Ident { name: n, .. } if n == var)
                || (!has_call(a) && !has_closure_or_spawn(a))
        })
        && !has_call(callee)
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
    }

    #[test]
    fn must_not_double_read() {
        assert_eq!(kind("b = vec.push(b, b)\n"), None);
        assert_eq!(kind("x = x + x\n"), None);
        assert_eq!(kind("x = f(x, x)\n"), None);
    }

    #[test]
    fn must_not_closure_or_spawn() {
        assert_eq!(kind("x = f(x, |v| v)\n"), None);
        assert_eq!(kind("x = apply(|v| x)\n"), None);
        assert_eq!(kind("x = f(x, spawn(g))\n"), None);
        assert_eq!(kind("b = vec.push(b, spawn(g))\n"), None);
    }

    #[test]
    fn must_not_nested_calls() {
        // Element / non-x args must be call-free.
        assert_eq!(kind("b = vec.push(b, f())\n"), None);
        assert_eq!(kind("x = f(x, g(1))\n"), None);
        // Non-push, non-Ident callees are out of scope.
        assert_eq!(kind("x = m.f(x)\n"), None);
    }

    #[test]
    fn must_not_other_shapes() {
        assert_eq!(kind("x = 1\n"), None);
        assert_eq!(kind("x = y\n"), None);
        assert_eq!(kind("d[0] = vec.push(d[0], 1)\n"), None);
    }
}
