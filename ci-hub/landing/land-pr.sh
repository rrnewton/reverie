#!/usr/bin/env bash
# Shared single-PR lander for the FIFO-serialized manifest / e2e / backend-parity
# bucket. This is the DISCOVERABLE home for the landing sequence that used to live
# only in scratch/. It carries the three race-tolerance fixes below AND is always
# self-wrapped in `ci-hub land-lock run --child-deadline`, so the landing lease is
# bound to THIS bounded child's lifetime -- no hand-rolled `renewer.sh` loop that
# can outlive a dead agent and wedge the FIFO (the 2040-minute starvation bug).
#
# Sequence (while holding the land-lock):
#   fetch fresh main -> exact-head local-ledger authority -> rebase
#   (union|plain, SKIPPED by --no-rebase) + push -> recheck the exact pushed head
#   -> derive the optional locally-validated cache only for a local receipt ->
#   advisory merge-gate observation -> gh pr merge --rebase (NEVER --admin)
#   -> ancestry-verify.
#
# Three fixes distilled from the 2026-08-03 stuck-gate diagnosis and superseded
# by the owner's validate-first decision on 2026-08-10:
#   1. The exact-head LEDGER result authorizes a normal Hermit landing. The
#      merge-gate is observed once as GH Actions diagnostics and never waited on.
#   2. The merge command is the mergeability arbiter, NOT mergeStateStatus (which
#      sticks at UNKNOWN); attempt `gh pr merge --rebase` in a bounded retry loop.
#   3. Labels and GH Actions state are diagnostics/caches, never substitutes for
#      the selected exact-head authority.
#
# Boxing principle applied to ourselves: EVERY wait here is bounded, and every
# terminal bail emits a visible ABANDON signal (stderr + a role-tagged PR comment)
# so an abandoned PR never silently languishes (the #244 pattern). On any bail the
# `land-lock run` wrapper releases the lock so the next FIFO waiter proceeds.
#
# Usage:
#   ci-hub/landing/land-pr.sh <PR> <BRANCH> [--repo OWNER/REPO]
#                             [--union|--no-rebase] [--agent NAME]
#                             [--gate-deadline SECS] [--child-deadline SECS]
#                             [--checkout REPO_SLOT] [--team TEAM] [--foreground]
#   --repo OWNER/REPO supported repository (rrnewton/hermit by default, or
#                    rrnewton/reverie). The checkout origin must match it.
#   --union          use the additive manifest union-rebase (union-rebase.sh);
#                    default is a plain `git rebase origin/main`.
#   --no-rebase      OPT-IN. Skip step 2 (the local rebase + force-push) and
#                    merge the already-authorized head as it stands. Mutually
#                    exclusive with --union. Rationale + how to re-verify it:
#                    see the step-2 comment. Default behaviour is UNCHANGED.
#   --agent NAME     reviewer + lock-holder identity (default: DG_AGENT_NAME).
#   --team TEAM      reviewer disclosure team (default: hermit2).
#   --checkout PATH  reviewer's slot for --repo. When omitted, use the current
#                    directory only if its origin matches --repo.
#   --gate-deadline  compatibility bound retained for callers and for sizing the
#                    default whole-child deadline; no GH Actions poll waits on it.
#   --child-deadline hard ceiling for the whole land subtree (default: twice the
#                    gate deadline); passed to `land-lock run`, which kills and
#                    releases on breach. It must exceed the compatibility bound.
#   --foreground     diagnostic escape hatch; default launches under nohup+setsid
#                    with a durable timestamped log and returns immediately.
set -uo pipefail

# The eligibility predicate binds landing to the validate ledger, but it honours
# one test override (CI_HUB_VALIDATE_STATUS_BIN substitutes the authority binary).
# It exists so the predicate can be bracketed inertly, but inherited into a real
# landing it makes the authority depend on the lander's environment. Measured:
# with CI_HUB_VALIDATE_STATUS_BIN pointed at a two-line `exit 0`
# script, EVERY unbacked/stale/tampered head returns ELIGIBILITY=VALIDATED.
# Clear them once, before anything reads them and before the detached re-exec,
# so the ledger on disk is the only authority a landing can consult. Nothing in
# this repo legitimately sets either for the lander.
unset CI_HUB_VALIDATE_STATUS_BIN

PR=""; BR=""; R=rrnewton/hermit; UNION=0; NO_REBASE=0; INNER=0; DETACHED_CHILD=0; FOREGROUND=0
AGENT="${DG_AGENT_NAME:-hermit-lander}"
TEAM="hermit2"
CHECKOUT=""
# Measured 2026-08-04 over 11 successful pull_request demo-hot-path runs created
# since 2026-08-03T23:00Z: median=586s, p90=646s, p95/p99/max=864s. The default
# is ceil(max * 1.25) = 1080s; the whole-child ceiling gets two gate windows.
GATE_DEADLINE=1080
CHILD_DEADLINE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { echo "land-pr: --repo requires OWNER/REPO" >&2; exit 2; }
      R=$2; shift ;;
    --union) UNION=1 ;;
    --no-rebase) NO_REBASE=1 ;;
    --agent) AGENT="$2"; shift ;;
    --team) TEAM="$2"; shift ;;
    --checkout) CHECKOUT="$2"; shift ;;
    --gate-deadline) GATE_DEADLINE="$2"; shift ;;
    --child-deadline) CHILD_DEADLINE="$2"; shift ;;
    --foreground) FOREGROUND=1 ;;
    --_detached) DETACHED_CHILD=1 ;;
    --_inner) INNER=1 ;;
    -h|--help) sed -n '1,50p' "$0"; exit 0 ;;
    -*) echo "land-pr: unknown flag $1" >&2; exit 2 ;;
    *) if [ -z "$PR" ]; then PR="$1"; elif [ -z "$BR" ]; then BR="$1"; else echo "land-pr: extra arg $1" >&2; exit 2; fi ;;
  esac
  shift
done
if [ -z "$PR" ] || [ -z "$BR" ]; then
  echo "usage: land-pr.sh <PR> <BRANCH> [--repo OWNER/REPO] [--union|--no-rebase] [--agent NAME] [--gate-deadline S] [--child-deadline S]" >&2
  exit 2
