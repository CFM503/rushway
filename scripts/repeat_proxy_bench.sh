#!/usr/bin/env bash
set -euo pipefail

IMPLEMENTATION=${1:?implementation name required}
PROXY_BIN=${2:?proxy binary path required}
SAMPLES=${3:-5}
BENCH_BIN=${4:-target/release/proxy_bench}

if [[ ! -x "$BENCH_BIN" ]]; then
  echo "benchmark runner not executable: $BENCH_BIN" >&2
  exit 1
fi

if (( SAMPLES < 3 )); then
  echo "SAMPLES must be >= 3 for a meaningful median" >&2
  exit 1
fi

results_file=$(mktemp)
trap 'rm -f "$results_file"' EXIT

for sample in $(seq 1 "$SAMPLES"); do
  output=$("$BENCH_BIN" --implementation "$IMPLEMENTATION" --bin "$PROXY_BIN")
  printf '%s\n' "$output"
  printf '%s\n' "$output" >> "$results_file"
done

python3 - "$IMPLEMENTATION" "$results_file" <<'PY'
import re
import statistics
import sys

implementation, path = sys.argv[1:]
text = open(path, encoding="utf-8").read()
pattern = re.compile(r"proxy_e2e implementation=" + re.escape(implementation) + r" payload_mib=4 roundtrip_echo=1 c1_mib_s=([0-9.]+) c8_mib_s=([0-9.]+) c32_mib_s=([0-9.]+)")
matches = pattern.findall(text)
if not matches:
    raise SystemExit("no benchmark samples matched expected output")

if len(matches) < 3:
    raise SystemExit(f"only {len(matches)} samples collected; need at least 3")

for idx, values in enumerate(zip(*matches), start=1):
    nums = [float(v) for v in values]
    median = statistics.median(nums)
    label = {1: "c1", 2: "c8", 3: "c32"}[idx]
    print(f"proxy_e2e_median implementation={implementation} samples={len(nums)} {label}_mib_s={median:.2f}")

all_values = list(zip(*matches))
medians = [statistics.median(float(v) for v in column) for column in all_values]
print(
    f"proxy_e2e_summary implementation={implementation} samples={len(matches)} "
    f"payload_mib=4 roundtrip_echo=1 c1_mib_s={medians[0]:.2f} "
    f"c8_mib_s={medians[1]:.2f} c32_mib_s={medians[2]:.2f}"
)
PY
