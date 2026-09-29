#!/bin/bash
# Two more reverted-checkout negative controls for asyncssl seed 11, after the
# first control was nondeterministic. Refuses unless the stress ELF is still the
# unfixed ELF.
set -u
c=asyncssl
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
P=$E3/stress/$c/postfix
pre() { python3 -c "import json;print(json.load(open('$E3/stress/prefix-elves.json'))['$c']['$1'])"; }
ELF=$(pre elf); UNFIXED=$(pre sha256)
[ "$(sha256sum "$ELF" | cut -d' ' -f1)" = "$UNFIXED" ] || { echo "stress ELF is not the unfixed ELF" > $P/revert-extra.errors; exit 1; }
for i in 1 2; do
  $E3/witness/witness.sh --case $c --template fail --seed 11 --elf "$ELF" --expect-elf-sha256 $UNFIXED \
    --out $P/revert-control-seed-011-repeat-$i --note "reverted-checkout negative control repeat $i (unfixed FAIL seed 11)" >> $P/run.out 2>&1
  echo "revert-control-seed-011-repeat-$i rc=$?" >> $P/run.rcs
done
