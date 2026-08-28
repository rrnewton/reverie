#!/usr/bin/env bash
# Bracketed test for CALLER-ANCESTRY OWNER BINDING in
# scripts/allocate-worktree.rs::observe_caller_owner_lease.
#
# WHAT BROKE. observe_owner_lease resolves an agent name through
# ignored/ci-hub/agent-snapshot.json and requires it to be under 600s old. That
# file is a cache written by one ORC session. When its producer stalls, the
# allocator cannot bind owner evidence, and the Err arm used to just drop the
# sidecar and let the allocation succeed anyway. Owner evidence has to be
# captured AT BINDING TIME and, per
# ai_docs/slot-owner-sidecar-migration-contract_20260810.md, cannot be
# reconstructed afterwards -- so the row was unassessable forever:
# slot-owner-authority.py answers indeterminate and release-worktree.rs refuses.
# Neither usable nor recoverable.
#
# Measured 2026-08-27T02:13Z: the snapshot's captured_at was 8h00m old against
# the 600s bound, and every mutating allocation after 2026-08-26T18:13:03Z
# (coordinator, lseekcmd, 133) was recorded with no sidecar, joining 18 earlier
# rows in the same state. 21 rows, one reason code, nothing counting them.
#
# THE FIX UNDER TEST. An agent allocating its OWN slot never needed the cache:
# its tmux pane supervisor is an ancestor of the calling process, which is a
# kernel fact readable now. So the allocator tries the caller's ancestry first
# and falls back to the snapshot.
#
# WHAT THIS SUITE HAS TO PROVE, and why each half is here:
#
#   ADMIT   -- with a snapshot too stale to use, a self-allocation BINDS.
#              Without this the change could be a no-op.
#   BEFORE  -- the SAME fixture against the pre-change build does NOT bind.
#              Without this the admit proves nothing about the change.
#   REFUSE  -- an allocation made FOR SOMEONE ELSE still does not bind.
#              This is the dangerous direction: binding the caller's own
#              supervisor as another agent's owner would make a LIVE agent's
#              slot read dead and let release reclaim it underneath them.
#   RECORD  -- when nothing can bind, the row carries a typed reason, and the
#              authority still answers indeterminate with rc 2. A row that
#              explains itself must not become a row that permits recovery.
#
# Runs entirely in a temp fixture root. It never writes the live snapshot and
# never touches the live registry. It does read this box's real tmux pane list
# and this process's real ancestry, because those are the predicates under test
# and a fabricated pane cannot exercise them.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"

if [[ -z "${DG_AGENT_NAME:-}" ]]; then
  echo "SKIP: no DG_AGENT_NAME; the caller-ancestry path is only reachable from an agent." >&2
  exit 2
fi
if ! tmux list-panes -a -F '#{pane_id}' >/dev/null 2>&1; then
  echo "SKIP: no tmux server visible; the pane ancestry predicate cannot be exercised." >&2
  exit 2
fi

# Prove the precondition this suite depends on before asserting anything about
# the allocator: one of this process's ancestors must be a live tmux pane.
# If it is not, an ADMIT failure below would be the environment, not the code.
ancestor_pane="$(
  python3 - <<'PY'
import os, subprocess
def ppid(pid):
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("PPid:"):
            return int(line.split()[1])
    return 0
chain, pid = set(), os.getpid()
while pid > 1 and len(chain) < 64:
    chain.add(pid)
    pid = ppid(pid)
out = subprocess.run(["tmux", "list-panes", "-a", "-F", "#{pane_id}\t#{pane_pid}"],
                     capture_output=True, text=True)
for line in out.stdout.splitlines():
    pane, _, raw = line.partition("\t")
    if raw.isdigit() and int(raw) in chain:
        print(pane)
        break
PY
)"
if [[ -z "$ancestor_pane" ]]; then
  echo "SKIP: no live tmux pane on this process's ancestry; nothing to bind from." >&2
  exit 2
fi

root="$(mktemp -d "${TMPDIR:-/tmp}/allocate-caller-binding-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

pass=0
fail=0
note() { printf '%s\n' "$*"; }
ok()   { printf '  PASS  %s\n' "$*"; pass=$((pass + 1)); }
bad()  { printf '  FAIL  %s\n' "$*"; fail=$((fail + 1)); }

note "Fixture root: $root"
note "Caller is '$DG_AGENT_NAME', owned by live pane $ancestor_pane on its own ancestry."
note ""

