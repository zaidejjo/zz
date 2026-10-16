# VM vs native parity — what is guaranteed

VM (`zz run`) and AOT (`zz build` / `zz run --native` / `zz test --engine native`)
must agree unless this page says otherwise. If agreement is impossible, the checker
errors instead of letting backends diverge.

## Guaranteed identical

- Integers: wrapping `+ - *`, `INT64_MIN / -1` errors, divide-by-zero errors,
  `-fwrapv`, shifts mask `& 63`, arithmetic `>>`.
- Floats: no `-ffast-math`, no `-march=native`, cross uses safe baseline CPU.
- Strings: interpolation, `{{`/`}}`, backticks, format specs, byte-offset search
  matching Rust `str::find` edges, Unicode `trim_span` on both.
- Values: deep-copy reads/binds, CoW unobservable, struct print
  `Type{field: value, ...}` on both, `bytes` JSON as int arrays.
- Enums/aliases/generics: erase identically (same opcodes / same C, verified by test).
- Errors: `fs:<op>:<code>: <path>` diagnostics, `Result`/`Option` shapes,
  `regexp`/`crypto`/`process` twins via `zz_native_rt`.
- Concurrency: snapshot isolation + unbounded channels on both
  (`zz_value_dup`, thread-local arenas, thread-safe refcounts/intern).

## Test the guarantee

- `parity_strict!(...)` per fixture in `dual_engine_parity.rs`.
- Sweep `parity_discover_all_fixtures` fails on any unexpected result.
- Genuine gaps file an issue and list in `known_native_failures()`; the sweep
  shouts `FIXED!` when a listing goes stale — remove it same day.
- Every strict fixture must be batched or `EXCLUDED` with reason
  (`batched_parity.rs` + `batch_lists.rs` inventory test enforces).
- Order-independent: libtest runs parallel — never rely on another test's global;
  re-register idempotently.

## Known divergences (by design)

- `,compile_fail` doctests: VM-only (AOT compiles whole files upfront → ignored there).
- `zz run --watch` is VM-only (native rebuilds are minutes, not ms).
- PGO/profile flows are native-host-only; `--pgo --target <foreign>` rejected.
- `--static` rejected on macOS targets.
- Coverage binary detail differs; semantics do not (see `docs/coverage.md`).

## Adding backend-sensitive work

Follow the five locksteps in [Architecture](../compiler/architecture.md):
`funcs.rs` signature + VM impl + C twin + `native_impl` mapping + fixtures in
`e2e.rs` + `dual_engine_parity.rs` (+ batch or `EXCLUDED`). Contracts for
search/byte natives: byte offsets O(1), backend-identical, Rust `str::find` edges.
