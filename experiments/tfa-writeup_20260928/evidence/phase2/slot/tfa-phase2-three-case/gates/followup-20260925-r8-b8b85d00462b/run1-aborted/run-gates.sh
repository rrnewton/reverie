#!/bin/bash
# Exact-head gates for b8b85d00462b; each gate is its own unpiped command with .out/.rc.
# Every Python run gets a fresh PYTHONPYCACHEPREFIX so a mutant's bytecode can never be
# reused by a later run (equal-size mutants written in the same second share pyc keys).
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
HEAD12=b8b85d00462b
G=$E3/gates/followup-20260925-r8-$HEAD12
G7=$E3/gates/followup-20260925-r7-c1f8817a5ebb
S=$E3/gates/followup-r8b-sensitivity
R=/home/newton/work/tfa/fbsource
T=$R/fbcode/testinfra/tfa
PAR=$R/buck-out/v2/art/fbcode/f131b89c4a309a34/testinfra/tfa/__tfa_phase2__/tfa_phase2.par
M=$T/cases.v1.json
LT=$R/buck-out/v2/art/fbcode/c62dfa3398b37794/testinfra/tfa/tests/__tfa_phase2_test__/tfa_phase2_test#link-tree
PY=/usr/local/fbcode/platform010/bin/python3.12
export TMPDIR=$E3/tmpdir
cd $R
gate() { local n=$1; shift; "$@" > $G/$n.out 2>&1; echo $? > $G/$n.rc; }
pygate() { local n=$1; shift; PYTHONPYCACHEPREFIX=$G/pyc/$n "$@" > $G/$n.out 2>&1; echo $? > $G/$n.rc; }
followup_test() { local n=$1; (cd "$LT" && PYTHONPYCACHEPREFIX=$G/pyc/$n $PY -m unittest -v testinfra.tfa.tests.test_prototype.FollowupTest) > $G/$n.out 2>&1; echo $? > $G/$n.rc; }
gate commit-identity sl log -r '.^::.' -T '{node} {p1node}\n{desc}\n---\n' --reason "record gate head identity - sl help log"
gate sl-status-pre sl status --reason "confirm clean checkout before gates - sl help status"
gate arc-lint arc lint fbcode/testinfra/tfa/followup.py fbcode/testinfra/tfa/report.py fbcode/testinfra/tfa/tests/test_prototype.py fbcode/testinfra/tfa/README.md
gate pyre arc pyre check-changed-targets
gate build-cli buck2 build --show-full-output fbcode//testinfra/tfa:tfa_phase2
sha256sum $PAR > $G/cli-par.sha256
sha256sum $(readlink -f $LT/testinfra/tfa/followup.py) $T/followup.py $(readlink -f $LT/testinfra/tfa/report.py) $T/report.py > $G/cli-linktree-sources.sha256
gate tfa_phase2_test buck2 test fbcode//testinfra/tfa/tests:tfa_phase2_test
mkdir -p $G/render
for c in thrift asyncssl httpcoro; do
  pygate render-$c $PAR render-evidence --manifest $M --case-id $c --record-dir $E3/authoritative/$c --output-dir $G/render/$c-initial --relocation-summary $E3/relocated-v2/$c/summary.json
  pygate render-followup-$c $PAR render-followup --manifest $M --case-id $c --ledger $E3/followup/$c/ledger.json --output-dir $G/render/$c-followup
done
{ rc=0
  for c in thrift asyncssl httpcoro; do
    for f in $(ls $E3/stages/$c/stage1-2-relocated); do cmp $G/render/$c-initial/$f $E3/stages/$c/stage1-2-relocated/$f && echo "cmp 0 $c initial $f" || rc=1; done
    for f in followup-comment.md followup-runs.json; do cmp $G/render/$c-followup/$f $E3/stages/$c/stage4-followup-hypothesis-r3/$f && echo "cmp 0 $c followup $f vs stage4-r3" || rc=1; done
    cmp $G/render/$c-followup/followup-runs.json $E3/stages/$c/stage3-followup/followup-runs.json && echo "cmp 0 $c followup-runs.json vs stage3" || rc=1
  done; exit $rc; } > $G/render-cmp.out 2>&1; echo $? > $G/render-cmp.rc
cp $T/followup.py $G/negctl-followup.py.head
sha256sum $T/followup.py > $G/negctl-followup.head.sha256
cp $G7/negctl-mutate-followup.py $G/negctl-mutate-followup.py
cp $S/negctl-mutate-plaintext.py $G/negctl-mutate-plaintext.py
for m in no-artifact-binding no-patched-fail no-wall-evidence no-phase-distinct no-guest-check no-stdout-binding trust-recorded-label no-failing-seed-rule no-witness-caveat; do
  gate negctl-$m.mutate $PY $G/negctl-mutate-followup.py $T/followup.py $m
  followup_test negctl-$m
  cp $G/negctl-followup.py.head $T/followup.py
  gate negctl-$m.restore sha256sum -c $G/negctl-followup.head.sha256
done
for m in no-control-filter no-backtick-escape no-length-cap no-text-fence reread-report; do
  gate negctl-$m.mutate $PY $G/negctl-mutate-plaintext.py $T/followup.py $m
  followup_test negctl-$m
  cp $G/negctl-followup.py.head $T/followup.py
  gate negctl-$m.restore sha256sum -c $G/negctl-followup.head.sha256
done
followup_test negctl-restored-baseline
for rev in c1f8817a5ebb 2c30cbedb0ba; do
  sl cat -r $rev fbcode/testinfra/tfa/followup.py --reason "fetch earlier followup for sensitivity gate - sl help cat" > $G/sensitivity-$rev-followup.py
  { cat $G/sensitivity-$rev-followup.py; printf '\nAGENT_TEXT_LIMIT: int = 1000\n'; } > $T/followup.py
  followup_test sensitivity-$rev-renderer
  cp $G/negctl-followup.py.head $T/followup.py
  gate sensitivity-$rev.restore sha256sum -c $G/negctl-followup.head.sha256
done
mkdir -p $G/negctl-real
for n in swap-phases patched-unfixed-sha; do
  cp $G7/negctl-real/$n.json $G/negctl-real/$n.json
  PYTHONPYCACHEPREFIX=$G/pyc/negctl-real-$n $PAR render-followup --manifest $M --case-id thrift --ledger $G/negctl-real/$n.json --output-dir $G/negctl-real/$n-out > $G/negctl-real/$n.out 2>&1; echo $? > $G/negctl-real/$n.rc
done
touch $G/PRE-CONTAINMENT-DONE
free -m > $G/free-pre-containment.out
gate containment_test buck2 test fbcode//testinfra/tfa/tests:tfa_production_containment_test
gate sl-status-post sl status --reason "confirm clean checkout after gates - sl help status"
touch $G/ALL-GATES-DONE
