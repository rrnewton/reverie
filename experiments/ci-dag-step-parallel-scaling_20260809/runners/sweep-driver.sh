#!/usr/bin/env bash
# Per-step parallel-scaling driver.
#
# The MEASUREMENT is taken by safe-ci-dag-runner's own `sweep` subcommand, which appends every
# sample to the machine-scoped profile store. This script only SEQUENCES those invocations; it does
# not time anything itself and does not post-process a wall clock. That keeps the owner's
# "the ci-dag runner captures all the data" requirement intact -- there is no external stopwatch.
#
# Why one invocation per width instead of `--jobs LO..HI`:
#   1. `_cmd_sweep` iterates range(lo, hi+1) -- LINEAR. A 1..64 sweep is 64 points, most of them
#      uninteresting. We want geometric widths (1,2,4,...).
#   2. `_cmd_sweep` runs widths in strictly ASCENDING order. On a shared box whose ambient load
#      drifts, ascending order aliases load drift onto the width axis and MANUFACTURES an apparent
#      regression at high widths. We randomize the width order inside every replicate so drift
#      cannot correlate with width.
#
# cgroup delegation: an agent sandbox scope exposes only `io memory pids` -- no `cpu` controller --
# so cpu.max cannot be set and the sweep's width would be INERT. `systemd-run --user` yields
# `cpuset cpu io memory pids`, so each sweep runs inside a transient user unit. This is the same
# admitted producer path ci-hub validate-run uses.
set -euo pipefail

RUNNER=${RUNNER:?set RUNNER to the safe-ci-dag-runner entry point}
CHECKOUT=${CHECKOUT:?set CHECKOUT to the hermit worktree}
DAG=${DAG:?set DAG to the dag file (relative to CHECKOUT)}
STORE=${STORE:?set STORE to the profile store dir}
STEP=${STEP:?set STEP to group.job}
WIDTHS=${WIDTHS:-"1 2 4 8 16 32 64"}
REPS=${REPS:-3}
QUOTA=${QUOTA:-31600}   # CPUQuota% for the outer unit; must exceed the widest width*100

mkdir -p "$STORE"
log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }

for rep in $(seq 1 "$REPS"); do
  # Randomize width order within this replicate (shuf seeded by rep for reproducibility).
  order=$(printf '%s\n' $WIDTHS | shuf --random-source=<(yes "$rep"))
  log "replicate $rep/$REPS  order: $(echo $order | tr '\n' ' ')"
  for j in $order; do
    unit="scalesweep-$$-r${rep}-j${j}"
    # Record ambient load at the moment of launch alongside the runner's own per-sample capture.
    printf '%s,%s,%s,%s,%s\n' "$(date -u +%FT%TZ)" "$STEP" "$rep" "$j" "$(cut -d' ' -f1 /proc/loadavg)" \
      >> "$STORE/driver-order.csv"
    if ! timeout 1800 systemd-run --user --pipe --quiet --collect --unit="$unit" \
        --property=Delegate=yes --property="CPUQuota=${QUOTA}%" \
        --working-directory="$CHECKOUT" \
        python3 "$RUNNER" sweep --dag "$DAG" --step "$STEP" \
          --jobs "${j}..${j}" --perf-dir "$STORE" >/dev/null 2>"$STORE/last-error.txt"; then
      log "  FAILED step=$STEP rep=$rep j=$j (see $STORE/last-error.txt)"
      tail -3 "$STORE/last-error.txt" >&2 || true
    else
      log "  ok j=$j"
    fi
  done
done
log "done step=$STEP widths=[$WIDTHS] reps=$REPS"
