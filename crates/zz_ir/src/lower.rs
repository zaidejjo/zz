//! Lower a VM [`Chunk`][vm] to an IR [`Module`].
//!
//! Nested bodies (`MakeFunc` / `MakeClosure` / `SpawnClosure`) lift into
//! the function table in deterministic depth-first order; the top-level
//! chunk becomes the entry function. Fused/optimizer ops desugar to
//! core-op sequences before serialization:
//!
//! - `TakeSlot(s)` → `LoadSlot s; PushConst unit; StoreSlot s`
//! - `VecPush{home}` → take sequence, `ArrayPush`, store sequence
//! - `SlotAddInt/Inc/Imm/Less*/Binary*` → slot loads, generic `BinOp`,
//!   store (their non-int fallbacks already run generic `eval_binary`)
//!
//! `for` loops lower in the rotated v4 layout: the test sits at the bottom
//! and `ForNext` is both the test and the back-edge. Backends tuned to the
//! historical layout call [`derotate_loops`] before their own passes.
//!
//! Ops with dispatch/fallback semantics stay core: `TakeVar` (falls back
//! to the funcs/natives chain), `VecPushField` / `VecPushMethod`
//! (field/method dispatch), `SpawnClosure` (task-hook dispatch).
//!
//! Desugaring is behavior-identical on success paths. Two notes:
//! - fused ops report errors at `Span::default()` on their slow paths;
//!   desugared sequences report the real op span (message text is not
//!   parity-gated, spec §10);
//! - `VecPush` restores its home on a type error while the desugared
//!   sequence leaves `Unit` behind; both paths fail the program, so the
//!   difference is only observable through `defer` (accepted).
//!
//! [vm]: zz_runtime::vm

use std::collections::HashMap;

use zz_checker::{FuncSig as CheckerSig, Type as CheckerType};
use zz_frontend::ast::{
    BinOp as AstBinOp, Lit, Param as AstParam, Pattern as AstPattern, UnOp as AstUnOp,
};
use zz_runtime::vm::{Chunk as VmChunk, Op as VmOp, TakeHome};

use crate::op::BinOp as IrBinOp;
use crate::op::UnOp as IrUnOp;
use crate::{
    Const, ConstId, FuncDef, FuncId, FuncSig, IrError, IrType, Module, Op, Param, Pattern, Span,
    StrId, TypeId,
};

struct Lowerer<'a> {
    strings: Vec<String>,
    str_ids: HashMap<String, StrId>,
    consts: Vec<Const>,
    types: Vec<IrType>,
    funcs: Vec<FuncDef>,
    /// HIR function signatures by dotted name (guest for lookups;
    /// missing names lower to `Unknown`).
    func_sigs: &'a HashMap<String, CheckerSig>,
}

impl<'a> Lowerer<'a> {
    fn new(func_sigs: &'a HashMap<String, CheckerSig>) -> Self {
        Lowerer {
            strings: Vec::new(),
            str_ids: HashMap::new(),
            consts: Vec::new(),
            types: Vec::new(),
            funcs: Vec::new(),
            func_sigs,
        }
    }

    fn intern(&mut self, s: &str) -> StrId {
        if let Some(id) = self.str_ids.get(s) {
            return *id;
        }
        let id = StrId(self.strings.len() as u32);
        self.strings.push(s.to_string());
        self.str_ids.insert(s.to_string(), id);
        id
    }

    /// Intern a checker type into the type table (deduplicated).
    fn intern_type(&mut self, ty: &CheckerType) -> TypeId {
        let t = match ty {
            CheckerType::Int => IrType::Int,
            CheckerType::Float => IrType::Float,
            CheckerType::Bool => IrType::Bool,
            CheckerType::Str => IrType::Str,
            CheckerType::Unit => IrType::Unit,
            CheckerType::Void => IrType::Void,
            CheckerType::Bytes => IrType::Bytes,
            CheckerType::Json => IrType::Json,
            CheckerType::Db => IrType::Db,
            CheckerType::HttpServer => IrType::HttpServer,
            CheckerType::TcpStream => IrType::TcpStream,
            CheckerType::TcpListener => IrType::TcpListener,
            CheckerType::Response => IrType::Response,
            CheckerType::HttpRequest => IrType::HttpRequest,
            CheckerType::Chan => IrType::Chan,
            CheckerType::TaskJoin => IrType::TaskJoin,
            CheckerType::Error => IrType::Error,
            CheckerType::Tuple(items) => {
                IrType::Tuple(items.iter().map(|t| self.intern_type(t)).collect())
            }
            CheckerType::Option(inner) => IrType::Option(self.intern_type(inner)),
            CheckerType::Result(ok, err) => {
                IrType::Result(self.intern_type(ok), self.intern_type(err))
            }
            CheckerType::Func(params, ret) => IrType::Func(
                params.iter().map(|t| self.intern_type(t)).collect(),
                self.intern_type(ret),
            ),
            CheckerType::Array(inner) => IrType::Array(self.intern_type(inner)),
            CheckerType::Dict(k, v) => IrType::Dict(self.intern_type(k), self.intern_type(v)),
            CheckerType::Union(items) => {
                IrType::Union(items.iter().map(|t| self.intern_type(t)).collect())
            }
            CheckerType::Range(inner) => IrType::Range(self.intern_type(inner)),
            CheckerType::Opaque(tag) => IrType::Opaque(self.intern(tag)),
            CheckerType::Named(name) => IrType::Named(self.intern(name)),
            CheckerType::Var(n) => IrType::Var(*n),
            CheckerType::Struct(name, args) => IrType::Struct(
                self.intern(name),
                args.iter().map(|t| self.intern_type(t)).collect(),
            ),
            CheckerType::Enum(name, args) => IrType::Enum(
                self.intern(name),
                args.iter().map(|t| self.intern_type(t)).collect(),
            ),
            CheckerType::Ptr { mutable, inner } => IrType::Ptr {
                mutable: *mutable,
                inner: self.intern_type(inner),
            },
            // Inference leftovers and the bottom type carry no layout:
            // backends treat them as opaque (same as `Unknown`).
            CheckerType::Never => IrType::Unknown,
        };
        if let Some(i) = self.types.iter().position(|e| *e == t) {
            return TypeId(i as u32);
        }
        let id = TypeId(self.types.len() as u32);
        self.types.push(t);
        id
    }

    fn unknown(&mut self) -> TypeId {
        self.intern_type(&CheckerType::Never)
    }

