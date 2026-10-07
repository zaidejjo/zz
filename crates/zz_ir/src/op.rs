//! Typed core ops: the only instructions that may appear in `.zzc`.
//!
//! Frontend-independent: names are [`StrId`], constants are [`ConstId`],
//! functions are [`FuncId`], spans travel in the parallel span array
//! (except [`Op::CallPath`]'s second span, which rides along).
//!
//! Stack effects are fixed per op (see [`verify`]); jumps use absolute
//! instruction indices within the owning function.
//!
//! [`verify`]: crate::verify

use crate::{ConstId, FuncId, Span, StrId};

/// Binary operators (mirror of the frontend's `BinOp`; order is fixed —
/// append only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Elvis,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

/// Unary operators (mirror of the frontend's `UnOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Pos,
    Not,
    BitNot,
}

/// Match patterns. Literal payloads are pool references (only
/// int/float/str/bool consts are valid — enforced by [`verify`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Binding(StrId),
    Lit(ConstId),
    Variant {
        name: StrId,
        arg: Option<Box<Pattern>>,
    },
    Tuple(Vec<Pattern>),
    Or(Vec<Pattern>),
}

/// Core bytecode instructions.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // ---- stack ----
    /// Push a pool constant.
    PushConst(ConstId),
    /// Discard the top of the stack.
    Pop,
    /// Swap the top two stack values (left-to-right evaluation feeding
    /// value-below op layouts).
    Swap,
    /// Pop `n` values below the top, keeping the top.
    PopN(u16),
    /// Replace the top with `Bool(v.is_truthy())`.
    Truthy,

    // ---- variables ----
    /// Push the value bound to a name (env, then funcs, then natives).
    LoadVar(StrId),
    /// Push the value of a dotted path.
    LoadPath(Vec<StrId>),
    /// Pop a value, bind it in the current scope, push it back.
    DefineVar(StrId),
    /// Pop a value and assign it to a name (scope-chain walk).
    StoreVar(StrId),
    /// Pop a value and assign it to a dotted path.
    StorePath(Vec<StrId>),
    /// Push a compile-time-resolved local slot.
    LoadSlot(u16),
    /// Pop a value into a compile-time-resolved local slot.
    StoreSlot(u16),
    /// Take an env binding out (leaving `Unit`); falls back to the
    /// `LoadVar` chain with a clone for non-env bindings.
    TakeVar(StrId),

    // ---- move-take stores with dispatch semantics (stay core) ----
    /// Pop an element; take the named field out of the home object,
    /// push the element in, store the field back.
    VecPushField {
        home_slot: Option<u16>,
        home_var: Option<StrId>,
        field: StrId,
    },
    /// Pop an element; array fast path, else generic method call.
    VecPushMethod {
        home_slot: Option<u16>,
        home_var: Option<StrId>,
        method: StrId,
    },

    // ---- typed integer arithmetic (wrap; trap MIN/-1, MIN%-1, div0) ----
    IntAdd,
    IntSub,
    IntMul,
    IntDiv,
    IntRem,
    IntNeg,
    /// Generic `a op b` (int/float/str semantics).
    BinOp(BinOp),
    /// Generic `op v`.
    UnOp(UnOp),

    // ---- control flow ----
    Jump(u32),
    /// Pop a value; jump if falsy.
    JumpIfFalse(u32),
    /// Pop a value; jump if truthy.
    JumpIfTrue(u32),
    /// Pop a value; error unless bool; jump if false.
    JumpIfFalseBool(u32),
    /// Pop a value and return it from the current frame.
    Return,

    // ---- loops ----
    /// Pop an iterable; push it back plus a counter and a loop frame.
    ForSetup {
        exit: u32,
        header: u32,
        num_vars: u8,
    },
    /// Advance a `for` loop or exit when exhausted.
    ForNext {
        vars: Vec<StrId>,
        exit: u32,
        in_env: bool,
    },
    /// Push a `while` loop frame.
    WhileSetup {
        exit: u32,
        header: u32,
    },
    /// Pop the condition (must be bool); exit when falsy.
    WhileCond {
        exit: u32,
    },
    /// Exit the innermost loop.
    Break,
    /// Jump to the innermost loop's header.
    Continue,
    /// Pop a value as the innermost loop's result.
    SetLoopResult,
    /// Loop-header safepoint (stack-neutral timeslice check).
    Safepoint,

    // ---- collections ----
    /// Pop `n` values, push an array.
    MakeArray(u16),
    /// Pop a tuple/array, push its elements.
    UnpackTuple(u8),
    /// Pop a value and an array; push the array with the value appended.
    ArrayPush,
    /// Pop `2n` values (key/value pairs), push a dict.
    MakeDict(u16),
    /// Pop an index and an object; push `object[index]`.
    IndexOp,
    /// Pop value, index, object; write `object[index] = value`; push
    /// the object back. Operands evaluate base, index, value (§7).
    StoreIndexOp,
    /// Pop rhs, index, object; single-evaluation compound store.
    CompoundIndexOp {
        op: BinOp,
    },
    /// Pop end, start, object; push the slice.
    SliceOp,
    /// Pop end, pop start; push a range.
    MakeRange,

    // ---- structs ----
    /// Pop `fields.len()` values, build a struct instance.
    MakeStruct {
        name: StrId,
        fields: Vec<StrId>,
    },
    /// Pop an object, push `object.field`.
    GetField(StrId),
    /// Pop an object, push `object.fields[idx]`.
    GetFieldIdx(u16),
    /// Pop a value and an object; write `object.field = value`; push
    /// the object back. Operands evaluate base, then value (§7).
    SetField(StrId),
    /// Pop a value and an object; write `object.fields[idx] = value`.
    SetFieldIdx(u16),
    /// Pop a value and an object; compound field store.
    CompoundFieldOp {
        name: StrId,
        op: BinOp,
    },
    /// Register a struct definition (name → ordered fields).
    RegisterStruct {
        name: StrId,
        fields: Vec<StrId>,
    },
    /// Register a user enum (name → variants with payload flags).
    RegisterEnum {
        name: StrId,
        variants: Vec<(StrId, bool)>,
    },
    /// Pop `argc` payloads, push a qualified enum variant value.
    MakeEnum {
        enum_name: StrId,
        variant: StrId,
        argc: u16,
    },

    // ---- closures ----
    /// Create a closure from a tabled body, capturing the env.
    MakeClosure {
        func: FuncId,
    },
    /// Create a named function value, register it, push unit.
    MakeFunc {
        func: FuncId,
    },
    /// Fused `task.spawn(closure-literal)`; creation env captured.
    SpawnClosure {
        func: FuncId,
    },

    // ---- variants ----
    /// Pop an optional argument, push an Option/Result variant.
    MakeVariant {
        name: StrId,
        has_arg: bool,
    },

    // ---- pattern matching ----
    MatchArm {
        pat: Pattern,
        next: u32,
        has_env: bool,
        restore: bool,
    },
    MatchGuard {
        next: u32,
        has_env: bool,
    },
    MatchError,
    IfLetMatch {
        pat: Pattern,
        els: u32,
        has_env: bool,
    },
    /// Unwrap Option/Result or return the None/Err variant.
    TryOp,

    // ---- elvis ----
    Elvis,
    ElvisResult,

    // ---- calls ----
    /// Pop `argc` args and the callee; call; push the result.
    /// Callee, then args, left-to-right (§7).
    Call {
        argc: u16,
    },
    /// Dotted-path call with receiver-first method semantics.
    CallPath {
        parts: Vec<StrId>,
        argc: u16,
        pspan: Span,
    },
    /// Call a method on a slot-loaded receiver.
    CallMethod {
        name: StrId,
        argc: u16,
    },
    /// Direct native call by name, bypassing resolution.
    CallNative {
        name: StrId,
        argc: u16,
    },

    // ---- fmt ----
    /// Pop `n` values, push their concatenated Display forms.
    Concat(u16),
    /// Pop a format spec and a value, push the formatted string.
    FormatValue,
    /// Pop `nparams` bound values + template parts, push the SQL
    /// template then re-push each bound value in order.
    DbQuery {
        nparams: u16,
    },

    // ---- scopes ----
    EnterScope,
    ExitScope,
    /// Record a deferred closure (LIFO at scope exit).
    DeferRecord,
}

