//! Minimal `.zzc` verifier: structural checks plus static stack depth.
//!
//! Every function body simulates operand-stack depth over its op stream
//! (forward edges and back-edges via worklist). Rules enforced:
//!
//! - all table ids (strings, consts, funcs, types) are in range,
//! - all jump targets land inside the owning function,
//! - every join (jump target, fall-through after a jump) agrees on one
//!   depth — the "static stack depth at joins" rule (spec §7),
//! - the stack never underflows, and the declared `max_stack` equals the
//!   observed maximum.
//!
//! The verifier is type-agnostic (depth only); annotations are ignored.

use crate::{Const, FuncDef, IrError, Module, Op, Pattern, Span};

/// Net stack effect of one core op (pushed minus popped).
fn effect(op: &Op) -> i64 {
    match op {
        Op::PushConst(_) => 1,
        Op::Pop => -1,
        Op::Swap => 0,
        Op::PopN(n) => -(*n as i64),
        Op::Truthy => 0,
        Op::LoadVar(_) | Op::LoadPath(_) | Op::LoadSlot(_) => 1,
        Op::TakeVar(_) => 1,
        Op::DefineVar(_) => 0,
        Op::StoreVar(_) | Op::StorePath(_) | Op::StoreSlot(_) => -1,
        Op::VecPushField { .. } | Op::VecPushMethod { .. } => -1,
        Op::IntAdd | Op::IntSub | Op::IntMul | Op::IntDiv | Op::IntRem => -1,
        Op::IntNeg => 0,
        Op::BinOp(_) => -1,
        Op::UnOp(_) => 0,
        Op::Jump(_) => 0,
        Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::JumpIfFalseBool(_) => -1,
        Op::Return => -1,
        Op::ForSetup { num_vars, .. } => 1 + *num_vars as i64,
        Op::ForNext { .. } => 0,
        Op::WhileSetup { .. } => 0,
        Op::WhileCond { .. } => -1,
        Op::Break | Op::Continue => 0,
        Op::SetLoopResult => -1,
        Op::Safepoint => 0,
        Op::MakeArray(n) => 1 - *n as i64,
        Op::UnpackTuple(n) => *n as i64 - 1,
        Op::ArrayPush => -1,
        Op::MakeDict(n) => 1 - 2 * *n as i64,
        Op::IndexOp => -1,
        Op::StoreIndexOp => -2,
        Op::CompoundIndexOp { .. } => -2,
        Op::SliceOp => -2,
        Op::MakeRange => -1,
        Op::MakeStruct { fields, .. } => 1 - fields.len() as i64,
        Op::MakeEnum { argc, .. } => 1 - (*argc as i64),
        Op::GetField(_) | Op::GetFieldIdx(_) => 0,
        Op::SetField(_) | Op::SetFieldIdx(_) => -1,
        Op::CompoundFieldOp { .. } => -1,
        Op::RegisterStruct { .. } | Op::RegisterEnum { .. } => 1,
        Op::MakeClosure { .. } | Op::MakeFunc { .. } | Op::SpawnClosure { .. } => 1,
        Op::MakeVariant { has_arg, .. } => {
            if *has_arg {
                0
            } else {
                1
            }
        }
        Op::MatchArm { .. } => -1,
        Op::MatchGuard { .. } => -1,
        Op::MatchError => 0,
        Op::IfLetMatch { .. } => -1,
        Op::TryOp => 0,
        // Elvis pops the tested value and pushes unwrapped + success flag.
        Op::Elvis => 1,
        Op::ElvisResult => -2,
        Op::Call { argc } => -(*argc as i64),
        Op::CallMethod { argc, .. } => -(*argc as i64),
        Op::CallNative { argc, .. } => 1 - (*argc as i64),
        Op::CallPath { argc, .. } => 1 - (*argc as i64),
        Op::Concat(n) => 1 - *n as i64,
        Op::FormatValue => -1,
        // DbQuery only reorders stack values (template/params) net 0.
        Op::DbQuery { .. } => 0,
        Op::EnterScope | Op::ExitScope => 0,
        Op::DeferRecord => -1,
    }
}

fn jump_targets(op: &Op) -> Vec<u32> {
    // Loop SETUPs never transfer control to their exit (the field only
    // feeds `break` resolution at runtime); only the exhausting ops do.
    match op {
        Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
            vec![*t]
        }
        Op::ForNext { exit, .. } => vec![*exit],
        Op::WhileCond { exit } => vec![*exit],
        Op::MatchArm { next, .. } => vec![*next],
        Op::MatchGuard { next, .. } => vec![*next],
        Op::IfLetMatch { els, .. } => vec![*els],
        _ => Vec::new(),
    }
}

fn check_str(module: &Module, id: crate::StrId, what: &str, span: Span) -> Result<(), IrError> {
    if (id.0 as usize) < module.strings.len() {
        Ok(())
    } else {
        Err(IrError::spanned(
            format!("{what}: string id out of range"),
            span,
        ))
    }
}

fn check_pattern(module: &Module, pat: &Pattern, span: Span) -> Result<(), IrError> {
    match pat {
        Pattern::Wildcard => Ok(()),
        Pattern::Binding(id) => check_str(module, *id, "pattern binding", span),
        Pattern::Lit(id) => {
            if (id.0 as usize) >= module.consts.len() {
                return Err(IrError::spanned(
                    "pattern literal: const id out of range",
                    span,
                ));
            }
            match &module.consts[id.0 as usize] {
                Const::Int(_) | Const::Float(_) | Const::Str(_) | Const::Bool(_) => Ok(()),
                _ => Err(IrError::spanned(
                    "pattern literal must be int/float/str/bool",
                    span,
                )),
            }
        }
        Pattern::Variant { name, arg } => {
            check_str(module, *name, "pattern variant", span)?;
            if let Some(p) = arg {
                check_pattern(module, p, span)?;
            }
            Ok(())
        }
        Pattern::Tuple(pats) | Pattern::Or(pats) => {
            for p in pats {
                check_pattern(module, p, span)?;
            }
            Ok(())
        }
    }
}

