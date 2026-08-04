#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

set -uo pipefail

# CI installs the development package and links with -llzma. Some supported
# hosts provide only the versioned runtime library; rust-lld accepts its absolute
# path and still resolves libunwind-ptrace's transitive xz symbols.
LZMA_LINK_TARGET=-llzma
if [[ $(cc -print-file-name=liblzma.so 2>/dev/null) == liblzma.so ]] &&
    command -v ldconfig >/dev/null 2>&1; then
    lzma_runtime=$(ldconfig -p 2>/dev/null |
        awk '$1 ~ /^liblzma[.]so[.][0-9]+$/ { print $NF; exit }')
    if [[ -n $lzma_runtime && -e $lzma_runtime ]]; then
        LZMA_LINK_TARGET=$lzma_runtime
    fi
fi
readonly LZMA_LINK_TARGET
unset lzma_runtime

export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings -C link-arg=$LZMA_LINK_TARGET"
export RUSTDOCFLAGS="${RUSTDOCFLAGS:+$RUSTDOCFLAGS }-D warnings"

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly ROOT_DIR
cd "$ROOT_DIR" || exit 1

# Find the dev-hermit wrapper without making it a prerequisite. A canonical
# nested slot is three directories below the parent; bounding the walk keeps a
# standalone Reverie checkout independent of unrelated ancestors.
find_dev_hermit_parent() {
    local candidate=$ROOT_DIR
    local reverie_path

    for _ in 0 1 2 3; do
        if [[ -f $candidate/.gitmodules ]]; then
            reverie_path=$(git -C "$candidate" config -f .gitmodules \
                --get submodule.reverie.path 2>/dev/null || true)
            if [[ $reverie_path == reverie ]]; then
                printf '%s\n' "$candidate"
                return 0
            fi
        fi
        [[ $candidate != / ]] || break
        candidate=$(dirname -- "$candidate")
    done
    return 1
}

validation_slot_name() {
    local parent=$1
    local relative

    if [[ -z $parent ]]; then
        printf 'standalone\n'
        return
    fi
    relative=${ROOT_DIR#"$parent"/}
    case "$relative" in
        reverie) printf 'primary\n' ;;
        worktrees/*/reverie)
            relative=${relative#worktrees/}
            printf '%s\n' "${relative%%/*}"
            ;;
        *) printf 'standalone\n' ;;
    esac
}

# Honest startup peer scan for the "concurrent_validates" ledger field. Reverie
# has no validate-lock, so count OTHER live validate.sh processes by reading
# /proc/<pid>/cmdline. This is strictly READ-ONLY: it never sends a signal to any
# process (Hard Invariant 15 forbids broad/pattern kills on this shared box).
# This PID and its own ancestry are excluded so a parent wrapper that re-exec'd
# into validate.sh is never miscounted as a peer.
count_concurrent_validates() {
    local self=$$ cur=$$ pid arg cmdline count=0
    local -A ancestry=()

    while [[ $cur =~ ^[1-9][0-9]*$ ]] && ((cur > 1)); do
        ancestry[$cur]=1
        cur=$(sed -n 's/^PPid:[[:space:]]*//p' "/proc/$cur/status" 2>/dev/null)
        [[ -n $cur ]] || break
    done

    for cmdline in /proc/[0-9]*/cmdline; do
        pid=${cmdline#/proc/}
        pid=${pid%/cmdline}
        [[ $pid =~ ^[1-9][0-9]*$ ]] || continue
        [[ -n ${ancestry[$pid]:-} ]] && continue
        [[ -r $cmdline ]] || continue
        # 2>/dev/null precedes the input redirect so a PID that exits between the
        # glob and the open (TOCTOU) cannot leak a "No such file" line to stderr.
        while IFS= read -r -d '' arg; do
            if [[ ${arg##*/} == validate.sh ]]; then
                count=$((count + 1))
                break
            fi
        done 2>/dev/null <"$cmdline"
    done
    printf '%s\n' "$count"
}

LABEL_PR=1
[[ ${VALIDATE_LABEL_PR:-1} == 0 ]] && LABEL_PR=0
PR_NUMBER=${PR_NUMBER:-}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --label-pr) LABEL_PR=1; shift ;;
        --no-label-pr) LABEL_PR=0; shift ;;
        -h|--help)
            echo "Usage: ./validate.sh [--label-pr|--no-label-pr]"
            echo "A green run labels the current PR locally-validated by default."
            exit 0
            ;;
        *)
            echo "validate.sh: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

