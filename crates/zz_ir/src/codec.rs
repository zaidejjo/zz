//! `.zzc` binary codec (spec §8).
//!
//! Little-endian throughout. Layout:
//!
//! ```text
//! magic[4] = "ZZC1", version u32, section-count u32,
//! per section: tag u32, offset u32, length u32,
//! TYPES | STRINGS | CONSTS | FUNCS | CODE | SPANS | ANNOT payloads
//! ```
//!
//! Section tags: `TYPES=1 STRINGS=2 CONSTS=3 FUNCS=4 CODE=5 SPANS=6
//! ANNOT=7`. `FUNCS` entries reference (offset, length) ranges into the
//! concatenated `CODE`/`SPANS` streams, in function order. The encoder
//! is deterministic: decoding then re-encoding yields identical bytes.
//!
//! Decoding never panics: every read is bounds-checked and every id is
//! range-checked, so fuzzed inputs surface as [`IrError`], never a crash.

use crate::op::{BinOp, UnOp};
use crate::ty::IrType;
use crate::{
    Const, ConstId, FuncDef, FuncId, FuncSig, IrError, Module, Op, Param, Pattern, Span, StrId,
    TypeId,
};

const TAG_TYPES: u32 = 1;
const TAG_STRINGS: u32 = 2;
const TAG_CONSTS: u32 = 3;
const TAG_FUNCS: u32 = 4;
const TAG_CODE: u32 = 5;
const TAG_SPANS: u32 = 6;
const TAG_ANNOT: u32 = 7;

pub fn encode(module: &Module) -> Vec<u8> {
    let types = encode_types(module);
    let strings = encode_strings(module);
    let consts = encode_consts(module);
    let code = encode_code(module);
    let spans = encode_spans(module);
    let annot: Vec<u8> = {
        let mut v = Vec::new();
        put_u32(&mut v, 0); // zero annotations in v1
        v
    };
    // Function records need code ranges: lay CODE/SPANS out first.
    let funcs = encode_funcs(module, &code_ranges(module));
    let mut sections: Vec<(u32, Vec<u8>)> = vec![
        (TAG_TYPES, types),
        (TAG_STRINGS, strings),
        (TAG_CONSTS, consts),
        (TAG_FUNCS, funcs),
        (TAG_CODE, code),
        (TAG_SPANS, spans),
        (TAG_ANNOT, annot),
    ];
    let header_len = 4 + 4 + 4 + sections.len() * 12;
    let mut out = Vec::new();
    out.extend_from_slice(&crate::MAGIC);
    put_u32(&mut out, crate::VERSION);
    put_u32(&mut out, sections.len() as u32);
    let mut offset = header_len as u32;
    for (tag, payload) in &sections {
        put_u32(&mut out, *tag);
        put_u32(&mut out, offset);
        put_u32(&mut out, payload.len() as u32);
        offset += payload.len() as u32;
    }
    for (_, payload) in &mut sections {
        out.append(payload);
    }
    out
}

/// Starting op index of each function inside the concatenated CODE
/// stream, in function order.
fn code_ranges(module: &Module) -> Vec<(u32, u32)> {
    let mut ranges = Vec::with_capacity(module.funcs.len());
    let mut at = 0u32;
    for f in &module.funcs {
        ranges.push((at, f.code.len() as u32));
        at += f.code.len() as u32;
    }
    ranges
}

fn put_u8(v: &mut Vec<u8>, x: u8) {
    v.push(x);
}

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put_i64(v: &mut Vec<u8>, x: i64) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put_str(v: &mut Vec<u8>, s: &str) {
    put_u32(v, s.len() as u32);
    v.extend_from_slice(s.as_bytes());
}

fn encode_types(module: &Module) -> Vec<u8> {
    let mut v = Vec::new();
    put_u32(&mut v, module.types.len() as u32);
    for t in &module.types {
        encode_type(&mut v, t);
    }
    v
}

fn encode_type(v: &mut Vec<u8>, t: &IrType) {
    put_u8(v, t.tag());
    match t {
        IrType::Tuple(items) | IrType::Union(items) | IrType::Func(items, _) => {
            // Func's return id is appended after the params below.
            put_u32(v, items.len() as u32);
            for id in items {
                put_u32(v, id.0);
            }
            if let IrType::Func(_, ret) = t {
                put_u32(v, ret.0);
            }
        }
        IrType::Option(inner) | IrType::Array(inner) | IrType::Range(inner) => {
            put_u32(v, inner.0);
        }
        IrType::Result(a, b) | IrType::Dict(a, b) => {
            put_u32(v, a.0);
            put_u32(v, b.0);
        }
        IrType::Opaque(id) | IrType::Named(id) => put_u32(v, id.0),
        IrType::Struct(id, args) | IrType::Enum(id, args) => {
            put_u32(v, id.0);
            put_u32(v, args.len() as u32);
            for a in args {
                put_u32(v, a.0);
            }
        }
        IrType::Var(n) => put_u32(v, *n),
        IrType::Ptr { mutable, inner } => {
            put_u8(v, *mutable as u8);
            put_u32(v, inner.0);
        }
        _ => {}
    }
}

