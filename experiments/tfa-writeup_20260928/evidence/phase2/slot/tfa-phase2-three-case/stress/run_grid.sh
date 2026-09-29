#!/bin/bash
# Usage: run_grid.sh <phase: prefix|postfix> <with-fresh: 0|1> <case>...
# One Hermit replay at a time. Every attempt gets a new directory and a fresh owned cwd.
# The ELF is always the stress-checkout build; its sha256 is re-read and bound per phase.
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
phase=$1; fresh=$2; shift 2
for c in "$@"; do
  elf=$(python3 -c "import json;print(json.load(open('$E3/stress/prefix-elves.json'))['$c']['elf'])")
  sha=$(sha256sum "$elf" | cut -d' ' -f1)
  root=$E3/stress/$c/$phase
  mkdir -p $root
  echo "$sha  $elf" >> $root/elf.sha256
  seeds=$(python3 -c "
import json
s=json.load(open('$E3/relocated-v2/$c/summary.json'))['witness']
print(s['fail']['seed'], s['pass']['seed'])")
  set -- $seeds; fs=$1; ps=$2
  for attempt in 1 2; do
    $E3/witness/witness.sh --case $c --template fail --elf "$elf" --expect-elf-sha256 $sha \
      --out $root/fail-witness-seed-$(printf %03d $fs)-attempt-$attempt --note "$phase FAIL-witness schedule" >> $root/run.out 2>&1
    echo "fail $attempt rc=$?" >> $root/run.rcs
    $E3/witness/witness.sh --case $c --template pass --elf "$elf" --expect-elf-sha256 $sha \
      --out $root/pass-witness-seed-$(printf %03d $ps)-attempt-$attempt --note "$phase PASS-witness schedule" >> $root/run.out 2>&1
    echo "pass $attempt rc=$?" >> $root/run.rcs
  done
  if [ "$fresh" = 1 ]; then
    for s in $(python3 -c "
import json
m=json.load(open('/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/cases.v1.json'))
print(*[c['fresh_stress_seeds'] for c in m['cases'] if c['case_id']=='$c'][0])"); do
      $E3/witness/witness.sh --case $c --template pass --seed $s --elf "$elf" --expect-elf-sha256 $sha \
        --out $root/fresh-seed-$(printf %03d $s) --note "$phase fresh stress seed" >> $root/run.out 2>&1
      echo "fresh $s rc=$?" >> $root/run.rcs
    done
  fi
  touch $root/DONE
done
