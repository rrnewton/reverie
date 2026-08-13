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
# The completed detached result is handed to the NEXT invocation through one
# atomically-written status file. That invocation still launches the retry, but
# returns the completed publish status instead of treating unit acceptance as
# publication success. A report is thereby bound to the fact it claims: a
# failed publish is a failed tick, not a successful systemd-run call.
#
# One unit name means one sweep at a time on this box. A second launch while a
# sweep is running is refused by systemd, which is the desired behaviour: the
# spool is still there and the next tick relaunches.
set -uo pipefail

UNIT=${LEDGER_PUBLISH_UNIT:-ledger-publish-backlog}
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
STATUS_FILE=${LEDGER_PUBLISH_STATUS_FILE:-$ROOT/ignored/ci-hub/ledger-publisher-status.json}

previous_status=
previous_rc=0
if systemctl --user is-active --quiet "$UNIT.service" 2>/dev/null; then
  echo "state=running"
  echo "summary=ledger publish sweep already active as $UNIT"
  exit 0
fi

if [ -e "$STATUS_FILE" ]; then
  previous_status=$(python3 "$ROOT/ci-hub/ledger/publish_backlog.py" \
    --report-status "$STATUS_FILE")
  previous_rc=$?
fi

if ! command -v systemd-run >/dev/null 2>&1; then
  # No detached substrate: say so rather than running it inline on the tick
  # budget, which is the failure this wrapper exists to avoid.
  echo "state=unavailable"
  echo "summary=systemd-run unavailable; publish sweep not launched"
  exit 1
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "state=unavailable"
  echo "summary=gh credential helper is not on the launcher PATH; publish sweep not launched"
  exit 1
fi

if ! python3 "$ROOT/ci-hub/ledger/publish_backlog.py" \
    --mark-started "$STATUS_FILE" >/dev/null; then
  echo "state=failed"
  echo "summary=could not mark ledger publish sweep started; publish sweep not launched"
  exit 1
fi

if systemd-run --user --quiet --collect \
    --unit="$UNIT" \
    --description="drain ledger spool backlog across producing parents" \
    --property=CPUQuota=100% \
    --setenv=PATH="$PATH" \
    -- python3 "$ROOT/ci-hub/ledger/publish_backlog.py" \
      --status-file "$STATUS_FILE" >/dev/null 2>&1; then
  if [ "$previous_rc" -ne 0 ]; then
    printf '%s\n' "$previous_status"
    echo "retry=launched detached as $UNIT"
    exit "$previous_rc"
  fi
  echo "state=launched"
  echo "summary=ledger publish sweep launched detached as $UNIT"
  exit 0
fi

echo "state=refused"
echo "summary=could not launch $UNIT; spool retained for the next tick"
exit 1