fn encode_strings(module: &Module) -> Vec<u8> {
    let mut v = Vec::new();
    put_u32(&mut v, module.strings.len() as u32);
    for s in &module.strings {
        put_str(&mut v, s);
    }
    v
}

fn encode_consts(module: &Module) -> Vec<u8> {
    let mut v = Vec::new();
    put_u32(&mut v, module.consts.len() as u32);
    for c in &module.consts {
        encode_const(&mut v, c);
    }
    v
}

fn encode_const(v: &mut Vec<u8>, c: &Const) {
    match c {
        Const::Unit => put_u8(v, 0),
        Const::Bool(b) => {
            put_u8(v, 1);
            put_u8(v, *b as u8);
        }
        Const::Int(i) => {
            put_u8(v, 2);
            put_i64(v, *i);
        }
        Const::Float(f) => {
            put_u8(v, 3);
            v.extend_from_slice(&f.to_le_bytes());
        }
        Const::Str(id) => {
            put_u8(v, 4);
            put_u32(v, id.0);
        }
        Const::Array(items) => {
            put_u8(v, 5);
            put_u32(v, items.len() as u32);
            for id in items {
                put_u32(v, id.0);
            }
        }
        Const::Dict(pairs) => {
            put_u8(v, 6);
            put_u32(v, pairs.len() as u32);
            for (k, val) in pairs {
                put_u32(v, k.0);
                put_u32(v, val.0);
            }
        }
        Const::Option(inner) => {
            put_u8(v, 7);
            match inner {
                Some(id) => {
                    put_u8(v, 1);
                    put_u32(v, id.0);
                }
                None => put_u8(v, 0),
            }
        }
        Const::Result { ok, val } => {
            put_u8(v, 8);
            put_u8(v, *ok as u8);
            put_u32(v, val.0);
        }
    }
}

fn encode_funcs(module: &Module, ranges: &[(u32, u32)]) -> Vec<u8> {
    let mut v = Vec::new();
    put_u32(&mut v, module.funcs.len() as u32);
    for (f, (off, len)) in module.funcs.iter().zip(ranges.iter()) {
        put_u32(&mut v, f.name.0);
        put_u32(&mut v, f.arity);
        put_u32(&mut v, f.params.len() as u32);
        for p in &f.params {
            put_u32(&mut v, p.name.0);
            match p.default {
                Some(id) => {
                    put_u8(&mut v, 1);
                    put_u32(&mut v, id.0);
                }
                None => put_u8(&mut v, 0),
            }
        }
        put_u32(&mut v, f.sig.params.len() as u32);
        for id in &f.sig.params {
            put_u32(&mut v, id.0);
        }
        put_u32(&mut v, f.sig.ret.0);
        put_u32(&mut v, *off);
        put_u32(&mut v, *len);
        put_u32(&mut v, f.max_stack);
        put_u32(&mut v, f.toplevel_slots.len() as u32);
        for (name, slot) in &f.toplevel_slots {
            put_u32(&mut v, name.0);
            put_u16(&mut v, *slot);
        }
    }
    v
}

fn encode_code(module: &Module) -> Vec<u8> {
    let mut v = Vec::new();
    let total: usize = module.funcs.iter().map(|f| f.code.len()).sum();
    put_u32(&mut v, total as u32);
    for f in &module.funcs {
        for op in &f.code {
            encode_op(&mut v, op);
        }
    }
    v
}

fn put_ids(v: &mut Vec<u8>, ids: &[StrId]) {
    put_u32(v, ids.len() as u32);
    for id in ids {
        put_u32(v, id.0);
    }
}

fn encode_binop(v: &mut Vec<u8>, op: BinOp) {
    put_u8(v, op as u8);
}

fn encode_unop(v: &mut Vec<u8>, op: UnOp) {
    put_u8(v, op as u8);
}

fn encode_pattern(v: &mut Vec<u8>, pat: &Pattern) {
    match pat {
        Pattern::Wildcard => put_u8(v, 0),
        Pattern::Binding(id) => {
            put_u8(v, 1);
            put_u32(v, id.0);
        }
        Pattern::Lit(id) => {
            put_u8(v, 2);
            put_u32(v, id.0);
        }
        Pattern::Variant { name, arg } => {
            put_u8(v, 3);
            put_u32(v, name.0);
            match arg {
                Some(p) => {
                    put_u8(v, 1);
                    encode_pattern(v, p);
                }
                None => put_u8(v, 0),
            }
        }
        Pattern::Tuple(pats) => {
            put_u8(v, 4);
            put_u32(v, pats.len() as u32);
            for p in pats {
                encode_pattern(v, p);
            }
        }
        Pattern::Or(pats) => {
            put_u8(v, 5);
            put_u32(v, pats.len() as u32);
            for p in pats {
                encode_pattern(v, p);
            }
        }
    }
}

