#!/usr/bin/env bash

set -uo pipefail

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir" || exit 1

inventory=''
metadata=''

run_inventory() {
    local description=$1
    shift
    if ! inventory=$(CARGO_TERM_COLOR=never "$@" 2>&1); then
        printf '%s inventory failed\n' "$description" >&2
        printf '%s\n' "$inventory" >&2
        exit 1
    fi
    printf '%s\n' "$inventory"
}

assert_exact_tests() {
    local description=$1
    shift
    local -a expected_names=("$@")
    local -a actual_tests=()
    mapfile -t actual_tests < <(awk '/: test$/ { print }' <<<"$inventory")

    if [[ ${#actual_tests[@]} -ne ${#expected_names[@]} ]]; then
        printf '%s selected %s tests; expected %s\n' \
            "$description" "${#actual_tests[@]}" "${#expected_names[@]}" >&2
        exit 1
    fi
    local test_name
    for test_name in "${expected_names[@]}"; do
        if ! grep -Fqx -- "$test_name: test" <<<"$inventory"; then
            printf '%s omitted exact test %s\n' "$description" "$test_name" >&2
            exit 1
        fi
    done
}

assert_exact_targets() {
    local package=$1
    shift
    local expected actual
    expected=$(printf '%s\n' "$@" | LC_ALL=C sort)
    if ! actual=$(jq -r --arg package "$package" '
        .packages[]
        | select(.name == $package)
        | .targets[]
        | [.name, (.kind | join(",")), (.test | tostring), (.doctest | tostring)]
        | join("|")
    ' <<<"$metadata" | LC_ALL=C sort); then
        printf 'failed to read %s target topology from Cargo metadata\n' "$package" >&2
        exit 1
    fi
    if [[ $actual != "$expected" ]]; then
        printf '%s target topology changed\nexpected:\n%s\nactual:\n%s\n' \
            "$package" "$expected" "$actual" >&2
        exit 1
    fi
}

assert_exact_harness_false_targets() {
    local manifest=$1
    shift
    if ! python3 - "$manifest" "$@" <<'PY'
import sys
import tomllib

manifest = sys.argv[1]
expected = sorted(sys.argv[2:])
with open(manifest, "rb") as source:
    document = tomllib.load(source)

actual = []
for kind in ("lib", "bin", "example", "test", "bench"):
    targets = document.get(kind, [])
    if isinstance(targets, dict):
        targets = [targets]
    for target in targets:
        if target.get("harness", True) is False:
            actual.append(f"{kind}:{target.get('name', '<implicit>')}")
actual.sort()
if actual != expected:
    print(f"{manifest} harness=false targets changed", file=sys.stderr)
    print(f"expected: {expected}", file=sys.stderr)
    print(f"actual:   {actual}", file=sys.stderr)
    raise SystemExit(1)
PY
    then
        printf 'Cargo harness topology check failed for %s\n' "$manifest" >&2
        exit 1
    fi
}

if ! command -v jq >/dev/null 2>&1; then
    printf 'jq is required to verify Cargo target topology\n' >&2
    exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
    printf 'python3 is required to verify Cargo harness topology\n' >&2
    exit 1
fi
if ! metadata=$(CARGO_TERM_COLOR=never cargo metadata \
    --no-deps \
    --format-version=1 \
    --locked); then
    printf 'Cargo target metadata failed\n' >&2
    exit 1
fi

assert_exact_targets reverie-liteinst \
    'reverie_liteinst|cdylib,rlib|true|true' \
    'reverie-liteinst-exec-guest|bin|true|false' \
    'reverie-liteinst-fork-guest|bin|true|false' \
    'reverie-liteinst-lifecycle-guest|bin|true|false' \
    'reverie-liteinst-rpc-tool-guest|bin|true|false' \
    'reverie-liteinst-spoof-guest|bin|true|false' \
    'reverie-liteinst-strace|bin|true|false' \
    'reverie-liteinst-trap-count-guest|bin|true|false' \
    'after_loader|test|true|false' \
    'concurrent_patching|test|true|false' \
    'host_initializer|test|true|false' \
    'hybrid|test|true|false' \
    'lifecycle|test|true|false' \
    'preparation_allocator|test|true|false' \
    'rpc_tool|test|true|false' \
    'strace|test|true|false' \
    'build-script-build|custom-build|false|false'
assert_exact_harness_false_targets reverie-liteinst/Cargo.toml test:preparation_allocator

assert_exact_targets liteinst2 \
    'liteinst2|lib|true|true' \
    'liteinst2-arena-fork-fixture|bin|true|false' \
    'preload_consumer|example|false|false' \
    'replace_first|example|false|false' \
    'arena_fork|test|true|false' \
    'stress|test|true|false' \
    'trampoline_tail|test|true|false'
assert_exact_harness_false_targets third_party/liteinst2/Cargo.toml

run_inventory "after-loader evidence" cargo test \
    -p reverie-liteinst \
    --release \
    --locked \
    --no-default-features \
    --features liteinst-after-loader-experiment \
    --test after_loader \
    -- \
    --list

readonly -a after_loader_tests=(
    staged_reviewed_profiles_bind_exact_union_graph
    command_environment_mismatch_is_typed_and_never_enters_the_guest
    stats_output_api_matches_old_api_raw_bytes_for_four_calls
    non_output_stats_api_reports_four_call_dispatch_exactly
    one_getpid_call_has_no_direct_hook_or_fallback_dispatch
    unpatchable_getpid_refuses_installation_and_uses_one_retained_fallback
)
assert_exact_tests "after-loader evidence" "${after_loader_tests[@]}"

run_inventory "ignored after-loader evidence" cargo test \
    -p reverie-liteinst \
    --release \
    --locked \
    --no-default-features \
    --features liteinst-after-loader-experiment \
    --test after_loader \
    -- \
    --list \
    --ignored
assert_exact_tests "ignored after-loader evidence"

run_inventory "ordinary LiteInst hybrid evidence" cargo test \
    -p reverie-liteinst \
    --locked \
    --test hybrid \
    -- \
    --list
if ! grep -Fqx -- 'hybrid_follows_a_grandchild: test' <<<"$inventory"; then
    printf 'ordinary LiteInst hybrid evidence omitted hybrid_follows_a_grandchild\n' >&2
    exit 1
fi

run_inventory "ignored ordinary LiteInst libtest evidence" cargo test \
    -p reverie-liteinst \
    --all-features \
    --locked \
    --lib \
    --bins \
    --test after_loader \
    --test concurrent_patching \
    --test host_initializer \
    --test hybrid \
    --test lifecycle \
    --test rpc_tool \
    --test strace \
    -- \
    --list \
    --ignored
assert_exact_tests "ignored ordinary LiteInst libtest evidence"

run_inventory "ignored ordinary LiteInst doctest evidence" cargo test \
    -p reverie-liteinst \
    --all-features \
    --locked \
    --doc \
    -- \
    --list \
    --ignored
assert_exact_tests "ignored ordinary LiteInst doctest evidence"

run_inventory "ignored LiteInst2 debug libtest evidence" cargo test \
    -p liteinst2 \
    --all-features \
    --locked \
    --lib \
    --bins \
    --examples \
    --test arena_fork \
    --test stress \
    --test trampoline_tail \
    -- \
    --list \
    --ignored
assert_exact_tests "ignored LiteInst2 debug libtest evidence" \
    rapid::tests::live::benchmark_single_byte_toggle_latency \
    live_probe_stress_matrix \
    probe_overhead_benchmark

run_inventory "ignored LiteInst2 release libtest evidence" cargo test \
    -p liteinst2 \
    --release \
    --all-features \
    --locked \
    --lib \
    --bins \
    --examples \
    --test arena_fork \
    --test stress \
    --test trampoline_tail \
    -- \
    --list \
    --ignored
assert_exact_tests "ignored LiteInst2 release libtest evidence" \
    rapid::tests::live::benchmark_single_byte_toggle_latency \
    live_probe_stress_matrix \
    probe_overhead_benchmark

run_inventory "ignored LiteInst2 debug doctest evidence" cargo test \
    -p liteinst2 \
    --all-features \
    --locked \
    --doc \
    -- \
    --list \
    --ignored
assert_exact_tests "ignored LiteInst2 debug doctest evidence"

run_inventory "ignored LiteInst2 release doctest evidence" cargo test \
    -p liteinst2 \
    --release \
    --all-features \
    --locked \
    --doc \
    -- \
    --list \
    --ignored
assert_exact_tests "ignored LiteInst2 release doctest evidence"
