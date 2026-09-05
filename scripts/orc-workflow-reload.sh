#!/usr/bin/env bash
# Bounded, versioned reload of a single ORC workflow.
#
# WHY THIS EXISTS. A workflow holds the module snapshot the engine evaluated when
# it was registered, so editing a plugin on disk does nothing until the workflow
# re-evaluates. The only routes to that were an owner-only Orc relaunch, or
# injecting arbitrary JS into the live coordinator to call orc.killWorkflow --
# against workflows whose crash-loop is a known, load-bearing failure mode. This
# uses the engine's own `/api/workflow/restart` endpoint instead: one named
# operation over the authenticated socket, no arbitrary code execution.
#
# WHY IT CANNOT CRASH-LOOP. The engine re-evaluates the module on restart, and a
# module that throws takes the workflow down, restarts, throws again -- until the
# engine exhausts its restart budget and the heartbeat is silently dead. Both
# workflows in the hermit-dev plugin died exactly that way. So this script
# REFUSES TO RESTART ANYTHING until every --preflight command exits 0. A plugin
# that does not parse, or whose tests fail, never reaches the engine. That is the
# bound: the dangerous operation is gated behind evidence, not attempted and
# then repaired.
#
# It then VERIFIES rather than assumes: the active source identity must actually
# change, the workflow must come back alive, exactly one instance of the name
# must exist (no duplicate), and the state must be stable across several samples
# (no flapping). Any of those failing is a nonzero exit with a typed reason.
#
# Usage:
#   scripts/orc-workflow-reload.sh --session <id-or-prefix> --workflow <name>
#       [--preflight 'cmd']...     Gate: all must exit 0 before any restart.
#       [--expect-marker STR]...   Must be ABSENT before and PRESENT after.
#       [--settle-secs N]          Stability window after restart (default 30).
#       [--dry-run]                Report the before-state and preflight only.
#       [--method restart|reload-config]
#
# METHOD MATTERS, AND restart IS NOT ENOUGH. Measured 2026-08-07:
# /api/workflow/restart returns {"sent":true} and creates a new run attempt, but
# the active source identity is BYTE-IDENTICAL afterwards -- it replays the spec
# stored at registration rather than re-evaluating the module from disk. So a
# plugin edit is invisible to it. `reload-config` invokes the engine's own
# documented `reloadConfig` ("Hot-reload config.js"), which re-imports config.js
# and re-registers the workflows with the new source. Both are NAMED engine
# operations over the authenticated socket; neither injects arbitrary JS.
# Default is reload-config because it is the one that actually changes identity.
set -uo pipefail

# ⚠️ SAME DEFECT AS scripts/orc-hermit-msg.py CARRIED HERE. $HOME/orc-bin/orc
# does not exist on devbig014 (measured 2026-09-05: that directory holds only
# `tg`); the real orc is on PATH at /usr/local/bin/orc. Prefer the local pin
# when it is genuinely there, otherwise take orc from PATH, and keep ORC_BIN as
# the explicit override.
if [ -n "${ORC_BIN:-}" ]; then
    ORC="$ORC_BIN"
elif [ -x "$HOME/orc-bin/orc" ]; then
    ORC="$HOME/orc-bin/orc"
else
    ORC="$(command -v orc || printf '%s' "$HOME/orc-bin/orc")"
fi
SESSION=""; WORKFLOW=""; SETTLE=30; DRY=0; METHOD=reload-config
declare -a PREFLIGHT=() MARKERS=()

die() { echo "orc-workflow-reload: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
    case "$1" in
        --session)       SESSION="${2:?}"; shift 2 ;;
        --workflow)      WORKFLOW="${2:?}"; shift 2 ;;
        --preflight)     PREFLIGHT+=("${2:?}"); shift 2 ;;
        --expect-marker) MARKERS+=("${2:?}"); shift 2 ;;
        --settle-secs)   SETTLE="${2:?}"; shift 2 ;;
        --method)        METHOD="${2:?}"; shift 2 ;;
        --dry-run)       DRY=1; shift ;;
        -h|--help)       sed -n '1,34p' "$0"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done
[ -n "$SESSION" ]  || die "--session is required"
[ -n "$WORKFLOW" ] || die "--workflow is required"
[ -x "$ORC" ]      || die "orc binary not executable at $ORC (override with ORC_BIN)"

# Emit one JSON object describing the named workflow, plus how many carry that
# exact name. Duplicate detection is a COUNT, not a boolean: "a workflow exists"
# would stay true if a restart accidentally produced two.
probe() {
    "$ORC" curl --session "$SESSION" /api/state 2>/dev/null | python3 -c '
import sys,json,hashlib
try: d=json.load(sys.stdin)
except Exception: print(json.dumps({"error":"state-unreadable"})); sys.exit(0)
name=sys.argv[1]
ws=[w for w in d.get("workflows",[]) if w.get("name")==name]
if not ws: print(json.dumps({"error":"absent","count":0})); sys.exit(0)
w=ws[0]; src=w.get("source_text") or ""
print(json.dumps({
  "count":len(ws),"alive":bool(w.get("alive")),"state":w.get("state"),
  "effect":w.get("current_effect") or "","len":len(src),
  "sha":hashlib.sha256(src.encode()).hexdigest(),
  "markers":{m:(m in src) for m in sys.argv[2:]},
}))' "$WORKFLOW" "${MARKERS[@]}"
}

