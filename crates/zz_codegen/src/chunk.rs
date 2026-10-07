//! Chunk→C backend: compile a verified IR [`Module`][zz_ir] to C.
//!
//! The AOT-from-chunk slice of the unified pipeline. Every core op maps
//! to a small C fragment against the same runtime the HIR backend uses;
//! arithmetic, comparison, branches, and POD stay inline C (UB-free —
//! the build uses `-fwrapv`, matching the spec's wrapping rules), while
//! handle-backed and heavy builtins route through `zz_native_rt`/C impls.
//!
//! Execution model: one C function per IR function over a value array
//! (`st[]`). Slots live at `[0..nslots)` (params seated first), the
//! operand stack grows above them — the same layout as the VM frame, so
//! slot indices transfer verbatim. Ownership discipline: every stack
//! slot holds an owned reference (push clones, pop-discard releases,
//! stores assign + release the temp); runtime calls borrow unless named
//! `take` (which move). Frame teardown retains the return value and
//! releases everything else, so even error paths stay balanced.
//!
//! Two emission modes per function:
//! - **boxed** (default): full `zz_value` traffic, all types.
//! - **unboxed int twin** (`_u` suffix): when the signature is
//!   all-`int`→`int` and every op is int-closed, the body compiles to
//!   raw `int64_t` traffic with direct recursive calls. Boxed callers
//!   unbox through the wrapper (checker-guaranteed ints, unchecked like
//!   the HIR backend). This is what keeps `fib`/`tak` within 5% of the
//!   HIR backend's own unboxed twins.
//!
//! Coverage: [`coverage`] gates the parity harness — anything else is a
//! clean build error naming the op (never a miscompile).

use std::collections::{HashMap, HashSet};

use zz_ir::{Const, ConstId, FuncDef, FuncId, Module, Op, StrId};

use super::lower::mangle;

/// A build failure: always a coverage gap (unsupported op/shape), never
/// silent. Messages name the offending construct for the coverage tool.
#[derive(Debug, Clone)]
pub struct ChunkError {
    pub message: String,
}

impl ChunkError {
    fn op(what: &str) -> Self {
        ChunkError {
            message: format!("chunk backend: unsupported {what}"),
        }
    }

    fn name(what: &str) -> Self {
        ChunkError {
            message: format!("chunk backend: cannot resolve {what}"),
        }
    }
}

impl std::fmt::Display for ChunkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ChunkError {}

/// True when this backend can compile `op` (call shapes and name
/// resolution are checked separately by [`coverage`]).
pub fn supported(op: &Op) -> bool {
    matches!(
        op,
        Op::PushConst(_)
            | Op::Pop
            | Op::PopN(_)
            | Op::Swap
            | Op::Truthy
            | Op::LoadSlot(_)
            | Op::StoreSlot(_)
            | Op::LoadVar(_)
            | Op::IntAdd
            | Op::IntSub
            | Op::IntMul
            | Op::IntDiv
            | Op::IntRem
            | Op::IntNeg
            | Op::BinOp(_)
            | Op::UnOp(_)
            | Op::Jump(_)
            | Op::JumpIfFalse(_)
            | Op::JumpIfTrue(_)
            | Op::JumpIfFalseBool(_)
            | Op::Return
            | Op::ForSetup { .. }
            | Op::ForNext { .. }
            | Op::WhileSetup { .. }
            | Op::WhileCond { .. }
            | Op::SetLoopResult
            | Op::Safepoint
            | Op::MakeArray(_)
            | Op::ArrayPush
            | Op::IndexOp
            | Op::StoreIndexOp
            | Op::MakeRange
            | Op::MakeFunc { .. }
            | Op::Call { .. }
            | Op::CallPath { .. }
            | Op::CallNative { .. }
    )
}

/// Callee resolution for `Call{argc}` via forward origin tracking: only
/// directly-resolvable callees are covered. The `usize` tags the
/// `LoadVar` pc that pushed the value, so coverage can prove every
/// function/native load is consumed by a call (never observed as a
/// value — the dummy `unit` push must never leak).
#[derive(Debug, Clone, PartialEq)]
enum Callee {
    /// C-impl native (`zz_io_println`, …) loaded at `pc`.
    Native(String, usize),
    /// Own tabled function (dotted IR name) loaded at `pc`.
    Func(String, usize),
    /// Anything else (slots, computed values, unknown names).
    Unknown,
}

/// Per-pc operand-stack origins for `Call` dispatch (entry state).
/// Joins intersect (differing origins collapse to `Unknown`, which fails
/// coverage). `resolve` classifies `LoadVar` pushes (tagged by pc).
fn origins(code: &[Op], resolve: &dyn Fn(StrId, usize) -> Callee) -> Vec<Vec<Callee>> {
    fn targets(op: &Op) -> Vec<u32> {
        match op {
            Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
                vec![*t]
            }
            Op::ForNext { exit, .. } | Op::WhileCond { exit } => vec![*exit],
            _ => Vec::new(),
        }
    }
    fn falls(op: &Op) -> bool {
        !matches!(op, Op::Jump(_) | Op::Return | Op::Break | Op::Continue)
    }
    /// Apply `op` to an entry stack, yielding the exit stack.
    /// Exact pop/push counts (net effects lie for calls); unknown
    /// pushes are `Unknown` except resolved `LoadVar`.
    /// `pc` tags `LoadVar` origins for the call-consumption check.
    fn apply(
        op: &Op,
        pc: usize,
        mut st: Vec<Callee>,
        resolve: &dyn Fn(StrId, usize) -> Callee,
    ) -> Vec<Callee> {
        // (pops, pushes); pushes resolve per-op below.
        let (pops, pushes): (usize, usize) = match op {
            Op::PushConst(_) => (0, 1),
            Op::Pop => (1, 0),
            Op::PopN(n) => (*n as usize + 1, 1),
            Op::Swap => (0, 0),
            Op::Truthy => (1, 1),
            Op::LoadSlot(_) | Op::LoadVar(_) => (0, 1),
            Op::StoreSlot(_) => (1, 0),
            Op::IntAdd | Op::IntSub | Op::IntMul | Op::IntDiv | Op::IntRem => (2, 1),
            Op::IntNeg => (1, 1),
            Op::BinOp(_) => (2, 1),
            Op::UnOp(_) => (1, 1),
            Op::Jump(_) => (0, 0),
            Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::JumpIfFalseBool(_) => (1, 0),
            Op::Return => (1, 0),
            Op::ForSetup { num_vars, .. } => (1, 2 + *num_vars as usize),
            Op::ForNext { vars, .. } => (vars.len() + 1, vars.len() + 1),
            Op::WhileSetup { .. } => (0, 0),
            Op::WhileCond { .. } => (1, 0),
            Op::Break | Op::Continue => (0, 0),
            Op::SetLoopResult => (1, 0),
            Op::Safepoint => (0, 0),
            Op::MakeArray(n) => (*n as usize, 1),
            Op::ArrayPush => (2, 1),
            Op::IndexOp => (2, 1),
            Op::StoreIndexOp => (3, 1),
            Op::MakeRange => (2, 1),
            Op::MakeFunc { .. } => (0, 1),
            Op::Call { argc } => (*argc as usize + 1, 1),
            Op::CallPath { argc, .. } | Op::CallNative { argc, .. } => (*argc as usize, 1),
            _ => (0, 0),
        };
        // PopN keeps its top; Swap exchanges the top two.
        if let Op::PopN(_) = op {
            let mut tmp = Vec::new();
            for _ in 0..pops {
                tmp.push(st.pop().unwrap_or(Callee::Unknown));
            }
            let top = tmp.into_iter().next().unwrap_or(Callee::Unknown);
            st.push(top);
            return st;
        }
        if let Op::Swap = op {
            let n = st.len();
            if n >= 2 {
                st.swap(n - 1, n - 2);
            }
            return st;
        }
        // Saturating pops (mirrors the lenient runtime Pop).
        for _ in 0..pops {
            if st.pop().is_none() {
                break;
            }
        }
        for _ in 0..pushes {
            let origin = match op {
                Op::LoadVar(id) => resolve(*id, pc),
                _ => Callee::Unknown,
            };
            st.push(origin);
        }
        st
    }
    let n = code.len();
    let mut out: Vec<Vec<Callee>> = vec![Vec::new(); n];
    let mut state: Vec<Option<Vec<Callee>>> = vec![None; n];
    let mut work = vec![0usize];
    if n == 0 {
        return out;
    }
    state[0] = Some(Vec::new());
    while let Some(pc) = work.pop() {
        let st = match state[pc].clone() {
            Some(st) => st,
            None => continue,
        };
        // Entry state (what the op observes — what `Call` needs).
        out[pc] = st.clone();
        let next = apply(&code[pc], pc, st, resolve);
        let mut succs = Vec::new();
        if falls(&code[pc]) && pc + 1 < n {
            succs.push(pc + 1);
        }
        for t in targets(&code[pc]) {
            if (t as usize) < n {
                succs.push(t as usize);
            }
        }
        for s in succs {
            match &state[s] {
                None => {
                    state[s] = Some(next.clone());
                    work.push(s);
                }
                Some(cur) => {
                    let mut merged = Vec::new();
                    let len = cur.len().max(next.len());
                    let mut changed = false;
                    for i in 0..len {
                        let (a, b) = (
                            cur.get(i).unwrap_or(&Callee::Unknown),
                            next.get(i).unwrap_or(&Callee::Unknown),
                        );
                        let m = if a == b { a.clone() } else { Callee::Unknown };
                        if Some(&m) != cur.get(i) {
                            changed = true;
                        }
                        merged.push(m);
                    }
                    if changed {
                        state[s] = Some(merged);
                        work.push(s);
                    }
                }
            }
        }
    }
    out
}

/// Store-index fusion: `LoadSlot(s) … StoreIndexOp StoreSlot(s)` with a
/// straight-line, call-free middle compiles to an in-place slot store
/// (like the HIR backend) instead of clone → detach-dup → store-back
/// (which is O(n) per store — quadratic for loop-carried arrays like
/// sieve's bitset).
///
/// Soundness: the abstract stack proves the consumed object is exactly
/// the `LoadSlot(s)` clone; no `StoreSlot(s)`/control-flow/labels sit
/// between (so `st[s]` still holds the same buffer); other owners only
/// cost a detach-dup, never correctness (the clone share releases
/// first, restoring uniqueness in the common two-owner case).
#[derive(Debug, Clone, Copy, PartialEq)]
enum PushTag {
    Slot(u16, usize),
    Other,
}

