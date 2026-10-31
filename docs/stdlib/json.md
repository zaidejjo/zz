# `std.json` — JSON parse, query, and stringify

Parse JSON into a `json` value, navigate it with path expressions, check
types, extract scalars, and serialize back to text.

```zz
import std.json

match json.parse("{\"name\":\"Ada\",\"age\":36}") {
    .ok(j) => {
        println(json.as_str(json.get(j, "name")?)) // "Ada"
        println(json.deep_get(j, "addr.city")?)    // path query
    },
    .err(e) => println(e),
}

out := json.stringify(j)?   // back to text
println(json.pretty(j))     // pretty-printed
```

## Types

| Type | Notes |
|---|---|
| `json` | Opaque JSON value (object/array/string/number/bool/null). Inspect with `json.type` / `json.is_*`, extract with `json.as_*`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `parse(s)` | `parse(s: str) -> Result<json, str>` | Parse JSON text. |
| `parse_or(s, default)` | `parse_or(s: str, default: json) -> json` | Parse, or `default` on failure. |
| `parse_or_null(s)` | `parse_or_null(s: str) -> json` | Parse, or `null` on failure. |
| `validate(s)` | `validate(s: str) -> bool` | Whether `s` is valid JSON. |
| `stringify(v)` | `stringify(v: T) -> Result<str, str>` | Serialize any value to JSON (generic). |
| `pretty(j)` | `pretty(j: json) -> str` | Pretty-printed JSON text. |
| `type(j)` | `type(j: json) -> str` | Type name: `object`/`array`/`string`/`number`/`bool`/`null`. |
| `is_array` / `is_bool` / `is_empty` / `is_null` / `is_number` / `is_object` / `is_string` | `(j: json) -> bool` | Type predicates. |
| `as_bool` / `as_float` / `as_int` / `as_str` | `(j: json) -> bool\|float\|int\|str` | Extract a scalar (errors on wrong type). |
| `get(j, key)` | `get(j: json, key: str) -> Result<json, str>` | Object field or `.err`. |
| `has(j, key)` | `has(j: json, key: str) -> bool` | Whether object has `key`. |
| `keys(j)` | `keys(j: json) -> [str]` | Object keys. |
| `len(j)` | `len(j: json) -> int` | Length (object entries or array length). |
| `merge(a, b)` | `merge(a: json, b: json) -> json` | Deep-merge two JSON values. |
| `null()` | `null() -> json` | JSON null value. |
| `array_push(j, val)` | `array_push(j: json, val: T) -> json` | Push `val` onto a JSON array. |
| `deep_get(j, path)` | `deep_get(j: json, path: str) -> Result<json, str>` | Dot-path query (e.g. `"addr.city"`). |
| `path_exists(j, path)` | `path_exists(j: json, path: str) -> bool` | Whether a dot-path exists. |
| `path_get_or(j, path, default)` | `path_get_or(j: json, path: str, default: json) -> json` | Dot-path get or `default`. |