    fn intern_const(&mut self, c: Const) -> ConstId {
        if let Some(i) = self.consts.iter().position(|e| *e == c) {
            return ConstId(i as u32);
        }
        let id = ConstId(self.consts.len() as u32);
        self.consts.push(c);
        id
    }

    fn unit_const(&mut self) -> ConstId {
        self.intern_const(Const::Unit)
    }

    fn int_const(&mut self, i: i64) -> ConstId {
        self.intern_const(Const::Int(i))
    }

    fn lower_value(&mut self, v: &zz_runtime::Value) -> Result<ConstId, IrError> {
        use zz_runtime::Value as V;
        match v {
            V::Unit => Ok(self.intern_const(Const::Unit)),
            V::Bool(b) => Ok(self.intern_const(Const::Bool(*b))),
            V::Int(i) => Ok(self.intern_const(Const::Int(*i))),
            V::Float(f) => Ok(self.intern_const(Const::Float(*f))),
            V::Str(s) => {
                let id = self.intern(s);
                Ok(self.intern_const(Const::Str(id)))
            }
            V::Array(items) => {
                let mut ids = Vec::with_capacity(items.len());
                for item in items.iter() {
                    ids.push(self.lower_value(item)?);
                }
                Ok(self.intern_const(Const::Array(ids)))
            }
            V::Dict(pairs) => {
                let mut out = Vec::with_capacity(pairs.len());
                for (k, val) in pairs.iter() {
                    out.push((self.lower_value(k)?, self.lower_value(val)?));
                }
                Ok(self.intern_const(Const::Dict(out)))
            }
            V::Option(inner) => {
                let inner = match inner {
                    Some(x) => Some(self.lower_value(x)?),
                    None => None,
                };
                Ok(self.intern_const(Const::Option(inner)))
            }
            V::Result(r) => match &**r {
                Ok(x) => {
                    let val = self.lower_value(x)?;
                    Ok(self.intern_const(Const::Result { ok: true, val }))
                }
                Err(x) => {
                    let val = self.lower_value(x)?;
                    Ok(self.intern_const(Const::Result { ok: false, val }))
                }
            },
            other => Err(IrError::new(format!(
                "cannot pool non-literal constant `{other}`"
            ))),
        }
    }

    fn lower_binop(op: &AstBinOp) -> IrBinOp {
        match op {
            AstBinOp::Add => IrBinOp::Add,
            AstBinOp::Sub => IrBinOp::Sub,
            AstBinOp::Mul => IrBinOp::Mul,
            AstBinOp::Div => IrBinOp::Div,
            AstBinOp::Rem => IrBinOp::Rem,
            AstBinOp::Pow => IrBinOp::Pow,
            AstBinOp::Eq => IrBinOp::Eq,
            AstBinOp::Ne => IrBinOp::Ne,
            AstBinOp::Lt => IrBinOp::Lt,
            AstBinOp::Gt => IrBinOp::Gt,
            AstBinOp::Le => IrBinOp::Le,
            AstBinOp::Ge => IrBinOp::Ge,
            AstBinOp::And => IrBinOp::And,
            AstBinOp::Or => IrBinOp::Or,
            AstBinOp::Elvis => IrBinOp::Elvis,
            AstBinOp::BitAnd => IrBinOp::BitAnd,
            AstBinOp::BitOr => IrBinOp::BitOr,
            AstBinOp::BitXor => IrBinOp::BitXor,
            AstBinOp::Shl => IrBinOp::Shl,
            AstBinOp::Shr => IrBinOp::Shr,
        }
    }

    fn lower_unop(op: &AstUnOp) -> IrUnOp {
        match op {
            AstUnOp::Neg => IrUnOp::Neg,
            AstUnOp::Pos => IrUnOp::Pos,
            AstUnOp::Not => IrUnOp::Not,
            AstUnOp::BitNot => IrUnOp::BitNot,
        }
    }

    fn lower_lit(&mut self, lit: &Lit) -> Result<ConstId, IrError> {
        match lit {
            Lit::Int(i) => Ok(self.int_const(*i)),
            Lit::Float(f) => Ok(self.intern_const(Const::Float(*f))),
            Lit::Str(s) => {
                let id = self.intern(s);
                Ok(self.intern_const(Const::Str(id)))
            }
            Lit::Bool(b) => Ok(self.intern_const(Const::Bool(*b))),
        }
    }

    fn lower_pattern(&mut self, pat: &AstPattern) -> Result<Pattern, IrError> {
        match pat {
            AstPattern::Wildcard { .. } => Ok(Pattern::Wildcard),
            AstPattern::Binding { name } => Ok(Pattern::Binding(self.intern(&name.name))),
            AstPattern::Literal { value, .. } => Ok(Pattern::Lit(self.lower_lit(value)?)),
            AstPattern::Variant { name, arg, .. } => Ok(Pattern::Variant {
                name: self.intern(name),
                arg: match arg {
                    Some(p) => Some(Box::new(self.lower_pattern(p)?)),
                    None => None,
                },
            }),
            AstPattern::Tuple { pats, .. } => {
                let mut out = Vec::with_capacity(pats.len());
                for p in pats {
                    out.push(self.lower_pattern(p)?);
                }
                Ok(Pattern::Tuple(out))
            }
            AstPattern::Or { pats, .. } => {
                let mut out = Vec::with_capacity(pats.len());
                for p in pats {
                    out.push(self.lower_pattern(p)?);
                }
                Ok(Pattern::Or(out))
            }
        }
    }

    fn lower_home(&mut self, home: &TakeHome) -> (Option<u16>, Option<StrId>) {
        match home {
            TakeHome::Slot(s) => (Some(*s), None),
            TakeHome::Env(name) => (None, Some(self.intern(name))),
        }
    }