/// Verify one function body: ids, jumps, join-depth agreement, then
/// stack-slot type inference against the declared locals table.
/// Returns the observed maximum stack depth.
fn verify_func(
    module: &Module,
    func_idx: usize,
    funcs_by_name: &std::collections::HashMap<String, usize>,
) -> Result<u32, IrError> {
    let func: &FuncDef = &module.funcs[func_idx];
    let n = func.code.len();
    if func.spans.len() != n {
        return Err(IrError::new("span/code length mismatch"));
    }
    let span_at = |pc: usize| func.spans.get(pc).copied().unwrap_or(Span::new(0, 0));
    // Id range checks first (cheap, precise errors).
    for (pc, op) in func.code.iter().enumerate() {
        let span = span_at(pc);
        let str = |id: crate::StrId, what: &str| check_str(module, id, what, span);
        match op {
            Op::PushConst(id) => {
                if (id.0 as usize) >= module.consts.len() {
                    return Err(IrError::spanned("const id out of range", span));
                }
            }
            Op::LoadVar(id) | Op::DefineVar(id) | Op::StoreVar(id) | Op::TakeVar(id) => {
                str(*id, "variable name")?;
            }
            Op::LoadPath(parts) | Op::StorePath(parts) => {
                for id in parts {
                    str(*id, "path component")?;
                }
            }
            Op::VecPushField {
                home_slot,
                home_var,
                field,
                ..
            }
            | Op::VecPushMethod {
                home_slot,
                home_var,
                method: field,
                ..
            } => {
                if home_slot.is_some() == home_var.is_some() {
                    return Err(IrError::spanned("push home must be a slot xor a var", span));
                }
                if let Some(id) = home_var {
                    str(*id, "push home")?;
                }
                str(*field, "push field/method")?;
            }
            Op::MakeStruct { name, fields } => {
                str(*name, "struct name")?;
                for id in fields {
                    str(*id, "struct field")?;
                }
            }
            Op::GetField(id) | Op::SetField(id) => str(*id, "field name")?,
            Op::CompoundFieldOp { name, .. } => str(*name, "field name")?,
            Op::RegisterStruct { name, fields } => {
                str(*name, "struct name")?;
                for id in fields {
                    str(*id, "struct field")?;
                }
            }
            Op::RegisterEnum { name, variants } => {
                str(*name, "enum name")?;
                for (id, _) in variants {
                    str(*id, "enum variant")?;
                }
            }
            Op::MakeEnum {
                enum_name, variant, ..
            } => {
                str(*enum_name, "enum name")?;
                str(*variant, "enum variant")?;
            }
            Op::MakeClosure { func } | Op::MakeFunc { func } | Op::SpawnClosure { func } => {
                if (func.0 as usize) >= module.funcs.len() {
                    return Err(IrError::spanned("func id out of range", span));
                }
            }
            Op::MakeVariant { name, .. } => str(*name, "variant name")?,
            Op::MatchArm { pat, .. } | Op::IfLetMatch { pat, .. } => {
                check_pattern(module, pat, span)?;
            }
            Op::CallPath { parts, .. } => {
                for id in parts {
                    str(*id, "call path component")?;
                }
            }
            Op::CallMethod { name, .. } | Op::CallNative { name, .. } => {
                str(*name, "call name")?;
            }
            Op::ForNext { vars, .. } => {
                for id in vars {
                    str(*id, "loop variable")?;
                }
            }
            _ => {}
        }
        for t in jump_targets(op) {
            if (t as usize) > n {
                return Err(IrError::spanned("jump target out of range", span));
            }
        }
    }
    // Depth simulation with a worklist (handles back-edges).
    let observed = simulate(&func.code, &func.spans)?;
    if func.max_stack != observed {
        return Err(IrError::new(format!(
            "max_stack mismatch: declared {}, observed {observed}",
            func.max_stack
        )));
    }
    // Locals-table integrity: one entry per referenced slot, params
    // coherent with the signature. Checked here (after ids, before the
    // inference walk that reads the table).
    let mut maxref: Option<usize> = None;
    for op in &func.code {
        match op {
            Op::LoadSlot(s) | Op::StoreSlot(s) => {
                maxref = Some(maxref.map_or(*s as usize, |m: usize| m.max(*s as usize)));
            }
            _ => {}
        }
    }
    for (_, s) in &func.toplevel_slots {
        maxref = Some(maxref.map_or(*s as usize, |m: usize| m.max(*s as usize)));
    }
    if func.locals.len() != maxref.map_or(0, |m| m + 1) {
        return Err(IrError::new(format!(
            "locals length mismatch: table has {}, code references {}",
            func.locals.len(),
            maxref.map_or(0, |m| m + 1)
        )));
    }
    if func.sig.params.len() != func.arity as usize || func.params.len() != func.arity as usize {
        return Err(IrError::new("signature/params arity mismatch"));
    }
    for id in func.locals.iter().chain(func.sig.params.iter()) {
        if (id.0 as usize) >= module.types.len() {
            return Err(IrError::new("locals: type id out of range"));
        }
    }
    // Param slots are authoritative from the signature: entries must
    // match structurally (lowering seeds them; mismatch is corruption).
    for (i, t) in func.sig.params.iter().enumerate() {
        let (Some(a), Some(b)) = (
            module.types.get(func.locals[i].0 as usize),
            module.types.get(t.0 as usize),
        ) else {
            return Err(IrError::new("locals: type id out of range"));
        };
        if a != b {
            return Err(IrError::new(format!(
                "locals: param slot {i} disagrees with signature"
            )));
        }
    }
    // vartab coherence: kth entry ↔ kth `ForNext`, arity matches.
    // Content is compiler-attested (like `locals`); shape is checked so
    // hand-built tables fail closed instead of misaligning the emitter.
    let nfornext = func
        .code
        .iter()
        .filter(|op| matches!(op, Op::ForNext { .. }))
        .count();
    if func.vartab.len() != nfornext {
        return Err(IrError::new(format!(
            "vartab length mismatch: table has {}, code has {nfornext} for-loops",
            func.vartab.len()
        )));
    }
    let mut vit = func.vartab.iter();
    for op in &func.code {
        if let Op::ForNext { vars, .. } = op {
            match vit.next() {
                Some(entry) if entry.len() == vars.len() => {}
                _ => {
                    return Err(IrError::new("vartab entry arity mismatch"));
                }
            }
        }
    }
    // Typed inference + checks (never trusts annotations: none exist).
    infer_func(module, func_idx, funcs_by_name, prim_ids(module))?;
    Ok(observed)
}

/// Simulate stack depth over flat code, enforcing join agreement.
/// Returns the observed maximum depth. Used by [`verify`] per function
/// and by [`lower`] to stamp `max_stack` before verification.
///
/// [`verify`]: verify()
/// [`lower`]: crate::lower::lower
pub fn max_stack_for(code: &[Op]) -> Result<u32, IrError> {
    let blanks = vec![Span::new(0, 0); code.len()];
    simulate(code, &blanks)
}

