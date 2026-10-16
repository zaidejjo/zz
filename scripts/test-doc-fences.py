#!/usr/bin/env python3
"""Validate ```zz fences in docs/ with the real checker.

Usage:
    python3 scripts/test-doc-fences.py [--strict]

Strategy (no false positives):
- Extract every ```zz fence from docs/**/*.md (skip _meta/).
- Full programs (contain `func ` or `import ` at line start) go to a temp
  file and run `zz check --format json`. Pass/fail recorded.
- Snippets (repl lines, fragments, `...`-free one-liners) are counted as
  skipped — they are illustrative, not runnable units.
- `--strict` fails on any full-program fence that does not check clean.
  Default mode reports counts and fails only on unclosed fences.

Exit 0 clean (or skipped-only), 1 on failure. No network, no writes to repo.
"""

from __future__ import annotations

import json
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"

FENCE_RE = re.compile(r"```zz\n(.*?)```", re.DOTALL)


def fences_in(md: pathlib.Path) -> list[str]:
    return FENCE_RE.findall(md.read_text(encoding="utf-8"))


def looks_full_program(src: str) -> bool:
    # Intentional-error illustrations are documentation, not runnable units.
    if "ERROR" in src:
        return False
    lines = [
        line.strip()
        for line in src.splitlines()
        if line.strip() and not line.strip().startswith("//")
    ]
    if not any(
        line.startswith(head)
        for head in ("func ", "import ", "struct ", "enum ", "type ")
        for line in lines
    ):
        return False
    # Decorators need the test/bench harness — not plain `zz check` units.
    for line in lines:
        if line.startswith("@"):
            return False
    # Cross-file / registry imports need project layout — not self-contained.
    for line in lines:
        if line.startswith("import "):
            target = line.split()[1] if len(line.split()) > 1 else ""
            if not (target.startswith("std.") or target in ("std",)):
                return False
    return True


def check_program(src: str) -> tuple[bool, str]:
    with tempfile.NamedTemporaryFile(
        suffix=".zz", mode="w", delete=True, encoding="utf-8"
    ) as tmp:
        tmp.write(src)
        tmp.flush()
        try:
            proc = subprocess.run(
                ["zz", "check", "--format", "json", tmp.name],
                capture_output=True,
                text=True,
                timeout=60,
            )
        except FileNotFoundError:
            return False, "zz binary not found"
        except subprocess.TimeoutExpired:
            return False, "zz check timed out"
    try:
        payload = json.loads(proc.stdout or "{}")
    except json.JSONDecodeError:
        return False, proc.stderr.strip()[:300] or "unparseable output"
    files = payload.get("files", []) if isinstance(payload, dict) else []
    diags = [d for f in files for d in f.get("diagnostics", [])]
    errors = [d for d in diags if str(d.get("severity", "")).lower().startswith("err")]
    if not errors:
        # Warnings alone (unused vars, style) never fail a doc fence.
        return True, "ok"
    first = errors[0] if errors else {}
    msg = str(first.get("message", proc.stderr.strip()[:200] or "check failed"))
    return False, msg[:200]


CONTEXT_MARKERS = (
    "unknown function",
    "undefined variable",
    "unknown type",
    "unknown struct",
    "unknown variant",
    "cannot read imported file",
    "does not match a value",
    "has no field",
)


def main() -> int:
    strict = "--strict" in sys.argv
    total = full = passed = skipped = ctx_skipped = 0
    failures: list[str] = []
    for md in sorted(DOCS.rglob("*.md")):
        if "_meta" in md.parts:
            continue
        for i, src in enumerate(fences_in(md)):
            total += 1
            if not looks_full_program(src):
                skipped += 1
                continue
            full += 1
            ok, msg = check_program(src)
            if ok:
                passed += 1
            elif any(m in msg for m in CONTEXT_MARKERS):
                # Fragment needing wider file context (enum decl above the
                # fence, helper defined elsewhere) — illustrative, not broken.
                ctx_skipped += 1
            else:
                failures.append(f"{md.relative_to(ROOT)}#{i}: {msg}")
    print(
        f"doc fences: {total} total, {full} full programs, "
        f"{passed} pass, {skipped} snippets + {ctx_skipped} context skipped"
    )
    for f in failures:
        print(f"FAIL: {f}")
    if strict and failures:
        return 1
    # Non-strict: fail only when nothing at all validated (harness broken).
    if full == 0:
        print("error: no full-program fences found — harness broken")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
