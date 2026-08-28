#!/usr/bin/env bash
# Bracketed (Proxy-Binding) test for the OWNER-LEASE REFUSAL WORDING in
# scripts/allocate-worktree.rs::observe_owner_lease.
#
# WHAT IS UNDER TEST IS THE MESSAGE, NOT THE POLICY. The lease must keep failing
# closed in exactly the cases it failed closed before; only the text that
# explains WHY is split by cause. So every case below asserts BOTH halves:
#
#   (a) the message names the correct cause, and
#   (b) the lease is still UNBOUND in worktree-state.json.
#
# (b) is the guard that matters. A rewording that accidentally admitted would
# still print nice text, so the wording assertions alone would pass a broken
# gate. Binding is the observable that proves the refusal fired.
#
# WHY THIS EXISTS: on 2026-08-08 a snapshot that was 52s old but held only
# {name:"worker", tmux_pane_id:null} made every real agent resolve zero times.
# The single message "owner snapshot resolves agent X 0 times" was read as
# "X is not a real agent". Four slots were recorded permanently lease-less,
# adoption was re-run against a remedy that could not work, and one
# release-then-reallocate left the validate-rust slot in a collision state.
#
#   IMPLAUSIBLE (stub / empty / dead panes) -> the message must accuse the SNAPSHOT
#   PLAUSIBLE  (real fleet, agent not in it) -> the message must accuse the AGENT
#   INVALID    (agent listed twice)          -> the shared type must name the duplicate
#   PLAUSIBLE  (agent present on a live pane)-> the lease must BIND (admit control)
#
# The last case is not decoration: without it this suite would pass on a build
# that refuses unconditionally.
#
# Runs entirely in a temp fixture root. It NEVER writes the live
# ignored/ci-hub/agent-snapshot.json and never touches the live registry. It
# does READ this box's real tmux pane list, because "is this pane real" is the
# predicate under test and a fabricated pane id cannot exercise it.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
root="$(mktemp -d "${TMPDIR:-/tmp}/allocate-lease-message-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

pass=0
fail=0
note() { printf '%s\n' "$*"; }
ok()   { printf '  PASS  %s\n' "$*"; pass=$((pass + 1)); }
bad()  { printf '  FAIL  %s\n' "$*"; fail=$((fail + 1)); }

# A pane that genuinely exists on this box, used to make a fixture PLAUSIBLE.
live_pane="$(tmux list-panes -a -F '#{pane_id}' 2>/dev/null | head -1 || true)"
if [[ -z "$live_pane" ]]; then
  note "SKIP: no live tmux panes visible; the plausible-snapshot cases cannot be exercised."
  note "This suite requires a real pane because the predicate under test is pane reality."
  exit 2
fi
live_pane_count="$(tmux list-panes -a -F '#{pane_id}' | wc -l)"
note "Fixture root: $root"
note "Using live pane $live_pane of $live_pane_count visible on this box (read-only)."
note ""

# ---------------------------------------------------------------- fixture ----
# dev-hermit root shape that find_root() looks for.
mkdir -p "$root/scripts" "$root/hermit" "$root/reverie" "$root/liteinst2" \
         "$root/worktrees" "$root/ignored/ci-hub"
touch "$root/.gitmodules"
# ALLOC_SRC lets an A/B run point this suite at a different build of the
# allocator (e.g. the pre-change version out of git) to prove that only the
# WORDING moved and the bind/refuse pattern did not.
cp "${ALLOC_SRC:-$script_dir/allocate-worktree.rs}" "$root/scripts/allocate-worktree.rs"
cp "$script_dir/check-worktree-registry.rs" "$root/scripts/"
cp "$script_dir/worktree_registry.rs" "$root/scripts/"
cp "$script_dir/agent_snapshot.rs" "$root/scripts/"

git -C "$root" init -q -b main
git -C "$root" config user.email test@example.invalid
git -C "$root" config user.name test
touch "$root/parent-seed"
git -C "$root" add parent-seed .gitmodules
git -C "$root" commit -q -m 'seed parent checkout'

for product in hermit reverie liteinst2; do
  git -C "$root/$product" init -q -b main
  git -C "$root/$product" config user.email test@example.invalid
  git -C "$root/$product" config user.name test
  touch "$root/$product/seed"
  git -C "$root/$product" add seed
  git -C "$root/$product" commit -q -m 'seed product primary'
