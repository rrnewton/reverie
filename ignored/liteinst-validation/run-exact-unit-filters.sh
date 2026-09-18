#!/usr/bin/env bash
set -u
set -o pipefail
set +m

readonly WORKTREE=/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c
readonly VALIDATION_SET=${1:-full}
if [ "$#" -gt 1 ]; then
    echo "usage: $0 [full|exit-stop-569f]" >&2
    exit 90
fi
case "$VALIDATION_SET" in
    full)
        readonly MANIFEST="$WORKTREE/ignored/liteinst-validation/exact-unit-filters.tsv"
        readonly MANIFEST_SHA256=99399fdda89ae3c46f35338af79651235c84146c5cd1ac5ce609076e0372f60d
        readonly EXPECTED_TEST_COUNT=52
        ;;
    exit-stop-569f)
        readonly MANIFEST="$WORKTREE/ignored/liteinst-validation/exit-stop-targets-569f.tsv"
        readonly MANIFEST_SHA256=be3dd0186b91fe77f1a23e945ce949b059de72eb1cd1765dedb91b944dc4eb6b
        readonly EXPECTED_TEST_COUNT=4
        ;;
    *)
        echo "unknown validation set: $VALIDATION_SET" >&2
        exit 90
        ;;
esac
readonly FLOOR_BYTES=429496729600
readonly TEST_TIMEOUT_SECONDS=120
readonly COMMAND_TERM_SECONDS=$((TEST_TIMEOUT_SECONDS - 1))
readonly TERM_GRACE_SECONDS=5
readonly FILE_BLOCK_LIMIT=131072

current_leader=
current_pgid=

group_members() {
    ps -eo pid=,pgid=,stat=,comm= | awk -v wanted="$1" '$2 == wanted { print $1 ":" $3 ":" $4 }'
}

monotonic_seconds() {
    local uptime

    read -r uptime _ < /proc/uptime || return 1
    uptime=${uptime%%.*}
    case "$uptime" in
        ''|*[!0-9]*) return 1 ;;
    esac
    echo "$uptime"
}

filesystem_available_bytes() {
    local fields
    local blocks
    local block_size

    fields=$(stat -f -c '%a %S' "$WORKTREE") || return 1
    read -r blocks block_size <<< "$fields" || return 1
    case "$blocks" in
        ''|*[!0-9]*) return 1 ;;
    esac
    case "$block_size" in
        ''|*[!0-9]*) return 1 ;;
    esac
    echo $((blocks * block_size))
}

count_fixed_lines() {
    local needle=$1
    local path=$2
    local count
    local rc

    count=$(rg -Fxc -- "$needle" "$path")
    rc=$?
    case "$rc" in
        0) ;;
        1) count=0 ;;
        *) return "$rc" ;;
    esac
    case "$count" in
        ''|*[!0-9]*) return 2 ;;
    esac
    echo "$count"
}

count_regex_lines() {
    local pattern=$1
    local path=$2
    local count
    local rc

    count=$(rg -xc -- "$pattern" "$path")
    rc=$?
    case "$rc" in
        0) ;;
        1) count=0 ;;
        *) return "$rc" ;;
    esac
    case "$count" in
        ''|*[!0-9]*) return 2 ;;
    esac
    echo "$count"
}

reaped_rc=
reap_leader_if_ready() {
    local pid=$1
    local state

    if kill -0 "$pid" 2>/dev/null; then
        state=$(ps -o stat= -p "$pid") || return 1
        read -r state _ <<< "$state" || return 1
        case "$state" in
            Z*) ;;
            *) return 1 ;;
        esac
    fi
    wait "$pid"
    reaped_rc=$?
    return 0
}

observe_leader() {
    local pid=$1
    local state

    if ! kill -0 "$pid" 2>/dev/null; then
        echo absent
        return 0
    fi
    if ! state=$(ps -o stat= -p "$pid"); then
        if ! kill -0 "$pid" 2>/dev/null; then
            echo absent
            return 0
        fi
        return 1
    fi
    read -r state _ <<< "$state" || return 1
    case "$state" in
        Z*) echo zombie ;;
        '') return 1 ;;
        *) echo active ;;
    esac
}