fn encode_home(v: &mut Vec<u8>, slot: &Option<u16>, var: &Option<StrId>) {
    match (slot, var) {
        (Some(s), _) => {
            put_u8(v, 0);
            put_u16(v, *s);
        }
        (None, Some(id)) => {
            put_u8(v, 1);
            put_u32(v, id.0);
        }
        (None, None) => put_u8(v, 2),
    }
}

fn encode_op(v: &mut Vec<u8>, op: &Op) {
    put_u16(v, op.tag());
    match op {
        Op::PushConst(id) => put_u32(v, id.0),
        Op::PopN(n) => put_u16(v, *n),
        Op::LoadVar(id) | Op::DefineVar(id) | Op::StoreVar(id) | Op::TakeVar(id) => {
            put_u32(v, id.0);
        }
        Op::LoadPath(parts) | Op::StorePath(parts) => put_ids(v, parts),
        Op::LoadSlot(s) | Op::StoreSlot(s) | Op::GetFieldIdx(s) | Op::SetFieldIdx(s) => {
            put_u16(v, *s);
        }
        Op::VecPushField {
            home_slot,
            home_var,
            field,
        } => {
            encode_home(v, home_slot, home_var);
            put_u32(v, field.0);
        }
        Op::VecPushMethod {
            home_slot,
            home_var,
            method,
        } => {
            encode_home(v, home_slot, home_var);
            put_u32(v, method.0);
        }
        Op::BinOp(op) => encode_binop(v, *op),
        Op::UnOp(op) => encode_unop(v, *op),
        Op::CompoundIndexOp { op } => encode_binop(v, *op),
        Op::CompoundFieldOp { name, op } => {
            put_u32(v, name.0);
            encode_binop(v, *op);
        }
        Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) | Op::JumpIfFalseBool(t) => {
            put_u32(v, *t);
        }
        Op::ForSetup {
            exit,
            header,
            num_vars,
        } => {
            put_u32(v, *exit);
            put_u32(v, *header);
            put_u8(v, *num_vars);
        }
        Op::ForNext { vars, exit, in_env } => {
            put_ids(v, vars);
            put_u32(v, *exit);
            put_u8(v, *in_env as u8);
        }
        Op::WhileSetup { exit, header } => {
            put_u32(v, *exit);
            put_u32(v, *header);
        }
        Op::WhileCond { exit } => put_u32(v, *exit),
        Op::MakeArray(n) | Op::MakeDict(n) | Op::Concat(n) => put_u16(v, *n),
        Op::UnpackTuple(n) => put_u8(v, *n),
        Op::DbQuery { nparams } => put_u16(v, *nparams),
        Op::MakeStruct { name, fields } => {
            put_u32(v, name.0);
            put_ids(v, fields);
        }
        Op::GetField(id) | Op::SetField(id) => put_u32(v, id.0),
        Op::RegisterStruct { name, fields } => {
            put_u32(v, name.0);
            put_ids(v, fields);
        }
        Op::RegisterEnum { name, variants } => {
            put_u32(v, name.0);
            put_u32(v, variants.len() as u32);
            for (id, has_arg) in variants {
                put_u32(v, id.0);
                put_u8(v, *has_arg as u8);
            }
        }
        Op::MakeEnum {
            enum_name,
            variant,
            argc,
        } => {
            put_u32(v, enum_name.0);
            put_u32(v, variant.0);
            put_u16(v, *argc);
        }
        Op::MakeClosure { func } | Op::MakeFunc { func } | Op::SpawnClosure { func } => {
            put_u32(v, func.0);
        }
        Op::MakeVariant { name, has_arg } => {
            put_u32(v, name.0);
            put_u8(v, *has_arg as u8);
        }
        Op::MatchArm {
            pat,
            next,
            has_env,
            restore,
        } => {
            encode_pattern(v, pat);
            put_u32(v, *next);
            put_u8(v, *has_env as u8);
            put_u8(v, *restore as u8);
        }
        Op::MatchGuard { next, has_env } => {
            put_u32(v, *next);
            put_u8(v, *has_env as u8);
        }
        Op::IfLetMatch { pat, els, has_env } => {
            encode_pattern(v, pat);
            put_u32(v, *els);
            put_u8(v, *has_env as u8);
        }
        Op::Call { argc } => put_u16(v, *argc),
        Op::CallPath { parts, argc, pspan } => {
            put_ids(v, parts);
            put_u16(v, *argc);
            put_u32(v, pspan.start);
            put_u32(v, pspan.end);
        }
        Op::CallMethod { name, argc } => {
            put_u32(v, name.0);
            put_u16(v, *argc);
        }
        Op::CallNative { name, argc } => {
            put_u32(v, name.0);
            put_u16(v, *argc);
        }
        _ => {}
    }
}

