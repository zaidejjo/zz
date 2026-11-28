//! Pure-region analysis for `state` reads.
//!
//! A **pure region** is a maximal subexpression whose evaluation cannot run
//! user code, a closure, a native (whitelisted or not), I/O, or a blocking
//! call. Every `state` read inside one pure region is snapshotted under one
//! lock acquisition; two regions in one statement may observe different
//! snapshots (the checker rejects that when they name different states).
//!
//! The three consumers — the type checker (Z0019), the VM compiler
//! (`StateBatchBegin`/`End`) and the AOT lowering (`zz_state_lock`/`unlock`)
//! — must agree on the decomposition, so it lives here next to the AST.

use std::collections::BTreeSet;

use super::expr::{Expr, FmtPart};

/// Backends that can execute arbitrary code (or block) end a pure region.
/// This is deliberately conservative: *every* call is a boundary, including
/// whitelisted pure natives, so the checker's verdict can never be more
/// permissive than the code the two engines emit.
pub fn expr_is_pure(e: &Expr) -> bool {
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Ident { .. }
        | Expr::Path { .. } => true,
        // Creating a closure does not run its body; the body is a separate
        // chunk with its own statement-level regions.
        Expr::Closure { .. } => true,
        Expr::Fmt { parts, .. } => parts.iter().all(|p| match p {
            FmtPart::Expr(e, _) => expr_is_pure(e),
            FmtPart::Text(_) => true,
        }),
        Expr::Paren { expr, .. } => expr_is_pure(expr),
        Expr::Tuple { items, .. } => items.iter().all(expr_is_pure),
        Expr::Unary { expr, .. } => expr_is_pure(expr),
        Expr::Binary { left, right, .. } => expr_is_pure(left) && expr_is_pure(right),
        Expr::Variant { arg, .. } => arg.as_deref().is_none_or(expr_is_pure),
        Expr::Array { elems, .. } => elems.iter().all(expr_is_pure),
        Expr::Dict { entries, .. } => entries
            .iter()
            .all(|(k, v)| expr_is_pure(k) && expr_is_pure(v)),
        Expr::Field { obj, .. } => expr_is_pure(obj),
        Expr::Range { start, end, .. } => expr_is_pure(start) && expr_is_pure(end),
        Expr::StructInit { fields, .. } => fields.iter().all(|(_, v)| expr_is_pure(v)),
        Expr::Index { obj, index, .. } => expr_is_pure(obj) && expr_is_pure(index),
        Expr::Slice {
            obj, start, end, ..
        } => {
            expr_is_pure(obj)
                && start.as_deref().is_none_or(expr_is_pure)
                && end.as_deref().is_none_or(expr_is_pure)
        }
        Expr::Is { expr, .. } => expr_is_pure(expr),
        // A call is a boundary (`Call`); `?` can leave the region by
        // propagating an error (`Try`); `if`/`while`/`match`/blocks execute
        // their bodies (each a statement unit of its own).
        Expr::Call { .. }
        | Expr::Try { .. }
        | Expr::If { .. }
        | Expr::While { .. }
        | Expr::Match { .. }
        | Expr::IfLet { .. }
        | Expr::Block(_)
        | Expr::ListComp { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => false,
    }
}

/// The state names read directly by `e` (no bodies).
pub fn expr_state_reads(e: &Expr, is_state: &impl Fn(&str) -> bool) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_reads(e, is_state, &mut out);
    out
}

