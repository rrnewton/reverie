#!/bin/bash
# Recovery continuation after controller OOM-kill. One Hermit replay at a time.
# Never reuses an attempt directory; the interrupted httpcoro fail attempt-2 is preserved.
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
W=$E3/witness/witness.sh
elf_of() { python3 -c "import json;print(json.load(open('$E3/stress/prefix-elves.json'))['$1']['elf'])"; }
seed_of() { python3 -c "import json;print(json.load(open('$E3/relocated-v2/$1/summary.json'))['witness']['$2']['seed'])"; }
fresh_of() { python3 -c "
import json
m=json.load(open('/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/cases.v1.json'))
print(*[c['fresh_stress_seeds'] for c in m['cases'] if c['case_id']=='$1'][0])"; }
fresh_prefix() {
  c=$1; elf=$(elf_of $c); sha=$(sha256sum "$elf" | cut -d' ' -f1)
  [ "$sha" = "$(python3 -c "import json;print(json.load(open('$E3/stress/prefix-elves.json'))['$c']['sha256'])")" ] || { echo "$c prefix ELF changed: $sha" >> $E3/stress/resume.errors; return 1; }
  root=$E3/stress/$c/prefix-fresh; mkdir -p $root; echo "$sha  $elf" >> $root/elf.sha256
  for s in $(fresh_of $c); do
    $W --case $c --template pass --seed $s --elf "$elf" --expect-elf-sha256 $sha \
      --out $root/fresh-seed-$(printf %03d $s) --note "prefix fresh stress seed (unfixed control)" >> $root/run.out 2>&1
    echo "fresh $s rc=$?" >> $root/run.rcs
  done
  touch $root/DONE
}
# 1. complete httpcoro prefix witnesses
c=httpcoro; elf=$(elf_of $c); sha=$(sha256sum "$elf" | cut -d' ' -f1); root=$E3/stress/$c/prefix
echo "$sha  $elf" >> $root/elf.sha256
$W --case $c --template fail --elf "$elf" --expect-elf-sha256 $sha \
  --out $root/fail-witness-seed-$(printf %03d $(seed_of $c fail))-attempt-3 --note "prefix FAIL-witness schedule (resumed after interrupted attempt-2)" >> $root/run.out 2>&1
echo "fail 3 rc=$?" >> $root/run.rcs
$W --case $c --template pass --elf "$elf" --expect-elf-sha256 $sha \
  --out $root/pass-witness-seed-$(printf %03d $(seed_of $c pass))-attempt-2 --note "prefix PASS-witness schedule" >> $root/run.out 2>&1
echo "pass 2 rc=$?" >> $root/run.rcs
touch $root/DONE-resumed
# 2. unfixed fresh-seed controls
for c in thrift asyncssl httpcoro; do fresh_prefix $c; done
touch $E3/stress/resume.DONE
