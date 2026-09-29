#!/bin/bash
# Exact-head gates for 2c30cbedb0ba; each gate is its own unpiped command with .out/.rc.
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
G=$E3/gates/followup-20260925-r8-2c30cbedb0ba
G7=$E3/gates/followup-20260925-r7-c1f8817a5ebb
R=/home/newton/work/tfa/fbsource
T=$R/fbcode/testinfra/tfa
PAR=$R/buck-out/v2/art/fbcode/f131b89c4a309a34/testinfra/tfa/__tfa_phase2__/tfa_phase2.par
M=$T/cases.v1.json
LT=$R/buck-out/v2/art/fbcode/c62dfa3398b37794/testinfra/tfa/tests/__tfa_phase2_test__/tfa_phase2_test#link-tree
PY=/usr/local/fbcode/platform010/bin/python3.12
export TMPDIR=$E3/tmpdir
cd $R
gate() { local n=$1; shift; "$@" > $G/$n.out 2>&1; echo $? > $G/$n.rc; }
gate commit-identity sl log -r '.^::.' -T '{node} {p1node}\n{desc}\n---\n' --reason "record gate head identity - sl help log"
gate sl-status-pre sl status --reason "confirm clean checkout before gates - sl help status"
gate arc-lint arc lint fbcode/testinfra/tfa/followup.py fbcode/testinfra/tfa/tests/test_prototype.py fbcode/testinfra/tfa/README.md
gate pyre arc pyre check-changed-targets
gate build-cli buck2 build --show-full-output fbcode//testinfra/tfa:tfa_phase2
sha256sum $PAR > $G/cli-par.sha256
sha256sum $(readlink -f $LT/testinfra/tfa/followup.py) $T/followup.py > $G/cli-linktree-sources.sha256
gate tfa_phase2_test buck2 test fbcode//testinfra/tfa/tests:tfa_phase2_test
mkdir -p $G/render
for c in thrift asyncssl httpcoro; do
  gate render-$c $PAR render-evidence --manifest $M --case-id $c --record-dir $E3/authoritative/$c --output-dir $G/render/$c-initial --relocation-summary $E3/relocated-v2/$c/summary.json
  gate render-followup-$c $PAR render-followup --manifest $M --case-id $c --ledger $E3/followup/$c/ledger.json --output-dir $G/render/$c-followup
done
{ rc=0
  for c in thrift asyncssl httpcoro; do
    for f in $(ls $E3/stages/$c/stage1-2-relocated); do cmp $G/render/$c-initial/$f $E3/stages/$c/stage1-2-relocated/$f && echo "cmp 0 $c initial $f" || rc=1; done
    for f in followup-comment.md followup-runs.json; do cmp $G/render/$c-followup/$f $E3/stages/$c/stage4-followup-hypothesis-r2/$f && echo "cmp 0 $c followup $f vs stage4-r2" || rc=1; done
    cmp $G/render/$c-followup/followup-runs.json $E3/stages/$c/stage3-followup/followup-runs.json && echo "cmp 0 $c followup-runs.json vs stage3" || rc=1
  done; exit $rc; } > $G/render-cmp.out 2>&1; echo $? > $G/render-cmp.rc
cp $T/followup.py $G/negctl-followup.py.head
(cd $T && sha256sum $T/followup.py) > $G/negctl-followup.head.sha256
cp $G7/negctl-mutate-followup.py $G/negctl-mutate-followup.py
for m in no-artifact-binding no-patched-fail no-wall-evidence no-phase-distinct no-guest-check no-stdout-binding trust-recorded-label no-failing-seed-rule no-witness-caveat; do
  gate negctl-$m.mutate $PY $G/negctl-mutate-followup.py $T/followup.py $m
  (cd "$LT" && $PY -m unittest testinfra.tfa.tests.test_prototype.FollowupTest) > $G/negctl-$m.out 2>&1; echo $? > $G/negctl-$m.rc
  cp $G/negctl-followup.py.head $T/followup.py
  gate negctl-$m.restore sha256sum -c $G/negctl-followup.head.sha256
done
(cd "$LT" && $PY -m unittest testinfra.tfa.tests.test_prototype.FollowupTest) > $G/negctl-restored-baseline.out 2>&1; echo $? > $G/negctl-restored-baseline.rc
sl cat -r 'c1f8817a5ebb' fbcode/testinfra/tfa/followup.py --reason "fetch parent followup for sensitivity gate - sl help cat" > $G/sensitivity-parent-followup.py
cp $G/sensitivity-parent-followup.py $T/followup.py
(cd "$LT" && $PY -m unittest -v testinfra.tfa.tests.test_prototype.FollowupTest) > $G/sensitivity-parent-renderer.out 2>&1; echo $? > $G/sensitivity-parent-renderer.rc
cp $G/negctl-followup.py.head $T/followup.py
gate sensitivity.restore sha256sum -c $G/negctl-followup.head.sha256
mkdir -p $G/negctl-real
for n in swap-phases patched-unfixed-sha; do
  cp $G7/negctl-real/$n.json $G/negctl-real/$n.json
  $PAR render-followup --manifest $M --case-id thrift --ledger $G/negctl-real/$n.json --output-dir $G/negctl-real/$n-out > $G/negctl-real/$n.out 2>&1; echo $? > $G/negctl-real/$n.rc
done
touch $G/PRE-CONTAINMENT-DONE
