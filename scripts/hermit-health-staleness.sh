#!/usr/bin/env bash
# EXTERNAL staleness alarm for the dev-hermit operational health tick.
#
# Design invariant: this alarm deliberately does NOT depend on any tick-hub code
# (self-reference trap — a monitor that shares fate with the thing it monitors
# fails silently together). It only stat()s two files. If the heartbeat is
# older than N tick intervals, THAT is the alert. Without this, the next
# silence is as invisible as the 38h gap that motivated it.
#
# WHAT WAKES THIS ALARM (the recursive question): an INDEPENDENT trigger —
# hermit-health-staleness.timer (systemd --user, OnUnitActiveSec=10min), which
# re-arms off its OWN last activation, NOT off the tick service. A stopped
# tick-hub therefore does NOT stop this watchdog; they do not share fate at the
# application layer. The only common substrate is the systemd --user manager
# itself (Linger=yes survives reboot, Persistent=true catches up missed runs).
#
# ---------------------------------------------------------------------------
# TWO FILES, TWO FACTS. Watching the wrong one makes the alarm useless.
#
#   HEARTBEAT  = ~/.local/state/hermit-health-tick-invocations.log
#       Appended on EVERY tick by hermit-health-tick.sh, regardless of the
#       tick's rc. This is the "is the monitor ALIVE / did the timer FIRE"
#       signal. It going FLAT is the exact 38h-silence failure mode. THIS is
#       the file whose staleness must page loudly.
#
#   PRODUCTIVE = ~/work/dev-hermit/.tick-hub/fired-state
#       mtime bumps ONLY on a productive rc=0 tick. It freezes for a LEGITIMATE
#       external reason (the nested agent-utils pin in Hermit gates
#       health-tick to rc=1) while the monitor is perfectly alive. Paging on
#       THIS alone screams forever and gets muted — as useless as never firing.
#       So a frozen PRODUCTIVE signal, when the HEARTBEAT is fresh, is
#       classified, named, and (for the
#       known pin-drift gate) NOT paged as a dead timer.
# ---------------------------------------------------------------------------
# WHAT THIS ALARM CANNOT SEE (stated plainly, not left implied).
#   This alarm verifies TWO things only, both by file mtime: (1) that the timer
#   FIRED (heartbeat freshness = liveness) and (2) that a tick did PRODUCTIVE
#   work (fired-state freshness). It does NOT verify that a firing tick's
#   INTERNAL health checks actually RAN and MEANT anything. A tick that fires on
#   schedule and returns rc=0 while its checks silently no-op — accepted,
#   running, verifying nothing (the `--cgroups` shape) — would advance BOTH
#   signals and read here as HEALTHY. Closing that gap requires the tick to emit
#   a POSITIVE, counted receipt (e.g. "polled main @SHA, N checks executed") and
#   this alarm to assert on that count, not merely on freshness. Until then:
#   this alarm proves the monitor is ALIVE, NOT that it is doing correct work.
# ---------------------------------------------------------------------------
set -uo pipefail

# $HOME, not a literal: this file is version-controlled and the parent's
# portability gate rejects owner-specific paths. A shell DOES expand $HOME
# (unlike a systemd unit, which needs %h).
ROOT="${HOME:?HOME must be set}/work/dev-hermit"
# Paths are env-overridable ONLY to allow hermetic fixture testing (bracket both
# sides without touching live artifacts). Production leaves them all unset.
HEARTBEAT="${HERMIT_HEALTH_HEARTBEAT:-$HOME/.local/state/hermit-health-tick-invocations.log}"
PRODUCTIVE="${HERMIT_HEALTH_FIRED_STATE:-$ROOT/.tick-hub/fired-state}"
LOG="${HERMIT_HEALTH_LOG:-$HOME/.local/state/hermit-health-staleness.log}"
COOLDOWN_STAMP="${HERMIT_HEALTH_COOLDOWN_STAMP:-$HOME/.local/state/hermit-health-staleness.last-alarm}"
GATED_STAMP="${HERMIT_HEALTH_GATED_STAMP:-$HOME/.local/state/hermit-health-staleness.last-gated-nag}"
ORC_MSG="${HERMIT_HEALTH_ORC_MSG:-$ROOT/scripts/orc-hermit-msg.py}"
# The second escalation, and the one that does not need Orc. $ORC_MSG delivers by
# typing into the coordinator's TUI pane; measured 2026-09-03 across 1656 recorded
# attempts, 1032 failed, the last success was 2026-08-25 and the 202 attempts
# after it all failed. This alarm's whole subject is that monitoring on this host
# has stopped, so a channel that needs a healthy coordinator on this host is the
# wrong one to depend on alone.
ESCALATE="${HERMIT_HEALTH_ESCALATE:-$ROOT/ci-hub/bin/escalate}"
ESCALATION_KEY="${HERMIT_HEALTH_ESCALATION_KEY:-health-tick-staleness}"
ESCALATION_AGENT="${HERMIT_HEALTH_ESCALATION_AGENT:-health-tick}"
ESCALATION_TEAM="${HERMIT_HEALTH_ESCALATION_TEAM:-hermit2}"
# Both sinks. The fork is off this host and covers the coordinator being ABSENT,
# which is the case this alarm most often reports. The work queue gets the alarm
# DISPATCHED rather than needing to be noticed, which covers the coordinator
# being present and not looking. Neither covers the other.
ESCALATION_SINKS="${HERMIT_HEALTH_ESCALATION_SINKS:---sink github --sink taskgraph}"
# Named, never inherited: a scheduled service resolves a different database from
# the one coordinators read.
ESCALATION_TG_DB="${HERMIT_HEALTH_ESCALATION_TG_DB:-hermit2}"

