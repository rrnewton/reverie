#!/usr/bin/env bash
# collect-fullcorpus.sh — the LOCAL full-corpus compat-envelope gate.
#
# The portable/privileged split (collect-envelope.rs + validate-envelope.sh) is
# for GitHub CI, where a runner may lack /dev/kvm, the third-party-backend
# feature build, or the SaBRe loader. On a fully-provisioned local box (this
# machine has /dev/kvm) the definition-of-done gate should instead measure the
# UNION — the FULL 235-cell e2e verify corpus across EVERY backend the local
# binary can run — not the ~28-cell ci=true portable subset.
#
# This script enumerates the full 235-cell ptrace-verify corpus (the same
# denominator as compat-envelope/corpus-manifest.csv: 214 compiled C guests +
# 21 shell/interpreter cells, listed in corpus/corpus-c.tsv + corpus/corpus-nonc.tsv),
# and for every locally-available backend runs, per cell:
#   det    = <backend> --strict --verify exits 0. This is a STRIPPED-DETLOG
#            self-verify, NOT a bitwise one: plain --verify compares the DETLOG
#            under the Stripped policy, which normalises numbers, addresses and
#            paths, and whose own --verify-json reports bitwise_parity:false.
#            Mutation testing measured 3 of 5 planted defects surviving it (a
#            differing read() return length, pointer arg and openat path); see
#            experiments/strict-certification-mutation-sweep_20260806/. This
#            line previously read "L2 DETLOG-bitwise self-verify", which claimed
#            an assurance the flag cannot earn.
#   parity = <backend> --strict stdout == ptrace --strict --verify stdout
# It writes the merged fullcorpus-scorecard.csv, ASSERTS green-stays-green
# against a per-backend ratchet baseline (a real regression fails the gate), and
# renders the scorecard.
#
# Backends are AUTO-DETECTED from the binary + host so a partially-provisioned
# box degrades honestly (a missing backend is recorded n/a, never a false red):
#   ptrace, liteinst   — always (default binary)
#   dbi, sabre, e9patch — require a --features third-party-backends binary
#   kvm                — requires /dev/kvm
#
# Usage:
#   HERMIT_BIN=<third-party-backends release binary> collect-fullcorpus.sh
#     [--backends b1,b2,...]   restrict to these (default: auto-detect)
#     [--par N]                parallelism (default: 16)
#     [--no-assert]            measure only; do not fail on ratchet regression
#     [--assert-only PATH]     re-check the ratchet against an EXISTING scorecard
#                              CSV and exit; runs no guests. Lets a stale-floor or
#                              regression verdict be re-derived in a second instead
#                              of re-sweeping for hours.
#     [--out PATH]             output CSV (default: fullcorpus-scorecard.csv)
#     [--self-check-env-pin]    bracket the generated Hermit argv; run no guests
#     [--self-check-containment] bracket probe routing + timeout/refusal mapping; run no guests
#     [--self-check-corpus-sources] bracket complete and missing source inventories; run no guests
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Canonicalize repo roots: the e9patch AOT rewriter REJECTS any guest path
# containing parent ("..") components, so a relative "$here/../hermit" would make
# every e9patch cell fail "guest path cannot contain parent components" — a false
# 0/200 regression. realpath -m strips the "..".
HROOT="$(realpath -m "${HERMIT_REPO:-$here/../hermit}")"
RROOT="$(realpath -m "${REVERIE_REPO:-$here/../reverie}")"
BIN="${HERMIT_BIN:-$HROOT/target/release/hermit}"
BUILD="$(realpath -m "${FULLCORPUS_BUILD:-$HROOT/ignored/kvm-fullcorpus}")"   # gitignored guest build tree (shared)
CORPUS_C="${CORPUS_C:-$here/corpus/corpus-c.tsv}"
CORPUS_NONC="${CORPUS_NONC:-$here/corpus/corpus-nonc.tsv}"
OUT="$here/fullcorpus-scorecard.csv"
PAR="${PAR:-16}"
TMO_RUN="${TMO_RUN:-90}"
TMO_VERIFY="${TMO_VERIFY:-120}"
BOX_RUN="${HERMIT_BOX_RUN:-$here/../scripts/hermit-box-run}"
backends_arg=""
do_assert=1
assert_only=""
self_check_env_pin=0
self_check_containment=0

# Run one Hermit leg in a transient cgroup and reap that ENTIRE cgroup on exit.
# GNU timeout bounded the foreground Hermit process but did not contain a KVM
# supervisor which moved into its own process group and reparented to PID 1.
# On 2026-08-09 the collector correctly recorded two 120s TIMEOUT rows, then
# four escaped KVM supervisors kept logging sched_yield for another 11-12h.
# A process-group kill cannot close that hole; cgroup.kill does.
bounded_run() { # $1=seconds $2=diag-log $3=label ; rest=command argv
  local bound="$1" diag="$2" label="$3" rc=0; shift 3
  : >"$diag"
  "$BOX_RUN" --passthrough --cpu-budget "$bound" --wall "$bound" \
    --label "$label" --diag-log "$diag" -- "$@" || rc=$?
  if [ "$rc" = 3 ]; then
    # A row measured without the promised containment would make the same leak
    # possible again. Mark the parallel sweep globally refused; never publish it.
    : >"$ROWS/BOXING_REFUSED"
    return 125
  fi
  if [ "$rc" = 137 ] && grep -qE '(^| )((CPU|WALL)-TIMEOUT):' "$diag"; then
    return 124
  fi
  return "$rc"
}

