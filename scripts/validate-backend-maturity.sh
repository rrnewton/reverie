#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

# Measure the backend-maturity levels defined by the shared Reverie Tool
# contract.  Every row says what was compared; an exit status alone can never
# be mistaken for canonical Hermit evidence.

set -uo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
readonly ROOT_DIR
cd "$ROOT_DIR" || exit 1

TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT_DIR/target"}
PROFILE=${BACKEND_MATURITY_PROFILE:-debug}
REPEATS=${BACKEND_MATURITY_REPEATS:-2}
CASE_TIMEOUT=${BACKEND_MATURITY_TIMEOUT_SECONDS:-300}
REPORT=${BACKEND_MATURITY_REPORT:-"$TARGET_DIR/backend-maturity.tsv"}
FULL_BACKENDS='ptrace kvm dbt sabre liteinst e9patch'
BACKENDS=${BACKEND_MATURITY_BACKENDS:-"$FULL_BACKENDS"}
SKIP_RELEASE_BUILD=${BACKEND_MATURITY_SKIP_RELEASE_BUILD:-0}
SKIP_PREPARE=${BACKEND_MATURITY_SKIP_PREPARE:-0}
readonly TARGET_DIR PROFILE REPEATS CASE_TIMEOUT REPORT FULL_BACKENDS BACKENDS
readonly SKIP_RELEASE_BUILD SKIP_PREPARE
if [[ $PROFILE != debug ]]; then
    printf 'BACKEND_MATURITY_PROFILE must be debug; got %q\n' "$PROFILE" >&2
    exit 2
fi
if [[ ! $REPEATS =~ ^[0-9]+$ ]] || ((REPEATS < 2)); then
    printf 'BACKEND_MATURITY_REPEATS must be an integer of at least 2; got %q\n' "$REPEATS" >&2
    exit 2
fi
if [[ $BACKENDS == "$FULL_BACKENDS" ]]; then
    PARTIAL_SELECTION=0
else
    PARTIAL_SELECTION=1
fi
readonly PARTIAL_SELECTION
read -r -a BACKEND_LIST <<<"$BACKENDS"
readonly BACKEND_LIST
for backend in "${BACKEND_LIST[@]}"; do
    case "$backend" in
        ptrace|kvm|dbt|sabre|liteinst|e9patch) ;;
        *)
            printf 'unknown backend in BACKEND_MATURITY_BACKENDS: %q\n' "$backend" >&2
            exit 2
            ;;
    esac
done

mkdir -p "$(dirname -- "$REPORT")"
# Keep generated build scripts and fixtures under the writable checkout. Some
# host execution policies permit file creation in /tmp but refuse to execute a
# newly built child from there, which is an infrastructure result rather than a
# backend result.
WORK_DIR=$(mktemp -d "$TARGET_DIR/backend-maturity-work.XXXXXX")
trap 'rm -rf -- "$WORK_DIR"' EXIT
RELEASE_TARGET="$WORK_DIR/release-target"
readonly RELEASE_TARGET

REPOSITORY_SHA=$(git rev-parse HEAD)
if [[ -n $(git status --porcelain --untracked-files=all) ]]; then
    TREE_DIRTY=1
else
    TREE_DIRTY=0
fi
SOURCE_TREE_SHA256=$(
    {
        git diff --binary HEAD
        while IFS= read -r -d '' path; do
            printf 'untracked-path=%q\n' "$path"
            if [[ -L $path ]]; then
                printf 'symlink-target=%s\n' "$(readlink -- "$path")"
            elif [[ -f $path ]]; then
                sha256sum -- "$path"
            else
                printf 'non-regular\n'
            fi
        done < <(git ls-files --others --exclude-standard -z)
    } | sha256sum | awk '{print $1}'
)
RUST_TOOLCHAIN=$(rustc --version)
{
    printf '# repository_sha\t%s\n' "$REPOSITORY_SHA"
    printf '# tree_dirty\t%s\n' "$TREE_DIRTY"
    printf '# partial_selection\t%s\n' "$PARTIAL_SELECTION"
    printf '# skipped_runtime_preparation\t%s\n' "$SKIP_PREPARE"
    printf '# source_tree_sha256\t%s\n' "$SOURCE_TREE_SHA256"
    printf '# rust_toolchain\t%s\n' "$RUST_TOOLCHAIN"
    printf 'backend\tlevel\toutcome\trepeats\texit_status\tguest_stdout\ttool_output\tprocess_thread_accounting\tbitwise_parity\tcanonical_info\tpositive_info_counts\tfull_corpus\tevidence\tdetail\n'
} >"$REPORT"

declare -A LEVEL_OUTCOME=()
declare -A MAXIMUM_LEVEL=()
declare -A CASE_INFRASTRUCTURE_DENIED=()
declare -A MINIMUM_LEVEL=(
    [ptrace]=B1.5
    [kvm]=B1.5
    [dbt]=B1
    [sabre]=B1.5
    [liteinst]=B1.5
    [e9patch]=B1.5
)

