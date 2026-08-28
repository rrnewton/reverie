#!/usr/bin/env bash
# Fail closed when an agent has no assigned worktree slot.
#
# WHY. A spawn without `opts.cwd` silently inherits the shared parent root
# resolved below from this script's own location. Nothing errors and nothing
# alarms -- it works
# perfectly until two agents write at once, which on 2026-08-07 produced two
# transient file deletions, a 329-line file nearly destroyed, index contention
# forcing commit-tree with a temp GIT_INDEX_FILE, a `core.bare` primary, and 148
# uncommitted files in the shared tree. The silent default is indistinguishable
# from correct operation, so it must be made loud.
#
# WHAT THIS IS NOT. It cannot stop a spawn: `spawnCliAgent` lives inside the
# compiled `orc` binary, and the one startup hook this repo owns
# (.orc/plugins/hermit-dev/index.ts:851) carries an explicit never-throw
# constraint -- a throwing startup hook has already crash-looped the plugin and
# would take every policy down with it. This is the detection predicate, exact
# and testable, for a caller that decides what to do with the verdict.
#
# REFUSING WITHOUT SAYING HOW TO FIX IT JUST MOVES THE STALL, so every refusal
# names the self-allocation command with the agent's own name filled in.
#
# Usage:  check-agent-slot.sh --agent NAME [--cwd DIR] [--root DIR] [--quiet]
# Exit:   0 OK (in a registered slot, or a declared owner-direct-ask parent use)
#         1 REFUSE (in the shared parent with no slot, or in someone else's slot)
#         2 usage error
set -uo pipefail

agent=""; cwd="${PWD}"; root=""; quiet=0
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
while (($# > 0)); do
  case "$1" in
    --agent) agent=${2:-}; shift 2 ;;
    --cwd)   cwd=${2:-};   shift 2 ;;
    --root)  root=${2:-};  shift 2 ;;
    --quiet) quiet=1; shift ;;
    *) echo "check-agent-slot: unknown argument '$1'" >&2; exit 2 ;;
  esac
done
[[ -n $agent ]] || { echo "check-agent-slot: --agent NAME is required" >&2; exit 2; }

if [[ -z $root ]]; then
  root=$(cd -- "$script_dir/.." && pwd)
fi
state="$root/worktree-state.json"

say() { ((quiet)) || printf '%s\n' "$*" >&2; }

# Canonicalize without requiring the path to exist (tests use synthetic roots).
norm() { printf '%s' "${1%/}"; }
cwd=$(norm "$cwd"); root=$(norm "$root")

# --- where is this agent standing? ------------------------------------------
in_slot=""
if [[ $cwd == "$root/worktrees/slots/"* ]]; then
  rest=${cwd#"$root/worktrees/slots/"}
  in_slot=${rest%%/*}
fi

# --- which slot, if any, does the registry assign to this agent? ------------
assigned=""
if [[ -r $state ]]; then
  registry_error_file=$(mktemp "${TMPDIR:-/tmp}/check-agent-slot.XXXXXX")
  assigned=$("$script_dir/check-worktree-registry.rs" --assigned-slot "$agent" \
    <"$state" 2>"$registry_error_file") || {
      registry_error=$(<"$registry_error_file")
      rm -f -- "$registry_error_file"
      say "REFUSE: worktree registry cannot determine agent '$agent' assignment."
      say "  $registry_error"
      exit 1
    }
  rm -f -- "$registry_error_file"
fi

allocate="./scripts/allocate-worktree.rs --agent $agent --task <task-id> --product all --purpose \"<one line>\""

# --- verdicts ----------------------------------------------------------------
if [[ -n $in_slot ]]; then
  if [[ -z $assigned ]]; then
    say "REFUSE: agent '$agent' is working in worktrees/slots/$in_slot but the registry assigns it NO slot."
    say "  Working in an unassigned slot is Invariant 2 territory -- another agent may own it."
    say "  Allocate your own:"
    say "    $allocate"
    exit 1
  fi
  if [[ $in_slot != "$assigned" ]]; then
    say "REFUSE: agent '$agent' is in worktrees/slots/$in_slot but the registry assigns it worktrees/slots/$assigned."
    say "  Do not borrow another agent's slot. Move to your own:"
    say "    cd $root/worktrees/slots/$assigned"
    exit 1
  fi
  say "OK: agent '$agent' is in its registered slot worktrees/slots/$assigned."
  exit 0
fi

if [[ $cwd == "$root" || $cwd == "$root/"* ]]; then
  # The shared parent. Legitimate ONLY as the owner's direct-ask case, and that
  # has to be declared rather than defaulted -- a default is what got us here.
  if [[ ${HERMIT_OWNER_DIRECT_ASK:-} == "1" ]]; then
    say "OK: agent '$agent' is in the shared parent under a DECLARED owner-direct-ask (HERMIT_OWNER_DIRECT_ASK=1)."
    say "  Parent policy: reads are fine; every WRITE here is the hazard. Keep it to what the owner asked for."
    exit 0
  fi
  # The headline must not assert something the body then contradicts: an agent
  # that HAS a slot but is standing in the parent is misplaced, not unassigned.
  if [[ -n $assigned ]]; then
    say "REFUSE: agent '$agent' is standing in the SHARED PARENT ($root) instead of its assigned slot worktrees/slots/$assigned."
  else
    say "REFUSE: agent '$agent' has NO assigned worktree slot and is standing in the SHARED PARENT ($root)."
  fi
  say "  This is a silent-inheritance spawn (no opts.cwd). It looks fine until two agents write at once:"
  say "  on 2026-08-07 it caused two transient deletions, a near-destroyed 329-line file, and index contention."
  if [[ -n $assigned ]]; then
    say "  The registry DOES assign you a slot -- go there:"
    say "    cd $root/worktrees/slots/$assigned"
  else
    say "  Allocate your own slot, then work only inside it:"
    say "    $allocate"
    say "    cd $root/worktrees/slots/$agent   # or the --slot you chose"
  fi
  say "  If this really is an owner direct ask, declare it: HERMIT_OWNER_DIRECT_ASK=1"
  exit 1
fi

say "OK: agent '$agent' is outside the shared parent ($cwd); this guard has no claim on it."
exit 0