# Probe a backend through the SAME whole-subtree containment used by measured
# cells. A functional probe is still a real Hermit execution: GNU timeout alone
# cannot reap a KVM supervisor which reparents or creates a new process group.
# This is a functional probe, not clap acceptance: a compiled feature backend
# can still be unusable on this host (for example, e9patch without its rewriter).
have_backend() { # $1 = backend name; probes the binary's --backend enum + host
  local b="$1" rc=0
  case "$b" in
    ptrace|liteinst) return 0 ;;
    kvm) [ -e /dev/kvm ] || return 1 ;;
  esac
  bounded_run 30 "$ROWS/probe-$b.box.log" "compat.probe.$b" \
    "$BIN" --backend "$b" run -- /bin/true >/dev/null 2>&1 || rc=$?
  [ "$rc" = 0 ]
}

self_check_subtree_containment_adapter() {
  local tmp fake saved_box saved_bin rc probe_calls
  tmp="$(mktemp -d)" || return 1
  fake="$tmp/fake-box"
  cat >"$fake" <<'EOF'
#!/usr/bin/env bash
diag=""
while [ $# -gt 0 ]; do
  case "$1" in
    --diag-log) diag="$2"; shift 2 ;;
    --) shift; break ;;
    *) shift ;;
  esac
done
[ -n "${FAKE_BOX_CALLS:-}" ] && printf '%s\n' "$*" >>"$FAKE_BOX_CALLS"
[ -n "$diag" ] && [ -n "${FAKE_BOX_MARKER:-}" ] && printf '%s\n' "$FAKE_BOX_MARKER" >>"$diag"
exit "${FAKE_BOX_RC:-0}"
EOF
  chmod +x "$fake"
  saved_box="$BOX_RUN"; saved_bin="$BIN"
  BOX_RUN="$fake"; BIN=/planted/hermit; ROWS="$tmp/rows"; mkdir -p "$ROWS"

  probe_calls="$tmp/probe.calls"
  FAKE_BOX_RC=0 FAKE_BOX_MARKER='' FAKE_BOX_CALLS="$probe_calls" have_backend dbi || {
    BOX_RUN="$saved_box"; BIN="$saved_bin"; rm -rf "$tmp"; return 1
  }
  if [ "$(wc -l <"$probe_calls")" != 1 ] ||
     ! grep -Fxq '/planted/hermit --backend dbi run -- /bin/true' "$probe_calls"; then
    BOX_RUN="$saved_box"; BIN="$saved_bin"; rm -rf "$tmp"; return 1
  fi

  rm -f "$ROWS/BOXING_REFUSED"
  FAKE_BOX_RC=3 FAKE_BOX_MARKER='' FAKE_BOX_CALLS="$probe_calls" have_backend dbi; rc=$?
  if [ "$rc" != 1 ] || [ ! -e "$ROWS/BOXING_REFUSED" ]; then
    BOX_RUN="$saved_box"; BIN="$saved_bin"; rm -rf "$tmp"; return 1
  fi
  rm -f "$ROWS/BOXING_REFUSED"

  FAKE_BOX_RC=137 FAKE_BOX_MARKER='WALL-TIMEOUT: planted' \
    bounded_run 1 "$tmp/wall.diag" test -- true; rc=$?
  [ "$rc" = 124 ] || { BOX_RUN="$saved_box"; BIN="$saved_bin"; rm -rf "$tmp"; return 1; }

  FAKE_BOX_RC=137 FAKE_BOX_MARKER='' \
    bounded_run 1 "$tmp/signal.diag" test -- true; rc=$?
  [ "$rc" = 137 ] || { BOX_RUN="$saved_box"; BIN="$saved_bin"; rm -rf "$tmp"; return 1; }

  FAKE_BOX_RC=3 FAKE_BOX_MARKER='' \
    bounded_run 1 "$tmp/refused.diag" test -- true; rc=$?
  if [ "$rc" != 125 ] || [ ! -e "$ROWS/BOXING_REFUSED" ]; then
    BOX_RUN="$saved_box"; BIN="$saved_bin"; rm -rf "$tmp"; return 1
  fi

  BOX_RUN="$saved_box"; BIN="$saved_bin"; rm -rf "$tmp"
  echo "self-check: backend probe boxed 1/1 and unavailable box marked refusal 1/1; wall/cpu reaps normalize to timeout; ordinary SIGKILL stays distinct; absent cgroup refuses publication"
}

# Build every measured Hermit command in one place. `--base-env host` is the
# default, but that makes the caller's ambient environment part of the guest's
# initial stack and invalidates output comparisons across invocations. Keep the
# exact pin in step with collect-envelope.rs and the DBI/e9patch collectors.
build_hermit_argv() { # $1=array name $2=backend $3=verify(0|1) $4=lane
  local output_name="$1" backend="$2" verify="$3" lane="$4"
  local -n output_ref="$output_name"
  output_ref=("$BIN")
  [ "$backend" = ptrace ] || output_ref+=(--backend "$backend")
  output_ref+=(run --strict)
  [ "$verify" = 0 ] || output_ref+=(--verify)
  [ "$lane" = portable ] && output_ref+=(--no-virtualize-cpuid --max-timeslice=disabled)
  output_ref+=(--base-env minimal -e LC_ALL=C -e TZ=UTC --)
}

