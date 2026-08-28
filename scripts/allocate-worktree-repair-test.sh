#!/usr/bin/env bash
# Bracketed (Proxy-Binding) test for the single-writer REPAIR/SYNC mode of
# scripts/allocate-worktree.rs --repair.
#
# The mode reconciles the recorded {product}_branch cells in worktree-state.json
# (and the managed ACTIVE.md block) FROM physical submodule porcelain, WITHOUT
# ever running a git branch/checkout/delete. This test brackets that guarantee
# from both sides:
#
#   POSITIVE (mechanism fires): plant a slot whose recorded branch DISAGREES with
#     the physical checkout; run --repair; assert the verifier now reports
#     0 drift AND ACTIVE.md was rewritten to the physical branch.
#   NEGATIVE (mechanism is not destructive): assert that after repair the
#     UNPUSHED local branch that was recorded-but-not-checked-out STILL RESOLVES
#     (git rev-parse succeeds) — repair rewrote a string, it did not delete a ref.
#   CONTROL: a slot already in agreement is left byte-for-byte unchanged.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
root="$(mktemp -d "${TMPDIR:-/tmp}/allocate-repair-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

# dev-hermit root shape the scripts' find_root() looks for.
mkdir -p "$root/scripts" "$root/hermit" "$root/reverie" "$root/liteinst2" "$root/worktrees"
touch "$root/.gitmodules"
cp "$script_dir/allocate-worktree.rs" "$root/scripts/"
cp "$script_dir/check-worktree-registry.rs" "$root/scripts/"
cp "$script_dir/worktree_registry.rs" "$root/scripts/"
cp "$script_dir/agent_snapshot.rs" "$root/scripts/"

git -C "$root" init -q -b main
git -C "$root" config user.email test@example.invalid
git -C "$root" config user.name test
touch "$root/parent-seed"
git -C "$root" add parent-seed .gitmodules
git -C "$root" commit -q -m 'seed parent checkout'
git -C "$root/hermit" init -q -b main
git -C "$root/hermit" config user.email test@example.invalid
git -C "$root/hermit" config user.name test
touch "$root/hermit/seed"
git -C "$root/hermit" add seed
git -C "$root/hermit" commit -q -m 'seed product primary'

new_repo() { # dir  checked-out-branch  [extra-local-branch]
  local dir=$1 branch=$2 extra=${3:-}
  mkdir -p "$dir"
  git -C "$dir" init -q -b main
  git -C "$dir" config user.email test@example.invalid
  git -C "$dir" config user.name test
  touch "$dir/seed"; git -C "$dir" add seed; git -C "$dir" commit -q -m seed
  [[ -n $extra ]] && git -C "$dir" branch "$extra"   # local ref, never checked out
  git -C "$dir" switch -q -c "$branch"
  git init -q --bare "$dir-origin.git"
  git -C "$dir" remote add origin "$dir-origin.git"
  git -C "$dir" push -q -u origin "$branch"
}

# drift70:  physical hermit on physical-70, but state records an UNPUSHED local
#           branch stale-unpushed-70 that exists but is not checked out (the
#           slot-250 shape: repurposed slot, stale recorded branch).
# ok71:     physical == recorded (correct control; must stay untouched).
new_repo "$root/worktrees/slots/drift70/hermit" "physical-70" "stale-unpushed-70"
new_repo "$root/worktrees/slots/ok71/hermit"    "correct-71"

