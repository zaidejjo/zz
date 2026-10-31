# `std.csv` — CSV parse and stringify

Parse delimited text into rows of strings and serialize rows back to
delimited text. Default delimiter is comma; use `parse_delim` /
`stringify_delim` for TSV or other separators.

```zz
import std.csv

rows := csv.parse("name,age\nAda,36\nGrace,43")
println(csv.len(rows))           // 2
println(csv.header(rows))        // ["name", "age"]
println(csv.get_cell(rows, 1, 0, "")) // "Ada"

out := csv.stringify(rows)       // "name,age\nAda,36\nGrace,43\n"
```

## Types

None (works on `[[str]]` row arrays).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `parse(src)` | `parse(src: str) -> [[str]]` | Parse comma-separated text into rows. |
| `parse_delim(src, delimiter)` | `parse_delim(src: str, delimiter: str) -> [[str]]` | Parse with a custom delimiter (e.g. `"\t"`). |
| `stringify(rows)` | `stringify(rows: [[str]]) -> str` | Serialize rows to comma-separated text. |
| `stringify_delim(rows, delimiter)` | `stringify_delim(rows: [[str]], delimiter: str) -> str` | Serialize with a custom delimiter. |
| `to_json(rows)` | `to_json(rows: [[str]]) -> json` | Convert rows (first row = header) to a JSON array of objects. |
| `len(rows)` | `len(rows: [[str]]) -> int` | Number of data rows (excluding header). |
| `header(rows)` | `header(rows: [[str]]) -> [str]` | First row as column names. |
| `get_cell(rows, r, c, default)` | `get_cell(rows: [[str]], r: int, c: int, default: str) -> str` | Cell at row `r`, column `c`; `default` when out of range. |
| `records(rows)` | `records(rows: [[str]]) -> [[str]]` | All data rows (alias of input minus header handling). |
| `escape_cell(cell)` | `escape_cell(cell: str) -> str` | Quote/escapes a single cell for CSV. |
| `needs_quote(cell, d)` | `needs_quote(cell: str, d: str) -> bool` | Whether a cell needs quoting under delimiter `d`. |
| `json_escape(s)` | `json_escape(s: str) -> str` | JSON-escape a string. |
| `validate(src)` | `validate(src: str) -> bool` | Whether `src` is well-formed delimited text. |
| `delim_first(delimiter)` | `delim_first(delimiter: str) -> str` | Detect/return the first delimiter in effect. |
