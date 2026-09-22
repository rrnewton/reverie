#!/usr/bin/env bash
# Extract and verify the bounded evidence used by this experiment.
set -euo pipefail

experiment_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
extract_dir="$experiment_dir/source-extracts"
measurements="$experiment_dir/measurements.tsv"

fail() {
    printf 'extract-measurements.sh: %s\n' "$*" >&2
    exit 2
}

require_bounded_file() {
    local path=$1 limit=$2 size
    [[ -f $path ]] || fail "missing source file: $path"
    size=$(stat -c %s "$path")
    ((size <= limit)) || fail "source file exceeds $limit bytes: $path ($size bytes)"
}

record_upstream_hash() {
    local path=$1 hash label
    hash=$(sha256sum "$path" | awk '{print $1}')
    case $path in
        */ignored/*) label="ignored/${path#*/ignored/}" ;;
        *) label=$(basename -- "$path") ;;
    esac
    printf '%s  %s\n' "$hash" "$label"
}

capture() {
    [[ $# == 6 ]] || fail "capture requires COMPLETE-LOG COMPLETE-JOURNAL E2E-ROOT PARTIAL-PROFILE-CSV HERMIT-REPO CONSTRUCTED-PLAN"
    local complete_log=$1 complete_journal=$2 e2e_root=$3 partial_csv=$4 hermit_repo=$5 constructed_plan=$6
    local tmp summary count=0 relative

    require_bounded_file "$complete_log" $((10 * 1024 * 1024))
    require_bounded_file "$complete_journal" $((32 * 1024 * 1024))
    require_bounded_file "$partial_csv" $((8 * 1024 * 1024))
    require_bounded_file "$constructed_plan" $((4 * 1024 * 1024))
    [[ -d $e2e_root ]] || fail "missing E2E summary root: $e2e_root"
    git -C "$hermit_repo" cat-file -e '105ea982641f3a119fb82cb8af30417e9dbdc7f5^{commit}' ||
        fail "Hermit repository lacks timing-evidence commit 105ea982641f3a119fb82cb8af30417e9dbdc7f5"

    tmp=$(mktemp -d "$experiment_dir/.source-extracts.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT

    awk '
        /^TOTAL[[:space:]]+[0-9]+([.][0-9]+)?[[:space:]]+[(]sum of node wall[)]/ ||
        /^✅ validate PASS/ ||
        /^   every blocking gate passed/ ||
        /^   [0-9]+ test[(]s[)] executed/ ||
        /^   retries:/ ||
        /^   no retries,/ ||
        /^   nodes:/ ||
        /^   wall [0-9]/ ||
        /^FINAL_VALIDATE_STATUS:/
    ' "$complete_log" >"$tmp/complete-final-summary.txt"
    jq -c '
        select(.event == "step_end")
        | {step, ok, wall_elapsed_s, cpu_usage_usec}
    ' "$complete_journal" >"$tmp/complete-step-ends.jsonl"
    jq -c '
        select((.event == "step_start" or .event == "step_end")
               and (.step | startswith("e2e.manifest_")))
        | {ts, event, step}
    ' "$complete_journal" >"$tmp/complete-e2e-boundaries.jsonl"

    : >"$tmp/e2e-summaries.jsonl"
    for summary in "$e2e_root"/*/*/summary.json; do
        [[ -f $summary ]] || continue
        require_bounded_file "$summary" $((1024 * 1024))
        relative=${summary#"$e2e_root"/}
        jq -c --arg source "$relative" '{source: $source, summary: .}' "$summary" \
            >>"$tmp/e2e-summaries.jsonl"
        count=$((count + 1))
        ((count <= 128)) || fail "more than 128 E2E summary files"
    done
    ((count > 0)) || fail "no E2E summary files found under $e2e_root"

    cp -- "$partial_csv" "$tmp/partial-profile.csv"
    jq -c '
        (.dag_json | fromjson) as $dag
        | {
            path,
            hermit_sha,
            run_id,
            steps: [$dag.steps[] | {group, job}]
        }
    ' "$constructed_plan" >"$tmp/constructed-plan-steps.json"
    git -C "$hermit_repo" show -s --format=%B 105ea982641f3a119fb82cb8af30417e9dbdc7f5 \
        >"$tmp/cold-build-commit.txt"
    {
        record_upstream_hash "$complete_log"
        record_upstream_hash "$complete_journal"
        record_upstream_hash "$partial_csv"
        record_upstream_hash "$constructed_plan"
        for summary in "$e2e_root"/*/*/summary.json; do
            [[ -f $summary ]] && record_upstream_hash "$summary"
        done
    } >"$tmp/UPSTREAM_SHA256SUMS"
    (
        cd "$tmp"
        sha256sum cold-build-commit.txt complete-final-summary.txt complete-step-ends.jsonl complete-e2e-boundaries.jsonl constructed-plan-steps.json \
            e2e-summaries.jsonl partial-profile.csv UPSTREAM_SHA256SUMS >EXTRACT_SHA256SUMS
    )

    rm -rf "$extract_dir"
    mv "$tmp" "$extract_dir"
    trap - EXIT
}

expected() {
    local profile=$1 metric=$2
    awk -F '\t' -v profile="$profile" -v metric="$metric" \
        '$1 == profile && $4 == metric { print $5; found = 1 } END { if (!found) exit 1 }' \
        "$measurements"
}

assert_equal() {
    local profile=$1 metric=$2 actual=$3 want
    want=$(expected "$profile" "$metric") || fail "missing measurement: $profile/$metric"
    [[ $actual == "$want" ]] || fail "$profile/$metric: extracted $actual, table says $want"
    printf '%s\t%s\t%s\n' "$profile" "$metric" "$actual"
}

complete_cpu() {
    local prefix=${1:-}
    jq -rs --arg prefix "$prefix" '
        [ .[]
          | select((.cpu_usage_usec | type) == "string")
          | select(.cpu_usage_usec | test("^[0-9]+$"))
          | select((.step == "pre.submodules"
                    and .ok == "false"
                    and .cpu_usage_usec == "4204602") | not)
          | select($prefix == "" or (.step | startswith($prefix + ".")))
          | (.cpu_usage_usec | tonumber)
        ] | add / 1000000
    ' "$extract_dir/complete-step-ends.jsonl"
}

complete_wall() {
    local prefix=${1:-}
    jq -rs --arg prefix "$prefix" '
        [ .[]
          | select((.cpu_usage_usec | type) == "string")
          | select(.cpu_usage_usec | test("^[0-9]+$"))
          | select((.step == "pre.submodules"
                    and .ok == "false"
                    and .cpu_usage_usec == "4204602") | not)
          | select($prefix == "" or (.step | startswith($prefix + ".")))
          | (.wall_elapsed_s | tonumber)
        ] | (add * 1000 | round) / 1000
    ' "$extract_dir/complete-step-ends.jsonl"
}

complete_summary_metric() {
    local mode=$1 duration
    case $mode in
        node_wall_sum)
            awk '/^TOTAL[[:space:]]/ { printf "%.3f\n", $2 }' \
                "$extract_dir/complete-final-summary.txt"
            ;;
        makespan)
            duration=$(awk '$1 == "wall" { print $2 }' "$extract_dir/complete-final-summary.txt")
            duration_seconds "$duration"
            ;;
        process_cpu)
            duration=$(awk '$1 == "wall" { for (i = 1; i < NF; ++i) if ($i == "CPU") print $(i + 1) }' \
                "$extract_dir/complete-final-summary.txt")
            duration_seconds "$duration"
            ;;
        *) fail "unknown complete summary metric: $mode" ;;
    esac
}