sanitize() {
    local value=$1
    value=${value//$'\t'/ }
    value=${value//$'\n'/ }
    value=${value//$'\r'/ }
    printf '%s' "$value"
}

record() {
    local backend=$1 level=$2 outcome=$3 repeats=$4
    local exit_status=$5 guest_stdout=$6 tool_output=$7 process_threads=$8
    local bitwise=$9 canonical=${10} info_counts=${11} full_corpus=${12}
    local evidence=${13} detail=${14}

    LEVEL_OUTCOME["$backend:$level"]=$outcome
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$backend" "$level" "$outcome" "$repeats" "$exit_status" \
        "$guest_stdout" "$tool_output" "$process_threads" "$bitwise" \
        "$canonical" "$info_counts" "$full_corpus" \
        "$(sanitize "$evidence")" "$(sanitize "$detail")" >>"$REPORT"
    printf 'backend=%-8s level=%-4s outcome=%-12s compared=%s detail=%s\n' \
        "$backend" "$level" "$outcome" "$evidence" "$detail"
}

run_case() {
    local label=$1
    shift
    local stdout="$WORK_DIR/$label.stdout"
    local stderr="$WORK_DIR/$label.stderr"
    local status=0 denied=0 line log

    if timeout --signal=TERM --kill-after=5s "${CASE_TIMEOUT}s" "$@" \
        >"$stdout" 2>"$stderr"; then
        status=0
    else
        status=$?
    fi
    printf '%s' "$status" >"$WORK_DIR/$label.status"
    for log in "$stdout" "$stderr"; do
        if [[ ! -e $log ]]; then
            denied=1
            continue
        fi
        while IFS= read -r line || [[ -n $line ]]; do
            case "$line" in
                *bpfjailer*|*'Bunnylol'*security*|*'Operation not permitted'*|\
                *'Permission denied'*|*'Cannot fork'*|*'Resource temporarily unavailable'*)
                    denied=1
                    break
                    ;;
            esac
        done <"$log"
    done
    CASE_INFRASTRUCTURE_DENIED["$label"]=$denied
    return "$status"
}

case_status() {
    local status
    IFS= read -r status <"$WORK_DIR/$1.status" || true
    printf '%s' "$status"
}

infrastructure_denied() {
    [[ ${CASE_INFRASTRUCTURE_DENIED["$1"]:-0} == 1 ]]
}

any_infrastructure_denied() {
    local prefix=$1 label
    for label in "${!CASE_INFRASTRUCTURE_DENIED[@]}"; do
        if [[ $label == "$prefix"* && ${CASE_INFRASTRUCTURE_DENIED[$label]} == 1 ]]; then
            return 0
        fi
    done
    return 1
}

record_runtime_failure() {
    local backend=$1 level=$2 label=$3 evidence=$4 detail=$5
    local outcome=fail
    if infrastructure_denied "$label"; then
        outcome=unmeasurable
        detail="host execution policy denied the measurement; $detail"
    fi
    record "$backend" "$level" "$outcome" 0 compared compared missing \
        not_measured not_measured not_measured not_measured not_measured \
        "$evidence" "$detail (exit $(case_status "$label"))"
}

record_b15_failure() {
    local backend=$1 prefix=$2 evidence=$3 detail=$4 outcome=fail
    if any_infrastructure_denied "$prefix"; then
        outcome=unmeasurable
        detail="host execution policy denied at least one required command; $detail"
    fi
    record "$backend" B1.5 "$outcome" "$REPEATS" compared compared missing missing \
        not_measured not_measured not_measured not_measured "$evidence" "$detail"
}

measure_b0() {
    local backend=$1 package=$2 label="b0-$1"
    if [[ $TREE_DIRTY == 1 ]]; then
        record "$backend" B0 unmeasurable 0 not_measured not_applicable not_applicable \
            not_applicable not_measured not_measured not_measured not_measured \
            'clean release build exit status' \
            'source checkout is dirty, so the clean-checkout B0 prerequisite cannot be awarded'
        return
    fi
    if [[ $SKIP_RELEASE_BUILD == 1 ]]; then
        record "$backend" B0 unmeasurable 0 not_measured not_applicable not_applicable \
            not_applicable not_measured not_measured not_measured not_measured \
            'clean release build exit status' \
            'release build omitted by BACKEND_MATURITY_SKIP_RELEASE_BUILD=1'
        return
    fi
    if run_case "$label" cargo build --release --target-dir "$RELEASE_TARGET" -p "$package"; then
        record "$backend" B0 pass 1 compared not_applicable not_applicable \
            not_applicable not_measured not_measured not_measured not_measured \
            'clean release build exit status' \
            "cargo build --release --target-dir <fresh-directory> -p $package; repository=$REPOSITORY_SHA; rustc=$RUST_TOOLCHAIN"
    else
        record_runtime_failure "$backend" B0 "$label" \
            'clean release build exit status' \
            "cargo build --release --target-dir <fresh-directory> -p $package"
    fi
}

compile_fixture() {
    local source=$1 output=$2
    "${CC:-cc}" -O2 -g -std=c11 -Wall -Wextra -Werror "$source" -o "$output"
}

test_binary_from_case() {
    local label=$1 target_name=$2
    python3 - "$WORK_DIR/$label.stdout" "$target_name" <<'PY'
import json
import sys

path, target_name = sys.argv[1:]
executable = None
with open(path, encoding="utf-8") as stream:
    for line in stream:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = message.get("target", {})
        if target.get("name") == target_name and message.get("executable"):
            executable = message["executable"]
if executable is None:
    raise SystemExit(1)
print(executable)
PY
}

measure_ptrace() {
    local fixture="$WORK_DIR/ptrace-chaos"
    local data="$WORK_DIR/ptrace-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record ptrace B1 unmeasurable 0 missing missing missing not_measured \
            not_measured not_measured not_measured not_measured \
            'real guest action and exact guest stdout' 'C fixture compilation failed'
        return
    fi
    if run_case ptrace-b1 "$TARGET_DIR/$PROFILE/chaos" --runner ptrace \
        --no-interrupt --no-host-envs "$fixture" "$data" &&
        cmp -s "$WORK_DIR/ptrace-b1.stdout" "$data" &&
        grep -Eq ', 1\) = 1$' "$WORK_DIR/ptrace-b1.stderr"; then
        record ptrace B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + read-length replacement trace' \
            'Chaos limited guest reads to one byte while preserving output; this witness is single-process and does not claim library-call interception; no backend fallback was used'
    else
        record_runtime_failure ptrace B1 ptrace-b1 \
            'exit status + exact guest stdout + read-length replacement trace' \
            'Chaos action was not proved'
        return
    fi

    local repeat counter1='' counter2='' ok=1 i
    for ((i = 1; i <= REPEATS; i++)); do
        repeat="ptrace-counter1-$i"
        run_case "$repeat" "$TARGET_DIR/$PROFILE/counter1" \
            --no-host-envs -- /bin/echo ptrace-counter1 || ok=0
        grep -q '^ptrace-counter1$' "$WORK_DIR/$repeat.stdout" || ok=0
        local value
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/$repeat.stderr" | tail -1)
        [[ -n $value ]] || ok=0
        [[ -z $counter1 || $counter1 == "$value" ]] || ok=0
        counter1=$value

        repeat="ptrace-counter2-$i"
        run_case "$repeat" "$TARGET_DIR/$PROFILE/counter2" \
            --no-host-envs -- /bin/sh -c '/bin/true & wait' || ok=0
        [[ ! -s $WORK_DIR/$repeat.stdout ]] || ok=0
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/$repeat.stderr" | tail -1)
        [[ $value == *' 2 2' ]] || ok=0
        [[ -z $counter2 || $counter2 == "$value" ]] || ok=0
        counter2=$value

        repeat="ptrace-strace-$i"
        run_case "$repeat" "$TARGET_DIR/$PROFILE/strace" --runner ptrace \
            --trace write --no-host-envs /bin/echo ptrace-strace || ok=0
        grep -q '^ptrace-strace$' "$WORK_DIR/$repeat.stdout" || ok=0
        grep -Eq 'write\(1,.*\) = 14$' "$WORK_DIR/$repeat.stderr" || ok=0
    done
    if ((ok == 1)); then
        record ptrace B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable counter totals + process/thread totals + semantic write trace' \
            "counter1=$counter1; counter2=$counter2"
    else
        record_b15_failure ptrace ptrace- \
            'exit status + exact guest stdout + stable counter totals + process/thread totals + semantic write trace' \
            'one or more repeated exact-tool checks failed'
    fi
}

probe_kvm() {
    python3 - <<'PY'
import os
try:
    fd = os.open('/dev/kvm', os.O_RDWR | os.O_CLOEXEC)
except OSError as error:
    print(f'errno={error.errno} name={error.__class__.__name__} message={error}', flush=True)
    raise SystemExit(1)
else:
    os.close(fd)
    print('open(O_RDWR|O_CLOEXEC)=ok', flush=True)
PY
}

measure_kvm() {
    if ! probe_kvm >"$WORK_DIR/kvm-probe.stdout" 2>"$WORK_DIR/kvm-probe.stderr"; then
        local detail
        detail=$(<"$WORK_DIR/kvm-probe.stdout")
        detail+=" $(<"$WORK_DIR/kvm-probe.stderr")"
        record kvm B1 unmeasurable 0 missing missing missing not_measured \
            not_measured not_measured not_measured not_measured \
            'open /dev/kvm with O_RDWR|O_CLOEXEC' "$detail"
        record kvm B1.5 unmeasurable 0 missing missing missing missing \
            not_measured not_measured not_measured not_measured \
            'required KVM execution' 'B1 prerequisite was unmeasurable'
        return
    fi
    local fixture="$WORK_DIR/kvm-chaos" data="$WORK_DIR/kvm-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record kvm B1 unmeasurable 0 missing missing missing not_measured \
            not_measured not_measured not_measured not_measured \
            'real KVM guest action' 'C fixture compilation failed'
        return
    fi
    if run_case kvm-b1 env REVERIE_REQUIRE_KVM=1 \
        "$TARGET_DIR/$PROFILE/chaos" --runner kvm --no-interrupt --no-host-envs \
        "$fixture" "$data" &&
        cmp -s "$WORK_DIR/kvm-b1.stdout" "$data" &&
        grep -Eq ', 1\) = 1$' "$WORK_DIR/kvm-b1.stderr"; then
        record kvm B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'required KVM execution + exact guest stdout + read arguments/return + read-length replacement trace' \
            'real guest ELF; Chaos limited reads to one byte; this witness is single-process and does not claim library-call interception; unsupported syscall paths remain backend errors rather than ptrace fallback'
    else
        record_runtime_failure kvm B1 kvm-b1 \
            'required KVM execution + exact guest stdout + read arguments/return + read-length replacement trace' \
            'KVM real-ELF Chaos action was not proved'
        return
    fi

    local i ok=1 counter1='' counter2='' value
    run_case kvm-b15-suite env REVERIE_REQUIRE_KVM=1 cargo test -p reverie-examples \
        --test kvm_cli -- --test-threads=1 --nocapture || ok=0
    grep -q 'test result: ok' "$WORK_DIR/kvm-b15-suite.stdout" || ok=0
    grep -q 'skipping KVM' "$WORK_DIR/kvm-b15-suite.stdout" \
        "$WORK_DIR/kvm-b15-suite.stderr" && ok=0
    for ((i = 1; i <= REPEATS; i++)); do
        run_case "kvm-counter1-$i" "$TARGET_DIR/$PROFILE/reverie-kvm-counter1" \
            /bin/echo kvm-counter1 || ok=0
        grep -q '^kvm-counter1$' "$WORK_DIR/kvm-counter1-$i.stdout" || ok=0
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/kvm-counter1-$i.stderr" | tail -1)
        [[ -n $value ]] || ok=0
        [[ -z $counter1 || $counter1 == "$value" ]] || ok=0
        counter1=$value

        run_case "kvm-counter2-$i" "$TARGET_DIR/$PROFILE/reverie-kvm-counter2" \
            /bin/echo kvm-counter2 || ok=0
        grep -q '^kvm-counter2$' "$WORK_DIR/kvm-counter2-$i.stdout" || ok=0
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/kvm-counter2-$i.stderr" | tail -1)
        [[ $value == *' 1 1' ]] || ok=0
        [[ -z $counter2 || $counter2 == "$value" ]] || ok=0
        counter2=$value

        run_case "kvm-strace-$i" "$TARGET_DIR/$PROFILE/strace" --runner kvm \
            --trace write --no-host-envs -- /bin/echo kvm-strace || ok=0
        grep -q '^kvm-strace$' "$WORK_DIR/kvm-strace-$i.stdout" || ok=0
        grep -Eq 'write\(1,.*\) = 11$' "$WORK_DIR/kvm-strace-$i.stderr" || ok=0
    done
    if ((ok == 1)); then
        record kvm B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable counter totals + semantic write trace + ptrace counter2 total on echo + process/thread totals on a process tree' \
            "counter1=$counter1; counter2=$counter2; process-tree syscall total is not compared; no canonical INFO comparison"
    else
        record_b15_failure kvm kvm- \
            'required KVM exact-tool and ptrace-comparison tests' \
            'one or more required non-skipping KVM tests failed'
    fi
}

dbt_paths() {
    DBT_CLIENT="$TARGET_DIR/$PROFILE/reverie-dbt-native/libreverie_dbt_client.so"
    local helper="$TARGET_DIR/$PROFILE/reverie-dbt-dynamorio-path"
    [[ -x $helper && -r $DBT_CLIENT ]] || return 1
    DBT_DRRUN=$($helper drrun) || return 1
    DBT_HOME=$($helper home) || return 1
    export DBT_CLIENT DBT_DRRUN DBT_HOME
}

measure_dbt() {
    local fixture="$WORK_DIR/dbt-chaos" data="$WORK_DIR/dbt-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! dbt_paths || ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record dbt B1 unmeasurable 0 missing missing missing not_applicable \
            not_measured not_measured not_measured not_measured \
            'DynamoRIO client + real guest read replacement' \
            'required DBT artifact or C fixture is unavailable'
        return
    fi
    if run_case dbt-b1 env HERMIT_DBT_CHAOS=1 "$DBT_DRRUN" -quiet \
        -disable_rseq -stack_size 2M -c "$DBT_CLIENT" -- "$fixture" "$data" &&
        cmp -s "$WORK_DIR/dbt-b1.stdout" "$data" &&
        [[ $(grep -Ec 'chaos \[pid [0-9]+ n [0-9]+\] read\(.*\) = 1' \
            "$WORK_DIR/dbt-b1.stderr") -ge 10 ]]; then
        record dbt B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + observed read arguments/return + read-length replacement' \
            'real guest ELF reconstructed the file after 10+ one-byte reads; copied children do not run the Rust Tool'
    else
        record_runtime_failure dbt B1 dbt-b1 \
            'exit status + exact guest stdout + observed read arguments/return + read-length replacement' \
            'DBT Chaos action was not proved'
        return
    fi

    local i ok=1 counter1='' counter2='' problems=''
    for ((i = 1; i <= REPEATS; i++)); do
        if ! run_case "dbt-counter1-$i" env HERMIT_DBT_COUNTER1_EXACT=1 "$DBT_DRRUN" \
            -quiet -disable_rseq -stack_size 2M -c "$DBT_CLIENT" -- /bin/echo dbt-counter1; then
            ok=0
            problems+="counter1[$i] exit=$(case_status "dbt-counter1-$i"); "
        fi
        if ! grep -q '^dbt-counter1$' "$WORK_DIR/dbt-counter1-$i.stdout"; then
            ok=0
            problems+="counter1[$i] guest stdout differed; "
        fi
        local value
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/dbt-counter1-$i.stderr" | tail -1)
        if [[ -z $value ]]; then
            ok=0
            problems+="counter1[$i] missing exact summary; "
        elif [[ -n $counter1 && $counter1 != "$value" ]]; then
            ok=0
            problems+="counter1 total changed from $counter1 to $value; "
        fi
        counter1=$value

        if ! run_case "dbt-counter2-$i" env DYNAMORIO_HOME="$DBT_HOME" \
            REVERIE_DBT_CLIENT="$DBT_CLIENT" \
            "$TARGET_DIR/$PROFILE/reverie-dbt-counter2-exact" -- /bin/echo dbt-counter2; then
            ok=0
            problems+="counter2[$i] exit=$(case_status "dbt-counter2-$i"); "
        fi
        if grep -q 'prototype stack overflow' "$WORK_DIR/dbt-counter2-$i.stderr"; then
            problems+="counter2[$i] DynamoRIO prototype stack overflow; "
        fi
        if ! grep -q '^dbt-counter2$' "$WORK_DIR/dbt-counter2-$i.stdout"; then
            ok=0
            problems+="counter2[$i] guest stdout differed; "
        fi
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/dbt-counter2-$i.stderr" | tail -1)
        if [[ $value != *' 1 1' ]]; then
            ok=0
            problems+="counter2[$i] missing 1-process/1-thread exact summary; "
        elif [[ -n $counter2 && $counter2 != "$value" ]]; then
            ok=0
            problems+="counter2 total changed from $counter2 to $value; "
        fi
        counter2=$value

        if ! run_case "dbt-strace-$i" env HERMIT_DBT_STRACE=1 "$DBT_DRRUN" -quiet \
            -disable_rseq -stack_size 2M -c "$DBT_CLIENT" -- /bin/echo dbt-strace; then
            ok=0
            problems+="strace[$i] exit=$(case_status "dbt-strace-$i"); "
        fi
        if ! grep -q '^dbt-strace$' "$WORK_DIR/dbt-strace-$i.stdout" ||
            ! grep -q 'dbt strace' "$WORK_DIR/dbt-strace-$i.stderr"; then
            ok=0
            problems+="strace[$i] guest stdout or semantic trace missing; "
        fi
    done
    if ((ok == 1)); then
        record dbt B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable exact counter totals + semantic syscall trace' \
            "counter1=$counter1; counter2=$counter2"
    else
        record_b15_failure dbt dbt- \
            'exit status + exact guest stdout + stable exact counter totals + semantic syscall trace' \
            "$problems"
    fi
}

sabre_paths() {
    SABRE_RUNNER="$TARGET_DIR/$PROFILE/reverie-sabre-strace"
    SABRE_PLUGIN="$TARGET_DIR/$PROFILE/libreverie_sabre_strace_plugin.so"
    SABRE_LOADER=$(find "$TARGET_DIR/$PROFILE/build" -path '*/out/sabre-build-v4/sabre' \
        -type f -perm -u+x -print -quit 2>/dev/null)
    [[ -x $SABRE_RUNNER && -r $SABRE_PLUGIN && -n $SABRE_LOADER ]] || return 1
    export SABRE_RUNNER SABRE_PLUGIN SABRE_LOADER
}

run_sabre() {
    local label=$1 tool=$2
    shift 2
    run_case "$label" "$SABRE_RUNNER" --sabre "$SABRE_LOADER" \
        --plugin "$SABRE_PLUGIN" --tool "$tool" -- "$@"
}

measure_sabre() {
    local fixture="$WORK_DIR/sabre-chaos" data="$WORK_DIR/sabre-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! sabre_paths || ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record sabre B1 unmeasurable 0 missing missing missing not_applicable \
            not_measured not_measured not_measured not_measured \
            'SaBRe loader/plugin + real guest action' 'required artifact or fixture is unavailable'
        return
    fi
    if run_case sabre-b1 "$SABRE_RUNNER" --sabre "$SABRE_LOADER" \
        --plugin "$SABRE_PLUGIN" --tool chaos --no-interrupt -- "$fixture" "$data" &&
        cmp -s "$WORK_DIR/sabre-b1.stdout" "$data" &&
        grep -Eq ', 1\) = 1$' "$WORK_DIR/sabre-b1.stderr"; then
        record sabre B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + read-length replacement trace' \
            'Chaos limited guest reads to one byte while preserving output; this witness is single-process and does not claim library-call interception; no ptrace fallback was used'
    else
        record_runtime_failure sabre B1 sabre-b1 \
            'exit status + exact guest stdout + read-length replacement trace' \
            'SaBRe Chaos action was not proved'
        return
    fi

    local i ok=1 counter1='' counter2=''
    for ((i = 1; i <= REPEATS; i++)); do
        run_sabre "sabre-counter1-$i" counter1-exact /bin/echo sabre-counter1 || ok=0
        grep -q '^sabre-counter1$' "$WORK_DIR/sabre-counter1-$i.stdout" || ok=0
        local value
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/sabre-counter1-$i.stderr" | tail -1)
        [[ -n $value ]] || ok=0
        [[ -z $counter1 || $counter1 == "$value" ]] || ok=0
        counter1=$value

        run_sabre "sabre-counter2-$i" counter2-exact /bin/sh -c \
            '/bin/true & wait' || ok=0
        [[ ! -s $WORK_DIR/sabre-counter2-$i.stdout ]] || ok=0
        value=$(sed -n \
            -e 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            -e 's/.*counter2-global syscalls=\([0-9][0-9]*\) processes=\([0-9][0-9]*\) threads=\([0-9][0-9]*\).*/\1 \2 \3/p' \
            "$WORK_DIR/sabre-counter2-$i.stderr" | tail -1)
        [[ $value == *' 2 2' ]] || ok=0
        [[ -z $counter2 || $counter2 == "$value" ]] || ok=0
        counter2=$value

        run_sabre "sabre-strace-$i" strace-minimal /bin/echo sabre-strace || ok=0
        grep -q '^sabre-strace$' "$WORK_DIR/sabre-strace-$i.stdout" || ok=0
        grep -Eq 'write\(1,.*, 13\) = \?$' "$WORK_DIR/sabre-strace-$i.stderr" || ok=0
    done
    if ((ok == 1)); then
        record sabre B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable exact counter totals + process/thread totals + semantic write arguments in trace adapter' \
            "counter1=$counter1; counter2=$counter2; trace adapter reports return value as unknown"
    else
        record_b15_failure sabre sabre- \
            'exit status + exact guest stdout + stable exact counter totals + process/thread totals + semantic write trace' \
            'one or more repeated exact-tool checks failed'
    fi
}

measure_liteinst() {
    local ok=1 i tree_detail preload counter1='' counter2='' value
    if run_case liteinst-b1 cargo test -p reverie-examples --test liteinst \
        exact_chaos_tool_limits_reads_after_skip -- --exact --nocapture; then
        record liteinst B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + asserted read-length replacement' \
            'exact Chaos integration test; B1 witness is single-process and does not claim library-call interception; no ptrace fallback was used'
    else
        record_runtime_failure liteinst B1 liteinst-b1 \
            'exit status + exact guest stdout + asserted read-length replacement' \
            'LiteInst Chaos action was not proved'
        return
    fi
    for preload in "$TARGET_DIR/$PROFILE/libreverie_examples.so" \
        "$TARGET_DIR/$PROFILE/deps/libreverie_examples.so"; do
        [[ -f $preload ]] && break
    done
    if [[ ! -f $preload ]]; then
        record liteinst B1.5 unmeasurable 0 missing missing missing missing \
            not_measured not_measured not_measured not_measured \
            'repeated exact counter1/counter2/strace execution' \
            'the required tool preload artifact is missing'
        return
    fi
    for ((i = 1; i <= REPEATS; i++)); do
        run_case "liteinst-counter1-$i" env \
            REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
            "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool counter1 \
            --preload "$preload" -- /bin/echo liteinst-counter1 || ok=0
        grep -q '^liteinst-counter1$' "$WORK_DIR/liteinst-counter1-$i.stdout" || ok=0
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\)$/\1/p' \
            "$WORK_DIR/liteinst-counter1-$i.stderr" | tail -1)
        [[ -n $value ]] || ok=0
        [[ -z $counter1 || $counter1 == "$value" ]] || ok=0
        counter1=$value

        run_case "liteinst-counter2-$i" env \
            REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
            "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool counter2 \
            --preload "$preload" -- /bin/echo liteinst-counter2 || ok=0
        grep -q '^liteinst-counter2$' "$WORK_DIR/liteinst-counter2-$i.stdout" || ok=0
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/liteinst-counter2-$i.stderr" | tail -1)
        [[ $value == *' 1 1' ]] || ok=0
        [[ -z $counter2 || $counter2 == "$value" ]] || ok=0
        counter2=$value

        run_case "liteinst-strace-$i" env \
            REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
            "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool strace \
            --preload "$preload" --trace write -- /bin/echo liteinst-strace || ok=0
        grep -q '^liteinst-strace$' "$WORK_DIR/liteinst-strace-$i.stdout" || ok=0
        grep -Eq 'write\(1,.*\) = 16$' "$WORK_DIR/liteinst-strace-$i.stderr" || ok=0
    done
    if run_case liteinst-process-tree timeout --signal=TERM --kill-after=2s 10s \
        env REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
        "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool counter2 \
        --preload "$preload" -- \
        /bin/sh -c '/bin/true & wait'; then
        local tree_value
        tree_value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/liteinst-process-tree.stderr" | tail -1)
        if [[ $tree_value == *' 2 2' ]]; then
            tree_detail="process-tree counter2 completed with $tree_value"
        else
            tree_detail='process-tree counter2 completed without the expected 2-process/2-thread summary'
        fi
    elif [[ $(case_status liteinst-process-tree) == 124 ]]; then
        tree_detail='process-tree counter2 timed out after 10 seconds'
    else
        tree_detail="process-tree counter2 exited $(case_status liteinst-process-tree)"
    fi
    if ((ok == 1)); then
        record liteinst B1.5 pass "$REPEATS" compared compared compared limited \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable exact counter totals + semantic write trace' \
            "counter1=$counter1; counter2=$counter2; $tree_detail; process-tree omissions remain a documented B1.5 limitation"
    else
        record_b15_failure liteinst liteinst- \
            'repeated exact counter1/counter2/strace integration tests' \
            'one or more exact-tool tests failed'
    fi
}

e9patch_paths() {
    E9TOOL=$(find "$TARGET_DIR/$PROFILE/build" -path '*/out/e9patch-build/e9tool' \
        -type f -perm -u+x -print -quit 2>/dev/null)
    E9PATCH=$(find "$TARGET_DIR/$PROFILE/build" -path '*/out/e9patch-build/e9patch' \
        -type f -perm -u+x -print -quit 2>/dev/null)
    [[ -n $E9TOOL && -n $E9PATCH ]] || return 1
    export E9TOOL E9PATCH
}

run_e9patch_test() {
    local label=$1 test_binary=$2 test_name=$3
    run_case "$label" env REVERIE_E9TOOL="$E9TOOL" REVERIE_E9PATCH_BACKEND="$E9PATCH" \
        "$test_binary" "$test_name" --ignored --exact --nocapture
}

measure_e9patch() {
    local E9_EXAMPLES_TEST
    if ! e9patch_paths; then
        record e9patch B1 unmeasurable 0 missing missing missing not_applicable \
            not_measured not_measured not_measured not_measured \
            'e9tool/e9patch pair + direct Tool action' 'required built pair is unavailable'
        return
    fi
    if ! run_case e9patch-prepare-examples cargo test -p reverie-examples \
            --test e9patch_direct --no-run --message-format=json ||
        ! E9_EXAMPLES_TEST=$(test_binary_from_case e9patch-prepare-examples e9patch_direct); then
        record e9patch B1 unmeasurable 0 missing missing missing not_applicable \
            not_measured not_measured not_measured not_measured \
            'e9patch direct Tool tests' 'required test executables could not be built'
        return
    fi
    if run_e9patch_test e9patch-b1 "$E9_EXAMPLES_TEST" \
        production_strace_observes_filtered_rewritten_write; then
        record e9patch B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + observed write arguments/result + injected write action' \
            'real root guest ELF executed a rewritten write through the direct strace Tool; shared-library sites and process creation are outside this path; no ptrace fallback was used'
    else
        record_runtime_failure e9patch B1 e9patch-b1 \
            'exit status + exact guest stdout + observed write arguments/result + injected write action' \
            'e9patch direct strace action was not proved'
        return
    fi
    local i ok=1 test
    for ((i = 1; i <= REPEATS; i++)); do
        for test in production_counter1_reports_rewritten_syscall_total \
            production_counter2_reports_exit_lifecycle_totals \
            production_strace_observes_filtered_rewritten_write; do
            run_e9patch_test "e9patch-$test-$i" "$E9_EXAMPLES_TEST" "$test" || ok=0
        done
    done
    if ((ok == 1)); then
        record e9patch B1.5 pass "$REPEATS" compared compared compared limited \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + exact counter totals + exit lifecycle totals + semantic write trace' \
            'single-process direct AOT evidence; process creation is outside this backend path'
    else
        record_b15_failure e9patch e9patch- \
            'repeated exact counter1/counter2/strace integration tests' \
            'one or more production exact-tool tests failed'
    fi
}

rank() {
    case "$1" in
        none) printf 0 ;;
        B0) printf 1 ;;
        B1) printf 2 ;;
        B1.5) printf 3 ;;
        *) return 1 ;;
    esac
}