fn collect_reads(e: &Expr, is_state: &impl Fn(&str) -> bool, out: &mut BTreeSet<String>) {
    match e {
        Expr::Ident { name, .. } => {
            if is_state(name) {
                out.insert(name.clone());
            }
        }
        Expr::Path { parts, .. } => {
            // A dotted path may name a state directly (`ns.x`) or read one
            // as its head (`ns.field`).
            let joined = parts.join(".");
            if is_state(&joined) {
                out.insert(joined);
            }
            for p in parts {
                if is_state(p) {
                    out.insert(p.clone());
                }
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let FmtPart::Expr(e, _) = p {
                    collect_reads(e, is_state, out);
                }
            }
        }
        Expr::Paren { expr, .. }
        | Expr::Unary { expr, .. }
        | Expr::Field { obj: expr, .. }
        | Expr::Is { expr, .. }
        | Expr::Try { expr, .. } => collect_reads(expr, is_state, out),
        Expr::Tuple { items, .. } | Expr::Array { elems: items, .. } => {
            for e in items {
                collect_reads(e, is_state, out);
            }
        }
        Expr::Binary { left, right, .. }
        | Expr::Range {
            start: left,
            end: right,
            ..
        } => {
            collect_reads(left, is_state, out);
            collect_reads(right, is_state, out);
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_reads(k, is_state, out);
                collect_reads(v, is_state, out);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_reads(v, is_state, out);
            }
        }
        Expr::Index { obj, index, .. } => {
            collect_reads(obj, is_state, out);
            collect_reads(index, is_state, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_reads(obj, is_state, out);
            if let Some(e) = start {
                collect_reads(e, is_state, out);
            }
            if let Some(e) = end {
                collect_reads(e, is_state, out);
            }
        }
        Expr::Variant { arg, .. } => {
            if let Some(e) = arg {
                collect_reads(e, is_state, out);
            }
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_reads(callee, is_state, out);
            for a in args {
                collect_reads(a, is_state, out);
            }
            for (_, v) in named {
                collect_reads(v, is_state, out);
            }
        }
        Expr::If { cond, .. } => collect_reads(cond, is_state, out),
        Expr::IfLet { value, .. } => collect_reads(value, is_state, out),
        Expr::While { cond, .. } => collect_reads(cond, is_state, out),
        Expr::Match { scrutinee, .. } => collect_reads(scrutinee, is_state, out),
        // Bodies / closures are separate statement units.
        Expr::Closure { .. }
        | Expr::Block(_)
        | Expr::ListComp { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. } => {}
    }
}

/// A call whose *cell* (callee/receiver plus every argument) is pure: the
/// whole cell is one region, and the call itself runs after the region
/// closes (R2).
pub fn call_cell_is_pure(e: &Expr) -> bool {
    match e {
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            expr_is_pure(callee)
                && args.iter().all(expr_is_pure)
                && named.iter().all(|(_, v)| expr_is_pure(v))
        }
        _ => false,
    }
}

/// Decompose `e` into the pure regions it evaluates, in evaluation order.
/// Each region is the set of state names read inside it. Regions with no
/// state reads are omitted. Bodies (`if`/`while`/`match` arms/blocks) are
/// not descended into: they are statement units handled separately.
pub fn expr_regions(e: &Expr, is_state: &impl Fn(&str) -> bool) -> Vec<BTreeSet<String>> {
    let mut out = Vec::new();
    regions_into(e, is_state, &mut out);
    out
}

/// Push the one region that covers `e` when `e` is a pure state-reading
/// expression (or a pure call cell); otherwise recurse in evaluation order.
fn regions_into(e: &Expr, is_state: &impl Fn(&str) -> bool, out: &mut Vec<BTreeSet<String>>) {
    let cell_pure = expr_is_pure(e) || call_cell_is_pure(e);
    if cell_pure {
        let reads = expr_state_reads(e, is_state);
        if !reads.is_empty() {
            out.push(reads);
        }
        return;
    }
    match e {
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            regions_into(callee, is_state, out);
            for a in args {
                regions_into(a, is_state, out);
            }
            for (_, v) in named {
                regions_into(v, is_state, out);
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let FmtPart::Expr(e, _) = p {
                    regions_into(e, is_state, out);
                }
            }
        }
        Expr::Paren { expr, .. }
        | Expr::Unary { expr, .. }
        | Expr::Field { obj: expr, .. }
        | Expr::Is { expr, .. }
        | Expr::Try { expr, .. } => regions_into(expr, is_state, out),
        Expr::Tuple { items, .. } | Expr::Array { elems: items, .. } => {
            for e in items {
                regions_into(e, is_state, out);
            }
        }
        Expr::Binary { left, right, .. }
        | Expr::Range {
            start: left,
            end: right,
            ..
        } => {
            regions_into(left, is_state, out);
            regions_into(right, is_state, out);
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                regions_into(k, is_state, out);
                regions_into(v, is_state, out);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                regions_into(v, is_state, out);
            }
        }
        Expr::Index { obj, index, .. } => {
            regions_into(obj, is_state, out);
            regions_into(index, is_state, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            regions_into(obj, is_state, out);
            if let Some(e) = start {
                regions_into(e, is_state, out);
            }
            if let Some(e) = end {
                regions_into(e, is_state, out);
            }
        }
        Expr::Variant { arg, .. } => {
            if let Some(e) = arg {
                regions_into(e, is_state, out);
            }
        }
        Expr::If { cond, .. } => regions_into(cond, is_state, out),
        Expr::IfLet { value, .. } => regions_into(value, is_state, out),
        Expr::While { cond, .. } => regions_into(cond, is_state, out),
        Expr::Match { scrutinee, .. } => regions_into(scrutinee, is_state, out),
        Expr::Closure { .. }
        | Expr::Block(_)
        | Expr::ListComp { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Ident { .. }
        | Expr::Path { .. } => {}
    }
}
