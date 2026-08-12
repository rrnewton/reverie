#!/usr/bin/env bash
set -uo pipefail

PARENT=/home/newton/work/dev-hermit
HROOT=/home/newton/work/dev-hermit/worktrees/codex-tgaudit/hermit
RROOT=/home/newton/work/dev-hermit/worktrees/codex-tgaudit/reverie
MATRIX_ROOT=/home/newton/work/dev-hermit/worktrees/compat-envelope-program/hermit
DRIVER="$MATRIX_ROOT/ci/compat/backend-matrix.rs"
CORPUS="$MATRIX_ROOT/ci/compat/corpus.json"
OUT="$HROOT/ignored/compat/repeat-sweep"
INSTALL_DIR="$OUT/install_pkg"
E9TOOL="$OUT/e9patch-pinned/e9tool"
E9PATCH="$OUT/e9patch-pinned/e9patch"
INSTALL_RESOURCE_SOURCE_HERMIT_SHA="b5a4748e87a6bc32bc3d87935ab571c1c57c25ad"
FIXTURES="$OUT/fixtures"
BIN="$HROOT/target/release/hermit"
RUNS=${COMPAT_SWEEP_RUNS:-5}
EXPECTED_DRIVER_SHA256=5e2fdb653526999d8bf763007d3fbbacd7c73f074a10feb4864b09ff224f8927
EXPECTED_CORPUS_SHA256=b22a65391cc223086c86ab22377330ae4993adbd9d7426a2a4c2ef54fc678687

mkdir -p "$OUT/logs" "$OUT/contention" "$OUT/localhost"
exec > >(tee -a "$OUT/producer.log") 2>&1

printf 'started_utc=%s\n' "$(date -u +%FT%TZ)"
printf 'host=%s\n' "$(hostname -s)"
printf 'hermit_sha=%s\n' "$(git -C "$HROOT" rev-parse HEAD)"
printf 'reverie_sha=%s\n' "$(git -C "$RROOT" rev-parse HEAD)"
printf 'matrix_driver_commit=%s\n' "$(git -C "$MATRIX_ROOT" rev-parse HEAD)"
printf 'matrix_driver_sha256=%s\n' "$(sha256sum "$DRIVER" | cut -d' ' -f1)"
printf 'matrix_corpus_sha256=%s\n' "$(sha256sum "$CORPUS" | cut -d' ' -f1)"
printf 'e9tool_sha256=%s\n' "$(sha256sum "$E9TOOL" | cut -d' ' -f1)"
printf 'e9patch_sha256=%s\n' "$(sha256sum "$E9PATCH" | cut -d' ' -f1)"
  printf 'install_resource_source_hermit_sha=%s\n' "$INSTALL_RESOURCE_SOURCE_HERMIT_SHA"
printf 'liteinst_runtime_sha256=%s\n' "$(sha256sum "$INSTALL_DIR/rsrcs/libreverie_liteinst.so" | cut -d' ' -f1)"
printf 'sabre_binary_sha256=%s\n' "$(sha256sum "$INSTALL_DIR/rsrcs/sabre" | cut -d' ' -f1)"
printf 'sabre_plugin_sha256=%s\n' "$(sha256sum "$INSTALL_DIR/rsrcs/libdetcore_sabre.so" | cut -d' ' -f1)"
[[ $(sha256sum "$DRIVER" | cut -d' ' -f1) == "$EXPECTED_DRIVER_SHA256" ]] || {
  printf 'refused=matrix-driver-changed\n'; exit 77;
}
[[ $(sha256sum "$CORPUS" | cut -d' ' -f1) == "$EXPECTED_CORPUS_SHA256" ]] || {
  printf 'refused=matrix-corpus-changed\n'; exit 78;
}