    /// Lift a params list, compiling AST defaults to tabled chunks.
    /// Lift a params list, compiling AST defaults to tabled chunks. A
    /// default chunk takes no params and evaluates to its parameter's
    /// type when the owner signature provides one.
    fn lower_params(
        &mut self,
        params: &[AstParam],
        owner: Option<&CheckerSig>,
    ) -> Result<Vec<Param>, IrError> {
        let mut out = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let default = match &p.default {
                Some(expr) => {
                    let chunk = zz_runtime::vm::Compiler::compile_default_expr(expr);
                    let id = self.lift_chunk(&chunk, &p.name.name, 0)?;
                    // Stamp the default's return type from the owner.
                    let ret = owner
                        .and_then(|s| s.params.get(i))
                        .map(|(_, t)| self.intern_type(t))
                        .unwrap_or_else(|| self.unknown());
                    let f = &mut self.funcs[id.0 as usize];
                    f.sig = FuncSig {
                        params: vec![],
                        ret,
                    };
                    Some(id)
                }
                None => None,
            };
            out.push(Param {
                name: self.intern(&p.name.name),
                default,
            });
        }
        Ok(out)
    }

    /// Signature for a lifted function: HIR lookup by dotted name,
    /// `Unknown` when absent (closures, entry, untyped compiles).
    /// Sealing also stamps the locals table's param slots from the
    /// signature (params are authoritative for calls; the recorded
    /// entries can only agree on valid code).
    fn seal_sig(&mut self, id: FuncId, name: Option<&str>) {
        let arity = self.funcs[id.0 as usize].params.len();
        let found = name.and_then(|n| self.func_sigs.get(n).cloned());
        let sig = match found {
            Some(s) => FuncSig {
                params: s.params.iter().map(|(_, t)| self.intern_type(t)).collect(),
                ret: self.intern_type(&s.ret),
            },
            None => {
                let u = self.unknown();
                FuncSig {
                    params: vec![u; arity],
                    ret: u,
                }
            }
        };
        let f = &mut self.funcs[id.0 as usize];
        f.sig = sig;
        f.arity = arity as u32;
        // Seed param slots from the sealed signature.
        let seeded: Vec<TypeId> = f.sig.params.clone();
        for (i, t) in seeded.into_iter().enumerate() {
            if i < f.locals.len() {
                f.locals[i] = t;
            }
        }
    }

    /// Lift one VM chunk (plus everything nested inside it) into the
    /// function table. Returns the new function's id. The caller seals
    /// params and signature afterwards (except defaults, sealed inline).
    fn lift_chunk(&mut self, chunk: &VmChunk, name: &str, arity: u32) -> Result<FuncId, IrError> {
        // Index reservation first so recursive lifts nest in order.
        let id = FuncId(self.funcs.len() as u32);
        self.funcs.push(FuncDef {
            name: StrId(u32::MAX),
            arity,
            params: Vec::new(),
            sig: FuncSig {
                params: Vec::new(),
                ret: TypeId(u32::MAX),
            },
            toplevel_slots: Vec::new(),
            locals: Vec::new(),
            vartab: Vec::new(),
            code: Vec::new(),
            spans: Vec::new(),
            max_stack: 0,
        });
        let expanded = self.expand_chunk(chunk)?;
        let vm_spans: Vec<Span> = chunk
            .spans
            .iter()
            .map(|s| Span::new(s.start, s.end))
            .collect();
        let (code, spans) = remap_and_flatten(expanded, &vm_spans)?;
        let max_stack = crate::verify::max_stack_for(&code).map_err(|e| {
            let head: Vec<String> = code.iter().take(6).map(|o| format!("{o:?}")).collect();
            IrError::new(format!(
                "cannot lower {name}: {} [head: {}]",
                e.message,
                head.join(" | ")
            ))
        })?;
        let toplevel_slots = chunk
            .toplevel_slots
            .iter()
            .map(|(n, s)| (self.intern(n), *s))
            .collect();
        // Locals table: intern the compiler-recorded slot types, sized
        // to one past the highest referenced slot id (params are seeded
        // from the signature later, in `seal_sig`), and floored at the
        // signature arity. Arity is a property of the signature: a
        // parameter the body never reads emits no `LoadSlot`/`StoreSlot`,
        // so sizing from referenced slots alone can leave the table
        // shorter than the parameter list, which `verify` then rejects
        // ("locals: fewer slots than signature params").
        let mut max_slot: Option<usize> = None;
        for op in &code {
            if let Some(s) = op.slot_extent() {
                max_slot = Some(max_slot.map_or(s as usize, |m: usize| m.max(s as usize)));
            }
        }
        for (_, s) in chunk.toplevel_slots.iter() {
            max_slot = Some(max_slot.map_or(*s as usize, |m: usize| m.max(*s as usize)));
        }
        let unknown = self.unknown();
        let local_count = max_slot.map_or(0, |m| m + 1).max(arity as usize);
        let mut locals = vec![unknown; local_count];
        for (i, recorded) in chunk.slot_types.iter().enumerate() {
            if let (Some(dst), Some(ty)) = (locals.get_mut(i), recorded) {
                *dst = self.intern_type(ty);
            }
        }
        let name_id = self.intern(name);
        let f = &mut self.funcs[id.0 as usize];
        f.name = name_id;
        f.code = code;
        f.spans = spans;
        f.max_stack = max_stack;
        f.toplevel_slots = toplevel_slots;
        f.locals = locals;
        // vartab: kth `ForNext` VmOp ↔ kth recorded slot vec, in chunk
        // order (nested lifts carry their own tables, so order is stable
        // within one lift). Count mismatch is a compiler bug: fail here,
        // never silently misalign.
        let mut vartab = Vec::new();
        let mut slots = chunk.fornext_slots.iter();
        for op in &chunk.code {
            if matches!(op, zz_runtime::vm::Op::ForNext { .. }) {
                match slots.next() {
                    Some(v) => vartab.push(v.clone()),
                    None => {
                        return Err(IrError::new(format!(
                            "cannot lower {name}: fornext_slots shorter than ForNext ops"
                        )));
                    }
                }
            }
        }
        if slots.next().is_some() {
            return Err(IrError::new(format!(
                "cannot lower {name}: fornext_slots longer than ForNext ops"
            )));
        }
        f.vartab = vartab;
        Ok(id)
    }

    /// Expand one op into core ops (desugaring fused ops inline).
    /// `consts` is the owning chunk's pool, resolving `PushConst`.
    fn expand_op(&mut self, op: &VmOp, consts: &[zz_runtime::Value]) -> Result<Vec<Op>, IrError> {
        match op {
            VmOp::PushConst(i) => {
                let value = consts
                    .get(*i as usize)
                    .ok_or_else(|| IrError::new("PushConst index out of range"))?;
                Ok(vec![Op::PushConst(self.lower_value(value)?)])
            }
            VmOp::Pop => Ok(vec![Op::Pop]),
            VmOp::Swap => Ok(vec![Op::Swap]),
            VmOp::PopN { n, .. } => Ok(vec![Op::PopN(*n)]),
            VmOp::Truthy => Ok(vec![Op::Truthy]),
            VmOp::LoadVar(name, _) => Ok(vec![Op::LoadVar(self.intern(name))]),
            VmOp::LoadPath(parts, _) => Ok(vec![Op::LoadPath(
                parts.iter().map(|p| self.intern(p)).collect(),
            )]),
            VmOp::DefineVar(name) => Ok(vec![Op::DefineVar(self.intern(name))]),
            VmOp::StoreVar(name, _) => Ok(vec![Op::StoreVar(self.intern(name))]),
            VmOp::StorePath(parts, _) => Ok(vec![Op::StorePath(
                parts.iter().map(|p| self.intern(p)).collect(),
            )]),
            VmOp::LoadSlot(s) => Ok(vec![Op::LoadSlot(*s)]),
            VmOp::StoreSlot(s) => Ok(vec![Op::StoreSlot(*s)]),
            VmOp::TakeSlot(s) => {
                let unit = self.unit_const();
                Ok(vec![
                    Op::LoadSlot(*s),
                    Op::PushConst(unit),
                    Op::StoreSlot(*s),
                ])
            }
            VmOp::TakeVar(name, _) => Ok(vec![Op::TakeVar(self.intern(name))]),
            VmOp::VecPush { home, .. } => {
                let (slot, var) = self.lower_home(home);
                let unit = self.unit_const();
                // Stack starts [elem]: take the home (leaving Unit, like
                // the fused op), Swap to [home, elem] for ArrayPush, then
                // store back.
                let mut seq = Vec::with_capacity(6);
                match (slot, var) {
                    (Some(s), _) => {
                        seq.push(Op::LoadSlot(s));
                        seq.push(Op::PushConst(unit));
                        seq.push(Op::StoreSlot(s));
                    }
                    (_, Some(v)) => {
                        seq.push(Op::LoadVar(v));
                        seq.push(Op::PushConst(unit));
                        seq.push(Op::StoreVar(v));
                    }
                    (None, None) => return Err(IrError::new("VecPush without a home")),
                }
                seq.push(Op::Swap);
                seq.push(Op::ArrayPush);
                match home {
                    TakeHome::Slot(s) => seq.push(Op::StoreSlot(*s)),
                    TakeHome::Env(name) => seq.push(Op::StoreVar(self.intern(name))),
                }
                Ok(seq)
            }
            VmOp::VecPushField { home, field, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::VecPushField {
                    home_slot,
                    home_var,
                    field: self.intern(field),
                }])
            }
            VmOp::VecPushMethod { home, method, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::VecPushMethod {
                    home_slot,
                    home_var,
                    method: self.intern(method),
                }])
            }
            VmOp::SetFieldTake { home, field, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::SetFieldTake {
                    home_slot,
                    home_var,
                    field: self.intern(field),
                }])
            }
            VmOp::SetFieldIndexTake { home, field, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::SetFieldIndexTake {
                    home_slot,
                    home_var,
                    field: self.intern(field),
                }])
            }
            VmOp::StoreIndexTake { home, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::StoreIndexTake {
                    home_slot,
                    home_var,
                }])
            }
            VmOp::CompoundIndexTake { home, op, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::CompoundIndexTake {
                    home_slot,
                    home_var,
                    op: Self::lower_binop(op),
                }])
            }
            VmOp::FieldBinopTake {
                home, field, op, ..
            } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::FieldBinopTake {
                    home_slot,
                    home_var,
                    field: self.intern(field),
                    op: Self::lower_binop(op),
                }])
            }
            VmOp::GetFieldIndex { home, field, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::GetFieldIndex {
                    home_slot,
                    home_var,
                    field: self.intern(field),
                }])
            }
            VmOp::LenField { home, field, .. } => {
                let (home_slot, home_var) = self.lower_home(home);
                Ok(vec![Op::LenField {
                    home_slot,
                    home_var,
                    field: self.intern(field),
                }])
            }
            // Fused slot fast paths travel as ops (v3): the interpreter
            // runs them natively, and the desugared form cost it 3 extra
            // stack ops per loop iteration on cached runs. The expansion
            // lives in `expand_fused`, which the chunk→C backend applies
            // before its own analyses.
            VmOp::SlotAddInt { dst, src } => Ok(vec![Op::SlotAddInt {
                dst: *dst,
                src: *src,
            }]),
            VmOp::SlotInc { slot } => Ok(vec![Op::SlotInc { slot: *slot }]),
            VmOp::SlotAddIntImm { dst, imm } => Ok(vec![Op::SlotAddIntImm {
                dst: *dst,
                imm: *imm,
            }]),
            VmOp::SlotLessIntSlot { a, b } => Ok(vec![Op::SlotLessIntSlot { a: *a, b: *b }]),
            VmOp::SlotLessIntImm { a, imm } => Ok(vec![Op::SlotLessIntImm { a: *a, imm: *imm }]),
            VmOp::SlotCmpIntSlot { a, b, op } => Ok(vec![Op::SlotCmpIntSlot {
                a: *a,
                b: *b,
                op: Self::lower_binop(op),
            }]),
            VmOp::SlotCmpIntImm { a, imm, op } => Ok(vec![Op::SlotCmpIntImm {
                a: *a,
                imm: *imm,
                op: Self::lower_binop(op),
            }]),
            VmOp::SlotCmpConst { a, k, op } => {
                let value = consts
                    .get(*k as usize)
                    .ok_or_else(|| IrError::new("SlotCmpConst index out of range"))?;
                Ok(vec![Op::SlotCmpConst {
                    a: *a,
                    k: self.lower_value(value)?,
                    op: Self::lower_binop(op),
                }])
            }
            VmOp::SlotBinaryInt { dst, lhs, rhs, op } => Ok(vec![Op::SlotBinaryInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
                op: Self::lower_binop(op),
            }]),
            VmOp::SlotBinaryIntImm { dst, lhs, imm, op } => Ok(vec![Op::SlotBinaryIntImm {
                dst: *dst,
                lhs: *lhs,
                imm: *imm,
                op: Self::lower_binop(op),
            }]),
            VmOp::MakeFunc {
                name,
                params,
                chunk,
                ..
            } => {
                let id = self.lift_chunk(chunk, name, params.len() as u32)?;
                let owner = self.func_sigs.get(name).cloned();
                let lowered = self.lower_params(params, owner.as_ref())?;
                self.funcs[id.0 as usize].params = lowered;
                self.seal_sig(id, Some(name));
                Ok(vec![Op::MakeFunc { func: id }])
            }
            VmOp::RegisterStruct { name, fields } => Ok(vec![Op::RegisterStruct {
                name: self.intern(name),
                fields: fields.iter().map(|f| self.intern(f)).collect(),
            }]),
            VmOp::RegisterEnum { name, variants } => Ok(vec![Op::RegisterEnum {
                name: self.intern(name),
                variants: variants.iter().map(|(v, a)| (self.intern(v), *a)).collect(),
            }]),
            VmOp::MakeEnum {
                enum_name,
                variant,
                argc,
                ..
            } => Ok(vec![Op::MakeEnum {
                enum_name: self.intern(enum_name),
                variant: self.intern(variant),
                argc: *argc,
            }]),
            VmOp::IntAdd(_) => Ok(vec![Op::IntAdd]),
            VmOp::IntSub(_) => Ok(vec![Op::IntSub]),
            VmOp::IntMul(_) => Ok(vec![Op::IntMul]),
            VmOp::IntDiv(_) => Ok(vec![Op::IntDiv]),
            VmOp::IntRem(_) => Ok(vec![Op::IntRem]),
            VmOp::IntNeg(_) => Ok(vec![Op::IntNeg]),
            // Typed int comparisons (#364) are VM-interpreter-only: the
            // chunk→C backend already specializes int comparisons inside
            // generic `BinOp`, so the IR keeps the generic form (zero
            // codec/chunk/verify churn, no .zzc compat risk). `zz run`
            // executes `VmOp` directly and takes the fast path.
            VmOp::IntLt(_) => Ok(vec![Op::BinOp(Self::lower_binop(&AstBinOp::Lt))]),
            VmOp::IntGt(_) => Ok(vec![Op::BinOp(Self::lower_binop(&AstBinOp::Gt))]),
            VmOp::IntLe(_) => Ok(vec![Op::BinOp(Self::lower_binop(&AstBinOp::Le))]),
            VmOp::IntGe(_) => Ok(vec![Op::BinOp(Self::lower_binop(&AstBinOp::Ge))]),
            VmOp::IntEq(_) => Ok(vec![Op::BinOp(Self::lower_binop(&AstBinOp::Eq))]),
            VmOp::IntNe(_) => Ok(vec![Op::BinOp(Self::lower_binop(&AstBinOp::Ne))]),
            VmOp::BinOp(op, _) => Ok(vec![Op::BinOp(Self::lower_binop(op))]),
            VmOp::UnOp(op, _) => Ok(vec![Op::UnOp(Self::lower_unop(op))]),
            VmOp::Jump(t) => Ok(vec![Op::Jump(*t as u32)]),
            VmOp::JumpIfFalse(t) => Ok(vec![Op::JumpIfFalse(*t as u32)]),
            VmOp::JumpIfTrue(t) => Ok(vec![Op::JumpIfTrue(*t as u32)]),
            VmOp::JumpIfFalseBool(t, _) => Ok(vec![Op::JumpIfFalseBool(*t as u32)]),
            VmOp::Return => Ok(vec![Op::Return]),
            VmOp::ForSetup {
                exit,
                header,
                span: _,
                num_vars,
            } => Ok(vec![Op::ForSetup {
                exit: *exit as u32,
                header: *header as u32,
                num_vars: *num_vars,
            }]),
            VmOp::ForNext {
                vars,
                exit,
                body,
                in_env,
                ..
            } => Ok(vec![Op::ForNext {
                vars: vars.iter().map(|v| self.intern(v)).collect(),
                exit: *exit as u32,
                body: *body as u32,
                in_env: *in_env,
            }]),
            VmOp::WhileSetup { exit, header } => Ok(vec![Op::WhileSetup {
                exit: *exit as u32,
                header: *header as u32,
            }]),
            VmOp::WhileCond { exit, .. } => Ok(vec![Op::WhileCond { exit: *exit as u32 }]),
            VmOp::Break(_) => Ok(vec![Op::Break]),
            VmOp::Continue(_) => Ok(vec![Op::Continue]),
            VmOp::SetLoopResult => Ok(vec![Op::SetLoopResult]),
            VmOp::Safepoint => Ok(vec![Op::Safepoint]),
            VmOp::MakeArray(n) => Ok(vec![Op::MakeArray(*n)]),
            VmOp::UnpackTuple(n) => Ok(vec![Op::UnpackTuple(*n)]),
            VmOp::ArrayPush(_) => Ok(vec![Op::ArrayPush]),
            VmOp::MakeDict(n) => Ok(vec![Op::MakeDict(*n)]),
            VmOp::IndexOp(_) => Ok(vec![Op::IndexOp]),
            VmOp::StoreIndexOp(_) => Ok(vec![Op::StoreIndexOp]),
            VmOp::CompoundIndexOp { op, .. } => Ok(vec![Op::CompoundIndexOp {
                op: Self::lower_binop(op),
            }]),
            VmOp::SliceOp(_) => Ok(vec![Op::SliceOp]),
            VmOp::MakeRange(_) => Ok(vec![Op::MakeRange]),
            VmOp::MakeStruct {
                name, field_names, ..
            } => Ok(vec![Op::MakeStruct {
                name: self.intern(name),
                fields: field_names.iter().map(|f| self.intern(f)).collect(),
            }]),
            VmOp::GetField(name, _) => Ok(vec![Op::GetField(self.intern(name))]),
            VmOp::GetFieldIdx(i, _) => Ok(vec![Op::GetFieldIdx(*i)]),
            VmOp::SetField(name, _) => Ok(vec![Op::SetField(self.intern(name))]),
            VmOp::SetFieldIdx(i, _) => Ok(vec![Op::SetFieldIdx(*i)]),
            VmOp::CompoundFieldOp { name, op, .. } => Ok(vec![Op::CompoundFieldOp {
                name: self.intern(name),
                op: Self::lower_binop(op),
            }]),
            VmOp::MakeClosure { params, chunk, .. } => {
                let id = self.lift_chunk(chunk, "closure", params.len() as u32)?;
                let lowered = self.lower_params(params, None)?;
                self.funcs[id.0 as usize].params = lowered;
                self.seal_sig(id, None);
                Ok(vec![Op::MakeClosure { func: id }])
            }
            VmOp::SpawnClosure { params, chunk, .. } => {
                let id = self.lift_chunk(chunk, "spawn", params.len() as u32)?;
                let lowered = self.lower_params(params, None)?;
                self.funcs[id.0 as usize].params = lowered;
                self.seal_sig(id, None);
                Ok(vec![Op::SpawnClosure { func: id }])
            }
            VmOp::MakeVariant { name, has_arg, .. } => Ok(vec![Op::MakeVariant {
                name: self.intern(name),
                has_arg: *has_arg,
            }]),
            VmOp::MatchArm {
                pat,
                next,
                has_env,
                restore,
            } => Ok(vec![Op::MatchArm {
                pat: self.lower_pattern(pat)?,
                next: *next as u32,
                has_env: *has_env,
                restore: *restore,
            }]),
            VmOp::MatchGuard { next, has_env } => Ok(vec![Op::MatchGuard {
                next: *next as u32,
                has_env: *has_env,
            }]),
            VmOp::MatchError(_) => Ok(vec![Op::MatchError]),
            VmOp::IfLetMatch { pat, els, has_env } => Ok(vec![Op::IfLetMatch {
                pat: self.lower_pattern(pat)?,
                els: *els as u32,
                has_env: *has_env,
            }]),
            VmOp::TryOp(_) => Ok(vec![Op::TryOp]),
            VmOp::Elvis(_) => Ok(vec![Op::Elvis]),
            VmOp::ElvisResult => Ok(vec![Op::ElvisResult]),
            VmOp::Call { argc, .. } => Ok(vec![Op::Call { argc: *argc }]),
            VmOp::CallPath {
                parts, argc, pspan, ..
            } => Ok(vec![Op::CallPath {
                parts: parts.iter().map(|p| self.intern(p)).collect(),
                argc: *argc,
                pspan: Span::new(pspan.start, pspan.end),
            }]),
            VmOp::CallMethod { name, argc, .. } => Ok(vec![Op::CallMethod {
                name: self.intern(name),
                argc: *argc,
            }]),
            VmOp::CallNative { name, argc, .. } => Ok(vec![Op::CallNative {
                name: self.intern(name),
                argc: *argc,
            }]),
            VmOp::Concat(n) => Ok(vec![Op::Concat(*n)]),
            VmOp::FormatValue(_) => Ok(vec![Op::FormatValue]),
            VmOp::DbQuery { nparams, .. } => Ok(vec![Op::DbQuery { nparams: *nparams }]),
            VmOp::EnterScope => Ok(vec![Op::EnterScope]),
            VmOp::ExitScope => Ok(vec![Op::ExitScope]),
            VmOp::DeferRecord => Ok(vec![Op::DeferRecord]),
        }
    }

    /// Expand a whole chunk, remapping absolute jump targets across
    /// desugared expansions. Pool indices in `PushConst` resolve against
    /// the owning chunk's constants here.
    fn expand_chunk(&mut self, chunk: &VmChunk) -> Result<Vec<Vec<Op>>, IrError> {
        let mut out = Vec::with_capacity(chunk.code.len());
        for op in &chunk.code {
            out.push(self.expand_op(op, &chunk.constants)?);
        }
        Ok(out)
    }
}