impl Op {
    /// Stable tag for the codec. Append-only: never reuse a tag.
    pub fn tag(&self) -> u16 {
        match self {
            Op::PushConst(_) => 0,
            Op::Pop => 1,
            Op::Swap => 74,
            Op::PopN(_) => 2,
            Op::Truthy => 3,
            Op::LoadVar(_) => 4,
            Op::LoadPath(_) => 5,
            Op::DefineVar(_) => 6,
            Op::StoreVar(_) => 7,
            Op::StorePath(_) => 8,
            Op::LoadSlot(_) => 9,
            Op::StoreSlot(_) => 10,
            Op::TakeVar(_) => 11,
            Op::VecPushField { .. } => 12,
            Op::VecPushMethod { .. } => 13,
            Op::IntAdd => 14,
            Op::IntSub => 15,
            Op::IntMul => 16,
            Op::IntDiv => 17,
            Op::IntRem => 18,
            Op::IntNeg => 19,
            Op::BinOp(_) => 20,
            Op::UnOp(_) => 21,
            Op::Jump(_) => 22,
            Op::JumpIfFalse(_) => 23,
            Op::JumpIfTrue(_) => 24,
            Op::JumpIfFalseBool(_) => 25,
            Op::Return => 26,
            Op::ForSetup { .. } => 27,
            Op::ForNext { .. } => 28,
            Op::WhileSetup { .. } => 29,
            Op::WhileCond { .. } => 30,
            Op::Break => 31,
            Op::Continue => 32,
            Op::SetLoopResult => 33,
            Op::Safepoint => 34,
            Op::MakeArray(_) => 35,
            Op::UnpackTuple(_) => 36,
            Op::ArrayPush => 37,
            Op::MakeDict(_) => 38,
            Op::IndexOp => 39,
            Op::StoreIndexOp => 40,
            Op::CompoundIndexOp { .. } => 41,
            Op::SliceOp => 42,
            Op::MakeRange => 43,
            Op::MakeStruct { .. } => 44,
            Op::GetField(_) => 45,
            Op::GetFieldIdx(_) => 46,
            Op::SetField(_) => 47,
            Op::SetFieldIdx(_) => 48,
            Op::CompoundFieldOp { .. } => 49,
            Op::RegisterStruct { .. } => 50,
            Op::RegisterEnum { .. } => 51,
            Op::MakeEnum { .. } => 52,
            Op::MakeClosure { .. } => 53,
            Op::MakeFunc { .. } => 54,
            Op::SpawnClosure { .. } => 55,
            Op::MakeVariant { .. } => 56,
            Op::MatchArm { .. } => 57,
            Op::MatchGuard { .. } => 58,
            Op::MatchError => 59,
            Op::IfLetMatch { .. } => 60,
            Op::TryOp => 61,
            Op::Elvis => 62,
            Op::ElvisResult => 63,
            Op::Call { .. } => 64,
            Op::CallPath { .. } => 65,
            Op::CallMethod { .. } => 66,
            Op::CallNative { .. } => 67,
            Op::Concat(_) => 68,
            Op::FormatValue => 69,
            Op::DbQuery { .. } => 70,
            Op::EnterScope => 71,
            Op::ExitScope => 72,
            Op::DeferRecord => 73,
        }
    }

