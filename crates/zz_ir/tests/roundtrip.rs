//! `.zzc` round-trip tests: compile → lower → encode → decode →
//! verify → raise → execute, and the loaded run must equal the direct
//! run with no AST on the load path.

use zz_ir::{codec, lower, raise, verify};
use zz_runtime::vm::Compiler;
use zz_runtime::{Interp, Value};

fn round_trip_value(src: &str) -> Value {
    let parsed = zz_frontend::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    // Direct run (AST path).
    let mut direct = Interp::new();
    let want = direct.run(&parsed.program).expect("direct run failed");
    // Bytecode path: only bytes cross into execution.
    let chunk = Compiler::compile_program(&parsed.program);
    let module = lower::lower(&chunk).expect("lower failed");
    let bytes = codec::encode(&module);
    drop(chunk);
    let loaded = codec::decode(&bytes).expect("decode failed");
    verify::verify(&loaded).expect("verify failed");
    let chunk = raise::raise(&loaded).expect("raise failed");
    let mut interp = Interp::new();
    let got = interp.run_loaded_chunk(&chunk).expect("loaded run failed");
    assert_eq!(got, want, "loaded run differs for: {src}");
    // Bit-identical re-encoding (deterministic encoder).
    let bytes2 = codec::encode(&loaded);
    assert_eq!(bytes, bytes2, "re-encode differs for: {src}");
    got
}

#[test]
fn round_trip_arithmetic() {
    assert_eq!(round_trip_value("1 + 2 * 3"), Value::Int(7));
}

#[test]
fn round_trip_locals_and_loop() {
    assert_eq!(
        round_trip_value("s := 0\nfor i in 0..10 {\n s = s + i\n}\ns"),
        Value::Int(45)
    );
}

#[test]
fn round_trip_default_args() {
    // Default bodies travel as tabled chunks (no AST on the load path).
    assert_eq!(
        round_trip_value("func f(a = 41) {\n a + 1\n}\nf()"),
        Value::Int(42)
    );
    assert_eq!(
        round_trip_value("func f(a = 41) {\n a + 1\n}\nf(10)"),
        Value::Int(11)
    );
}

#[test]
fn round_trip_closure_capture() {
    assert_eq!(
        round_trip_value("func mk() {\n n := 10\n |x| { n + x }\n}\nf := mk()\nf(5)"),
        Value::Int(15)
    );
}

#[test]
fn round_trip_match() {
    assert_eq!(
        round_trip_value("x := 2\nmatch x {\n 1 => \"a\",\n 2 => \"b\",\n _ => \"c\"\n}"),
        Value::Str("b".to_string().into())
    );
}

#[test]
fn round_trip_struct_store() {
    // Exercises Swap + SetField through the new store order.
    assert_eq!(
        round_trip_value("struct P { x: int }\np := P{x: 3}\np.x = 4\np.x"),
        Value::Int(4)
    );
}

#[test]
fn round_trip_index_store_order() {
    // Source order: base(1), index(0), value(9). Needs the `vec` native,
    // so this test seeds stdlib natives on both interpreters.
    let parsed = zz_frontend::parse(
        "log := []\nfunc rec(n: int) -> int {\n log = vec.push(log, n)\n n\n}\nfunc mkarr() {\n rec(1)\n [10, 20, 30]\n}\nmkarr()[rec(0)] = rec(9)\nlog",
    );
    assert!(parsed.errors.is_empty());
    let natives = zz_stdlib::stdlib_natives();
    let mut direct = Interp::with_natives(natives.clone());
    let want = direct.run(&parsed.program).expect("direct run failed");
    let chunk = Compiler::compile_program(&parsed.program);
    let module = lower::lower(&chunk).expect("lower failed");
    let bytes = codec::encode(&module);
    drop(chunk);
    let loaded = codec::decode(&bytes).expect("decode failed");
    verify::verify(&loaded).expect("verify failed");
    let chunk = raise::raise(&loaded).expect("raise failed");
    let mut interp = Interp::with_natives(natives);
    let got = interp.run_loaded_chunk(&chunk).expect("loaded run failed");
    assert_eq!(got, want);
    match got {
        Value::Array(items) => {
            let got: Vec<i64> = items
                .iter()
                .map(|x| match x {
                    Value::Int(i) => *i,
                    other => panic!("non-int log entry: {other:?}"),
                })
                .collect();
            assert_eq!(got, vec![1, 0, 9]);
        }
        other => panic!("expected array log, got {other:?}"),
    }
}

