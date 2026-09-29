#!/bin/bash
# Create one writable, per-investigator Eden checkout per case at the manifest source revision.
# The case checkouts under phase2-cases/<case> stay read-only by policy.
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
REV=7f239d5b37dfbecf32fed144468da461f998add5
L=$E3/investigation/logs
mkdir -p $L /home/newton/work/tfa/phase2-cases/investigators
for c in thrift asyncssl httpcoro; do
  D=/home/newton/work/tfa/phase2-cases/investigators/$c
  date -u +%FT%TZ > $L/clone-$c.started
  sl clone --eden fb:fbsource $D --reason "create isolated TFA $c investigator checkout - sl help clone" > $L/clone-$c.out 2>&1
  rc=$?
  if [ $rc -eq 0 ]; then
    (cd $D && sl goto $REV --reason "check out TFA phase-two manifest source revision - sl help goto") >> $L/clone-$c.out 2>&1
    rc=$?
  fi
  echo $rc > $L/clone-$c.rc
  date -u +%FT%TZ > $L/clone-$c.finished
done
touch $L/clone.DONE
