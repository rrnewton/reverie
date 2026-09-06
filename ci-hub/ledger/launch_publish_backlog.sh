#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

# Launch the ledger publish sweep DETACHED and return immediately.
#
# The tick may observe cheaply; it may not block on a mutation. tick-hub already
# proved that the hard way: `unpushed_parent_commits --rescue` shared the 30s
# gate budget with a 65-75s round trip, so the gate reported "could not run ...
# timed out" every cycle for a day and emitted no measurement at all while the
# backlog it existed to watch kept growing. Publishing is heavier still, so this
# wrapper costs the tick one systemd-run call and nothing else: it never waits.
# Publication success is reported by the next ledger_spool_backlog observation,
# which is the correct place for it. A FAILED detached unit is different: it is
# retained until the next launch, reported through this command's exit status,
# reset, and retried. Otherwise `--collect` erases the result and a tick whose
# publisher failed is reported as CLEAN.
#
# One unit name means one sweep at a time on this box. A second launch while a
# sweep is running is refused by systemd, which is the desired behaviour: the
# spool is still there and the next tick relaunches.
set -uo pipefail

UNIT=${LEDGER_PUBLISH_UNIT:-ledger-publish-backlog}
TOOL_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
STATE_ROOT=${DEV_HERMIT_PARENT:-$TOOL_ROOT}

# WHERE THE SWEEP'S OUTPUT GOES, AND WHY IT IS NOT THE JOURNAL.
#
# A `systemd-run --user` unit sends stdout/stderr to the journal by default. On
# this host that is a write-only destination for the account that needs it:
# `journalctl --user` answers "No journal files were opened due to insufficient
# permissions" for EVERY user unit, because the account is in none of `adm`,
# `systemd-journal` or `wheel`. So the sweep has been describing its own failure
# on every run and putting the description somewhere unreadable.
#
# That is what made the credential fault below cost a day: the error was not
# missing and not wrong, it was addressed to a reader who could not open it.
# Appending to a plain file costs nothing and makes the next fault legible.
LOG_DIR=${LEDGER_PUBLISH_LOG_DIR:-$STATE_ROOT/ignored/ci-hub}
LOG=$LOG_DIR/ledger-publish-backlog.log
mkdir -p "$LOG_DIR" 2>/dev/null || true

# WHY THE UNIT NEEDS AN EXPLICIT PATH, AND WHY ONLY WRITES FAILED.
#
# Publication pushes through `parent-main-write`, whose git credential helper
# shells out to `gh`. A `systemd --user` unit inherits the user manager's PATH,
# which on this host is `/home/newton/orc-bin:/usr/local/sbin:/usr/local/bin:
# /usr/sbin:/usr/bin` and does NOT contain `$HOME/bin`, where `gh` lives. An
# interactive shell picks it up from the profile, so the sweep succeeded
# whenever a human ran it and failed silently whenever the unit did -- a
# credential failure ON THE WRITE PATH ONLY, invisible to any read-only
# reachability probe, because fetch and ls-remote need no credential helper.
#
# Resolve it here, in the caller's environment, and hand the absolute directory
# to the unit. REFUSE LOUDLY IF gh IS ABSENT rather than launching a sweep that
# is guaranteed to fail at the push: a silent absence is the exact defect this
# comment exists to explain.
if [[ ${LEDGER_PUBLISH_GH_BIN+x} ]]; then
  GH_CANDIDATES=("$LEDGER_PUBLISH_GH_BIN")
else
  # The user manager's PATH is not the login-shell PATH. In particular, the
  # production host keeps gh in $HOME/bin while systemd --user omits that
  # directory. Resolve the known per-user locations explicitly before falling
  # back to PATH, then pass the selected directory to the transient unit.
  GH_CANDIDATES=(
    "$HOME/bin/gh"
    "${FLEET_REAL_GH:-}"
    "$(command -v gh 2>/dev/null || true)"
    "$HOME/.local/libexec/dev-hermit/gh"
  )
fi
GH_BIN=
for candidate in "${GH_CANDIDATES[@]}"; do
  if [[ -n $candidate && -f $candidate && -x $candidate ]]; then
    GH_BIN=$candidate
    break
  fi
done
if [[ -z $GH_BIN ]]; then
  echo "state=refused summary=gh not found in the configured or per-user locations; a detached sweep would fail at the push. Not launched; spool retained." \
    | tee -a "$LOG" 2>/dev/null || true
  exit 1
fi
UNIT_PATH="$(dirname -- "$GH_BIN"):${PATH}"

# A health tick executes this launcher from its verified private `.run.<sha>`
# tree. That tree belongs to the foreground tick: the wrapper empties it as soon
# as health-tick returns, while this unit deliberately continues detached. The
# first Python process can start before cleanup and still fail later when it
# launches publisher.py, parent-main-write, or check-series-rows.rs from a tree
# that no longer exists. Measured 2026-09-05: five complete series batches stayed
# spooled for 6,546.9 seconds; the logged refusal named the removed
# check-series-rows.rs path.
#
# Re-enter the exact cached parent revision through the installed operational
# tool. Its private root is then owned for the publisher's complete lifetime.
# Direct/manual launches have no such handoff and retain the stable checkout path
# they were invoked from. If a tick supplies only half of the handoff, refuse:
# falling back to its short-lived root would recreate the silent stall.
declare -a PUBLISH_COMMAND
PUBLISH_WORKING_DIRECTORY=$TOOL_ROOT
OPERATIONAL_TOOL=${DEV_HERMIT_OPERATIONAL_TOOL:-}
if [[ -z $OPERATIONAL_TOOL && -n ${DEV_HERMIT_TOOL_PARENT_SHA:-} ]]; then
  installed_operational_tool=$HOME/.local/libexec/dev-hermit/operational-tool
  if [[ ! -L $installed_operational_tool && -f $installed_operational_tool && \
        -x $installed_operational_tool ]]; then
    OPERATIONAL_TOOL=$installed_operational_tool
  fi