#[test]
fn verify_rejects_bad_jump() {
    let parsed = zz_frontend::parse("1 + 2").program;
    let chunk = Compiler::compile_program(&parsed);
    let mut module = lower::lower(&chunk).expect("lower failed");
    module.funcs[0].code.push(zz_ir::Op::Jump(9999));
    module.funcs[0].spans.push(zz_ir::Span::new(0, 0));
    let err = verify::verify(&module).expect_err("bad jump accepted");
    assert!(err.message.contains("jump target"), "wrong error: {err}");
}

#[test]
fn verify_matcherror_is_terminal() {
    // Tail match with a dead non-exhaustive trap + slot cleanup (#253):
    // `MatchError` aborts at runtime, so the verifier must not feed its
    // restored-scrutinee state into the join — nor check the epilogue
    // against it. Before the fix this failed with a spurious falloff
    // return mismatch (the trap's `Result` outliving the arms' `Str`).
    use zz_ir::{FuncDef, FuncSig, Module, Op, Pattern, Span};
    let module = Module {
        types: vec![
            zz_ir::IrType::Unknown,
            zz_ir::IrType::Str,
            zz_ir::IrType::Result(zz_ir::TypeId(1), zz_ir::TypeId(1)),
        ],
        strings: vec!["f".to_string(), "x".to_string()],
        consts: vec![
            zz_ir::Const::Str(zz_ir::StrId(1)),
            zz_ir::Const::Str(zz_ir::StrId(1)),
        ],
        funcs: vec![FuncDef {
            name: zz_ir::StrId(0),
            arity: 0,
            params: vec![],
            sig: FuncSig {
                params: vec![],
                ret: zz_ir::TypeId(1),
            },
            toplevel_slots: vec![],
            vartab: vec![],
            locals: vec![zz_ir::TypeId(1), zz_ir::TypeId(2)],
            // base, scrutinee (declared Result), arm to trap terminator,
            // keep-top cleanup: the trap edge is dead, the arm value
            // (Str) is what the epilogue returns.
            code: vec![
                Op::PushConst(zz_ir::ConstId(0)),
                Op::LoadSlot(1),
                Op::MatchArm {
                    pat: Pattern::Wildcard,
                    next: 5,
                    has_env: false,
                    restore: true,
                },
                Op::PushConst(zz_ir::ConstId(1)),
                Op::Jump(6),
                Op::MatchError,
                Op::PopN(1),
            ],
            spans: vec![Span::new(0, 0); 7],
            max_stack: 2,
        }],
        entry: zz_ir::FuncId(0),
    };
    verify::verify(&module).expect("matcherror trap edge must be dead");
}

#[test]
fn verify_rejects_join_mismatch() {
    // if/else shape with unbalanced arms: join depths diverge.
    use zz_ir::{FuncDef, FuncSig, Module, Op, Span};
    let jfb = Op::JumpIfFalseBool(3);
    let module = Module {
        types: vec![zz_ir::IrType::Unknown],
        strings: vec!["b".to_string()],
        consts: vec![
            zz_ir::Const::Bool(true),
            zz_ir::Const::Int(1),
            zz_ir::Const::Int(2),
        ],
        funcs: vec![FuncDef {
            name: zz_ir::StrId(0),
            arity: 0,
            params: vec![],
            sig: FuncSig {
                params: vec![],
                ret: zz_ir::TypeId(0),
            },
            toplevel_slots: vec![],
            vartab: vec![],
            locals: vec![],
            // arm 1 pushes two values, arm 2 pushes one → join mismatch.
            code: vec![
                Op::PushConst(zz_ir::ConstId(0)),
                jfb,
                Op::PushConst(zz_ir::ConstId(1)),
                Op::PushConst(zz_ir::ConstId(2)),
            ],
            spans: vec![Span::new(0, 0); 4],
            max_stack: 3,
        }],
        entry: zz_ir::FuncId(0),
    };
    let err = verify::verify(&module).expect_err("join mismatch accepted");
    assert!(err.message.contains("join depth"), "wrong error: {err}");
}

#[test]
fn decode_rejects_garbage_without_panicking() {
    // Malformed inputs must surface as Err, never panic (spec §8).
    // Seeded xorshift: deterministic, no dev-dependency.
    let parsed = zz_frontend::parse(
        "s := 0\nfor i in 0..20 {\n s = s + i\n}\nfunc f(a = 3) {\n a * 2\n}\nf(s)",
    )
    .program;
    let chunk = Compiler::compile_program(&parsed);
    let module = lower::lower(&chunk).expect("lower failed");
    let bytes = codec::encode(&module);
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..500 {
        let mut bad = bytes.clone();
        let n = 1 + (next() % 4) as usize;
        for _ in 0..n {
            let i = (next() as usize) % bad.len();
            bad[i] ^= 1 + (next() % 255) as u8;
        }
        // A panic here fails the test — that is the assertion.
        let _ = codec::decode(&bad).and_then(|m| {
            verify::verify(&m)?;
            Ok(())
        });
        let _ = round;
    }
}

