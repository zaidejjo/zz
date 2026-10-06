//! Raise a verified IR [`Module`] back to an executable VM chunk.
//!
//! The inverse of [`lower`]: nested function bodies rebuild into
//! `Arc<Chunk>` trees carried by `MakeFunc` / `MakeClosure` /
//! `SpawnClosure`, constants materialize from the pool, and param
//! defaults arrive as pre-compiled chunks (the load path has no AST to
//! evaluate). String tables resolve to owned names; `CallPath`'s
//! pre-joined lookup string is recomputed, never serialized.
//!
//! [`lower`]: crate::lower::lower
//! [`Module`]: crate::Module

use std::sync::Arc;

use zz_frontend::ast::{
    BinOp as AstBinOp, Ident, Lit, Param as AstParam, Pattern as AstPattern, UnOp as AstUnOp,
};
use zz_frontend::span::Span as VmSpan;
use zz_runtime::vm::{Chunk as VmChunk, Op as VmOp, TakeHome};
use zz_runtime::Value as VmValue;

use crate::op::BinOp as IrBinOp;
use crate::op::UnOp as IrUnOp;
use crate::{Const, ConstId, FuncDef, FuncId, IrError, Module, Op, Pattern, StrId};

struct Raiser<'a> {
    module: &'a Module,
}

/// Raised closure parameters: AST params alongside the pre-compiled
/// default bodies (parallel to `params`; `None` = required parameter).
type RaisedParams = (Vec<AstParam>, Vec<Option<Arc<VmChunk>>>);

impl<'a> Raiser<'a> {
    fn str(&self, id: StrId) -> Result<String, IrError> {
        self.module
            .strings
            .get(id.0 as usize)
            .cloned()
            .ok_or_else(|| IrError::new("raise: string id out of range (unverified module?)"))
    }

