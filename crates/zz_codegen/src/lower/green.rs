//! Green (suspendable) closures — Phase B3 state-machine lowering.
//!
//! A closure whose body suspends at `chan.recv` / `task.join` instead of
//! parking the OS thread lowers to an explicit state machine: every local
//! lives in a heap cell owned by the task frame, each blocking call site
//! gets a resume label, and suspending returns to the executor
//! trampoline (which resumes the task when the value is handed off).
//!
//! Eligibility is decided by a pure pre-scan ([`closure_green_eligible`])
//! before emission; ambiguous shapes fall back to the blocking path
//! (always correct, just parks the thread). Nested closures inside a
//! green closure are never green themselves (they would share the
//! frame's resume id space).

use zz_frontend::ast::{Expr, FmtPart, Stmt};

use super::{Lowerer, NameCtx};

/// Dotted callee names that suspend the task when lowered green.
const GREEN_BLOCKING: &[&str] = &[
    "chan.recv",
    "std.chan.recv",
    "task.recv",
    "std.task.recv",
    "task.join",
    "std.task.join",
];

/// Per-closure state for an in-progress green transform. Lives in
/// [`Lowerer::green`] while the closure body is emitted.
pub(crate) struct GreenCtx {
    /// Next frame slot index (cell storage).
    pub(super) next_slot: usize,
    /// Next resume label id (ids start at 1; 0 = fresh entry).
    pub(super) next_resume: usize,
    /// Frame cells in allocation order: (gcell counter, slot, C type).
    /// Drives the declaration + restore placeholder expansion.
    pub(super) cells: Vec<(usize, usize, String)>,
    /// Resume ids with labels, in order. Drives the dispatch switch.
    pub(super) cases: Vec<usize>,
}

impl GreenCtx {
    pub(crate) fn new() -> Self {
        GreenCtx {
            next_slot: 0,
            next_resume: 1,
            cells: Vec::new(),
            cases: Vec::new(),
        }
    }
}

/// Resolve a call callee to a dotted name for green matching. Handles
/// only the static shapes (`Ident` / `Path`); method-on-value and exotic
/// callees return `None` (callers treat those conservatively).
fn callee_dotted(callee: &Expr) -> Option<String> {
    match callee {
        Expr::Ident { name, .. } => Some(name.clone()),
        Expr::Path { parts, .. } => Some(parts.join(".")),
        _ => None,
    }
}

/// True when `callee` is a suspendable blocking point (`chan.recv`,
/// `task.join` / `task.recv`, with or without the `std.` prefix).
pub(super) fn green_blocking_callee(callee: &Expr) -> bool {
    match callee_dotted(callee) {
        Some(n) => GREEN_BLOCKING.contains(&n.as_str()),
        None => false,
    }
}

/// True when a call must go through a closure value at runtime (the
/// callee is not statically known): any non-`Ident`/`Path` callee, or an
/// `Ident` that names neither a user function, a suspendable point, a
/// native, nor a builtin. Indirect calls are the one shape a green
/// closure cannot contain: the callee might itself suspend on the shared
/// frame, and its resume id would collide with the caller's.
fn call_is_indirect(lower: &Lowerer, callee: &Expr) -> bool {
    match callee {
        Expr::Ident { name, .. } => {
            if lower.tp.funcs.contains_key(name) {
                return false;
            }
            if GREEN_BLOCKING.contains(&name.as_str()) {
                return false;
            }
            let std_name = format!("std.{name}");
            if lower.tp.funcs.contains_key(&std_name) {
                return false;
            }
            if super::native_supported(name) || super::native_supported(&std_name) {
                return false;
            }
            if lower.reachable_natives.contains(name) || lower.reachable_natives.contains(&std_name)
            {
                return false;
            }
            // Bare builtins (`range`, `len`, ...) never suspend.
            if name == "range" || name == "len" {
                return false;
            }
            true
        }
        Expr::Path { parts, .. } => {
            let joined = parts.join(".");
            if lower.tp.funcs.contains_key(&joined) {
                return false;
            }
            if GREEN_BLOCKING.contains(&joined.as_str()) {
                return false;
            }
            if super::native_supported(&joined) || lower.reachable_natives.contains(&joined) {
                return false;
            }
            // `head.method(...)` on a value (e.g. `arr.push(x)`) dispatches
            // to a native or user function — neither suspends across the
            // call boundary (blocking happens on the parked thread inside,
            // which is correct, just slower).
            if parts.len() == 2 {
                return false;
            }
            // Longer unknown paths: conservative taint.
            true
        }
        // Field/exotic callees: rare; taint rather than analyze.
        _ => true,
    }
}

