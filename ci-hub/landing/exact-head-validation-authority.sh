#!/usr/bin/env bash
# Owner-authorized exact-head landing authority: counted local OR versioned
# hosted, with any genuine red blocking. The landing consumer may select one
# authority explicitly: ordinary Hermit and Reverie changes use --local-only,
# while a Hermit scripts/validate.rs producer change uses --hosted-only because
# candidate code cannot vouch for its own receipt. Labels and copied statuses
# are never inputs.
set -uo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/../.." && pwd)
ci_hub=${CI_HUB_BIN:-$root/ci-hub/ci-hub}
receipt_verifier=${VERIFY_RECEIPT_BIN:-$root/ci-hub/validation/verify_receipt.sh}
repo=rrnewton/hermit
sha=
comments=
local_only=0
hosted_only=0
current_base=
current_reverie_base=
repo_checkout=
reverie_checkout=

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo) repo=${2:-}; shift 2 ;;
        --sha) sha=${2:-}; shift 2 ;;
        --comments) comments=${2:-}; shift 2 ;;
        --local-only) local_only=1; shift ;;
        --hosted-only) hosted_only=1; shift ;;
        --current-base) current_base=${2:-}; shift 2 ;;
        --current-reverie-base) current_reverie_base=${2:-}; shift 2 ;;
        --repo-checkout) repo_checkout=${2:-}; shift 2 ;;
        --reverie-checkout) reverie_checkout=${2:-}; shift 2 ;;
        -h|--help)
            echo "usage: exact-head-validation-authority.sh --repo OWNER/REPO --sha 40_HEX [--comments FILE] [--local-only|--hosted-only]"
            exit 0
            ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
if [[ ! $sha =~ ^[0-9a-f]{40}$ ]] || [[ ! $repo =~ ^[^/]+/[^/]+$ ]]; then
    echo "exact-head authority requires --repo OWNER/REPO and lowercase 40-hex --sha" >&2
    exit 2
fi
if [[ -n $comments && ! -r $comments ]]; then
    echo "exact-head authority comments file is unreadable: $comments" >&2
    exit 2
fi
if [[ $local_only -eq 1 && $repo != rrnewton/hermit && $repo != rrnewton/reverie ]]; then
    echo "exact-head authority --local-only requires rrnewton/hermit or rrnewton/reverie" >&2
    exit 2
fi
if [[ $hosted_only -eq 1 && $repo != rrnewton/hermit ]]; then
    echo "exact-head authority --hosted-only is defined only for rrnewton/hermit" >&2
    exit 2
fi
if [[ $local_only -eq 1 && $hosted_only -eq 1 ]]; then
    echo "exact-head authority --local-only and --hosted-only are mutually exclusive" >&2
    exit 2
fi
boundary_args=()
if [[ -n $current_base || -n $current_reverie_base || -n $repo_checkout || -n $reverie_checkout ]]; then
    if [[ ! $current_base =~ ^[0-9a-f]{40}$ ]] || \
       [[ ! $current_reverie_base =~ ^[0-9a-f]{40}$ ]] || \
       [[ ! -d $repo_checkout ]] || [[ ! -d $reverie_checkout ]]; then
        echo "exact-head authority requires complete, valid merge-boundary base arguments" >&2
        exit 2
    fi
    boundary_args=(
        --current-base "$current_base"
        --current-reverie-base "$current_reverie_base"
        --repo-checkout "$repo_checkout"
        --reverie-checkout "$reverie_checkout"
    )
fi

authority_tmp=$(mktemp -d)
local_report=$authority_tmp/local.json
hosted_report=$authority_tmp/hosted.json
trap 'rm -rf -- "$authority_tmp"' EXIT

github_api() {
    if [[ -n ${EXACT_HEAD_GH_BIN:-} ]]; then
        "$EXACT_HEAD_GH_BIN" "$@"
    elif command -v with-proxy >/dev/null 2>&1; then
        with-proxy gh api "$@"
    else
        gh api "$@"
    fi
}

