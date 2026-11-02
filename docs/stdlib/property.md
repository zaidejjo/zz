# Property — `std.property`

Seeded generators + shrinking for property-based tests. Pure ZZ — no engine support needed. Values derive from `(seed, index)` pairs, so a failing seed replays the exact same cases. `check`/`for_all_int` abort with the seed and the shrunk minimal case.

```zz
import std.property
```

## Purpose

`std.property` lets you write property-based tests: generators produce values from `(seed, index)` pairs, shrinkers reduce failing cases, and `check` asserts a property over generated inputs. Because values derive from `(seed, index)`, a failing seed deterministically replays — ideal for CI flake-hunting.

## Conceptual model

- **Generator:** `func(seed: int, i: int) -> T` — produces the i-th value from a seed.
- **Shrinker:** `func(T) -> [T]` — produces simpler candidates from a failing value.
- **`check(gen, shrink, prop, iters, seed)`:** runs `iters` iterations; on failure, shrinks and reports the minimal case + seed.
- **`for_all_int(lo, hi, prop, iters, seed)`:** convenience for integer ranges.

Named functions and `|x| ...` closures both work as arguments.

## Quickstart

```zz
import std.property

func gen_small(seed: int, i: int) -> int {
    return property.int_in(seed, i, 0, 10)
}

func small(x: int) -> bool {
    return x < 100
}

property.check(gen_small, property.shrink_int, small, 200, 7)
property.for_all_int(0, 10, small, 200, 7)
```

## Exhaustive reference

### Generators

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `property.seed` | `property.seed(n: int) -> int` | Normalize any int into LCG state | |
| `property.int_in` | `property.int_in(seed, i, lo, hi) -> int` | Uniform int in `[lo,hi]` (swaps reversed) | |
| `property.pick` | `property.pick(seed, i, n) -> int` | Uniform int in `[0,n)` (`0` when `n<=0`) | |
| `property.boolean` | `property.boolean(seed, i) -> bool` | Uniform bool | |
| `property.alpha_char` | `property.alpha_char(seed, i) -> str` | Lowercase letter | |
| `property.alpha_str` | `property.alpha_str(seed, i, len) -> str` | Lowercase string of `len` chars | |

### Shrinkers

| Function | Signature | Description |
|----------|-----------|-------------|
| `property.shrink_int` | `property.shrink_int(v: int) -> [int]` | Simpler ints toward zero |
| `property.shrink_str` | `property.shrink_str(s: str) -> [str]` | Shorter prefixes |

### Combinators

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `property.shrink_value` | `property.shrink_value(prop, shrink, v) -> T` | Minimize a falsifying value | |
| `property.check` | `property.check(gen, shrink, prop, iters, seed) -> unit` | Assert `prop` over generated cases | `property.check(gen, shrink_int, p, 200, 7)` |
| `property.for_all_int` | `property.for_all_int(lo, hi, prop, iters, seed) -> unit` | `check` for int ranges | `property.for_all_int(0,10,p,200,7)` |

## Edge cases & gotchas

- **Deterministic replay.** Record the seed from a failure to reproduce it exactly.
- **`check` aborts on first failure** with the shrunk minimal case.
- **`int_in` swaps reversed bounds** — `int_in(s,i,10,0)` == `int_in(s,i,0,10)`.
- **`pick` returns `0` when `n <= 0`.**
- Custom generators/shrinkers are plain functions — no trait/object system needed.

## Production patterns

```zz
import std.property

@test
func test_roundtrip() {
    gen := |seed: int, i: int| -> str { return property.alpha_str(seed, i, 5) }
    prop := |s: str| -> bool { return len(s) == 5 }
    property.check(gen, property.shrink_str, prop, 500, 42)
}
```