# Project just the environment-bearing options before the guest separator and
# require the precise, ordered profile. Used only by the fail-closed self-check.
guest_env_pin_is_exact() { # $1=array name
  local argv_name="$1"
  local -n argv_ref="$argv_name"
  local -a projected=() expected=(--base-env minimal -e LC_ALL=C -e TZ=UTC)
  local separator=-1 index=0
  for ((index=0; index<${#argv_ref[@]}; index++)); do
    if [ "${argv_ref[$index]}" = -- ]; then separator=$index; break; fi
  done
  [ "$separator" -ge 0 ] || return 1
  index=0
  while [ "$index" -lt "$separator" ]; do
    case "${argv_ref[$index]}" in
      --base-env|-e|--env)
        [ $((index + 1)) -lt "$separator" ] || return 1
        projected+=("${argv_ref[$index]}" "${argv_ref[$((index + 1))]}")
        index=$((index + 2))
        ;;
      --base-env=*|--env=*|-e?*)
        projected+=("${argv_ref[$index]}")
        index=$((index + 1))
        ;;
      *) index=$((index + 1)) ;;
    esac
  done
  [ "${#projected[@]}" -eq "${#expected[@]}" ] || return 1
  for ((index=0; index<${#expected[@]}; index++)); do
    [ "${projected[$index]}" = "${expected[$index]}" ] || return 1
  done
}

self_check_guest_env_pin() {
  local -a argv mutation omission reordered
  local backend verify lane index lc_index=-1 tz_index=-1 positives=0
  for backend in ptrace kvm dbi sabre e9patch liteinst; do
    for verify in 0 1; do
      for lane in portable privileged; do
        build_hermit_argv argv "$backend" "$verify" "$lane"
        guest_env_pin_is_exact argv || {
          echo "self-check: generated argv lacks exact pin: ${argv[*]}" >&2
          return 1
        }
        positives=$((positives + 1))
      done
    done
  done

  mutation=("${argv[@]}")
  for ((index=0; index<${#mutation[@]}; index++)); do
    [ "${mutation[$index]}" = TZ=UTC ] && mutation[$index]=TZ=localtime
  done
  ! guest_env_pin_is_exact mutation || { echo "self-check: mutation accepted" >&2; return 1; }

  omission=("${argv[@]}")
  for ((index=0; index<${#omission[@]}; index++)); do
    [ "${omission[$index]}" = LC_ALL=C ] && lc_index=$index
  done
  [ "$lc_index" -gt 0 ] || return 1
  unset "omission[$lc_index]" "omission[$((lc_index - 1))]"
  omission=("${omission[@]}")
  ! guest_env_pin_is_exact omission || { echo "self-check: omission accepted" >&2; return 1; }

  reordered=("${argv[@]}")
  for ((index=0; index<${#reordered[@]}; index++)); do
    [ "${reordered[$index]}" = LC_ALL=C ] && lc_index=$index
    [ "${reordered[$index]}" = TZ=UTC ] && tz_index=$index
  done
  [ "$lc_index" -ge 0 ] && [ "$tz_index" -ge 0 ] || return 1
  reordered[$lc_index]=TZ=UTC
  reordered[$tz_index]=LC_ALL=C
  ! guest_env_pin_is_exact reordered || { echo "self-check: reordering accepted" >&2; return 1; }

  echo "self-check: $positives generated argv variants accepted; mutation, omission, and reordering refused"
}

check_corpus_sources() { # $1=corpus-c.tsv $2=Hermit root
  local corpus="$1" hroot="$2" id prog cflags extra lane cstate source
  local cells=0 missing_cells=0 cell_missing
  while IFS='|' read -r id prog cflags extra lane cstate; do
    [ -n "$id" ] || continue
    cells=$((cells + 1)); cell_missing=0
    if [ ! -f "$hroot/$prog" ]; then
      printf '  missing-source: %s (%s)\n' "$id" "$prog" >&2
      cell_missing=1
    fi
    for source in $extra; do
      if [ ! -f "$hroot/$source" ]; then
        printf '  missing-source: %s (%s)\n' "$id" "$source" >&2
        cell_missing=1
      fi
    done
    [ "$cell_missing" -eq 0 ] || missing_cells=$((missing_cells + 1))
  done < "$corpus"
  if [ "$missing_cells" -ne 0 ]; then
    printf 'collect-fullcorpus: REFUSED — %s of %s C corpus cells have missing sources; no guests run\n' \
      "$missing_cells" "$cells" >&2
    return 4
  fi
  printf '== corpus sources: %s/%s C cells present ==\n' "$cells" "$cells"
}

self_check_corpus_sources() {
  local tmp complete missing output rc=0
  tmp="$(mktemp -d)" || return 1
  complete="$tmp/complete.tsv"; missing="$tmp/missing.tsv"
  mkdir -p "$tmp/root/tests"
  : > "$tmp/root/tests/one.c"
  : > "$tmp/root/tests/helper.c"
  printf '%s\n' \
    'one|tests/one.c|||portable|disabled' \
    'two|tests/one.c||tests/helper.c|portable|disabled' > "$complete"
  output="$(check_corpus_sources "$complete" "$tmp/root" 2>&1)" || rc=$?
  if [ "$rc" -ne 0 ] || [ "$output" != '== corpus sources: 2/2 C cells present ==' ]; then
    rm -rf "$tmp"
    echo "self-check: complete corpus was not accepted: rc=$rc output=$output" >&2
    return 1
  fi

  printf '%s\n' \
    'one|tests/missing.c|||portable|disabled' \
    'two|tests/one.c||tests/missing-helper.c|portable|disabled' > "$missing"
  rc=0; output="$(check_corpus_sources "$missing" "$tmp/root" 2>&1)" || rc=$?
  if [ "$rc" -ne 4 ] ||
     ! grep -Fq 'missing-source: one (tests/missing.c)' <<<"$output" ||
     ! grep -Fq 'missing-source: two (tests/missing-helper.c)' <<<"$output" ||
     ! grep -Fq '2 of 2 C corpus cells have missing sources' <<<"$output"; then
    rm -rf "$tmp"
    echo "self-check: missing corpus was not refused cell by cell: rc=$rc output=$output" >&2
    return 1
  fi
  rm -rf "$tmp"
  echo "self-check: complete corpus accepted; two missing cells refused and named"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --backends) backends_arg="$2"; shift 2 ;;
    --par) PAR="$2"; shift 2 ;;
    --no-assert) do_assert=0; shift ;;
    --assert-only) assert_only="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --self-check-env-pin) self_check_env_pin=1; shift ;;
    --self-check-containment) self_check_containment=1; shift ;;
    --self-check-corpus-sources) self_check_corpus_sources=1; shift ;;
    -h|--help) grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "collect-fullcorpus: unknown arg $1" >&2; exit 2 ;;
  esac
done

if [ "$self_check_env_pin" = 1 ]; then
  self_check_guest_env_pin
  exit $?
fi
if [ "$self_check_containment" = 1 ]; then
  self_check_subtree_containment_adapter
  exit $?
fi
if [ "${self_check_corpus_sources:-0}" = 1 ]; then
  self_check_corpus_sources
  exit $?
fi

[ -n "$assert_only" ] || [ -x "$BIN" ] || { echo "collect-fullcorpus: HERMIT_BIN not executable: $BIN" >&2; exit 2; }
[ -n "$assert_only" ] || [ -x "$BOX_RUN" ] || { echo "collect-fullcorpus: cgroup box not executable: $BOX_RUN" >&2; exit 3; }
[ -f "$CORPUS_C" ] || { echo "collect-fullcorpus: missing $CORPUS_C" >&2; exit 2; }
[ -n "$assert_only" ] || check_corpus_sources "$CORPUS_C" "$HROOT" || exit $?

HSHA="$(git -C "$HROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
RSHA="$(git -C "$RROOT" rev-parse HEAD 2>/dev/null || echo unknown)"

# --- backend auto-detection --------------------------------------------------
ALL_BACKENDS="ptrace kvm dbi sabre e9patch liteinst"
# --- per-backend ratchet baselines (green-stays-green floor) ------------------
# Existing 205-cell floors were measured at hermit 82a8e853; the thirty performance
# cells were measured with the same binary and uniform lane flags.
# A backend that drops below its floor fails the gate. New backends default 0.
baseline() {
  # Combined floors over the full 235-cell corpus; det =
  # <backend> --strict --verify exits 0. A backend dropping
  # below its floor fails the gate.
  case "$1" in
    ptrace) echo 214 ;;
    kvm) echo 160 ;;
    liteinst) echo 118 ;;
    dbi) echo 190 ;;
    sabre) echo 199 ;;
    e9patch) echo 214 ;;
    *) echo 0 ;;
  esac
}