cat >"$root/worktree-state.json" <<'JSON'
{
  "version": 3,
  "updated": "2026-08-28T00:00:00Z",
  "slots": {
    "drift70": {
      "agents": [{"name":"agent-70","read_only":false,"task":"task-70"}],
      "allocated":"2026-08-28T00:00:00Z", "updated":"2026-08-28T00:00:00Z",
      "hermit_branch":"stale-unpushed-70", "hermit_path":"worktrees/slots/drift70/hermit",
      "reverie_branch":"-", "reverie_path":"worktrees/slots/drift70/reverie",
      "liteinst2_branch":"-", "liteinst2_path":"worktrees/slots/drift70/liteinst2",
      "task":"task-70", "status":"active"
    },
    "ok71": {
      "agents": [{"name":"agent-71","read_only":false,"task":"task-71"}],
      "allocated":"2026-08-28T00:00:00Z", "updated":"2026-08-28T00:00:00Z",
      "hermit_branch":"correct-71", "hermit_path":"worktrees/slots/ok71/hermit",
      "reverie_branch":"-", "reverie_path":"worktrees/slots/ok71/reverie",
      "liteinst2_branch":"-", "liteinst2_path":"worktrees/slots/ok71/liteinst2",
      "task":"task-71", "status":"active"
    }
  }
}
JSON

cat >"$root/worktrees/ACTIVE.md" <<'MD'
# Active Worktrees

Human notes above the managed block must survive verbatim.
<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->
| Slot | Agent | HermitBranch | ReverieBranch | LiteInst2Branch | Task | Status | ReadOnly |
| --- | --- | --- | --- | --- | --- | --- | --- |
| drift70 | agent-70 | stale-unpushed-70 | - | - | task-70 | active | no |
| ok71 | agent-71 | correct-71 | - | - | task-71 | active | no |
<!-- END worktree-state -->

Human notes below must survive too.
MD

# --- Pre-state: verifier must SEE the planted drift (mechanism has something to fix).
if "$root/scripts/check-worktree-registry.rs" --root "$root" >"$root/pre.out" 2>&1; then
  echo "FAIL: verifier saw no drift before repair (test fixture is inert)" >&2; exit 1
fi
grep -Fq 'DRIFT slot=drift70 hermit recorded=stale-unpushed-70 actual=physical-70' "$root/pre.out" \
  || { echo "FAIL: expected drift70 hermit drift not reported" >&2; cat "$root/pre.out" >&2; exit 1; }
grep -Fq 'DRIFT slot=ok71' "$root/pre.out" \
  && { echo "FAIL: correct control ok71 was flagged before repair" >&2; exit 1; }

# --- POSITIVE: run the reconciler; it must FIRE and drive drift to zero.
(cd "$root" && "$root/scripts/allocate-worktree.rs" --repair) >"$root/repair.out" 2>&1 \
  || { echo "FAIL: --repair exited nonzero" >&2; cat "$root/repair.out" >&2; exit 1; }
grep -Fq "reconcile drift70/hermit: 'stale-unpushed-70' -> 'physical-70'" "$root/repair.out" \
  || { echo "FAIL: repair did not reconcile drift70 hermit" >&2; cat "$root/repair.out" >&2; exit 1; }

# Verifier is now clean (mechanism actually fixed the recorded cells).
"$root/scripts/check-worktree-registry.rs" --root "$root" >"$root/post.out" 2>&1 \
  || { echo "FAIL: drift remains after repair" >&2; cat "$root/post.out" >&2; exit 1; }
grep -Fq 'PASS rows=2 correct_rows=2 drift_rows=0 product_cells=6 drift_cells=0' "$root/post.out" \
  || { echo "FAIL: post-repair verifier not fully clean" >&2; cat "$root/post.out" >&2; exit 1; }

# ACTIVE.md managed row now reflects the physical branch...
grep -Fq '| drift70 | agent-70 | physical-70 |' "$root/worktrees/ACTIVE.md" \
  || { echo "FAIL: ACTIVE.md not rewritten to physical branch" >&2; exit 1; }
# ...and human content outside the markers survived.
grep -Fq 'Human notes above the managed block must survive verbatim.' "$root/worktrees/ACTIVE.md" \
  || { echo "FAIL: human content above managed block was lost" >&2; exit 1; }
grep -Fq 'Human notes below must survive too.' "$root/worktrees/ACTIVE.md" \
  || { echo "FAIL: human content below managed block was lost" >&2; exit 1; }

# --- NEGATIVE: repair must NOT be destructive. The unpushed local branch that
#     used to be the recorded value must STILL EXIST (repair rewrote a string in
#     the registry, it never touched a git ref).
git -C "$root/worktrees/slots/drift70/hermit" rev-parse --verify --quiet stale-unpushed-70 >/dev/null \
  || { echo "FAIL: repair DELETED the unpushed local branch stale-unpushed-70" >&2; exit 1; }

