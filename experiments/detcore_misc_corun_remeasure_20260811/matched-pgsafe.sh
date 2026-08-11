#!/usr/bin/env bash
# matched-pgsafe.sh <conc> <timeout_s> <waves> <label:binpath>...
#
# EXACTLY the experimental design of the sibling `matched.sh` -- matched-load
# co-scheduling, single-shot instances, trinary PASS/FAIL/FLAKY per (label,wave),
# calibrator-gated wave validity -- with ONE change, and it is a safety change,
# not a design change:
#
#   `matched.sh` reaps strays with `pkill -9 -x "$(basename $BIN)"`. dev-hermit
#   Hard Invariant 15 forbids any pattern/name-matched kill on this box: up to
#   eighteen agents share it, `comm` is truncated to 15 chars, and a validate's
#   own `test.detcore_misc` node runs a binary whose basename is also
#   `tests_misc-<hash>`. That pkill can therefore kill a sibling agent's live
#   test run. Here each instance is launched into ITS OWN process group (bash
#   monitor mode gives every background job a fresh pgid, and `$!` is that
#   group's leader), the pgids are recorded as they are created, and the reap
#   signals ONLY those negative pgids -- processes this script provably started
#   seconds earlier. Nothing is matched by name.
#
# Everything measurable is unchanged: same TEST, same `--exact --test-threads=1`,
# same round-robin interleave so all labels share one instantaneous load window,
# same exit-code classification (124 == the `timeout` fired == a hang).
#
# A wave counts ONLY if the known-bad calibrator label comes back FAIL or FLAKY.
# A calibrator that returns all-pass means the wave was under-powered and proves
# nothing about the other labels.
set -uo pipefail
EXP="$(cd "$(dirname "$0")" && pwd)"
CONC="$1"; TMO="$2"; WAVES="$3"; shift 3
TEST="${PROBE_TEST:-vfork::vfork_parent_resumes_after_child_exec}"
STAMP="$(date +%Y%m%dT%H%M%S)"
WD="$EXP/ignored/matched-pgsafe/$STAMP"
mkdir -p "$WD"

specs=("$@")
labels=(); declare -A BIN
for spec in "${specs[@]}"; do
  l="${spec%%:*}"; b="${spec#*:}"
  labels+=("$l"); BIN[$l]="$b"
  [ -x "$b" ] || { echo "MISSING BIN $l: $b" >&2; exit 3; }
done

classify() { # <file> -> "CLASS hangs passes other total"
  local f="$1" h p o t c
  h=$(grep -cx 124 "$f" 2>/dev/null); h=${h:-0}
  p=$(grep -cx 0 "$f" 2>/dev/null); p=${p:-0}
  t=$(wc -l < "$f" 2>/dev/null); t=${t:-0}
  o=$((t - h - p))
  if   [ "$t" -eq 0 ]; then c=NORUN
  elif [ "$p" -eq "$t" ]; then c=PASS
  elif [ "$p" -eq 0 ]; then c=FAIL
  else c=FLAKY; fi
  echo "$c $h $p $o $t"
}

echo "=== MATCHED-PGSAFE conc=$CONC timeout=${TMO}s waves=$WAVES labels=${labels[*]} ==="
echo "test=$TEST"
echo "nproc=$(nproc) load.start=$(cat /proc/loadavg)"
echo "results: $WD"

# Monitor mode ON: every background job below becomes its own process-group
# leader, so `$!` IS a pgid we created and may signal (Invariant 15).
set -m
for w in $(seq 1 "$WAVES"); do
  for l in "${labels[@]}"; do : > "$WD/w$w.$l"; done
  pgids=()
  for i in $(seq 1 "$CONC"); do
    for l in "${labels[@]}"; do
      { timeout "$TMO" "${BIN[$l]}" "$TEST" --exact --test-threads=1 >/dev/null 2>&1
        echo $? >> "$WD/w$w.$l"; } &
      pgids+=("$!")
    done
  done
  wait
  # Reap only the groups this wave created. Not by name, not by pattern.
  for g in "${pgids[@]}"; do kill -9 -- "-$g" 2>/dev/null; done
  line="wave$w load=$(cut -d' ' -f1 /proc/loadavg) |"
  for l in "${labels[@]}"; do
    read -r c h p o t <<<"$(classify "$WD/w$w.$l")"
    line="$line  $l:$c(hang $h/$t)"
  done
  echo "$line"
done
set +m
echo "load.end=$(cat /proc/loadavg)"
echo "results: $WD"
