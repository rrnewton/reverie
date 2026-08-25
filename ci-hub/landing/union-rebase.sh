#!/usr/bin/env bash
# Authoritative additive union-rebase for the e2e / backend-parity manifest
# bucket. Rebase one PR branch onto current origin/main, then AUTHORITATIVELY
# recompute every managed append-only registry the PR touched by a pure additive
# 3-way delta-union (scripts/e2e-union-resolve.py) from (fork-point, main,
# PR-tip) -- overwriting whatever git's line-level merge produced.
#
# This is required because git silently corrupts adjacent TOML block additions
# (two sides appending a [[test]] at the same anchor collapse the shared header
# line, yielding one table with two `id` keys and NO conflict). Trusting git's
# merge for these files is unsafe; the semantic resolver is the source of truth.
#
# Managed source registries:
#   tests/e2e/manifests/*.toml                      -> [[test]] blocks by id
#   tests/e2e/manifests/inventory/test-files.json   -> files[] by path
#   tests/backend-parity/matrix.tsv                 -> rows by test_name
# ci/expected-e2e-plan.json is the DERIVED ratchet: regenerated from the merged
# manifests via ci/test_harness.sh plan, never hand-unioned.
#
# SAFE: additive-only. A key on both sides with differing content, or any
# conflict on a file outside the managed set, ABORTS and reports HUMAN (never
# hand-edits semantics, never drops a row). Post-resolution self-consistency is
# proven by the #1518 symmetry lint + exact ratchet.
#
# STATELESS: leaves the worktree on a DETACHED HEAD at the pushed tip (or back at
# origin/main), so it never depends on or clobbers a caller's checked-out branch.
# The scratch predecessors hardcoded HOME_BRANCH=_batch10_union; this does not.
#
# Usage: union-rebase.sh <hermit-worktree> <pr-branch> [--push]
# Prints one status line: RESULT <branch> CLEAN|UNIONED|HUMAN:<reason>|LINT-FAIL:*|REBASE-FAIL:*
set -uo pipefail

WT=${1:?hermit worktree path}
BR=${2:?pr branch (e.g. codex/e2e-foo)}
PUSH=${3:-}
# Resolve the parent-repo resolver relative to this script, not a hardcoded home.
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
PARENT_ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
RESOLVER="$PARENT_ROOT/scripts/e2e-union-resolve.py"
WIP="_uwip_$$"           # PID-unique wip branch: no fleet-wide collision
cd "$WT" || exit 9

MANAGED_PLAN="ci/expected-e2e-plan.json"
status() { echo "RESULT $BR $1"; }
# Return the worktree to a stateless detached HEAD and drop the wip branch.
detach_home() {
  git checkout -q --detach origin/main 2>/dev/null \
    || git checkout -q --detach HEAD 2>/dev/null || true
  git branch -D "$WIP" >/dev/null 2>&1 || true
}
abort() { git rebase --abort >/dev/null 2>&1; detach_home; }

