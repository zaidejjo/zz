# Data helpers — `std.map`, `std.set`, `std.dec`, `std.json`, `std.csv`, `std.property`

## `std.map` — pure dict helpers

Dicts are `{K: V}`, insertion-ordered, O(n) lookup. API is stable so a hashed
impl can drop in later.

```zz
import std.map

d := {"a": 1, "b": 2}
map.get_or(d, "z", 99)   // 99, no error
map.has(d, "a")          // true
map.keys(d)              // ["a", "b"]
map.values(d)            // [1, 2]
map.merge(d, {"c": 3})
map.remove(d, "a")
```

`get_str`, `keys_str`, `values_str`, `merge_str` are the `{str: str}` variants.
`len` / `is_empty` are size predicates.

## `std.set` — arrays with uniqueness

```zz
import std.set

set.insert(["a"], "b")                 // ["a", "b"]
set.union(["a"], ["b"])                // ["a", "b"]
set.intersect(["a", "b"], ["b", "c"])  // ["b"]
```

`*_int` variants for `[int]`. `has`, `remove`, `diff`, `len`, `is_empty`.

## `std.dec` — exact decimals as strings

`[-]digits[.digits]`, scale capped at 18. No float drift: `add("0.1","0.2")=="0.3"`.

```zz
import std.dec

dec.add("0.1", "0.2")     // "0.3"
dec.mul("1.5", "2")       // "3"
dec.format("3.14159", 2)  // "3.14"
dec.cmp("1.5", "2")       // -1
```

`is_valid`, `sub`, `eq/lt/gt`, `scale_of`, `pow10`, `to_scaled`, `from_scaled`, `trim_zeros`.

## `std.json`

```zz
import std.json

func main() -> Result<int, str> {
    data := json.parse(`{"name": "Ada", "age": 36}`)?
    println(json.as_str(json.get(data, "name")?))
    println(json.as_int(json.get(data, "age")?))
    println(json.stringify({"name": "Bo", "age": 12})?)
    return .ok(0)
}
```

`json.get`, `as_str/as_int/as_float/as_bool`. Buffers (`bytes`) serialize as int arrays.

## `std.csv` — RFC4180 subset

Quoted fields, `""` escapes, embedded newlines, CRLF. Rows are `[[str]]`.

```zz
import std.csv

rows := csv.parse("a,b\n1,2\n")
csv.header(rows)              // ["a", "b"]
csv.records(rows)             // after first row
csv.get_cell(rows, 1, 0, "?") // "1"
csv.stringify(rows)
csv.to_json(rows)             // header-keyed JSON array
```

`parse_delim` / `stringify_delim` for one-char delimiters, `len`, `validate`.
TOML is not in stdlib — use the external `toml` package (pure ZZ).

## `std.property` — seeded generators + shrinking

Pure ZZ, no engine support. `(seed, index)` pairs replay exactly.

```zz
import std.property

func gen_small(seed: int, i: int) -> int {
    return property.int_in(seed, i, 0, 10)
}

func small(x: int) -> bool { return x < 100 }

property.check(gen_small, property.shrink_int, small, 200, 7)
property.for_all_int(0, 10, small, 200, 7)
```

`seed`, `int_in`, `pick`, `boolean`, `alpha_char`, `alpha_str`,
`shrink_int`, `shrink_str`, `shrink_value`, `check`, `for_all_int`.
Custom generators are `func(seed: int, i: int) -> T`; shrinkers `func(T) -> [T]`.
