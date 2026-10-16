# Control flow

## If / else

```zz
if age < 18 {
    println("minor")
} else {
    println("adult")
}
```

Blocks are expressions: `result := { x := 10; x + 1 }` is `11`.
Functions without `-> T` return `unit`.

## If let

```zz
x := .some(5)
if let .some(n) = x {
    println("got {n}")
} else {
    println("nothing")
}
```

## Loops

```zz
i := 0
while i < 10 {
    println("{i}")
    i = i + 1
}

for i in 0..5 {
    println("{i}")
}

for name in ["Ada", "Bo"] {
    println("hi {name}")
}

for i in range(0, 10, 2) {
    println("{i}")
}
```

`break` exits, `continue` skips. Every `for`/`while` has a safepoint so
green threads stay fair even in CPU loops.

## Defer

Runs at scope exit, LIFO:

```zz
func process() {
    println("start")
    defer println("cleanup")
    println("done")
    // start, done, cleanup
}
```

Use for terminal restore, file close, lock release.

## Errors as values: `?` and `??`

```zz
func next_age(s: str) -> Option<int> {
    val := int(s)?      // .none returns early
    .some(val + 1)
}

port := int("8080") ?? 80   // fallback on .none
```

`?` joins with the next line. For multi-line branching prefer `match`:

```zz
match int(input("age: ")) {
    .some(v) => println("age {v}"),
    .none    => println("not a number"),
}
```

Next: [Collections](collections.md), [Pattern matching](pattern-matching.md).
