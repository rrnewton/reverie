#!/usr/bin/env bash
# Shared single-PR lander for the FIFO-serialized manifest / e2e / backend-parity
# bucket. This is the DISCOVERABLE home for the landing sequence that used to live
# only in scratch/. It carries the three race-tolerance fixes below AND is always
# self-wrapped in `ci-hub land-lock run --child-deadline`, so the landing lease is
# bound to THIS bounded child's lifetime -- no hand-rolled `renewer.sh` loop that
# can outlive a dead agent and wedge the FIFO (the 2040-minute starvation bug).
#
# Sequence (while holding the land-lock):
#   fetch fresh main -> exact-head local-OR-hosted authority -> rebase
#   (union|plain, SKIPPED by --no-rebase) + push -> recheck the exact pushed head
#   -> derive the optional locally-validated cache only for a local receipt ->
#   bounded merge-gate poll -> gh pr merge --rebase (NEVER --admin)
#   -> ancestry-verify.
#
# Three fixes distilled from the 2026-08-03 stuck-gate diagnosis:
#   1. Trinary gate poll: PASSED lands, FAILED stops, and NO_RESULT blocks while
#      re-dispatching. PR statusCheckRollup is too narrow: it omits the
#      workflow_run-triggered dispatch that can carry the success.
#   2. The merge command is the mergeability arbiter, NOT mergeStateStatus (which
#      sticks at UNKNOWN); attempt `gh pr merge --rebase` in a bounded retry loop.
#   3. A genuine gate failure is never overwritten by re-stamping metadata.
#
# Boxing principle applied to ourselves: EVERY wait here is bounded, and every
# terminal bail emits a visible ABANDON signal (stderr + a role-tagged PR comment)
# so an abandoned PR never silently languishes (the #244 pattern). On any bail the
# `land-lock run` wrapper releases the lock so the next FIFO waiter proceeds.
#
# Usage:
#   ci-hub/landing/land-pr.sh <PR> <BRANCH> [--union|--no-rebase] [--agent NAME]
#                             [--gate-deadline SECS] [--child-deadline SECS]
#                             [--foreground]
#   --union          use the additive manifest union-rebase (union-rebase.sh);
#                    default is a plain `git rebase origin/main`.
#   --no-rebase      OPT-IN. Skip step 2 (the local rebase + force-push) and
#                    merge the already-authorized head as it stands. Mutually
#                    exclusive with --union. Rationale + how to re-verify it:
#                    see the step-2 comment. Default behaviour is UNCHANGED.
#   --agent NAME     lock holder + PR-comment role tag (default: hermit-lander).
#   --gate-deadline  bound on the merge-gate poll (default 1080).
#   --child-deadline hard ceiling for the whole land subtree (default: twice the
#                    gate deadline); passed to `land-lock run`, which kills and
#                    releases on breach. It must be greater than gate-deadline.
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

PR=""; BR=""; UNION=0; NO_REBASE=0; INNER=0; DETACHED_CHILD=0; FOREGROUND=0
AGENT="hermit-lander"
MODEL="${LANDER_MODEL:-opus-4.8}"
# Measured 2026-08-04 over 11 successful pull_request demo-hot-path runs created
# since 2026-08-03T23:00Z: median=586s, p90=646s, p95/p99/max=864s. The default
# is ceil(max * 1.25) = 1080s; the whole-child ceiling gets two gate windows.
GATE_DEADLINE=1080
CHILD_DEADLINE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --union) UNION=1 ;;
    --no-rebase) NO_REBASE=1 ;;
    --agent) AGENT="$2"; shift ;;
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
  echo "usage: land-pr.sh <PR> <BRANCH> [--union|--no-rebase] [--agent NAME] [--gate-deadline S] [--child-deadline S]" >&2
  exit 2
fi
# --union IS a rebase driver (union-rebase.sh rewrites and pushes the branch), so
# asking for both is incoherent rather than merely redundant. Refuse instead of
# silently picking one: a lander that thinks it skipped the rewrite while the
# union driver performed it is exactly the failure this flag exists to prevent.
if [ "$UNION" -eq 1 ] && [ "$NO_REBASE" -eq 1 ]; then
  echo "land-pr: --union and --no-rebase are mutually exclusive (--union rebases by definition)" >&2
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
  printf 'DOCS PARSE OK: land-pr.sh pr=%s branch=%s union=%s no_rebase=%s agent=%s\n' \
    "$PR" "$BR" "$UNION" "$NO_REBASE" "$AGENT"
  exit 0