# --- CONTROL: the already-correct slot's recorded value is unchanged.
grep -Fq '"hermit_branch": "correct-71"' "$root/worktree-state.json" \
  || { echo "FAIL: correct control ok71 recorded branch was mutated" >&2; exit 1; }

# --- IDEMPOTENCE: a second repair reports zero changes.
(cd "$root" && "$root/scripts/allocate-worktree.rs" --repair) >"$root/repair2.out" 2>&1
grep -Fq 'repair: 0 branch cells needed reconciliation' "$root/repair2.out" \
  || { echo "FAIL: repair is not idempotent" >&2; cat "$root/repair2.out" >&2; exit 1; }

# FALSE-ASCENT NEGATIVE: a generated-residue directory sees the fixture
# parent's `main`, but neither repair nor allocator adoption may treat that
# ancestor as the requested Hermit checkout.
mkdir -p "$root/worktrees/slots/ascent72/hermit/target"
printf 'generated\n' >"$root/worktrees/slots/ascent72/hermit/target/artifact"
python3 - "$root/worktree-state.json" "$root/worktrees/ACTIVE.md" <<'PY'
import json
import pathlib
import sys

state_path, active_path = map(pathlib.Path, sys.argv[1:])
state = json.loads(state_path.read_text())
state["slots"]["ascent72"] = {
    "agents": [{"name": "agent-ascent", "read_only": False}],
    "hermit_branch": "main", "hermit_path": "worktrees/slots/ascent72/hermit",
    "reverie_branch": "-", "reverie_path": "worktrees/slots/ascent72/reverie",
    "liteinst2_branch": "-", "liteinst2_path": "worktrees/slots/ascent72/liteinst2",
    "task": "task-ascent", "status": "released",
}
state_path.write_text(json.dumps(state, indent=2) + "\n")
active = active_path.read_text()
row = "| ascent72 | agent-ascent | main | - | - | task-ascent | released | no |\n"
active_path.write_text(active.replace("<!-- END worktree-state -->", row + "<!-- END worktree-state -->"))
PY
ascent_hash=$(sha256sum "$root/worktree-state.json")
if (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --dry-run) \
    >"$root/ascent-repair.out" 2>&1; then
  : # zero writable cells is a successful dry run
fi
grep -Fq "skip ascent72/hermit: worktree unreadable; recorded='main' left as-is" \
  "$root/ascent-repair.out" \
  || { echo 'FAIL: repair did not refuse the parent-ascent proxy' >&2; cat "$root/ascent-repair.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$ascent_hash" ]] \
  || { echo 'FAIL: dry-run parent-ascent refusal mutated registry state' >&2; exit 1; }
if (cd "$root" && "$root/scripts/allocate-worktree.rs" \
    --agent agent-ascent --slot ascent72 --task task-ascent --product hermit \
    --hermit-branch requested-ascent) >"$root/ascent-adopt.out" 2>&1; then
  echo 'FAIL: allocator adopted parent-ascent residue as a product worktree' >&2; exit 1
fi
grep -Fq 'path exists but is not the exact requested git worktree' "$root/ascent-adopt.out" \
  || { echo 'FAIL: allocator did not expose exact-root adoption refusal' >&2; cat "$root/ascent-adopt.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$ascent_hash" ]] \
  || { echo 'FAIL: refused parent-ascent adoption mutated registry state' >&2; exit 1; }

# PATH-ALIAS NEGATIVE: repair and adoption must not canonicalize an alternate
# spelling into registry authority or silently rewrite around it.
python3 - "$root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["ascent72"]["hermit_path"] = "worktrees/slots/ascent72/./hermit"
path.write_text(json.dumps(state, indent=2) + "\n")
PY
alias_hash=$(sha256sum "$root/worktree-state.json")
(cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --dry-run) \
  >"$root/alias-repair.out" 2>&1