fi
case "$R" in
  rrnewton/hermit|rrnewton/reverie) ;;
  *)
    echo "land-pr: unsupported --repo '$R' (expected rrnewton/hermit or rrnewton/reverie)" >&2
    exit 2
    ;;
esac
# --union IS a rebase driver (union-rebase.sh rewrites and pushes the branch), so
# asking for both is incoherent rather than merely redundant. Refuse instead of
# silently picking one: a lander that thinks it skipped the rewrite while the
# union driver performed it is exactly the failure this flag exists to prevent.
if [ "$UNION" -eq 1 ] && [ "$NO_REBASE" -eq 1 ]; then
  echo "land-pr: --union and --no-rebase are mutually exclusive (--union rebases by definition)" >&2
  exit 2
fi
if [ "$UNION" -eq 1 ] && [ "$R" != rrnewton/hermit ]; then
  echo "land-pr: --union is defined only for rrnewton/hermit" >&2
  exit 2
fi
case "$GATE_DEADLINE" in ''|*[!0-9]*|0) echo "land-pr: gate deadline must be positive seconds" >&2; exit 2 ;; esac
if [ -z "$CHILD_DEADLINE" ]; then
  CHILD_DEADLINE=$((GATE_DEADLINE * 2))
fi
case "$CHILD_DEADLINE" in ''|*[!0-9]*|0) echo "land-pr: child deadline must be positive seconds" >&2; exit 2 ;; esac
if [ "$CHILD_DEADLINE" -le "$GATE_DEADLINE" ]; then
  echo "land-pr: child deadline must be greater than gate deadline" >&2
  exit 2
fi
if [ -n "${CI_HUB_DOCS_PARSE_ONLY:-}" ]; then
  printf 'DOCS PARSE OK: land-pr.sh repo=%s pr=%s branch=%s union=%s no_rebase=%s agent=%s team=%s checkout=%s\n' \
    "$R" "$PR" "$BR" "$UNION" "$NO_REBASE" "$AGENT" "$TEAM" "${CHECKOUT:-current-directory}"
  exit 0
fi

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
if [ -n "$CHECKOUT" ]; then
  WT=$(cd -- "$CHECKOUT" 2>/dev/null && pwd) || {
    echo "land-pr: --checkout is not a readable directory: $CHECKOUT" >&2
    exit 2
  }
else
  WT=$PWD
