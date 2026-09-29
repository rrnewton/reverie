#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/../.." && pwd)
hub=$root/ci-hub/ci-hub
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
state_root=$tmp/state

other_sha=dddddddddddddddddddddddddddddddddddddddd
receipt_commit=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
wrong_receipt_commit=cccccccccccccccccccccccccccccccccccccccc
pr=1976
deleted_cwd=$tmp/deleted-validation-cwd
log_file=$tmp/validate.log
printf 'running 12 tests\ntest result: ok. 12 passed; 0 failed\n' >"$log_file"
log_sha256=$(sha256sum "$log_file" | awk '{print $1}')
# Any real Hermit commit: the receipt records its producer blobs as provenance
# only (https://github.com/rrnewton/dev-hermit/issues/414).
sha=ce1aca3aba3a4bb1db244e1ca2ab6903ea4bb7a8
producer_record=$tmp/producer-record.json
"$root/ci-hub/validation/verify_receipt.sh" --producer-definition-observe \
    --repo rrnewton/hermit --sha "$sha" --repo-checkout "$root/hermit" \
    >"$producer_record"
mkdir -p "$state_root"
ln -s "$root/hermit" "$state_root/hermit"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

hub_call() {
    CI_HUB_TOOL_COST_ACTIVE=1 "$hub" "$@"
}

jq -cn \
    --arg sha "$sha" --arg log "$log_file" --arg cwd "$deleted_cwd" '{
      schema_version: 4,
      started_at: "2026-08-08T10:01:14Z",
      finished_at: "2026-08-08T10:11:14Z",
      host: "fixture-host",
      slot: "fixture-slot",
      repo: "hermit",
      cwd: $cwd,
      profile: "full",
      selection_mode: "full",
      commit: $sha,
      tree: ("e" * 40),
      commit_anchored: true,
      tree_dirty: false,
      result: "pass",
      raw_result: "pass",
      exit_code: 0,
      executed_tests: 12,
      filtered_tests: 3,
      checks: 2,
      gates_run: 2,
      gates_expected: 2,
      failures: 0,
      log_file: $log,
      gates: [
        {name: "fmt", result: "pass", exit_code: 0},
        {name: "test", result: "pass", exit_code: 0}
      ]
    }' >"$tmp/base-row.json"
git clone --shared --no-checkout "$root/hermit" "$deleted_cwd" >/dev/null 2>&1
git -C "$deleted_cwd" checkout --detach "$sha" >/dev/null 2>&1
DEV_HERMIT_PARENT="$state_root" \
    python3 "$root/ci-hub/ledger/validate_rows.py" record \
    <"$tmp/base-row.json" >"$tmp/record.json"

artifact_counter=0
build_artifact() { # build_artifact ROW TARGET_SHA
    local row=$1 target_sha=$2
    local canonical artifact
    artifact_counter=$((artifact_counter + 1))
    canonical=$tmp/canonical-$artifact_counter.json
    artifact=$tmp/artifact-$artifact_counter.json
    hub_call receipt-digest --sha "$target_sha" --canonical-row \
        <"$row" >"$canonical"
    ARTIFACT_SELECTED=$(sha256sum "$canonical" | awk '{print $1}')
    jq -cn --slurpfile row "$canonical" --slurpfile producer "$producer_record" \
        --arg sha "$target_sha" --arg selected "$ARTIFACT_SELECTED" \
        --arg log_sha "$log_sha256" --arg cwd "$deleted_cwd" '{
          schema_version: 1,
          repository: "rrnewton/hermit",
          commit: $sha,
          run_id: ($sha + "@" + $row[0].started_at + "@" + $row[0].host),
          source_log_file: $row[0].log_file,
          durable_log_file: "/durable/validate.log",
          log_sha256: $log_sha,
          producer: {
            resolved_from: $cwd,
            definition: $producer[0].definition,
            coverage_status: $producer[0].coverage_status,
            paths: ($producer[0].definition | keys | sort)
          },
          selected_receipt_identity: {
            digest_algorithm: "sha256",
            canonicalization: "serde_json::to_vec(HistoryRow)-v1",
            digest: $selected
          },
          ledger_record: $row[0]
        }' >"$artifact"
    ARTIFACT_FILE=$artifact
    ARTIFACT_SHA256=$(sha256sum "$artifact" | awk '{print $1}')
    ARTIFACT_PATH="validation-receipts/rrnewton/hermit/$target_sha/$ARTIFACT_SHA256.json"
}