/// Returns `(fused_stores, skip_pcs)`: `fused_stores[pc] = s` means the
/// `StoreIndexOp` at `pc` fuses onto slot `s`; `skip_pcs` holds the
/// following `StoreSlot(s)` pcs to skip.
fn fuse_scan(code: &[Op]) -> (HashMap<usize, u16>, HashSet<usize>) {
    // (pops, pushes) — mirrors `origins::apply` for the covered subset.
    fn effect(op: &Op) -> Option<(usize, usize)> {
        Some(match op {
            Op::PushConst(_) => (0, 1),
            Op::Pop => (1, 0),
            Op::PopN(n) => (*n as usize + 1, 1),
            Op::Swap => (0, 0),
            Op::Truthy => (1, 1),
            Op::LoadSlot(_) | Op::LoadVar(_) => (0, 1),
            Op::StoreSlot(_) => (1, 0),
            Op::IntAdd | Op::IntSub | Op::IntMul | Op::IntDiv | Op::IntRem => (2, 1),
            Op::IntNeg => (1, 1),
            Op::BinOp(_) => (2, 1),
            Op::UnOp(_) => (1, 1),
            Op::Jump(_) | Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::JumpIfFalseBool(_) => (1, 0),
            Op::Return => (1, 0),
            Op::ForSetup { num_vars, .. } => (1, 2 + *num_vars as usize),
            Op::ForNext { vars, .. } => (vars.len() + 1, vars.len() + 1),
            Op::WhileSetup { .. } => (0, 0),
            Op::WhileCond { .. } => (1, 0),
            Op::SetLoopResult => (1, 0),
            Op::Safepoint => (0, 0),
            Op::MakeArray(n) => (*n as usize, 1),
            Op::ArrayPush => (2, 1),
            Op::IndexOp => (2, 1),
            Op::StoreIndexOp => (3, 1),
            Op::MakeRange => (2, 1),
            Op::MakeFunc { .. } => (0, 1),
            Op::Call { argc } => (*argc as usize + 1, 1),
            Op::CallPath { argc, .. } | Op::CallNative { argc, .. } => (*argc as usize, 1),
            _ => return None,
        })
    }
    // Abstract stacks at each pc (entry states). Bail out (empty map)
    // on anything unmodeled — fusion is an optimization, never required.
    let n = code.len();
    let mut at: Vec<Option<Vec<PushTag>>> = vec![None; n];
    if n == 0 {
        return (HashMap::new(), HashSet::new());
    }
    // Linear scan suffices: fusion regions are straight-line by
    // construction (validated below); joins just need *a* sound state,
    // so intersect like origins (differing tags -> Other).
    let mut state: Vec<PushTag> = Vec::new();
    let mut entry: Vec<Vec<PushTag>> = vec![Vec::new(); n];
    // Iterate to a fixpoint (bounded: tags only degrade to Other).
    for _ in 0..n + 2 {
        let mut changed = false;
        state.clear();
        for (pc, op) in code.iter().enumerate() {
            // Merge on revisit: intersection degrades.
            if let Some(prev) = &at[pc] {
                let mut merged = Vec::new();
                let len = prev.len().max(state.len());
                for i in 0..len {
                    let (a, b) = (
                        prev.get(i).copied().unwrap_or(PushTag::Other),
                        state.get(i).copied().unwrap_or(PushTag::Other),
                    );
                    merged.push(match (a, b) {
                        (PushTag::Slot(s, p), PushTag::Slot(s2, p2)) if s == s2 && p == p2 => {
                            PushTag::Slot(s, p)
                        }
                        _ => PushTag::Other,
                    });
                }
                if &merged != prev {
                    at[pc] = Some(merged.clone());
                    changed = true;
                }
                state = merged;
            } else {
                at[pc] = Some(state.clone());
            }
            entry[pc] = state.clone();
            let Some((pops, pushes)) = effect(op) else {
                return (HashMap::new(), HashSet::new());
            };
            if let Op::PopN(_) = op {
                let mut tmp = Vec::new();
                for _ in 0..pops {
                    tmp.push(state.pop().unwrap_or(PushTag::Other));
                }
                let top = tmp.into_iter().next().unwrap_or(PushTag::Other);
                state.push(top);
                continue;
            }
            if let Op::Swap = op {
                let m = state.len();
                if m >= 2 {
                    state.swap(m - 1, m - 2);
                }
                continue;
            }
            // ForNext pushes fresh (index + vars are new values).
            if matches!(op, Op::ForNext { .. }) {
                for _ in 0..pops {
                    state.pop();
                }
                for _ in 0..pushes {
                    state.push(PushTag::Other);
                }
                continue;
            }
            for _ in 0..pops {
                if state.pop().is_none() {
                    break;
                }
            }
            for _ in 0..pushes {
                match op {
                    Op::LoadSlot(s) => state.push(PushTag::Slot(*s, pc)),
                    _ => state.push(PushTag::Other),
                }
            }
            // Control flow ends linearity — stop the scan here is
            // wrong (later pcs need states); the fixpoint loop above
            // re-walks, but successors after a Jump are unreachable
            // linearly... For fusion purposes unreachable code needs
            // no state: keep scanning (tags degrade soundly).
        }
        if !changed {
            break;
        }
    }
    // Jump/label inventory.
    let mut is_target: HashSet<usize> = HashSet::new();
    for op in code {
        match op {
            Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
                is_target.insert(*t as usize);
            }
            Op::ForSetup { exit, header, .. } => {
                is_target.insert(*exit as usize);
                is_target.insert(*header as usize);
            }
            Op::ForNext { exit, .. } | Op::WhileSetup { exit, .. } | Op::WhileCond { exit } => {
                is_target.insert(*exit as usize);
            }
            _ => {}
        }
    }
    let has_flow = |op: &Op| -> bool {
        matches!(
            op,
            Op::Jump(_)
                | Op::JumpIfFalse(_)
                | Op::JumpIfTrue(_)
                | Op::JumpIfFalseBool(_)
                | Op::ForSetup { .. }
                | Op::ForNext { .. }
                | Op::WhileSetup { .. }
                | Op::WhileCond { .. }
                | Op::Return
                | Op::Call { .. }
                | Op::CallPath { .. }
                | Op::CallNative { .. }
                | Op::SetLoopResult
                | Op::StoreIndexOp
        )
    };
    let mut fused: HashMap<usize, u16> = HashMap::new();
    let mut skip: HashSet<usize> = HashSet::new();
    for (pc, op) in code.iter().enumerate() {
        if !matches!(op, Op::StoreIndexOp) {
            continue;
        }
        // Object tag is third from top in the entry state.
        let st = &entry[pc];
        if st.len() < 3 {
            continue;
        }
        let PushTag::Slot(s, lpc) = st[st.len() - 3] else {
            continue;
        };
        // Immediately followed by StoreSlot(s)?
        let Op::StoreSlot(s2) = code.get(pc + 1).unwrap_or(&Op::Safepoint) else {
            continue;
        };
        if *s2 != s {
            continue;
        }
        // Middle must be straight-line, call-free, slot-s clean, and
        // contain no labels (no entry from elsewhere).
        let mut ok = true;
        for (off, mid) in code[lpc..=pc].iter().enumerate() {
            let q = lpc + off;
            if q != lpc && q != pc && is_target.contains(&q) {
                ok = false;
                break;
            }
            if q == lpc || q == pc {
                continue;
            }
            if has_flow(mid) {
                ok = false;
                break;
            }
            if matches!(mid, Op::StoreSlot(t) if *t == s) {
                ok = false;
                break;
            }
        }
        if ok {
            fused.insert(pc, s);
            skip.insert(pc + 1);
        }
    }
    (fused, skip)
}
/// Coverage verdict for one module: every op supported, no default args
/// (arity fill needs caller-env evaluation), and every `Call`/`CallPath`
/// statically resolvable. Fails with the first gap found.
pub fn coverage(module: &Module) -> Result<(), ChunkError> {
    // Index funcs by dotted name for resolution.
    let mut func_names: HashSet<String> = HashSet::new();
    for f in &module.funcs {
        let name = module
            .strings
            .get(f.name.0 as usize)
            .ok_or_else(|| ChunkError::op("func name id"))?;
        func_names.insert(name.clone());
    }
    let resolve_load = |id: StrId, pc: usize| -> Callee {
        let name = match module.strings.get(id.0 as usize) {
            Some(s) => s,
            None => return Callee::Unknown,
        };
        if super::lower::native_impl(name).is_some() || super::ffi_impl(name).is_some() {
            return Callee::Native(name.clone(), pc);
        }
        // `std.` twin spelling (e.g. `std.io.println`).
        let std_name = format!("std.{name}");
        if super::lower::native_impl(&std_name).is_some() || super::ffi_impl(&std_name).is_some() {
            return Callee::Native(std_name, pc);
        }
        if func_names.contains(name) {
            return Callee::Func(name.clone(), pc);
        }
        Callee::Unknown
    };
    for f in &module.funcs {
        for p in &f.params {
            if p.default.is_some() {
                let name = module
                    .strings
                    .get(f.name.0 as usize)
                    .map(String::as_str)
                    .unwrap_or("?");
                return Err(ChunkError::op(&format!("default args in {name}")));
            }
        }
        for op in &f.code {
            if !supported(op) {
                return Err(ChunkError::op(&format!(
                    "{} (func {:?})",
                    op.name(),
                    f.name
                )));
            }
        }
        // Resolve every Call's callee origin.
        let org = origins(&f.code, &resolve_load);
        // LoadVar pcs consumed as Call callees (only legal observation
        // of a function/native value in this slice).
        let mut consumed: HashSet<usize> = HashSet::new();
        for (pc, op) in f.code.iter().enumerate() {
            if let Op::Call { argc } = op {
                // Entry state at the Call: [..., callee, args].
                let st = &org[pc];
                if st.len() < (*argc as usize) + 1 {
                    return Err(ChunkError::op("call stack underflow"));
                }
                match &st[st.len() - (*argc as usize) - 1] {
                    Callee::Native(_, lpc) | Callee::Func(_, lpc) => {
                        consumed.insert(*lpc);
                    }
                    _ => {
                        let name = module
                            .strings
                            .get(f.name.0 as usize)
                            .map(String::as_str)
                            .unwrap_or("?");
                        return Err(ChunkError::op(&format!(
                            "unresolvable Call callee in {name} at {pc}"
                        )));
                    }
                }
            }
            if let Op::BinOp(zz_ir::op::BinOp::Elvis) = op {
                return Err(ChunkError::op("elvis binop"));
            }
            if let Op::ForNext { in_env: true, .. } = op {
                return Err(ChunkError::op("for loop with captured env vars"));
            }
            if let Op::CallPath { parts, .. } = op {
                let joined = parts
                    .iter()
                    .map(|id| {
                        module
                            .strings
                            .get(id.0 as usize)
                            .cloned()
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                if !func_names.contains(&joined)
                    && super::lower::native_impl(&joined).is_none()
                    && super::ffi_impl(&joined).is_none()
                {
                    // `std.` twin spelling.
                    let std_name = format!("std.{joined}");
                    if !func_names.contains(&std_name)
                        && super::lower::native_impl(&std_name).is_none()
                        && super::ffi_impl(&std_name).is_none()
                    {
                        return Err(ChunkError::name(&format!("callpath {joined}")));
                    }
                }
            }
            if let Op::CallNative { name, .. } = op {
                let nm = module
                    .strings
                    .get(name.0 as usize)
                    .map(String::as_str)
                    .unwrap_or("");
                if super::lower::native_impl(nm).is_none() && super::ffi_impl(nm).is_none() {
                    // `std.` twin spelling.
                    let std_name = format!("std.{nm}");
                    if super::lower::native_impl(&std_name).is_none()
                        && super::ffi_impl(&std_name).is_none()
                    {
                        return Err(ChunkError::name(&format!("native {nm}")));
                    }
                }
            }
        }
        // Every function/native load must be call-consumed: otherwise the
        // dummy `unit` would leak into observable values. Unknown loads
        // (globals, captures) are rejected outright.
        for (pc, op) in f.code.iter().enumerate() {
            if let Op::LoadVar(id) = op {
                match resolve_load(*id, pc) {
                    Callee::Unknown => {
                        let name = module
                            .strings
                            .get(f.name.0 as usize)
                            .map(String::as_str)
                            .unwrap_or("?");
                        let var = module
                            .strings
                            .get(id.0 as usize)
                            .map(String::as_str)
                            .unwrap_or("?");
                        return Err(ChunkError::op(&format!(
                            "unresolvable load `{var}` in {name} at {pc}"
                        )));
                    }
                    Callee::Native(_, _) | Callee::Func(_, _) if !consumed.contains(&pc) => {
                        let name = module
                            .strings
                            .get(f.name.0 as usize)
                            .map(String::as_str)
                            .unwrap_or("?");
                        return Err(ChunkError::op(&format!(
                            "unapplied function value in {name} at {pc}"
                        )));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// True when `f` can compile to an unboxed `int64_t` twin: all-int
/// signature and an int-closed body. Sound by the checker contract
/// (typed calls only ever pass ints where `int` is declared).
fn closable(module: &Module, f: &FuncDef, func_names: &HashSet<String>) -> bool {
    use zz_ir::op::BinOp as B;
    use zz_ir::op::UnOp as U;
    if f.sig.params.len() != f.params.len() {
        return false;
    }
    let int = |id: zz_ir::TypeId| module.types.get(id.0 as usize) == Some(&zz_ir::IrType::Int);
    if !f.sig.params.iter().all(|id| int(*id)) || !int(f.sig.ret) {
        return false;
    }
    // Every constant must be an int.
    for op in &f.code {
        if let Op::PushConst(id) = op {
            match module.consts.get(id.0 as usize) {
                Some(Const::Int(_)) => {}
                _ => return false,
            }
        }
        match op {
            Op::PushConst(_)
            | Op::Pop
            | Op::Swap
            | Op::LoadSlot(_)
            | Op::StoreSlot(_)
            | Op::IntAdd
            | Op::IntSub
            | Op::IntMul
            | Op::IntDiv
            | Op::IntRem
            | Op::IntNeg
            | Op::Jump(_)
            | Op::JumpIfFalseBool(_)
            | Op::JumpIfTrue(_)
            | Op::JumpIfFalse(_)
            | Op::Return
            | Op::Safepoint => {}
            Op::BinOp(B::Eq)
            | Op::BinOp(B::Ne)
            | Op::BinOp(B::Lt)
            | Op::BinOp(B::Gt)
            | Op::BinOp(B::Le)
            | Op::BinOp(B::Ge)
            | Op::BinOp(B::Add)
            | Op::BinOp(B::Sub)
            | Op::BinOp(B::Mul)
            | Op::BinOp(B::Div)
            | Op::BinOp(B::Rem) => {}
            Op::UnOp(U::Neg) | Op::UnOp(U::Pos) | Op::UnOp(U::BitNot) => {}
            Op::CallPath { parts, .. } => {
                // Only direct recursion into closable twins (checked
                // below by name); natives and unknown shapes bail out.
                let joined = parts
                    .iter()
                    .map(|id| {
                        module
                            .strings
                            .get(id.0 as usize)
                            .cloned()
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                if !func_names.contains(&joined) {
                    return false;
                }
                // The target itself must be closable — verified by the
                // caller with the precomputed set (this check is
                // structural; recursion terminates via `closable_all`).
                let _ = joined;
            }
            _ => return false,
        }
    }
    true
}

/// Names of all int-closable functions (fixpoint-free: a CallPath keeps
/// closability only when its target is itself closable — computed by
/// iterating to a fixpoint over the call graph edges).
fn closable_all(module: &Module) -> HashSet<String> {
    let mut names: HashSet<String> = HashSet::new();
    for f in &module.funcs {
        if let Some(nm) = module.strings.get(f.name.0 as usize) {
            names.insert(nm.clone());
        }
    }
    // Candidate set: signature + structural shape (ignoring call targets).
    let mut ok: HashSet<String> = HashSet::new();
    for f in &module.funcs {
        let nm = match module.strings.get(f.name.0 as usize) {
            Some(nm) => nm.clone(),
            None => continue,
        };
        // Structural check with all CallPath targets assumed closable.
        if closable(module, f, &names) {
            ok.insert(nm);
        }
    }
    // Prune: drop functions calling non-closable targets.
    loop {
        let mut drop = Vec::new();
        for f in &module.funcs {
            let nm = match module.strings.get(f.name.0 as usize) {
                Some(nm) => nm.clone(),
                None => continue,
            };
            if !ok.contains(&nm) {
                continue;
            }
            for op in &f.code {
                if let Op::CallPath { parts, .. } = op {
                    let joined = parts
                        .iter()
                        .map(|id| {
                            module
                                .strings
                                .get(id.0 as usize)
                                .cloned()
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join(".");
                    // Native-impl callees are not int twins; only calls
                    // into the closable set stay unboxed.
                    if !ok.contains(&joined) {
                        drop.push(nm.clone());
                        break;
                    }
                }
            }
        }
        if drop.is_empty() {
            break;
        }
        for nm in drop {
            ok.remove(&nm);
        }
    }
    ok
}

/// C integer literal (LL-suffixed; MIN spelled without overflow).
fn c_int(i: i64) -> String {
    if i == i64::MIN {
        "(-9223372036854775807LL - 1)".to_string()
    } else {
        format!("{i}LL")
    }
}

/// C string literal with octal escapes (mirrors the HIR backend: NUL-safe,
/// unambiguous in C no matter what follows).
fn c_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\0' => o.push_str("\\000"),
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                o.push_str(&format!("\\{:03o}", c as u32))
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Emitter state for one module.
struct Emitter<'a> {
    module: &'a Module,
    /// Dotted IR name -> C symbol.
    csyms: HashMap<String, String>,
    /// Dotted IR names with unboxed twins.
    twins: HashSet<String>,
    /// C-impl natives referenced: (dotted name, C impl, needs Rust staticlib).
    /// Populated during emission for the func table + link flags.
    used_natives: Vec<(String, &'static str, bool)>,
    needs_native_rt: bool,
    needs_float_fmt: bool,
}

impl<'a> Emitter<'a> {
    fn str(&self, id: StrId) -> &str {
        self.module
            .strings
            .get(id.0 as usize)
            .map(String::as_str)
            .unwrap_or("")
    }

    fn csym(&self, dotted: &str) -> String {
        format!("zz_chunk_{}", mangle(dotted))
    }

    /// Resolve a dotted call target: own function, C-impl native
    /// (bare or `std.` twin), or Rust-staticlib native. Mirrors the
    /// runtime resolution order the VM uses.
    fn resolve_call(&mut self, dotted: &str) -> Result<CallTarget, ChunkError> {
        if self.csyms.contains_key(dotted) {
            return Ok(CallTarget::Own(self.csym(dotted)));
        }
        if let Some(impl_name) = super::lower::native_impl(dotted) {
            return Ok(CallTarget::CImpl(impl_name, false));
        }
        if let Some(impl_name) = super::ffi_impl(dotted) {
            self.needs_native_rt = true;
            return Ok(CallTarget::CImpl(impl_name, true));
        }
        let std_name = format!("std.{dotted}");
        if let Some(impl_name) = super::lower::native_impl(&std_name) {
            return Ok(CallTarget::CImpl(impl_name, false));
        }
        if let Some(impl_name) = super::ffi_impl(&std_name) {
            self.needs_native_rt = true;
            return Ok(CallTarget::CImpl(impl_name, true));
        }
        Err(ChunkError::name(&format!("call target {dotted}")))
    }
}

#[derive(Debug, Clone)]
enum CallTarget {
    Own(String),
    CImpl(&'static str, bool),
}

/// Trap with a message on stderr + exit 1 (VM trap parity: kind is in
/// the message, exit code is what the harness gates). Use [`ctrap`];
/// this alias stays for call sites written against the older name.
#[allow(dead_code)]
fn trap(msg: &str) -> String {
    format!("{{ fprintf(stderr, \"zz error: {msg}\\n\"); exit(1); }}")
}

/// Trap helper: message on stderr, exit 1 (VM trap parity on the exit
/// code; message text is never gated).
fn ctrap(msg: &str) -> String {
    format!("{{ fprintf(stderr, \"zz error: {msg}\\n\"); exit(1); }}")
}

fn binop_const(op: &zz_ir::op::BinOp) -> Option<&'static str> {
    use zz_ir::op::BinOp as B;
    Some(match op {
        B::Add => "ZZOP_ADD",
        B::Sub => "ZZOP_SUB",
        B::Mul => "ZZOP_MUL",
        B::Div => "ZZOP_DIV",
        B::Rem => "ZZOP_REM",
        B::Pow => "ZZOP_POW",
        B::Eq => "ZZOP_EQ",
        B::Ne => "ZZOP_NE",
        B::Lt => "ZZOP_LT",
        B::Gt => "ZZOP_GT",
        B::Le => "ZZOP_LE",
        B::Ge => "ZZOP_GE",
        B::And => "ZZOP_AND",
        B::Or => "ZZOP_OR",
        B::Elvis => return None,
        B::BitAnd => "ZZOP_AND",
        B::BitOr => "ZZOP_OR",
        B::BitXor => "ZZOP_XOR",
        B::Shl => "ZZOP_SHL",
        B::Shr => "ZZOP_SHR",
    })
}

impl<'a> Emitter<'a> {
    /// Emit fresh-owned construction of a pool constant. Every call
    /// builds an independent value (ownership transfers to the stack);
    /// sharing is unobservable under detach-on-write (spec §1.2).
    fn emit_const_new(&mut self, id: ConstId, out: &mut String) -> Result<String, ChunkError> {
        let c = self
            .module
            .consts
            .get(id.0 as usize)
            .ok_or_else(|| ChunkError::op("const id"))?
            .clone();
        match c {
            Const::Unit => Ok("zz_unit()".to_string()),
            Const::Bool(b) => Ok(format!("zz_bool({})", if b { "true" } else { "false" })),
            Const::Int(i) => Ok(format!("zz_int({})", c_int(i))),
            Const::Float(f) => {
                self.needs_float_fmt = true;
                Ok(format!("zz_float({f:?})"))
            }
            Const::Str(s) => {
                let text = self.str(s).to_string();
                Ok(format!("zz_str_new({}, {})", c_str(&text), text.len()))
            }
            Const::Array(items) => {
                let tmp = format!("_kc{}", out.len());
                out.push_str(&format!("    zz_value {tmp} = zz_array_new();\n"));
                for item in items {
                    let e = self.emit_const_new(item, out)?;
                    // Move convention: the array adopts the element.
                    out.push_str(&format!("    zz_array_push({tmp}.arr, {e});\n"));
                }
                Ok(tmp)
            }
            Const::Dict(pairs) => {
                let tmp = format!("_kc{}", out.len());
                out.push_str(&format!("    zz_value {tmp} = zz_dict_new();\n"));
                // Last-wins dedup (mirrors MakeDict pair order): every
                // insert then takes the new-key path, whose contract is
                // fixed — keys retained, values moved. Release key temps
                // only; releasing values would free dict-owned payloads.
                let mut seen: HashSet<String> = HashSet::new();
                let mut uniq: Vec<(ConstId, ConstId)> = Vec::new();
                for (k, v) in pairs.iter().rev() {
                    // Keys are strings; dedup by string content.
                    let key = match self.module.consts.get(k.0 as usize) {
                        Some(Const::Str(s)) => self
                            .module
                            .strings
                            .get(s.0 as usize)
                            .cloned()
                            .unwrap_or_default(),
                        _ => "\u{0}nonstring".to_string(),
                    };
                    if seen.insert(key) {
                        uniq.push((*k, *v));
                    }
                }
                uniq.reverse();
                for (k, v) in uniq {
                    let ke = self.emit_const_new(k, out)?;
                    let ve = self.emit_const_new(v, out)?;
                    let kt = format!("_kk{}", out.len());
                    let vt = format!("_kv{}", out.len());
                    out.push_str(&format!("    zz_value {kt} = {ke};\n"));
                    out.push_str(&format!("    zz_value {vt} = {ve};\n"));
                    out.push_str(&format!("    zz_dict_set({tmp}.dict, {kt}, {vt});\n"));
                    out.push_str(&format!("    zz_release(&{kt});\n"));
                }
                Ok(tmp)
            }
            Const::Option(inner) => match inner {
                None => Ok("(zz_value){ZZ_OPTION_NONE, {0}}".to_string()),
                Some(x) => {
                    let e = self.emit_const_new(x, out)?;
                    let tmp = format!("_kc{}", out.len());
                    out.push_str(&format!(
                        "    zz_value *_kp{tmp} = malloc(sizeof(zz_value));\n"
                    ));
                    out.push_str(&format!("    *_kp{tmp} = {e};\n"));
                    Ok(format!(
                        "(zz_value){{ZZ_OPTION_SOME, {{.payload = _kp{tmp}}}}}",
                        tmp = tmp
                    ))
                }
            },
            Const::Result { ok, val } => {
                let e = self.emit_const_new(val, out)?;
                let tmp = format!("_kc{}", out.len());
                let tag = if ok { "ZZ_RESULT_OK" } else { "ZZ_RESULT_ERR" };
                out.push_str(&format!(
                    "    zz_value *_kp{tmp} = malloc(sizeof(zz_value));\n"
                ));
                out.push_str(&format!("    *_kp{tmp} = {e};\n"));
                Ok(format!(
                    "(zz_value){{{tag}, {{.payload = _kp{tmp}}}}}",
                    tmp = tmp
                ))
            }
        }
    }
}

impl<'a> Emitter<'a> {
    /// C symbol for an IR function.
    fn fsym(&self, id: FuncId) -> String {
        let name = self
            .module
            .funcs
            .get(id.0 as usize)
            .and_then(|f| self.module.strings.get(f.name.0 as usize))
            .map(String::as_str)
            .unwrap_or("anon");
        format!("zz_chunk_{}", mangle(name))
    }

    /// Frame slots: one past the max slot id referenced (slots and the
    /// operand stack share one array, exactly like the VM frame).
    fn frame_size(&self, f: &FuncDef) -> usize {
        let mut max_slot = 0usize;
        let mut any_slot = false;
        for op in &f.code {
            match op {
                Op::LoadSlot(s) | Op::StoreSlot(s) => {
                    any_slot = true;
                    max_slot = max_slot.max(*s as usize);
                }
                _ => {}
            }
        }
        let slots = if any_slot { max_slot + 1 } else { 0 };
        // Operand stack above all slots + slack (loop temps, const
        // temporaries live in C locals, never in `st`).
        slots + f.max_stack as usize + 32
    }

    /// Emit one function body (boxed).
    ///
    /// Frame model mirrors the VM exactly: `st` is the whole frame,
    /// params are seated at `[0..nparams)`, `sp` starts at `nparams`,
    /// and every slot access is `st[s]` below `sp`. Loop state
    /// (iterable, index, vars) lives in `st` itself, so only the loop
    /// base needs side storage (`_lbase`).
    fn emit_func(
        &mut self,
        id: FuncId,
        origins_map: &HashMap<usize, Vec<Vec<Callee>>>,
        out: &mut String,
    ) -> Result<(), ChunkError> {
        let f = self
            .module
            .funcs
            .get(id.0 as usize)
            .ok_or_else(|| ChunkError::op("func id"))?;
        let sym = self.fsym(id);
        let frame = self.frame_size(f);
        let nparams = f.params.len();
        // Twin functions compile twice: the boxed symbol below is a thin
        // unbox/call/box adapter (checker-guaranteed ints, unchecked like
        // the HIR backend); the `_u` twin carries the real body.
        let dotted = self
            .module
            .strings
            .get(f.name.0 as usize)
            .cloned()
            .unwrap_or_default();
        let is_twin = self.twins.contains(&dotted);
        out.push_str(&format!(
            "static zz_value {sym}(zz_value *args, size_t argc) {{\n"
        ));
        if is_twin {
            out.push_str(&format!(
                "    if (argc != {nparams}) {trap};\n",
                trap = ctrap("arity mismatch")
            ));
            let mut unbox = Vec::new();
            for i in 0..nparams {
                unbox.push(format!("(args[{i}].i)"));
            }
            out.push_str(&format!(
                "    return zz_int({sym}_u({args}));\n}}\n",
                args = unbox.join(", ")
            ));
            self.emit_twin(id, out)?;
            return Ok(());
        }
        out.push_str(&format!("    zz_value st[{frame}];\n"));
        out.push_str(&format!(
            "    for (int _i = 0; _i < {frame}; _i++) st[_i] = zz_unit();\n"
        ));
        out.push_str(&format!("    int sp = {nparams};\n"));
        out.push_str("    int _lbase[256];\n    int _ldepth = 0;\n");
        if nparams > 0 {
            out.push_str(&format!(
                "    if (argc != {nparams}) {trap};\n",
                trap = ctrap("arity mismatch")
            ));
            for i in 0..nparams {
                out.push_str(&format!("    st[{i}] = zz_clone(args[{i}]);\n"));
            }
        }
        // Jump targets get labels.
        let mut targets: HashSet<u32> = HashSet::new();
        for op in &f.code {
            match op {
                Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
                    targets.insert(*t);
                }
                Op::ForSetup { exit, header, .. } => {
                    targets.insert(*exit);
                    targets.insert(*header);
                }
                Op::ForNext { exit, .. } | Op::WhileSetup { exit, .. } | Op::WhileCond { exit } => {
                    targets.insert(*exit);
                }
                Op::MatchArm { next, .. } | Op::MatchGuard { next, .. } => {
                    targets.insert(*next);
                }
                Op::IfLetMatch { els, .. } => {
                    targets.insert(*els);
                }
                _ => {}
            }
        }
        let origins_empty: Vec<Vec<Callee>> = Vec::new();
        let org = origins_map.get(&(id.0 as usize)).unwrap_or(&origins_empty);
        // Store-index fusion (in-place slot stores; see `fuse_scan`).
        let (fused, skip) = fuse_scan(&f.code);
        for (pc, op) in f.code.iter().enumerate() {
            if skip.contains(&pc) {
                continue;
            }
            if targets.contains(&(pc as u32)) {
                out.push_str(&format!("L{pc}:;\n"));
            }
            // The callee a `Call` observes (entry-stack slot).
            let callee_owned: Option<Callee> = match op {
                Op::Call { argc } => org
                    .get(pc)
                    .and_then(|st| st.get(st.len().checked_sub((*argc as usize) + 1)?))
                    .cloned(),
                _ => None,
            };
            let cx = OpCx {
                callee: callee_owned.as_ref(),
                fuse_slot: fused.get(&pc).copied(),
                frame,
            };
            self.emit_op(id, pc, op, &cx, out)?;
        }
        // A jump may target one-past-the-end (fall off into the
        // implicit return, like the VM's ip-past-end).
        if targets.contains(&(f.code.len() as u32)) {
            out.push_str(&format!("L{}:;\n", f.code.len()));
        }
        // Implicit return of the top value (function bodies fall off).
        out.push_str(&format!(
            "    {{ zz_value _r = st[--sp]; for (int _i = 0; _i < {frame}; _i++) {{ if (_i != sp) zz_release(&st[_i]); }} return _r; }}\n"
        ));
        out.push_str("}\n");
        Ok(())
    }
}

impl<'a> Emitter<'a> {
    /// Record a referenced native impl for link-flag computation.
    fn note_native(&mut self, impl_name: &'static str, needs_rt: bool, dotted: &str) {
        if !self.used_natives.iter().any(|(n, _, _)| n == dotted) {
            self.used_natives
                .push((dotted.to_string(), impl_name, needs_rt));
        }
        if needs_rt {
            self.needs_native_rt = true;
        }
    }

    /// Emit a direct C call to a `zz_*` impl with `argc` boxed args.
    /// Args move into C temporaries; the impl borrows (like the HIR
    /// backend's `zz_call_nativeN` wrappers) and our shares release
    /// after the call — except `take`-suffixed impls, which consume.
    fn emit_native_call(
        &mut self,
        impl_name: &str,
        argc: u16,
        out: &mut String,
    ) -> Result<(), ChunkError> {
        if argc > 6 {
            return Err(ChunkError::op("native call with >6 args"));
        }
        let takes = impl_name.ends_with("_take") || impl_name.ends_with("_take_arena");
        out.push_str("    {\n");
        // Pop in reverse so `_na0` is the first (bottom) argument.
        for i in (0..argc).rev() {
            out.push_str(&format!("    zz_value _na{i} = st[--sp];\n"));
        }
        let args = (0..argc)
            .map(|i| format!("_na{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let call = match argc {
            0 => format!("zz_call_native0({impl_name})"),
            1 => format!("zz_call_native1({impl_name}, {args})"),
            2 => format!("zz_call_native2({impl_name}, {args})"),
            3 => format!("zz_call_native3({impl_name}, {args})"),
            4 => format!("zz_call_native4({impl_name}, {args})"),
            5 => format!("zz_call_native5({impl_name}, {args})"),
            _ => format!("zz_call_native6({impl_name}, {args})"),
        };
        out.push_str(&format!("    st[sp++] = {call};\n"));
        if !takes {
            for i in 0..argc {
                out.push_str(&format!("    zz_release(&_na{i});\n"));
            }
        }
        out.push_str("    }\n");
        Ok(())
    }

    /// Emit a call to an own tabled function: pop `argc` args, call the
    /// C symbol (which clones params on entry, so our shares release
    /// after), push the result.
    fn emit_own_call(&mut self, sym: &str, argc: u16, out: &mut String) {
        out.push_str("    {\n");
        // Pop in reverse so `_ca0` is the first (bottom) argument.
        for i in (0..argc).rev() {
            out.push_str(&format!("    zz_value _ca{i} = st[--sp];\n"));
        }
        if argc == 0 {
            out.push_str(&format!("    st[sp++] = {sym}(NULL, 0);\n"));
        } else {
            let args = (0..argc)
                .map(|i| format!("_ca{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "    {{ zz_value _argv[{argc}] = {{{args}}}; st[sp++] = {sym}(_argv, {argc}); }}\n"
            ));
            for i in 0..argc {
                out.push_str(&format!("    zz_release(&_ca{i});\n"));
            }
        }
        out.push_str("    }\n");
    }
}

/// Per-op emission context: the precomputed `Call` callee (if any),
/// the [`fuse_scan`] slot for a fusing `StoreIndexOp`, and the frame
/// size for the `Return` release sweep.
struct OpCx<'x> {
    callee: Option<&'x Callee>,
    fuse_slot: Option<u16>,
    frame: usize,
}

impl<'a> Emitter<'a> {
    /// Emit one boxed op (see [`OpCx`]).
    fn emit_op(
        &mut self,
        _id: FuncId,
        _pc: usize,
        op: &Op,
        cx: &OpCx<'_>,
        out: &mut String,
    ) -> Result<(), ChunkError> {
        let (callee, fuse_slot, frame) = (cx.callee, cx.fuse_slot, cx.frame);
        match op {
            Op::PushConst(c) => {
                let e = self.emit_const_new(*c, out)?;
                out.push_str(&format!("    st[sp++] = {e};\n"));
            }
            Op::Pop => {
                out.push_str("    if (sp > 0) zz_release(&st[--sp]);\n");
            }
            Op::PopN(n) => {
                out.push_str(&format!(
                    "    {{ if (sp < {} + 1) {}; zz_value _t = st[sp-1]; for (int _i = 1; _i <= {}; _i++) zz_release(&st[sp-1-_i]); st[sp-{}-1] = _t; sp -= {}; }}\n",
                    n, ctrap("block cleanup underflow"), n, n, n
                ));
            }
            Op::Swap => {
                out.push_str(
                    "    { zz_value _t = st[sp-1]; st[sp-1] = st[sp-2]; st[sp-2] = _t; }\n",
                );
            }
            Op::Truthy => {
                out.push_str(
                    "    { zz_value _v = st[--sp]; st[sp++] = zz_bool(zz_truthy(_v)); zz_release(&_v); }\n",
                );
            }
            Op::LoadSlot(s) => {
                out.push_str(&format!("    st[sp++] = zz_clone(st[{s}]);\n"));
            }
            Op::StoreSlot(s) => {
                out.push_str(&format!("    zz_cstore(&st[{s}], st[--sp]);\n"));
            }
            // LoadVar pushes a dummy: every covered use resolves its
            // origin statically and consumes the slot directly (see
            // `Call`). Unconsumed loads fail coverage first.
            Op::LoadVar(_) => {
                out.push_str("    st[sp++] = zz_unit();\n");
            }
            Op::IntAdd => {
                out.push_str(
                    "    { zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_INT && _b.tag == ZZ_INT) { st[sp++] = zz_int(_a.i + _b.i); } else { st[sp++] = zz_binop(ZZOP_ADD, _a, _b); } zz_release(&_a); zz_release(&_b); }\n",
                );
            }
            Op::IntSub => {
                out.push_str(
                    "    { zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_INT && _b.tag == ZZ_INT) { st[sp++] = zz_int(_a.i - _b.i); } else { st[sp++] = zz_binop(ZZOP_SUB, _a, _b); } zz_release(&_a); zz_release(&_b); }\n",
                );
            }
            Op::IntMul => {
                out.push_str(
                    "    { zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_INT && _b.tag == ZZ_INT) { st[sp++] = zz_int(_a.i * _b.i); } else { st[sp++] = zz_binop(ZZOP_MUL, _a, _b); } zz_release(&_a); zz_release(&_b); }\n",
                );
            }
            Op::IntDiv => {
                out.push_str(
                    "    { zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_INT && _b.tag == ZZ_INT) { if (_b.i == 0) { fprintf(stderr, \"zz error: division by zero\\n\"); exit(1); } if (_a.i == INT64_MIN && _b.i == -1) { fprintf(stderr, \"zz error: integer overflow in division\\n\"); exit(1); } st[sp++] = zz_int(_a.i / _b.i); } else { st[sp++] = zz_binop(ZZOP_DIV, _a, _b); } zz_release(&_a); zz_release(&_b); }\n",
                );
            }
            Op::IntRem => {
                out.push_str(
                    "    { zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_INT && _b.tag == ZZ_INT) { if (_b.i == 0) { fprintf(stderr, \"zz error: modulo by zero\\n\"); exit(1); } if (_a.i == INT64_MIN && _b.i == -1) { fprintf(stderr, \"zz error: integer overflow in modulo\\n\"); exit(1); } st[sp++] = zz_int(_a.i % _b.i); } else { st[sp++] = zz_binop(ZZOP_REM, _a, _b); } zz_release(&_a); zz_release(&_b); }\n",
                );
            }
            Op::IntNeg => {
                out.push_str(
                    "    { zz_value _v = st[--sp]; if (_v.tag == ZZ_INT) { st[sp++] = zz_int(-_v.i); } else { st[sp++] = zz_neg(_v); } zz_release(&_v); }\n",
                );
            }
            Op::BinOp(b) => {
                let c = binop_const(b).ok_or_else(|| ChunkError::op("elvis binop"))?;
                // Structural range equality: the runtime compares the
                // overlapped `.i` bits (pointer garbage for boxed
                // ranges); the VM compares (start, end, step).
                if matches!(b, zz_ir::op::BinOp::Eq | zz_ir::op::BinOp::Ne) {
                    let eq = matches!(b, zz_ir::op::BinOp::Eq);
                    let op = if eq { "==" } else { "!=" };
                    out.push_str(&format!(
                        "    {{ zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_RANGE && _b.tag == ZZ_RANGE) {{ zz_crange *_x = (zz_crange*)_a.payload; zz_crange *_y = (zz_crange*)_b.payload; int _eq = _x && _y && _x->start == _y->start && _x->end == _y->end && _x->step == _y->step; st[sp++] = zz_bool(_eq {op} 1); }} else {{ st[sp++] = zz_binop({c}, _a, _b); }} zz_release(&_a); zz_release(&_b); }}\n"
                    ));
                } else if matches!(
                    b,
                    zz_ir::op::BinOp::Add
                        | zz_ir::op::BinOp::Sub
                        | zz_ir::op::BinOp::Mul
                        | zz_ir::op::BinOp::Lt
                        | zz_ir::op::BinOp::Gt
                        | zz_ir::op::BinOp::Le
                        | zz_ir::op::BinOp::Ge
                ) {
                    // Int fast path (UB-free under `-fwrapv`); mixed
                    // types fall back to the full operator.
                    let e = match b {
                        zz_ir::op::BinOp::Add => "zz_int(_a.i + _b.i)",
                        zz_ir::op::BinOp::Sub => "zz_int(_a.i - _b.i)",
                        zz_ir::op::BinOp::Mul => "zz_int(_a.i * _b.i)",
                        zz_ir::op::BinOp::Lt => "zz_bool(_a.i < _b.i)",
                        zz_ir::op::BinOp::Gt => "zz_bool(_a.i > _b.i)",
                        zz_ir::op::BinOp::Le => "zz_bool(_a.i <= _b.i)",
                        zz_ir::op::BinOp::Ge => "zz_bool(_a.i >= _b.i)",
                        _ => unreachable!(),
                    };
                    out.push_str(&format!(
                        "    {{ zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_INT && _b.tag == ZZ_INT) {{ st[sp++] = {e}; }} else {{ st[sp++] = zz_binop({c}, _a, _b); }} zz_release(&_a); zz_release(&_b); }}\n"
                    ));
                } else {
                    out.push_str(&format!(
                        "    {{ zz_value _b = st[--sp]; zz_value _a = st[--sp]; st[sp++] = zz_binop({c}, _a, _b); zz_release(&_a); zz_release(&_b); }}\n"
                    ));
                }
            }
            Op::UnOp(u) => {
                match u {
                    // Identity (VM returns the value unchanged).
                    zz_ir::op::UnOp::Pos => {
                        out.push_str("    { zz_value _v = st[--sp]; st[sp++] = _v; }\n");
                    }
                    _ => {
                        let f = match u {
                            zz_ir::op::UnOp::Neg => "zz_neg",
                            zz_ir::op::UnOp::Not => "zz_not",
                            zz_ir::op::UnOp::BitNot => "zz_bitnot",
                            zz_ir::op::UnOp::Pos => unreachable!(),
                        };
                        out.push_str(&format!(
                            "    {{ zz_value _v = st[--sp]; st[sp++] = {f}(_v); zz_release(&_v); }}\n"
                        ));
                    }
                }
            }
            Op::Jump(t) => {
                out.push_str(&format!("    goto L{t};\n"));
            }
            Op::JumpIfFalse(t) => {
                out.push_str(&format!(
                    "    {{ zz_value _c = st[--sp]; bool _t = zz_truthy(_c); zz_release(&_c); if (!_t) goto L{t}; }}\n"
                ));
            }
            Op::JumpIfTrue(t) => {
                out.push_str(&format!(
                    "    {{ zz_value _c = st[--sp]; bool _t = zz_truthy(_c); zz_release(&_c); if (_t) goto L{t}; }}\n"
                ));
            }
            Op::JumpIfFalseBool(t) => {
                out.push_str(&format!(
                    "    {{ zz_value _c = st[--sp]; if (_c.tag != ZZ_BOOL) {}; bool _t = _c.b; zz_release(&_c); if (!_t) goto L{t}; }}\n",
                    ctrap("condition must be a bool")
                ));
            }
            Op::Return => {
                out.push_str(&format!(
                    "    {{ zz_value _r = st[--sp]; for (int _i = 0; _i < {frame}; _i++) {{ if (_i != sp) zz_release(&st[_i]); }} return _r; }}\n"
                ));
            }
            Op::Safepoint => {
                // Cooperative yield check in the VM; a no-op in AOT
                // (native threads preempt; no green-thread scheduler).
                out.push_str("    ;\n");
            }
            Op::ForSetup { .. } => {
                // Stack: [..., result_slot(Unit), iterable]. Pop the
                // iterable, record the result slot as the loop base,
                // then push iterable + index + var placeholders.
                out.push_str("    { zz_value _it = st[--sp];\n");
                out.push_str(
                    "      if (_it.tag != ZZ_RANGE && _it.tag != ZZ_ARRAY && _it.tag != ZZ_BYTES && _it.tag != ZZ_DICT) ",
                );
                out.push_str(&format!("{};\n", ctrap("cannot iterate this value")));
                out.push_str("      if (_ldepth >= 256) ");
                out.push_str(&format!("{};\n", ctrap("loop nesting too deep")));
                out.push_str("      if (_it.tag == ZZ_ARRAY) { zz_value _d = zz_array_dup(_it.arr); zz_release(&_it); _it = _d; }\n");
                out.push_str("      if (_it.tag == ZZ_DICT) { zz_value _d = zz_dict_dup_value(_it.dict); zz_release(&_it); _it = _d; }\n");
                out.push_str("      _lbase[_ldepth++] = sp - 1;\n");
                out.push_str("      st[sp++] = _it;\n");
                // Index placeholder; var placeholders pushed by count
                // below (num_vars is a codegen-time constant — read it
                // from the op).
                let nvars = match op {
                    Op::ForSetup { num_vars, .. } => *num_vars as usize,
                    _ => 0,
                };
                out.push_str("      st[sp++] = zz_int(0);\n");
                for _ in 0..nvars {
                    out.push_str("      st[sp++] = zz_unit();\n");
                }
                out.push_str("    }\n");
            }
            Op::ForNext { vars, exit, in_env } => {
                if *in_env {
                    return Err(ChunkError::op("for loop with captured env vars"));
                }
                let nvars = vars.len();
                if nvars > 2 {
                    return Err(ChunkError::op("for loop with >2 vars"));
                }
                // Release the previous iteration's vars, pop the index;
                // the iterable sits on top.
                for _ in 0..nvars {
                    out.push_str("    zz_release(&st[--sp]);\n");
                }
                out.push_str("    { zz_value _idx = st[--sp];\n");
                out.push_str("      zz_value _it = st[sp-1];\n");
                out.push_str("      int _done = 0;\n");
                // Range fast path (chunk-local box: start/end/step).
                out.push_str("      if (_it.tag == ZZ_RANGE) {\n");
                out.push_str("        zz_crange *_rg = (zz_crange*)_it.payload;\n");
                out.push_str("        if (!_rg) ");
                out.push_str(&format!("{};\n", ctrap("cannot iterate this value")));
                out.push_str("        int64_t _cur = _idx.i;\n");
                out.push_str("        int64_t _end = _rg->end;\n");
                out.push_str("        int64_t _step = _rg->step;\n");
                out.push_str("        _done = _step > 0 ? (_cur >= _end) : (_cur <= _end);\n");
                out.push_str("        if (!_done) {\n");
                out.push_str("          zz_value _nx = zz_int(_cur + _step);\n");
                out.push_str("          zz_value _v0 = zz_int(_cur);\n");
                out.push_str("          zz_release(&_idx);\n");
                out.push_str("          st[sp++] = _nx;\n");
                out.push_str("          st[sp++] = _v0;\n");
                if nvars == 2 {
                    // The checker rejects multi-var range loops; trap
                    // rather than diverge (the VM would unbalance here).
                    out.push_str(&format!("          {};\n", ctrap("cannot unpack a non-pair value into 2 loop variables (expected `enumerate()` pairs)")));
                }
                out.push_str("        }\n");
                out.push_str("      } else if (_it.tag == ZZ_ARRAY) {\n");
                out.push_str("        int64_t _i = _idx.i;\n");
                out.push_str("        int64_t _n = (int64_t)_it.arr->len;\n");
                out.push_str("        _done = _i < 0 || _i >= _n;\n");
                out.push_str("        if (!_done) {\n");
                out.push_str("          zz_value _v0 = zz_clone(_it.arr->items[_i]);\n");
                out.push_str("          zz_value _nx = zz_int(_i + 1);\n");
                out.push_str("          zz_release(&_idx);\n");
                out.push_str("          st[sp++] = _nx;\n");
                if nvars == 2 {
                    // Enumerate pairs (tuple/array of 2) mirror the VM.
                    out.push_str("          { zz_value _p = _v0; _v0 = zz_int(_i);\n");
                    out.push_str("            zz_value *_pair = NULL;\n");
                    out.push_str("            if (_p.tag == ZZ_ARRAY && _p.arr->len == 2) _pair = _p.arr->items;\n");
                    out.push_str("            else if (_p.tag == ZZ_TUPLE && _p.payload && _p.payload->tag == ZZ_ARRAY && _p.payload->arr->len == 2) _pair = _p.payload->arr->items;\n");
                    out.push_str("            if (_pair) { st[sp++] = zz_clone(_pair[0]); st[sp++] = zz_clone(_pair[1]); zz_release(&_p); zz_release(&_v0); }\n");
                    out.push_str("            else { zz_release(&_p); zz_release(&_v0); ");
                    out.push_str(&format!("{} }}\n", ctrap("cannot unpack a non-pair value into 2 loop variables (expected `enumerate()` pairs)")));
                    out.push_str("          }\n");
                } else {
                    out.push_str("          st[sp++] = _v0;\n");
                }
                out.push_str("        }\n");
                out.push_str("      } else if (_it.tag == ZZ_BYTES) {\n");
                out.push_str("        int64_t _i = _idx.i;\n");
                out.push_str("        int64_t _n = (int64_t)(_it.bytes ? _it.bytes->len : 0);\n");
                out.push_str("        _done = _i < 0 || _i >= _n;\n");
                out.push_str("        if (!_done) {\n");
                out.push_str(
                    "          int _e2 = 0; zz_value _v0 = zz_bytes_get(_it.bytes, _idx, &_e2);\n",
                );
                out.push_str("          if (_e2) ");
                out.push_str(&format!("{};\n", ctrap("index out of bounds")));
                out.push_str("          zz_value _nx = zz_int(_i + 1);\n");
                out.push_str("          zz_release(&_idx);\n");
                out.push_str("          st[sp++] = _nx;\n");
                out.push_str("          st[sp++] = _v0;\n");
                out.push_str("        }\n");
                out.push_str("      } else if (_it.tag == ZZ_DICT) {\n");
                out.push_str("        int64_t _i = _idx.i;\n");
                out.push_str("        int64_t _n = (int64_t)zz_dict_len(_it.dict);\n");
                out.push_str("        _done = _i < 0 || _i >= _n;\n");
                out.push_str("        if (!_done) {\n");
                out.push_str("          zz_dict_entry *_de = &_it.dict->entries[_i];\n");
                out.push_str(
                    "          zz_value _k = zz_clone((zz_value){ZZ_STR, {.s = _de->key}});\n",
                );
                out.push_str("          zz_value _v0 = zz_clone(_de->val);\n");
                out.push_str("          zz_value _nx = zz_int(_i + 1);\n");
                out.push_str("          zz_release(&_idx);\n");
                out.push_str("          st[sp++] = _nx;\n");
                out.push_str("          st[sp++] = _k;\n");
                if nvars == 2 {
                    out.push_str("          st[sp++] = _v0;\n");
                } else {
                    out.push_str("          zz_release(&_v0);\n");
                }
                out.push_str("        }\n");
                out.push_str("      } else ");
                out.push_str(&format!("{};\n", ctrap("cannot iterate this value")));
                out.push_str("      if (_done) {\n");
                out.push_str("        zz_release(&_idx);\n");
                out.push_str("        zz_release(&st[--sp]);\n");
                out.push_str("        _ldepth--;\n");
                out.push_str("        sp = _lbase[_ldepth] + 1;\n");
                out.push_str(&format!("        goto L{exit};\n"));
                out.push_str("      }\n");
                out.push_str("    }\n");
            }
            Op::WhileSetup { .. } => {
                out.push_str("    { if (_ldepth >= 256) ");
                out.push_str(&format!("{};\n", ctrap("loop nesting too deep")));
                // The Unit result slot pushes next (compiler emits it
                // after setup); the base is where it will land. (For
                // `for` the Unit is already below — hence `sp - 1`
                // there. VM parity: `stack_base = stack.len()`.)
                out.push_str("      _lbase[_ldepth++] = sp; }\n");
            }
            Op::WhileCond { exit } => {
                out.push_str("    { zz_value _c = st[--sp];\n");
                out.push_str("      if (_c.tag != ZZ_BOOL) ");
                out.push_str(&format!("{};\n", ctrap("`while` condition must be a bool")));
                out.push_str("      int _t = _c.b; zz_release(&_c);\n");
                out.push_str("      if (!_t) { _ldepth--; sp = _lbase[_ldepth] + 1; ");
                out.push_str(&format!("goto L{exit}; }} }}\n"));
            }
            Op::SetLoopResult => {
                out.push_str(
                    "    { zz_value _v = st[--sp]; zz_cstore(&st[_lbase[_ldepth-1]], _v); }\n",
                );
            }
            Op::MakeArray(n) => {
                out.push_str(&format!(
                    "    {{ zz_value _a = zz_array_new(); for (int _k = 0; _k < {n}; _k++) {{ zz_value _e = st[sp-{n}+_k]; zz_array_push(_a.arr, zz_clone(_e)); }} for (int _k = 0; _k < {n}; _k++) zz_release(&st[sp-{n}+_k]); sp -= {n}; st[sp++] = _a; }}\n"
                ));
            }
            Op::ArrayPush => {
                // VM: pop value, pop array, push array+[value].
                // Dup-if-shared first (loads clone, sharing the buffer).
                out.push_str("    { zz_value _v = st[--sp]; zz_value _a = st[--sp];\n");
                out.push_str("      if (_a.tag != ZZ_ARRAY) ");
                out.push_str(&format!("{};\n", ctrap("ArrayPush: expected array")));
                out.push_str("      if (_a.arr->refs != 1) { zz_value _d = zz_array_dup(_a.arr); zz_release(&_a); _a = _d; }\n");
                out.push_str("      zz_array_push(_a.arr, _v);\n");
                out.push_str("      st[sp++] = _a; }\n");
            }
            Op::IndexOp => {
                out.push_str("    { zz_value _ix = st[--sp]; zz_value _ob = st[--sp];\n");
                out.push_str("      int _e3 = 0; zz_value _r = zz_index_get(_ob, _ix, &_e3);\n");
                out.push_str("      zz_release(&_ob); zz_release(&_ix);\n");
                out.push_str("      if (_e3) ");
                out.push_str(&format!("{};\n", ctrap("index out of bounds")));
                out.push_str("      st[sp++] = _r; }\n");
            }
            Op::StoreIndexOp => {
                // Evaluation order: base, index, value (§7) — the stack
                // holds [object, index, value].
                if let Some(s) = fuse_slot {
                    // Fused: the object is the `LoadSlot(s)` clone on
                    // top-3. Drop our share first (restoring slot
                    // uniqueness in the two-owner case), operate
                    // directly on the slot, and consume all three —
                    // the following `StoreSlot(s)` is skipped.
                    out.push_str("    { zz_value _fv = st[--sp]; zz_value _fix = st[--sp];\n");
                    out.push_str("      zz_release(&st[--sp]);\n");
                    out.push_str(&format!(
                        "      int _fe = 0; zz_index_set(&st[{s}], _fix, _fv, &_fe);\n"
                    ));
                    out.push_str("      zz_release(&_fix); zz_release(&_fv);\n");
                    out.push_str("      if (_fe) ");
                    out.push_str(&format!("{};\n", ctrap("index out of bounds")));
                    out.push_str("    }\n");
                    return Ok(());
                }
                out.push_str("    { zz_value _v = st[--sp]; zz_value _ix = st[--sp];\n");
                out.push_str("      int _e4 = 0; zz_index_set(&st[sp-1], _ix, _v, &_e4);\n");
                out.push_str("      zz_release(&_ix); zz_release(&_v);\n");
                out.push_str("      if (_e4) ");
                out.push_str(&format!("{};\n", ctrap("index out of bounds")));
                out.push_str("    }\n");
            }
            Op::MakeRange => {
                // Stack holds [start, end]; step is always 1 here.
                // Ranges box (start, end, step) on the heap via a
                // chunk-local immortal box: the C runtime's range is a
                // degenerate start-marker (HIR parity only), which
                // cannot drive a loop. Boxes are immutable, so sharing
                // under clone is sound; they are never freed (32B per
                // MakeRange execution — a designed ownership pass will
                // intern them (#188.1, #254).
                out.push_str("    { zz_value _e5 = st[--sp]; zz_value _s5 = st[--sp];\n");
                out.push_str("      if (_s5.tag != ZZ_INT || _e5.tag != ZZ_INT) ");
                out.push_str(&format!("{};\n", ctrap("range bounds must be integers")));
                out.push_str("      zz_value _r = zz_crange_new(_s5.i, _e5.i, 1);\n");
                out.push_str("      zz_release(&_s5); zz_release(&_e5);\n");
                out.push_str("      st[sp++] = _r; }\n");
            }
            Op::MakeFunc { .. } => {
                // All functions compile to static C symbols; creation
                // is a no-op pushing unit (first-class func values that
                // escape into slots fail `Call` coverage first).
                out.push_str("    st[sp++] = zz_unit();\n");
            }
            Op::Call { argc } => {
                // Pop args in reverse (so `_cx0` is the first argument),
                // then the callee slot (a dummy — `LoadVar` pushes unit;
                // the real target resolved statically via origins).
                out.push_str("    {\n");
                for i in (0..*argc).rev() {
                    out.push_str(&format!("    zz_value _cx{i} = st[--sp];\n"));
                }
                out.push_str("    zz_release(&st[--sp]);\n");
                // Re-seat args for the shared call helpers below.
                let reseat = |out: &mut String| {
                    for i in 0..*argc {
                        out.push_str(&format!("    st[sp++] = _cx{i};\n"));
                    }
                };
                match callee.cloned().unwrap_or(Callee::Unknown) {
                    Callee::Func(name, _) => {
                        let sym = self.csym(&name);
                        // Twin functions called through a value keep the
                        // boxed symbol (adapter unboxes once); direct
                        // twin-to-twin uses CallPath (see twin emitter).
                        for i in (0..*argc).rev() {
                            let _ = i;
                        }
                        // Pop the reseat: call directly off `_cx` temps.
                        let mut argv = String::new();
                        for i in 0..*argc {
                            if i > 0 {
                                argv.push_str(", ");
                            }
                            argv.push_str(&format!("_cx{i}"));
                        }
                        if *argc == 0 {
                            out.push_str(&format!("    st[sp++] = {sym}(NULL, 0);\n"));
                        } else {
                            out.push_str(&format!(
                                "    {{ zz_value _argv[{argc}] = {{{argv}}}; st[sp++] = {sym}(_argv, {argc}); }}\n"
                            ));
                        }
                        for i in 0..*argc {
                            out.push_str(&format!("    zz_release(&_cx{i});\n"));
                        }
                    }
                    Callee::Native(name, _) => {
                        let dotted = name.clone();
                        let target = self.resolve_call(&dotted)?;
                        match target {
                            CallTarget::Own(sym) => {
                                let mut argv = String::new();
                                for i in 0..*argc {
                                    if i > 0 {
                                        argv.push_str(", ");
                                    }
                                    argv.push_str(&format!("_cx{i}"));
                                }
                                if *argc == 0 {
                                    out.push_str(&format!("    st[sp++] = {sym}(NULL, 0);\n"));
                                } else {
                                    out.push_str(&format!(
                                        "    {{ zz_value _argv[{argc}] = {{{argv}}}; st[sp++] = {sym}(_argv, {argc}); }}\n"
                                    ));
                                }
                                for i in 0..*argc {
                                    out.push_str(&format!("    zz_release(&_cx{i});\n"));
                                }
                            }
                            CallTarget::CImpl(impl_name, needs_rt) => {
                                self.note_native(impl_name, needs_rt, &dotted);
                                reseat(out);
                                self.emit_native_call(impl_name, *argc, out)?;
                            }
                        }
                    }
                    Callee::Unknown => {
                        return Err(ChunkError::op("unresolvable Call callee"));
                    }
                }
                out.push_str("    }\n");
            }
            Op::CallPath { parts, argc, .. } => {
                let joined = parts
                    .iter()
                    .map(|id| self.str(*id).to_string())
                    .collect::<Vec<_>>()
                    .join(".");
                let target = self.resolve_call(&joined).or_else(|_| {
                    let std_name = format!("std.{joined}");
                    self.resolve_call(&std_name)
                })?;
                match target {
                    CallTarget::Own(sym) => {
                        // Twin-to-twin shortcut happens in the twin
                        // emitter; boxed bodies call boxed symbols.
                        self.emit_own_call(&sym, *argc, out);
                    }
                    CallTarget::CImpl(impl_name, needs_rt) => {
                        let dotted = joined.clone();
                        self.note_native(impl_name, needs_rt, &dotted);
                        self.emit_native_call(impl_name, *argc, out)?;
                    }
                }
            }
            Op::CallNative { name, argc } => {
                let dotted = self.str(*name).to_string();
                let target = self.resolve_call(&dotted).or_else(|_| {
                    let std_name = format!("std.{dotted}");
                    self.resolve_call(&std_name)
                })?;
                match target {
                    CallTarget::Own(sym) => self.emit_own_call(&sym, *argc, out),
                    CallTarget::CImpl(impl_name, needs_rt) => {
                        self.note_native(impl_name, needs_rt, &dotted);
                        self.emit_native_call(impl_name, *argc, out)?;
                    }
                }
            }
            _ => return Err(ChunkError::op(op.name())),
        }
        Ok(())
    }
}

impl<'a> Emitter<'a> {
    /// Emit the unboxed `int64_t` twin for an int-closed function.
    /// Only twin allowlisted ops reach here (see [`closable`]); every
    /// value is a raw `int64_t`, comparisons yield 0/1, and calls to
    /// twin targets are direct C calls.
    fn emit_twin(&mut self, id: FuncId, out: &mut String) -> Result<(), ChunkError> {
        let f = self
            .module
            .funcs
            .get(id.0 as usize)
            .ok_or_else(|| ChunkError::op("func id"))?;
        let sym = self.fsym(id);
        let nparams = f.params.len();
        // Frame: unified int stack; slots below, pushes above.
        let mut max_slot = 0usize;
        let mut any_slot = false;
        for op in &f.code {
            match op {
                Op::LoadSlot(s) | Op::StoreSlot(s) => {
                    any_slot = true;
                    max_slot = max_slot.max(*s as usize);
                }
                _ => {}
            }
        }
        let slots = if any_slot { max_slot + 1 } else { 0 };
        let frame = slots + f.max_stack as usize + 32;
        let params = (0..nparams)
            .map(|i| format!("int64_t _p{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("static int64_t {sym}_u({params}) {{\n"));
        out.push_str(&format!("    int64_t st[{frame}];\n"));
        out.push_str(&format!("    int sp = {nparams};\n"));
        for i in 0..nparams {
            out.push_str(&format!("    st[{i}] = _p{i};\n"));
        }
        let mut targets: HashSet<u32> = HashSet::new();
        for op in &f.code {
            match op {
                Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
                    targets.insert(*t);
                }
                _ => {}
            }
        }
        for (pc, op) in f.code.iter().enumerate() {
            if targets.contains(&(pc as u32)) {
                out.push_str(&format!("L{pc}_u:;\n"));
            }
            self.emit_twin_op(op, out)?;
        }
        if targets.contains(&(f.code.len() as u32)) {
            out.push_str(&format!("L{}_u:;\n", f.code.len()));
        }
        out.push_str("    return st[--sp];\n}\n");
        Ok(())
    }

    fn emit_twin_op(&self, op: &Op, out: &mut String) -> Result<(), ChunkError> {
        use zz_ir::op::BinOp as B;
        use zz_ir::op::UnOp as U;
        match op {
            Op::PushConst(c) => {
                let v = self
                    .module
                    .consts
                    .get(c.0 as usize)
                    .ok_or_else(|| ChunkError::op("const id"))?;
                match v {
                    Const::Int(i) => out.push_str(&format!("    st[sp++] = {};\n", c_int(*i))),
                    _ => return Err(ChunkError::op("non-int const in twin")),
                }
            }
            Op::Pop => out.push_str("    sp--;\n"),
            Op::Swap => out.push_str("    { int64_t _t = st[sp-1]; st[sp-1] = st[sp-2]; st[sp-2] = _t; }\n"),
            Op::LoadSlot(s) => out.push_str(&format!("    st[sp++] = st[{s}];\n")),
            Op::StoreSlot(s) => out.push_str(&format!("    st[{s}] = st[--sp];\n")),
            Op::IntAdd => out.push_str("    { int64_t _b = st[--sp]; int64_t _a = st[--sp]; st[sp++] = _a + _b; }\n"),
            Op::IntSub => out.push_str("    { int64_t _b = st[--sp]; int64_t _a = st[--sp]; st[sp++] = _a - _b; }\n"),
            Op::IntMul => out.push_str("    { int64_t _b = st[--sp]; int64_t _a = st[--sp]; st[sp++] = _a * _b; }\n"),
            Op::IntDiv => out.push_str("    { int64_t _b = st[--sp]; int64_t _a = st[--sp]; if (_b == 0) { fprintf(stderr, \"zz error: division by zero\\n\"); exit(1); } if (_a == INT64_MIN && _b == -1) { fprintf(stderr, \"zz error: integer overflow in division\\n\"); exit(1); } st[sp++] = _a / _b; }\n"),
            Op::IntRem => out.push_str("    { int64_t _b = st[--sp]; int64_t _a = st[--sp]; if (_b == 0) { fprintf(stderr, \"zz error: modulo by zero\\n\"); exit(1); } if (_a == INT64_MIN && _b == -1) { fprintf(stderr, \"zz error: integer overflow in modulo\\n\"); exit(1); } st[sp++] = _a % _b; }\n"),
            Op::IntNeg => out.push_str("    st[sp-1] = -st[sp-1];\n"),
            Op::BinOp(b) => {
                let e = match b {
                    B::Add => "_a + _b",
                    B::Sub => "_a - _b",
                    B::Mul => "_a * _b",
                    B::Div => "_a / _b",
                    B::Rem => "_a % _b",
                    B::Eq => "_a == _b",
                    B::Ne => "_a != _b",
                    B::Lt => "_a < _b",
                    B::Gt => "_a > _b",
                    B::Le => "_a <= _b",
                    B::Ge => "_a >= _b",
                    _ => return Err(ChunkError::op("non-int binop in twin")),
                };
                // Div/Rem by zero trap like the VM (checked above for
                // IntDiv/IntRem; BinOp forms need the same guard).
                if matches!(b, B::Div | B::Rem) {
                    let e2 = if matches!(b, B::Div) { "_a / _b" } else { "_a % _b" };
                    out.push_str(&format!("    {{ int64_t _b = st[--sp]; int64_t _a = st[--sp]; if (_b == 0) {{ fprintf(stderr, \"zz error: division by zero\\n\"); exit(1); }} st[sp++] = {e2}; }}\n"));
                } else {
                    out.push_str(&format!("    {{ int64_t _b = st[--sp]; int64_t _a = st[--sp]; st[sp++] = {e}; }}\n"));
                }
            }
            Op::UnOp(u) => match u {
                U::Neg => out.push_str("    st[sp-1] = -st[sp-1];\n"),
                U::Pos => {}
                U::BitNot => out.push_str("    st[sp-1] = ~st[sp-1];\n"),
                _ => return Err(ChunkError::op("non-int unop in twin")),
            },
            Op::Jump(t) => out.push_str(&format!("    goto L{t}_u;\n")),
            Op::JumpIfFalse(t) => out.push_str(&format!("    {{ int64_t _c = st[--sp]; if (!_c) goto L{t}_u; }}\n")),
            Op::JumpIfTrue(t) => out.push_str(&format!("    {{ int64_t _c = st[--sp]; if (_c) goto L{t}_u; }}\n")),
            Op::JumpIfFalseBool(t) => out.push_str(&format!("    {{ int64_t _c = st[--sp]; if (!_c) goto L{t}_u; }}\n")),
            Op::Return => out.push_str("    return st[--sp];\n"),
            Op::Safepoint => out.push_str("    ;\n"),
            Op::CallPath { parts, argc, .. } => {
                let joined = parts
                    .iter()
                    .map(|id| {
                        self.module.strings.get(id.0 as usize).cloned().unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                if !self.twins.contains(&joined) {
                    return Err(ChunkError::op("twin calls non-twin"));
                }
                let sym = self.csym(&joined);
                // Sequenced pops first (argument evaluation order);
                // the call itself never touches `sp`.
                out.push_str("    {\n");
                for i in (0..*argc).rev() {
                    out.push_str(&format!("    int64_t _ta{i} = st[--sp];\n"));
                }
                let args = (0..*argc)
                    .map(|i| format!("_ta{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("    st[sp++] = {sym}_u({args});\n"));
                out.push_str("    }\n");
            }
            _ => return Err(ChunkError::op("non-twin op in twin")),
        }
        Ok(())
    }
}

/// Chunk-local preamble: store helper + immortal range boxes.
const CHUNK_PREAMBLE: &str = r#"
static inline void zz_cstore(zz_value *dst, zz_value v) {
    zz_release(dst);
    *dst = zz_clone(v);
    zz_release(&v);
}

// IR ranges box (start, end, step). The C runtime's range is a
// degenerate start-marker (enough for HIR's special-cased loops but
// not for a value-driven loop); boxes are immutable after creation,
// so clone-sharing is sound. Immortal by design: `zz_clone` shares
// and `zz_release` ignores them (no UAF, no double-free); each
// `MakeRange` execution leaks 32 bytes until the ownership pass
// interns them (#188.1, #254).
typedef struct { int64_t start, end, step; } zz_crange;
static inline zz_value zz_crange_new(int64_t s, int64_t e, int64_t st) {
    zz_crange *b = (zz_crange*)malloc(sizeof(zz_crange));
    if (!b) { fprintf(stderr, "zz error: out of memory\n"); exit(1); }
    b->start = s; b->end = e; b->step = st;
    zz_value v; v.tag = ZZ_RANGE; v.payload = (zz_value*)b;
    return v;
}
"#;

/// Compile a verified IR module to C source behind the HIR backend's
/// `LoweredC` shape (source + link flags), so the build pipeline can
/// treat both backends uniformly.
///
/// `entry_main` is the dotted main (e.g. `fib35.main`); the module
/// entry chunk (top-level effects) runs first, then `entry_main`.
/// Fails cleanly via [`coverage`] before emitting anything.
pub fn build_module(
    module: &Module,
    entry_main: &str,
) -> Result<super::lower::LoweredC, ChunkError> {
    coverage(module)?;
    let mut em = Emitter {
        module,
        csyms: HashMap::new(),
        twins: closable_all(module),
        used_natives: Vec::new(),
        needs_native_rt: false,
        needs_float_fmt: false,
    };
    for f in &module.funcs {
        let nm = module
            .strings
            .get(f.name.0 as usize)
            .ok_or_else(|| ChunkError::op("func name id"))?;
        em.csyms.insert(nm.clone(), em.csym(nm));
    }
    if !em.csyms.contains_key(entry_main) {
        return Err(ChunkError::name(&format!("entry {entry_main}")));
    }
    // Origins per function for `Call` dispatch.
    let mut origins_map: HashMap<usize, Vec<Vec<Callee>>> = HashMap::new();
    for (i, f) in module.funcs.iter().enumerate() {
        let resolve = |id: StrId, pc: usize| -> Callee {
            let name = match module.strings.get(id.0 as usize) {
                Some(s) => s,
                None => return Callee::Unknown,
            };
            if super::lower::native_impl(name).is_some() || super::ffi_impl(name).is_some() {
                return Callee::Native(name.clone(), pc);
            }
            let std_name = format!("std.{name}");
            if super::lower::native_impl(&std_name).is_some()
                || super::ffi_impl(&std_name).is_some()
            {
                return Callee::Native(std_name, pc);
            }
            if em.csyms.contains_key(name) {
                return Callee::Func(name.clone(), pc);
            }
            Callee::Unknown
        };
        origins_map.insert(i, origins(&f.code, &resolve));
    }
    let mut bodies = String::new();
    // Forward declarations (boxed symbols only; twins are static and
    // called after definition order — emit twins first instead).
    for f in &module.funcs {
        let nm = module
            .strings
            .get(f.name.0 as usize)
            .ok_or_else(|| ChunkError::op("func name id"))?;
        let sym = em.csym(nm);
        bodies.push_str(&format!(
            "static zz_value {sym}(zz_value *args, size_t argc);\n"
        ));
        if em.twins.contains(nm) {
            let nparams = f.params.len();
            let params = (0..nparams)
                .map(|_| "int64_t".to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bodies.push_str(&format!("static int64_t {sym}_u({params});\n"));
        }
    }
    for (i, _) in module.funcs.iter().enumerate() {
        em.emit_func(FuncId(i as u32), &origins_map, &mut bodies)?;
    }
    // Entry glue: top-level chunk effects, then `entry_main`.
    let entry_sym = em.csym(
        module
            .strings
            .get(module.entry.0 as usize)
            .map(|_| {
                module
                    .funcs
                    .get(module.entry.0 as usize)
                    .and_then(|f| module.strings.get(f.name.0 as usize))
                    .map(String::as_str)
                    .unwrap_or("main")
            })
            .unwrap_or("main"),
    );
    let main_sym = em.csym(entry_main);
    let main_arity = module
        .funcs
        .iter()
        .find(|f| {
            module
                .strings
                .get(f.name.0 as usize)
                .map(|n| n == entry_main)
                .unwrap_or(false)
        })
        .map(|f| f.params.len())
        .unwrap_or(0);
    let main_call = if main_arity == 0 {
        format!("{main_sym}(NULL, 0)")
    } else {
        // `main(cli_args)` receives argv (mirrors the VM + HIR backend).
        format!(
            "{{ int _e = 0; zz_value _cli = zz_env_args(zz_unit(), &_e); {main_sym}(&_cli, 1); }}"
        )
    };
    // Link flags from the referenced natives (same helpers as HIR).
    let native_names: HashSet<String> = em.used_natives.iter().map(|(n, _, _)| n.clone()).collect();
    // Headers + user code only: the TU links the precompiled runtime
    // archive (`-lzz_rt`), exactly like the HIR backend's precompiled
    // path. Quoted includes are concatenated-header residue — strip
    // them (system includes like <math.h> stay).
    let raw = format!(
        "{runtime_h}\n{chunk_pre}\n// ---- chunk-compiled code ----\n{bodies}\nvoid zz_main(void) {{\n    zz_value _t = {entry_sym}(NULL, 0);\n    zz_release(&_t);\n}}\n\nint zz_call_main(void) {{\n    zz_value _r = {main_call};\n    return zz_main_result_code(_r);\n}}\n",
        runtime_h = crate::RUNTIME_H,
        chunk_pre = CHUNK_PREAMBLE,
    );
    let source = raw
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("#include \"") && t.ends_with('"'))
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(super::lower::LoweredC {
        source,
        needs_native_rt: em.needs_native_rt || crate::ffi::needs_native_rt(&native_names),
        needs_pg_link: crate::ffi::needs_pg_link(&native_names),
        needs_float_fmt: em.needs_float_fmt,
        needs_curl: crate::ffi::needs_curl_link(&native_names),
        needs_sqlite: crate::ffi::needs_sqlite_link(&native_names),
    })
}
