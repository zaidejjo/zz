# Cheatsheet — one page, zero fluff

## New Project
```bash
zz new project   # creates new zz project
```


## Run / Build

```bash

zz run [main.zz]             # runs the code in the ZVM(zz virtual machine)
zz build [main.zz]           # build the code into native binary
zz build --release [main.zz] # builds an optimized native binary

zz eval "1 + 2"              # execute code diretly
```

## Check / Format
```bash

zz check [src]   # check the code for any bugs/warnings
zz fix src/      # fix some bugs like unusued imports/variables/functions/...
zz fmt src/      # format the code
```

## Tests / Benchmarking
```bash
zz test    # runs the tests that have the @test decorator
zz bensh   # runs a benshmark that have the @bensh decorator


```

## Vars / fns

```zz
x := 42             // auto datatype
pi: float = 3.14    // explicit datatype
func add(a: int, b: int) -> int { 
  return a + b 
}
f := |x: int| x * 2 // lamda function
```

## Strings

```zz
name := "zaidejjo"
pi := 3.14159

println("hi {name}")              // hi zaidejjo
print("{{name}}")                 // {name}
print(`raw {"a":1}`)              // raw {"a":1}
print("PI: {pi:.2f}")             // PI: 3.14
print("  hi  ".trim().to_upper()) // HI
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
s = http.route(s, "GET", "/", |req| "ok")
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