# The floors above were measured over a 235-cell corpus. Carry that denominator
# WITH them: a floor is meaningless against a different population, and four of
# the six are numerically unreachable once the corpus shrinks (e9patch's 214
# cannot be met by 205 cells no matter how green the run is). Comparing anyway
# reported four "REGRESSION" lines that were denominator errors, not findings --
# the failure mode this gate exists to prevent, committed by the gate itself.
FLOOR_CORPUS=235

# assert_ratchet <csv> <backend>... -> 0 green | 1 real regression | 4 not comparable
assert_ratchet() {
  local csv="$1"; shift
  local fail=0 incomparable=0 backend det tot floor
  echo "== green-stays-green ratchet (per-backend det floor; floors measured over ${FLOOR_CORPUS} cells) =="
  for backend in "$@"; do
    det=$(awk -F, -v b="$backend" 'NR>1 && $11==b && $14=="1"{n++} END{print n+0}' "$csv")
    tot=$(awk -F, -v b="$backend" 'NR>1 && $11==b{n++} END{print n+0}' "$csv")
    floor=$(baseline "$backend")
    [ "$tot" -eq 0 ] && { echo "  n/a: $backend contributed no rows (not measurable here)"; continue; }
    if [ "$tot" -lt "$FLOOR_CORPUS" ]; then
      incomparable=1
      printf '  NOT-COMPARABLE: %s det %s/%s vs floor %s — floor was measured over %s cells, %s are missing here%s\n' \
        "$backend" "$det" "$tot" "$floor" "$FLOOR_CORPUS" "$((FLOOR_CORPUS - tot))" \
        "$([ "$floor" -gt "$tot" ] && echo " (and $floor > $tot, so it is unreachable by construction)")" >&2
    elif [ "$det" -lt "$floor" ]; then
      echo "  REGRESSION: $backend det $det/$tot < floor $floor" >&2; fail=1
    else
      echo "  OK: $backend det $det/$tot (floor $floor)"
    fi
  done
  [ "$fail" -ne 0 ] && return 1
  [ "$incomparable" -ne 0 ] && return 4
  return 0
}

