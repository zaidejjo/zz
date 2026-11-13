# Issue: C runtime JSON float formatting uses `%.15g` instead of canonical shortest round-trip

## Location
`crates/zz_codegen_runtime/src/runtime/json.c` — `json_serialize` ZZ_FLOAT arm (json.c:302–308) and `json_pretty_print` ZZ_FLOAT arm (json.c:633–642)

## Problem
JSON float serialization in the C twin formats floats with `snprintf("%.15g", ...)`:
```c
} else {
    char buf[64]; int n = snprintf(buf, sizeof buf, "%.15g", v.f); sb_str(sb, buf, (size_t)n);
}
```
The VM (Rust core) uses the canonical shortest decimal string that round-trips (Rust `Display` / `zz_float_format_raw` from `libzz_native_rt.a`). `%.15g` does not round-trip all f64 values and does not match the VM's spelling, so `std.json.stringify` / `json.pretty` / HTTP bodies can differ between VM and native engines for floats.

## Impact
VM/native parity gap: any program that serializes floats to JSON gets different text on the two engines. Affects `json.stringify`, `json.pretty`, `json.deep_get` round-trips, and HTTP JSON bodies built via the C runtime.

## Suggested fix
Route JSON float formatting through the same Rust-core canonical formatter used by `zz_print_double`/`zz_append_double` (`zz_float_format_raw`, gated by `ZZ_LINK_FLOAT`), instead of `%.15g`. Fall back gracefully when `ZZ_LINK_FLOAT` is 0 (programs that can never hold a float need not link the core).

## References
Found during C-runtime audit (fix/c-runtime-audit). Related: `zz_canonical_double` (strings.c), `ZZ_LINK_FLOAT` build flag, `libzz_native_rt.a` float link profile.