fn encode_spans(module: &Module) -> Vec<u8> {
    let mut v = Vec::new();
    let total: usize = module.funcs.iter().map(|f| f.spans.len()).sum();
    put_u32(&mut v, total as u32);
    for f in &module.funcs {
        for s in &f.spans {
            put_u32(&mut v, s.start);
            put_u32(&mut v, s.end);
        }
    }
    v
}

// ---------------------------------------------------------------------------
// Decoder: bounds-checked, panic-free.
// ---------------------------------------------------------------------------

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }

    fn err<T>(&self, what: &str) -> Result<T, IrError> {
        Err(IrError::new(format!("truncated .zzc ({what})")))
    }

    fn u8(&mut self) -> Result<u8, IrError> {
        let b = self.bytes.get(self.pos).copied();
        match b {
            Some(x) => {
                self.pos += 1;
                Ok(x)
            }
            None => self.err("u8"),
        }
    }

    fn u16(&mut self) -> Result<u16, IrError> {
        let s = self.bytes.get(self.pos..self.pos + 2);
        match s {
            Some(w) => {
                self.pos += 2;
                Ok(u16::from_le_bytes([w[0], w[1]]))
            }
            None => self.err("u16"),
        }
    }

    fn u32(&mut self) -> Result<u32, IrError> {
        let s = self.bytes.get(self.pos..self.pos + 4);
        match s {
            Some(w) => {
                self.pos += 4;
                Ok(u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            }
            None => self.err("u32"),
        }
    }

    fn i64(&mut self) -> Result<i64, IrError> {
        let mut buf = [0u8; 8];
        let s = self.bytes.get(self.pos..self.pos + 8);
        match s {
            Some(w) => {
                buf.copy_from_slice(w);
                self.pos += 8;
                Ok(i64::from_le_bytes(buf))
            }
            None => self.err("i64"),
        }
    }

    fn f64(&mut self) -> Result<f64, IrError> {
        let mut buf = [0u8; 8];
        let s = self.bytes.get(self.pos..self.pos + 8);
        match s {
            Some(w) => {
                buf.copy_from_slice(w);
                self.pos += 8;
                Ok(f64::from_le_bytes(buf))
            }
            None => self.err("f64"),
        }
    }

    fn raw_str(&mut self) -> Result<String, IrError> {
        let len = self.u32()? as usize;
        let s = self.bytes.get(self.pos..self.pos + len);
        match s {
            Some(w) => {
                self.pos += len;
                match std::str::from_utf8(w) {
                    Ok(text) => Ok(text.to_string()),
                    Err(_) => Err(IrError::new("invalid utf-8 in .zzc string")),
                }
            }
            None => self.err("string bytes"),
        }
    }
}

fn section<'a>(
    bytes: &'a [u8],
    entries: &[(u32, u32, u32)],
    tag: u32,
) -> Result<&'a [u8], IrError> {
    let mut found = None;
    for (t, off, len) in entries {
        if *t == tag {
            if found.is_some() {
                return Err(IrError::new("duplicate .zzc section"));
            }
            found = Some((*off as usize, *len as usize));
        }
    }
    let (off, len) = found.ok_or_else(|| IrError::new("missing .zzc section"))?;
    let end = off
        .checked_add(len)
        .ok_or_else(|| IrError::new("bad .zzc section range"))?;
    bytes
        .get(off..end)
        .ok_or_else(|| IrError::new("bad .zzc section range"))
}

pub fn decode(bytes: &[u8]) -> Result<Module, IrError> {
    let mut r = Reader::new(bytes);
    let mut magic = [0u8; 4];
    for slot in magic.iter_mut() {
        *slot = r.u8()?;
    }
    if magic != crate::MAGIC {
        return Err(IrError::new("bad .zzc magic"));
    }
    let version = r.u32()?;
    if version != crate::VERSION {
        return Err(IrError::new(format!(
            "unsupported .zzc version {version} (want {})",
            crate::VERSION
        )));
    }
    let nsec = r.u32()?;
    if nsec == 0 || nsec > 32 {
        return Err(IrError::new("bad .zzc section count"));
    }
    let mut entries = Vec::with_capacity(nsec as usize);
    for _ in 0..nsec {
        entries.push((r.u32()?, r.u32()?, r.u32()?));
    }
    let types = decode_types(section(bytes, &entries, TAG_TYPES)?)?;
    let strings = decode_strings(section(bytes, &entries, TAG_STRINGS)?)?;
    let consts = decode_consts(section(bytes, &entries, TAG_CONSTS)?)?;
    let (mut funcs, ranges) = decode_funcs(section(bytes, &entries, TAG_FUNCS)?)?;
    let code = decode_code(section(bytes, &entries, TAG_CODE)?)?;
    let spans = decode_spans(section(bytes, &entries, TAG_SPANS)?)?;
    decode_annot(section(bytes, &entries, TAG_ANNOT)?)?;
    // Zip CODE/SPANS ranges into functions, in order.
    let mut code_at = 0usize;
    let mut span_at = 0usize;
    for (f, (off, len)) in funcs.iter_mut().zip(ranges.iter()) {
        if *off != code_at as u32 {
            return Err(IrError::new("non-contiguous .zzc code range"));
        }
        let len = *len as usize;
        let code_end = code_at
            .checked_add(len)
            .ok_or_else(|| IrError::new("bad code range"))?;
        let span_end = span_at
            .checked_add(len)
            .ok_or_else(|| IrError::new("bad span range"))?;
        let ops = code
            .get(code_at..code_end)
            .ok_or_else(|| IrError::new("bad code range"))?;
        let sps = spans
            .get(span_at..span_end)
            .ok_or_else(|| IrError::new("bad span range"))?;
        f.code = ops.to_vec();
        f.spans = sps.to_vec();
        code_at = code_end;
        span_at = span_end;
    }
    if code_at != code.len() || span_at != spans.len() {
        return Err(IrError::new("trailing .zzc code/spans"));
    }
    if funcs.is_empty() {
        return Err(IrError::new("empty .zzc function table"));
    }
    Ok(Module {
        types,
        strings,
        consts,
        funcs,
        entry: FuncId(0),
    })
}