duration_seconds() {
    local value=$1 hours=0 minutes=0 seconds=0
    [[ $value =~ ^([0-9]+h)?([0-9]+m)?([0-9]+([.][0-9]+)?s)?$ ]] ||
        fail "invalid duration: $value"
    if [[ $value == *h* ]]; then
        hours=${value%%h*}
        value=${value#*h}
    fi
    if [[ $value == *m* ]]; then
        minutes=${value%%m*}
        value=${value#*m}
    fi
    if [[ $value == *s ]]; then
        seconds=${value%s}
    fi
    awk -v h="$hours" -v m="$minutes" -v s="$seconds" \
        'BEGIN { printf "%.3f\n", h * 3600 + m * 60 + s }'
}

cold_build_metric() {
    local kind=$1 clock
    case $kind in
        cargo_release_wall)
            clock=$(awk '/^- cold cargo build --release:/ {
                for (i = 1; i < NF; ++i) if ($i == "--release:") print $(i + 1)
            }' \
                "$extract_dir/cold-build-commit.txt")
            ;;
        buck_hermit_target_wall)
            clock=$(awk '/^- cold Buck2 Hermit target:/ {
                for (i = 1; i < NF; ++i) if ($i == "target:") print $(i + 1)
            }' \
                "$extract_dir/cold-build-commit.txt")
            ;;
        *) fail "unknown cold-build metric: $kind" ;;
    esac
    [[ $clock =~ ^([0-9]+):([0-9]+([.][0-9]+)?)$ ]] || fail "invalid cold-build clock: $clock"
    awk -v m="${BASH_REMATCH[1]}" -v s="${BASH_REMATCH[2]}" \
        'BEGIN { printf "%.3f\n", m * 60 + s }'
}

