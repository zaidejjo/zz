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

/// Verify one function body: ids, jumps, and join-depth agreement.
/// Returns the observed maximum stack depth.
fn verify_func(module: &Module, func_idx: usize) -> Result<u32, IrError> {
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
    // Loop-exit pcs mapped to their expected depth. Every loop exit
    // truncates the stack to (setup depth + 1) at runtime, so all edges
    // into an exit must carry exactly that — never the local `after`.
    let mut loop_exits: std::collections::HashMap<usize, i64> = std::collections::HashMap::new();
    while let Some(pc) = work.pop() {
        let depth = depth_at[pc].expect("verifier worklist");
        let op = &code[pc];
        // `Swap` needs two live values; the net effect (0) cannot show it.
        if matches!(op, Op::Swap) && depth < 2 {
            return Err(IrError::spanned("swap needs two stack values", span_at(pc)));
        }
        if let Op::ForSetup { exit, .. } | Op::WhileSetup { exit, .. } = op {
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
        verify_func(module, idx)?;
    }
    Ok(())
}
