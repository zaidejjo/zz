#!/bin/bash
# =====================================================================
#  Full Benchmark Suite: Clang-only backend vs pre-migration baseline
#  Metrics: build time, binary size, runtime perf, VM/native correctness,
#           cross-compile sanity, cold vs warm cache.
#
#  Usage:  bash bench/clang/full_bench.sh
#  Writes: bench/clang/results.csv  +  bench/clang/results.md
# =====================================================================
set -u

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NEW=/home/zaid/.bench/zz_new
BASE=/home/zaid/.bench/zz_base_bin
TMPD=/home/zaid/.bench/tmp_full
export TMPDIR="$TMPD"
mkdir -p "$TMPD"

for b in "$NEW" "$BASE"; do
	[ -x "$b" ] || {
		echo "missing binary: $b"
		exit 1
	}
done

# ── File list ────────────────────────────────────────────────────────
# bench programs (measurable workload)
BENCH_PROGS=(
	bench/clang/fib_rec.zz
	bench/clang/loop_sum.zz
	bench/clang/str_build.zz
	bench/clang/float_math.zz
	bench/clang/array_sum.zz
)
# examples (safe, non-interactive, VM-exit-0)
EXAMPLE_PROGS=(
	examples/demo.zz
	examples/math_example.zz
	examples/str_example.zz
	examples/arrays.zz
	examples/structs.zz
	examples/consts.zz
	examples/str_advanced_example.zz
	examples/for.zz
	examples/vec_example.zz
	examples/list.zz
	examples/iterators.zz
	examples/encoding_example.zz
	examples/mathz.zz
	examples/json_example.zz
	examples/elvis.zz
	examples/built_in.zz
	examples/dict.zz
	examples/arrow.zz
	examples/join.zz
	examples/multi_string.zz
	examples/multiline.zz
	examples/test.zz
	examples/test-if.zz
	examples/test-string-compare-blocks.zz
	examples/test-while-string.zz
	examples/name.zz
	examples/names-args.zz
	examples/selective_imports.zz
	examples/phase3c_demo.zz
	examples/test-fstrings.zz
)
# syntax fixtures
SYNTAX_PROGS=(
	tests/fixtures/syntax/declarations.zz
	tests/fixtures/syntax/functions.zz
	tests/fixtures/syntax/control_flow.zz
	tests/fixtures/syntax/match.zz
	tests/fixtures/syntax/pipelines.zz
	tests/fixtures/syntax/arrays.zz
	tests/fixtures/syntax/fstrings.zz
	tests/fixtures/syntax/operators.zz
	tests/fixtures/syntax/struct_impl.zz
	tests/fixtures/syntax/struct_array_push.zz
	tests/fixtures/syntax/pipe_elvis.zz
	tests/fixtures/syntax/multiline_strings.zz
	tests/fixtures/syntax/dicts.zz
	tests/fixtures/syntax/dict_iteration.zz
	tests/fixtures/syntax/destructuring.zz
	tests/fixtures/syntax/empty_infer.zz
	tests/fixtures/syntax/const.zz
	tests/fixtures/syntax/closure_annotations.zz
	tests/fixtures/syntax/hof.zz
	tests/fixtures/syntax/function_types.zz
	tests/fixtures/syntax/defer.zz
	tests/fixtures/syntax/return_in_loops.zz
	tests/fixtures/syntax/question_operator_newline.zz
	tests/fixtures/syntax/string_blocks.zz
	tests/fixtures/syntax/main_entrypoint.zz
	tests/fixtures/syntax/match_guards.zz
)
TYPE_PROGS=(
	tests/fixtures/types/structs.zz
	tests/fixtures/types/type_inference.zz
	tests/fixtures/types/generics.zz
	tests/fixtures/types/generic_bounds.zz
	tests/fixtures/types/variants.zz
)
STDLIB_PROGS=(
	tests/fixtures/stdlib/math_ops.zz
	tests/fixtures/stdlib/strings.zz
	tests/fixtures/stdlib/vectors.zz
	tests/fixtures/stdlib/time_ops.zz
	tests/fixtures/stdlib/encoding_test.zz
	tests/fixtures/stdlib/console.zz
)

ALL_PROGS=("${BENCH_PROGS[@]}" "${EXAMPLE_PROGS[@]}" "${SYNTAX_PROGS[@]}" "${TYPE_PROGS[@]}" "${STDLIB_PROGS[@]}")

# ── Timing helpers ───────────────────────────────────────────────────
now() { date +%s%N; }
elapsed() { awk "BEGIN{printf \"%.3f\", ($2-$1)/1000000000}"; }

# ── CSV ──────────────────────────────────────────────────────────────
CSV="$ROOT/bench/clang/results.csv"
echo "file,toolchain,mode,build_cold_s,build_warm_s,size_bytes,run_s,vm_exit,bin_exit,output_match" >"$CSV"

# Binary path produced by each toolchain for a source file.
# base: <stem> next to source. new: <sourcedir>/bin/<stem>.
binpath() { # $1=toolchain $2=src
	local src="$ROOT/$2" stem
	stem="$(basename "$src" .zz)"
	if [ "$1" = base ]; then
		echo "${src%.zz}"
	else
		echo "$(dirname "$src")/bin/$stem"
	fi
}