if [ -n "$assert_only" ]; then
  [ -r "$assert_only" ] || { echo "collect-fullcorpus: cannot read $assert_only" >&2; exit 2; }
  ao_backends=$(awk -F, 'NR>1{print $11}' "$assert_only" | sort -u | tr '\n' ' ')
  [ -n "${ao_backends// /}" ] || { echo "collect-fullcorpus: $assert_only has no rows — a no-result, not a pass" >&2; exit 3; }
  assert_ratchet "$assert_only" $ao_backends; exit $?
fi

[ -n "$backends_arg" ] && ALL_BACKENDS="${backends_arg//,/ }"
ROWS="$(mktemp -d)"
if ! bounded_run 1 "$ROWS/preflight.box.log" compat.preflight true; then
  echo "collect-fullcorpus: REFUSED — cgroup containment preflight failed; no Hermit process started and no scorecard published" >&2
  rm -rf "$ROWS"
  exit 3
fi
DETECTED=""
for b in $ALL_BACKENDS; do
  if have_backend "$b"; then
    DETECTED="$DETECTED $b"
  elif [ -e "$ROWS/BOXING_REFUSED" ]; then
    echo "collect-fullcorpus: REFUSED — cgroup containment unavailable during backend probe; no guests run and no scorecard published" >&2
    rm -rf "$ROWS"
    exit 3
  else
    echo "  backend $b: NOT available here (recorded n/a)"
  fi
done
DETECTED="${DETECTED# }"
echo "== full-corpus gate: hermit=$HSHA backends=[$DETECTED] par=$PAR =="
echo "   corpus = $(wc -l <"$CORPUS_C") C + $(grep -vc '^#' "$CORPUS_NONC") non-C cells"


# RFC4180-quote a field, but only when it needs it, so today's comma-free
# reasons stay byte-identical and a future free-text reason cannot silently
# shift every column after it (`reason` is mid-row here, ahead of
# verify_compare/det_tier, so an unquoted comma misattributes those too).
csv_field() {
  case $1 in
    *'"'*|*,*|*$'\n'*|*$'\r'*) printf '"%s"' "${1//\"/\"\"}" ;;
    *) printf '%s' "$1" ;;
  esac
}
export -f csv_field

HDR=$(jq -er '.variants["fullcorpus-scorecard.csv"].columns | join(",")' \
  "$here/scorecard-schema.json") || {
  echo "collect-fullcorpus: cannot read the shared fullcorpus scorecard row" >&2
  exit 2
}
RUN_UTC="@$(date +%s)"

# --- one (backend,cell) measurement ------------------------------------------
# Classify a non-zero --verify exit into an OUTCOME that says what actually
# happened, instead of collapsing every non-zero exit to "diverge".
#
# "diverge" is a determinism verdict and must be reserved for one. Two other
# things produce a non-zero exit and are NOT determinism failures:
#
#   usage        the guest printed its usage banner, i.e. THIS HARNESS invoked
#                it wrong. This is a positive control and must stay at zero:
#                a non-zero count means the guest-argument channel below is
#                broken again (rrnewton/hermit#1815), not that a backend
#                regressed.
#   unsupported  the backend refused the operation (ENOTSUPP). A fail-closed
#                refusal is honest behaviour; labelling it "diverge" hides that
#                the backend declined rather than computed a wrong answer.
#
# $1=backend $2=exit-code $3=run stdout file $4=run stderr file $5=verify stderr file
# Echoes "<outcome> <reason>".
classify_verify_failure() {
  local backend="$1" ve="$2" run_out="$3" run_err="$4" verify_err="$5"
  if grep -qiE '^[[:space:]]*usage:' "$run_out" "$run_err" 2>/dev/null; then
    echo "usage $backend-verify-usage-exit$ve"
    return
  fi
  if grep -qE 'ENOTSUPP|Operation is not supported' "$verify_err" "$run_err" 2>/dev/null; then
    echo "unsupported $backend-verify-unsupported-exit$ve"
    return
  fi
  echo "diverge $backend-verify-fail-exit$ve"
}