build_fixture() { # <allocator-source>
  rm -rf "$root/tree"
  mkdir -p "$root/tree/scripts" "$root/tree/ci-hub/lib" "$root/tree/hermit" "$root/tree/reverie" \
           "$root/tree/liteinst2" "$root/tree/worktrees" "$root/tree/ignored/ci-hub"
  touch "$root/tree/.gitmodules"
  cp "$1" "$root/tree/scripts/allocate-worktree.rs"
  # `git show` output is mode 644; without this the pre-change A/B run in CASE 2
  # would fail to execute and report a missing registry rather than a verdict.
  chmod +x "$root/tree/scripts/allocate-worktree.rs"
  cp "$script_dir/check-worktree-registry.rs" "$root/tree/scripts/"
  cp "$script_dir/worktree_registry.rs" "$root/tree/scripts/"
  cp "$script_dir/agent_snapshot.rs" "$root/tree/scripts/"
  cp "$script_dir/slot-owner-authority.py" "$root/tree/scripts/"
  cp "$script_dir/../ci-hub/lib/worktree_registry.py" "$root/tree/ci-hub/lib/"
  git -C "$root/tree" init -q -b main
  git -C "$root/tree" config user.email test@example.invalid
  git -C "$root/tree" config user.name test
  touch "$root/tree/parent-seed"
  git -C "$root/tree" add parent-seed .gitmodules
  git -C "$root/tree" commit -q -m 'seed parent checkout'
  for product in hermit reverie liteinst2; do
    git -C "$root/tree/$product" init -q -b main
    git -C "$root/tree/$product" config user.email test@example.invalid
    git -C "$root/tree/$product" config user.name test
    touch "$root/tree/$product/seed"
    git -C "$root/tree/$product" add seed
    git -C "$root/tree/$product" commit -q -m 'seed product primary'
  done
}

# The exact condition that produced the 21 rows: a snapshot that parses, carries
# a real fleet shape, and is far too old to use.
write_stale_snapshot() {
  cat >"$root/tree/ignored/ci-hub/agent-snapshot.json" <<JSON
{
  "schema_version": 1,
  "captured_at": $(( $(date +%s) - 28800 )),
  "agents": [
    {"name": "$DG_AGENT_NAME", "status": "busy", "tmux_pane_id": "$ancestor_pane"}
  ]
}
JSON
}

allocate() { # <agent> <slot>
  ( cd "$root/tree" && ./scripts/allocate-worktree.rs \
      --agent "$1" --slot "$2" --product hermit \
      --task "caller-binding-test" --purpose "bracket caller-ancestry owner binding" \
  ) >"$root/out.$2" 2>"$root/err.$2" || true
}

# Read the SIDECAR, not the agents[] lease. The sidecar is what
# slot-owner-authority.py reads and therefore what decides assessability; a row
# can carry a pane on its agent entry and still be unassessable.
sidecar_state() { # <slot>
  python3 - "$root/tree/worktree-state.json" "$1" <<'PY'
import json, sys
try:
    state = json.load(open(sys.argv[1]))
except OSError:
    print("NO-REGISTRY"); raise SystemExit
row = state.get("slots", {}).get(sys.argv[2])
if row is None:
    print("NO-ROW"); raise SystemExit
car = row.get("owner_sidecar")
if not isinstance(car, dict):
    print("NO-SIDECAR"); raise SystemExit
print("SIDECAR", car.get("source"), car.get("generation"), car.get("tmux_pane_id"))
PY
}

binding_failure() { # <slot>
  python3 - "$root/tree/worktree-state.json" "$1" <<'PY'
import json, sys
state = json.load(open(sys.argv[1]))
row = state.get("slots", {}).get(sys.argv[2], {})
rec = row.get("owner_binding_failure")
if not isinstance(rec, dict):
    print("NONE"); raise SystemExit
print("RECORD", rec.get("schema_version"), rec.get("agent"), "|", rec.get("reason", "")[:120])
PY
}

# ------------------------------------------------------------------ CASE 1 ---
note "CASE 1  ADMIT -- self-allocation binds through the caller's own ancestry"
note "        with a snapshot 8h too stale for observe_owner_lease to use."
build_fixture "$script_dir/allocate-worktree.rs"
write_stale_snapshot
allocate "$DG_AGENT_NAME" "selfslot"
c1="$(sidecar_state selfslot)"
if [[ "$c1" == SIDECAR* ]]; then
  ok "C1: sidecar BOUND ($c1)"
else
  bad "C1: expected a bound sidecar, got: $c1"
  sed -n '1,20p' "$root/err.selfslot" >&2 || true
fi
if [[ "$c1" == *"orc-tmux-pane-supervisor-v1"* ]]; then
  ok "C1: bound as an ORC tmux pane supervisor -- same subject and source as the snapshot path"
else
  bad "C1: sidecar source is not orc-tmux-pane-supervisor-v1: $c1"
fi
if [[ "$c1" == *"$ancestor_pane"* ]]; then
  ok "C1: bound to THIS caller's own pane $ancestor_pane, not some other pane"
else
  bad "C1: sidecar does not name the caller's pane $ancestor_pane: $c1"
fi
if [[ "$(binding_failure selfslot)" == "NONE" ]]; then
  ok "C1: no binding-failure record on a row that bound"
