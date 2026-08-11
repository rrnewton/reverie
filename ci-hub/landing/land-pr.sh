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
#                             [--foreground] [--gate-rotation]
#   --union          use the additive manifest union-rebase (union-rebase.sh);
#                    default is a plain `git rebase origin/main`.
#   --no-rebase      OPT-IN. Skip step 2 (the local rebase + force-push) and
#                    merge the already-authorized head as it stands. Mutually
#                    exclusive with --union. Rationale + how to re-verify it:
#                    see the step-2 comment. Default behaviour is UNCHANGED.
#   --agent NAME     lock holder + PR-comment role tag (default: hermit-lander).
#   --gate-deadline  compatibility bound retained for callers and for sizing the
#                    default whole-child deadline; no GH Actions poll waits on it.
#   --child-deadline hard ceiling for the whole land subtree (default: twice the
#                    gate deadline); passed to `land-lock run`, which kills and
#                    releases on breach. It must exceed the compatibility bound.
#   --foreground     diagnostic escape hatch; default launches under nohup+setsid
#                    with a durable timestamped log and returns immediately.
#   --gate-rotation  independently verify an owner-recorded merge-gate definition
#                    rotation at every mutation boundary. Requires --no-rebase.
#                    This is an authority mode, not an exemption: the exact live
#                    record, reviews, blobs, paths, receipt, and typed candidate
#                    self-refusal are all re-derived by trusted current-main code.
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

PR=""; BR=""; UNION=0; NO_REBASE=0; INNER=0; DETACHED_CHILD=0; FOREGROUND=0; GATE_ROTATION=0
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
    --gate-rotation) GATE_ROTATION=1 ;;
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
if [ "$GATE_ROTATION" -eq 1 ] && [ "$NO_REBASE" -ne 1 ]; then
  echo "land-pr: --gate-rotation requires --no-rebase; exact head/base approval cannot survive a rewrite" >&2
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
  printf 'DOCS PARSE OK: land-pr.sh pr=%s branch=%s union=%s no_rebase=%s gate_rotation=%s agent=%s\n' \
    "$PR" "$BR" "$UNION" "$NO_REBASE" "$GATE_ROTATION" "$AGENT"
  exit 0
fi

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
WT="$ROOT/worktrees/lander/hermit"
R=rrnewton/hermit
say(){ echo "[land#$PR] $*"; }
verify_rotation_authority(){
  local expected_head="$1" detail rc
  detail=$(python3 "$SCRIPT_DIR/gate_definition_rotation.py" --verify-live \
    --pr "$PR" --checkout "$WT" --expected-head "$expected_head" 2>&1)
  rc=$?
  say "gate-rotation authority(head=$expected_head) rc=$rc: $detail"
  [ "$rc" -eq 0 ] || return "$rc"
}

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
  [ "$GATE_ROTATION" -eq 1 ] && detached_args+=(--gate-rotation)
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
  [ "$GATE_ROTATION" -eq 1 ] && args+=(--gate-rotation)
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
  [ "$rc" -eq 0 ] || exit "$rc"

  # The bounded child has exited and `land-lock run` has proven its cleanup
  # domain empty before returning. Arm the durable post-land obligation only
  # NOW, outside that cleanup boundary. Starting the verifier/watchers inside
  # `_inner` made their deliberately detached process groups descendants of
  # the lock supervisor: successful landings then froze and quarantined live
  # post-land work as if it were an escaped payload. We must not weaken the
  # descendant census instead -- an arbitrary payload can use `setsid` too.
  if "$ROOT/ci-hub/remediation/land_and_arm.py" complete --repo "$R" --pr "$PR"; then
    intent_dir="${CI_HUB_LAND_INTENT_DIR:-$ROOT/ignored/ci-hub/land-intents}"
    intent_path="$intent_dir/${R//\//-}-pr${PR}.json"
    MC=$(jq -er '.landed_sha | select(test("^[0-9a-f]{40}$"))' "$intent_path" 2>/dev/null) || {
      say "POST-LAND ARM PENDING: obligation armed, but exact landed SHA was unreadable from $intent_path"
      exit 8
    }

    # Rotation registration is part of the normal lander composition, but it
    # runs only after the bounded child returned: that proves its mergeCommit
    # ancestry and lets land-lock finish its descendant census and release.
    # register-only re-derives the nine immutable facts, refreshes main, proves
    # ancestry again, reads the landed blob, then PATCHes and verifies the
    # registration. A direct --gate-rotation invocation therefore cannot merge
    # through the checked path while silently omitting post-ancestry registration.
    if [ "$GATE_ROTATION" -eq 1 ]; then
      python3 "$SCRIPT_DIR/gate_definition_rotation.py" --register-only \
        --pr "$PR" --checkout "$WT" \
        || {
          say "POST-LAND REGISTRATION PENDING: main is fail-closed; retry --register-only"
          exit 8
        }
    fi

    # Wave-level pile reconciliation. The inner ancestry check proves THIS PR
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
  say "POST-LAND ARM PENDING: merge is landed; durable recovery will retry"
  with-proxy gh pr comment "$PR" -R "$R" --body \
    "[coordinator, $MODEL] LANDED, but immediate exact-SHA verification arming failed after the landing lock released. The durable prepared intent remains open and the ORC recovery watcher will retry; this landing is not being reported complete." \
    >/dev/null 2>&1 || say "WARN: could not post post-land arm warning"
  exit 8
