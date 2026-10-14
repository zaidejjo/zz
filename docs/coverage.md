# `zz cover` — Line Coverage (VM)

Runs a program exactly like `zz run`, records which lines execute, and
reports per-file hits. Unattributed code (stdlib, cached bytecode) is
excluded by construction.

```
zz cover [<file.zz>] [--lcov <path>] [--fail-under <n>] [-- args...]
```

Without a file, uses the project entry like `zz run`.

## Output

```
42
small
Coverage: 83.3% lines (5/6)
  src/main.zz: 83.3% (5/6)
    uncovered: 15
```

- Program output prints normally; the report follows.
- Percentages are exact — no timings involved, so output is
  deterministic and CI-assertable.
- `uncovered` lists 1-based lines (ranges compacted: `15-18`).
- A file with no coverable lines reports `100.0% (0/0)`.

## Flags

| Flag | Meaning |
|------|---------|
| `--lcov <path>` | Write an LCOV report (`TN`/`SF`/`DA`/`LF`/`LH`) for CI/editors |
| `--fail-under <n>` | Exit 1 when total line coverage is below `n%` (0-100) |
| `-- args...` | Forwarded to the program as script args |

Exit codes: `0` program ok (and gate passed, if set), `1` program
failed or gate tripped. When the program fails, the partial report
still prints first — it shows how far execution got.

## Semantics

- **Coverable** = lines spanned by emitted bytecode. Pure declarations
  (`struct`, `type`, unused `import`) emit no code and don't count;
  dead-code-eliminated functions don't count either (they're not in the
  binary). This is correct: coverage measures the executed artifact.
- **Hits** count executions per line (see `DA` records); the headline
  percentage is lines hit ≥ once.
- Multi-module programs attribute per defining module: a helper called
  from `main` counts toward its own file.
- Snapshots of truth: `#[cfg(test)]`-style unit tests live in
  `crates/zz_runtime/src/coverage.rs`.

## Limits (v1)

- VM only. Native binaries: use `llvm-cov` on the generated C (the
  existing clang driver accepts `--coverage` plumbing as a follow-up).
- Single program runs. Test-suite aggregation (`--tests`, merging runs
  via `CoverageState::merge`) is designed and the merge primitive is
  implemented + unit-tested — wiring it to test discovery is next.
- Branch coverage is not tracked; line hits subsume the common cases.
  `if` conditions themselves count as covered when evaluated.
