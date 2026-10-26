# Collections

## Arrays

```zz
nums := [1, 2, 3]
names := ["Ada", "Bo"]
empty: [int] = []
```

Homogeneous and dynamically sized. `+` concatenates. See `std.vec` for
`push`, `pop`, `sort`, `reverse`, `join`, `contains`, `insert`, `remove`.

## Dicts

Insertion-ordered, O(n) lookup:

```zz
ages := {"Ada": 30, "Bo": 25}
ages["Cy"] = 41
```

Prefer `std.map.get_or(d, key, default)` over direct index when keys may miss.
`std.map.keys/values/merge/remove` are pure helpers.

## Tuples

Fixed shape, integer-literal index only (checked at compile time):

```zz
t := (7, "seven")
t[0]      // 7
t[-1]     // "seven"
len(t)    // 2

(a, b) := t   // destructure; _ skips
c, d := t     // bare form, same meaning
```

Dynamic `t[i]` is a compile error — destructure instead. Tuples share the
array representation: `t[0] = 99` writes in place on both engines.

## Slicing

```zz
arr := [10, 20, 30, 40]
arr[0]    // 10
arr[-1]   // 40
arr[1:3]  // [20, 30]
arr[:2]   // [10, 20]
arr[2:]   // [30, 40]
s := "hello"
s[1:3]    // "el"
```

## Comprehensions

```zz
squares := [x ** 2 for x in range(0, 6)]
evens := [x for x in range(21) if x % 2 == 0]
```

## Pipelines and higher-order built-ins

```zz
result := range(1, 6)
    |> map(|x| x * 2)
    |> filter(|x| x > 4)
```

`map`, `filter`, `enumerate`, `zip` work bare and as `std.vec.*`.
Prefer `std.vec.*` in new code; bare forms stay until 0.3.

## Ranges

`0..5` is `0,1,2,3,4`. `range(stop)` / `range(start, stop)` / `range(start, stop, step)`.
`len` works on arrays, tuples, bytes, strings, dicts, ranges.

## Directory collections

The value types above are unrelated to `[collect.<name>]` registries, which
turn a directory of `.zz` files into a typed module with `get` / `all`.
See [Collections](../tooling/collections.md).

Next: [Structs and enums](structs-enums.md).