# The portable aggregate deliberately fails closed when any required child is
# cancelled.  That failure records absence, not a negative test verdict.  Only
# downgrade it to NO_RESULT when the exact run, every leaf conclusion, and the
# aggregate's own step list prove that the reducer is the sole failure.  Any
# missing/unknown evidence or genuine leaf/aggregate failure keeps the original
# hosted red.
cancellation_derived_portable_absence() {
    local report=$1 run_id job_id
    local run_report=$authority_tmp/portable-run.json
    local jobs_report=$authority_tmp/portable-jobs.json
    local aggregate_report=$authority_tmp/portable-aggregate.json

    jq -e --arg repo "$repo" --arg sha "$sha" '
        .authority == "github-actions-exact-head-jobs"
        and .repo == $repo and .sha == $sha and .state == "red"
        and .policy_schema_version == 3
        and .required_positive_count == 1 and .positive_count == 0
        and (.jobs | type == "array" and length == 1)
        and .jobs[0].state == "red"
        and .jobs[0].status == "completed"
        and .jobs[0].conclusion == "failure"
        and .jobs[0].job_name == "Regular tests (GitHub-managed portable)"
        and (.jobs[0].run_id | type == "number" and . > 0)
        and (.jobs[0].job_id | type == "number" and . > 0)
    ' "$report" >/dev/null 2>&1 || return 1

    run_id=$(jq -r '.jobs[0].run_id' "$report")
    job_id=$(jq -r '.jobs[0].job_id' "$report")
    github_api "repos/${repo}/actions/runs/${run_id}" >"$run_report" 2>/dev/null || return 1
    github_api "repos/${repo}/actions/runs/${run_id}/jobs?filter=latest&per_page=100" \
        >"$jobs_report" 2>/dev/null || return 1
    github_api "repos/${repo}/actions/jobs/${job_id}" >"$aggregate_report" 2>/dev/null || return 1

    jq -e --arg sha "$sha" '
        .head_sha == $sha
        and .status == "completed" and .conclusion == "cancelled"
        and .name == "CI (GitHub-managed portable)"
        and (.path == ".github/workflows/ci-portable.yml"
             or .path == "ci-portable.yml")
    ' "$run_report" >/dev/null 2>&1 || return 1

    jq -e --argjson aggregate_id "$job_id" '
        .total_count == (.jobs | length)
        and .total_count > 1 and .total_count <= 100
        and ([.jobs[].id] | unique | length) == (.jobs | length)
        and ([.jobs[] | select(.id == $aggregate_id)] | length) == 1
        and ([.jobs[] | select(.name == "Regular tests (GitHub-managed portable)")]
             | length) == 1
        and any(.jobs[];
            .id != $aggregate_id and .status == "completed"
            and .conclusion == "cancelled")
        and all(.jobs[];
            if .id == $aggregate_id then
                .status == "completed" and .conclusion == "failure"
            else
                .status == "completed"
                and (.conclusion == "success"
                     or .conclusion == "skipped"
                     or .conclusion == "cancelled")
            end)
    ' "$jobs_report" >/dev/null 2>&1 || return 1

    jq -e --argjson aggregate_id "$job_id" '
        .id == $aggregate_id
        and .name == "Regular tests (GitHub-managed portable)"
        and .status == "completed" and .conclusion == "failure"
        and (.steps | type == "array" and length > 0)
        and ([.steps[] | select(.conclusion == "failure")] | length) == 1
        and any(.steps[];
            .name == "Require every portable DAG job to succeed or be deselected"
            and .status == "completed" and .conclusion == "failure")
        and all(.steps[];
            .status == "completed"
            and (.conclusion == "success"
                 or .conclusion == "skipped"
                 or (.name == "Require every portable DAG job to succeed or be deselected"
                     and .conclusion == "failure")))
    ' "$aggregate_report" >/dev/null 2>&1
}

local_state=no_result
reverie_policy=
if [[ $hosted_only -eq 1 ]]; then
    local_state=not_consulted
else
    local_rc=0
    "$ci_hub" validate-status --sha "$sha" --repo "$repo" --json \
        >"$local_report" 2>/dev/null || local_rc=$?
    if [[ $local_rc -eq 0 ]] && jq -e --arg repo "$repo" --arg sha "$sha" '
    .newest_qualifying as $row
    | $row.coverage as $coverage
    | .repo == $repo and .sha == $sha and .verdict == "VALIDATED"
    and .exit_code == 0 and (.qualifying_count | type == "number" and . > 0)
    and (($row | type) == "object") and $row.sha == $sha
    and $row.profile == "full" and $row.selection_mode == "full"
    and $row.coverage_satisfied == true and $row.coverage_status == "satisfied"
    and ($coverage.planned_test_nodes | type == "number" and . > 0)
    and ($coverage.executed_test_nodes | type == "number"
        and . == $coverage.planned_test_nodes)
    and ($coverage.absent_nodes | type == "array" and length == 0)
    and ($coverage.zero_executed_nodes | type == "array" and length == 0)
    and ($row.gates | type == "array" and length > 0)
    and all($row.gates[]; .result == "pass")
    and ($row.failures | type == "number" and . == 0)
    and ($row.executed_tests | type == "number" and . > 0)
    and ($row.filtered_tests | type == "number" and . >= 0)
