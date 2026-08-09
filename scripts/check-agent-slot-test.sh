#!/usr/bin/env bash
# Bracket check-agent-slot.sh BOTH WAYS on a synthetic root: every refusal fires
# on a planted violating case, and every OK fires on a planted qualifying case.
# A guard that blocks every spawn is as useless as one that blocks none.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-agent-slot.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/check-agent-slot-test.XXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT

mkdir -p "$tmp/worktrees/mine/hermit" "$tmp/worktrees/theirs/hermit"
cat >"$tmp/worktree-state.json" <<'JSON'
{
  "version": 3,
  "slots": {
    "mine":   { "agents": [{ "name": "sol-example", "read_only": false }] },
    "theirs": { "agents": [{ "name": "other-agent", "read_only": false }] }
  }
}
JSON

pass=0; fail=0
check() { # name expected_rc must_contain agent cwd [env...]
  local name=$1 want=$2 needle=$3 agent=$4 dir=$5; shift 5
  local out rc=0
  out=$(env "$@" "$guard" --agent "$agent" --cwd "$dir" --root "$tmp" 2>&1) || rc=$?
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
check no-slot-names-allocator  1 "allocate-worktree.rs --agent sol-nobody" sol-nobody "$tmp"        IGNORE=1
check assigned-but-in-parent   1 "instead of its assigned slot worktrees/mine" sol-example "$tmp" IGNORE=1
check assigned-headline-honest 1 "cd $tmp/worktrees/mine"    sol-example "$tmp"                     IGNORE=1
check borrowing-another-slot   1 "assigns it worktrees/mine" sol-example "$tmp/worktrees/theirs"    IGNORE=1
check unassigned-in-some-slot  1 "registry assigns it NO slot" sol-nobody "$tmp/worktrees/mine"     IGNORE=1
check parent-subdir-counts     1 "NO assigned worktree slot" sol-nobody  "$tmp/ci-hub"              IGNORE=1

# ---- ALLOWED: the planted qualifying case must fire, not sit inert ----------
check own-slot-ok              0 "registered slot worktrees/mine" sol-example "$tmp/worktrees/mine" IGNORE=1
check own-slot-nested-ok       0 "registered slot worktrees/mine" sol-example "$tmp/worktrees/mine/hermit" IGNORE=1
check declared-owner-ask-ok    0 "DECLARED owner-direct-ask"  sol-nobody  "$tmp"  HERMIT_OWNER_DIRECT_ASK=1
check outside-parent-ok        0 "has no claim on it"         sol-nobody  "/tmp"                    IGNORE=1

# ---- the refusal must always be ACTIONABLE ----------------------------------
out=$("$guard" --agent sol-nobody --cwd "$tmp" --root "$tmp" 2>&1) || true
if grep -qF "allocate-worktree.rs" <<<"$out" && grep -qF "HERMIT_OWNER_DIRECT_ASK=1" <<<"$out"; then
  echo "ok   [refusal-is-actionable] names the allocator AND the declared-exception escape"; pass=$((pass+1))
else
  echo "FAIL [refusal-is-actionable]: a refusal that does not say how to fix it just moves the stall"; fail=$((fail+1))
fi

echo
echo "check-agent-slot-test: $pass passed, $fail failed"
((fail == 0)) || exit 1