/// Flatten per-op expansions into a single op stream, remapping absolute
/// jump targets (they address the pre-expansion stream) and stamping every
/// produced op with the span of the op it came from. Shared by the VM
/// lowering and by [`expand_fused`], so both remap exactly the same set.
fn remap_and_flatten(
    expanded: Vec<Vec<Op>>,
    old_spans: &[Span],
) -> Result<(Vec<Op>, Vec<Span>), IrError> {
    // Old pc -> new pc (expansion starts).
    let mut base: Vec<u32> = Vec::with_capacity(expanded.len() + 1);
    let mut at = 0u32;
    for seq in &expanded {
        base.push(at);
        at += seq.len() as u32;
    }
    base.push(at);
    let mut code = Vec::with_capacity(at as usize);
    let mut spans = Vec::with_capacity(at as usize);
    for (old_pc, seq) in expanded.into_iter().enumerate() {
        let span = old_spans
            .get(old_pc)
            .copied()
            .unwrap_or_else(|| Span::new(0, 0));
        for mut op in seq {
            let targets: Option<Vec<u32>> = match &op {
                Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
                    Some(vec![*t])
                }
                Op::ForSetup { exit, .. } => Some(vec![*exit]),
                Op::ForNext { exit, body, .. } => Some(vec![*exit, *body]),
                Op::WhileSetup { exit, .. } => Some(vec![*exit]),
                Op::WhileCond { exit } => Some(vec![*exit]),
                Op::MatchArm { next, .. } => Some(vec![*next]),
                Op::MatchGuard { next, .. } => Some(vec![*next]),
                Op::IfLetMatch { els, .. } => Some(vec![*els]),
                _ => None,
            };
            if let Some(ts) = targets {
                let nt: Vec<u32> = ts
                    .iter()
                    .map(|t| {
                        base.get(*t as usize)
                            .copied()
                            .ok_or_else(|| IrError::new("jump target out of range"))
                    })
                    .collect::<Result<_, _>>()?;
                op = match op {
                    Op::Jump(_) => Op::Jump(nt[0]),
                    Op::JumpIfFalse(_) => Op::JumpIfFalse(nt[0]),
                    Op::JumpIfTrue(_) => Op::JumpIfTrue(nt[0]),
                    Op::JumpIfFalseBool(_) => Op::JumpIfFalseBool(nt[0]),
                    Op::ForSetup {
                        header, num_vars, ..
                    } => Op::ForSetup {
                        exit: nt[0],
                        header: base
                            .get(header as usize)
                            .copied()
                            .ok_or_else(|| IrError::new("loop header out of range"))?,
                        num_vars,
                    },
                    Op::ForNext {
                        vars,
                        body: _,
                        in_env,
                        ..
                    } => Op::ForNext {
                        vars,
                        exit: nt[0],
                        body: nt[1],
                        in_env,
                    },
                    Op::WhileSetup { header, .. } => Op::WhileSetup {
                        exit: nt[0],
                        header: base
                            .get(header as usize)
                            .copied()
                            .ok_or_else(|| IrError::new("loop header out of range"))?,
                    },
                    Op::WhileCond { .. } => Op::WhileCond { exit: nt[0] },
                    Op::MatchArm {
                        pat,
                        has_env,
                        restore,
                        ..
                    } => Op::MatchArm {
                        pat,
                        next: nt[0],
                        has_env,
                        restore,
                    },
                    Op::MatchGuard { has_env, .. } => Op::MatchGuard {
                        next: nt[0],
                        has_env,
                    },
                    Op::IfLetMatch { pat, has_env, .. } => Op::IfLetMatch {
                        pat,
                        els: nt[0],
                        has_env,
                    },
                    other => other,
                };
            }
            code.push(op);
            spans.push(span);
        }
    }
    Ok((code, spans))
}