fi
normalize_github_remote(){
  local url=$1 path
  case "$url" in
    https://github.com/*) path=${url#https://github.com/} ;;
    git@github.com:*) path=${url#git@github.com:} ;;
    ssh://git@github.com/*) path=${url#ssh://git@github.com/} ;;
    *) return 1 ;;
  esac
  path=${path%.git}
  [[ $path =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || return 1
  printf '%s\n' "$path"
}

# ⚠️ LEAVE THE CALLER'S CHECKOUT AS WE FOUND IT.
#
# This script changes "$WT" during its rebase paths -- including cleanup after a
# conflicted rebase and parking after a successful push. Those paths previously
# left the worktree ON MAIN when the lander exited, including on ABANDON.
#
# THAT SILENTLY DESTROYS THE CALLER'S BRANCH, and by a route that does not look
# like this script's fault. The caller hands us a worktree sitting on their
# feature branch; a failed landing returns it on main; their next perfectly
# ordinary `git rebase origin/main && git push --force origin HEAD:<branch>`
# then rebases main onto main, yields a no-op head, and force-pushes that OVER
# THE BRANCH. Measured: three branch wipes in one session, each attributed to an
# external actor because the destructive command ran minutes later in a
# different tool. Recovery depended entirely on the agent having kept rescue
# refs.
#
# So record the entry state and restore it on EVERY exit. A branch name is
# restored as a branch; a detached HEAD is restored detached. Restoration is
# best-effort and never changes the exit code -- a landing verdict must not turn
# into a different one because a checkout could not be put back -- but it warns
# loudly rather than failing silently, because a silent failure here is exactly
# the defect being fixed.
ENTRY_REF=$(git -C "$WT" symbolic-ref -q --short HEAD 2>/dev/null || true)
if [ -z "$ENTRY_REF" ]; then
  ENTRY_REF=$(git -C "$WT" rev-parse -q --verify HEAD 2>/dev/null || true)
  ENTRY_DETACHED=1
else
  ENTRY_DETACHED=0
fi
restore_entry_checkout(){
  [ -n "$ENTRY_REF" ] || return 0
  local now
  now=$(git -C "$WT" symbolic-ref -q --short HEAD 2>/dev/null \
        || git -C "$WT" rev-parse -q --verify HEAD 2>/dev/null || true)
  [ "$now" = "$ENTRY_REF" ] && return 0
  if [ "$ENTRY_DETACHED" -eq 1 ]; then
    git -C "$WT" checkout -q --detach "$ENTRY_REF" 2>/dev/null
  else
    git -C "$WT" checkout -q "$ENTRY_REF" 2>/dev/null
  fi || echo "land-pr: WARNING: could not restore $WT to $ENTRY_REF; it is left at ${now:-unknown}. Check the branch before your next force-push." >&2
}
trap restore_entry_checkout EXIT
WT_MUTATED=0

remote_url=$(git -C "$WT" remote get-url origin 2>/dev/null || true)
checkout_repo=$(normalize_github_remote "$remote_url") || checkout_repo=
if [ "$checkout_repo" != "$R" ]; then
  echo "land-pr: checkout origin '$remote_url' does not match --repo $R" >&2
  exit 2
fi
say(){ echo "[land $R#$PR] $*"; }

# BIND THE PR TO ITS REPOSITORY BEFORE DEREFERENCING ANY AUTHORITY.
# A PR number is not an identity. The validated --repo, matching checkout
# origin, and named head branch are one tuple; refuse before reading authority
# unless the live PR resolves to that exact repository/branch pair.
verify_pr_belongs_to_repo(){
  local meta actual state
  # ⚠️ STATE IS PART OF THE IDENTITY, AND OMITTING IT COST A LANDING LEASE.
  # Measured 2026-08-26: this lander ran against hermit#2647 two minutes after it
  # merged, took the box-global FIFO lock for 51.4s, and posted an "abandoned"
  # landing comment on a pull request that had landed successfully. It never
  # noticed, because nothing here read `state`. The head-branch check below
  # cannot catch it: GitHub deletes the branch on merge (404 on the ref) but
  # `gh pr view --json headRefName` still returns the RECORDED name, so the
  # branch comparison passes on a merged pull request.
  #
  # Read in the SAME call as headRefName -- one request, not two, because a
  # per-head extra read is what trips the shared secondary rate limit. `state`
  # is a fixed enum (OPEN/CLOSED/MERGED) with no spaces, so putting it first
  # makes the split unambiguous for any branch name.
  meta=$(with-proxy gh pr view "$PR" -R "$R" --json headRefName,state \
    -q '.state + " " + .headRefName' 2>/dev/null) || {
    echo "land-pr: REFUSE: PR #$PR does not resolve in $R" >&2
    exit 2
  }
  state=${meta%% *}
  actual=${meta#* }
  # ⚠️ FAIL CLOSED. An unreadable state is not an open one. This is the landing
  # path: "I could not tell" must stop, because the alternative is acting on a
  # pull request whose disposition we do not know.
  if [ -z "$state" ]; then
    echo "land-pr: REFUSE: $R#$PR state is unreadable; refusing rather than assuming OPEN" >&2
    exit 2
  fi
  if [ "$state" != OPEN ]; then
    echo "land-pr: REFUSE: $R#$PR is $state, not OPEN. A landing acts only on an open" >&2
    echo "land-pr:   pull request; there is nothing here to land." >&2
    exit 2
  fi
  if [ -z "$actual" ]; then
    echo "land-pr: REFUSE: PR #$PR has no head branch in $R" >&2
    exit 2
  fi
  if [ "$actual" != "$BR" ]; then
    echo "land-pr: REFUSE: $R#$PR has head branch '$actual', but '$BR' was named." >&2
    echo "land-pr:   A same-number PR or same-name branch in another repository" >&2
    echo "land-pr:   is a different object and cannot supply this landing's evidence." >&2
    exit 2
  fi
}
publish_landing_comment(){
  local kind="$1" reason="$2"
  python3 "$SCRIPT_DIR/publish_comment.py" \
    --repo "$R" --pr "$PR" --agent "$AGENT" --team "$TEAM" \
    --kind "$kind" --reason "$reason" \
    --record-dir "${CI_HUB_LANDING_LOG_DIR:-$ROOT/ignored/ci-hub/landing}"
}
comment_abandon(){
  publish_landing_comment abandoned "$1"
}
comment_no_result(){
  publish_landing_comment no-result "$1"
}

# A real land exceeds the agent shell's foreground time budget. Launch the
# entire self-healing lock/land/arm sequence in a new session by default; the
# timestamped log is the durable observation surface across agent recycling.
if [ "$INNER" -eq 0 ] && [ "$DETACHED_CHILD" -eq 0 ] && [ "$FOREGROUND" -eq 0 ]; then
  # ⚠️ REFUSE BEFORE FORKING, NOT ONLY INSIDE THE LOCK. The inner call at the
  # bottom of this script is the authority and stays -- state can change between
  # this check and the lock, which is exactly the race that happened (#2647
  # merged at 23:56:00Z, the lander started 23:58:08Z). But checking only there
  # means the caller sees "DETACHED LAND: pid=..." and has to poll a log to find
  # out it refused, after a lease was already taken and a comment posted.
  verify_pr_belongs_to_repo
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  safe_pr=${PR//[^A-Za-z0-9_.-]/_}
  log_dir="${CI_HUB_LANDING_LOG_DIR:-$ROOT/ignored/ci-hub/landing}"
  log="$log_dir/land-pr${safe_pr}-${stamp}-$$.log"
  mkdir -p "$log_dir"
  detached_args=(--_detached "$PR" "$BR" --repo "$R" --agent "$AGENT" --team "$TEAM" --checkout "$WT" \
    --gate-deadline "$GATE_DEADLINE" --child-deadline "$CHILD_DEADLINE")
  [ "$UNION" -eq 1 ] && detached_args+=(--union)
  [ "$NO_REBASE" -eq 1 ] && detached_args+=(--no-rebase)
  printf 'DETACHED LAND START repo=%s pr=%s branch=%s agent=%s started_at=%s\n' \
    "$R" "$PR" "$BR" "$AGENT" "$stamp" >"$log"
  nohup setsid "$0" "${detached_args[@]}" </dev/null >>"$log" 2>&1 &
  detached_pid=$!
  printf 'DETACHED LAND PID pid=%s\n' "$detached_pid" >>"$log"
  printf 'DETACHED LAND: pid=%s log=%s\n' "$detached_pid" "$log"
  exit 0
fi

# --- outer: self-wrap in land-lock run so the lease is bound to this bounded ---
# child. `run` acquires (FIFO), heartbeats only while we live, and ALWAYS releases
# on exit; --child-deadline hard-kills + releases if we ever wedge.
if [ "$INNER" -eq 0 ]; then
  args=(--_inner "$PR" "$BR" --repo "$R" --agent "$AGENT" --team "$TEAM" --checkout "$WT" --gate-deadline "$GATE_DEADLINE")
  [ "$UNION" -eq 1 ] && args+=(--union)
  [ "$NO_REBASE" -eq 1 ] && args+=(--no-rebase)
  "$ROOT/ci-hub/ci-hub" land-lock run --agent "$AGENT" --pr "$PR" \
    --child-deadline "$CHILD_DEADLINE" -- "$0" "${args[@]}"
  rc=$?
  # The hard deadline kills the entire inner process group, so only this outer
  # supervisor remains able to emit the durable PR-side abandonment signal.
  case "$rc" in
    1) comment_abandon "timed out waiting for the landing lock" ;;
    124) comment_abandon "land subtree exceeded the ${CHILD_DEADLINE}s hard deadline and was killed" ;;
  esac
  [ "$rc" -eq 0 ] || exit "$rc"

  # The merge and ancestry proof are already complete. MC is read because the
  # LANDED line below reports it.
  MC=$(with-proxy gh pr view "$PR" -R "$R" --json mergeCommit -q .mergeCommit.oid 2>/dev/null || true)

  # Detached and single-flight: this audit must never delay or veto a land.
  if [ "${CI_HUB_POST_LAND_AUDIT:-1}" = 1 ]; then
    (
      exec 9>"$ROOT/ignored/.ancestry-audit.lock" 2>/dev/null || exit 0
      flock -n 9 || exit 0
      setsid "$ROOT/ci-hub/bin/ancestry-audit" --herdr-agent "${AGENT:-hermit-lander}" \
        --json "$ROOT/ignored/ancestry/post-land-audit.json" \
        >"$ROOT/ignored/ancestry/post-land-audit.log" 2>&1
    ) >/dev/null 2>&1 &
  fi
  say "LANDED:${MC:-merge-commit-unavailable}"; exit 0
fi

# --- inner: we now hold the land-lock ----------------------------------------
# Visible terminal ABANDON signal: stderr + role-tagged PR comment, then exit.
# NEVER LEAVE THE WORKTREE PARKED AT origin/main ON THE WAY OUT.
#
# This turns a failed landing into a DESTROYED BRANCH on the next ordinary
# command, silently and at a distance. The route, measured on
# rrnewton/hermit#2448 which was wiped three times before anyone saw why:
#
#   1. a landing path that already changed the checkout fails while HEAD is
#      detached at origin/main and `abandon` exits without touching it again;
#   2. the shared worktree now LOOKS like a checkout of the branch and is
#      actually main;
#   3. the next `push --force-with-lease HEAD:$BR` -- from a later lander run,
#      another agent, or a human in that directory -- writes main's tip over
#      the pull request branch. `--force-with-lease` does not help: the lease
#      only proves nobody else moved $BR, never that what we are writing is
#      the branch's own work.
#   4. GitHub then sees head == base, reports zero commits, and REFUSES to
#      reopen, so the pull request cannot even be recovered through the UI.
#
# Parking at the pull request's OWN head instead makes step 3 idempotent: the
# stray push writes $ORIG to $BR, which is where $BR already points. Same
# accident, no data loss.
#
# Guarded three ways because `abandon` is reachable before $ORIG exists: only
# acts when WT and ORIG are both set and ORIG still resolves. Failure to park
# is reported, never fatal -- an abandon must still abandon.
park_worktree_off_main(){
  [ "${WT_MUTATED:-0}" -eq 1 ] || return 0
  [ -n "${WT:-}" ] && [ -n "${ORIG:-}" ] || return 0
  git -C "$WT" rev-parse --verify -q "${ORIG}^{commit}" >/dev/null 2>&1 || return 0
  git -C "$WT" checkout -q --detach "$ORIG" 2>/dev/null \
    || say "WARNING: could not park $WT at $ORIG; it may still be detached at main -- do not push HEAD:<branch> from it"
}

abandon(){
  local reason="$1" code="${2:-1}"
  say "ABANDON: $reason"
  park_worktree_off_main
  comment_abandon "$reason" \
    || say "ABANDON COMMENT PUBLICATION FAILED: use the durable record printed above"
  exit "$code"
}

# 1. fresh main
with-proxy git -C "$WT" fetch -q origin main || abandon "fetch origin/main failed" 2
with-proxy git -C "$WT" fetch -q origin "$BR" 2>/dev/null || true

# 1b. Validation landability is defined only by the tracked
# repository-specific validation-authority skill. This entrypoint calls its
# consumer; it does not restate hard/soft-green policy. Re-check after every
# SHA-changing rebase.
verify_pr_belongs_to_repo
ORIG=$(git -C "$WT" rev-parse "origin/$BR" 2>/dev/null) || abandon "cannot resolve origin/$BR head for eligibility gate" 4
authority_mode=local-only
validation_landability() {
  local candidate=$1 current_main=$2
  shift 2
  python3 "$SCRIPT_DIR/rebase_wrapper.py" authorize \
    --repo "$R" --result "$candidate" --current-base "$current_main" \
    --authority-mode "$authority_mode" --repo-checkout "$WT" "$@"
}
eligibility_base=$(git -C "$WT" rev-parse --verify origin/main) \
  || abandon "cannot resolve current main for validation landability" 4
VS=$(validation_landability "$ORIG" "$eligibility_base" --json 2>&1); VRC=$?
say "validation landability(head=$ORIG) rc=$VRC: $VS"
# ⚠️ "NO REBASE RECORD" AT THE *PRE*-CHECK IS A MISSING PRECONDITION, NOT A REFUSAL.
# The record is created by step 2's wrapper-managed rebase, which has not run yet.
# A head rebased by hand therefore fails this check for a condition the lander is
# about to satisfy itself -- measured on
# https://github.com/rrnewton/hermit/pull/2512, which abandoned rc=2 with
# "no rebase record for result fd994268a31e" before the merge step existed.
#
# THIS DOES NOT WEAKEN THE GATE, and the reason is structural rather than a
# judgement call: the post-push re-authorize below is UNCONDITIONAL and abandons on
# any non-zero, so every fact this pre-check would have established is established
# there against the head actually being merged -- which is the better subject
# anyway, since step 2 rewrites the sha.
#
# ⚠️ BUT ONLY WHEN STEP 2 WILL RUN. Under --no-rebase step 2 is skipped, so this
# pre-check IS the only gate and must keep refusing. Relaxing it there would be the
# gate ceasing to refuse because it ceased to look.
if [ "$VRC" -ne 0 ] && [ "$NO_REBASE" -eq 0 ] \
   && grep -q "no rebase record for result" <<<"$VS"; then
  say "pre-check: no rebase record yet for $ORIG -- step 2 creates it; deferring to the post-push re-authorize"
  VRC=0
fi
# JUST-IN-TIME PR-HEAD CI. A prepared, reviewed head whose exact commit has no
# accepted verdict gets one run through the same centralized producer as every
# other qualifying receipt. Do not validate a stale base: the producer would
# correctly refuse it. The helper re-reads the live PR head after VALIDATE, so a
# concurrent push expires this result instead of inheriting it.
if [ "$VRC" -ne 0 ]; then
  existing_status=$("$ROOT/ci-hub/ci-hub" validate-status --repo "$R" --sha "$ORIG" --json 2>&1)
  existing_rc=$?
  if [ "$existing_rc" -eq 4 ] && \
     git -C "$WT" merge-base --is-ancestor "$eligibility_base" "$ORIG"; then
    jit_detail=$("$ROOT/ci-hub/bin/pr-head-ci" \
      --repo "$R" --pr "$PR" --expected-head "$ORIG" \
      --checkout "$WT" --agent "$AGENT" --execute 2>&1)
    jit_rc=$?
    say "just-in-time exact-head CI(head=$ORIG) rc=$jit_rc: $jit_detail"
    [ "$jit_rc" -eq 0 ] \
      || abandon "just-in-time exact-head CI produced no qualifying verdict for $ORIG (rc=$jit_rc)" 4
    with-proxy git -C "$WT" fetch -q origin main \
      || abandon "could not refresh current main after just-in-time validation" 4
    eligibility_base=$(git -C "$WT" rev-parse --verify origin/main) \
      || abandon "cannot resolve current main after just-in-time validation" 4
    VS=$(validation_landability "$ORIG" "$eligibility_base" --json 2>&1); VRC=$?
    say "post-JIT validation landability(head=$ORIG) rc=$VRC: $VS"
  elif [ "$existing_rc" -eq 4 ]; then
    # ⚠️ SAY WHICH OF THE TWO CONDITIONS ACTUALLY HELD. This branch is reached when
    # `merge-base --is-ancestor` did not succeed, and that has TWO causes: the head
    # genuinely does not contain current main, or the ancestry check itself could
    # not run (a sha not present locally makes git exit non-zero, which `&&` cannot
    # distinguish from "not an ancestor").
    #
    # Measured on https://github.com/rrnewton/hermit/pull/2512: the operator was
    # told "PR head lacks current origin/main; rebase before VALIDATE" on a run
    # whose actual failure was a 120-second TimeoutExpired in the authority script.
    # Advising a rebase for a timeout sends the next person to fix the wrong thing,
    # which is the diagnostic class this project has now hit repeatedly.
    if git -C "$WT" rev-parse --verify --quiet "$ORIG^{commit}" >/dev/null \
       && git -C "$WT" rev-parse --verify --quiet "$eligibility_base^{commit}" >/dev/null; then
      say "just-in-time exact-head CI not admitted: PR head does not contain current origin/main (base=$eligibility_base); rebase before VALIDATE"
    else
      say "just-in-time exact-head CI not admitted: could not compare PR head $ORIG against current origin/main $eligibility_base -- one of them is not present locally. This is NOT a statement that the head needs rebasing."
    fi
  else
    say "just-in-time exact-head CI not admitted: canonical exact-head status rc=$existing_rc: $existing_status"
  fi
fi
[ "$VRC" -eq 0 ] \
  || abandon "validation landability refused PR head $ORIG (rc=$VRC)" 4

# 2. rebase onto latest main + push
#
# --no-rebase (OPT-IN; the default path below is byte-for-byte unchanged) skips
# this whole step and merges the head that step 1b already authorized.
#
# WHY that is safe -- re-verify this rather than trusting the comment:
#   with-proxy gh api repos/rrnewton/hermit/rulesets --jq '.[]|"\(.id)\t\(.name)"'
#   with-proxy gh api repos/rrnewton/hermit/rulesets/<id-of-"main check gating"> \
#     --jq '.rules[]|select(.type=="required_status_checks")
#           |.parameters.strict_required_status_checks_policy'
# Observed 2026-08-07: `false`. A false strict policy means main does NOT require
# a PR branch to be up to date, so being behind main is not a merge blocker; and
# `gh pr merge --rebase` in step 6 replays the PR commits onto the current tip
# server-side regardless. The local rebase therefore produces nothing the merge
# needs.
#
# What it DOES produce is a rewritten head. Step 2 force-pushes, step 4 then
# re-derives the exact-head authority at the NEW sha, and every exact-head green
# earned at the old sha is orphaned -- a rebase can only ever downgrade an
# already-authorized head to NO_RESULT. Measured 2026-08-07 on #1705/#1711/#1678:
# all three held a qualifying historical `AUTHORITY=hosted` result (~30 min of
# GH Actions each) that an unconditional rebase would have voided the moment another team
# advanced main. See also rrnewton/hermit#1812, where an unconditional
# rebase-and-force-push in the union driver amended main's tip onto two PR
# branches and landed #1188/#1209 as semantic no-ops.
#
# This flag removes a MUTATION, never a CHECK. Still executed on this path: the
# land-lock (outer `land-lock run`), the step-1b exact-head authority, the step-4
# recheck at the head actually being merged, the merge-gate diagnostic observation, the step-5b
# final-boundary authority + receipt dereference, `--match-head-commit`, and the
# post-merge mergeCommit.oid ancestry proof.
plain_rebase=0
rebase_base=$(git -C "$WT" rev-parse --verify origin/main) \
  || abandon "cannot resolve current main before rebase" 3
if [ "$NO_REBASE" -eq 1 ]; then
  say "no-rebase: skipping the local rebase + force-push; merging the authorized head as it stands"
elif [ "$UNION" -eq 1 ]; then
  WT_MUTATED=1
  ulog="/tmp/land-$PR-union.log"
  uresult=$(mktemp)
  union_rc=0
  "$SCRIPT_DIR/union-rebase.sh" "$WT" "$BR" --push \
    --result-json "$uresult" >"$ulog" 2>&1 || union_rc=$?
  if [ "$union_rc" -ne 0 ]; then
    rm -f -- "$uresult"
    abandon "union-rebase could not write its result contract (rc=$union_rc; see $ulog)" 3
  fi
  result_rc=0
  RES=$(python3 "$SCRIPT_DIR/union_rebase_result.py" require-success \
    --path "$uresult" --expect-branch "$BR" 2>&1) || result_rc=$?
  rm -f -- "$uresult"
  say "union: ${RES:-<no structured result> (see $ulog)}"
  [ "$result_rc" -eq 0 ] \
    || abandon "union-rebase did not converge: ${RES:-see $ulog}" 3
else
  plain_rebase=1
  WT_MUTATED=1
  git -C "$WT" checkout -q -B "_land_$PR" "origin/$BR" || abandon "checkout origin/$BR failed" 3
  if ! GIT_EDITOR=true git -C "$WT" rebase origin/main >/dev/null 2>&1; then
    git -C "$WT" rebase --abort >/dev/null 2>&1
    git -C "$WT" checkout -q --detach origin/main 2>/dev/null || true
    abandon "plain rebase onto origin/main conflicted (needs owner/--union)" 3
  fi
  # REFUSE TO PUBLISH A HEAD THAT IS NOT AHEAD OF MAIN. See the identical guard
  # in ci-hub/landing/union-rebase.sh. Detach sites are many and this push is
  # one of only three places in the tree that writes HEAD to a pull-request
  # branch, so the check belongs here rather than at every checkout.
  if [ "$(git -C "$WT" rev-list --count origin/main..HEAD)" -eq 0 ]; then
    abandon "refusing to push a head that is not ahead of main; the worktree is at main and pushing would wipe $BR" 3
  fi
  # ⚠️ RE-READ STATE IMMEDIATELY BEFORE WRITING TO THE BRANCH, not only at the top
  # of the inner. `verify_pr_belongs_to_repo` ran there, but the landability
  # evaluation and the optional just-in-time CI wait sit in between and take tens of
  # seconds -- and a pull request can merge inside that window. That is the actual
  # shape of this defect: hermit#2647 merged at 23:56:00Z and the lander started at
  # 23:58:08Z. On a merged pull request GitHub has DELETED the head branch, so this
  # push does not update a branch, it RECREATES one: a live-looking branch at a
  # rewritten head, for a pull request that can never be landed. One read, taken only
  # on the path that actually writes. Unreadable is treated as not-OPEN.
  live_state=$(with-proxy gh pr view "$PR" -R "$R" --json state -q .state 2>/dev/null || true)
  if [ "$live_state" != OPEN ]; then
    abandon "refusing to force-push $BR: $R#$PR is ${live_state:-UNREADABLE}, not OPEN -- the branch would be recreated for a pull request that cannot be landed" 3
  fi
  with-proxy git -C "$WT" push -q --force-with-lease origin "HEAD:$BR" || abandon "force-with-lease push failed" 3
  # Park at what we just published, NOT at main. Leaving a shared worktree on
  # main is what converts the next ordinary force-push into a branch wipe; HEAD
  # already equals origin/$BR here, so a stray repeat is idempotent.
  git -C "$WT" checkout -q --detach "$(git -C "$WT" rev-parse HEAD)" 2>/dev/null || true
fi

# 3. record the head that will actually be merged. Re-fetched from the remote in
# every mode, so this also catches a concurrent push by someone else -- under
# --no-rebase nothing was pushed by us, but the remote is still the source of
# truth and step 4 re-authorizes whatever it finds.
with-proxy git -C "$WT" fetch -q origin "$BR"
HEAD=$(git -C "$WT" rev-parse "origin/$BR")
if [ "$NO_REBASE" -eq 1 ]; then
  say "unrebased head=$HEAD (expected to equal the authorized head $ORIG)"
else
  say "pushed head=$HEAD"
fi
if [ "$plain_rebase" -eq 1 ] && [ "$HEAD" != "$ORIG" ] && [ "$R" = rrnewton/hermit ]; then
  record_rc=0
  record_detail=$(python3 "$SCRIPT_DIR/rebase_wrapper.py" record \
    --source "$ORIG" --base "$rebase_base" --result "$HEAD" \
    --conflicts none --repo-checkout "$WT" --no-fetch \
    --no-receipt-check 2>&1) || record_rc=$?
  record_rc=${record_rc:-0}
  say "rebase provenance(head=$HEAD) rc=$record_rc: $record_detail"
  # A hard-green result does not need this cache.  A soft-green result will
  # fail closed in the authority consumer if recording did not succeed.
elif [ "$plain_rebase" -eq 1 ]; then
  if [ "$HEAD" = "$ORIG" ]; then
    say "rebase provenance unchanged: rebase did not rewrite $ORIG"
  else
    say "rebase rewrote $ORIG to $HEAD; $R requires fresh exact-head validation"
  fi
fi

# 4. Re-evaluate validation landability at the pushed head through the same
# consumer. Only a hard-green result may materialize the optional exact-head
# cache label; soft-green remains explicitly distinct.
#
# 4a. Re-mint one exact count-backed schema-5 row from its durable log BEFORE
# reading the ledger. Hermit's Rust driver can write a count-less receipt
# when it can't reach the parent count helper; with the uncounted-receipt
# grandfather removed, such a genuine green would be NotValidated. Select one
# exact HEAD row by its Rust-canonical digest, then pass both identities into the
# append-safe finalizer so a real green is not stranded by a producer that
# failed to inline its counts. Best-effort: never aborts landing — eligibility
# below remains the authoritative fail-closed gate.
if [ "$R" = rrnewton/hermit ]; then
  selected_row_sha256=$("$ROOT/ci-hub/validate/scan-finalize.sh" \
    --select-candidate-sha256 --sha "$HEAD" --hermit-checkout "$WT" \
    2>/dev/null) || selected_row_sha256=
  if [[ $selected_row_sha256 =~ ^[0-9a-f]{64}$ ]]; then
    "$ROOT/ci-hub/validate/scan-finalize.sh" --hermit-checkout "$WT" \
      --sha "$HEAD" --selected-row-sha256 "$selected_row_sha256" || true
  else
    say "scan-finalize: no unique exact source row selected for $HEAD (best-effort)"
  fi
fi
python3 "$ROOT/ci-hub/validate/attribute_reds.py" --last 0 --persist >/dev/null 2>&1 || true
with-proxy git -C "$WT" fetch -q origin main \
  || abandon "could not refresh current main for post-push landability" 4
post_push_base=$(git -C "$WT" rev-parse --verify origin/main) \
  || abandon "could not resolve current main for post-push landability" 4
VS=$(validation_landability "$HEAD" "$post_push_base" --json 2>&1); VRC=$?
say "post-push validation landability(head=$HEAD) rc=$VRC: $VS"
[ "$VRC" -eq 0 ] \
  || abandon "pushed head $HEAD has no accepted validation landability (rc=$VRC)" 4
landability_state=$(jq -er .state <<<"$VS") \
  || abandon "validation landability emitted no state for $HEAD" 4
if [ "$landability_state" = HARD_GREEN ]; then
  # The exact-head ledger result above is authority. Label reconciliation is a
  # scanning convenience and cannot veto the approving reviewer's land.
  ALL_OUT=$("$ROOT/ci-hub/ci-hub" apply-local-label --pr "$PR" --repo "$R" 2>&1); ALRC=$?
  [ -n "$ALL_OUT" ] && say "apply-local-label: $ALL_OUT"
  [ "$ALRC" -eq 0 ] \
    || say "WARN: locally-validated cache reconciliation failed; exact-head authority remains valid"
elif [ "$landability_state" = SOFT_GREEN ]; then
  say "soft-green landability accepted; no exact-head cache label is minted at $HEAD"
fi

# 4b. A draft PR cannot be merged. Marking it ready may fire advisory GH Actions, but
# that hosted result is not landing authority.
if [ "$(with-proxy gh pr view "$PR" -R "$R" --json isDraft -q .isDraft)" = "true" ]; then
  with-proxy gh pr ready "$PR" -R "$R" >/dev/null && say "marked ready (was draft)"
  sleep 4
fi

# 5. Observe merge-gate once as a diagnostic. The owner deliberately removed
# required hosted checks: this observation must not authorize, veto, dispatch,
# or delay a ledger-authorized landing.
selector="$SCRIPT_DIR/merge-gate-status.jq"
outcome_helper="$ROOT/ci-hub/check_outcome.py"
if actions_json=$(with-proxy gh api \
    "repos/$R/actions/workflows/merge-gate.yml/runs?head_sha=$HEAD&per_page=100" \
    2>/dev/null); then
  gate_run=$(python3 "$outcome_helper" \
      --select-latest-run --head-sha "$HEAD" \
      --event pull_request --event workflow_dispatch <<<"$actions_json")
  row=$(jq -r -f "$selector" <<<"$gate_run")
  IFS=$'\t' read -r gate_status gate_conclusion gate_event gate_run gate_url gate_created <<<"$row"
  check_outcome=$(python3 "$outcome_helper" --status "$gate_status" --conclusion "$gate_conclusion")
  say "merge-gate diagnostic only: ${gate_status}/${gate_conclusion} outcome=$check_outcome event=$gate_event run=$gate_run created=$gate_created url=$gate_url"
else
  say "merge-gate diagnostic only: unavailable (landing authority remains the local ledger)"
fi

# 5b. Re-evaluate the local-ledger policy at the final mutation boundary. The
# positive now additionally dereferences its immutable receipt comment.
# Mutable-tip currency is asserted HERE, exactly once: refresh both authorities
# immediately before dereferencing the receipt. A stale receipt costs this gate
# rerun, never another full validation. No ancestry-distance budget exists.
with-proxy git -C "$WT" fetch -q origin main \
  || abandon "could not refresh $R main at the final receipt boundary" 5
current_base=$(git -C "$WT" rev-parse --verify origin/main) \
  || abandon "could not resolve $R main at the final receipt boundary" 5
current_reverie_base=
reverie_checkout=
if [ "$R" = rrnewton/hermit ]; then
  reverie_checkout="$ROOT/reverie"
  with-proxy git -C "$reverie_checkout" fetch -q origin main \
    || abandon "could not refresh Reverie main at the final receipt boundary" 5
  current_reverie_base=$(git -C "$reverie_checkout" rev-parse --verify origin/main) \
    || abandon "could not resolve Reverie main at the final receipt boundary" 5
fi
live_head=$(with-proxy gh pr view "$PR" -R "$R" --json headRefOid -q .headRefOid 2>/dev/null) \
  || abandon "could not resolve the live PR head before receipt authorization" 5
[ "$live_head" = "$HEAD" ] \
  || abandon "PR head moved before receipt authorization (expected $HEAD, observed ${live_head:-missing})" 5

# Review labels are caches, never authority. Re-read their APPROVED-AT sources
# at the final mutation boundary. An exact current-head binding passes directly.
# A binding at an older head passes only when git range-diff proves that every
# reviewed patch was replayed unchanged onto current main. Missing,
# undetermined, conflicted, or semantically changed review evidence refuses;
# no label or comment is planted or deleted here.
approval_json=$(mktemp) \
  || abandon "could not allocate the approval-binding observation file" 5
if ! python3 "$ROOT/ci-hub/health/approval_binding.py" \
    --repo "$R" --pr "$PR" --json >"$approval_json"; then
  rm -f -- "$approval_json"
  abandon "could not read approval-binding evidence for $HEAD" 5
fi
review_detail=$(python3 "$SCRIPT_DIR/review_rebase.py" \
  --approval-json "$approval_json" --repo-checkout "$WT" \
  --current-base "$current_base" --current-head "$HEAD" 2>&1)
review_rc=$?
rm -f -- "$approval_json"
say "review applicability(head=$HEAD) rc=$review_rc: $review_detail"
[ "$review_rc" -eq 0 ] \
  || abandon "review applicability REFUSED for $HEAD: ${review_detail:-no result}" 5

if [ "$R" = rrnewton/hermit ]; then
  receipt_comments=$(mktemp) \
    || abandon "could not allocate the receipt-comment observation file" 5
  if ! with-proxy gh api --paginate --slurp \
      "repos/$R/issues/$PR/comments?per_page=100" >"$receipt_comments"; then
    printf '[]\n' >"$receipt_comments"
  fi
  receipt_detail=$(validation_landability "$HEAD" "$current_base" \
    --comments "$receipt_comments" \
    --current-reverie-base "$current_reverie_base" \
    --reverie-checkout "$reverie_checkout" 2>&1)
  receipt_rc=$?
  rm -f -- "$receipt_comments"
else
  # Reverie's canonical exact-head local row is the receipt authority. Its
  # centralized producer has no Hermit-style published receipt-comment layer.
  receipt_detail=$(validation_landability "$HEAD" "$current_base" 2>&1)
  receipt_rc=$?
fi
if [ "$receipt_rc" -ne 0 ]; then
  abandon "validation landability REFUSED for $HEAD: ${receipt_detail:-no result}" 5
fi
say "validation landability authorized: $receipt_detail"

# 5d. ⚠️ A REVIEW REFUSAL BOUND TO THIS EXACT HEAD BLOCKS THE MERGE.
#
# Until 2026-08-25 nothing on any merge path read a refusal marker: `grep -rn
# CHANGES-REQUESTED-AT` over every .py/.sh/.rs hit only ci-hub/health/
# approval_binding.py and its own tests, and that module only REPORTS. hermit#2591
# merged 3m42s after a plain, parseable, binding refusal was posted, and its
# approval label was applied three minutes AFTER the merge -- neither the marker
# path nor the label path was consulted, because neither was wired to anything.
#
# This runs LAST, immediately before the mutation, for the same reason the receipt
# is dereferenced here: a refusal posted while the earlier steps ran must still
# stop the merge. Checking it earlier would reintroduce the read-then-act window.
#
# rc 2 is INDETERMINATE and blocks exactly like rc 1. A marker whose binding
# cannot be read is not a marker that does not apply -- that is the fail-open a
# fourth marker variant already walked through tonight.
#
# ⚠️ AUTHORISATION IS A VERDICT LINE **AND** A ZERO STATUS, NEVER THE STATUS ALONE.
# A status is what a gate that RAN reports. A gate that is not on disk reports
# nothing and cannot choose a status, so keying on `rc` alone cannot express the
# most common failure there is: measured 2026-08-26, 49 of 76 dev-hermit checkouts
# on this box -- including the shared primary, 334 commits behind -- have no
# `ci-hub/bin/changes-requested-gate` at all. Here that surfaces as python3's
# `can't open file` and rc 2, which does abandon; but one `| tail` anywhere in the
# chain turns it into rc 0, and rc 0 is permission to merge.
#
# So absence is detected by the ABSENCE OF A VERDICT, which no missing file can
# fake and no pipe can strip out of the captured text.
#
# ⚠ THE PATTERN IS DUPLICATED FROM `no_verdict_in()` IN THE GATE, DELIBERATELY, AND
# THIS IS THE ONE PLACE IT MUST BE. Importing the definition would mean loading the
# very file whose absence is being detected -- the check would be unavailable in
# exactly the case it exists for. It is spelled the same in both homes and the
# gate's self-test owns the meaning; if they ever disagree, this one is the copy
# that still runs.
#
# ⚠ AND THE LABEL IS PART OF THE PATTERN, not decoration. The diagnostic a caller
# gets INSTEAD of a verdict contains this tool's own name and a colon --
# `python3: can't open file '.../changes-requested-gate': [Errno 2] ...` -- and it
# arrives here through `2>&1`. Matching the name alone would read the operating
# system's report that the gate is missing as the gate having spoken.
cr_gate_out=$(python3 "$ROOT/ci-hub/bin/changes-requested-gate" \
  --repo "$R" --pr "$PR" --head "$HEAD" --source "$WT" 2>&1); cr_gate_rc=$?
if ! grep -qE '^changes-requested-gate: (CLEAR|BLOCKED|INDETERMINATE|MOOT)\b' \
     <<<"$cr_gate_out"; then
  abandon "REFUSING TO MERGE: the changes-requested gate returned NO VERDICT (rc=$cr_gate_rc). \
It did not run, so nothing was checked -- this is not a statement about $PR. \
Confirm $ROOT/ci-hub/bin/changes-requested-gate exists and that $ROOT is current with \
origin/main. Output was: ${cr_gate_out:-<empty>}" 5
fi
case "$cr_gate_rc" in
  0) say "changes-requested gate: clear -- $cr_gate_out" ;;
  *) abandon "REFUSING TO MERGE: $cr_gate_out" 5 ;;
