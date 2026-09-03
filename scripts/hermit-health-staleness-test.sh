#!/usr/bin/env bash
# Fixture tests for the external health-timer staleness alarm.
#
# THE DEFECT THESE PIN. On 2026-08-07T02:54:14Z the alarm paged
# "TIMER DEAD -- no tick observed in 16min" while the durable invocation log
# showed a successful tick 206s earlier. The alarm was neither delayed nor
# mis-delivered; it read a signal that LAGS. hermit-health-tick.sh stamps its
# log line at tick START but appends it at tick END, so the invocation log's
# MTIME marks when a tick last COMPLETED. Measured live on this box: a tick
# started 03:25:58Z and completed 03:27:23Z -- an 85s append lag with the
# service running throughout. While a tick is in flight the mtime necessarily
# ages, so mtime alone cannot tell a DEAD timer from a SLOW tick.
#
# Every case below runs the REAL script against hermetic fixtures via its
# env-override hooks; nothing touches live state, and no alert is ever pushed.
#
# Run: scripts/hermit-health-staleness-test.sh
set -uo pipefail

SCRIPT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/hermit-health-staleness.sh"
PASS=0
declare -a FAILURES=()

fail() { FAILURES+=("$1${2:+  -- $2}"); }
ok()   { PASS=$(( PASS + 1 )); }
check() { if [ "$1" = "yes" ]; then ok; else fail "$2" "${3:-}"; fi; }

# A fake `systemctl` whose answers are scripted per case. It handles exactly the
# two queries the alarm makes, and fails loudly on anything else so a future
# query cannot silently read as "absent".
make_systemctl() { # <dir> <last-trigger-date-or-empty> <is-active-state>
    local dir=$1 trigger=$2 state=$3
    cat > "$dir/systemctl" <<EOF
#!/usr/bin/env bash
for a in "\$@"; do
  case "\$a" in
    LastTriggerUSec) printf '%s\n' '$trigger'; exit 0 ;;
    is-active)       printf '%s\n' '$state';   exit 0 ;;
  esac
done
echo "fake systemctl: unhandled query: \$*" >&2
exit 64
EOF
    chmod +x "$dir/systemctl"
}

# Run the alarm against a fixture. Echoes the alarm log it produced.
#   run_case <heartbeat-age-s|missing> <productive-age-s> <trigger-date|""> <state> [heartbeat-last-line] [stat-shim]
run_case() {
    local hb_age=$1 pr_age=$2 trigger=$3 state=$4 hb_line=${5:-} stat_shim=${6:-}
    local d; d="$(mktemp -d)"
    mkdir -p "$d/bin"
    make_systemctl "$d/bin" "$trigger" "$state"
    : > "$d/orc-msg"; chmod +x "$d/orc-msg"   # stub: never pushes anywhere
    printf '#!/usr/bin/env bash\nexit %s\n' "${CASE_ORC_RC:-0}" > "$d/orc-msg"
    chmod +x "$d/orc-msg"

    # The second escalation route, stubbed the same way: it records the exact
    # bytes it was handed and exits with the scripted status. CASE_ESCALATE
    # selects which of the three shapes this case exercises -- present and
    # working, present and failing, or absent entirely -- because the alarm log
    # must read differently in all three and the difference is the whole point.
    case "${CASE_ESCALATE:-ok}" in
        ok)
            printf '#!/usr/bin/env bash\nwhile [ $# -gt 0 ]; do [ "$1" = --message-file ] && cp "$2" "%s"; shift; done\necho "escalate: outcome=published key=k issue=https://example.invalid/1"\nexit 0\n' \
                "$d/escalated" > "$d/escalate"
            chmod +x "$d/escalate"
            ;;
        fail)
            printf '#!/usr/bin/env bash\necho "escalate: outcome=recorded-locally-only key=k error=fork unreachable" >&2\nexit 1\n' > "$d/escalate"
            chmod +x "$d/escalate"
            ;;
        absent)
            rm -f "$d/escalate"
            ;;
    esac

    local now; now="$(date +%s)"
    if [ "$hb_age" != "missing" ]; then
        printf '%s\n' "${hb_line:-$(date -u -d "@$(( now - hb_age ))" +%Y-%m-%dT%H:%M:%SZ) rc=0 advanced=yes}" > "$d/heartbeat"
        touch -d "@$(( now - hb_age ))" "$d/heartbeat"
    fi
    printf 'x\n' > "$d/fired"; touch -d "@$(( now - pr_age ))" "$d/fired"

    [ -n "$stat_shim" ] && { cp "$stat_shim" "$d/bin/stat"; chmod +x "$d/bin/stat"; }

    PATH="$d/bin:$PATH" \
    HERMIT_HEALTH_HEARTBEAT="$d/heartbeat" \
    HERMIT_HEALTH_FIRED_STATE="$d/fired" \
    HERMIT_HEALTH_LOG="$d/alarm.log" \
    HERMIT_HEALTH_COOLDOWN_STAMP="$d/cool" \
    HERMIT_HEALTH_GATED_STAMP="$d/gated" \
    HERMIT_HEALTH_ORC_MSG="$d/orc-msg" \
    HERMIT_HEALTH_ESCALATE="$d/escalate" \
    HERMIT_HEALTH_SYSTEMCTL=systemctl \
    HB_STAT_COUNTER="$d/hbstat" \
        bash "$SCRIPT" >/dev/null 2>&1
    cat "$d/alarm.log" 2>/dev/null
    [ -n "${CASE_ESCALATED_OUT:-}" ] && cp -f "$d/escalated" "$CASE_ESCALATED_OUT" 2>/dev/null
    rm -rf "$d"
}

