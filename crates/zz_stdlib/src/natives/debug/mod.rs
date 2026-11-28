//! Test-only debug natives for inspecting state lock/batch depth.
//!
//! Used by the step 3b.4(d) lock-leak regression test to assert that batch
//! depth and lock depth return to 0 after functions with early return /
//! break / continue / error complete.

use zz_runtime::{EvalError, Interp, Span, Value};

/// Return the current state batch depth (number of active
/// `StateBatchBegin`/`StateBatchEnd` regions). Should be 0 outside any
/// batched state-read region.
pub(crate) fn debug_state_batch_depth(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let depth = zz_runtime::state_batch_depth_debug();
    Ok(Value::Int(depth as i64))
}

/// Return the current state lock depth (number of active `atomic { }` blocks).
/// Should be 0 outside any `atomic { }` block.
pub(crate) fn debug_state_lock_depth(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    let depth = zz_runtime::state_lock_depth_debug();
    Ok(Value::Int(depth as i64))
}
