//! `zz dis`: stable disassembly of a `.zzc` module.
//!
//! Output is deterministic for a given module (function order, op order,
//! resolved names and constants inline) so it golden-tests cleanly.

use crate::{Const, Module, Op, Pattern};

fn const_text(module: &Module, id: crate::ConstId) -> String {
    match module.consts.get(id.0 as usize) {
        Some(Const::Unit) => "unit".to_string(),
        Some(Const::Bool(b)) => format!("bool({b})"),
        Some(Const::Int(i)) => format!("int({i})"),
        Some(Const::Float(f)) => format!("float({f:?})"),
        Some(Const::Str(s)) => format!("str({})", str_text(module, *s)),
        Some(Const::Array(items)) => format!("array[{}]", items.len()),
        Some(Const::Dict(pairs)) => format!("dict[{}]", pairs.len()),
        Some(Const::Option(inner)) => match inner {
            Some(x) => format!("option(some c{})", x.0),
            None => "option(none)".to_string(),
        },
        Some(Const::Result { ok, val }) => {
            format!("result({} c{})", if *ok { "ok" } else { "err" }, val.0)
        }
        None => format!("c{}?<missing>", id.0),
    }
}

fn str_text(module: &Module, id: crate::StrId) -> String {
    match module.strings.get(id.0 as usize) {
        Some(s) => format!("{s:?}"),
        None => format!("s{}?<missing>", id.0),
    }
}

fn pat_text(module: &Module, pat: &Pattern) -> String {
    match pat {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Binding(id) => str_text(module, *id),
        Pattern::Lit(id) => const_text(module, *id),
        Pattern::Variant { name, arg } => match arg {
            Some(p) => format!("{}({})", str_text(module, *name), pat_text(module, p)),
            None => str_text(module, *name),
        },
        Pattern::Tuple(pats) => {
            let inner: Vec<String> = pats.iter().map(|p| pat_text(module, p)).collect();
            format!("({})", inner.join(", "))
        }
        Pattern::Or(pats) => {
            let inner: Vec<String> = pats.iter().map(|p| pat_text(module, p)).collect();
            inner.join(" | ")
        }
    }
}