echo "== health-timer staleness alarm: fixture tests =="

# --- 1. Healthy steady state stays quiet -------------------------------------
out="$(run_case 60 60 "$(date -d @$(( $(date +%s) - 60 )))" inactive)"
check "$([[ $out == *"OK heartbeat_age="* ]] && echo yes || echo no)" \
      "healthy: logs OK" "got: $out"
check "$([[ $out != *ALARM* ]] && echo yes || echo no)" "healthy: no ALARM"

# The wider threshold is intentional: 179 minutes is still fewer than three
# missed hourly ticks and must not page as a dead timer.
near_threshold_age=10740
near="$(date -d @$(( $(date +%s) - near_threshold_age )))"
out="$(run_case "$near_threshold_age" "$near_threshold_age" "$near" inactive)"
check "$([[ $out != *ALARM* ]] && echo yes || echo no)" \
      "below three missed hourly ticks: no ALARM" "got: $out"

# --- 2. GENUINE dead timer still fires (the >3x cadence gap must survive) ----
# Heartbeat 181 min stale, nothing running, timer itself last fired 181 min ago.
stale_age=10860
old="$(date -d @$(( $(date +%s) - stale_age )))"
out="$(run_case "$stale_age" "$stale_age" "$old" inactive)"
check "$([[ $out == *"ALARM: TIMER DEAD"* ]] && echo yes || echo no)" \
      "genuine gap: still pages TIMER DEAD" "got: $out"
check "$([[ $out == *"last fired 9"*"s ago"* || $out == *"last fired"* ]] && echo yes || echo no)" \
      "genuine gap: names the timer's own last fire"

# --- 3. THE REGRESSION: a tick is RUNNING; this must NOT read as TIMER DEAD ---
# The same shape as the 02:54:14Z incident: heartbeat older than the current
# threshold (last COMPLETION), but the timer fired 206s ago and a tick is
# executing right now.
recent="$(date -d @$(( $(date +%s) - 206 )))"
out="$(run_case "$stale_age" "$stale_age" "$recent" activating)"
check "$([[ $out == *"TICK-RUNNING-LONG"* ]] && echo yes || echo no)" \
      "running tick: typed as TICK-RUNNING-LONG" "got: $out"
check "$([[ $out != *"TIMER DEAD"* ]] && echo yes || echo no)" \
      "running tick: MUST NOT say TIMER DEAD (this is the false alarm)"
check "$([[ $out == *"schedule is ALIVE"* ]] && echo yes || echo no)" \
      "running tick: states the schedule is alive"
# `activating` is the state a Type=oneshot sits in for its whole run; missing it
# would miss the entire long-tick case.
out2="$(run_case "$stale_age" "$stale_age" "$recent" active)"
check "$([[ $out2 == *"TICK-RUNNING-LONG"* ]] && echo yes || echo no)" \
      "running tick: 'active' also counts as running"

# --- 4. Timer firing but ticks not completing is its OWN class ---------------
out="$(run_case "$stale_age" "$stale_age" "$recent" inactive)"
check "$([[ $out == *"TICKS NOT COMPLETING"* ]] && echo yes || echo no)" \
      "firing-but-failing: typed separately" "got: $out"
check "$([[ $out != *"TIMER DEAD"* ]] && echo yes || echo no)" \
      "firing-but-failing: not conflated with a dead timer"
check "$([[ $out == *"timer is ALIVE"* ]] && echo yes || echo no)" \
      "firing-but-failing: says the timer is alive"