write_comments() { # write_comments COMMIT PATH DIGEST
    local commit=$1 path=$2 digest=$3
    jq -cn --arg commit "$commit" --arg path "$path" --arg digest "$digest" '[[{
      user: {login: "rrnewton"},
      body: ("[coordinator, gpt-5.6-sol]\n\n<!-- locally-validated-receipt commit="
             + $commit + " path=" + $path + " sha256=" + $digest + " -->")
    }]]' >"$tmp/comments.json"
}

write_two_comments() { # exact marker first; newer nonselected marker second
    local exact_path=$1 exact_digest=$2 other_path=$3 other_digest=$4
    jq -cn --arg commit "$receipt_commit" \
        --arg exact_path "$exact_path" --arg exact_digest "$exact_digest" \
        --arg other_path "$other_path" --arg other_digest "$other_digest" '[[
      {
        user: {login: "rrnewton"},
        body: ("[coordinator, gpt-5.6-sol]\n\n<!-- locally-validated-receipt commit="
               + $commit + " path=" + $exact_path + " sha256=" + $exact_digest + " -->")
      },
      {
        user: {login: "rrnewton"},
        body: ("[coordinator, gpt-5.6-sol]\n\n<!-- locally-validated-receipt commit="
               + $commit + " path=" + $other_path + " sha256=" + $other_digest + " -->")
      }
    ]]' >"$tmp/comments.json"
}

artifact_map=$tmp/artifacts.tsv
register_artifact() { # register_artifact COMMIT
    printf '%s\t%s\t%s\n' "$1" "$ARTIFACT_PATH" "$ARTIFACT_FILE" >>"$artifact_map"
}

mkdir -p "$tmp/bin"
cat >"$tmp/bin/with-proxy" <<'EOF'
#!/usr/bin/env bash
exec "$@"
EOF
cat >"$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s ' "$@" >>"$FAKE_GH_LOG"
printf '\n' >>"$FAKE_GH_LOG"
args=" $* "
if [[ ${1:-} == api &&
      $args == *" repos/rrnewton/hermit/commits/$FAKE_HEAD/status?per_page=100 "* ]]; then
    printf '{"statuses":[]}\n'
    exit 0
fi
if [[ ${1:-} == api && $args == *" --method POST "* &&
      $args == *" repos/rrnewton/hermit/statuses/$FAKE_HEAD "* ]]; then
    printf 'STATUS\n' >>"$FAKE_MUTATION_LOG"
    exit 0
fi
if [[ $args == *" --method PUT "* || $args == *" --method POST "* ||
      $args == *" --method PATCH "* || $args == *" --method DELETE "* ]]; then
    printf 'REMOTE_MUTATION %s\n' "$*" >>"$FAKE_MUTATION_LOG"
    exit 91
fi
if [[ ${1:-} == pr && ${2:-} == view ]]; then
    printf '{"headRefOid":"%s","labels":[]}\n' "$FAKE_HEAD"
    exit 0
fi
if [[ ${1:-} == pr && ${2:-} == edit ]]; then
    printf 'LABEL\n' >>"$FAKE_MUTATION_LOG"
    exit 0
fi
if [[ ${1:-} == pr && ${2:-} == comment ]]; then
    printf 'COMMENT\n' >>"$FAKE_MUTATION_LOG"
    exit 0