measure() { # $1=backend $2=cell-dir(holds ptv.out ref) $3=lane $4=id ; rest=guest argv
  local backend="$1" cell="$2" lane="$3" id="$4"; shift 4
  local -a gcmd=("$@")
  local bucket="${id%%/*}"
  local -a run_cmd verify_cmd
  build_hermit_argv run_cmd "$backend" 0 "$lane"
  build_hermit_argv verify_cmd "$backend" 1 "$lane"
  export LC_ALL=C TZ=UTC
  local re=0
  # ptrace: TWO runs.
  #  (1) plain --strict  -> ptv.out = the PARITY REFERENCE (real guest stdout).
  #      NB: --strict --verify does an internal DOUBLE-run and emits NO guest
  #      stdout to the parent, so a --verify capture would be 0 bytes and every
  #      backend's parity-vs-reference would be spuriously ~0. Reference MUST be
  #      plain --strict, matching how the backend .out files are produced.
  #  (2) --strict --verify -> det signal (L2 self-verify exit code).
  if [ "$backend" = ptrace ]; then
    local t0 t1 dur re ve ohash det outcome reason
    bounded_run "$TMO_RUN" "$cell/ptrace-run.box.log" "compat.${id//\//_}.ptrace-run" \
      "${run_cmd[@]}" "${gcmd[@]}" >"$cell/ptv.out" 2>"$cell/ptv.err"; re=$?
    ohash=$(sha256sum "$cell/ptv.out" | cut -c1-64)
    t0=$(date +%s%3N)
    bounded_run "$TMO_VERIFY" "$cell/ptrace-verify.box.log" "compat.${id//\//_}.ptrace-verify" \
      "${verify_cmd[@]}" "${gcmd[@]}" >/dev/null 2>"$cell/ptvv.err"; ve=$?
    t1=$(date +%s%3N); dur=$((t1-t0))
    if [ "$ve" = 0 ]; then det=1; outcome=pass; reason="";
    elif [ "$ve" = 124 ]; then det=0; outcome=timeout; reason="ptrace-verify-timeout-${TMO_VERIFY}s";
    else det=0
      read -r outcome reason <<<"$(classify_verify_failure ptrace "$ve" "$cell/ptv.out" "$cell/ptv.err" "$cell/ptvv.err")"
    fi
    [ "$outcome" = usage ] && echo "  HARNESS-ERROR usage banner from $id (ptrace): guest argument channel is wrong" >&2
    # a failed plain --strict reference is unusable for parity: mark it so
    # downstream backends record parity="" (unmeasured), never a false match
    # (an empty-but-valid reference must still be comparable, hence a marker
    # file rather than relying on emptiness).
    if [ "$re" != 0 ]; then
      : >"$cell/ptv.out"; ohash=$(sha256sum "$cell/ptv.out" | cut -c1-64)
      : >"$cell/ptv.fail"; [ -z "$reason" ] && reason="ptrace-run-fail-exit$re"
    else
      rm -f "$cell/ptv.fail"
    fi
    local verify_compare="" det_tier=""
    if [ "$det" = 1 ]; then verify_compare=stripped; det_tier=stripped-uncounted; fi
    echo "fullcorpus,$RUN_UTC,$HSHA,$RSHA,false,expansion,$lane,$bucket,$id,verify,ptrace,expansion,$outcome,$det,,$ohash,$dur,,$(csv_field "$reason"),$verify_compare,,,$det_tier,,$ohash,,,,,,,,unqualified-stdout-only,,,,0,0" > "$ROWS/ptrace_${id//\//_}.row"
    return
  fi
  # non-ptrace backend: strict (for parity) + strict --verify (for det)
  bounded_run "$TMO_RUN" "$cell/$backend-run.box.log" "compat.${id//\//_}.$backend-run" \
    "${run_cmd[@]}" "${gcmd[@]}" >"$cell/$backend.out" 2>"$cell/$backend.err"; re=$?
  local t0 t1 dur ve bhash phash det outcome reason parity ohash
  t0=$(date +%s%3N)
  bounded_run "$TMO_VERIFY" "$cell/$backend-verify.box.log" "compat.${id//\//_}.$backend-verify" \
    "${verify_cmd[@]}" "${gcmd[@]}" >"$cell/${backend}v.out" 2>"$cell/${backend}v.err"; ve=$?
  t1=$(date +%s%3N); dur=$((t1-t0))
  bhash=$(sha256sum "$cell/$backend.out" | cut -c1-64); ohash="$bhash"
  if [ "$ve" = 0 ]; then det=1; outcome=pass; reason="";
  elif [ "$ve" = 124 ]; then det=0; outcome=timeout; reason="$backend-verify-timeout-${TMO_VERIFY}s";
  else det=0
    read -r outcome reason <<<"$(classify_verify_failure "$backend" "$ve" "$cell/$backend.out" "$cell/$backend.err" "$cell/${backend}v.err")"
  fi
  [ "$outcome" = usage ] && echo "  HARNESS-ERROR usage banner from $id ($backend): guest argument channel is wrong" >&2
  if [ ! -f "$cell/ptv.out" ] || [ -f "$cell/ptv.fail" ]; then parity="";  # ref missing/failed -> unmeasured
  elif [ "$re" != 0 ]; then parity=0; [ -z "$reason" ] && reason="$backend-run-fail-exit$re";
  else
    phash=$(sha256sum "$cell/ptv.out" | cut -c1-64)
    [ "$bhash" = "$phash" ] && parity=1 || parity=0
  fi
  local verify_compare="" det_tier="" legacy_parity="" ref_hash=""
  if [ "$det" = 1 ]; then verify_compare=stripped; det_tier=stripped-uncounted; fi
  if [ -n "$parity" ]; then legacy_parity="stdout_parity:$parity"; fi
  if [ -f "$cell/ptv.out" ] && [ ! -f "$cell/ptv.fail" ]; then ref_hash=$(sha256sum "$cell/ptv.out" | cut -c1-64); fi
  echo "fullcorpus,$RUN_UTC,$HSHA,$RSHA,false,expansion,$lane,$bucket,$id,verify,$backend,expansion,$outcome,$det,,$ohash,$dur,,$(csv_field "$reason"),$verify_compare,,,$det_tier,$legacy_parity,$ref_hash,,,,,,,,unqualified-stdout-only,,,,0,0" > "$ROWS/${backend}_${id//\//_}.row"
}
export -f build_hermit_argv bounded_run measure classify_verify_failure
export BIN BOX_RUN ROWS RUN_UTC HSHA RSHA TMO_RUN TMO_VERIFY