#[test]
fn dis_is_stable() {
    let parsed = zz_frontend::parse("x := 1 + 2\nx").program;
    let chunk = Compiler::compile_program(&parsed);
    let module = lower::lower(&chunk).expect("lower failed");
    let a = zz_ir::dis::disassemble(&module);
    let b = zz_ir::dis::disassemble(&module);
    assert_eq!(a, b);
    assert!(a.contains("func f0"), "missing func header:\n{a}");
    assert!(a.contains("binop Add"), "missing add op:\n{a}");
}

/// v3 carries the compiler's fused slot fast paths through the format.
/// The interpreter must get the op back on the load path (the desugared
/// form paid three extra stack ops per loop iteration; that is the whole
/// reason the format changed), while the chunk→C backend must still see
/// the desugared stream it is tuned for.
#[test]
fn fused_slot_ops_survive_the_round_trip() {
    fn is_fused(op: &zz_ir::Op) -> bool {
        use zz_ir::Op::*;
        matches!(
            op,
            SlotAddInt { .. }
                | SlotInc { .. }
                | SlotAddIntImm { .. }
                | SlotLessIntSlot { .. }
                | SlotLessIntImm { .. }
                | SlotBinaryInt { .. }
                | SlotBinaryIntImm { .. }
        )
    }
    // Typed compile: the fusion sits behind the compiler's `Int` type
    // guard, which is exactly what a real `zz run` supplies.
    let module = lower_typed_spanmap(
        "s := 0\nfor _i in 0..10 {\n s = s + 1\n}\ns",
        &std::collections::HashMap::new(),
    );
    let dis = zz_ir::dis::disassemble(&module);
    let count = |m: &zz_ir::Module| -> usize {
        m.funcs
            .iter()
            .flat_map(|f| f.code.iter())
            .filter(|o| is_fused(o))
            .count()
    };
    assert!(count(&module) > 0, "fusion lost in lowering:\n{dis}");
    assert!(dis.contains("slotinc"), "dis must show SlotInc:\n{dis}");
    // The bytes keep them, and the decoder hands the same module back.
    let bytes = codec::encode(&module);
    let loaded = codec::decode(&bytes).expect("decode failed");
    verify::verify(&loaded).expect("verify failed");
    assert_eq!(codec::encode(&loaded), bytes, "re-encode differs");
    assert_eq!(count(&loaded), count(&module), "fused ops lost in codec");
    // Raising restores the VM op itself, not its expansion — and the
    // loaded run still produces the program's value.
    fn has_slot_inc(chunk: &zz_runtime::vm::Chunk) -> bool {
        chunk.code.iter().any(|op| match op {
            zz_runtime::vm::Op::SlotInc { .. } => true,
            zz_runtime::vm::Op::MakeFunc { chunk, .. } => has_slot_inc(chunk),
            _ => false,
        })
    }
    let raised = raise::raise(&loaded).expect("raise failed");
    assert!(
        has_slot_inc(&raised),
        "raise must restore the fused op, got {:?}",
        raised.code
    );
    // Both forms must run the loop: the typed entry compiles top-level
    // statements as statements, so the effect is read back from the
    // promoted top-level slot (`s`) rather than the program value.
    let mut interp = Interp::new();
    let got = interp.run_loaded_chunk(&raised).expect("loaded run failed");
    assert_eq!(
        interp.env.get("s"),
        Some(Value::Int(10)),
        "fused-op loop did not run (program value {got:?})"
    );
    // The chunk→C entry stream: expanded back to the pre-v3 core ops, one
    // extra value-producing op per fused op, same behavior.
    let expanded = lower::expand_fused(&loaded).expect("expand failed");
    verify::verify(&expanded).expect("expanded must verify");
    assert_eq!(count(&expanded), 0, "expansion left fused ops behind");
    let n_fused_ops: usize = loaded.funcs.iter().map(|f| f.code.len()).sum();
    let n_expanded_ops: usize = expanded.funcs.iter().map(|f| f.code.len()).sum();
    assert!(
        n_expanded_ops > n_fused_ops,
        "expansion must re-add the stack traffic ({n_expanded_ops} vs {n_fused_ops})"
    );
    let reexpanded = raise::raise(&expanded).expect("raise failed");
    let mut interp2 = Interp::new();
    let got2 = interp2
        .run_loaded_chunk(&reexpanded)
        .expect("expanded run failed");
    assert_eq!(
        interp2.env.get("s"),
        Some(Value::Int(10)),
        "expanded loop did not run"
    );
    assert_eq!(got2, got, "expanded run differs from fused run");
}

