#!/bin/bash
# G1 Round-1 A/B: paired alternating baseline(HEAD) vs optimized rushway on the
# same WSL host, AB/BA order per pair, to cancel environment drift.
# usage: g1_ab_wsl.sh <mode> [n]
#   mode: steady | setup
set -u
MODE=${1:?mode: steady|setup}
N=${2:-5}
OPT=/root/rw-g1/target/release/rushway
BASE=/root/rw-g0/target/release/rushway
PB=/root/rw-g1/target/release/proxy_bench
KEY=rushway-proxy-bench-test-key
ST=""
[ "$MODE" = "steady" ] && ST="--steady-state"

median() { printf '%s\n' "$@" | sort -n | awk '{a[NR]=$1} END{if(NR%2) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2}'; }

run_one() {
  # $1=bin $2=tag $3=sample_idx $4=order
  local BIN=$1 TAG=$2 IDX=$3 ORD=$4
  local SP CP SRV_PID CLI_PID ok t line c1 c8 c32
  SP=$((20100 + RANDOM % 900))
  CP=$((21100 + RANDOM % 900))
  "$BIN" -p "$SP" -k "$KEY" --no-block-local --log ERROR > /tmp/g1ab_srv.log 2>&1 & SRV_PID=$!
  "$BIN" -p "$CP" -k "$KEY" --no-block-local --log ERROR --up "ws://127.0.0.1:$SP/" > /tmp/g1ab_cli.log 2>&1 & CLI_PID=$!
  ok=0
  for t in $(seq 1 80); do
    if ss -ltn | grep -q ":$CP " && ss -ltn | grep -q ":$SP "; then ok=1; break; fi
    sleep 0.1
  done
  if [ "$ok" != 1 ]; then
    echo "PORT FAIL tag=$TAG i=$IDX"
    kill -9 "$SRV_PID" "$CLI_PID" 2>/dev/null
    exit 1
  fi
  line=$($PB --implementation rushway --external --client-port "$CP" $ST 2>&1 | grep '^proxy_e2e ' | tail -1)
  if [ -z "$line" ]; then
    echo "BENCH FAIL tag=$TAG i=$IDX"
    kill -9 "$SRV_PID" "$CLI_PID" 2>/dev/null
    exit 1
  fi
  c1=$(echo "$line" | sed -n 's/.*c1_mib_s=\([0-9.]*\).*/\1/p')
  c8=$(echo "$line" | sed -n 's/.*c8_mib_s=\([0-9.]*\).*/\1/p')
  c32=$(echo "$line" | sed -n 's/.*c32_mib_s=\([0-9.]*\).*/\1/p')
  echo "pair=$IDX order=$ORD tag=$TAG c1=$c1 c8=$c8 c32=$c32"
  kill "$SRV_PID" "$CLI_PID" 2>/dev/null
  wait "$SRV_PID" "$CLI_PID" 2>/dev/null
  sleep 0.3
  RES_C1=$c1; RES_C8=$c8; RES_C32=$c32
}

O_C1=(); O_C8=(); O_C32=()
B_C1=(); B_C8=(); B_C32=()
for i in $(seq 1 "$N"); do
  if [ $((i % 2)) -eq 1 ]; then ORDER=AB; FIRST=$OPT; SECOND=$BASE; else ORDER=BA; FIRST=$BASE; SECOND=$OPT; fi
  run_one "$FIRST" first "$i" "$ORDER"
  f1=$RES_C1; f8=$RES_C8; f32=$RES_C32
  run_one "$SECOND" second "$i" "$ORDER"
  s1=$RES_C1; s8=$RES_C8; s32=$RES_C32
  if [ "$ORDER" = "AB" ]; then o1=$f1; o8=$f8; o32=$f32; b1=$s1; b8=$s8; b32=$s32
  else o1=$s1; o8=$s8; o32=$s32; b1=$f1; b8=$f8; b32=$f32; fi
  echo "pair=$i order=$ORDER OPT c1=$o1 c8=$o8 c32=$o32 | BASE c1=$b1 c8=$b8 c32=$b32"
  O_C1+=("$o1"); O_C8+=("$o8"); O_C32+=("$o32")
  B_C1+=("$b1"); B_C8+=("$b8"); B_C32+=("$b32")
done
echo "SUMMARY mode=$MODE n=$N OPT c1=$(median "${O_C1[@]}") c8=$(median "${O_C8[@]}") c32=$(median "${O_C32[@]}")"
echo "SUMMARY mode=$MODE n=$N BASE c1=$(median "${B_C1[@]}") c8=$(median "${B_C8[@]}") c32=$(median "${B_C32[@]}")"
