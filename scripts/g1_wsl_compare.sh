#!/bin/bash
# G1: same-OS (WSL Linux loopback) comparison sing-box vs rushway vs goway.
# usage: g1_wsl_compare.sh <impl> <mode> [n]
#   impl: singbox | rushway | goway
#   mode: steady | setup
set -u
IMPL=${1:?impl: singbox|rushway|goway}
MODE=${2:?mode: steady|setup}
N=${3:-5}
SB=/root/w2bin/sing-box
GBIN=/root/w2bin/goway
RBIN=/root/rw-g1/target/release/rushway
PB=/root/rw-g1/target/release/proxy_bench
KEY=rushway-proxy-bench-test-key
UUID=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
ST=""
[ "$MODE" = "steady" ] && ST="--steady-state"

median() { printf '%s\n' "$@" | sort -n | awk '{a[NR]=$1} END{if(NR%2) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2}'; }

C1=(); C8=(); C32=()
for i in $(seq 1 "$N"); do
  SP=$((20100 + RANDOM % 900))
  CP=$((21100 + RANDOM % 900))
  SRV_PID=""; CLI_PID=""
  case "$IMPL" in
    singbox)
      cat > /tmp/g1ssrv.json <<EOF
{"log":{"level":"warn"},"inbounds":[{"type":"vless","tag":"vless-ws-in","listen":"127.0.0.1","listen_port":$SP,"users":[{"uuid":"$UUID"}],"transport":{"type":"ws","path":"/bench"}}],"outbounds":[{"type":"direct","tag":"direct"}]}
EOF
      cat > /tmp/g1scli.json <<EOF
{"log":{"level":"warn"},"inbounds":[{"type":"socks","tag":"socks-in","listen":"127.0.0.1","listen_port":$CP}],"outbounds":[{"type":"vless","tag":"proxy","server":"127.0.0.1","server_port":$SP,"uuid":"$UUID","transport":{"type":"ws","path":"/bench"}}]}
EOF
      $SB run -c /tmp/g1ssrv.json > /tmp/g1ssrv.log 2>&1 & SRV_PID=$!
      $SB run -c /tmp/g1scli.json > /tmp/g1scli.log 2>&1 & CLI_PID=$!
      ;;
    rushway)
      $RBIN -p "$SP" -k "$KEY" --no-block-local --log ERROR > /tmp/g1rsrv.log 2>&1 & SRV_PID=$!
      $RBIN -p "$CP" -k "$KEY" --no-block-local --log ERROR --up "ws://127.0.0.1:$SP/" > /tmp/g1rcli.log 2>&1 & CLI_PID=$!
      ;;
    goway)
      $GBIN -p ":$SP" -k "$KEY" --no-block-local --log ERROR > /tmp/g1gsrv.log 2>&1 & SRV_PID=$!
      $GBIN -p ":$CP" -k "$KEY" --no-block-local --log ERROR --up "ws://127.0.0.1:$SP/" > /tmp/g1gcli.log 2>&1 & CLI_PID=$!
      ;;
    *) echo "bad impl"; exit 1 ;;
  esac
  ok=0
  for t in $(seq 1 80); do
    if ss -ltn | grep -q ":$CP " && ss -ltn | grep -q ":$SP "; then ok=1; break; fi
    sleep 0.1
  done
  if [ "$ok" != 1 ]; then echo "PORT FAIL impl=$IMPL i=$i"; kill -9 "$SRV_PID" "$CLI_PID" 2>/dev/null; exit 1; fi
  line=$($PB --implementation "$IMPL" --external --client-port "$CP" $ST 2>&1 | grep '^proxy_e2e ' | tail -1)
  if [ -z "$line" ]; then echo "BENCH FAIL impl=$IMPL i=$i"; kill -9 "$SRV_PID" "$CLI_PID" 2>/dev/null; exit 1; fi
  c1=$(echo "$line" | sed -n 's/.*c1_mib_s=\([0-9.]*\).*/\1/p')
  c8=$(echo "$line" | sed -n 's/.*c8_mib_s=\([0-9.]*\).*/\1/p')
  c32=$(echo "$line" | sed -n 's/.*c32_mib_s=\([0-9.]*\).*/\1/p')
  echo "impl=$IMPL mode=$MODE sample=$i c1=$c1 c8=$c8 c32=$c32"
  C1+=("$c1"); C8+=("$c8"); C32+=("$c32")
  kill "$SRV_PID" "$CLI_PID" 2>/dev/null
  wait "$SRV_PID" "$CLI_PID" 2>/dev/null
  sleep 0.3
done
echo "SUMMARY impl=$IMPL mode=$MODE n=$N c1=$(median "${C1[@]}") c8=$(median "${C8[@]}") c32=$(median "${C32[@]}")"
