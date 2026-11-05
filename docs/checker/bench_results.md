#  benchmark results

Binary: `./target/debug/zz` (dev profile). Cold check, VM engine.
Generated programs: many funcs/structs/deep generics/long match chains/big arrays/imports.

| Lines | Approx funcs |  time (s) | Peak RSS (KB) |
|-------|--------------|---------------------|---------------|
| 1250 | ~69 | 0.0078 | 34652 |
| 3076 | ~170 | 0.0091 | 39440 |
| 6134 | ~340 | 0.0176 | 49084 |
| 12250 | ~680 | 0.0411 | 63644 |
| 24482 | ~1360 | 0.0931 | 94936 |
