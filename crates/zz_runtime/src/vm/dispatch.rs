//! Direct-threaded dispatch infrastructure for the VM bytecode interpreter.
//!
//! Instead of a single giant `match` on `Op` in the dispatch loop, each
//! opcode handler is extracted into its own `#[inline(always)]` function and
//! looked up through a function-pointer dispatch table indexed by a dense
//! opcode tag. This gives the CPU's branch predictor a stable indirect-branch
//! target per opcode and lets LLVM optimise each handler in isolation.
//!
//! Hot opcodes (the ones that dominate tight-loop execution) have dedicated
//! handler slots; everything else falls through to the general `match` in
//! `run_loop_impl`.

use std::ops::ControlFlow;

use zz_frontend::ast::BinOp;
use zz_frontend::span::Span;

use crate::eval::EvalError;
use crate::eval::Interp;
use crate::runtime::ops::{eval_binary, eval_int_binary};
use crate::runtime::Flow;
use crate::value::Value;
use crate::vm::runtime::Vm;
use crate::vm::Op;

/// Result of executing one opcode handler.
///
/// `Continue(())` means execution proceeds to the next opcode.
/// `Break(flow)` means the handler produced a terminal flow (`Return`,
/// `Yield`, `Break`, `Continue`, or an error) and the dispatch loop should
/// unwind.
type OpResult = ControlFlow<Flow, ()>;

