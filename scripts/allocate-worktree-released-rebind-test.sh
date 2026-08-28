#!/usr/bin/env bash
# Bracket the RELEASED-slot rebind in scripts/allocate-worktree.rs.
#
# THE DEFECT: the collision check derived the incumbent owner from
# `existing["agents"]` and never consulted `existing["status"]`. release-worktree
# retains `agents` as the historical record of who held a slot, so a RELEASED
# slot still reported an owner and refused re-allocation to anyone else. That is
# why `validate-rust` could not be rebound to a live agent and why 14 dead-owner
# slots stayed pinned against the 12-active cap with no route to reclamation.
#
# THE SAFETY PROPERTY THAT MUST SURVIVE, and the whole reason for the negative
# cases: the collision check is what stops two mutating agents landing in one
# slot. Widening it by one status must not widen it by two. So this asserts the
# refusal still fires for every state that is NOT a clean release:
#
#   POSITIVE  status=released        -> re-allocation to a DIFFERENT agent ADMITS
#   NEGATIVE  status=active          -> still REFUSED  (the ordinary collision)
#   NEGATIVE  status=owner-lease-revoked -> still REFUSED (revoked != handed back)
#   NEGATIVE  status=releasing       -> still REFUSED (half-finished transaction)
#   CONTROL   released + same agent  -> admits, and is not a collision at all
#   CONTROL   read-mostly sharing    -> unchanged
#
# A "released" slot is one whose owner gave it up. A "revoked" or "releasing"
# slot is one whose disposition is unresolved, and guessing on those is exactly
# the failure the collision check exists to prevent.
#
# Inert: a temp fixture root, its own registry. Never touches the live registry,
# the real worktrees, tmux, or the network.
set -uo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
alloc_src="${ALLOC_SRC:-$script_dir/allocate-worktree.rs}"
root="$(mktemp -d "${TMPDIR:-/tmp}/allocate-released-rebind.XXXXXX")"
trap 'rm -rf "$root"' EXIT

pass=0; fail=0
ok()  { printf '  PASS  %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  FAIL  %s\n' "$*"; fail=$((fail + 1)); }

mkdir -p "$root/scripts" "$root/hermit" "$root/reverie" "$root/liteinst2" \
         "$root/worktrees" "$root/ignored/ci-hub"
touch "$root/.gitmodules"
cp "$alloc_src" "$root/scripts/allocate-worktree.rs"
cp "$script_dir/check-worktree-registry.rs" "$root/scripts/"
cp "$script_dir/worktree_registry.rs" "$root/scripts/"
# `git show HEAD:scripts/allocate-worktree.rs > /tmp/old.rs` -- the documented way
# to produce an ALLOC_SRC build for an A/B -- writes a file with NO executable
# bit. The fixture invokes the script as `./scripts/allocate-worktree.rs`, so
# without this every case dies "Permission denied" and reports REFUSE. That
# reads as a total regression in the old build and is purely an artifact of the
# harness. Cost one A/B cycle on 2026-08-08; do not remove.
chmod +x "$root/scripts/allocate-worktree.rs" "$root/scripts/check-worktree-registry.rs"

git -C "$root" init -q -b main
git -C "$root" config user.email t@example.invalid
git -C "$root" config user.name t
touch "$root/parent-seed"; git -C "$root" add parent-seed .gitmodules
git -C "$root" commit -q -m seed
for p in hermit reverie liteinst2; do
  git -C "$root/$p" init -q -b main
  git -C "$root/$p" config user.email t@example.invalid
  git -C "$root/$p" config user.name t
  touch "$root/$p/seed"; git -C "$root/$p" add seed
  git -C "$root/$p" commit -q -m seed
done
printf '{"schema_version":1,"captured_at":%s,"agents":[]}\n' "$(date +%s)" \
  >"$root/ignored/ci-hub/agent-snapshot.json"

# Plant a slot record in the given status with the given incumbent owner.
#
# EVERY CASE GETS ITS OWN INCUMBENT AND ITS OWN CLAIMANT. The allocator enforces
# one slot per agent, and that rule fires BEFORE the collision check -- so a
# reused agent name makes a later case refuse for an unrelated reason that looks
# exactly like the refusal under test. The message assertions below exist to
# catch precisely that, and they did catch it while this suite was being written.
plant() { # <slot> <status> <incumbent>
  python3 - "$root/worktree-state.json" "$1" "$2" "$3" <<'PY'
import json, os, sys
path, slot, status, incumbent = sys.argv[1:5]
state = json.load(open(path)) if os.path.exists(path) else {
    "version": 3,
    "updated": "2026-08-08T00:00:00Z",
    "slots": {},
}
state.setdefault("slots", {})[slot] = {
    "status": status,
    "agents": [{"name": incumbent, "task": "t", "read_only": False}],
    "hermit_branch": "-", "reverie_branch": "-", "liteinst2_branch": "-",
    "hermit_path": f"worktrees/slots/{slot}/hermit",
    "reverie_path": f"worktrees/slots/{slot}/reverie",
    "liteinst2_path": f"worktrees/slots/{slot}/liteinst2",
    "purpose": "planted by allocate-worktree-released-rebind-test",
    "allocated": "2026-08-08T00:00:00Z",
    "updated": "2026-08-08T00:00:00Z",
}
json.dump(state, open(path, "w"), indent=2)
PY
}

