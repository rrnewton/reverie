#!/usr/bin/env bash
# Bracket the REVOKED-OWNER discharge in scripts/allocate-worktree.rs.
#
# THE DEADLOCK. A slot in `owner-lease-revoked` counted toward one-slot-per-agent,
# so its owner could not allocate anywhere else. It also could not use the slot
# (revoked), could not release it (revoked), and the reaper skips it because it
# predates TTL leases. There was NO PATH OUT FROM INSIDE THE SYSTEM. Measured
# 2026-08-12: six such slots -- codex-rescue, codex-tickgate, codex-validatelock,
# slot04, slot74, w7 -- each imprisoning exactly its own owner, one of them
# blocking the owner's stated priority task.
#
# This is the SAME unsatisfiable-refusal defect already fixed for `released`: the
# error names a remedy the agent cannot perform.
#
# ⚠ THE PROPERTY THAT MUST NOT MOVE. Fixing this by making the allocator
# permissive would clear the bar by lowering it. Two checks answer DIFFERENT
# questions and only one changes:
#
#   this check      "may agent A, owning revoked X, take a NEW slot Y?"  -> now ADMIT
#   collision check "may agent B take revoked slot X?"                   -> still REFUSE
#
# so the negatives below are not decoration; they are the whole point.
#
#   POSITIVE  owner of a REVOKED slot allocates elsewhere      -> ADMIT
#   NEGATIVE  owner of an ACTIVE slot allocates elsewhere      -> still REFUSE
#   NEGATIVE  owner of a RELEASING slot allocates elsewhere    -> still REFUSE
#   NEGATIVE  a DIFFERENT agent tries to take the revoked slot -> still REFUSE
#   CONTROL   owner of a RELEASED slot allocates elsewhere     -> ADMIT (pre-existing)
#
# Inert: a temp fixture root, its own registry. Never touches the live registry,
# the real worktrees, tmux, or the network.
set -uo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
alloc_src="${ALLOC_SRC:-$script_dir/allocate-worktree.rs}"
root="$(mktemp -d "${TMPDIR:-/tmp}/allocate-revoked-owner.XXXXXX")"
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

expect() { # <expected> <got> <label>
  if [ "$1" = "$2" ]; then ok "$3 -> $2"; else bad "$3 -> got $2, wanted $1"; fi
}

# Each case gets its OWN incumbent and its OWN target slot, because
# one-slot-per-agent fires before the collision check and a reused name makes a
# case refuse for an unrelated reason that looks identical.
plant own-revoked   owner-lease-revoked agent-revoked
plant own-active    active              agent-active
plant own-releasing releasing           agent-releasing
plant own-released  released            agent-released

echo "== the discharge =="
expect ADMIT  "$(try_alloc target-a agent-revoked)"   "owner of a REVOKED slot may allocate elsewhere"

echo "== the properties that must not move =="
expect REFUSE "$(try_alloc target-b agent-active)"    "owner of an ACTIVE slot still blocked"
expect REFUSE "$(try_alloc target-c agent-releasing)" "owner of a RELEASING slot still blocked"
expect REFUSE "$(try_alloc own-revoked agent-other)"  "a DIFFERENT agent still cannot take the revoked slot"

echo "== pre-existing behaviour, unchanged =="
expect ADMIT  "$(try_alloc target-d agent-released)"  "owner of a RELEASED slot may allocate elsewhere"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