fn decode_types(bytes: &[u8]) -> Result<Vec<IrType>, IrError> {
    let mut r = Reader::new(bytes);
    let n = r.u32()? as usize;
    if n > 1_000_000 {
        return Err(IrError::new("absurd .zzc type count"));
    }
    let mut out = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        out.push(decode_type(&mut r)?);
    }
    Ok(out)
}

fn type_id(r: &mut Reader) -> Result<TypeId, IrError> {
    Ok(TypeId(r.u32()?))
}

fn decode_type(r: &mut Reader) -> Result<IrType, IrError> {
    let tag = r.u8()?;
    match tag {
        0 => Ok(IrType::Unknown),
        1 => Ok(IrType::Unit),
        2 => Ok(IrType::Bool),
        3 => Ok(IrType::Int),
        4 => Ok(IrType::Float),
        5 => Ok(IrType::Str),
        6 => Ok(IrType::Bytes),
        7 => {
            let n = r.u32()? as usize;
            let mut items = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                items.push(type_id(r)?);
            }
            Ok(IrType::Tuple(items))
        }
        8 => Ok(IrType::Option(type_id(r)?)),
        9 => {
            let a = type_id(r)?;
            Ok(IrType::Result(a, type_id(r)?))
        }
        10 => Ok(IrType::Array(type_id(r)?)),
        11 => {
            let a = type_id(r)?;
            Ok(IrType::Dict(a, type_id(r)?))
        }
        12 => {
            let n = r.u32()? as usize;
            let mut items = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                items.push(type_id(r)?);
            }
            Ok(IrType::Func(items, type_id(r)?))
        }
        13 => Ok(IrType::Range(type_id(r)?)),
        14 => Ok(IrType::Json),
        15 => Ok(IrType::Db),
        16 => Ok(IrType::HttpServer),
        17 => Ok(IrType::TcpStream),
        18 => Ok(IrType::TcpListener),
        19 => Ok(IrType::Response),
        20 => Ok(IrType::HttpRequest),
        21 => Ok(IrType::Opaque(StrId(r.u32()?))),
        22 => Ok(IrType::Chan),
        23 => Ok(IrType::TaskJoin),
        24 => {
            let id = StrId(r.u32()?);
            let n = r.u32()? as usize;
            let mut args = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                args.push(type_id(r)?);
            }
            Ok(IrType::Struct(id, args))
        }
        25 => {
            let id = StrId(r.u32()?);
            let n = r.u32()? as usize;
            let mut args = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                args.push(type_id(r)?);
            }
            Ok(IrType::Enum(id, args))
        }
        26 => {
            let n = r.u32()? as usize;
            let mut items = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                items.push(type_id(r)?);
            }
            Ok(IrType::Union(items))
        }
        27 => Ok(IrType::Named(StrId(r.u32()?))),
        28 => Ok(IrType::Var(r.u32()?)),
        29 => Ok(IrType::Error),
        30 => Ok(IrType::Void),
        31 => {
            let mutable = r.u8()?;
            if mutable > 1 {
                return Err(IrError::new("bad .zzc ptr flag"));
            }
            Ok(IrType::Ptr {
                mutable: mutable == 1,
                inner: type_id(r)?,
            })
        }
        _ => Err(IrError::new("unknown .zzc type tag")),
    }
}

fn decode_strings(bytes: &[u8]) -> Result<Vec<String>, IrError> {
    let mut r = Reader::new(bytes);
    let n = r.u32()? as usize;
    if n > 1_000_000 {
        return Err(IrError::new("absurd .zzc string count"));
    }
    let mut out = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        out.push(r.raw_str()?);
    }
    Ok(out)
}