# --- 5. Never-fired timer is still a dead timer ------------------------------
out="$(run_case "$stale_age" "$stale_age" "" inactive)"
check "$([[ $out == *"TIMER DEAD"* ]] && echo yes || echo no)" \
      "never-fired timer: pages TIMER DEAD" "got: $out"
check "$([[ $out == *"NEVER"* ]] && echo yes || echo no)" \
      "never-fired timer: says NEVER rather than a bogus age"

# --- 6. Missing heartbeat log is unchanged -----------------------------------
out="$(run_case missing "$stale_age" "$old" inactive)"
check "$([[ $out == *"heartbeat log MISSING"* ]] && echo yes || echo no)" \
      "missing heartbeat: preserved behaviour" "got: $out"

# --- 7. SELF-CORRECTION: premise withdrawn between observation and decision --
# A `stat` shim that reports the heartbeat STALE on its first call and FRESH
# afterwards -- i.e. a tick completed while the alarm was deciding. This is the
# "delayed alarm whose premise is stale" case; it must be suppressed.
SHIM="$(mktemp)"
cat > "$SHIM" <<'SHIMEOF'
#!/usr/bin/env bash
# Only intercepts `stat -c %Y <heartbeat>`; everything else defers to real stat.
real=/usr/bin/stat
target="${!#}"
# Counter path comes from the environment, NOT from $PPID: each `stat` runs
# inside its own command-substitution subshell, so $PPID differs between calls
# and the counter would reset every time (it did -- this test failed silently
# until that was found).
if [ "${1:-}" = "-c" ] && [ "${2:-}" = "%Y" ] && [[ "$target" == *heartbeat ]]; then
  n="${HB_STAT_COUNTER:?stat shim requires HB_STAT_COUNTER}"; c=0; [ -f "$n" ] && c="$(cat "$n")"
  echo $(( c + 1 )) > "$n"
  if [ "$c" -eq 0 ]; then echo $(( $(date +%s) - 10860 )); else echo $(( $(date +%s) - 30 )); fi
  exit 0
fi
exec "$real" "$@"
SHIMEOF
chmod +x "$SHIM"
out="$(run_case "$stale_age" 60 "$recent" inactive "" "$SHIM")"
rm -f "$SHIM"
check "$([[ $out == *"SELF-CORRECTED"* ]] && echo yes || echo no)" \
      "stale premise: withdrawn on re-read" "got: $out"
check "$([[ $out != *ALARM* ]] && echo yes || echo no)" \
      "stale premise: no ALARM emitted"

# --- 8. Every alarm carries the full observation tuple -----------------------
out="$(run_case "$stale_age" "$stale_age" "$old" inactive)"
for field in observed_at= cadence_s= threshold_s= last_completion= last_invocation_start= timer_last_trigger= service=; do
  check "$([[ $out == *"$field"* ]] && echo yes || echo no)" "observation tuple carries $field"
done

# --- 9. The known WORK-GATED path is untouched -------------------------------
gated_line="$(date -u +%Y-%m-%dT%H:%M:%SZ) rc=1 advanced=no summary=agent-tool: agent-utils is dirty at pinned commit"
out="$(run_case 60 99999 "$(date -d @$(( $(date +%s) - 60 )))" inactive "$gated_line")"
check "$([[ $out == *"WORK-GATED"* ]] && echo yes || echo no)" \
      "work-gated classification preserved" "got: $out"
check "$([[ $out != *"TIMER DEAD"* ]] && echo yes || echo no)" \
      "work-gated: not escalated to TIMER DEAD"

# --- N. Findings lines must not displace the invocation record ----------------
# THE DEFECT THIS PINS. hermit-health-tick.sh now also appends the untruncated
# ACTION/NOTE text as `<stamp> finding=...` lines, because the 240-char summary
# spent its whole budget on boilerplate and recorded ZERO of 5415 ticks' actual
# findings. Those lines carry the same timestamp but no `rc=`, so a reader doing
# a bare `tail -1` lands on one, fails every classification pattern, and
# downgrades a KNOWN, named gate into "WORK-STALLED (unexpected)" -- swapping a
# correct quiet verdict for a spurious page. The reader selects the last
# `... rc=` record instead; these cases hold it to that.
finding_line="$(date -u +%Y-%m-%dT%H:%M:%SZ) finding=ACTION: hard-warn-coordinator component=worktree-disk-residue title=\"ACTIVE SLOT CAP BREACH: 77 active slot(s) against cap 12\""

# Findings AFTER the record (the ordering hermit-health-tick.sh avoids, but any
# other appender could produce): classification must still hold.
out="$(run_case 60 99999 "$(date -d @$(( $(date +%s) - 60 )))" inactive "$gated_line
$finding_line")"
check "$([[ $out == *"WORK-GATED"* ]] && echo yes || echo no)" \
      "findings after record: work-gated classification survives" "got: $out"
