# Cheatsheet — one page, zero fluff

## Run / check

```bash
zz run main.zz | zz run --native main.zz | zz eval "1 + 2"
zz check src/ | zz fix src/ | zz fmt src/ | zz test | zz bench
```

## Vars / fns

```zz
x := 42
pi: float = 3.14
func add(a: int, b: int) -> int { return a + b }
f := |x: int| x * 2
```

## Strings

```zz
"hi {name}" | "{{literal}}" | `raw {"a":1}` | "{n:x} {pi:.2f}"
"  hi  ".trim().to_upper()
```

## Collections

```zz
[1, 2, 3] | {"a": 1} | (7, "seven")
arr[1:3] | arr[-1] | [x * 2 for x in range(10) if x % 2 == 0]
range(5) |> map(|x| x * 2) |> filter(|x| x > 4)
```

## Match / errors

```zz
match int(s) {
    .some(v) => println(v),
    .none    => println("bad"),
}
val := int(s)?            // propagate
port := int(s) ?? 80      // fallback
```

## Structs / enums

```zz
struct Point { x: int, y: int }
p := Point{ x: 1, y: 2 }
enum T { Eof, IntLit(int) }
t := T.IntLit(42)
```

## Modules

```zz
import std.math
import std.str
import app.math
import app.math(add as f)
```

## HTTP / fs / concurrency

```zz
s := http.server()
s = http.route(s, "GET", "/", |_req| "ok")
http.listen(s, 8080)
fs.read("f.txt") // match .ok/.err
c := chan()
chan.send(c, 1)
v: int = chan.recv(c)
h := task.spawn(|_| { 42 })
```

## Where next

- Learning: [Overview](../language/overview.md).
- Tables: [Operators](../language/operators.md), [Stdlib](../stdlib/overview.md).
- Stuck: [Errors](../guides/errors.md), [FAQ](../guides/faq-glossary.md).
