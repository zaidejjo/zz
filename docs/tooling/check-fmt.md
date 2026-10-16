# Check, fix, format

## `zz check`

```bash
zz check
zz check src/main.zz
zz check src/
zz check --stats src/main.zz   # modules, cache hits, seed size, wall time
zz check --format json          # machine diagnostics, exit codes unchanged
```

Flags: `--fix/-f` safe fixes, `--hard` all incl. ambiguous, `--interactive/-i`
prompt for ambiguous. `zz check --check` is rejected — use `zz fmt --check`.

Cold single file is ~40ms; warm is cache-served (`0 checked`). Budget: p50 ≤0.5s
on 10k-line cold, ≤50ms warm per file. Verify with `--stats` twice (second shows `cached`).

## `zz fix`

Shortcut for `zz check --fix`:

```bash
zz fix src/
zz fix --hard src/
```

## `zz fmt`

```bash
zz fmt
zz fmt src/main.zz
zz fmt -c src/    # dry run, exit 1 if changed
zz fmt src/
```

## Diagnostics shape

```
error[E001]: type mismatch
  --> src/main.zz:5:12
   |
 5 |     x: int = "hello"
   |     ----   ^^^^^^^^ expected int, found str
   |
   = help: cast string to int with `int()` function
```

Every pass reports all errors, not just the first. Source context + span +
code + help. Warnings (`W001` unused var) carry `= fix:` lines `zz fix` applies.

File loading: `std.*` from stdlib, `.name` from current dir,
`utils.helper` relative to importer, `<pkg>.path` to `<root>/src/path.zz`
from anywhere (incl. `tests/` → `src/`). Precedence `std` > package > registry >
relative. Circular imports error with a restructure hint (extract shared `c.zz`).
One file under several namespaces composes (first wins canonical); two files
under one namespace stay an error.

Common fixes: see [Error catalog](../guides/errors.md).
