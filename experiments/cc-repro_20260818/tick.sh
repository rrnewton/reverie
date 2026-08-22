#!/usr/bin/env bash
cd /home/newton/work/dev-hermit/experiments/cc-repro_20260818
APPEND="tsc=nowatchdog no_timer_check"
CMD='grep -E "^LOC" /proc/interrupts; cat /proc/uptime; order_long; grep -E "^LOC" /proc/interrupts; cat /proc/uptime; echo TICK"DONE"'
timeout 900 ./cc_qemu.py resume --tag "$1" ${2:+--seed $2} --timeout 800 --settle 5 \
  --append-extra "$APPEND" --command "$CMD" --expect TICKDONE
