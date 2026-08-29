#!/usr/bin/env bash
# matched-burst.sh <sha> <width> <timeout_s> <workload>  ->  burst CSV on stdout
#
# The CALIBRATOR-GATED matched-load burst: the $STRESS_BURST_CMD implementation
# nightly.sh drives. It is the piece the owner said the old stress-burst was
# MISSING — a validity calibrator — and it deliberately does NOT reimplement the
# flaky detector. It shells out to hermit-multisect's already-CALIBRATED
# matched-load probe (matched.sh) and adds ONE thing on top: the autonomous
# validity-gate + per-wave verdict that multisect applied by hand.
#
# WHY a calibrator (owner rule): the vfork/reap flake is LOAD-DEPENDENT. On a
# quiet host a burst can report 64/64 pass and a naive nightly rounds that to
# GREEN — a false negative that hides a broken subject. So every wave co-runs a
# KNOWN-FLAKY binary (calib-9c964fce, 21.6% measured); a wave COUNTS only if that
# calibrator comes back FLAKY/FAIL, proving the wave was powerful enough to expose
# the race. A wave where the calibrator stays clean is UNDER-POWERED and is
# DISCARDED, not scored. If NO wave is valid, the run is RED (could-not-validate),
# never silent-green.
#
# MATCHED LOAD: matched.sh launches <width> single-shot instances of EVERY label
# (subject + calibrator) interleaved in the SAME wave, so subject and calibrator
# see identical instantaneous host load on this 316-core box. That is what makes
# the calibrator a valid witness for the subject's wave.
#
# OUTPUT: one CSV row PER VALID WAVE for the SUBJECT (so stress_store.py judges
# each wave's trinary independently: all-pass=CLEAN, mixed=FLAKY, zero-pass=
# FAILING). Zero valid waves -> a single non-OK row so the run is RED.
#   Row: sha,short,build_s,burst_N,hangs,passes,other,hang_rate,STATUS
#
# ENV:
#   STRESS_WAVES      matched waves to run                       (default 10)
#   STRESS_CALIB_BIN  known-flaky calibrator binary  (default ignored/ci-hub/stress-calib/calib-9c964fce)
#   MATCHED_SH        path to multisect's matched.sh (default experiments/multisect_detcore_misc_20260803/matched.sh)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CALIBDIR="$ROOT/ignored/ci-hub/stress-calib"

SHA="$1"; WIDTH="${2:-64}"; TIMEOUT="${3:-20}"; WL="$4"
BIN_NAME="${WL%%:*}"                       # e.g. tests_misc
WRKSLOTS="$ROOT/ci-hub/bin/wrkslots"
SLOT="stress-matched-${SHA:0:12}-$$"
SLOT_CREATED=0
TARGET_DIR="$ROOT/ignored/ci-hub/stress-target/matched"
WT=""
WAVES="${STRESS_WAVES:-10}"
CALIB_BIN="${STRESS_CALIB_BIN:-$CALIBDIR/calib-9c964fce}"
MATCHED_SH="${MATCHED_SH:-$ROOT/experiments/multisect_detcore_misc_20260803/matched.sh}"

cleanup() {
  local rc=$?
  trap - EXIT
  if [ "$SLOT_CREATED" -eq 1 ]; then
    cleanup_output=$(
      "$WRKSLOTS" --allow-existing-unregistered-worktrees \
        remove "$SLOT" --validate-complete \
        --coordinator-pid "$$" --expected-generation 1 2>&1
    )
    cleanup_rc=$?
    if [ "$cleanup_rc" -ne 0 ]; then
      printf '%s\n' \
        "matched-burst: validation checkout RETAINED after wrkslots cleanup refused." \
        "state: RETAINED -- the stress result remains exit $rc; cleanup did not complete." \
        "remedy: $cleanup_output" >&2
    fi
  fi
  exit "$rc"
}
trap cleanup EXIT

short() { git -C "$WT" rev-parse --short HEAD 2>/dev/null || echo "${SHA:0:12}"; }
row_err() { echo "$SHA,$(short 2>/dev/null || echo "${SHA:0:12}"),${1:-},$WIDTH,,,,,$2"; }
detcore_package_name() {
  grep -m1 -E '^name = "(hermit-)?detcore"$' "$1/detcore/Cargo.toml" |
    cut -d '"' -f 2
}

# --- preconditions: the calibrated primitive + the known-flaky witness ---------
[ -x "$MATCHED_SH" ]  || { row_err "" MATCHED_MISSING; exit 0; }
[ -x "$CALIB_BIN" ]   || { row_err "" CALIB_MISSING;   exit 0; }

# --- build the SUBJECT at main HEAD in a registered validation checkout -------
mkdir -p "$TARGET_DIR" "$CALIBDIR"
create_output=$(
  "$WRKSLOTS" --allow-existing-unregistered-worktrees \
    create "$SLOT" --format json --slot-type validate \
    --coordinator-authorized --agent "stress-matched-$$" \
    --task "matched-stress-$SHA" \
    --purpose "build the matched-load stress subject at its recorded commit" \
    --owner-pid "$$" --coordinator-pid "$$" \
    --repo checkout=hermit --start "checkout=$SHA"
)
create_rc=$?
if [ "$create_rc" -ne 0 ]; then
  printf '%s\n' \
    "matched-burst: wrkslots could not create the validation checkout." \
    "state: REFUSED -- no matched workload ran, so this is WT_FAIL rather than a test result." \
    "remedy: follow the wrkslots refusal above, then rerun this command." >&2
  row_err "" WT_FAIL
  exit 0