fi

# --- inner: we now hold the land-lock ----------------------------------------
# ``--_inner`` is an implementation detail elsewhere in this lander, not a
# rotation capability. A direct caller that supplies it must still prove this
# process descends from the exact PID/start-ticks identity recorded by
# ``land-lock run``. Thus the rotation mode cannot bypass serialization even if
# every command-line flag is known.
if [ "$GATE_ROTATION" -eq 1 ]; then
  python3 "$SCRIPT_DIR/gate_definition_rotation.py" --assert-land-lock \
    --pr "$PR" --agent "$AGENT" \
    || { echo "land-pr: REFUSE: gate rotation is outside its recorded land-lock subtree" >&2; exit 2; }
  say "GATE-ROTATION-EVENT enter_land_lock"
fi
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

# 1b. Owner-authorized exact-head local-ledger gate. GH Actions is diagnostic:
# it neither authorizes nor vetoes a landing. Missing/partial/stale local
# evidence is NO_RESULT, never green. Re-check after every SHA-changing rebase.
verify_pr_belongs_to_repo
ORIG=$(git -C "$WT" rev-parse "origin/$BR" 2>/dev/null) || abandon "cannot resolve origin/$BR head for eligibility gate" 4
# A gate-changing branch is not allowed to choose the ordinary authority path.
# Detect from the branch's own merge-base delta (not the mutable two-tip diff)
# so a caller cannot omit --gate-rotation and let a branch-owned producer vouch
# for itself. The independent path below must emit and then supersede the typed
# SELF_AUTHORIZATION_REFUSED with the exact owner/review/blob record.
branch_base=$(git -C "$WT" merge-base origin/main "$ORIG") \
  || abandon "cannot resolve PR merge-base for self-authorization check" 4
producer_exception=0
authority_mode=(--local-only)
if git -C "$WT" diff --name-only "$branch_base" "$ORIG" -- \
    scripts/validate.rs | grep -Fxq scripts/validate.rs; then
  producer_exception=1
  authority_mode=(--hosted-only)
  [ "$NO_REBASE" -eq 1 ] || abandon "producer-changing PR requires --no-rebase so exact-head hosted authority survives" 4
  say "producer exception: selecting registered exact-head hosted authority"
fi
if git -C "$WT" diff --name-only "$branch_base" "$ORIG" -- \
    .github/workflows/merge-gate.yml | grep -Fxq .github/workflows/merge-gate.yml; then
  if [ "$GATE_ROTATION" -ne 1 ]; then
    say "SELF_AUTHORIZATION_REFUSED: branch changes .github/workflows/merge-gate.yml without an independent rotation record"
    abandon "gate-definition change requires the sanctioned --gate-rotation authority" 4
  fi
fi
if [ "$GATE_ROTATION" -eq 1 ]; then
  verify_rotation_authority "$ORIG" \
    || abandon "independent gate-rotation authority refused PR head $ORIG" 4
else
  VS=$("$SCRIPT_DIR/exact-head-validation-authority.sh" "${authority_mode[@]}" --repo "$R" --sha "$ORIG" 2>&1); VRC=$?
  say "exact-head validation(head=$ORIG) rc=$VRC: $VS"
  case "$VRC" in
    0) say "landing eligibility: exact-head authority green for $ORIG" ;;
    3) abandon "exact-head authority reported a genuine red for PR head $ORIG" 4 ;;
    4) abandon "neither exact-head authority produced green for PR head $ORIG" 4 ;;
    *) abandon "could not evaluate exact-head validation authority (rc=$VRC)" 4 ;;
  esac