#[test]
fn typed_signatures_populate() {
    use std::collections::HashMap;
    use zz_checker::{FuncSig as CheckerSig, Type as CheckerType};
    // HIR-style signature for `add`: (int, int) -> int.
    let sig = CheckerSig {
        generics: vec![],
        bounds: vec![],
        params: vec![
            ("a".to_string(), CheckerType::Int),
            ("b".to_string(), CheckerType::Int),
        ],
        has_default: vec![false, false],
        ret: CheckerType::Int,
        is_extern: false,
        extern_c_symbol: None,
    };
    let mut sigs = HashMap::new();
    sigs.insert("add".to_string(), sig);
    let parsed = zz_frontend::parse("func add(a: int, b: int) -> int {\n a + b\n}\nadd(40, 2)");
    assert!(parsed.errors.is_empty());
    let chunk = Compiler::compile_program(&parsed.program);
    let module = lower::lower_typed(&chunk, &sigs).expect("lower_typed failed");
    verify::verify(&module).expect("verify failed");
    // Type table holds Int (Unknown may also appear for entry/closures).
    assert!(module.types.contains(&zz_ir::IrType::Int));
    let f = module
        .funcs
        .iter()
        .find(|f| module.strings.get(f.name.0 as usize).map(String::as_str) == Some("add"))
        .expect("add lifted");
    let int = module
        .types
        .iter()
        .position(|t| *t == zz_ir::IrType::Int)
        .unwrap() as u32;
    assert_eq!(
        f.sig,
        zz_ir::FuncSig {
            params: vec![zz_ir::TypeId(int), zz_ir::TypeId(int)],
            ret: zz_ir::TypeId(int),
        }
    );
    // Round-trip determinism holds with real signatures too.
    let bytes = codec::encode(&module);
    let loaded = codec::decode(&bytes).expect("decode failed");
    assert_eq!(codec::encode(&loaded), bytes);
}

fn check_typed(
    src: &str,
) -> (
    std::collections::HashMap<zz_checker::SpanKey, zz_checker::Type>,
    zz_frontend::ast::Program,
) {
    use std::collections::HashMap;
    let parsed = zz_frontend::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (result, spanmap) = zz_checker::check_program_typed(
        &parsed.program,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    assert!(
        result.errors.is_empty(),
        "check errors: {:?}",
        result.errors
    );
    (spanmap, parsed.program)
}

fn int_sig(params: usize) -> zz_checker::FuncSig {
    zz_checker::FuncSig {
        generics: vec![],
        bounds: vec![],
        params: (0..params)
            .map(|i| (format!("p{i}"), zz_checker::Type::Int))
            .collect(),
        has_default: vec![false; params],
        ret: zz_checker::Type::Int,
        is_extern: false,
        extern_c_symbol: None,
    }
}

fn lower_typed_spanmap(
    src: &str,
    sigs: &std::collections::HashMap<String, zz_checker::FuncSig>,
) -> zz_ir::Module {
    use std::collections::HashMap;
    use std::sync::Arc;
    let (spanmap, program) = check_typed(src);
    let chunk = zz_runtime::vm::Compiler::compile_program_typed(
        &program,
        Arc::new(spanmap),
        HashMap::new(),
        HashMap::new(),
        Arc::new(std::collections::HashSet::new()),
    );
    zz_ir::lower::lower_typed(&chunk, sigs).expect("lower_typed failed")
}

fn local_types(module: &zz_ir::Module, fname: &str) -> Vec<String> {
    let f = module
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, f)| module.strings.get(f.name.0 as usize).map(String::as_str) == Some(fname))
        // The entry chunk ("main") shadows a user `func main`: prefer
        // the non-entry function.
        .find(|(i, _)| zz_ir::FuncId(*i as u32) != module.entry)
        .map(|(_, f)| f)
        .unwrap_or_else(|| panic!("{fname} lifted"));
    verify::verify(module).expect("verify failed");
    f.locals
        .iter()
        .map(|id| {
            module
                .types
                .get(id.0 as usize)
                .map(|t| format!("{t:?}"))
                .unwrap_or_else(|| "?".to_string())
        })
        .collect()
}

#[test]
fn locals_declare_int_slots() {
    use std::collections::HashMap;
    let mut sigs = HashMap::new();
    sigs.insert("f".to_string(), int_sig(1));
    let module = lower_typed_spanmap(
        "func f(n: int) -> int {\n s := n + 1\n s = s + n\n s\n}\nf(1)",
        &sigs,
    );
    // Slot 0 = param n (Int from sig), slot 1 = s (Int recorded).
    assert_eq!(local_types(&module, "f"), vec!["Int".to_string(); 2]);
}

