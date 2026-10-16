#!/usr/bin/env python3
"""Generate docs/stdlib/reference.md from crates/zz_stdlib/src/funcs.rs.

Usage:
    python3 scripts/gen-stdlib-ref.py           # write reference.md
    python3 scripts/gen-stdlib-ref.py --check   # fail if stale (CI)

Reads every "std.<mod>.<fn>" string literal, groups by module, and emits
one table per module with counts. Signatures live in funcs.rs; this file
lists names + module so prose docs never drift from the registry.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
FUNCS = ROOT / "crates/zz_stdlib/src/funcs.rs"
OUT = ROOT / "docs/stdlib/reference.md"

PAT = re.compile(r'"(std\.[a-z_0-9]+(?:\.[a-z_0-9]+)+)"')


def collect() -> dict[str, list[str]]:
    mods: dict[str, list[str]] = {}
    for m in PAT.finditer(FUNCS.read_text(encoding="utf-8")):
        full = m.group(1)
        parts = full.split(".")
        mod = parts[1] if len(parts) > 2 else "top"
        mods.setdefault(mod, [])
        if full not in mods[mod]:
            mods[mod].append(full)
    for v in mods.values():
        v.sort()
    return dict(sorted(mods.items()))


def render(mods: dict[str, list[str]]) -> str:
    total = sum(len(v) for v in mods.values())
    lines = [
        "# Stdlib reference — generated",
        "",
        f"Exhaustive name index: **{total} functions across {len(mods)} modules**.",
        "Generated from `crates/zz_stdlib/src/funcs.rs` — do not hand-edit.",
        "Regenerate with `python3 scripts/gen-stdlib-ref.py`.",
        "Signatures: see `funcs.rs`; semantics: per-module guides linked below.",
        "",
    ]
    guide = {
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
    for mod, fns in mods.items():
        g = guide.get(mod, "overview.md")
        lines.append(f"## `std.{mod}` — {len(fns)} fns ([guide]({g}))")
        lines.append("")
        lines.append("| Function |")
        lines.append("|---|")
        for fn in fns:
            lines.append(f"| `{fn}` |")
        lines.append("")
    lines.append(
        f"Total: {total} functions. Top-level built-ins: see [Built-ins](builtins.md)."
    )
    return "\n".join(lines)


def main() -> int:
    mods = collect()
    text = render(mods) + "\n"
    if "--check" in sys.argv:
        cur = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        if cur != text:
            print(f"stale: {OUT.relative_to(ROOT)} — run gen-stdlib-ref.py")
            return 1
        print(f"fresh: {sum(len(v) for v in mods.values())} fns")
        return 0
    OUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)}: {sum(len(v) for v in mods.values())} fns")
    return 0


if __name__ == "__main__":
    sys.exit(main())
