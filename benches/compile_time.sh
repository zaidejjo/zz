#!/bin/bash
# Compile-time benchmarks: time zz check (VM frontend) and zz build (AOT)
# across program sizes (small / loops / big).
#
# Usage: benches/compile_time.sh [runs] [zz_bin]
#   runs     best-of-N timed repetitions per mode (default 3)
#   zz_bin   zz binary to test (default `zz` on PATH)
#
# For each fixture it measures:
#   check   `zz check <fixture>`        (frontend type-check)
#   build   `zz build --dynamic <fixture>` (AOT dev build)
#   release `zz build <fixture>`        (AOT release build)
#
# Prints best-of-N elapsed ms per fixture/mode.
# Exit nonzero if any command fails.

set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export ZZ_NATIVE_RT_DIR="${ZZ_NATIVE_RT_DIR:-$ROOT}"
FIXTURES_DIR="$ROOT/benches/fixtures"
RUNS="${1:-3}"
ZZ="${2:-zz}"

command -v "$ZZ" >/dev/null 2>&1 || {
	echo "missing zz binary: $ZZ"
	echo "usage: benches/compile_time.sh [runs] [zz_bin]"
	exit 1
}
ZZ="$(command -v "$ZZ")"

# Elapsed milliseconds for a command; "fail:<rc>" on failure.
ms() {
	local start end rc
	start=$(date +%s%N)
	"$@" >/dev/null 2>&1
	rc=$?
	end=$(date +%s%N)
	[ $rc -ne 0 ] && {
		echo "fail:$rc"
		return
	}
	echo $(((end - start) / 1000000))
}

# Best of N repetitions for a command; returns fail:<rc> on any failure.
best_of() {
	local best="" n=0 m
	while [ "$n" -lt "$RUNS" ]; do
		m="$(ms "$@")" || return 1
		case "$m" in fail:*) return 1 ;; esac
		[ -z "$best" ] || [ "$m" -lt "$best" ] && best="$m"
		n=$((n + 1))
	done
	echo "$best"
}

echo "ZZ compile-time benchmarks (binary: $ZZ, best-of-$RUNS)"
echo "Host load: $(cut -d' ' -f1-3 /proc/loadavg) on $(nproc) cores"
echo ""
echo "| fixture | mode      | best ms | last ms |"
echo "|---------|-----------|---------|---------|"

fail=0
for f in small loops big; do
	fix="$FIXTURES_DIR/$f.zz"
	[ -f "$fix" ] || {
		echo "| $f      | check     | missing fixture | - |"
		fail=1
		continue
	}

	# Warmup (cold caches / module cache populate).
	"$ZZ" check "$fix" >/dev/null 2>&1
	"$ZZ" build --dynamic -o /tmp/zz-bench-warmup "$fix" >/dev/null 2>&1

	# check (VM frontend)
	cb="$(best_of "$ZZ" check "$fix")" || {
		echo "| $f      | check     | FAIL   | -       |"
		fail=1
		continue
	}
	cl="$(ms "$ZZ" check "$fix")"
	echo "| $f      | check     | ${cb}ms  | ${cl#fail:}ms |"

	# build --dynamic (AOT dev)
	cb="$(best_of "$ZZ" build --dynamic -o /tmp/zz-bench-dev "$fix")" || {
		echo "| $f      | build dev | FAIL   | -       |"
		fail=1
		continue
	}
	cl="$(ms "$ZZ" build --dynamic -o /tmp/zz-bench-dev "$fix")"
	echo "| $f      | build dev | ${cb}ms  | ${cl#fail:}ms |"

	# build release (AOT ThinLTO)
	cb="$(best_of "$ZZ" build -o /tmp/zz-bench-rel "$fix")" || {
		echo "| $f      | build rel | FAIL   | -       |"
		fail=1
		continue
	}
	cl="$(ms "$ZZ" build -o /tmp/zz-bench-rel "$fix")"
	echo "| $f      | build rel | ${cb}ms  | ${cl#fail:}ms |"
	echo "|---------|-----------|---------|---------|"
done

rm -f /tmp/zz-bench-warmup /tmp/zz-bench-dev /tmp/zz-bench-rel

echo ""
if [ "$fail" -ne 0 ]; then
	echo "Some benchmarks failed. See FAIL rows above."
	exit 1
fi
echo "All benchmarks ran successfully."
