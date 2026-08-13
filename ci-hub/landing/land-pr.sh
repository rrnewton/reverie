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
#   ci-hub/landing/land-pr.sh <PR> <BRANCH> [--union|--no-rebase] [--agent NAME]
#                             [--gate-deadline SECS] [--child-deadline SECS]
#                             [--checkout HERMIT_SLOT] [--team TEAM] [--foreground]
#   --union          use the additive manifest union-rebase (union-rebase.sh);
#                    default is a plain `git rebase origin/main`.
#   --no-rebase      OPT-IN. Skip step 2 (the local rebase + force-push) and
#                    merge the already-authorized head as it stands. Mutually
#                    exclusive with --union. Rationale + how to re-verify it:
#                    see the step-2 comment. Default behaviour is UNCHANGED.
#   --agent NAME     reviewer + lock-holder identity (default: DG_AGENT_NAME).
#   --team TEAM      reviewer disclosure team (default: hermit2).
#   --checkout PATH  reviewer's Hermit slot. When omitted, use the current
#                    directory only if it is an rrnewton/hermit checkout.
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

PR=""; BR=""; UNION=0; NO_REBASE=0; INNER=0; DETACHED_CHILD=0; FOREGROUND=0
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
  printf 'DOCS PARSE OK: land-pr.sh pr=%s branch=%s union=%s no_rebase=%s agent=%s team=%s checkout=%s\n' \
    "$PR" "$BR" "$UNION" "$NO_REBASE" "$AGENT" "$TEAM" "${CHECKOUT:-current-directory}"
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
remote_url=$(git -C "$WT" remote get-url origin 2>/dev/null || true)
case "$remote_url" in
  *github.com/rrnewton/hermit.git|*github.com/rrnewton/hermit|\
  *github.com:rrnewton/hermit.git|*github.com:rrnewton/hermit) ;;
  *)
    echo "land-pr: reviewer must run from an rrnewton/hermit slot or pass --checkout" >&2
    exit 2
    ;;