fi
SLOT_CREATED=1
WT=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["checkouts"][0]["path"])' <<<"$create_output")
parse_rc=$?
if [ "$parse_rc" -ne 0 ] || [ ! -d "$WT" ]; then
  printf '%s\n' \
    "matched-burst: wrkslots returned no readable checkout path." \
    "state: REFUSED -- no matched workload ran, so this is WT_FAIL rather than a test result." \
    "remedy: run '$WRKSLOTS status --slot $SLOT --format json' and inspect the recorded path." >&2
  row_err "" WT_FAIL
  exit 0
fi

bstart=$(date +%s)
DETCOR_PACKAGE="$(detcore_package_name "$WT")"
case "$DETCOR_PACKAGE" in
  detcore | hermit-detcore) ;;
  *) row_err "$(( $(date +%s)-bstart ))" BUILD_FAIL; exit 0 ;;
esac
if ! ( cd "$WT" && CARGO_TARGET_DIR="$TARGET_DIR" with-proxy cargo test -p "$DETCOR_PACKAGE" --test "$BIN_NAME" --no-run ) >/dev/null 2>&1; then
  row_err "$(( $(date +%s)-bstart ))" BUILD_FAIL; exit 0
fi
BUILD_S=$(( $(date +%s)-bstart ))

# Newest built test binary (skip .d depfiles); copy to a stable subject path.
# ls -t is deliberate: this needs the NEWEST build artifact and a glob cannot order
# by mtime. cargo names these <bin>-<hex>, so there is no whitespace to split on.
# shellcheck disable=SC2010
SUBJ_SRC="$(ls -t "$TARGET_DIR"/debug/deps/"$BIN_NAME"-* 2>/dev/null | grep -v '\.d$' | head -1)"
[ -x "$SUBJ_SRC" ] || { row_err "$BUILD_S" NOBIN; exit 0; }
SUBJ_BIN="$CALIBDIR/subject-$(short)"
cp "$SUBJ_SRC" "$SUBJ_BIN" && chmod +x "$SUBJ_BIN" || { row_err "$BUILD_S" NOBIN; exit 0; }

# --- matched-load probe: subject + calibrator, interleaved every wave ----------
# matched.sh emits raw per-(label,wave) exit-code files under a timestamped dir it
# prints as "results: <dir>". We parse those directly (robust to its log format).
OUT="$("$MATCHED_SH" "$WIDTH" "$TIMEOUT" "$WAVES" \
        "subject:$SUBJ_BIN" "zcalib:$CALIB_BIN" 2>/dev/null)"
WD="$(sed -n 's/^results: //p' <<<"$OUT" | tail -1)"
[ -n "$WD" ] && [ -d "$WD" ] || { row_err "$BUILD_S" PROBE_FAIL; exit 0; }

# Trinary classifier over a raw exit-code file (124=hang, 0=pass, else other).
classify() { # <file> -> "VERDICT hangs passes other total"
  local f="$1" h p t o
  h=$(grep -cx 124 "$f" 2>/dev/null); h=${h:-0}
  p=$(grep -cx 0   "$f" 2>/dev/null); p=${p:-0}
  t=$(wc -l < "$f" 2>/dev/null); t=${t:-0}
  o=$((t - h - p))
  local c
  if   [ "$t" -eq 0 ];   then c=NORUN
  elif [ "$p" -eq "$t" ]; then c=PASS
  elif [ "$p" -eq 0 ];    then c=FAIL
  else c=FLAKY; fi
  echo "$c $h $p $o $t"
}

SHORT="$(short)"
valid=0
for w in $(seq 1 "$WAVES"); do
  cf="$WD/w$w.zcalib"; sf="$WD/w$w.subject"
  [ -f "$cf" ] && [ -f "$sf" ] || continue
  read -r cv _ch _cp _co _ct <<<"$(classify "$cf")"
  # Validity gate: only waves where the KNOWN-FLAKY calibrator actually flaked.
  case "$cv" in FLAKY|FAIL) : ;; *) continue ;; esac
  read -r _sv sh sp so st <<<"$(classify "$sf")"
  [ "$st" -gt 0 ] || continue
  rate=$(awk "BEGIN{printf \"%.4f\", $st?$sh/$st:0}")
  echo "$SHA,$SHORT,$BUILD_S,$st,$sh,$sp,$so,$rate,OK"
  valid=$((valid+1))
done

# No valid wave = the calibrator never witnessed its own known bug = the probe was
# under-powered (or broken). That is RED: the nightly cannot certify green.
[ "$valid" -gt 0 ] || row_err "$BUILD_S" CALIB_UNDERPOWERED
exit 0