/// Lower a compiled VM chunk to a verified IR module (untyped:
/// every signature slot is `Unknown`).
pub fn lower(chunk: &VmChunk) -> Result<Module, IrError> {
    lower_typed(chunk, &HashMap::new())
}

/// Lower with HIR signatures: each lifted function resolves its
/// signature by dotted name, falling back to `Unknown` per slot.
/// Default-argument chunks take their return type from the owner.
pub fn lower_typed(
    chunk: &VmChunk,
    func_sigs: &HashMap<String, CheckerSig>,
) -> Result<Module, IrError> {
    let mut l = Lowerer::new(func_sigs);
    l.funcs.reserve(16);
    let entry = l.lift_chunk(chunk, "main", 0)?;
    debug_assert_eq!(entry, FuncId(0));
    // The entry chunk is the program top level (never a function
    // body): Unknown signature even if a user `main` exists.
    l.seal_sig(entry, None);
    let module = Module {
        types: std::mem::take(&mut l.types),
        strings: std::mem::take(&mut l.strings),
        consts: std::mem::take(&mut l.consts),
        funcs: std::mem::take(&mut l.funcs),
        entry,
    };
    crate::verify::verify(&module)?;
    Ok(module)
}

/// True for the fused slot fast paths the format carries (v3).
fn is_fused_slot_op(op: &Op) -> bool {
    matches!(
        op,
        Op::SlotAddInt { .. }
            | Op::SlotInc { .. }
            | Op::SlotAddIntImm { .. }
            | Op::SlotLessIntSlot { .. }
            | Op::SlotLessIntImm { .. }
            | Op::SlotCmpIntSlot { .. }
            | Op::SlotCmpIntImm { .. }
            | Op::SlotCmpConst { .. }
            | Op::SlotBinaryInt { .. }
            | Op::SlotBinaryIntImm { .. }
    )
}

