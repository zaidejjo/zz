#!/bin/bash
# S5-c: nolock_selftest hardening test
# Builds with --features nolock_selftest and asserts contention fixtures FAIL
# (i.e., violations > 0). This proves the lock is actually disabled.

set -euo pipefail

cd /home/zaid/Projects/zz

echo "=== Building zz_cli with nolock_selftest ==="
cargo build -p zz_cli --features nolock_selftest 2>&1 | tail -3

echo ""
echo "=== Running state_multiread_atomicity under nolock (expect violations > 0) ==="
set +e
OUTPUT=$(./target/debug/zz run tests/fixtures/stdlib/state_multiread_atomicity.zz 2>&1)
EXIT=$?
set -e
echo "$OUTPUT"

# Check for violations: each variant should have non-zero count
FAIL_COUNT=0
for variant in V1 V2 V3 V4 V5 V6; do
    # Extract the value for this variant from the last line
    VAL=$(echo "$OUTPUT" | grep -o "${variant}=[0-9]*" | head -1 | cut -d= -f2)
    if [ -z "$VAL" ] || [ "$VAL" = "0" ]; then
        echo "FAIL: $variant has $VAL violations (expected > 0)"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    else
        echo "PASS: $variant has $variant=$VAL violations (> 0)"
    fi
done

if [ $FAIL_COUNT -gt 0 ]; then
    echo ""
    echo "RESULT: nolock_selftest does NOT disable lock (violations still 0 for some variants)"
    exit 1
else
    echo ""
    echo "RESULT: nolock_selftest correctly disables lock (all variants show violations > 0)"
    exit 0
fi