LOG_FILE=${VALIDATE_LOG_FILE:-}
if [[ -z $LOG_FILE ]]; then
    LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/reverie-validate.XXXXXX.log")"
fi
readonly LOG_FILE
printf "Reverie validation\nRoot: %s\n\n" "$ROOT_DIR" >"$LOG_FILE"

checks=0
failures=0
declare -a ledger_gate_names=()
declare -a ledger_gate_statuses=()
declare -a ledger_gate_durations=()

VALIDATION_STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
VALIDATION_STARTED_EPOCH=$(date +%s)
VALIDATION_HOST=$(hostname -s 2>/dev/null || hostname 2>/dev/null || printf unknown)
DEV_HERMIT_PARENT=$(find_dev_hermit_parent || true)
VALIDATION_SLOT=$(validation_slot_name "$DEV_HERMIT_PARENT")
VALIDATION_LEDGER_FILE=${REVERIE_VALIDATE_LEDGER:-}
if [[ -z $VALIDATION_LEDGER_FILE && -n $DEV_HERMIT_PARENT ]]; then
    VALIDATION_LEDGER_FILE="$DEV_HERMIT_PARENT/ignored/validate-run-ledger.jsonl"
fi
VALIDATION_COMMIT=$(git rev-parse HEAD 2>/dev/null || printf unknown)
VALIDATION_GIT_DEPTH=$(git rev-list --count HEAD 2>/dev/null || printf 0)
VALIDATION_GIT_AHEAD=0
VALIDATION_GIT_BEHIND=0
if git rev-parse --verify --quiet refs/remotes/origin/main >/dev/null; then
    read -r VALIDATION_GIT_BEHIND VALIDATION_GIT_AHEAD < <(
        git rev-list --left-right --count origin/main...HEAD 2>/dev/null || printf '0 0\n'
    )
fi
if [[ -n $(git status --porcelain 2>/dev/null || printf '') ]]; then
    VALIDATION_TREE_DIRTY=1
else
    VALIDATION_TREE_DIRTY=0
fi
if [[ $VALIDATION_COMMIT != unknown ]] && ((VALIDATION_TREE_DIRTY == 0)); then
    VALIDATION_COMMIT_ANCHORED=1
else
    VALIDATION_COMMIT_ANCHORED=0
fi
if [[ -d $ROOT_DIR/target/debug/deps ]]; then
    VALIDATION_CACHE_STATE=warm
else
    VALIDATION_CACHE_STATE=cold
fi
# Startup peer scan (labeled proof "startup_peer_scan"). Non-null (>=0) whenever
# /proc is readable; null/null only when /proc is absent and the scan is
# impossible. This is a start-time sample, not a whole-run overlap monitor.
if [[ -d /proc ]]; then
    VALIDATION_CONCURRENT_VALIDATES=$(count_concurrent_validates)
    [[ $VALIDATION_CONCURRENT_VALIDATES =~ ^[0-9]+$ ]] || VALIDATION_CONCURRENT_VALIDATES=null
    if [[ $VALIDATION_CONCURRENT_VALIDATES == null ]]; then
        VALIDATION_CONCURRENCY_PROOF=null
    else
        VALIDATION_CONCURRENCY_PROOF=startup_peer_scan
    fi
else
    VALIDATION_CONCURRENT_VALIDATES=null
    VALIDATION_CONCURRENCY_PROOF=null
