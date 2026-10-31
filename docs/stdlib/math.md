# `std.math` — mathematics

Rounding, trig, roots/powers, integer aggregates, linear algebra, and
constants. VM and AOT agree bit-for-bit (builds use `-fwrapv`, never
`-ffast-math`).

```zz
import std.math

println(math.abs(-5))        // 5
println(math.sqrt(9.0))      // 3.0
println(math.pow(2, 10))     // 1024.0
println(math.sin(math.PI / 2)) // 1.0
println(math.floor(3.7))     // 3
println(math.gcd(48, 18))    // 6
println(math.factorial(5))   // 120

vec := [1.0, 2.0, 3.0]
println(math.mean(vec))      // 2.0
println(math.magnitude(vec)) // 3.741…
```

## Types

None (operates on `int` / `float` / arrays).

## Variables

| Variable | Type | Value |
|---|---|---|
| `math.E` | `float` | 2.718281828459045 |
| `math.PI` | `float` | 3.141592653589793 |
| `math.TAU` | `float` | 6.283185307179586 |
| `math.LN_2` | `float` | 0.6931471805599453 |
| `math.LN_10` | `float` | 2.302585092994046 |
| `math.LOG2_E` | `float` | 1.4426950408889634 |
| `math.LOG10_E` | `float` | 0.4342944819032518 |
| `math.SQRT_2` | `float` | 1.4142135623730951 |
| `math.SQRT_1_2` | `float` | 0.7071067811865476 |
| `math.INF` | `float` | infinity |
| `math.NAN` | `float` | not-a-number |

## Functions

| Function | Signature | Notes |
|---|---|---|
| `abs(v)` | `abs(v: T) -> T` | Absolute value (generic). |
| `round(x)` | `round(x: T) -> float` | Round to nearest integer. |
| `floor(v)` | `floor(v: float) -> int` | Round down. |
| `ceil(v)` | `ceil(v: float) -> int` | Round up. |
| `trunc(x)` | `trunc(x: T) -> float` | Truncate toward zero. |
| `clamp(val, min, max)` | `clamp(val: T, min: T, max: T) -> float` | Clamp into `[min, max]`. |
| `signum(x)` | `signum(x: T) -> T` | `-1`, `0`, or `1`. |
| `sin` / `cos` / `tan` | `(x: T) -> float` | Radians. |
| `asin` / `acos` / `atan` | `(x: T) -> float` | Inverse, radians. |
| `sin_deg` / `cos_deg` / `tan_deg` | `(x: T) -> float` | Degrees. |
| `to_radians(deg)` / `to_degrees(rad)` | `(x: T) -> float` | Convert angle units. |
| `sqrt(v)` | `sqrt(v: T) -> float` | Square root. |
| `pow(base, exp)` | `pow(base: T, exp: T) -> float` | Power. |
| `root(x, n)` | `root(x: T, n: T) -> float` | `n`-th root. |
| `exp(x)` | `exp(x: T) -> float` | Natural exponential. |
| `log(x)` / `log10(x)` | `(x: T) -> float` | Natural / base-10 logarithm. |
| `hypot(x, y)` | `hypot(x: T, y: T) -> float` | Hypotenuse `sqrt(x²+y²)`. |
| `is_nan(x)` / `is_inf(x)` | `(x: T) -> bool` | NaN / infinity predicates. |
| `is_even(n)` / `is_odd(n)` | `(n: int) -> bool` | Parity predicates. |
| `factorial(n)` | `factorial(n: int) -> Result<int, str>` | `n!`. |
| `gcd(a, b)` / `lcm(a, b)` | `(a: int, b: int) -> int` | Greatest common divisor / least common multiple. |
| `min(a, b)` / `max(a, b)` | `(a: T, b: T) -> T` | Min / max (generic). |
| `min_arr(arr)` / `max_arr(arr)` | `(arr: [T]) -> Option<T>` | Min / max element. |
| `sum(arr)` | `sum(arr: [int]) -> int` | Sum of ints. |
| `sum_f(arr)` | `sum_f(arr: [float]) -> float` | Sum of floats. |
| `product(arr)` | `product(arr: [int]) -> int` | Product of ints. |
| `product_f(arr)` | `product_f(arr: [float]) -> float` | Product of floats. |
| `count(arr, target)` | `count(arr: [int], target: int) -> int` | Occurrences of `target`. |
| `mean(list)` / `mean_f(arr)` | `([float]) -> Result<float, str>` | Arithmetic mean. |
| `median(list)` / `median_f(arr)` | `([float]) -> Result<float, str>` | Median. |
| `dot_product(v1, v2)` | `([float], [float]) -> Result<float, str>` | Dot product. |
| `magnitude(v)` | `([float]) -> float` | Vector magnitude. |
| `matrix_mul(m1, m2)` | `([[float]], [[float]]) -> Result<[[float]], str>` | Matrix multiply. |
| `rand_range(min, max)` | `(min: T, max: T) -> float` | Random float in `[min, max]`. |
| `random()` | `random() -> float` | Random float in `[0, 1)`. |
| `isqrt(n)` | `isqrt(n: int) -> Result<int, str>` | Integer square root. |