fn decode_consts(bytes: &[u8]) -> Result<Vec<Const>, IrError> {
    let mut r = Reader::new(bytes);
    let n = r.u32()? as usize;
    if n > 1_000_000 {
        return Err(IrError::new("absurd .zzc const count"));
    }
    let mut out = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let tag = r.u8()?;
        out.push(match tag {
            0 => Const::Unit,
            1 => {
                let b = r.u8()?;
                if b > 1 {
                    return Err(IrError::new("bad .zzc bool const"));
                }
                Const::Bool(b == 1)
            }
            2 => Const::Int(r.i64()?),
            3 => Const::Float(r.f64()?),
            4 => Const::Str(StrId(r.u32()?)),
            5 => {
                let n = r.u32()? as usize;
                let mut items = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    items.push(ConstId(r.u32()?));
                }
                Const::Array(items)
            }
            6 => {
                let n = r.u32()? as usize;
                let mut pairs = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    pairs.push((ConstId(r.u32()?), ConstId(r.u32()?)));
                }
                Const::Dict(pairs)
            }
            7 => {
                let some = r.u8()?;
                if some > 1 {
                    return Err(IrError::new("bad .zzc option const"));
                }
                Const::Option(if some == 1 {
                    Some(ConstId(r.u32()?))
                } else {
                    None
                })
            }
            8 => {
                let ok = r.u8()?;
                if ok > 1 {
                    return Err(IrError::new("bad .zzc result const"));
                }
                Const::Result {
                    ok: ok == 1,
                    val: ConstId(r.u32()?),
                }
            }
            _ => return Err(IrError::new("unknown .zzc const tag")),
        });
    }
    Ok(out)
}

/// Decoded function table plus per-function (offset, length) ranges
/// into the concatenated CODE/SPANS streams.
type FuncTable = (Vec<FuncDef>, Vec<(u32, u32)>);

fn decode_funcs(bytes: &[u8]) -> Result<FuncTable, IrError> {
    let mut r = Reader::new(bytes);
    let n = r.u32()? as usize;
    if n == 0 || n > 100_000 {
        return Err(IrError::new("bad .zzc func count"));
    }
    let mut funcs = Vec::with_capacity(n.min(1024));
    let mut ranges = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let name = StrId(r.u32()?);
        let arity = r.u32()?;
        let np = r.u32()? as usize;
        if np > 10_000 {
            return Err(IrError::new("absurd .zzc param count"));
        }
        let mut params = Vec::with_capacity(np.min(64));
        for _ in 0..np {
            let pname = StrId(r.u32()?);
            let has = r.u8()?;
            if has > 1 {
                return Err(IrError::new("bad .zzc param flag"));
            }
            params.push(Param {
                name: pname,
                default: if has == 1 {
                    Some(FuncId(r.u32()?))
                } else {
                    None
                },
            });
        }
        let ns = r.u32()? as usize;
        let mut sig_params = Vec::with_capacity(ns.min(64));
        for _ in 0..ns {
            sig_params.push(TypeId(r.u32()?));
        }
        let sig = FuncSig {
            params: sig_params,
            ret: TypeId(r.u32()?),
        };
        let off = r.u32()?;
        let len = r.u32()?;
        let max_stack = r.u32()?;
        let nt = r.u32()? as usize;
        let mut toplevel_slots = Vec::with_capacity(nt.min(1024));
        for _ in 0..nt {
            toplevel_slots.push((StrId(r.u32()?), r.u16()?));
        }
        ranges.push((off, len));
        funcs.push(FuncDef {
            name,
            arity,
            params,
            sig,
            toplevel_slots,
            code: Vec::new(),
            spans: Vec::new(),
            max_stack,
        });
    }
    Ok((funcs, ranges))
}

fn decode_code(bytes: &[u8]) -> Result<Vec<Op>, IrError> {
    let mut r = Reader::new(bytes);
    let n = r.u32()? as usize;
    if n > 10_000_000 {
        return Err(IrError::new("absurd .zzc op count"));
    }
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        out.push(decode_op(&mut r)?);
    }
    Ok(out)
}

fn decode_spans(bytes: &[u8]) -> Result<Vec<Span>, IrError> {
    let mut r = Reader::new(bytes);
    let n = r.u32()? as usize;
    if n > 10_000_000 {
        return Err(IrError::new("absurd .zzc span count"));
    }
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        out.push(Span::new(r.u32()?, r.u32()?));
    }
    Ok(out)
}

fn decode_annot(bytes: &[u8]) -> Result<(), IrError> {
    // v1 writes zero annotations; the length-prefixed shape lets future
    // decoders skip unknown entries.
    let mut r = Reader::new(bytes);
    let n = r.u32()? as usize;
    for _ in 0..n {
        let len = r.u32()? as usize;
        if r.bytes.get(r.pos..r.pos + len).is_none() {
            return r.err("annotation bytes");
        }
        r.pos += len;
    }
    Ok(())
}

fn str_ids(r: &mut Reader) -> Result<Vec<StrId>, IrError> {
    let n = r.u32()? as usize;
    if n > 10_000 {
        return Err(IrError::new("absurd .zzc id list"));
    }
    let mut out = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        out.push(StrId(r.u32()?));
    }
    Ok(out)
}