grep -Fq "skip ascent72: slot ascent72 records noncanonical hermit_path; expected exact path 'worktrees/slots/ascent72/hermit'; all branch cells left as-is" \
  "$root/alias-repair.out" \
  || { echo 'FAIL: repair did not refuse lexical path alias' >&2; cat "$root/alias-repair.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$alias_hash" ]] \
  || { echo 'FAIL: alias-refusing repair mutated registry state' >&2; exit 1; }
if (cd "$root" && "$root/scripts/allocate-worktree.rs" \
    --agent agent-ascent --slot ascent72 --task task-ascent --product hermit \
    --hermit-branch requested-ascent) >"$root/alias-adopt.out" 2>&1; then
  echo 'FAIL: allocator adopted a slot with an aliased registry path' >&2; exit 1
fi
grep -Fq "slot ascent72 records noncanonical hermit_path; expected exact path 'worktrees/slots/ascent72/hermit'" \
  "$root/alias-adopt.out" \
  || { echo 'FAIL: allocator did not expose lexical path alias' >&2; cat "$root/alias-adopt.out" >&2; exit 1; }
python3 - "$root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["ascent72"]["hermit_path"] = "worktrees/slots/ascent72/hermit"
path.write_text(json.dumps(state, indent=2) + "\n")
PY

# The parent-ascent fixture has served its purpose. Remove only its temporary
# registry row so the scoped two-fault bracket below contains exactly one
# excluded journal and one repairable drift.
python3 - "$root/worktree-state.json" "$root/worktrees/ACTIVE.md" <<'PY'
import json
import pathlib
import sys

state_path, active_path = map(pathlib.Path, sys.argv[1:])
state = json.loads(state_path.read_text())
state["slots"].pop("ascent72")
state_path.write_text(json.dumps(state, indent=2) + "\n")
active_path.write_text("\n".join(
    line for line in active_path.read_text().splitlines()
    if not line.startswith("| ascent72 |")
) + "\n")
PY

# --- RELEASE-JOURNAL ISOLATION: a fenced checkout is intentionally absent at
# its canonical path. Neither repair nor ordinary re-adoption may translate
# that transient absence into branch="-" or status=active.
fenced="$root/worktrees/slots/ok71/.hermit.release-worktree-123-456"
mv "$root/worktrees/slots/ok71/hermit" "$fenced"
python3 - "$root/worktree-state.json" "$fenced" <<'PY'
import json
import pathlib
import sys

state_path = pathlib.Path(sys.argv[1])
state = json.loads(state_path.read_text())
record = state["slots"]["ok71"]
record["status"] = "releasing"
record["release_journal"] = {
    "schema_version": 1,
    "label": "hermit",
    "original": str(state_path.parent / "worktrees/slots/ok71/hermit"),
    "fenced": sys.argv[2],
}
state_path.write_text(json.dumps(state, indent=2) + "\n")
PY
journal_hash=$(sha256sum "$root/worktree-state.json")
if (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair) \
    >"$root/releasing-repair.out" 2>&1; then
  echo 'FAIL: repair consumed an unfinished release journal' >&2; exit 1
fi
grep -Fq 'REFUSING repair while slot release transaction(s) require guarded recovery: ok71' \
  "$root/releasing-repair.out" \
  || { echo 'FAIL: repair did not identify the journaled slot' >&2; cat "$root/releasing-repair.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$journal_hash" ]] \
  || { echo 'FAIL: refused repair mutated the release journal' >&2; exit 1; }

# TWO-FAULT POSITIVE: an armed journal in ok71 and an unrelated drift in
# drift70 no longer deadlock. The exclusion is explicit in output, the excluded
# row/journal is untouched, and only the clean, remote-contained drift is fixed.
#
# The recorded name is created as a real ref first. This fixture exercises exclusion scoping, and
# it re-arms drift by planting a recorded branch; a name that resolves is the shape a repurposed
# slot actually has (see stale-unpushed-70 above). A recorded name that resolves nowhere is a
# separate case with its own fixture below, where repair must refuse rather than reconcile.
git -C "$root/worktrees/slots/drift70/hermit" branch stale-again
python3 - "$root/worktree-state.json" <<'PY'
import json
import pathlib