#[test]
fn locals_declare_loop_and_array_slots() {
    use std::collections::HashMap;
    let mut sigs = HashMap::new();
    sigs.insert("m".to_string(), int_sig(0));
    let module = lower_typed_spanmap(
        "func m() {\n a := [1, 2]\n a[0] = 3\n s := 0\n for v in a {\n s = s + v\n }\n s\n}\nm()",
        &sigs,
    );
    let locals = local_types(&module, "m");
    // a = Array(Int) (literal carries element types), s/v = Int.
    assert!(
        locals.iter().any(|t| t.starts_with("Array(")),
        "array slot missing: {locals:?}"
    );
    assert!(
        locals.iter().filter(|t| *t == "Int").count() >= 2,
        "int slots missing: {locals:?}"
    );
    // No slot may be *wrongly* typed here (Unknown tolerated only for
    // machine temps, never a named local).
    assert!(
        !locals.iter().any(|t| t == "Bool" || t == "Str"),
        "wrong slot type: {locals:?}"
    );
}

#[test]
fn locals_reuse_conflicts_box() {
    use std::collections::HashMap;
    let sigs = HashMap::new();
    let module = lower_typed_spanmap(
        "func main() {\n { s := 0\n s = s + 1 }\n { s := \"x\"\n s = s }\n}\n",
        &sigs,
    );
    let locals = local_types(&module, "main");
    // One slot holds Int then Str across disjoint scopes: genuinely
    // polymorphic, so the table honestly reports the "unknown" top
    // (`Error`, wildcard-accept + never unboxed). A `Union` here would
    // poison every load into member-typed stores.
    assert!(
        locals.iter().any(|t| t == "Error"),
        "reuse did not box: {locals:?}"
    );
}

#[test]
fn verify_rejects_slot_store_mismatch() {
    use zz_ir::{FuncDef, FuncSig, Module, Op, Span};
    // Hand-built corrupt module: slot 0 declared Int, stores Bool.
    let u = zz_ir::TypeId(0);
    let int = zz_ir::TypeId(1);
    let module = Module {
        types: vec![
            zz_ir::IrType::Unknown,
            zz_ir::IrType::Int,
            zz_ir::IrType::Bool,
        ],
        strings: vec!["f".to_string()],
        consts: vec![zz_ir::Const::Bool(true)],
        funcs: vec![FuncDef {
            name: zz_ir::StrId(0),
            arity: 0,
            params: vec![],
            sig: FuncSig {
                params: vec![],
                ret: u,
            },
            locals: vec![int],
            toplevel_slots: vec![],
            vartab: vec![],
            code: vec![Op::PushConst(zz_ir::ConstId(0)), Op::StoreSlot(0)],
            spans: vec![Span::new(0, 0); 2],
            max_stack: 1,
        }],
        entry: zz_ir::FuncId(0),
    };
    let err = verify::verify(&module).expect_err("store mismatch accepted");
    assert!(err.message.contains("type mismatch"), "wrong error: {err}");
}

#[test]
fn verify_rejects_bad_call() {
    use zz_ir::{FuncDef, FuncSig, Module, Op, Span, StrId, TypeId};
    // callee g(x: int) -> int; caller passes a bool.
    let u = TypeId(0);
    let int = TypeId(1);
    let caller = FuncDef {
        name: StrId(0),
        arity: 0,
        params: vec![],
        sig: FuncSig {
            params: vec![],
            ret: u,
        },
        locals: vec![],
        toplevel_slots: vec![],
        vartab: vec![],
        code: vec![
            Op::PushConst(zz_ir::ConstId(0)),
            Op::CallPath {
                parts: vec![StrId(1)],
                argc: 1,
                pspan: Span::new(0, 0),
            },
        ],
        spans: vec![Span::new(0, 0); 2],
        max_stack: 1,
    };
    let g_sig = FuncSig {
        params: vec![int],
        ret: int,
    };
    let callee = FuncDef {
        name: StrId(1),
        arity: 1,
        params: vec![zz_ir::Param {
            name: StrId(2),
            default: None,
        }],
        sig: g_sig,
        locals: vec![int],
        toplevel_slots: vec![],
        vartab: vec![],
        code: vec![Op::LoadSlot(0)],
        spans: vec![Span::new(0, 0); 1],
        max_stack: 1,
    };
    let module = Module {
        types: vec![
            zz_ir::IrType::Unknown,
            zz_ir::IrType::Int,
            zz_ir::IrType::Bool,
        ],
        strings: vec!["f".to_string(), "g".to_string(), "x".to_string()],
        consts: vec![zz_ir::Const::Bool(true)],
        funcs: vec![caller, callee],
        entry: zz_ir::FuncId(0),
    };
    let err = verify::verify(&module).expect_err("bad call accepted");
    assert!(
        err.message.contains("argument type mismatch"),
        "wrong error: {err}"
    );
}

