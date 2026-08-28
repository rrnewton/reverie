#!/usr/bin/env bash
# Nightly Hermit determinism stress run — driver.
#
# WHAT: at main HEAD, run the shared concurrent-BURST probe for each workload in
# the stress set, then fold every burst through the flaky-is-red verdict and
# record one durable run per workload into the ci-hub store
# (ignored/ci-hub/stress-runs.jsonl). Any non-CLEAN verdict raises a P0 alarm.
#
# WHY it lives here (parent host), not on a GitHub runner: the flake this exists
# to catch is LOAD-DEPENDENT (multisect measured ~28% per-instance hang at 128
# concurrent under real fleet load; an idle single-tenant GH runner shows ~0% =
# false green). The nightly must run where the load is — this dev host.
#
# HARNESS SHARING (owner rule "do NOT write a second one"): this driver owns NO
# burst logic. It shells out to ONE shared primitive via $STRESS_BURST_CMD, whose
# contract is:
#
#     $STRESS_BURST_CMD <sha> <width> <timeout_s> <workload>  ->  burst CSV on stdout
#       CSV row: sha,short,build_s,burst_N,hangs,passes,other,hang_rate,STATUS
#
# multisect's experiments/.../probe.sh already emits that CSV; the only missing
# piece for nightly is a relocatable / --prebuilt build mode so a main-HEAD run
# does not collide with multisect's SHA-keyed worktrees. Until that shared tool
# is agreed & placed (see ci-hub/stress/README.md), $STRESS_BURST_CMD is unset
# and this driver records a RED "ERROR: primitive not wired" run rather than
# silently passing. That is intentional: a silent-green nightly is the exact
# failure mode this task exists to kill.
#
# ENV:
#   STRESS_BURST_CMD   command template (see contract above). REQUIRED to probe.
#   STRESS_WIDTH       concurrent instances per burst          (default 64)
#   STRESS_TIMEOUT     per-instance timeout seconds            (default 20)
#   STRESS_WORKLOADS   space/newline-separated workload slugs  (default set below)
#   STRESS_REPO        OWNER/REPO                              (default rrnewton/hermit)
#   CI_HUB_STRESS_STORE  store path (default ignored/ci-hub/stress-runs.jsonl)
#   CI_HUB_AGENT       agent tag recorded on each run          (default hermit-250)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERMIT="$ROOT/hermit"
STORE_PY="$ROOT/ci-hub/stress/stress_store.py"
WIDTH="${STRESS_WIDTH:-64}"
TIMEOUT="${STRESS_TIMEOUT:-20}"
REPO="${STRESS_REPO:-rrnewton/hermit}"
# SUPPRESSED BUT NOT RESOLVED, AND THE DIFFERENCE MATTERS. shellcheck is right: AGENT
# is assigned here and read nowhere. Line 36 above documents CI_HUB_AGENT as the "agent
# tag recorded on each run", and the CSV row this script emits (line 19) is
# sha,short,build_s,burst_N,hangs,passes,other,hang_rate,STATUS -- no agent column. So
# the documented tag is not recorded on any run. Deleting the variable would remove the
# only remaining trace of that, so it stays with this note; implementing the column is
# a change to the stress-run store's schema and belongs in its own task.
# shellcheck disable=SC2034
AGENT="${CI_HUB_AGENT:-hermit-250}"
# Alarm sink: a STANDING open task, NOT the (closable) setup task — else a closed
# task swallows every future nightly P0 and the alarm goes silently invisible.
ALARM_TASK="${STRESS_ALARM_TASK:-nightly_stress_red_triage}"

# Default workload set — the paths where flakiness actually bites (NOT /bin/echo).
# Seeded with the CONFIRMED reproducer; grows to reap/waitpid + scheduling/futex.
DEFAULT_WORKLOADS='tests_misc:vfork::vfork_parent_resumes_after_child_exec'
WORKLOADS="${STRESS_WORKLOADS:-$DEFAULT_WORKLOADS}"

ts() { TZ=UTC date +%Y-%m-%dT%H:%M:%SZ; }
log() { echo "[$(ts)] $*" >&2; }

# Evidence capture for ATTRIBUTION. Default ON for the nightly: the whole point of
# this run is to catch a flake, and a flake we cannot attribute is half-useful. We
# set STRESS_CAPTURE_DIR so the shared burst primitive (which flows it down to
# matched.sh) preserves a bundle per FAILING instance; then on any non-CLEAN
# verdict we fold `attribution.py report` into the alarm so the P0 states a CAUSE
# (INFRASTRUCTURE / HERMIT_NONDETERMINISM / ENVIRONMENT / …), not just a rate.
CAPTURE="${STRESS_CAPTURE:-1}"
CAPBASE="$ROOT/ignored/ci-hub/stress-capture"
ATTR_PY="$ROOT/ci-hub/attribution/attribution.py"

