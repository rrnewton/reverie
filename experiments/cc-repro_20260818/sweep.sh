#!/usr/bin/env bash
# sweep.sh <tag> <seed|none> [timeout]
# One resume run of the order detector. Kept in a file so callers never put the
# process-matching pattern into their own command line.
cd /home/newton/work/dev-hermit/experiments/cc-repro_20260818
APPEND="tsc=nowatchdog no_timer_check"
tag=$1; seed=$2; tmo=${3:-800}
if [[ $seed == none ]]; then
  timeout $((tmo+60)) ./cc_qemu.py resume --tag "$tag" --hermit --timeout "$tmo" --settle 5 \
    --append-extra "$APPEND" --command order --expect ORDER
else
  timeout $((tmo+60)) ./cc_qemu.py resume --tag "$tag" --seed "$seed" --timeout "$tmo" --settle 5 \
    --append-extra "$APPEND" --command order --expect ORDER
fi
