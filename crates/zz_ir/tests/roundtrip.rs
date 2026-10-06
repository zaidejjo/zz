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
