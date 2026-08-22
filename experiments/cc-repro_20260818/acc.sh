#!/usr/bin/env bash
# acc.sh <tag> <order|order_long> <seed|none> [append-extra]
cd /home/newton/work/dev-hermit/experiments/cc-repro_20260818
export HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/cc-coherent-timebase/target/release/hermit
APPEND=${4-"tsc=nowatchdog no_timer_check"}
CMD='grep -E "^LOC" /proc/interrupts; cat /proc/uptime; '"$2"'; grep -E "^LOC" /proc/interrupts; cat /proc/uptime; echo TICK"DONE"'
if [[ $3 == none ]]; then
  timeout 900 ./cc_qemu.py resume --tag "$1" --timeout 800 --settle 5 --append-extra "$APPEND" --command "$CMD" --expect TICKDONE
else
  timeout 900 ./cc_qemu.py resume --tag "$1" --seed "$3" --timeout 800 --settle 5 --append-extra "$APPEND" --command "$CMD" --expect TICKDONE
fi
grep -aE "LOC:|ORDER|^[0-9]+\.[0-9]+ " "res-$1.capture" | grep -v grep
