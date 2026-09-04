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
#                        [--result-json PATH]
# Writes the strict result contract when --result-json is supplied. The RESULT
# line is presentation for operators; land-pr consumes the JSON contract.
set -uo pipefail

WT=${1:?hermit worktree path}
BR=${2:?pr branch (e.g. codex/e2e-foo)}
shift 2
PUSH=0
RESULT_JSON=""
while [ $# -gt 0 ]; do
  case "$1" in
    --push) PUSH=1 ;;
    --result-json)
      [ $# -ge 2 ] || { echo "union-rebase: --result-json needs a path" >&2; exit 2; }
      RESULT_JSON=$2; shift ;;
    *) echo "union-rebase: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done
# Resolve the parent-repo resolver relative to this script, not a hardcoded home.
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
PARENT_ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
RESOLVER="$PARENT_ROOT/scripts/e2e-union-resolve.py"
RESULT_HELPER="$SCRIPT_DIR/union_rebase_result.py"
WIP="_uwip_$$"           # PID-unique wip branch: no fleet-wide collision
cd "$WT" || exit 9

record_mutation() {
  [ -n "${LANDER_MUTATION_FENCE:-}" ] || return 0
  source "$SCRIPT_DIR/worktree-mutation-fence.sh"
  lander_record_checkout_mutation \
    "${LANDER_MUTATION_CHECKOUT:-$WT}" \
    "${LANDER_MUTATION_ENTRY_STATE:-}" \
    "$LANDER_MUTATION_FENCE" "${LANDER_MUTATION_TOKEN:-}" "$@"
}

MANAGED_PLAN="ci/expected-e2e-plan.json"
BASE_SHA=""
INPUT_SHA=""
FORK=""
RESULT_SHA=""
status() {
  local outcome=$1 detail=${2:-} observed_exit=${3:-0} pushed=false display
  case "$outcome" in
    clean) display=CLEAN ;;
    unioned) display=UNIONED ;;
    human) display=HUMAN ;;
    lint-fail) display=LINT-FAIL ;;
    rebase-fail) display=REBASE-FAIL ;;
    *) echo "union-rebase: internal unknown outcome $outcome" >&2; return 9 ;;
  esac
  [ "$PUSH" -eq 1 ] && [ "$outcome" != human ] \
    && [ "$outcome" != lint-fail ] && [ "$outcome" != rebase-fail ] && pushed=true
  if [ -n "$RESULT_JSON" ]; then
    python3 "$RESULT_HELPER" write --path "$RESULT_JSON" --branch "$BR" \
      --outcome "$outcome" --detail "$detail" --base-sha "$BASE_SHA" \
      --input-sha "$INPUT_SHA" --fork-sha "$FORK" --result-sha "$RESULT_SHA" \
      --pushed "$pushed" --exit-code "$observed_exit" || return $?
  fi
  if [ -n "$detail" ]; then
    echo "RESULT $BR $display:$detail"
  else
    echo "RESULT $BR $display"
  fi
}
finish() {
  status "$@" || exit $?
  exit 0
}
# Return the worktree to a stateless detached HEAD and drop the wip branch.
detach_home() {
  git checkout -q --detach origin/main 2>/dev/null \
    || git checkout -q --detach HEAD 2>/dev/null || true
  record_mutation DETACHED || true
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

fetch_rc=0
with-proxy git fetch -q origin main "$BR" 2>/dev/null || fetch_rc=$?
[ "$fetch_rc" -eq 0 ] || finish rebase-fail fetch "$fetch_rc"
BASE_SHA=$(git rev-parse origin/main) || finish rebase-fail resolve-main "$?"
INPUT_SHA=$(git rev-parse "origin/$BR") || finish rebase-fail resolve-input "$?"
FORK=$(git merge-base "origin/$BR" origin/main) || finish rebase-fail no-merge-base "$?"
git branch -D "$WIP" >/dev/null 2>&1
checkout_rc=0
git checkout -q -B "$WIP" "origin/$BR" || checkout_rc=$?
[ "$checkout_rc" -eq 0 ] || finish rebase-fail checkout "$checkout_rc"
record_mutation "$WIP" || { abort; finish rebase-fail mutation-fence 2; }

# --- rebase; resolve conflicts just enough to continue -----------------------
# Managed conflicts: take main's side now (the authoritative pass rewrites them).
# Any UNMANAGED conflict is a genuine content collision -> HUMAN.
UNIONED=0
GIT_EDITOR=true git rebase origin/main >/dev/null 2>&1
rc=$?
record_mutation "$WIP" DETACHED || { abort; finish rebase-fail mutation-fence 2; }
while [ $rc -ne 0 ]; do
  mapfile -t U < <(git diff --name-only --diff-filter=U)
  if [ ${#U[@]} -eq 0 ]; then abort; finish rebase-fail stopped-no-conflict "$rc"; fi
  for f in "${U[@]}"; do
    if [ "$f" = "$MANAGED_PLAN" ] || is_managed "$f"; then
      git checkout -q --ours -- "$f" 2>/dev/null || git checkout -q --theirs -- "$f" 2>/dev/null || git rm -q -- "$f"
      git add -- "$f" 2>/dev/null || true
    else
      abort; finish human "unmanaged-conflict:$f" "$rc"
    fi
  done
  record_mutation "$WIP" DETACHED || { abort; finish rebase-fail mutation-fence 2; }
  GIT_EDITOR=true git rebase --continue >/dev/null 2>&1
  rc=$?
  record_mutation "$WIP" DETACHED || { abort; finish rebase-fail mutation-fence 2; }
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
      3) finish human "non-additive:$f" "$prc" ;;
      4) finish human "unmanaged:$f" "$prc" ;;
      *) finish human "resolve-err:$f" "$prc" ;;
    esac
  fi
  if ! cmp -s "$tmp/out" "$f"; then cp "$tmp/out" "$f"; git add -- "$f"; UNIONED=1; fi
  rm -rf "$tmp"
