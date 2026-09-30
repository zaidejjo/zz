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


if __name__ == "__main__":
    start, count, outdir = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    import os

    os.makedirs(outdir, exist_ok=True)
    for i in range(start, start + count):
        with open(f"{outdir}/fz{i:05d}.zz", "w") as f:
            f.write(gen_program(i))
    print(f"wrote {count} programs")
