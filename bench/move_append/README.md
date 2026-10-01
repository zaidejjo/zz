# Move-append elision benchmarks

Harness for the quadratic-append elimination work (`perf/move-append-elision`).

- `gen_corpus.py` — deterministic TOML corpus (seed 7) at 1 KB / 8 KB /
  32 KB / 128 KB / 1 MB, flat-key and realistic-mixed shapes. Rerun to
  regenerate; `corpus/` is gitignored, `MANIFEST.txt` shas pin the bytes.
- `parse_proxy.zz` — line-based proxy for the toml package's arena pattern
  (`doc = push_*(doc, …)` on an 11-array struct).
- `push_int.zz` / `push_str.zz` — `b = vec.push(b, x)` accumulators.
- `field_push.zz` — `s.f = vec.push(s.f, x)` struct-field accumulator.
- `thread_doc.zz` — `doc = f(doc, x)` threading an 11-array struct.
- `run.py` — builds native binaries, runs every case on VM + native,
  measures wall time (monotonic) and peak RSS (per-child `wait4`
  `ru_maxrss` — the same kernel counter GNU `time -v` reports; GNU time
  is not installed here). Writes `docs/perf/baseline.md`.

```bash
python3 bench/move_append/gen_corpus.py
python3 bench/move_append/run.py
```

Full run takes a while (VM corpus cases are slow — that is the point).
VM cases above 32 KB are skipped as infeasible; native 1 MB may hit the
590 s timeout, which is itself baseline signal.