# validate-lock admits this child only after the prior local producer is gone.
# Independently require all visible self-hosted runners to be idle at the
# actual start boundary; a queued portable GitHub-hosted job is not contention
# on these runners, while any nonzero `busy=` count is.
{
  printf 'snapshot_utc=%s\n' "$(date -u +%FT%TZ)"
  with-proxy gh api repos/rrnewton/hermit/actions/runners
  hermit_runners_rc=$?
  printf '\n'
  with-proxy gh api repos/rrnewton/reverie/actions/runners
  reverie_runners_rc=$?
  printf '\n'
  printf 'runner_health_rc=%s,%s\n' "$hermit_runners_rc" "$reverie_runners_rc"
} >"$OUT/contention/admission.txt" 2>&1
if ! grep -q '^runner_health_rc=0,0$' "$OUT/contention/admission.txt"; then
  printf 'admission_refused=runner-health-no-result\n'
  exit 76
fi
if grep -q '"busy":true' "$OUT/contention/admission.txt"; then
  printf 'admission_refused=self-hosted-runner-busy\n'
  exit 75
fi
printf 'admission_local_validator_predecessor=none (exclusive validate-lock acquired)\n'
printf 'admission_self_hosted_busy=0\n'

# Build the exact checked-out Hermit once while holding the sole local
# validation admission. This is preparation, not a timed corpus attempt.
(
  cd "$HROOT"
  cargo build --release -p hermit --bin hermit --features third-party-backends
) 2>&1 | tee "$OUT/logs/build.log"
build_rc=${PIPESTATUS[0]}
if ((build_rc != 0)); then
  printf 'build_failed_rc=%s\n' "$build_rc"
  exit "$build_rc"
fi

"$HROOT/tests/compat/prepare_real_compat_fixtures.sh" "$FIXTURES" \
  >"$OUT/logs/fixtures.log" 2>&1

if [[ ! -s $OUT/localhost-attempts.csv ]]; then
  printf 'attempt,backend_mode,program,rc,wall_ms,contention_snapshot,stdout_path,stderr_path\n' \
    >"$OUT/localhost-attempts.csv"
fi

snapshot_contention() {
  local attempt=$1
  local snapshot="$OUT/contention/attempt-$attempt.txt"
  {
    printf 'snapshot_utc=%s\n' "$(date -u +%FT%TZ)"
    printf 'attempt=%s\n' "$attempt"
    printf 'host=%s\n' "$(hostname -s)"
    printf 'proc_loadavg='; cat /proc/loadavg
    "$PARENT/ci-hub/ci-hub" load-probe --sample-seconds 3 --json
    with-proxy gh api repos/rrnewton/hermit/actions/runners
    hermit_runners_rc=$?
    printf '\n'
    with-proxy gh api repos/rrnewton/reverie/actions/runners
    reverie_runners_rc=$?
    printf '\n'
    printf 'runner_health_rc=%s,%s\n' "$hermit_runners_rc" "$reverie_runners_rc"
  } >"$snapshot" 2>&1
  printf '%s\n' "$snapshot"
}

run_localhost() {
  local attempt=$1 mode=$2 program=$3 snapshot=$4
  local cell="$OUT/localhost/attempt-$attempt-$mode-$program"
  local stdout="$cell/stdout" stderr="$cell/stderr"
  local t0 t1 rc
  mkdir -p "$cell"
  t0=$(date +%s%3N)
  (
    cd "$cell"
    if [[ $mode == rr ]]; then
      timeout 90 "$BIN" record start --verify --verify-strict -- \
        /usr/bin/env "REAL_COMPAT_FIXTURES=$FIXTURES" /usr/bin/bash \
        "$HROOT/tests/compat/real_compat_workload.sh" "$program"
    else
      timeout 90 "$BIN" --backend e9patch run --strict --verify -- \
        /usr/bin/env "REAL_COMPAT_FIXTURES=$FIXTURES" /usr/bin/bash \
        "$HROOT/tests/compat/real_compat_workload.sh" "$program"
    fi
  ) >"$stdout" 2>"$stderr"
  rc=$?
  t1=$(date +%s%3N)
  printf '%s,%s,%s,%s,%s,%s,%s,%s\n' \
    "$attempt" "$mode" "$program" "$rc" "$((t1-t0))" "$snapshot" "$stdout" "$stderr" \
    >>"$OUT/localhost-attempts.csv"
  printf 'localhost attempt=%s mode=%s program=%s rc=%s raw_wall_ms=%s\n' \
    "$attempt" "$mode" "$program" "$rc" "$((t1-t0))"
}