terminate_group() {
    local pgid=$1
    local deadline
    local members=
    local enumeration_failed=0
    local now

    kill -TERM -- "-$pgid" 2>/dev/null || true
    if ! now=$(monotonic_seconds); then
        kill -KILL -- "-$pgid" 2>/dev/null || true
        return 1
    fi
    deadline=$((now + TERM_GRACE_SECONDS))
    while [ "$now" -lt "$deadline" ]; do
        if members=$(group_members "$pgid"); then
            if [ -z "$members" ]; then
                return "$enumeration_failed"
            fi
        else
            enumeration_failed=1
        fi
        sleep 1
        if ! now=$(monotonic_seconds); then
            enumeration_failed=1
            break
        fi
    done
    kill -KILL -- "-$pgid" 2>/dev/null || true
    if ! now=$(monotonic_seconds); then
        return 1
    fi
    deadline=$((now + TERM_GRACE_SECONDS))
    while [ "$now" -lt "$deadline" ]; do
        if members=$(group_members "$pgid"); then
            if [ -z "$members" ]; then
                return "$enumeration_failed"
            fi
        else
            enumeration_failed=1
        fi
        sleep 1
        if ! now=$(monotonic_seconds); then
            return 1
        fi
    done
    return 1
}

expected_list_sha256() {
    case "$VALIDATION_SET:$1" in
        full:safeptrace) echo 4b45992c72a404e34ea2a94ab6d8dc4f2a524f2a0fda690621fc24a416fe9c6e ;;
        full:reverie-preload) echo 0d720be03048785985e06e4588e57dcb51a2dd479e9305d2285dde53ce04d208 ;;
        full:reverie-ptrace) echo b9091b4077d7e9e9be5ff8f93a30894c4f1ae42d07fc34a98f6ccf6f14e54642 ;;
        full:reverie-liteinst) echo ef9d6c69dcf9732a8666fca2099b3e32f24920eb106a69e89be2b03e7e1e3c07 ;;
        full:reverie-e9patch) echo 98de136e7a593f313a09545de9bfaad4860ad93ded4c2cdb48865e3edd8a8901 ;;
        exit-stop-569f:safeptrace-notifier) echo 5f8a63d21ca906067cf4f37f56d2aaf802dfc5eb8990f5b609d642b1013f90cd ;;
        exit-stop-569f:reverie-ptrace) echo 0fe0184ad2444618fdadfa55a5b0ed3b28669cdb1000b65d8a89296484e603c9 ;;
        *) return 1 ;;
    esac
}

list_log_path() {
    case "$VALIDATION_SET:$1" in
        full:safeptrace) echo "$WORKTREE/ignored/liteinst-validation/full-569f-safeptrace-list.log" ;;
        full:reverie-preload) echo "$WORKTREE/ignored/liteinst-validation/full-569f-reverie-preload-list.log" ;;
        full:reverie-ptrace) echo "$WORKTREE/ignored/liteinst-validation/full-569f-reverie-ptrace-list.log" ;;
        full:reverie-liteinst) echo "$WORKTREE/ignored/liteinst-validation/full-569f-reverie-liteinst-cap256-list.log" ;;
        full:reverie-e9patch) echo "$WORKTREE/ignored/liteinst-validation/full-569f-reverie-e9patch-cap256-list.log" ;;
        exit-stop-569f:safeptrace-notifier) echo "$WORKTREE/ignored/liteinst-validation/safeptrace-notifier-569f-list.log" ;;
        exit-stop-569f:reverie-ptrace) echo "$WORKTREE/ignored/liteinst-validation/reverie-ptrace-569f-cap256-list.log" ;;
        *) return 1 ;;
    esac
}