fi
# Number of gates a full serial reverie validate run executes (the run_check
# calls below). Lets a truncated run be classified TRUNCATED downstream.
VALIDATION_GATES_EXPECTED=6
VALIDATION_CPU_TIMES_FILE=$(mktemp "${TMPDIR:-/tmp}/reverie-validate-cpu.XXXXXX")
readonly VALIDATION_STARTED_AT VALIDATION_STARTED_EPOCH VALIDATION_HOST
readonly DEV_HERMIT_PARENT VALIDATION_SLOT VALIDATION_LEDGER_FILE
readonly VALIDATION_COMMIT VALIDATION_GIT_DEPTH VALIDATION_GIT_AHEAD
readonly VALIDATION_GIT_BEHIND VALIDATION_TREE_DIRTY VALIDATION_COMMIT_ANCHORED
readonly VALIDATION_CACHE_STATE VALIDATION_CPU_TIMES_FILE
readonly VALIDATION_CONCURRENT_VALIDATES VALIDATION_CONCURRENCY_PROOF
readonly VALIDATION_GATES_EXPECTED

if [[ -z $VALIDATION_LEDGER_FILE ]]; then
    printf 'No validation ledger: standalone Reverie checkout; dev-hermit parent is absent.\n'
fi

record_ledger_gate() {
    ledger_gate_names+=("$1")
    ledger_gate_statuses+=("$2")
    ledger_gate_durations+=("$3")
}