# SYSTEMD IS THE AUTHORITY FOR "DID THE TIMER FIRE", AND THE HEARTBEAT FILE IS
# NOT. Measured 2026-08-07: hermit-health-tick.sh captures its timestamp at tick
# START but appends the line at tick END, so the invocation log's mtime marks
# when a tick last COMPLETED. A live tick observed on this box started 03:25:58Z
# and completed 03:27:23Z -- an 85s append lag, with the service seen running
# across 42 consecutive polls. During that whole window the mtime still pointed
# at the PREVIOUS tick and aged without bound while the timer was firing
# perfectly. That divergence produced a false "TIMER DEAD" at 02:54:14Z claiming
# 960s when the true last-invocation age was 206s.
# So mtime answers "a tick last FINISHED", not "the timer is firing", and only
# the timer's own LastTriggerUSec answers the latter. All three are read.
# Overridable for the same hermetic-fixture reason as the paths above.
SYSTEMCTL="${HERMIT_HEALTH_SYSTEMCTL:-systemctl}"
TIMER_UNIT="${HERMIT_HEALTH_TIMER_UNIT:-hermit-health-tick.timer}"
SERVICE_UNIT="${HERMIT_HEALTH_SERVICE_UNIT:-hermit-health-tick.service}"

# THRESHOLD IS DERIVED, NOT PICKED. 90 min = 3 x the owner-directed 30-min
# interval. INTERVAL_MIN=30 matches tick_frequency_min,
# OPERATIONAL_TICK_INTERVAL_MS, and hermit-health-tick.timer.
#   N=3 tolerates up to 2 consecutive MISSED ticks (one slow tick under this
#   box's load is not a fault); the 3rd missed interval trips it.
INTERVAL_MIN=30
N=3
THRESH=$(( INTERVAL_MIN * N * 60 ))   # 90 min = 3 x 30-min interval
COOLDOWN=$(( 60 * 60 ))               # loud pages: at most once per hour
GATED_NAG=$(( 24 * 60 * 60 ))         # known-gated nag: at most once per day

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }
now="$(date +%s)"

# ---- OBSERVATION HELPERS -----------------------------------------------------
# Each reads the live source at the moment it is called. Nothing is cached at
# script start, because the whole defect this fixes was acting on a value that
# had gone stale between observation and decision.

# When the TIMER last fired, as epoch seconds; 0 when unknown/never. This is the
# authoritative answer to "is the schedule alive" -- it is true the instant the
# timer fires, independent of how long the resulting tick then runs.
timer_last_trigger_epoch() {
    local raw
    raw="$("$SYSTEMCTL" --user show "$TIMER_UNIT" -p LastTriggerUSec --value 2>/dev/null)"
    # systemd prints an empty value for a timer that has never fired.
    [ -n "$raw" ] || { echo 0; return; }
    date -d "$raw" +%s 2>/dev/null || echo 0
}

# Whether a tick is executing RIGHT NOW. `activating` is included deliberately:
# a Type=oneshot service sits in `activating/start` for its entire run, so
# omitting it would miss exactly the long-running case this exists to catch.
service_is_running() {
    local state
    state="$("$SYSTEMCTL" --user is-active "$SERVICE_UNIT" 2>/dev/null)"
    case "$state" in
        active|activating|reloading) return 0 ;;
        *) return 1 ;;
    esac
}