# Try to allocate <slot> to <agent>; echo ADMIT or REFUSE.
try_alloc() { # <slot> <agent> [extra-flags...]
  local slot=$1 agent=$2; shift 2
  if ( cd "$root" && ./scripts/allocate-worktree.rs \
        --agent "$agent" --slot "$slot" --product hermit \
        --task rebind-test --purpose "bracket released rebind" "$@" \
      ) >"$root/out.$slot.$agent" 2>&1; then
    echo ADMIT
  else
    echo REFUSE
  fi
}

check() { # <label> <expected> <actual> <slot> <agent>
  if [[ "$3" == "$2" ]]; then
    ok "$1: $2 (as required)"
  else
    bad "$1: expected $2, got $3"
    sed 's/^/        /' "$root/out.$4.$5" 2>/dev/null | tail -4
  fi
}

# Report the slot's ownership OUTCOME as "<mutating-count>:<mutating names>".
#
# ADMIT/REFUSE alone is not the whole contract, and that gap is what let the
# append bug through: the released rebind was made to ADMIT, and nobody asserted
# how many owners the admit LEFT BEHIND. `agents.push()` appended the claimant
# beside the released incumbent, producing two mutating owners -- a state
# release-worktree.rs correctly refuses to touch ("must have exactly one
# mutating owner, found 2"), so the slot wedges for BOTH agents. Neither can
# clear it: dropping the other's row is what Hard Invariant 5 forbids.
#
# It is invisible in ACTIVE.md, which is rendered FROM this state and shows only
# the FIRST mutating owner -- so the human-readable registry reports a healthy
# single-owner slot while the state file holds the wedge.
owners() { # <slot>
  python3 - "$root/worktree-state.json" "$1" <<'PY'
import json, sys
state = json.load(open(sys.argv[1]))
slot = state.get("slots", {}).get(sys.argv[2])
if slot is None:
    print("MISSING"); raise SystemExit
mut = [a["name"] for a in slot.get("agents", [])
       if not a.get("read_only", False)]
print(f"{len(mut)}:{','.join(sorted(mut))}")
PY
}

check_owners() { # <label> <expected "n:names"> <slot>
  local got; got=$(owners "$3")
  if [[ "$got" == "$2" ]]; then
    ok "$1: mutating owners $got (as required)"
  else
    bad "$1: expected mutating owners $2, got $got"
  fi
}

echo "POSITIVE -- a released slot must be rebindable to a different agent"
plant relslot released inc-rel
got=$(try_alloc relslot new-rel)
check "released -> different agent" ADMIT "$got" relslot new-rel
if grep -qF "was released by 'inc-rel'" "$root/out.relslot.new-rel" 2>/dev/null; then
  ok "released rebind states WHY it was allowed (auditable, not silent)"
else
  bad "released rebind admitted silently; it must say why"
fi
# THE APPEND BUG. Admitting is only half the contract: the rebind must REPLACE
# the released incumbent, not join it. Pre-fix this reported "2:inc-rel,new-rel".
check_owners "released rebind REPLACES the incumbent" "1:new-rel" relslot
echo

echo "NEGATIVE -- every state that is NOT a clean release must STILL refuse"
plant actslot active inc-act
got=$(try_alloc actslot new-act)
check "active -> different agent" REFUSE "$got" actslot new-act
if grep -qF "Refusing collision" "$root/out.actslot.new-act" 2>/dev/null; then
  ok "active refusal still names it as a collision"
else
  bad "active refusal lost its collision message"
fi
# THE COUNTERWEIGHT TO THE FIX ABOVE, and the reason it is scoped to `released`.
# A replace that fired unconditionally would EVICT A LIVE OWNER -- strictly worse
# than the wedge it repairs, because the wedge is loud and an eviction is silent.
# A LIVE incumbent must survive the refusal completely intact: not replaced, and
# not appended to either.
check_owners "active incumbent survives the refusal untouched" "1:inc-act" actslot