/// True when `e` is directly a suspendable call (the statement-level
/// shapes the green yield interception handles).
fn is_direct_blocking_call(e: &Expr) -> bool {
    match e {
        Expr::Call { callee, .. } => green_blocking_callee(callee),
        _ => false,
    }
}

struct Prescan {
    found_yield: bool,
    tainted: bool,
}

impl Prescan {
    fn expr(&mut self, lower: &Lowerer, e: &Expr, direct: bool) {
        match e {
            Expr::Call { callee, args, .. } => {
                // `sqlz.transaction` inlines its closure body into the
                // caller's frame: a green yield inside would suspend with
                // a transaction open and confuse resume ownership. Taint
                // (blocking fallback holds the txn on a parked thread).
                if let Some(dotted) = callee_dotted(callee) {
                    if dotted.ends_with("transaction") {
                        self.tainted = true;
                    }
                }
                if direct && is_direct_blocking_call(e) {
                    self.found_yield = true;
                }
                if call_is_indirect(lower, callee) {
                    self.tainted = true;
                }
                for a in args {
                    self.expr(lower, a, false);
                }
            }
            // Nested closures are separate compilation units: the active-
            // green rule forces them non-green at emission. Skip here.
            Expr::Closure { .. } => {}
            Expr::Block(b) => self.block(lower, b),
            Expr::If {
                cond, then, els, ..
            } => {
                self.expr(lower, cond, false);
                self.block(lower, then);
                if let Some(el) = els {
                    self.expr(lower, el, false);
                }
            }
            Expr::While { cond, body, .. } => {
                self.expr(lower, cond, false);
                self.block(lower, body);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(lower, scrutinee, false);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(lower, g, false);
                    }
                    self.expr(lower, &arm.body, false);
                }
            }
            Expr::IfLet {
                value, then, els, ..
            } => {
                self.expr(lower, value, false);
                self.block(lower, then);
                if let Some(el) = els {
                    self.expr(lower, el, false);
                }
            }
            Expr::Binary { left, right, .. } => {
                self.expr(lower, left, false);
                self.expr(lower, right, false);
            }
            Expr::Unary { expr, .. } => self.expr(lower, expr, false),
            Expr::Paren { expr, .. } => self.expr(lower, expr, false),
            Expr::Field { obj, .. } => self.expr(lower, obj, false),
            Expr::Index { obj, index, .. } => {
                self.expr(lower, obj, false);
                self.expr(lower, index, false);
            }
            Expr::Array { elems, .. } => {
                for el in elems {
                    self.expr(lower, el, false);
                }
            }
            Expr::Tuple { items, .. } => {
                for el in items {
                    self.expr(lower, el, false);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.expr(lower, k, false);
                    self.expr(lower, v, false);
                }
            }
            Expr::StructInit { fields, .. } => {
                for (_, v) in fields {
                    self.expr(lower, v, false);
                }
            }
            Expr::Variant { arg: Some(p), .. } => self.expr(lower, p, false),
            Expr::Variant { .. } => {}
            Expr::Range { start, end, .. } => {
                self.expr(lower, start, false);
                self.expr(lower, end, false);
            }
            Expr::ListComp {
                body, iter, filter, ..
            } => {
                self.expr(lower, body, false);
                self.expr(lower, iter, false);
                if let Some(f) = filter {
                    self.expr(lower, f, false);
                }
            }
            Expr::Try { expr, .. } => self.expr(lower, expr, false),
            Expr::Fmt { parts, .. } => {
                for p in parts {
                    if let FmtPart::Expr(inner, _) = p {
                        self.expr(lower, inner, false);
                    }
                }
            }
            _ => {}
        }
    }

    fn block(&mut self, lower: &Lowerer, b: &zz_frontend::ast::Block) {
        for s in &b.stmts {
            self.stmt(lower, s);
        }
    }

    fn stmt(&mut self, lower: &Lowerer, s: &Stmt) {
        match s {
            Stmt::Decl { value, .. } => self.expr(lower, value, true),
            Stmt::Assign { target, value, .. } => {
                // A blocking RHS stored through a non-plain target
                // (field/index) cannot resume the base-address
                // computation: taint the whole closure (blocking
                // fallback parks the thread with the stack intact).
                if is_direct_blocking_call(value) && !matches!(target, Expr::Ident { .. }) {
                    self.tainted = true;
                }
                self.expr(lower, target, false);
                self.expr(lower, value, true);
            }
            Stmt::Expr(e) => self.expr(lower, e, true),
            Stmt::For { iter, body, .. } => {
                self.expr(lower, iter, false);
                self.block(lower, body);
            }
            Stmt::Return { value: Some(v), .. } => self.expr(lower, v, false),
            Stmt::Return { .. } => {}
            Stmt::Defer { .. } => {
                // Deferred snippets run on stack state at scope exit;
                // unsound across a suspend. Taint (blocking fallback).
                self.tainted = true;
            }
            // Nested named items are separate units (never green).
            Stmt::Func { .. } | Stmt::Struct { .. } | Stmt::Impl { .. } => {}
            _ => {}
        }
    }
}