service_state_text() {
    "$SYSTEMCTL" --user is-active "$SERVICE_UNIT" 2>/dev/null || echo unknown
}

# The last INVOCATION RECORD, not merely the last line. The heartbeat file also
# carries `<stamp> finding=...` lines (the untruncated ACTION/NOTE text that
# hermit-health-tick.sh records; see its comment on why that exists). Those are
# stamped identically but carry no `rc=`, so a bare `tail -1` can land on one
# and silently stop matching the classification patterns below -- turning a
# KNOWN, named gate into "WORK-STALLED (unexpected)". Select by shape instead.
last_invocation_record() {
    grep -E '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z rc=' \
        "$HEARTBEAT" 2>/dev/null | tail -n 1 || true
}

# START time of the most recent invocation LINE, as epoch seconds; 0 if none.
# Distinct from the file mtime: the line is stamped when the tick began and
# written when it ended, so this is "when the last COMPLETED tick started".
last_invocation_start_epoch() {
    local line stamp
    line="$(last_invocation_record)"
    stamp="$(printf '%s' "$line" | grep -oE '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z' || true)"
    [ -n "$stamp" ] || { echo 0; return; }
    date -d "$stamp" +%s 2>/dev/null || echo 0
}

# One line carrying every fact the verdict rests on, so a reader never has to
# re-derive the premise -- and so a stale claim is self-evident from the record.
observation_line() {
    local observed_at=$1 hb_mt=$2 inv=$3 trig=$4 svc=$5
    printf 'observed_at=%s cadence_s=%s threshold_s=%s last_completion=%s last_invocation_start=%s timer_last_trigger=%s service=%s' \
        "$(date -u -d "@$observed_at" +%Y-%m-%dT%H:%M:%SZ)" \
        "$(( INTERVAL_MIN * 60 ))" "$THRESH" \
        "$( [ "$hb_mt" -gt 0 ] && date -u -d "@$hb_mt" +%Y-%m-%dT%H:%M:%SZ || echo none )" \
        "$( [ "$inv" -gt 0 ] && date -u -d "@$inv" +%Y-%m-%dT%H:%M:%SZ || echo none )" \
        "$( [ "$trig" -gt 0 ] && date -u -d "@$trig" +%Y-%m-%dT%H:%M:%SZ || echo none )" \
        "$svc"
}

# page <cooldown_stamp> <cooldown_secs> <message>
# Logs the message durably, then escalates to the coordinator at most once per
# cooldown window (loud, not spam).
page() {
    local stamp="$1" cd="$2" msg="$3" last=0
    echo "$(ts) $msg" >>"$LOG"
    [ -f "$stamp" ] && last="$(cat "$stamp" 2>/dev/null || echo 0)"
    if [ $(( now - last )) -ge "$cd" ]; then
        local tmp rc=0; tmp="$(mktemp)"
        printf '%s\n' "$msg" >"$tmp"
        "$ORC_MSG" --message-file "$tmp" >>"$HOME/.local/state/orc-hermit-msg.log" 2>&1 || rc=$?
        rm -f "$tmp"
        echo "$now" >"$stamp"
        # "pushed" and "tried-but-failed-to-push" must NOT render the same — a
        # durable log that reads "alerted" when nobody was reached is the same
        # silence-read-as-success defect this alarm exists to kill.
        if [ "$rc" -eq 0 ]; then
            echo "$(ts) alerted-coordinator (push OK)" >>"$LOG"
        else
            echo "$(ts) escalation-PUSH-FAILED rc=$rc — alarm is durably logged HERE but the coordinator was NOT reached (push channel down; see orc-hermit-msg.log). Read this log directly; do NOT assume notification landed." >>"$LOG"
        fi
        # BOTH ROUTES ARE TRIED, ALWAYS, and neither one's failure hides the
        # other's outcome. The Orc route is faster when the coordinator is up;
        # this one works when it is not. Both are reported by name, because a
        # log line that says "escalated" without saying WHERE is the same
        # ambiguity the line above was written to remove.
        if [ -x "$ESCALATE" ]; then
            local out2 rc2=0 tmp2; tmp2="$(mktemp)"
            printf '%s\n' "$msg" >"$tmp2"
            # Through with-proxy: this runs from a scheduled service, which
            # inherits no proxy variables, and the host has no direct external
            # route. Without it the publication fails with "network is
            # unreachable" and the alarm goes no further than this file.
            # shellcheck disable=SC2086 -- ESCALATION_SINKS is a flag list
            out2="$(DG_AGENT_NAME="$ESCALATION_AGENT" with-proxy "$ESCALATE" \
                $ESCALATION_SINKS --tg-db "$ESCALATION_TG_DB" \
                --key "$ESCALATION_KEY" \
                --title "operational health tick is not reporting on $(uname -n)" \
                --team "$ESCALATION_TEAM" --role relay --message-file "$tmp2" 2>&1)" || rc2=$?
            rm -f "$tmp2"
            if [ "$rc2" -eq 0 ]; then
                echo "$(ts) escalation-published rc=0 $(printf '%s' "$out2" | tr '\n' ' ')" >>"$LOG"
            else
                echo "$(ts) escalation-PUBLISH-FAILED rc=$rc2 — BOTH routes are now down; the alarm exists only in this file and in the escalation log. $(printf '%s' "$out2" | tr '\n' ' ')" >>"$LOG"
            fi
        else
            echo "$(ts) escalation-UNAVAILABLE — $ESCALATE is missing or not executable, so the Orc route above was the only one tried." >>"$LOG"
        fi
    else
        echo "$(ts) suppressed-by-cooldown" >>"$LOG"
    fi
}

