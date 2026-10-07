#!/usr/bin/env python3
"""Random zz program generator, v4: type-tracked (int vs bool), valid syntax.

v4 shapes (floats, precedence, agreeing closures, nested stores,
strings/interp, compound stores, casts, plain structs) print LABELED
lines so the harness oracle compares real values (bare numbers are
stripped by the normalizer). Known-open divergences are excluded by
design — see the v4 section header.
"""

import random
import sys

# Batch prefix: when --batch emits many cases into one file, CPREFIX is
# set per case (`c0_`, `c1_`, ...) so top-level bindings, funcs, and
# struct names cannot collide. fresh() and N() read it at call time;
# single-case generation leaves it empty (byte-identical output).
CPREFIX = ""


def N(name):
    """Prefix a top-level binding/func/struct name for batch mode."""
    return f"{CPREFIX}{name}"


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
        return f"{CPREFIX}{prefix}{n[0]}"

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
        return f"{CPREFIX}{prefix}{n[0]}"

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
        return f"{CPREFIX}{prefix}{n[0]}"

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
        return f"{CPREFIX}{prefix}{n[0]}"

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
    lines.append(f"{N('a')} := [{', '.join(map(str, a))}]")
    lines.append(f"{N('b')} := {N('a')}")
    lines.append(f"{N('a')} = vec.push({N('a')}, {k})")
    lines.append(f"println(len({N('a')}))")
    lines.append(f"println(len({N('b')}))")
    lines.append(f"println({N('b')}[0])")
    lines.append(f"println({N('a')}[{len(a)}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_selfpush(seed):
    """Push the array (nested) or an element read of it back in."""
    rng = random.Random(1000093 * seed + 31)
    if rng.random() < 0.5:
        x0 = rng.randint(0, 9)
        x1 = rng.randint(0, 9)
        lines = ["import std.vec"]
        lines.append(f"{N('x')} := [[{x0}], [{x1}]]")
        lines.append(f"{N('x')} = vec.push({N('x')}, {N('x')}[0])")
        lines.append(f"println(len({N('x')}))")
        lines.append(f"println(len({N('x')}[2]))")
        lines.append(f"println({N('x')}[2][0])")
    else:
        y0, y1 = rng.randint(0, 9), rng.randint(0, 9)
        lines = ["import std.vec"]
        lines.append(f"{N('y')} := [{y0}, {y1}]")
        lines.append(f"{N('y')} = vec.push({N('y')}, {N('y')}[0])")
        lines.append(f"println(len({N('y')}))")
        lines.append(f"println({N('y')}[2])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_methodloop(seed):
    """`x = x.push(e)` in a loop (fused method path)."""
    rng = random.Random(1000097 * seed + 41)
    n = rng.randint(2, 7)
    m = rng.randint(1, 5)
    lines = ["import std.vec"]
    lines.append(f"{N('x')} := []")
    lines.append(f"for {N('i')} in 0..{n} {{")
    lines.append(f"    {N('x')} = {N('x')}.push({N('i')} * {m})")
    lines.append("}")
    lines.append(f"println(len({N('x')}))")
    lines.append(f"println({N('x')}[{n - 1}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_appendloop(seed):
    """`x = vec.append(x, e)` in a loop (append value shape)."""
    rng = random.Random(1000103 * seed + 51)
    n = rng.randint(2, 7)
    k = rng.randint(0, 9)
    lines = ["import std.vec"]
    lines.append(f"{N('x')} := [{k}]")
    lines.append(f"for {N('i')} in 0..{n} {{")
    lines.append(f"    {N('x')} = vec.append({N('x')}, {N('i')})")
    lines.append("}")
    lines.append(f"println(len({N('x')}))")
    lines.append(f"println({N('x')}[{n}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_fieldloop(seed):
    """Two struct fields pushed per iteration (field-take path)."""
    rng = random.Random(1000121 * seed + 61)
    n = rng.randint(2, 6)
    k = rng.randint(0, 9)
    lines = ["import std.vec"]
    lines.append(f"struct {N('Fq')} {{ q: [int], w: [int] }}")
    lines.append(f"{N('s')} := {N('Fq')}{{q: [], w: [{k}]}}")
    lines.append(f"for {N('i')} in 0..{n} {{")
    lines.append(f"    {N('s')}.q = vec.push({N('s')}.q, {N('i')})")
    lines.append(f"    {N('s')}.w = vec.push({N('s')}.w, {N('i')})")
    lines.append("}")
    lines.append(f"println(len({N('s')}.q))")
    lines.append(f"println(len({N('s')}.w))")
    lines.append(f"println({N('s')}.q[{n - 1}])")
    lines.append(f"println({N('s')}.w[{n}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_thread(seed):
    """`x = f(x, k)` threading an array through a call."""
    rng = random.Random(1000127 * seed + 71)
    n = rng.randint(2, 6)
    c = rng.randint(1, 9)
    lines = ["import std.vec"]
    lines.append(f"func {N('ad')}(d: [int], k: int) -> [int] {{")
    lines.append(f"    vec.push(d, k + {c})")
    lines.append("}")
    lines.append(f"{N('x')} := []")
    lines.append(f"for {N('i')} in 0..{n} {{")
    lines.append(f"    {N('x')} = {N('ad')}({N('x')}, {N('i')})")
    lines.append("}")
    lines.append(f"println(len({N('x')}))")
    lines.append(f"println({N('x')}[{n - 1}])")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_closurecap(seed):
    """Closure over the array, called after a push (sees stored value)."""
    rng = random.Random(1000133 * seed + 81)
    a = [rng.randint(0, 20) for _ in range(rng.randint(1, 3))]
    k = rng.randint(0, 50)
    lines = ["import std.vec"]
    lines.append(f"{N('a')} := [{', '.join(map(str, a))}]")
    lines.append(f"{N('c')} := |d: int| len({N('a')}) + d")
    lines.append(f"{N('a')} = vec.push({N('a')}, {k})")
    lines.append(f"println({N('c')}(0))")
    lines.append(f"println(len({N('a')}))")
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_dictpush(seed):
    """Array behind a dict slot pushed back through the slot."""
    rng = random.Random(1000141 * seed + 91)
    a, b = rng.randint(0, 20), rng.randint(0, 20)
    k = rng.randint(0, 50)
    lines = ["import std.vec"]
    lines.append(f'{N("d")} := {{"k": [{a}, {b}]}}')
    lines.append(f'{N("d")}["k"] = vec.push({N("d")}["k"], {k})')
    lines.append(f'println(len({N("d")}["k"]))')
    lines.append(f'println({N("d")}["k"][2])')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v3_earlyexit(seed):
    """`?` inside a push: error propagates, success path pushes."""
    rng = random.Random(1000157 * seed + 101)
    a, b = rng.randint(0, 20), rng.randint(0, 20)
    lines = ["import std.vec"]
    lines.append(f"func {N('pk')}(v: int) -> Result<int, str> {{")
    lines.append("    if v == 0 {")
    lines.append('        return .err("bad")')
    lines.append("    }")
    lines.append("    return .ok(v * 2)")
    lines.append("}")
    lines.append(f"func {N('bd')}(v: int) -> Result<[int], str> {{")
    lines.append(f"    o := [{a}, {b}]")
    lines.append(f"    o = vec.push(o, {N('pk')}(v)?)")
    lines.append("    .ok(o)")
    lines.append("}")
    lines.append(f"{N('r')} := {N('bd')}(1)")
    lines.append("match r {")
    lines.append("    .ok(v) => println(len(v)),")
    lines.append("    .err(e) => println(-1),")
    lines.append("}")
    lines.append(f"{N('r2')} := {N('bd')}(0)")
    lines.append("match r2 {")
    lines.append("    .ok(v) => println(len(v)),")
    lines.append("    .err(e) => println(-1),")
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


# --- v4 shapes: floats, precedence, agreeing closures, nested stores,
# --- strings/interp, compound stores, casts, plain structs. ---
#
# Every case prints LABELED lines (`name=value`, never bare numbers):
# the harness strips purely-numeric lines, so bare prints would leave
# the oracle comparing only the DONE marker. Labels make values real.
#
# Deliberately EXCLUDED (known-open divergences, pinned by fixtures
# instead of fuzz — generating them would flood VMFAIL/DIFF by design):
# - `task.spawn` (native shares cells, VM snapshots; §1.8)
# - closures capturing loop/block locals (both engines deviate)
# - plain index stores with side-effecting index/value (store-order
#   split: VM value-first, native source-order)
# - struct methods / whole-struct display (deferred AOT-lowering bugs)
# - `rand_*` (nondeterministic), huge allocations (CI-hostile),
#   negative int exponents (both trap by design).


def gen_v4_float(seed):
    """Float arithmetic, casts, math.pow, NaN/INF display (spec §4)."""
    rng = random.Random(1000203 * seed + 201)
    lines = ["import std.math"]
    n = [0]

    def fresh(prefix="fl"):
        n[0] += 1
        return f"{CPREFIX}{prefix}{n[0]}"

    def flit():
        style = rng.random()
        if style < 0.25:
            return rng.choice(["0.1", "2.5", "3.14", "-0.0", "100.0"])
        return f"{rng.randint(0, 99)}.{rng.randint(0, 9)}"

    fvars = []
    for _ in range(rng.randint(1, 3)):
        v = fresh()
        lines.append(f"{v} := {flit()}")
        fvars.append(v)
    for _ in range(rng.randint(2, 4)):
        v = fresh()
        op = rng.choice(["+", "-", "*"])
        a = rng.choice(fvars) if fvars and rng.random() < 0.7 else flit()
        b = rng.choice(fvars) if fvars and rng.random() < 0.7 else flit()
        lines.append(f"{v} := {a} {op} {b}")
        fvars.append(v)
        lines.append(f'println("{v}={{{v}}}")')
    # Guarded division: nonzero literal divisor, never traps.
    v = fresh()
    a = rng.choice(fvars) if fvars else flit()
    lines.append(f"{v} := {a} / {rng.randint(1, 30)}.0")
    lines.append(f'println("{v}={{{v}}}")')
    # Mixed int+float promotes (spec §4 arithmetic).
    v = fresh()
    lines.append(f"{v} := {rng.randint(0, 20)} + {flit()}")
    lines.append(f'println("{v}={{{v}}}")')
    # Casts with safe values.
    v = fresh()
    lines.append(f"{v} := int({rng.randint(0, 99)}.{rng.randint(1, 9)})")
    lines.append(f'println("{v}={{{v}}}")')
    v = fresh()
    lines.append(f"{v} := float({rng.randint(0, 99)})")
    lines.append(f'println("{v}={{{v}}}")')
    v = fresh()
    lines.append(f"{v} := math.pow(10.0, {rng.randint(1, 3)}.0)")
    lines.append(f'println("{v}={{{v}}}")')
    # NaN/INF spellings (both print NaN/inf/-inf; strict conformance
    # lives in edge_float_format — here they just ride along).
    v = fresh()
    lines.append(f"{v} := 0.0 / 0.0")
    lines.append(f'println("{v}={{{v}}}")')
    v = fresh()
    lines.append(f"{v} := math.INF")
    lines.append(f'println("{v}={{{v}}}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v4_prec(seed):
    """Mixed `*`/`/` with `**` (the parse_multiplicative bug family)."""
    rng = random.Random(1000211 * seed + 211)
    lines = []
    n = [0]

    def fresh(prefix="pp"):
        n[0] += 1
        return f"{CPREFIX}{prefix}{n[0]}"

    def pow_expr(nonzero=False):
        # Small bases, exponents 0..3 (nonnegative: neg traps by design).
        # Division RHS must be nonzero (0**N traps like any zero divisor).
        lo = 1 if nonzero else 0
        return f"{rng.randint(lo, 9)}**{rng.randint(0, 3)}"

    for _ in range(rng.randint(2, 4)):
        v = fresh()
        op = rng.choice(["*", "/", "+", "-"])
        pe = pow_expr(nonzero=(op == "/"))
        style = rng.random()
        if style < 0.4:
            lines.append(f"{v} := {rng.randint(1, 30)} {op} {pe}")
        elif style < 0.7:
            lines.append(
                f"{v} := ({rng.randint(1, 30)}*{rng.randint(1, 9)}+1) {op} {pe}"
            )
        else:
            lines.append(
                f"{v} := {rng.randint(1, 9)} {op} {rng.randint(1 if op == '/' else 0, 9)}**{rng.randint(0, 3)}"
            )
        lines.append(f'println("{v}={{{v}}}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v4_closure(seed):
    """Agreeing closure shapes: accumulator, factory, late-bound global."""
    rng = random.Random(1000229 * seed + 221)
    lines = []
    style = rng.random()
    if style < 0.4:
        # By-ref accumulator, called repeatedly.
        lines.append(f"{N('c')} := 0")
        lines.append(f"{N('f')} := |x: int| {{ {N('c')} = {N('c')} + x }}")
        for _ in range(rng.randint(2, 3)):
            lines.append(f"{N('f')}({rng.randint(1, 20)})")
        lines.append(f'println("c={{{N("c")}}}")')
    elif style < 0.7:
        # Escaping factory: func returns a closure over its param.
        k = rng.randint(1, 50)
        lines.append(f"func {N('mk')}(k: int) {{")
        lines.append("    |x: int| k + x")
        lines.append("}")
        lines.append(f"{N('a')} := {N('mk')}({k})")
        lines.append(f'println("a={{{N("a")}({rng.randint(1, 9)})}}")')
        lines.append(f'println("b={{{N("a")}({rng.randint(1, 9)})}}")')
    else:
        # Closure observes later assignment to the captured global.
        lines.append(f"{N('g')} := {rng.randint(0, 20)}")
        lines.append(f"{N('h')} := |x: int| {N('g')} + x")
        lines.append(f"{N('g')} = {rng.randint(21, 99)}")
        lines.append(f'println("h={{{N("h")}(1)}}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v4_neststore(seed):
    """Nested/aliased stores with pure indices (value semantics)."""
    rng = random.Random(1000249 * seed + 231)
    lines = ["import std.vec"]
    style = rng.random()
    if style < 0.25:
        n = rng.randint(2, 4)
        m = rng.randint(2, 4)
        row0 = ", ".join(str(rng.randint(0, 9)) for _ in range(m))
        row1 = ", ".join(str(rng.randint(0, 9)) for _ in range(m))
        i, j = rng.randint(0, 1), rng.randint(0, m - 1)
        v = rng.randint(10, 99)
        lines.append(f"{N('m')} := [[{row0}], [{row1}]]")
        lines.append(f"{N('m')}[{i}][{j}] = {v}")
        lines.append(f'println("e={{{N("m")}[{i}][{j}]}}")')
        lines.append(f'println("n={n}")')
    elif style < 0.5:
        v = rng.randint(10, 99)
        lines.append(f"struct {N('Rv')} {{ v: int }}")
        lines.append(f"{N('s')} := [{N('Rv')}{{v: {rng.randint(0, 9)}}}]")
        lines.append(f"{N('s')}[0].v = {v}")
        lines.append(f'println("sv={{{N("s")}[0].v}}")')
        lines.append(f"{N('t')} := {N('s')}")
        lines.append(f"{N('t')}[0].v = {v + 1}")
        lines.append(f'println("st={{{N("s")}[0].v}}")')
        lines.append(f'println("tt={{{N("t")}[0].v}}")')
    elif style < 0.75:
        a = [rng.randint(0, 9) for _ in range(rng.randint(2, 4))]
        v = rng.randint(10, 99)
        i = rng.randint(0, len(a) - 1)
        lines.append(f"{N('a')} := [{', '.join(map(str, a))}]")
        lines.append(f"{N('a2')} := {N('a')}")
        lines.append(f"{N('a2')}[{i}] = {v}")
        lines.append(f'println("a0={{{N("a")}[{i}]}}")')
        lines.append(f'println("a20={{{N("a2")}[{i}]}}")')
    else:
        lines.append(f'{N("d")} := {{"k": [1]}}')
        lines.append(f'{N("kk")} := "k"')
        lines.append(
            f"{N('d')}[{N('kk')}] = vec.push({N('d')}[{N('kk')}], {rng.randint(2, 9)})"
        )
        dn, kkn = N("d"), N("kk")
        lines.append('println("dl={len(' + dn + "[" + kkn + '])}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v4_str(seed):
    """Concat chains, interpolation of every scalar kind, str() casts."""
    rng = random.Random(1000259 * seed + 241)
    lines = []
    n = [0]

    def fresh(prefix="st"):
        n[0] += 1
        return f"{CPREFIX}{prefix}{n[0]}"

    strs = []
    for _ in range(rng.randint(1, 2)):
        v = fresh()
        lit = rng.choice(["ab", "x", "hello", "zz9"])
        lines.append(f'{v} := "{lit}"')
        strs.append(v)
    for _ in range(rng.randint(2, 3)):
        v = fresh()
        style = rng.random()
        if style < 0.4 and len(strs) >= 1:
            a = rng.choice(strs)
            b = rng.choice(strs)
            lines.append(f"{v} := {a} + {b} + {a}")
        elif style < 0.7:
            lines.append(
                f'{v} := "i={{{rng.randint(0, 99)}}} f={{{rng.randint(0, 9)}.{rng.randint(0, 9)}}} b={str(rng.random() < 0.5).lower()}"'
            )
        else:
            lines.append(
                f"{v} := str({rng.randint(0, 999)}) + str({rng.randint(0, 9)}.{rng.randint(0, 9)})"
            )
        strs.append(v)
        lines.append(f'println("{v}={{{v}}}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v4_compound(seed):
    """Compound stores with pure operands (each side evaluated once)."""
    rng = random.Random(1000271 * seed + 251)
    lines = []
    an, sn, cwn = N("a"), N("s"), N("Cw")
    n = rng.randint(2, 5)
    a = [rng.randint(0, 9) for _ in range(n)]
    i = rng.randint(0, n - 1)
    v = rng.randint(1, 9)
    op = rng.choice(["+=", "-=", "*="])
    lines.append(an + " := [" + ", ".join(map(str, a)) + "]")
    lines.append(an + "[" + str(i) + "] " + op + " " + str(v))
    lines.append('println("e={' + an + "[" + str(i) + ']}")')
    lines.append("struct " + cwn + " { w: int }")
    lines.append(sn + " := " + cwn + "{w: " + str(rng.randint(0, 20)) + "}")
    lines.append(sn + ".w " + rng.choice(["+=", "*="]) + " " + str(rng.randint(1, 9)))
    lines.append('println("sw={' + sn + '.w}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v4_cast(seed):
    """Total casts incl. exponent-string float parsing. String literals
    are hoisted to bindings: ZZ strings cannot nest double quotes."""
    rng = random.Random(1000289 * seed + 261)
    lines = []
    sn1n = N("sn1")
    lines.append(sn1n + ' := "' + str(rng.randint(0, 999)) + '"')
    lines.append('println("i1={int(' + sn1n + ')}")')
    lines.append(f'println("i2={{int({rng.randint(0, 99)}.{rng.randint(1, 9)})}}")')
    lines.append(f'println("f1={{float({rng.randint(0, 99)})}}")')
    sen = N("se")
    lines.append(sen + ' := "' + str(rng.randint(1, 9)) + "e" + str(rng.randint(1, 3)) + '"')
    lines.append('println("f2={float(' + sen + ')}")')
    lines.append(f'println("s1={{str({rng.randint(0, 99)}.{rng.randint(1, 9)})}}")')
    szn = N("sz")
    lines.append(szn + ' := "zz"')
    lines.append('println("d={int(' + szn + ') ?? -1}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_v4_struct(seed):
    """Plain structs: nesting, field writes, copies (no methods, no
    whole-struct display — both are deferred AOT-lowering bugs)."""
    rng = random.Random(1000303 * seed + 271)
    lines = []
    style = rng.random()
    if style < 0.5:
        ptn, pn, qn = N("Pt"), N("p"), N("q")
        lines.append("struct " + ptn + " { x: int, y: int }")
        lines.append(pn + " := " + ptn + "{x: " + str(rng.randint(0, 20)) + ", y: " + str(rng.randint(0, 20)) + "}")
        lines.append(pn + ".x = " + pn + ".x + " + str(rng.randint(1, 9)))
        lines.append('println("px={' + pn + '.x}")')
        lines.append('println("py={' + pn + '.y}")')
        lines.append(qn + " := " + pn)
        lines.append(qn + ".y = " + str(rng.randint(21, 99)))
        lines.append('println("qy={' + qn + '.y}")')
        lines.append('println("py2={' + pn + '.y}")')
    else:
        inn, outn, on = N("In"), N("Out"), N("o")
        lines.append("struct " + inn + " { n: int }")
        lines.append("struct " + outn + " { inner: " + inn + ", tag: int }")
        lines.append(on + " := " + outn + "{inner: " + inn + "{n: " + str(rng.randint(0, 9)) + "}, tag: 1}")
        lines.append(on + ".inner.n = " + str(rng.randint(10, 99)))
        lines.append('println("n={' + on + '.inner.n}")')
        lines.append('println("t={' + on + '.tag}")')
    lines.append('println("DONE")')
    return "\n".join(lines) + "\n"


def gen_case(shape, seed):
    """Dispatch one (shape, seed) to its generator (batch + regen)."""
    global CPREFIX
    if shape == "default":
        return gen_program(seed)
    if shape == "v2":
        return gen_program_v2(seed)
    if shape == "v3":
        return gen_program_v3(seed)
    if shape == "v4":
        return gen_program_v4(seed)
    raise ValueError(f"unknown shape {shape}")


def emit_batch(shape, seeds, perbatch, outdir):
    """Group cases into batch files with prefixed names + markers.

    Returns a manifest dict {batch_file: [{idx, shape, seed, prefix}]}.
    Imports are unioned to the top; each case's DONE marker is replaced
    by BEGIN/END markers so the runner can demux per-case output.
    """
    import json

    global CPREFIX
    manifest = {}
    chunks = [seeds[i : i + perbatch] for i in range(0, len(seeds), perbatch)]
    for b, chunk in enumerate(chunks):
        imports = []
        bodies = []
        cases = []
        for idx, seed in enumerate(chunk):
            prefix = "c%d_" % idx
            CPREFIX = prefix
            src = gen_case(shape, seed)
            CPREFIX = ""
            blines = []
            for line in src.splitlines():
                if line.startswith("import "):
                    if line not in imports:
                        imports.append(line)
                    continue
                if line.strip() == 'println("DONE")':
                    continue
                blines.append(line)
            bodies.append(blines)
            cases.append({"idx": idx, "shape": shape, "seed": seed, "prefix": prefix})
        bfile = "b%05d.zz" % b
        with open(outdir + "/" + bfile, "w") as f:
            for imp in imports:
                f.write(imp + "\n")
            for c, blines in zip(cases, bodies):
                f.write(
                    'println("ZZBEGIN ' + c["prefix"] + " s" + str(c["seed"]) + '")\n'
                )
                f.write("\n".join(blines) + "\n")
                f.write('println("ZZEND ' + c["prefix"] + '")\n')
            f.write('println("ZZDONE")\n')
        manifest[bfile] = cases
    with open(outdir + "/manifest.json", "w") as f:
        json.dump(manifest, f, indent=1)
    return manifest


def gen_program_v4(seed):
    rng = random.Random(999961 * seed + 13)
    k = rng.random()
    if k < 0.14:
        return gen_v4_float(seed)
    if k < 0.26:
        return gen_v4_prec(seed)
    if k < 0.38:
        return gen_v4_closure(seed)
    if k < 0.52:
        return gen_v4_neststore(seed)
    if k < 0.64:
        return gen_v4_str(seed)
    if k < 0.76:
        return gen_v4_compound(seed)
    if k < 0.88:
        return gen_v4_cast(seed)
    return gen_v4_struct(seed)


if __name__ == "__main__":
    import os

    # v2 shapes (branch call-returns, string accumulators, stringify-like
    # struct emits) live behind `--shapes v2` so the default mode stays
    # byte-identical for every seed (fixed-seed CI smoke fixtures).
    if len(sys.argv) > 1 and sys.argv[1] == "--batch":
        # gen.py --batch SHAPE START COUNT OUTDIR PERBATCH
        # Many cases per file (prefixed names + markers); the runner
        # demuxes per-case output and bisects failures into singles.
        shape, start, count, outdir, perbatch = (
            sys.argv[2],
            int(sys.argv[3]),
            int(sys.argv[4]),
            sys.argv[5],
            int(sys.argv[6]),
        )
        os.makedirs(outdir, exist_ok=True)
        seeds = list(range(start, start + count))
        manifest = emit_batch(shape, seeds, perbatch, outdir)
        print(f"wrote {len(manifest)} batch files ({count} cases)")
    elif len(sys.argv) > 1 and sys.argv[1] == "--one":
        # gen.py --one SHAPE SEED PREFIX OUTFILE (bisect regen from manifest)
        shape, seed, prefix, outfile = (
            sys.argv[2],
            int(sys.argv[3]),
            sys.argv[4],
            sys.argv[5],
        )
        CPREFIX = prefix
        with open(outfile, "w") as f:
            f.write(gen_case(shape, seed))
        CPREFIX = ""
        print(f"wrote {outfile}")
    elif len(sys.argv) > 1 and sys.argv[1] == "--shapes":
        assert sys.argv[2] in ("v2", "v3", "v4"), "only --shapes v2/v3/v4 are supported"
        start, count, outdir = int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
        os.makedirs(outdir, exist_ok=True)
        if sys.argv[2] == "v3":
            for i in range(start, start + count):
                with open(f"{outdir}/ga{i:05d}.zz", "w") as f:
                    f.write(gen_program_v3(i))
            print(f"wrote {count} v3 programs")
        elif sys.argv[2] == "v4":
            for i in range(start, start + count):
                with open(f"{outdir}/gv{i:05d}.zz", "w") as f:
                    f.write(gen_program_v4(i))
            print(f"wrote {count} v4 programs")
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