selected() {
    local candidate=$1 backend
    for backend in "${BACKEND_LIST[@]}"; do
        [[ $backend == "$candidate" ]] && return 0
    done
    return 1
}

derive_maximum() {
    local backend=$1 maximum=none
    [[ ${LEVEL_OUTCOME["$backend:B0"]:-missing} == pass ]] && maximum=B0
    [[ $maximum == B0 && ${LEVEL_OUTCOME["$backend:B1"]:-missing} == pass ]] && maximum=B1
    [[ $maximum == B1 && ${LEVEL_OUTCOME["$backend:B1.5"]:-missing} == pass ]] && maximum=B1.5
    MAXIMUM_LEVEL[$backend]=$maximum
}

for backend_package in \
    ptrace:reverie-ptrace \
    kvm:reverie-kvm \
    dbt:reverie-dbt \
    sabre:reverie-sabre-strace \
    liteinst:reverie-liteinst \
    e9patch:reverie-e9patch; do
    selected "${backend_package%%:*}" && \
        measure_b0 "${backend_package%%:*}" "${backend_package#*:}"
done

# Runtime binaries are deliberately built after the independent release-build
# rows. A failed preparation cannot retroactively turn a release-build failure
# into a pass.
PREPARED=1
PREPARED_OUTCOME=pass
if [[ $SKIP_PREPARE != 1 ]]; then
    if selected ptrace || selected kvm || selected liteinst || selected e9patch; then
        run_case prepare-examples cargo build -p reverie-examples --bins || PREPARED=0
    fi
    if selected dbt; then
        run_case prepare-dbt cargo build -p reverie-dbt --bins || PREPARED=0
    fi
    if selected sabre; then
        run_case prepare-sabre cargo build -p reverie-sabre-strace || PREPARED=0
    fi