hb_mt="$(stat -c %Y "$HEARTBEAT" 2>/dev/null || echo 0)"
pr_mt="$(stat -c %Y "$PRODUCTIVE" 2>/dev/null || echo 0)"

# ---- PRIMARY: is the monitor ALIVE? (the real 38h-silence detector) ---------
if [ "$hb_mt" -eq 0 ]; then
    page "$COOLDOWN_STAMP" "$COOLDOWN" \
        "ALARM: TIMER DEAD — heartbeat log MISSING ($HEARTBEAT). No tick has EVER been observed: tick-hub state is UNKNOWN, NOT healthy (absence of a tick is not proof of health — UNKNOWN and HEALTHY are different). This alarm could not verify the monitor is alive. The operational health tick is not running at all (the 38h-silence failure mode). Diagnose: systemctl --user status hermit-health-tick.timer hermit-health-tick.service."
    exit 0
fi
hb_age=$(( now - hb_mt ))
if [ "$hb_age" -gt "$THRESH" ]; then
    # RE-READ BEFORE ALARMING. Everything below is observed FRESH, not reused
    # from the top of the script: a tick can complete between the first stat and
    # this point, and alarming on a premise that has since gone false is exactly
    # the 02:54:14Z false "TIMER DEAD". Re-deriving costs three cheap reads.
    now="$(date +%s)"
    hb_mt="$(stat -c %Y "$HEARTBEAT" 2>/dev/null || echo 0)"
    hb_age=$(( now - hb_mt ))
    inv_start="$(last_invocation_start_epoch)"
    trig="$(timer_last_trigger_epoch)"
    svc="$(service_state_text)"
    obs="$(observation_line "$now" "$hb_mt" "$inv_start" "$trig" "$svc")"

    # (1) PREMISE WITHDRAWN. A tick completed while we were deciding.
    if [ "$hb_age" -le "$THRESH" ]; then
        echo "$(ts) SELF-CORRECTED (premise withdrawn on re-read): heartbeat is now ${hb_age}s old, within ${THRESH}s. An alarm computed moments earlier would have been false. $obs" >>"$LOG"
        exit 0
    fi

    hb_min=$(( hb_age / 60 ))
    trig_age=$(( now - trig ))

    # (2) A TICK IS RUNNING RIGHT NOW. Typed separately, and deliberately NOT as
    # "TIMER DEAD" -- the schedule is provably alive; one tick is merely slow.
    # This is the measured case: ticks stamp at START and append at END, so a
    # long tick makes the heartbeat age without bound while everything is fine.
    if service_is_running; then
        page "$GATED_STAMP" "$GATED_NAG" \
            "TICK-RUNNING-LONG (NOT a dead timer): heartbeat is ${hb_min}min stale but ${SERVICE_UNIT} is ${svc} RIGHT NOW, so a tick is executing and has not yet appended its line. The invocation log records tick COMPLETION, not tick start, so an in-flight tick necessarily ages it. The schedule is ALIVE. If this repeats, the tick is too slow for its ${INTERVAL_MIN}-min cadence -- investigate tick duration, not timer liveness. $obs"
        exit 0
    fi

    # (3) THE TIMER IS FIRING BUT TICKS ARE NOT COMPLETING. A genuinely different
    # fault from a dead timer, and it would be misdiagnosed for hours if both
    # rendered as "TIMER DEAD": here the schedule works and the WORK is failing
    # (killed by TimeoutStartSec, crashing before its append, etc.).
    if [ "$trig" -gt 0 ] && [ "$trig_age" -le "$THRESH" ]; then
        page "$COOLDOWN_STAMP" "$COOLDOWN" \
            "ALARM: TICKS NOT COMPLETING (timer is ALIVE): ${TIMER_UNIT} last fired ${trig_age}s ago, within ${THRESH}s, but no invocation line has been appended for ${hb_min}min and ${SERVICE_UNIT} is ${svc}. The schedule is firing and the WORK is failing -- a tick is dying before it can record itself (TimeoutStartSec kill, crash, or a write failure). This is NOT the dead-timer failure mode; do not go looking at the timer. Diagnose: systemctl --user status ${SERVICE_UNIT}; journalctl --user -u ${SERVICE_UNIT} -n 50. $obs"
        exit 0
    fi

    # (4) GENUINELY DEAD: heartbeat stale, no tick running, and the timer itself
    # has not fired within the threshold (or has never fired at all).
    page "$COOLDOWN_STAMP" "$COOLDOWN" \
        "ALARM: TIMER DEAD — no tick observed in ${hb_min}min (>${THRESH}s = ${N} x measured ${INTERVAL_MIN}-min interval), no tick running, and ${TIMER_UNIT} last fired $( [ "$trig" -gt 0 ] && echo "${trig_age}s ago" || echo "NEVER" ). tick-hub state is UNKNOWN, NOT healthy — this alarm verifies liveness only and just lost it; a stale heartbeat is not a clean bill of health. The operational health monitor has STOPPED FIRING (the 38h-silence failure mode). Diagnose: systemctl --user status ${TIMER_UNIT} ${SERVICE_UNIT}; tail $HEARTBEAT. $obs"
    exit 0
