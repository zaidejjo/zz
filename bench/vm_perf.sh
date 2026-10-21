#!/bin/bash
# VM perf gate: runs bench/vm_gate fixtures under two zz binaries and
# compares best-of-N elapsed ms (inside-VM timers, startup excluded).
#
# Usage: bench/vm_perf.sh [runs] [new_bin] [base_bin]
#   runs     best-of-N repetitions per fixture (default 3)
#   new_bin  candidate binary (default ZZ_NEW_BIN or ~/.bench/zz_new)
#   base_bin baseline binary (default ZZ_BASE_BIN or ~/.bench/zz_base)
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

FIXTURES="loop_sum array_sum if_loop fib22 str_index str_slice_len dict_lookup method_push"

# run_one <bin> <fixture> -> prints "<ms> <result>" (last line parsed).
run_one() {
	$1 run "$GATE/$2.zz" 2>/dev/null | tail -n 1 | sed -e 's/^.*: //' -e 's/ms result=/ /'
}

# best_of_pair <fixture> -> prints "<base_ms> <base_res> <new_ms> <new_res>".
# Interleaves the two binaries one repetition at a time, alternating the
# order, so host load drift lands on both sides instead of the last runner.
#
# Both caches are warmed first. `zz run` stores lowered IR on a cold miss and
# executes it on every later run; the AST-compiled chunk (miss) and the
# IR-raised chunk (hit) are different code shapes (the raised chunk went
# through lowering and re-raising, and pays for the module-hop). Timing a
# cold first run against a warm one (or two binaries with differently-aged
# caches) would compare code shapes rather than the change under test.
best_of_pair() {
	base_best_ms=""
	base_best_res=""
	new_best_ms=""
	new_best_res=""
	run_one "$BASE_BIN" "$1" >/dev/null
	run_one "$NEW_BIN" "$1" >/dev/null
	n=0
	while [ "$n" -lt "$RUNS" ]; do
		if [ $((n % 2)) -eq 0 ]; then
			order="$BASE_BIN $NEW_BIN"
		else
			order="$NEW_BIN $BASE_BIN"
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
			if [ "$bin" = "$BASE_BIN" ]; then
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
	pair="$(best_of_pair "$f")"
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