# A path is a managed source registry (excludes the derived plan).
is_managed() {
  case "$1" in
    tests/e2e/manifests/inventory/test-files.json) return 0 ;;
    tests/backend-parity/matrix.tsv) return 0 ;;
    tests/e2e/manifests/*/*) return 1 ;;              # deeper than category dir
    tests/e2e/manifests/*.toml) return 0 ;;
    *) return 1 ;;
  esac
}

with-proxy git fetch -q origin main "$BR" 2>/dev/null || { status "REBASE-FAIL:fetch"; exit 0; }
FORK=$(git merge-base "origin/$BR" origin/main) || { status "REBASE-FAIL:no-merge-base"; exit 0; }
git branch -D "$WIP" >/dev/null 2>&1
git checkout -q -B "$WIP" "origin/$BR" || { status "REBASE-FAIL:checkout"; exit 0; }

# --- rebase; resolve conflicts just enough to continue -----------------------
# Managed conflicts: take main's side now (the authoritative pass rewrites them).
# Any UNMANAGED conflict is a genuine content collision -> HUMAN.
UNIONED=0
GIT_EDITOR=true git rebase origin/main >/dev/null 2>&1
rc=$?
while [ $rc -ne 0 ]; do
  mapfile -t U < <(git diff --name-only --diff-filter=U)
  if [ ${#U[@]} -eq 0 ]; then abort; status "REBASE-FAIL:stopped-no-conflict"; exit 0; fi
  for f in "${U[@]}"; do
    if [ "$f" = "$MANAGED_PLAN" ] || is_managed "$f"; then
      git checkout -q --ours -- "$f" 2>/dev/null || git checkout -q --theirs -- "$f" 2>/dev/null || git rm -q -- "$f"
      git add -- "$f" 2>/dev/null || true
    else
      abort; status "HUMAN:unmanaged-conflict:$f"; exit 0
    fi
  done
  GIT_EDITOR=true git rebase --continue >/dev/null 2>&1
  rc=$?
done

# --- authoritative pass: recompute every touched managed registry ------------
mapfile -t TOUCHED < <(git diff --name-only "$FORK" "origin/$BR")
for f in "${TOUCHED[@]}"; do
  is_managed "$f" || continue
  tmp=$(mktemp -d)
  git show "$FORK:$f"       >"$tmp/base"   2>/dev/null || : >"$tmp/base"
  git show "origin/main:$f" >"$tmp/ours"   2>/dev/null || : >"$tmp/ours"
  git show "origin/$BR:$f"  >"$tmp/theirs" 2>/dev/null || : >"$tmp/theirs"
  python3 "$RESOLVER" "$f" "$tmp/base" "$tmp/ours" "$tmp/theirs" "$tmp/out"
  prc=$?
  if [ $prc -ne 0 ]; then
    rm -rf "$tmp"; abort
    case $prc in
      3) status "HUMAN:non-additive:$f" ;;
      4) status "HUMAN:unmanaged:$f" ;;
      *) status "HUMAN:resolve-err:$f" ;;
    esac
    exit 0
  fi
  if ! cmp -s "$tmp/out" "$f"; then cp "$tmp/out" "$f"; git add -- "$f"; UNIONED=1; fi
  rm -rf "$tmp"
done
if ! git diff --cached --quiet; then GIT_EDITOR=true git commit -q --amend --no-edit; fi

# --- regenerate the derived plan from the merged manifests, amend if changed --
if ! ./ci/test_harness.sh plan --format json >/tmp/_uplan.$$ 2>/tmp/_uperr.$$; then
  abort; status "LINT-FAIL:plan-emit"; exit 0
fi
jq -S '{schema:1, cells:(.|sort_by(.category,.test,.mode,.backend))}' /tmp/_uplan.$$ >"$MANAGED_PLAN"
rm -f /tmp/_uplan.$$ /tmp/_uperr.$$
if ! git diff --quiet -- "$MANAGED_PLAN"; then
  git add -- "$MANAGED_PLAN"; GIT_EDITOR=true git commit -q --amend --no-edit; UNIONED=1
fi

# --- prove self-consistency: #1518 symmetry lint + exact ratchet -------------
if ! cargo run --quiet -p hermit-manifest-plan -- --format harness-json >/dev/null 2>&1; then
  abort; status "LINT-FAIL:symmetry"; exit 0
fi

if [ "$PUSH" = "--push" ]; then
  # REFUSE TO PUBLISH A HEAD THAT IS NOT AHEAD OF MAIN.
  #
  # The push below is `--force-with-lease HEAD:$BR`, and the lease only proves
  # nobody else moved $BR -- it says NOTHING about what we are about to write.
  # If HEAD is exactly origin/main, this force-writes main's tip over the PR
  # branch and DESTROYS the PR's commits. GitHub then sees head == base, shows
  # the PR with zero commits, and REFUSES to reopen it, so the work is not even
  # recoverable through the UI.
  #
  # Two ways HEAD legitimately arrives at origin/main by this line: the rebase
  # above dropped every commit as already-applied or empty, or an earlier error
  # path called detach_home(), which checks out --detach origin/main.
  #
  # This is not hypothetical and it is not new. rrnewton/hermit#1812 is the same
  # driver amending main's tip onto two PR branches, landing #1188/#1209 as
  # semantic no-ops. It recurred on 2026-08-25 against rrnewton/hermit#2448:
  # branch restore/stop-path-test-shard_20260824 was left pointing at
  # f5f77abd6d4b, a commit on main, the PR closed unmerged with its content NOT
  # on main, reopen was refused, and the reviewed head bcfd338361 survived only
  # as a loose object. That PR carried a fix for a red `make lint`.
  #
  # A no-op rebase is a legitimate outcome; silently publishing it is not. Say
  # so and let the caller decide -- land-pr.sh treats any non-CLEAN/UNIONED
  # RESULT as an abandon, which leaves the branch INTACT for a human.
  if [ "$(git rev-list --count origin/main..HEAD)" -eq 0 ]; then
    detach_home
    status "REBASE-FAIL:head-not-ahead-of-main"; exit 0
  fi
  if ! with-proxy git push -q --force-with-lease "origin" "HEAD:$BR"; then
    detach_home
    status "REBASE-FAIL:push"; exit 0
  fi
fi

detach_home
if [ "$UNIONED" -eq 1 ]; then
  status "UNIONED"
else
  status "CLEAN"
fi
