use std::sync::Arc;

use zz_frontend::ast::{BinOp, Param, Pattern, UnOp};
use zz_frontend::span::Span;

use super::chunk::Chunk;

/// A move-take home shared by the fused push/take ops: a frame slot or an
/// env binding name.
#[derive(Debug, Clone, PartialEq)]
pub enum TakeHome {
    Slot(u16),
    Env(String),
}

/// Bytecode instructions.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // ---- stack ----
    /// Push a constant from the chunk's constant pool.
    PushConst(u32),
    /// Discard the top of the stack.
    Pop,
    /// Swap the top two stack values. Lets left-to-right evaluation
    /// (base, then value) feed value-below op layouts (`SetField` pops
    /// object-then-value): `obj, value, Swap, SetField`. Stack-neutral
    /// and side-effect-free.
    Swap,
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
    /// Take the slot's value out (leaving `Unit`), pushing it: the
    /// single-occurrence load in `x = f(x, ...)` / `x = vec.push(x, e)`.
    /// The final store completes before any user code can re-read the
    /// slot (see `zz_frontend::move_elide`), and slots are never
    /// scope-shared, so the transient is unobservable.
    TakeSlot(u16),
    /// Take the env binding's value out (leaving `Unit`), pushing it:
    /// like `TakeSlot` for names that live in the environment. Falls back
    /// to the `LoadVar` resolution chain (funcs, natives, plugins) with a
    /// clone when the name is not an env binding, and errors identically
    /// when unbound.
    TakeVar(String, Span),
    /// Pop an element; take the array home, push the element in (reusing
    /// the owned `Vec`), store it back. Fused `x = vec.push(x, e)`:
    /// zero clones on the steady path. Restores the home on a type error
    /// (same error as `vec.push`).
    VecPush { home: TakeHome, span: Span },
    /// Pop an element; take the named field out of the home object
    /// (leaving `Unit` in the field), push the element in, store the field
    /// back. Fused `s.f = vec.push(s.f, e)`.
    VecPushField {
        home: TakeHome,
        field: String,
        span: Span,
    },
    /// Pop an element; take the home, and when it holds an array push in
    /// place and store back (fused `x.push(e)` / `x.append(e)`). Otherwise
    /// restore the home and run the generic method call with write-back,
    /// so user-defined methods behave exactly as before.
    VecPushMethod {
        home: TakeHome,
        method: String,
        span: Span,
    },
    /// Pop a value; take the home object, write `object.field = value` in
    /// place, store it back. Fused `s.f = v` with pure `v` (#242): zero
    /// clones on the steady path. Restores the home on error (same
    /// errors as the generic path).
    SetFieldTake {
        home: TakeHome,
        field: String,
        span: Span,
    },
    /// Pop an index then a value; take the home, write
    /// `object.field[index] = value` in place, store back. Fused
    /// `s.f[i] = v` (#242). Restores the home on error.
    SetFieldIndexTake {
        home: TakeHome,
        field: String,
        span: Span,
    },
    /// Pop an index then a value; take the home container out, write
    /// `home[index] = value` in place, store it back. Fused `a[i] = v`
    /// when the base is a direct home: the write then reaches a container
    /// with refcount 1, so `set_index` mutates it instead of
    /// `Arc::make_mut` deep-copying on every element write (the O(n²)
    /// index-fill pathology).
    ///
    /// Index and value are evaluated **before** the take (the op runs
    /// after both are on the stack), so the window is a single
    /// instruction with no safepoint and no user code inside it.
    /// Restores the home — mutated or not — on error, like the other
    /// take ops.
    StoreIndexTake { home: TakeHome, span: Span },
    /// Pop an index and a right-hand side; take the home container out, read
    /// `home[index]`, apply `op` against the rhs, write the result back, and
    /// store the container home. Fused `a[i] += v` with the same guarantees
    /// as [`Op::StoreIndexTake`]: both operands are evaluated before the op,
    /// the take window is one instruction, and the home is restored on error.
    CompoundIndexTake {
        home: TakeHome,
        op: BinOp,
        span: Span,
    },
    /// Pop a right-hand side; take the home, take the field, apply
    /// `field OP rhs` via `eval_binary`, store back. Fused
    /// `s.f = s.f OP v` (#242, e.g. string-concat accumulation).
    /// Restores the home on error.
    FieldBinopTake {
        home: TakeHome,
        field: String,
        op: BinOp,
        span: Span,
    },
    /// Pop an index; take the home, clone one element of the named
    /// field, restore the home, push the element. Fused `s.f[i]` reads
    /// (#242): O(1) instead of cloning the field array. Non-direct
    /// shapes complete generically at runtime (same errors).
    GetFieldIndex {
        home: TakeHome,
        field: String,
        span: Span,
    },
    /// Take the home, clone one element of the named field at `idx`, store
    /// it straight into slot `dst`, restore the home. Fused `s.f[i]` reads:
    /// O(1) element clone instead of cloning the field array, no stack
    /// traffic, fused with the assignment destination. Non-direct shapes
    /// complete generically at runtime (same errors).
    GetFieldSlotIdx {
        home: TakeHome,
        idx: u16,
        dst: u16,
        span: Span,
    },
    /// Take the home, push the length of the named field, restore the
    /// home. Fused `len(s.f)` (#242): no container or field clone.
    /// Non-direct shapes complete generically at runtime (same errors).
    LenField {
        home: TakeHome,
        field: String,
        span: Span,
    },
    /// Read `array[idx]` where both are stack slots: single bounds check,
    /// no `normalize_index` call, no `Value` tag loop. Fused `a[i]` reads
    /// when both operands are slot-resolvable. Result written to `dst`.
    IndexIntSlot {
        arr: u16,
        idx: u16,
        dst: u16,
        span: Span,
    },
    /// Read `dict[key]` where key is an int: probe DictData with a
    /// precomputed hash + open-addressed table. Fused int-keyed dict reads
    /// when both operands are slot-resolvable. Non-int keys fall back to
    /// generic `IndexOp`. Result written to `dst`.
    IndexDictInt {
        dict: u16,
        key: u16,
        dst: u16,
        span: Span,
    },
    /// In-place integer add: `slot[dst] = Int(slot[dst] + slot[src])`.
    ///
    /// Fused fast path for `x = x + y` inside hot loops: no stack traffic,
    /// no generic `BinOp` dispatch. Falls back to the generic path when
    /// either slot does not hold an `Int`.
    SlotAddInt { dst: u16, src: u16 },
    /// In-place integer increment: `slot[slot] = Int(slot[slot] + 1)`.
    ///
    /// Fused fast path for `x = x + 1` / `x += 1` inside hot loops.
    SlotInc { slot: u16 },
    /// In-place integer add of an immediate: `slot[dst] = Int(slot[dst] + imm)`.
    ///
    /// Fused fast path for `x = x + N` with a literal `N`.
    SlotAddIntImm { dst: u16, imm: i64 },
    /// In-place integer subtract of an immediate: `slot[dst] = Int(slot[dst] - imm)`.
    ///
    /// Fused fast path for `x = x - N` with a literal `N`.
    SlotSubIntImm { dst: u16, imm: i64 },
    /// In-place integer multiply by an immediate: `slot[dst] = Int(slot[dst] * imm)`.
    ///
    /// Fused fast path for `x = x * N` with a literal `N`.
    SlotMulIntImm { dst: u16, imm: i64 },
    /// Compare two slots as integers, push `Bool(a < b)`.
    SlotLessIntSlot { a: u16, b: u16 },
    /// Compare a slot against an immediate integer, push `Bool(a < imm)`.
    SlotLessIntImm { a: u16, imm: i64 },
    /// Compare two slots as integers with any comparison operator, push
    /// `Bool(slot[a] op slot[b])`.
    ///
    /// `SlotLessIntSlot` covers `<` (the loop-condition shape); this carries
    /// the other five, so `if i > 0` stops materialising `LoadSlot PushConst
    /// IntGt` before the jump. Falls back to `eval_binary` when either slot
    /// is not an `Int` (same semantics as the generic comparison).
    SlotCmpIntSlot {
        a: u16,
        b: u16,
        op: zz_frontend::ast::BinOp,
    },
    /// Compare a slot against an immediate integer with any comparison
    /// operator, push `Bool(slot[a] op imm)`.
    SlotCmpIntImm {
        a: u16,
        imm: i64,
        op: zz_frontend::ast::BinOp,
    },
    /// 3-address integer binary op: `slot[dst] = Int(slot[lhs] op slot[rhs])`.
    ///
    /// Fused fast path for `x = y op z` where all three resolve to local
    /// slots: no stack traffic for the operands, no generic `BinOp`
    /// dispatch. Fallback to `eval_binary` when either operand is not an
    /// `Int`.
    SlotBinaryInt {
        dst: u16,
        lhs: u16,
        rhs: u16,
        op: zz_frontend::ast::BinOp,
    },
    /// 3-address integer binary op with an immediate RHS:
    /// `slot[dst] = Int(slot[lhs] op imm)`.
    SlotBinaryIntImm {
        dst: u16,
        lhs: u16,
        imm: i64,
        op: zz_frontend::ast::BinOp,
    },
    /// Push `Int(slot[a] op imm)`.
    ///
    /// [`SlotBinaryIntImm`](Self::SlotBinaryIntImm) needs a destination
    /// slot, so it only fits assignment shapes; value-position arithmetic
    /// (`fib(n - 1)` as an argument, `x := n + 1`, `return i * 2`) otherwise
    /// pays `LoadSlot PushConst IntOp` — three dispatches. Falls back to
    /// `eval_binary` when the slot is not an `Int`.
    ///
    /// Unlike the older fused slot ops this one carries its expression span,
    /// so coverage attributes the line (a `Span::default()` op drops the
    /// source line from the report) and a trapping `Div`/`Rem` keeps its
    /// error location.
    PushSlotIntImm {
        a: u16,
        imm: i64,
        op: zz_frontend::ast::BinOp,
        span: Span,
    },
    /// Jump to `target` unless `Int(slot[a]) op imm` holds.
    ///
    /// The fused `SlotCmpIntImm` + `JumpIfFalseBool` pair an `if` condition
    /// emits: a loop guard written `if i < n` cost two dispatches per
    /// iteration plus the `Bool` on the stack. `while` tests keep the two-op
    /// shape because `WhileCond` carries the loop bookkeeping (back-edge,
    /// `break` target, yield check).
    ///
    /// Falls back to `eval_binary` when the slot is not an `Int`, and keeps
    /// the `if`-condition-must-be-a-bool error of the op it replaces.
    JumpIfFalseSlotCmpIntImm {
        a: u16,
        imm: i64,
        op: zz_frontend::ast::BinOp,
        target: usize,
        span: Span,
    },
    /// Jump to `target` unless `Int(slot[a]) op Int(slot[b])` holds; the
    /// fused `SlotCmpIntSlot` + `JumpIfFalseBool` pair.
    JumpIfFalseSlotCmpIntSlot {
        a: u16,
        b: u16,
        op: zz_frontend::ast::BinOp,
        target: usize,
        span: Span,
    },
    /// Compare a slot against a constant-pool entry, push `Bool(a op c)`.
    ///
    /// `SlotCmpIntImm` inlines `i64` literals; every other literal (string,
    /// float, bool) lived in the pool but still cost `LoadSlot PushConst
    /// BinOp` — three dispatches for one comparison. `k` indexes
    /// [`Chunk::constants`](super::Chunk::constants), like `PushConst`.
    ///
    /// Strings compare in place (`Arc<String>` equality, no clone) for
    /// `==`/`!=`; everything else falls back to `eval_binary`, so mixed
    /// types keep the generic semantics.
    SlotCmpConst {
        a: u16,
        k: u32,
        op: zz_frontend::ast::BinOp,
    },

    // ---- functions & structs ----
    /// Create a named function value from a pre-compiled body chunk, register
    /// it in `funcs` and the current scope, and push unit.
    MakeFunc {
        name: String,
        params: Vec<Param>,
        chunk: Arc<Chunk>,
        /// Pre-compiled default-argument bodies, parallel to `params`.
        /// Empty on the normal compile path (AST defaults apply); populated
        /// by `.zzc` loads, which carry no AST.
        defaults: Vec<Option<Arc<Chunk>>>,
    },
    /// Register a struct definition (name -> ordered field names).
    RegisterStruct { name: String, fields: Vec<String> },
    /// Register a user enum (name -> variant names). Values are plain
    /// `Object`s; this table lets the VM resolve qualified construction
    /// (`Token.IntLit(1)`) without type information.
    RegisterEnum {
        name: String,
        variants: Vec<(String, bool)>,
    },
    /// Build a user-enum variant value: pops `argc` payload values
    /// (0 or 1 after checking) and pushes the qualified `Object`
    /// (`Token.IntLit`). The head resolves at runtime (direct hit or
    /// selective-import alias, mirroring `MakeStruct`), so untyped
    /// compiles need no seed tables.
    MakeEnum {
        enum_name: String,
        variant: String,
        argc: u16,
        span: Span,
    },

    // ---- typed arithmetic (unboxed, int-only) ----
    /// Pop two ints, push `a + b` (wrapping in release, checked in debug).
    IntAdd(Span),
    /// Pop two ints, push `a - b`.
    IntSub(Span),
    /// Pop two ints, push `a * b`.
    IntMul(Span),
    /// Pop two ints, push `a / b` (error on zero).
    IntDiv(Span),
    /// Pop two ints, push `a % b` (error on zero).
    IntRem(Span),
    /// Pop one int, push `-a`.
    IntNeg(Span),
    /// Pop two ints, push `a < b` (#364: mirrors the IntAdd fast path;
    /// comparisons previously always took generic `BinOp` dispatch).
    IntLt(Span),
    /// Pop two ints, push `a > b`.
    IntGt(Span),
    /// Pop two ints, push `a <= b`.
    IntLe(Span),
    /// Pop two ints, push `a >= b`.
    IntGe(Span),
    /// Pop two ints, push `a == b`.
    IntEq(Span),
    /// Pop two ints, push `a != b`.
    IntNe(Span),

    // ---- generic arithmetic (fallback) ----
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
    ///
    /// `for` loops are laid out rotated — the test sits at the bottom of
    /// the loop and jumps forward into the body — so this op is both the
    /// test and the back-edge: one dispatch per iteration instead of a
    /// test plus an unconditional `Jump`.
    ForNext {
        vars: Vec<String>,
        /// Loop exit: the iterable is exhausted, so control lands just
        /// past the `Pop` that drops it.
        exit: usize,
        /// Body entry: where a non-exhausted iteration resumes.
        body: usize,
        in_env: bool,
        span: Span,
    },
    /// Push a `while` loop frame. `exit`/`header` are patched by the
    /// compiler.
    WhileSetup { exit: usize, header: usize },
    /// Pop the condition; error unless it is a bool; exit the loop when
    /// falsy. Also the loop's cooperative safepoint (folded in from the
    /// header — see `Compiler::compile_while`): this op is reached once per
    /// test from every edge, so the yield check cannot be skipped, and it
    /// runs before the condition is popped so a yield re-executes the op
    /// whole.
    WhileCond { exit: usize, span: Span },
    /// Exit the innermost loop (restores the loop's environment).
    Break(Span),
    /// Jump to the innermost loop's header (restores the loop's environment).
    Continue(Span),
    /// Pop a value and store it as the innermost loop's result.
    SetLoopResult,
    /// Cooperative safepoint (stack-neutral: decrements the VM's slice
    /// budget, and once per budget worth of iterations checks the timeslice
    /// clock — yielding to the executor when the task overruns its quantum
    /// so CPU-bound loops cannot starve sibling tasks). `for` loops charge
    /// the check to `ForNext` and `while` loops to `WhileCond`, both of
    /// which every edge of the loop passes through; this standalone op is
    /// what a producer that cannot fold it emits, with header (not
    /// back-edge) placement so `continue` cannot skip the check.
    Safepoint,

    // ---- collections ----
    /// Pop `n` values and push an array.
    MakeArray(u16),
    /// Pop a tuple/array and push its elements on the stack.
    UnpackTuple(u8),
    /// Pop a value and an array; push the array with the value appended.
    ArrayPush(Span),
    /// Pop `2n` values (key, value pairs) and push a dict.
    MakeDict(u16),
    /// Pop an index, pop an object, push `object[index]`.
    IndexOp(Span),
    /// Pop a value, an index, and an object; write `object[index] = value`;
    /// push the mutated object back (for write-back).
    StoreIndexOp(Span),
    /// Pop a value, an index, and an object; push the mutated object
    /// back with `object[index] OP value` stored (for write-back).
    /// Single evaluation: the object/index were compiled once.
    CompoundIndexOp {
        op: zz_frontend::ast::BinOp,
        span: Span,
    },
    /// Pop an end bound, a start bound (either `int` or `Unit` for absent),
    /// and an object; push the slice.
    SliceOp(Span),
    /// Pop an end, pop a start, push a range (both must be ints).
    MakeRange(Span),

    // ---- structs ----
    /// Pop `field_names.len()` values and build a struct instance, matching
    /// them to the registered field order by name.
    ///
    /// The names are the chunk's interned copies: the literal site pays for
    /// them once at compile time, and every execution copies a refcount into
    /// the instance instead of allocating a `String` per field.
    MakeStruct {
        name: String,
        field_names: Vec<Arc<str>>,
        span: Span,
    },
    /// Pop an object, push `object.field`.
    GetField(String, Span),
    /// Pop an object, push `object.fields[idx]` (O(1) for known struct types).
    GetFieldIdx(u16, Span),
    /// Pop a value and an object; write `object.field = value`; push the
    /// mutated object back (for write-back).
    SetField(String, Span),
    /// Pop a value and an object; write `object.field OP value`; push
    /// the mutated object back (for write-back). Single evaluation:
    /// the object was compiled once.
    CompoundFieldOp {
        name: String,
        op: zz_frontend::ast::BinOp,
        span: Span,
    },
    /// Pop a value and an object; write `object.fields[idx] = value`; push
    /// the mutated object back (O(1) for known struct types).
    SetFieldIdx(u16, Span),

    // ---- closures & variants ----
    /// Create a closure value from a pre-compiled body chunk, capturing the
    /// current environment.
    MakeClosure {
        params: Vec<Param>,
        chunk: Arc<Chunk>,
        /// Pre-compiled default-argument bodies (see `MakeFunc`).
        defaults: Vec<Option<Arc<Chunk>>>,
    },
    /// Fused `task.spawn(closure-literal)`: build the task directly from
    /// the pre-compiled body chunk, skipping the `FuncValue` box + native
    /// call dispatch. Behaviorally identical to MakeClosure + task.spawn;
    /// emitted only for literal closures (variables keep the generic
    /// path with its arity errors).
    SpawnClosure {
        params: Vec<Param>,
        chunk: Arc<Chunk>,
        span: Span,
        /// Pre-compiled default-argument bodies (see `MakeFunc`).
        defaults: Vec<Option<Arc<Chunk>>>,
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
    /// otherwise jump to `next` (the following arm or the non-exhaustive
    /// error). When `restore` is true, push the scrutinee back before
    /// jumping (used when the compiler reloads the scrutinee from a slot).
    MatchArm {
        pat: Pattern,
        next: usize,
        has_env: bool,
        restore: bool,
    },
    /// Pop a bool guard value; if false, jump to `next` (next arm).
    /// When `has_env` is true, also exit the scope created by MatchArm.
    MatchGuard { next: usize, has_env: bool },
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
        /// `parts` pre-joined (`a.b.c`): computed once at compile time so
        /// the hot dispatch loop never allocates a lookup string per call.
        joined: String,
        argc: u16,
        span: Span,
        pspan: Span,
    },

    /// Call the last path component as a field or method on a receiver that
    /// was loaded from a local slot (`p.dist()` where `p` is a local).
    /// Pops `argc` args, then the receiver.
    CallMethod { name: String, argc: u16, span: Span },

    /// Direct native function call: resolves the function by name in the
    /// native registry and calls it directly, bypassing env/func/method
    /// resolution. Pops `argc` args, pushes result.
    CallNative { name: String, argc: u16, span: Span },

    // ---- fmt ----
    /// Pop `n` values, concatenate their Display forms, push the string.
    Concat(u16),
    /// Pop a format spec string, pop a value, push the formatted string.
    FormatValue(Span),
    /// sqlz prepared-query split: pop `n` bound values, pop the Fmt
    /// template parts count `m`... (compiled inline — see DbQuery).
    /// Pops `nparams` bound values + `nparts` template parts, pushes the
    /// SQL template string (with `{expr}` segments replaced by `?N`
    /// placeholders), then re-pushes each bound value in order. The
    /// following `CallNative(db.query/db.exec)` consumes
    /// `[template, p1, ...]` and binds via prepared statements.
    DbQuery { nparams: u16, span: Span },

    // ---- scopes ----
    /// Enter a new child scope (only emitted when the scope declares
    /// captured variables).
    EnterScope,
    /// Leave the current scope, restoring its parent.
    ExitScope,
    /// Pop the top value, discard `n` values below it, and push the value
    /// back: leaves a scope's result while dropping its local slots.
    PopN { n: u16, span: Span },

    // ---- defer ----
    /// Record a deferred closure: pop a closure value, push onto the
    /// frame's defer stack. Executed LIFO on scope exit.
    DeferRecord,
}