    /// Short stable name for `zz dis`.
    pub fn name(&self) -> &'static str {
        match self {
            Op::PushConst(_) => "const",
            Op::Pop => "pop",
            Op::Swap => "swap",
            Op::PopN(_) => "popn",
            Op::Truthy => "truthy",
            Op::LoadVar(_) => "load",
            Op::LoadPath(_) => "loadpath",
            Op::DefineVar(_) => "define",
            Op::StoreVar(_) => "store",
            Op::StorePath(_) => "storepath",
            Op::LoadSlot(_) => "loadslot",
            Op::StoreSlot(_) => "storeslot",
            Op::TakeVar(_) => "take",
            Op::VecPushField { .. } => "vecpushfield",
            Op::VecPushMethod { .. } => "vecpushmethod",
            Op::IntAdd => "iadd",
            Op::IntSub => "isub",
            Op::IntMul => "imul",
            Op::IntDiv => "idiv",
            Op::IntRem => "irem",
            Op::IntNeg => "ineg",
            Op::BinOp(_) => "binop",
            Op::UnOp(_) => "unop",
            Op::Jump(_) => "jmp",
            Op::JumpIfFalse(_) => "jf",
            Op::JumpIfTrue(_) => "jt",
            Op::JumpIfFalseBool(_) => "jfb",
            Op::Return => "ret",
            Op::ForSetup { .. } => "forsetup",
            Op::ForNext { .. } => "fornext",
            Op::WhileSetup { .. } => "whilesetup",
            Op::WhileCond { .. } => "whilecond",
            Op::Break => "break",
            Op::Continue => "continue",
            Op::SetLoopResult => "setloopresult",
            Op::Safepoint => "safepoint",
            Op::MakeArray(_) => "makearray",
            Op::UnpackTuple(_) => "unpack",
            Op::ArrayPush => "arraypush",
            Op::MakeDict(_) => "makedict",
            Op::IndexOp => "index",
            Op::StoreIndexOp => "storeindex",
            Op::CompoundIndexOp { .. } => "compoundindex",
            Op::SliceOp => "slice",
            Op::MakeRange => "makerange",
            Op::MakeStruct { .. } => "makestruct",
            Op::GetField(_) => "getfield",
            Op::GetFieldIdx(_) => "getfieldidx",
            Op::SetField(_) => "setfield",
            Op::SetFieldIdx(_) => "setfieldidx",
            Op::CompoundFieldOp { .. } => "compoundfield",
            Op::RegisterStruct { .. } => "regstruct",
            Op::RegisterEnum { .. } => "regenum",
            Op::MakeEnum { .. } => "makeenum",
            Op::MakeClosure { .. } => "makeclosure",
            Op::MakeFunc { .. } => "makefunc",
            Op::SpawnClosure { .. } => "spawnclosure",
            Op::MakeVariant { .. } => "makevariant",
            Op::MatchArm { .. } => "matcharm",
            Op::MatchGuard { .. } => "matchguard",
            Op::MatchError => "matcherror",
            Op::IfLetMatch { .. } => "iflet",
            Op::TryOp => "try",
            Op::Elvis => "elvis",
            Op::ElvisResult => "elvisresult",
            Op::Call { .. } => "call",
            Op::CallPath { .. } => "callpath",
            Op::CallMethod { .. } => "callmethod",
            Op::CallNative { .. } => "callnative",
            Op::Concat(_) => "concat",
            Op::FormatValue => "formatvalue",
            Op::DbQuery { .. } => "dbquery",
            Op::EnterScope => "enterscope",
            Op::ExitScope => "exitscope",
            Op::DeferRecord => "defer",
        }
    }
}