/// Signature of a single-opcode handler.
///
/// Handlers receive mutable access to the VM, the interpreter, the
/// instruction pointer (which they advance as needed), the current chunk's
/// code and constant pools, and the `Op` itself (for pattern-matching its
/// payload).
type OpHandler = fn(
    vm: &mut Vm,
    interp: &mut Interp,
    ip: &mut usize,
    code: &[Op],
    constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError>;

/// Dense opcode tag for direct-threaded dispatch.
///
/// Returns `Some(tag)` for hot opcodes that have a dedicated dispatch-table
/// slot, `None` for everything else (which falls through to the general
/// `match`).
#[inline(always)]
pub fn hot_tag(op: &Op) -> Option<u8> {
    match op {
        // ---- most frequent in tight loops ----
        Op::SlotAddInt { .. } => Some(0),
        Op::SlotInc { .. } => Some(1),
        Op::SlotBinaryInt { .. } => Some(2),
        Op::LoadSlot { .. } => Some(3),
        Op::StoreSlot { .. } => Some(4),
        Op::ForNext { .. } => Some(5),
        Op::Jump { .. } => Some(6),
        Op::JumpIfFalse { .. } => Some(7),
        Op::CallNative { .. } => Some(8),
        Op::IndexDictInt { .. } => Some(9),
        // ---- typed integer arithmetic ----
        Op::IntAdd { .. } => Some(10),
        Op::IntSub { .. } => Some(11),
        Op::IntMul { .. } => Some(12),
        // ---- slot arithmetic fast paths ----
        Op::SlotAddIntImm { .. } => Some(13),
        Op::SlotSubIntImm { .. } => Some(14),
        Op::SlotMulIntImm { .. } => Some(15),
        _ => None,
    }
}

/// Number of entries in the dispatch table.
const DISPATCH_TABLE_SIZE: usize = 16;

/// Function-pointer dispatch table for hot opcodes.
///
/// Indexed by [`hot_tag`]. Entries for tags without a dedicated handler are
/// `None` and fall through to the general match.
static DISPATCH_TABLE: [Option<OpHandler>; DISPATCH_TABLE_SIZE] = [
    Some(slot_add_int_handler),
    Some(slot_inc_handler),
    Some(slot_binary_int_handler),
    Some(load_slot_handler),
    Some(store_slot_handler),
    None, // tag 5: ForNext — delegates to general match (complex logic)
    Some(jump_handler),
    Some(jump_if_false_handler),
    None, // tag 8: CallNative — delegates to general match (scratch args, arity gate, yield)
    Some(index_dict_int_handler),
    Some(int_add_handler),
    Some(int_sub_handler),
    Some(int_mul_handler),
    Some(slot_add_int_imm_handler),
    Some(slot_sub_int_imm_handler),
    Some(slot_mul_int_imm_handler),
];

/// Execute one opcode through the dispatch table if it has a dedicated
/// handler; return `None` for non-hot opcodes so the caller falls through
/// to the general `match`.
///
/// Returns `Some(Ok(ControlFlow::Continue(())))` to proceed to the next
/// opcode, `Some(Ok(ControlFlow::Break(flow)))` to unwind with a terminal
/// flow, `Some(Err(e))` on runtime error, or `None` when the opcode has no
/// dedicated handler.
#[inline(always)]
pub fn dispatch_op(
    vm: &mut Vm,
    interp: &mut Interp,
    ip: &mut usize,
    code: &[Op],
    constants: &[Value],
    op: &Op,
) -> Option<Result<OpResult, EvalError>> {
    if let Some(tag) = hot_tag(op) {
        if let Some(handler) = DISPATCH_TABLE[tag as usize] {
            return Some(handler(vm, interp, ip, code, constants, op));
        }
    }
    // Non-hot opcodes fall through to the general match in run_loop_impl.
    None
}

// =========================================================================
// Hot-opcode handlers
// =========================================================================

/// `slot[dst] = Int(slot[dst] + slot[src])` — fused in-place integer add.
#[inline(always)]
fn slot_add_int_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let (dst, src) = match op {
        Op::SlotAddInt { dst, src } => (*dst as usize, *src as usize),
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let idx_dst = base + dst;
    let idx_src = base + src;
    match (&vm.stack[idx_dst], &vm.stack[idx_src]) {
        (Value::Int(a), Value::Int(b)) => {
            let v = Value::Int(a.wrapping_add(*b));
            unsafe { super::runtime::overwrite_scalar(&mut vm.stack[idx_dst], v) };
        }
        _ => {
            let (a, b) = (vm.stack[idx_dst].clone(), vm.stack[idx_src].clone());
            let span = Span::default();
            let r = eval_binary(BinOp::Add, a, b, span)?;
            vm.stack[idx_dst] = r;
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `slot[slot] = Int(slot[slot] + 1)` — fused integer increment.
#[inline(always)]
fn slot_inc_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let slot = match op {
        Op::SlotInc { slot } => *slot as usize,
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let idx = base + slot;
    match &vm.stack[idx] {
        Value::Int(a) => {
            let v = Value::Int(a.wrapping_add(1));
            unsafe { super::runtime::overwrite_scalar(&mut vm.stack[idx], v) };
        }
        _ => {
            let v = vm.stack[idx].clone();
            let span = Span::default();
            let r = eval_binary(BinOp::Add, v, Value::Int(1), span)?;
            vm.stack[idx] = r;
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `slot[dst] = Int(slot[lhs] op slot[rhs])` — 3-address integer binary op.
#[inline(always)]
fn slot_binary_int_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let (dst, lhs, rhs, op_binop) = match op {
        Op::SlotBinaryInt { dst, lhs, rhs, op } => {
            (*dst as usize, *lhs as usize, *rhs as usize, *op)
        }
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let idx_d = base + dst;
    let idx_l = base + lhs;
    let idx_r = base + rhs;
    match (&vm.stack[idx_l], &vm.stack[idx_r]) {
        (Value::Int(a), Value::Int(b)) => {
            let v = eval_int_binary(op_binop, *a, *b, Span::default())?;
            super::runtime::write_slot_checked(&mut vm.stack[idx_d], v);
        }
        _ => {
            let (l, r) = (vm.stack[idx_l].clone(), vm.stack[idx_r].clone());
            let span = Span::default();
            let v = eval_binary(op_binop, l, r, span)?;
            super::runtime::write_slot_checked(&mut vm.stack[idx_d], v);
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// Push `stack[base + slot]` onto the stack.
#[inline(always)]
fn load_slot_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let slot = match op {
        Op::LoadSlot(slot) => *slot as usize,
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let v = vm.stack[base + slot].clone();
    vm.stack.push(v);
    Ok(ControlFlow::Continue(()))
}

/// Pop a value and write it to `stack[base + slot]`.
#[inline(always)]
fn store_slot_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let slot = match op {
        Op::StoreSlot(slot) => *slot as usize,
        _ => unreachable!(),
    };
    let v = vm.stack.pop().unwrap();
    let base = vm.current_base();
    vm.stack[base + slot] = v;
    Ok(ControlFlow::Continue(()))
}

/// Unconditional jump to `target`.
#[inline(always)]
fn jump_handler(
    _vm: &mut Vm,
    _interp: &mut Interp,
    ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let target = match op {
        Op::Jump(target) => *target,
        _ => unreachable!(),
    };
    *ip = target;
    Ok(ControlFlow::Continue(()))
}

/// Pop a value; jump if it is falsy.
#[inline(always)]
fn jump_if_false_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let target = match op {
        Op::JumpIfFalse(target) => *target,
        _ => unreachable!(),
    };
    let v = vm.stack.pop().unwrap();
    if !v.is_truthy() {
        *ip = target;
    }
    Ok(ControlFlow::Continue(()))
}

/// Read `dict[key]` where key is an int — fused int-keyed dict read.
#[inline(always)]
fn index_dict_int_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let (dict_slot, key_slot, dst) = match op {
        Op::IndexDictInt { dict, key, dst, .. } => (*dict as usize, *key as usize, *dst as usize),
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let ov = vm.stack[base + dict_slot].clone();
    let iv = vm.stack[base + key_slot].clone();
    match (&ov, &iv) {
        (Value::Dict(dict), key) => {
            // Try the inline cache first.
            let dict_ptr = std::sync::Arc::as_ptr(dict) as usize;
            let key_hash = crate::value::DictKey::of(key)
                .map(|dk| {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    dk.hash(&mut hasher);
                    hasher.finish()
                })
                .unwrap_or(0);
            if let Some(cached) = vm.inline_cache_lookup(dict_ptr, key_hash) {
                vm.stack[base + dst] = cached;
                return Ok(ControlFlow::Continue(()));
            }
            // Cache miss: do the full lookup.
            match dict.get_cloned(key) {
                Some(v) => {
                    vm.inline_cache_store(dict_ptr, key_hash, v.clone());
                    vm.stack[base + dst] = v;
                }
                None => {
                    return Err(vm.error(format!("key `{key}` not found in dict"), Span::default()));
                }
            }
        }
        _ => {
            return Err(vm.error(
                "expected dict for indexed access".to_string(),
                Span::default(),
            ));
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `IntAdd`: pop two ints, push `a + b`.
#[inline(always)]
fn int_add_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let _span = match op {
        Op::IntAdd(span) => *span,
        _ => unreachable!(),
    };
    let b = vm.stack.pop().unwrap();
    let a = vm.stack.pop().unwrap();
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => {
            vm.stack.push(Value::Int(a.wrapping_add(b)));
        }
        _ => {
            return Err(vm.error("arithmetic on non-int value".to_string(), Span::default()));
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `IntSub`: pop two ints, push `a - b`.
#[inline(always)]
fn int_sub_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let _span = match op {
        Op::IntSub(span) => *span,
        _ => unreachable!(),
    };
    let b = vm.stack.pop().unwrap();
    let a = vm.stack.pop().unwrap();
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => {
            vm.stack.push(Value::Int(a.wrapping_sub(b)));
        }
        _ => {
            return Err(vm.error("arithmetic on non-int value".to_string(), Span::default()));
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `IntMul`: pop two ints, push `a * b`.
#[inline(always)]
fn int_mul_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let _span = match op {
        Op::IntMul(span) => *span,
        _ => unreachable!(),
    };
    let b = vm.stack.pop().unwrap();
    let a = vm.stack.pop().unwrap();
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => {
            vm.stack.push(Value::Int(a.wrapping_mul(b)));
        }
        _ => {
            return Err(vm.error("arithmetic on non-int value".to_string(), Span::default()));
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `slot[dst] = Int(slot[dst] + imm)` — fused integer add-immediate.
#[inline(always)]
fn slot_add_int_imm_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let (dst, imm) = match op {
        Op::SlotAddIntImm { dst, imm } => (*dst as usize, *imm),
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let idx = base + dst;
    match &vm.stack[idx] {
        Value::Int(a) => {
            let v = Value::Int(a.wrapping_add(imm));
            unsafe { super::runtime::overwrite_scalar(&mut vm.stack[idx], v) };
        }
        _ => {
            let v = vm.stack[idx].clone();
            let span = Span::default();
            let r = eval_binary(BinOp::Add, v, Value::Int(imm), span)?;
            vm.stack[idx] = r;
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `slot[dst] = Int(slot[dst] - imm)` — fused integer subtract-immediate.
#[inline(always)]
fn slot_sub_int_imm_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let (dst, imm) = match op {
        Op::SlotSubIntImm { dst, imm } => (*dst as usize, *imm),
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let idx = base + dst;
    match &vm.stack[idx] {
        Value::Int(a) => {
            let v = Value::Int(a.wrapping_sub(imm));
            unsafe { super::runtime::overwrite_scalar(&mut vm.stack[idx], v) };
        }
        _ => {
            let v = vm.stack[idx].clone();
            let span = Span::default();
            let r = eval_binary(BinOp::Sub, v, Value::Int(imm), span)?;
            vm.stack[idx] = r;
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// `slot[dst] = Int(slot[dst] * imm)` — fused integer multiply-immediate.
#[inline(always)]
fn slot_mul_int_imm_handler(
    vm: &mut Vm,
    _interp: &mut Interp,
    _ip: &mut usize,
    _code: &[Op],
    _constants: &[Value],
    op: &Op,
) -> Result<OpResult, EvalError> {
    let (dst, imm) = match op {
        Op::SlotMulIntImm { dst, imm } => (*dst as usize, *imm),
        _ => unreachable!(),
    };
    let base = vm.current_base();
    let idx = base + dst;
    match &vm.stack[idx] {
        Value::Int(a) => {
            let v = Value::Int(a.wrapping_mul(imm));
            unsafe { super::runtime::overwrite_scalar(&mut vm.stack[idx], v) };
        }
        _ => {
            let v = vm.stack[idx].clone();
            let span = Span::default();
            let r = eval_binary(BinOp::Mul, v, Value::Int(imm), span)?;
            vm.stack[idx] = r;
        }
    }
    Ok(ControlFlow::Continue(()))
}
