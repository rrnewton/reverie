#!/usr/bin/env bash
set -uo pipefail

WT=/home/newton/work/dev-hermit/worktrees/010/hermit
OUT=/home/newton/work/dev-hermit/experiments/portable-memory-14-20260825
REPS=${1:-1}
CAP_BYTES=17179869184

nodes=(
  build.liteinst_runtime_release
  lint.clippy
  test.hermit_unit
  test.detcore_misc
  test.detcore_parallel
  test.hermit_integration
  test.arbitrary_binaries
  test.cli
  test.hermit_modes
  test.app_strict_verify
  test.command_strict_verify
  test.ignored_syscall_regressions
  test.dbt_parity
  test.envelope_levels
)

mkdir -p "$OUT/logs"
CSV="$OUT/results.csv"
printf 'sha,rep,node,result,memory_peak_bytes,wall_seconds,load1_start,load1_end\n' > "$CSV"

for rep in $(seq 1 "$REPS"); do
  for name in "${nodes[@]}"; do
    cmd=$(jq -er --arg name "$name" '.steps[] | select((.group + "." + .job) == $name) | .cmd' "$WT/ci/dag/portable.json")
    unit="mem14-${name//./-}-${rep}-$$"
    log="$OUT/logs/${name//./-}-r${rep}.log"
    sha=$(git -C "$WT" rev-parse HEAD)
    load_start=$(cut -d' ' -f1 /proc/loadavg)
    start=$SECONDS

    systemd-run --user --unit="$unit" --remain-after-exit \
      -p MemoryAccounting=1 \
      -p MemoryMax="$CAP_BYTES" \
      -p MemorySwapMax=0 \
      --working-directory="$WT" \
      --setenv=HOME="$HOME" \
      --setenv=PATH="$PATH" \
      --setenv=CARGO_BUILD_JOBS=8 \
      --setenv=DAGRUN_JOBS_ENV=CARGO_BUILD_JOBS \
      /bin/bash -c '
        exec >"$1" 2>&1
        /bin/bash -c "$2"
      ' bash "$log" "$cmd" >"$log.launch" 2>&1

    while state=$(systemctl --user show "$unit.service" -p SubState --value 2>/dev/null) \
      && [[ $state == running || $state == start ]]; do
      sleep 0.2
    done

    peak=$(systemctl --user show "$unit.service" -p MemoryPeak --value)
    rc=$(systemctl --user show "$unit.service" -p ExecMainStatus --value)
    result=$(systemctl --user show "$unit.service" -p Result --value)
    systemctl --user stop "$unit.service" >/dev/null 2>&1 || true
    systemctl --user reset-failed "$unit.service" >/dev/null 2>&1 || true
    load_end=$(cut -d' ' -f1 /proc/loadavg)
    wall=$((SECONDS - start))
    printf '%s,%s,%s,%s:%s,%s,%s,%s,%s\n' \
      "$sha" "$rep" "$name" "$result" "$rc" "${peak:-NA}" "$wall" "$load_start" "$load_end" \
      | tee -a "$CSV"
  done
done
