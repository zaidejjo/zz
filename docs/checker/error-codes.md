# ZZ Error Code Reference

ZZ uses stable error codes (`Z0001`, `Z0002`, ...) on every diagnostic.
Codes appear in:

- `zz check` text output: `error: Z0001: undefined variable ...`
- `zz check --format json`: `"code": "Z0001"` on each diagnostic
- `zz explain Z0001`: detailed description with bad/fixed examples

## Code index

| Code | Title |
|------|-------|
| Z0001 | undefined variable |
| Z0002 | type mismatch |
| Z0003 | generic bound violation |
| Z0004 | non-exhaustive match |
| Z0005 | unreachable match arm |
| Z0006 | missing return |
| Z0007 | use before initialization |
| Z0008 | index out of bounds |
| Z0009 | division by zero |
| Z0010 | unknown field |

## Examples

```sh
# Explain a specific code
zz explain Z0001

# List all codes
zz explain
```

## Adding a new code

1. Add an entry to `ERROR_CATALOG` in `crates/zz_diag_core/src/lib.rs` with a unique code number.
2. Add `.with_code(ErrorCode(N))` to the diagnostic emission site(s) in the checker.
3. Run `zz explain ZNNNN` to verify the entry renders correctly.
4. The test `error_codes_have_explanations` in `crates/zz_cli/tests/e2e.rs` verifies every catalog entry has a description, bad example, and fixed example.