portable_e2e_window() {
    jq -rs '
        ([.[] | select(.event == "step_start") | (.ts | tonumber)] | min) as $start
        | ([.[] | select(.event == "step_end") | (.ts | tonumber)] | max) as $finish
        | (($finish - $start) * 1000 | round) / 1000
    ' "$extract_dir/complete-e2e-boundaries.jsonl"
}

partial_metric() {
    local mode=$1
    awk -F ',' -v mode="$mode" '
        NR == 1 {
            for (i = 1; i <= NF; ++i) col[$i] = i
            next
        }
        {
            latest = $col["run_id"]
            row[NR] = $0
        }
        END {
            for (n = 2; n <= NR; ++n) {
                split(row[n], f, ",")
                if (f[col["run_id"]] != latest) continue
                step = f[col["step"]]
                elapsed = f[col["elapsed_s"]] + 0
                finished = f[col["finished_offset_s"]] + 0
                cpu = f[col["cpu.usage_usec"]] + 0
                wall_sum += elapsed
                cpu_sum += cpu
                if (finished > makespan) makespan = finished
                if (step == "pre.submodules" || step == "pre.reverie_pin" ||
                    step == "setup.pinned_root_fetch" ||
                    step == "build.rust_scripts_in_pinned_root" ||
                    step == "build.workspace_in_pinned_root" ||
                    step == "build.e2e_artifact_in_pinned_root" ||
                    step == "build.liteinst_runtime_release_in_pinned_root")
                    critical += elapsed
                if (step == "build.workspace_in_pinned_root") workspace_finish = finished
                if (step == "build.runtime_release_in_pinned_root") runtime_finish = finished
                if (step == "build.workspace_in_pinned_root" ||
                    step == "build.runtime_release_in_pinned_root" ||
                    step == "build.rust_scripts" ||
                    step == "build.rust_scripts_in_pinned_root")
                    core_build += cpu
            }
            if (mode == "makespan") printf "%.3f\n", makespan
            else if (mode == "node_wall_sum") printf "%.3f\n", wall_sum
            else if (mode == "node_cgroup_cpu_sum") printf "%.3f\n", cpu_sum / 1000000
            else if (mode == "observed_critical_chain") printf "%.3f\n", critical
            else if (mode == "four_core_build_nodes_cpu") printf "%.3f\n", core_build / 1000000
            else if (mode == "artifact_dependency_upper_bound") printf "%.3f\n", workspace_finish - runtime_finish
            else exit 2
        }
    ' "$extract_dir/partial-profile.csv"
}

