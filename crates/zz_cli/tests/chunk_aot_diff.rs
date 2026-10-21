//! Chunk↔HIR AOT diff: the dual-codegen gate. Every program in the
//! slice corpus must produce byte-identical stdout and exit codes under
//! `zz build` (HIR→C) and `zz build --chunk` (Chunk→C).
//!
//! Corpus stays small and fast (dynamic `-O0` builds, sub-second runs)
//! so PR CI keeps it; the bench-timing gate (fib35/tak/sieve/arraysum
//! within 5%) is measured manually, not here.
//!
//! Sieve slice: `sieve_small` ([bool] + while + store-index fusion),
//! `nested_vec` (take-push fusion on two live slots), and `bool_iter`
//! ([bool] iteration through the boxed path) pin chunk↔HIR parity for
//! the shapes the sieve bench exercises at scale.
//!
//! `match` family (`MatchArm` / `MatchGuard` / `MatchError` /
//! `IfLetMatch`): covered since #508. Arm bindings live in function-local
//! `zz_value` cells (the VM uses a child env), pattern tests read through
//! borrowed places, and `|`, guards, nested payloads, user enums (with
//! `MakeEnum`/`RegisterEnum`) and `if let` all pin chunk↔HIR parity in
//! `match_option` / `match_result` / `match_guard` / `match_literals` /
//! `match_enum` / `match_iflet`.
//!
//! Deliberately uncovered, pending #508: `Op::CallMethod` for receivers the
//! compiler cannot type (struct methods, chained field receivers, and
//! methods on module-scope bindings). Variant methods that *are* typed
//! (`Option`/`Result` receivers; `unwrap` / `unwrap_or` / `expect`) no
//! longer reach the dynamic op — the compiler emits the resolved
//! `option.*` / `result.*` native, so `variant_methods` pins them on all
//! backends. Struct methods still need build-time resolution (or an
//! emitted method table) and stay out of the corpus. Variant
//! *construction* is covered since #508.
//!
//! Not in the corpus because the *HIR* backend miscompiles them today
//! (chunk output matches the VM): tuple patterns (`match (3, 4)`, HIR
//! prints 0) and `match` on `bool` with `true`/`false` arms (HIR emits
//! unbalanced C).

use std::path::{Path, PathBuf};
use std::process::Command;

fn zz_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let debug = deps.parent().unwrap();
    debug.join("zz")
}

fn run_zz_in(dir: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(zz_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("zz binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_bin(bin: &Path) -> (i32, String) {
    let out = Command::new(bin).output().expect("binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn require_native() -> bool {
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native backend unsupported (ZZ_SKIP_NATIVE=1)");
        return false;
    }
    true
}

const FIB: &str = r#"
func fib(n: int) -> int {
    if n <= 1 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}
func main() {
    println(fib(20))
}
"#;

const SUM_RANGE: &str = r#"
func main() {
    s := 0
    for i in 0..100000 {
        if i % 2 == 0 {
            s = s + i
        } else {
            s = s - 1
        }
    }
    println(s)
}
"#;

const ARRAY_IO: &str = r#"
func main() {
    a := []
    for i in 0..10000 {
        a = vec.push(a, i % 97)
    }
    a[0] = 42
    a[9999] = -1
    s := 0
    for v in a {
        s = s + v
    }
    println(a[0])
    println(a[9999])
    println(s)
}
"#;

const WHILE_NESTED: &str = r#"
func main() {
    s := 0
    for i in 0..100 {
        j := 0
        while j < 100 {
            s = s + 1
            j = j + 1
        }
    }
    println(s)
}
"#;

const STR_CONCAT: &str = r#"
func main() {
    s := ""
    for i in 0..2000 {
        s = s + "a"
    }
    println(len(s))
}
"#;

const TAK: &str = r#"
func tak(x: int, y: int, z: int) -> int {
    if y >= x {
        z
    } else {
        tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y))
    }
}
func main() {
    println(tak(12, 6, 0))
}
"#;

