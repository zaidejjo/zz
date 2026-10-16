# Regexp — `std.regexp`

Compiled patterns are opaque `"regexp"` handles, so method syntax works.
Backed by the same engine on VM and AOT (`zz_native_rt::regexp`).

```zz
import std.regexp

match regexp.compile(`^[0-9a-f]{8}$`) {
    .ok(re) => {
        println(re.is_match("deadbeef"))  // true
        match re.find("xx deadbeef yy") {
            .some(m) => println("found {m}"),
            .none    => println("no match"),
        }
        println(re.replace_all("a1 b2", "X"))  // "aX bX" per pattern
        println(re.captures("2024-02-29"))     // groups, "" for unmatched, [] on no match
    },
    .err(e) => println("bad pattern: {e}"),
}
```

| Function | Description |
|---|---|
| `compile(pat)` | `Result<regexp, str>` — invalid pattern is `.err`, never panic |
| `is_match(re, s)` | bool |
| `find(re, s)` | `Option<str>` — first match |
| `replace_all(re, s, rep)` | `str` |
| `captures(re, s)` | `[str]` — group list, `""` for unmatched, `[]` on no match |
| `is_email(s)` | convenience predicate |

## Tips

- Prefer raw backticks for patterns: `` `^[0-9a-f]{8}$` `` needs no `{{ }}` doubling.
  In `"..."` strings, quantifier braces must double (`"{{8}}"`) like Rust `format!`.
- Compile once, reuse: handles are cheap to query, compilation is the cost.
- Handles can go stale across weird lifecycles — calls then error
  `` `...`: regexp handle is no longer valid `` instead of hanging.
- For email extraction end-to-end see `examples/regexp_email_demo/extract_emails.zz`.
- For byte-offset search without regex, `std.str.find/rfind/*_in` is faster.