fi

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
WT="$ROOT/worktrees/lander/hermit"
R=rrnewton/hermit
say(){ echo "[land#$PR] $*"; }

# BIND THE PR TO ITS REPOSITORY BEFORE DEREFERENCING ANY AUTHORITY.
# `R` is hard-coded to rrnewton/hermit and there is no --repo option, so invoking
# this with a PR number from another repository silently resolves the SAME NUMBER
# in hermit and validates against a completely unrelated commit. Observed
# 2026-08-08: `land-pr.sh 409 policy/commit-message-role-team-tag` for
# rrnewton/reverie#409 reported "exact-head authority green for 18758e9a" -- the
# head of hermit#1911 -- and dereferenced a hermit merge-gate run for it. It
# abandoned only because head resolution happened to fail further down; nothing
# had checked that PR 409 was a hermit PR at all.
#
# A PR number is not an identity. Refuse unless the number resolves in THIS
# repository to a PR whose head branch is the branch the caller named.
verify_pr_belongs_to_repo(){
  local actual
  actual=$(with-proxy gh pr view "$PR" -R "$R" --json headRefName -q .headRefName 2>/dev/null) || {
    echo "land-pr: REFUSE: PR #$PR does not resolve in $R" >&2
    exit 2
  }
  if [ -z "$actual" ]; then
    echo "land-pr: REFUSE: PR #$PR has no head branch in $R" >&2
    exit 2
  fi
  if [ "$actual" != "$BR" ]; then
    echo "land-pr: REFUSE: $R#$PR has head branch '$actual', but '$BR' was named." >&2
    echo "land-pr:   This lander only lands $R. If #$PR belongs to another" >&2
    echo "land-pr:   repository, it CANNOT be landed here -- the same number in $R" >&2
    echo "land-pr:   is a different pull request and its green is not your evidence." >&2
    exit 2
  fi
}
comment_abandon(){
  local reason="$1"
  with-proxy gh pr comment "$PR" -R "$R" --body \
    "[coordinator, $MODEL] ABANDONED landing attempt: ${reason}. The lock was released so the FIFO can continue. Retry if the PR remains open; durable recovery will arm exact-SHA verification if GitHub completed the merge." \
    >/dev/null 2>&1 || say "WARN: could not post ABANDON comment"
}
comment_no_result(){
  local reason="$1"
  with-proxy gh pr comment "$PR" -R "$R" --body \
    "[coordinator, $MODEL] NO-RESULT landing pause: ${reason}. No failing verdict was recorded; the missing check was re-dispatched and this PR remains blocked until it produces PASSED or FAILED." \
    >/dev/null 2>&1 || say "WARN: could not post NO-RESULT comment"
}