else
  bad "C1: a bound row must not carry a binding-failure record: $(binding_failure selfslot)"
fi
note ""

# ------------------------------------------------------------------ CASE 2 ---
# Without this the ADMIT above would not be evidence about the change.
note "CASE 2  BEFORE -- the SAME fixture against the pre-change build must NOT bind."
#
# The A/B has to keep working AFTER this change lands, so it cannot read HEAD --
# once committed, HEAD *is* the post-change build and the comparison silently
# becomes post-vs-post. Walk this file's own history instead and take the newest
# revision that does not contain the function. That is "the build immediately
# before this change" on any day, not just the day it was written.
before_src="$root/allocate-worktree.before.rs"
before_sha=""
# NOT `git show ... | grep -q`. Under `set -o pipefail`, grep -q exits on its
# first match, git show takes SIGPIPE, and the PIPELINE reports failure -- so a
# revision that DOES contain the function tests as one that does not, and the
# A/B silently selects the post-change build as its own control. Materialize the
# file first and grep the file.
while read -r sha; do
  [[ -n "$sha" ]] || continue
  git -C "$script_dir/.." show "$sha:scripts/allocate-worktree.rs" >"$before_src" 2>/dev/null || continue
  if grep -q "observe_caller_owner_lease" "$before_src"; then
    continue
  fi
  before_sha="$sha"
  break
done < <(git -C "$script_dir/.." log --format=%H -n 200 -- scripts/allocate-worktree.rs 2>/dev/null)

if [[ -n "$before_sha" ]]; then
  note "        pre-change allocator taken from $before_sha"
  build_fixture "$before_src"
  write_stale_snapshot
  allocate "$DG_AGENT_NAME" "selfslot"
  c2="$(sidecar_state selfslot)"
  if [[ "$c2" == "NO-SIDECAR" ]]; then
    ok "C2: pre-change build left the row with NO sidecar -- this is the defect, reproduced"
  else
    bad "C2: pre-change build was expected to leave no sidecar, got: $c2"
  fi
else
  # Deliberately a FAILURE, not a skip. This repository has the history, so not
  # finding a pre-change revision means the comparison did not happen -- and an
  # A/B that quietly stops comparing reads exactly like one that passed.
  bad "C2: no pre-change revision of scripts/allocate-worktree.rs in the last 200 commits touching it; the A/B did not run"
fi
note ""

# ------------------------------------------------------------------ CASE 3 ---
note "CASE 3  REFUSE -- an allocation made FOR ANOTHER AGENT must not bind the"
note "        caller's own supervisor. This is the direction that could reclaim"
note "        a live agent's slot."
build_fixture "$script_dir/allocate-worktree.rs"
write_stale_snapshot
allocate "not-$DG_AGENT_NAME" "otherslot"
c3="$(sidecar_state otherslot)"
if [[ "$c3" == "NO-SIDECAR" ]]; then
  ok "C3: no sidecar bound for an agent the caller is not"
else
  bad "C3: caller-ancestry binding leaked to another agent's slot: $c3"
fi
if grep -q "wrong subject" "$root/err.otherslot"; then
  ok "C3: the refusal names the wrong-subject cause"
else
  bad "C3: refusal did not explain the wrong-subject cause"
  sed -n '1,20p' "$root/err.otherslot" >&2 || true
fi
note ""

# ------------------------------------------------------------------ CASE 4 ---
note "CASE 4  RECORD -- an unbindable row says WHY and WHEN, and the authority"
note "        still refuses it."
c4="$(binding_failure otherslot)"
if [[ "$c4" == RECORD* ]]; then
  ok "C4: row carries a typed binding-failure record ($c4)"
else
  bad "C4: expected a binding-failure record on an unbound row, got: $c4"
fi
verdict="$(
  cd "$root/tree" && python3 scripts/slot-owner-authority.py --slot otherslot --json 2>&1
)" && rc=0 || rc=$?
if [[ "$rc" -eq 2 ]]; then
  ok "C4: authority exits 2 -- the row is still indeterminate, recovery still refused"
else
  bad "C4: authority exit code was $rc, expected 2 (indeterminate)"
fi
if grep -q '"reason_code": "owner-binding-failed"' <<<"$verdict"; then
  ok "C4: the reason is named owner-binding-failed, not the legacy sidecar-invalid"
else
  bad "C4: authority did not distinguish the recorded failure: $verdict"
fi
if grep -q '"may_recover": false' <<<"$verdict"; then
  ok "C4: may_recover is false -- naming a cause did not soften the verdict"
else
  bad "C4: a named cause must not permit recovery: $verdict"
fi
note ""

note "======================================================================"
note "assertions: $pass passed, $fail failed"
if [[ "$fail" -ne 0 ]]; then
  note "RESULT: FAIL"
  exit 1
fi
note "RESULT: PASS"
