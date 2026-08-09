#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
authority=$script_dir/exact-head-validation-authority.sh
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
stale=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
comments=$tmp/comments.json
printf '[]\n' >"$comments"

cat >"$tmp/ci-hub" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
command=$1
shift
repo= sha=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo) repo=$2; shift 2 ;;
        --sha) sha=$2; shift 2 ;;
        --json) shift ;;
        *) shift ;;
    esac
done
observed_sha=${STALE_SHA:-$sha}
if [[ $command == validate-status ]]; then
    case ${LOCAL_STATE:-no_result} in
        green)
            qualifying_count=${LOCAL_COUNT:-1}
            if [[ ${LOCAL_COVERAGE:-full} == partial ]]; then
                planned=19
                executed_nodes=18
            else
                planned=19
                executed_nodes=19
            fi
            gates='[{"name":"test.strict_compat","result":"pass"},{"name":"e2e.metadata","result":"pass"}]'
            printf '{"repo":"%s","sha":"%s","verdict":"VALIDATED","exit_code":0,"qualifying_count":%s,"newest_qualifying":{"sha":"%s","profile":"full","selection_mode":"full","coverage_satisfied":true,"coverage_status":"satisfied","coverage":{"planned_test_nodes":%s,"executed_test_nodes":%s,"absent_nodes":[],"zero_executed_nodes":[]},"gates":%s,"failures":0,"executed_tests":862,"filtered_tests":350}}\n' "$repo" "$observed_sha" "$qualifying_count" "$observed_sha" "$planned" "$executed_nodes" "$gates"
            exit 0 ;;
        red)
            printf '{"repo":"%s","sha":"%s","verdict":"FAILED","exit_code":3,"qualifying_count":0,"newest_qualifying":null}\n' "$repo" "$observed_sha"
            exit 3 ;;
        *)
            printf '{"repo":"%s","sha":"%s","verdict":"NO-RESULT","exit_code":4,"qualifying_count":0,"newest_qualifying":null}\n' "$repo" "$observed_sha"
            exit 4 ;;
    esac
fi
case ${HOSTED_STATE:-no_result} in
    green)
        policy_schema=3
        job_name='Regular tests (GitHub-managed portable)'
        [[ ${HOSTED_POLICY:-valid} == wrong-schema ]] && policy_schema=2
        [[ ${HOSTED_POLICY:-valid} == wrong-job ]] && job_name='Privileged capability and E2E tests'
        printf '{"authority":"github-actions-exact-head-jobs","repo":"%s","sha":"%s","policy_schema_version":%s,"state":"green","required_positive_count":1,"positive_count":1,"jobs":[{"state":"green","run_id":1,"job_id":2,"job_name":"%s"}]}\n' "$repo" "$observed_sha" "$policy_schema" "$job_name"
        exit 0 ;;
    red)
        printf '{"authority":"github-actions-exact-head-jobs","repo":"%s","sha":"%s","policy_schema_version":3,"state":"red","required_positive_count":1,"positive_count":0,"jobs":[{"state":"red","status":"completed","conclusion":"failure","run_id":1,"job_id":2,"job_name":"Regular tests (GitHub-managed portable)"}]}\n' "$repo" "$observed_sha"
        exit 3 ;;
    partial)
        printf '{"authority":"github-actions-exact-head-jobs","repo":"%s","sha":"%s","state":"no_result","required_positive_count":2,"positive_count":1,"jobs":[{"state":"green","run_id":1,"job_id":2},{"state":"no_result","run_id":1,"job_id":3}]}\n' "$repo" "$observed_sha"
        exit 4 ;;
    *)
        printf '{"authority":"github-actions-exact-head-jobs","repo":"%s","sha":"%s","state":"no_result","required_positive_count":1,"positive_count":0,"jobs":[]}\n' "$repo" "$observed_sha"
        exit 4 ;;