done

write_snapshot() { # <json-agents-array>
  cat >"$root/ignored/ci-hub/agent-snapshot.json" <<JSON
{
  "schema_version": 1,
  "captured_at": $(date +%s),
  "agents": $1
}
JSON
}

# Allocate into a fresh slot and capture stderr. Allocation itself is expected
# to SUCCEED (rc 0) in every case -- an unbindable lease has always been a
# warning, not an allocation failure. The refusal we are testing is the LEASE
# refusal, observed as an absent lease in the registry.
allocate() { # <agent> <slot>
  ( cd "$root" && ./scripts/allocate-worktree.rs \
      --agent "$1" --slot "$2" --product hermit \
      --task "lease-msg-test" --purpose "bracket the lease refusal wording" \
  ) >"$root/out.$2" 2>"$root/err.$2" || true
  cat "$root/out.$2" "$root/err.$2"
}

# Did a lease bind for this slot? Reads the fixture registry only.
lease_bound() { # <slot>
  python3 - "$root/worktree-state.json" "$1" <<'PY'
import json, sys
state = json.load(open(sys.argv[1]))
slot = state.get("slots", {}).get(sys.argv[2], {})
for agent in slot.get("agents", []):
    if agent.get("tmux_pane_id") or agent.get("cgroup_path"):
        print("BOUND", agent.get("tmux_pane_id"), agent.get("cgroup_path"))
        break
else:
    print("UNBOUND")
PY
}

# The allocator refuses a second slot for an agent that already owns one. If a
# case is refused for such an unrelated reason it never reaches the lease code,
# and every wording assertion would fail for the wrong reason. Prove the slot
# was really registered before judging any message.
assert_reached() { # <slot> <label>
  if python3 -c 'import json,sys; sys.exit(0 if sys.argv[2] in json.load(open(sys.argv[1])).get("slots",{}) else 1)' \
       "$root/worktree-state.json" "$1" 2>/dev/null; then
    ok "$2: allocation reached the lease path (slot registered)"
  else
    bad "$2: slot never registered -- allocation was refused BEFORE the lease code; wording assertions below are meaningless. Allocator said:"
    sed 's/^/        /' "$root/err.$1" | head -5
  fi
}

expect_unbound() { # <slot> <label>
  local got
  got="$(lease_bound "$1")"
  if [[ "$got" == UNBOUND ]]; then
    ok "$2: lease is UNBOUND -- refusal fired, gate still fail-closed"
  else
    bad "$2: lease BOUND ($got) -- THE GATE ADMITTED. Wording change broke the refusal."
  fi
}

has()    { grep -qF -- "$2" "$root/err.$1"; }
assert_has()    { if has "$1" "$2"; then ok "$3"; else bad "$3 (missing: $2)"; fi; }
assert_lacks()  { if has "$1" "$2"; then bad "$3 (present but must not be: $2)"; else ok "$3"; fi; }

# ============================================================ CASE 1: STUB ====
# The exact historical payload that caused the incident.
note "CASE 1  IMPLAUSIBLE / stub  -- agents=[{name:worker, tmux_pane_id:null}]"
write_snapshot '[{"name": "worker", "status": "busy", "tmux_pane_id": null}]'
allocate sol-stub case1 >/dev/null
assert_reached case1 "C1"
assert_has   case1 "IS NOT USABLE"            "C1 message accuses the SNAPSHOT"
assert_has   case1 "not one of its 1 entries carries a usable tmux pane id" \
                                              "C1 states the concrete defect and the count"
assert_has   case1 "says nothing about whether 'sol-stub' exists" \
                                              "C1 explicitly disclaims the agent-absence reading"
assert_has   case1 "do NOT release or reallocate" \
                                              "C1 warns off the action that broke validate-rust"
assert_lacks case1 "is absent from the owner snapshot" \
                                              "C1 does NOT blame the agent"
expect_unbound case1 "C1"
note ""

# =========================================================== CASE 2: EMPTY ====
note "CASE 2  IMPLAUSIBLE / empty  -- agents=[]"
write_snapshot '[]'
allocate sol-empty case2 >/dev/null
assert_reached case2 "C2"
assert_has   case2 "IS NOT USABLE"        "C2 message accuses the SNAPSHOT"
assert_has   case2 "it lists no agents at all" "C2 names emptiness as the defect"
assert_lacks case2 "is absent from the owner snapshot" "C2 does NOT blame the agent"
expect_unbound case2 "C2"
note ""