harness_relative_path() {
    case "$VALIDATION_SET:$1" in
        full:safeptrace) echo target/debug/deps/safeptrace-019878df1cfd8222 ;;
        full:reverie-preload) echo target/debug/deps/reverie_preload-3cabe338076a1e3d ;;
        full:reverie-ptrace) echo target/debug/deps/reverie_ptrace-ed31c6fcd03692ed ;;
        full:reverie-liteinst) echo target/debug/deps/reverie_liteinst-de74cf6b08fac3a1 ;;
        full:reverie-e9patch) echo target/debug/deps/reverie_e9patch-3de0afa4dcf19fe1 ;;
        exit-stop-569f:safeptrace-notifier) echo target/debug/deps/safeptrace-b36d703b206c9adc ;;
        exit-stop-569f:reverie-ptrace) echo target/debug/deps/reverie_ptrace-ed31c6fcd03692ed ;;
        *) return 1 ;;
    esac
}

expected_harness_sha256() {
    case "$VALIDATION_SET:$1" in
        full:safeptrace) echo e767a2bde859a1e31919c639ecc1006c468c3252c41498d81bead78a3ae779aa ;;
        full:reverie-preload) echo 2ad4cd201ca79839dc4fe1c441c50b5aac9fac366ed58236c2ec30724d873552 ;;
        full:reverie-ptrace) echo b8a42fb5319e2686513280619df476d0220f4c53620a3fbbc748788d707b9743 ;;
        full:reverie-liteinst) echo e07d578b718631111544539aff4e4d6a337ef67564f32c7bd8bde6bfb78f5619 ;;
        full:reverie-e9patch) echo 1546ae0d60803878212919fdbb0742ef5cd5cb9ba7ab369846e0c02d007616de ;;
        exit-stop-569f:safeptrace-notifier) echo 8dbec7d510566824307c86d44699707b8a029976734ec921a687655935487ec7 ;;
        exit-stop-569f:reverie-ptrace) echo b8a42fb5319e2686513280619df476d0220f4c53620a3fbbc748788d707b9743 ;;
        *) return 1 ;;
    esac
}

verify_file_sha256() {
    local path=$1
    local expected=$2
    local hash_output
    local observed

    hash_output=$(sha256sum "$path") || return 1
    observed=${hash_output%% *}
    [ "$observed" = "$expected" ]
}

require_file_sha256() {
    local path=$1
    local expected=$2

    if ! verify_file_sha256 "$path" "$expected"; then
        echo "source or evidence checksum mismatch: path=$path" >&2
        return 1
    fi
}

worktree_source_digest() {
    local output

    output=$(
        cd "$WORKTREE" || exit 1
        rg --files -uu -0 -g '!target/**' -g '!ignored/**' -g '!.git' -g '!.git/**' \
            | LC_ALL=C sort -z \
            | xargs -0 sha256sum -z -- \
            | sha256sum
    ) || return 1
    echo "${output%% *}"
}

# ShellCheck cannot see calls made through the EXIT trap installed below.
# shellcheck disable=SC2317
cleanup_on_exit() {
    local rc=$?
    local cleanup_failed=0
    local members=

    trap - EXIT
    trap '' INT TERM HUP
    if [ -n "${current_pgid:-}" ]; then
        kill -TERM -- "-$current_pgid" 2>/dev/null || true
        sleep 1
        kill -KILL -- "-$current_pgid" 2>/dev/null || true
        if [ -n "${current_leader:-}" ]; then
            if ! reap_leader_if_ready "$current_leader"; then
                echo "exit cleanup could not safely reap leader: pid=$current_leader" >&2
                cleanup_failed=1
            fi
        fi
        terminate_group "$current_pgid" || cleanup_failed=1
        if members=$(group_members "$current_pgid"); then
            if [ -n "$members" ]; then
                echo "exit cleanup left process-group members: pgid=$current_pgid members=$members" >&2
                cleanup_failed=1
            fi
        else
            echo "exit cleanup could not enumerate process group: pgid=$current_pgid" >&2
            cleanup_failed=1
        fi
    elif [ -n "${current_leader:-}" ]; then
        kill -CONT "$current_leader" 2>/dev/null || true
        kill -TERM "$current_leader" 2>/dev/null || true
        sleep 1
        kill -KILL "$current_leader" 2>/dev/null || true
        if ! reap_leader_if_ready "$current_leader"; then
            echo "exit cleanup could not safely reap unverified leader: pid=$current_leader" >&2
            cleanup_failed=1
        fi
    fi
    if [ "$cleanup_failed" -ne 0 ]; then
        rc=99
    fi
    exit "$rc"
}