/// Coarse class of an opcode, used by `zz dis --vm` and `ZZ_VM_STATS`.
///
/// The split answers one question: how much of the executed stream runs
/// through a fused or specialized fast path (`Fused`, `IntOp`) instead of
/// generic dispatch?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpClass {
    /// Ops that remove stack traffic or a generic call (`Slot*`, `Take*`,
    /// `VecPush*`, fused field/index writes).
    Fused,
    /// Typed i64 arithmetic and comparison fast paths (`IntAdd`…`IntNe`).
    IntOp,
    /// Stack, slot, scope, and environment movement.
    Stack,
    /// Generic operators (`BinOp`, `UnOp`, `Concat`, formatting).
    Arith,
    /// Jumps, loops, branches, returns, and match/trap control.
    Control,
    /// Function and native invocation, plus function construction.
    Call,
    /// Field, index, slice, and tuple access.
    Access,
    /// Container and aggregate construction.
    Alloc,
    /// Everything else.
    Other,
}

impl OpClass {
    pub const ALL: [OpClass; 9] = [
        OpClass::Fused,
        OpClass::IntOp,
        OpClass::Stack,
        OpClass::Arith,
        OpClass::Control,
        OpClass::Call,
        OpClass::Access,
        OpClass::Alloc,
        OpClass::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            OpClass::Fused => "fused",
            OpClass::IntOp => "int-op",
            OpClass::Stack => "stack",
            OpClass::Arith => "arith",
            OpClass::Control => "control",
            OpClass::Call => "call",
            OpClass::Access => "access",
            OpClass::Alloc => "alloc",
            OpClass::Other => "other",
        }
    }

    /// Dense index into counter arrays (`0..ALL.len()`).
    pub fn index(self) -> usize {
        self as usize
    }

    /// `true` for the fast paths that fusion work is measured against.
    pub fn is_fast(self) -> bool {
        matches!(self, OpClass::Fused | OpClass::IntOp)
    }
}

