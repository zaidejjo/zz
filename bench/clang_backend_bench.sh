#!/bin/bash
# Benchmark: Clang-only `zz build` (feat/clang-only-backend) vs pre-migration baseline.
#
# Usage: ./bench/clang_backend_bench.sh
# Requires: $BENCH_DIR/zz_new and $BENCH_DIR/zz_base binaries
#   (BENCH_DIR defaults to ~/.bench; override with ZZ_BENCH_DIR).
# Writes: bench/clang_backend_bench_raw.csv + bench/clang_backend_bench_results.md
#
# Design notes (why it stays fast enough):
# - curated file set: non-interactive, VM-exit-0 programs only (verified up front);
# - isolated HOME per toolchain (no cache collisions between old/new fingerprints);
# - cold = cache wiped before build, warm = immediate rebuild into same cache;
# - every artifact (old next-to-source binary, new bin/) removed after measuring.

set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BENCH_DIR="${ZZ_BENCH_DIR:-$HOME/.bench}"
NEW_BIN="$BENCH_DIR/zz_new"
BASE_BIN="$BENCH_DIR/zz_base"
TMPD="$BENCH_DIR/tmp"
export TMPDIR="$TMPD"
mkdir -p "$TMPD"

for b in "$NEW_BIN" "$BASE_BIN"; do
	[ -x "$b" ] || {
		echo "missing binary: $b"
		exit 1
	}
done

FILES=(
	examples/demo.zz
	examples/math_example.zz
	examples/str_example.zz
	examples/arrays.zz
	examples/structs.zz
	examples/json_example.zz
	examples/consts.zz
	examples/str_advanced_example.zz
	examples/for.zz
	examples/vec_example.zz
	examples/list.zz
	examples/iterators.zz
	examples/encoding_example.zz
	examples/mathz.zz
	tests/fixtures/syntax/declarations.zz
	tests/fixtures/syntax/functions.zz
	tests/fixtures/syntax/control_flow.zz
	tests/fixtures/syntax/match.zz
	tests/fixtures/syntax/pipelines.zz
	tests/fixtures/syntax/arrays.zz
	tests/fixtures/syntax/fstrings.zz
	tests/fixtures/types/structs.zz
	tests/fixtures/types/type_inference.zz
	tests/fixtures/stdlib/math_ops.zz
	tests/fixtures/stdlib/strings.zz
	tests/fixtures/stdlib/vectors.zz
)

CSV="$ROOT/bench/clang_backend_bench_raw.csv"
echo "file,toolchain,mode,build_cold_s,build_warm_s,size_bytes,run_s,vm_exit,bin_exit,output_match" >"$CSV"

now() { date +%s%N; }
elapsed() { awk "BEGIN{printf \"%.2f\", ($2-$1)/1000000000}"; }

# Binary path produced by each toolchain for a source file.
# base: <stem> next to source. new: <sourcedir>/bin/<stem>.
binpath() { # $1=toolchain $2=src
	local src="$ROOT/$2" stem
	stem="$(basename "$src" .zz)"
	if [ "$1" = base ]; then
		echo "${src%.zz}"
	else echo "$(dirname "$src")/bin/$stem"; fi
}

cleanup() { # $1=toolchain $2=src
	local b
	b="$(binpath "$1" "$2")"
	rm -f "$b"
	if [ "$1" = new ]; then rmdir "$(dirname "$b")" 2>/dev/null; fi
}

for rel in "${FILES[@]}"; do
	src="$ROOT/$rel"
	[ -f "$src" ] || {
		echo "SKIP missing $rel"
		continue
	}
	# VM reference (new binary; VM code is identical across trees).
	timeout 30 "$NEW_BIN" run "$src" >"$TMPD/vm.out" 2>"$TMPD/vm.err"
	vm_exit=$?
	vm_out="$(cat "$TMPD/vm.out")"
	if [ $vm_exit -ne 0 ]; then
		echo "SKIP vm-exit-$vm_exit $rel"
		continue
	fi
	for tc in base new; do
		if [ "$tc" = base ]; then BIN="$BASE_BIN"; else BIN="$NEW_BIN"; fi
		home="$BENCH_DIR/home_$tc"
		for mode in default p; do
			if [ "$mode" = p ]; then FLAGS=(-p); else FLAGS=(); fi
			rm -rf "$home"
			mkdir -p "$home"
			# cold
			t0=$(now)
			HOME="$home" timeout 120 "$BIN" build "${FLAGS[@]}" "$src" >"$TMPD/b.out" 2>&1
			bcode=$?
			t1=$(now)
			# warm (same cache)
			t2=$(now)
			HOME="$home" timeout 120 "$BIN" build "${FLAGS[@]}" "$src" >>"$TMPD/b.out" 2>&1
			t3=$(now)
			if [ $bcode -ne 0 ]; then
				echo "BUILD-FAIL tc=$tc mode=$mode $rel"
				head -3 "$TMPD/b.out"
				echo "$rel,$tc,$mode,$(elapsed $t0 $t1),0,0,0,$vm_exit,-1,build-fail" >>"$CSV"
				cleanup "$tc" "$rel"
				continue
			fi
			b="$(binpath "$tc" "$rel")"
			size=$(stat -c%s "$b" 2>/dev/null || echo 0)
			t4=$(now)
			timeout 30 "$b" >"$TMPD/bin.out" 2>&1
			run_exit=$?
			t5=$(now)
			if [ "$vm_out" = "$(cat "$TMPD/bin.out")" ] && [ $run_exit -eq 0 ]; then match=yes; else match="NO(exit=$run_exit)"; fi
			echo "$rel,$tc,$mode,$(elapsed $t0 $t1),$(elapsed $t2 $t3),$size,$(elapsed $t4 $t5),$vm_exit,$run_exit,$match" >>"$CSV"
			echo "done tc=$tc mode=$mode $rel cold=$(elapsed $t0 $t1)s warm=$(elapsed $t2 $t3)s size=$size match=$match"
			cleanup "$tc" "$rel"
		done
	done
done

# Cross-compile sanity (new binary only): flag presence via --verbose.
echo "--- cross sanity ---"
for triple in aarch64-unknown-linux-gnu x86_64-pc-windows-gnu; do
	rm -rf "$BENCH_DIR/home_new"
	mkdir -p "$BENCH_DIR/home_new"
	echo 'println("x")' >"$TMPD/cross.zz"
	HOME="$BENCH_DIR/home_new" timeout 120 "$NEW_BIN" build -p --verbose --target "$triple" "$TMPD/cross.zz" >"$TMPD/cross.out" 2>&1
	code=$?
	echo "target=$triple exit=$code"
	grep -o "\-\-target=[^ ]*" "$TMPD/cross.out" | head -1
	grep -o "\-march=native" "$TMPD/cross.out" | head -1 || echo "(no -march=native)"
	grep -o "\-fuse-ld=lld" "$TMPD/cross.out" | head -1 || echo "(no -fuse-ld=lld)"
	grep -o "\-lws2_32" "$TMPD/cross.out" | head -1 || echo "(no -lws2_32)"
	rm -rf "$TMPD/cross_bin"
	mkdir -p "$TMPD/cross_bin"
done
echo "CSV: $CSV"