fn op_text(module: &Module, op: &Op) -> String {
    match op {
        Op::PushConst(id) => format!("const c{} ; {}", id.0, const_text(module, *id)),
        Op::Pop => "pop".to_string(),
        Op::Swap => "swap".to_string(),
        Op::PopN(n) => format!("popn {n}"),
        Op::Truthy => "truthy".to_string(),
        Op::LoadVar(id) => format!("load {}", str_text(module, *id)),
        Op::LoadPath(parts) => {
            let p: Vec<String> = parts.iter().map(|x| str_text(module, *x)).collect();
            format!("loadpath {}", p.join("."))
        }
        Op::DefineVar(id) => format!("define {}", str_text(module, *id)),
        Op::StoreVar(id) => format!("store {}", str_text(module, *id)),
        Op::StorePath(parts) => {
            let p: Vec<String> = parts.iter().map(|x| str_text(module, *x)).collect();
            format!("storepath {}", p.join("."))
        }
        Op::LoadSlot(s) => format!("loadslot {s}"),
        Op::StoreSlot(s) => format!("storeslot {s}"),
        Op::TakeVar(id) => format!("take {}", str_text(module, *id)),
        Op::VecPushField {
            home_slot,
            home_var,
            field,
        } => {
            format!(
                "vecpushfield {} {}",
                home_text(home_slot, home_var, module),
                str_text(module, *field)
            )
        }
        Op::VecPushMethod {
            home_slot,
            home_var,
            method,
        } => {
            format!(
                "vecpushmethod {} {}",
                home_text(home_slot, home_var, module),
                str_text(module, *method)
            )
        }
        Op::IntAdd => "iadd".to_string(),
        Op::IntSub => "isub".to_string(),
        Op::IntMul => "imul".to_string(),
        Op::IntDiv => "idiv".to_string(),
        Op::IntRem => "irem".to_string(),
        Op::IntNeg => "ineg".to_string(),
        Op::BinOp(o) => format!("binop {o:?}"),
        Op::UnOp(o) => format!("unop {o:?}"),
        Op::Jump(t) => format!("jmp {t}"),
        Op::JumpIfFalse(t) => format!("jf {t}"),
        Op::JumpIfTrue(t) => format!("jt {t}"),
        Op::JumpIfFalseBool(t) => format!("jfb {t}"),
        Op::Return => "ret".to_string(),
        Op::ForSetup {
            exit,
            header,
            num_vars,
        } => {
            format!("forsetup exit={exit} header={header} vars={num_vars}")
        }
        Op::ForNext { vars, exit, in_env } => {
            let v: Vec<String> = vars.iter().map(|x| str_text(module, *x)).collect();
            format!("fornext [{}] exit={exit} env={in_env}", v.join(", "))
        }
        Op::WhileSetup { exit, header } => format!("whilesetup exit={exit} header={header}"),
        Op::WhileCond { exit } => format!("whilecond exit={exit}"),
        Op::Break => "break".to_string(),
        Op::Continue => "continue".to_string(),
        Op::SetLoopResult => "setloopresult".to_string(),
        Op::Safepoint => "safepoint".to_string(),
        Op::MakeArray(n) => format!("makearray {n}"),
        Op::UnpackTuple(n) => format!("unpack {n}"),
        Op::ArrayPush => "arraypush".to_string(),
        Op::MakeDict(n) => format!("makedict {n}"),
        Op::IndexOp => "index".to_string(),
        Op::StoreIndexOp => "storeindex".to_string(),
        Op::CompoundIndexOp { op } => format!("compoundindex {op:?}"),
        Op::SliceOp => "slice".to_string(),
        Op::MakeRange => "makerange".to_string(),
        Op::MakeStruct { name, fields } => {
            let f: Vec<String> = fields.iter().map(|x| str_text(module, *x)).collect();
            format!("makestruct {} ({})", str_text(module, *name), f.join(", "))
        }
        Op::GetField(id) => format!("getfield {}", str_text(module, *id)),
        Op::GetFieldIdx(i) => format!("getfieldidx {i}"),
        Op::SetField(id) => format!("setfield {}", str_text(module, *id)),
        Op::SetFieldIdx(i) => format!("setfieldidx {i}"),
        Op::CompoundFieldOp { name, op } => {
            format!("compoundfield {} {op:?}", str_text(module, *name))
        }
        Op::RegisterStruct { name, fields } => {
            let f: Vec<String> = fields.iter().map(|x| str_text(module, *x)).collect();
            format!("regstruct {} ({})", str_text(module, *name), f.join(", "))
        }
        Op::RegisterEnum { name, variants } => {
            let v: Vec<String> = variants
                .iter()
                .map(|(id, a)| format!("{}{}", str_text(module, *id), if *a { "(+)" } else { "" }))
                .collect();
            format!("regenum {} ({})", str_text(module, *name), v.join(", "))
        }
        Op::MakeEnum {
            enum_name,
            variant,
            argc,
        } => format!(
            "makeenum {}.{} argc={argc}",
            str_text(module, *enum_name),
            str_text(module, *variant)
        ),
        Op::MakeClosure { func } => format!("makeclosure f{}", func.0),
        Op::MakeFunc { func } => format!("makefunc f{}", func.0),
        Op::SpawnClosure { func } => format!("spawnclosure f{}", func.0),
        Op::MakeVariant { name, has_arg } => {
            format!("makevariant {} arg={has_arg}", str_text(module, *name))
        }
        Op::MatchArm {
            pat,
            next,
            has_env,
            restore,
        } => {
            format!(
                "matcharm {} next={next} env={has_env} restore={restore}",
                pat_text(module, pat)
            )
        }
        Op::MatchGuard { next, has_env } => format!("matchguard next={next} env={has_env}"),
        Op::MatchError => "matcherror".to_string(),
        Op::IfLetMatch { pat, els, has_env } => {
            format!("iflet {} els={els} env={has_env}", pat_text(module, pat))
        }
        Op::TryOp => "try".to_string(),
        Op::Elvis => "elvis".to_string(),
        Op::ElvisResult => "elvisresult".to_string(),
        Op::Call { argc } => format!("call argc={argc}"),
        Op::CallPath { parts, argc, .. } => {
            let p: Vec<String> = parts.iter().map(|x| str_text(module, *x)).collect();
            format!("callpath {} argc={argc}", p.join("."))
        }
        Op::CallMethod { name, argc } => {
            format!("callmethod {} argc={argc}", str_text(module, *name))
        }
        Op::CallNative { name, argc } => {
            format!("callnative {} argc={argc}", str_text(module, *name))
        }
        Op::Concat(n) => format!("concat {n}"),
        Op::FormatValue => "formatvalue".to_string(),
        Op::DbQuery { nparams } => format!("dbquery nparams={nparams}"),
        Op::EnterScope => "enterscope".to_string(),
        Op::ExitScope => "exitscope".to_string(),
        Op::DeferRecord => "defer".to_string(),
    }
}

fn home_text(home_slot: &Option<u16>, home_var: &Option<crate::StrId>, module: &Module) -> String {
    match (home_slot, home_var) {
        (Some(s), _) => format!("slot({s})"),
        (None, Some(v)) => format!("var({})", str_text(module, *v)),
        (None, None) => "<no-home>".to_string(),
    }
}

