//! Move-on-self-reassign lowering for the native backend.
//!
//! `x = vec.push(x, e)` takes the value out of its slot and calls
//! `zz_vec_push_take`, which grows the buffer in place when the array is
//! uniquely owned (`refs == 1`) and falls back to the copy path otherwise.
//! `s.f = vec.push(s.f, e)` does the same through
//! `zz_object_push_field_take`. `x = f(x, ...)` moves the argument into the
//! call (no `zz_clone` bump) when `x` is dead afterwards and unobservable
//! mid-statement.
//!
//! Static eligibility comes from [`zz_frontend::move_elide`]; the guards
//! below are the native-specific half:
//! - `VecPush`/`FieldPush`: the callee never runs user code and sibling
//!   args are pure, so the take→push→store window is unobservable. Plain
//!   locals, owner cells and globals are all fine; closure *environment*
//!   slots (`cap_deref`) are excluded (different lifetime rules).
//! - `ThreadCall`: the callee is user code, so `x` must additionally be a
//!   stack local (plain functions can read module globals — verified by
//!   probe), not cell-shared, and not captured by any closure in the
//!   function.
//! - Green (suspendable) bodies are excluded everywhere: a suspend must
//!   never observe a moved-from slot.
//! - Module-qualified references (`ns.b` for top-level `b`) are normalized
//!   before classification; when normalization rewrote anything while a
//!   same-named local is live, the occurrence may name the global while the
//!   slot is the local (shadowing), so the attempt is abandoned.

use zz_frontend::ast::Expr;
use zz_frontend::move_elide::{classify_self_assign, unqualify_expr, MoveKind};

use super::{box_scalar_operand, Lowerer, NameCtx};

/// Marker C type for a moved-from-temporary binding: [`Lowerer::emit_expr`]
/// emits the bare C identifier (no `zz_clone` bump) for it. The entry is
/// pushed for exactly one call emission and popped right after.
const MOVED_TYPE: &str = "zz_value_moved";

/// The [`MOVED_TYPE`] check for expression lowering (see `expr.rs`).
pub(super) fn is_moved_type(ctype: Option<&str>) -> bool {
    ctype == Some(MOVED_TYPE)
}

impl Lowerer {
    /// Try the move fast paths for `target = value`. Returns true when
    /// emitted (caller must skip the generic path).
    pub(super) fn try_emit_move_assign(
        &self,
        target: &Expr,
        value: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) -> bool {
        if self.green_active() {
            return false;
        }
        let is_global = |k: &str| names.globals.contains_key(k);
        let nt = unqualify_expr(target, &is_global);
        let nv = unqualify_expr(value, &is_global);
        let t2 = nt.as_ref().unwrap_or(target);
        let v2 = nv.as_ref().unwrap_or(value);
        let Some(kind) = classify_self_assign(t2, v2) else {
            return false;
        };
        // Shadowing: a rewrite plus a live same-named local means the
        // single occurrence may have named the global while slot
        // resolution would land on the local. Abandon (safe direction).
        if nt.is_some() || nv.is_some() {
            let var = kind.var();
            if names.stack.get(var).is_some_and(|v| !v.is_empty()) {
                return false;
            }
        }
        match kind {
            MoveKind::VecPush(var) => self.emit_push_take(&var, value, names, out),
            MoveKind::FieldPush { obj, field } => {
                self.emit_field_push_take(&obj, &field, value, names, out)
            }
            MoveKind::ThreadCall(var) => self.emit_thread_call_take(&var, value, names, out),
        }
    }

    /// C identifier + locality for a takeable slot. Stack locals shadow
    /// globals; a unique `*.var` global also resolves (module-qualified
    /// references); captures (`cap_deref`) never do.
    fn takeable_slot(&self, names: &NameCtx, var: &str) -> Option<(String, bool)> {
        if names.cap_deref.contains_key(var) {
            return None;
        }
        if let Some((cid, ctype)) = names.stack.get(var).and_then(|v| v.last()) {
            if ctype == "zz_value" {
                return Some((cid.clone(), true));
            }
            return None;
        }
        if let Some((cid, ctype)) = names.globals.get(var) {
            if ctype == "zz_value" {
                return Some((cid.clone(), false));
            }
            return None;
        }
        let suffix = format!(".{var}");
        let mut hit: Option<(String, bool)> = None;
        for (key, (cid, ctype)) in &names.globals {
            if key.ends_with(suffix.as_str()) && ctype == "zz_value" {
                if hit.is_some() {
                    return None;
                }
                hit = Some((cid.clone(), false));
            }
        }
        hit
    }

