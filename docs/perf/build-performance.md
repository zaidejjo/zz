# AOT build performance — plan and status

Goal: make `zz build --release` compile like Rust/Go/Zig (seconds, not
minutes) while keeping the runtime performance of the generated binary.

Measured baseline (this branch, `fib(30)`, 3.8 GiB host):

| Stage | Before | After link-gate fix |
| --- | --- | --- |
| frontend + type-check + DCE | ~0.1 s | ~0.1 s |
| lower to C | < 0.05 s | < 0.05 s |
| clang `-O3` (one unit) | ~1 s | ~1 s |
| link | ~1 s | ~1 s |
| `cargo build -p zz_native_rt --release` | **~170 s** | 0 (skipped) |
| runtime of `fib(30)` | ~9.5 s | **0.015 s** |

The 170 s spike came from a link-gate false positive: the float-formatter
scan looked at the *merged* program (`stdlib` included), so every program
appeared float-typed and pulled the Rust staticlib. Fixed in
`fix(lower): precise float-format link gate`.

## What already exists (upstream PR #745)

- **Per-module translation units** — `LoweredUnits` partitions lowering by
  module namespace; `compile_units_with_sink` compiles each unit to its own
  object and links them. This is already the default release path; the
  single-TU path is opt-out (`ZZ_CODEGEN_UNITS=0`).
- **Content-addressed object cache** — `~/.zz/cache/objects/units/<slug>`
  keyed by runtime base, option fingerprint, compiler build stamp, shared
  header, and unit code. Unchanged units skip clang entirely.
- **Memory-aware parallel clang** — unit jobs capped by cores and a
  per-job memory budget, overridable with `ZZ_CODEGEN_JOBS`.

So steps 1 and 2 of the original sketch are not greenfield work. What is
missing is *precision*: the cache key includes the whole-program header,
so any user edit invalidates every stdlib object too.

## Step 1 — multi-TU default: verified

Status: **done (no code change needed).**

- `zz_build` already routes release builds through
  `build_native_units_with_sink`; `ZZ_CODEGEN_UNITS=0` is the only way
  back to a single TU.
- Verified end to end on this branch: a two-module program importing
  `std.str` (struct + closure + stdlib) prints identically through the
  unit path and through `ZZ_CODEGEN_UNITS=0`, and so do the
  regexp/cell/clone/json native fixtures.
- Per-unit cache hits are already reported through `CodegenSink`; step 3
  makes them visible in `zz doctor`.

## Step 2 — per-unit headers (dependency graph for declarations)

Status: **implemented.**

Today every unit is prefixed with the *entire* header:

```
runtime decls + ffi + every struct + every global extern
+ every function prototype + every closure + every init prototype
```

`unit_obj_slug` hashes that header, so adding one function invalidates
every cached object, including pure-ZZ stdlib modules the edit cannot
possibly affect.

Plan: partition the header into addressable sections and give each unit
only what it can reference.

- **Base section** (stable): runtime decls, process-global arena externs,
  plugin externs, all struct definitions + debug functions.
- **Stdlib section** (stable per compiler build): prototypes, closure
  declarations, and `zz_init_*` declarations for stdlib namespaces.
- **Program section** (changes with user code): user prototypes, user
  globals, user closures, entry init driver, reachable FFI prelude.

Classification of a namespace as stdlib uses the definitions in
`zz_stdlib_programs()` (the embedded pure-ZZ modules), not a name prefix:
a namespace that also contains user functions is treated as program code
(conservative). Stdlib units receive base + stdlib; user units receive
base + stdlib + program.

`compile_units` takes a `UnitSource { ns, header, code }` per unit and
`CodeUnit` carries its own header; the cache slug (128-bit, two salted
SipHash passes) is computed from the exact TU that will be compiled.
`LoweredUnits::header` remains the entry unit's superset for dumps.
Missing declarations are compile errors (never silent), and the
e2e/parity suites cover the generic/stdlib surface.

Also fixed while implementing: the struct preamble walked `tp.structs`
in `HashMap` order, so identical sources could emit different bytes (and
therefore different cache keys) between runs. Roots are now sorted.

## Step 3 — compile the stdlib once ("link packages once")

Status: **implemented.**

- `stdlib_unit_is_stable_across_program_edits` lowers two different
  programs and asserts the stdlib unit's header and body are
  byte-identical, so they hash to the same object slug.
- `zz doctor` reports unit objects separately from the rest of the build
  cache, making reuse auditable (`N unit objects … reusable across
  projects`).
- `--verbose` already prints `zz: timing units_cache=<hits>/<total>`; a
  second build of a different program that imports the same stdlib
  modules reports the stdlib unit as a hit.