/// Intern an int literal (deduplicated, like the lowering pool).
fn intern_int(consts: &mut Vec<Const>, i: i64) -> ConstId {
    let c = Const::Int(i);
    if let Some(p) = consts.iter().position(|e| *e == c) {
        return ConstId(p as u32);
    }
    let id = ConstId(consts.len() as u32);
    consts.push(c);
    id
}

/// Undo the v4 rotated `for` layout for backends that expect the
/// historical shape. Borrows the input when every loop already falls
/// through (`ForNext.body == pc + 1`).
///
/// The VM compiler emits each `for` loop rotated — the entry jumps to the
/// test at the bottom, and a live `ForNext` jumps forward into the body:
///
/// ```text
///   ForSetup{exit, header = check}
///   Jump(check)                    ; entry
/// body_start:
///   <body>
/// check:
///   ForNext{body = body_start, exit}
/// ```
///
/// The chunk→C backend's int-twin / peel / cond-fuse passes recognize the
/// historical layout (`ForNext` header, straight-line body, `Jump` back
/// edge), so this pass swaps the two op positions back:
///
/// ```text
///   ForSetup{exit, header = check}
/// check:
///   ForNext{body = check + 1, exit}  ; falls through into the body
///   <body>
///   Jump(check)
/// ```
///
/// The swap is position-preserving (`ForNext.body` is already the
/// fall-through pc) and every pc-carrying field is retargeted, so the
/// result is the pre-v4 stream — same op count, same spans. `vartab` (the
/// per-`ForNext` loop-var slot table the peeler indexes by op order) is
/// reordered with the ops: nesting reverses the pair, the inner test being
/// emitted first in the rotated layout.
pub fn derotate_loops(module: &Module) -> Result<std::borrow::Cow<'_, Module>, IrError> {
    let rotated = module.funcs.iter().any(|f| {
        f.code
            .iter()
            .enumerate()
            .any(|(pc, op)| matches!(op, Op::ForNext { body, .. } if (*body as usize) <= pc))
    });
    if !rotated {
        return Ok(std::borrow::Cow::Borrowed(module));
    }
    let mut out = module.clone();
    for f in &mut out.funcs {
        // Rotated loops as (entry `Jump` pc, `ForNext` pc).
        let mut swaps: Vec<(usize, usize)> = Vec::new();
        for (pc, op) in f.code.iter().enumerate() {
            let Op::ForNext { body, .. } = op else {
                continue;
            };
            let body = *body as usize;
            if body == pc + 1 {
                // Already the historical layout: falls through.
                continue;
            }
            if body > pc {
                return Err(IrError::new(format!(
                    "cannot derotate loop: `ForNext` at {pc} has a forward body {body}"
                )));
            }
            let entry = body.checked_sub(1);
            let entry =
                entry.filter(|e| matches!(f.code.get(*e), Some(Op::Jump(t)) if *t as usize == pc));
            // A rotated loop whose entry jump is missing cannot happen in
            // the compiler's output; fail closed instead of handing a
            // backend a shape its peeler would misread.
            let Some(entry) = entry else {
                return Err(IrError::new(format!(
                    "cannot derotate loop: `ForNext` at {pc} has body {body} but no entry `Jump`"
                )));
            };
            swaps.push((entry, pc));
        }
        if swaps.is_empty() {
            continue;
        }
        // vartab is indexed by `ForNext` order, and the swap can reverse a
        // nested pair (the inner test is emitted first in the rotated
        // layout). Record the old order so it can follow the ops.
        let fornext_pcs: Vec<usize> = f
            .code
            .iter()
            .enumerate()
            .filter(|(_, op)| matches!(op, Op::ForNext { .. }))
            .map(|(pc, _)| pc)
            .collect();
        if f.vartab.len() != fornext_pcs.len() {
            return Err(IrError::new(
                "cannot derotate loop: vartab length does not match `ForNext` count",
            ));
        }
        // Old pc -> new pc for every pc-carrying field. A target that
        // named the test follows it to the entry slot (the historical
        // `ForNext` header). One-past-the-end stays terminal.
        let mut map: Vec<u32> = (0..=f.code.len() as u32).collect();
        for &(entry, check) in &swaps {
            map[check] = entry as u32;
        }
        for &(entry, check) in &swaps {
            let test = std::mem::replace(&mut f.code[check], Op::Jump(entry as u32));
            f.code[entry] = test;
        }
        for op in &mut f.code {
            retarget_in_place(&map, op)?;
        }
        // The moved test falls through into the body in the historical
        // layout, so its `body` is the op right after it — never a mapped
        // old pc (`body == check` for an empty body, and an inner test can
        // sit at `entry + 1`).
        for &(entry, _) in &swaps {
            if let Op::ForNext { body, .. } = &mut f.code[entry] {
                *body = entry as u32 + 1;
            }
        }
        let mut order: Vec<usize> = (0..fornext_pcs.len()).collect();
        order.sort_by_key(|&i| map[fornext_pcs[i]]);
        let vartab = std::mem::take(&mut f.vartab);
        f.vartab = order.into_iter().map(|i| vartab[i].clone()).collect();
    }
    Ok(std::borrow::Cow::Owned(out))
}

