#!/bin/bash
# VM perf gate: runs bench/vm_gate fixtures under two zz binaries and
# compares best-of-N elapsed ms (inside-VM timers, startup excluded).
#
# Usage: bench/vm_perf.sh [runs] [new_bin] [base_bin]
#   runs     best-of-N repetitions per fixture (default 3)
#   new_bin  candidate binary (default ZZ_NEW_BIN or ~/.bench/zz_new)
#   base_bin baseline binary (default ZZ_BASE_BIN or ~/.bench/zz_base)
#
# Environment:
#   ZZ_FIXTURES="a b"  restrict the run to these vm_gate fixtures
#   ZZ_MODE=native     time release AOT binaries (`zz build -p`) instead of
#                      the bytecode VM
#
# Every repetition runs the base binary and the new binary back to back,
# alternating which goes first, so both sample the same span of host load.
# Running all base reps then all new reps credits whichever binary happens
# to run while the machine is quieter — on a shared box that alone swings
# deltas by tens of percent. The minimums stay comparable because neither
# binary is favoured by a drifting or spiky load.
#
# Each fixture prints "<name>: <ms>ms result=<value>". The result= payload
# must match between binaries or the fixture is reported FAIL (behavior
# gate: speedups must never change semantics).
# Prints a markdown table with per-fixture delta percent; exit nonzero on
# any result mismatch or fixture failure.

set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GATE="$ROOT/bench/vm_gate"
RUNS="${1:-3}"
NEW_BIN="${2:-${ZZ_NEW_BIN:-$HOME/.bench/zz_new}}"
BASE_BIN="${3:-${ZZ_BASE_BIN:-$HOME/.bench/zz_base}}"

for b in "$NEW_BIN" "$BASE_BIN"; do
	[ -x "$b" ] || {
		echo "missing binary: $b"
		echo "usage: bench/vm_perf.sh [runs] [new_bin] [base_bin]"
		exit 1
	}
done

# Override with `ZZ_FIXTURES="math_agg math_scalar"` to gate one module.
FIXTURES="${ZZ_FIXTURES:-loop_sum while_sum array_sum if_loop fib22 str_index str_slice_len dict_lookup method_push alloc_churn math_agg math_scalar}"

# Execution mode: `ZZ_MODE=vm` (default) times the bytecode VM; `ZZ_MODE=native`
# compiles each fixture with `zz build -p` (release AOT) and times the produced
# binary, which is what the plan's C-from-ZZ gate needs.
MODE="${ZZ_MODE:-vm}"
WORK=""
if [ "$MODE" = "native" ]; then
	WORK="$(mktemp -d)"
	trap 'rm -rf "$WORK"' EXIT
fi

# build_aot <role> <compiler> <fixture> -> prints the release AOT binary path.
#
# `role` (`base` / `new`) keeps the two artifacts distinct even when both
# compiler paths share a basename (e.g. two `target/release/zz` builds).
# `ZZ_BUILD_CACHE=0` is deliberate: the build-cache key does not include the
# compiler binary's identity, so a cached artifact from the base compiler
# would be reused by the new compiler and hide a codegen change.
build_aot() {
	out="$WORK/$1-$3"
	if [ ! -x "$out" ]; then
		if ! build_log="$(ZZ_BUILD_CACHE=0 "$2" build -p -o "$out" "$GATE/$3.zz" 2>&1)"; then
			echo "fixture $3 failed to build (zz build -p) under $2:" >&2
			printf '%s\n' "$build_log" >&2
			return 1
		fi
	fi
	printf '%s\n' "$out"
}

# run_one <exe> <fixture> -> prints "<ms> <result>" (last line parsed).
# In VM mode `exe` is the zz compiler; in native mode it is the release AOT
# binary produced by `build_aot`.
run_one() {
	if [ "$MODE" = "native" ]; then
		"$1" 2>/dev/null | tail -n 1 | sed -e 's/^.*: //' -e 's/ms result=/ /'
	else
		"$1" run "$GATE/$2.zz" 2>/dev/null | tail -n 1 | sed -e 's/^.*: //' -e 's/ms result=/ /'
	fi
}

# best_of_pair <fixture> <base_exe> <new_exe>
#   -> prints "<base_ms> <base_res> <new_ms> <new_res>".
# Interleaves the two executables one repetition at a time, alternating the
# order, so host load drift lands on both sides instead of the last runner.
#
# Both sides are warmed first. In VM mode `zz run` stores lowered IR on a cold
# miss and executes it on every later run; the AST-compiled chunk (miss) and the
# IR-raised chunk (hit) are different code shapes (the raised chunk went
# through lowering and re-raising, and pays for the module-hop). Timing a
# cold first run against a warm one (or two binaries with differently-aged
# caches) would compare code shapes rather than the change under test. The
# warmups are harmless in native mode, where each artifact is built from
# scratch.
best_of_pair() {
	base_exe="$2"
	new_exe="$3"
	base_best_ms=""
	base_best_res=""
	new_best_ms=""
	new_best_res=""
	run_one "$base_exe" "$1" >/dev/null
	run_one "$new_exe" "$1" >/dev/null
	n=0
	while [ "$n" -lt "$RUNS" ]; do
		if [ $((n % 2)) -eq 0 ]; then
			order="$base_exe $new_exe"
		else
			order="$new_exe $base_exe"
		fi
		for bin in $order; do
			out="$(run_one "$bin" "$1")"
			ms="$(echo "$out" | awk '{print $1}')"
			res="$(echo "$out" | awk '{print $2}')"
			case "$ms" in
			'' | *[^0-9]*)
				echo "fixture $1 failed under $bin (output: $out)"
				exit 1
				;;
			esac
			if [ "$bin" = "$base_exe" ]; then
				if [ -z "$base_best_ms" ] || [ "$ms" -lt "$base_best_ms" ]; then
					base_best_ms="$ms"
					base_best_res="$res"
				fi
			elif [ -z "$new_best_ms" ] || [ "$ms" -lt "$new_best_ms" ]; then
				new_best_ms="$ms"
				new_best_res="$res"
			fi
		done
		n=$((n + 1))
	done
	echo "$base_best_ms $base_best_res $new_best_ms $new_best_res"
}

# Host load in the report: a busy shared machine widens every delta, and
# silent load is how a phantom regression gets chased.
echo "<!-- load $(cut -d' ' -f1-3 /proc/loadavg) on $(nproc) cores -->"
echo "| fixture | base best | new best | delta | result |"
echo "|---|---|---|---|---|"

fail=0
for f in $FIXTURES; do
	if [ "$MODE" = "native" ]; then
		base_exe="$(build_aot base "$BASE_BIN" "$f")" || exit 1
		new_exe="$(build_aot new "$NEW_BIN" "$f")" || exit 1
	else
		base_exe="$BASE_BIN"
		new_exe="$NEW_BIN"
	fi
	pair="$(best_of_pair "$f" "$base_exe" "$new_exe")"
	base_ms="$(echo "$pair" | awk '{print $1}')"
	base_res="$(echo "$pair" | awk '{print $2}')"
	new_ms="$(echo "$pair" | awk '{print $3}')"
	new_res="$(echo "$pair" | awk '{print $4}')"
	if [ "$base_res" = "$new_res" ]; then
		status="same"
	else
		status="MISMATCH"
		fail=1
	fi
	delta="$(awk -v b="$base_ms" -v n="$new_ms" 'BEGIN { if (b > 0) printf "%+.1f%%", (b - n) * 100.0 / b; else print "n/a" }')"
	echo "| $f | ${base_ms}ms | ${new_ms}ms | $delta | $status |"
done

exit "$fail"