check "$([[ $out != *"WORK-STALLED"* ]] && echo yes || echo no)" \
      "findings after record: not misread as WORK-STALLED" "got: $out"

# Findings BEFORE the record (the ordering hermit-health-tick.sh actually uses).
out="$(run_case 60 99999 "$(date -d @$(( $(date +%s) - 60 )))" inactive "$finding_line
$gated_line")"
check "$([[ $out == *"WORK-GATED"* ]] && echo yes || echo no)" \
      "findings before record: work-gated classification survives" "got: $out"

# A heartbeat of findings with NO invocation record must not read as a fresh
# tick: no record means the start time is unknown, not recent.
out="$(run_case 60 60 "$(date -d @$(( $(date +%s) - 60 )))" inactive "$finding_line")"
check "$([[ $out != *"WORK-STALLED"* ]] && echo yes || echo no)" \
      "findings-only heartbeat: no spurious WORK-STALLED" "got: $out"

# --- 8. THE ALARM MUST ARRIVE WHEN THE ORC ROUTE IS DOWN ----------------------
#
# This is the condition the whole second route exists for, so it is tested with
# the Orc route FAILING rather than succeeding. A test that passes only while
# the coordinator is reachable would be measuring the defect, not the fix.
stale_age=10860
old_trigger="$(date -d @$(( $(date +%s) - stale_age )))"

payload="$(mktemp)"
out="$(CASE_ORC_RC=1 CASE_ESCALATE=ok CASE_ESCALATED_OUT="$payload" \
       run_case "$stale_age" "$stale_age" "$old_trigger" inactive)"
check "$([[ $out == *"escalation-PUSH-FAILED"* ]] && echo yes || echo no)" \
      "orc route down: its own failure is still named" "got: $out"
check "$([[ $out == *"escalation-published rc=0"* ]] && echo yes || echo no)" \
      "orc route down: the second route still reports the alarm" "got: $out"
check "$([[ $(cat "$payload" 2>/dev/null) == *"TIMER DEAD"* ]] && echo yes || echo no)" \
      "orc route down: the alarm text itself is what gets published" \
      "got: $(head -c 200 "$payload" 2>/dev/null)"
rm -f "$payload"

# Both routes down must not read like either one working. "Escalated" with no
# destination is the ambiguity this alarm was written to remove.
out="$(CASE_ORC_RC=1 CASE_ESCALATE=fail run_case "$stale_age" "$stale_age" "$old_trigger" inactive)"
check "$([[ $out == *"escalation-PUBLISH-FAILED"* ]] && echo yes || echo no)" \
      "both routes down: the second failure is named too" "got: $out"
check "$([[ $out == *"BOTH routes are now down"* ]] && echo yes || echo no)" \
      "both routes down: says so in one line" "got: $out"
check "$([[ $out != *"escalation-published rc=0"* ]] && echo yes || echo no)" \
      "both routes down: nothing claims a publication" "got: $out"

# An absent second route is a third state, distinct from a failed one: nothing
# was attempted, and the log has to say which.
out="$(CASE_ORC_RC=0 CASE_ESCALATE=absent run_case "$stale_age" "$stale_age" "$old_trigger" inactive)"
check "$([[ $out == *"escalation-UNAVAILABLE"* ]] && echo yes || echo no)" \
      "second route absent: reported as absent, not as failed" "got: $out"
check "$([[ $out == *"alerted-coordinator (push OK)"* ]] && echo yes || echo no)" \
      "second route absent: the orc route's own outcome still stands" "got: $out"

# The ordinary case: both routes work, and both are reported.
out="$(CASE_ORC_RC=0 CASE_ESCALATE=ok run_case "$stale_age" "$stale_age" "$old_trigger" inactive)"
check "$([[ $out == *"alerted-coordinator (push OK)"* ]] && echo yes || echo no)" \
      "both routes up: orc reported" "got: $out"
check "$([[ $out == *"escalation-published rc=0"* ]] && echo yes || echo no)" \
      "both routes up: publication reported" "got: $out"

# --- report -------------------------------------------------------------------
total=$(( PASS + ${#FAILURES[@]} ))
if [ ${#FAILURES[@]} -gt 0 ]; then
    echo "FAIL  ${#FAILURES[@]} of $total checks failed:"
    for f in "${FAILURES[@]}"; do echo "  - $f"; done
    exit 1
fi
echo "ok  $PASS/$total checks passed"
