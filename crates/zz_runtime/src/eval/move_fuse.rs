//! Tree-walker move fusion for #242.
//!
//! The bytecode VM and AOT backends already fuse self-reassignment
//! shapes (`zz_frontend::move_elide`: `VecPush`, `FieldPush`,
//! `ThreadCall`) — the tree-walker (`Interp`, used by `zz test` and
//! plain `zz run`) cloned on every read, so Doc-by-value workloads (the
//! toml package's 15-array `Doc`) paid O(n²) deep copies (measured:
//! 1.18s → 3.6s → 14.4s for 175/350/700 keys). This module ports the
//! shared shapes plus take-restore store/read rules to the tree-walker,
//! reusing `classify_self_assign` so all engines agree on WHAT fuses.
//!
//! Soundness lemma (every shape): the transient-`Unit` window runs no
//! user code, and the moved name occurs exactly once (or zero times, for
//! pure stores/reads), so the transient is unobservable and fused
//! execution is indistinguishable from cloning:
//! - `VecPush` / `FieldPush`: the element evaluates pre-take (sibling
//!   reads intact, spawn snapshots match); the take→push→store window
//!   runs only the `vec.push` native. No guard needed.
//! - `ThreadCall`: siblings are call-free/closure-free/spawn-free by
//!   analysis; the user-code call runs under a visibility guard (the
//!   callee's captured chain must not contain the owner's link —
//!   closures over the caller's scope fall back to cloning). The taken
//!   value is restored if the call fails, so error paths keep the
//!   pre-statement state.
//! - Pure stores (`s.f`, `s.f[i]`, `s[i]` with pure RHS): the window runs
//!   only pure evaluation; completion reuses the real
//!   `set_object_field`/`set_index` on owned values.
//! - Indexed field reads (`s.f[i]` with pure index): take, extract one
//!   element, restore — O(1) instead of cloning the field array.
//! - Every fallback (unbound names, non-array values, shadowed push,
//!   observing callees, non-direct fields) runs the generic path with
//!   zero prior non-pure evaluation, so error precedence matches the
//!   unfused order. Pure prefix work may evaluate twice on fallback —
//!   unobservable by purity.

use zz_frontend::ast::Expr;
use zz_frontend::move_elide::{
    classify_self_assign, count_refs, has_call, has_closure_or_spawn, has_early_exit, MoveKind,
};
use zz_frontend::span::Span;

use crate::env::EnvLink;
use crate::runtime::ops::{normalize_index, set_index, set_object_field};
use crate::runtime::EvalError;
use crate::value::{FuncValue, Value};

use super::Interp;

/// True for expressions with no user-code execution and no early exit:
/// evaluating one cannot observe a take transient (or fail to return).
fn is_pure(e: &Expr) -> bool {
    !has_call(e) && !has_closure_or_spawn(e) && !has_early_exit(e)
}

/// Resolve a call callee exactly far enough to fuse: `Native` values fuse
/// unconditionally (Rust code never observes ZZ scopes); `Func` values
/// need the caller-side visibility guard. Anything else (methods, enums,
/// unresolvable, shadowed push) returns `None` → generic path. Mirrors
/// the generic resolution order (env → funcs → natives → C plugins →
/// selective-import aliases) so misses fall back to identical errors.
fn resolve_fusable_callee(interp: &Interp, callee: &Expr, span: Span) -> Option<Value> {
    match callee {
        Expr::Ident { name, .. } => {
            if let Some(v) = interp.env.get(name) {
                return Some(v);
            }
            if let Some(fv) = zz_checker::lookup_std(&interp.funcs, name) {
                return Some(Value::Func(std::rc::Rc::clone(fv)));
            }
            if let Some(entry) = zz_checker::lookup_std(&interp.natives, name) {
                return Some(Value::Native(Box::new(crate::value::NativeFunc {
                    name: name.clone(),
                    arity: entry.arity,
                })));
            }
            if let Some(nv) = crate::c_abi::native_value(name) {
                return Some(nv);
            }
            if let Some(qualified) = interp.import_aliases.get(name).cloned() {
                let parts: Vec<String> = qualified.split('.').map(str::to_string).collect();
                return interp.resolve_path_value(&parts, span).ok();
            }
            None
        }
        Expr::Path { parts, .. } => {
            let joined = parts.join(".");
            if let Some(v) = interp.env.get(&joined) {
                return Some(v);
            }
            if let Some(fv) = zz_checker::lookup_std(&interp.funcs, &joined) {
                return Some(Value::Func(std::rc::Rc::clone(fv)));
            }
            if let Some(entry) = zz_checker::lookup_std(&interp.natives, &joined) {
                return Some(Value::Native(Box::new(crate::value::NativeFunc {
                    name: joined,
                    arity: entry.arity,
                })));
            }
            None
        }
        _ => None,
    }
}