/// Render a function signature with resolved type names.
fn sig_text(module: &Module, sig: &crate::FuncSig) -> String {
    let ty = |id: crate::TypeId| {
        module
            .types
            .get(id.0 as usize)
            .map(|t| type_text(module, t))
            .unwrap_or_else(|| format!("t{}?<missing>", id.0))
    };
    let params: Vec<String> = sig.params.iter().map(|id| ty(*id)).collect();
    format!("({})->{}", params.join(", "), ty(sig.ret))
}

/// Render a type with names resolved where cheap; nested ids inline.
pub(crate) fn type_text(module: &Module, t: &crate::IrType) -> String {
    use crate::IrType as T;
    match t {
        T::Unknown => "unknown".to_string(),
        T::Unit => "unit".to_string(),
        T::Bool => "bool".to_string(),
        T::Int => "int".to_string(),
        T::Float => "float".to_string(),
        T::Str => "str".to_string(),
        T::Bytes => "bytes".to_string(),
        T::Json => "json".to_string(),
        T::Db => "db".to_string(),
        T::HttpServer => "httpserver".to_string(),
        T::TcpStream => "tcpstream".to_string(),
        T::TcpListener => "tcplistener".to_string(),
        T::Response => "response".to_string(),
        T::HttpRequest => "httprequest".to_string(),
        T::Chan => "chan".to_string(),
        T::TaskJoin => "taskjoin".to_string(),
        T::Error => "error".to_string(),
        T::Void => "void".to_string(),
        T::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(|id| short_type(module, *id)).collect();
            format!("({})", inner.join(", "))
        }
        T::Option(inner) => format!("option({})", short_type(module, *inner)),
        T::Result(a, b) => {
            format!(
                "result({}, {})",
                short_type(module, *a),
                short_type(module, *b)
            )
        }
        T::Array(inner) => format!("[{}]", short_type(module, *inner)),
        T::Dict(k, v) => {
            format!("{{{}: {}}}", short_type(module, *k), short_type(module, *v))
        }
        T::Func(params, ret) => {
            let ps: Vec<String> = params.iter().map(|id| short_type(module, *id)).collect();
            format!("func({})->{}", ps.join(", "), short_type(module, *ret))
        }
        T::Range(inner) => format!("range({})", short_type(module, *inner)),
        T::Opaque(id) => format!("opaque({})", str_text(module, *id)),
        T::Struct(id, args) => {
            let a: Vec<String> = args.iter().map(|x| short_type(module, *x)).collect();
            if a.is_empty() {
                str_text(module, *id)
            } else {
                format!("{}[{}]", str_text(module, *id), a.join(", "))
            }
        }
        T::Enum(id, args) => {
            let a: Vec<String> = args.iter().map(|x| short_type(module, *x)).collect();
            if a.is_empty() {
                str_text(module, *id)
            } else {
                format!("{}[{}]", str_text(module, *id), a.join(", "))
            }
        }
        T::Union(items) => {
            let inner: Vec<String> = items.iter().map(|id| short_type(module, *id)).collect();
            format!("union({})", inner.join(" | "))
        }
        T::Named(id) => format!("named({})", str_text(module, *id)),
        T::Var(n) => format!("var({n})"),
        T::Ptr { mutable, inner } => format!(
            "{}({})",
            if *mutable { "*mut" } else { "*const" },
            short_type(module, *inner)
        ),
    }
}

fn short_type(module: &Module, id: crate::TypeId) -> String {
    module
        .types
        .get(id.0 as usize)
        .map(|t| type_text(module, t))
        .unwrap_or_else(|| "?".to_string())
}

/// Disassemble a module to stable text: one `func` block per table
/// entry, indexed ops with resolved names.
pub fn disassemble(module: &Module) -> String {
    let mut out = String::new();
    out.push_str(&format!("; zzcz v{}\n", crate::VERSION));
    out.push_str(&format!(
        "; types={} strings={} consts={} funcs={}\n",
        module.types.len(),
        module.strings.len(),
        module.consts.len(),
        module.funcs.len()
    ));
    for (i, f) in module.funcs.iter().enumerate() {
        let entry = if crate::FuncId(i as u32) == module.entry {
            " entry"
        } else {
            ""
        };
        out.push_str(&format!(
            "func f{i} {} arity={}{} max_stack={} sig={} locals=[{}]\n",
            str_text(module, f.name),
            f.arity,
            entry,
            f.max_stack,
            sig_text(module, &f.sig),
            f.locals
                .iter()
                .map(|id| short_type(module, *id))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for (pc, op) in f.code.iter().enumerate() {
            let span = f.spans.get(pc).copied().unwrap_or(crate::Span::new(0, 0));
            out.push_str(&format!(
                "  {pc:04}  {}  ; @{}..{}\n",
                op_text(module, op),
                span.start,
                span.end
            ));
        }
    }
    out
}