fi

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

# 4. The pushed exact head needs a fresh positive from the local ledger. A
# rebase that changed the SHA cannot inherit the old authorization.
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
if [ "$GATE_ROTATION" -eq 1 ]; then
  # The independent record already requires a qualifying counted local receipt,
  # but the branch-owned gate cannot authorize itself and no cache label is used
  # as a substitute. Re-run all nine live checks at the exact head instead.
  verify_rotation_authority "$HEAD" \
    || abandon "independent gate-rotation authority refused resolved head $HEAD" 4
else
  if [ "$producer_exception" -ne 1 ]; then
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
    # durable append-only sidecar before the /tmp log is evicted.
    python3 "$ROOT/ci-hub/validate/attribute_reds.py" --last 0 --persist >/dev/null 2>&1 || true
  fi
  VS=$("$SCRIPT_DIR/exact-head-validation-authority.sh" "${authority_mode[@]}" --repo "$R" --sha "$HEAD" 2>&1); VRC=$?
  say "post-push exact-head validation(head=$HEAD) rc=$VRC: $VS"
  [ "$VRC" -eq 0 ] || abandon "pushed head $HEAD has no accepted exact-head validation authority (rc=$VRC)" 4
  if [ "$producer_exception" -ne 1 ]; then
    "$ROOT/ci-hub/ci-hub" apply-local-label --pr "$PR" --repo "$R" \
      || abandon "ledger-guarded apply-local-label failed" 4
    sleep 4
    LB=$(with-proxy gh pr view "$PR" -R "$R" --json labels -q '[.labels[].name]|join(",")')
    grep -q locally-validated <<<"$LB" || abandon "locally-validated stripped immediately (labels=$LB)" 4
    say "ledger-derived label present; labels=$LB"
  fi
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

if [ "$GATE_ROTATION" -eq 1 ]; then
  verify_rotation_authority "$HEAD" \
    || abandon "independent gate-rotation authority refused at final current-main boundary" 5
  say "GATE-ROTATION-EVENT verify_final"
else
  receipt_comments=$(mktemp) \
    || abandon "could not allocate the receipt-comment observation file" 5
  if ! with-proxy gh api --paginate --slurp \
      "repos/$R/issues/$PR/comments?per_page=100" >"$receipt_comments"; then
    printf '[]\n' >"$receipt_comments"
  fi
  receipt_detail=$("$SCRIPT_DIR/exact-head-validation-authority.sh" \
    "${authority_mode[@]}" --repo "$R" --sha "$HEAD" --comments "$receipt_comments" \
    --current-base "$current_base" \
    --current-reverie-base "$current_reverie_base" \
    --repo-checkout "$WT" --reverie-checkout "$ROOT/reverie" 2>&1)
  receipt_rc=$?
  rm -f -- "$receipt_comments"
  if [ "$receipt_rc" -ne 0 ]; then
    abandon "exact-head validation authority REFUSED for $HEAD: ${receipt_detail:-no result}" 5
  fi
  say "exact-head validation authority authorized: $receipt_detail"
fi

# An exact-head approval cannot inherit semantic coverage from a source PR.
# A cherry-picked commit has a new base, neighbours, and stated intent even when
# its patch is byte-identical.  Require the PR Summary to disclose each explicit
# source trailer and require every relied-upon current approval lane to attest
# the COMPLETE source-to-semantics digest in THIS head context.  Run this after
# every other potentially slow boundary check because the PR body is mutable
# without moving HEAD.  This is a review/disclosure gate, not a claim that
# cherry-picked code is defective.
cherry_pick_detail=$(python3 "$SCRIPT_DIR/cherry_pick_context.py" \
  --repo "$R" --pr "$PR" --expected-head "$HEAD" --gate 2>&1)
cherry_pick_rc=$?
say "cherry-pick context authority(head=$HEAD) rc=$cherry_pick_rc: $cherry_pick_detail"
[ "$cherry_pick_rc" -eq 0 ] \
  || abandon "cherry-pick context contract REFUSED for $HEAD: ${cherry_pick_detail:-no result}" 5

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
[ "$GATE_ROTATION" -eq 1 ] && say "GATE-ROTATION-EVENT normal_non_admin_merge"

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