cleanup_tc() { # $1=toolchain $2=src
	local b
	b="$(binpath "$1" "$2")"
	rm -f "$b"
	if [ "$1" = new ]; then
		rmdir "$(dirname "$b")" 2>/dev/null
	fi
}

# ── Main benchmark loop ─────────────────────────────────────────────
echo "=== Clang-only backend benchmark suite ==="
echo "new: $NEW"
echo "base: $BASE"
echo "files: ${#ALL_PROGS[@]}"
echo

for rel in "${ALL_PROGS[@]}"; do
	src="$ROOT/$rel"
	[ -f "$src" ] || {
		echo "SKIP missing $rel"
		continue
	}

	# VM reference (new binary; VM is identical across trees).
	timeout 30 "$NEW" run "$src" >"$TMPD/vm.out" 2>"$TMPD/vm.err"
	vm_exit=$?
	vm_out="$(cat "$TMPD/vm.out")"
	if [ $vm_exit -ne 0 ]; then
		echo "SKIP vm-exit-$vm_exit $rel"
		continue
	fi

	for tc in base new; do
		if [ "$tc" = base ]; then BIN="$BASE"; else BIN="$NEW"; fi
		for mode in default p; do
			if [ "$mode" = p ]; then FLAGS=(-p); else FLAGS=(); fi
			home="/home/zaid/.bench/home_${tc}_${mode}"
			rm -rf "$home"
			mkdir -p "$home"

			# Cold build
			t0=$(now)
			HOME="$home" timeout 120 "$BIN" build "${FLAGS[@]}" "$src" >"$TMPD/b.out" 2>&1
			bcode=$?
			t1=$(now)

			# Warm rebuild (same cache)
			t2=$(now)
			HOME="$home" timeout 120 "$BIN" build "${FLAGS[@]}" "$src" >>"$TMPD/b.out" 2>&1
			t3=$(now)

			if [ $bcode -ne 0 ]; then
				echo "BUILD-FAIL tc=$tc mode=$mode $rel"
				head -2 "$TMPD/b.out"
				echo "$rel,$tc,$mode,$(elapsed $t0 $t1),0,0,0,$vm_exit,-1,build-fail" >>"$CSV"
				cleanup_tc "$tc" "$rel"
				continue
			fi

			b="$(binpath "$tc" "$rel")"
			size=$(stat -c%s "$b" 2>/dev/null || echo 0)

			# Runtime
			t4=$(now)
			timeout 30 "$b" >"$TMPD/bin.out" 2>&1
			run_exit=$?
			t5=$(now)

			if [ "$vm_out" = "$(cat "$TMPD/bin.out")" ] && [ $run_exit -eq 0 ]; then
				match=yes
			else
				match="NO(exit=$run_exit)"
			fi

			echo "$rel,$tc,$mode,$(elapsed $t0 $t1),$(elapsed $t2 $t3),$size,$(elapsed $t4 $t5),$vm_exit,$run_exit,$match" >>"$CSV"
			echo "done tc=$tc mode=$mode $rel cold=$(elapsed $t0 $t1)s warm=$(elapsed $t2 $t3)s size=$size match=$match"
			cleanup_tc "$tc" "$rel"
		done
	done
done

# ── Cross-compile sanity (new binary only) ──────────────────────────
echo
echo "=== Cross-compile sanity ==="
CROSS_CSV="$TMPD/cross.csv"
echo "target,exit,has_target_flag,has_march_native,has_fuse_lld,has_lws2_32" >"$CROSS_CSV"

for triple in aarch64-unknown-linux-gnu x86_64-pc-windows-gnu; do
	home="/home/zaid/.bench/home_cross"
	rm -rf "$home"
	mkdir -p "$home"
	echo 'println("x")' >"$TMPD/cross.zz"
	HOME="$home" timeout 120 "$NEW" build -p --verbose --target "$triple" "$TMPD/cross.zz" >"$TMPD/cross.out" 2>&1
	code=$?

	has_target=$(grep -c "\-\-target=$triple" "$TMPD/cross.out" 2>/dev/null || echo 0)
	has_march=$(grep -c "\-march=native" "$TMPD/cross.out" 2>/dev/null || echo 0)
	has_lld=$(grep -c "\-fuse-ld=lld" "$TMPD/cross.out" 2>/dev/null || echo 0)
	has_ws232=$(grep -c "\-lws2_32" "$TMPD/cross.out" 2>/dev/null || echo 0)

	echo "$triple,$code,$has_target,$has_march,$has_lld,$has_ws232" >>"$CROSS_CSV"
	echo "target=$triple exit=$code target_flag=$has_target march_native=$has_march lld=$has_lld ws2_32=$has_ws232"

	# Show the actual clang flags if verbose
	if [ $code -eq 0 ] || grep -q "clang" "$TMPD/cross.out" 2>/dev/null; then
		grep -E "^\s*(clang|clang-)" "$TMPD/cross.out" 2>/dev/null | head -1 || true
	fi
	rm -rf "$home"
done

# ── Summary ──────────────────────────────────────────────────────────
echo
echo "CSV: $CSV"
echo "Cross-compile: $CROSS_CSV"
echo "=== benchmark suite complete ==="