// Loop-iteration snapshot (mirrors
// tests/fixtures/regression/loop_iter_snapshot.zz): push / index-write /
// rebind inside the body must not affect visited elements or trip
// count (VM ground truth: 3/60/4/99, 3/60/200, 3/60/2/7/8).
const LOOP_MUTATE: &str = r#"
func main() {
    a := [10, 20, 30]
    n := 0
    s1 := 0
    for v in a {
        n = n + 1
        s1 = s1 + v
        if n == 1 {
            a = vec.push(a, 99)
        }
    }
    println(n)
    println(s1)
    println(len(a))
    println(a[3])
    b := [10, 20, 30]
    s := 0
    m := 0
    for v in b {
        m = m + 1
        s = s + v
        if m == 1 {
            b[1] = 200
        }
    }
    println(m)
    println(s)
    println(b[1])
    c := [10, 20, 30]
    t := 0
    k := 0
    for v in c {
        k = k + 1
        t = t + v
        if k == 1 {
            c = [7, 8]
        }
    }
    println(k)
    println(t)
    println(len(c))
    println(c[0])
    println(c[1])
    d := {"a": 10, "b": 20, "c": 30}
    n2 := 0
    s2 := 0
    for _k, v in d {
        n2 = n2 + 1
        s2 = s2 + v
        if n2 == 1 {
            d["b"] = 200
            d["z"] = 99
        }
    }
    println(n2)
    println(s2)
    println(d["b"])
    println(len(d))
}
"#;

// Sieve slice at test scale: [bool] bitset, index stores, a `while`
// inner loop, and int accumulation. Same shapes as bench/ir_gate/sieve.zz
// (n=200000 there); n=20000 here keeps both dynamic builds + runs fast.
const SIEVE_SMALL: &str = r#"
func main() {
    n := 20000
    is_prime := []
    for i in 0..n {
        is_prime = vec.push(is_prime, true)
    }
    is_prime[0] = false
    is_prime[1] = false
    count := 0
    for i in 2..n {
        if is_prime[i] {
            count = count + 1
            j := i * 2
            while j < n {
                is_prime[j] = false
                j = j + i
            }
        }
    }
    println(count)
}
"#;

// Nested vectors at test scale: take-push fusion on two live array
// slots (row + grid) plus value-semantics on inner capture.
const NESTED_VEC: &str = r#"
func main() {
    a := []
    for i in 0..50 {
        row := []
        for j in 0..50 {
            row = vec.push(row, (i + j) % 97)
        }
        a = vec.push(a, row)
    }
    s := 0
    for row in a {
        for v in row {
            s = s + v
        }
    }
    println(s)
    println(len(a))
}
"#;

// Bool-array iteration through the boxed path (branchy body: never a
// single fused int window, so no peel — parity must hold without it).
const BOOL_ITER: &str = r#"
func main() {
    a := []
    for i in 0..1000 {
        a = vec.push(a, i % 3 == 0)
    }
    n := 0
    for b in a {
        if b {
            n = n + 1
        }
    }
    println(n)
}
"#;

// String-append fusion pin: shared-string append must not leak into the
// other binding (fresh-alloc path), unicode/empty appends exact.
const STRAPP_ALIAS: &str = r#"
func main() {
    a := "hello"
    b := a
    a = a + "!"
    println(a)
    println(b)
    u := "héllo"
    u = u + "→✓"
    println(u)
    e := ""
    e = e + ""
    println(len(e))
}
"#;

// Index-read fusion pins: accumulate-through-read (prefix operand
// below the base), bare/branch reads, and trap parity (OOB + negative
// normalize) on the fused paths. Trap cases exit 1 on both backends
// (message text is not gated, exit codes are).
const IDX_ACCUM: &str = r#"
func main() {
    a := [10, 20, 30, 40]
    s := 0
    i := 0
    n := len(a)
    while i < n {
        s = s + a[i]
        i = i + 1
    }
    println(s)
    x := 0
    x = a[0] + 100
    println(x)
    y := 0
    y = a[3]
    println(y)
    z := 0
    k := 1
    z = a[k + 1] * 2
    println(z)
}
"#;