/// Rewrite every pc-carrying field of `op` through `map`.
fn retarget_in_place(map: &[u32], op: &mut Op) -> Result<(), IrError> {
    let at = |t: &mut u32| -> Result<(), IrError> {
        *t = map
            .get(*t as usize)
            .copied()
            .ok_or_else(|| IrError::new("derotate: jump target out of range"))?;
        Ok(())
    };
    match op {
        Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => at(t)?,
        Op::ForSetup { exit, header, .. } => {
            at(exit)?;
            at(header)?;
        }
        Op::ForNext { exit, body, .. } => {
            at(exit)?;
            at(body)?;
        }
        Op::WhileSetup { exit, header } => {
            at(exit)?;
            at(header)?;
        }
        Op::WhileCond { exit } => at(exit)?,
        Op::MatchArm { next, .. } => at(next)?,
        Op::MatchGuard { next, .. } => at(next)?,
        Op::IfLetMatch { els, .. } => at(els)?,
        _ => {}
    }
    Ok(())
}

/// Desugar the fused slot fast paths into the core sequences they stand
/// for — exactly the stream this module produced before v3. Borrowed input
/// when there is nothing to expand.
///
/// [`lower`] keeps the fused ops because the interpreter runs them
/// natively; the chunk→C backend's int-twin / peel / cond-fuse passes are
/// tuned to the core stream, so it expands first and sees the pre-v3
/// stream unchanged. `max_stack` is restamped (the desugared form is
/// deeper) so the result still verifies; `locals` needs no restamping —
/// both forms name the same slots ([`Op::slot_extent`]).
pub fn expand_fused(module: &Module) -> Result<std::borrow::Cow<'_, Module>, IrError> {
    if !module
        .funcs
        .iter()
        .any(|f| f.code.iter().any(is_fused_slot_op))
    {
        return Ok(std::borrow::Cow::Borrowed(module));
    }
    let mut out = module.clone();
    for fi in 0..out.funcs.len() {
        if !out.funcs[fi].code.iter().any(is_fused_slot_op) {
            continue;
        }
        let code = out.funcs[fi].code.clone();
        let spans = out.funcs[fi].spans.clone();
        let mut groups: Vec<Vec<Op>> = Vec::with_capacity(code.len());
        for op in &code {
            let group: Vec<Op> = match op {
                Op::SlotAddInt { dst, src } => vec![
                    Op::LoadSlot(*dst),
                    Op::LoadSlot(*src),
                    Op::BinOp(IrBinOp::Add),
                    Op::StoreSlot(*dst),
                ],
                Op::SlotInc { slot } => {
                    let one = intern_int(&mut out.consts, 1);
                    vec![
                        Op::LoadSlot(*slot),
                        Op::PushConst(one),
                        Op::BinOp(IrBinOp::Add),
                        Op::StoreSlot(*slot),
                    ]
                }
                Op::SlotAddIntImm { dst, imm } => {
                    let c = intern_int(&mut out.consts, *imm);
                    vec![
                        Op::LoadSlot(*dst),
                        Op::PushConst(c),
                        Op::BinOp(IrBinOp::Add),
                        Op::StoreSlot(*dst),
                    ]
                }
                Op::SlotLessIntSlot { a, b } => {
                    vec![Op::LoadSlot(*a), Op::LoadSlot(*b), Op::BinOp(IrBinOp::Lt)]
                }
                Op::SlotLessIntImm { a, imm } => {
                    let c = intern_int(&mut out.consts, *imm);
                    vec![Op::LoadSlot(*a), Op::PushConst(c), Op::BinOp(IrBinOp::Lt)]
                }
                Op::SlotCmpIntSlot { a, b, op } => {
                    vec![Op::LoadSlot(*a), Op::LoadSlot(*b), Op::BinOp(*op)]
                }
                Op::SlotCmpIntImm { a, imm, op } => {
                    let c = intern_int(&mut out.consts, *imm);
                    vec![Op::LoadSlot(*a), Op::PushConst(c), Op::BinOp(*op)]
                }
                Op::SlotCmpConst { a, k, op } => {
                    vec![Op::LoadSlot(*a), Op::PushConst(*k), Op::BinOp(*op)]
                }
                Op::SlotBinaryInt { dst, lhs, rhs, op } => vec![
                    Op::LoadSlot(*lhs),
                    Op::LoadSlot(*rhs),
                    Op::BinOp(*op),
                    Op::StoreSlot(*dst),
                ],
                Op::SlotBinaryIntImm { dst, lhs, imm, op } => {
                    let c = intern_int(&mut out.consts, *imm);
                    vec![
                        Op::LoadSlot(*lhs),
                        Op::PushConst(c),
                        Op::BinOp(*op),
                        Op::StoreSlot(*dst),
                    ]
                }
                other => vec![other.clone()],
            };
            groups.push(group);
        }
        // Jump targets address the fused stream: remap them across the
        // expansion with the same pass the VM lowering runs.
        let (nc, ns) = remap_and_flatten(groups, &spans)?;
        let max_stack = crate::verify::max_stack_for(&nc)?;
        let f = &mut out.funcs[fi];
        f.code = nc;
        f.spans = ns;
        f.max_stack = max_stack;
    }
    Ok(std::borrow::Cow::Owned(out))
}
