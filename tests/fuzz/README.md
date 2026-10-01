# Differential fuzzer (VM vs native)

`gen.py` generates random programs exercising integer arithmetic
(including constant-left non-commutative ops), tuples + destructuring
(+ wildcards), `if`/`match` with early returns, variable shadowing,
closures capturing destructured vars, and loops. Every program prints
only deterministic `int`/`bool` lines plus a `DONE` marker.

`--shapes v2` adds three more families (new file prefix `gz*.zz`):
S1 return-of-call-result in `if`/`else`, early-return, and `match`
branches; S2 string accumulators over loops (calls, cross-iteration
copies, array/dict pushes and slot stores); S3 stringify-like struct
emits (by-value arena structs + key loops). These caught the native
loop-arena store bug (NUL bytes), a SIGSEGV in dict-set of arena keys,
and the index-set `zz_int` mis-boxing of string concats. Default mode
is byte-identical for every seed (fixed-seed CI smoke fixtures).

`run.sh` runs each case on the VM and native engines and compares exit
codes plus stdout (purely-numeric lines stripped, like the parity
harness). Result classes:

- `VMFAIL` — the VM rejected/failed the case. Triage first: usually a
  generator bug, otherwise a real VM/checker bug.
- `NATFAIL` — native failed. Bucket the C diagnostic against
  `known_failures.txt`; unmatched signatures are new bugs.
- `DIFF` — both engines exit 0 but outputs differ. Always a new bug.

## Big runs (not in CI)

```sh
python3 tests/fuzz/gen.py 0 1000 /tmp/fuzz/cases
./target/debug/zz build -p zz_cli   # or cargo build -p zz_cli
tests/fuzz/run.sh /tmp/fuzz/cases /tmp/fuzz/results
```

Keep `nproc`-scale parallelism low (`-P 2`): concurrent clang
invocations thrash small machines (see `run.sh`).

## CI smoke set

`tests/fixtures/regression/fuzz_smoke_*.zz` are fixed-seed outputs of
`gen.py` that pass strictly on both engines. They run in the existing
`e2e` + `dual_engine_parity` harnesses like any other fixture — no
Python needed in CI. To regenerate (e.g. after backend fixes widen
coverage), screen fresh seeds with both engines and keep only
strict-parity cases:

```sh
python3 tests/fuzz/gen.py 5000 60 /tmp/fuzz/new
# keep files where `zz run` and `zz run --native` both exit 0
# with identical normalized stdout
```
