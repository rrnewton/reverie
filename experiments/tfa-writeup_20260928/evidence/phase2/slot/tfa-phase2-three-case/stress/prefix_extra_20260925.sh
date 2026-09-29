#!/bin/bash
# Extra unfixed controls before any patch is applied: repeats of path-local FAIL
# seeds and a bounded discriminator search. One Hermit replay at a time.
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
W=$E3/witness/witness.sh
elf_of() { python3 -c "import json;print(json.load(open('$E3/stress/prefix-elves.json'))['$1']['$2'])"; }
one() {  # case template seed label note
  c=$1; elf=$(elf_of $c elf); want=$(elf_of $c sha256); sha=$(sha256sum "$elf" | cut -d' ' -f1)
  [ "$sha" = "$want" ] || { echo "$c prefix ELF changed: $sha" >> $E3/stress/prefix-extra.errors; return 1; }
  root=$E3/stress/$c/prefix-extra; mkdir -p $root
  $W --case $c --template $2 --seed $3 --elf "$elf" --expect-elf-sha256 $sha \
    --out $root/$4 --note "$5" >> $root/run.out 2>&1
  echo "$4 rc=$?" >> $root/run.rcs
}
for n in 1 2; do one thrift fail 3 discriminator-seed-003-repeat-$n "unfixed repeat of path-local FAIL seed 3"; done
for s in 6 7 8 9 10 11; do one thrift fail $s search-seed-$(printf %03d $s) "unfixed bounded discriminator search"; done
for n in 3 4; do one asyncssl fail 11 fail-witness-seed-011-attempt-$n "unfixed FAIL-witness repeat"; done
one asyncssl pass 15 fresh-seed-015-repeat-1 "unfixed repeat of OTHER_FAILURE fresh seed 15"
touch $E3/stress/prefix-extra.DONE