esac

# 6. The merge command is the mergeability arbiter. Make exactly one attempt
# under the final-boundary verdict above. Retrying here would let an authorization
# age for minutes after the mutable tips were observed. A refusal reruns the gate,
# which re-fetches both tips and re-verifies the receipt; it never revalidates.
out=$(with-proxy gh pr merge "$PR" -R "$R" --rebase \
  --match-head-commit "$HEAD" 2>&1) && merged=ok || merged=
if [ -z "$merged" ] && grep -qi 'already merged' <<<"$out"; then
  say "already merged"
  merged=ok
fi
[ "$merged" = ok ] || abandon "gh pr merge --rebase refused after the fresh final-boundary check; rerun the gate (last: $(tr '\n' ' ' <<<"$out" | tail -c 160))" 6

# 7. ancestry-verify: a PR-head hash is NOT a landing. Confirm the merge commit is
# reachable from origin/main before declaring success.
with-proxy git -C "$WT" fetch -q origin main \
  || abandon "could not refresh $R main for the ancestry proof" 7
MC=$(with-proxy gh pr view "$PR" -R "$R" --json mergeCommit -q .mergeCommit.oid)
if git -C "$WT" merge-base --is-ancestor "$MC" origin/main 2>/dev/null; then
  # Return to the outer process. `land-lock run` must finish its descendant
  # census and release before any durable post-land verifier is launched.
  say "MERGED UNDER LOCK:$MC"
  exit 0
else
  abandon "merge reported but $MC is NOT an ancestor of origin/main (verify manually)" 7
fi