' "$local_report" >/dev/null 2>&1; then
        local_state=green
        if [[ $repo == rrnewton/reverie ]]; then
            reverie_policy=$(jq -r '.newest_qualifying.reverie_validation_policy // empty' "$local_report")
        fi
    elif [[ $local_rc -eq 3 ]] && jq -e --arg repo "$repo" --arg sha "$sha" '
    .repo == $repo and .sha == $sha and .verdict == "FAILED" and .exit_code == 3
' "$local_report" >/dev/null 2>&1; then
        local_state=red
    fi
fi

# At the final mutation boundary, a local green must additionally dereference
# its immutable receipt comment. Hosted authority needs no local receipt.
if [[ $local_state == green && -n $comments ]]; then
    receipt_rc=0
    "$receipt_verifier" --repo "$repo" --sha "$sha" --comments "$comments" \
        "${boundary_args[@]}" \
        >/dev/null 2>&1 || receipt_rc=$?
    if [[ $receipt_rc -ne 0 ]]; then
        local_state=no_result
    fi
fi

# The owner-authorized landing policy is local-ledger only for both product
# repositories. Keep this mode ahead of every hosted query: hosted green must
# not authorize and hosted red or unavailability must not veto a qualifying
# local receipt. The broader evaluator remains available for historical and
# remediation consumers.
if [[ $local_only -eq 1 ]]; then
    case "$repo" in
        rrnewton/hermit)
            local_coverage=portable+privileged
            required_coverage=local:portable+privileged
            ;;
        rrnewton/reverie)
            case "$reverie_policy" in
                reverie-local-validation/v1)
                    local_coverage=full-seven-gate-v1
                    required_coverage=local:full-seven-gate-v1
                    ;;
                reverie-local-validation/v2)
                    local_coverage=full-six-gate-v2
                    required_coverage=local:full-six-gate-v2
                    ;;
                reverie-local-validation/v3)
                    local_coverage=full-seven-gate-v3
                    required_coverage=local:full-seven-gate-v3
                    ;;
                reverie-local-validation/v4)
                    local_coverage=full-ten-gate-v4
                    required_coverage=local:full-ten-gate-v4
                    ;;
                *)
                    local_coverage=none
                    required_coverage=local:versioned-full-profile
                    [[ $local_state == green ]] && local_state=no_result
                    ;;
            esac
            ;;
    esac
    case $local_state in
        green)
            printf 'AUTHORITY=local LOCAL=green HOSTED=not_consulted LOCAL_COVERAGE=%s HOSTED_COVERAGE=none REQUIRED_COVERAGE=%s SHA=%s\n' "$local_coverage" "$required_coverage" "$sha"
            exit 0
            ;;
        red)
            printf 'AUTHORITY=refused LOCAL=red HOSTED=not_consulted LOCAL_COVERAGE=none HOSTED_COVERAGE=none REQUIRED_COVERAGE=%s SHA=%s\n' "$required_coverage" "$sha"
            exit 3
            ;;
        *)
            printf 'AUTHORITY=no_result LOCAL=no_result HOSTED=not_consulted LOCAL_COVERAGE=none HOSTED_COVERAGE=none REQUIRED_COVERAGE=%s SHA=%s\n' "$required_coverage" "$sha"
            exit 4
            ;;
    esac
fi

hosted_rc=0
"$ci_hub" hosted-status --repo "$repo" --sha "$sha" --json \
    >"$hosted_report" 2>/dev/null || hosted_rc=$?
hosted_state=no_result
if [[ $hosted_rc -eq 0 ]] && jq -e --arg repo "$repo" --arg sha "$sha" '
    .required_positive_count as $required |
    .authority == "github-actions-exact-head-jobs"
    and .repo == $repo and .sha == $sha and .state == "green"
    and ($required | type == "number" and . > 0)
    and .positive_count == $required
    and (.jobs | type == "array" and length == $required)
    and all(.jobs[]; .state == "green" and .job_id > 0 and .run_id > 0)
' "$hosted_report" >/dev/null 2>&1; then
    hosted_state=green
elif [[ $hosted_rc -eq 3 ]] && jq -e --arg repo "$repo" --arg sha "$sha" '
    .authority == "github-actions-exact-head-jobs"
    and .repo == $repo and .sha == $sha and .state == "red"
    and any(.jobs[]; .state == "red" and .job_id > 0 and .run_id > 0)
' "$hosted_report" >/dev/null 2>&1; then
    hosted_state=red