fi
if [[ ${1:-} == api && $args == *" repos/rrnewton/hermit/issues/$FAKE_PR/comments?per_page=100 "* ]]; then
    cat "$FAKE_COMMENTS_FILE"
    exit 0
fi
if [[ ${1:-} == api && $args == *" repos/rrnewton/dev-hermit/compare/"* ]]; then
    endpoint=
    for argument in "$@"; do
        [[ $argument == repos/rrnewton/dev-hermit/compare/* ]] && endpoint=$argument
    done
    [[ -n $endpoint ]] || exit 92
    compared=${endpoint#repos/rrnewton/dev-hermit/compare/}
    compared=${compared%%...*}
    if [[ -n ${FAKE_REJECT_COMMIT:-} && $compared == "$FAKE_REJECT_COMMIT" ]]; then
        printf 'diverged\n'
    else
        printf 'ahead\n'
    fi
    exit 0
fi
if [[ ${1:-} == api && $args == *" repos/rrnewton/dev-hermit/contents/"* ]]; then
    endpoint=
    for argument in "$@"; do
        [[ $argument == repos/rrnewton/dev-hermit/contents/* ]] && endpoint=$argument
    done
    [[ -n $endpoint ]] || exit 93
    identity=${endpoint#repos/rrnewton/dev-hermit/contents/}
    path=${identity%%\?ref=*}
    commit=${identity#*\?ref=}
    artifact=$(awk -F '\t' -v commit="$commit" -v path="$path" \
        '$1 == commit && $2 == path {print $3; exit}' "$FAKE_ARTIFACT_MAP")
    [[ -n $artifact && -f $artifact ]] || exit 93
    if [[ $args == *" --jq .content "* ]]; then
        base64 "$artifact" | tr -d '\n'
        printf '\n'
    else
        cat "$artifact"
    fi
    exit 0
fi
printf 'unsupported fake gh invocation: %s\n' "$*" >&2
exit 94
EOF
chmod +x "$tmp/bin/with-proxy" "$tmp/bin/gh"

invoke_apply() {
    PATH="$tmp/bin:$PATH" \
    CI_HUB_TOOL_COST_ACTIVE=1 \
    FAKE_HEAD="$sha" \
    FAKE_PR="$pr" \
    FAKE_COMMENTS_FILE="$tmp/comments.json" \
    FAKE_ARTIFACT_MAP="$artifact_map" \
    FAKE_REJECT_COMMIT="${REJECT_COMMIT:-}" \
    FAKE_GH_LOG="$tmp/gh.log" \
    FAKE_MUTATION_LOG="$tmp/mutations.log" \
    DEV_HERMIT_PARENT="$state_root" \
    "$hub" apply-local-label --pr "$pr" --repo rrnewton/hermit --json
}

prepare_run_logs() {
    : >"$tmp/gh.log"
    : >"$tmp/mutations.log"
}

prepare_scenario() {
    prepare_run_logs
    : >"$artifact_map"
}

prepare_scenario
build_artifact "$tmp/base-row.json" "$sha"
register_artifact "$receipt_commit"
exact_path=$ARTIFACT_PATH
exact_digest=$ARTIFACT_SHA256
jq '.finished_at = "2026-08-08T10:12:14Z"' \
    "$tmp/base-row.json" >"$tmp/positive-other-row.json"
build_artifact "$tmp/positive-other-row.json" "$sha"
register_artifact "$receipt_commit"
other_path=$ARTIFACT_PATH
other_digest=$ARTIFACT_SHA256
[[ $exact_path != "$other_path" ]] || fail "positive receipt variants did not differ"
write_two_comments "$exact_path" "$exact_digest" "$other_path" "$other_digest"
# Immutable reuse must not depend on either ephemeral producer input: the
# recorded checkout and source log may both have disappeared by binding time.
# A rejected immutable artifact must then fail locally at the missing log,
# before the fallback publisher can perform any remote mutation.
rm -rf -- "$deleted_cwd"
rm -f -- "$log_file"
REJECT_COMMIT=
invoke_apply >"$tmp/positive.out" 2>"$tmp/positive.err" || \
    fail "deleted-cwd/missing-log exact immutable artifact was not reused: $(cat "$tmp/positive.err")"
grep -Fq '"receipt_source": "reused-immutable"' "$tmp/positive.out" || \
    fail "positive result did not report immutable reuse"
grep -Fq "contents/$other_path?ref=$receipt_commit" "$tmp/gh.log" || \
    fail "positive result did not inspect the newer nonselected receipt"
grep -Fq "contents/$exact_path?ref=$receipt_commit" "$tmp/gh.log" || \
    fail "positive result did not continue to the exact selected receipt"
[[ $(cat "$tmp/mutations.log") == $'STATUS\nLABEL' ]] || \
    fail "positive result did not publish only the commit status and label"
if grep -Fq 'git/ref/heads/validation-receipts' "$tmp/gh.log"; then
    fail "positive result invoked the receipt publisher"
fi
[[ ! -e $tmp/validation-evidence ]] || \
    fail "positive reuse copied the log through the publisher"

run_negative() { # run_negative NAME
    local name=$1 status=0
    prepare_run_logs
    invoke_apply >"$tmp/$name.out" 2>"$tmp/$name.err" || status=$?
    [[ $status -ne 0 ]] || fail "$name unexpectedly succeeded"
    grep -Fq "ledger log is not a readable absolute file" "$tmp/$name.err" || \
        fail "$name did not fail locally at the missing source log: $(cat "$tmp/$name.err")"
    [[ ! -s $tmp/mutations.log ]] || \
        fail "$name performed a label/comment/publish mutation: $(cat "$tmp/mutations.log")"
    if grep -Fq 'git/ref/heads/validation-receipts' "$tmp/gh.log"; then
        fail "$name reached the remote publisher"
    fi
}

jq --arg sha "$other_sha" '.commit = $sha | .tree = ("d" * 40)' \
    "$tmp/base-row.json" >"$tmp/wrong-head-row.json"
prepare_scenario
build_artifact "$tmp/wrong-head-row.json" "$other_sha"
register_artifact "$receipt_commit"
write_comments "$receipt_commit" "$ARTIFACT_PATH" "$ARTIFACT_SHA256"
REJECT_COMMIT=
run_negative wrong-head

jq '.finished_at = "2026-08-08T10:12:14Z"' \
    "$tmp/base-row.json" >"$tmp/wrong-row.json"
prepare_scenario
build_artifact "$tmp/wrong-row.json" "$sha"
register_artifact "$receipt_commit"
write_comments "$receipt_commit" "$ARTIFACT_PATH" "$ARTIFACT_SHA256"
REJECT_COMMIT=
run_negative wrong-row-digest

prepare_scenario
build_artifact "$tmp/base-row.json" "$sha"
write_comments "$receipt_commit" "$ARTIFACT_PATH" "$ARTIFACT_SHA256"
printf 'tampered\n' >>"$ARTIFACT_FILE"
register_artifact "$receipt_commit"
REJECT_COMMIT=
run_negative wrong-artifact-hash

prepare_scenario
build_artifact "$tmp/base-row.json" "$sha"
register_artifact "$wrong_receipt_commit"
write_comments "$wrong_receipt_commit" "$ARTIFACT_PATH" "$ARTIFACT_SHA256"
REJECT_COMMIT=$wrong_receipt_commit
run_negative wrong-receipt-commit

prepare_scenario
build_artifact "$tmp/base-row.json" "$sha"
register_artifact "$receipt_commit"
printf '[[]]\n' >"$tmp/comments.json"
REJECT_COMMIT=
run_negative missing-marker

printf '%s\n' \
    'PASS: split-root current-primary deleted-cwd/missing-log immutable artifact reused (including skip-over of a newer nonselected receipt); wrong head, row digest, artifact hash, receipt commit, and missing marker all refused before label/comment/remote publish'
