#!/usr/bin/env python3
"""Random zz program generator, v2: type-tracked (int vs bool), valid syntax."""

import random
import sys


def gen_int(depth, ints, rng):
    opts = ["lit", "lit"]
    if ints:
        opts += ["var", "var", "var"]
    if depth > 0:
        opts += ["binop"] * 4 + ["neg"]
    k = rng.choice(opts)
    if k == "lit":
        return str(rng.randint(-20, 100))
    if k == "var":
        return rng.choice(ints)
    if k == "neg":
        return f"-({gen_int(depth - 1, ints, rng)})"
    op = rng.choice(["+", "-", "*", "/", "%"])
    style = rng.random()
    if style < 0.35:
        l, r = str(rng.randint(0, 30)), gen_int(depth - 1, ints, rng)
    elif style < 0.7:
        l, r = gen_int(depth - 1, ints, rng), str(rng.randint(1, 30))
    else:
        l, r = gen_int(depth - 1, ints, rng), gen_int(depth - 1, ints, rng)
    if op in ("/", "%"):
        # divisor in 2..58, always positive (truncation-safe)
        return f"({l} {op} ((({r}) % 29) + 30))"
    return f"({l} {op} {r})"


def gen_cond(ints, rng):
    op = rng.choice(["<", ">", "==", "!="])
    return f"({gen_int(1, ints, rng)} {op} {gen_int(1, ints, rng)})"


def gen_program(seed):
    rng = random.Random(seed)
    lines = []
    n = [0]

    def fresh(prefix="v"):
        n[0] += 1
        return f"{prefix}{n[0]}"

    ints, bools = [], []
    if rng.random() < 0.6:
        a, b = fresh("a"), fresh("b")
        e1, e2 = gen_int(2, ints, rng), gen_int(2, ints, rng)
        if rng.random() < 0.3:
            lines.append(f"(_, {b}) := ({e1}, {e2})")
            ints.append(b)
        else:
            lines.append(f"({a}, {b}) := ({e1}, {e2})")
            ints += [a, b]
    for _ in range(rng.randint(1, 3)):
        if rng.random() < 0.7 or not ints:
            v = fresh()
            lines.append(f"{v} := {gen_int(2, ints, rng)}")
            ints.append(v)
        else:
            v = fresh("t")
            lines.append(f"{v} := {gen_cond(ints, rng)}")
            bools.append(v)
    fname = fresh("f")
    param = fresh("p")
    want_bool = rng.random() < 0.3
    ret = "bool" if want_bool else "int"
    lines.append(f"func {fname}({param}: int) -> {ret} {{")
    pints = ints + [param]
    if rng.random() < 0.5:
        lines.append(f"    if {param} < 0 {{")
        lines.append(
            f"        return {gen_int(1, [], rng) if not want_bool else 'false'}"
        )
        lines.append("    }")
    if rng.random() < 0.4:
        m = fresh("m")
        lines.append(f"    {m} := {param} % 3")
        lines.append(f"    match {m} {{")
        lines.append(
            f"        0 => {{ return {gen_int(1, [], rng) if not want_bool else 'true'} }},"
        )
        lines.append(f"        _ => {{ {fresh('q')} := {gen_int(0, pints, rng)} }},")
        lines.append("    }")
    if want_bool:
        lines.append(f"    ({gen_int(2, pints, rng)} > 0)")
    else:
        lines.append(f"    {gen_int(2, pints, rng)}")
    lines.append("}")
    if ints and rng.random() < 0.5:
        v = rng.choice(ints)
        lines.append("if true {")
        lines.append(f"    ({v}, {fresh('s')}) := ({gen_int(1, ints, rng)}, 1)")
        lines.append(f"    println({v})")
        lines.append("}")
    if ints and rng.random() < 0.5:
        v = rng.choice(ints)
        c, d = fresh("cl"), fresh("d")
        lines.append(f"{c} := |{d}: int| {v} + {d}")
        lines.append(f"println({c}(1))")
    acc = fresh("acc")
    lines.append(f"{acc} := 0")
    lo, hi = rng.randint(0, 5), rng.randint(6, 12)
    lines.append(f"for {fresh('i')} in {lo}..{hi} {{")
    if rng.random() < 0.3 and ints:
        v = rng.choice(ints)
        lines.append(f"    {acc} = {rng.randint(0, 5)} - ({v} + {acc})")
    else:
        lines.append(f"    {acc} = {acc} + {gen_int(1, ints, rng)}")
    lines.append("}")
    lines.append(f"println({acc})")
    arg = rng.randint(-5, 20)
    lines.append(f"println({fname}({arg}))")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


