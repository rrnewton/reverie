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

# THE FINDINGS THEMSELVES, IN FULL, UNTRUNCATED.
#
# WHY THIS EXISTS. The `summary=` record below is a 240-character window onto
# the whole tick, and the tick's own boilerplate is wider than the window: the
# tool-cost banner is 159 characters and the "fired-state persisted to <abs
# path>" line is another 85, so 244 characters are spent before the first
# character of any gate's output. Measured 2026-08-24: across 5415 recorded
# invocations, the number containing the string "SLOT CAP BREACH" was ZERO, and
# the number containing the word "worktree" was ZERO -- while the slot gate was
# firing "ACTIVE SLOT CAP BREACH: 77 active slot(s) against cap 12" every 900
# seconds. Nothing else retained the text either: `out` captures stdout AND
# stderr, so the service journal saw none of it. The detector worked; its
# findings were destroyed before any human or agent could read them.
#
# ACTION: and NOTE: are the two line kinds tick-hub emits as findings (see
# agent-utils/rs/tick-hub/src/emit.rs). Each is recorded verbatim on its own
# line, tagged with the same start timestamp so a reader can join it to the
# invocation record. A tick that finds nothing writes nothing here, so this
# stays a log of findings rather than a transcript.
#
# NOT `%q`. The summary field is shell-quoted, which renders "SLOT CAP BREACH"
# as "SLOT\ CAP\ BREACH" and defeats a plain grep for the phrase -- recorded but
# still unfindable, which is the same defect one layer down. Findings are single
# lines by construction (they are produced by `read -r` over the tick's output),
# so line integrity does not need quoting and greppability is worth more.
#
# FINDINGS ARE WRITTEN BEFORE THE INVOCATION RECORD, DELIBERATELY.
# scripts/hermit-health-staleness.sh reads this file with `tail -1` and expects
# an invocation record there; a findings line last would make its known
# agent-utils-pin-drift branch stop matching and downgrade a correctly
# classified state into "WORK-STALLED (unexpected)". That reader is also
# hardened to select the last invocation record explicitly, but the ordering
# here keeps the file correct for any reader that has not been.
printf '%s' "$out" | while IFS= read -r finding || [ -n "$finding" ]; do
    case "$finding" in
        ACTION:*|NOTE:*) printf '%s finding=%s\n' "$start" "$finding" >>"$INVLOG" ;;
    esac
done

# Append-only invocation record so the next investigation has a HISTORY, not a
# single mtime. (fired-state itself is gitignored and keeps no per-run history.)
summary="$(printf '%s' "$out" | tr '\n' ' ' | sed 's/  */ /g' | cut -c1-240)"
printf '%s rc=%s advanced=%s fired_state_mtime=%s summary=%q\n' \
    "$start" "$rc" "$advanced" "$mt_after" "$summary" >>"$INVLOG"

exit "$rc"
