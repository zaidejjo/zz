# Test Isolation & Timeout — Design Decisions

Production determinants for `zz test`. Resolved before P1; P2-P5 build on these.

## 1. Panic Isolation — In-Process (v1)

**Design:** Per-test fresh `Interp` + fresh `Env` (no state bleed). Wrap the test call in `std::panic::catch_unwind(AssertUnwindSafe)`. Collect `EvalError` or Rust panic payload, run `defer` stacks and `@teardown` first, continue suite, render full report.

**Why not subprocess v1:** 10–50× slower, breaks seeded-shuffle determinism, complicates output capture. In-process keeps `cargo test`-like speed.

**Honest limitation (documented):** A true Rust segfault, stack overflow, or hung native loop (e.g. blocking `std.net`/`std.http` call) still kills the whole runner process — `catch_unwind` cannot recover those. Mitigation is the reserved v2 flag `--isolate` (per-test child process `zz test --run-one`, kill on crash, collect report over pipe). v1 documents this gap; CI-hardening should use `--isolate` when available, or run `zz test` under a supervisor that re-invokes the remaining shards.

**Guaranteed cleanup:** Both VM (`vm/runtime.rs` defer stack) and tree-walker defer stacks drain even when the test returns `EvalError` or unwinds. `@teardown` is invoked in the same `catch_unwind` scope so teardown failures are reported as test errors, not lost. A teardown panic is caught separately and attached as a secondary note under the primary result.

## 2. Timeout — Cooperative Budget (v1)

**Design:** VM main loop (`vm/runtime.rs`) + tree-walker per-call counter checks `Instant::now()` every N ops (default 1024) against the test's deadline (`@test(timeout = ms)` or `[test] timeout` or CLI flag). Exceeding the budget aborts with `EvalError::timeout`, drains defers/teardown, marks `FAIL (timeout)`.

**Why cooperative:** No thread-abandon (leaks threads, nondeterministic resource use), no `kill` (needs subprocess). Deterministic, cheap, reproducible under `--seed`. Every run with the same seed and load exhibits the same timeout outcome for the same ZZ source.

**Honest limitation (documented):** Native Rust functions that block inside the native call (e.g. `std.fs.read_to_string` on a hung NFS mount, `std.net` listener `accept` without timeout, `std.http` handler, `std.process.wait`) cannot be preempted — the VM only regains control between ops. `@test(timeout = 500)` therefore enforces ZZ-code time, not wall time inside a single blocking native. Timeouts on blocking I/O require either (a) natives that expose their own timeout args (already the case for `std.net.set_read_timeout` etc.), or (b) `--isolate` subprocess mode where the runner can `kill` the child after deadline (v2).

**Future:** `--isolate` upgrades timeout to wall-clock `child.kill()` — accurate even for blocking natives, at subprocess cost. The cooperative budget remains the fast default; `--isolate --timeout` composes both.

## 3. Isolation Boundary Beyond `Interp`/`Env`

Fresh `Interp` + `Env` isolates ZZ-level state (bindings, call frames, defer stacks, `Value`s). Native stdlib side effects that are **process-global** and can leak across parallel tests in the same process:

**Audit of `crates/zz_stdlib/src/natives/` and `crates/zz_native_rt/` (2026-09-15):**