fi
if ((PREPARED == 0)); then
    if any_infrastructure_denied prepare-; then
        PREPARED_OUTCOME=unmeasurable
    else
        PREPARED_OUTCOME=fail
    fi
    printf 'WARN: one or more runtime-artifact builds failed; affected rows will be fail or unmeasurable\n' >&2
fi

selected ptrace && measure_ptrace
selected kvm && measure_kvm
selected dbt && measure_dbt
selected sabre && measure_sabre
selected liteinst && measure_liteinst
selected e9patch && measure_e9patch

printf '\nMaximum defensible maturity on this run\n'
printf '%-10s %-10s %-10s\n' backend maximum minimum
overall=0
for backend in "${BACKEND_LIST[@]}"; do
    derive_maximum "$backend"
    printf '%-10s %-10s %-10s\n' "$backend" "${MAXIMUM_LEVEL[$backend]}" "${MINIMUM_LEVEL[$backend]}"
    if (( $(rank "${MAXIMUM_LEVEL[$backend]}") < $(rank "${MINIMUM_LEVEL[$backend]}") )); then
        backend_result=2
        for level in B0 B1 B1.5; do
            (( $(rank "$level") <= $(rank "${MINIMUM_LEVEL[$backend]}") )) || break
            outcome=${LEVEL_OUTCOME["$backend:$level"]:-missing}
            if [[ $outcome == fail ]]; then
                backend_result=1
                break
            fi
        done
        if ((backend_result == 1)); then
            overall=1
        elif ((overall == 0)); then
            overall=2
        fi
    fi
done

if ((PARTIAL_SELECTION == 1 || SKIP_PREPARE == 1)); then
    [[ $overall == 1 ]] || overall=2
fi
if [[ $PREPARED_OUTCOME == fail ]]; then
    overall=1
elif [[ $PREPARED_OUTCOME == unmeasurable && $overall == 0 ]]; then
    overall=2
fi

printf '\nB2 and above: not measured. This validate does not implement or substitute for the shared canonical Hermit predicate.\n'
printf 'Report: %s\n' "$REPORT"
exit "$overall"