    fn raise_const(&self, id: ConstId, seen: &mut Vec<ConstId>) -> Result<VmValue, IrError> {
        if seen.contains(&id) {
            return Err(IrError::new("raise: cyclic const pool"));
        }
        seen.push(id);
        let c = self
            .module
            .consts
            .get(id.0 as usize)
            .ok_or_else(|| IrError::new("raise: const id out of range (unverified module?)"))?;
        match c {
            Const::Unit => Ok(VmValue::Unit),
            Const::Bool(b) => Ok(VmValue::Bool(*b)),
            Const::Int(i) => Ok(VmValue::Int(*i)),
            Const::Float(f) => Ok(VmValue::Float(*f)),
            Const::Str(s) => Ok(VmValue::Str(Box::new(self.str(*s)?))),
            Const::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.raise_const(*item, seen)?);
                }
                Ok(VmValue::Array(Box::new(out)))
            }
            Const::Dict(pairs) => {
                let mut out = Vec::with_capacity(pairs.len());
                for (k, v) in pairs {
                    out.push((self.raise_const(*k, seen)?, self.raise_const(*v, seen)?));
                }
                Ok(VmValue::Dict(Box::new(out)))
            }
            Const::Option(inner) => Ok(VmValue::Option(match inner {
                Some(x) => Some(Box::new(self.raise_const(*x, seen)?)),
                None => None,
            })),
            Const::Result { ok, val } => {
                let v = self.raise_const(*val, seen)?;
                if *ok {
                    Ok(VmValue::Result(Box::new(Ok(v))))
                } else {
                    Ok(VmValue::Result(Box::new(Err(v))))
                }
            }
        }
    }

    fn raise_binop(op: IrBinOp) -> AstBinOp {
        match op {
            IrBinOp::Add => AstBinOp::Add,
            IrBinOp::Sub => AstBinOp::Sub,
            IrBinOp::Mul => AstBinOp::Mul,
            IrBinOp::Div => AstBinOp::Div,
            IrBinOp::Rem => AstBinOp::Rem,
            IrBinOp::Pow => AstBinOp::Pow,
            IrBinOp::Eq => AstBinOp::Eq,
            IrBinOp::Ne => AstBinOp::Ne,
            IrBinOp::Lt => AstBinOp::Lt,
            IrBinOp::Gt => AstBinOp::Gt,
            IrBinOp::Le => AstBinOp::Le,
            IrBinOp::Ge => AstBinOp::Ge,
            IrBinOp::And => AstBinOp::And,
            IrBinOp::Or => AstBinOp::Or,
            IrBinOp::Elvis => AstBinOp::Elvis,
            IrBinOp::BitAnd => AstBinOp::BitAnd,
            IrBinOp::BitOr => AstBinOp::BitOr,
            IrBinOp::BitXor => AstBinOp::BitXor,
            IrBinOp::Shl => AstBinOp::Shl,
            IrBinOp::Shr => AstBinOp::Shr,
        }
    }

    fn raise_unop(op: IrUnOp) -> AstUnOp {
        match op {
            IrUnOp::Neg => AstUnOp::Neg,
            IrUnOp::Pos => AstUnOp::Pos,
            IrUnOp::Not => AstUnOp::Not,
            IrUnOp::BitNot => AstUnOp::BitNot,
        }
    }

    fn raise_pattern(&self, pat: &Pattern) -> Result<AstPattern, IrError> {
        match pat {
            Pattern::Wildcard => Ok(AstPattern::Wildcard {
                span: VmSpan::default(),
            }),
            Pattern::Binding(id) => Ok(AstPattern::Binding {
                name: Ident {
                    name: self.str(*id)?,
                    span: VmSpan::default(),
                },
            }),
            Pattern::Lit(id) => {
                let lit = match self.const_lit(*id)? {
                    (Lit::Int(i), _) => Lit::Int(i),
                    (Lit::Float(f), _) => Lit::Float(f),
                    (Lit::Str(s), _) => Lit::Str(s),
                    (Lit::Bool(b), _) => Lit::Bool(b),
                };
                Ok(AstPattern::Literal {
                    value: lit,
                    span: VmSpan::default(),
                })
            }
            Pattern::Variant { name, arg } => Ok(AstPattern::Variant {
                name: self.str(*name)?,
                arg: match arg {
                    Some(p) => Some(Box::new(self.raise_pattern(p)?)),
                    None => None,
                },
                span: VmSpan::default(),
            }),
            Pattern::Tuple(pats) => {
                let mut out = Vec::with_capacity(pats.len());
                for p in pats {
                    out.push(self.raise_pattern(p)?);
                }
                Ok(AstPattern::Tuple {
                    pats: out,
                    span: VmSpan::default(),
                })
            }
            Pattern::Or(pats) => {
                let mut out = Vec::with_capacity(pats.len());
                for p in pats {
                    out.push(self.raise_pattern(p)?);
                }
                Ok(AstPattern::Or {
                    pats: out,
                    span: VmSpan::default(),
                })
            }
        }
    }

    fn const_lit(&self, id: ConstId) -> Result<(Lit, ()), IrError> {
        match self.module.consts.get(id.0 as usize) {
            Some(Const::Int(i)) => Ok((Lit::Int(*i), ())),
            Some(Const::Float(f)) => Ok((Lit::Float(*f), ())),
            Some(Const::Str(s)) => Ok((Lit::Str(self.str(*s)?), ())),
            Some(Const::Bool(b)) => Ok((Lit::Bool(*b), ())),
            _ => Err(IrError::new(
                "raise: pattern literal is not int/float/str/bool",
            )),
        }
    }

    fn raise_home(&self, slot: &Option<u16>, var: &Option<StrId>) -> Result<TakeHome, IrError> {
        match (slot, var) {
            (Some(s), _) => Ok(TakeHome::Slot(*s)),
            (None, Some(v)) => Ok(TakeHome::Env(self.str(*v)?)),
            (None, None) => Err(IrError::new("raise: push home is neither slot nor var")),
        }
    }

    /// Raise one tabled function to a runtime chunk, recursively.
    fn raise_func(&self, id: FuncId, seen: &mut Vec<FuncId>) -> Result<Arc<VmChunk>, IrError> {
        if seen.contains(&id) {
            return Err(IrError::new("raise: cyclic func table"));
        }
        seen.push(id);
        let func: &FuncDef = self
            .module
            .funcs
            .get(id.0 as usize)
            .ok_or_else(|| IrError::new("raise: func id out of range (unverified module?)"))?;
        // Pool map first (first-use order over the flat code): the
        // per-chunk pool a fresh compile would produce has no meaning
        // across the serialization boundary — only the values do.
        let mut order: Vec<ConstId> = Vec::new();
        for op in &func.code {
            if let Op::PushConst(cid) = op {
                if !order.contains(cid) {
                    if (cid.0 as usize) >= self.module.consts.len() {
                        seen.pop();
                        return Err(IrError::new("raise: const id out of range"));
                    }
                    order.push(*cid);
                }
            }
        }
        let mut constants = Vec::with_capacity(order.len());
        for cid in &order {
            let mut seen_c = Vec::new();
            constants.push(self.raise_const(*cid, &mut seen_c)?);
        }
        let mut code = Vec::with_capacity(func.code.len());
        for op in &func.code {
            let mut raised = self.raise_op(op, seen)?;
            if let VmOp::PushConst(_) = raised {
                let Op::PushConst(cid) = op else {
                    unreachable!()
                };
                let idx = order.iter().position(|x| x == cid).expect("pool order") as u32;
                raised = VmOp::PushConst(idx);
            }
            code.push(raised);
        }
        let mut chunk = VmChunk::new();
        chunk.code = code;
        chunk.spans = func
            .spans
            .iter()
            .map(|s| VmSpan::new(s.start, s.end))
            .collect();
        chunk.params = func
            .params
            .iter()
            .map(|p| {
                self.str(p.name).map(|name| AstParam {
                    name: Ident {
                        name,
                        span: VmSpan::default(),
                    },
                    ty: None,
                    default: None,
                    span: VmSpan::default(),
                })
            })
            .collect::<Result<_, _>>()?;
        chunk.constants = constants;
        chunk.toplevel_slots = func
            .toplevel_slots
            .iter()
            .map(|(n, s)| self.str(*n).map(|name| (name, *s)))
            .collect::<Result<_, _>>()?;
        seen.pop();
        Ok(Arc::new(chunk))
    }

    fn raise_op(&self, op: &Op, seen: &mut Vec<FuncId>) -> Result<VmOp, IrError> {
        match op {
            Op::PushConst(_) => {
                // Rewritten with the per-function pool map by `raise`.
                Ok(VmOp::PushConst(u32::MAX))
            }
            Op::Pop => Ok(VmOp::Pop),
            Op::Swap => Ok(VmOp::Swap),
            Op::PopN(n) => Ok(VmOp::PopN {
                n: *n,
                span: VmSpan::default(),
            }),
            Op::Truthy => Ok(VmOp::Truthy),
            Op::LoadVar(id) => Ok(VmOp::LoadVar(self.str(*id)?, VmSpan::default())),
            Op::LoadPath(parts) => Ok(VmOp::LoadPath(
                parts
                    .iter()
                    .map(|p| self.str(*p))
                    .collect::<Result<_, _>>()?,
                VmSpan::default(),
            )),
            Op::DefineVar(id) => Ok(VmOp::DefineVar(self.str(*id)?)),
            Op::StoreVar(id) => Ok(VmOp::StoreVar(self.str(*id)?, VmSpan::default())),
            Op::StorePath(parts) => Ok(VmOp::StorePath(
                parts
                    .iter()
                    .map(|p| self.str(*p))
                    .collect::<Result<_, _>>()?,
                VmSpan::default(),
            )),
            Op::LoadSlot(s) => Ok(VmOp::LoadSlot(*s)),
            Op::StoreSlot(s) => Ok(VmOp::StoreSlot(*s)),
            Op::TakeVar(id) => Ok(VmOp::TakeVar(self.str(*id)?, VmSpan::default())),
            Op::VecPushField {
                home_slot,
                home_var,
                field,
            } => Ok(VmOp::VecPushField {
                home: self.raise_home(home_slot, home_var)?,
                field: self.str(*field)?,
                span: VmSpan::default(),
            }),
            Op::VecPushMethod {
                home_slot,
                home_var,
                method,
            } => Ok(VmOp::VecPushMethod {
                home: self.raise_home(home_slot, home_var)?,
                method: self.str(*method)?,
                span: VmSpan::default(),
            }),
            Op::IntAdd => Ok(VmOp::IntAdd(VmSpan::default())),
            Op::IntSub => Ok(VmOp::IntSub(VmSpan::default())),
            Op::IntMul => Ok(VmOp::IntMul(VmSpan::default())),
            Op::IntDiv => Ok(VmOp::IntDiv(VmSpan::default())),
            Op::IntRem => Ok(VmOp::IntRem(VmSpan::default())),
            Op::IntNeg => Ok(VmOp::IntNeg(VmSpan::default())),
            Op::BinOp(o) => Ok(VmOp::BinOp(Self::raise_binop(*o), VmSpan::default())),
            Op::UnOp(o) => Ok(VmOp::UnOp(Self::raise_unop(*o), VmSpan::default())),
            Op::Jump(t) => Ok(VmOp::Jump(*t as usize)),
            Op::JumpIfFalse(t) => Ok(VmOp::JumpIfFalse(*t as usize)),
            Op::JumpIfTrue(t) => Ok(VmOp::JumpIfTrue(*t as usize)),
            Op::JumpIfFalseBool(t) => Ok(VmOp::JumpIfFalseBool(*t as usize, VmSpan::default())),
            Op::Return => Ok(VmOp::Return),
            Op::ForSetup {
                exit,
                header,
                num_vars,
            } => Ok(VmOp::ForSetup {
                exit: *exit as usize,
                header: *header as usize,
                span: VmSpan::default(),
                num_vars: *num_vars,
            }),
            Op::ForNext { vars, exit, in_env } => Ok(VmOp::ForNext {
                vars: vars
                    .iter()
                    .map(|v| self.str(*v))
                    .collect::<Result<_, _>>()?,
                exit: *exit as usize,
                in_env: *in_env,
                span: VmSpan::default(),
            }),
            Op::WhileSetup { exit, header } => Ok(VmOp::WhileSetup {
                exit: *exit as usize,
                header: *header as usize,
            }),
            Op::WhileCond { exit } => Ok(VmOp::WhileCond {
                exit: *exit as usize,
                span: VmSpan::default(),
            }),
            Op::Break => Ok(VmOp::Break(VmSpan::default())),
            Op::Continue => Ok(VmOp::Continue(VmSpan::default())),
            Op::SetLoopResult => Ok(VmOp::SetLoopResult),
            Op::Safepoint => Ok(VmOp::Safepoint),
            Op::MakeArray(n) => Ok(VmOp::MakeArray(*n)),
            Op::UnpackTuple(n) => Ok(VmOp::UnpackTuple(*n)),
            Op::ArrayPush => Ok(VmOp::ArrayPush(VmSpan::default())),
            Op::MakeDict(n) => Ok(VmOp::MakeDict(*n)),
            Op::IndexOp => Ok(VmOp::IndexOp(VmSpan::default())),
            Op::StoreIndexOp => Ok(VmOp::StoreIndexOp(VmSpan::default())),
            Op::CompoundIndexOp { op } => Ok(VmOp::CompoundIndexOp {
                op: Self::raise_binop(*op),
                span: VmSpan::default(),
            }),
            Op::SliceOp => Ok(VmOp::SliceOp(VmSpan::default())),
            Op::MakeRange => Ok(VmOp::MakeRange(VmSpan::default())),
            Op::MakeStruct { name, fields } => Ok(VmOp::MakeStruct {
                name: self.str(*name)?,
                field_names: fields
                    .iter()
                    .map(|f| self.str(*f))
                    .collect::<Result<_, _>>()?,
                span: VmSpan::default(),
            }),
            Op::GetField(id) => Ok(VmOp::GetField(self.str(*id)?, VmSpan::default())),
            Op::GetFieldIdx(i) => Ok(VmOp::GetFieldIdx(*i, VmSpan::default())),
            Op::SetField(id) => Ok(VmOp::SetField(self.str(*id)?, VmSpan::default())),
            Op::SetFieldIdx(i) => Ok(VmOp::SetFieldIdx(*i, VmSpan::default())),
            Op::CompoundFieldOp { name, op } => Ok(VmOp::CompoundFieldOp {
                name: self.str(*name)?,
                op: Self::raise_binop(*op),
                span: VmSpan::default(),
            }),
            Op::RegisterStruct { name, fields } => Ok(VmOp::RegisterStruct {
                name: self.str(*name)?,
                fields: fields
                    .iter()
                    .map(|f| self.str(*f))
                    .collect::<Result<_, _>>()?,
            }),
            Op::RegisterEnum { name, variants } => Ok(VmOp::RegisterEnum {
                name: self.str(*name)?,
                variants: variants
                    .iter()
                    .map(|(v, a)| self.str(*v).map(|name| (name, *a)))
                    .collect::<Result<_, _>>()?,
            }),
            Op::MakeEnum {
                enum_name,
                variant,
                argc,
            } => Ok(VmOp::MakeEnum {
                enum_name: self.str(*enum_name)?,
                variant: self.str(*variant)?,
                argc: *argc,
                span: VmSpan::default(),
            }),
            Op::MakeClosure { func } => {
                let chunk = self.raise_func(*func, seen)?;
                let (params, defaults) = self.raise_closure_params(*func, seen)?;
                Ok(VmOp::MakeClosure {
                    params,
                    chunk,
                    defaults,
                })
            }
            Op::MakeFunc { func } => {
                let f = self.func(*func)?;
                let name = self.str(f.name)?;
                let chunk = self.raise_func(*func, seen)?;
                let (params, defaults) = self.raise_closure_params(*func, seen)?;
                Ok(VmOp::MakeFunc {
                    name,
                    params,
                    chunk,
                    defaults,
                })
            }
            Op::SpawnClosure { func } => {
                let chunk = self.raise_func(*func, seen)?;
                let (params, defaults) = self.raise_closure_params(*func, seen)?;
                Ok(VmOp::SpawnClosure {
                    params,
                    chunk,
                    span: VmSpan::default(),
                    defaults,
                })
            }
            Op::MakeVariant { name, has_arg } => Ok(VmOp::MakeVariant {
                name: self.str(*name)?,
                has_arg: *has_arg,
                span: VmSpan::default(),
            }),
            Op::MatchArm {
                pat,
                next,
                has_env,
                restore,
            } => Ok(VmOp::MatchArm {
                pat: self.raise_pattern(pat)?,
                next: *next as usize,
                has_env: *has_env,
                restore: *restore,
            }),
            Op::MatchGuard { next, has_env } => Ok(VmOp::MatchGuard {
                next: *next as usize,
                has_env: *has_env,
            }),
            Op::MatchError => Ok(VmOp::MatchError(VmSpan::default())),
            Op::IfLetMatch { pat, els, has_env } => Ok(VmOp::IfLetMatch {
                pat: self.raise_pattern(pat)?,
                els: *els as usize,
                has_env: *has_env,
            }),
            Op::TryOp => Ok(VmOp::TryOp(VmSpan::default())),
            Op::Elvis => Ok(VmOp::Elvis(VmSpan::default())),
            Op::ElvisResult => Ok(VmOp::ElvisResult),
            Op::Call { argc } => Ok(VmOp::Call {
                argc: *argc,
                span: VmSpan::default(),
            }),
            Op::CallPath { parts, argc, pspan } => {
                let parts: Vec<String> = parts
                    .iter()
                    .map(|p| self.str(*p))
                    .collect::<Result<_, _>>()?;
                Ok(VmOp::CallPath {
                    joined: parts.join("."),
                    parts,
                    argc: *argc,
                    span: VmSpan::default(),
                    pspan: VmSpan::new(pspan.start, pspan.end),
                })
            }
            Op::CallMethod { name, argc } => Ok(VmOp::CallMethod {
                name: self.str(*name)?,
                argc: *argc,
                span: VmSpan::default(),
            }),
            Op::CallNative { name, argc } => Ok(VmOp::CallNative {
                name: self.str(*name)?,
                argc: *argc,
                span: VmSpan::default(),
            }),
            Op::Concat(n) => Ok(VmOp::Concat(*n)),
            Op::FormatValue => Ok(VmOp::FormatValue(VmSpan::default())),
            Op::DbQuery { nparams } => Ok(VmOp::DbQuery {
                nparams: *nparams,
                span: VmSpan::default(),
            }),
            Op::EnterScope => Ok(VmOp::EnterScope),
            Op::ExitScope => Ok(VmOp::ExitScope),
            Op::DeferRecord => Ok(VmOp::DeferRecord),
        }
    }

    fn func(&self, id: FuncId) -> Result<&FuncDef, IrError> {
        self.module
            .funcs
            .get(id.0 as usize)
            .ok_or_else(|| IrError::new("raise: func id out of range (unverified module?)"))
    }

    /// Closure/func params plus their pre-compiled default bodies.
    fn raise_closure_params(
        &self,
        id: FuncId,
        seen: &mut Vec<FuncId>,
    ) -> Result<RaisedParams, IrError> {
        let f = self.func(id)?;
        let mut params = Vec::with_capacity(f.params.len());
        let mut defaults = Vec::with_capacity(f.params.len());
        for p in &f.params {
            params.push(AstParam {
                name: Ident {
                    name: self.str(p.name)?,
                    span: VmSpan::default(),
                },
                ty: None,
                default: None,
                span: VmSpan::default(),
            });
            defaults.push(match p.default {
                Some(d) => Some(self.raise_func(d, seen)?),
                None => None,
            });
        }
        Ok((params, defaults))
    }
}

/// Raise a verified module to its entry chunk. Nested bodies raise
/// recursively through the closure ops.
pub fn raise(module: &Module) -> Result<Arc<VmChunk>, IrError> {
    let r = Raiser { module };
    let mut seen = Vec::new();
    r.raise_func(module.entry, &mut seen)
}
