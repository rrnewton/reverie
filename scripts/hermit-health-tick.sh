#!/usr/bin/env bash
# Durable driver for the dev-hermit operational health tick.
#
# ROOT CAUSE this replaces: the operational-health poll was previously driven by
# an ORC-session-bound wf.loop (operationalHealthHeartbeat in
# .orc/plugins/hermit-dev/index.ts). Session-runtime state is NOT a durable
# schedule: when the hosting coordinator session was recycled (~2026-08-03
# 08:53 PDT) the loop stopped and the replacement session restored only the
# stale reminder-only hermit-dev-pr-health spec, which never calls health-tick.
# fired-state froze for ~38h and the silence looked exactly like health.
#
# This script is invoked by a systemd --user timer (Linger=yes) so it survives
# logout, reboot, and session recycling.
set -uo pipefail

# $HOME, not a literal: this file is version-controlled and the parent's
# portability gate rejects owner-specific paths. A shell DOES expand $HOME
# (unlike a systemd unit, which needs %h).
ROOT="${HOME:?HOME must be set}/work/dev-hermit"
FIRED_STATE="$ROOT/.tick-hub/fired-state"
INVLOG="$HOME/.local/state/hermit-health-tick-invocations.log"

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

start="$(ts)"
mt_before="$(stat -c %Y "$FIRED_STATE" 2>/dev/null || echo 0)"

if ! cd "$ROOT" 2>/dev/null; then
    printf '%s rc=126 event=cd-failed root=%s\n' "$start" "$ROOT" >>"$INVLOG"
    exit 126
fi

# health-tick self-gates when the checkout reached through the parent
# agent-utils symlink drifts from the nested pin in Hermit (exits non-zero
# without advancing fired-state); that is a real health signal, recorded here.
out="$(with-proxy ./ci-hub/bin/health-tick --flush --no-header 2>&1)"
rc=$?

mt_after="$(stat -c %Y "$FIRED_STATE" 2>/dev/null || echo 0)"
advanced=no
[ "$mt_after" -gt "$mt_before" ] && advanced=yes

# Append-only invocation record so the next investigation has a HISTORY, not a
# single mtime. (fired-state itself is gitignored and keeps no per-run history.)
summary="$(printf '%s' "$out" | tr '\n' ' ' | sed 's/  */ /g' | cut -c1-240)"
printf '%s rc=%s advanced=%s fired_state_mtime=%s summary=%q\n' \
    "$start" "$rc" "$advanced" "$mt_after" "$summary" >>"$INVLOG"

exit "$rc"