fi

# ---- Monitor is alive. Is it doing PRODUCTIVE work? -------------------------
pr_age=$(( now - pr_mt ))
if [ "$pr_mt" -ne 0 ] && [ "$pr_age" -le "$THRESH" ]; then
    echo "$(ts) OK heartbeat_age=${hb_age}s productive_age=${pr_age}s (both <${THRESH}s)" >>"$LOG"
    exit 0
fi

# Heartbeat fresh but productive signal stale: work is gated, monitor is fine.
# Classify WHY from the last heartbeat line so a KNOWN, external gate is named
# and NOT paged as a dead timer.
pr_min=$(( pr_age / 60 ))
last_line="$(last_invocation_record)"
if printf '%s' "$last_line" | grep -q 'rc=1 advanced=no' \
   && printf '%s' "$last_line" | grep -qi 'agent-utils.*dirty'; then
    # KNOWN pin-drift gate — EXCLUDED BY NAME. Not a stopped monitor; the tick
    # fires every ~${INTERVAL_MIN}min but self-gates on another agent's dirty
    # checkout reached through the parent agent-utils symlink. Nag at most once
    # per day so it is not forgotten, but never the loud hourly "timer dead"
    # page.
    page "$GATED_STAMP" "$GATED_NAG" \
        "WORK-GATED (agent-utils pin drift): tick heartbeat is FRESH (last ${hb_age}s ago, timer healthy) but productive fired-state is ${pr_min}min stale because health-tick self-gates at the agent-utils pin guard (rc=1 advanced=no). NOT a stopped monitor. Resolve by reconciling the checkout reached through the parent agent-utils symlink to the nested pin in Hermit (owner of the dirty checkout). Last heartbeat: ${last_line}"
    exit 0
fi

# Heartbeat fresh, productive stale, reason UNRECOGNIZED — a new gate worth
# surfacing (lower urgency than timer-dead, but do not swallow it).
page "$COOLDOWN_STAMP" "$COOLDOWN" \
    "ALARM: WORK-STALLED (unexpected) — tick heartbeat FRESH (last ${hb_age}s ago) but productive fired-state ${pr_min}min stale for an UNRECOGNIZED reason (not the known agent-utils pin gate). Investigate: tail $HEARTBEAT. Last heartbeat: ${last_line}"
exit 0