| Surface | Current behavior | Leak? | Mitigation / what to watch |
|---------|-----------------|-------|----------------------------|
| `std.env.get_var` / `var` / `args` | Read-only (`std::env::var`, `Interp.args`). No `set_var`/`remove_var` exposed. | **None currently** | If a future `std.env.set_var` is added, it mutates process-global `environ` — must gate with `#[cfg(test)]` global lock or forbid in `zz test` parallel mode (document, or force `--serial`). |
| Process `cwd` | No `std::env::set_current_dir` / `current_dir` mutation exposed. | **None currently** | Adding a `std.fs` cwd-changing native must be guarded: recommend scoped `std.fs.with_dir` instead of global `chdir`; otherwise parallel tests race on cwd. |
| `std.fs` read/write/remove | Direct `std::fs::*` on real filesystem | **Yes — shared FS** | Parallel tests writing the same path race. Guidance: tests must use `std::env.temp_dir` + unique filenames (e.g. `tmp/<test_name>_<pid>`), or the harness should set `ZZ_TEST_TMPDIR` per test. Framework does not virtualize FS. |
| `std.process` run/spawn/wait/exit | Spawns real OS children; `process.exit` kills the process | **Yes — OS global** | `process.exit` in a test kills the runner even with `catch_unwind` (it bypasses unwind). Harness should treat `exit` as immediate `FAIL` and, in v1, warn that `process.exit` is not isolated without `--isolate`. Spawned children outlive the test unless `teardown` kills/waits them. |
| `std.db` `TX_DEPTH` | `static TX_DEPTH: OnceLock<Mutex<HashMap<usize,u32>>>` — global transaction depth per handle | **Yes — global lock** | Keyed by handle address; concurrent tests with distinct handles do not collide on key, but contend on the single `Mutex`. Not a correctness leak today, but a scalability bottleneck; future SDK should make it per-`Interp` or sharded. |
| `zz_native_rt::process` handle table | `static POOL: LazyLock<Mutex<HashMap<u64,Entry>>>` + `NEXT_ID: AtomicU64` — process-global handle registry | **Shared but keyed** | Distinct handles don't alias, but global `Mutex` serializes. |
| `zz_native_rt::log` global level/sink | `static LEVEL: AtomicU8`, `FILE_SINK: LazyLock<Mutex<Option<File>>>` | **Yes — global** | One test's `log.set_level` / file sink races with others. Tests mutating log config should use `--serial` or be marked `tag = "serial"`. |
| `std.http` / `std.net` listeners | Bind real TCP ports | **Yes — port contention** | Parallel tests binding the same port race (`AddrInUse`). Tests must use `port = 0` (ephemeral) or allocate unique ports. Documented. |
| `OnceLock` stdlib cache (`zz_stdlib_programs`) | Immutable after init | **None** | Read-only after `OnceLock`; safe. |
| `Value::Opaque` tags (`process`, `db`, `http`) | Cloned `Arc<Mutex<…>>` per handle | **Per-handle** | Safe when handles not shared across tests; sharing a handle across tests is user error (document). |

**Rule for future natives:** Any new native that mutates process-global state (env, cwd, global statics, file-sink, device) must be (a) listed in this table, (b) evaluated for parallel safety, and (c) either made per-`Interp`, gated behind `--serial` detection (harness auto-serializes `tag = "serial"` tests), or deferred to `--isolate`. The cheapest gate is a `#[test(tag="serial")]` convention that forces those tests onto the serial lane even in parallel runs.

## 4. Retry vs. Seeded Determinism — Isolated RNG Stream

**Confirmed:** `@test(retry = N)` retries use an **isolated RNG stream scoped to that test's retries only**. They do **not** consume draws from the shared seeded RNG that orders the suite. Implementation: suite shuffle draws from `StdRng::seed_from_u64(seed)` sequentially once at discovery; each retried test derives a per-test retry RNG as `StdRng::seed_from_u64(hash(seed, test_name, attempt))` for any retry-internal randomness (e.g. jitter) without advancing the global sequence. This keeps `--seed` reproducibility intact: a retried test's extra attempts do not shift results for later tests under the same `--seed`. The report notes `attempt 2/3, retried` and the wall-clock per attempt; `--json` includes `attempts: [{outcome, duration_ms}]`.

Alternative rejected: shared RNG consumption — same `@test(retry)` count changes would reorder the remaining suite on replay, defeating `--seed` ergonomics.

## 5. Related Decisions (reference)

- `--nocapture` forces `--serial` — see `docs/testing.md` flag table.
- Zero-tests-matched exits `0` — see `docs/testing.md` exit codes (CI should parse `--json`/`--junit` or use future `--fail-on-empty`).
- Non-TTY auto-plain output — no spinner/colors when piped/CI.

## 6. Operational Checklist for Contributors

- [ ] New `natives/` with global state → append row above, decide per-Interp vs serial-tag vs isolate-only.
- [ ] New `zz_native_rt` global → wrap in `#[cfg(not(test))]` or make test harness reset it between tests.
- [ ] Adding `std.env.set_var` or `cwd` mutation → force `--serial` or refuse in `zz test` with diagnostic.
- [ ] Benchmark `TX_DEPTH`/`POOL` `Mutex` contention before adding more DB-heavy parallel tests; consider sharding.
