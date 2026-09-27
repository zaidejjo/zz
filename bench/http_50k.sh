#!/usr/bin/env bash
# bench/http_50k.sh — reproducible AOT HTTP throughput + RSS harness.
# Usage: [ZZ=./target/debug/zz] ./bench/http_50k.sh [wrk-seconds]
# Runs 3 iterations, prints RPS/avg/p99 + worker RSS table.
# Fails loudly on port conflicts. No CPU pinning: on small boxes pinning
# server and wrk apart starves the forked workers; best-of-3 absorbs noise.
#
# CRITICAL: the `zz` binary embeds the C runtime (include_str!). Rebuild
# it after ANY runtime/*.c change or numbers measure stale code:
#   cargo build -p zz_cli   (then ZZ=./target/debug/zz for iteration)
set -u
SECS="${1:-10}"
PORT=8080
ZZ="${ZZ:-./target/release/zz}"
BIN=bench/performance_check/zz/bin/bench_http_throughput

command -v wrk >/dev/null || {
	echo "need wrk"
	exit 1
}
if curl -s -m 1 "http://127.0.0.1:$PORT/" >/dev/null 2>&1; then
	echo "port $PORT busy — stop the other server first"
	exit 1
fi

"$ZZ" build bench/performance_check/zz/bench_http_throughput.zz || exit 1

run_once() {
	"$BIN" >/tmp/opencode/bench50k_srv.log 2>&1 &
	SRV=$!
	for _ in $(seq 1 50); do
		grep -q SERVER_READY /tmp/opencode/bench50k_srv.log 2>/dev/null && break
		sleep 0.1
	done
	OUT=$(wrk -t4 -c100 -d"${SECS}s" "http://127.0.0.1:$PORT/" 2>&1)
	# Worker RSS (forked children) + parent RSS separately.
	WORKERS_RSS=$(ps --ppid "$SRV" -o rss= 2>/dev/null | awk '{s+=$1} END {print s+0}')
	PARENT_RSS=$(ps -o rss= -p "$SRV" 2>/dev/null | tr -d ' ')
	kill "$SRV" 2>/dev/null
	wait "$SRV" 2>/dev/null
	pkill -f "[b]ench_http_throughput" 2>/dev/null
	true
	RPS=$(echo "$OUT" | grep -oE "Requests/sec:[ ]+[0-9.]+" | grep -oE "[0-9.]+$")
	AVG=$(echo "$OUT" | grep -E "^ *Latency" | awk '{print $2}')
	MAXLAT=$(echo "$OUT" | grep -E "^ *Latency" | awk '{print $4}')
	echo "$RPS $AVG ${MAXLAT:-n/a} ${WORKERS_RSS:-0}/${PARENT_RSS:-0}"
}

echo "iter | RPS | avg | maxlat | workersRSS/parentRSS(KB)"
i=1
BEST=0
while [ "$i" -le 3 ]; do
	# shellcheck disable=SC2086
	set -- $(run_once)
	echo "$i | $1 | $2 | $3 | $4"
	INT=${1%.*}
	if [ "${INT:-0}" -gt "$BEST" ]; then BEST=$INT; fi
	i=$((i + 1))
	sleep 1
done
echo "best_RPS=$BEST"