esac
SH
cat >"$tmp/gh-api" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case ${GITHUB_EVIDENCE:-missing}:$1 in
    cancel-derived:repos/rrnewton/hermit/actions/runs/1|\
    leaf-failure:repos/rrnewton/hermit/actions/runs/1|\
    aggregate-double-failure:repos/rrnewton/hermit/actions/runs/1)
        printf '{"head_sha":"%s","status":"completed","conclusion":"cancelled","name":"CI (GitHub-managed portable)","path":".github/workflows/ci-portable.yml"}\n' "$EXPECTED_SHA"
        ;;
    stale-run:repos/rrnewton/hermit/actions/runs/1)
        printf '{"head_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","status":"completed","conclusion":"cancelled","name":"CI (GitHub-managed portable)","path":".github/workflows/ci-portable.yml"}\n'
        ;;
    cancel-derived:repos/rrnewton/hermit/actions/runs/1/jobs?filter=latest\&per_page=100|\
    aggregate-double-failure:repos/rrnewton/hermit/actions/runs/1/jobs?filter=latest\&per_page=100|\
    stale-run:repos/rrnewton/hermit/actions/runs/1/jobs?filter=latest\&per_page=100)
        printf '%s\n' '{"total_count":3,"jobs":[{"id":2,"name":"Regular tests (GitHub-managed portable)","status":"completed","conclusion":"failure"},{"id":3,"name":"test: strict-compat","status":"completed","conclusion":"cancelled"},{"id":4,"name":"test: unit","status":"completed","conclusion":"success"}]}'
        ;;
    leaf-failure:repos/rrnewton/hermit/actions/runs/1/jobs?filter=latest\&per_page=100)
        printf '%s\n' '{"total_count":4,"jobs":[{"id":2,"name":"Regular tests (GitHub-managed portable)","status":"completed","conclusion":"failure"},{"id":3,"name":"test: strict-compat","status":"completed","conclusion":"cancelled"},{"id":4,"name":"test: unit","status":"completed","conclusion":"success"},{"id":5,"name":"test: integration","status":"completed","conclusion":"failure"}]}'
        ;;
    cancel-derived:repos/rrnewton/hermit/actions/jobs/2|\
    leaf-failure:repos/rrnewton/hermit/actions/jobs/2|\
    stale-run:repos/rrnewton/hermit/actions/jobs/2)
        printf '%s\n' '{"id":2,"name":"Regular tests (GitHub-managed portable)","status":"completed","conclusion":"failure","steps":[{"name":"Set up job","status":"completed","conclusion":"success"},{"name":"Require every portable DAG job to succeed or be deselected","status":"completed","conclusion":"failure"},{"name":"Complete job","status":"completed","conclusion":"success"}]}'
        ;;
    aggregate-double-failure:repos/rrnewton/hermit/actions/jobs/2)
        printf '%s\n' '{"id":2,"name":"Regular tests (GitHub-managed portable)","status":"completed","conclusion":"failure","steps":[{"name":"Verify completeness","status":"completed","conclusion":"failure"},{"name":"Require every portable DAG job to succeed or be deselected","status":"completed","conclusion":"failure"},{"name":"Complete job","status":"completed","conclusion":"success"}]}'
        ;;
    *) exit 1 ;;
esac
SH
cat >"$tmp/verify-receipt" <<'SH'
#!/usr/bin/env bash
exit "${RECEIPT_RC:-0}"
SH
chmod +x "$tmp/ci-hub" "$tmp/gh-api" "$tmp/verify-receipt"

run_case() {
    local expected=$1 label=$2
    shift 2
    local rc=0 output
    output=$(env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
        EXACT_HEAD_GH_BIN="$tmp/gh-api" EXPECTED_SHA="$sha" \
        "$@" "$authority" --repo rrnewton/hermit --sha "$sha") || rc=$?
    if [[ $rc -ne $expected ]]; then
        printf 'FAIL: %s expected rc=%s got rc=%s output=%s\n' \
            "$label" "$expected" "$rc" "$output" >&2
        exit 1
    fi
    last_output=$output
}

