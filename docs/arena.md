# ZZ Compiler Arena (P0–P6 record)

Fast + light + automatic memory discipline for the compile pipeline.
No language semantics changed; every step measured; full suite green.

## What shipped (branch `feat/arena-allocator`)

| Phase | Change | Effect (`zz check`, large 2500-func file) |
|---|---|---|
| P0 | `bench/compiler/{small,medium,large}.zz` + `many_funcs` + `many_exprs` + `scripts/bench-compiler.sh` | baseline: 305ms debug / — release |
| P1 | New `zz_arena` crate: `Bump` (64KB→4MB adaptive blocks, ¼-block spill, caps as `ArenaError`, checkpoint/rewind, closure `scoped`, reset-with-recycle, `FrozenBump`), deterministic `Symbol` interner, `ArenaVec`, `Stats` + `ZZ_ARENA_STATS=1` | infra, 15 tests |
| P2a | Zero-copy lexer: `Token.text: String → Cow<'a, str>`, trivia borrows source, alloc-free `advance()` (was: full Token+trivia clone per consumed token) | 305ms → 276ms debug (~10%) |
| P2b | `has_any_decorators()` skip in `check_program_impl` kills the wasted no-op re-expand clone (HIR already expanded once) | 276ms → 270ms debug |
| P3a | `build_program` moves (not clones) the 4 result maps (`funcs` = one entry per function) | release 68ms → 53ms (~22%, with P3b) |
| P3b | `SpanKey.func: String → Arc<str>`, scope stack `Vec<Arc<str>>` — one heap alloc killed per checked expression | release 68ms → 53ms (~22%, with P3a) |
| P4 | `Op::CallPath.joined` precomputed at compile time — no lookup-string alloc per dotted call | directionally certain, ~zero on current benches |
| P5 | LSP: no code change — daemon reuses the same parse/check pipeline, inherits all wins; owned `Program` per doc retained (bounded RSS) | 168 lsp tests green |

Scoreboard: debug 305ms → 270ms (~11%); release large.zz 68ms → 53ms (~22%), many_funcs 49ms → 39ms (~20%).

## Deliberate non-goals (measured rejections)

- **Full `&'a Expr` arena AST**: runtime spawn snapshots + LSP need owned
  trees; copies at every boundary go net-negative.
- **Full `&'a Type` interning**: `unify.rs` is already zero-alloc
  (journaled rollback, borrow-only traversal); boundary copies would cost.
- **VM scratch-args threading** (attempted, reverted): Amdahl-capped at
  ~9% on fib(30) while adding TLS+RefCell traffic to the hottest path —
  unprovable on a loaded box, so kept only the certain `joined` win.
  Re-land from git history if a quiet box ever shows +5%.
- **`--no-arena` rollback flag**: inapplicable — nothing behavioral was
  added (fewer allocs, same values). Rollback = revert the commits.

## `zz_arena` production notes

- **Caps/DoS**: `Config { initial_block, max_block, max_bytes, max_blocks,
  retain_blocks }`. Breaches return `ArenaError` (never abort); `alloc`
  panics with the message only on cap breach, `try_alloc` for untrusted
  paths. 10MB-input + 10k-nesting fuzz recommended before untrusted use.
- **Determinism**: `Interner` ids follow intern order (single map, no
  sharding) — identical sources agree, build-cache keys stable.
- **Concurrency**: `Bump: !Send + !Sync` (Rc marker); `FrozenBump`
  (`Arc<[u8]>` chunks) is `Send + Sync` for cross-thread handoff.
  `SpanKey.func` is `Arc<str>` (not `Rc`) because `TypedProgram` lives in
  a `static OnceLock` and the runtime shares the types map across spawns.
- **Memory discipline**: `reset()` retains the largest N blocks
  (default 2, no syscall when the next unit fits); `shrink()` on
  daemon/LSP idle. `Drop` types placed in the arena are leaked by design
  — only trivial/`Copy` data and arena strings belong here.
- **Observability**: `Bump::stats()` (`bytes_used`, `capacity_bytes`,
  `blocks_alive`, `spills_alive`, `grows`, `resets`, `waste_ratio`);
  `ZZ_ARENA_STATS=1` logs per phase.
- **Unsafe surface**: confined to `Bump::try_alloc_raw` (+ typed wrappers);
  blocks are heap-stable `Box<[u8]>`, offsets always aligned up, refs tied
  to the arena lifetime.

## Open gates (toolchain-limited here)

- `cargo miri test -p zz_arena` (needs nightly; stable toolchain lacks it).
- `cargo fuzz` 1h on `Bump` + `lex` (needs nightly + harness).
- loom test for freeze/share path (needs `loom` dev-dep).
- CI bench gate: fail on >10% `bench-compiler.sh` regression
  (repo CI is currently TODO per AGENTS.md).

## Follow-up: arena-next (in `dev` via PR #200, #201)

- Q0 multi-file corpus (`bench/compiler/proj`, `single_4k`,
  `heavyproj`) + bench rows.
- Q1 lexer/parser pre-sizing (`block_depth`-gated; mechanism-gated).
- Q2 mimalloc A/B rejected by RSS gate (+50% RSS; zero code kept).
- Q3 loader seeds move into checker (proj 187ms → 147ms, −21%).
- Q4a entry-point seed moves (`zz run` proj 395ms → 320ms).
- Q5 interning parked (trigger never fired).
- S3 clean-string borrows in lexer (debug self-verified).
- S1 dep-aware check cache (`zz check` only): warm proj −16%,
  heavyproj −35%, touch-1-leaf == warm (precision-tested).
- S2 shared signature maps de-scoped (transient-only duplication).