verify() {
    local invalid_cpu plan_path plan_steps
    [[ -d $extract_dir ]] || fail "missing tracked source extracts"
    (cd "$extract_dir" && sha256sum -c EXTRACT_SHA256SUMS)

    invalid_cpu=$(jq -rs '
        [ .[] | select(.cpu_usage_usec != null)
          | select(if (.cpu_usage_usec | type) == "string"
                   then ((.cpu_usage_usec | test("^[0-9]+$")) | not)
                   else true
                   end)
        ] | length
    ' "$extract_dir/complete-step-ends.jsonl")
    [[ $invalid_cpu == 0 ]] || fail "$invalid_cpu populated cpu_usage_usec field(s) are not decimal strings"

    plan_path=$(jq -r '.path' "$extract_dir/constructed-plan-steps.json")
    [[ $plan_path == full ]] || fail "constructed plan path is $plan_path, expected full"
    plan_steps=$(jq '[.steps[] | [.group, .job] | @tsv] | unique | length' \
        "$extract_dir/constructed-plan-steps.json")
    [[ $plan_steps == 270 ]] || fail "constructed full plan has $plan_steps unique group/job steps, expected 270"
    printf 'constructed-plan\tfull_unique_group_job_steps\t%s\n' "$plan_steps"

    assert_equal cold-local-20260921 cargo_release_wall "$(cold_build_metric cargo_release_wall)"
    assert_equal cold-local-20260921 buck_hermit_target_wall "$(cold_build_metric buck_hermit_target_wall)"

    assert_equal complete-20260905 makespan "$(complete_summary_metric makespan)"
    assert_equal complete-20260905 process_cpu "$(complete_summary_metric process_cpu)"
    assert_equal complete-20260905 node_wall_sum "$(complete_summary_metric node_wall_sum)"

    assert_equal complete-20260905 outer_node_cgroup_cpu_sum "$(complete_cpu)"
    assert_equal complete-20260905 build_group_cpu "$(complete_cpu build)"
    assert_equal complete-20260905 build_group_wall "$(complete_wall build)"
    assert_equal complete-20260905 test_group_cpu "$(complete_cpu test)"
    assert_equal complete-20260905 test_group_wall "$(complete_wall test)"
    assert_equal complete-20260905 check_group_cpu "$(complete_cpu check)"
    assert_equal complete-20260905 check_group_wall "$(complete_wall check)"
    assert_equal complete-20260905 doc_group_cpu "$(complete_cpu doc)"
    assert_equal complete-20260905 doc_group_wall "$(complete_wall doc)"

    while IFS=$'\t' read -r metric value; do
        assert_equal complete-20260905 "$metric" "$value"
    done < <(jq -rs '
        def sums($which):
            [ .[] | select(.source | startswith($which)) | .summary ]
            | {cells: (map(.cells) | add), cpu: (map(.cell_cpu_usage_usec) | add / 1000000)};
        (sums("") | ["all_e2e_cells", (.cells|tostring)],
                    ["all_e2e_cell_cpu", (.cpu|tostring)]),
        (sums("portable/") | ["portable_e2e_cells", (.cells|tostring)],
                              ["portable_e2e_cell_cpu", (.cpu|tostring)]),
        (sums("privileged/") | ["privileged_e2e_cells", (.cells|tostring)],
                                ["privileged_e2e_cell_cpu", (.cpu|tostring)]),
        (.[] | select(.source == "portable/manifest_c_programs/summary.json") | .summary
          | ["portable_e2e_c_programs_cells", (.cells|tostring)],
            ["portable_e2e_c_programs_cpu", ((.cell_cpu_usage_usec/1000000)|tostring)]),
        (.[] | select(.source == "portable/manifest_backend_parity_c/summary.json") | .summary
          | ["portable_e2e_backend_parity_cells", (.cells|tostring)],
            ["portable_e2e_backend_parity_cpu", ((.cell_cpu_usage_usec/1000000)|tostring)]),
        (.[] | select(.source == "portable/manifest_applications/summary.json") | .summary
          | ["portable_e2e_applications_cells", (.cells|tostring)],
            ["portable_e2e_applications_cpu", ((.cell_cpu_usage_usec/1000000)|tostring)])
        | @tsv
    ' "$extract_dir/e2e-summaries.jsonl")

    assert_equal complete-20260905 portable_e2e_observed_window "$(portable_e2e_window)"

    for metric in makespan node_wall_sum node_cgroup_cpu_sum observed_critical_chain \
        four_core_build_nodes_cpu artifact_dependency_upper_bound; do
        assert_equal partial-20260922 "$metric" "$(partial_metric "$metric")"
    done
}

case ${1:-verify} in
    capture)
        shift
        capture "$@"
        ;;
    verify)
        [[ $# == 0 || $# == 1 ]] || fail "verify takes no arguments"
        verify
        ;;
    *)
        fail "usage: $0 [verify | capture COMPLETE-LOG COMPLETE-JOURNAL E2E-ROOT PARTIAL-PROFILE-CSV HERMIT-REPO CONSTRUCTED-PLAN]"
        ;;
esac