esac
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
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  safe_pr=${PR//[^A-Za-z0-9_.-]/_}
  log_dir="${CI_HUB_LANDING_LOG_DIR:-$ROOT/ignored/ci-hub/landing}"
  log="$log_dir/land-pr${safe_pr}-${stamp}-$$.log"
  mkdir -p "$log_dir"
  detached_args=(--_detached "$PR" "$BR" --agent "$AGENT" --team "$TEAM" --checkout "$WT" \
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
  args=(--_inner "$PR" "$BR" --agent "$AGENT" --team "$TEAM" --checkout "$WT" --gate-deadline "$GATE_DEADLINE")
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

  # The merge and ancestry proof are already complete. Post-land remediation is
  # useful bookkeeping, never retroactive authorization and never a reason to
  # report a successful land as failed. Prepare and arm only after the merge.
  MC=$(with-proxy gh pr view "$PR" -R "$R" --json mergeCommit -q .mergeCommit.oid 2>/dev/null || true)
  if "$ROOT/ci-hub/remediation/land_and_arm.py" prepare \
      --repo "$R" --pr "$PR" --source "$WT" \
      --land-mode speculative --actor "$AGENT" >/dev/null 2>&1 \
      && "$ROOT/ci-hub/remediation/land_and_arm.py" complete \
        --repo "$R" --pr "$PR" >/dev/null 2>&1; then
    say "post-land verification obligation armed"
  else
    say "WARN: post-land verification obligation was not armed; landing remains successful"
    publish_landing_comment post-land-arm-failed \
      "post-land verification arming returned no completed obligation; the merge remains landed" \
      || say "POST-LAND COMMENT PUBLICATION FAILED: use the durable record printed above"
  fi

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
abandon(){
  local reason="$1" code="${2:-1}"
  say "ABANDON: $reason"
  comment_abandon "$reason" \
    || say "ABANDON COMMENT PUBLICATION FAILED: use the durable record printed above"
  exit "$code"
}

# 1. fresh main
with-proxy git -C "$WT" fetch -q origin main || abandon "fetch origin/main failed" 2
with-proxy git -C "$WT" fetch -q origin "$BR" 2>/dev/null || true

# 1b. Validation landability is defined only by the tracked
# hermit-validation-authority skill. This entrypoint calls its consumer; it does
# not restate hard/soft-green policy. Re-check after every SHA-changing rebase.
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
# JUST-IN-TIME PR-HEAD CI. A prepared, reviewed head whose exact commit has no
# accepted verdict gets one run through the same centralized producer as every
# other qualifying receipt. Do not validate a stale base: the producer would
# correctly refuse it. The helper re-reads the live PR head after VALIDATE, so a
# concurrent push expires this result instead of inheriting it.
if [ "$VRC" -ne 0 ]; then
  existing_status=$("$ROOT/ci-hub/ci-hub" validate-status --sha "$ORIG" --json 2>&1)
  existing_rc=$?
  if [ "$existing_rc" -eq 4 ] && \
     git -C "$WT" merge-base --is-ancestor "$eligibility_base" "$ORIG"; then
    git -C "$WT" checkout -q --detach "$ORIG" \
      || abandon "cannot materialize exact PR head $ORIG for just-in-time validation" 4
    jit_detail=$("$ROOT/ci-hub/bin/pr-head-ci" \
      --repo "$R" --pr "$PR" --expected-head "$ORIG" \
      --checkout "$WT" --agent "$AGENT" --execute 2>&1)
    jit_rc=$?
    git -C "$WT" checkout -q --detach origin/main \
      || abandon "cannot restore lander checkout after just-in-time validation" 4
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
    say "just-in-time exact-head CI not admitted: PR head lacks current origin/main; rebase before VALIDATE"
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
# final-boundary authority + receipt dereference, `--match-head-commit`,
# obligation arming, and the post-merge mergeCommit.oid ancestry proof.
plain_rebase=0
rebase_base=$(git -C "$WT" rev-parse --verify origin/main) \
  || abandon "cannot resolve current main before rebase" 3
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
  plain_rebase=1
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
if [ "$plain_rebase" -eq 1 ] && [ "$HEAD" != "$ORIG" ]; then
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
  say "rebase provenance unchanged: rebase did not rewrite $ORIG"
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
selected_row_sha256=$("$ROOT/ci-hub/validate/scan-finalize.sh" \
  --select-candidate-sha256 --sha "$HEAD" --hermit-checkout "$WT" \
  2>/dev/null) || selected_row_sha256=
if [[ $selected_row_sha256 =~ ^[0-9a-f]{64}$ ]]; then
  "$ROOT/ci-hub/validate/scan-finalize.sh" --hermit-checkout "$WT" \
    --sha "$HEAD" --selected-row-sha256 "$selected_row_sha256" || true
else
  say "scan-finalize: no unique exact source row selected for $HEAD (best-effort)"
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

# A changed validation producer cannot land until its exact parent transition
# is active. Check this at the final mutation boundary, after refreshing the PR
# head, so a transition for another PR or an earlier push cannot authorize it.
producer_detail=$(python3 "$SCRIPT_DIR/producer_transition_guard.py" \
  --repo "$R" --pr "$PR" --head "$HEAD" --checkout "$WT" 2>&1)
producer_rc=$?
say "producer transition authority(head=$HEAD) rc=$producer_rc: $producer_detail"
[ "$producer_rc" -eq 0 ] \
  || abandon "producer transition authority REFUSED for $HEAD: ${producer_detail:-no result}" 5

# Review labels are caches, never authority. Re-read this PR through the one
# APPROVED-AT verifier at the final mutation boundary. A passed-review-* label
# with no exact-head binding is ABSENT approval and refuses the merge; it is not
# softened into a present-but-unverified state. No label is planted or deleted.
review_detail=$(python3 "$ROOT/ci-hub/health/approval_binding.py" \
  --repo "$R" --pr "$PR" --gate 2>&1)
review_rc=$?
say "exact-head review authority(head=$HEAD) rc=$review_rc: $review_detail"
[ "$review_rc" -eq 0 ] \
  || abandon "exact-head review authority REFUSED for $HEAD: ${review_detail:-no result}" 5

receipt_comments=$(mktemp) \
  || abandon "could not allocate the receipt-comment observation file" 5
if ! with-proxy gh api --paginate --slurp \
    "repos/$R/issues/$PR/comments?per_page=100" >"$receipt_comments"; then
  printf '[]\n' >"$receipt_comments"
fi
receipt_detail=$(validation_landability "$HEAD" "$current_base" \
  --comments "$receipt_comments" \
  --current-reverie-base "$current_reverie_base" \
  --reverie-checkout "$ROOT/reverie" 2>&1)
receipt_rc=$?
rm -f -- "$receipt_comments"
if [ "$receipt_rc" -ne 0 ]; then
  abandon "validation landability REFUSED for $HEAD: ${receipt_detail:-no result}" 5
fi
say "validation landability authorized: $receipt_detail"

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
  # Return to the outer process. `land-lock run` must finish its descendant
  # census and release before any durable post-land verifier is launched.
  say "MERGED UNDER LOCK:$MC; post-land obligation arming deferred until release"
  exit 0
else
  abandon "merge reported but $MC is NOT an ancestor of origin/main (verify manually)" 7
fi
