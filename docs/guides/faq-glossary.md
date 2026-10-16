# FAQ and glossary

## FAQ

**How is ZZ different from Rust?**
Independent language (own frontend/checker/VM/AOT/stdlib). Value semantics, no
`&`/lifetimes/borrow checker; green threads + channels by default; batteries
(HTTP/JSON/fs/SQL). No `unsafe`, zero-cost abstractions, `no_std`, or manual
layout — use Rust as a ZZ native plugin for those.

**VM or native?**
`zz run` for iteration (no artifacts), `zz build` for real binaries.
They agree bit-for-bit; see [Parity](parity.md).

**Where do imports resolve?**
Relative to the importing file; package name maps to `src/` anywhere in the
project. Precedence `std` > package > registry > relative. See
[Modules](../language/modules.md).

**Why does `fs.write(...)` warn?**
`Result` must-use: `match`, `?`, or `_ := ...`. See [Pattern matching](../language/pattern-matching.md).

**Why did my match fail to compile?**
Missing variant (add it or `_`) or bare `.ok` (needs payload: `.ok(_)`).
Guards allow bindings+literals+operators only.

**Empty filter exits 0 — bug?**
No. `zz test <substr>` matching 0 prints a warning + hint, exits 0 (local DX
convenience, like `cargo test -- --skip`). CI should use `--fail-on-empty` or
assert `passed == 0` via `--json`.

**How do I do decimals/money?**
`std.dec` string decimals (`add("0.1","0.2")=="0.3"`), scale ≤18. Never floats.

**TOML?**
Not in stdlib — external `toml` package (pure ZZ, versioned separately).

## Glossary

- **AOT**: ahead-of-time native binary via HIR→C/chunk→C + clang.
- **Alias**: `type X = ...`, erased at check time.
- **Arena**: thread-local bump store for values (AOT).
- **CoW**: copy-on-write — O(1) clone, copy on shared write, unobservable.
- **DCE**: dead-code elimination via callgraph.
- **Doctest**: `/// ```zz` fence run as a test.
- **Embed**: `--embed <dir>` baked assets via `fs.embedfs()`.
- **Guard**: `pattern if cond =>` arm filter.
- **HIR/IR**: typed high-level IR / portable IR + codec/disassembler.
- **Parity**: VM==AOT guarantee + sweep enforcement.
- **Provider**: `fs.FS` backend (`osfs/memfs/tarfs/embedfs`).
- **Safepoint**: loop-top yield keeping green threads fair.
- **Snapshot**: `assert_snapshot` golden file under `snapshots/`.
- **Take-restore**: field-window move optimization, unobservable.
- **Value semantics**: independent copies, no aliasing.
- **VM**: bytecode/tree-walker interpreter for `zz run`/`zz test`.
