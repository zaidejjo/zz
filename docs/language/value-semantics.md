# Value semantics

Every binding holds an independent value. After `y := x`, mutating one side
never shows through the other. There are no references, borrows, or lifetimes.

```zz
struct Counter { n: int }

func bump(c: Counter) -> int {
    c.n += 100  // local copy only
    return c.n
}

c := Counter{ n: 10 }
bump(c)  // 110
c.n      // still 10
```

## What you can rely on

- Reads copy. Evaluating a variable yields a deep, independent copy.
- Binds copy. `p := expr` stores a copy; the source stays live and untouched.
- Calls copy in, move internally. Args are evaluated (cloning live vars) then
  moved into the callee. Returns move to the caller.
- Plain stores move. `x = v` overwrites without cloning.
- No use-after-move exists — no move tracking needed because no move is observable.

## What the engine may elide (invisible)

Copy-on-write with refcounts: cloning bumps a count O(1); mutation copies once
when shared, in place when uniquely owned. Equality stays deep; sharing is
never observable.

Also invisible: self-reassignment threading (`doc = push(doc, ...)` moves the
local in), field-window take/restore (`s.f = pure`, `len(s.f)` takes zero clones),
AOT readonly-param borrows. Differential runs must read identically with every
optimization on or off. If a future analysis made a move observable, it would
ship as a checker error, not a silent change.

## Practical rules

- Pass big values freely; CoW keeps the common cases O(1) until you write.
- `clone(v)` pins a fully independent deep copy — use before handing data to
  tasks/plugins or snapshotting before mutation.
- `bytes` buffers are the exception: immutable, slices share the store zero-copy,
  `b[i] = x` is an error.
- Spawning copies captures (snapshot isolation). Channels and join handles are
  the only sharing. See [Concurrency](../stdlib/concurrency.md).

Deep spec: [types.md](../types.md) Value Semantics.