# --- v2 shapes: return-of-call in branches, string-building loops,
# --- stringify-like struct emits (native loop-arena stress). ---


def gen_str(depth, strs, ints, rng):
    opts = ["lit", "lit"]
    if strs:
        opts += ["var", "var"]
    if ints and depth > 0:
        opts += ["strint"]
    if depth > 0:
        opts += ["cat", "cat"]
    k = rng.choice(opts)
    if k == "lit":
        return '"' + rng.choice(["k", "ab", "x", "s", "q", "zz"]) + '"'
    if k == "var":
        return rng.choice(strs)
    if k == "strint":
        return f"str({gen_int(depth - 1, ints, rng)})"
    return f"({gen_str(depth - 1, strs, ints, rng)} + {gen_str(depth - 1, strs, ints, rng)})"


def gen_calls_program(seed):
    """S1: return-of-call-result in both branches, early returns + calls."""
    rng = random.Random(1000003 * seed + 11)
    lines = []
    n = [0]

    def fresh(prefix="v"):
        n[0] += 1
        return f"{prefix}{n[0]}"

    g, h = fresh("g"), fresh("h")
    lines.append(f"func {g}(x: int) -> int {{")
    lines.append(f"    x + {rng.randint(1, 5)}")
    lines.append("}")
    lines.append(f"func {h}(x: int) -> int {{")
    lines.append(f"    x + {rng.randint(6, 9)}")
    lines.append("}")
    f = fresh("f")
    p = fresh("p")
    style = rng.random()
    lines.append(f"func {f}({p}: int) -> int {{")
    if style < 0.4:
        c = fresh("c")
        lines.append(f"    {c} := {p} > {rng.randint(0, 10)}")
        lines.append(f"    if {c} {{")
        lines.append(f"        return {g}({p})")
        lines.append("    } else {")
        lines.append(f"        return {h}({p})")
        lines.append("    }")
    elif style < 0.7:
        lines.append(f"    if {p} < {rng.randint(0, 5)} {{")
        lines.append(f"        return {g}({p})")
        lines.append("    }")
        if rng.random() < 0.5:
            lines.append(f"    if {p} > {rng.randint(10, 20)} {{")
            lines.append(f"        return {g}({p})")
            lines.append("    }")
        lines.append(f"    return {h}({p})")
    else:
        m = fresh("m")
        lines.append(f"    {m} := {p} % 3")
        lines.append(f"    match {m} {{")
        lines.append(f"        0 => {{ return {g}({p}) }},")
        lines.append(f"        1 => {{ return {h}({p}) }},")
        lines.append(f"        _ => {{ return {g}({p}) }},")
        lines.append("    }")
    lines.append("}")
    for a in (rng.randint(-5, 5), rng.randint(6, 20)):
        lines.append(f"println({f}({a}))")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_strloop_program(seed):
    """S2: string accumulators with calls, copies, and retaining stores."""
    rng = random.Random(1000033 * seed + 101)
    lines = ["import std.vec"]
    n = [0]

    def fresh(prefix="v"):
        n[0] += 1
        return f"{prefix}{n[0]}"

    ints = []
    for _ in range(rng.randint(1, 2)):
        v = fresh()
        lines.append(f"{v} := {rng.randint(0, 50)}")
        ints.append(v)
    strs = []
    for _ in range(rng.randint(1, 2)):
        v = fresh("s")
        lines.append(f'{v} := "{rng.choice(["a", "k", "x"])}"')
        strs.append(v)
    # helper: wrap a string with affixes (call result in accumulator)
    w = fresh("w")
    pa, pb = fresh("pa"), fresh("pb")
    lines.append(f"func {w}({pa}: str, {pb}: int) -> str {{")
    lines.append(f'    {pa} + "=" + str({pb}) + ";"')
    lines.append("}")
    out = fresh("out")
    lines.append(f"{out} := {gen_str(1, strs, ints, rng)}")
    lo, hi = rng.randint(0, 3), rng.randint(4, 8)
    it = fresh("i")
    lines.append(f"for {it} in {lo}..{hi} {{")
    piece = f"{w}({rng.choice(strs)}, {it})"
    if rng.random() < 0.5:
        piece = f"({piece} + {gen_str(1, strs, ints, rng)})"
    lines.append(f"    {out} = {out} + {piece}")
    if rng.random() < 0.4:
        # copy across iterations + immediate use (alias must stay valid)
        cp = fresh("cp")
        lines.append(f"    {cp} := {out}")
        lines.append(f"    {out} = {cp} + {gen_str(0, strs, ints, rng)}")
    lines.append("}")
    lines.append(f"println({out})")
    shape = rng.random()
    if shape < 0.3:
        # retaining stores of loop-built strings
        arr = fresh("arr")
        lines.append(f"{arr} := [{out}, {gen_str(1, strs, ints, rng)}]")
        lines.append(f"{arr}[0] = {out} + {gen_str(0, strs, ints, rng)}")
        lines.append(f"println({arr}[0])")
        lines.append(f"println({arr}[1])")
    elif shape < 0.6:
        d = fresh("d")
        lines.append(f'{d} := {{"k": {out}}}')
        lines.append(f'println({d}["k"])')
        lines.append(f'{d}["j"] = {out} + "!"')
        lines.append(f'println({d}["j"])')
    else:
        lst = fresh("lst")
        lines.append(f"{lst}: [str] = []")
        lines.append(f"{lst} = vec.push({lst}, {out})")
        lines.append(f"println({lst}[0])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_stringify_program(seed):
    """S3: stringify-like struct emit (by-value arena + key loop)."""
    rng = random.Random(1000037 * seed + 1001)
    lines = ["import std.vec"]
    n = [0]

    def fresh(prefix="v"):
        n[0] += 1
        return f"{prefix}{n[0]}"

    nk = rng.randint(1, 3)
    keys = [
        rng.choice(["k", "j", "a", "b"]) + str(rng.randint(0, 9)) for _ in range(nk)
    ]
    vals = [rng.randint(0, 99) for _ in range(nk)]
    tname = rng.choice(["table", "sect", "grp"])
    kinds = ["table", "table"] + ["int"] * nk
    ivals = [0, 0] + vals
    tbl = [0] + [1] * nk
    tkeys = [tname] + keys
    kids = [1] + list(range(2, 2 + nk))
    lines.append("struct Doc {")
    lines.append("    kinds: [str],")
    lines.append("    ivals: [int],")
    lines.append("    t_tbl: [int],")
    lines.append("    t_keys: [str],")
    lines.append("    t_kids: [int],")
    lines.append("}")
    kk = fresh("kk")
    lines.append(f"func {kk}(doc: Doc, id: int) -> [str] {{")
    lines.append("    out: [str] = []")
    lines.append("    for i in 0..len(doc.t_tbl) {")
    lines.append("        if doc.t_tbl[i] == id {")
    lines.append("            out = vec.push(out, doc.t_keys[i])")
    lines.append("        }")
    lines.append("    }")
    lines.append("    out")
    lines.append("}")
    kid = fresh("kid")
    lines.append(f"func {kid}(doc: Doc, id: int, key: str) -> int {{")
    lines.append("    for i in 0..len(doc.t_tbl) {")
    lines.append("        if doc.t_tbl[i] == id && doc.t_keys[i] == key {")
    lines.append("            return doc.t_kids[i]")
    lines.append("        }")
    lines.append("    }")
    lines.append("    0 - 1")
    lines.append("}")
    em = fresh("em")
    lines.append(f"func {em}(doc: Doc, id: int) -> str {{")
    lines.append('    out := ""')
    lines.append(f"    keys := {kk}(doc, id)")
    lines.append("    for i in 0..len(keys) {")
    lines.append("        key := keys[i]")
    lines.append(f"        kd := {kid}(doc, id, key)")
    lines.append('        out = out + key + "=" + str(doc.ivals[kd]) + ";"')
    lines.append("    }")
    lines.append("    out")
    lines.append("}")
    doc = fresh("doc")
    q = lambda xs: "[" + ", ".join(f'"{x}"' for x in xs) + "]"
    lines.append(
        f"{doc} := Doc{{kinds: {q(kinds)}, ivals: {ivals}, "
        f"t_tbl: {tbl}, t_keys: {q(tkeys)}, t_kids: {kids}}}"
    )
    lines.append(f"println({em}({doc}, 1))")
    lines.append(f"println({kk}({doc}, 0)[0])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"




# --- v3 shapes: move-append takes, aliasing, early exits. ---
# Every case prints only deterministic ints plus a DONE marker.
# Covered: free/method/append pushes, field pushes, thread calls,
# live aliases, self-push, double-read elems, dict slots, closures
# over the pushed array, and `?` early exit inside a push.


def gen_v3_alias(seed):
    """Copied array must not observe the later push (b keeps old len)."""
    rng = random.Random(1000091 * seed + 21)
    a = [rng.randint(0, 20) for _ in range(rng.randint(1, 3))]
    k = rng.randint(0, 50)
    lines = ["import std.vec"]
    lines.append(f"a := [{', '.join(map(str, a))}]")
    lines.append("b := a")
    lines.append(f"a = vec.push(a, {k})")
    lines.append("println(len(a))")
    lines.append("println(len(b))")
    lines.append("println(b[0])")
    lines.append(f"println(a[{len(a)}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_selfpush(seed):
    """Push the array (nested) or an element read of it back in."""
    rng = random.Random(1000093 * seed + 31)
    if rng.random() < 0.5:
        x0 = rng.randint(0, 9)
        x1 = rng.randint(0, 9)
        lines = ["import std.vec"]
        lines.append(f"x := [[{x0}], [{x1}]]")
        lines.append("x = vec.push(x, x[0])")
        lines.append("println(len(x))")
        lines.append("println(len(x[2]))")
        lines.append("println(x[2][0])")
    else:
        y0, y1 = rng.randint(0, 9), rng.randint(0, 9)
        lines = ["import std.vec"]
        lines.append(f"y := [{y0}, {y1}]")
        lines.append("y = vec.push(y, y[0])")
        lines.append("println(len(y))")
        lines.append(f"println(y[2])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_methodloop(seed):
    """`x = x.push(e)` in a loop (fused method path)."""
    rng = random.Random(1000097 * seed + 41)
    n = rng.randint(2, 7)
    m = rng.randint(1, 5)
    lines = ["import std.vec"]
    lines.append("x := []")
    lines.append(f"for i in 0..{n} {{")
    lines.append(f"    x = x.push(i * {m})")
    lines.append("}")
    lines.append("println(len(x))")
    lines.append(f"println(x[{n - 1}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_appendloop(seed):
    """`x = vec.append(x, e)` in a loop (append value shape)."""
    rng = random.Random(1000103 * seed + 51)
    n = rng.randint(2, 7)
    k = rng.randint(0, 9)
    lines = ["import std.vec"]
    lines.append(f"x := [{k}]")
    lines.append(f"for i in 0..{n} {{")
    lines.append("    x = vec.append(x, i)")
    lines.append("}")
    lines.append("println(len(x))")
    lines.append(f"println(x[{n}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_fieldloop(seed):
    """Two struct fields pushed per iteration (field-take path)."""
    rng = random.Random(1000121 * seed + 61)
    n = rng.randint(2, 6)
    k = rng.randint(0, 9)
    lines = ["import std.vec"]
    lines.append("struct Fq { q: [int], w: [int] }")
    lines.append(f"s := Fq{{q: [], w: [{k}]}}")
    lines.append(f"for i in 0..{n} {{")
    lines.append("    s.q = vec.push(s.q, i)")
    lines.append("    s.w = vec.push(s.w, i)")
    lines.append("}")
    lines.append("println(len(s.q))")
    lines.append("println(len(s.w))")
    lines.append(f"println(s.q[{n - 1}])")
    lines.append(f"println(s.w[{n}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_thread(seed):
    """`x = f(x, k)` threading an array through a call."""
    rng = random.Random(1000127 * seed + 71)
    n = rng.randint(2, 6)
    c = rng.randint(1, 9)
    lines = ["import std.vec"]
    lines.append(f"func ad(d: [int], k: int) -> [int] {{")
    lines.append(f"    vec.push(d, k + {c})")
    lines.append("}")
    lines.append("x := []")
    lines.append(f"for i in 0..{n} {{")
    lines.append("    x = ad(x, i)")
    lines.append("}")
    lines.append("println(len(x))")
    lines.append(f"println(x[{n - 1}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_closurecap(seed):
    """Closure over the array, called after a push (sees stored value)."""
    rng = random.Random(1000133 * seed + 81)
    a = [rng.randint(0, 20) for _ in range(rng.randint(1, 3))]
    k = rng.randint(0, 50)
    lines = ["import std.vec"]
    lines.append(f"a := [{', '.join(map(str, a))}]")
    lines.append("c := |d: int| len(a) + d")
    lines.append(f"a = vec.push(a, {k})")
    lines.append("println(c(0))")
    lines.append("println(len(a))")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_dictpush(seed):
    """Array behind a dict slot pushed back through the slot."""
    rng = random.Random(1000141 * seed + 91)
    a, b = rng.randint(0, 20), rng.randint(0, 20)
    k = rng.randint(0, 50)
    lines = ["import std.vec"]
    lines.append(f'd := {{"k": [{a}, {b}]}}')
    lines.append(f'd["k"] = vec.push(d["k"], {k})')
    lines.append('println(len(d["k"]))')
    lines.append(f'println(d["k"][2])')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_earlyexit(seed):
    """`?` inside a push: error propagates, success path pushes."""
    rng = random.Random(1000157 * seed + 101)
    a, b = rng.randint(0, 20), rng.randint(0, 20)
    lines = ["import std.vec"]
    lines.append("func pk(v: int) -> Result<int, str> {")
    lines.append("    if v == 0 {")
    lines.append('        return .err("bad")')
    lines.append("    }")
    lines.append("    return .ok(v * 2)")
    lines.append("}")
    lines.append("func bd(v: int) -> Result<[int], str> {")
    lines.append(f"    o := [{a}, {b}]")
    lines.append("    o = vec.push(o, pk(v)?)")
    lines.append("    .ok(o)")
    lines.append("}")
    lines.append("r := bd(1)")
    lines.append("match r {")
    lines.append('    .ok(v) => println(len(v)),')
    lines.append('    .err(e) => println(-1),')
    lines.append("}")
    lines.append("r2 := bd(0)")
    lines.append("match r2 {")
    lines.append('    .ok(v) => println(len(v)),')
    lines.append('    .err(e) => println(-1),')
    lines.append("}")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_program_v3(seed):
    rng = random.Random(999979 * seed + 17)
    k = rng.random()
    if k < 0.12:
        return gen_v3_alias(seed)
    if k < 0.24:
        return gen_v3_selfpush(seed)
    if k < 0.36:
        return gen_v3_methodloop(seed)
    if k < 0.48:
        return gen_v3_appendloop(seed)
    if k < 0.60:
        return gen_v3_fieldloop(seed)
    if k < 0.72:
        return gen_v3_thread(seed)
    if k < 0.84:
        return gen_v3_closurecap(seed)
    if k < 0.93:
        return gen_v3_dictpush(seed)
    return gen_v3_earlyexit(seed)



def gen_program_v2(seed):
    rng = random.Random(999983 * seed + 7)
    k = rng.random()
    if k < 0.35:
        return gen_calls_program(seed)
    if k < 0.7:
        return gen_strloop_program(seed)
    return gen_stringify_program(seed)


if __name__ == "__main__":
    import os

    # v2 shapes (branch call-returns, string accumulators, stringify-like
    # struct emits) live behind `--shapes v2` so the default mode stays
    # byte-identical for every seed (fixed-seed CI smoke fixtures).
    if len(sys.argv) > 1 and sys.argv[1] == "--shapes":
        assert sys.argv[2] in ("v2", "v3"), "only --shapes v2/v3 are supported"
        start, count, outdir = int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
        os.makedirs(outdir, exist_ok=True)
        if sys.argv[2] == "v3":
            for i in range(start, start + count):
                with open(f"{outdir}/ga{i:05d}.zz", "w") as f:
                    f.write(gen_program_v3(i))
            print(f"wrote {count} v3 programs")
        else:
            for i in range(start, start + count):
                with open(f"{outdir}/gz{i:05d}.zz", "w") as f:
                    f.write(gen_program_v2(i))
            print(f"wrote {count} v2 programs")
    else:
        start, count, outdir = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
        os.makedirs(outdir, exist_ok=True)
        for i in range(start, start + count):
            with open(f"{outdir}/fz{i:05d}.zz", "w") as f:
                f.write(gen_program(i))
        print(f"wrote {count} programs")