fn simulate(code: &[Op], spans: &[Span]) -> Result<u32, IrError> {
    let n = code.len();
    let span_at = |pc: usize| spans.get(pc).copied().unwrap_or(Span::new(0, 0));
    let mut depth_at: Vec<Option<i64>> = vec![None; n];
    let mut work: Vec<usize> = vec![0];
    if n == 0 {
        return Ok(0);
    }
    depth_at[0] = Some(0);
    let mut max_depth: i64 = 0;
    // Loop-exit pcs mapped to their expected depth. `ForNext`
    // exhaustion pops the index/vars AND the iterable, then truncates
    // to the result placeholder (`stack_base + 1`): the exit edge
    // carries setup_depth - 1 ([..., PH]). `WhileCond` exhaustion
    // truncates to its base with no pop: the exit carries setup_depth,
    // and the historical +1 there is load-bearing for while-emission
    // shapes — do not "fix" it without re-verifying every while
    // fixture. (A uniform +1 used to cover both; harmless for
    // unnested loops whose exits have a single predecessor, but it
    // poisoned outer back-edges around nested `for` loops: #311.)
    let mut loop_exits: std::collections::HashMap<usize, i64> = std::collections::HashMap::new();
    while let Some(pc) = work.pop() {
        let depth = depth_at[pc].expect("verifier worklist");
        let op = &code[pc];
        // `Swap` needs two live values; the net effect (0) cannot show it.
        if matches!(op, Op::Swap) && depth < 2 {
            return Err(IrError::spanned("swap needs two stack values", span_at(pc)));
        }
        if let Op::ForSetup { exit, .. } = op {
            loop_exits.entry(*exit as usize).or_insert(depth - 1);
        } else if let Op::WhileSetup { exit, .. } = op {
            loop_exits.entry(*exit as usize).or_insert(depth + 1);
        }
        // `Pop` on an empty stack is a runtime no-op (type aliases emit
        // bare `Pop`s; the compiler's own accounting saturates too), so
        // the verifier clamps at zero instead of erroring. (Note:
        // `saturating_sub` on the signed depth only clamps at `MIN` —
        // clamp explicitly.) Every other op unwraps and must stay strict.
        let after = if matches!(op, Op::Pop) {
            (depth - 1).max(0)
        } else {
            depth + effect(op)
        };
        if after < 0 {
            return Err(IrError::spanned(
                format!("stack underflow at {pc}"),
                span_at(pc),
            ));
        }
        if after > max_depth {
            max_depth = after;
        }
        // Fall-through edge. Jump/Return/Break/Continue never fall
        // through at runtime (break/continue unwind the loop, so code
        // after them is dead): excluding them keeps dead stack
        // pollution out of downstream joins.
        let falls_through = !matches!(op, Op::Jump(_) | Op::Return | Op::Break | Op::Continue);
        //
        // Restoring match ops push the scrutinee back on their MISS edge
        // (net 0 there, -1 on the taken edge): model each edge exactly.
        let miss_pushback = match op {
            Op::MatchArm {
                next,
                restore: true,
                ..
            } => Some(*next as usize),
            Op::IfLetMatch { els, .. } => Some(*els as usize),
            _ => None,
        };
        if falls_through && pc + 1 < n {
            match depth_at[pc + 1] {
                Some(d) if d != after => {
                    return Err(IrError::spanned("join depth mismatch", span_at(pc + 1)));
                }
                Some(_) => {}
                None => {
                    depth_at[pc + 1] = Some(after);
                    work.push(pc + 1);
                }
            }
        }
        for t in jump_targets(op) {
            let t = t as usize;
            if t > n {
                return Err(IrError::spanned("jump target out of range", span_at(pc)));
            }
            if t == n {
                // Terminal edge: jumping one-past-end halts the frame
                // (loop exits, arm bodies). No join to check.
                continue;
            }
            // Miss edge of a restoring match op: scrutinee pushed back.
            // Loop exits truncate to (setup depth + 1) — see above.
            let edge_depth = if Some(t) == miss_pushback {
                depth
            } else if let Some(loop_depth) = loop_exits.get(&t) {
                *loop_depth
            } else {
                after
            };
            match depth_at[t] {
                Some(d) if d != edge_depth => {
                    return Err(IrError::spanned(
                        format!(
                            "join depth mismatch at {t}: have {d}, edge {edge_depth} (from {pc})"
                        ),
                        span_at(t),
                    ));
                }
                Some(_) => {}
                None => {
                    depth_at[t] = Some(edge_depth);
                    work.push(t);
                }
            }
        }
    }
    Ok(max_depth as u32)
}