# --- resolve main HEAD (read-only; never mutates the primary checkout) --------
if ! with-proxy git -C "$HERMIT" fetch -q origin main 2>/dev/null; then
  log "WARN: could not fetch origin/main; using local ref"
fi
SHA="$(git -C "$HERMIT" rev-parse origin/main 2>/dev/null || git -C "$HERMIT" rev-parse HEAD)"
log "nightly stress at main HEAD $SHA (width=$WIDTH timeout=${TIMEOUT}s repo=$REPO)"

overall_alarm=0
# Attribute the preserved failing-run bundles in a capture dir to a CAUSE.
# Echoes a one-line attribution summary (empty if nothing to attribute) and,
# when there is evidence, writes the full per-bundle report to a sidecar.
attribute_capture() {
  # $1=capture_dir  $2=marker_path (sidecar is <marker>.attribution.txt)
  local capdir="$1" marker="$2"
  [ -n "$capdir" ] && [ -d "$capdir" ] || return 0
  find "$capdir" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | grep -q . || return 0
  [ -f "$ATTR_PY" ] || return 0
  local report
  report="$(python3 "$ATTR_PY" report "$capdir" 2>/dev/null)" || return 0
  [ -n "$report" ] || return 0
  printf '%s\n' "$report" > "${marker%.json}.attribution.txt" 2>/dev/null
  # One-line headline: the SUMMARY line the report prints last, plus a dropped-cap note.
  local headline dropped
  headline="$(sed -n 's/^ATTRIBUTION SUMMARY: //p' <<<"$report" | tail -1)"
  dropped="$(cat "$capdir/.dropped-over-cap" 2>/dev/null | wc -l)"
  [ "${dropped:-0}" -gt 0 ] && headline="$headline (+$dropped bundle(s) dropped over cap)"
  echo "$headline"
}

