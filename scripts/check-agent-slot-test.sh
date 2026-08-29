#!/usr/bin/env bash
# Bracket check-agent-slot.sh BOTH WAYS on a synthetic root: every refusal fires
# on a planted violating case, and every OK fires on a planted qualifying case.
# A guard that blocks every spawn is as useless as one that blocks none.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-agent-slot.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/check-agent-slot-test.XXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT

mkdir -p "$tmp/worktrees/slots/mine/hermit" "$tmp/worktrees/slots/theirs/hermit"
cat >"$tmp/worktree-state.json" <<'JSON'
{
  "version": 3,
  "updated": "2026-08-28T00:00:00Z",
  "slots": {
    "old": {
      "agents": [{ "name": "sol-example", "read_only": false, "task": "old-task" }],
      "status": "released",
      "hermit_path": "worktrees/slots/old/hermit", "hermit_branch": "detached",
      "reverie_path": "worktrees/slots/old/reverie", "reverie_branch": "-",
      "liteinst2_path": "worktrees/slots/old/liteinst2", "liteinst2_branch": "-"
    },
    "revoked": {
      "agents": [{ "name": "sol-example", "read_only": false, "task": "old-task" }],
      "status": "owner-lease-revoked",
      "hermit_path": "worktrees/slots/revoked/hermit", "hermit_branch": "detached",
      "reverie_path": "worktrees/slots/revoked/reverie", "reverie_branch": "-",
      "liteinst2_path": "worktrees/slots/revoked/liteinst2", "liteinst2_branch": "-"
    },
    "mine": {
      "agents": [{ "name": "sol-example", "read_only": false, "task": "task-a" }],
      "allocated": "2026-08-28T00:00:00Z", "updated": "2026-08-28T00:00:00Z",
      "status": "active", "task": "task-a",
      "hermit_path": "worktrees/slots/mine/hermit", "hermit_branch": "detached",
      "reverie_path": "worktrees/slots/mine/reverie", "reverie_branch": "-",
      "liteinst2_path": "worktrees/slots/mine/liteinst2", "liteinst2_branch": "-"
    },
    "theirs": {
      "agents": [{ "name": "other-agent", "read_only": false, "task": "task-b" }],
      "allocated": "2026-08-28T00:00:00Z", "updated": "2026-08-28T00:00:00Z",
      "status": "active", "task": "task-b",
      "hermit_path": "worktrees/slots/theirs/hermit", "hermit_branch": "detached",
      "reverie_path": "worktrees/slots/theirs/reverie", "reverie_branch": "-",
      "liteinst2_path": "worktrees/slots/theirs/liteinst2", "liteinst2_branch": "-"
    }
  }
}
JSON

cat >"$tmp/wrkslots" <<'PY'
#!/usr/bin/env python3
import json
import os
import pathlib
import sys

args = sys.argv[1:]
root = pathlib.Path(args[args.index("--project-root") + 1])
state = json.loads((root / "worktree-state.json").read_text())
if os.environ.get("FAKE_WRKSLOTS_WARNING"):
    print(os.environ["FAKE_WRKSLOTS_WARNING"], file=sys.stderr)
active = []
for slot, row in state["slots"].items():
    if row.get("status") != "active":
        continue
    for owner in row.get("agents", []):
        active.append({"slot": slot, "slot_type": "agent", "agent": owner["name"]})
print(json.dumps({"active": active}))
PY
chmod +x "$tmp/wrkslots"

pass=0; fail=0
check() { # name expected_rc must_contain agent cwd [env...]
  local name=$1 want=$2 needle=$3 agent=$4 dir=$5; shift 5
  local out rc=0
  out=$(env "$@" "$guard" --agent "$agent" --cwd "$dir" --root "$tmp" \
    --wrkslots "$tmp/wrkslots" 2>&1) || rc=$?
  if ((rc != want)); then
    echo "FAIL [$name]: expected rc=$want got rc=$rc"; echo "$out" | sed 's/^/    /'; fail=$((fail+1)); return
  fi
  if [[ -n $needle ]] && ! grep -qF "$needle" <<<"$out"; then
    echo "FAIL [$name]: output did not contain '$needle'"; echo "$out" | sed 's/^/    /'; fail=$((fail+1)); return
  fi
  echo "ok   [$name] rc=$rc, said '${needle:0:46}'"; pass=$((pass+1))
}

# ---- REFUSALS: the planted violating case must be refused -------------------
check no-slot-in-parent        1 "NO assigned worktree slot" sol-nobody  "$tmp"                     IGNORE=1
check no-slot-names-remedy     1 "ask the coordinator" sol-nobody "$tmp"                           IGNORE=1
check assigned-but-in-parent   1 "instead of its assigned slot worktrees/slots/mine" sol-example "$tmp" IGNORE=1
check assigned-headline-honest 1 "cd $tmp/worktrees/slots/mine"    sol-example "$tmp"                     IGNORE=1
check borrowing-another-slot   1 "assigns it worktrees/slots/mine" sol-example "$tmp/worktrees/slots/theirs"    IGNORE=1
check unassigned-in-some-slot  1 "registry assigns it NO slot" sol-nobody "$tmp/worktrees/slots/mine"     IGNORE=1
check parent-subdir-counts     1 "NO assigned worktree slot" sol-nobody  "$tmp/ci-hub"              IGNORE=1

# ---- ALLOWED: the planted qualifying case must fire, not sit inert ----------
check own-slot-ok              0 "registered slot worktrees/slots/mine" sol-example "$tmp/worktrees/slots/mine" IGNORE=1
check own-slot-nested-ok       0 "registered slot worktrees/slots/mine" sol-example "$tmp/worktrees/slots/mine/hermit" IGNORE=1
check own-slot-warning-visible 0 "historical worktree retained" sol-example "$tmp/worktrees/slots/mine" \
  FAKE_WRKSLOTS_WARNING="historical worktree retained" IGNORE=1
check declared-owner-ask-ok    0 "DECLARED owner-direct-ask"  sol-nobody  "$tmp"  HERMIT_OWNER_DIRECT_ASK=1
check outside-parent-ok        0 "has no claim on it"         sol-nobody  "/tmp"                    IGNORE=1

# Two current rows for one agent are an invalid assignment, not permission to
# choose whichever key happens to appear first in the JSON object.
python3 - "$tmp/worktree-state.json" <<'PY'
import json
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
row = dict(state["slots"]["mine"])
row["hermit_path"] = "worktrees/slots/second/hermit"
row["reverie_path"] = "worktrees/slots/second/reverie"
row["liteinst2_path"] = "worktrees/slots/second/liteinst2"
state["slots"]["second"] = row
path.write_text(json.dumps(state))
PY
check ambiguous-current-slots 1 "ambiguous assignment" sol-example "$tmp" IGNORE=1

# ---- the refusal must always be ACTIONABLE ----------------------------------
out=$("$guard" --agent sol-nobody --cwd "$tmp" --root "$tmp" --wrkslots "$tmp/wrkslots" 2>&1) || true
if grep -qF "ask the coordinator" <<<"$out" && grep -qF "HERMIT_OWNER_DIRECT_ASK=1" <<<"$out"; then
  echo "ok   [refusal-is-actionable] names the coordinator remedy AND the declared-exception escape"; pass=$((pass+1))
else
  echo "FAIL [refusal-is-actionable]: a refusal that does not say how to fix it just moves the stall"; fail=$((fail+1))
fi

echo
echo "check-agent-slot-test: $pass passed, $fail failed"
((fail == 0)) || exit 1