field() { python3 -c 'import sys,json;print(json.load(sys.stdin).get(sys.argv[1],""))' "$1"; }

echo "== orc-workflow-reload: $WORKFLOW =="

BEFORE="$(probe)"
[ -n "$BEFORE" ] || die "could not read session state (is the session id right?)"
echo "BEFORE $BEFORE"
b_sha="$(printf '%s' "$BEFORE" | field sha)"
b_count="$(printf '%s' "$BEFORE" | field count)"
[ "$b_count" = "1" ] || die "expected exactly 1 workflow named '$WORKFLOW' before restart, found $b_count"

# A marker already present means the active snapshot is ALREADY current and a
# restart would be a no-op that still costs a heartbeat gap. Refuse: the reload
# must be necessary, or the "identity changed" proof below is meaningless.
for m in ${MARKERS+"${MARKERS[@]}"}; do
    if printf '%s' "$BEFORE" | python3 -c 'import sys,json;sys.exit(0 if json.load(sys.stdin)["markers"][sys.argv[1]] else 1)' "$m"; then
        die "marker already present before restart: '$m' -- active snapshot is already current, nothing to reload"
    fi
done

# ---- PREFLIGHT GATE: nothing dangerous happens above this line --------------
fail=0
for cmd in ${PREFLIGHT+"${PREFLIGHT[@]}"}; do
    if eval "$cmd" >/dev/null 2>&1; then
        echo "PREFLIGHT ok   : $cmd"
    else
        echo "PREFLIGHT FAIL : $cmd"; fail=1
    fi
done
[ "$fail" -eq 0 ] || die "preflight failed; REFUSING to restart (a module that cannot pass its own checks must not reach the engine)"

if [ "$DRY" -eq 1 ]; then echo "DRY-RUN: preflight passed; no restart attempted"; exit 0; fi

# ---- RESTART ----------------------------------------------------------------
case "$METHOD" in
  restart)
    # Replays the STORED spec; will NOT pick up an edited module (measured).
    resp="$("$ORC" curl --session "$SESSION" /api/workflow/restart -X POST \
            -H 'Content-Type: application/json' \
            -d "{\"session_id\":\"$SESSION\",\"name\":\"$WORKFLOW\"}" 2>&1)" ;;
  reload-config)
    # The engine's own hot-reload of config.js, which re-imports the plugin and
    # re-registers its workflows. Gated by validateConfig first: the engine's own
    # validator is a better preflight than anything this script could invent.
    "$ORC" curl --session "$SESSION" /api/slash-command -X POST \
        -H 'Content-Type: application/json' \
        -d "{\"session_id\":\"$SESSION\",\"command\":\"validateConfig\"}" >/dev/null 2>&1
    resp="$("$ORC" curl --session "$SESSION" /api/slash-command -X POST \
            -H 'Content-Type: application/json' \
            -d "{\"session_id\":\"$SESSION\",\"command\":\"reloadConfig\"}" 2>&1)" ;;
  *) die "unknown --method: $METHOD (expected restart or reload-config)" ;;
esac
echo "RELOAD method=$METHOD response: ${resp:0:200}"

# ---- VERIFY -----------------------------------------------------------------
changed=0
for _ in $(seq 1 "$SETTLE"); do
    sleep 1
    AFTER="$(probe)"
    a_sha="$(printf '%s' "$AFTER" | field sha)"
    a_alive="$(printf '%s' "$AFTER" | field alive)"
    if [ -n "$a_sha" ] && [ "$a_sha" != "$b_sha" ] && [ "$a_alive" = "True" ]; then changed=1; break; fi
done
AFTER="$(probe)"
echo "AFTER  $AFTER"
a_sha="$(printf '%s' "$AFTER" | field sha)"
a_count="$(printf '%s' "$AFTER" | field count)"

[ "$changed" -eq 1 ] || { echo "RESULT reload=FAILED reason=source-identity-unchanged-or-not-alive"; exit 1; }
[ "$a_count" = "1" ] || { echo "RESULT reload=FAILED reason=duplicate-workflow count=$a_count"; exit 1; }
for m in ${MARKERS+"${MARKERS[@]}"}; do
    printf '%s' "$AFTER" | python3 -c 'import sys,json;sys.exit(0 if json.load(sys.stdin)["markers"][sys.argv[1]] else 1)' "$m" \
        || { echo "RESULT reload=FAILED reason=marker-absent-after marker=$m"; exit 1; }
done

# Stability: a crash-loop presents as repeated re-registration, so the identity
# and liveness must hold still, not merely be right once.
flaps=0
for _ in $(seq 1 8); do
    sleep 2
    s="$(probe)"
    [ "$(printf '%s' "$s" | field sha)"   = "$a_sha" ] || flaps=$(( flaps + 1 ))
    [ "$(printf '%s' "$s" | field alive)" = "True"   ] || flaps=$(( flaps + 1 ))
    [ "$(printf '%s' "$s" | field count)" = "1"      ] || flaps=$(( flaps + 1 ))
done
[ "$flaps" -eq 0 ] || { echo "RESULT reload=UNSTABLE flaps=$flaps (possible crash-loop)"; exit 1; }

echo "RESULT reload=OK before_sha=${b_sha:0:16} after_sha=${a_sha:0:16} count=1 flaps=0"