export HERMIT_E9TOOL="$E9TOOL"
export HERMIT_E9PATCH_BACKEND="$E9PATCH"
export HERMIT_INSTALL_DIR="$INSTALL_DIR"
export HERMIT_REPO="$HROOT"
export REVERIE_REPO="$RROOT"
export HERMIT_BIN="$BIN"

# The matrix's four-state semantics make an unavailable backend an honest N/A,
# but this repeatability task explicitly requires all six functioning. Refuse
# the whole producer before any sweep unless each front door runs /bin/true.
for backend in ptrace dbt liteinst sabre kvm e9patch; do
  timeout 60 "$BIN" --backend "$backend" run -- /bin/true \
    >"$OUT/logs/smoke-$backend.stdout" 2>"$OUT/logs/smoke-$backend.stderr"
  smoke_rc=$?
  printf 'backend_smoke backend=%s rc=%s\n' "$backend" "$smoke_rc"
  ((smoke_rc == 0)) || exit 79
done

overall_rc=0
for attempt in $(seq 1 "$RUNS"); do
  if [[ -f $OUT/attempt-$attempt.complete ]]; then
    printf 'sweep attempt=%s already_complete=1\n' "$attempt"
    continue
  fi
  [[ $(sha256sum "$DRIVER" | cut -d' ' -f1) == "$EXPECTED_DRIVER_SHA256" ]] || {
    printf 'sweep attempt=%s refused=matrix-driver-changed\n' "$attempt"; exit 77;
  }
  [[ $(sha256sum "$CORPUS" | cut -d' ' -f1) == "$EXPECTED_CORPUS_SHA256" ]] || {
    printf 'sweep attempt=%s refused=matrix-corpus-changed\n' "$attempt"; exit 78;
  }
  snapshot=$(snapshot_contention "$attempt")
  printf 'sweep attempt=%s contention_snapshot=%s\n' "$attempt" "$snapshot"
  if ! grep -q '^runner_health_rc=0,0$' "$snapshot" || grep -q '"busy":true' "$snapshot"; then
    printf 'sweep attempt=%s refused=contention-or-runner-health-no-result\n' "$attempt"
    exit 75
  fi
  sweep_t0=$(date +%s%3N)
  (
    cd "$MATRIX_ROOT"
    "$DRIVER" --hermit "$BIN" --out "$OUT/sweep-$attempt.csv" \
      --jobs 16 --timeout 90 --fixtures "$FIXTURES"
  ) >"$OUT/logs/sweep-$attempt.log" 2>&1
  sweep_rc=$?
  sweep_t1=$(date +%s%3N)
  printf 'sweep attempt=%s rc=%s raw_wall_ms=%s raw_end_utc=%s\n' \
    "$attempt" "$sweep_rc" "$((sweep_t1-sweep_t0))" "$(date -u +%FT%TZ)"
  if ((sweep_rc != 0)) || [[ ! -s $OUT/sweep-$attempt.csv ]] \
    || [[ $(wc -l <"$OUT/sweep-$attempt.csv") -ne 1285 ]]; then
    overall_rc=$((sweep_rc == 0 ? 80 : sweep_rc))
    printf 'sweep attempt=%s incomplete=1 rows=%s\n' \
      "$attempt" "$(wc -l <"$OUT/sweep-$attempt.csv" 2>/dev/null || printf 0)"
    continue
  fi

  run_localhost "$attempt" rr curl-localhost "$snapshot"
  run_localhost "$attempt" rr wget-localhost "$snapshot"
  run_localhost "$attempt" e9patch curl-localhost "$snapshot"
  run_localhost "$attempt" e9patch wget-localhost "$snapshot"
  touch "$OUT/attempt-$attempt.complete"
done

printf 'finished_utc=%s overall_rc=%s\n' "$(date -u +%FT%TZ)" "$overall_rc"
exit "$overall_rc"
