#!/usr/bin/env python3
"""Check ZZ docs: relative links resolve, no ../../ escapes, stdlib coverage.

Usage:
    python3 scripts/check-docs.py

Exit 0 when clean, 1 with a report otherwise. No network, no writes.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
FUNCS = ROOT / "crates/zz_stdlib/src/funcs.rs"

LINK_RE = re.compile(r"\]\(([^)#\s]+)(?:#[^)]*)?\)")
STD_RE = re.compile(r"std\.([a-z_]+)")


def check_links() -> list[str]:
    errors: list[str] = []
    for md in sorted(DOCS.rglob("*.md")):
        text = md.read_text(encoding="utf-8")
        for m in LINK_RE.finditer(text):
            link = m.group(1).strip()
            if not link or link.startswith(("http://", "https://", "mailto:")):
                continue
            if not link.endswith(".md") and not link.endswith(".json"):
                # Allow anchors and command snippets only when not .md-like.
                if " " in link or link.startswith(("-", "`")):
                    continue
                if "." in pathlib.Path(link).suffix:
                    pass
                else:
                    continue
            if "../.." in link:
                errors.append(f"{md.relative_to(ROOT)}: escapes repo: {link}")
                continue
            target = (md.parent / link).resolve()
            try:
                target.relative_to(DOCS.resolve())
            except ValueError:
                # Links from docs/README.md to docs/* are fine; anything
                # outside docs/ is an error except explicit legacy pointers.
                if link.startswith("../"):
                    errors.append(f"{md.relative_to(ROOT)}: outside docs/: {link}")
                continue
            if not target.exists():
                errors.append(f"{md.relative_to(ROOT)}: missing: {link}")
    return errors


def check_stdlib_coverage() -> list[str]:
    warnings: list[str] = []
    if not FUNCS.exists():
        return ["funcs.rs not found, skipping coverage"]
    mods = sorted(set(STD_RE.findall(FUNCS.read_text(encoding="utf-8"))))
    # Modules that are intentionally undocumented as user API.
    skip = {"module", "span", "toml"}
    corpus = "\n".join(p.read_text(encoding="utf-8") for p in DOCS.rglob("*.md"))
    for mod in mods:
        if mod in skip:
            continue
        if f"std.{mod}" not in corpus:
            warnings.append(f"warn: std.{mod} has no docs mention")
    return warnings


def check_fences() -> list[str]:
    errors: list[str] = []
    for md in sorted(DOCS.rglob("*.md")):
        text = md.read_text(encoding="utf-8")
        opens = text.count("```zz")
        closes = text.count("```")
        if closes < opens:
            errors.append(f"{md.relative_to(ROOT)}: unclosed ```zz fence")
        # Flag task markers (TODO: / FIXME: / XXX:) but allow prose mentions
        # like "no open tasks" without a colon.
        for marker in ("TODO:", "FIXME:", "XXX:", "TBD:"):
            if marker in text:
                errors.append(f"{md.relative_to(ROOT)}: contains {marker}")
    return errors


def main() -> int:
    link_errors = check_links()
    fence_errors = check_fences()
    coverage = check_stdlib_coverage()
    for e in link_errors + fence_errors:
        print(f"error: {e}")
    for w in coverage:
        print(w)
    failed = bool(link_errors or fence_errors)
    print(
        f"docs check: {len(link_errors)} link errors, "
        f"{len(fence_errors)} fence errors, "
        f"{len([w for w in coverage if w.startswith('warn')])} coverage warnings"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