With step 2, a stdlib unit's TU is a pure function of (compiler build,
flags, toolchain, embedded stdlib source). Two consequences:

- The first build in any project compiles the stdlib units once and
  publishes the objects under content-addressed slugs.
- Every later build — in this project or any other, on the same machine —
  reuses those objects without re-running clang, even after user code
  changes.

This is Go's "compiled package" model without a separate archive format:
the object cache is the package store, deduplicated by content. We will:

- Cache statistics in `zz doctor` and `--verbose` (done above).
- Regression test at the lowering layer (done above). Driving a second
  clang build in a unit test would need a private `ZZ_HOME` and a full
  link; the byte-identity test is the actual invariant that guarantees
  the hit.

## Step 4 — cold start: cache the Rust native runtime archive

Status: **implemented.**

Programs that genuinely need regex/crypto/TLS/sql must link
`libzz_native_rt.a`. In a source checkout the archive is built in
`target/<profile>`; a second checkout or a cleaned tree used to rebuild
it (minutes). Release installs already ship a prebuilt archive.

The archive is now content-addressed in the user cache:

```
~/.zz/cache/native-rt/v1-<128-bit>/libzz_native_rt.a
```

The key covers every byte under `crates/zz_native_rt`, the workspace
`Cargo.toml` (profile), `Cargo.lock` (dependency graph), `.cargo/config*`
and `RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS`, plus the profile. A source
change therefore produces a different key (never a stale link), and the
same sources on the same machine hit without invoking cargo.

Publishing is a temp copy followed by one atomic `rename`; entries are
validated by the `ar` magic (`!<arch>\n`) before reuse, so a truncated or
corrupted file is rebuilt instead of linked. Superseded keys are pruned
after a 24 h grace window, keeping the newest three. `zz doctor` reports
the archive as `cached (content-addressed cache)`, and `zz cache clean`
removes it with the rest of the build cache.

Measured: with `CARGO_TARGET_DIR` pointed at an empty directory (no
`target/<profile>` archive at all), a multi-module native build links in
~1.7 s and creates no cargo target tree — the cache hit supplies the
archive.

## Non-goals

- No whole-program LTO by default. Default release is `-O3` (matching
  Rust/Go/Zig); `--full` keeps full LTO.
- No change to generated-code semantics or runtime performance.

## Step 5 — external package unit reuse ("compile once, link everywhere")

Status: **implemented.**

External ZZ packages (`zz_pm` registry/git/path deps outside the project
root) are now classified as `"pkg"` (not program) in the loader's
`module_origins` map, threaded through `TypedProgram` into the per-unit
Lowerer. Unit headers are split into:

- **Base section** (stable): runtime, FFI, structs, debug fns, arena externs.
- **Stdlib section** (stable per compiler build): embedded pure-ZZ stdlib prototypes.
- **Dependency section** (stable per package set): external package
  prototypes + package-owned globals. Shared by every unit; changes only
  when the package set changes.
- **Program section** (changes with user code): user prototypes, user
  globals, entry init driver. Only project-owned namespaces.

Package units receive base + stdlib + dependency (+ own closures), never
the project section. Project units receive all four sections.

Consequences:

- Editing project code never invalidates a package unit's object — its TU
  is byte-identical across unrelated project edits.
- Two projects importing the same package set produce byte-identical
  package units, so their objects are shared in the content-addressed
  cache (`~/.zz/cache/objects/units/<slug>/`).
- Adding/removing a package grows the dependency section → all units
  rebuild once (one-time cost), then steady state resumes.

Classification is conservative: any namespace without an explicit `"pkg"`
tag is treated as project, so no unit is wrongly denied its prototypes.
Stdlib classification still uses `stdlib_func_names()` (compiled stdlib
ASTs), not name prefixes, so a namespace that also contains user functions
is treated as program code.

Regression test: `pkg_unit_is_reusable_across_projects` in
`zz_lower::units::tests` lowers two programs sharing one package and
asserts byte-identical package unit headers and bodies while project
bodies differ.

## Validation

- `cargo test -p zz_lower -p zz_codegen -p zz_codegen_cc`
- `cargo clippy --all-targets -- -D warnings`
- `./scripts/test-fast.sh`
- `ZZ_PARITY_VM_ONLY=1 cargo test -p zz_cli --test dual_engine_parity`
- Manual: cold `zz build --release` of `fib`, then a body edit and a
  second build; assert the stdlib units stay cached and runtime stays
  0.015 s.
- CLI e2e: `build_release_reuses_stdlib_units_on_project_edit` in
  `crates/zz_cli/tests/native_build.rs` builds a package-importing project
  twice (project-only edit between builds) and asserts stdlib units are
  cache hits on the second build.
