# Math — `std.math`

Numerics: rounding, roots/powers, trig, predicates, number theory, statistics, linear algebra, constants. Pure-ZZ integer implementations plus native float overloads. Bit-identical on VM and AOT (`-fwrapv` + no `-ffast-math`).

```zz
import std.math
```

## Purpose

`std.math` is the numeric workhorse. Constants (`PI`, `E`, `TAU`, …) are true `Float` values — the checker resolves them without `()`; calling one is an error. Functions are generic over int|float where applicable.

## Quickstart

```zz
import std.math

math.abs(-5)          // 5
math.floor(3.7)       // 3
math.ceil(3.2)        // 4
math.sqrt(9.0)        // 3.0
math.pow(2, 10)       // 1024.0
x := math.random()    // 0.0 <= x < 1.0
math.PI               // 3.1415926... (constant, no parens)
```

## Exhaustive reference

### Rounding

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.abs` | `math.abs<T>(v: T) -> T` | Absolute value |
| `math.floor` | `math.floor(v: float) -> int` | Floor to int |
| `math.ceil` | `math.ceil(v: float) -> int` | Ceil to int |
| `math.round` | `math.round<T>(x: T) -> float` | Round |
| `math.trunc` | `math.trunc<T>(x: T) -> float` | Truncate |
| `math.clamp` | `math.clamp(val, min, max: T) -> float` | Clamp to range |
| `math.signum` | `math.signum<T>(x: T) -> T` | Sign (-1/0/1) |
| `math.hypot` | `math.hypot<T>(x, y: T) -> float` | Hypotenuse |
| `math.is_nan` | `math.is_nan<T>(x: T) -> bool` | NaN check |
| `math.is_inf` | `math.is_inf<T>(x: T) -> bool` | Infinity check |

### Roots / powers / exp/log

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.sqrt` | `math.sqrt<T>(v: T) -> float` | Square root |
| `math.pow` | `math.pow<T>(base, exp: T) -> float` | Power |
| `math.root` | `math.root<T>(x, n: T) -> float` | n-th root |
| `math.isqrt` | `math.isqrt(n: int) -> Result<int, str>` | Integer sqrt |
| `math.exp` | `math.exp<T>(x: T) -> float` | e^x |
| `math.log` | `math.log<T>(x: T) -> float` | Natural log |
| `math.log10` | `math.log10<T>(x: T) -> float` | Base-10 log |

### Trigonometry

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.sin` | `math.sin<T>(x: T) -> float` | Sine (radians) |
| `math.cos` | `math.cos<T>(x: T) -> float` | Cosine |
| `math.tan` | `math.tan<T>(x: T) -> float` | Tangent |
| `math.asin` | `math.asin<T>(x: T) -> float` | Arcsine |
| `math.acos` | `math.acos<T>(x: T) -> float` | Arccosine |
| `math.atan` | `math.atan<T>(x: T) -> float` | Arctangent |
| `math.sin_deg` | `math.sin_deg<T>(x: T) -> float` | Sine (degrees) |
| `math.cos_deg` | `math.cos_deg<T>(x: T) -> float` | Cosine (degrees) |
| `math.tan_deg` | `math.tan_deg<T>(x: T) -> float` | Tangent (degrees) |
| `math.to_radians` | `math.to_radians<T>(deg: T) -> float` | Deg → rad |
| `math.to_degrees` | `math.to_degrees<T>(rad: T) -> float` | Rad → deg |

### Number theory

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.factorial` | `math.factorial(n: int) -> Result<int, str>` | Factorial |
| `math.gcd` | `math.gcd(a, b: int) -> int` | GCD |
| `math.lcm` | `math.lcm(a, b: int) -> int` | LCM |
| `math.is_even` | `math.is_even(n: int) -> bool` | Even check |
| `math.is_odd` | `math.is_odd(n: int) -> bool` | Odd check |
| `math.min` | `math.min<T>(a, b: T) -> T` | Min |
| `math.max` | `math.max<T>(a, b: T) -> T` | Max |

### Aggregates (pure-ZZ)

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.sum` | `math.sum(arr: [int]) -> int` | Sum ints |
| `math.product` | `math.product(arr: [int]) -> int` | Product ints |
| `math.count` | `math.count(arr: [int], target: int) -> int` | Occurrences |
| `math.sum_f` | `math.sum_f(arr: [float]) -> float` | Kahan float sum |
| `math.product_f` | `math.product_f(arr: [float]) -> float` | Float product |
| `math.min_arr` | `math.min_arr<T>(arr: [T]) -> Option<T>` | Min array |
| `math.max_arr` | `math.max_arr<T>(arr: [T]) -> Option<T>` | Max array |
| `math.mean` | `math.mean(list: [float]) -> Result<float, str>` | Mean |
| `math.median` | `math.median(list: [float]) -> Result<float, str>` | Median |
| `math.mean_f` | `math.mean_f(arr: [float]) -> Result<float, str>` | Mean |
| `math.median_f` | `math.median_f(arr: [float]) -> Result<float, str>` | Median |
| `math.rand_range` | `math.rand_range<T>(min, max: T) -> float` | Random in range |

### Linear algebra

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.dot_product` | `math.dot_product(v1, v2: [float]) -> Result<float, str>` | Dot product |
| `math.magnitude` | `math.magnitude(v: [float]) -> float` | Vector magnitude |
| `math.matrix_mul` | `math.matrix_mul(m1, m2: [[float]]) -> Result<[[float]], str>` | Matrix multiply |

### Random

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.random` | `math.random() -> float` | Random [0, 1) |

### Constants (true `Float` values, no `()`)

| Constant | Description |
|----------|-------------|
| `math.PI` | π |
| `math.E` | e |
| `math.TAU` | τ |
| `math.SQRT_2` | √2 |
| `math.SQRT_1_2` | 1/√2 |
| `math.LN_2` | ln 2 |
| `math.LN_10` | ln 10 |
| `math.LOG10_E` | log₁₀ e |
| `math.LOG2_E` | log₂ e |
| `math.INF` | Infinity |
| `math.NAN` | NaN |

## Gotchas

- **`isqrt`/`factorial` return `Result<int, str>`** — error on negative input.
- **`mean`/`median` return `Result<float, str>`** — error on empty input.
- **`min`/`max`** work for int, float, str (anything comparable).
- **`rand_range`** is generic over `T` returning float.
- **Constants** — calling `math.PI()` is an error; use `math.PI` directly.

## Performance notes

- Bit-identical on VM and AOT (`-fwrapv` + no `-ffast-math`).
- Kahan-compensated `sum_f`/`mean_f` for float precision.
- Pure-ZZ integer implementations shadow natives at type-check time.