trap cleanup_on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

cd "$WORKTREE" || exit 90
if ! manifest_hash_output=$(sha256sum "$MANIFEST"); then
    echo "could not hash manifest: path=$MANIFEST" >&2
    exit 91
fi
observed_manifest_sha=${manifest_hash_output%% *}
if [ "$observed_manifest_sha" != "$MANIFEST_SHA256" ]; then
    echo "manifest checksum mismatch: $observed_manifest_sha" >&2
    exit 91
fi
require_file_sha256 "$WORKTREE/Cargo.lock" c2cd2ebd9da120b1d5a74d5dc27005a6645d3ca562f7fda1cb795f6ecb445812 || exit 91
if [ "$VALIDATION_SET" = full ]; then
    if ! observed_source_tree_digest=$(worktree_source_digest); then
        echo "could not hash complete worktree source tree" >&2
        exit 91
    fi
    if [ "$observed_source_tree_digest" != 136ad5c44489adf1fb5cd05d77d47ced090787429c44d8d02f3693a4e70f4e8f ]; then
        echo "complete worktree source-tree checksum mismatch: observed=$observed_source_tree_digest" >&2
        exit 91
    fi
    require_file_sha256 "$WORKTREE/safeptrace/src/physical_observer.rs" 5e61748b92f32561319d7660deb1119bbd52ada6b352409e04a7a3d6069ba594 || exit 91
    require_file_sha256 "$WORKTREE/safeptrace/src/waitid.rs" af09974a6c01d98b7f591d965cdcbf3b565af16c2e146ca2b3e134e39896ecf0 || exit 91
    require_file_sha256 "$WORKTREE/safeptrace/src/notifier.rs" 569fb2493b13acb4c3085faac23b9144d5d4907880596fb5289ac8cbfc3afe2f || exit 91
    require_file_sha256 "$WORKTREE/reverie-preload/src/dispatch.rs" 729709d2c8c091817b4017c630f34b7f479830eeb53f3a3e8144c2c65fd72fd8 || exit 91
    require_file_sha256 "$WORKTREE/reverie-preload/src/trap.rs" b6473c47619f42bcff02c50fe4fe500dce7bcba52590b60e2fdf89d8dcf5226e || exit 91
    require_file_sha256 "$WORKTREE/reverie-ptrace/src/task.rs" f846354e67eb9f3169d2df41849e3ed115147973ddf419b6e782e648b467a029 || exit 91
    require_file_sha256 "$WORKTREE/reverie-ptrace/src/task/after_loader_task.rs" 1322febab397987a555f32e9d0da4f706c1f1c7af81255419aea5b96dd959472 || exit 91
    require_file_sha256 "$WORKTREE/reverie-ptrace/src/tracer.rs" 4a1088049414390a3bb5db5e4a2cdbee73e32aa47a76ba914859d9659126ba2a || exit 91
    require_file_sha256 "$WORKTREE/reverie-liteinst/src/patch_alloc.rs" 73ce51479745f09c52a5c6d75e1f9cfdd2355ad3bdc6069009eea5b85aff1148 || exit 91
    require_file_sha256 "$WORKTREE/reverie-liteinst/src/runtime.rs" 12a99fd62399443cd9c257f7932c4550b175d693acad9d45c57695a1f376d058 || exit 91
    require_file_sha256 "$WORKTREE/reverie-liteinst/build.rs" 604556fe3e8c34670e9736c3d323a46e2a3290c65d7c70faf1f82ac9d95744d9 || exit 91
    require_file_sha256 "$WORKTREE/reverie-liteinst/liteinst-helper.ld" a892daa7a484296a578e0912c70a035f65f9a6af3ab9b3f2a0a2ebefd8d74767 || exit 91
    require_file_sha256 "$WORKTREE/reverie-e9patch/src/aot.rs" c6f7fb18164e859029a1afc6667f678933d87ad9a7a75ff16798f436cf282403 || exit 91
    require_file_sha256 "$WORKTREE/reverie-e9patch/build.rs" 4886fcacdbfbfb88e7a4fd7bb3089c1caa09d91b010b50b7338451bb9ccc9ce9 || exit 91
