#!/usr/bin/env python3
"""M0 helper: propose `// features:` tags for every fixture from content.

Heuristics only -- output is reviewed before applying. Run:
  python3 scripts/tag_fixtures.py --report   # tag frequency + untagged files
  python3 scripts/tag_fixtures.py --apply    # prepend `// features:` line 1
"""

import os
import re
import sys

ROOT = os.path.join(os.path.dirname(__file__), "..", "tests", "fixtures")

STD_MODS = {
    "args": "std-args",
    "bytes": "std-bytes",
    "chan": "std-chan",
    "colors": "std-colors",
    "crypto": "std-crypto",
    "csv": "std-csv",
    "db": "std-db",
    "dec": "std-dec",
    "encoding": "std-encoding",
    "env": "std-env",
    "envmod": "std-env",
    "fs": "std-fs",
    "http": "std-http",
    "json": "std-json",
    "log": "std-log",
    "map": "std-map",
    "math": "std-math",
    "net": "std-net",
    "path": "std-path",
    "process": "std-process",
    "regexp": "std-regexp",
    "sqlz": "std-sqlz",
    "str": "std-str",
    "sys": "std-sys",
    "task": "std-task",
    "term": "std-term",
    "time": "std-time",
    "uuid": "std-uuid",
    "vec": "std-vec",
    "set": "std-map",
}


def strip_strings_and_comments(src):
    out = []
    for line in src.splitlines():
        s = line.strip()
        if s.startswith("//"):
            continue
        # crude: drop double-quoted spans (keeps code shape for op detection)
        line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
        line = re.sub(r"'(?:[^'\\]|\\.)*'", "''", line)
        out.append(line)
    return "\n".join(out)


def propose(path, src):
    tags = set()
    d = os.path.basename(os.path.dirname(path))
    if d == "errors":
        tags.add("error-case")
    if d == "test":
        tags.add("zz-test")

    for line in src.splitlines():
        s = line.strip()
        if s.startswith("import "):
            tags.add("imports")
            if "(" in s:
                tags.add("selective-import")
            if " as " in s:
                tags.add("import-alias")
            if "(*)" in s:
                tags.add("wildcard-import")
            m = re.search(r"import\s+(?:[\w.]+\s+as\s+)?([\w.]+)", s)
            if m:
                parts = m.group(1).split(".")
                if parts[0] == "std" and len(parts) > 1 and parts[1] in STD_MODS:
                    tags.add(STD_MODS[parts[1]])
                if parts[0] in ("support", "helpers", "math"):
                    tags.add("imports")

    code = strip_strings_and_comments(src)
    has = lambda pat: re.search(pat, code) is not None  # noqa: E731

    if has(r"\bprint(?!_)"):
        tags.add("print")
    if has(r"\binput\s*\("):
        tags.add("stdin")
    if has(r"\bargs\b"):
        tags.add("cli-args")
    if has(r"embed(fs)?\b"):
        tags.add("embed")
    if has(r"\b(memfs|tarfs|osfs)\b"):
        tags.add("vfs")
    if has(r":="):
        tags.add("locals")
    if has(r"\bconst\b"):
        tags.add("const")
    if has(r"\bif\b"):
        tags.add("if-else")
    if has(r"\bif\s+let\b"):
        tags.add("if-let")
    if has(r"\bwhile\b"):
        tags.add("while-loop")
    if has(r"\bfor\b"):
        tags.add("for-loop")
    if has(r"\bbreak\b|\bcontinue\b"):
        tags.add("break-continue")
    if has(r"\bfunc\b"):
        tags.add("func-call")
    if has(r"\breturn\b"):
        tags.add("return")
    if has(r"\bassert"):
        tags.add("assert")
    if has(r"@test\b"):
        pass  # test attribute: zz-test via dir/@test below, not decorators
    if has(r"@(?!test\b)\w"):
        tags.add("decorators")
    if has(r"=\s*\|[^|\n]*\||\|\s*\w+\s*(:[^|\n]*)?\|"):
        tags.add("closures")
    if has(r"\bmap\s*\(|\bfilter\s*\(|\benumerate\s*\(|\bzip\s*\(|\brange\s*\("):
        tags.add("hof")
    if has(r"\bmatch\b"):
        tags.add("match")
    if has(r"\bdefer\b"):
        tags.add("defer")
    if has(r"&&|\|\|"):
        tags.add("short-circuit")
    if has(r"\btrue\b|\bfalse\b|!\w|!\(|~"):
        tags.add("bool-logic")
    if has(r"\?\?"):
        tags.add("elvis")
    if has(r"\|>"):
        tags.add("pipe")
    # Bitwise: checked on code with closure pipes removed (`|x| ...`
    # would otherwise read as BitOr).
    code_noclos = re.sub(r"\|\s*\w+[^|\n]*\|", "CLOS", code)
    has_nc = lambda pat: re.search(pat, code_noclos) is not None  # noqa: E731
    if has_nc(r"<<|>>|\^|~") or re.search(r"[^|]\|[^|>]", code_noclos):
        tags.add("bitwise")
    if has(r"==|!=|<=|>=|<[^<]|>[^>]"):
        tags.add("comparison")
    if has(r"\d\s*(\+|-|\*|/|%|\*\*)\s*\d|[a-zA-Z_)]\s*(\+|-|\*|/|%)\s*[\w\d\"'(]"):
        tags.add("int-arith")
    if has(r"\d+\.\d+"):
        tags.add("float-arith")
    if has(r"\bint\s*\(|\bfloat\s*\(|\bbool\s*\(|\bstr\s*\("):
        tags.add("casts")
    if has(r"\bstruct\b"):
        tags.add("structs")
    if has(r"\bimpl\b"):
        tags.add("struct-methods")
    # Recursion: a `func name` whose name is called inside its own body.
    for m in re.finditer(r"\bfunc\s+(\w+)", code):
        name = m.group(1)
        start = code.find("{", m.end())
        if start < 0:
            continue
        depth = 0
        body = None
        for i in range(start, len(code)):
            if code[i] == "{":
                depth += 1
            elif code[i] == "}":
                depth -= 1
                if depth == 0:
                    body = code[start:i]
                    break
        if body and re.search(r"\b" + re.escape(name) + r"\s*\(", body):
            tags.add("recursion")
    # Methods: dotted calls on non-module receivers in struct/impl files.
    if has(r"\bstruct\b") or has(r"\bimpl\b"):
        std_heads = set(STD_MODS) | {
            "std",
            "math",
            "vec",
            "str",
            "json",
            "http",
            "fs",
            "env",
            "net",
            "chan",
            "task",
            "dict",
            "bytes",
            "args",
        }
        for m in re.finditer(r"\b([a-z_]\w*)\.\w+\s*\(", code):
            if m.group(1) not in std_heads:
                tags.add("methods")
                break
    if has(r"\bgenerics?\b|<[A-Z]\w*(,|>)"):
        tags.add("generics")
    if has(r"\btype\s+\w+\s*="):
        tags.add("aliases")
    if has(r"\benum\b"):
        tags.add("enums")
    if has(r"\.(some|none|ok|err)\b|Option<|Result<"):
        tags.add("option-result")
    if has(r'""'):
        tags.add("string-literal")
    if has(r'f""'):
        tags.add("fstrings")
        tags.add("string-ops")
    if has(r'"""'):
        tags.add("string-blocks")
    # Arrays: `[` that does not index (not preceded by a receiver).
    # `d["a"]` is indexing; `:= [1, 2]` and `([1], [2])` are arrays.
    if re.search(r"(?<![\w)\]\"'])\[", code):
        tags.add("arrays")
    if has(r"vec\.push|vec\.append|\.push\(|\.append\("):
        tags.add("vec-push")
    if has(r"dict\.") or has(r"\.keys\(\)"):
        tags.add("dicts")
    if has(r'\{[^}\n]*"[^"\n]*"\s*:'):
        tags.add("dicts")
    if has(r":=\s*\([^)]*,"):
        tags.add("tuples")
    if has(r"\.\."):
        tags.add("ranges")
    if has(r"\w\[[^\]]*:[^\]]*\]"):
        tags.add("slicing")
    if has(r"\w\[[^\]]+\]"):
        tags.add("indexing")
    if has(r"\bbytes\b|read_bytes|write_bytes"):
        tags.add("bytes")
    if has(r"\(\s*\w+\s*,\s*\w+.*\)\s*:="):
        tags.add("destructuring")
    if has(r"\bpub\b"):
        tags.add("pub-visibility")
    if has(r"\btry\b|\?\s*$|\?[^?=\n]"):
        if has(r"\btry\b"):
            tags.add("try-question")
    if has(r"\b(spawn|sleep_ms|now_ms)\b"):
        pass  # covered via std-task/std-time/std-chan imports
    if "@test" in src:
        tags.add("zz-test")
    if has(r"\b(none|NAN|INF)\b"):
        pass
    return sorted(tags)