raise_alarm() {
  # $1=workload $2=verdict $3=one-line detail $4=capture_dir(optional)
  overall_alarm=1

  # A P0 alarm that cannot describe itself is worse than no alarm: it sets the
  # exit status without leaving anything to triage. Refuse loudly instead.
  if [ $# -lt 3 ] || [ -z "${1:-}" ] || [ -z "${2:-}" ] || [ -z "${3:-}" ]; then
    log "🔴 raise_alarm INVOCATION ERROR: need workload, verdict and detail; got $# arg(s): '${1:-}' '${2:-}' '${3:-}'"
    return 2
  fi

  local marker
  marker="$ROOT/ignored/ci-hub/stress-alarm-$(TZ=UTC date +%Y%m%dT%H%M%SZ)-$$.json"
  mkdir -p "$(dirname "$marker")"
  local attr; attr="$(attribute_capture "${4:-}" "$marker")"
  local detail="$3"; [ -n "$attr" ] && detail="$3 | ATTRIBUTION: $attr"

  # The marker IS the durable record, so its write is verified rather than
  # silenced. stderr previously went to /dev/null, which turned a failed write
  # into a P0 that left no evidence at all.
  #
  # The record is SERIALIZED BY A JSON WRITER, not by printf. The previous
  # printf interpolated workload/verdict/detail raw, so a quote or backslash in
  # a stress detail string emitted a marker no consumer could parse — the
  # alarm's own durable record could be corrupt exactly when the detail was
  # most interesting. Values arrive via argv, never inside the program text, so
  # there is nothing to inject. printf remains the fallback on a host without
  # python3, and the validity check below still covers that path.
  local wrote=0
  if command -v python3 >/dev/null 2>&1; then
    if python3 -c '
import json, sys
keys = ("ts", "repo", "sha", "workload", "verdict", "detail", "attribution")
record = dict(zip(keys, sys.argv[2:9]))
record["severity"] = "P0"
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    json.dump(record, handle, ensure_ascii=False, sort_keys=True)
    handle.write("\n")
' "$marker" "$(ts)" "$REPO" "$SHA" "$1" "$2" "$detail" "$attr"; then
      wrote=1
    fi
  elif printf '{"ts":"%s","repo":"%s","sha":"%s","workload":"%s","verdict":"%s","detail":"%s","attribution":"%s","severity":"P0"}\n' \
      "$(ts)" "$REPO" "$SHA" "$1" "$2" "$detail" "$attr" > "$marker"; then
    wrote=1
  fi

  if [ "$wrote" -ne 1 ]; then
    log "🔴 P0 ALARM RECORD FAILED: could not write $marker — the alarm below has NO durable record"
  elif command -v python3 >/dev/null 2>&1 && ! python3 -c 'import json,sys;json.load(open(sys.argv[1]))' "$marker" 2>/dev/null; then
    # Retained for the printf fallback path: a record no consumer can parse is
    # not a record, so say so rather than leaving a corrupt marker behind.
    log "🔴 P0 ALARM RECORD IS NOT VALID JSON: $marker — likely an unescaped quote in workload/detail"
  else
    log "P0 alarm record persisted: $marker"
  fi

  log "🔴 P0 ALARM [$2] $1 — $detail (marker: $marker)"
  [ -n "${STRESS_NO_ESCALATE:-}" ] && return 0

  # External escalation is acknowledged, not fire-and-forget. Previously this
  # was `>/dev/null 2>&1 || true`, so a failed escalation was indistinguishable
  # from a delivered one.
  if command -v tg >/dev/null 2>&1; then
    local tg_err; tg_err="$(tg note "$ALARM_TASK" "P0 NIGHTLY-STRESS RED [$2] $REPO@${SHA:0:12} workload=$1 — $detail. Flaky-is-red: determinism of outcome violated. Triage immediately." 2>&1)"
    if [ $? -eq 0 ]; then
      log "P0 alarm escalated to TaskGraph task $ALARM_TASK"
    else
      log "🔴 P0 ALARM ESCALATION FAILED: tg note $ALARM_TASK rc!=0 — $tg_err (durable record still at $marker)"
    fi
  else
    log "🔴 P0 ALARM NOT ESCALATED: tg not on PATH (durable record still at $marker)"
  fi

  # NOTE: this deliberately does NOT call scripts/status-log.rs. It used to,
  # as `status-log.rs --append "<text>"`, but there has never been an --append
  # flag: the script exits 2 on an unknown argument, and the call was wrapped in
  # `>/dev/null 2>&1 || true`, so every P0 escalation through it was silently
  # discarded. status-log.rs is also the wrong sink by design — it appends
  # COORDINATOR HOURLY STATUS rows and now requires a workstream->worker mapping
  # validated against the live TaskGraph, a --repos denominator and the hourly
  # counts. A stress alarm has none of those, and forcing one in would pollute
  # the hourly series. The durable marker above plus the TaskGraph note are the
  # two real channels.
}

# --- per-workload burst + record ---------------------------------------------
REPS="${STRESS_REPS:-1}"
for wl in $WORKLOADS; do
  log "workload: $wl (reps=$REPS width=$WIDTH)"
  # Per-workload capture dir; the burst primitive inherits it via env and
  # preserves a bundle per failing instance. Keyed by workload+sha+run so a
  # multi-workload night never cross-contaminates evidence.
  wl_cap=""
  if [ "$CAPTURE" != "0" ]; then
    wl_cap="$CAPBASE/$(printf '%s' "$wl" | tr ':/' '__')-${SHA:0:12}-$(TZ=UTC date +%Y%m%dT%H%M%SZ)"
    export STRESS_CAPTURE_DIR="$wl_cap"
  else
    unset STRESS_CAPTURE_DIR
  fi
  if [ -z "${STRESS_BURST_CMD:-}" ]; then
    # Primitive not wired yet (awaiting shared-harness agreement). Record RED.
    csv="$SHA,,,,,,,,NOWIRE"
  else
    # Run REPS burst waves; concatenate CSV rows — stress_store aggregates them,
    # so "over the night's total reps" is one verdict across every wave.
    csv=""
    for rep in $(seq 1 "$REPS"); do
      row="$($STRESS_BURST_CMD "$SHA" "$WIDTH" "$TIMEOUT" "$wl" 2>/dev/null)"
      [ -z "$row" ] && row="$SHA,,,,,,,,NOOUTPUT"
      csv="${csv:+$csv$'\n'}$row"
      log "  wave $rep/$REPS: $row"
    done
  fi
  # Record; stress_store prints a machine summary (VERDICT=... ) to stdout and
  # exits 2 on RED, 0 on CLEAN. Capture stdout for the alarm text.
  summary="$(printf '%s\n' "$csv" | python3 "$STORE_PY" record --csv - \
        --repo "$REPO" --sha "$SHA" --workload "$wl" \
        --width "$WIDTH" --timeout "$TIMEOUT" \
        --source-tool "${STRESS_BURST_CMD:-<unwired>}" --trigger nightly)"
  rc=$?
  verdict="$(sed -n 's/^VERDICT=\([A-Z]*\).*/\1/p' <<<"$summary")"
  case "$rc" in
    0) log "🟢 ${verdict:-CLEAN} $wl" ;;
    2) raise_alarm "$wl" "${verdict:-RED}" "${summary#VERDICT=* }" "$wl_cap" ;;
    *) raise_alarm "$wl" "ERROR" \
         "stress_store refused the burst row (exit $rc); no measurement was recorded" \
         "$wl_cap" ;;
  esac
done

log "nightly stress complete — overall $([ $overall_alarm -eq 0 ] && echo GREEN || echo 🔴 RED/P0)"
exit $overall_alarm