done
if ! git diff --cached --quiet; then
  GIT_EDITOR=true git commit -q --amend --no-edit
  record_mutation "$WIP" || { abort; finish rebase-fail mutation-fence 2; }
fi

# --- regenerate the derived plan from the merged manifests, amend if changed --
plan_rc=0
./ci/test_harness.sh plan --format json >/tmp/_uplan.$$ 2>/tmp/_uperr.$$ || plan_rc=$?
if [ "$plan_rc" -ne 0 ]; then
  abort; finish lint-fail plan-emit "$plan_rc"
fi
jq -S '{schema:1, cells:(.|sort_by(.category,.test,.mode,.backend))}' /tmp/_uplan.$$ >"$MANAGED_PLAN"
rm -f /tmp/_uplan.$$ /tmp/_uperr.$$
if ! git diff --quiet -- "$MANAGED_PLAN"; then
  git add -- "$MANAGED_PLAN"; GIT_EDITOR=true git commit -q --amend --no-edit; UNIONED=1
  record_mutation "$WIP" || { abort; finish rebase-fail mutation-fence 2; }
fi

# --- prove self-consistency: #1518 symmetry lint + exact ratchet -------------
symmetry_rc=0
cargo run --quiet -p hermit-manifest-plan -- --format harness-json >/dev/null 2>&1 || symmetry_rc=$?
if [ "$symmetry_rc" -ne 0 ]; then
  abort; finish lint-fail symmetry "$symmetry_rc"
fi

RESULT_SHA=$(git rev-parse HEAD) || finish rebase-fail resolve-result "$?"
if [ "$PUSH" -eq 1 ]; then
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
  # so and let the caller decide -- land-pr.sh requires a successful value from
  # the serialized result, which leaves the branch INTACT for a human.
  if [ "$(git rev-list --count origin/main..HEAD)" -eq 0 ]; then
    detach_home
    finish rebase-fail head-not-ahead-of-main 1
  fi
  push_rc=0
  with-proxy git push -q --force-with-lease "origin" "HEAD:$BR" || push_rc=$?
  if [ "$push_rc" -ne 0 ]; then
    detach_home
    finish rebase-fail push "$push_rc"
  fi
fi

detach_home
if [ "$UNIONED" -eq 1 ]; then
  finish unioned
else
  finish clean
fi