const IDX_BRANCH: &str = r#"
func main() {
    b := [true, false, true]
    if b[0] {
        println("t")
    }
    if b[1] {
        println("f")
    } else {
        println("not-f")
    }
    n := 0
    for v in b {
        if v {
            n = n + 1
        }
    }
    println(n)
}
"#;

const IDX_TRAP: &str = r#"
func main() {
    a := [1, 2, 3]
    x := 0
    x = a[5]
    println(x)
}
"#;

const IDX_NEG: &str = r#"
func main() {
    a := [10, 20, 30]
    x := 0
    x = a[-1]
    println(x)
}
"#;

// Nonzero-start ranges visit start..end (the chunk backend once seeded
// every range index with 0 — extra iterations with wrong values).
const RANGE_START: &str = r#"
func main() {
    s := 0
    n := 0
    for i in 2..10 {
        s = s + i
        n = n + 1
    }
    println(s)
    println(n)
    t := 0
    m := 0
    for i in 5..8 {
        t = t + i
        m = m + 1
    }
    println(t)
    println(m)
}
"#;

// Pure-ZZ stdlib helper coverage (#253): helpers implemented in ZZ
// (not natives) must resolve under `--chunk` via the referenced-only
// stdlib merge, matching HIR output exactly.
const STDLIB_STR: &str = r#"
import std.str

func main() {
    println(str.repeat("ab", 3))
    println(str.repeat("x", 0))
    println(str.pad_left("hi", 5, "-"))
}
"#;

// Bare range values print s..e through the shared first-class range
// (chunk boxes once rendered pointer bits, #254), with structural
// equality on both backends.
const RANGE_DISPLAY: &str = r#"
func main() {
    r := 0..5
    println(r)
    println(2..7)
    println((0..5) == (0..5))
    println((0..5) == (0..6))
}
"#;

// Variant construction (`MakeVariant`) and variant boxes crossing
// container boundaries: `.some` / `.none` / `.ok` / `.err` were rejected
// outright ("unsupported makevariant"), which kept the whole Option/Result
// stdlib surface off the chunk backend (#508). The array case pins the
// ownership rule too: variants are refcounted boxes now (`zz_vbox`), so
// `MakeArray`'s clone-then-release is a net move — with the pre-#507
// immortal box it double-freed (repro in the issue).
const VARIANT_CONSTRUCT: &str = r#"
import std.math
import std.vec

func mk(v: int) -> Option<int> {
    return .some(v)
}

func main() {
    println(vec.min_val([3, 1, 2]))
    println(vec.max_val([3, 1, 2]))
    println(math.min_arr([4, 2, 9]))
    a := [mk(1), mk(2), mk(3)]
    for v in a {
        println(v)
    }
    println(.some(7))
    println(.none)
    println(.ok(41))
    println(.err("boom"))
}
"#;

// Variant methods resolved at compile time (#508): a typed receiver turns
// `x.unwrap_or(d)` / `x.unwrap()` / `x.expect(m)` into the `option.` /
// `result.` native (same receiver namespace the VM's dynamic `CallMethod`
// dispatch would pick), so the call reaches the chunk backend, which has no
// run-time method table. Covers locals, a chained receiver, both variant
// families, and the error-arm `unwrap_or` fallback.
const VARIANT_METHODS: &str = r#"
func main() {
    a := .some(1)
    println(a.unwrap_or(0))
    b: Result<int, str> = .ok(2)
    println(b.unwrap_or(3))
    println(.some(7).unwrap_or(0))
    c := .some(5)
    println(c.unwrap())
    d: Result<int, str> = .err("bad")
    println(d.unwrap_or(-1))
    e := .some(4)
    println(e.expect("boom"))
    f: Result<int, str> = .ok(6)
    println(f.expect("boom"))
}
"#;