#[test]
fn decode_rejects_v1() {
    // v1 bytes (same framing, version field 1) must be rejected with a
    // version error, never decoded.
    let parsed = zz_frontend::parse("1 + 2").program;
    let chunk = zz_runtime::vm::Compiler::compile_program(&parsed);
    let module = lower::lower(&chunk).expect("lower failed");
    let mut bytes = codec::encode(&module);
    assert!(bytes.len() > 8);
    // Version field sits right after magic[4].
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    let err = codec::decode(&bytes).expect_err("v1 accepted");
    assert!(err.message.contains("version"), "wrong error for v1: {err}");
}

#[test]
fn annot_section_is_removable() {
    // ANNOT carries hints only: a module with a non-empty ANNOT section
    // decodes, verifies, and raises identically to one without. Proves
    // the strip-annotations differential (spec §9) by construction.
    let parsed = zz_frontend::parse("x := 1 + 2\nx").program;
    let chunk = zz_runtime::vm::Compiler::compile_program(&parsed);
    let module = lower::lower(&chunk).expect("lower failed");
    let plain = codec::encode(&module);
    let mut doctored = plain.clone();
    let nsec = u32::from_le_bytes(doctored[8..12].try_into().unwrap()) as usize;
    let mut annot_off = None;
    let mut annot_len = None;
    for i in 0..nsec {
        let base = 12 + i * 12;
        let tag = u32::from_le_bytes(doctored[base..base + 4].try_into().unwrap());
        if tag == 7 {
            annot_off =
                Some(u32::from_le_bytes(doctored[base + 4..base + 8].try_into().unwrap()) as usize);
            annot_len = Some(
                u32::from_le_bytes(doctored[base + 8..base + 12].try_into().unwrap()) as usize,
            );
        }
    }
    let (off, len) = (
        annot_off.expect("annot section"),
        annot_len.expect("annot len"),
    );
    assert_eq!(len, 4, "v2 test assumes empty annot section");
    // Replace the 4-byte zero-count with count=1 + one 3-byte entry.
    // Payload grows by 7 bytes: ANNOT is last in practice; assert that
    // instead of handling the general splice.
    assert_eq!(off + len, doctored.len(), "annot must be last");
    doctored.truncate(off);
    doctored.extend_from_slice(&1u32.to_le_bytes());
    doctored.extend_from_slice(&3u32.to_le_bytes());
    doctored.extend_from_slice(b"abc");
    // Fix the section header length.
    let mut hdr_off = None;
    for i in 0..nsec {
        let base = 12 + i * 12;
        if u32::from_le_bytes(doctored[base..base + 4].try_into().unwrap()) == 7 {
            hdr_off = Some(base);
        }
    }
    let hb = hdr_off.unwrap();
    let new_len = (doctored.len() - off) as u32;
    doctored[hb + 8..hb + 12].copy_from_slice(&new_len.to_le_bytes());
    // Decodes and verifies exactly like the plain module.
    let loaded = codec::decode(&doctored).expect("annot decode failed");
    verify::verify(&loaded).expect("annot verify failed");
    let plain_loaded = codec::decode(&plain).expect("plain decode failed");
    assert_eq!(
        format!("{:?}", loaded),
        format!("{:?}", plain_loaded),
        "annot changed the module"
    );
}

#[test]
fn dis_shows_locals_and_format_version() {
    use std::collections::HashMap;
    let mut sigs = HashMap::new();
    sigs.insert("f".to_string(), int_sig(1));
    let module = lower_typed_spanmap(
        "func f(n: int) -> int {\n s := n + 1\n s = s + n\n s\n}\nf(1)",
        &sigs,
    );
    let text = zz_ir::dis::disassemble(&module);
    assert!(text.contains("; zzcz v3"), "missing v3 header:\n{text}");
    assert!(text.contains("locals=[int"), "missing locals:\n{text}");
}

