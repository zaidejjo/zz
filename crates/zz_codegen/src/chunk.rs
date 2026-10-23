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

use zz_ir::{Const, ConstId, FuncDef, FuncId, IrType, Module, Op, Pattern, StrId};

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
            | Op::LoadPath(_)
            | Op::DefineVar(_)
            | Op::StoreVar(_)
            | Op::StorePath(_)
            | Op::RegisterEnum { .. }
            | Op::MakeEnum { .. }
            | Op::MatchArm { .. }
            | Op::MatchGuard { .. }
            | Op::MatchError
            | Op::IfLetMatch { .. }
            | Op::ExitScope
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
            | Op::MakeDict(_)
            | Op::MakeVariant { .. }
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
            Op::MatchArm { next, .. } | Op::MatchGuard { next, .. } => vec![*next],
            Op::IfLetMatch { els, .. } => vec![*els],
            _ => Vec::new(),
        }
    }
    fn falls(op: &Op) -> bool {
        // MatchError traps (non-exhaustive match aborts) — never falls
        // through; a fallthrough edge would feed dead state downstream.
        !matches!(
            op,
            Op::Jump(_) | Op::Return | Op::Break | Op::Continue | Op::MatchError
        )
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
            Op::LoadSlot(_) | Op::LoadVar(_) | Op::LoadPath(_) => (0, 1),
            Op::StoreSlot(_) => (1, 0),
            Op::DefineVar(_) => (1, 1),
            Op::StoreVar(_) | Op::StorePath(_) => (1, 0),
            // `MatchArm`/`IfLetMatch` consume the scrutinee on the take
            // path; the miss path keeps it for the next test (the linear
            // walk sees the fall-through, exactly like the VM's height
            // fiction). `MatchGuard` drops the guard value.
            Op::MatchArm { .. } | Op::MatchGuard { .. } | Op::IfLetMatch { .. } => (1, 0),
            Op::MatchError | Op::ExitScope => (0, 0),
            Op::RegisterEnum { .. } => (0, 0),
            Op::MakeEnum { argc, .. } => (*argc as usize, 1),
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
            Op::MakeDict(n) => (2 * *n as usize, 1),
            Op::MakeVariant { has_arg, .. } => (usize::from(*has_arg), 1),
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
    // Loop-exit prefix lengths, mirroring the verifier: `ForNext`
    // exhaustion pops the index/vars AND the iterable, so a loop-exit
    // edge carries only the setup-entry prefix (length L-1); a
    // `WhileCond` exit carries L. Without this, abstract stacks grow by
    // the inner frame on every lap around a nested loop and the walk
    // never stabilizes (single loops terminate only because no setup
    // sits inside their cycle).
    let mut exit_lens: HashMap<usize, usize> = HashMap::new();
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
        // Record loop-entry prefix lengths before applying the op (the
        // state still holds the setup prologue: [..., PH, iterable]).
        // `saturating_sub` keeps hostile input panic-free (a wrong
        // length only degrades precision to Unknown downstream).
        // Setups dominate their exits on compiler output, so the entry
        // is always recorded before any exit edge reads it.
        match &code[pc] {
            Op::ForSetup { exit, .. } => {
                exit_lens
                    .entry(*exit as usize)
                    .or_insert(st.len().saturating_sub(1));
            }
            Op::WhileSetup { exit, .. } => {
                exit_lens.entry(*exit as usize).or_insert(st.len());
            }
            _ => {}
        }
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
        // Loop-exhaustion edges carry the setup-entry prefix, not the
        // end-of-body stack (see above).
        let exit_of = match &code[pc] {
            Op::ForNext { exit, .. } | Op::WhileCond { exit } => Some(*exit as usize),
            _ => None,
        };
        // A pattern miss either reloads the scrutinee (`MatchArm` with a
        // guard scheme) or keeps it on the stack for the next test: those
        // jump edges carry the *entry* state, not the take path's.
        let miss_keeps = matches!(
            &code[pc],
            Op::MatchArm { restore: true, .. } | Op::IfLetMatch { .. }
        );
        for s in succs {
            let mut edge = if miss_keeps && s != pc + 1 {
                out[pc].clone()
            } else {
                next.clone()
            };
            if exit_of == Some(s) {
                if let Some(want) = exit_lens.get(&s) {
                    edge.truncate(*want);
                }
            }
            match &state[s] {
                None => {
                    state[s] = Some(edge);
                    work.push(s);
                }
                Some(cur) => {
                    let mut merged = Vec::new();
                    let len = cur.len().max(edge.len());
                    let mut changed = false;
                    for i in 0..len {
                        let (a, b) = (
                            cur.get(i).unwrap_or(&Callee::Unknown),
                            edge.get(i).unwrap_or(&Callee::Unknown),
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

/// Static jump targets of one op (mirrors the verifier's edge set; used
/// for label collection and peel region checks).
fn jump_targets(op: &Op) -> Vec<u32> {
    match op {
        Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
            vec![*t]
        }
        Op::ForSetup { exit, header, .. } => vec![*exit, *header],
        Op::ForNext { exit, .. } | Op::WhileSetup { exit, .. } | Op::WhileCond { exit } => {
            vec![*exit]
        }
        Op::MatchArm { next, .. } | Op::MatchGuard { next, .. } => vec![*next],
        Op::IfLetMatch { els, .. } => vec![*els],
        _ => Vec::new(),
    }
}

/// Int-declared frame slots eligible for fusion windows in one function:
/// `locals` entries that are exactly `Int`. Windows only read slots
/// through int guards (fail-closed), so take homes and reused slots need
/// no exclusion here — the take-push tail matches structurally in
/// [`int_fuse`] instead.
fn int_slots(module: &Module, f: &FuncDef) -> HashSet<u16> {
    let mut out = HashSet::new();
    for (i, id) in f.locals.iter().enumerate() {
        if i > u16::MAX as usize {
            break;
        }
        if matches!(module.types.get(id.0 as usize), Some(IrType::Int)) {
            out.insert(i as u16);
        }
    }
    out
}

/// Scan for fusable unboxed-int windows (see [`FusedTail`]). Runs after
/// [`fuse_scan`]: pcs it already claimed, and any window interior that is
/// a jump target, are left alone (entering mid-window would skip emission).
/// Shape: a straight-line int expression (loads of int slots, int consts,
/// int-closed arithmetic) terminated by an int store or a take-push tail.
/// Expression ops are capped (12) so matching stays linear; longest match
/// wins and the walk advances past it.
/// A peelable counted loop: `for v in <range|[int]>` with a straight-line
/// fused body. The peel replaces `[ForNext..exit]` with a raw C loop;
/// `ForSetup` keeps its boxed emission (placeholder pushes + `_lbase`
/// bookkeeping), so exit stack accounting is untouched.
#[derive(Debug, Clone)]
struct Peel {
    /// Exit pc (stays visible; peel jumps here when exhausted).
    exit: usize,
    /// Iteration variable slot (never `u16::MAX` — env vars bail).
    var: u16,
    /// Fused body window start + length (from [`int_fuse`]).
    body: usize,
    body_len: usize,
    body_tail: FusedTail,
    /// What is iterated (producer-gated at match time).
    source: PeelSource,
    /// Int slots promoted to C locals for the loop (see below): every
    /// int slot the body loads or stores, except the iteration variable
    /// (which substitutes separately). Ints cannot alias, so entry
    /// unbox + exit write-back brackets all body accesses transparently:
    /// entry guards only loaded slots (a store-only slot keeps boxed
    /// exactness — no new traps), write-back releases the old value
    /// (sound for any old content).
    promo_loads: Vec<u16>,
    promo_stores: Vec<u16>,
}

/// Source a peeled loop iterates: int bounds from a range box (whose
/// producer is an adjacent `MakeRange` — our own emission, so the tag
/// is proven), or an int-element array slot (tag-checked at runtime).
#[derive(Debug, Clone, Copy)]
enum PeelSource {
    Range,
    Array,
}

/// Scan for peelable loops (see [`Peel`]). Strict shapes only — anything
/// else stays boxed:
/// - `ForSetup{E,H,1 var}` with an adjacent producer (`MakeRange`, or
///   `LoadSlot` of an array-declared slot), `Safepoint`s only between
///   setup and `ForNext{E}!in_env` at/below the header;
/// - body = one fused window plus optional safepoints and an optional
///   `[PushConst unit, SetLoopResult]` tail, then `Jump(H)`;
/// - no calls, jumps (other than the back-edge), breaks, nesting, or
///   indexing anywhere in `[S..E]`; no outside targets into `(S,E)`.
fn peel_scan(
    f: &FuncDef,
    module: &Module,
    ints: &HashSet<u16>,
    targets: &HashSet<u32>,
    fused: &HashMap<usize, (usize, FusedTail)>,
) -> (HashMap<usize, Peel>, HashSet<usize>) {
    let mut peels = HashMap::new();
    let mut skip = HashSet::new();
    let code = &f.code;
    let is_array_slot = |s: u16| -> bool {
        match f
            .locals
            .get(s as usize)
            .and_then(|id| module.types.get(id.0 as usize))
        {
            Some(IrType::Array(e)) => matches!(module.types.get(e.0 as usize), Some(IrType::Int)),
            _ => false,
        }
    };
    // kth ForNext ↔ kth vartab entry.
    for (spc, op) in code.iter().enumerate() {
        let (exit, header) = match op {
            Op::ForSetup {
                exit,
                header,
                num_vars,
            } if *num_vars == 1 => (*exit as usize, *header as usize),
            Op::ForSetup { .. } => continue,
            _ => continue,
        };
        if spc == 0 || exit <= spc || header > spc + 2 || header <= spc {
            // `header` sits at/below the `ForNext`, right after setup
            // (only `Safepoint`s between — anything else bails).
            if !(header == spc + 1
                || (header == spc + 2 && matches!(code.get(spc + 1), Some(Op::Safepoint))))
            {
                continue;
            }
        }
        // Producer immediately before setup: range bounds or int array.
        // `MakeRange` adjacency proves a range box (our own emission is
        // range-or-trap); an array slot is tag-checked at runtime.
        let (source, arr_slot) = match (spc.checked_sub(1), code.get(spc.wrapping_sub(1))) {
            (Some(_), Some(Op::MakeRange)) => (PeelSource::Range, None),
            (Some(_), Some(Op::LoadSlot(a))) if is_array_slot(*a) => (PeelSource::Array, Some(*a)),
            _ => continue,
        };
        // `ForNext` at/below the header with a matching exit.
        let mut fpc = None;
        for p in header..header + 2 {
            match code.get(p) {
                Some(Op::ForNext {
                    exit: e, in_env, ..
                }) if *e as usize == exit && !in_env => {
                    fpc = Some(p);
                    break;
                }
                Some(Op::Safepoint) => continue,
                _ => break,
            }
        }
        let fpc = match fpc {
            Some(p) => p,
            None => continue,
        };
        // vartab entry for this `ForNext` (kth in function order).
        let k = code[..fpc]
            .iter()
            .filter(|o| matches!(o, Op::ForNext { .. }))
            .count();
        let var = match f.vartab.get(k).and_then(|e| e.first()) {
            Some(v) if *v != u16::MAX => *v,
            _ => continue,
        };
        // Body: safepoints, one fused window, optional unit-result tail,
        // then the back-edge jump. Nothing else.
        let mut p = fpc + 1;
        while matches!(code.get(p), Some(Op::Safepoint)) {
            p += 1;
        }
        let (body, body_len, body_tail) = match fused.get(&p) {
            Some((len, tail)) => (p, *len, *tail),
            None => continue,
        };
        p += body_len;
        while matches!(code.get(p), Some(Op::Safepoint)) {
            p += 1;
        }
        let has_tail = matches!(
            (code.get(p), code.get(p + 1)),
            (Some(Op::PushConst(c)), Some(Op::SetLoopResult))
                if matches!(module.consts.get(c.0 as usize), Some(Const::Unit))
        );
        if has_tail {
            p += 2;
        }
        match code.get(p) {
            Some(Op::Jump(h)) if *h as usize == header => {}
            _ => continue,
        }
        let jmp = p;
        if exit <= jmp {
            continue;
        }
        // No outside targets into the peeled region (the back-edge to the
        // header is the only interior edge): entering mid-loop would run
        // the raw body without the setup state the boxed shape provides.
        let mut ok = true;
        for t in targets {
            let t = *t as usize;
            if t > spc && t < exit && t != header {
                ok = false;
                break;
            }
        }
        // The header itself is only reachable via fall-through and the
        // back-edge: any other jump into it bails (same reason).
        if ok {
            for (jpc, op) in code.iter().enumerate() {
                if jpc >= spc && jpc <= jmp {
                    continue;
                }
                if jump_targets(op).contains(&(header as u32)) {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        // Array peel must not rebind or index-mutate its iterable
        // mid-loop (`len` is hoisted): take tails and stores to the array
        // slot bail out (boxed handles them).
        if let Some(a) = arr_slot {
            if matches!(source, PeelSource::Array) {
                if let FusedTail::Take(t) = body_tail {
                    if t == a {
                        continue;
                    }
                }
                // No rebinding the iterable slot anywhere in the body.
                if code[body..body + body_len]
                    .iter()
                    .any(|o| matches!(o, Op::StoreSlot(s) if *s == a))
                {
                    continue;
                }
            }
        }
        // Body windows must be net-zero on the stack (peeled bodies do no
        // stack traffic): store tails qualify; take tails only with a
        // fused expression prefix (bare take pops the stack element).
        match body_tail {
            FusedTail::Store(_) => {}
            FusedTail::Take(_) if body_len > 6 => {}
            FusedTail::Take(_) => continue,
        }
        // The body must not write the iteration variable: an explicit
        // `StoreSlot(var)` (or take-append into it) would corrupt the
        // raw counter the peel substitutes, while boxed code discards
        // such stores at the next `ForNext`. Bail to boxed.
        let writes_var = code[body..body + body_len]
            .iter()
            .any(|o| matches!(o, Op::StoreSlot(s) if *s == var))
            || matches!(body_tail, FusedTail::Take(t) if t == var);
        if writes_var {
            continue;
        }
        // Promotion set: every int slot the body loads or stores (except
        // the iteration variable, which substitutes separately). Split
        // loads (entry-guarded) from stores (write-back only) so trap
        // behavior matches boxed code exactly.
        let mut promo_loads = Vec::new();
        let mut promo_stores = Vec::new();
        for op in &code[body..body + body_len] {
            match op {
                Op::LoadSlot(s) if ints.contains(s) && *s != var && !promo_loads.contains(s) => {
                    promo_loads.push(*s);
                }
                Op::StoreSlot(d) if ints.contains(d) && *d != var && !promo_stores.contains(d) => {
                    promo_stores.push(*d);
                }
                _ => {}
            }
        }
        // Claim [fornext..exit]: the peel emits at `fpc`, `exit` stays.
        for q in fpc + 1..exit {
            skip.insert(q);
        }
        peels.insert(
            fpc,
            Peel {
                exit,
                var,
                body,
                body_len,
                body_tail,
                source,
                promo_loads,
                promo_stores,
            },
        );
    }
    (peels, skip)
}

fn int_fuse(
    f: &FuncDef,
    consts: &[Const],
    ints: &HashSet<u16>,
    targets: &HashSet<u32>,
    taken: &HashSet<usize>,
) -> (HashMap<usize, (usize, FusedTail)>, HashSet<usize>) {
    /// Expression-op effect on the ministack, or `None` when the op can
    /// never be part of an int expression.
    fn expr_effect(ints: &HashSet<u16>, consts: &[Const], op: &Op) -> Option<i32> {
        match op {
            Op::LoadSlot(s) if ints.contains(s) => Some(1),
            Op::PushConst(c) => match consts.get(c.0 as usize) {
                Some(Const::Int(_)) => Some(1),
                _ => None,
            },
            Op::IntAdd | Op::IntSub | Op::IntMul | Op::IntDiv | Op::IntRem => Some(-1),
            Op::IntNeg => Some(0),
            Op::BinOp(zz_ir::op::BinOp::Add | zz_ir::op::BinOp::Sub | zz_ir::op::BinOp::Mul) => {
                Some(-1)
            }
            Op::BinOp(_) => None,
            _ => None,
        }
    }
    /// Take-push tail at `t` for array slot `a`: `[loadslot a, unit,
    /// storeslot a, swap, arraypush, storeslot a]`. The element arrives
    /// either on the stack (bare six-op tail) or from a fused prefix
    /// expression — both handled at emission.
    fn take_tail(code: &[Op], consts: &[Const], t: usize) -> Option<u16> {
        if t + 5 >= code.len() {
            return None;
        }
        let is_unit = matches!(&code[t + 1], Op::PushConst(c) if matches!(consts.get(c.0 as usize), Some(Const::Unit)));
        if let (Op::LoadSlot(a), Op::StoreSlot(b), Op::StoreSlot(c)) =
            (&code[t], &code[t + 2], &code[t + 5])
        {
            if a == b
                && b == c
                && is_unit
                && matches!(&code[t + 3], Op::Swap)
                && matches!(&code[t + 4], Op::ArrayPush)
            {
                return Some(*a);
            }
        }
        None
    }
    let mut fused = HashMap::new();
    let mut skip = HashSet::new();
    let code = &f.code;
    let mut pc = 0usize;
    while pc < code.len() {
        if taken.contains(&pc) {
            pc += 1;
            continue;
        }
        let mut placed: Option<(usize, FusedTail)> = None;
        // Latest expression end first (longest match wins); expression
        // itself is capped so the scan stays linear.
        let latest = (pc + 13).min(code.len());
        let mut t = latest;
        while t > pc {
            // Interior [pc+1..t] must be free (window starts at `pc`,
            // which may itself be targeted).
            if (pc + 1..t).any(|p| targets.contains(&(p as u32)) || taken.contains(&p)) {
                t -= 1;
                continue;
            }
            // Expression [pc..t] must balance 0 → 1.
            let mut depth = 0i32;
            let mut ok = true;
            for op in &code[pc..t] {
                match expr_effect(ints, consts, op) {
                    Some(e) => {
                        depth += e;
                        if depth < 1 && op_needs_value(op) {
                            ok = false;
                            break;
                        }
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && depth == 1 {
                // Terminator at `t`: int store, or take-push tail.
                if t < code.len() {
                    if let Op::StoreSlot(d) = &code[t] {
                        if ints.contains(d) && !targets.contains(&(t as u32)) && !taken.contains(&t)
                        {
                            placed = Some((t - pc + 1, FusedTail::Store(*d)));
                            break;
                        }
                    }
                }
                if let Some(a) = take_tail(code, consts, t) {
                    if !(t..t + 6).any(|p| targets.contains(&(p as u32)) || taken.contains(&p)) {
                        placed = Some((t - pc + 6, FusedTail::Take(a)));
                        break;
                    }
                }
            }
            t -= 1;
        }
        // Bare take-push tail with the element already on the stack.
        if placed.is_none() {
            if let Some(a) = take_tail(code, consts, pc) {
                if !(pc + 1..pc + 6).any(|p| targets.contains(&(p as u32)) || taken.contains(&p)) {
                    placed = Some((6, FusedTail::Take(a)));
                }
            }
        }
        if let Some((len, tail)) = placed {
            // Skip the window tail: the fused statement emits at `pc`,
            // so `pc` itself stays visible to the walk.
            for p in pc + 1..pc + len {
                skip.insert(p);
            }
            fused.insert(pc, (len, tail));
            pc += len;
        } else {
            pc += 1;
        }
    }
    (fused, skip)
}

/// True when `op` consumes ministack values (underflow-checked by the
/// matcher): binary ops need depth ≥ 2, negation ≥ 1, producers never
/// underflow.
fn op_needs_value(op: &Op) -> bool {
    matches!(
        op,
        Op::IntAdd | Op::IntSub | Op::IntMul | Op::IntDiv | Op::IntRem | Op::IntNeg | Op::BinOp(_)
    )
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

/// Returns `(fused, skip, borrow)`: `fused[pc] = s` means the
/// `StoreIndexOp` at `pc` fuses onto slot `s`; `skip` holds the
/// following `StoreSlot(s)` pcs to skip; `borrow` holds the subset of
/// fused pcs whose `LoadSlot` may also be skipped (see below).
///
/// Borrow-skip: when neither the `LoadSlot` (`lpc`) nor the
/// `StoreIndexOp` (`pc`) is a jump target, the load is not emitted at
/// all — the window borrows `st[s]` directly instead of cloning it
/// (saving an atomic refcount pair per store). Sound because stack
/// values are linear (each pushed value is popped exactly once, so the
/// clone this load would have pushed is consumed exactly once, by this
/// op — `Swap` only reorders, and the tag model tracks through it),
/// the middle is straight-line call/store-free code (observably
/// identical one slot lower — balance is verifier-guaranteed, and
/// every straight-line value op is position-relative except absolute
/// slot accesses, which are unaffected), and untargeted endpoints mean
/// no edge can enter expecting the pushed clone (or land on the store
/// expecting three stack values). Targeted endpoints keep the
/// clone-ful emission, which is today's behavior verbatim.
fn fuse_scan(code: &[Op]) -> (HashMap<usize, u16>, HashSet<usize>, HashSet<usize>) {
    // (pops, pushes) — mirrors `origins::apply` for the covered subset.
    fn effect(op: &Op) -> Option<(usize, usize)> {
        Some(match op {
            Op::PushConst(_) => (0, 1),
            Op::Pop => (1, 0),
            Op::PopN(n) => (*n as usize + 1, 1),
            Op::Swap => (0, 0),
            Op::Truthy => (1, 1),
            Op::LoadSlot(_) | Op::LoadVar(_) | Op::LoadPath(_) => (0, 1),
            Op::StoreSlot(_) => (1, 0),
            Op::DefineVar(_) => (1, 1),
            Op::StoreVar(_) | Op::StorePath(_) => (1, 0),
            // `MatchArm`/`IfLetMatch` consume the scrutinee on the take
            // path; the miss path keeps it for the next test (the linear
            // walk sees the fall-through, exactly like the VM's height
            // fiction). `MatchGuard` drops the guard value.
            Op::MatchArm { .. } | Op::MatchGuard { .. } | Op::IfLetMatch { .. } => (1, 0),
            Op::MatchError | Op::ExitScope => (0, 0),
            Op::RegisterEnum { .. } => (0, 0),
            Op::MakeEnum { argc, .. } => (*argc as usize, 1),
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
            Op::MakeDict(n) => (2 * *n as usize, 1),
            Op::MakeVariant { has_arg, .. } => (usize::from(*has_arg), 1),
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
        return (HashMap::new(), HashSet::new(), HashSet::new());
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
                return (HashMap::new(), HashSet::new(), HashSet::new());
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
    let mut borrow: HashSet<usize> = HashSet::new();
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
            // Borrow the home slot instead of cloning when no edge can
            // observe the difference (see header doc).
            if !is_target.contains(&lpc) && !is_target.contains(&pc) {
                borrow.insert(pc);
                skip.insert(lpc);
            }
        }
    }
    (fused, skip, borrow)
}
/// String-append fusion: `LoadSlot(s), PushConst(Str), BinOp(Add),
/// StoreSlot(s)` (adjacent, in that order) compiles to an in-place
/// `zz_str_append_lit` on the slot instead of clone → alloc+copy →
/// store-back (which is O(n) per append — quadratic for loop-carried
/// accumulators like strconcat).
///
/// Soundness: the four ops are adjacent with no jump target in the
/// interior (entering mid-window would skip the append), no other
/// claimant on those pcs (checked against the caller-provided skip
/// set, so int/store-index/peel windows always win ties), and the rhs
/// is a single string-literal push (no calls, no stores, no observable
/// evaluation — nothing can witness the missing clone). The runtime
/// helper reuses the buffer when uniquely owned (refs==1, not
/// interned) and grows it 2x amortized; shared/interned/arena sources
/// take the fresh-alloc path. A non-string slot (only reachable on
/// Unknown-typed or hostile modules — checker-typed programs always
/// hold a string here) takes a fail-closed boxed fallback with exact
/// `zz_binop` semantics instead of trapping or misappending.
///
/// Returns `(fused, skip)`: `fused[p] = (s, c)` means the `LoadSlot`
/// at `p` emits the whole window for slot `s` and literal const `c`;
/// `skip` holds the other three pcs.
fn strapp_scan(
    code: &[Op],
    consts: &[Const],
    targets: &HashSet<u32>,
    taken: &HashSet<usize>,
) -> (HashMap<usize, (u16, ConstId)>, HashSet<usize>) {
    let mut fused = HashMap::new();
    let mut skip = HashSet::new();
    if code.len() < 4 {
        return (fused, skip);
    }
    for p in 0..code.len() - 3 {
        if taken.contains(&p) {
            continue;
        }
        let (s, c) = match (&code[p], &code[p + 1], &code[p + 2], &code[p + 3]) {
            (Op::LoadSlot(s), Op::PushConst(c), Op::BinOp(b), Op::StoreSlot(d))
                if s == d && matches!(b, zz_ir::op::BinOp::Add) =>
            {
                (*s, *c)
            }
            _ => continue,
        };
        if !matches!(consts.get(c.0 as usize), Some(Const::Str(_))) {
            continue;
        }
        // Interior must be label-free (the start may itself be
        // targeted — it still emits) and unclaimed.
        if (p + 1..=p + 3).any(|q| targets.contains(&(q as u32)) || taken.contains(&q)) {
            continue;
        }
        if skip.contains(&p) {
            continue;
        }
        fused.insert(p, (s, c));
        skip.insert(p + 1);
        skip.insert(p + 2);
        skip.insert(p + 3);
    }
    (fused, skip)
}
/// Index-read fusion: `LoadSlot(a), <int idxexpr>, IndexOp` feeding an
/// int store or a bool branch compiles to guarded direct element access
/// instead of clone → runtime call → releases (an atomic refcount pair
/// plus a call per read — the dominant cost in index-driven loops like
/// sieve and arraywhile).
///
/// Window shape (all adjacent):
/// - `LoadSlot(a)` with `a` declared `Array(Int)` (store tail) or
///   `Array(Bool)` (branch tail). Declaration-checked, like
///   [`int_slots`]: checker-typed programs always hold the declared
///   array here, so the tag guard is assertive.
/// - idxexpr: 1..=8 ops from int loads, int consts, and trapping-free
///   `+ - *` (no div/rem, no calls, no stores at all).
/// - `IndexOp`, then either an int REST expr plus `StoreSlot(d)` with
///   `d` int-declared, or `JumpIfFalseBool`/`JumpIfTrue` (bool arrays).
/// - No jump target in the window interior (the start may be targeted),
///   no overlap with already-claimed pcs (every existing fusion wins
///   ties), no `StoreSlot(a)` in the window.
///
/// Guards mirror `zz_array_get` exactly (single `i += n` normalize,
/// then the OOB check) and every unboxing boundary is guarded with a
/// cold out-of-line trap — the same fail-closed discipline as
/// [`int_fuse`] and [`strapp_scan`]. Negative-index and OOB behavior
/// is therefore bit-identical to the boxed path (exit 1; message text
/// is never gated).
///
/// Returns `(fused, skip)`: `fused[start]` describes the window; `skip`
/// holds every other pc in it.
#[derive(Debug, Clone, Copy)]
enum IdxTail {
    /// REST int-expr stored to int-declared slot.
    Store(u16),
    /// Bool element branches to `target` (`jump_if_true` selects the
    /// polarity: false = `JumpIfFalseBool`, true = `JumpIfTrue`).
    Branch(u32, bool),
}

#[derive(Debug, Clone, Copy)]
struct IdxRead {
    arr: u16,
    /// Int-expr ops before the base load (the other operand, 0-8 ops).
    prefix_len: usize,
    /// Int-expr ops between the base load and `IndexOp` (1-8 ops).
    idx_len: usize,
    /// Int-expr ops between `IndexOp` and the terminator (0 for branches).
    rest_len: usize,
    tail: IdxTail,
    elem_bool: bool,
}

fn idxread_scan(
    f: &FuncDef,
    module: &Module,
    ints: &HashSet<u16>,
    targets: &HashSet<u32>,
    taken: &HashSet<usize>,
) -> (HashMap<usize, IdxRead>, HashSet<usize>) {
    /// Declared element kind of an array slot: `Some(false)` for
    /// `[int]`, `Some(true)` for `[bool]`, `None` otherwise.
    fn array_elem(module: &Module, f: &FuncDef, s: u16) -> Option<bool> {
        let ty = f
            .locals
            .get(s as usize)
            .and_then(|id| module.types.get(id.0 as usize));
        match ty {
            Some(IrType::Array(e)) => match module.types.get(e.0 as usize) {
                Some(IrType::Int) => Some(false),
                Some(IrType::Bool) => Some(true),
                _ => None,
            },
            _ => None,
        }
    }
    /// Effect of an int-expression op on the ministack, or `None` when
    /// the op can never appear in an idxexpr/REST expr.
    fn int_effect(ints: &HashSet<u16>, consts: &[Const], op: &Op) -> Option<i32> {
        match op {
            Op::LoadSlot(s) if ints.contains(s) => Some(1),
            Op::PushConst(c) => match consts.get(c.0 as usize) {
                Some(Const::Int(_)) => Some(1),
                _ => None,
            },
            Op::IntAdd | Op::IntSub | Op::IntMul => Some(-1),
            Op::BinOp(zz_ir::op::BinOp::Add | zz_ir::op::BinOp::Sub | zz_ir::op::BinOp::Mul) => {
                Some(-1)
            }
            _ => None,
        }
    }
    let code = &f.code;
    let mut fused = HashMap::new();
    let mut skip = HashSet::new();
    if code.len() < 4 {
        return (fused, skip);
    }
    let mut pc = 0usize;
    while pc + 3 < code.len() {
        if taken.contains(&pc) || skip.contains(&pc) {
            pc += 1;
            continue;
        }
        // Window value discipline (all counts are window values only —
        // below-window stack is never addressable, so every pop below
        // the phase floor fails matching and the shape stays boxed):
        // - prefix: optional int expr, 0 → P where P ∈ {0, 1};
        // - base: the single array load, P → P+1;
        // - idx: int expr, P+1 → P+2 (binaries need two values strictly
        //   above the protected `[X?, arr]` prefix, i.e. depth ≥ P+3 —
        //   popping the base and rebuilding the depth with other values
        //   would alias the wrong object, so it bails);
        // - IndexOp at exactly P+2, leaving P+1 (`[X?, elem]`);
        // - rest: int expr ending at exactly 1, then `StoreSlot`.
        // Phase caps (8/8/8) keep matching linear; longest match wins
        // by construction (the walk takes all it can).
        let mut q = pc;
        let mut w = 0i32;
        let mut ok = true;
        // Prefix: int producers and trapping-free `+ - *` only (every
        // other op — calls, stores, flow, non-int pushes — ends the
        // prefix; only an array load may follow).
        while q < code.len() && q - pc < 8 {
            if let Op::LoadSlot(a) = &code[q] {
                if array_elem(module, f, *a).is_some() {
                    if w == 0 || w == 1 {
                        break;
                    }
                    ok = false;
                    break;
                }
            }
            match int_effect(ints, &module.consts, &code[q]) {
                Some(e) => {
                    // Binaries consume two window values.
                    if e < 0 && w < 2 {
                        ok = false;
                        break;
                    }
                    w += e;
                    if w < 0 {
                        ok = false;
                        break;
                    }
                }
                None => {
                    ok = false;
                    break;
                }
            }
            q += 1;
        }
        if !ok {
            pc += 1;
            continue;
        }
        // Base: the array load (prefix length P = w ∈ {0, 1}).
        let (arr, prefix_len, p) = match code.get(q) {
            Some(Op::LoadSlot(a)) if array_elem(module, f, *a).is_some() && (w == 0 || w == 1) => {
                (*a, q - pc, w)
            }
            _ => {
                pc += 1;
                continue;
            }
        };
        let Some(elem_bool) = array_elem(module, f, arr) else {
            pc += 1;
            continue;
        };
        // Index expr: P+1 → P+2, floor P+1.
        let mut iq = q + 1;
        let mut iw = p + 1;
        let mut iok = true;
        while iw < p + 2 {
            if iq >= code.len() || iq - (q + 1) >= 8 {
                iok = false;
                break;
            }
            match int_effect(ints, &module.consts, &code[iq]) {
                Some(e) => {
                    if e < 0 && iw < p + 3 {
                        iok = false;
                        break;
                    }
                    iw += e;
                    if iw < p + 1 {
                        iok = false;
                        break;
                    }
                }
                None => {
                    iok = false;
                    break;
                }
            }
            iq += 1;
        }
        // Nonempty idxexpr (iw > P+1 guarantees at least one op ran).
        if !iok || iw != p + 2 {
            pc += 1;
            continue;
        }
        if !matches!(code.get(iq), Some(Op::IndexOp)) {
            pc += 1;
            continue;
        }
        let index_pc = iq;
        // Tail: bool branch (bool arrays, empty prefix and empty REST)
        // or int REST + store (int arrays). REST starts with P+1 window
        // values (`[X?, elem]`) and must end at exactly 1.
        let (tail, end) = if !elem_bool {
            let mut r = index_pc + 1;
            let mut rw = p + 1;
            let mut found = None;
            while r < code.len() && r - (index_pc + 1) < 8 {
                // The only store allowed is the terminating one, at
                // depth exactly 1 (it pops the computed value and
                // nothing below it).
                if let Some(Op::StoreSlot(d)) = code.get(r) {
                    if rw == 1 && ints.contains(d) {
                        found = Some((IdxTail::Store(*d), r));
                    }
                    break;
                }
                match int_effect(ints, &module.consts, &code[r]) {
                    Some(e) => {
                        if e < 0 && rw < 2 {
                            break;
                        }
                        rw += e;
                        if rw < 1 {
                            break;
                        }
                    }
                    None => break,
                }
                r += 1;
            }
            // An empty REST (bare element store `d = a[i]`) qualifies:
            // the loop meets the store immediately at depth 1.
            match found {
                Some(t) => t,
                None => {
                    pc += 1;
                    continue;
                }
            }
        } else {
            // A prefix value would leak (the branch pops only the
            // element), so bool branches require P == 0.
            if p != 0 {
                pc += 1;
                continue;
            }
            match code.get(index_pc + 1) {
                Some(Op::JumpIfFalseBool(t)) => (IdxTail::Branch(*t, false), index_pc + 1),
                Some(Op::JumpIfTrue(t)) => (IdxTail::Branch(*t, true), index_pc + 1),
                _ => {
                    pc += 1;
                    continue;
                }
            }
        };
        // Interior label-free and unclaimed; total window bounded
        // (8 prefix + base + 8 index + read + 8 rest + terminator).
        if end - pc > 28 {
            pc += 1;
            continue;
        }
        if (pc + 1..=end).any(|x| targets.contains(&(x as u32)) || taken.contains(&x)) {
            pc += 1;
            continue;
        }
        // The array home must not be rebound inside the window (keeps
        // the borrow trivially sound — the buffer cannot be swapped
        // mid-window; reads never mutate anyway).
        if code[pc..=end]
            .iter()
            .any(|o| matches!(o, Op::StoreSlot(s) if *s == arr))
        {
            pc += 1;
            continue;
        }
        let rd = IdxRead {
            arr,
            prefix_len,
            idx_len: index_pc - (pc + prefix_len + 1),
            // Ops between IndexOp and the terminator (0 for branches).
            rest_len: match tail {
                IdxTail::Store(_) => end - (index_pc + 1),
                IdxTail::Branch(_, _) => 0,
            },
            tail,
            elem_bool,
        };
        fused.insert(pc, rd);
        for x in pc + 1..=end {
            skip.insert(x);
        }
        pc = end + 1;
    }
    (fused, skip)
}
/// Condition fusion: `[int-operand, int-operand, BinOp(cmp),
/// WhileCond|JumpIfFalse|JumpIfTrue]` compiles to guarded raw compare +
/// branch — no clones, no `zz_binop` dispatch, no bool box/unwrap (the
/// dominant cost in `while`-driven loops: sieve's inner loop measured
/// 4x behind HIR here on `while j < n` alone).
///
/// Window shape (all adjacent):
/// - two int operands: `LoadSlot(s)` with `s` int-typed per `ints`, or
///   `PushConst` of an int. Arithmetic-nested conditions stay boxed;
///   loop conditions are overwhelmingly slot-vs-slot and slot-vs-const.
/// - `BinOp` limited to trapping-free total-order integer comparisons
///   (`== != < > <= >=`); short-circuiting, float, and div/rem never
///   match.
/// - terminator `WhileCond(exit)`, `JumpIfFalse(t)`, or `JumpIfTrue(t)`.
/// - No jump target in the window interior (the start may be targeted —
///   the label still emits, then our statement), no overlap with
///   already-claimed pcs (every existing fusion wins ties).
///
/// Guards mirror [`int_fuse`]: each slot load is int-guarded (fail-closed
/// cold trap); consts render direct. Emission never touches `sp` — no
/// values are pushed, so the `WhileCond` pop is already accounted for
/// and the exit protocol is verbatim. Fall-through pushes nothing.
///
/// Returns `(fused, skip)`: `fused[start]` describes the window; `skip`
/// holds the other three pcs.
#[derive(Debug, Clone, Copy)]
enum CondTerm {
    While(u32),
    JumpFalse(u32),
    JumpTrue(u32),
}

#[derive(Debug, Clone, Copy)]
enum CondOperand {
    Slot(u16),
    Const(i64),
}

#[derive(Debug, Clone, Copy)]
struct CondFuse {
    a: CondOperand,
    b: CondOperand,
    cmp: zz_ir::op::BinOp,
    term: CondTerm,
}

fn condfuse_scan(
    code: &[Op],
    consts: &[Const],
    ints: &HashSet<u16>,
    targets: &HashSet<u32>,
    taken: &HashSet<usize>,
) -> (HashMap<usize, CondFuse>, HashSet<usize>) {
    use zz_ir::op::BinOp as B;
    fn operand(ints: &HashSet<u16>, consts: &[Const], op: &Op) -> Option<CondOperand> {
        match op {
            Op::LoadSlot(s) if ints.contains(s) => Some(CondOperand::Slot(*s)),
            Op::PushConst(c) => match consts.get(c.0 as usize) {
                Some(Const::Int(k)) => Some(CondOperand::Const(*k)),
                _ => None,
            },
            _ => None,
        }
    }
    let mut fused = HashMap::new();
    let mut skip = HashSet::new();
    if code.len() < 4 {
        return (fused, skip);
    }
    let mut pc = 0usize;
    while pc + 3 < code.len() {
        if taken.contains(&pc) || skip.contains(&pc) {
            pc += 1;
            continue;
        }
        // Interior label-free and unclaimed (the start itself may be
        // targeted — the label emits at `pc`, then our statement).
        if (pc + 1..=pc + 3).any(|x| targets.contains(&(x as u32)) || taken.contains(&x)) {
            pc += 1;
            continue;
        }
        let (Some(a), Some(b)) = (
            operand(ints, consts, &code[pc]),
            operand(ints, consts, &code[pc + 1]),
        ) else {
            pc += 1;
            continue;
        };
        let cmp = match &code[pc + 2] {
            Op::BinOp(c @ (B::Eq | B::Ne | B::Lt | B::Gt | B::Le | B::Ge)) => *c,
            _ => {
                pc += 1;
                continue;
            }
        };
        let term = match &code[pc + 3] {
            Op::WhileCond { exit } => CondTerm::While(*exit),
            Op::JumpIfFalse(t) => CondTerm::JumpFalse(*t),
            Op::JumpIfTrue(t) => CondTerm::JumpTrue(*t),
            _ => {
                pc += 1;
                continue;
            }
        };
        fused.insert(pc, CondFuse { a, b, cmp, term });
        for x in pc + 1..=pc + 3 {
            skip.insert(x);
        }
        pc += 4;
    }
    (fused, skip)
}
/// Coverage verdict for one module: every op supported, no default args
/// (arity fill needs caller-env evaluation), and every `Call`/`CallPath`
/// statically resolvable. Fails with the first gap found.
/// Names bound by one pattern (`match` arms, `if let`), resolved to
/// printable names through the module's string pool.
fn pattern_binds(module: &Module, pat: &Pattern, out: &mut Vec<String>) {
    match pat {
        Pattern::Binding(id) => {
            if let Some(name) = module.strings.get(id.0 as usize) {
                out.push(name.clone());
            }
        }
        Pattern::Variant {
            arg: Some(inner), ..
        } => pattern_binds(module, inner, out),
        Pattern::Tuple(pats) | Pattern::Or(pats) => {
            for p in pats {
                pattern_binds(module, p, out);
            }
        }
        Pattern::Wildcard | Pattern::Lit(_) | Pattern::Variant { arg: None, .. } => {}
    }
}

/// Function-local env cells: names the VM keeps in the frame environment
/// — captured function-local decls, `match` arm bindings, and the
/// compiler's `__match_scrutinee_*` reload temps. Each becomes one
/// `zz_value` C local instead of an env lookup.
///
/// The VM binds `match` arm names in a *child* environment, so a nested
/// scope that reuses a name shadows the enclosing binding instead of
/// clobbering it. Cells are therefore named per nesting depth: the
/// function frame keeps `zz_e_<name>`, a scope at depth `d` uses
/// `zz_e<d>_<name>`. Sibling scopes at the same depth never overlap in
/// time (only one arm of a `match` runs, and a block is entered once per
/// execution), so they may share a cell.
struct EnvLayout {
    /// The function frame: captured function-locals and the compiler's
    /// `__match_scrutinee_*` reload temps. Module globals never appear
    /// here — `DefineVar` for a global stays the global.
    base: HashMap<String, String>,
    /// `MatchArm` / `IfLetMatch` pc -> the cells its pattern binds, innermost
    /// scope. The emitter pushes this frame for the arm body and the arm's
    /// trailing `ExitScope` pops it.
    arm_scopes: HashMap<usize, Vec<(String, String)>>,
    /// Every cell C local to declare in the prologue.
    decls: Vec<String>,
    /// Every IR name that has a cell at any depth (coverage checks).
    names: Vec<String>,
}

/// C local for the env cell of `name` in the scope at `depth`.
fn cell_id(depth: usize, name: &str) -> String {
    if depth == 0 {
        format!("zz_e_{}", mangle(name))
    } else {
        format!("zz_e{depth}_{}", mangle(name))
    }
}

/// Allocate (or find) the cell for `name` in the innermost scope.
fn bind_cell(
    frames: &mut [HashMap<String, String>],
    decls: &mut Vec<String>,
    names: &mut Vec<String>,
    name: &str,
) -> String {
    let depth = frames.len() - 1;
    let frame = &mut frames[depth];
    if let Some(id) = frame.get(name) {
        return id.clone();
    }
    let id = cell_id(depth, name);
    frame.insert(name.to_string(), id.clone());
    if !decls.iter().any(|d| d == &id) {
        decls.push(id.clone());
    }
    if !names.iter().any(|n| n == name) {
        names.push(name.to_string());
    }
    id
}

/// Walk one function and decide every env cell, mirroring the emitter's
/// scope discipline (`EnterScope` opens a frame, a binding arm opens one,
/// `ExitScope` closes either). A scope that does not balance is rejected:
/// a half-tracked frame would resolve a name to the wrong cell, so this
/// fails loudly instead.
fn env_layout(
    module: &Module,
    f: &FuncDef,
    is_global: &dyn Fn(&str) -> bool,
) -> Result<EnvLayout, ChunkError> {
    let mut frames: Vec<HashMap<String, String>> = vec![HashMap::new()];
    let mut arm_scopes: HashMap<usize, Vec<(String, String)>> = HashMap::new();
    let mut decls: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (pc, op) in f.code.iter().enumerate() {
        match op {
            Op::DefineVar(id) | Op::StoreVar(id) => {
                let Some(name) = module.strings.get(id.0 as usize) else {
                    continue;
                };
                if is_global(name) {
                    continue;
                }
                let name = name.clone();
                bind_cell(&mut frames, &mut decls, &mut names, &name);
            }
            Op::EnterScope => frames.push(HashMap::new()),
            Op::ExitScope => {
                if frames.len() == 1 {
                    return Err(ChunkError::op("unbalanced env scope"));
                }
                frames.pop();
            }
            Op::MatchArm { pat, .. } | Op::IfLetMatch { pat, .. } => {
                // Arm bindings shadow module globals (the VM binds them in
                // a child env), so they always get their own cell.
                let mut bound = Vec::new();
                pattern_binds(module, pat, &mut bound);
                if bound.is_empty() {
                    continue;
                }
                frames.push(HashMap::new());
                let mut cells = Vec::new();
                for name in bound {
                    let id = bind_cell(&mut frames, &mut decls, &mut names, &name);
                    cells.push((name, id));
                }
                arm_scopes.insert(pc, cells);
            }
            _ => {}
        }
    }
    if frames.len() != 1 {
        return Err(ChunkError::op("unbalanced env scope"));
    }
    let base = frames.pop().unwrap_or_default();
    Ok(EnvLayout {
        base,
        arm_scopes,
        decls,
        names,
    })
}

/// Reject pattern shapes the chunk emitter has no C form for.
fn pattern_supported(module: &Module, pat: &Pattern) -> bool {
    match pat {
        Pattern::Wildcard | Pattern::Binding(_) | Pattern::Lit(_) => true,
        Pattern::Variant { arg, .. } => arg
            .as_deref()
            .map(|p| pattern_supported(module, p))
            .unwrap_or(true),
        Pattern::Tuple(pats) => pats.iter().all(|p| pattern_supported(module, p)),
        Pattern::Or(pats) => {
            // Alternatives join into one C condition, so only binding-free
            // alternatives are covered (the VM absorbs a branch's
            // bindings; the chunk backend has no per-branch block).
            pats.iter().all(|p| {
                if !pattern_supported(module, p) {
                    return false;
                }
                let mut names = Vec::new();
                pattern_binds(module, p, &mut names);
                names.is_empty()
            })
        }
    }
}

/// Module-scope bindings the VM keeps in the environment: every name the
/// top-level function binds with `DefineVar`. The chunk backend has no
/// run-time environment (and closures are not covered here yet), so these
/// lower to file-scope C globals that every compiled function can read.
fn module_globals(module: &Module) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(top) = module.funcs.get(module.entry.0 as usize) {
        for op in &top.code {
            if let Op::DefineVar(id) = op {
                if let Some(name) = module.strings.get(id.0 as usize) {
                    out.insert(name.clone());
                }
            }
        }
    }
    out
}

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
    let globals = module_globals(module);
    for f in &module.funcs {
        let env = env_layout(module, f, &|n| globals.contains(n))?;
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
            if let Op::MatchArm { pat, .. } | Op::IfLetMatch { pat, .. } = op {
                if !pattern_supported(module, pat) {
                    return Err(ChunkError::op("or-pattern that binds"));
                }
            }
            if let Op::DefineVar(id) | Op::StoreVar(id) = op {
                let var = module
                    .strings
                    .get(id.0 as usize)
                    .map(String::as_str)
                    .unwrap_or("");
                if !globals.contains(var) && !env.names.iter().any(|n| n == var) {
                    let name = module
                        .strings
                        .get(f.name.0 as usize)
                        .map(String::as_str)
                        .unwrap_or("?");
                    return Err(ChunkError::op(&format!(
                        "runtime binding `{var}` in {name}"
                    )));
                }
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
                    return Err(ChunkError::op(&format!("call stack underflow at {pc}")));
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
                let var = module
                    .strings
                    .get(id.0 as usize)
                    .map(String::as_str)
                    .unwrap_or("");
                if globals.contains(var) || env.names.iter().any(|n| n == var) {
                    continue;
                }
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
/// Emit a pattern's arm-cell bindings (take path only).
fn emit_pat_binds(out: &mut String, binds: &[String]) {
    for b in binds {
        out.push_str("    ");
        out.push_str(b);
        out.push('\n');
    }
}

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
    /// Module-scope env bindings (`module_globals`): IR name -> C global id.
    globals: HashMap<String, String>,
    /// Function-local env scopes of the function being emitted, outermost
    /// first (index 0 is the function frame): IR name -> C local id. A
    /// binding arm pushes a frame, so a nested arm that reuses a name
    /// resolves to its own cell and leaves the enclosing binding intact.
    env_scopes: Vec<HashMap<String, String>>,
    /// `MatchArm` / `IfLetMatch` pc -> the cells its pattern binds (see
    /// [`env_layout`]).
    arm_scopes: HashMap<usize, Vec<(String, String)>>,
    /// Qualified unit enum variants (`Token.End`): a path naming one
    /// constructs the value (`RegisterEnum` supplies the set).
    unit_enums: HashSet<String>,
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

    /// C local of the innermost env cell for `name`, if this function has
    /// one (arm scopes shadow the function frame).
    fn env_cell(&self, name: &str) -> Option<&String> {
        self.env_scopes.iter().rev().find_map(|f| f.get(name))
    }

    /// Whether `name` can be a call target (mirrors the coverage-side
    /// `resolve_load` order), i.e. a `LoadVar` of it is a callee
    /// placeholder rather than a value.
    fn is_callee_name(&self, name: &str) -> bool {
        self.csyms.contains_key(name)
            || super::lower::native_impl(name).is_some()
            || super::ffi_impl(name).is_some()
            || super::lower::native_impl(&format!("std.{name}")).is_some()
            || super::ffi_impl(&format!("std.{name}")).is_some()
    }

    /// Dotted spelling of a path (`"g2"."count"` -> `g2.count`), which is
    /// the key module-scope bindings are recorded under.
    fn join_path(&self, parts: &[StrId]) -> String {
        parts
            .iter()
            .map(|p| self.str(*p))
            .collect::<Vec<_>>()
            .join(".")
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

/// Cold-trap call for fused/peel guards: identical stderr bytes + exit 1
/// as [`ctrap`], but the abort block lives out of line (`zz_ftrap` in the
/// preamble) so hot loops stay tight for icache and unrolling.
fn ftrap(msg: &str) -> String {
    format!("zz_ftrap(\"{msg}\")")
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
    /// C condition plus binding statements for one `match` pattern.
    ///
    /// `place` is a C expression of type `zz_value` that is safe to read
    /// whenever the condition held so far (the scrutinee slot, or a
    /// dereference of a payload the condition already proved present).
    /// Returns the condition and the statements that bind arm cells; the
    /// bindings are emitted only on the take path, and they clone through
    /// [`zz_cassign`], so a miss touches nothing.
    fn pat_block(
        &mut self,
        pat: &Pattern,
        place: &str,
    ) -> Result<(String, Vec<String>), ChunkError> {
        let mut conds = Vec::new();
        let mut binds = Vec::new();
        self.pat_code(pat, place, &mut conds, &mut binds)?;
        let cond = if conds.is_empty() {
            "1".to_string()
        } else {
            conds.join(" && ")
        };
        Ok((cond, binds))
    }

    /// C expression for a pattern's literal constant.
    fn pat_lit(&mut self, id: ConstId) -> Result<String, ChunkError> {
        let c = self
            .module
            .consts
            .get(id.0 as usize)
            .cloned()
            .ok_or_else(|| ChunkError::op("pattern const id"))?;
        Ok(match c {
            Const::Int(i) => format!("zz_int({})", c_int(i)),
            Const::Float(f) => {
                self.needs_float_fmt = true;
                format!("zz_float({f:?})")
            }
            Const::Bool(b) => format!("zz_bool({})", if b { "true" } else { "false" }),
            // Interned: immortal, so the comparison operand needs no
            // release (the same value the VM's literal holds).
            Const::Str(s) => format!("zz_str_static({})", c_str(self.str(s))),
            _ => return Err(ChunkError::op("non-literal pattern constant")),
        })
    }

    /// Emit the C test (and the arm-cell bindings) for one pattern.
    /// Mirrors the VM's `match_pattern`:
    ///
    /// - `some`/`ok`/`err` test the tag; a payload sub-pattern also needs
    ///   a payload slot, then reads through it.
    /// - `none` is a tag-only test (a payload form can never match).
    /// - any other variant name is a user enum: the qualified object type
    ///   ends in `.<name>` (cross-enum confusion is impossible — the
    ///   checker pins the scrutinee's type, same as the VM).
    /// - tuples erase to arrays: tag + length, then element-wise.
    /// - `|` joins alternatives; every deref sits behind its own guard in
    ///   the same `&&` chain, so a miss never reads a non-matching value.
    ///   Binding alternatives are rejected by [`pattern_supported`].
    fn pat_code(
        &mut self,
        pat: &Pattern,
        place: &str,
        conds: &mut Vec<String>,
        binds: &mut Vec<String>,
    ) -> Result<(), ChunkError> {
        match pat {
            Pattern::Wildcard => {}
            Pattern::Binding(id) => {
                let name = self.str(*id).to_string();
                let cell = self.env_cell(&name).cloned().ok_or_else(|| {
                    ChunkError::op(&format!("binding `{name}` outside an env cell"))
                })?;
                binds.push(format!("zz_cassign(&{cell}, {place});"));
            }
            Pattern::Lit(cid) => {
                let lit = self.pat_lit(*cid)?;
                conds.push(format!("zz_truthy(zz_binop(ZZOP_EQ, {place}, {lit}))"));
            }
            Pattern::Variant { name, arg } => {
                let short = self.str(*name).to_string();
                match short.as_str() {
                    "none" => {
                        if arg.is_some() {
                            // `.none(x)` can never match (VM: `arg.is_none()`).
                            conds.push("0".to_string());
                        } else {
                            conds.push(format!("({place}).tag == ZZ_OPTION_NONE"));
                        }
                    }
                    "some" | "ok" | "err" => {
                        let tag = match short.as_str() {
                            "some" => "ZZ_OPTION_SOME",
                            "ok" => "ZZ_RESULT_OK",
                            _ => "ZZ_RESULT_ERR",
                        };
                        conds.push(format!("({place}).tag == {tag}"));
                        if let Some(inner) = arg {
                            // A payload pattern needs a payload: the VM
                            // fails the arm when the variant has none.
                            conds.push(format!("({place}).payload != NULL"));
                            let inner_place = format!("(*({place}).payload)");
                            self.pat_code(inner, &inner_place, conds, binds)?;
                        }
                    }
                    _ => {
                        conds.push(format!("zz_cenum_suffix(&({place}), {})", c_str(&short)));
                        if let Some(inner) = arg {
                            conds.push(format!("zz_cfield(&({place}), \"value\") != NULL"));
                            let inner_place = format!("(*zz_cfield(&({place}), \"value\"))");
                            self.pat_code(inner, &inner_place, conds, binds)?;
                        }
                    }
                }
            }
            Pattern::Tuple(pats) => {
                conds.push(format!(
                    "({place}).tag == ZZ_ARRAY && ({place}).arr != NULL && ({place}).arr->len == {}",
                    pats.len()
                ));
                for (i, el) in pats.iter().enumerate() {
                    conds.push(format!("zz_carr_at(&({place}), {i}) != NULL"));
                    let el_place = format!("(*zz_carr_at(&({place}), {i}))");
                    self.pat_code(el, &el_place, conds, binds)?;
                }
            }
            Pattern::Or(pats) => {
                // The verifier admits an empty alternative list; no
                // alternative can match, exactly like the VM's loop.
                if pats.is_empty() {
                    conds.push("0".to_string());
                    return Ok(());
                }
                let mut alts = Vec::new();
                for alt in pats {
                    let mut c = Vec::new();
                    let mut st = Vec::new();
                    self.pat_code(alt, place, &mut c, &mut st)?;
                    if !st.is_empty() {
                        return Err(ChunkError::op("or-pattern that binds"));
                    }
                    alts.push(if c.is_empty() {
                        "1".to_string()
                    } else {
                        c.join(" && ")
                    });
                }
                conds.push(format!("({})", alts.join(" || ")));
            }
        }
        Ok(())
    }

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
                    out.push_str(&format!("    zz_value *_kp{tmp} = zz_vbox_new({e});\n"));
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
                out.push_str(&format!("    zz_value *_kp{tmp} = zz_vbox_new({e});\n"));
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
        // Function-local env cells (see `env_layout`).
        let env = env_layout(self.module, f, &|n| self.globals.contains_key(n))?;
        self.env_scopes = vec![env.base];
        self.arm_scopes = env.arm_scopes;
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
        for id in &env.decls {
            out.push_str(&format!("    zz_value {id} = zz_unit();\n"));
        }
        // Int-declared slots eligible for fused windows (see `int_slots`).
        let uint = int_slots(self.module, f);
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
            for t in jump_targets(op) {
                targets.insert(t);
            }
        }
        let origins_empty: Vec<Vec<Callee>> = Vec::new();
        let org = origins_map.get(&(id.0 as usize)).unwrap_or(&origins_empty);
        // Store-index fusion (in-place slot stores; see `fuse_scan`).
        // `borrow` (skipped `LoadSlot`s) joins the skip set up front so
        // every later scan treats borrowed loads as claimed.
        let (fused, skip, borrows) = fuse_scan(&f.code);
        debug_assert!(
            fused.keys().all(|k| !skip.contains(k)),
            "fuse store in skip set"
        );
        let (ufused, uskip) = int_fuse(f, &self.module.consts, &uint, &targets, &skip);
        let mut skip = skip;
        skip.extend(uskip);
        // Fused windows emit at their start pc: a start must never be
        // skipped (that would drop the ops silently — see arraysum).
        debug_assert!(
            ufused.keys().all(|k| !skip.contains(k)),
            "fused window start in skip set"
        );
        // Loop peels (see `peel_scan`): claim `[fornext..exit]`.
        let (peels, pskip) = peel_scan(f, self.module, &uint, &targets, &ufused);
        skip.extend(pskip);
        debug_assert!(
            peels.keys().all(|k| !skip.contains(k)),
            "peel start in skip set"
        );
        // String-append fusion (see `strapp_scan`): strict 4-op windows
        // lose every tie — anything already claimed stays claimed.
        let (strapped, sskip) = strapp_scan(&f.code, &self.module.consts, &targets, &skip);
        skip.extend(sskip);
        debug_assert!(
            strapped.keys().all(|k| !skip.contains(k)),
            "strapp start in skip set"
        );
        // Index-read fusion (see `idxread_scan`): same tie discipline.
        let (idxreads, iskip) = idxread_scan(f, self.module, &uint, &targets, &skip);
        skip.extend(iskip);
        debug_assert!(
            idxreads.keys().all(|k| !skip.contains(k)),
            "idxread start in skip set"
        );
        // Condition fusion (see `condfuse_scan`): same tie discipline —
        // every earlier fusion wins, so overlapping windows stay boxed.
        let (condfused, cskip) =
            condfuse_scan(&f.code, &self.module.consts, &uint, &targets, &skip);
        skip.extend(cskip);
        debug_assert!(
            condfused.keys().all(|k| !skip.contains(k)),
            "condfuse start in skip set"
        );
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
                fuse_borrow: borrows.contains(&pc),
                ufuse: ufused.get(&pc).copied(),
                peel: peels.get(&pc).cloned(),
                strapp: strapped.get(&pc).copied(),
                idxread: idxreads.get(&pc).copied(),
                condfuse: condfused.get(&pc).copied(),
            };
            self.emit_op(id, pc, op, &cx, out)?;
        }
        // A jump may target one-past-the-end (fall off into the
        // implicit return, like the VM's ip-past-end).
        if targets.contains(&(f.code.len() as u32)) {
            out.push_str(&format!("L{}:;\n", f.code.len()));
        }
        // Implicit return of the top value (function bodies fall off).
        // Release only the live prefix below it: positions at/above sp
        // are dead (stale aliases of already-released values — the VM
        // truncates them away, but the C frame keeps the bits). A
        // whole-frame sweep double-releases those aliases (UAF when the
        // object was freed since, silent count corruption otherwise).
        out.push_str(
            "    {{ zz_value _r = st[--sp]; for (int _i = 0; _i < sp; _i++) zz_release(&st[_i]); return _r; }}\n",
        );
        out.push_str("}\n");
        Ok(())
    }
}

impl<'a> Emitter<'a> {
    /// Emit one fused index-read window (see [`IdxRead`]): guarded direct
    /// element access with no clone, no runtime call, and no stack
    /// traffic. The window is self-contained by matching (net-zero stack
    /// effect, all pops matched by pushes inside), so emission never
    /// touches `sp`. Guards fail closed to cold out-of-line traps:
    /// - base must be an array (checker-typed programs always hold the
    ///   declared `[int]`/`[bool]` here);
    /// - every slot load is int-guarded (fail-closed like [`int_fuse`]);
    /// - bounds mirror `zz_array_get` exactly (single `i += n`
    ///   normalize, then the OOB check);
    /// - the element must match the declared kind.
    ///
    /// Store tails write back through release-then-move (sound for any
    /// old content); branch tails jump exactly like the boxed
    /// `JumpIfFalseBool`/`JumpIfTrue`.
    fn emit_idxread(
        module: &Module,
        code: &[Op],
        pc: usize,
        rd: &IdxRead,
        out: &mut String,
    ) -> Result<(), ChunkError> {
        /// Emit one int-expression op into window temps (see
        /// [`int_fuse`]): loads are int-guarded, consts direct,
        /// trapping-free `+ - *` compile to raw wrapping arithmetic
        /// (`-fwrapv`, like the boxed fast paths). Returns the temp
        /// holding the value.
        fn emit_int_op(
            module: &Module,
            op: &Op,
            tstack: &mut Vec<String>,
            tmp: &mut usize,
            out: &mut String,
        ) -> Result<(), ChunkError> {
            let t = format!("_qd{tmp}");
            *tmp += 1;
            match op {
                Op::LoadSlot(s) => {
                    out.push_str(&format!(
                        "      zz_value _b{t} = st[{s}]; if (_b{t}.tag != ZZ_INT) {};\n",
                        ftrap("int slot loaded non-int value")
                    ));
                    out.push_str(&format!("      int64_t {t} = _b{t}.i;\n"));
                    tstack.push(t);
                }
                Op::PushConst(c) => {
                    let k = match module.consts.get(c.0 as usize) {
                        Some(Const::Int(k)) => *k,
                        // Unreachable: the matcher only admits int consts.
                        _ => return Err(ChunkError::op("idxread const")),
                    };
                    out.push_str(&format!("      int64_t {t} = {};\n", c_int(k)));
                    tstack.push(t);
                }
                Op::IntAdd | Op::BinOp(zz_ir::op::BinOp::Add) => {
                    let b = tstack
                        .pop()
                        .ok_or_else(|| ChunkError::op("idxread stack"))?;
                    let a = tstack
                        .pop()
                        .ok_or_else(|| ChunkError::op("idxread stack"))?;
                    out.push_str(&format!("      int64_t {t} = {a} + {b};\n"));
                    tstack.push(t);
                }
                Op::IntSub | Op::BinOp(zz_ir::op::BinOp::Sub) => {
                    let b = tstack
                        .pop()
                        .ok_or_else(|| ChunkError::op("idxread stack"))?;
                    let a = tstack
                        .pop()
                        .ok_or_else(|| ChunkError::op("idxread stack"))?;
                    out.push_str(&format!("      int64_t {t} = {a} - {b};\n"));
                    tstack.push(t);
                }
                Op::IntMul | Op::BinOp(zz_ir::op::BinOp::Mul) => {
                    let b = tstack
                        .pop()
                        .ok_or_else(|| ChunkError::op("idxread stack"))?;
                    let a = tstack
                        .pop()
                        .ok_or_else(|| ChunkError::op("idxread stack"))?;
                    out.push_str(&format!("      int64_t {t} = {a} * {b};\n"));
                    tstack.push(t);
                }
                _ => return Err(ChunkError::op("idxread expr")),
            }
            Ok(())
        }
        let base_pc = pc + rd.prefix_len;
        let index_pc = base_pc + 1 + rd.idx_len;
        let rest_pc = index_pc + 1;
        out.push_str("    {\n");
        // Base borrow (read-only: no calls or stores in the window, so
        // the buffer cannot move under us — the same guarantee the
        // fused store-index object relies on in [`fuse_scan`]).
        out.push_str(&format!(
            "      zz_value _qb = st[{arr}]; if (_qb.tag != ZZ_ARRAY) {};\n",
            ftrap("cannot index this value"),
            arr = rd.arr,
        ));
        // Prefix expression (the other operand, evaluated first —
        // source order) then the index expression, into temps.
        let mut tstack: Vec<String> = Vec::new();
        let mut tmp = 0usize;
        for op in &code[pc..base_pc] {
            emit_int_op(module, op, &mut tstack, &mut tmp, out)?;
        }
        for op in &code[base_pc + 1..index_pc] {
            emit_int_op(module, op, &mut tstack, &mut tmp, out)?;
        }
        let top = tstack
            .pop()
            .ok_or_else(|| ChunkError::op("idxread stack"))?;
        // Bounds exactly like `zz_array_get`: normalize once, then check.
        out.push_str("      int64_t _qn = (int64_t)_qb.arr->len;\n");
        out.push_str(&format!(
            "      int64_t _qx = {top}; if (_qx < 0) _qx += _qn; if (_qx < 0 || _qx >= _qn) {};\n",
            ftrap("index out of bounds")
        ));
        if rd.elem_bool {
            out.push_str("      zz_value _qe = _qb.arr->items[_qx]; if (_qe.tag != ZZ_BOOL) ");
            out.push_str(&format!("{};\n", ftrap("array element is not a bool")));
            match rd.tail {
                IdxTail::Branch(t, false) => {
                    out.push_str(&format!("      if (!_qe.b) goto L{t};\n"));
                }
                IdxTail::Branch(t, true) => {
                    out.push_str(&format!("      if (_qe.b) goto L{t};\n"));
                }
                IdxTail::Store(_) => return Err(ChunkError::op("idxread tail")),
            }
        } else {
            out.push_str("      zz_value _qe = _qb.arr->items[_qx]; if (_qe.tag != ZZ_INT) ");
            out.push_str(&format!("{};\n", ftrap("array element is not an int")));
            tstack.push("_qe.i".to_string());
            for op in &code[rest_pc..rest_pc + rd.rest_len] {
                emit_int_op(module, op, &mut tstack, &mut tmp, out)?;
            }
            let fin = tstack
                .pop()
                .ok_or_else(|| ChunkError::op("idxread stack"))?;
            match rd.tail {
                IdxTail::Store(d) => {
                    out.push_str(&format!("      zz_release(&st[{d}]);\n"));
                    out.push_str(&format!("      st[{d}] = zz_int({fin});\n"));
                }
                IdxTail::Branch(_, _) => return Err(ChunkError::op("idxread tail")),
            }
        }
        out.push_str("    }\n");
        Ok(())
    }

    /// Emit one fused int-compare branch (see [`CondFuse`]): guarded raw
    /// `int64_t` loads plus the comparison and the terminator's exact
    /// protocol. No values are pushed, so `sp` is untouched: the
    /// `WhileCond` pop is already accounted for and the exit edge
    /// (`_ldepth--; sp = _lbase[...] + 1; goto`) is verbatim. Guards are
    /// fail-closed cold traps, like [`int_fuse`].
    fn emit_condfuse(cf: &CondFuse, out: &mut String) -> Result<(), ChunkError> {
        use zz_ir::op::BinOp as B;
        fn operand(o: &CondOperand, name: &str, out: &mut String) {
            match o {
                CondOperand::Slot(s) => {
                    out.push_str(&format!(
                        "      int64_t {name} = 0; {{ zz_value _b{name} = st[{s}]; if (_b{name}.tag != ZZ_INT) {}; {name} = _b{name}.i; }}\n",
                        ftrap("int slot loaded non-int value")
                    ));
                }
                CondOperand::Const(k) => {
                    out.push_str(&format!("      int64_t {name} = {};\n", c_int(*k)));
                }
            }
        }
        let cmp = match cf.cmp {
            B::Eq => "==",
            B::Ne => "!=",
            B::Lt => "<",
            B::Gt => ">",
            B::Le => "<=",
            B::Ge => ">=",
            _ => return Err(ChunkError::op("condfuse compare")),
        };
        out.push_str("    {\n");
        operand(&cf.a, "_cca", out);
        operand(&cf.b, "_ccb", out);
        out.push_str(&format!("      int _cct = (_cca {cmp} _ccb);\n"));
        match cf.term {
            CondTerm::While(exit) => {
                out.push_str(&format!(
                    "      if (!_cct) {{ _ldepth--; sp = _lbase[_ldepth] + 1; goto L{exit}; }} }}\n"
                ));
            }
            CondTerm::JumpFalse(t) => {
                out.push_str(&format!("      if (!_cct) goto L{t}; }}\n"));
            }
            CondTerm::JumpTrue(t) => {
                out.push_str(&format!("      if (_cct) goto L{t}; }}\n"));
            }
        }
        Ok(())
    }
    /// Emit one peeled counted loop (see [`Peel`]): pop the `ForSetup`
    /// placeholders, hoist promoted int slots to C locals (entry-guarded
    /// once), run a raw C loop over the fused body, write promotions back,
    /// then take the exit edge exactly as the boxed exhaustion path would
    /// (releases, `_ldepth--`, `sp` already at the exit position, `goto
    /// exit`). Body var/slot loads substitute their C locals directly (no
    /// guard, no box): proven-int by entry guard or construction.
    fn emit_peeled(module: &Module, code: &[Op], peel: &Peel, out: &mut String) {
        let v = peel.var;
        out.push_str("    {\n");
        // Pop the placeholders `ForSetup` pushed (iterable, index, var):
        // `sp` returns to the setup level, which IS the exit position
        // (`_lbase+1`), so no padding is ever needed.
        out.push_str("      zz_value _pit = st[sp-3];\n");
        out.push_str("      zz_release(&st[sp-2]);\n");
        out.push_str("      zz_release(&st[sp-1]);\n");
        out.push_str("      sp -= 3;\n");
        // Promotions: entry-guard loaded slots once; declare store-only
        // slots (defined by their first body store).
        let mut seen = HashSet::new();
        for s in &peel.promo_loads {
            if seen.insert(*s) {
                out.push_str(&format!(
                    "      zz_value _bp{s} = st[{s}]; if (_bp{s}.tag != ZZ_INT) {}; int64_t _ps{s} = _bp{s}.i;\n",
                    ftrap("int slot loaded non-int value")
                ));
            }
        }
        for s in &peel.promo_stores {
            if seen.insert(*s) {
                // Store-only: no entry guard (boxed exactness — the old
                // value is simply overwritten at write-back). Zero-init
                // for determinism; the straight-line body always assigns
                // before any read.
                out.push_str(&format!("      int64_t _ps{s} = 0;\n"));
            }
        }
        // Substitution map for the fused body: the iteration variable
        // (proven-int counter/element) plus every promoted slot. Reads
        // compile to the C local directly — no guard, no box.
        let mut subs: HashMap<u16, String> = HashMap::new();
        for s in peel.promo_loads.iter().chain(peel.promo_stores.iter()) {
            subs.entry(*s).or_insert_with(|| format!("_ps{s}"));
        }
        match peel.source {
            PeelSource::Range => {
                // Proven range: the adjacent `MakeRange` producer is our
                // own emission (range-or-trap, step hardcodes to 1 like
                // the VM and HIR), so no tag check and a plain
                // zero-trip-correct `<` loop (rule 5: no guards in
                // proved code).
                out.push_str("      zz_range *_pr = _pit.range;\n");
                out.push_str("      for (int64_t _pv = _pr->start; _pv < _pr->end; _pv++) {\n");
                subs.insert(v, "_pv".to_string());
                Self::emit_fused_window(
                    module,
                    code,
                    peel.body,
                    peel.body_len,
                    peel.body_tail,
                    &subs,
                    out,
                );
                subs.remove(&v);
                out.push_str("      }\n");
            }
            PeelSource::Array => {
                // Slot-proven array shape, runtime tag gate (the slot may
                // hold a non-array on adversarial modules; valid programs
                // always hold the declared array). Mirrors `ForSetup`'s
                // dispatch refusal message.
                out.push_str("      if (_pit.tag != ZZ_ARRAY) ");
                out.push_str(&format!("{};\n", ftrap("cannot iterate this value")));
                out.push_str("      size_t _pn = _pit.arr->len;\n");
                out.push_str("      for (size_t _pk = 0; _pk < _pn; _pk++) {\n");
                // Element boundary (fail-closed): valid programs hold
                // ints here (checker-vetted); anything else traps
                // instead of misreading bits as an int.
                out.push_str(
                    "        zz_value _pe = _pit.arr->items[_pk]; if (_pe.tag != ZZ_INT) ",
                );
                out.push_str(&format!("{};\n", ftrap("array element is not an int")));
                subs.insert(v, "_pe.i".to_string());
                Self::emit_fused_window(
                    module,
                    code,
                    peel.body,
                    peel.body_len,
                    peel.body_tail,
                    &subs,
                    out,
                );
                subs.remove(&v);
                out.push_str("      }\n");
            }
        }
        // Write promotions back (release-then-move: sound for any old
        // content, exact for ints), then the exit edge.
        for s in &peel.promo_stores {
            out.push_str(&format!("      zz_release(&st[{s}]);\n"));
            out.push_str(&format!("      st[{s}] = zz_int(_ps{s});\n"));
        }
        // Mirror the exhaustion path releases, then the exit edge.
        out.push_str("      zz_release(&_pit);\n");
        out.push_str("      _ldepth--;\n");
        out.push_str(&format!("      goto L{};\n", peel.exit));
        out.push_str("    }\n");
    }

    /// Render one fused unboxed-int window at `pc` (see [`FusedTail`]):
    /// the int expression over `len - taillen` ops compiles to C temps,
    /// then the tail consumes the result temp. No stack traffic, no shadow
    /// state: operands read the authoritative boxed slot (int guard,
    /// fail-closed); the result moves into the slot after releasing its
    /// old value (fresh immediates carry no refs, so no clone/release
    /// pair). `var_sub` substitutes loop-var loads with a proven-int C
    /// expression (peeled counters/elements): no guard, no box.
    /// Division carries the spec traps with the boxed path's messages
    /// (elided when a constant divisor rules them out); wrapping follows
    /// `-fwrapv`, like the boxed fast paths. Safe constant folding applies
    /// to trapping-free add/sub/mul/neg (`-k` via `wrapping_neg`: `MIN`
    /// must not panic the host compiler).
    fn emit_fused_window(
        module: &Module,
        code: &[Op],
        pc: usize,
        len: usize,
        tail: FusedTail,
        subs: &HashMap<u16, String>,
        out: &mut String,
    ) {
        let taillen = match tail {
            FusedTail::Store(_) => 1,
            FusedTail::Take(_) => 6,
        };
        out.push_str("    {\n");
        // Expression prefix: straight-line int ops into window temps.
        // Each temp tracks a known constant value, if any.
        let mut tstack: Vec<(String, Option<i64>)> = Vec::new();
        for (tmp, op) in code[pc..pc + len - taillen].iter().enumerate() {
            let t = format!("_ft{tmp}");
            match op {
                Op::LoadSlot(s) => {
                    if let Some(expr) = subs.get(s) {
                        out.push_str(&format!("      int64_t {t} = {expr};\n"));
                        tstack.push((t, None));
                        continue;
                    }
                    out.push_str(&format!(
                        "      zz_value _b{t} = st[{s}]; if (_b{t}.tag != ZZ_INT) {};\n",
                        ftrap("int slot loaded non-int value")
                    ));
                    out.push_str(&format!("      int64_t {t} = _b{t}.i;\n"));
                    tstack.push((t, None));
                }
                Op::PushConst(c) => {
                    let k = match module.consts.get(c.0 as usize) {
                        Some(Const::Int(k)) => *k,
                        // Unreachable: the matcher only admits int consts.
                        _ => return,
                    };
                    out.push_str(&format!("      int64_t {t} = {};\n", c_int(k)));
                    tstack.push((t, Some(k)));
                }
                Op::IntAdd | Op::BinOp(zz_ir::op::BinOp::Add) => {
                    let (b, bv) = tstack.pop().unwrap_or_default();
                    let (a, av) = tstack.pop().unwrap_or_default();
                    match (av, bv) {
                        (Some(x), Some(y)) => {
                            let r = x.wrapping_add(y);
                            out.push_str(&format!("      int64_t {t} = {};\n", c_int(r)));
                            tstack.push((t, Some(r)));
                        }
                        _ => {
                            out.push_str(&format!("      int64_t {t} = {a} + {b};\n"));
                            tstack.push((t, None));
                        }
                    }
                }
                Op::IntSub | Op::BinOp(zz_ir::op::BinOp::Sub) => {
                    let (b, bv) = tstack.pop().unwrap_or_default();
                    let (a, av) = tstack.pop().unwrap_or_default();
                    match (av, bv) {
                        (Some(x), Some(y)) => {
                            let r = x.wrapping_sub(y);
                            out.push_str(&format!("      int64_t {t} = {};\n", c_int(r)));
                            tstack.push((t, Some(r)));
                        }
                        _ => {
                            out.push_str(&format!("      int64_t {t} = {a} - {b};\n"));
                            tstack.push((t, None));
                        }
                    }
                }
                Op::IntMul | Op::BinOp(zz_ir::op::BinOp::Mul) => {
                    let (b, bv) = tstack.pop().unwrap_or_default();
                    let (a, av) = tstack.pop().unwrap_or_default();
                    match (av, bv) {
                        (Some(x), Some(y)) => {
                            let r = x.wrapping_mul(y);
                            out.push_str(&format!("      int64_t {t} = {};\n", c_int(r)));
                            tstack.push((t, Some(r)));
                        }
                        _ => {
                            out.push_str(&format!("      int64_t {t} = {a} * {b};\n"));
                            tstack.push((t, None));
                        }
                    }
                }
                Op::IntDiv => {
                    let (b, bv) = tstack.pop().unwrap_or_default();
                    let (a, _) = tstack.pop().unwrap_or_default();
                    // Elide provably-dead traps: a nonzero const divisor
                    // can never be zero; anything but -1 can never be
                    // `MIN / -1`. (Dead-code traps keep their runtime
                    // form — folding them away would change semantics.)
                    match bv {
                        // Zero divisor always traps: keep the check.
                        Some(0) => {
                            out.push_str(&format!(
                                "      {{ if ({b} == 0) {}; }}\n",
                                ftrap("division by zero")
                            ));
                        }
                        // -1 can only trap via `MIN`.
                        Some(-1) => {
                            out.push_str(&format!(
                                "      {{ if ({a} == INT64_MIN) {}; }}\n",
                                ftrap("integer overflow in division")
                            ));
                        }
                        // Any other constant traps never.
                        Some(_) => {}
                        None => {
                            out.push_str(&format!(
                                "      {{ if ({b} == 0) {}; if ({a} == INT64_MIN && {b} == -1) {}; }}\n",
                                ftrap("division by zero"),
                                ftrap("integer overflow in division")
                            ));
                        }
                    }
                    out.push_str(&format!("      int64_t {t} = {a} / {b};\n"));
                    tstack.push((t, None));
                }
                Op::IntRem => {
                    let (b, bv) = tstack.pop().unwrap_or_default();
                    let (a, _) = tstack.pop().unwrap_or_default();
                    match bv {
                        Some(0) => {
                            out.push_str(&format!(
                                "      {{ if ({b} == 0) {}; }}\n",
                                ftrap("modulo by zero")
                            ));
                        }
                        Some(-1) => {
                            out.push_str(&format!(
                                "      {{ if ({a} == INT64_MIN) {}; }}\n",
                                ftrap("integer overflow in modulo")
                            ));
                        }
                        Some(_) => {}
                        None => {
                            out.push_str(&format!(
                                "      {{ if ({b} == 0) {}; if ({a} == INT64_MIN && {b} == -1) {}; }}\n",
                                ftrap("modulo by zero"),
                                ftrap("integer overflow in modulo")
                            ));
                        }
                    }
                    out.push_str(&format!("      int64_t {t} = {a} % {b};\n"));
                    tstack.push((t, None));
                }
                Op::IntNeg => {
                    let (a, av) = tstack.pop().unwrap_or_default();
                    match av {
                        Some(x) => {
                            let r = x.wrapping_neg();
                            out.push_str(&format!("      int64_t {t} = {};\n", c_int(r)));
                            tstack.push((t, Some(r)));
                        }
                        None => {
                            out.push_str(&format!("      int64_t {t} = -{a};\n"));
                            tstack.push((t, None));
                        }
                    }
                }
                // Unreachable: the matcher only admits the above.
                _ => return,
            }
        }
        match tail {
            FusedTail::Store(d) => {
                let (v, _) = tstack.pop().unwrap_or_default();
                if let Some(local) = subs.get(&d) {
                    // Promoted slot: assign the C local (write-back at
                    // loop exit keeps the slot authoritative).
                    out.push_str(&format!("      {local} = {v};\n"));
                } else {
                    // Release-then-move: the old slot value is owned
                    // (release it); the fresh immediate carries no refs
                    // (no clone or source release needed). Equivalent to
                    // `zz_cstore` on immediates, minus two dispatches.
                    out.push_str(&format!("      zz_release(&st[{d}]);\n"));
                    out.push_str(&format!("      st[{d}] = zz_int({v});\n"));
                }
            }
            FusedTail::Take(a) => {
                // Element arrives either fused (expression temp) or on the
                // stack (bare six-op tail: the value was pushed before).
                if let Some((v, _)) = tstack.pop() {
                    out.push_str(&format!("      zz_value _rv = zz_int({v});\n"));
                } else {
                    out.push_str("      zz_value _rv = st[--sp];\n");
                }
                // Push straight into the slotted array: no park, no move.
                // Uniqueness never changed hands (nothing was cloned), so
                // the slot keeps its share throughout; the shared dup path
                // mirrors `ArrayPush` exactly (dup the buffer, release the
                // slot's old share, install the dup).
                out.push_str(&format!("      zz_value _av = st[{a}];\n"));
                out.push_str("      if (_av.tag != ZZ_ARRAY) ");
                out.push_str(&format!("{};\n", ftrap("ArrayPush: expected array")));
                out.push_str("      if (_av.arr->refs != 1) { zz_value _d = zz_array_dup(_av.arr); zz_release(&_av); _av = _d; ");
                out.push_str(&format!("st[{a}] = _av; }}\n"));
                out.push_str("      zz_array_push(_av.arr, _rv);\n");
            }
        }
        out.push_str("    }\n");
    }

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

/// Per-op emission context: the precomputed `Call` callee (if any)
/// and the [`fuse_scan`] slot for a fusing `StoreIndexOp`.
struct OpCx<'x> {
    callee: Option<&'x Callee>,
    fuse_slot: Option<u16>,
    /// True when the fused `StoreIndexOp` borrows its home slot (its
    /// `LoadSlot` was skipped — see `fuse_scan` borrow-skip).
    fuse_borrow: bool,
    /// Fused int window starting at this pc, if any: (total length, tail).
    /// See [`int_fuse`]. Window matching already consulted the int-slot
    /// set; emission re-walks the ops.
    ufuse: Option<(usize, FusedTail)>,
    /// Peeled loop starting at this pc, if any (see [`peel_scan`]).
    peel: Option<Peel>,
    /// String-append window starting at this pc, if any: (slot, literal).
    /// See [`strapp_scan`].
    strapp: Option<(u16, ConstId)>,
    /// Index-read window starting at this pc, if any. See [`idxread_scan`].
    idxread: Option<IdxRead>,
    /// Fused int-compare branch starting at this pc, if any. See
    /// [`condfuse_scan`].
    condfuse: Option<CondFuse>,
}

/// One fused unboxed-int window: a straight-line int expression over
/// int-declared slots and int consts, terminated by an int store or a
/// take-push tail. Compiles to guarded raw `int64_t` traffic with no
/// stack touches and no shadow state: operands read the authoritative
/// boxed slot (int guard, fail-closed — every read sees the current
/// value, so loop vars rewritten by `ForNext` and take homes are safe
/// by construction), the result writes back through `zz_cstore`, keeping
/// the slot authoritative for later boxed readers. Division carries the
/// spec traps with the boxed path's messages; wrapping follows `-fwrapv`.
#[derive(Debug, Clone, Copy)]
enum FusedTail {
    /// `d = <expr>`: int store terminator.
    Store(u16),
    /// `a = vec.push(a, <expr>)`: take-push tail (see below).
    Take(u16),
}

impl<'a> Emitter<'a> {
    /// Emit one boxed op (see [`OpCx`]).
    fn emit_op(
        &mut self,
        id: FuncId,
        pc: usize,
        op: &Op,
        cx: &OpCx<'_>,
        out: &mut String,
    ) -> Result<(), ChunkError> {
        let (callee, fuse_slot, fuse_borrow) = (cx.callee, cx.fuse_slot, cx.fuse_borrow);
        // A binding arm opens a scope: push its cells before the pattern's
        // bindings are emitted, so they (and the body's loads) resolve to
        // the arm's own cells. The arm's trailing `ExitScope` pops it.
        if let Op::MatchArm { pat, .. } | Op::IfLetMatch { pat, .. } = op {
            match self.arm_scopes.get(&pc) {
                Some(cells) => self.env_scopes.push(cells.iter().cloned().collect()),
                None => {
                    // `env_layout` records every binding arm; without the
                    // record the bindings would land in an enclosing cell.
                    let mut names = Vec::new();
                    pattern_binds(self.module, pat, &mut names);
                    if !names.is_empty() {
                        return Err(ChunkError::op("binding arm without an env scope"));
                    }
                }
            }
        }
        // Peeled counted loop (see `peel_scan`): raw C loop over the
        // fused body, then the exit edge. The `ForSetup` emission stands
        // (placeholders + `_lbase`), so exit accounting is untouched.
        if let Some(peel) = &cx.peel {
            let f = self
                .module
                .funcs
                .get(id.0 as usize)
                .ok_or_else(|| ChunkError::op("func id"))?;
            Self::emit_peeled(self.module, &f.code, peel, out);
            return Ok(());
        }
        // Fused unboxed-int window (see `int_fuse`): single statement,
        // no per-op emission.
        if let Some((len, tail)) = cx.ufuse {
            let f = self
                .module
                .funcs
                .get(id.0 as usize)
                .ok_or_else(|| ChunkError::op("func id"))?;
            Self::emit_fused_window(self.module, &f.code, pc, len, tail, &HashMap::new(), out);
            return Ok(());
        }
        // Fused string append (see `strapp_scan`): in-place literal
        // append on the slot, no clone, no stack traffic. The tag guard
        // keeps hostile/Unknown-typed modules on exact boxed semantics.
        if let Some((s, c)) = cx.strapp {
            let (lit, len) = match self.module.consts.get(c.0 as usize) {
                Some(Const::Str(id)) => {
                    let text = self
                        .module
                        .strings
                        .get(id.0 as usize)
                        .cloned()
                        .unwrap_or_default();
                    let len = text.len();
                    (c_str(&text), len)
                }
                _ => return Err(ChunkError::op("strapp const")),
            };
            out.push_str(&format!(
                "    {{ if (st[{s}].tag == ZZ_STR) {{ zz_str_append_lit(&st[{s}], {lit}, {len}); }} else {{ zz_value _sa = zz_clone(st[{s}]); zz_value _sb = zz_str_new({lit}, {len}); zz_value _sr = zz_binop(ZZOP_ADD, _sa, _sb); zz_release(&_sa); zz_release(&_sb); zz_cstore(&st[{s}], _sr); }} }}\n"
            ));
            return Ok(());
        }
        // Fused index read (see `idxread_scan`): guarded direct element
        // access with no clone, no call, no stack traffic.
        if let Some(rd) = cx.idxread {
            let f = self
                .module
                .funcs
                .get(id.0 as usize)
                .ok_or_else(|| ChunkError::op("func id"))?;
            Self::emit_idxread(self.module, &f.code, pc, &rd, out)?;
            return Ok(());
        }
        // Fused int-compare branch (see `condfuse_scan`): guarded raw
        // compare + terminator protocol, no stack traffic at all.
        if let Some(cf) = cx.condfuse {
            Self::emit_condfuse(&cf, out)?;
            return Ok(());
        }
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
            // A module-scope binding reads its C global and a
            // function-local env cell (`match` arm binding, captured
            // local) its C local. Every other LoadVar pushes a dummy:
            // covered uses resolve their origin statically and consume the
            // slot directly (see `Call`).
            Op::LoadVar(id) => {
                let name = self.str(*id).to_string();
                let cid = self
                    .env_cell(&name)
                    .cloned()
                    .or_else(|| self.globals.get(&name).cloned());
                match cid {
                    Some(cid) => out.push_str(&format!("    st[sp++] = zz_clone({cid});\n")),
                    // A callee load is a placeholder the following `Call`
                    // consumes (coverage guarantees it); anything else is a
                    // name that is not in scope here — refuse rather than
                    // hand the program a silent `unit`.
                    None if self.is_callee_name(&name) => {
                        out.push_str("    st[sp++] = zz_unit();\n")
                    }
                    None => {
                        return Err(ChunkError::op(&format!(
                            "load `{name}` outside an env cell"
                        )))
                    }
                }
            }
            // Dotted path read: a module-scope binding (`{greeting}` in a
            // template lowers to a path; namespaces are part of the
            // spelling) or a function-local env cell. Field access stays
            // loud.
            Op::LoadPath(parts) => {
                let joined = self.join_path(parts);
                let cid = self
                    .env_cell(&joined)
                    .cloned()
                    .or_else(|| self.globals.get(&joined).cloned());
                if let Some(cid) = cid {
                    out.push_str(&format!("    st[sp++] = zz_clone({cid});\n"));
                } else if self.unit_enums.contains(&joined) {
                    // Unit enum variant used as a value (`Token.End`).
                    out.push_str(&format!(
                        "    st[sp++] = zz_object_new({}, NULL, 0);\n",
                        c_str(&joined)
                    ));
                } else {
                    return Err(ChunkError::op(&format!("dotted path read `{joined}`")));
                }
            }
            // `DefineVar` binds and keeps the value on the stack: the cell
            // takes a clone, the stack keeps the original.
            Op::DefineVar(id) => {
                let name = self.str(*id).to_string();
                let cid = self
                    .env_cell(&name)
                    .cloned()
                    .or_else(|| self.globals.get(&name).cloned());
                let Some(cid) = cid else {
                    return Err(ChunkError::op("define outside an env cell"));
                };
                out.push_str(&format!("    zz_cstore(&{cid}, zz_clone(st[sp-1]));\n"));
            }
            Op::StoreVar(id) => {
                let name = self.str(*id).to_string();
                let cid = self
                    .env_cell(&name)
                    .cloned()
                    .or_else(|| self.globals.get(&name).cloned());
                let Some(cid) = cid else {
                    return Err(ChunkError::op("store to a runtime binding"));
                };
                out.push_str(&format!("    zz_cstore(&{cid}, st[--sp]);\n"));
            }
            // Assignment to a dotted path: a module-scope binding
            // (`count = count + 1` inside a function) or a function-local
            // env cell. Namespaces/fields stay loud.
            Op::StorePath(parts) => {
                let joined = self.join_path(parts);
                let cid = self
                    .env_cell(&joined)
                    .cloned()
                    .or_else(|| self.globals.get(&joined).cloned());
                let Some(cid) = cid else {
                    return Err(ChunkError::op(&format!("dotted path store `{joined}`")));
                };
                out.push_str(&format!("    zz_cstore(&{cid}, st[--sp]);\n"));
            }
            // ---- enums --------------------------------------------
            // Enum metadata is a VM run-time concern (variant validation
            // and display); chunk values carry the qualified type name on
            // the object itself.
            Op::RegisterEnum { .. } => out.push_str("    ;\n"),
            Op::MakeEnum {
                enum_name,
                variant,
                argc,
            } => {
                let qualified = format!("{}.{}", self.str(*enum_name), self.str(*variant));
                if *argc == 0 {
                    out.push_str(&format!(
                        "    st[sp++] = zz_object_new({}, NULL, 0);\n",
                        c_str(&qualified)
                    ));
                } else {
                    // Checker: at most one positional payload, no named.
                    out.push_str("    {\n");
                    out.push_str("        zz_value _pay = st[--sp];\n");
                    out.push_str("        zz_value _names[1] = {zz_str_static(\"value\")};\n");
                    out.push_str(&format!(
                        "        zz_value _obj = zz_object_new({}, _names, 1);\n",
                        c_str(&qualified)
                    ));
                    out.push_str("        zz_object_set_field(&_obj, \"value\", _pay);\n");
                    out.push_str("        st[sp++] = _obj;\n");
                    out.push_str("    }\n");
                }
            }
            // ---- match family -------------------------------------
            // The scrutinee sits on top of the stack. A miss either keeps
            // it (`restore`, the next arm tests the same value) or drops
            // it (guarded schemes reload it from their temp).
            Op::MatchArm {
                pat,
                next,
                has_env: _,
                restore,
            } => {
                let (cond, binds) = self.pat_block(pat, "st[sp-1]")?;
                out.push_str(&format!("    if (!({cond})) {{\n"));
                if !restore {
                    out.push_str("        zz_release(&st[sp-1]); sp--;\n");
                }
                out.push_str(&format!("        goto L{next};\n    }}\n"));
                emit_pat_binds(out, &binds);
                out.push_str("    zz_release(&st[sp-1]); sp--;\n");
            }
            Op::MatchGuard { next, has_env: _ } => {
                out.push_str(&format!(
                    "    {{ zz_value _g = st[--sp]; bool _t = zz_truthy(_g); zz_release(&_g); if (!_t) goto L{next}; }}\n"
                ));
            }
            Op::MatchError => {
                out.push_str("    zz_ftrap(\"non-exhaustive match: no arm matched\");\n");
            }
            // On a miss the scrutinee stays for the `else` path (which
            // starts with `Pop`); on a take it is consumed here.
            Op::IfLetMatch {
                pat,
                els,
                has_env: _,
            } => {
                let (cond, binds) = self.pat_block(pat, "st[sp-1]")?;
                out.push_str(&format!("    if (!({cond})) goto L{els};\n"));
                emit_pat_binds(out, &binds);
                out.push_str("    zz_release(&st[sp-1]); sp--;\n");
            }
            // The VM pops the arm's child environment (dropping its
            // bindings). Here the cells are C locals: pop the compile-time
            // frame so later loads resolve to the enclosing binding again.
            Op::ExitScope => {
                if self.env_scopes.len() > 1 {
                    self.env_scopes.pop();
                }
                out.push_str("    ;\n");
            }
            Op::EnterScope => {
                self.env_scopes.push(HashMap::new());
                out.push_str("    ;\n");
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
                        "    {{ zz_value _b = st[--sp]; zz_value _a = st[--sp]; if (_a.tag == ZZ_RANGE && _b.tag == ZZ_RANGE) {{ zz_range *_x = _a.range; zz_range *_y = _b.range; int _eq = _x && _y && _x->start == _y->start && _x->end == _y->end && _x->step == _y->step; st[sp++] = zz_bool(_eq {op} 1); }} else {{ st[sp++] = zz_binop({c}, _a, _b); }} zz_release(&_a); zz_release(&_b); }}\n"
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
                // Live-prefix sweep, like the implicit return: never the
                // whole frame (dead slots above sp hold stale aliases).
                out.push_str(
                    "    {{ zz_value _r = st[--sp]; for (int _i = 0; _i < sp; _i++) zz_release(&st[_i]); return _r; }}\n",
                );
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
                // NOTE: no dup of the iterable (arrays/dicts share like the
                // VM and HIR: `it.clone()` bumps the share, nothing more).
                // A deep copy here would cost O(n) per loop setup (16MB
                // memcpy for 1M arrays) AND diverge from VM iteration
                // semantics under mid-loop mutation (snapshot vs shared).
                // Sharing is safe: the iterator holds its own share, so
                // the buffer cannot be freed mid-loop; growth reallocs
                // the items (struct stable) and lengths re-read per step.
                out.push_str("      _lbase[_ldepth++] = sp - 1;\n");
                out.push_str("      st[sp++] = _it;\n");
                // Index placeholder; var placeholders pushed by count
                // below (num_vars is a codegen-time constant — read it
                // from the op).
                let nvars = match op {
                    Op::ForSetup { num_vars, .. } => *num_vars as usize,
                    _ => 0,
                };
                // Index placeholder: the VM seeds a range index with
                // `r.start` (not 0), so nonzero-start ranges (`2..n`)
                // visit the right values; arrays/dicts/bytes start at
                // 0. A null range payload (only forgeable in hostile
                // modules — `MakeRange` never builds one) falls back
                // to 0 and traps loudly at the first `ForNext`.
                out.push_str("      { int64_t _i0 = 0;\n");
                out.push_str(
                    "        if (_it.tag == ZZ_RANGE && _it.range) { _i0 = _it.range->start; }\n",
                );
                out.push_str("        st[sp++] = zz_int(_i0); }\n");
                for _ in 0..nvars {
                    out.push_str("      st[sp++] = zz_unit();\n");
                }
                out.push_str("    }\n");
            }
            Op::ForNext {
                vars, exit, in_env, ..
            } => {
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
                out.push_str("        zz_range *_rg = _it.range;\n");
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
                // Keys are stored as ordinary values, so clone the entry's
                // key whatever its type (int/bool/float keys are legal).
                out.push_str("          zz_value _k = zz_clone(_de->key);\n");
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
            Op::MakeVariant { name, has_arg } => {
                // `.some(x)` / `.ok(x)` / `.err(x)` / `.none`. `zz_variant_*`
                // is a take convention: `zz_vbox_new` stores the value with
                // refs == 1, so the popped stack slot's reference moves into
                // the box — the same call HIR emits for `Expr::Variant`.
                // Cloning first would strand the stack reference unless it
                // were released again; `.none` carries no payload, so it
                // pushes the bare tag (#508).
                let variant = self.str(*name).to_string();
                match (variant.as_str(), *has_arg) {
                    ("some", true) => out.push_str(
                        "    { zz_value _v = st[--sp]; st[sp++] = zz_variant_some(_v); }\n",
                    ),
                    ("ok", true) => out.push_str(
                        "    { zz_value _v = st[--sp]; st[sp++] = zz_variant_ok(_v); }\n",
                    ),
                    ("err", true) => out.push_str(
                        "    { zz_value _v = st[--sp]; st[sp++] = zz_variant_err(_v); }\n",
                    ),
                    ("none", false) => {
                        out.push_str("    st[sp++] = (zz_value){ZZ_OPTION_NONE, {0}};\n");
                    }
                    // The VM rejects these shapes at run time (`.ok` needs
                    // an argument, `.none` takes none); checked programs
                    // cannot build them, so fail the build loudly.
                    _ => {
                        return Err(ChunkError::op(&format!(
                            "makevariant {variant} (has_arg={has_arg})"
                        )))
                    }
                }
            }
            Op::MakeArray(n) => {
                // Net move per element: the push adopts the clone, then the
                // stack slot's own reference is dropped. A bare move (push
                // the slot, skip the release) would leak — `zz_array_push`
                // heals arena-derived elements into a fresh heap copy, so
                // the original wrapper's reference still has to go — and
                // before the box refcount landed (#507) that release freed
                // the immortal box the array element kept pointing at, i.e.
                // the double free reproduced in #508. Variants are ordinary
                // refcounted boxes now, so this pair is the box-aware path.
                out.push_str(&format!(
                    "    {{ zz_value _a = zz_array_new(); for (int _k = 0; _k < {n}; _k++) {{ zz_value _e = st[sp-{n}+_k]; zz_array_push(_a.arr, zz_clone(_e)); }} for (int _k = 0; _k < {n}; _k++) zz_release(&st[sp-{n}+_k]); sp -= {n}; st[sp++] = _a; }}\n"
                ));
            }
            Op::MakeDict(n) => {
                // Stack holds [k0, v0, k1, v1, …] (key first per pair,
                // program order). Fresh dict (refs == 1): the detach
                // check inside `zz_index_set` never fires — the same
                // call HIR emits per pair, so dup-key last-wins matches
                // HIR (the VM keeps dup pairs; pathological input only).
                // Ownership mirrors Const::Dict: keys retained (release
                // our share), values adopted (never release — the dict
                // owns them now, so the stack window is dropped, not
                // swept). Non-string keys are ignored by `zz_dict_set`
                // exactly like HIR (checker-valid programs never do).
                let m = 2 * (*n as usize);
                out.push_str(&format!(
                    "    {{ zz_value _d = zz_dict_new_sized({n}); for (int _k = 0; _k < {n}; _k++) {{ zz_value _kk = st[sp-{m}+2*_k]; zz_value _vv = st[sp-{m}+2*_k+1]; int _de = 0; zz_index_set(&_d, _kk, _vv, &_de); zz_release(&_kk); }} sp -= {m}; st[sp++] = _d; }}\n"
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
                    // top-3 — unless `fuse_borrow` skipped the load, in
                    // which case the middle executed one slot lower and
                    // the home slot is borrowed directly. Either way
                    // exactly [index, value] sit on top here; the
                    // following `StoreSlot(s)` is skipped.
                    if fuse_borrow {
                        out.push_str("    { zz_value _fv = st[--sp]; zz_value _fix = st[--sp];\n");
                    } else {
                        out.push_str("    { zz_value _fv = st[--sp]; zz_value _fix = st[--sp];\n");
                        out.push_str("      zz_release(&st[--sp]);\n");
                    }
                    // Unique-array fast path: `refs == 1` is exactly
                    // `zz_index_set`'s in-place condition (shared /
                    // arena / sentinel buffers all carry `refs != 1`
                    // and take the generic path, which dups or heals).
                    // With a non-negative in-bounds int index the
                    // outcome is bit-identical to the generic call
                    // minus the call overhead: same `zz_assign`
                    // (release-old, adopt-new), same releases. Anything
                    // else — dicts, non-int indices, negatives, OOB —
                    // runs the untouched generic emission below.
                    out.push_str(&format!(
                        "      int _fe = 0;\n      if (st[{s}].tag == ZZ_ARRAY && st[{s}].arr && st[{s}].arr->refs == 1 && _fix.tag == ZZ_INT && _fix.i >= 0 && _fix.i < (int64_t)st[{s}].arr->len) {{\n        zz_assign(&st[{s}].arr->items[(size_t)_fix.i], _fv);\n        zz_release(&_fv);\n        zz_release(&_fix);\n      }} else {{\n        zz_index_set(&st[{s}], _fix, _fv, &_fe);\n        zz_release(&_fix); zz_release(&_fv);\n        if (_fe) {ctrap};\n      }}\n",
                        ctrap = ctrap("index out of bounds")
                    ));
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
                // Ranges are first-class refcounted values shared with
                // the HIR backend (`zz_range_build_step`); the bounds
                // trap names the failure exactly like the VM (the
                // constructor itself would only degrade to `0..0`).
                out.push_str("    { zz_value _e5 = st[--sp]; zz_value _s5 = st[--sp];\n");
                out.push_str("      if (_s5.tag != ZZ_INT || _e5.tag != ZZ_INT) ");
                out.push_str(&format!("{};\n", ctrap("range bounds must be integers")));
                out.push_str("      zz_value _r = zz_range_build_step(_s5, _e5, zz_int(1));\n");
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

/// Chunk-local preamble: store helper, cold guard trap, immortal range boxes.
const CHUNK_PREAMBLE: &str = r#"
static inline void zz_cstore(zz_value *dst, zz_value v) {
    zz_release(dst);
    *dst = zz_clone(v);
    zz_release(&v);
}

// Cold trap for fused/peel guards: keeps abort blocks out of hot loops
// (icache + unrolling). Same bytes on stderr + exit 1 as inline traps.
// `__attribute__((cold))` places it with other cold code.
__attribute__((cold)) static void zz_ftrap(const char *msg) {
    fprintf(stderr, "zz error: %s\n", msg);
    exit(1);
}

// IR ranges use the shared first-class C runtime representation
// (`zz_range`: refcounted start/end/step, same as the HIR backend —
// see `zz_range_build_step`). The old chunk-local immortal `zz_crange`
// boxes are gone: printing a range renders `s..e` through the shared
// printer instead of box-pointer bits, and boxes free normally (#254).

// ---- match support -----------------------------------------------------
// Borrow-clone an arm binding into its env cell: the source is read-only
// (pattern places point into the scrutinee), so the cell takes a clone.
static inline void zz_cassign(zz_value *dst, zz_value src) {
    zz_release(dst);
    *dst = zz_clone(src);
}

// Borrowed payload of a user-enum/struct value (`value` field), or NULL.
// Pattern tests read through this, so a miss clones nothing and cannot
// leave a half-built binding behind.
static const zz_value *zz_cfield(const zz_value *v, const char *name) {
    if (!v || v->tag != ZZ_OBJECT || !v->obj) return NULL;
    for (size_t i = 0; i < v->obj->len; i++) {
        const zz_value *k = &v->obj->fields[i * 2];
        if (k->tag == ZZ_STR && k->s && strcmp(zz_str_cptr(k->s), name) == 0) {
            return &v->obj->fields[i * 2 + 1];
        }
    }
    return NULL;
}

// Borrowed array element, or NULL (tuples erase to arrays).
static const zz_value *zz_carr_at(const zz_value *v, size_t i) {
    if (!v || v->tag != ZZ_ARRAY || !v->arr || i >= v->arr->len) return NULL;
    return &v->arr->items[i];
}

// User-enum tag test: a pattern names the variant short (`.IntLit`) while
// enum values carry the qualified type (`Token.IntLit`), so compare the
// trailing segment — exactly the VM's rule.
static int zz_cenum_suffix(const zz_value *v, const char *suffix) {
    if (!v || v->tag != ZZ_OBJECT || !v->obj || !v->obj->type_name) return 0;
    const char *t = v->obj->type_name;
    size_t n = strlen(suffix), m = strlen(t);
    if (m < n + 1) return 0;
    return t[m - n - 1] == '.' && memcmp(t + m - n, suffix, n) == 0;
}

"#;

/// Compile a verified IR module to C source behind the HIR backend's
/// `LoweredC` shape (source + link flags), so the build pipeline can
/// treat both backends uniformly.
///
/// `entry_main` is the dotted main (e.g. `fib35.main`); the module
/// entry chunk (top-level effects) runs first, then `entry_main`. An
/// empty `entry_main` means the program has no user `main` (script
/// style: the entry chunk is the whole program, like the HIR backend's
/// `zz_main`-only shape). Fails cleanly via [`coverage`] before emitting
/// anything.
pub fn build_module(
    module: &Module,
    entry_main: &str,
) -> Result<super::lower::LoweredC, ChunkError> {
    // The format carries the VM's fused slot fast paths (the interpreter
    // runs them natively) and its rotated `for` layout. Every pass below
    // analyses and emits the core, unrotated stream — int-twin selection,
    // peels, cond-fusion — so derotate and expand first: the passes, and
    // therefore the C output, are exactly the pre-v3/v4 ones.
    let derotated = zz_ir::lower::derotate_loops(module)
        .map_err(|e| ChunkError::op(&format!("derotate loops: {e}")))?;
    let expanded = zz_ir::lower::expand_fused(derotated.as_ref())
        .map_err(|e| ChunkError::op(&format!("expand fused slot ops: {e}")))?;
    let module: &Module = expanded.as_ref();
    coverage(module)?;
    let mut em = Emitter {
        module,
        csyms: HashMap::new(),
        globals: HashMap::new(),
        env_scopes: vec![HashMap::new()],
        arm_scopes: HashMap::new(),
        unit_enums: HashSet::new(),
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
    // Script-style programs (no user `main`) pass an empty key: the
    // synthetic top-level chunk already ran in `zz_main`, so the CLI glue
    // has nothing to call (mirrors the HIR backend's `zz_call_main`).
    let has_main = !entry_main.is_empty();
    if has_main && !em.csyms.contains_key(entry_main) {
        return Err(ChunkError::name(&format!("entry {entry_main}")));
    }
    for name in module_globals(module) {
        em.globals
            .insert(name.clone(), format!("zz_g_{}", mangle(&name)));
    }
    // Unit enum variants are values (`Token.End`), not constructors: their
    // dotted path builds the object directly.
    for f in &module.funcs {
        for op in &f.code {
            if let Op::RegisterEnum { name, variants } = op {
                let Some(enum_name) = module.strings.get(name.0 as usize) else {
                    continue;
                };
                for (variant, has_payload) in variants {
                    if *has_payload {
                        continue;
                    }
                    if let Some(vname) = module.strings.get(variant.0 as usize) {
                        em.unit_enums.insert(format!("{enum_name}.{vname}"));
                    }
                }
            }
        }
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
    // Module-scope env bindings: zero-initialized (`ZZ_UNIT`) file-scope
    // globals, assigned by the top-level function before `main` runs.
    let mut gids: Vec<&String> = em.globals.values().collect();
    gids.sort();
    for cid in gids {
        bodies.push_str(&format!("static zz_value {cid};\n"));
    }
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
    let main_call = if !has_main {
        "zz_unit()".to_string()
    } else if main_arity == 0 {
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

#[cfg(test)]
mod condfuse_tests {
    use super::*;
    use zz_ir::op::BinOp as B;

    fn ints(slots: &[u16]) -> HashSet<u16> {
        slots.iter().copied().collect()
    }

    fn consts() -> Vec<Const> {
        vec![Const::Int(10)]
    }

    #[test]
    fn while_slot_slot_fuses() {
        // `loadslot 7, loadslot 0, binop Lt, whilecond` (sieve shape).
        let code = vec![
            Op::LoadSlot(7),
            Op::LoadSlot(0),
            Op::BinOp(B::Lt),
            Op::WhileCond { exit: 76 },
        ];
        let (fused, skip) = condfuse_scan(
            &code,
            &consts(),
            &ints(&[0, 7]),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(fused.len(), 1);
        let cf = &fused[&0];
        assert!(matches!(cf.term, CondTerm::While(76)));
        assert!(matches!(cf.cmp, B::Lt));
        assert_eq!(skip, HashSet::from([1, 2, 3]));
    }

    #[test]
    fn while_slot_const_fuses() {
        let code = vec![
            Op::LoadSlot(3),
            Op::PushConst(ConstId(0)),
            Op::BinOp(B::Gt),
            Op::WhileCond { exit: 9 },
        ];
        let (fused, _) = condfuse_scan(
            &code,
            &consts(),
            &ints(&[3]),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(fused.len(), 1);
    }

    #[test]
    fn jump_terminators_fuse() {
        for (term, ok) in [
            (Op::JumpIfFalse(5), true),
            (Op::JumpIfTrue(5), true),
            (Op::Jump(5), false),
        ] {
            let code = vec![Op::LoadSlot(1), Op::LoadSlot(2), Op::BinOp(B::Eq), term];
            let (fused, _) = condfuse_scan(
                &code,
                &consts(),
                &ints(&[1, 2]),
                &HashSet::new(),
                &HashSet::new(),
            );
            assert_eq!(!fused.is_empty(), ok);
        }
    }

    #[test]
    fn non_int_operands_and_ops_do_not_fuse() {
        // Unknown slot stays boxed.
        let code = vec![
            Op::LoadSlot(7),
            Op::LoadSlot(9),
            Op::BinOp(B::Lt),
            Op::WhileCond { exit: 1 },
        ];
        let (fused, _) = condfuse_scan(
            &code,
            &consts(),
            &ints(&[7]),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(fused.is_empty());
        // Non-comparison BinOp stays boxed.
        let code = vec![
            Op::LoadSlot(7),
            Op::LoadSlot(0),
            Op::BinOp(B::Add),
            Op::WhileCond { exit: 1 },
        ];
        let (fused, _) = condfuse_scan(
            &code,
            &consts(),
            &ints(&[0, 7]),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(fused.is_empty());
    }

    #[test]
    fn interior_jump_target_blocks() {
        let code = vec![
            Op::LoadSlot(7),
            Op::LoadSlot(0),
            Op::BinOp(B::Lt),
            Op::WhileCond { exit: 1 },
        ];
        let targets = HashSet::from([2u32]);
        let (fused, _) = condfuse_scan(&code, &consts(), &ints(&[0, 7]), &targets, &HashSet::new());
        assert!(fused.is_empty());
    }
}
