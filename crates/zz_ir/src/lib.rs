//! Canonical ZZ interchange model: the `.zzc` data model.
//!
//! A [`Module`] is the serialized form of one compiled ZZ program unit:
//! interned strings, a type table, a constant pool, and a function table
//! whose bodies are flat typed core [`Op`] streams. The binary encoding
//! (see [`codec`], spec §8: magic `ZZC1`, section table) is decoded and
//! [`verify`]ied before execution; `zz run --bytecode` executes only
//! bytes-derived data — no AST is available on the load path.
//!
//! Fused/optimizer ops from the VM compiler ([`TakeSlot`][vm], slot-int
//! fast paths, …) are desugared to core-op sequences by [`lower`] before
//! serialization, so the format only ever contains core ops. Ops with
//! dispatch/fallback semantics (`TakeVar`, `VecPushField`,
//! `VecPushMethod`, `SpawnClosure`) stay core: their behavior is not
//! expressible as a fixed core sequence.
//!
//! [vm]: zz_runtime::vm

pub mod codec;
pub mod dis;
pub mod lower;
pub mod op;
pub mod raise;
pub mod ty;
pub mod verify;

pub use op::{Op, Pattern};
pub use ty::IrType;

use std::fmt;

/// Magic bytes opening every `.zzc` file (spec §8).
pub const MAGIC: [u8; 4] = *b"ZZC1";
/// Format version this crate reads and writes (spec §8: this spec = 2).
/// v2 adds the per-function locals type table (`FUNCS` records); v1
/// files are rejected (re-emit with `zz build --emit-ir`).
pub const VERSION: u32 = 2;

/// Interned-string reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrId(pub u32);
/// Type-table reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeId(pub u32);
/// Constant-pool reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConstId(pub u32);
/// Function-table reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// Source span as raw offsets (frontend-independent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }
}

/// A compile-time constant. Only literals reach the pool — handles,
/// functions, and natives can never appear (rejected by [`lower`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(StrId),
    Array(Vec<ConstId>),
    Dict(Vec<(ConstId, ConstId)>),
    Option(Option<ConstId>),
    Result { ok: bool, val: ConstId },
}

/// A function parameter: interned name plus an optional default-value
/// chunk (compiled at lower time, executed in the caller's environment).
/// `None` means the parameter is required.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: StrId,
    pub default: Option<FuncId>,
}

/// A function signature: parameter types plus return type.
/// [`lower_typed`](crate::lower::lower_typed) resolves entries from HIR;
/// unknown slots (closures, entry, untyped compiles) stay
/// [`IrType::Unknown`].
#[derive(Debug, Clone, PartialEq)]
pub struct FuncSig {
    pub params: Vec<TypeId>,
    pub ret: TypeId,
}

/// One function body: params, signature, flat code with parallel spans,
/// and the verified maximum stack depth (sized by the AOT later).
#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: StrId,
    pub arity: u32,
    pub params: Vec<Param>,
    pub sig: FuncSig,
    /// Declared type per frame slot: params/locals recorded at typed
    /// compile time (see `vm::Chunk::slot_types`), params seeded from
    /// the signature. Length is one past the highest referenced slot id
    /// (empty when slotless). Entries over-approximate every value their
    /// slot ever holds (conflicts widen to `Error`, the "unknown" top);
    /// `Unknown` means "no information" (temps, untyped compiles). The
    /// verifier checks every store against its entry; backends decide
    /// representation from entries (never from inference).
    pub locals: Vec<TypeId>,
    /// Top-level vars promoted to frame slots, synced back to the
    /// environment at frame exit (REPL/multi-chunk flows).
    pub toplevel_slots: Vec<(StrId, u16)>,
    pub code: Vec<Op>,
    /// Same length as `code`.
    pub spans: Vec<Span>,
    /// Maximum operand-stack depth, computed by [`verify`].
    pub max_stack: u32,
}

/// A compiled program unit: tables plus an entry function.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub types: Vec<IrType>,
    pub strings: Vec<String>,
    pub consts: Vec<Const>,
    pub funcs: Vec<FuncDef>,
    pub entry: FuncId,
}

/// Any failure to decode, verify, lower, or raise IR.
#[derive(Debug, Clone, PartialEq)]
pub struct IrError {
    pub message: String,
    /// Bytecode span when the error has one (else `None`).
    pub span: Option<Span>,
}

impl IrError {
    pub fn new(message: impl Into<String>) -> Self {
        IrError {
            message: message.into(),
            span: None,
        }
    }

    pub fn spanned(message: impl Into<String>, span: Span) -> Self {
        IrError {
            message: message.into(),
            span: Some(span),
        }
    }
}

impl fmt::Display for IrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(s) => write!(f, "{} (bytecode {}..{})", self.message, s.start, s.end),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for IrError {}