#[test]
fn vartab_records_loop_var_slots() {
    use std::collections::HashMap;
    let mut sigs = HashMap::new();
    sigs.insert("m".to_string(), int_sig(0));
    let module = lower_typed_spanmap(
        "func m() {\n s := 0\n for v in 0..10 {\n s = s + v\n }\n s\n}\nm()",
        &sigs,
    );
    verify::verify(&module).expect("verify failed");
    let f = module
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, f)| module.strings.get(f.name.0 as usize).map(String::as_str) == Some("m"))
        .find(|(i, _)| zz_ir::FuncId(*i as u32) != module.entry)
        .map(|(_, f)| f)
        .expect("m lifted");
    // Exactly one for-loop with one var; the recorded slot must agree
    // with the slot the loop body reads (the body's `s = s + v` fuses
    // into a slot-referencing op, so ask the op rather than the
    // disassembly, which shows the fused form).
    assert_eq!(f.vartab.len(), 1, "one loop expected: {:?}", f.vartab);
    assert_eq!(f.vartab[0].len(), 1, "one var expected");
    let vslot = f.vartab[0][0];
    assert_ne!(vslot, u16::MAX, "loop var must be slot-bound");
    let referenced = f.code.iter().any(|op| {
        let mut hit = false;
        op.for_each_slot(|s| hit |= s == vslot);
        hit
    });
    assert!(referenced, "body never reads vartab slot {vslot}");
    // A vartab/ForNext count mismatch must fail closed.
    let mut bad = module.clone();
    let mf = bad
        .funcs
        .iter_mut()
        .find(|f| bad.strings.get(f.name.0 as usize).map(String::as_str) == Some("m"))
        .expect("m");
    mf.vartab.push(vec![0]);
    let err = verify::verify(&bad).expect_err("vartab mismatch accepted");
    assert!(err.message.contains("vartab"), "wrong error: {err}");
}

#[test]
fn nested_for_loops_verify() {
    // #311: any `for` nested inside any loop was rejected with
    // `join depth mismatch` — the depth model gave loop exits
    // setup_depth+1, but `ForNext` exhaustion truncates to the result
    // placeholder (setup_depth-1). Single loops never noticed
    // (single-predecessor exits); the outer back-edge exposed it.
    // Covers for-for, for-in-while, while-while, while-in-for and
    // triple-nested for loops (same signature, also failed).
    use std::collections::HashMap;
    let mut sigs = HashMap::new();
    sigs.insert("m".to_string(), int_sig(0));
    for src in [
        "func m() {\n s := 0\n for i in 0..3 {\n for j in 0..3 {\n s = s + i + j\n }\n }\n s\n}\nm()",
        "func m() {\n s := 0\n j := 0\n while j < 3 {\n for k in 0..3 {\n s = s + j + k\n }\n j = j + 1\n }\n s\n}\nm()",
        "func m() {\n s := 0\n i := 0\n while i < 3 {\n j := 0\n while j < 3 {\n s = s + i + j\n j = j + 1\n }\n i = i + 1\n }\n s\n}\nm()",
        "func m() {\n s := 0\n for i in 0..3 {\n j := 0\n while j < 3 {\n s = s + i + j\n j = j + 1\n }\n }\n s\n}\nm()",
        "func m() {\n s := 0\n for i in 0..3 {\n for j in 0..3 {\n for k in 0..3 {\n s = s + i + j + k\n }\n }\n }\n s\n}\nm()",
    ] {
        let module = lower_typed_spanmap(src, &sigs);
        // lower_typed runs max_stack_for + full verify internally, so a
        // clean return is the regression assertion; re-verify explicitly
        // to pin the module state too.
        verify::verify(&module).expect("nested loops must verify");
    }
}

#[test]
fn verify_accepts_error_declared_iterable() {
    // Slot reused across disjoint lifetimes widens to `Error`
    // (genuinely polymorphic): the verifier must wildcard-accept it at
    // iterable/bool/int positions instead of rejecting the program —
    // backends never unbox `Error` and dispatch guards fail closed.
    // Hand-built loop over an `Error`-declared slot (no allocator
    // dependence): must verify. Before the fix this failed with
    // `cannot iterate non-iterable` (the loop_mutate fixture hit it
    // via a dead int loop-var slot reused by an array binding).
    use zz_ir::{FuncDef, FuncSig, Module, Op, Span};
    let module = Module {
        types: vec![
            zz_ir::IrType::Unknown,
            zz_ir::IrType::Error,
            zz_ir::IrType::Unit,
        ],
        strings: vec!["m".to_string()],
        consts: vec![zz_ir::Const::Unit],
        funcs: vec![FuncDef {
            name: zz_ir::StrId(0),
            arity: 0,
            params: vec![],
            sig: FuncSig {
                params: vec![],
                ret: zz_ir::TypeId(0),
            },
            toplevel_slots: vec![],
            vartab: vec![vec![]],
            locals: vec![zz_ir::TypeId(1)],
            // [push result placeholder, load Error slot, setup,
            // header, empty body, back-edge, exit pop].
            code: vec![
                Op::PushConst(zz_ir::ConstId(0)),
                Op::LoadSlot(0),
                Op::ForSetup {
                    exit: 6,
                    header: 4,
                    num_vars: 0,
                },
                Op::Safepoint,
                Op::ForNext {
                    vars: vec![],
                    exit: 6,
                    in_env: false,
                },
                Op::Jump(4),
                Op::Pop,
            ],
            spans: vec![Span::new(0, 0); 7],
            max_stack: 3,
        }],
        entry: zz_ir::FuncId(0),
    };
    verify::verify(&module).expect("Error-declared iterable rejected");
}