const CASES: &[(&str, &str)] = &[
    ("fib", FIB),
    ("sum_range", SUM_RANGE),
    ("array_io", ARRAY_IO),
    ("while_nested", WHILE_NESTED),
    ("str_concat", STR_CONCAT),
    ("tak", TAK),
    ("loop_mutate", LOOP_MUTATE),
    ("sieve_small", SIEVE_SMALL),
    ("nested_vec", NESTED_VEC),
    ("bool_iter", BOOL_ITER),
    ("strapp_alias", STRAPP_ALIAS),
    ("idx_accum", IDX_ACCUM),
    ("idx_branch", IDX_BRANCH),
    ("idx_trap", IDX_TRAP),
    ("idx_neg", IDX_NEG),
    ("range_start", RANGE_START),
    ("stdlib_str", STDLIB_STR),
    ("range_display", RANGE_DISPLAY),
    ("cond_cmp", COND_CMP),
    ("variant_construct", VARIANT_CONSTRUCT),
    ("variant_methods", VARIANT_METHODS),
    ("module_globals", MODULE_GLOBALS),
    ("module_globals_main", MODULE_GLOBALS_MAIN),
    ("match_option", MATCH_OPTION),
    ("match_result", MATCH_RESULT),
    ("match_guard", MATCH_GUARD),
    ("match_literals", MATCH_LITERALS),
    ("match_enum", MATCH_ENUM),
    ("match_iflet", MATCH_IFLET),
];

// Module-scope bindings read and written from functions: the VM keeps
// them in the program environment, so the chunk backend emits file-scope
// C globals (`module_globals`). Script shape (top-level statements, no
// user `main`) and `func main` shape both pinned.
const MODULE_GLOBALS: &str = r#"
greeting := "hi"
count := 0
func show() {
    println(greeting)
    println(count)
}
func bump() {
    count = count + 1
}
show()
bump()
bump()
println(count)
show()
"#;

const MODULE_GLOBALS_MAIN: &str = r#"
total := 0
limit := 3
func add(n: int) {
    total = total + n
}
func main() {
    for i in 0..limit {
        add(i)
    }
    println(total)
}
"#;

// Condition fusion pin: slot-vs-slot and slot-vs-const int compares
// feeding WhileCond stay identical across backends (chunk fuses them
// to raw compare+branch; see `condfuse_scan`).
const COND_CMP: &str = r#"
func main() {
    s := 0
    i := 0
    n := 1000
    while i < n {
        s = s + i
        i = i + 1
    }
    println(s)
    t := 0
    j := 1000
    while j > 10 {
        j = j - 1
        t = t + 1
    }
    println(t)
}
"#;

// `match` family: the VM keeps arm bindings in a child environment; the
// chunk backend gives each binding a function-local `zz_value` cell and
// tests patterns with borrowed reads (no clone on a miss).
const MATCH_OPTION: &str = r#"func pick(n: int) -> Option<int> {
    if n > 0 { .some(n) } else { .none }
}
func wrap(n: int) -> Option<Result<int, str>> {
    if n > 0 { .some(.ok(n)) } else { .some(.err("no")) }
}
func main() {
    a := pick(5)
    println(match a {
        .some(x) => x * 2,
        .none => 0 - 1,
    })
    println(match pick(0) {
        .some(x) => x * 2,
        .none => 0 - 1,
    })
    nested := wrap(3)
    println(match nested {
        .some(.ok(y)) => y + 100,
        .some(.err(e)) => 0 - 2,
        .none => 0 - 3,
    })
    println(match wrap(0) {
        .some(.ok(y)) => y + 100,
        .some(.err(e)) => 0 - 2,
        .none => 0 - 3,
    })
}
"#;

const MATCH_RESULT: &str = r#"func check(n: int) -> Result<int, str> {
    if n > 0 { .ok(n * 2) } else { .err("neg") }
}
func main() {
    a := check(3)
    println(match a {
        .ok(x) => x,
        .err(e) => 0 - 1,
    })
    b := check(-2)
    println(match b {
        .ok(x) => x,
        .err(e) => 0 - 2,
    })
}
"#;

