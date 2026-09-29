#!/bin/bash
# Recreate clean read-only-by-policy case checkouts at the manifest source revision.
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
REV=7f239d5b37dfbecf32fed144468da461f998add5
for c in httpcoro thrift asyncssl; do
  D=/home/newton/work/tfa/phase2-cases/$c
  date -u +%FT%TZ > $E3/logs/clone-$c.started
  sl clone --eden fb:fbsource $D --reason "recreate clean TFA phase-two $c case checkout - sl help clone" > $E3/logs/clone-$c.out 2>&1
  rc=$?
  if [ $rc -eq 0 ]; then
    (cd $D && sl goto $REV --reason "check out TFA phase-two manifest source revision - sl help goto") >> $E3/logs/clone-$c.out 2>&1
    rc=$?
  fi
  echo $rc > $E3/logs/clone-$c.rc
  date -u +%FT%TZ > $E3/logs/clone-$c.finished
done
touch $E3/logs/clone.DONE
