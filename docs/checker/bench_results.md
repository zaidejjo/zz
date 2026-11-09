#  benchmark results

Binary: `/home/zaid/Projects/zz_lang/target/release/zz` (dev profile). Cold check, VM engine.
Generated programs: many funcs/structs/deep generics/long match chains/big arrays/imports.

| Lines | Approx funcs |  time (s) | Peak RSS (KB) |
|-------|--------------|---------------------|---------------|
| 1250 | ~69 | 0.0003 | 17144 |
| 3076 | ~170 | 0.0006 | 22016 |
| 6134 | ~340 | 0.0011 | 30536 |
| 12250 | ~680 | 0.002 | 45652 |
| 24482 | ~1360 | 0.0041 | 78320 |