path = pathlib.Path(__import__('sys').argv[1])
state = json.loads(path.read_text())
state["slots"]["drift70"]["hermit_branch"] = "stale-again"
path.write_text(json.dumps(state, indent=2) + "\n")
PY
excluded_journal=$(jq -cS '.slots.ok71.release_journal' "$root/worktree-state.json")
excluded_active_row=$(grep -F '| ok71 |' "$root/worktrees/ACTIVE.md")
(cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --exclude-slot ok71) \
  >"$root/scoped-exclude.out" 2>&1 \
  || { echo 'FAIL: explicit exclusion did not break the two-fault deadlock' >&2; \
       cat "$root/scoped-exclude.out" >&2; exit 1; }
grep -Fq 'repair scope: excluded=ok71 (not inspected or modified)' \
  "$root/scoped-exclude.out" \
  || { echo 'FAIL: excluded slot was not recorded in repair output' >&2; exit 1; }
jq -e '.slots.drift70.hermit_branch == "physical-70"' \
  "$root/worktree-state.json" >/dev/null \
  || { echo 'FAIL: scoped repair did not reconcile the included drift' >&2; exit 1; }
[[ $(jq -cS '.slots.ok71.release_journal' "$root/worktree-state.json") == "$excluded_journal" ]] \
  || { echo 'FAIL: scoped repair changed the excluded journal' >&2; exit 1; }
[[ $(grep -F '| ok71 |' "$root/worktrees/ACTIVE.md") == "$excluded_active_row" ]] \
  || { echo 'FAIL: scoped ACTIVE repair changed the excluded row' >&2; exit 1; }

# UNPUBLISHED NEGATIVE: scope is not force. A local-only current HEAD refuses
# before either registry file changes, even though the unrelated journal is
# explicitly excluded.
printf 'local-only\n' >"$root/worktrees/slots/drift70/hermit/local-only"
git -C "$root/worktrees/slots/drift70/hermit" add local-only
git -C "$root/worktrees/slots/drift70/hermit" commit -q -m 'local only fixture'
# Real ref, for the same reason as the fixture above: this case is about the unpublished-HEAD
# refusal, not about a recorded name that resolves nowhere.
git -C "$root/worktrees/slots/drift70/hermit" branch stale-unpublished
python3 - "$root/worktree-state.json" <<'PY'
import json
import pathlib

path = pathlib.Path(__import__('sys').argv[1])
state = json.loads(path.read_text())
state["slots"]["drift70"]["hermit_branch"] = "stale-unpublished"
path.write_text(json.dumps(state, indent=2) + "\n")
PY
unpublished_hash=$(sha256sum "$root/worktree-state.json")
if (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --exclude-slot ok71) \
    >"$root/scoped-unpublished.out" 2>&1; then
  echo 'FAIL: scoped repair accepted an unpushed current HEAD' >&2; exit 1
fi
grep -Fq 'commit(s) are reachable from HEAD but no remote ref' \
  "$root/scoped-unpublished.out" \
  || { echo 'FAIL: unpublished refusal was not explicit' >&2; \
       cat "$root/scoped-unpublished.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$unpublished_hash" ]] \
  || { echo 'FAIL: unpublished refusal changed registry state' >&2; exit 1; }

# Publish the fixture commit, then prove the alternate named-slot scope works
# while the unrelated ok71 journal remains armed.
git -C "$root/worktrees/slots/drift70/hermit" push -q origin physical-70
(cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --slot drift70) \
  >"$root/scoped-only.out" 2>&1 \
  || { echo 'FAIL: named-slot repair refused a published target' >&2; \
       cat "$root/scoped-only.out" >&2; exit 1; }
grep -Fq 'repair scope: only slot=drift70' "$root/scoped-only.out" \
  || { echo 'FAIL: named repair scope was not reported' >&2; exit 1; }