/// Decide whether a closure literal lowers to a suspendable state
/// machine. Eligible when its body holds at least one statement-level
/// blocking call and nothing the transform cannot model (indirect calls,
/// `defer`, transaction inlining, exotic assign targets). Everything
/// else keeps the blocking lowering: always correct, just parks.
pub(super) fn closure_green_eligible(lower: &Lowerer, body: &Expr) -> bool {
    // A green closure already in progress forces nested closures
    // non-green: they would share the frame's resume id space.
    if lower.green.borrow().is_some() {
        return false;
    }
    let mut ps = Prescan {
        found_yield: false,
        tainted: false,
    };
    ps.expr(lower, body, false);
    ps.found_yield && !ps.tainted
}

impl Lowerer {
    /// True while a green closure body is being emitted.
    pub(super) fn green_active(&self) -> bool {
        self.green.borrow().is_some()
    }

    /// Declare a green frame cell for one binding of C type `ctype`,
    /// emitting its allocation + frame registration. Returns the
    /// pointer/deref exprs; the caller registers them in `NameCtx`
    /// (via `enter_cell`) and emits the initializer store.
    ///
    /// `fresh` selects per-execution allocation (fresh heap cell every
    /// time the declaration site runs — mirroring the blocking path's
    /// per-iteration cells, required when nested closures may capture
    /// the binding) versus reuse (allocate once, keep the cell —
    /// leak-free for uncaptured locals). The pointer is always stored
    /// in the frame slot so the prologue restore re-seats the stack
    /// pointer after a resume jumps over the declaration.
    pub(super) fn green_cell(
        &self,
        names: &mut NameCtx,
        ctype: &str,
        fresh: bool,
        out: &mut String,
    ) -> (String, String, usize) {
        let mut g = self.green.borrow_mut();
        let gx = g.as_mut().expect("green_cell outside a green closure");
        let slot = gx.next_slot;
        gx.next_slot += 1;
        let n = names.bump_counter();
        gx.cells.push((n, slot, ctype.to_string()));
        let ptr = format!("_gcell{n}");
        let deref = format!("(*{ptr})");
        let kind = if ctype == "zz_value" { 0 } else { 1 };
        if fresh {
            out.push_str(&format!("    {ptr} = ({ctype}*)malloc(sizeof({ctype}));\n"));
        } else {
            out.push_str(&format!("    if (!{ptr}) {{\n"));
            out.push_str(&format!(
                "        {ptr} = ({ctype}*)malloc(sizeof({ctype}));\n"
            ));
            out.push_str("    }\n");
        }
        out.push_str(&format!("    zz_fr->cells[{slot}] = {ptr};\n"));
        out.push_str(&format!("    zz_fr->cell_kind[{slot}] = {kind};\n"));
        out.push_str(&format!(
            "    zz_fr->cell_size[{slot}] = sizeof({ctype});\n"
        ));
        (ptr, deref, n)
    }