json_quote() {
    local value=$1
    value=${value//\\/\\\\}
    value=${value//\"/\\\"}
    value=${value//$'\n'/\\n}
    value=${value//$'\r'/\\r}
    value=${value//$'\t'/\\t}
    printf '"%s"' "$value"
}

append_validation_ledger() {
    local exit_status=$1
    local wall_seconds=$2 cpu_user=$3 cpu_sys=$4
    local finished_at result gates_json gate_result line
    local commit_anchored_json tree_dirty_json
    local executed_tests filtered_tests
    local concurrent_validates_json concurrency_proof_json gates_run
    local evidence_helper evidence_json evidence_available=0
    local first_error_line_json=null failed_substep_classes_json='[]'
    local failed_substeps_json='[]' flaky_failed_substeps_json='[]'
    local known_flaky_failure_json=null solo_rerun_confirmation_json=false
    local solo_rerun_of_json=null gate_substeps_json
    local i

    [[ -n $VALIDATION_LEDGER_FILE ]] || return 0

    finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    if ((exit_status == 0 && failures == 0)); then
        result=pass
    else
        result=fail
    fi

    # concurrent_validates / concurrency_proof come from the start-time peer scan
    # captured before any gate ran (see count_concurrent_validates). They are
    # non-null whenever /proc was readable; the proof label names how they were
    # obtained rather than asserting proven exclusivity.
    if [[ $VALIDATION_CONCURRENT_VALIDATES =~ ^[0-9]+$ ]]; then
        concurrent_validates_json=$VALIDATION_CONCURRENT_VALIDATES
    else
        concurrent_validates_json=null
    fi
    if [[ -n $VALIDATION_CONCURRENCY_PROOF && $VALIDATION_CONCURRENCY_PROOF != null ]]; then
        concurrency_proof_json=$(json_quote "$VALIDATION_CONCURRENCY_PROOF")
    else
        concurrency_proof_json=null
    fi

    # One-schema-both-repos: call the SAME parent failure-evidence helper hermit
    # uses (not a reverie reimplementation). Reverie is serial with no DAG, so
    # --dag-jobs 1; the helper may honestly return a null first_error_line for a
    # non-DAG log. Any unavailability/failure keeps the additive defaults above.
    evidence_helper="$DEV_HERMIT_PARENT/ci-hub/validate/failure_evidence.py"
    if [[ -n $DEV_HERMIT_PARENT && -r $evidence_helper ]] \
        && command -v python3 >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
        if evidence_json=$(python3 "$evidence_helper" --log "$LOG_FILE" \
            --commit "$VALIDATION_COMMIT" --dag-jobs 1 \
            --concurrent-validates "$concurrent_validates_json" \
            --ledger "$VALIDATION_LEDGER_FILE" 2>>"$LOG_FILE"); then
            first_error_line_json=$(jq -c '.first_error_line' <<<"$evidence_json")
            failed_substep_classes_json=$(jq -c '.failed_substep_classes' <<<"$evidence_json")
            failed_substeps_json=$(jq -c '.failed_substeps' <<<"$evidence_json")
            flaky_failed_substeps_json=$(jq -c '.flaky_failed_substeps' <<<"$evidence_json")
            known_flaky_failure_json=$(jq -c '.known_flaky_failure' <<<"$evidence_json")
            solo_rerun_confirmation_json=$(jq -c '.solo_rerun_confirmation' <<<"$evidence_json")
            solo_rerun_of_json=$(jq -c '.solo_rerun_of' <<<"$evidence_json")
            evidence_available=1
        fi
    fi

    # Cargo's own result banners prove how many tests actually ran. A green
    # command that emitted no banners records zero and cannot satisfy ci-hub's
    # nonzero-test landing predicate.
    executed_tests=$(sed -n -E \
        's/^[[:space:]]*test result: (ok|FAILED)[.] ([0-9]+) passed;.*/\2/p' \
        "$LOG_FILE" | awk '{ total += $1 } END { print total + 0 }')
    filtered_tests=$(sed -n -E \
        's/^[[:space:]]*test result: (ok|FAILED)[.].*; ([0-9]+) filtered out;.*/\2/p' \
        "$LOG_FILE" | awk '{ total += $1 } END { print total + 0 }')

    gates_json='['
    for i in "${!ledger_gate_names[@]}"; do
        ((i == 0)) || gates_json+=','
        if ((ledger_gate_statuses[i] == 0)); then
            gate_result=pass
        else
            gate_result=fail
        fi
        gates_json+="{\"name\":$(json_quote "${ledger_gate_names[i]}"),"
        gates_json+="\"result\":\"$gate_result\","
        gates_json+="\"exit_code\":${ledger_gate_statuses[i]},"
        gates_json+="\"real_seconds\":${ledger_gate_durations[i]}"
        if ((ledger_gate_statuses[i] != 0)); then
            gate_substeps_json='[]'
            ((evidence_available == 1)) && gate_substeps_json=$failed_substeps_json
            gates_json+=",\"failed_substeps\":$gate_substeps_json"
        fi
        gates_json+='}'
    done
    gates_json+=']'
    gates_run=${#ledger_gate_names[@]}

    if ((VALIDATION_COMMIT_ANCHORED == 1)); then commit_anchored_json=true; else commit_anchored_json=false; fi
    if ((VALIDATION_TREE_DIRTY == 1)); then tree_dirty_json=true; else tree_dirty_json=false; fi

    line="{\"schema_version\":3,\"repo\":\"reverie\","
    line+="\"started_at\":$(json_quote "$VALIDATION_STARTED_AT"),"
    line+="\"finished_at\":$(json_quote "$finished_at"),\"host\":$(json_quote "$VALIDATION_HOST"),"
    line+="\"slot\":$(json_quote "$VALIDATION_SLOT"),\"cwd\":$(json_quote "$ROOT_DIR"),"
    line+="\"profile\":\"full\",\"selection_mode\":\"full\",\"full_coverage\":true,"
    line+="\"cache_state\":$(json_quote "$VALIDATION_CACHE_STATE"),"
    line+="\"commit\":$(json_quote "$VALIDATION_COMMIT"),\"git_depth\":$VALIDATION_GIT_DEPTH,"
    line+="\"git_ahead\":$VALIDATION_GIT_AHEAD,\"git_behind\":$VALIDATION_GIT_BEHIND,"
    line+="\"commit_anchored\":$commit_anchored_json,\"tree_dirty\":$tree_dirty_json,"
    line+="\"result\":\"$result\",\"exit_code\":$exit_status,"
    line+="\"executed_tests\":$executed_tests,\"filtered_tests\":$filtered_tests,"
    line+="\"checks\":$checks,\"failures\":$failures,"
    line+="\"concurrent_validates\":$concurrent_validates_json,"
    line+="\"concurrency_proof\":$concurrency_proof_json,"
    line+="\"known_flaky_failure\":$known_flaky_failure_json,"
    line+="\"first_error_line\":$first_error_line_json,"
    line+="\"failed_substep_classes\":$failed_substep_classes_json,"
    line+="\"flaky_failed_substeps\":$flaky_failed_substeps_json,"
    line+="\"solo_rerun_confirmation\":$solo_rerun_confirmation_json,"
    line+="\"solo_rerun_of\":$solo_rerun_of_json,"
    line+="\"gates_run\":$gates_run,\"gates_expected\":$VALIDATION_GATES_EXPECTED,"
    line+="\"real_seconds\":$wall_seconds,\"user_seconds\":$cpu_user,\"sys_seconds\":$cpu_sys,"
    line+="\"log_file\":$(json_quote "$LOG_FILE"),\"gates\":$gates_json}"

    if ! mkdir -p "$(dirname -- "$VALIDATION_LEDGER_FILE")"; then
        printf 'WARN: unable to create validation ledger directory for %s\n' \
            "$VALIDATION_LEDGER_FILE" >&2
        return 0
    fi
    if command -v flock >/dev/null 2>&1; then
        if ! (
            flock -x 9
            printf '%s\n' "$line" >&9
        ) 9>>"$VALIDATION_LEDGER_FILE"; then
            printf 'WARN: unable to append validation ledger %s\n' \
                "$VALIDATION_LEDGER_FILE" >&2
        fi
    elif ! printf '%s\n' "$line" >>"$VALIDATION_LEDGER_FILE"; then
        printf 'WARN: unable to append validation ledger %s\n' \
            "$VALIDATION_LEDGER_FILE" >&2
    fi
}

cleanup() {
    local exit_status=$?
    local finished_epoch validation_wall validation_user=0 validation_sys=0

    trap - EXIT
    finished_epoch=$(date +%s)
    validation_wall=$((finished_epoch - VALIDATION_STARTED_EPOCH))
    if times >"$VALIDATION_CPU_TIMES_FILE" 2>/dev/null; then
        read -r validation_user validation_sys < <(
            awk '
                function seconds(value, parts) {
                    split(value, parts, "m")
                    sub(/s$/, "", parts[2])
                    return parts[1] * 60 + parts[2]
                }
                NR == 1 { user += seconds($1); sys += seconds($2) }
                NR == 2 { user += seconds($1); sys += seconds($2) }
                END { printf "%.3f %.3f\n", user, sys }
            ' "$VALIDATION_CPU_TIMES_FILE"
        )
    fi
    append_validation_ledger "$exit_status" \
        "$validation_wall" "$validation_user" "$validation_sys"
    rm -f "$VALIDATION_CPU_TIMES_FILE"
    exit "$exit_status"
}

interrupted() {
    trap - INT TERM
    printf 'Validation interrupted (log: %s)\n' "$LOG_FILE" >&2
    exit 130
}

trap cleanup EXIT
trap interrupted INT TERM

run_check() {
    local name=$1
    shift
    local started=$SECONDS
    checks=$((checks + 1))

    {
        printf "== %s ==\nCommand:" "$name"
        printf " %q" "$@"
        printf "\n"
    } >>"$LOG_FILE"

    local status=0
    if "$@" >>"$LOG_FILE" 2>&1; then
        printf "PASS: %s (%ss)\n" "$name" "$((SECONDS - started))"
    else
        status=$?
        failures=$((failures + 1))
        printf "FAIL: %s (exit %s; %ss; log: %s)\n" \
            "$name" "$status" "$((SECONDS - started))" "$LOG_FILE" >&2
    fi
    record_ledger_gate "$name" "$status" "$((SECONDS - started))"
}

readonly LOCALLY_VALIDATED_LABEL=locally-validated

apply_locally_validated_label() {
    local pr=$PR_NUMBER
    local pr_head=""
    local local_head
    local -a gh_cmd=(gh)

    if ! command -v gh >/dev/null 2>&1; then
        echo "WARN: gh CLI not found; skipping $LOCALLY_VALIDATED_LABEL label" >&2
        return 0
    fi
    if command -v with-proxy >/dev/null 2>&1; then
        gh_cmd=(with-proxy gh)
    fi

    if [[ -z $pr ]]; then
        pr="$("${gh_cmd[@]}" pr view --json number -q .number 2>/dev/null)" || true
    fi
    if [[ -z $pr ]]; then
        echo "WARN: no PR found for this branch; skipping $LOCALLY_VALIDATED_LABEL label" >&2
        return 0
    fi
    pr_head=$("${gh_cmd[@]}" pr view "$pr" --json headRefOid -q .headRefOid \
        2>/dev/null) || true
    if [[ -z $pr_head ]]; then
        echo "WARN: could not read PR #$pr head; skipping $LOCALLY_VALIDATED_LABEL label" >&2
        return 0
    fi
    local_head=$(git rev-parse HEAD)
    if [[ $pr_head != "$local_head" ]]; then
        echo "WARN: PR #$pr advanced from $local_head to $pr_head; skipping $LOCALLY_VALIDATED_LABEL label" >&2
        return 0
    fi

    "${gh_cmd[@]}" label create "$LOCALLY_VALIDATED_LABEL" \
        --color 1d76db \
        --description "Full local validation passed for the current PR head" \
        --force >>"$LOG_FILE" 2>&1 || true

    if "${gh_cmd[@]}" pr edit "$pr" --add-label "$LOCALLY_VALIDATED_LABEL" \
        >>"$LOG_FILE" 2>&1; then
        echo "Applied $LOCALLY_VALIDATED_LABEL to PR #$pr"
    else
        echo "WARN: failed to label PR #$pr (log: $LOG_FILE)" >&2
    fi
}

readonly -a REGULAR_TEST_SKIP_ARGS=(
    --skip container::tests::bind_to_low_port
    --skip container::tests::pin_affinity_to_all_cores
    --skip tests::domainname
    --skip tests::hostname
    --skip tests::local_networking_loopback_flags
    --skip tests::local_networking_ping
    --skip tests::local_networking_there_can_be_only_one
    --skip tests::mount_and_move_tmpfs
    --skip tests::mount_bind
    --skip tests::mount_devpts_basic
    --skip tests::mount_devpts_isolated
    --skip tests::mount_proc
    --skip tests::mount_tmpfs
    --skip tests::pid_namespace
    --skip tests::port_isolation
    --skip tests::seccomp_notify
    --skip tests::uid_namespace
)

run_check "Merge-gate policy" "$ROOT_DIR/scripts/check-merge-gate-policy.sh"
run_check "Build workspace" cargo build --workspace --all-features
run_check "Test regular workspace cases" cargo test --workspace --all-features \
    -- --test-threads=1 "${REGULAR_TEST_SKIP_ARGS[@]}"
run_check "Documentation tests" cargo test --workspace --doc
run_check "Clippy" cargo clippy --workspace --all-targets --all-features -- -D warnings
run_check "Rustfmt" cargo fmt --all -- --check

passed=$((checks - failures))
if ((failures == 0)); then
    printf "Validation summary: %s passed, 0 failed (log: %s)\n" "$passed" "$LOG_FILE"
    if ((LABEL_PR == 1)); then
        apply_locally_validated_label
    fi
else
    printf "Validation summary: %s passed, %s failed (log: %s)\n" \
        "$passed" "$failures" "$LOG_FILE" >&2
fi

((failures == 0))