# A real land exceeds the agent shell's foreground time budget. Launch the
# entire self-healing lock/land/arm sequence in a new session by default; the
# timestamped log is the durable observation surface across agent recycling.
if [ "$INNER" -eq 0 ] && [ "$DETACHED_CHILD" -eq 0 ] && [ "$FOREGROUND" -eq 0 ]; then
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  safe_pr=${PR//[^A-Za-z0-9_.-]/_}
  log_dir="${CI_HUB_LANDING_LOG_DIR:-$ROOT/ignored/ci-hub/landing}"
  log="$log_dir/land-pr${safe_pr}-${stamp}-$$.log"
  mkdir -p "$log_dir"
  detached_args=(--_detached "$PR" "$BR" --agent "$AGENT" \
    --gate-deadline "$GATE_DEADLINE" --child-deadline "$CHILD_DEADLINE")
  [ "$UNION" -eq 1 ] && detached_args+=(--union)
  [ "$NO_REBASE" -eq 1 ] && detached_args+=(--no-rebase)
  printf 'DETACHED LAND START pr=%s branch=%s agent=%s started_at=%s\n' \
    "$PR" "$BR" "$AGENT" "$stamp" >"$log"
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
  # A replacement lander inherits durable remediation from state, without
  # depending on a wake sent to its predecessor. This acknowledges discovery;
  # the obligation remains open until the repair SHA is explicitly resolved.
  "$ROOT/ci-hub/ci-hub" inherit-obligations --agent "$AGENT" \
    --session "${ORC_AGENT_SESSION_ID:-${HOSTNAME:-unknown}:$$}"
  args=(--_inner "$PR" "$BR" --agent "$AGENT" --gate-deadline "$GATE_DEADLINE")
  [ "$UNION" -eq 1 ] && args+=(--union)
  [ "$NO_REBASE" -eq 1 ] && args+=(--no-rebase)
  # Persist the exact-SHA verification obligation intent before the bounded
  # child can merge. If this process dies after the merge, the ORC recovery
  # watcher observes the merged SHA and arms both verifiers.
  if ! "$ROOT/ci-hub/remediation/land_and_arm.py" prepare \
      --repo "$R" --pr "$PR" --source "$ROOT/hermit" \
      --land-mode speculative --actor "$AGENT"; then
    say "ABANDON: could not prepare the post-land verification obligation"
    comment_abandon "could not prepare the mandatory post-land verification obligation"
    exit 2
  fi
  "$ROOT/ci-hub/ci-hub" land-lock run --agent "$AGENT" --pr "$PR" \
    --child-deadline "$CHILD_DEADLINE" -- "$0" "${args[@]}"
  rc=$?
  # The hard deadline kills the entire inner process group, so only this outer
  # supervisor remains able to emit the durable PR-side abandonment signal.
  case "$rc" in
    1) comment_abandon "timed out waiting for the landing lock" ;;
    124) comment_abandon "land subtree exceeded the ${CHILD_DEADLINE}s hard deadline and was killed" ;;
  esac
  exit "$rc"
fi

# --- inner: we now hold the land-lock ----------------------------------------
# Visible terminal ABANDON signal: stderr + role-tagged PR comment, then exit.
abandon(){
  local reason="$1" code="${2:-1}"
  say "ABANDON: $reason"
  comment_abandon "$reason"
  exit "$code"
}

# 1. fresh main
with-proxy git -C "$WT" fetch -q origin main || abandon "fetch origin/main failed" 2
with-proxy git -C "$WT" fetch -q origin "$BR" 2>/dev/null || true

# 1b. Owner-authorized exact-head OR gate. A counted local receipt and the
# registered hosted job set are interchangeable positive authorities; a
# genuine red from either blocks. Missing/partial/stale evidence is NO_RESULT,
# never a green. The same predicate is checked again after a SHA-changing rebase.
verify_pr_belongs_to_repo
ORIG=$(git -C "$WT" rev-parse "origin/$BR" 2>/dev/null) || abandon "cannot resolve origin/$BR head for eligibility gate" 4
VS=$("$SCRIPT_DIR/exact-head-validation-authority.sh" --repo "$R" --sha "$ORIG" 2>&1); VRC=$?
say "exact-head validation(head=$ORIG) rc=$VRC: $VS"
case "$VRC" in
  0) say "landing eligibility: exact-head authority green for $ORIG" ;;
  3) abandon "exact-head authority reported a genuine red for PR head $ORIG" 4 ;;
  4) abandon "neither exact-head authority produced green for PR head $ORIG" 4 ;;
  *) abandon "could not evaluate exact-head validation authority (rc=$VRC)" 4 ;;
esac

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
# all three held a qualifying `AUTHORITY=hosted` green (~30 min of hosted CI
# each) that an unconditional rebase would have voided the moment another team
# advanced main. See also rrnewton/hermit#1812, where an unconditional
# rebase-and-force-push in the union driver amended main's tip onto two PR
# branches and landed #1188/#1209 as semantic no-ops.
#
# This flag removes a MUTATION, never a CHECK. Still executed on this path: the
# land-lock (outer `land-lock run`), the step-1b exact-head authority, the step-4
# recheck at the head actually being merged, the merge-gate poll, the step-5b
# final-boundary authority + receipt dereference, `--match-head-commit`,
# obligation arming, and the post-merge mergeCommit.oid ancestry proof.
if [ "$NO_REBASE" -eq 1 ]; then
  say "no-rebase: skipping the local rebase + force-push; merging the authorized head as it stands"