fn decode_binop(r: &mut Reader) -> Result<BinOp, IrError> {
    match r.u8()? {
        0 => Ok(BinOp::Add),
        1 => Ok(BinOp::Sub),
        2 => Ok(BinOp::Mul),
        3 => Ok(BinOp::Div),
        4 => Ok(BinOp::Rem),
        5 => Ok(BinOp::Pow),
        6 => Ok(BinOp::Eq),
        7 => Ok(BinOp::Ne),
        8 => Ok(BinOp::Lt),
        9 => Ok(BinOp::Gt),
        10 => Ok(BinOp::Le),
        11 => Ok(BinOp::Ge),
        12 => Ok(BinOp::And),
        13 => Ok(BinOp::Or),
        14 => Ok(BinOp::Elvis),
        15 => Ok(BinOp::BitAnd),
        16 => Ok(BinOp::BitOr),
        17 => Ok(BinOp::BitXor),
        18 => Ok(BinOp::Shl),
        19 => Ok(BinOp::Shr),
        _ => Err(IrError::new("unknown .zzc binop")),
    }
}

fn decode_unop(r: &mut Reader) -> Result<UnOp, IrError> {
    match r.u8()? {
        0 => Ok(UnOp::Neg),
        1 => Ok(UnOp::Pos),
        2 => Ok(UnOp::Not),
        3 => Ok(UnOp::BitNot),
        _ => Err(IrError::new("unknown .zzc unop")),
    }
}

fn decode_pattern(r: &mut Reader) -> Result<Pattern, IrError> {
    match r.u8()? {
        0 => Ok(Pattern::Wildcard),
        1 => Ok(Pattern::Binding(StrId(r.u32()?))),
        2 => Ok(Pattern::Lit(ConstId(r.u32()?))),
        3 => {
            let name = StrId(r.u32()?);
            let has = r.u8()?;
            if has > 1 {
                return Err(IrError::new("bad .zzc pattern flag"));
            }
            Ok(Pattern::Variant {
                name,
                arg: if has == 1 {
                    Some(Box::new(decode_pattern(r)?))
                } else {
                    None
                },
            })
        }
        4 => {
            let n = r.u32()? as usize;
            if n > 64 {
                return Err(IrError::new("absurd .zzc pattern arity"));
            }
            let mut pats = Vec::with_capacity(n);
            for _ in 0..n {
                pats.push(decode_pattern(r)?);
            }
            Ok(Pattern::Tuple(pats))
        }
        5 => {
            let n = r.u32()? as usize;
            if n > 64 {
                return Err(IrError::new("absurd .zzc pattern arity"));
            }
            let mut pats = Vec::with_capacity(n);
            for _ in 0..n {
                pats.push(decode_pattern(r)?);
            }
            Ok(Pattern::Or(pats))
        }
        _ => Err(IrError::new("unknown .zzc pattern tag")),
    }
}

fn decode_home(r: &mut Reader) -> Result<(Option<u16>, Option<StrId>), IrError> {
    match r.u8()? {
        0 => Ok((Some(r.u16()?), None)),
        1 => Ok((None, Some(StrId(r.u32()?)))),
        2 => Ok((None, None)),
        _ => Err(IrError::new("unknown .zzc home tag")),
    }
}

