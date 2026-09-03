#!/usr/bin/env bash
# ZZ native AOT benchmark: zz-Native vs zz-VM vs Go vs Bun.
set -u
cd "$(dirname "$0")"
ZZ=../../target/release/zz
GOROOT_DIR=_go

# Build Go helper with per-benchmark argv.
mkdir -p $GOROOT_DIR
cat >$GOROOT_DIR/main.go <<'GOEOF'
package main
import (
  "fmt"
  "os"
)
func loop() { var s int64; for i := int64(0); i < 10000000; i++ { s += i }; fmt.Println(s) }
func fib(n int) int { if n <= 1 { return n }; return fib(n-1)+fib(n-2) }
func strBench() {
  s := ""
  for i := 0; i < 20000; i++ { s += "a" }
  fmt.Println(len(s))
}
func main() {
  switch os.Args[1] {
  case "loop": loop()
  case "fib": fmt.Println(fib(30))
  case "str": strBench()
  }
}
GOEOF
cat >$GOROOT_DIR/go.mod <<'GOEOF'
module main

go 1.21
GOEOF
(cd $GOROOT_DIR && GOFLAGS=-mod=mod go build -o main .)

# Bun TS helper
cat >$GOROOT_DIR/main.ts <<'TSEOF'
function loopBun() { let s: number = 0; for (let i = 0; i < 10000000; i++) s += i; console.log(s); }
function fib(n: number): number { if (n <= 1) return n; return fib(n-1)+fib(n-2); }
function strBun() { let st = ""; for (let i = 0; i < 20000; i++) st += "a"; console.log(st.length); }
switch (process.argv[2]) {
  case "loop": loopBun(); break;
  case "fib": console.log(fib(30)); break;
  case "str": strBun(); break;
}
TSEOF

# time_ms <cmd...>: run 3x, report best ms
time_ms() {
	local best=99999999
	for _ in 1 2 3; do
		local t0=$(date +%s%N)
		"$@" >/dev/null 2>&1
		local t1=$(date +%s%N)
		local dt=$(((t1 - t0) / 1000000))
		[ "$dt" -lt "$best" ] && best=$dt
	done
	echo "$best"
}

$ZZ build -p bench_loop.zz >/dev/null
$ZZ build -p bench_fib.zz >/dev/null
# string bench file
cat >bench_str.zz <<'ZEOF'
import std.io
func main() {
    s := ""
    for i in 0..20000 {
        s = s + "a"
    }
    io.println(len(s))
}
ZEOF
$ZZ build -p bench_str.zz >/dev/null

echo "=== ZZ Native AOT vs VM vs Go vs Bun ==="
echo "machine: $(uname -m)"
echo
printf "%-22s | %10s | %10s | %10s | %10s\n" "Benchmark" "zz-Native" "zz-VM" "Go" "Bun"
printf -- "----------------------------------------------------------------------\n"

loop_native=$(time_ms ./bench_loop)
loop_vm=$(time_ms $ZZ run bench_loop.zz)
loop_go=$(time_ms $GOROOT_DIR/main loop)
loop_bun=$(time_ms bun $GOROOT_DIR/main.ts loop)
printf "%-22s | %10s | %10s | %10s | %10s\n" "10M-loop (sum)" "${loop_native}ms" "${loop_vm}ms" "${loop_go}ms" "${loop_bun}ms"

fib_native=$(time_ms ./bench_fib)
fib_vm=$(time_ms $ZZ run bench_fib.zz)
fib_go=$(time_ms $GOROOT_DIR/main fib)
fib_bun=$(time_ms bun $GOROOT_DIR/main.ts fib)
printf "%-22s | %10s | %10s | %10s | %10s\n" "fib(30) recursive" "${fib_native}ms" "${fib_vm}ms" "${fib_go}ms" "${fib_bun}ms"

str_native=$(time_ms ./bench_str)
str_vm=$(time_ms $ZZ run bench_str.zz)
str_go=$(time_ms $GOROOT_DIR/main str)
str_bun=$(time_ms bun $GOROOT_DIR/main.ts str)
printf "%-22s | %10s | %10s | %10s | %10s\n" "string-concat 20k" "${str_native}ms" "${str_vm}ms" "${str_go}ms" "${str_bun}ms"
