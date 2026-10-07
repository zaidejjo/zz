# zz v0.1.6 — draft release notes (DO NOT TAG yet)

## Highlights

- **Native memory-safety fix: loop-arena string retention.** String
  results built in the per-iteration loop arena (`zz_binop_cat_arena`,
  `zz_str_cast_arena`) are bump-allocated with a `refs==0` sentinel.
  Retaining stores (variable assignment, array/dict/object slots,
  `push`/`insert`, tuple elements, dict keys) aliased them past
  `zz_arena_reset`, so loop-carried accumulators read back NUL garbage —
  and dict keys got `refs++` adopted as heap-owned (wild free later;
  observed as SIGSEGV). Every retaining store now heals arena strings
  to heap copies (`zz_str_heal_arena`, mirroring the `zz_value_dup`
  string path used at thread crossings); append fallbacks explicitly
  abandon arena sources. Found via the toml package's `stringify`;
  verified by 1000/1000 differential fuzz cases plus an AddressSanitizer
  campaign (fixtures + fuzz, zero errors).
- **Native index-set fix.** `a[i] = s + t` (string concat RHS) was
  wrapped in `zz_int(...)` — a C type error — by an AST-only scalar
  check that misreads string idents. The wrap now fires only for
  genuinely raw scalars.
- **`-9223372036854775808` parses.** Unary minus around the exact
  boundary literal folds to `i64::MIN`; one-past-MIN still errors.
- **New `[package] zz = ">=x.y.z"` manifest field** (opt-in, no
  auto-stamp). `run`/`build`/`test` fail fast on the project's own
  requirement; `install` errors on root + dependency requirements
  (tool installs included); `add` warns for readable path deps;
  `publish` rejects malformed requirements. Note: parsers ≤ 0.1.5
  *ignore* unknown keys, so old compilers cannot be protected by it —
  enforcement guards new compilers only.

## New regression coverage

- `string_accum_loop`, `string_store_across_iter`,
  `string_index_set_binop`, `branch_early_return_calls` (e2e + strict
  VM-vs-native parity).
- Fuzzer `--shapes v2`: return-of-call branches, string accumulators
  with retaining stores, stringify-like struct emits (these shapes
  found all three native bugs above).

## Known limitations (unchanged)

- VM value copies are deep per read (`Env::get`, `DefineVar`,
  `LoadSlot`): config-parse workloads scale ~quadratically on the VM
  (10 KB TOML ~43 s). Native is unaffected (handle passing). Design doc
  `docs/design/vm-cow.md` proposes COW + last-use move (8–10 d).
- Tuple-literal boxing, scalar-global double-unbox (fuzz
  `known_failures.txt` classes 1–2).
- `float()` VM/AOT divergence (pre-existing).
- Imported-const selective bindings print empty on native
  (`multi_selective`); native base64/isqrt error-message text differs
  from the VM (same class: different backing libs).
- Closures cannot capture loop locals (VM errors; native yields unit) —
  consistent limitation, not memory unsafety.
- `defer` of loop-locals is a loud checker error (by design).

## Merge order (with dev commit ae9b86d)

`ae9b86d` ("native: yield tail call/return values from if/else
branches") is already ON dev and ANCESTOR of this work — nothing to
cherry-pick for it. This branch (`fix/toml-pkg-followups`,
ae9b86d^..HEAD) merges to dev as-is:

1. `350499e` runtime heal — needs the precompiled `libzz_rt.a` cache
   rebuilt on first native build after upgrade (automatic; mtime-keyed).
2. `0d334de` fixtures + harness entries.
3. `c4778d5` fuzz v2 (default seeds byte-identical — CI smoke safe).
4. `0150f93` MIN fold (parser-only, no codegen impact).
5. `a79dd80` index-set fix (codegen; rebuilds native cache entries).
6. `3dd6321` README (docs only).
7. `180a4d1` manifest field (opt-in; old manifests unaffected).
8. `dc8102b` design doc (docs only).

No cherry-picks needed: linear history from ae9b86d. Do NOT tag
v0.1.6 until the full e2e + parity suites pass on the merge commit
(they pass here: 225/225 e2e, 103/103 parity).