elif [ "$UNION" -eq 1 ]; then
  ulog="/tmp/land-$PR-union.log"
  "$SCRIPT_DIR/union-rebase.sh" "$WT" "$BR" --push >"$ulog" 2>&1
  RES=$(grep -E '^RESULT' "$ulog" | tail -1)
  say "union: ${RES:-<none> (see $ulog)}"
  case "$RES" in
    *" CLEAN"|*" UNIONED") : ;;
    *) abandon "union-rebase did not converge: ${RES:-see $ulog}" 3 ;;
  esac
else
  git -C "$WT" checkout -q -B "_land_$PR" "origin/$BR" || abandon "checkout origin/$BR failed" 3
  if ! GIT_EDITOR=true git -C "$WT" rebase origin/main >/dev/null 2>&1; then
    git -C "$WT" rebase --abort >/dev/null 2>&1
    git -C "$WT" checkout -q --detach origin/main 2>/dev/null || true
    abandon "plain rebase onto origin/main conflicted (needs owner/--union)" 3
  fi
  with-proxy git -C "$WT" push -q --force-with-lease origin "HEAD:$BR" || abandon "force-with-lease push failed" 3
  git -C "$WT" checkout -q --detach origin/main 2>/dev/null || true
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

# 4. The pushed exact head needs a fresh positive from either registered
# authority. A rebase that changed the SHA cannot inherit the old authorization.
# Only the ledger-guarded applier may materialize the optional local cache label.
#
# 4a. Re-mint one exact count-backed schema-5 row from its durable log BEFORE
# reading the ledger. Hermit's Rust driver can write a count-less receipt
# when it can't reach the parent count helper; with the uncounted-receipt
# grandfather removed, such a genuine green would be NotValidated. Select one
# exact HEAD row by its Rust-canonical digest, then pass both identities into the
# append-safe finalizer so a real green is not stranded by a producer that
# failed to inline its counts. Best-effort: never aborts landing — eligibility
# below remains the authoritative fail-closed gate.
selected_row_sha256=$("$ROOT/ci-hub/validate/scan-finalize.sh" \
  --select-candidate-sha256 --sha "$HEAD" --hermit-checkout "$WT" \
  2>/dev/null) || selected_row_sha256=
if [[ $selected_row_sha256 =~ ^[0-9a-f]{64}$ ]]; then
  "$ROOT/ci-hub/validate/scan-finalize.sh" --hermit-checkout "$WT" \
    --sha "$HEAD" --selected-row-sha256 "$selected_row_sha256" || true
else
  say "scan-finalize: no unique exact source row selected for $HEAD (best-effort)"
fi
# Capture the VERBATIM first_error_line of every surviving red log into the
# durable append-only sidecar (ignored/validate-red-attribution.jsonl) BEFORE the
# /tmp log is evicted. Append-only + idempotent, so it races no appender and never
# duplicates; best-effort (never a fatal error) and never affects the landing
# verdict below -- it only preserves attribution that would otherwise die with the
# log. Until Hermit's Rust driver inlines first_error_line into the red row it
# writes, this is what makes a red attributable to which BUG (not just which gate)
# after the log is gone.
python3 "$ROOT/ci-hub/validate/attribute_reds.py" --last 0 --persist >/dev/null 2>&1 || true
VS=$("$SCRIPT_DIR/exact-head-validation-authority.sh" --repo "$R" --sha "$HEAD" 2>&1); VRC=$?
say "post-push exact-head validation(head=$HEAD) rc=$VRC: $VS"
[ "$VRC" -eq 0 ] || abandon "pushed head $HEAD has no accepted exact-head validation authority (rc=$VRC)" 4
if grep -q 'LOCAL=green' <<<"$VS"; then
  "$ROOT/ci-hub/ci-hub" apply-local-label --pr "$PR" --repo "$R" \
    || abandon "ledger-guarded apply-local-label failed" 4
  sleep 4
  LB=$(with-proxy gh pr view "$PR" -R "$R" --json labels -q '[.labels[].name]|join(",")')
  grep -q locally-validated <<<"$LB" || abandon "locally-validated stripped immediately (labels=$LB)" 4
  say "ledger-derived label present; labels=$LB"
else
  say "hosted exact-head authority selected; no local-receipt cache label required"
fi