plant revslot owner-lease-revoked inc-rev
got=$(try_alloc revslot new-rev)
check "owner-lease-revoked -> different agent" REFUSE "$got" revslot new-rev
check_owners "revoked incumbent left intact" "1:inc-rev" revslot

plant ingslot releasing inc-ing
got=$(try_alloc ingslot new-ing)
check "releasing -> different agent" REFUSE "$got" ingslot new-ing
check_owners "releasing incumbent left intact" "1:inc-ing" ingslot
echo

echo "CONTROLS -- unchanged behaviour"
plant sameslot released inc-same
got=$(try_alloc sameslot inc-same)
check "released -> SAME agent (re-adoption)" ADMIT "$got" sameslot inc-same
# Re-adoption is the documented repair for a lease-less slot. It must stay a
# no-op on ownership -- one owner before, the same one owner after.
check_owners "re-adoption keeps the single owner" "1:inc-same" sameslot

plant shareslot active inc-share
got=$(try_alloc shareslot reader-share --i-promise-this-agent-is-read-mostly)
check "active -> read-mostly sharer" ADMIT "$got" shareslot reader-share
# Read-only sharing must not evict the mutating owner it is joining.
check_owners "read-mostly sharer joins without evicting" "1:inc-share" shareslot

# Read-mostly onto a RELEASED slot: the claimant is not taking mutating
# ownership, so there is nothing to replace. The released incumbent must be left
# exactly as found -- the replace fires on the mutating rebind, not on any
# arrival at a released slot.
plant relshare released inc-relshare
got=$(try_alloc relshare reader-relshare --i-promise-this-agent-is-read-mostly)
check "released -> read-mostly sharer" ADMIT "$got" relshare reader-relshare
check_owners "read-mostly on released does NOT replace" "1:inc-relshare" relshare
echo

echo "ONE-SLOT-PER-AGENT -- the same 'released means unowned' rule, second check"
# The 1-1 mapping check read agents[] without consulting status, so an agent that
# had RELEASED its slot still counted as owning it and could never allocate
# another. Its refusal was UNSATISFIABLE: it said "Release it first" to someone
# who already had, and releasing again cannot help because release deliberately
# retains the row as the historical record.
plant heldslot released mover
got=$(try_alloc newslot mover)
check "agent whose OTHER slot is released may allocate" ADMIT "$got" newslot mover
check_owners "the new slot is owned by the mover" "1:mover" newslot

# The rule itself must survive: a live slot elsewhere still blocks, and so does
# any disposition that is not a clean release.
plant busyslot active stayer
got=$(try_alloc otherslot stayer)
check "agent whose OTHER slot is ACTIVE still refused" REFUSE "$got" otherslot stayer
if grep -qF "1 slot per agent" "$root/out.otherslot.stayer" 2>/dev/null; then
  ok "one-slot-per-agent refusal still names the rule"
else
  bad "one-slot-per-agent refusal lost its message"
fi

# CHANGED 2026-08-12, and the justification is in this file's own words above:
# the `released` exemption exists because the refusal was UNSATISFIABLE -- "it
# said 'Release it first' to someone who already had". A REVOKED slot has the
# identical property and is worse: its owner cannot use it, cannot release it,
# and the reaper skips it because it predates TTL leases. There is no path out
# from inside. Six slots were in exactly that state on 2026-08-12, each
# imprisoning its own owner.
#
# The original assertion here rested on the generalisation "any disposition that
# is not a clean release still blocks" -- carried over from the COLLISION check.
# That generalisation is what was wrong: the two checks answer different
# questions. The collision property is INDEPENDENT and is asserted separately in
# this same file -- "revoked -> REFUSE" for a different agent taking the slot --
# and it still passes, unchanged. Only the one-slot-per-agent case flips.
# Full brackets in allocate-worktree-revoked-owner-test.sh.
plant revheld owner-lease-revoked revmover
got=$(try_alloc revnew revmover)
check "agent whose OTHER slot is revoked may now allocate (deadlock discharge)" ADMIT "$got" revnew revmover
echo

echo "======================================================================"
echo "admits expected and observed: 6 (released/different, released/same,"
echo "  read-mostly on active, read-mostly on released, released-elsewhere/new-slot,"
echo "  revoked-elsewhere/new-slot)"
echo "refusals expected and observed: 4 (active, owner-lease-revoked, releasing,"
echo "  active-elsewhere)"
echo "assertions: $pass passed, $fail failed"
if (( fail )); then echo "RESULT: FAIL"; exit 1; fi
echo "RESULT: PASS"