fi
if [[ $repo == rrnewton/hermit && $hosted_state == red ]] && \
   cancellation_derived_portable_absence "$hosted_report"; then
    echo "Hosted portable aggregate is cancellation-derived absence; treating it as NO_RESULT." >&2
    hosted_state=no_result
fi

local_coverage=none
hosted_coverage=none
required_coverage=unsupported
case "$repo" in
    rrnewton/hermit)
        required_coverage='local:portable+privileged|hosted:portable'
        [[ $local_state == green ]] && local_coverage=portable+privileged
        if [[ $hosted_state == green ]] && jq -e '
            .policy_schema_version == 3
            and .required_positive_count == 1
            and (.jobs | length == 1)
            and .jobs[0].job_name == "Regular tests (GitHub-managed portable)"
        ' "$hosted_report" >/dev/null 2>&1; then
            hosted_coverage=portable
        else
            [[ $hosted_state == green ]] && hosted_state=no_result
        fi
        ;;
    rrnewton/reverie)
        required_coverage=hosted:regular+host-dependent
        if [[ $hosted_state == green ]] && jq -e '
            .policy_schema_version == 2
            and .required_positive_count == 2
            and ([.jobs[].job_name] | sort) ==
                (["Host-dependent tests (self-hosted)", "Regular tests (GitHub-hosted)"] | sort)
        ' "$hosted_report" >/dev/null 2>&1; then
            hosted_coverage=regular+host-dependent
        else
            [[ $hosted_state == green ]] && hosted_state=no_result
        fi
        ;;
esac

# A producer-changing candidate must prove the registered hosted job itself.
# Do not consult, accept, or veto on the candidate-produced local ledger. This
# is a selector, not a fallback: missing/partial hosted evidence is NO_RESULT.
if [[ $hosted_only -eq 1 ]]; then
    case $hosted_state in
        green)
            printf 'AUTHORITY=hosted LOCAL=not_consulted HOSTED=green LOCAL_COVERAGE=none HOSTED_COVERAGE=portable REQUIRED_COVERAGE=hosted:portable SHA=%s\n' "$sha"
            exit 0
            ;;
        red)
            printf 'AUTHORITY=refused LOCAL=not_consulted HOSTED=red LOCAL_COVERAGE=none HOSTED_COVERAGE=none REQUIRED_COVERAGE=hosted:portable SHA=%s\n' "$sha"
            exit 3
            ;;
        *)
            printf 'AUTHORITY=no_result LOCAL=not_consulted HOSTED=no_result LOCAL_COVERAGE=none HOSTED_COVERAGE=none REQUIRED_COVERAGE=hosted:portable SHA=%s\n' "$sha"
            exit 4
            ;;
    esac
fi

if [[ $local_state == red || $hosted_state == red ]]; then
    printf 'AUTHORITY=refused LOCAL=%s HOSTED=%s LOCAL_COVERAGE=%s HOSTED_COVERAGE=%s REQUIRED_COVERAGE=%s SHA=%s\n' \
        "$local_state" "$hosted_state" "$local_coverage" "$hosted_coverage" \
        "$required_coverage" "$sha"
    exit 3
fi
if [[ $repo == rrnewton/hermit && ( $local_state == green || $hosted_state == green ) ]]; then
    if [[ $local_state == green && $hosted_state == green ]]; then
        authority=local+hosted
    elif [[ $local_state == green ]]; then
        authority=local
    else
        authority=hosted
    fi
    printf 'AUTHORITY=%s LOCAL=%s HOSTED=%s LOCAL_COVERAGE=%s HOSTED_COVERAGE=%s REQUIRED_COVERAGE=%s SHA=%s\n' \
        "$authority" "$local_state" "$hosted_state" "$local_coverage" \
        "$hosted_coverage" "$required_coverage" "$sha"
    exit 0
fi
if [[ $repo == rrnewton/reverie && $hosted_state == green ]]; then
    printf 'AUTHORITY=hosted LOCAL=%s HOSTED=%s LOCAL_COVERAGE=%s HOSTED_COVERAGE=%s REQUIRED_COVERAGE=%s SHA=%s\n' \
        "$local_state" "$hosted_state" "$local_coverage" "$hosted_coverage" \
        "$required_coverage" "$sha"
    exit 0
fi
printf 'AUTHORITY=no_result LOCAL=%s HOSTED=%s LOCAL_COVERAGE=%s HOSTED_COVERAGE=%s REQUIRED_COVERAGE=%s SHA=%s\n' \
    "$local_state" "$hosted_state" "$local_coverage" "$hosted_coverage" \
    "$required_coverage" "$sha"
exit 4