# --- compile C guests once (shared build tree) -------------------------------
echo "== compiling/reusing C guests =="
mkdir -p "$BUILD"
build_failures=0
# cstate names the last corpus column so the split stays self-documenting; unread here.
# shellcheck disable=SC2034
while IFS='|' read -r id prog cflags extra lane cstate; do
  [ -n "$id" ] || continue
  key="${id//\//_}"; cell="$BUILD/$key"; mkdir -p "$cell"
  guest="$cell/guest"
  [ -x "$guest" ] && continue
  extra_abs=""
  for e in $extra; do extra_abs="$extra_abs $HROOT/$e"; done
  # shellcheck disable=SC2086
  if ! cc -std=c11 -O2 -g -Wall -Wextra -Werror $cflags "$HROOT/$prog" $extra_abs -o "$guest" 2>"$cell/cc.err"; then
    echo "  build-fail: $id (diagnostic: $cell/cc.err)" >&2
    build_failures=$((build_failures + 1))
  fi
done < "$CORPUS_C"
if [ "$build_failures" -ne 0 ]; then
  echo "collect-fullcorpus: REFUSED — $build_failures C corpus cells did not compile; no guests run" >&2
  exit 4
fi

# --- guest arguments, sourced from the e2e manifests -------------------------
# Some corpus guests REQUIRE an argument (a scenario name, a fixture path). Run
# bare they print a usage banner and exit non-zero, which this collector used to
# record as `<backend>-verify-fail-exit1` -- indistinguishable from a real
# determinism divergence. That was rrnewton/hermit#1815: nine cells were false
# reds in EVERY backend column, and ptrace's true non-green count was 12, not 21.
#
# The arguments are read from the e2e manifests' per-backend `guest_args`, via
# the product's own manifest reader, rather than from a column in corpus-c.tsv.
# A second hand-maintained list is exactly how the manifests and this corpus
# would drift back apart.
#
# Fail closed: a harness that cannot establish how to invoke its guests must not
# quietly record a corpus-wide red.
GARGS_DIR="$BUILD/guest-args"
rm -rf "$GARGS_DIR"; mkdir -p "$GARGS_DIR"
gargs_tsv="$GARGS_DIR/declared.tsv"
# TRANSITION: `--guest-args` arrives with rrnewton/hermit#1833. Against a hermit
# checkout that predates it, degrade LOUDLY instead of exiting -- this collector
# is shared, and hard-failing it would block every other lane on a flag that has
# not landed yet. Degrading is safe because the fail-closed property lives at the
# cell level: any guest invoked without a required argument prints a usage banner
# and is classified `usage`, never `pass` and never `diverge`. The measurement can
# be incomplete, but it can no longer be silently wrong.
gargs_available=1
if ! ( cd "$HROOT" && ./scripts/manifest-to-commands.rs --guest-args ) >"$gargs_tsv" 2>"$GARGS_DIR/err"; then
  gargs_available=0
  : >"$gargs_tsv"
  echo "WARNING: cannot read declared guest arguments from" >&2
  echo "         $HROOT/scripts/manifest-to-commands.rs --guest-args" >&2
  sed 's/^/         /' "$GARGS_DIR/err" >&2
  echo "         Continuing WITHOUT guest arguments. Every argument-taking guest" >&2
  echo "         will be invoked bare and recorded as outcome=usage, NOT as a" >&2
  echo "         determinism failure (rrnewton/hermit#1815). Those cells are" >&2
  echo "         UNMEASURED, not red. Land hermit#1833 to measure them." >&2
fi
gargs_n=0
while IFS=$'\t' read -r ga_id ga_mode ga_backend ga_rest; do
  [ -n "$ga_id" ] || continue
  [ "$ga_mode" = verify ] || continue   # this collector measures the verify mode only
  printf '%s\n' "$ga_rest" | tr '\t' '\n' > "$GARGS_DIR/${ga_id//\//_}.$ga_backend"
  gargs_n=$((gargs_n+1))
  # The manifest may only declare `guest_args` for a backend it ENABLES, but this
  # collector deliberately measures every backend in expansion -- i.e. it asks
  # "can backend X do what ptrace does on this cell?". The reference invocation
  # for that question is ptrace's, so ptrace's vector (and only ptrace's) is
  # reused when the measured backend declares none of its own.
  #
  # Deliberately NOT a fall back to "any declared vector": c-programs/madvise-
  # determinism declares `--kvm` for kvm alone and nothing for ptrace, so a
  # promiscuous fallback would hand `--kvm` to every other backend. Absence of a
  # ptrace declaration stays absence.
  [ "$ga_backend" = ptrace ] && cp "$GARGS_DIR/${ga_id//\//_}.$ga_backend" "$GARGS_DIR/${ga_id//\//_}.__reference"
done < "$gargs_tsv"
if [ "$gargs_available" = 1 ]; then
  echo "== guest arguments: $gargs_n declared (verify mode) from $HROOT manifests =="
else
  echo "== guest arguments: UNAVAILABLE -- argument-taking cells will report outcome=usage ==" >&2
fi
export GARGS_DIR