[[ $(jq -cS '.slots.ok71.release_journal' "$root/worktree-state.json") == "$excluded_journal" ]] \
  || { echo 'FAIL: named-slot repair changed the unrelated journal' >&2; exit 1; }

# ACTIVE-ONLY UNCOMMITTED NEGATIVE: even when no branch cell changes, scoped
# repair rewrites the selected ACTIVE row and therefore must prove the selected
# checkout clean before touching it.
python3 - "$root/worktrees/ACTIVE.md" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.write_text(path.read_text().replace(
    "| drift70 | agent-70 | physical-70 |",
    "| drift70 | agent-70 | stale-active-only |",
))
PY
printf 'dirty\n' >"$root/worktrees/slots/drift70/hermit/untracked-active-only"
active_only_state_hash=$(sha256sum "$root/worktree-state.json")
active_only_active_hash=$(sha256sum "$root/worktrees/ACTIVE.md")
if (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --slot drift70) \
    >"$root/scoped-active-only.out" 2>&1; then
  echo 'FAIL: scoped ACTIVE-only repair accepted uncommitted selected work' >&2; exit 1
fi
grep -Fq 'uncommitted work is present' "$root/scoped-active-only.out" \
  || { echo 'FAIL: ACTIVE-only unpublished refusal was not explicit' >&2; \
       cat "$root/scoped-active-only.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$active_only_state_hash" ]] \
  || { echo 'FAIL: ACTIVE-only refusal changed registry state' >&2; exit 1; }
[[ $(sha256sum "$root/worktrees/ACTIVE.md") == "$active_only_active_hash" ]] \
  || { echo 'FAIL: ACTIVE-only refusal changed ACTIVE' >&2; exit 1; }
rm -f "$root/worktrees/slots/drift70/hermit/untracked-active-only"
(cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --slot drift70) \
  >"$root/scoped-active-only-clean.out" 2>&1 \
  || { echo 'FAIL: clean ACTIVE-only repair was refused' >&2; \
       cat "$root/scoped-active-only-clean.out" >&2; exit 1; }

preadopt_hash=$(sha256sum "$root/worktree-state.json")
if (cd "$root" && "$root/scripts/allocate-worktree.rs" \
    --agent agent-71 --slot ok71 --task task-71 --product hermit) \
    >"$root/releasing-adopt.out" 2>&1; then
  echo 'FAIL: re-adoption consumed an unfinished release journal' >&2; exit 1
fi
grep -Fq 'slot ok71 has an unfinished release transaction' "$root/releasing-adopt.out" \
  || { echo 'FAIL: re-adoption did not refuse the journaled slot' >&2; cat "$root/releasing-adopt.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$preadopt_hash" ]] \
  || { echo 'FAIL: refused re-adoption mutated the release journal' >&2; exit 1; }


# --- COORDINATOR-LEASE-REVOCATION PHASE GATE -------------------------------
# A revocation journal is an IN-FLIGHT TRANSACTION marker, not an audit record:
# its siblings (release_journal, orphan_residue_journal) delete themselves on
# completion, and arm_coordinator_lease_revocation moves no file, so a revocation
# can never create the fenced-checkout hazard this guard exists for. Blocking on
# the journal's mere PRESENCE made every completed revocation a permanent,
# undischargeable veto over repair for the WHOLE registry. Bracket all three ways.
# First retire the ok71 release journal so it is not the thing refusing below.
mv "$fenced" "$root/worktrees/slots/ok71/hermit"
python3 - "$root/worktree-state.json" <<'RETIRE_OK71'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
record = state["slots"]["ok71"]
record.pop("release_journal", None)
record["status"] = "active"
path.write_text(json.dumps(state, indent=2) + "\n")
RETIRE_OK71

plant_revocation() {  # $1 = journal phase
  python3 - "$root/worktree-state.json" "$1" <<'PLANT_REVOCATION'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["drift70"]["coordinator_lease_revocation"] = {
    "schema_version": 1,
    "source": "codex-systemd-sentinel-v1",
    "phase": sys.argv[2],
    "lease": {"slot": "drift70", "unit": "fixture.service"},
}
path.write_text(json.dumps(state, indent=2) + "\n")
PLANT_REVOCATION
}

