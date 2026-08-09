#!/bin/bash
# Measure the resource footprint of ONE `make validate` (hermit), using a cgroup
# as the meter: memory.peak (peak RSS of the whole subtree, load-immune),
# cpu.stat usage_usec (total CPU-seconds -> mean cores), and sampled cpu.stat
# deltas (-> peak instantaneous cores). Reports numbers only; does NOT need
# validate to pass — the footprint is the deliverable.
#
# Usage: measure-validate.sh <hermit-checkout-dir> [ARGS-for-validate]
set -u
HERMIT="${1:?need hermit checkout dir}"; shift || true
VARGS="$*"
OUT="${OUT:-/tmp/validate-measure}"; mkdir -p "$OUT"
BASE=/sys/fs/cgroup
MYCG=$(awk -F: '/^0::/{print $3}' /proc/self/cgroup)
CG="$BASE$MYCG/validate-measure-$$"
mkdir -p "$CG" || { echo "FATAL: cannot create cgroup $CG"; exit 2; }
SAMPLES="$OUT/samples.csv"
echo "t_s,mem_current_bytes,cpu_usage_usec,inst_cores" > "$SAMPLES"

# Sampler: every 2s record memory.current and cpu.stat usage_usec; derive the
# instantaneous cores over the interval from the usage_usec delta.
(
  prev_cpu=0; prev_t=0; start=$(date +%s)
  while :; do
    sleep 2 || break
    t=$(( $(date +%s) - start ))
    # Sum RSS over cgroup members (works without memory-controller delegation):
    # /proc/<pid>/statm field 2 = resident pages; x page size -> bytes.
    mem=0
    for pid in $(cat "$CG/cgroup.procs" 2>/dev/null); do
      rss=$(awk '{print $2}' "/proc/$pid/statm" 2>/dev/null)
      [ -n "$rss" ] && mem=$(( mem + rss ))
    done
    mem=$(( mem * 4096 ))
    cpu=$(awk '/^usage_usec/{print $2}' "$CG/cpu.stat" 2>/dev/null || echo 0)
    dt=$(( t - prev_t )); [ "$dt" -le 0 ] && dt=2
    inst=$(awk -v c="$cpu" -v p="$prev_cpu" -v dt="$dt" 'BEGIN{printf "%.2f",(c-p)/1e6/dt}')
    echo "$t,$mem,$cpu,$inst" >> "$SAMPLES"
    prev_cpu=$cpu; prev_t=$t
  done
) & SAMPLER=$!

# Run validate with its leader migrated into the meter cgroup so every child
# (cargo build, test harness, DAG runner, guests) inherits it.
# Bound validate so the footprint report ALWAYS runs (a timed-out run still gives
# a valid peak-so-far). Override with VALIDATE_TIMEOUT (seconds).
VT="${VALIDATE_TIMEOUT:-2700}"
wall_start=$(date +%s.%N)
bash -c "echo \$\$ > '$CG/cgroup.procs'; cd '$HERMIT'; exec timeout ${VT}s ./scripts/validate.rs $VARGS" \
  > "$OUT/validate.stdout" 2> "$OUT/validate.stderr"
RC=$?
wall_end=$(date +%s.%N)
kill "$SAMPLER" 2>/dev/null; wait "$SAMPLER" 2>/dev/null

# Peak RSS = max summed-RSS sample (memory.peak is unusable without memory-controller
# delegation on this populated scope). 2s sampling of a multi-minute run is ample.
peak_rss=$(awk -F, 'NR>1&&$2>m{m=$2}END{printf "%d",m}' "$SAMPLES")
total_cpu=$(awk '/^usage_usec/{print $2}' "$CG/cpu.stat" 2>/dev/null || echo 0)
wall=$(awk -v a="$wall_start" -v b="$wall_end" 'BEGIN{printf "%.1f",b-a}')
mean_cores=$(awk -v c="$total_cpu" -v w="$wall" 'BEGIN{printf "%.2f", (w>0)?c/1e6/w:0}')
peak_cores=$(awk -F, 'NR>1&&$4>m{m=$4}END{printf "%.2f",m}' "$SAMPLES")

echo "==== VALIDATE FOOTPRINT ===="
echo "exit_code:        $RC   (footprint is valid regardless of pass/fail)"
echo "wall_s:           $wall"
echo "cpu_seconds:      $(awk -v c="$total_cpu" 'BEGIN{printf "%.1f",c/1e6}')"
echo "mean_cores:       $mean_cores   (cpu_seconds / wall)"
echo "peak_cores:       $peak_cores   (max 2s-window usage_usec rate)"
echo "peak_rss_bytes:   $peak_rss"
echo "peak_rss_GiB:     $(awk -v b="$peak_rss" 'BEGIN{printf "%.2f",b/1073741824}')"
echo "samples_csv:      $SAMPLES"
echo "============================"
rmdir "$CG" 2>/dev/null || true
