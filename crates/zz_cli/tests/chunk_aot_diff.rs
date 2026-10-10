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
];

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