fn decode_op(r: &mut Reader) -> Result<Op, IrError> {
    match r.u16()? {
        0 => Ok(Op::PushConst(ConstId(r.u32()?))),
        1 => Ok(Op::Pop),
        74 => Ok(Op::Swap),
        2 => Ok(Op::PopN(r.u16()?)),
        3 => Ok(Op::Truthy),
        4 => Ok(Op::LoadVar(StrId(r.u32()?))),
        5 => Ok(Op::LoadPath(str_ids(r)?)),
        6 => Ok(Op::DefineVar(StrId(r.u32()?))),
        7 => Ok(Op::StoreVar(StrId(r.u32()?))),
        8 => Ok(Op::StorePath(str_ids(r)?)),
        9 => Ok(Op::LoadSlot(r.u16()?)),
        10 => Ok(Op::StoreSlot(r.u16()?)),
        11 => Ok(Op::TakeVar(StrId(r.u32()?))),
        12 => {
            let (home_slot, home_var) = decode_home(r)?;
            Ok(Op::VecPushField {
                home_slot,
                home_var,
                field: StrId(r.u32()?),
            })
        }
        13 => {
            let (home_slot, home_var) = decode_home(r)?;
            Ok(Op::VecPushMethod {
                home_slot,
                home_var,
                method: StrId(r.u32()?),
            })
        }
        14 => Ok(Op::IntAdd),
        15 => Ok(Op::IntSub),
        16 => Ok(Op::IntMul),
        17 => Ok(Op::IntDiv),
        18 => Ok(Op::IntRem),
        19 => Ok(Op::IntNeg),
        20 => Ok(Op::BinOp(decode_binop(r)?)),
        21 => Ok(Op::UnOp(decode_unop(r)?)),
        22 => Ok(Op::Jump(r.u32()?)),
        23 => Ok(Op::JumpIfFalse(r.u32()?)),
        24 => Ok(Op::JumpIfTrue(r.u32()?)),
        25 => Ok(Op::JumpIfFalseBool(r.u32()?)),
        26 => Ok(Op::Return),
        27 => Ok(Op::ForSetup {
            exit: r.u32()?,
            header: r.u32()?,
            num_vars: r.u8()?,
        }),
        28 => Ok(Op::ForNext {
            vars: str_ids(r)?,
            exit: r.u32()?,
            in_env: r.u8()? == 1,
        }),
        29 => Ok(Op::WhileSetup {
            exit: r.u32()?,
            header: r.u32()?,
        }),
        30 => Ok(Op::WhileCond { exit: r.u32()? }),
        31 => Ok(Op::Break),
        32 => Ok(Op::Continue),
        33 => Ok(Op::SetLoopResult),
        34 => Ok(Op::Safepoint),
        35 => Ok(Op::MakeArray(r.u16()?)),
        36 => Ok(Op::UnpackTuple(r.u8()?)),
        37 => Ok(Op::ArrayPush),
        38 => Ok(Op::MakeDict(r.u16()?)),
        39 => Ok(Op::IndexOp),
        40 => Ok(Op::StoreIndexOp),
        41 => Ok(Op::CompoundIndexOp {
            op: decode_binop(r)?,
        }),
        42 => Ok(Op::SliceOp),
        43 => Ok(Op::MakeRange),
        44 => Ok(Op::MakeStruct {
            name: StrId(r.u32()?),
            fields: str_ids(r)?,
        }),
        45 => Ok(Op::GetField(StrId(r.u32()?))),
        46 => Ok(Op::GetFieldIdx(r.u16()?)),
        47 => Ok(Op::SetField(StrId(r.u32()?))),
        48 => Ok(Op::SetFieldIdx(r.u16()?)),
        49 => Ok(Op::CompoundFieldOp {
            name: StrId(r.u32()?),
            op: decode_binop(r)?,
        }),
        50 => Ok(Op::RegisterStruct {
            name: StrId(r.u32()?),
            fields: str_ids(r)?,
        }),
        51 => {
            let name = StrId(r.u32()?);
            let n = r.u32()? as usize;
            if n > 1024 {
                return Err(IrError::new("absurd .zzc variant count"));
            }
            let mut variants = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                variants.push((StrId(r.u32()?), r.u8()? == 1));
            }
            Ok(Op::RegisterEnum { name, variants })
        }
        52 => Ok(Op::MakeEnum {
            enum_name: StrId(r.u32()?),
            variant: StrId(r.u32()?),
            argc: r.u16()?,
        }),
        53 => Ok(Op::MakeClosure {
            func: FuncId(r.u32()?),
        }),
        54 => Ok(Op::MakeFunc {
            func: FuncId(r.u32()?),
        }),
        55 => Ok(Op::SpawnClosure {
            func: FuncId(r.u32()?),
        }),
        56 => Ok(Op::MakeVariant {
            name: StrId(r.u32()?),
            has_arg: r.u8()? == 1,
        }),
        57 => Ok(Op::MatchArm {
            pat: decode_pattern(r)?,
            next: r.u32()?,
            has_env: r.u8()? == 1,
            restore: r.u8()? == 1,
        }),
        58 => Ok(Op::MatchGuard {
            next: r.u32()?,
            has_env: r.u8()? == 1,
        }),
        59 => Ok(Op::MatchError),
        60 => Ok(Op::IfLetMatch {
            pat: decode_pattern(r)?,
            els: r.u32()?,
            has_env: r.u8()? == 1,
        }),
        61 => Ok(Op::TryOp),
        62 => Ok(Op::Elvis),
        63 => Ok(Op::ElvisResult),
        64 => Ok(Op::Call { argc: r.u16()? }),
        65 => {
            let parts = str_ids(r)?;
            let argc = r.u16()?;
            Ok(Op::CallPath {
                parts,
                argc,
                pspan: Span::new(r.u32()?, r.u32()?),
            })
        }
        66 => Ok(Op::CallMethod {
            name: StrId(r.u32()?),
            argc: r.u16()?,
        }),
        67 => Ok(Op::CallNative {
            name: StrId(r.u32()?),
            argc: r.u16()?,
        }),
        68 => Ok(Op::Concat(r.u16()?)),
        69 => Ok(Op::FormatValue),
        70 => Ok(Op::DbQuery { nparams: r.u16()? }),
        71 => Ok(Op::EnterScope),
        72 => Ok(Op::ExitScope),
        73 => Ok(Op::DeferRecord),
        _ => Err(IrError::new("unknown .zzc op tag")),
    }
}
