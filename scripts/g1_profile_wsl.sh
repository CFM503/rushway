#!/bin/bash
# G1 goal-#1: capture Linux pprof CPU profiles of rushway server+client
# under the official steady-state loopback load (c1/c8/c32, 4 MiB/flow).
set -u
BIN=/root/rw-g1/target/release/rushway
PBIN=/root/rw-g1/target/release/proxy_bench
KEY=rushway-proxy-bench-test-key
SP=${SP:-18801}
CP=${CP:-18802}
MODE=${1:-steady}   # steady | setup
SRV_PB=/root/g1_srv.pb
CLI_PB=/root/g1_cli.pb
OUT=/root/g1_profile_run.log

rm -f "$SRV_PB" "$CLI_PB" "$OUT"
STEADY=""
[ "$MODE" = "steady" ] && STEADY="--steady-state"

"$BIN" -p "$SP" -k "$KEY" --no-block-local --log WARN \
  --cpuprofile "$SRV_PB" --cpuprofile-duration 70 > /root/g1_srv.out 2>&1 &
SRVPID=$!
"$BIN" -p "$CP" -k "$KEY" --no-block-local --log WARN --up "ws://127.0.0.1:$SP/" \
  --cpuprofile "$CLI_PB" --cpuprofile-duration 70 > /root/g1_cli.out 2>&1 &
CLIPID=$!

cleanup() {
  kill -INT "$CLIPID" 2>/dev/null
  kill -INT "$SRVPID" 2>/dev/null
  sleep 3
  kill -9 "$CLIPID" "$SRVPID" 2>/dev/null
  wait 2>/dev/null
}
trap cleanup EXIT

# wait for ports
for i in $(seq 1 100); do
  if ss -ltn 2>/dev/null | grep -q ":$SP " && ss -ltn 2>/dev/null | grep -q ":$CP "; then
    break
  fi
  sleep 0.1
done
if ! ss -ltn 2>/dev/null | grep -q ":$CP "; then
  echo "PORTS FAILED"
  cat /root/g1_srv.out /root/g1_cli.out 2>/dev/null
  exit 1
fi

echo "=== bench mode=$MODE pids srv=$SRVPID cli=$CLIPID ==="
# sustain load for most of the 70s profile window (ITIMER_PROF samples CPU-time only)
BENCH_LOG=/tmp/g1_bench_loop.log
: > "$BENCH_LOG"
END_TS=$(( $(date +%s) + 55 ))
n=0
while [ "$(date +%s)" -lt "$END_TS" ]; do
  n=$((n+1))
  if ! "$PBIN" --implementation rushway --external --client-port "$CP" $STEADY >> "$BENCH_LOG" 2>&1; then
    echo "bench iteration $n FAILED" | tee -a "$BENCH_LOG"
    break
  fi
  echo "bench iteration $n ok"
done
cp "$BENCH_LOG" "$OUT"
echo "bench_iterations=$n"

# profiles: duration thread (70s) or graceful INT both write them
for i in $(seq 1 90); do
  if [ -s "$SRV_PB" ] && [ -s "$CLI_PB" ]; then break; fi
  sleep 1
done
# graceful stop early so finish() flushes without waiting the full duration
kill -INT "$CLIPID" 2>/dev/null
kill -INT "$SRVPID" 2>/dev/null
sleep 5
for i in $(seq 1 30); do
  if [ -s "$SRV_PB" ] && [ -s "$CLI_PB" ]; then break; fi
  sleep 1
done
ls -la "$SRV_PB" "$CLI_PB" 2>&1
echo "srv_out:"; tail -5 /root/g1_srv.out
echo "cli_out:"; tail -5 /root/g1_cli.out
echo DONE
