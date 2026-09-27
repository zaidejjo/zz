#!/usr/bin/env bash
# bench/http_duel/duel.sh — ZZ AOT vs Go vs Rust HTTP shootout.
# Same route (GET / -> "OK", keep-alive), same wrk flags, sequential on
# :8080, 3 iterations each. Prints RPS/avg/max + peak RSS per contender.
# Usage: ./bench/http_duel/duel.sh [wrk-seconds]   (default 10)
set -u
SECS="${1:-10}"
PORT=8080
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BIN="$HERE/bin"
mkdir -p "$BIN"

ZZ="${ZZ:-$ROOT/target/release/zz}"
[ -x "$ZZ" ] || ZZ="$ROOT/target/debug/zz"

command -v wrk >/dev/null || {
	echo "need wrk"
	exit 1
}
command -v go >/dev/null || {
	echo "need go"
	exit 1
}
command -v rustc >/dev/null || {
	echo "need rustc"
	exit 1
}
if curl -s -m 1 "http://127.0.0.1:$PORT/" >/dev/null 2>&1; then
	echo "port $PORT busy — stop the other server first"
	exit 1
fi

echo "== building contenders =="
# zz has no -o flag: output lands next to the source; copy it over.
"$ZZ" build --release "$ROOT/bench/performance_check/zz/bench_http_throughput.zz" || exit 1
cp "$ROOT/bench/performance_check/zz/bin/bench_http_throughput" "$BIN/zz_server" || exit 1
(cd "$HERE" && go build -o "$BIN/go_server" go_server.go) || exit 1
rustc -O -o "$BIN/rs_server" "$HERE/rs_server.rs" || exit 1
ls -la "$BIN"

wait_ready() {
	# $1 = server pid. Fails if the process dies (e.g. bind conflict)
	# instead of letting curl hit a stale server on the port.
	for _ in $(seq 1 50); do
		kill -0 "$1" 2>/dev/null || return 1
		curl -s -m 1 "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && return 0
		sleep 0.1
	done
	return 1
}

kill_tree() {
	# $1 = supervisor pid, $2 = binary tag (zz_server|go_server|rs_server).
	# Children first: ZZ workers are forked and would otherwise keep the
	# port after the supervisor dies. Patterns are bracket-guarded and
	# bin-path anchored so they can never match our own shells.
	sup="$1"
	tag="$2"
	pat="http_duel/bin/[${tag:0:1}]${tag:1}"
	for p in $(ps --ppid "$sup" -o pid= 2>/dev/null); do kill -9 "$p" 2>/dev/null; done
	kill -9 "$sup" 2>/dev/null
	sleep 0.3
	pkill -9 -f "$pat" 2>/dev/null
	true
}

run_duel() {
	name="$1"
	cmd="$2"
	bintag="$3"
	"$cmd" >"/tmp/opencode/shootout_$name.log" 2>&1 &
	SRV=$!
	if ! wait_ready "$SRV"; then
		echo "$name: failed to start"
		head -5 "/tmp/opencode/shootout_$name.log"
		kill_tree "$SRV" "$bintag"
		return 1
	fi
	# Warm up (fault pages, fill accept queues) — not counted.
	wrk -t4 -c100 -d3s "http://127.0.0.1:$PORT/" >/dev/null 2>&1
	i=1
	BEST=0
	SUM=0
	while [ "$i" -le 3 ]; do
		OUT=$(wrk -t4 -c100 -d"${SECS}s" "http://127.0.0.1:$PORT/" 2>&1)
		RPS=$(echo "$OUT" | grep -oE "Requests/sec:[ ]+[0-9.]+" | grep -oE "[0-9.]+$")
		RPS=${RPS:-0}
		AVG=$(echo "$OUT" | grep -E "^ *Latency" | awk '{print $2}')
		MAXL=$(echo "$OUT" | grep -E "^ *Latency" | awk '{print $4}')
		RSS=$(ps --ppid "$SRV" -o rss= 2>/dev/null | awk '{s+=$1} END {print s+0}')
		SELF=$(ps -o rss= -p "$SRV" 2>/dev/null | tr -d ' ')
		TOTAL=$((RSS + SELF))
		echo "  iter$i | RPS=$RPS avg=$AVG max=$MAXL rssKB(workers+self)=$RSS+$SELF=$TOTAL"
		INT=${RPS%.*}
		if [ "${INT:-0}" -gt "$BEST" ]; then BEST=$INT; fi
		SUM=$(awk "BEGIN {print $SUM + $RPS}")
		i=$((i + 1))
	done
	MEAN=$(awk "BEGIN {print $SUM / 3}")
	echo "  => $name best=$BEST mean=$MEAN"
	kill_tree "$SRV" "$bintag"
	wait "$SRV" 2>/dev/null
	sleep 2
}

echo "== ZZ AOT =="
run_duel zz "$BIN/zz_server" zz_server
echo "== Go net/http =="
run_duel go "$BIN/go_server" go_server
echo "== Rust std =="
run_duel rs "$BIN/rs_server" rs_server
echo "== done =="
