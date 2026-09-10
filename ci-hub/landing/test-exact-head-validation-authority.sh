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
            if [[ $repo == rrnewton/reverie ]]; then
                reverie_policy=${REVERIE_POLICY:-reverie-local-validation/v4}
                case $reverie_policy in
                    reverie-local-validation/v2)
                        planned=2
                        executed_nodes=2
                        gates='[{"name":"Cross-client skill discovery","result":"pass"},{"name":"Build workspace","result":"pass"},{"name":"Test regular workspace cases","result":"pass"},{"name":"Documentation tests","result":"pass"},{"name":"Clippy","result":"pass"},{"name":"Rustfmt","result":"pass"}]'
                        ;;
                    reverie-local-validation/v4)
                        planned=3
                        executed_nodes=3
                        gates='[{"name":"Cross-client skill discovery","result":"pass"},{"name":"Build workspace","result":"pass"},{"name":"DBT virtual identity and pidfd_open policy","result":"pass"},{"name":"Compile owned public instruction target","result":"pass"},{"name":"Inventory owned public instruction target","result":"pass"},{"name":"Test all owned public instruction cases","result":"pass"},{"name":"Test regular workspace cases","result":"pass"},{"name":"Documentation tests","result":"pass"},{"name":"Clippy","result":"pass"},{"name":"Rustfmt","result":"pass"}]'
                        ;;
                    *)
                        planned=2
                        executed_nodes=2
                        gates='[{"name":"Cross-client skill discovery","result":"pass"},{"name":"Build workspace","result":"pass"},{"name":"DBT virtual identity and pidfd_open policy","result":"pass"},{"name":"Test regular workspace cases","result":"pass"},{"name":"Documentation tests","result":"pass"},{"name":"Clippy","result":"pass"},{"name":"Rustfmt","result":"pass"}]'
                        ;;
                esac
            else
                planned=19
                executed_nodes=19
                gates='[{"name":"test.strict_compat","result":"pass"},{"name":"e2e.metadata","result":"pass"}]'
                reverie_policy=
            fi
            if [[ ${LOCAL_COVERAGE:-full} == partial ]]; then
                executed_nodes=$((planned - 1))
            fi
            printf '{"repo":"%s","sha":"%s","verdict":"VALIDATED","exit_code":0,"qualifying_count":%s,"newest_qualifying":{"sha":"%s","profile":"full","selection_mode":"full","coverage_satisfied":true,"coverage_status":"satisfied","coverage":{"planned_test_nodes":%s,"executed_test_nodes":%s,"absent_nodes":[],"zero_executed_nodes":[]},"reverie_validation_policy":"%s","gates":%s,"failures":0,"executed_tests":862,"filtered_tests":350}}\n' "$repo" "$observed_sha" "$qualifying_count" "$observed_sha" "$planned" "$executed_nodes" "$reverie_policy" "$gates"
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

run_local_only_case() {
    local expected=$1 label=$2
    shift 2
    local rc=0 output
    output=$(env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
        EXACT_HEAD_GH_BIN="$tmp/gh-api" EXPECTED_SHA="$sha" \
        "$@" "$authority" --local-only --repo rrnewton/hermit --sha "$sha") || rc=$?
    if [[ $rc -ne $expected ]]; then
        printf 'FAIL: %s expected rc=%s got rc=%s output=%s\n' \
            "$label" "$expected" "$rc" "$output" >&2
        exit 1
    fi
    last_output=$output
}

run_reverie_local_only_case() {
    local expected=$1 label=$2
    shift 2
    local rc=0 output
    output=$(env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
        EXACT_HEAD_GH_BIN="$tmp/gh-api" EXPECTED_SHA="$sha" \
        "$@" "$authority" --local-only --repo rrnewton/reverie --sha "$sha") || rc=$?
    if [[ $rc -ne $expected ]]; then
        printf 'FAIL: %s expected rc=%s got rc=%s output=%s\n' \
            "$label" "$expected" "$rc" "$output" >&2
        exit 1
    fi
    last_output=$output
}

