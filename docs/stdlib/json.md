# JSON — `std.json`

JSON parse/stringify/get/as_* with an opaque `json` handle. Buffers (`bytes`) serialize as int arrays. `json.parse` is fallible (`.err` on invalid JSON) and never panics.

```zz
import std.json
```

## Purpose

`std.json` parses JSON strings into an opaque `json` handle, extracts typed fields, and serializes values back to JSON. Extraction functions (`as_str`, `as_int`, …) are total but panic on type mismatch — guard with `is_*` or use `?`-able `get` first.

## Conceptual model

`json` is an opaque handle — you cannot inspect it directly. You navigate via `get`/`deep_get` (object) or indexing conventions, then extract via `as_str`/`as_int`/`as_float`/`as_bool`. Type checks (`is_null`, `is_bool`, `is_number`, `is_string`, `is_array`, `is_object`, `is_empty`) let you branch safely before extraction.

## Quickstart

```zz
import std.json

func main() -> Result<int, str> {
    data := json.parse(`{"name": "Alice", "age": 30, "tags": ["a","b"]}`)?
    println(json.as_str(json.get(data, "name")?))   // Alice
    println(json.as_int(json.get(data, "age")?))    // 30
    person := {"name": "Bob", "age": 25}
    println(json.stringify(person)?)                // {"name":"Bob","age":25}
    return .ok(0)
}

// Safe navigation with type checks
func safe_nav() -> Result<int, str> {
    data := json.parse(`{"name": "Alice"}`)?
    if json.is_object(data) && json.has(data, "name") {
        println(json.as_str(json.get(data, "name")?))
    }
    return .ok(0)
}
```

## Exhaustive reference

### Core

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `json.parse` | `json.parse(s: str) -> Result<json, str>` | Parse JSON string (`.err` on invalid) | `json.parse(`{"a":1}`)` |
| `json.stringify` | `json.stringify(v: T) -> Result<str, str>` | Serialize any value to JSON | `json.stringify({"a":1})` |
| `json.get` | `json.get(j: json, key: str) -> Result<json, str>` | Get object field (`?`-able) | `json.get(data,"name")?` |
| `json.as_str` | `json.as_str(j: json) -> str` | Extract string (panics on mismatch) | |
| `json.as_int` | `json.as_int(j: json) -> int` | Extract integer | |
| `json.as_float` | `json.as_float(j: json) -> float` | Extract float | |
| `json.as_bool` | `json.as_bool(j: json) -> bool` | Extract boolean | |
| `json.null` | `json.null() -> json` | JSON null | |

### Inspection

| Function | Signature | Description |
|----------|-----------|-------------|
| `json.pretty` | `json.pretty(j: json) -> str` | Pretty-print JSON |
| `json.type` | `json.type(j: json) -> str` | Type name string ("null","bool","number","string","array","object") |
| `json.len` | `json.len(j: json) -> int` | Array/object length |
| `json.keys` | `json.keys(j: json) -> [str]` | Object keys |
| `json.has` | `json.has(j: json, key: str) -> bool` | Key exists |
| `json.is_null` | `json.is_null(j: json) -> bool` | Is null |
| `json.is_bool` | `json.is_bool(j: json) -> bool` | Is bool |
| `json.is_number` | `json.is_number(j: json) -> bool` | Is number |
| `json.is_string` | `json.is_string(j: json) -> bool` | Is string |
| `json.is_array` | `json.is_array(j: json) -> bool` | Is array |
| `json.is_object` | `json.is_object(j: json) -> bool` | Is object |
| `json.is_empty` | `json.is_empty(j: json) -> bool` | Is empty |

### Mutation & paths

| Function | Signature | Description |
|----------|-----------|-------------|
| `json.merge` | `json.merge(a: json, b: json) -> json` | Deep merge |
| `json.deep_get` | `json.deep_get(j: json, path: str) -> Result<json, str>` | Dot-path lookup (`"a.b.c"`) |
| `json.array_push` | `json.array_push(j: json, val: T) -> json` | Push to array |
| `json.path_exists` | `json.path_exists(j: json, path: str) -> bool` | Path exists |
| `json.path_get_or` | `json.path_get_or(j: json, path: str, default: json) -> json` | Path with default |

### Validation & tolerance

| Function | Signature | Description |
|----------|-----------|-------------|
| `json.validate` | `json.validate(s: str) -> bool` | Validate without parsing |
| `json.parse_or` | `json.parse_or(s: str, default: json) -> json` | Parse with default on failure |
| `json.parse_or_null` | `json.parse_or_null(s: str) -> json` | Parse, null on failure |

## Error model

- `parse` returns `.err(message)` on invalid JSON — **never panics**.
- `as_*` panic on type mismatch — prefer `is_*` guards or `get` + `?`.
- `get`/`deep_get` return `.err` on missing key — `?`-able.
- `parse_or`/`parse_or_null` never fail (tolerant).

## Gotchas

- `json` is opaque — inspect only via `as_*`, `get`, `keys`, `is_*`.
- `bytes` serialize to JSON as int arrays.
- `json.stringify` is generic over `T` — dicts, arrays, scalars all work.
- `as_*` panics on mismatch — always check with `is_*` first in production code.

## Production patterns

```zz
import std.json

func main() -> Result<int, str> {
    // Tolerant parse
    user_input := `{"address": {"city": "NYC"}}`
    data := json.parse_or(user_input, json.null())

    // Deep path with default
    city := json.path_get_or(data, "address.city", json.null())
    if json.is_string(city) {
        println(json.as_str(city))
    }

    // Type-safe extraction
    if json.is_object(data) && json.has(data, "email") {
        _ = json.as_str(json.get(data, "email")?)
    }
    return .ok(0)
}
```