const MATCH_GUARD: &str = r#"func grade(o: Option<int>) -> int {
    match o {
        .some(x) if x > 10 => 1,
        .some(x) => x * 10,
        .none => 0,
    }
}
func main() {
    println(grade(.some(50)))
    println(grade(.some(5)))
    println(grade(.none))
}
"#;

const MATCH_LITERALS: &str = r#"func classify(n: int) -> int {
    match n {
        0 => 100,
        1 | 2 => 200,
        _ => 300,
    }
}
func nested(n: int) -> int {
    match n {
        7 => match n {
            7 => 77,
            _ => 0,
        },
        _ => 0 - 1,
    }
}
func main() {
    println(classify(0))
    println(classify(1))
    println(classify(2))
    println(classify(9))
    println(nested(7))
    println(nested(8))
}
"#;

const MATCH_ENUM: &str = r#"enum Shape {
    Circle(int),
    Square(int),
    Empty,
}
func area(s: Shape) -> int {
    match s {
        .Circle(r) => r * r,
        .Square(w) => w * w,
        .Empty => 0,
    }
}
func main() {
    println(area(Shape.Circle(3)))
    println(area(Shape.Square(4)))
    println(area(Shape.Empty))
    e := Shape.Empty
    println(match e {
        .Circle(r) => r,
        .Square(w) => w,
        .Empty => 0 - 1,
    })
}
"#;

const MATCH_IFLET: &str = r#"func take(o: Option<int>) -> int {
    if let .some(x) = o {
        x
    } else {
        0 - 1
    }
}
func main() {
    println(take(.some(9)))
    println(take(.none))
    v := 0
    if let .some(x) = .some(4) {
        v = v + x
    }
    if let .some(y) = .none {
        v = v + 100
    }
    println(v)
}
"#;

#[test]
fn chunk_matches_hir_stdout_and_exit() {
    if !require_native() {
        return;
    }
    // One dir per backend so output names never collide.
    let root = std::env::temp_dir().join(format!("zz-chunk-diff-{}", std::process::id()));
    for (name, src) in CASES {
        for backend in ["hir", "chunk"] {
            let dir = root.join(format!("{name}_{backend}"));
            std::fs::create_dir_all(&dir).unwrap();
            let f = dir.join(format!("{name}.zz"));
            std::fs::write(&f, src).unwrap();
            let mut args = vec!["build", "--dynamic"];
            if backend == "chunk" {
                args.push("--chunk");
            }
            args.push(f.to_str().unwrap());
            let (code, _out, err) = run_zz_in(&dir, &args);
            assert_eq!(code, 0, "{backend} build of {name} failed: {err}");
            let bin = dir.join(name);
            assert!(bin.exists(), "{backend} binary missing for {name}");
            let (rcode, stdout) = run_bin(&bin);
            std::fs::write(dir.join("got.stdout"), &stdout).unwrap();
            std::fs::write(dir.join("got.exit"), rcode.to_string()).unwrap();
        }
        // Diff the two backends.
        let hir_out = std::fs::read_to_string(root.join(format!("{name}_hir/got.stdout"))).unwrap();
        let chunk_out =
            std::fs::read_to_string(root.join(format!("{name}_chunk/got.stdout"))).unwrap();
        let hir_exit = std::fs::read_to_string(root.join(format!("{name}_hir/got.exit"))).unwrap();
        let chunk_exit =
            std::fs::read_to_string(root.join(format!("{name}_chunk/got.exit"))).unwrap();
        assert_eq!(
            hir_exit, chunk_exit,
            "{name}: exit differs (hir={hir_exit} chunk={chunk_exit})"
        );
        assert_eq!(
            hir_out, chunk_out,
            "{name}: stdout differs\n--- hir ---\n{hir_out}\n--- chunk ---\n{chunk_out}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