impl Op {
    /// Variant name without its payload — stable enough for disassembly,
    /// histogram keys, and PR evidence.
    pub fn mnemonic(&self) -> &'static str {
        match self {
            Op::PushConst(..) => "PushConst",
            Op::Pop => "Pop",
            Op::Swap => "Swap",
            Op::Truthy => "Truthy",
            Op::LoadVar(..) => "LoadVar",
            Op::LoadPath(..) => "LoadPath",
            Op::DefineVar(..) => "DefineVar",
            Op::StoreVar(..) => "StoreVar",
            Op::StorePath(..) => "StorePath",
            Op::LoadSlot(..) => "LoadSlot",
            Op::StoreSlot(..) => "StoreSlot",
            Op::TakeSlot(..) => "TakeSlot",
            Op::TakeVar(..) => "TakeVar",
            Op::VecPush { .. } => "VecPush",
            Op::VecPushField { .. } => "VecPushField",
            Op::VecPushMethod { .. } => "VecPushMethod",
            Op::SetFieldTake { .. } => "SetFieldTake",
            Op::SetFieldIndexTake { .. } => "SetFieldIndexTake",
            Op::StoreIndexTake { .. } => "StoreIndexTake",
            Op::CompoundIndexTake { .. } => "CompoundIndexTake",
            Op::FieldBinopTake { .. } => "FieldBinopTake",
            Op::GetFieldIndex { .. } => "GetFieldIndex",
            Op::GetFieldSlotIdx { .. } => "GetFieldSlotIdx",
            Op::IndexIntSlot { .. } => "IndexIntSlot",
            Op::IndexDictInt { .. } => "IndexDictInt",
            Op::LenField { .. } => "LenField",
            Op::SlotAddInt { .. } => "SlotAddInt",
            Op::SlotInc { .. } => "SlotInc",
            Op::SlotAddIntImm { .. } => "SlotAddIntImm",
            Op::SlotSubIntImm { .. } => "SlotSubIntImm",
            Op::SlotMulIntImm { .. } => "SlotMulIntImm",
            Op::SlotLessIntSlot { .. } => "SlotLessIntSlot",
            Op::SlotLessIntImm { .. } => "SlotLessIntImm",
            Op::SlotCmpIntSlot { .. } => "SlotCmpIntSlot",
            Op::SlotCmpIntImm { .. } => "SlotCmpIntImm",
            Op::SlotCmpConst { .. } => "SlotCmpConst",
            Op::SlotBinaryInt { .. } => "SlotBinaryInt",
            Op::SlotBinaryIntImm { .. } => "SlotBinaryIntImm",
            Op::PushSlotIntImm { .. } => "PushSlotIntImm",
            Op::JumpIfFalseSlotCmpIntImm { .. } => "JumpIfFalseSlotCmpIntImm",
            Op::JumpIfFalseSlotCmpIntSlot { .. } => "JumpIfFalseSlotCmpIntSlot",
            Op::MakeFunc { .. } => "MakeFunc",
            Op::RegisterStruct { .. } => "RegisterStruct",
            Op::RegisterEnum { .. } => "RegisterEnum",
            Op::MakeEnum { .. } => "MakeEnum",
            Op::IntAdd(..) => "IntAdd",
            Op::IntSub(..) => "IntSub",
            Op::IntMul(..) => "IntMul",
            Op::IntDiv(..) => "IntDiv",
            Op::IntRem(..) => "IntRem",
            Op::IntNeg(..) => "IntNeg",
            Op::IntLt(..) => "IntLt",
            Op::IntGt(..) => "IntGt",
            Op::IntLe(..) => "IntLe",
            Op::IntGe(..) => "IntGe",
            Op::IntEq(..) => "IntEq",
            Op::IntNe(..) => "IntNe",
            Op::BinOp(..) => "BinOp",
            Op::UnOp(..) => "UnOp",
            Op::Jump(..) => "Jump",
            Op::JumpIfFalse(..) => "JumpIfFalse",
            Op::JumpIfTrue(..) => "JumpIfTrue",
            Op::JumpIfFalseBool(..) => "JumpIfFalseBool",
            Op::Return => "Return",
            Op::ForSetup { .. } => "ForSetup",
            Op::ForNext { .. } => "ForNext",
            Op::WhileSetup { .. } => "WhileSetup",
            Op::WhileCond { .. } => "WhileCond",
            Op::Break(..) => "Break",
            Op::Continue(..) => "Continue",
            Op::SetLoopResult => "SetLoopResult",
            Op::Safepoint => "Safepoint",
            Op::MakeArray(..) => "MakeArray",
            Op::UnpackTuple(..) => "UnpackTuple",
            Op::ArrayPush(..) => "ArrayPush",
            Op::MakeDict(..) => "MakeDict",
            Op::IndexOp(..) => "IndexOp",
            Op::StoreIndexOp(..) => "StoreIndexOp",
            Op::CompoundIndexOp { .. } => "CompoundIndexOp",
            Op::SliceOp(..) => "SliceOp",
            Op::MakeRange(..) => "MakeRange",
            Op::MakeStruct { .. } => "MakeStruct",
            Op::GetField(..) => "GetField",
            Op::GetFieldIdx(..) => "GetFieldIdx",
            Op::SetField(..) => "SetField",
            Op::CompoundFieldOp { .. } => "CompoundFieldOp",
            Op::SetFieldIdx(..) => "SetFieldIdx",
            Op::MakeClosure { .. } => "MakeClosure",
            Op::SpawnClosure { .. } => "SpawnClosure",
            Op::MakeVariant { .. } => "MakeVariant",
            Op::MatchArm { .. } => "MatchArm",
            Op::MatchGuard { .. } => "MatchGuard",
            Op::MatchError(..) => "MatchError",
            Op::IfLetMatch { .. } => "IfLetMatch",
            Op::TryOp(..) => "TryOp",
            Op::Elvis(..) => "Elvis",
            Op::ElvisResult => "ElvisResult",
            Op::Call { .. } => "Call",
            Op::CallPath { .. } => "CallPath",
            Op::CallMethod { .. } => "CallMethod",
            Op::CallNative { .. } => "CallNative",
            Op::Concat(..) => "Concat",
            Op::FormatValue(..) => "FormatValue",
            Op::DbQuery { .. } => "DbQuery",
            Op::EnterScope => "EnterScope",
            Op::ExitScope => "ExitScope",
            Op::PopN { .. } => "PopN",
            Op::DeferRecord => "DeferRecord",
        }
    }

    /// Coarse class (see [`OpClass`]).
    pub fn class(&self) -> OpClass {
        match self {
            Op::TakeSlot(..)
            | Op::TakeVar(..)
            | Op::VecPush { .. }
            | Op::VecPushField { .. }
            | Op::VecPushMethod { .. }
            | Op::SetFieldTake { .. }
            | Op::SetFieldIndexTake { .. }
            | Op::StoreIndexTake { .. }
            | Op::CompoundIndexTake { .. }
            | Op::FieldBinopTake { .. }
            | Op::GetFieldIndex { .. }
            | Op::GetFieldSlotIdx { .. }
            | Op::LenField { .. }
            | Op::SlotAddInt { .. }
            | Op::SlotInc { .. }
            | Op::SlotAddIntImm { .. }
            | Op::SlotSubIntImm { .. }
            | Op::SlotMulIntImm { .. }
            | Op::SlotLessIntSlot { .. }
            | Op::SlotLessIntImm { .. }
            | Op::SlotCmpIntSlot { .. }
            | Op::SlotCmpIntImm { .. }
            | Op::SlotCmpConst { .. }
            | Op::SlotBinaryInt { .. }
            | Op::SlotBinaryIntImm { .. }
            | Op::PushSlotIntImm { .. }
            | Op::JumpIfFalseSlotCmpIntImm { .. }
            | Op::JumpIfFalseSlotCmpIntSlot { .. } => OpClass::Fused,

            Op::IntAdd(..)
            | Op::IntSub(..)
            | Op::IntMul(..)
            | Op::IntDiv(..)
            | Op::IntRem(..)
            | Op::IntNeg(..)
            | Op::IntLt(..)
            | Op::IntGt(..)
            | Op::IntLe(..)
            | Op::IntGe(..)
            | Op::IntEq(..)
            | Op::IntNe(..) => OpClass::IntOp,

            Op::PushConst(..)
            | Op::Pop
            | Op::PopN { .. }
            | Op::Swap
            | Op::Truthy
            | Op::LoadSlot(..)
            | Op::StoreSlot(..)
            | Op::LoadVar(..)
            | Op::LoadPath(..)
            | Op::StoreVar(..)
            | Op::StorePath(..)
            | Op::DefineVar(..)
            | Op::EnterScope
            | Op::ExitScope => OpClass::Stack,

            Op::BinOp(..)
            | Op::UnOp(..)
            | Op::Concat(..)
            | Op::FormatValue(..)
            | Op::Elvis(..)
            | Op::ElvisResult => OpClass::Arith,

            Op::Jump(..)
            | Op::JumpIfFalse(..)
            | Op::JumpIfTrue(..)
            | Op::JumpIfFalseBool(..)
            | Op::Return
            | Op::ForSetup { .. }
            | Op::ForNext { .. }
            | Op::WhileSetup { .. }
            | Op::WhileCond { .. }
            | Op::Break(..)
            | Op::Continue(..)
            | Op::SetLoopResult
            | Op::Safepoint
            | Op::MatchArm { .. }
            | Op::MatchGuard { .. }
            | Op::MatchError(..)
            | Op::IfLetMatch { .. }
            | Op::TryOp(..)
            | Op::DeferRecord => OpClass::Control,

            Op::Call { .. }
            | Op::CallPath { .. }
            | Op::CallMethod { .. }
            | Op::CallNative { .. }
            | Op::MakeFunc { .. }
            | Op::MakeClosure { .. }
            | Op::SpawnClosure { .. } => OpClass::Call,

            Op::GetField(..)
            | Op::GetFieldIdx(..)
            | Op::SetField(..)
            | Op::SetFieldIdx(..)
            | Op::CompoundFieldOp { .. }
            | Op::IndexOp(..)
            | Op::IndexIntSlot { .. }
            | Op::IndexDictInt { .. }
            | Op::StoreIndexOp(..)
            | Op::CompoundIndexOp { .. }
            | Op::SliceOp(..)
            | Op::ArrayPush(..)
            | Op::UnpackTuple(..) => OpClass::Access,

            Op::MakeArray(..)
            | Op::MakeDict(..)
            | Op::MakeRange(..)
            | Op::MakeStruct { .. }
            | Op::MakeEnum { .. }
            | Op::MakeVariant { .. }
            | Op::RegisterStruct { .. }
            | Op::RegisterEnum { .. }
            | Op::DbQuery { .. } => OpClass::Alloc,
        }
    }
}
