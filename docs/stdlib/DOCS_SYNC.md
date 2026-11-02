# Docs sync — stdlib per-module files → zz website

These `.md` files under `docs/stdlib/` are the single source of truth for the
zz website stdlib docs. One file per stdlib module; no file contains two or
more stdlibs. The website ingests them by convention.

## Convention

### Frontmatter (optional but recommended)

Each file may start with:

```markdown
---
module: std.str
title: String — `std.str`
source: crates/zz_stdlib/src/funcs.rs
---
```

If omitted, the website derives `module` from the filename (`str.md` →
`std.str`) and `title` from the H1.

### H1

`# <Module> — \`std.<name>\``

### Exhaustive table (source of truth for the website name index)

Every per-stdlib file ends with (or contains) a table:

| Function / Constant | Signature | Description | Example |
|---------------------|-----------|-------------|---------|
| `str.trim` | `str.trim(s: str) -> str` | Strip whitespace | `"  hi  ".trim()` → `"hi"` |

Columns (in order):
1. `Function / Constant` — qualified name, e.g. `std.str.trim`
2. `Signature` — ZZ type signature
3. `Description` — one-line plain text
4. `Example` — runnable snippet or `→` result

The website name index is generated from these tables. Every entry in
`crates/zz_stdlib/src/funcs.rs` (685 signatures, asserted by the checker
test) must appear in exactly one table across all per-stdlib files.

### Examples

Runnable examples live in a separate "Examples" section below the table
(keeps the table scannable). Examples should be `zz run`-able copy-paste
snippets.

## Sync rules

- Add a new stdlib function → add it to `funcs.rs` AND to the matching
  per-stdlib `.md` table in the same commit.
- Rename/remove a function → update both `funcs.rs` and the `.md` table.
- Never document two stdlibs in one file. Split combined files.
- Do not duplicate table content in `stdlib.md` or `reference.md` — those
  are aggregate indexes that link to per-module files.
- Keep examples copy-paste runnable; spot-check with `zz check`/`zz run`.

## Verification

- `cargo test -p zz_stdlib` — confirms `funcs.len() == 685`
- Confirm no combined files remain in `docs/stdlib/`
- Confirm every `funcs.rs` entry appears in exactly one `.md` table
- No dangling internal links in `SUMMARY.md`, `overview.md`, `stdlib.md`
