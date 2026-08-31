use std::rc::Rc;

use zz_frontend::ast::{BinOp, Param, Pattern, UnOp};
use zz_frontend::span::Span;

use super::chunk::Chunk;

/// Bytecode instructions.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // ---- stack ----
    /// Push a constant from the chunk's constant pool.
    PushConst(u32),
    /// Discard the top of the stack.
    Pop,
    /// Replace the top of the stack with `Bool(v.is_truthy())`.
    Truthy,

    // ---- variables ----
    /// Push the value bound to `name` (env, then funcs, then natives).
    LoadVar(String, Span),
    /// Push the value of a dotted path (direct binding, func, native, or
    /// struct-field walk).
    LoadPath(Vec<String>, Span),
    /// Pop a value, bind it to `name` in the current scope, and push it back
    /// (a declaration evaluates to its value).
    DefineVar(String),
    /// Pop a value and assign it to `name` (walks the scope chain).
    StoreVar(String, Span),
    /// Pop a value and assign it to a dotted path (direct binding or
    /// struct-field walk with write-back).
    StorePath(Vec<String>, Span),
    /// Push the value of a compile-time-resolved local slot.
    LoadSlot(u16),
    /// Pop a value and write it to a compile-time-resolved local slot.
    StoreSlot(u16),

    // ---- functions & structs ----
    /// Create a named function value from a pre-compiled body chunk, register
    /// it in `funcs` and the current scope, and push unit.
    MakeFunc {
        name: String,
        params: Vec<Param>,
        chunk: Rc<Chunk>,
    },
    /// Register a struct definition (name -> ordered field names).
    RegisterStruct { name: String, fields: Vec<String> },

    // ---- arithmetic ----
    /// Pop `b`, pop `a`, push `a op b` (int/float/str semantics).
    BinOp(BinOp, Span),
    /// Pop `v`, push `op v`.
    UnOp(UnOp, Span),

    // ---- control flow ----
    /// Unconditional jump to an absolute instruction index.
    Jump(usize),
    /// Pop a value; jump if it is falsy.
    JumpIfFalse(usize),
    /// Pop a value; jump if it is truthy.
    JumpIfTrue(usize),
    /// Pop a value; error unless it is a bool; jump if false.
    JumpIfFalseBool(usize, Span),
    /// Pop a value and return it from the current function frame.
    Return,

    // ---- loops ----
    /// Pop an iterable (array or range), push it back plus an index counter,
    /// and push a loop frame. `exit`/`header` are patched by the compiler.
    ForSetup {
        exit: usize,
        header: usize,
        span: Span,
        num_vars: u8,
    },
    /// Advance a `for` loop: pop the index, push `index + 1` and the current
    /// item(s) (bound to `var` in a fresh iteration scope when `in_env`, or
    /// left on the stack as a local slot(s)), or exit when the iterable is
    /// exhausted.
    ForNext {
        vars: Vec<String>,
        exit: usize,
        in_env: bool,
    },
    /// Push a `while` loop frame. `exit`/`header` are patched by the
    /// compiler.
    WhileSetup { exit: usize, header: usize },
    /// Pop the condition; error unless it is a bool; exit the loop when
    /// falsy.
    WhileCond { exit: usize, span: Span },
    /// Exit the innermost loop (restores the loop's environment).
    Break(Span),
    /// Jump to the innermost loop's header (restores the loop's environment).
    Continue(Span),
    /// Pop a value and store it as the innermost loop's result.
    SetLoopResult,

    // ---- collections ----
    /// Pop `n` values and push an array.
    MakeArray(u16),
    /// Pop a value and an array; push the array with the value appended.
    ArrayPush(Span),
    /// Pop `2n` values (key, value pairs) and push a dict.
    MakeDict(u16),
    /// Pop an index, pop an object, push `object[index]`.
    IndexOp(Span),
    /// Pop a value, an index, and an object; write `object[index] = value`;
    /// push the mutated object back (for write-back).
    StoreIndexOp(Span),
    /// Pop an end bound, a start bound (either `int` or `Unit` for absent),
    /// and an object; push the slice.
    SliceOp(Span),
    /// Pop an end, pop a start, push a range (both must be ints).
    MakeRange(Span),

    // ---- structs ----
    /// Pop `field_names.len()` values and build a struct instance, matching
    /// them to the registered field order by name.
    MakeStruct {
        name: String,
        field_names: Vec<String>,
        span: Span,
    },
    /// Pop an object, push `object.field`.
    GetField(String, Span),
    /// Pop a value and an object; write `object.field = value`; push the
    /// mutated object back (for write-back).
    SetField(String, Span),

    // ---- closures & variants ----
    /// Create a closure value from a pre-compiled body chunk, capturing the
    /// current environment.
    MakeClosure {
        params: Vec<Param>,
        chunk: Rc<Chunk>,
    },
    /// Pop an optional argument and push an Option/Result variant.
    MakeVariant {
        name: String,
        has_arg: bool,
        span: Span,
    },

    // ---- pattern matching ----
    /// Pop the scrutinee; try `pat` in a fresh scope (when `has_env`, i.e.
    /// the pattern binds names). On a match, run the body in that scope;
    /// otherwise push the scrutinee back and jump to `next` (the following
    /// arm or the non-exhaustive error).
    MatchArm {
        pat: Pattern,
        next: usize,
        has_env: bool,
    },
    /// Error: no match arm matched.
    MatchError(Span),
    /// Pop the value; try `pat` in a fresh scope (when `has_env`). On a
    /// match, run the `then` block in that scope; otherwise push the value
    /// back and jump to `els`.
    IfLetMatch {
        pat: Pattern,
        els: usize,
        has_env: bool,
    },
    /// Pop a value; unwrap Option/Result or `return` the None/Err variant.
    TryOp(Span),

    // ---- elvis ----
    /// Pop a value; if `.some(v)`, push `v` then `Bool(true)`.
    /// If `.none`, push `Bool(false)`.
    Elvis(Span),
    /// Pop `Bool(success)`, pop right side, pop unwrapped left side.
    /// If success: push unwrapped left. Otherwise: push right side.
    ElvisResult,

    // ---- calls ----
    /// Pop `argc` arguments, pop the callee, call it, push the result.
    Call { argc: u16, span: Span },
    /// Like `Call` but the callee is a dotted path: resolves method calls
    /// (`p.dist()`) with receiver-first semantics.
    CallPath {
        parts: Vec<String>,
        argc: u16,
        span: Span,
        pspan: Span,
    },

    /// Call the last path component as a field or method on a receiver that
    /// was loaded from a local slot (`p.dist()` where `p` is a local).
    /// Pops `argc` args, then the receiver.
    CallMethod { name: String, argc: u16, span: Span },

    // ---- fmt ----
    /// Pop `n` values, concatenate their Display forms, push the string.
    Concat(u16),
    /// Pop a format spec string, pop a value, push the formatted string.
    FormatValue(Span),

    // ---- scopes ----
    /// Enter a new child scope (only emitted when the scope declares
    /// captured variables).
    EnterScope,
    /// Leave the current scope, restoring its parent.
    ExitScope,
    /// Pop the top value, discard `n` values below it, and push the value
    /// back: leaves a scope's result while dropping its local slots.
    PopN(u16),

    // ---- defer ----
    /// Record a deferred closure: pop a closure value, push onto the
    /// frame's defer stack. Executed LIFO on scope exit.
    DeferRecord,
}
