#!/usr/bin/env python3
"""Generate docs/stdlib/reference.md from compiled FuncSig data.

Usage:
    python3 scripts/gen-stdlib-ref.py           # write reference.md
    python3 scripts/gen-stdlib-ref.py --check   # fail if stale (CI)

Source of truth is `crates/zz_stdlib/src/funcs.rs`, read through the real
`FuncSig` table (not regex): `cargo run -q -p zz_stdlib --example dump_sigs`
emits `name \\t generics \\t params \\t return` per function, and this script
renders one signature table per module. Docs can never drift from what
`zz check` enforces.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "docs/stdlib/reference.md"

GUIDE = {
    "str": "str-vec.md",
    "vec": "str-vec.md",
    "bytes": "str-vec.md",
    "map": "data.md",
    "set": "data.md",
    "dec": "data.md",
    "json": "data.md",
    "csv": "data.md",
    "property": "data.md",
    "fs": "fs-bytes.md",
    "http": "http.md",
    "net": "net.md",
    "env": "env-process.md",
    "args": "env-process.md",
    "process": "env-process.md",
    "sys": "cell-path-sys.md",
    "log": "env-process.md",
    "math": "math-time.md",
    "time": "math-time.md",
    "term": "terminal.md",
    "colors": "terminal.md",
    "encoding": "encoding-crypto.md",
    "crypto": "encoding-crypto.md",
    "uuid": "encoding-crypto.md",
    "regexp": "regexp.md",
    "sqlz": "sql-db.md",
    "chan": "concurrency.md",
    "task": "concurrency.md",
    "cell": "cell-path-sys.md",
    "path": "cell-path-sys.md",
    "test": "testing-lib.md",
}


def dump() -> list[tuple[str, str, str, str]]:
    proc = subprocess.run(
        ["cargo", "run", "-q", "-p", "zz_stdlib", "--example", "dump_sigs"],
        capture_output=True,
        text=True,
        cwd=ROOT,
        timeout=300,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"dump_sigs failed:\n{proc.stderr[-2000:]}")
    rows = []
    for line in proc.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) != 4:
            continue
        rows.append((parts[0], parts[1], parts[2], parts[3]))
    rows.sort(key=lambda r: r[0])
    return rows


def module_of(name: str) -> str:
    parts = name.split(".")
    if len(parts) >= 3 and parts[0] == "std":
        return parts[1]
    if len(parts) == 1:
        return "builtins"
    return parts[0].lower()


def render(rows: list[tuple[str, str, str, str]]) -> str:
    mods: dict[str, list[tuple[str, str, str, str]]] = {}
    for row in rows:
        mods.setdefault(module_of(row[0]), []).append(row)
    total = len(rows)
    lines = [
        "# Stdlib reference — generated",
        "",
        f"Exhaustive signature index: **{total} entries across {len(mods)} groups**.",
        "Generated from compiled `FuncSig` data (`crates/zz_stdlib/src/funcs.rs`",
        "via `dump_sigs`) — do not hand-edit. Regenerate with",
        "`python3 scripts/gen-stdlib-ref.py`. Semantics: per-module guides linked below.",
        "A `= …` param has a default and may be omitted. Bare names (`print`,",
        "`assert_eq`, `map`) need no import; `std.*` names need their module import.",
        "",
    ]
    for mod in sorted(mods):
        fns = mods[mod]
        guide = GUIDE.get(mod, "overview.md")
        lines.append(f"## `{mod}` — {len(fns)} entries ([guide]({guide}))")
        lines.append("")
        lines.append("| Function | Signature |")
        lines.append("|---|---|")
        for name, generics, params, ret in fns:
            short = name.split(".")[-1]
            gen = f"<{generics}>" if generics else ""
            lines.append(f"| `{name}` | `{short}{gen}({params}) -> {ret}` |")
        lines.append("")
    lines.append(
        "Top-level built-ins live under `builtins` above. "
        "Method aliases (`File.open`, `Regexp.is_match`) dispatch on receivers — "
        "see [Built-ins](builtins.md) and [Regexp](regexp.md)."
    )
    return "\n".join(lines)


def main() -> int:
    text = render(dump()) + "\n"
    if "--check" in sys.argv:
        cur = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        if cur != text:
            print(f"stale: {OUT.relative_to(ROOT)} — run gen-stdlib-ref.py")
            return 1
        print(f"fresh: {text.count(chr(10) + '| `')} entries")
        return 0
    OUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
