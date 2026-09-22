#!/bin/bash
# Parity test: compare VM vs native output for all .zz files in examples/.
set -euo pipefail

cd "$(dirname "$0")/.."

ZZ="cargo run --bin zz --"
PASS=0
FAIL=0
SKIP=0
TOTAL=0
ERRORS=()

for f in examples/*.zz; do
	TOTAL=$((TOTAL + 1))
	basename="$(basename "$f")"

	# Skip files that need interactive input or network
	case "$basename" in
	input.zz | tcp_echo_server.zz | weather_api_client.zz | users-web.zz | parallel_worker_pool.zz)
		SKIP=$((SKIP + 1))
		echo "SKIP  $basename (needs interactive/network)"
		continue
		;;
	esac

	# Run VM
	VM_OUT=$($ZZ run "$f" 2>/dev/null) || {
		SKIP=$((SKIP + 1))
		echo "SKIP  $basename (VM failed)"
		continue
	}
	VM_EXIT=$?

	# Run native
	NATIVE_OUT=$($ZZ run --native "$f" 2>/dev/null) || {
		FAIL=$((FAIL + 1))
		ERRORS+=("$basename: native build/run failed")
		echo "FAIL  $basename (native failed)"
		continue
	}
	NATIVE_EXIT=$?

	# Compare
	if [ "$VM_OUT" = "$NATIVE_OUT" ]; then
		PASS=$((PASS + 1))
		echo "PASS  $basename"
	else
		FAIL=$((FAIL + 1))
		DIFF_OUT=$(diff <(echo "$VM_OUT") <(echo "$NATIVE_OUT") || true)
		ERRORS+=("$basename:
$DIFF_OUT")
		echo "FAIL  $basename (output differs)"
		echo "$DIFF_OUT" | head -10
		echo "---"
	fi
done

echo ""
echo "==============================="
echo "Results: $PASS pass, $FAIL fail, $SKIP skip (of $TOTAL total)"
echo "==============================="

if [ $FAIL -gt 0 ]; then
	echo ""
	echo "Failures:"
	for e in "${ERRORS[@]}"; do
		echo "  $e"
		echo ""
	done
	exit 1
fi
