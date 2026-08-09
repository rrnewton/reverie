#!/bin/bash
# sol-timing A/B harness: uniform wall+CPU accounting for both validate drivers.
# RUSAGE_CHILDREN via /usr/bin/time -v is the SAME metric validate.sh's own
# `times` builtin reports, so the two sides are measured identically.
set -u
label=$1; shift
slot=/home/newton/work/dev-hermit/worktrees/soltiming/hermit
out=/home/newton/work/dev-hermit/ignored/soltiming
cd "$slot" || exit 9
{
  echo "=== $label ==="
  echo "started_utc=$(date -u +%FT%TZ)"
  echo "head=$(git rev-parse HEAD)"
  echo "dirty=$(git status --porcelain | wc -l)"
  echo "loadavg=$(cut -d' ' -f1-3 /proc/loadavg)"
  echo "cmd=$*"
} > "$out/$label.meta"
/usr/bin/time -v -o "$out/$label.time" "$@" > "$out/$label.log" 2>&1
rc=$?
{
  echo "rc=$rc"
  echo "finished_utc=$(date -u +%FT%TZ)"
  echo "loadavg_end=$(cut -d' ' -f1-3 /proc/loadavg)"
} >> "$out/$label.meta"
exit $rc