# Manual overrides: fixtures whose tags need human judgment
# (comment-only files, inference subjects, method-syntax subjects).
MANUAL = {
    "modules/reexports.zz": ["imports", "pub-visibility"],
    "types/type_inference.zz": [
        "arrays",
        "bool-logic",
        "dicts",
        "float-arith",
        "int-arith",
        "locals",
        "print",
        "string-literal",
        "type-inference",
    ],
}

# Files matched by stem substring get these extra tags.
STEM_TAGS = {
    "infer": ["type-inference"],
}


def manual_tags(path):
    """Manual tags by path relative to fixtures root, or None."""
    rel = os.path.relpath(path, ROOT)
    if rel in MANUAL:
        return sorted(MANUAL[rel])
    stem = os.path.basename(path)[:-3]
    for sub, tags in STEM_TAGS.items():
        if sub in stem:
            return sorted(tags)
    return None


def all_fixtures():
    out = []
    for dirpath, _, files in os.walk(ROOT):
        for f in sorted(files):
            if f.endswith(".zz"):
                out.append(os.path.join(dirpath, f))
    return sorted(out)


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "--report"
    problems = []
    results = {}
    for path in all_fixtures():
        with open(path) as fh:
            src = fh.read()
        if re.match(r"\s*// features:", src):
            continue  # already tagged
        manual = manual_tags(path)
        if manual is not None:
            tags = sorted(set(manual) | set(propose(path, src)))
        else:
            tags = propose(path, src)
        results[path] = tags
        if not tags:
            problems.append(path)
    if mode == "--report":
        from collections import Counter

        c = Counter()
        for tags in results.values():
            c.update(tags)
        print("files needing tags:", len(results))
        for tag, n in c.most_common():
            print(f"  {tag:20s} {n}")
        if problems:
            print("UNTAGGED:")
            for p in problems:
                print("  ", p)
    elif mode == "--apply":
        for path, tags in results.items():
            with open(path) as fh:
                src = fh.read()
            header = "// features: " + ", ".join(tags) + "\n"
            with open(path, "w") as fh:
                fh.write(header + src)
            print("tagged", os.path.relpath(path, ROOT))
        if problems:
            print("WARNING: untagged files skipped:", problems)


if __name__ == "__main__":
    main()