#[test]
fn verify_short_locals_fails_closed() {
    // Regression: `func.sig.params` longer than `func.locals` (closure /
    // spawn shapes from lowering) panicked with index-OOB instead of
    // failing closed. Broke 29 e2e fixtures once `zz run` lowered on the
    // miss path for the run cache. Must return Err, never panic.
    use zz_ir::{FuncDef, FuncId, FuncSig, IrType, Module, Param, StrId, TypeId};
    let module = Module {
        types: vec![IrType::Unknown],
        strings: vec!["f".to_string()],
        consts: vec![],
        funcs: vec![FuncDef {
            name: StrId(0),
            arity: 1,
            params: vec![Param {
                name: StrId(0),
                default: None,
            }],
            sig: FuncSig {
                params: vec![TypeId(0)],
                ret: TypeId(0),
            },
            locals: vec![],
            vartab: vec![],
            toplevel_slots: vec![],
            code: vec![],
            spans: vec![],
            max_stack: 0,
        }],
        entry: FuncId(0),
    };
    let err = verify::verify(&module).expect_err("short locals must fail closed");
    assert!(
        err.message.contains("fewer slots"),
        "unexpected error: {}",
        err.message
    );
}

#[test]
fn round_trip_unread_parameters() {
    // Regression (#512): a parameter the body never reads emits no
    // `LoadSlot`/`StoreSlot`, so sizing the locals table from referenced
    // slots alone left it shorter than the signature arity and lowering
    // died in verification with "locals: fewer slots than signature
    // params". `b` is never read; `a` is.
    assert_eq!(
        round_trip_value("func f(a: int, b: int) -> int {\n a\n}\nf(7, 9)"),
        Value::Int(7)
    );
    // No parameter read at all.
    assert_eq!(
        round_trip_value("func g(n: int) -> int {\n 5\n}\ng(1)"),
        Value::Int(5)
    );
    // Same shape for a closure: its lifted chunk carries the lambda's
    // arity, and an unused tail parameter must not shrink the table.
    assert_eq!(
        round_trip_value("f := |a: int, b: int| a\nf(3, 4)"),
        Value::Int(3)
    );
}

/// Minimal one-function module for verifier-shape tests: `arity` params,
/// `locals_len` frame slots, and the given code.
fn shape_module(arity: usize, locals_len: usize, code: Vec<zz_ir::Op>) -> zz_ir::Module {
    use zz_ir::{FuncDef, FuncId, FuncSig, IrType, Module, Param, StrId, TypeId};
    let params: Vec<Param> = (0..arity)
        .map(|_| Param {
            name: StrId(0),
            default: None,
        })
        .collect();
    Module {
        types: vec![IrType::Unknown],
        strings: vec!["f".to_string()],
        consts: vec![],
        funcs: vec![FuncDef {
            name: StrId(0),
            arity: arity as u32,
            params,
            sig: FuncSig {
                params: vec![TypeId(0); arity],
                ret: TypeId(0),
            },
            locals: vec![TypeId(0); locals_len],
            vartab: vec![],
            toplevel_slots: vec![],
            code,
            spans: vec![],
            max_stack: 0,
        }],
        entry: FuncId(0),
    }
}

#[test]
fn verify_accepts_locals_floored_at_arity() {
    // #512: an unread parameter still owns a frame slot, so a table sized to
    // the parameter list with no slot references at all is canonical.
    let module = shape_module(2, 2, vec![]);
    verify::verify(&module).expect("param-floored locals must verify");
}

#[test]
fn verify_rejects_locals_longer_than_needed() {
    // The canonical size is `max(referenced slots, arity)`: slack beyond
    // that is still corruption, not a roomier table.
    let module = shape_module(2, 3, vec![]);
    let err = verify::verify(&module).expect_err("over-long locals must fail closed");
    assert!(
        err.message.contains("locals length mismatch"),
        "unexpected error: {}",
        err.message
    );
}