# --- run every detected backend, ptrace FIRST (writes the parity reference) ---
order="ptrace"
for b in $DETECTED; do [ "$b" = ptrace ] || order="$order $b"; done
# sweep_backend BACKEND PAR REPAIR
#   REPAIR=0 : measure every cell (parallel storm).
#   REPAIR=1 : re-measure ONLY cells whose current row is a --verify TIMEOUT.
#     A 120s timeout under a 24-way parallel storm is a load/scheduling artifact,
#     not a determinism verdict; a second, serial (PAR=1) pass gives each such
#     cell a fair shake. A genuine hang (e.g. a dbi preemption-ceiling cell) times
#     out again and correctly stays failed; only load flake recovers. This keeps
#     the ratchet trustworthy on a contended box without lowering any det floor.
sweep_backend() {
  local backend="$1" par="$2" repair="$3"
  xargs -a "$CORPUS_C" -d '\n' -P "$par" -I{} bash -c '
    IFS="|" read -r id prog cflags extra lane cstate <<<"$1"
    key="${id//\//_}"; cell="'"$BUILD"'/$key"
    [ -x "$cell/guest" ] || exit 0
    if [ "'"$repair"'" = 1 ]; then
      row="$ROWS/'"$backend"'_${id//\//_}.row"
      case "$(cut -d, -f13 "$row" 2>/dev/null)" in timeout) ;; *) exit 0;; esac
    fi
    gargs=(); ga_file="$GARGS_DIR/$key.'"$backend"'"
    [ -f "$ga_file" ] || ga_file="$GARGS_DIR/$key.__reference"
    [ -f "$ga_file" ] && mapfile -t gargs < "$ga_file"
    measure "'"$backend"'" "$cell" "$lane" "$id" "$cell/guest" ${gargs[@]+"${gargs[@]}"}
  ' _ {}
  xargs -a "$CORPUS_NONC" -d '\n' -P "$par" -I{} bash -c '
    line="$1"; case "$line" in \#*) exit 0;; esac
    id="${line%%|*}"; rest="${line#*|}"; lane="${rest%%|*}"; cmd="${rest#*|}"
    cmd="${cmd//HERMITROOT/'"$HROOT"'}"
    key="${id//\//_}"; cell="'"$BUILD"'/nonc_$key"; mkdir -p "$cell"
    if [ "'"$repair"'" = 1 ]; then
      row="$ROWS/'"$backend"'_${id//\//_}.row"
      case "$(cut -d, -f13 "$row" 2>/dev/null)" in timeout) ;; *) exit 0;; esac
    fi
    # shellcheck disable=SC2086
    measure "'"$backend"'" "$cell" "$lane" "$id" $cmd
  ' _ {}
  [ ! -e "$ROWS/BOXING_REFUSED" ]
}
export ROWS
for backend in $order; do
  echo "== sweep backend=$backend =="
  if ! sweep_backend "$backend" "$PAR" 0; then
    echo "collect-fullcorpus: REFUSED — cgroup containment unavailable; no scorecard published" >&2
    rm -rf "$ROWS"
    exit 3
  fi
  nto=$(grep -l ',timeout,' "$ROWS/${backend}"_*.row 2>/dev/null | wc -l)
  if [ "$nto" -gt 0 ]; then
    echo "   repair: re-running $nto timed-out $backend cell(s) serially"
    if ! sweep_backend "$backend" 1 1; then
      echo "collect-fullcorpus: REFUSED — cgroup containment unavailable during repair; no scorecard published" >&2
      rm -rf "$ROWS"
      exit 3
    fi
  fi
done

{ echo "$HDR"; cat "$ROWS"/*.row; } > "$OUT"
if ! "$here/check-scorecard-schema.rs" --file "$OUT" \
      --variant fullcorpus-scorecard.csv >/dev/null; then
  echo "collect-fullcorpus: REFUSED — emitted rows do not match the shared scorecard row" >&2
  rm -rf "$ROWS"
  exit 2
fi
rm -rf "$ROWS"
echo "== full-corpus scorecard written: $OUT =="

# --- ratchet assert + render -------------------------------------------------
assert_ratchet "$OUT" $order; ratchet_rc=$?

echo "== rendered scorecard =="
"$here/render-scorecard.rs" --csv "$OUT" --all --backends kvm,dbi,sabre,liteinst,e9patch || true

if [ "$do_assert" -eq 1 ] && [ "$ratchet_rc" -eq 1 ]; then
  echo "collect-fullcorpus: FAILED — backend regressed below ratchet floor" >&2
  exit 1
fi
if [ "$do_assert" -eq 1 ] && [ "$ratchet_rc" -eq 4 ]; then
  # Fail CLOSED, but with the right cause. A gate that cannot evaluate its own
  # floor is a NO-RESULT, never a green -- and never a "regression" either.
  echo "collect-fullcorpus: REFUSED — the ratchet cannot be evaluated: this corpus is smaller than the ${FLOOR_CORPUS} cells its floors were measured over." >&2
  echo "  Re-derive the floors against the corpus that actually builds, or restore the missing cells. This is NOT a regression report." >&2
  exit 4
fi
[ "$ratchet_rc" -eq 4 ] && echo "== full-corpus gate: NOT COMPARABLE (floors stale vs corpus; --no-assert so not enforced) =="
scorecard_row_count=$(( $(wc -l < "$OUT") - 1 ))
[ "$ratchet_rc" -eq 0 ] && echo "== full-corpus raw floor PASS; comparison_tier=unqualified-stdout-only; strict greens emitted: 0/$scorecard_row_count =="
