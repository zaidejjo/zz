# Plugins — C ABI, `plugin.zzi`, native hooks

ZZ plugins speak C ABI (`plugin.zzi`). Rust crates can back plugins, but the ZZ
side never exposes Rust semantics. Use for `unsafe`, zero-cost abstractions,
`no_std`/embedded, manual layout — everything ZZ deliberately omits.

## Minimal shape

```
my-plugin/
  plugin.zzi        # interface: func signatures ZZ sees
  src/lib.rs        # or .c — C ABI exports
  zz.toml           # [native] build hook (see below)
```

```zz
// consumer.zz
import myplugin
println(myplugin.fast_hash("hi"))
```

## Build hooks

Prefer `[native.build-cc]`; bare `[native] build = "<hook>"` is deprecated
(build warns with a migrate hint pointing at `docs/plugin-author-guide.md §4`).
Export the ABI version stamp or the loader errors with a pointer to §11.

```toml
[native.build-cc]
# cc + args to compile the plugin for the current target
```

`--allow-source-builds` / `--allow-hooks` gate source builds in `zz build` and
AOT `zz test` (same meaning both places).

## Authoring checklist

1. Keep the `plugin.zzi` surface narrow and total — fallible ops return
   `Result<_, str>` with `plugin:op:code: detail`.
2. Deep-copy at the boundary (ZZ values are CoW; never retain a borrowed pointer).
3. Mirror VM and AOT semantics exactly; add a fixture under `tests/fixtures/`
   and register in `e2e.rs` + `dual_engine_parity.rs` (+ `batch_lists.rs` or
   `EXCLUDED` with reason) per [Parity](../guides/parity.md).
4. Document thread-safety: arenas are thread-local, string refcounts + intern
   table are thread-safe; your global state must be too for `task.spawn` callers.
5. Ship `DOCS.md` or `docs/**/*.md` so `zz info`/`publish` bundle it.

Full guides: [plugin-author-guide.md](../plugin-author-guide.md),
[writing-native-plugins.md](../writing-native-plugins.md).
