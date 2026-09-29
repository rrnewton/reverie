#!/bin/bash
# Recovery gates for TFA phase-two Muse follow-ups. rc is captured before any filtering.
set -u
E=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-evidence/gates-recovery
R=/home/newton/work/tfa/fbsource
T=$R/fbcode/testinfra/tfa
cd $R
gate() {
  local name=$1; shift
  date -u +%Y-%m-%dT%H:%M:%SZ > $E/$name.started
  local t0=$(date +%s.%N)
  "$@" > $E/$name.out 2>&1
  local rc=$?
  echo $rc > $E/$name.rc
  date -u +%Y-%m-%dT%H:%M:%SZ > $E/$name.finished
  echo "$(echo "$(date +%s.%N) - $t0" | bc)" > $E/$name.seconds
  return $rc
}
tree_sha() { (cd $T && sha256sum process.py payload_wrapper.py hermit.py tests/test_prototype.py); }
verify_good() {
  tree_sha > $E/tree.now.sha
  if ! cmp -s $E/tree.good.sha $E/tree.now.sha; then echo "TREE DRIFT" | tee $E/TREE-DRIFT; diff $E/tree.good.sha $E/tree.now.sha; exit 99; fi
}
tree_sha > $E/tree.good.sha
sl status --reason "record tracked status for TFA recovery gates - sl help status" > $E/status.pre.out 2>&1
cp $T/payload_wrapper.py $E/payload_wrapper.py.good; cp $T/hermit.py $E/hermit.py.good

gate focused buck test fbcode//testinfra/tfa/tests:tfa_phase2_test -- -r 'packaged_hermit|wrapper_refuses_empty|test_wrapper_|entrypoint_requires'
gate unit buck test fbcode//testinfra/tfa/tests:tfa_phase2_test
gate manifest buck run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json
gate pyre arc pyre check-changed-targets
gate arc-f arc f
verify_good
gate arc-lint arc lint -a
verify_good

# Negative control 1: delete the empty-argv refusal.
python3 - "$T/payload_wrapper.py" <<'PY'
import sys; p=sys.argv[1]; s=open(p).read()
old='    if not upstream or not upstream[0]:\n        raise SystemExit("payload wrapper received no upstream executable")\n'
assert s.count(old)==1; open(p,'w').write(s.replace(old,''))
PY
(cd $T && sha256sum payload_wrapper.py) > $E/mutant-argv.sha
gate mutant-argv buck test fbcode//testinfra/tfa/tests:tfa_phase2_test -- -r 'wrapper_refuses_empty'
cp $E/payload_wrapper.py.good $T/payload_wrapper.py
verify_good

# Negative control 2: evaluate the reviewed packaged state before the provenance digest.
python3 - "$T/hermit.py" <<'PY'
import sys; p=sys.argv[1]; s=open(p).read()
old=('    if packaged_sha256 != packaged["sha256"]:\n        compatibility_state = "PROVENANCE_MISMATCH"\n'
     '    elif packaged["state"] != "QUALIFIED":\n        compatibility_state = "BLOCKED"\n')
new=('    if packaged["state"] != "QUALIFIED":\n        compatibility_state = "BLOCKED"\n'
     '    elif packaged_sha256 != packaged["sha256"]:\n        compatibility_state = "PROVENANCE_MISMATCH"\n')
assert s.count(old)==1; open(p,'w').write(s.replace(old,new))
PY
(cd $T && sha256sum hermit.py) > $E/mutant-order.sha
gate mutant-order buck test fbcode//testinfra/tfa/tests:tfa_phase2_test -- -r 'packaged_hermit'
cp $E/hermit.py.good $T/hermit.py
verify_good
sl status --reason "record tracked status for TFA recovery gates - sl help status" > $E/status.post-controls.out 2>&1

tree_sha > $E/production.tree.sha
gate production buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test
verify_good
sl status --reason "record tracked status for TFA recovery gates - sl help status" > $E/status.post.out 2>&1
echo ALL-GATES-RAN > $E/DONE