run_case 0 "current Rust schema-5 counted local exact-head positive" env LOCAL_STATE=green HOSTED_STATE=no_result
[[ $last_output == *"LOCAL_COVERAGE=portable+privileged"* ]] || {
    echo "FAIL: local authority did not name its full coverage" >&2; exit 1;
}
[[ $last_output == *"AUTHORITY=local "* ]] || {
    echo "FAIL: local-only positive did not select local authority" >&2; exit 1;
}
run_case 0 "versioned hosted exact-head positive" env LOCAL_STATE=no_result HOSTED_STATE=green
[[ $last_output == *"HOSTED_COVERAGE=portable"* ]] || {
    echo "FAIL: hosted authority did not name its versioned coverage" >&2; exit 1;
}
[[ $last_output == *"AUTHORITY=hosted "* ]] || {
    echo "FAIL: hosted-only positive did not select hosted authority" >&2; exit 1;
}
[[ $last_output == *"REQUIRED_COVERAGE=local:portable+privileged|hosted:portable"* ]] || {
    echo "FAIL: authority did not name the OR coverage policy" >&2; exit 1;
}
run_case 0 "both exact-head positives" env LOCAL_STATE=green HOSTED_STATE=green
[[ $last_output == *"AUTHORITY=local+hosted "* ]] || {
    echo "FAIL: dual positive did not report both authorities" >&2; exit 1;
}
run_case 3 "local genuine red blocks hosted green" env LOCAL_STATE=red HOSTED_STATE=green
run_case 3 "hosted genuine red blocks local green" env LOCAL_STATE=green HOSTED_STATE=red
run_case 0 "cancelled leaf makes aggregate failure no-result" env LOCAL_STATE=green HOSTED_STATE=red GITHUB_EVIDENCE=cancel-derived
[[ $last_output == *"AUTHORITY=local "* && $last_output == *"HOSTED=no_result"* ]] || {
    echo "FAIL: cancellation-derived absence did not preserve local authority" >&2; exit 1;
}
run_case 4 "cancelled leaf alone is not a hosted positive" env LOCAL_STATE=no_result HOSTED_STATE=red GITHUB_EVIDENCE=cancel-derived
run_case 3 "genuine leaf failure still vetoes local green" env LOCAL_STATE=green HOSTED_STATE=red GITHUB_EVIDENCE=leaf-failure
run_case 3 "independent aggregate failure still vetoes local green" env LOCAL_STATE=green HOSTED_STATE=red GITHUB_EVIDENCE=aggregate-double-failure
run_case 3 "stale run cannot downgrade hosted red" env LOCAL_STATE=green HOSTED_STATE=red GITHUB_EVIDENCE=stale-run
run_case 4 "both authorities no-result" env LOCAL_STATE=no_result HOSTED_STATE=no_result
run_case 4 "partial hosted set is no-result" env LOCAL_STATE=no_result HOSTED_STATE=partial
run_case 4 "stale local evidence is refused" env LOCAL_STATE=green HOSTED_STATE=no_result STALE_SHA="$stale"
run_case 4 "stale hosted evidence is refused" env LOCAL_STATE=no_result HOSTED_STATE=green STALE_SHA="$stale"
run_case 4 "uncounted local report is refused" env LOCAL_STATE=green HOSTED_STATE=no_result LOCAL_COUNT=0
run_case 4 "synthetic-name-free incomplete local coverage is refused" env LOCAL_STATE=green HOSTED_STATE=no_result LOCAL_COVERAGE=partial
run_case 4 "wrong hosted policy schema is refused" env LOCAL_STATE=no_result HOSTED_STATE=green HOSTED_POLICY=wrong-schema
run_case 4 "wrong hosted policy job is refused" env LOCAL_STATE=no_result HOSTED_STATE=green HOSTED_POLICY=wrong-job

rc=0
env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
    LOCAL_STATE=green HOSTED_STATE=no_result RECEIPT_RC=1 \
    "$authority" --repo rrnewton/hermit --sha "$sha" --comments "$comments" \
    >/dev/null || rc=$?
[[ $rc -eq 4 ]] || { echo "FAIL: unbound local receipt passed final boundary" >&2; exit 1; }
rc=0
env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
    LOCAL_STATE=green HOSTED_STATE=green RECEIPT_RC=1 \
    "$authority" --repo rrnewton/hermit --sha "$sha" --comments "$comments" \
    >"$tmp/receipt-fallback" || rc=$?
[[ $rc -eq 0 ]] || { echo "FAIL: hosted authority did not survive local receipt refusal" >&2; exit 1; }
grep -Fq 'AUTHORITY=hosted ' "$tmp/receipt-fallback" || {
    echo "FAIL: receipt refusal did not downgrade the local path" >&2; exit 1;
}

echo "PASS: local/hosted OR positives=5; cancellation no-results=2; red/partial/stale/unbound/count/coverage/policy negatives=14"