fi
if [[ -n $OPERATIONAL_TOOL || -n ${DEV_HERMIT_TOOL_PARENT_SHA:-} ]]; then
  if [[ -z $OPERATIONAL_TOOL || ! -x $OPERATIONAL_TOOL || \
        ! ${DEV_HERMIT_TOOL_PARENT_SHA:-} =~ ^[0-9a-f]{40}$ ]]; then
    echo "state=refused summary=incomplete operational-tool handoff; publish sweep not launched; spool retained." \
      | tee -a "$LOG" 2>/dev/null || true
    exit 1
  fi
  PUBLISH_COMMAND=(
    "$OPERATIONAL_TOOL"
    --run-current-tool
    --state-root "$STATE_ROOT"
  )
  if [[ -n ${DEV_HERMIT_TOOL_CACHE:-} ]]; then
    PUBLISH_COMMAND+=(--cache-root "$DEV_HERMIT_TOOL_CACHE")
  fi
  PUBLISH_COMMAND+=(
    --tool-parent-sha "$DEV_HERMIT_TOOL_PARENT_SHA"
    --tool-relative ci-hub/ledger/publish_backlog.py
    --
  )
  PUBLISH_WORKING_DIRECTORY=$STATE_ROOT
else
  PUBLISH_COMMAND=(python3 "$TOOL_ROOT/ci-hub/ledger/publish_backlog.py")
fi

if systemctl --user is-active --quiet "$UNIT.service" 2>/dev/null; then
  echo "state=running summary=ledger publish sweep is still active as $UNIT; no second sweep launched; spool retained"
  # The scheduled gate runs once per hour. Reaching the next invocation while
  # the previous one is still active is not a completed sweep, so do not report
  # it as CLEAN.
  exit 1
fi

# A collected transient unit vanishes as soon as it exits. `systemctl show`
# then describes a synthetic not-found unit whose Result=success and
# ExecMainStatus=0 are ABSENCE, not evidence that the command passed. Keep a
# failed unit loaded until this next sweep reads its real terminal result. Reset
# it only after capturing that result so the same unit name can be retried.
PRIOR_FAILURE=
if systemctl --user is-failed --quiet "$UNIT.service" 2>/dev/null; then
  UNIT_RESULT=$(systemctl --user show "$UNIT.service" --property=Result --value \
    2>/dev/null || true)
  UNIT_STATUS=$(systemctl --user show "$UNIT.service" --property=ExecMainStatus --value \
    2>/dev/null || true)
  PRIOR_FAILURE="previous $UNIT failed with Result=${UNIT_RESULT:-unknown} ExecMainStatus=${UNIT_STATUS:-unknown}"
  if ! reset_error=$(systemctl --user reset-failed "$UNIT.service" 2>&1); then
    echo "state=refused summary=$PRIOR_FAILURE; could not reset it for retry; spool retained; systemctl said: ${reset_error:-<no output>}" \
      | tee -a "$LOG" 2>/dev/null || true
    exit 1
  fi
fi

if ! command -v systemd-run >/dev/null 2>&1; then
  # No detached substrate: say so rather than running it inline on the tick
  # budget, which is the failure this wrapper exists to avoid.
  echo "state=unavailable summary=${PRIOR_FAILURE:+$PRIOR_FAILURE; }systemd-run unavailable; publish sweep not launched; spool retained"
  exit 1
fi

if launch_error=$(systemd-run --user --quiet \
    --unit="$UNIT" \
    --description="drain ledger spool backlog across producing parents" \
    --property=CPUQuota=100% \
    --property="StandardOutput=append:$LOG" \
    --property="StandardError=append:$LOG" \
    --working-directory="$PUBLISH_WORKING_DIRECTORY" \
    --setenv="PATH=$UNIT_PATH" \
    --setenv="DEV_HERMIT_PARENT=$STATE_ROOT" \
    --setenv="DEV_HERMIT_TOOL_ROOT=$TOOL_ROOT" \
    -- "${PUBLISH_COMMAND[@]}" 2>&1 >/dev/null); then
  if [[ -n $PRIOR_FAILURE ]]; then
    echo "state=relaunched summary=$PRIOR_FAILURE; retry launched detached as $UNIT; output appended to $LOG" \
      | tee -a "$LOG" 2>/dev/null || true
    # This invocation DID launch a retry, but it also observed a real failed
    # publication. Keep that failure visible to the tick instead of calling the
    # sweep clean merely because the retry was accepted.
    exit 1
  fi
  echo "state=launched summary=ledger publish sweep launched detached as $UNIT; output appended to $LOG"
  exit 0
fi

# Keep the launcher's OWN failure too. It was previously discarded by
# `>/dev/null 2>&1`, so a refused launch and a launch that ran and failed were
# indistinguishable from the outside.
echo "state=refused summary=could not launch $UNIT; spool retained for the next tick; systemd-run said: ${launch_error:-<no output>}" \
  | tee -a "$LOG" 2>/dev/null || true
exit 1
