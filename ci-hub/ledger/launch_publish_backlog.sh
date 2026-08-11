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
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)

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

if systemd-run --user --quiet --collect \
    --unit="$UNIT" \
    --description="drain ledger spool backlog across producing parents" \
    --property=CPUQuota=100% \
    -- python3 "$ROOT/ci-hub/ledger/publish_backlog.py" >/dev/null 2>&1; then
  echo "state=launched summary=ledger publish sweep launched detached as $UNIT"
  exit 0
fi

echo "state=refused summary=could not launch $UNIT; spool retained for the next tick"
exit 0
