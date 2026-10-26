# `zz cover` — Line + Branch Coverage (VM)

Runs a program exactly like `zz run` (or every test like `zz test`
with `--tests`), records which lines and branch directions execute, and
reports per-file hits. Unattributed code (stdlib, cached bytecode) is
excluded by construction.

```
zz cover [<file.zz>] [--lcov <path>] [--html <path>] [--fail-under <n>] [-- args...]
zz cover --tests [path] [--lcov <path>] [--html <path>] [--fail-under <n>]
```

Without a file, uses the project entry like `zz run`; bare `--tests`
uses the same default as `zz test`.

## Output

```
42
small
Coverage: 83.3% lines (5/6), 50.0% branches (1/2)
  src/main.zz: 83.3% (5/6), branches 50.0% (1/2)
    uncovered: 15
    uncovered branches: 14 (fallthrough)
```

- Program (or test) output prints normally; the report follows.
- Percentages are exact — no timings involved, so output is
  deterministic and CI-assertable.
- `uncovered` lists 1-based lines (ranges compacted: `15-18`).
- `uncovered branches` lists jump sites as `line->taken` (jump never
  taken) or `line (fallthrough)` (straight path never taken). Branch
  lines point at the jump instruction's span, which the compiler may
  attribute to the block rather than the condition — same line region.
- A file with no coverable lines reports `100.0% (0/0)`.

## Flags

| Flag | Meaning |
|------|---------|
| `--tests [path]` | Cover the test suite (merged across tests) instead of a program |
| `--lcov <path>` | Write an LCOV report (`TN`/`SF`/`DA`/`BRDA`/`LF`/`LH`/`BRF`/`BRH`) |
| `--html <path>` | Write a standalone HTML report (green/red gutters) |
| `--fail-under <n>` | Exit 1 when total line coverage is below `n%` (0-100) |
| `-- args...` | Forwarded to the program as script args (program mode only) |

Exit codes: `0` program ok (and gate passed, if set), `1` program
failed or gate tripped. When the program fails, the partial report
still prints first — it shows how far execution got. In `--tests`
mode, failing tests keep their partial coverage, print to stderr, and
flip the exit code.

## Semantics

- **Coverable** = lines spanned by emitted bytecode. Pure declarations
  (`struct`, `type`, unused `import`) emit no code and don't count;
  dead-code-eliminated functions don't count either (they're not in the
  binary). This is correct: coverage measures the executed artifact.
- **Hits** count executions per line (see `DA` records); the headline
  percentage is lines hit ≥ once.
- **Branches** = conditional jumps (`if`/`match` guards, `while`/`for`
  exits), each with jump-taken and fallthrough directions. A site counts
  as covered per direction, so one `if` contributes two units.
- Multi-module programs attribute per defining module: a helper called
  from `main` counts toward its own file. `--tests` merges per-test
  states by (file, line), order-independently.
- Snapshots of truth: `#[cfg(test)]`-style unit tests live in
  `crates/zz_runtime/src/coverage.rs`.

## Limits

- VM only. Native binaries: use `llvm-cov` on the generated C. True AOT
  line coverage needs file labels through IR lowering (the merged chunk
  loses per-module identity today) — designed, not yet built.
- `--tests` runs inline without per-test timeout workers; a hanging test
  hangs coverage. `should_panic` tests count as passed, like the runner.