# NEGATIVE 1/2 -- an UNFINISHED revocation must still refuse.
plant_revocation stop-authorized
if (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair) \
    >"$root/revoke-unfinished.out" 2>&1; then
  echo 'FAIL: repair ran despite an unfinished coordinator lease revocation' >&2; exit 1
fi
grep -Fq 'require guarded recovery: drift70' "$root/revoke-unfinished.out" \
  || { echo 'FAIL: repair did not name the unfinished revocation slot' >&2; \
       cat "$root/revoke-unfinished.out" >&2; exit 1; }

# POSITIVE -- a FINISHED revocation on a clean slot is skipped; repair completes.
plant_revocation revoked
if ! (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair) \
    >"$root/revoke-finished.out" 2>&1; then
  echo 'FAIL: repair still refused a completed (phase=revoked) revocation' >&2
  cat "$root/revoke-finished.out" >&2; exit 1
fi
python3 - "$root/worktree-state.json" <<'ASSERT_PRESERVED'
import json
import pathlib
import sys

state = json.loads(pathlib.Path(sys.argv[1]).read_text())
journal = state["slots"]["drift70"].get("coordinator_lease_revocation")
if journal is None:
    raise SystemExit("FAIL: repair ERASED the revocation journal; it must only skip it")
if journal.get("phase") != "revoked":
    raise SystemExit("FAIL: repair mutated the revocation journal phase")
ASSERT_PRESERVED

# NEGATIVE 2/2 -- finished, but the slot holds uncommitted work: still refused.
echo dirty >"$root/worktrees/slots/drift70/hermit/uncommitted-file"
if (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair) \
    >"$root/revoke-dirty.out" 2>&1; then
  echo 'FAIL: repair skipped a revoked journal whose slot has uncommitted work' >&2; exit 1
fi
grep -Fq 'require guarded recovery: drift70' "$root/revoke-dirty.out" \
  || { echo 'FAIL: dirty-slot refusal did not name the slot' >&2; \
       cat "$root/revoke-dirty.out" >&2; exit 1; }
rm -f "$root/worktrees/slots/drift70/hermit/uncommitted-file"

# ============================================================================
# UNFULFILLED-INTENT NEGATIVE: the recorded branch does not exist in the checkout.
#
# This is the re-allocation shape: a slot handed to a new task gets the NEW branch written into the
# registry, but the branch is never created and the checkout is still on the PREVIOUS task's branch.
# Reconciling registry->disk here would record the previous task's branch as correct, which is worse
# than the divergence it "fixes" because the result is self-consistent and the verifier goes quiet.
#
# Bracketed three ways, because a change that simply made repair refuse everything would pass a
# refusal-only test:
#   (a) REFUSE with a message naming both branches, and write NOTHING;
#   (b) the drift is STILL VISIBLE to the verifier afterwards (repair did not launder it, and the
#       detector was not taught to tolerate it);
#   (c) the mechanism STILL FIRES for a recorded branch that does resolve -- proving the refusal
#       discriminates on ref existence rather than disabling repair.
# ============================================================================
git -C "$root/worktrees/slots/drift70/hermit" checkout -q previous-task-branch 2>/dev/null \
  || git -C "$root/worktrees/slots/drift70/hermit" switch -q -c previous-task-branch
git -C "$root/worktrees/slots/drift70/hermit" push -q -u origin previous-task-branch
python3 - "$root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
# The branch the re-allocation asked for. Deliberately never created as a ref.
state["slots"]["drift70"]["hermit_branch"] = "agent/new-task-never-created"
path.write_text(json.dumps(state, indent=2) + "\n")
PY
git -C "$root/worktrees/slots/drift70/hermit" rev-parse --verify --quiet \
  refs/heads/agent/new-task-never-created >/dev/null \
  && { echo 'FAIL: fixture is inert -- the "never created" branch exists' >&2; exit 1; }

intent_state_hash=$(sha256sum "$root/worktree-state.json")
intent_active_hash=$(sha256sum "$root/worktrees/ACTIVE.md")