# ==================================================== CASE 3: DEAD PANE IDS ====
# Syntactically valid %NN pane ids that do not exist on this box. This is the
# branch that a purely syntactic check would misclassify as a good snapshot.
note "CASE 3  IMPLAUSIBLE / dead panes -- pane ids well-formed but not live"
write_snapshot '[{"name": "ghost-a", "status": "busy", "tmux_pane_id": "%99991"},
                 {"name": "ghost-b", "status": "busy", "tmux_pane_id": "%99992"}]'
allocate sol-dead case3 >/dev/null
assert_reached case3 "C3"
assert_has   case3 "IS NOT USABLE"                    "C3 message accuses the SNAPSHOT"
assert_has   case3 "exists in \`tmux list-panes -a\`" "C3 names the liveness cross-check"
assert_has   case3 "not running here"                 "C3 explains the payload is a dead fleet"
assert_lacks case3 "is absent from the owner snapshot" "C3 does NOT blame the agent"
expect_unbound case3 "C3"
note ""

# ============================== CASE 4: PLAUSIBLE SNAPSHOT, AGENT GENUINELY ABSENT ====
note "CASE 4  PLAUSIBLE / agent absent -- real live pane present, queried agent not listed"
write_snapshot '[{"name": "someone-else", "status": "busy", "tmux_pane_id": "'"$live_pane"'"}]'
allocate sol-nobody case4 >/dev/null
assert_reached case4 "C4"
assert_has   case4 "agent 'sol-nobody' is absent from the owner snapshot" \
                                             "C4 message accuses the AGENT"
assert_has   case4 "which does carry a real fleet" \
                                             "C4 states the snapshot was judged good"
assert_has   case4 "Re-run this exact allocation/adoption from the live pane" \
                                             "C4 gives the remedy that CAN work here"
assert_lacks case4 "IS NOT USABLE"           "C4 does NOT blame the snapshot"
expect_unbound case4 "C4"
note ""

# ================================================ CASE 5: DUPLICATE ENTRIES ====
note "CASE 5  INVALID / duplicate -- agent listed twice"
write_snapshot '[{"name": "sol-dup", "status": "busy", "tmux_pane_id": "'"$live_pane"'"},
                 {"name": "sol-dup", "status": "busy", "tmux_pane_id": "'"$live_pane"'"}]'
allocate sol-dup case5 >/dev/null
assert_reached case5 "C5"
assert_has   case5 "owner snapshot is invalid"               "C5 refuses at the shared contract"
assert_has   case5 'duplicate agent name "sol-dup"'           "C5 names the duplicate agent"
assert_lacks case5 "IS NOT USABLE"                           "C5 does NOT blame the snapshot"
expect_unbound case5 "C5"
note ""

# ====================================== CASE 6: ADMIT CONTROL -- MUST BIND ====
# Without this, a build that refuses unconditionally would pass every case above.
note "CASE 6  ADMIT CONTROL -- agent present on a genuinely live pane; lease MUST bind"
write_snapshot '[{"name": "sol-live", "status": "busy", "tmux_pane_id": "'"$live_pane"'"}]'
allocate sol-live case6 >/dev/null
assert_reached case6 "C6"
got="$(lease_bound case6)"
if [[ "$got" == BOUND* ]]; then
  ok "C6: lease BOUND ($got) -- the path still admits a real owner"
else
  bad "C6: lease $got -- refusal now fires on a GOOD snapshot; this is over-refusal, not a wording fix."
fi
assert_lacks case6 "IS NOT USABLE"                     "C6 prints no snapshot accusation"
assert_lacks case6 "is absent from the owner snapshot" "C6 prints no agent accusation"
note ""

# ------------------------------------------------------------------ result ----
note "======================================================================"
note "cases bracketed: 3 implausible (stub/empty/dead-pane), 1 invalid"
note "(duplicate), 1 plausible absent-agent case, 1 admit control."
note "refusals expected and observed: 5.  admits expected and observed: 1."
note "assertions: $pass passed, $fail failed"
if (( fail )); then
  note "RESULT: FAIL"
  exit 1
fi
note "RESULT: PASS"
