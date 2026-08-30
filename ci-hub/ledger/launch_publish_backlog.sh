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
# wrapper costs the tick one systemd-run call and nothing else: it never waits,
# and its exit code reports only whether the unit was ACCEPTED, never whether
# publication succeeded. Publication success is reported by the next
# ledger_spool_backlog observation, which is the correct place for it.
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
  GH_BIN=$LEDGER_PUBLISH_GH_BIN
else
  GH_BIN=$(command -v gh 2>/dev/null || true)
fi
if [[ -z $GH_BIN ]]; then
  echo "state=refused summary=gh not found on PATH; a detached sweep would fail at the push. Not launched; spool retained." \
    | tee -a "$LOG" 2>/dev/null || true
  exit 0
fi
UNIT_PATH="$(dirname -- "$GH_BIN"):${PATH}"

if systemctl --user is-active --quiet "$UNIT.service" 2>/dev/null; then
  echo "state=running summary=ledger publish sweep already active as $UNIT"
  exit 0
fi

if ! command -v systemd-run >/dev/null 2>&1; then
  # No detached substrate: say so rather than running it inline on the tick
  # budget, which is the failure this wrapper exists to avoid.
  echo "state=unavailable summary=systemd-run unavailable; publish sweep not launched"
  exit 0
fi

if launch_error=$(systemd-run --user --quiet --collect \
    --unit="$UNIT" \
    --description="drain ledger spool backlog across producing parents" \
    --property=CPUQuota=100% \
    --property="StandardOutput=append:$LOG" \
    --property="StandardError=append:$LOG" \
    --working-directory="$TOOL_ROOT" \
    --setenv="PATH=$UNIT_PATH" \
    --setenv="DEV_HERMIT_PARENT=$STATE_ROOT" \
    --setenv="DEV_HERMIT_TOOL_ROOT=$TOOL_ROOT" \
    -- python3 "$TOOL_ROOT/ci-hub/ledger/publish_backlog.py" 2>&1 >/dev/null); then
  echo "state=launched summary=ledger publish sweep launched detached as $UNIT; output appended to $LOG"
  exit 0
fi

# Keep the launcher's OWN failure too. It was previously discarded by
# `>/dev/null 2>&1`, so a refused launch and a launch that ran and failed were
# indistinguishable from the outside.
echo "state=refused summary=could not launch $UNIT; spool retained for the next tick; systemd-run said: ${launch_error:-<no output>}" \
  | tee -a "$LOG" 2>/dev/null || true
exit 0