# (a) REFUSE, and name both sides so the operator can act.
if (cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --slot drift70) \
    >"$root/intent.out" 2>&1; then
  echo 'FAIL: repair reconciled a recorded branch that was never created' >&2
  cat "$root/intent.out" >&2; exit 1
fi
grep -Fq "recorded branch 'agent/new-task-never-created' does not exist in the checkout" \
  "$root/intent.out" \
  || { echo 'FAIL: refusal did not name the recorded branch' >&2; cat "$root/intent.out" >&2; exit 1; }
grep -Fq "which is on 'previous-task-branch'" "$root/intent.out" \
  || { echo 'FAIL: refusal did not name the branch actually checked out' >&2; \
       cat "$root/intent.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$intent_state_hash" ]] \
  || { echo 'FAIL: unfulfilled-intent refusal mutated worktree-state.json' >&2; exit 1; }
[[ $(sha256sum "$root/worktrees/ACTIVE.md") == "$intent_active_hash" ]] \
  || { echo 'FAIL: unfulfilled-intent refusal mutated ACTIVE.md' >&2; exit 1; }

# (b) The divergence is still VISIBLE -- refusing must not quiet the detector.
if "$root/scripts/check-worktree-registry.rs" --root "$root" --slot drift70 \
    >"$root/intent-verify.out" 2>&1; then
  echo 'FAIL: verifier reports clean after a refused repair; the drift was hidden' >&2
  cat "$root/intent-verify.out" >&2; exit 1
fi
grep -Fq 'DRIFT slot=drift70 hermit recorded=agent/new-task-never-created actual=previous-task-branch' \
  "$root/intent-verify.out" \
  || { echo 'FAIL: verifier did not still report the drift' >&2; \
       cat "$root/intent-verify.out" >&2; exit 1; }

# (c) DELETION-MUTATION GUARD: the same slot, same drift, but now the recorded branch EXISTS.
# Repair must FIRE. If this reconcile stops happening, the fix has become a blanket disable.
git -C "$root/worktrees/slots/drift70/hermit" branch agent/new-task-actually-created
python3 - "$root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["drift70"]["hermit_branch"] = "agent/new-task-actually-created"
path.write_text(json.dumps(state, indent=2) + "\n")
PY
(cd "$root" && "$root/scripts/allocate-worktree.rs" --repair --slot drift70) \
  >"$root/intent-fires.out" 2>&1 \
  || { echo 'FAIL: repair refused even though the recorded branch resolves' >&2; \
       cat "$root/intent-fires.out" >&2; exit 1; }
grep -Fq "reconcile drift70/hermit: 'agent/new-task-actually-created' -> 'previous-task-branch'" \
  "$root/intent-fires.out" \
  || { echo 'FAIL: repair did not reconcile a resolvable recorded branch' >&2; \
       cat "$root/intent-fires.out" >&2; exit 1; }
jq -e '.slots.drift70.hermit_branch == "previous-task-branch"' \
  "$root/worktree-state.json" >/dev/null \
  || { echo 'FAIL: resolvable-branch reconcile did not reach the registry' >&2; exit 1; }
# ...and the ref it stopped recording still resolves: repair rewrote a string, not a ref.
git -C "$root/worktrees/slots/drift70/hermit" rev-parse --verify --quiet \
  refs/heads/agent/new-task-actually-created >/dev/null \
  || { echo 'FAIL: repair deleted the ref it stopped recording' >&2; exit 1; }

echo "allocate-worktree-repair-test: PASS (scoped two-fault exclusion and named-slot repair; unpushed + ACTIVE-only uncommitted work REFUSED without mutation; excluded journal/row preserved; POSITIVE: 1 planted drift reconciled -> 0 drift; parent-ascent/path-alias residue skipped/refused; CONTROL: 1/1 correct row untouched; revocation phase gate bracketed 3 ways; UNFULFILLED INTENT refused without mutation, drift still visible to the verifier, and reconcile still fires for a resolvable recorded branch; human content preserved; idempotent)"