/// Verify a whole module: table coherence plus every function body.
pub fn verify(module: &Module) -> Result<(), IrError> {
    if module.funcs.is_empty() {
        return Err(IrError::new("empty function table"));
    }
    if (module.entry.0 as usize) >= module.funcs.len() {
        return Err(IrError::new("entry func id out of range"));
    }
    // Dotted-name index for call-target resolution in inference.
    let mut funcs_by_name: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (i, f) in module.funcs.iter().enumerate() {
        if let Some(name) = module.strings.get(f.name.0 as usize) {
            funcs_by_name.entry(name.clone()).or_insert(i);
        }
    }
    // Const pool: nested ids precede their users (single pass, in order).
    for (i, c) in module.consts.iter().enumerate() {
        let ok = |id: crate::ConstId| (id.0 as usize) < i;
        let bad = match c {
            Const::Array(items) => items.iter().any(|id| !ok(*id)),
            Const::Dict(pairs) => pairs.iter().any(|(k, v)| !ok(*k) || !ok(*v)),
            Const::Option(inner) => inner.is_some_and(|id| !ok(id)),
            Const::Result { val, .. } => !ok(*val),
            Const::Str(id) => (id.0 as usize) >= module.strings.len(),
            _ => false,
        };
        if bad {
            return Err(IrError::new(format!(
                "const {i} references out-of-range id"
            )));
        }
    }
    // Type table: nested ids precede their users.
    for (i, t) in module.types.iter().enumerate() {
        let mut ids = Vec::new();
        t.collect_ids(&mut ids);
        for id in ids {
            if (id.0 as usize) >= i {
                return Err(IrError::new(format!("type {i} references out-of-range id")));
            }
            if (id.0 as usize) >= module.types.len() {
                return Err(IrError::new(format!("type {i} references missing id")));
            }
        }
        if let crate::IrType::Opaque(id) | crate::IrType::Named(id) = t {
            if (id.0 as usize) >= module.strings.len() {
                return Err(IrError::new(format!("type {i} references missing string")));
            }
        }
        if let crate::IrType::Struct(id, _) | crate::IrType::Enum(id, _) = t {
            if (id.0 as usize) >= module.strings.len() {
                return Err(IrError::new(format!("type {i} references missing string")));
            }
        }
    }
    // Signatures reference the table.
    for (i, f) in module.funcs.iter().enumerate() {
        for id in f.sig.params.iter().chain(std::iter::once(&f.sig.ret)) {
            if (id.0 as usize) >= module.types.len() {
                return Err(IrError::new(format!(
                    "func {i} signature: type id out of range"
                )));
            }
        }
        if (f.name.0 as usize) >= module.strings.len() {
            return Err(IrError::new(format!("func {i}: name id out of range")));
        }
        for p in &f.params {
            if (p.name.0 as usize) >= module.strings.len() {
                return Err(IrError::new(format!("func {i}: param name out of range")));
            }
            if let Some(d) = p.default {
                if (d.0 as usize) >= module.funcs.len() {
                    return Err(IrError::new(format!("func {i}: default func out of range")));
                }
            }
        }
        for (name, _) in &f.toplevel_slots {
            if (name.0 as usize) >= module.strings.len() {
                return Err(IrError::new(format!("func {i}: slot name out of range")));
            }
        }
    }
    for idx in 0..module.funcs.len() {
        verify_func(module, idx, &funcs_by_name)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Typed verification (v2): stack-slot type inference + checks.
//
// The verifier tracks an abstract type per operand-stack position through
// the same edges as the depth simulation (forward, jumps, loop exits,
// match miss-pushback). Slot loads yield the DECLARED locals entry
// (bidirectional checking); slot stores, calls, returns, and boolean
// conditions are checked against declarations with a weak compatibility
// rule: `Unknown` (or a wildcard: inference variable, error type, named
// alias, opaque, or layout-bearing Func/Struct/Enum/Ptr) is compatible
// with everything; exact types compare structurally with covariant
// constructors. Consequences:
//
// - Valid checker-produced code can never be rejected: every exact claim
//   the transfer functions make holds on every value that actually flows
//   (verified against `vm::runtime` + `runtime::ops` semantics), and
//   precision gaps degrade to `Unknown`, which the weak rule absorbs.
// - Corrupt modules are rejected exactly where they contradict themselves
//   observably (wrong store/call/return/condition types). Anything
//   unobservable stays boxed-or-guarded downstream: backends decide
//   representation from declarations and guard every unboxing boundary,
//   so acceptance never implies trust.
// - The verifier never consults per-op annotations (none exist in the
//   format) and has no oracle: natives, env values, and dynamic callees
//   all infer `Unknown`.

use crate::IrType;

/// Abstract value type: unknown or an owned (possibly constructed) type.
/// Owned (rather than table ids) so inference can build types the module
/// table never mentions; comparison is structural.
#[derive(Debug, Clone, PartialEq)]
enum TV {
    U,
    T(IrType),
}

impl TV {
    fn of_id(module: &Module, id: crate::TypeId) -> TV {
        match module.types.get(id.0 as usize) {
            // Dangling ids are table corruption (rejected by the
            // integrity check); infer Unknown so one error surfaces.
            // `Unknown` table entries also normalize to `U`: the two
            // spellings must never diverge in checks.
            None => TV::U,
            Some(IrType::Unknown) => TV::U,
            Some(t) => TV::T(t.clone()),
        }
    }
}

/// Shallow wildcards (no table lookup): Unknown/Var/Error/Named/Opaque,
/// plus any composite the verifier cannot resolve layouts for
/// (Func/Struct/Enum/Ptr — no defs in IR, always boxed downstream).
/// Wildcards accept everything in compatibility; acceptance is sound
/// because wildcards never unbox.
fn is_shallow_wild(t: &IrType) -> bool {
    matches!(
        t,
        IrType::Unknown
            | IrType::Var(_)
            | IrType::Error
            | IrType::Named(_)
            | IrType::Opaque(_)
            | IrType::Func(_, _)
            | IrType::Struct(_, _)
            | IrType::Enum(_, _)
            | IrType::Ptr { .. }
    )
}

/// Weak compatibility: inferred ⊑ declared (both fully expanded here).
/// Unions: an exact inferred value belongs to a declared union when ANY
/// member accepts; an inferred union is bounded by a declared type only
/// when ALL members accept.
fn compatible_expanded(
    module: &Module,
    inferred: &IrType,
    declared: &IrType,
    depth: usize,
) -> bool {
    if depth > 32 {
        return true;
    }
    if is_shallow_wild(inferred) || is_shallow_wild(declared) {
        return true;
    }
    let expand = |id: &crate::TypeId| module.types.get(id.0 as usize).cloned();
    // Inferred unions distribute: every member must fit the declaration.
    if let IrType::Union(members) = inferred {
        return members.iter().all(|m| match expand(m) {
            Some(mt) => compatible_expanded(module, &mt, declared, depth + 1),
            None => true,
        });
    }
    // Declared unions accept member-wise: any fitting member suffices.
    if let IrType::Union(members) = declared {
        return members.iter().any(|m| match expand(m) {
            Some(mt) => compatible_expanded(module, inferred, &mt, depth + 1),
            None => true,
        });
    }
    match (inferred, declared) {
        (IrType::Array(a), IrType::Array(b)) => both_accept(module, a, b, depth),
        (IrType::Dict(a1, b1), IrType::Dict(a2, b2)) => {
            both_accept(module, a1, a2, depth) && both_accept(module, b1, b2, depth)
        }
        (IrType::Option(a), IrType::Option(b)) | (IrType::Range(a), IrType::Range(b)) => {
            both_accept(module, a, b, depth)
        }
        (IrType::Result(a1, b1), IrType::Result(a2, b2)) => {
            both_accept(module, a1, a2, depth) && both_accept(module, b1, b2, depth)
        }
        (IrType::Tuple(a), IrType::Tuple(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| both_accept(module, x, y, depth))
        }
        _ => inferred == declared,
    }
}

fn both_accept(module: &Module, a: &crate::TypeId, b: &crate::TypeId, depth: usize) -> bool {
    match (
        module.types.get(a.0 as usize).cloned(),
        module.types.get(b.0 as usize).cloned(),
    ) {
        (Some(x), Some(y)) => compatible_expanded(module, &x, &y, depth + 1),
        // Dangling ids are table corruption (rejected by the integrity
        // check); accept here so one error surfaces, not two.
        _ => true,
    }
}

/// Compatibility over abstract values.
fn compatible_tv(module: &Module, inferred: &TV, declared: &TV) -> bool {
    match (inferred, declared) {
        (TV::U, _) | (_, TV::U) => true,
        (TV::T(a), TV::T(b)) => compatible_expanded(module, a, b, 0),
    }
}

/// Structural join for control-flow merges: equal stays, else Unknown.
/// Never rejects; precision loss is always sound downstream.
fn join_tv(a: TV, b: TV) -> TV {
    if a == b {
        a
    } else {
        TV::U
    }
}

/// Join whole states (equal lengths guaranteed by depth agreement).
fn join_state(a: Vec<TV>, b: &[TV]) -> Vec<TV> {
    a.into_iter()
        .zip(b.iter())
        .map(|(x, y)| join_tv(x, y.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Transfer functions + inference walk.
// ---------------------------------------------------------------------------

use crate::op::{BinOp, UnOp};

/// Primitive ids preseeded per function (missing → Unknown fallback).
#[derive(Clone, Copy, Default)]
struct PrimIds {
    unit: Option<crate::TypeId>,
    bool: Option<crate::TypeId>,
    int: Option<crate::TypeId>,
    float: Option<crate::TypeId>,
    str: Option<crate::TypeId>,
}

fn prim_ids(module: &Module) -> PrimIds {
    let mut out = PrimIds::default();
    for (i, t) in module.types.iter().enumerate() {
        let id = crate::TypeId(i as u32);
        match t {
            IrType::Unit => out.unit = out.unit.or(Some(id)),
            IrType::Bool => out.bool = out.bool.or(Some(id)),
            IrType::Int => out.int = out.int.or(Some(id)),
            IrType::Float => out.float = out.float.or(Some(id)),
            IrType::Str => out.str = out.str.or(Some(id)),
            _ => {}
        }
    }
    out
}

fn tv_of_const(
    module: &Module,
    consts: &[Const],
    id: crate::ConstId,
    memo: &mut Vec<Option<TV>>,
) -> TV {
    let idx = id.0 as usize;
    if let Some(hit) = memo.get(idx).and_then(|x| x.clone()) {
        return hit;
    }
    // Pool order is verified before functions run (nested precede
    // users), so recursion strictly descends and always terminates.
    let tv = match consts.get(idx) {
        None => TV::U,
        Some(Const::Unit) => scan_prim(module, &IrType::Unit),
        Some(Const::Bool(_)) => scan_prim(module, &IrType::Bool),
        Some(Const::Int(_)) => scan_prim(module, &IrType::Int),
        Some(Const::Float(_)) => scan_prim(module, &IrType::Float),
        Some(Const::Str(_)) => scan_prim(module, &IrType::Str),
        Some(Const::Array(items)) => {
            let mut elem: Option<TV> = None;
            for item in items {
                let t = tv_of_const(module, consts, *item, memo);
                elem = Some(match elem {
                    None => t,
                    Some(e) => join_tv(e, t),
                });
            }
            match elem {
                Some(TV::T(t)) => match array_of_id(module, &t) {
                    // Rebuild Array(elem-id): only exact-id elements
                    // qualify; anything else stays Unknown (sound: the
                    // weak rule absorbs).
                    Some(id) => TV::of_id(module, id),
                    None => TV::U,
                },
                _ => TV::U,
            }
        }
        // Dict/Option/Result consts stay Unknown: sound (weak rule
        // absorbs), precision is future work.
        Some(Const::Dict(_)) | Some(Const::Option(_)) | Some(Const::Result { .. }) => TV::U,
    };
    if memo.len() <= idx {
        memo.resize(idx + 1, None);
    }
    memo[idx] = Some(tv.clone());
    tv
}

/// Scan the type table for a structurally equal entry.
fn scan_prim(module: &Module, want: &IrType) -> TV {
    match module.types.iter().position(|t| t == want) {
        Some(i) => TV::of_id(module, crate::TypeId(i as u32)),
        None => TV::U,
    }
}

/// Table id of `Array(elem)` when present (structural scan).
fn array_of_id(module: &Module, elem: &IrType) -> Option<crate::TypeId> {
    for (i, t) in module.types.iter().enumerate() {
        if let IrType::Array(inner) = t {
            if let Some(et) = module.types.get(inner.0 as usize) {
                if et == elem {
                    return Some(crate::TypeId(i as u32));
                }
            }
        }
    }
    None
}

/// `BinOp` result over abstract operands. Mirrors
/// `runtime::ops::eval_binary` (+ the `Int*` fallback arms, which run the
/// same function on non-int inputs): exact only where the runtime result
/// is determined for the given operand types, `Unknown` elsewhere.
/// `Unknown` is always sound (it over-approximates every value).
fn binop_tv(module: &Module, prims: PrimIds, op: BinOp, l: &TV, r: &TV) -> TV {
    let t = |id: Option<crate::TypeId>| id.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
    let (li, ri) = match (l, r) {
        (TV::T(a), TV::T(b)) => (a, b),
        // Equality never errors, on any pair — even untyped ones.
        _ => {
            return match op {
                BinOp::Eq | BinOp::Ne => t(prims.bool),
                _ => TV::U,
            };
        }
    };
    use IrType as T;
    match op {
        BinOp::Eq | BinOp::Ne => t(prims.bool),
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
            match (li, ri) {
                (T::Int, T::Int) => t(prims.int),
                (T::Float, T::Float) | (T::Int, T::Float) | (T::Float, T::Int) => t(prims.float),
                (T::Str, T::Str) if matches!(op, BinOp::Add) => t(prims.str),
                _ => TV::U,
            }
        }
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => match (li, ri) {
            (T::Int, T::Int)
            | (T::Float, T::Float)
            | (T::Int, T::Float)
            | (T::Float, T::Int)
            | (T::Str, T::Str) => t(prims.bool),
            _ => TV::U,
        },
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => match (li, ri) {
            (T::Int, T::Int) => t(prims.int),
            _ => TV::U,
        },
        // `&&` / `||` short-circuit in eval (never reach the binary
        // helper); Elvis is its own op. Unknown either way.
        BinOp::And | BinOp::Or | BinOp::Elvis => TV::U,
    }
}

/// `UnOp` result (mirrors `runtime::ops::eval_unary`).
fn unop_tv(module: &Module, prims: PrimIds, op: UnOp, v: &TV) -> TV {
    let t = |id: Option<crate::TypeId>| id.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
    let inner = match v {
        TV::T(x) => x,
        TV::U => {
            return if matches!(op, UnOp::Pos) {
                v.clone()
            } else {
                TV::U
            }
        }
    };
    use IrType as T;
    match op {
        UnOp::Pos => v.clone(),
        UnOp::Neg => match inner {
            T::Int => t(prims.int),
            T::Float => t(prims.float),
            _ => TV::U,
        },
        UnOp::Not => match inner {
            T::Bool => t(prims.bool),
            _ => TV::U,
        },
        UnOp::BitNot => match inner {
            T::Int => t(prims.int),
            _ => TV::U,
        },
    }
}

/// Take-sequence middle store: `(LoadSlot s, PushConst unit, StoreSlot s)`
/// is the desugared take/restore dance (VecPush homes, TakeSlot). The
/// intermediate `Unit` never escapes (no loads can sit between adjacent
/// ops, and jump targets never land inside an expansion), so the store
/// is exempt from the declaration check. The restore store is checked
/// normally.
fn is_take_store(code: &[Op], consts: &[Const], pc: usize, s: u16) -> bool {
    if pc < 2 {
        return false;
    }
    matches!(&code[pc - 2], Op::LoadSlot(t) if *t == s)
        && matches!(&code[pc - 1], Op::PushConst(c) if matches!(consts.get(c.0 as usize), Some(Const::Unit)))
        && matches!(&code[pc], Op::StoreSlot(t) if *t == s)
}

/// Iterable element tags for `ForNext` (mirrors the runtime's four
/// iterable kinds; anything else cannot iterate, so `Unknown` there is
/// only reachable on corrupt input).
fn iter_elem_tv(module: &Module, prims: PrimIds, it: &TV, nvars: usize) -> Vec<TV> {
    let t = |id: Option<crate::TypeId>| id.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
    let inner = match it {
        TV::T(x) => x,
        TV::U => return vec![TV::U; nvars],
    };
    match inner {
        IrType::Range(_) => vec![t(prims.int); nvars],
        IrType::Array(e) => vec![TV::of_id(module, *e); nvars],
        IrType::Dict(k, v) => match nvars {
            1 => vec![TV::of_id(module, *k)],
            2 => vec![TV::of_id(module, *k), TV::of_id(module, *v)],
            _ => vec![TV::U; nvars],
        },
        IrType::Bytes => vec![t(prims.int); nvars],
        _ => vec![TV::U; nvars],
    }
}

/// Index result over an abstract object (mirrors `runtime::ops::get_index`:
/// exact element/field types where determined, `Unknown` elsewhere).
fn index_tv(module: &Module, prims: PrimIds, ob: &TV) -> TV {
    let t = |id: Option<crate::TypeId>| id.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
    match ob {
        TV::T(IrType::Array(e)) => TV::of_id(module, *e),
        TV::T(IrType::Tuple(members)) => {
            let mut out: Option<TV> = None;
            for m in members {
                let tv = TV::of_id(module, *m);
                out = Some(match out {
                    None => tv,
                    Some(e) => join_tv(e, tv),
                });
            }
            out.unwrap_or(TV::U)
        }
        TV::T(IrType::Bytes) => t(prims.int),
        TV::T(IrType::Dict(_, v)) => TV::of_id(module, *v),
        TV::T(IrType::Str) => t(prims.str),
        _ => TV::U,
    }
}

/// True when `t` is the exit edge of a loop op (exhaustion path, which
/// truncates the stack at runtime).
fn is_loop_exit(op: &Op, t: usize) -> bool {
    match op {
        Op::ForNext { exit, .. } | Op::WhileCond { exit } => *exit as usize == t,
        _ => false,
    }
}

/// Short display for abstract values in diagnostics: resolves table ids
/// to readable names (`[int]`, not `Array(TypeId(1))`).
fn tv_name(module: &Module, tv: &TV) -> String {
    match tv {
        TV::U => "unknown".to_string(),
        TV::T(t) => crate::dis::type_text(module, t),
    }
}

// ---------------------------------------------------------------------------
// Inference walk.
// ---------------------------------------------------------------------------

/// Infer + check one function body. Runs after the depth checks, so
/// depths agree at every join; every access below is bounds-checked
/// (corrupt input errors, never panics).
fn infer_func(
    module: &Module,
    func_idx: usize,
    funcs_by_name: &std::collections::HashMap<String, usize>,
    prims: PrimIds,
) -> Result<(), IrError> {
    let func: &FuncDef = &module.funcs[func_idx];
    let n = func.code.len();
    if n == 0 {
        return Ok(());
    }
    let span_at = |pc: usize| func.spans.get(pc).copied().unwrap_or(Span::new(0, 0));
    let declared = |s: u16| -> Result<TV, IrError> {
        match func.locals.get(s as usize) {
            Some(id) => Ok(TV::of_id(module, *id)),
            None => Err(IrError::spanned(
                format!("slot {s} out of range for locals table"),
                span_at(0),
            )),
        }
    };
    let check_store = |inferred: &TV, s: u16, span: Span| -> Result<(), IrError> {
        let d = declared(s)?;
        if !compatible_tv(module, inferred, &d) {
            return Err(IrError::spanned(
                format!(
                    "type mismatch: slot {s} stores {} but declares {}",
                    tv_name(module, inferred),
                    tv_name(module, &d)
                ),
                span,
            ));
        }
        Ok(())
    };
    let check_exact_bool = |v: TV, what: &str, span: Span| -> Result<(), IrError> {
        if let TV::T(t) = &v {
            if *t != IrType::Bool {
                return Err(IrError::spanned(format!("type mismatch: {what}"), span));
            }
        }
        Ok(())
    };
    let check_exact_int = |v: TV, what: &str, span: Span| -> Result<(), IrError> {
        if let TV::T(t) = &v {
            if *t != IrType::Int {
                return Err(IrError::spanned(format!("type mismatch: {what}"), span));
            }
        }
        Ok(())
    };
    // Pop helper with underflow errors (never panics on corrupt input).
    let pop = |st: &mut Vec<TV>, pc: usize| -> Result<TV, IrError> {
        st.pop()
            .ok_or_else(|| IrError::spanned(format!("stack underflow at {pc}"), span_at(pc)))
    };
    // Loop-exit truncation depths (mirrors the depth simulation's
    // split rule: `for` exits carry setup length - 1, `while` exits
    // keep the historical + 1).
    let mut loop_exits: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut states: Vec<Option<Vec<TV>>> = vec![None; n];
    let mut work = vec![0usize];
    states[0] = Some(Vec::new());
    while let Some(pc) = work.pop() {
        let mut st = match states[pc].clone() {
            Some(st) => st,
            None => continue,
        };
        let op = &func.code[pc];
        let span = span_at(pc);
        if let Op::ForSetup { exit, .. } = op {
            // Never underflow on hostile input: the verifier rejects,
            // never panics (spec §8).
            loop_exits.insert(*exit as usize, st.len().saturating_sub(1));
        } else if let Op::WhileSetup { exit, .. } = op {
            loop_exits.insert(*exit as usize, st.len() + 1);
        }
        match op {
            Op::PushConst(c) => {
                let mut memo = Vec::new();
                let tv = tv_of_const(module, &module.consts, *c, &mut memo);
                st.push(tv);
            }
            Op::Pop => {
                let _ = st.pop();
            }
            Op::Swap => {
                if st.len() < 2 {
                    return Err(IrError::spanned("swap needs two stack values", span));
                }
                let m = st.len();
                st.swap(m - 1, m - 2);
            }
            Op::PopN(k) => {
                // Mirror the runtime exactly (checked, never wrapping).
                let k = *k as usize;
                if st.len() < k + 1 {
                    return Err(IrError::spanned("stack underflow", span));
                }
                let top = st.pop().unwrap();
                st.truncate(st.len() - k);
                st.push(top);
            }
            Op::Truthy => {
                pop(&mut st, pc)?;
                st.push(prims.bool.map(|i| TV::of_id(module, i)).unwrap_or(TV::U));
            }
            Op::LoadVar(_) | Op::LoadPath(_) | Op::TakeVar(_) => st.push(TV::U),
            // DefineVar pops its value and pushes it straight back.
            Op::DefineVar(_) => {}
            Op::StoreVar(_) | Op::StorePath(_) => {
                pop(&mut st, pc)?;
            }
            Op::LoadSlot(s) => {
                let d = declared(*s)?;
                st.push(d);
            }
            Op::StoreSlot(s) => {
                let v = pop(&mut st, pc)?;
                if !is_take_store(&func.code, &module.consts, pc, *s) {
                    check_store(&v, *s, span)?;
                }
            }
            Op::VecPushField { .. } | Op::VecPushMethod { .. } => {
                pop(&mut st, pc)?;
            }
            Op::IntAdd => {
                let r = pop(&mut st, pc)?;
                let l = pop(&mut st, pc)?;
                st.push(binop_tv(module, prims, BinOp::Add, &l, &r));
            }
            Op::IntSub => {
                let r = pop(&mut st, pc)?;
                let l = pop(&mut st, pc)?;
                st.push(binop_tv(module, prims, BinOp::Sub, &l, &r));
            }
            Op::IntMul => {
                let r = pop(&mut st, pc)?;
                let l = pop(&mut st, pc)?;
                st.push(binop_tv(module, prims, BinOp::Mul, &l, &r));
            }
            Op::IntDiv => {
                let r = pop(&mut st, pc)?;
                let l = pop(&mut st, pc)?;
                st.push(binop_tv(module, prims, BinOp::Div, &l, &r));
            }
            Op::IntRem => {
                let r = pop(&mut st, pc)?;
                let l = pop(&mut st, pc)?;
                st.push(binop_tv(module, prims, BinOp::Rem, &l, &r));
            }
            Op::IntNeg => {
                let v = pop(&mut st, pc)?;
                st.push(unop_tv(module, prims, UnOp::Neg, &v));
            }
            Op::BinOp(b) => {
                let r = pop(&mut st, pc)?;
                let l = pop(&mut st, pc)?;
                st.push(binop_tv(module, prims, *b, &l, &r));
            }
            Op::UnOp(u) => {
                let v = pop(&mut st, pc)?;
                st.push(unop_tv(module, prims, *u, &v));
            }
            Op::Jump(_) | Op::Break | Op::Continue => {}
            Op::Return => {
                let v = pop(&mut st, pc)?;
                let ret = TV::of_id(module, func.sig.ret);
                if !compatible_tv(module, &v, &ret) {
                    return Err(IrError::spanned(
                        "type mismatch: return type mismatch".to_string(),
                        span,
                    ));
                }
            }
            Op::JumpIfFalse(_) | Op::JumpIfTrue(_) => {
                pop(&mut st, pc)?;
            }
            Op::JumpIfFalseBool(_) => {
                let v = pop(&mut st, pc)?;
                check_exact_bool(v, "non-bool `if` condition", span)?;
            }
            Op::ForSetup { num_vars, .. } => {
                let it = pop(&mut st, pc)?;
                if let TV::T(t) = &it {
                    match t {
                        IrType::Range(_)
                        | IrType::Array(_)
                        | IrType::Bytes
                        | IrType::Dict(_, _) => {}
                        _ => {
                            return Err(IrError::spanned(
                                "type mismatch: cannot iterate non-iterable".to_string(),
                                span,
                            ));
                        }
                    }
                }
                let int = prims.int.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
                let unit = prims.unit.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
                st.push(it);
                st.push(int);
                for _ in 0..*num_vars {
                    st.push(unit.clone());
                }
            }
            Op::ForNext { vars, .. } => {
                // Pop the previous iteration's vars, then the index; the
                // iterable stays on top for dispatch.
                if st.len() < vars.len() + 2 {
                    return Err(IrError::spanned("stack underflow", span));
                }
                for _ in 0..vars.len() {
                    st.pop();
                }
                let idx = st.pop().unwrap();
                check_exact_int(idx, "for index must be int", span)?;
                let it = st.last().cloned().unwrap_or(TV::U);
                let elems = iter_elem_tv(module, prims, &it, vars.len());
                let int = prims.int.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
                st.push(int);
                st.extend(elems);
            }
            Op::WhileSetup { .. } => {}
            Op::WhileCond { .. } => {
                let v = pop(&mut st, pc)?;
                check_exact_bool(v, "`while` condition must be a bool", span)?;
            }
            // Loop-result slots are flow-internal (uninitialized
            // placeholders, multi-lifetime positions): no declaration
            // check. Emitters guard every unboxed write instead.
            Op::SetLoopResult => {
                pop(&mut st, pc)?;
            }
            Op::Safepoint => {}
            Op::MakeArray(k) => {
                let k = *k as usize;
                if st.len() < k {
                    return Err(IrError::spanned("stack underflow", span));
                }
                let mut elem: Option<TV> = None;
                for _ in 0..k {
                    let t = st.pop().unwrap();
                    elem = Some(match elem {
                        None => t,
                        Some(e) => join_tv(e, t),
                    });
                }
                match elem {
                    Some(TV::T(t)) => {
                        let arr = IrType::Array(
                            module
                                .types
                                .iter()
                                .position(|x| x == &t)
                                .map(|i| crate::TypeId(i as u32))
                                .unwrap_or(crate::TypeId(u32::MAX)),
                        );
                        st.push(scan_prim(module, &arr));
                    }
                    _ => st.push(TV::U),
                }
            }
            Op::UnpackTuple(k) => {
                let v = pop(&mut st, pc)?;
                let k = *k as usize;
                match v {
                    TV::T(IrType::Array(e)) => {
                        let t = TV::of_id(module, e);
                        for _ in 0..k {
                            st.push(t.clone());
                        }
                    }
                    _ => {
                        for _ in 0..k {
                            st.push(TV::U);
                        }
                    }
                }
            }
            Op::ArrayPush => {
                let v = pop(&mut st, pc)?;
                let a = pop(&mut st, pc)?;
                match (a, v) {
                    (TV::T(IrType::Array(e)), TV::T(t)) => {
                        let keep = module
                            .types
                            .get(e.0 as usize)
                            .map(|et| et == &t)
                            .unwrap_or(false);
                        if keep {
                            st.push(TV::T(IrType::Array(e)));
                        } else {
                            st.push(TV::U);
                        }
                    }
                    _ => st.push(TV::U),
                }
            }
            Op::MakeDict(k) => {
                let k = *k as usize;
                if st.len() < 2 * k {
                    return Err(IrError::spanned("stack underflow", span));
                }
                let mut kt: Option<TV> = None;
                let mut vt: Option<TV> = None;
                for _ in 0..k {
                    let v = st.pop().unwrap();
                    let kk = st.pop().unwrap();
                    kt = Some(match kt {
                        None => kk,
                        Some(e) => join_tv(e, kk),
                    });
                    vt = Some(match vt {
                        None => v,
                        Some(e) => join_tv(e, v),
                    });
                }
                match (kt, vt) {
                    (Some(TV::T(a)), Some(TV::T(b))) => {
                        let ai = module.types.iter().position(|x| x == &a);
                        let bi = module.types.iter().position(|x| x == &b);
                        match (ai, bi) {
                            (Some(x), Some(y)) => st.push(scan_prim(
                                module,
                                &IrType::Dict(crate::TypeId(x as u32), crate::TypeId(y as u32)),
                            )),
                            _ => st.push(TV::U),
                        }
                    }
                    _ => st.push(TV::U),
                }
            }
            Op::IndexOp => {
                pop(&mut st, pc)?;
                let ob = pop(&mut st, pc)?;
                st.push(index_tv(module, prims, &ob));
            }
            Op::StoreIndexOp | Op::CompoundIndexOp { .. } => {
                pop(&mut st, pc)?;
                pop(&mut st, pc)?;
                let ob = pop(&mut st, pc)?;
                st.push(ob);
            }
            Op::SliceOp => {
                pop(&mut st, pc)?;
                pop(&mut st, pc)?;
                let ob = pop(&mut st, pc)?;
                match &ob {
                    TV::T(IrType::Array(_)) | TV::T(IrType::Bytes) | TV::T(IrType::Str) => {
                        st.push(ob)
                    }
                    _ => st.push(TV::U),
                }
            }
            Op::MakeRange => {
                pop(&mut st, pc)?;
                pop(&mut st, pc)?;
                // Bounds trap unless both ints; ranges are Range(Int).
                let range = prims.int.map(IrType::Range).unwrap_or(IrType::Unknown);
                st.push(scan_prim(module, &range));
            }
            Op::MakeStruct { name, fields } => {
                if st.len() < fields.len() {
                    return Err(IrError::spanned("stack underflow", span));
                }
                let mut args = Vec::with_capacity(fields.len());
                for _ in fields {
                    match st.pop().unwrap() {
                        TV::T(t) => args.push(t),
                        TV::U => args.push(IrType::Unknown),
                    }
                }
                args.reverse();
                let mut ids = Vec::with_capacity(args.len());
                let mut ok = true;
                for a in &args {
                    match module.types.iter().position(|x| x == a) {
                        Some(i) => ids.push(crate::TypeId(i as u32)),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    st.push(scan_prim(module, &IrType::Struct(*name, ids)));
                } else {
                    st.push(TV::U);
                }
            }
            Op::GetField(_) | Op::GetFieldIdx(_) => {
                pop(&mut st, pc)?;
                st.push(TV::U);
            }
            Op::SetField(_) | Op::SetFieldIdx(_) | Op::CompoundFieldOp { .. } => {
                pop(&mut st, pc)?;
                let ob = pop(&mut st, pc)?;
                st.push(ob);
            }
            Op::RegisterStruct { .. } | Op::RegisterEnum { .. } => {
                st.push(prims.unit.map(|i| TV::of_id(module, i)).unwrap_or(TV::U));
            }
            Op::MakeEnum { argc, .. } => {
                for _ in 0..*argc {
                    pop(&mut st, pc)?;
                }
                st.push(TV::U);
            }
            Op::MakeVariant { has_arg, .. } => {
                if *has_arg {
                    pop(&mut st, pc)?;
                }
                st.push(TV::U);
            }
            Op::MakeClosure { .. } | Op::MakeFunc { .. } | Op::SpawnClosure { .. } => {
                st.push(TV::U);
            }
            // Bindings go to the environment; scrutinee popped. Miss
            // edges push the scrutinee back (mirrored below).
            Op::MatchArm { .. } | Op::IfLetMatch { .. } => {
                pop(&mut st, pc)?;
            }
            Op::MatchGuard { .. } => {
                // Non-bool guards miss (no trap), so no check.
                pop(&mut st, pc)?;
            }
            Op::MatchError => {}
            Op::TryOp => {
                let v = pop(&mut st, pc)?;
                match v {
                    TV::T(IrType::Option(i)) | TV::T(IrType::Result(i, _)) => {
                        st.push(TV::of_id(module, i))
                    }
                    _ => st.push(TV::U),
                }
            }
            Op::Elvis => {
                let v = pop(&mut st, pc)?;
                let b = prims.bool.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
                let u = prims.unit.map(|i| TV::of_id(module, i)).unwrap_or(TV::U);
                match v {
                    TV::T(IrType::Option(i)) | TV::T(IrType::Result(i, _)) => {
                        let inner = TV::of_id(module, i);
                        st.push(b);
                        st.push(join_tv(inner, u));
                    }
                    TV::U => {
                        st.push(b);
                        st.push(TV::U);
                    }
                    other => {
                        st.push(b);
                        st.push(other);
                    }
                }
            }
            Op::ElvisResult => {
                let r = pop(&mut st, pc)?;
                let i = pop(&mut st, pc)?;
                pop(&mut st, pc)?;
                st.push(join_tv(i, r));
            }
            Op::Call { argc } | Op::CallMethod { argc, .. } => {
                // Dynamic callee (or slot receiver): result Unknown.
                // Both pop argc+1 (callee/receiver + args), push one.
                let argc = *argc as usize;
                if st.len() < argc + 1 {
                    return Err(IrError::spanned("stack underflow", span));
                }
                for _ in 0..argc + 1 {
                    st.pop();
                }
                st.push(TV::U);
            }
            Op::CallNative { argc, .. } => {
                let argc = *argc as usize;
                if st.len() < argc {
                    return Err(IrError::spanned("stack underflow", span));
                }
                for _ in 0..argc {
                    st.pop();
                }
                st.push(TV::U);
            }
            Op::CallPath { parts, argc, .. } => {
                let argc = *argc as usize;
                if st.len() < argc {
                    return Err(IrError::spanned("stack underflow", span));
                }
                let mut args = Vec::with_capacity(argc);
                for _ in 0..argc {
                    args.push(st.pop().unwrap());
                }
                args.reverse();
                let name = parts
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
                match funcs_by_name.get(&name) {
                    Some(tgt) => {
                        let callee = &module.funcs[*tgt];
                        if argc > callee.sig.params.len() {
                            return Err(IrError::spanned(
                                format!(
                                    "type mismatch: callpath {name} takes at most {} args, got {argc}",
                                    callee.sig.params.len()
                                ),
                                span,
                            ));
                        }
                        for (a, p) in args.iter().zip(callee.sig.params.iter()) {
                            if !compatible_tv(module, a, &TV::of_id(module, *p)) {
                                return Err(IrError::spanned(
                                    format!(
                                        "type mismatch: callpath {name} argument type mismatch"
                                    ),
                                    span,
                                ));
                            }
                        }
                        st.push(TV::of_id(module, callee.sig.ret));
                    }
                    // Unknown callee (natives, aliases, dynamic shapes):
                    // result Unknown, no checks. Weak rule absorbs.
                    None => st.push(TV::U),
                }
            }
            Op::Concat(k) => {
                let k = *k as usize;
                if st.len() < k {
                    return Err(IrError::spanned("stack underflow", span));
                }
                for _ in 0..k {
                    st.pop();
                }
                st.push(prims.str.map(|i| TV::of_id(module, i)).unwrap_or(TV::U));
            }
            Op::FormatValue => {
                // Pop format spec and value, push the formatted string.
                if st.len() < 2 {
                    return Err(IrError::spanned("stack underflow", span));
                }
                st.pop();
                st.pop();
                st.push(prims.str.map(|i| TV::of_id(module, i)).unwrap_or(TV::U));
            }
            Op::DbQuery { nparams } => {
                // Exact permutation (mirror runtime): pop n+1, push
                // template then params in order.
                let k = *nparams as usize + 1;
                if st.len() < k {
                    return Err(IrError::spanned("stack underflow", span));
                }
                let mut tmp = Vec::with_capacity(k);
                for _ in 0..k {
                    tmp.push(st.pop().unwrap());
                }
                let template = tmp.pop().unwrap();
                tmp.reverse();
                st.push(template);
                st.extend(tmp);
            }
            Op::EnterScope | Op::ExitScope => {}
            Op::DeferRecord => {
                pop(&mut st, pc)?;
            }
        }
        // Successor edges (mirror the depth simulation exactly).
        let falls = !matches!(op, Op::Jump(_) | Op::Return | Op::Break | Op::Continue);
        let miss = match op {
            Op::MatchArm {
                next,
                restore: true,
                ..
            } => Some(*next as usize),
            Op::IfLetMatch { els, .. } => Some(*els as usize),
            _ => None,
        };
        let mut succs: Vec<(usize, Vec<TV>)> = Vec::new();
        if falls && pc + 1 < n {
            succs.push((pc + 1, st.clone()));
        }
        for t in jump_targets(op) {
            let t = t as usize;
            if t > n {
                return Err(IrError::spanned("jump target out of range", span));
            }
            if t == n {
                // Terminal edge: the halting value must satisfy the
                // declared return type.
                if let Some(top) = st.last() {
                    let ret = TV::of_id(module, func.sig.ret);
                    if !compatible_tv(module, top, &ret) {
                        return Err(IrError::spanned(
                            "type mismatch: return type mismatch".to_string(),
                            span,
                        ));
                    }
                }
                continue;
            }
            if Some(t) == miss {
                // Pushback edge: scrutinee restored; carry the entry state.
                let back = match states[pc].clone() {
                    Some(b) => b,
                    None => continue,
                };
                succs.push((t, back));
                continue;
            }
            if is_loop_exit(op, t) {
                // Loop exits truncate (mirror depth rule): a `for` exit
                // keeps setup length - 1 entries, a `while` exit the
                // historical + 1; the top (loop result) is admitted
                // Unknown, which downstream checks absorb.
                match loop_exits.get(&t) {
                    Some(want) => {
                        let mut trunc = st.clone();
                        trunc.truncate(*want);
                        if let Some(top) = trunc.last_mut() {
                            *top = TV::U;
                        }
                        succs.push((t, trunc));
                    }
                    // Setup not yet visited (unreachable on compiler
                    // output: setups dominate their exits): carry through;
                    // the join below keeps it sound.
                    None => succs.push((t, st.clone())),
                }
                continue;
            }
            succs.push((t, st.clone()));
        }
        // Implicit falloff past the last op returns the top value.
        if pc + 1 == n && falls {
            if let Some(top) = st.last() {
                let ret = TV::of_id(module, func.sig.ret);
                if !compatible_tv(module, top, &ret) {
                    return Err(IrError::spanned(
                        "type mismatch: return type mismatch".to_string(),
                        span,
                    ));
                }
            }
        }
        for (s, state) in succs {
            match &states[s] {
                None => {
                    states[s] = Some(state);
                    work.push(s);
                }
                Some(cur) => {
                    if cur.len() != state.len() {
                        return Err(IrError::spanned("join state length mismatch", span_at(s)));
                    }
                    let merged = join_state(cur.clone(), &state);
                    if &merged != cur {
                        states[s] = Some(merged);
                        work.push(s);
                    }
                }
            }
        }
    }
    Ok(())
}