    /// Allocate a green temp slot (in-flight yield values). Same storage
    /// as user cells, but reused across executions (never captured, never
    /// in `NameCtx`): the declaration + restore placeholders cover it via
    /// the shared cells list.
    pub(super) fn green_temp(&self, names: &mut NameCtx, out: &mut String) -> String {
        let mut g = self.green.borrow_mut();
        let gx = g.as_mut().expect("green_temp outside a green closure");
        let slot = gx.next_slot;
        gx.next_slot += 1;
        let n = names.bump_counter();
        gx.cells.push((n, slot, "zz_value".to_string()));
        let ptr = format!("_gcell{n}");
        out.push_str(&format!("    if (!{ptr}) {{\n"));
        out.push_str(&format!(
            "        {ptr} = (zz_value*)malloc(sizeof(zz_value));\n"
        ));
        out.push_str(&format!("    zz_fr->cells[{slot}] = {ptr};\n"));
        out.push_str(&format!("    zz_fr->cell_kind[{slot}] = 0;\n"));
        out.push_str(&format!(
            "    zz_fr->cell_size[{slot}] = sizeof(zz_value);\n"
        ));
        out.push_str("    }\n");
        format!("(*{ptr})")
    }

    /// Expand the green placeholders (`/*GREEN_NSLOTS*/`,
    /// `/*GREEN_DECLS*/`, `/*GREEN_RESTORE*/`, `/*GREEN_DISPATCH*/`) in a
    /// finished closure body and pop the transform state.
    pub(super) fn green_finish(&self, body: &mut String) {
        let gx = self.green.borrow_mut().take().expect("green_finish idle");
        let nslots = gx.next_slot;
        let mut decls = String::new();
        let mut restore = String::new();
        for (n, slot, ctype) in &gx.cells {
            decls.push_str(&format!("    {ctype} *_gcell{n};\n"));
            restore.push_str(&format!(
                "    _gcell{n} = ({ctype}*)zz_fr->cells[{slot}];\n"
            ));
        }
        let mut dispatch = String::new();
        for k in &gx.cases {
            dispatch.push_str(&format!("    case {k}: goto green_L_{k};\n"));
        }
        *body = body.replace("/*GREEN_NSLOTS*/", &nslots.to_string());
        *body = body.replace("/*GREEN_DECLS*/", &decls);
        *body = body.replace("/*GREEN_RESTORE*/", &restore);
        *body = body.replace("/*GREEN_DISPATCH*/", &dispatch);
    }

    /// Take a resume id for one yield site, recording its dispatch case.
    /// Returns the id `K` used by the `green_L_K` / `green_done_K` labels.
    pub(super) fn green_resume_id(&self) -> usize {
        let mut g = self.green.borrow_mut();
        let gx = g.as_mut().expect("green_resume_id outside a green closure");
        let k = gx.next_resume;
        gx.next_resume += 1;
        gx.cases.push(k);
        k
    }
}
