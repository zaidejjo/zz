# CSV — `std.csv`

RFC4180 subset: quoted fields, `""` escapes, embedded newlines, CRLF. Rows are `[[str]]` (first row = header when present). Unterminated trailing quotes are treated as end-of-field.

```zz
import std.csv
```

## Purpose

`std.csv` parses and stringifies CSV. `csv.parse(src)` uses `,`; `csv.parse_delim(src, d)` uses a custom one-char delimiter. Round-trips via `stringify`/`stringify_delim`. Header-keyed JSON via `to_json`. Suitable for config, data import/export, and simple spreadsheets.

## Quickstart

```zz
import std.csv

rows := csv.parse("a,b\n1,2\n")
csv.header(rows)            // ["a", "b"]
csv.get_cell(rows, 1, 0, "?")  // "1"
csv.stringify(rows)         // round-trips: "a,b\n1,2\n"
csv.to_json(rows)           // JSON array of objects
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `csv.delim_first` | `csv.delim_first(delimiter: str) -> str` | Detect/normalize first delimiter | |
| `csv.json_escape` | `csv.json_escape(s: str) -> str` | Escape a string for JSON embedding | |
| `csv.parse` | `csv.parse(src: str) -> [[str]]` | Parse with `,` delimiter | `csv.parse("a,b\n1,2")` |
| `csv.parse_delim` | `csv.parse_delim(src: str, delimiter: str) -> [[str]]` | Parse with custom delimiter | |
| `csv.needs_quote` | `csv.needs_quote(cell: str, d: str) -> bool` | Cell needs quoting under delimiter | |
| `csv.escape_cell` | `csv.escape_cell(cell: str) -> str` | Escape a single cell | |
| `csv.stringify` | `csv.stringify(rows: [[str]]) -> str` | Serialize with `,` (quotes when needed) | |
| `csv.stringify_delim` | `csv.stringify_delim(rows: [[str]], delimiter: str) -> str` | Serialize with custom delimiter | |
| `csv.header` | `csv.header(rows: [[str]]) -> [str]` | First row (`[]` when empty) | `csv.header(rows)` → `["a","b"]` |
| `csv.records` | `csv.records(rows: [[str]]) -> [[str]]` | All rows after the first | |
| `csv.len` | `csv.len(rows: [[str]]) -> int` | Row count | |
| `csv.get_cell` | `csv.get_cell(rows, r: int, c: int, default: str) -> str` | Bounds-checked cell | `csv.get_cell(rows,1,0,"?")` → "1" |
| `csv.validate` | `csv.validate(src: str) -> bool` | Non-empty parse | |
| `csv.to_json` | `csv.to_json(rows: [[str]]) -> json` | Header-keyed JSON array | |

## Edge cases & gotchas

- **First row is header.** `header`/`to_json` treat row 0 as column names; use `records` to skip it.
- **`get_cell` bounds-checked.** Returns `default` on out-of-range row/col.
- **Quoted fields & `""` escapes** handled per RFC4180; embedded newlines and CRLF supported.
- **Unterminated trailing quote** treated as end-of-field (lenient).
- **TOML is not in stdlib.** Use the external pure-ZZ `toml` package.

## Performance notes

- Parse is O(n) single pass.
- `stringify` quotes cells only when needed (contains delimiter, quote, or newline).
- Pure ZZ — identical on VM and AOT.