# 4b. a draft PR cannot be merged; marking ready fires a fresh (label-present)
# merge-gate run, which the poll below waits on.
if [ "$(with-proxy gh pr view "$PR" -R "$R" --json isDraft -q .isDraft)" = "true" ]; then
  with-proxy gh pr ready "$PR" -R "$R" >/dev/null && say "marked ready (was draft)"
  sleep 4
fi

# 5. FIX 1 + FIX 3: bounded, race-tolerant merge-gate poll. Query Actions by the
# exact PR head and admit only pull_request/workflow_dispatch runs. The latter is
# load-bearing: merge-gate's workflow_run controller dispatches the real PR-head
# gate, but that success is absent from PR statusCheckRollup. A genuine FAILED
# result stops immediately. NO_RESULT (cancelled/skipped/neutral/pending/absent)
# blocks without becoming a failure and re-dispatches the gate once per observed
# terminal hole. UNKNOWN/stuck exits with a distinct temporary/no-result code.
selector="$SCRIPT_DIR/merge-gate-status.jq"
outcome_helper="$ROOT/ci-hub/check_outcome.py"
deadline=$((SECONDS+GATE_DEADLINE)); gate=""; cj="UNAVAILABLE/PENDING"; gate_detail="no successful Actions query"; dispatched_run=""; gate_status="UNAVAILABLE"; gate_conclusion="PENDING"
while (( SECONDS < deadline )); do
  if actions_json=$(with-proxy gh api \
      "repos/$R/actions/workflows/merge-gate.yml/runs?head_sha=$HEAD&per_page=100" \
      2>/dev/null); then
    gate_run=$(python3 "$outcome_helper" \
        --select-latest-run --head-sha "$HEAD" \
        --event pull_request --event workflow_dispatch <<<"$actions_json")
    row=$(jq -r -f "$selector" <<<"$gate_run")
    IFS=$'\t' read -r gate_status gate_conclusion gate_event gate_run gate_url gate_created <<<"$row"
    cj="${gate_status}/${gate_conclusion}"
    gate_detail="event=$gate_event run=$gate_run created=$gate_created url=$gate_url"
  else
    gate_status="UNAVAILABLE"
    gate_conclusion="PENDING"
    cj="UNAVAILABLE/PENDING"
    gate_detail="Actions API unavailable"
  fi
  check_outcome=$(python3 "$outcome_helper" --status "$gate_status" --conclusion "$gate_conclusion")
  say "merge-gate(exact-head)=$cj outcome=$check_outcome $gate_detail"
  case "$check_outcome" in
    PASSED) gate=ok; break ;;
    FAILED) abandon "merge-gate produced a genuine FAILED verdict for exact head $HEAD ($cj; $gate_detail)" 5 ;;
    NO_RESULT)
      # A terminal hole or an absent run needs a new observation. Do not duplicate
      # an already queued/running run, and do not dispatch the same terminal hole
      # repeatedly while GitHub is still creating its successor.
      if { [ "$gate_status" = "COMPLETED" ] || [ "$gate_status" = "MISSING" ]; } &&
         [ "${gate_run:--}" != "$dispatched_run" ]; then
        if with-proxy gh workflow run merge-gate.yml -R "$R" --ref "$BR" -f pr_number="$PR"; then
          dispatched_run="${gate_run:--}"
          say "merge-gate NO_RESULT re-dispatched for exact head $HEAD"
        else
          say "merge-gate NO_RESULT re-dispatch failed; will retry"
        fi
      fi
      ;;
  esac
  sleep 15
done
if [ "$gate" != ok ]; then
  reason="merge-gate remained NO_RESULT for exact head $HEAD through the ${GATE_DEADLINE}s deadline (last=$cj; $gate_detail)"
  say "NO-RESULT: $reason"
  comment_no_result "$reason"
  exit 75
fi

# 5b. Re-evaluate the exact-head OR policy at the final mutation boundary. A
# local positive now additionally dereferences its immutable receipt comment;
# a hosted positive remains independently sufficient. Any genuine red blocks.
# Mutable-tip currency is asserted HERE, exactly once: refresh both authorities
# immediately before dereferencing the receipt. A stale receipt costs this gate
# rerun, never another full validation. No ancestry-distance budget exists.
with-proxy git -C "$WT" fetch -q origin main \
  || abandon "could not refresh Hermit main at the final receipt boundary" 5