run_hosted_only_case() {
    local expected=$1 label=$2
    shift 2
    local rc=0 output
    output=$(env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
        EXACT_HEAD_GH_BIN="$tmp/gh-api" EXPECTED_SHA="$sha" \
        "$@" "$authority" --hosted-only --repo rrnewton/hermit --sha "$sha") || rc=$?
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

# Hermit's landing consumer is deliberately narrower than the historical OR
# evaluator above: only the local ledger decides. These cases bracket both
# directions and prove hosted state is not even consulted as a fallback/veto.
run_local_only_case 0 "local-only admits qualifying ledger green despite hosted red" \
    env LOCAL_STATE=green HOSTED_STATE=red
[[ $last_output == *"AUTHORITY=local "* && $last_output == *"HOSTED=not_consulted"* ]] || {
    echo "FAIL: local-only green did not exclude hosted authority" >&2; exit 1;
}
run_local_only_case 4 "local-only refuses hosted-only green" \
    env LOCAL_STATE=no_result HOSTED_STATE=green
run_local_only_case 3 "local-only preserves genuine ledger red" \
    env LOCAL_STATE=red HOSTED_STATE=green
rc=0
env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
    LOCAL_STATE=green HOSTED_STATE=green RECEIPT_RC=1 \
    "$authority" --local-only --repo rrnewton/hermit --sha "$sha" --comments "$comments" \
    >"$tmp/local-only-unbound" || rc=$?
[[ $rc -eq 4 ]] || { echo "FAIL: local-only accepted an unbound receipt via hosted fallback" >&2; exit 1; }

# Reverie's centralized full-profile row is likewise the exact-head landing
# authority. Hosted state is diagnostic and never a fallback or veto here.
run_reverie_local_only_case 0 "Reverie local-only admits qualifying ten-gate v4 row" \
    env LOCAL_STATE=green HOSTED_STATE=red
[[ $last_output == *"AUTHORITY=local "* && \
   $last_output == *"LOCAL_COVERAGE=full-ten-gate-v4"* && \
   $last_output == *"HOSTED=not_consulted"* ]] || {
    echo "FAIL: Reverie local-only green did not select its ten-gate v4 authority" >&2; exit 1;
}
run_reverie_local_only_case 0 "Reverie local-only preserves qualifying seven-gate v3 authority" \
    env LOCAL_STATE=green HOSTED_STATE=red REVERIE_POLICY=reverie-local-validation/v3
[[ $last_output == *"AUTHORITY=local "* && \
   $last_output == *"LOCAL_COVERAGE=full-seven-gate-v3"* && \
   $last_output == *"HOSTED=not_consulted"* ]] || {
    echo "FAIL: Reverie local-only green did not preserve its seven-gate v3 authority" >&2; exit 1;
}
run_reverie_local_only_case 0 "Reverie local-only preserves qualifying six-gate v2 authority" \
    env LOCAL_STATE=green HOSTED_STATE=red REVERIE_POLICY=reverie-local-validation/v2
[[ $last_output == *"AUTHORITY=local "* && \
   $last_output == *"LOCAL_COVERAGE=full-six-gate-v2"* && \
   $last_output == *"HOSTED=not_consulted"* ]] || {
    echo "FAIL: Reverie local-only green did not preserve its six-gate v2 authority" >&2; exit 1;
}
run_reverie_local_only_case 0 "Reverie local-only preserves qualifying seven-gate v1 authority" \
    env LOCAL_STATE=green HOSTED_STATE=red REVERIE_POLICY=reverie-local-validation/v1
[[ $last_output == *"AUTHORITY=local "* && \
   $last_output == *"LOCAL_COVERAGE=full-seven-gate-v1"* && \
   $last_output == *"HOSTED=not_consulted"* ]] || {
    echo "FAIL: Reverie local-only green did not preserve its seven-gate v1 authority" >&2; exit 1;
}
run_reverie_local_only_case 4 "Reverie local-only refuses an unknown receipt policy" \
    env LOCAL_STATE=green HOSTED_STATE=red REVERIE_POLICY=reverie-local-validation/v999
run_reverie_local_only_case 4 "Reverie local-only refuses hosted-only green" \
    env LOCAL_STATE=no_result HOSTED_STATE=green
run_reverie_local_only_case 3 "Reverie local-only preserves genuine ledger red" \
    env LOCAL_STATE=red HOSTED_STATE=green

# The producer exception is the inverse selector: only registered hosted
# evidence decides, and candidate-produced local evidence is not consulted.
run_hosted_only_case 0 "hosted-only admits registered portable green despite local red" \
    env LOCAL_STATE=red HOSTED_STATE=green
[[ $last_output == *"AUTHORITY=hosted "* && $last_output == *"LOCAL=not_consulted"* ]] || {
    echo "FAIL: hosted-only green consulted or selected local authority" >&2; exit 1;
}
run_hosted_only_case 4 "hosted-only refuses local-only green" \
    env LOCAL_STATE=green HOSTED_STATE=no_result
run_hosted_only_case 3 "hosted-only preserves genuine hosted red" \
    env LOCAL_STATE=green HOSTED_STATE=red
run_hosted_only_case 4 "hosted-only refuses partial hosted evidence" \
    env LOCAL_STATE=green HOSTED_STATE=partial

rc=0
env CI_HUB_BIN="$tmp/ci-hub" VERIFY_RECEIPT_BIN="$tmp/verify-receipt" \
    LOCAL_STATE=green HOSTED_STATE=green \
    "$authority" --local-only --hosted-only --repo rrnewton/hermit --sha "$sha" \
    >/dev/null 2>&1 || rc=$?
[[ $rc -eq 2 ]] || { echo "FAIL: mutually exclusive authority selectors were accepted" >&2; exit 1; }

echo "PASS: historical local/hosted OR positives=5; Hermit local-only positive=1 negatives=3; Reverie local-only positives=4 negatives=3; hosted-only positive=1 negatives=4; cancellation no-results=2; red/partial/stale/unbound/count/coverage/policy negatives=14"
