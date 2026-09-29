#!/bin/bash
# Controller post-fix validation for one case in its isolated stress checkout:
# apply the candidate patch, rebuild with the manifest build_argv, replay the
# patched ELF, run the manifest test_argv and lint, then revert, rebuild, and
# prove the unfixed ELF (and its FAIL) returns. One Hermit replay at a time.
set -u
c=$1
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
W=$E3/witness/witness.sh
D=/home/newton/work/tfa/phase2-cases/stress/$c
P=$E3/stress/$c/postfix
mkdir -p $P
PATCH=$E3/investigation/$c/agent-output/patch.diff
man() { python3 -c "
import json,shlex;m=json.load(open('/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/cases.v1.json'))
print(shlex.join([x for x in m['cases'] if x['case_id']=='$c'][0]['$1'][1:]))"; }
pre() { python3 -c "import json;print(json.load(open('$E3/stress/prefix-elves.json'))['$c']['$1'])"; }
ELF=$(pre elf); UNFIXED=$(pre sha256)
build() {  # label
  (cd $D && eval buck $(man build_argv)) > $P/build-$1.out 2> $P/build-$1.err
  echo $? > $P/build-$1.rc
  sha256sum "$ELF" | cut -d' ' -f1 > $P/build-$1.elf.sha256
  python3 - "$P/build-$1.out" "$ELF" > $P/build-$1.elf-path-check <<'PY'
import json,sys
outs=[p for v in json.load(open(sys.argv[1])).values() for p in ([v] if isinstance(v,str) else [])]
print("ELF_PATH_MATCH" if sys.argv[2] in outs else "ELF_PATH_MISMATCH", outs)
PY
}
replay() {  # template seed label note sha
  $W --case $c --template $1 --seed $2 --elf "$ELF" --expect-elf-sha256 $5 \
    --out $P/$3 --note "$4" >> $P/run.out 2>&1
  echo "$3 rc=$?" >> $P/run.rcs
}
sha256sum $PATCH > $P/patch.sha256
(cd $D && sl status --reason "confirm clean stress checkout before candidate patch - sl help status") > $P/status-before.out 2>&1
(cd $D && sl import --no-commit $PATCH --reason "apply candidate patch in isolated stress checkout - sl help import") > $P/import.out 2>&1
echo $? > $P/import.rc
[ "$(cat $P/import.rc)" = 0 ] || { echo IMPORT_FAILED > $P/ABORTED; exit 1; }
(cd $D && sl status --reason "record patched stress checkout state - sl help status" && sl diff --reason "record applied candidate diff - sl help diff") > $P/applied.out 2>&1
build patched
[ "$(cat $P/build-patched.rc)" = 0 ] || { echo BUILD_FAILED > $P/ABORTED; }
PATCHED=$(cat $P/build-patched.elf.sha256)
if [ ! -e $P/ABORTED ] && [ "$PATCHED" != "$UNFIXED" ]; then
  while read -r tmpl seed label note; do replay $tmpl $seed $label "$note" $PATCHED; done < $E3/stress/$c/postfix-plan.txt
  (cd $D && eval buck $(man test_argv)) > $P/unit.out 2>&1; echo $? > $P/unit.rc
  (cd $D && arc lint $(sed -n 's|^+++ b/||p' $PATCH)) > $P/lint.out 2>&1; echo $? > $P/lint.rc
elif [ ! -e $P/ABORTED ]; then echo PATCHED_ELF_UNCHANGED > $P/ABORTED; fi
(cd $D && sl revert --all --no-backup --reason "restore unfixed stress checkout after post-fix validation - sl help revert") > $P/revert.out 2>&1
(cd $D && sl status --reason "confirm stress checkout restored - sl help status") > $P/status-after.out 2>&1
build reverted
if [ "$(cat $P/build-reverted.elf.sha256)" = "$UNFIXED" ]; then
  read -r tmpl seed label note < $E3/stress/$c/revert-plan.txt
  replay $tmpl $seed $label "$note" $UNFIXED
else echo "reverted ELF differs from unfixed ELF" > $P/revert.errors; fi
touch $P/DONE