current_base=$(git -C "$WT" rev-parse --verify origin/main) \
  || abandon "could not resolve Hermit main at the final receipt boundary" 5
with-proxy git -C "$ROOT/reverie" fetch -q origin main \
  || abandon "could not refresh Reverie main at the final receipt boundary" 5
current_reverie_base=$(git -C "$ROOT/reverie" rev-parse --verify origin/main) \
  || abandon "could not resolve Reverie main at the final receipt boundary" 5
live_head=$(with-proxy gh pr view "$PR" -R "$R" --json headRefOid -q .headRefOid 2>/dev/null) \
  || abandon "could not resolve the live PR head before receipt authorization" 5
[ "$live_head" = "$HEAD" ] \
  || abandon "PR head moved before receipt authorization (expected $HEAD, observed ${live_head:-missing})" 5
receipt_comments=$(mktemp) \
  || abandon "could not allocate the receipt-comment observation file" 5
if ! with-proxy gh api --paginate --slurp \
    "repos/$R/issues/$PR/comments?per_page=100" >"$receipt_comments"; then
  printf '[]\n' >"$receipt_comments"
fi
receipt_detail=$("$SCRIPT_DIR/exact-head-validation-authority.sh" \
  --repo "$R" --sha "$HEAD" --comments "$receipt_comments" \
  --current-base "$current_base" \
  --current-reverie-base "$current_reverie_base" \
  --repo-checkout "$WT" --reverie-checkout "$ROOT/reverie" 2>&1)
receipt_rc=$?
rm -f -- "$receipt_comments"
if [ "$receipt_rc" -ne 0 ]; then
  abandon "exact-head validation authority REFUSED for $HEAD: ${receipt_detail:-no result}" 5
fi
say "exact-head validation authority authorized: $receipt_detail"

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
with-proxy git -C "$ROOT/hermit" fetch -q origin main 2>/dev/null \
  || with-proxy git -C "$WT" fetch -q origin main
MC=$(with-proxy gh pr view "$PR" -R "$R" --json mergeCommit -q .mergeCommit.oid)
GITDIR="$ROOT/hermit"; git -C "$GITDIR" cat-file -e "$MC" 2>/dev/null || GITDIR="$WT"
if git -C "$GITDIR" merge-base --is-ancestor "$MC" origin/main 2>/dev/null; then
  if "$ROOT/ci-hub/remediation/land_and_arm.py" complete --repo "$R" --pr "$PR"; then
    # Wave-level pile reconciliation. The ancestry-verify above proves THIS PR
    # landed; it says nothing about the task pile, which is closed on ancestry
    # only and therefore grows monotonically until something re-checks it. A
    # one-off check on 2026-08-06 found 163 of 302 already on main.
    #
    # Detached and single-flight: a serial wave lands many PRs, and this must
    # neither delay a land nor spawn one audit per PR. Wrapped so a defect in
    # the audit can never fail a landing that already succeeded.
    if [ "${CI_HUB_POST_LAND_AUDIT:-1}" = 1 ]; then
      (
        exec 9>"$ROOT/ignored/.ancestry-audit.lock" 2>/dev/null || exit 0
        flock -n 9 || exit 0   # an audit is already running; it will see this land too
        setsid "$ROOT/ci-hub/bin/ancestry-audit" --herdr-agent "${AGENT:-hermit-lander}" \
          --json "$ROOT/ignored/ancestry/post-land-audit.json" \
          >"$ROOT/ignored/ancestry/post-land-audit.log" 2>&1
      ) >/dev/null 2>&1 &
    fi
    say "LANDED:$MC"; exit 0
  fi
  say "POST-LAND ARM PENDING: $MC is landed; durable recovery will retry"
  with-proxy gh pr comment "$PR" -R "$R" --body \
    "[coordinator, $MODEL] LANDED at $MC, but immediate exact-SHA verification arming failed. The durable prepared intent remains open and the ORC recovery watcher will retry; this landing is not being reported complete." \
    >/dev/null 2>&1 || say "WARN: could not post post-land arm warning"
  exit 8
else
  abandon "merge reported but $MC is NOT an ancestor of origin/main (verify manually)" 7
fi