    /// Bare `var`, or a head that resolves to it: plain `var`, or a
    /// module-qualified `ns.var` global.
    fn is_var_head(head: &str, var: &str, names: &NameCtx) -> bool {
        if head == var {
            return true;
        }
        head.ends_with(format!(".{var}").as_str()) && names.globals.contains_key(head)
    }

    /// The element expression of a push call classified as [`MoveKind::VecPush`].
    /// Read off the *original* RHS so every other name emits exactly as written.
    fn push_elem<'a>(&self, var: &str, original: &'a Expr, names: &NameCtx) -> Option<&'a Expr> {
        let Expr::Call { callee, args, .. } = original else {
            return None;
        };
        // `vec.push(x, e)` / `std.vec.push(x, e)`.
        if args.len() == 2 {
            let receiver_is_var = match &args[0] {
                Expr::Ident { name, .. } => name == var,
                Expr::Path { parts, .. } => {
                    parts.len() == 2
                        && parts[1] == var
                        && names.globals.contains_key(&parts.join("."))
                }
                _ => false,
            };
            if receiver_is_var {
                return Some(&args[1]);
            }
            return None;
        }
        // `x.push(e)` (Path or Field receiver, possibly qualified).
        if args.len() == 1 {
            let receiver_is_var = match callee.as_ref() {
                Expr::Path { parts, .. } => {
                    parts.len() == 2
                        && parts[1] == "push"
                        && Self::is_var_head(&parts[0], var, names)
                }
                Expr::Field { obj, name, .. } => {
                    name == "push"
                        && match obj.as_ref() {
                            Expr::Ident { name: n, .. } => n == var,
                            Expr::Path { parts, .. } => {
                                parts.len() == 1 && Self::is_var_head(&parts[0], var, names)
                            }
                            _ => false,
                        }
                }
                _ => false,
            };
            if receiver_is_var {
                return Some(&args[0]);
            }
        }
        None
    }

    fn emit_push_take(
        &self,
        var: &str,
        original: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) -> bool {
        let (cid, _local) = match self.takeable_slot(names, var) {
            Some(slot) => slot,
            None => return false,
        };
        let elem = match self.push_elem(var, original, names) {
            Some(e) => e,
            None => return false,
        };
        // Element first, take second: the element observes the slot while
        // intact, so even impure siblings (calls/closures) are sound here
        // and the take→push→store window runs no user code at all.
        let elem_c = self.emit_expr(elem, names, out);
        let elem_boxed = box_scalar_operand(elem, names, &elem_c);
        let mv = names.fresh("__mv");
        let err = names.fresh("_e");
        out.push_str(&format!("    zz_value {mv} = {cid};\n"));
        out.push_str(&format!("    {cid} = zz_unit();\n"));
        out.push_str(&format!(
            "    {{ int {err} = 0; {cid} = zz_vec_push_take({mv}, {elem_boxed}, &{err}); }}\n"
        ));
        names.invalidate_array_len(var);
        true
    }

    /// `len(s.f)` without the getter's retain: measuring a field must not
    /// bump its array (each leaked share defeats the next move-aware push).
    /// Direct fields answer inline via `zz_len_field`; anything else keeps
    /// the old path. Pure read — same value either way.
    pub(super) fn try_emit_len_field(&self, arg: &Expr, names: &mut NameCtx) -> Option<String> {
        let (obj, field) = match arg {
            Expr::Path { parts, .. } if parts.len() == 2 => (parts[0].as_str(), parts[1].as_str()),
            Expr::Field { obj, name, .. } => match obj.as_ref() {
                Expr::Ident { name: o, .. } => (o.as_str(), name.as_str()),
                _ => return None,
            },
            _ => return None,
        };
        let cid = Self::obj_cid(names, obj)?;
        let err = names.fresh("_e");
        Some(format!(
            "({{ int {err} = 0; zz_len_field(&{cid}, \"{field}\", &{err}); }})"
        ))
    }

    /// Checker struct name for a possibly module-qualified object name.
    fn obj_struct(&self, names: &NameCtx, obj: &str) -> Option<String> {
        if let Some(zz_checker::Type::Struct(sname, _)) = names.checker_types.get(obj).cloned() {
            return Some(sname);
        }
        // Globals are keyed qualified (`ns.s`); the checker map may hold
        // the bare name.
        if let Some(tail) = obj.rsplit('.').next() {
            if let Some(zz_checker::Type::Struct(sname, _)) = names.checker_types.get(tail).cloned()
            {
                return Some(sname);
            }
        }
        None
    }

    /// Object C identifier for a possibly module-qualified name.
    pub(super) fn obj_cid(names: &NameCtx, obj: &str) -> Option<String> {
        if let Some(cid) = names.lookup(obj).map(str::to_string) {
            return Some(cid);
        }
        let suffix = format!(".{obj}");
        let mut hit = None;
        for (key, (cid, _)) in &names.globals {
            if key.ends_with(suffix.as_str()) {
                if hit.is_some() {
                    return None;
                }
                hit = Some(cid.clone());
            }
        }
        hit
    }

    fn emit_field_push_take(
        &self,
        obj: &str,
        field: &str,
        original: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) -> bool {
        let base_cid = match Self::obj_cid(names, obj) {
            Some(cid) => cid,
            None => return false,
        };
        // Boxed structs only: raw unboxed locals (`zz_struct_*` C type)
        // store fields inline and never reach the object helpers. The
        // condition mirrors the boxed field-assignment path in `stmt.rs`.
        let sname = match self.obj_struct(names, obj) {
            Some(s) => s,
            None => return false,
        };
        let base_is_raw = names
            .lookup_type(obj)
            .map(|t| t.starts_with("zz_struct_"))
            .unwrap_or(false);
        if base_is_raw {
            return false;
        }
        if !self
            .resolve_access_chain(&sname, &[field.to_string()])
            .is_some_and(|(chain, _)| chain.len() == 1)
        {
            return false;
        }
        // Receiver of `vec.push(s.f, e)` is args[0] (method spellings never
        // reach `FieldPush`: the classifier only builds it for Ident callees).
        let elem = match original {
            Expr::Call { args, .. } if args.len() == 2 => &args[1],
            _ => return false,
        };
        let elem_c = self.emit_expr(elem, names, out);
        let elem_boxed = box_scalar_operand(elem, names, &elem_c);
        let err = names.fresh("_e");
        out.push_str(&format!(
            "    {{ int {err} = 0; zz_object_push_field_take(&{base_cid}, \"{field}\", {elem_boxed}, &{err}); }}\n"
        ));
        true
    }

    /// Clone `value` with exactly the qualified references to this `var`'s
    /// global rewritten to the bare name (everything else untouched), for
    /// emission under the moved scope. Returns the original when nothing
    /// qualified names this var.
    fn thread_emit_tree(&self, var: &str, value: &Expr, names: &NameCtx) -> Expr {
        let suffix = format!(".{var}");
        let mut key: Option<String> = None;
        for k in names.globals.keys() {
            if k.ends_with(suffix.as_str()) {
                key = Some(k.clone());
                break;
            }
        }
        match key {
            Some(k) => unqualify_expr(value, &|q| q == k).unwrap_or_else(|| value.clone()),
            None => value.clone(),
        }
    }

    fn emit_thread_call_take(
        &self,
        var: &str,
        original: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) -> bool {
        // Stack locals only: plain functions can read module globals, so a
        // taken global would be observable mid-call. Cell-shared and
        // captured locals are excluded for the same reason.
        let (cid, is_local) = match self.takeable_slot(names, var) {
            Some(slot) => slot,
            None => return false,
        };
        if !is_local || names.is_owner_cell(var) || names.capture_set.contains(var) {
            return false;
        }
        let tree = self.thread_emit_tree(var, original, names);
        let mv = names.fresh("__mv");
        out.push_str(&format!("    zz_value {mv} = {cid};\n"));
        out.push_str(&format!("    {cid} = zz_unit();\n"));
        // Bind `var` to the moved temp without a clone bump for exactly
        // this call emission, then restore.
        let pushed = match names.stack.get_mut(var) {
            Some(stack) => {
                stack.push((mv.clone(), MOVED_TYPE.to_string()));
                true
            }
            None => false,
        };
        if !pushed {
            return false;
        }
        let call_c = self.emit_expr(&tree, names, out);
        if let Some(stack) = names.stack.get_mut(var) {
            stack.pop();
        }
        // Plain move: the slot holds unit, so no release is needed and no
        // retain either — the call result's share transfers to the slot.
        out.push_str(&format!("    {cid} = {call_c};\n"));
        names.invalidate_array_len(var);
        true
    }
}