/// True when `fv` can observe `owner` (its captured chain contains the
/// owner's link): the take transient would be visible through it, so the
/// fused call must not run. Top-level functions (chain = root scope)
/// never observe caller locals; closures over the caller's scope do.
fn callee_observes(fv: &FuncValue, owner: &EnvLink) -> bool {
    let mut cur = Some(fv.env.clone());
    while let Some(link) = cur {
        if EnvLink::ptr_eq(&link, owner) {
            return true;
        }
        cur = link.parent_link();
    }
    false
}

/// Restore a taken binding after a failed fused op. The take proved the
/// binding and nothing unbinds mid-statement, so a miss is impossible —
/// but the signature keeps that path honest instead of panicking.
fn restore(interp: &mut Interp, name: &str, value: Value) -> Result<(), EvalError> {
    match interp.env.try_assign(name, value) {
        Ok(()) => Ok(()),
        Err(_) => Err(EvalError::new(
            "internal error: fused move restore missed its binding",
            Span::new(0, 0),
        )),
    }
}

impl Interp {
    /// Fused `target = value` for #242. `Ok(true)` = store completed;
    /// `Ok(false)` = not fusable → caller runs the generic path.
    pub(crate) fn try_move_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
    ) -> Result<bool, EvalError> {
        match classify_self_assign(target, value) {
            Some(MoveKind::VecPush(var)) => self.fuse_vec_push(&var, value),
            Some(MoveKind::FieldPush { obj, field }) => self.fuse_field_push(&obj, &field, value),
            Some(MoveKind::ThreadCall(var)) => self.fuse_thread_call(&var, value),
            None => self.try_pure_store(target, value),
        }
    }

    /// `x = vec.push(x, e)`, free form only. `x.push(e)` method spellings
    /// keep the generic path: method lookup may resolve user code, and
    /// the analysis shape alone cannot prove nativeness.
    fn fuse_vec_push(&mut self, var: &str, value: &Expr) -> Result<bool, EvalError> {
        let (callee, args) = match value {
            Expr::Call {
                callee,
                args,
                named,
                ..
            } if named.is_empty() && args.len() == 2 => (callee, args),
            _ => return Ok(false),
        };
        // The callee must be the real push native — a shadowing user
        // function keeps generic semantics.
        match resolve_fusable_callee(self, callee, args[1].span()) {
            Some(Value::Native(nf)) if nf.name == "vec.push" || nf.name == "std.vec.push" => {}
            _ => return Ok(false),
        }
        // Bound check before any evaluation: unbound names take the
        // generic `undefined variable` error with identical precedence.
        if self.env.owner_link(var).is_none() {
            return Ok(false);
        }
        let elem = self.eval(&args[1])?.into_value()?;
        let taken = match self.env.take(var) {
            Some(v) => v,
            None => {
                return Err(EvalError::new(
                    format!("undefined variable `{var}`"),
                    args[0].span(),
                ));
            }
        };
        let mut vs = match taken {
            // Owned take: reuse the buffer when uniquely owned, copy
            // once otherwise (the copy value semantics requires).
            Value::Array(vs) => std::sync::Arc::try_unwrap(vs).unwrap_or_else(|a| (*a).clone()),
            other => {
                let msg = format!("`vec.push` expects an array, found `{other}`");
                restore(self, var, other)?;
                return Err(EvalError::new(msg, Span::new(0, 0)));
            }
        };
        // `vec.push` is infallible on arrays: inline the wrap instead of
        // re-entering the native (identical value, zero dispatch).
        vs.push(elem);
        restore(self, var, Value::Array(std::sync::Arc::new(vs)))?;
        Ok(true)
    }

    /// `s.f = vec.push(s.f, e)`: the container never moves, only the
    /// field — taken out of the taken object (no container clone), pushed
    /// in place, stored back.
    fn fuse_field_push(&mut self, obj: &str, field: &str, value: &Expr) -> Result<bool, EvalError> {
        let (callee, args) = match value {
            Expr::Call {
                callee,
                args,
                named,
                ..
            } if named.is_empty() && args.len() == 2 => (callee, args),
            _ => return Ok(false),
        };
        match resolve_fusable_callee(self, callee, args[1].span()) {
            Some(Value::Native(nf)) if nf.name == "vec.push" || nf.name == "std.vec.push" => {}
            _ => return Ok(false),
        }
        if self.env.owner_link(obj).is_none() {
            return Ok(false);
        }
        let elem = self.eval(&args[1])?.into_value()?;
        let mut taken = match self.env.take(obj) {
            Some(v) => v,
            None => {
                return Err(EvalError::new(
                    format!("undefined variable `{obj}`"),
                    args[0].span(),
                ));
            }
        };
        // Direct array field: move it out (no field clone), push, write
        // the slot back. Anything else (missing/embedded/non-array)
        // completes generically on owned values — same calls, same
        // errors, minus the container clone.
        let direct = matches!(
            &mut taken,
            Value::Object(o)
                if o.fields.iter().any(|(n, v)| n.as_ref() == field
                    && matches!(v, Value::Array(_)))
        );
        if direct {
            if let Value::Object(o) = &mut taken {
                let o = std::sync::Arc::make_mut(o);
                if let Some((_, slot)) = o.fields.iter_mut().find(|(n, _)| n.as_ref() == field) {
                    if let Value::Array(vs) = std::mem::replace(slot, Value::Unit) {
                        let mut vs =
                            std::sync::Arc::try_unwrap(vs).unwrap_or_else(|a| (*a).clone());
                        vs.push(elem);
                        *slot = Value::Array(std::sync::Arc::new(vs));
                        restore(self, obj, taken)?;
                        return Ok(true);
                    }
                }
            }
            // Unreachable: the matches! guard proved shape. Fall through
            // to generic completion rather than panicking.
        }
        // Generic completion on owned values (no re-evaluation, so the
        // already-evaluated element is never double-run).
        let field_val = match crate::runtime::ops::object_field(&taken, field, args[0].span()) {
            Ok(v) => v,
            Err(e) => {
                restore(self, obj, taken)?;
                return Err(e);
            }
        };
        let pushed = match self.call(
            Value::Native(Box::new(crate::value::NativeFunc {
                name: "vec.push".to_string(),
                arity: 2,
            })),
            vec![field_val, elem],
            args[1].span(),
        ) {
            Ok(v) => v,
            Err(e) => {
                restore(self, obj, taken)?;
                return Err(e);
            }
        };
        if let Err(e) = set_object_field(&mut taken, field, pushed, args[0].span()) {
            restore(self, obj, taken)?;
            return Err(e);
        }
        restore(self, obj, taken)?;
        Ok(true)
    }

    /// `x = f(x, ...)`: take `x`, evaluate the call on the owned value,
    /// store the result. The occurrence is always a bare `Ident` (the
    /// classifier requires it); sibling args are pure by analysis.
    /// Natives fuse unconditionally; user functions fuse only when the
    /// callee cannot observe the owner's scope. Call failure restores
    /// the taken value, preserving pre-statement state.
    fn fuse_thread_call(&mut self, var: &str, value: &Expr) -> Result<bool, EvalError> {
        let (callee, args) = match value {
            Expr::Call {
                callee,
                args,
                named,
                ..
            } if named.is_empty() => (callee, args),
            _ => return Ok(false),
        };
        let at = args
            .iter()
            .position(|a| matches!(a, Expr::Ident { name: n, .. } if n == var));
        let Some(at) = at else { return Ok(false) };
        let callee_val = match resolve_fusable_callee(self, callee, callee.span()) {
            Some(v) => v,
            None => return Ok(false),
        };
        let owner = match self.env.owner_link(var) {
            Some(o) => o,
            None => return Ok(false),
        };
        match &callee_val {
            Value::Native(_) => {}
            Value::Func(fv) => {
                if callee_observes(fv, &owner) {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
        let taken = match self.env.take(var) {
            Some(v) => v,
            None => {
                return Err(EvalError::new(
                    format!("undefined variable `{var}`"),
                    args[at].span(),
                ));
            }
        };
        let mut arg_vals: Vec<Value> = Vec::with_capacity(args.len());
        let mut taken_opt = Some(taken);
        for (i, a) in args.iter().enumerate() {
            if i == at {
                arg_vals.push(taken_opt.take().expect("move target used once"));
            } else {
                match self.eval(a) {
                    Ok(f) => arg_vals.push(f.into_value()?),
                    Err(e) => {
                        restore(self, var, taken_opt.take().expect("move target kept"))?;
                        return Err(e);
                    }
                }
            }
        }
        match self.call(callee_val, arg_vals, value.span()) {
            Ok(v) => {
                restore(self, var, v)?;
                Ok(true)
            }
            Err(e) => {
                restore(self, var, taken_opt.take().expect("move target kept"))?;
                Err(e)
            }
        }
    }

    /// Pure general stores for #242 (beyond the shared shapes):
    /// `s.f = rhs`, `s.f[i] = rhs`, `s[i] = rhs` where the RHS (and
    /// index) is pure and never mentions `s`. The take window runs only
    /// pure evaluation; completion reuses the real
    /// `set_object_field`/`set_index` on owned values, so errors match
    /// the generic path exactly.
    fn try_pure_store(&mut self, target: &Expr, value: &Expr) -> Result<bool, EvalError> {
        enum StoreTarget<'a> {
            Field {
                obj: &'a str,
                field: &'a str,
                span: Span,
            },
            FieldIndex {
                obj: &'a str,
                field: &'a str,
                index: &'a Expr,
                span: Span,
            },
            Index {
                obj: &'a str,
                index: &'a Expr,
                span: Span,
            },
        }
        let store = match target {
            Expr::Field { obj, name, span } => match obj.as_ref() {
                Expr::Ident { name: o, .. } => StoreTarget::Field {
                    obj: o,
                    field: name,
                    span: *span,
                },
                _ => return Ok(false),
            },
            Expr::Index { obj, index, span } => match obj.as_ref() {
                Expr::Ident { name: o, .. } => StoreTarget::Index {
                    obj: o,
                    index,
                    span: *span,
                },
                Expr::Field {
                    obj: inner,
                    name: f,
                    ..
                } => match inner.as_ref() {
                    Expr::Ident { name: o, .. } => StoreTarget::FieldIndex {
                        obj: o,
                        field: f,
                        index,
                        span: *span,
                    },
                    _ => return Ok(false),
                },
                _ => return Ok(false),
            },
            _ => return Ok(false),
        };
        let (obj, field, index, span) = match &store {
            StoreTarget::Field { obj, field, span } => (*obj, Some(*field), None, *span),
            StoreTarget::FieldIndex {
                obj,
                field,
                index,
                span,
            } => (*obj, Some(*field), Some(*index), *span),
            StoreTarget::Index { obj, index, span } => (*obj, None, Some(*index), *span),
        };
        if !is_pure(value) || count_refs(value, obj) != 0 {
            return Ok(false);
        }
        if let Some(ix) = index {
            if !is_pure(ix) || count_refs(ix, obj) != 0 {
                return Ok(false);
            }
        }
        if self.env.owner_link(obj).is_none() {
            return Ok(false);
        }
        let mut taken = match self.env.take(obj) {
            Some(v) => v,
            None => return Ok(false),
        };
        // Eval index, then value (generic order), completing on owned.
        let idx_val = match index {
            Some(ix) => match self.eval(ix) {
                Ok(f) => Some(f.into_value()?),
                Err(e) => {
                    restore(self, obj, taken)?;
                    return Err(e);
                }
            },
            None => None,
        };
        let val = match self.eval(value) {
            Ok(f) => f.into_value()?,
            Err(e) => {
                restore(self, obj, taken)?;
                return Err(e);
            }
        };
        let done = match (field, idx_val) {
            (Some(f), None) => set_object_field(&mut taken, f, val, span).map(|_| ()),
            (None, Some(iv)) => set_index(&mut taken, &iv, val, span),
            (Some(f), Some(iv)) => {
                let mut field_val = match crate::runtime::ops::object_field(&taken, f, span) {
                    Ok(v) => v,
                    Err(e) => {
                        restore(self, obj, taken)?;
                        return Err(e);
                    }
                };
                match set_index(&mut field_val, &iv, val, span) {
                    Ok(()) => set_object_field(&mut taken, f, field_val, span).map(|_| ()),
                    Err(e) => Err(e),
                }
            }
            (None, None) => unreachable!("pure store always has a shape"),
        };
        if let Err(e) = done {
            restore(self, obj, taken)?;
            return Err(e);
        }
        restore(self, obj, taken)?;
        Ok(true)
    }

    /// Fused indexed field read `s.f[i]` for #242: take the container,
    /// clone one element (not the field array), restore. Only
    /// Array/Tuple/Dict fields with matching index shapes fast-path;
    /// anything else restores and returns `None` so the caller re-runs
    /// the generic path (sound: the index is pure, re-evaluation is
    /// unobservable).
    pub(crate) fn try_move_index_read(
        &mut self,
        obj: &Expr,
        index: &Expr,
        span: Span,
    ) -> Result<Option<Value>, EvalError> {
        let (container, field) = match obj {
            Expr::Field {
                obj: inner, name, ..
            } => match inner.as_ref() {
                Expr::Ident { name: o, .. } => (o.as_str(), name.as_str()),
                _ => return Ok(None),
            },
            Expr::Path { parts, .. } if parts.len() == 2 => (parts[0].as_str(), parts[1].as_str()),
            _ => return Ok(None),
        };
        if !is_pure(index) || count_refs(index, container) != 0 {
            return Ok(None);
        }
        if self.env.owner_link(container).is_none() {
            return Ok(None);
        }
        let taken = match self.env.take(container) {
            Some(v) => v,
            None => return Ok(None),
        };
        let idx_val = match self.eval(index) {
            Ok(f) => match f.into_value() {
                Ok(v) => v,
                Err(e) => {
                    restore(self, container, taken)?;
                    return Err(e);
                }
            },
            Err(e) => {
                restore(self, container, taken)?;
                return Err(e);
            }
        };
        let field_val = match &taken {
            Value::Object(o) => o
                .fields
                .iter()
                .find(|(n, _)| n.as_ref() == field)
                .map(|(_, v)| v),
            _ => None,
        };
        let elem = match (field_val, &idx_val) {
            (Some(Value::Array(items)), Value::Int(i)) => {
                match normalize_index(*i, items.len(), span) {
                    Ok(ix) => Some(items[ix].clone()),
                    Err(e) => {
                        restore(self, container, taken)?;
                        return Err(e);
                    }
                }
            }
            (Some(Value::Tuple(items)), Value::Int(i)) => {
                match normalize_index(*i, items.len(), span) {
                    Ok(ix) => Some(items[ix].clone()),
                    Err(e) => {
                        restore(self, container, taken)?;
                        return Err(e);
                    }
                }
            }
            (Some(Value::Dict(dict)), key) => dict.get(key).cloned(),
            _ => None,
        };
        match elem {
            Some(v) => {
                restore(self, container, taken)?;
                Ok(Some(v))
            }
            // Non-fast-path shape: restore and let the generic path run
            // (identical errors; the pure index re-evaluates silently).
            None => {
                restore(self, container, taken)?;
                Ok(None)
            }
        }
    }
}