else
    require_file_sha256 "$WORKTREE/safeptrace/src/notifier.rs" 569fb2493b13acb4c3085faac23b9144d5d4907880596fb5289ac8cbfc3afe2f || exit 91
    require_file_sha256 "$WORKTREE/reverie-ptrace/src/tracer.rs" 4a1088049414390a3bb5db5e4a2cdbee73e32aa47a76ba914859d9659126ba2a || exit 91
fi

output_root=$(mktemp -d "/tmp/liteinst-exact-unit-tests.$VALIDATION_SET.XXXXXX") || exit 92
index=0
failures=0
while IFS=$'\t' read -r package test_name; do
    [ "$package" = package ] && continue
    index=$((index + 1))
    if ! list_log=$(list_log_path "$package"); then
        echo "no pinned list path: package=$package" >&2
        failures=$((failures + 1))
        break
    fi
    if ! expected_list_sha=$(expected_list_sha256 "$package"); then
        echo "no pinned list checksum: package=$package" >&2
        failures=$((failures + 1))
        break
    fi
    if ! list_hash_output=$(sha256sum "$list_log"); then
        echo "could not hash pinned test list: package=$package path=$list_log" >&2
        failures=$((failures + 1))
        break
    fi
    observed_list_sha=${list_hash_output%% *}
    if [ "$observed_list_sha" != "$expected_list_sha" ]; then
        echo "test-list checksum mismatch: package=$package observed=$observed_list_sha expected=$expected_list_sha" >&2
        failures=$((failures + 1))
        break
    fi
    if ! exact_matches=$(count_fixed_lines "$test_name: test" "$list_log"); then
        echo "filter audit command failed: package=$package test=$test_name" >&2
        failures=$((failures + 1))
        break
    fi
    if [ "$exact_matches" -ne 1 ]; then
        echo "filter audit failed: package=$package matches=$exact_matches test=$test_name" >&2
        failures=$((failures + 1))
        break
    fi
    if ! harness_relative=$(harness_relative_path "$package"); then
        echo "no pinned harness path: package=$package" >&2
        failures=$((failures + 1))
        break
    fi
    if ! expected_harness_sha=$(expected_harness_sha256 "$package"); then
        echo "no pinned harness checksum: package=$package" >&2
        failures=$((failures + 1))
        break
    fi
    harness_path="$WORKTREE/$harness_relative"
    if ! verify_file_sha256 "$harness_path" "$expected_harness_sha"; then
        echo "harness checksum mismatch before test: package=$package path=$harness_relative" >&2
        failures=$((failures + 1))
        break
    fi

    if ! available_bytes=$(filesystem_available_bytes); then
        echo "disk floor query failed before test" >&2
        failures=$((failures + 1))
        break
    fi
    if [ "$available_bytes" -lt "$FLOOR_BYTES" ]; then
        echo "disk floor closed before test: available_bytes=$available_bytes" >&2
        failures=$((failures + 1))
        break
    fi

    log="$output_root/$(printf '%03d' "$index").log"
    cargo_package=$package
    cargo_features=
    if [ "$package" = safeptrace-notifier ]; then
        cargo_package=safeptrace
        cargo_features=notifier
    fi
    pending_signal_rc=0
    trap 'pending_signal_rc=130' INT
    trap 'pending_signal_rc=143' TERM
    trap 'pending_signal_rc=129' HUP
    # shellcheck disable=SC2016
    setsid bash -c '
        set -e
        trap - INT TERM HUP
        ulimit -c 0
        ulimit -f "$1"
        kill -STOP "$$"
        export CARGO_NET_OFFLINE=true
        export CARGO_BUILD_JOBS=1
        export CARGO_TERM_COLOR=never
        if [ -n "$5" ]; then
            exec timeout --foreground --signal=TERM --kill-after=1s "${4}s" \
                cargo test --locked --color never -p "$2" --features "$5" --lib "$3" -- --exact --test-threads=1 --color never
        fi
        exec timeout --foreground --signal=TERM --kill-after=1s "${4}s" \
            cargo test --locked --color never -p "$2" --lib "$3" -- --exact --test-threads=1 --color never
    ' liteinst-row "$FILE_BLOCK_LIMIT" "$cargo_package" "$test_name" "$COMMAND_TERM_SECONDS" "$cargo_features" >"$log" 2>&1 &
    leader=$!
    current_leader=$leader
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
    if [ "$pending_signal_rc" -ne 0 ]; then
        exit "$pending_signal_rc"
    fi

    pgid=
    state=
    for _ in 1 2 3 4 5; do
        if read -r pgid state < <(ps -o pgid=,stat= -p "$leader"); then
            case "$state" in
                T*) break ;;
            esac
        fi
        sleep 1
    done
    if [ "$pgid" != "$leader" ]; then
        echo "unverified process group: leader=$leader pgid=$pgid state=$state" >&2
        kill -CONT "$leader" 2>/dev/null || true
        kill -TERM "$leader" 2>/dev/null || true
        sleep 1
        kill -KILL "$leader" 2>/dev/null || true
        if reap_leader_if_ready "$leader"; then
            current_leader=
        else
            echo "could not safely reap unverified leader: pid=$leader" >&2
        fi
        failures=$((failures + 1))
        break
    fi
    current_pgid=$pgid
    case "$state" in
        T*) ;;
        *)
            echo "session leader did not stop before launch: leader=$leader state=$state" >&2
            terminate_group "$pgid" || true
            reap_leader_if_ready "$leader" || true
            failures=$((failures + 1))
            break
            ;;
    esac

    if ! started=$(monotonic_seconds); then
        echo "monotonic clock query failed before test launch" >&2
        terminate_group "$pgid" || true
        reap_leader_if_ready "$leader" || true
        failures=$((failures + 1))
        break
    fi
    deadline=$((started + TEST_TIMEOUT_SECONDS))
    min_available=$available_bytes
    stop_reason=
    termination_failure=0
    if ! kill -CONT "$leader"; then
        stop_reason=continue-failure
        terminate_group "$pgid" || termination_failure=1
    fi
    while [ -z "$stop_reason" ]; do
        if ! leader_observation=$(observe_leader "$leader"); then
            stop_reason='leader-observation-failure'
            terminate_group "$pgid" || termination_failure=1
            break
        fi
        case "$leader_observation" in
            absent|zombie) break ;;
            active) ;;
            *)
                stop_reason='leader-observation-failure'
                terminate_group "$pgid" || termination_failure=1
                break
                ;;
        esac
        if ! available_bytes=$(filesystem_available_bytes); then
            stop_reason=disk-query-failure
            terminate_group "$pgid" || termination_failure=1
            break
        fi
        if [ "$available_bytes" -lt "$min_available" ]; then
            min_available=$available_bytes
        fi
        if [ "$available_bytes" -lt "$FLOOR_BYTES" ]; then
            stop_reason=disk-floor
            terminate_group "$pgid" || termination_failure=1
            break
        fi
        if ! now=$(monotonic_seconds); then
            stop_reason=clock-failure
            terminate_group "$pgid" || termination_failure=1
            break
        fi
        if [ "$now" -ge "$deadline" ]; then
            stop_reason=timeout
            terminate_group "$pgid" || termination_failure=1
            break
        fi
        sleep 1
    done

    leader_reap_failure=0
    if reap_leader_if_ready "$leader"; then
        raw_rc=$reaped_rc
    else
        raw_rc=125
        leader_reap_failure=1
    fi
    if [ "$raw_rc" -eq 124 ] && [ -z "$stop_reason" ]; then
        stop_reason=timeout
    fi
    elapsed_seconds=-1
    clock_query_failure=0
    if finished=$(monotonic_seconds); then
        elapsed_seconds=$((finished - started))
    else
        clock_query_failure=1
    fi
    disk_query_failure=0
    if available_bytes=$(filesystem_available_bytes); then
        if [ "$available_bytes" -lt "$min_available" ]; then
            min_available=$available_bytes
        fi
        if [ "$available_bytes" -lt "$FLOOR_BYTES" ]; then
            stop_reason=disk-floor
        fi
    else
        disk_query_failure=1
    fi
    survivors=
    survivor_failure=0
    if survivors=$(group_members "$pgid"); then
        if [ -n "$survivors" ]; then
            survivor_failure=1
        fi
    else
        survivor_failure=1
    fi
    terminate_group "$pgid" || survivor_failure=1
    remaining=
    if remaining=$(group_members "$pgid"); then
        if [ -n "$remaining" ]; then
            survivor_failure=1
        fi
    else
        survivor_failure=1
    fi
    if [ "$survivor_failure" -eq 0 ]; then
        current_pgid=
        current_leader=
    fi

    harness_integrity_failure=0
    if ! verify_file_sha256 "$harness_path" "$expected_harness_sha"; then
        harness_integrity_failure=1
    fi

    running_count=0
    ok_count=0
    summary_count=0
    harness_count=0
    proof_scan_failure=0
    if ! harness_count=$(count_fixed_lines "     Running unittests src/lib.rs ($harness_relative)" "$log"); then
        proof_scan_failure=1
    fi
    if ! running_count=$(count_regex_lines 'running 1 test' "$log"); then
        proof_scan_failure=1
    fi
    if ! ok_count=$(count_fixed_lines "test $test_name ... ok" "$log"); then
        proof_scan_failure=1
    fi
    if ! summary_count=$(count_regex_lines 'test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [0-9.]+s' "$log"); then
        proof_scan_failure=1
    fi
    execution_proof_failure=0
    if [ "$proof_scan_failure" -ne 0 ] || [ "$harness_count" -ne 1 ] || [ "$running_count" -ne 1 ] || [ "$ok_count" -ne 1 ] || [ "$summary_count" -ne 1 ]; then
        execution_proof_failure=1
    fi

    effective_rc=$raw_rc
    [ "$stop_reason" != disk-floor ] || effective_rc=97
    [ "$stop_reason" != timeout ] || effective_rc=124
    [ "$survivor_failure" -eq 0 ] || effective_rc=98
    [ "$leader_reap_failure" -eq 0 ] || effective_rc=95
    [ "$clock_query_failure" -eq 0 ] || effective_rc=94
    [ "$disk_query_failure" -eq 0 ] || effective_rc=93
    [ "$stop_reason" != clock-failure ] || effective_rc=94
    [ "$stop_reason" != disk-query-failure ] || effective_rc=93
    [ "$stop_reason" != continue-failure ] || effective_rc=92
    [ "$stop_reason" != leader-observation-failure ] || effective_rc=89
    [ "$harness_integrity_failure" -eq 0 ] || effective_rc=88
    if [ "$effective_rc" -eq 0 ] && [ "$execution_proof_failure" -ne 0 ]; then
        effective_rc=96
    fi
    echo "index=$index package=$package rc=$effective_rc raw_rc=$raw_rc elapsed_seconds=$elapsed_seconds min_available_bytes=$min_available survivor_failure=$survivor_failure termination_failure=$termination_failure harness_integrity_failure=$harness_integrity_failure harness_count=$harness_count running_count=$running_count ok_count=$ok_count summary_count=$summary_count proof_scan_failure=$proof_scan_failure test=$test_name"
    if [ "$effective_rc" -ne 0 ]; then
        [ -z "$survivors" ] || echo "survivors_after_wait=$survivors" >&2
        [ -z "$remaining" ] || echo "survivors_after_cleanup=$remaining" >&2
        tail -c 65536 "$log"
        failures=$((failures + 1))
        break
    fi
done < "$MANIFEST"

if [ "$failures" -eq 0 ] && [ "$index" -ne "$EXPECTED_TEST_COUNT" ]; then
    echo "manifest row count mismatch: expected=$EXPECTED_TEST_COUNT attempted=$index" >&2
    failures=$((failures + 1))
fi
echo "validation_set=$VALIDATION_SET manifest_sha256=$observed_manifest_sha attempted=$index failures=$failures output_root=$output_root"
exit "$failures"
