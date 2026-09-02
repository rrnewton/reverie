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
# tells the agent to ask the coordinator for a wrkslots assignment.
#
# Usage:  check-agent-slot.sh --agent NAME [--cwd DIR] [--root DIR]
#                              [--wrkslots PATH] [--quiet]
# Exit:   0 OK (in a registered slot, or a declared owner-direct-ask parent use)
#         1 REFUSE (in the shared parent with no slot, or in someone else's slot)
#         2 usage error
set -uo pipefail

agent=""; cwd="${PWD}"; root=""; wrkslots=""; quiet=0
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
while (($# > 0)); do
  case "$1" in
    --agent) agent=${2:-}; shift 2 ;;
    --cwd)   cwd=${2:-};   shift 2 ;;
    --root)  root=${2:-};  shift 2 ;;
    --wrkslots) wrkslots=${2:-}; shift 2 ;;
    --quiet) quiet=1; shift ;;
    -h|--help)
      echo "usage: check-agent-slot.sh --agent NAME [--cwd DIR] [--root DIR] [--wrkslots PATH] [--quiet]"
      exit 0
      ;;
    *) echo "check-agent-slot: unknown argument '$1'" >&2; exit 2 ;;
  esac
done
[[ -n $agent ]] || { echo "check-agent-slot: --agent NAME is required" >&2; exit 2; }

if [[ -z $root ]]; then
  root=$(cd -- "$script_dir/.." && pwd)
fi
if [[ -z $wrkslots ]]; then
  wrkslots="$root/ci-hub/bin/wrkslots"
fi

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
if [[ ! -x $wrkslots ]]; then
  say "REFUSE: wrkslots is unavailable at $wrkslots."
  say "  state: REFUSED -- this guard could not determine the agent's assignment."
  say "  remedy: initialize hermit/agent-utils and use --wrkslots PATH if the project launcher is elsewhere."
  exit 1
fi
status_file=$(mktemp "${TMPDIR:-/tmp}/check-agent-slot.status.XXXXXX")
error_file=$(mktemp "${TMPDIR:-/tmp}/check-agent-slot.error.XXXXXX")
assignment_error_file=$(mktemp "${TMPDIR:-/tmp}/check-agent-slot.assignment-error.XXXXXX")
trap 'rm -f -- "$status_file" "$error_file" "$assignment_error_file"' EXIT
if ! "$wrkslots" --project-root "$root" --allow-existing-unregistered-worktrees \
    status --all-machines --format json \
    >"$status_file" 2>"$error_file"; then
  registry_error=$(<"$error_file")
  say "REFUSE: wrkslots could not determine agent '$agent' assignment."
  say "  state: REFUSED -- registry state is unavailable, so this path is not treated as free."
  say "  detail: ${registry_error:-wrkslots returned no diagnostic}"
  say "  remedy: run 'ci-hub/bin/wrkslots doctor --all-machines' and repair the reported state."
  exit 1
fi
if [[ -s $error_file ]]; then
  while IFS= read -r line; do
    say "$line"
  done <"$error_file"
fi
assigned=$(python3 - "$agent" "$status_file" 2>"$assignment_error_file" <<'PY'
import json
import sys

agent, path = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    payload = json.load(handle)
matches = sorted((
    (
        row["slot"],
        row,
    )
    for row in payload.get("active", [])
    if row.get("slot_type") == "agent" and row.get("agent") == agent
), key=lambda item: item[0])
if len(matches) > 1:
    print(
        f"wrkslots returned multiple active agent slots for {agent!r}: "
        + ", ".join(slot for slot, _row in matches),
        file=sys.stderr,
    )
    raise SystemExit(4)
if not matches:
    print("")
    raise SystemExit(0)

slot, row = matches[0]
if "storage_inconsistencies" not in row:
    print(
        f"wrkslots row for agent {agent!r} slot {slot!r} has no "
        "storage_inconsistencies field; update the wrkslots producer before "
        "using this row as assignment evidence",
        file=sys.stderr,
    )
    raise SystemExit(4)
inconsistencies = row["storage_inconsistencies"]
if not isinstance(inconsistencies, list):
    print(
        f"wrkslots row for agent {agent!r} slot {slot!r} has malformed "
        "storage_inconsistencies; expected a list",
        file=sys.stderr,
    )
    raise SystemExit(4)
if inconsistencies:
    print(
        f"wrkslots reports {len(inconsistencies)} storage inconsistency finding(s) "
        f"for agent {agent!r} slot {slot!r}",
        file=sys.stderr,
    )
    for index, finding in enumerate(inconsistencies, start=1):
        if not isinstance(finding, dict):
            print(f"  finding {index}: malformed entry {finding!r}", file=sys.stderr)
            continue
        kind = finding.get("kind", "unknown")
        detail = finding.get("detail", "no detail")
        remedy = finding.get("remedy", "run 'wrkslots status --all-machines'")
        print(f"  finding {index}: kind={kind}: {detail}", file=sys.stderr)
        print(f"    remedy: {remedy}", file=sys.stderr)
    raise SystemExit(5)
print(slot)
PY
) || {
  assignment_error=$(<"$assignment_error_file")
  say "REFUSE: wrkslots returned unusable assignment evidence for agent '$agent'."
  say "  state: REFUSED -- an ambiguous assignment, malformed evidence, or a selected-row inconsistency cannot authorize this path."
  say "  detail: ${assignment_error:-the assignment parser returned no diagnostic}"
  say "  remedy: follow the selected finding, then rerun 'ci-hub/bin/wrkslots status --all-machines --format json'."
  exit 1
}

# --- verdicts ----------------------------------------------------------------
if [[ -n $in_slot ]]; then
  if [[ -z $assigned ]]; then
    say "REFUSE: agent '$agent' is working in worktrees/slots/$in_slot but the registry assigns it NO slot."
    say "  Working in an unassigned slot is Invariant 2 territory -- another agent may own it."
    say "  state: REFUSED -- this slot is not recorded as yours."
    say "  remedy: ask the coordinator to assign a slot through ci-hub/bin/wrkslots."
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
    say "  state: REFUSED -- no writable checkout was assigned."
    say "  remedy: ask the coordinator to create and assign a slot through ci-hub/bin/wrkslots."
  fi
  say "  If this really is an owner direct ask, declare it: HERMIT_OWNER_DIRECT_ASK=1"
  exit 1
fi

say "OK: agent '$agent' is outside the shared parent ($cwd); this guard has no claim on it."
exit 0
