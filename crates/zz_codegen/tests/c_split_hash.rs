//! Scratch (NOT COMMITTED): hash lowered C for a fixed program corpus so the
//! crate split can be proven not to change generated code.
use std::collections::HashMap;

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn lower(name: &str, src: &str) -> String {
    let parsed = zz_frontend::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "{name} parse: {:?}",
        parsed.errors
    );
    let res = zz_hir::build_program(
        &parsed.program,
        HashMap::new(),
        zz_stdlib::stdlib_funcs(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let (pruned, reach) = zz_hir::dce(&res.program, "main");
    zz_codegen::lower_only(&pruned, &reach, "main").source
}

const PROGRAMS: &[(&str, &str)] = &[
    (
        "scalars",
        "func fib(n: int) -> int {\n    if n < 2 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nfunc main() {\n    total := 0\n    for i in 0..10 {\n        total = total + i\n    }\n    println(fib(10))\n    println(total)\n    println(3.5 * 2.0)\n    println(1 < 2)\n}\n",
    ),
    (
        "strings",
        "func main() {\n    s := \"hello\"\n    println(s + \" world\")\n    println(s.upper())\n    println(s.len())\n    println(s.find(\"ll\"))\n}\n",
    ),
    (
        "arrays",
        "func main() {\n    xs := [1, 2, 3]\n    xs.push(4)\n    for x in xs { println(x) }\n    ys := xs.map(|x| x * 2)\n    println(ys.len())\n}\n",
    ),
    (
        "dicts",
        "func main() {\n    d := {\"a\": 1, \"b\": 2}\n    d[\"c\"] = 3\n    println(d.len())\n    println(d.keys())\n}\n",
    ),
    (
        "structs",
        "struct Point {\n    x: int,\n    y: int,\n}\nfunc main() {\n    p := Point { x: 1, y: 2 }\n    println(p.x + p.y)\n}\n",
    ),
    (
        "enums",
        "enum Shape {\n    Circle(float),\n    Square(float),\n}\nfunc area(s: Shape) -> float {\n    match s {\n        .Circle(r) => {\n            return 3.14 * r * r\n        },\n        .Square(w) => {\n            return w * w\n        },\n    }\n    return 0.0\n}\nfunc main() {\n    println(area(Shape.Circle(2.0)))\n    println(area(Shape.Square(3.0)))\n}\n",
    ),
    (
        "closures",
        "func main() {\n    n := 10\n    f := |x| x + n\n    println(f(5))\n}\n",
    ),
    (
        "option",
        "func find(xs: [int], t: int) -> Option<int> {\n    for x in xs {\n        if x == t { return Some(x) }\n    }\n    return None\n}\nfunc main() {\n    println(find([1, 2, 3], 2).unwrap_or(0))\n    println(find([1, 2, 3], 9).unwrap_or(0))\n}\n",
    ),
    (
        "json",
        "func main() {\n    v := json.parse(\"{\\\"a\\\":1}\")\n    println(json.stringify(v))\n    println(json.get(v, \"a\"))\n}\n",
    ),
    (
        "testing",
        "func main() {\n    assert_eq(1 + 1, 2)\n    assert(true)\n    println(\"ok\")\n}\n",
    ),
];

#[test]
fn split_hash_corpus() {
    for (name, src) in PROGRAMS {
        let c = lower(name, src);
        // Lowering emits from hash sets in places, so line order varies per
        // process; compare the canonical (sorted) line multiset instead.
        let mut lines: Vec<&str> = c.lines().collect();
        lines.sort_unstable();
        let canon = lines.join("\n");
        println!("{name} {:016x} {}", fnv(canon.as_bytes()), lines.len());
    }
}
