#!/usr/bin/env bash
# sweep2.sh <tag> <seed|none> <timeout> [extra hermit flags...]
# Same as sweep.sh but forwards extra hermit flags, e.g. --max-timeslice=2000000.
cd /home/newton/work/dev-hermit/experiments/cc-repro_20260818
APPEND="tsc=nowatchdog no_timer_check"
tag=$1; seed=$2; tmo=$3; shift 3
extra="$*"
if [[ $seed == none ]]; then
  timeout $((tmo+60)) ./cc_qemu.py resume --tag "$tag" --hermit --hermit-extra="$extra" \
    --timeout "$tmo" --settle 5 --append-extra "$APPEND" --command order --expect ORDER
else
  timeout $((tmo+60)) ./cc_qemu.py resume --tag "$tag" --seed "$seed" --hermit-extra="$extra" \
    --timeout "$tmo" --settle 5 --append-extra "$APPEND" --command order --expect ORDER
fi
