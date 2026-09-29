#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/../.." && pwd)
verifier=$script_dir/verify_receipt.sh
publisher=$script_dir/publish_receipt.py
receipt_digest=$script_dir/../ci-hub
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

receipt_commit=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
mkdir -p "$tmp/receipts/$receipt_commit"

# --- PRODUCER DEFINITION PROVENANCE -----------------------------------------
# Every receipt names the check definition that produced it. A Hermit receipt's
# map is provenance only: no registered map is compared against it
# (https://github.com/rrnewton/dev-hermit/issues/414). ONE fixture producer
# checkout serves the whole bracket; the end-to-end leg mints through the real
# publisher, which derives the definition from `git rev-parse <sha>:<path>`.
producer_repo=$tmp/producer-checkout
mkdir -p "$producer_repo/.github/workflows"
printf '#!/usr/bin/env bash\necho validate\n' >"$producer_repo/validate.sh"
printf 'name: CI\n' >"$producer_repo/.github/workflows/ci-portable.yml"
git -C "$producer_repo" init -q
git -C "$producer_repo" -c user.name=t -c user.email=t@e add -A
git -C "$producer_repo" -c user.name=t -c user.email=t@e commit -qm 'producer fixture'
producer_sha=$(git -C "$producer_repo" rev-parse HEAD)
sha=$producer_sha
REG_VALIDATE=$(git -C "$producer_repo" rev-parse HEAD:validate.sh)
REG_PORTABLE=$(git -C "$producer_repo" rev-parse HEAD:.github/workflows/ci-portable.yml)

make_receipt() {
    local executed=$1 output=$2
    local raw=$tmp/receipt-build.json selected
    jq -cnS --arg sha "$sha" --argjson executed "$executed" \
            --arg reg_validate "$REG_VALIDATE" --arg reg_portable "$REG_PORTABLE" '{
      schema_version: 1,
      repository: "rrnewton/hermit",
      commit: $sha,
      run_id: ($sha + "@2026-08-04T12:00:00Z@fixture-host"),
      source_log_file: "/tmp/validate.log",
      durable_log_file: "/durable/validate.log",
      log_sha256: ("c" * 64),
      producer: {
        resolved_from: "/fixture/worktree",
        coverage_status: "legacy-selected-paths",
        paths: [".github/workflows/ci-portable.yml", "validate.sh"],
        definition: {
          "validate.sh": $reg_validate,
          ".github/workflows/ci-portable.yml": $reg_portable
        }
      },
      selected_receipt_identity: {
        digest_algorithm: "sha256",
        canonicalization: "serde_json::to_vec(HistoryRow)-v1",
        digest: "pending"
      },
      ledger_record: {
        schema_version: 4,
        started_at: "2026-08-04T12:00:00Z",
        finished_at: "2026-08-04T12:01:00Z",
        host: "fixture-host",
        slot: "fixture-slot",
        repo: "hermit",
        commit: $sha,
        tree: ("e" * 40),
        profile: "full",
        selection_mode: "full",
        commit_anchored: true,
        tree_dirty: false,
        result: "pass",
        raw_result: "pass",
        exit_code: 0,
        checks: 2,
        gates_run: 2,
        gates_expected: 2,
        gates: [
          {name: "fmt", result: "pass", exit_code: 0},
          {name: "test", result: "pass", exit_code: 0}
        ],
        failures: 0,
        executed_tests: $executed,
        passed_tests: $executed,
        filtered_tests: 0,
        log_file: "/tmp/validate.log"
      }
    }' >"$raw"
    selected=$(jq -c '.ledger_record' "$raw" | \
        "$receipt_digest" receipt-digest --sha "$sha")
    jq --arg selected "$selected" \
        '.selected_receipt_identity.digest = $selected' "$raw" >"$output"
}

refresh_selected_identity() {
    local input=$1 output=$2 selected
    selected=$(jq -c '.ledger_record' "$input" | \
        "$receipt_digest" receipt-digest --sha "$sha")
    jq --arg selected "$selected" \
        '.selected_receipt_identity.digest = $selected' "$input" >"$output"
}

retarget_receipt() {
    local input=$1 output=$2 target_sha=$3 selected
    jq --arg sha "$target_sha" '
      .commit = $sha
      | .ledger_record.commit = $sha
      | .run_id = ($sha + "@" + .ledger_record.started_at + "@" + .ledger_record.host)
    ' "$input" >"$tmp/retarget-raw.json"
    selected=$(jq -c '.ledger_record' "$tmp/retarget-raw.json" | \
        "$receipt_digest" receipt-digest --sha "$target_sha")
    jq --arg selected "$selected" \
        '.selected_receipt_identity.digest = $selected' \
        "$tmp/retarget-raw.json" >"$output"
}

write_comments() {
    local path=$1 digest=$2 role_tag=${3:-'[impl agent, ci-hub]'}
    jq -cn --arg commit "$receipt_commit" --arg path "$path" \
        --arg digest "$digest" --arg role_tag "$role_tag" '{
      user: {login: "rrnewton"},
      body: ($role_tag + "\n\n<!-- locally-validated-receipt commit=" + $commit + " path=" + $path + " sha256=" + $digest + " -->")
    } | [[.]]' >"$tmp/comments.json"
}

# --- mutation anchors -------------------------------------------------------
# Every mutated fixture must be shown to have ACTUALLY changed the receipt
# before its refusal is believed. A mutation expression that silently no-ops
# would otherwise be scored as "the guard refused it", i.e. this bracket would
# report robustness it never tested -- a proxy-binding defect in the bracket.
#
# The pre-merge harness compared with `cmp -s`. A byte compare is VACUOUS on
# this harness: make_receipt/refresh_selected_identity emit pretty-printed JSON
# while several mutations use `jq -cS`, so base and mutant differ in FORMATTING
# alone and a no-op expression would still look mutated. Measured: a `jq -cS .`
# no-op applied to a pretty base differs byte-wise but is identical once
# normalized. So the anchor compares CANONICAL CONTENT (`jq -S -c`), which is
# what "the mutation changed the receipt" actually means.
mutation_anchor_failures=0
mutation_anchors_total=20
assert_mutated() { # assert_mutated <base> <mutant> <label>
    local a b
    a=$(jq -S -c . "$1" 2>/dev/null) || a="<unparseable:$1>"
    b=$(jq -S -c . "$2" 2>/dev/null) || b="<unparseable:$2>"
    if [[ "$a" == "$b" ]]; then
        printf 'BAD  ANCHOR    mutation did not change the receipt: %s\n' "$3" >&2
        mutation_anchor_failures=$((mutation_anchor_failures + 1))
    fi
}

verify_file() {
    local file=$1 expected=$2 label=$3 role_tag=${4:-'[impl agent, ci-hub]'}
    local query_sha=${5:-$sha} producer_checkout=${6:-$producer_repo}
    local file_digest file_path status=0 expected_status=1
    case "$expected" in
        pass) expected_status=0 ;;
        fail) expected_status=1 ;;
        deploy-defect) expected_status=2 ;;
        *) printf 'FAIL: unknown expected verifier result: %s\n' "$expected" >&2; exit 1 ;;
    esac
    file_digest=$(sha256sum "$file" | awk '{print $1}')
    file_path="validation-receipts/rrnewton/hermit/$query_sha/$file_digest.json"
    mkdir -p "$tmp/receipts/$receipt_commit/$(dirname "$file_path")"
    cp "$file" "$tmp/receipts/$receipt_commit/$file_path"
    write_comments "$file_path" "$file_digest" "$role_tag"
    "$verifier" --sha "$query_sha" --comments "$tmp/comments.json" \
        --fixture-receipts "$tmp/receipts" \
        --producer-repo-checkout "$producer_checkout" >/dev/null 2>&1 || status=$?
    if [[ $status != "$expected_status" ]]; then
        printf 'FAIL: %s expected %s (rc=%s), verifier exit=%s\n' \
            "$label" "$expected" "$expected_status" "$status" >&2
        exit 1
    fi
}

# Missing but perfectly shaped: this is the negative #1578 omitted.
forged_digest=$(printf 'd%.0s' {1..64})
forged_path="validation-receipts/rrnewton/hermit/$sha/$forged_digest.json"
write_comments "$forged_path" "$forged_digest"
if "$verifier" --sha "$sha" --comments "$tmp/comments.json" \
    --fixture-receipts "$tmp/receipts" \
    --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
    echo "FAIL: well-shaped nonexistent receipt was accepted" >&2
    exit 1
fi

# One legitimate counted receipt is admitted.
make_receipt 12 "$tmp/receipt.json"
digest=$(sha256sum "$tmp/receipt.json" | awk '{print $1}')
path="validation-receipts/rrnewton/hermit/$sha/$digest.json"
mkdir -p "$tmp/receipts/$receipt_commit/$(dirname "$path")"
cp "$tmp/receipt.json" "$tmp/receipts/$receipt_commit/$path"
write_comments "$path" "$digest"
base_evidence=$("$verifier" --sha "$sha" --comments "$tmp/comments.json" \
    --fixture-receipts "$tmp/receipts" \
    --producer-repo-checkout "$producer_repo")
[[ $base_evidence == *"producer_coverage_status=legacy-selected-paths"* ]] || \
    { echo "FAIL: primary verdict omitted legacy coverage condition" >&2; exit 1; }
[[ $base_evidence == *"producer_paths=.github/workflows/ci-portable.yml,validate.sh"* ]] || \
    { echo "FAIL: primary verdict omitted exact two-path condition" >&2; exit 1; }
[[ $base_evidence == *"producer_valid_commits=unbounded"* ]] || \
    { echo "FAIL: primary verdict invented a registered commit bound" >&2; exit 1; }

# The registration gate is gone: a receipt whose producer map matches no
# registered map is still accepted, because the map is provenance only.
jq -cS '.producer.definition["validate.sh"] = ("0" * 40)' \
    "$tmp/receipt.json" >"$tmp/producer-unregistered.json"
assert_mutated "$tmp/receipt.json" "$tmp/producer-unregistered.json" \
    "PRODUCER unregistered map (gate removed)"
verify_file "$tmp/producer-unregistered.json" pass \
    "unregistered producer map is accepted (registration gate removed)"

# The wrapper's digest-addressed path is not permission to forge the selected
# row identity. Change only that inner digest; verify_file recomputes the outer
# artifact digest/path, and the final verifier must still refuse it.
jq '.selected_receipt_identity.digest = ("f" * 64)' \
    "$tmp/receipt.json" >"$tmp/tampered-selected-digest.json"
assert_mutated "$tmp/receipt.json" "$tmp/tampered-selected-digest.json" "IDENTITY tampered selected digest"
verify_file "$tmp/tampered-selected-digest.json" fail \
    "tampered selected digest with recomputed outer artifact identity" \
    '[coordinator, gpt-5.6-sol]'

# The receipt envelope is versioned independently of the embedded HistoryRow.
# Missing schema fails by that field name in the shared Rust parser; a newer
# schema is identified before the schema-1 body is interpreted.
jq 'del(.schema_version)' "$tmp/receipt.json" >"$tmp/missing-receipt-schema.json"
assert_mutated "$tmp/receipt.json" "$tmp/missing-receipt-schema.json" \
    "RECEIPT missing schema_version"
verify_file "$tmp/missing-receipt-schema.json" fail \
    "published receipt without schema_version" '[coordinator, gpt-5.6-sol]'

jq '.source_log_file = "/tmp/another-run.log"' \
    "$tmp/receipt.json" >"$tmp/wrong-source-log.json"
assert_mutated "$tmp/receipt.json" "$tmp/wrong-source-log.json" \
    "RECEIPT source_log_file differs from ledger row"
verify_file "$tmp/wrong-source-log.json" fail \
    "published receipt with mismatched source log" '[coordinator, gpt-5.6-sol]'

# Only the explicitly documented historical service-actor tag may consume an
# older artifact that predates the canonical selected-row identity.
jq '
  del(.selected_receipt_identity)
  | .ledger_record.schema_version = 1
  | del(
      .ledger_record.slot,
      .ledger_record.repo,
      .ledger_record.tree,
      .ledger_record.raw_result,
      .ledger_record.exit_code,
      .ledger_record.gates_run,
      .ledger_record.gates_expected,
      .ledger_record.gates
    )
' "$tmp/receipt.json" >"$tmp/historical.json"
assert_mutated "$tmp/receipt.json" "$tmp/historical.json" "IDENTITY legacy artifact without selected identity"
verify_file "$tmp/historical.json" pass "legacy service artifact without selected identity"
verify_file "$tmp/historical.json" fail "current role artifact without selected identity" \
    '[coordinator, gpt-5.6-sol]'
jq '.selected_receipt_identity = false' "$tmp/receipt.json" >"$tmp/malformed-identity.json"
assert_mutated "$tmp/receipt.json" "$tmp/malformed-identity.json" "IDENTITY malformed selected identity"
verify_file "$tmp/malformed-identity.json" fail "legacy artifact with malformed selected identity"
jq '
  .ledger_record.checks = 0
  | .ledger_record.gates_run = 0
  | .ledger_record.gates = []
' "$tmp/receipt.json" >"$tmp/current-weak-row-raw.json"
assert_mutated "$tmp/receipt.json" "$tmp/current-weak-row-raw.json" "ROW weak selected row"
refresh_selected_identity "$tmp/current-weak-row-raw.json" "$tmp/current-weak-row.json"
verify_file "$tmp/current-weak-row.json" fail "current role artifact with weak selected row" \
    '[coordinator, gpt-5.6-sol]'

# Preserve historical role and four-field artifacts, while accepting the final
# five-field prefix as the complete first line. New writes put the body on line
# two and admit only impl|debugger|reviewer|relay|coordinator|human.
valid_role_tags=(
    '[impl agent, ci-hub]'
    '[impl agent, gpt-5.6-sol]'
    '[adversarial-reviewer agent, gpt-5.6-sol]'
    '[coordinator, gpt-5.6-sol]'
    '[Human]'
    '[hermit2, codex-2, devbig014, role=impl]'
    '[hermit2, codex-2, devbig014, role=impl] [coordinator, gpt-5.6-sol]'
    '[hermit2, codex-2, gpt-5.6-sol, devbig014, role=impl]'
    '[hermit2, codex-2, gpt-5.6-sol, devbig014, role=debugger]'
    '[hermit2, codex-2, unknown-model, devbig014, role=relay]'
    '[hermit2, codex-2, unresolved, devbig014, role=relay]'
    '[hermit2, orc-coord-014, claude-opus-5, devbig014, role=coordinator]'
)
for role_tag in "${valid_role_tags[@]}"; do
    write_comments "$path" "$digest" "$role_tag"
    if ! "$verifier" --sha "$sha" --comments "$tmp/comments.json" \
        --fixture-receipts "$tmp/receipts" \
        --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
        printf 'FAIL: valid receipt role tag was refused: %s\n' "$role_tag" >&2
        exit 1
    fi
done

invalid_role_tags=(
    '[assistant, gpt-5.6-sol]'
    '[coordinator, ]'
    '[Human, gpt-5.6-sol]'
    'prefix [coordinator, gpt-5.6-sol]'
    '[hermit2, codex-2, devbig014, role=]'
    '[hermit2, codex-2, role=impl]'
    '[hermit2, codex-2, devbig014, role=impl] [assistant, gpt-5.6-sol]'
    '[hermit2, codex-2, gpt-5.6-sol, devbig014, role=auditor]'
    '[hermit2, codex-2, gpt-5.6-sol, devbig014, role=impl] [coordinator, gpt-5.6-sol]'
)
for role_tag in "${invalid_role_tags[@]}"; do
    write_comments "$path" "$digest" "$role_tag"
    if "$verifier" --sha "$sha" --comments "$tmp/comments.json" \
        --fixture-receipts "$tmp/receipts" \
        --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
        printf 'FAIL: malformed receipt role tag was accepted: %s\n' "$role_tag" >&2
        exit 1
    fi
done

# The role enum is DERIVED from `ROLES`, not restated here. Brackets in both
# directions, because the whole point is that this file need not change when the
# enum does:
#   POSITIVE  adding a role to ROLES makes a receipt carrying it land, with the
#             verifier untouched. `auditor` is deliberately reused from the
#             malformed list above, so the SAME tag that was just refused is now
#             accepted purely because the definition moved. That is the property;
#             a test that only checked the six current roles would pass against a
#             hardcoded restatement and prove nothing.
#   NEGATIVE  an underivable enum REFUSES. Failing open here would forge the
#             authority this filter binds, so it must exit nonzero rather than
#             fall back to a permissive pattern.
derived_role_tag='[hermit2, codex-2, claude-opus-5, devbig014, role=auditor]'
write_comments "$path" "$digest" "$derived_role_tag"

extended_roles=$tmp/extended_roles.py
printf 'ROLES = frozenset({"impl", "debugger", "reviewer", "relay", "coordinator", "human", "auditor"})\n' \
    >"$extended_roles"
if ! DISCLOSURE_ROLES_MODULE="$extended_roles" "$verifier" --sha "$sha" \
    --comments "$tmp/comments.json" --fixture-receipts "$tmp/receipts" \
    --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
    printf 'FAIL: role added to ROLES was refused; the enum is not derived\n' >&2
    exit 1
fi

for broken_roles in "$tmp/absent_roles.py" "$tmp/empty_roles.py" "$tmp/unsafe_roles.py"; do
    case $broken_roles in
        */empty_roles.py) printf 'ROLES = frozenset()\n' >"$broken_roles" ;;
        */unsafe_roles.py) printf 'ROLES = frozenset({"impl", "role=x|.*"})\n' >"$broken_roles" ;;
        *) rm -f -- "$broken_roles" ;;
    esac
    if DISCLOSURE_ROLES_MODULE="$broken_roles" "$verifier" --sha "$sha" \
        --comments "$tmp/comments.json" --fixture-receipts "$tmp/receipts" \
        --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
        printf 'FAIL: underivable role enum was accepted: %s\n' "$broken_roles" >&2
        exit 1
    fi
done

write_comments "$path" "$digest" '[coordinator, gpt-5.6-sol]'

# The same legitimate receipt must not authorize a different (rebased) head.
stale_sha=ffffffffffffffffffffffffffffffffffffffff
if "$verifier" --sha "$stale_sha" --comments "$tmp/comments.json" \
    --fixture-receipts "$tmp/receipts" \
    --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
    echo "FAIL: receipt for the prior head authorized a rebased head" >&2
    exit 1
fi

# A tampered body and a real zero-executed receipt are both refused.
printf '\n' >>"$tmp/receipts/$receipt_commit/$path"
if "$verifier" --sha "$sha" --comments "$tmp/comments.json" \
    --fixture-receipts "$tmp/receipts" \
    --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
    echo "FAIL: tampered receipt was accepted" >&2
    exit 1
fi
make_receipt 0 "$tmp/zero.json"
zero_digest=$(sha256sum "$tmp/zero.json" | awk '{print $1}')
zero_path="validation-receipts/rrnewton/hermit/$sha/$zero_digest.json"
mkdir -p "$tmp/receipts/$receipt_commit/$(dirname "$zero_path")"
cp "$tmp/zero.json" "$tmp/receipts/$receipt_commit/$zero_path"
write_comments "$zero_path" "$zero_digest"
if "$verifier" --sha "$sha" --comments "$tmp/comments.json" \
    --fixture-receipts "$tmp/receipts" \
    --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
    echo "FAIL: zero-executed receipt was accepted" >&2
    exit 1
fi

# Host provenance binds the wrapper identity for legacy and current roles.
make_receipt 12 "$tmp/host-good.json"
jq -cS '.run_id = (.commit + "@" + .ledger_record.started_at + "@other-host")' \
    "$tmp/host-good.json" >"$tmp/host-mismatch.json"
assert_mutated "$tmp/host-good.json" "$tmp/host-mismatch.json" "HOST run_id host mismatch"
verify_file "$tmp/host-mismatch.json" fail "run_id host disagrees with ledger host"
jq -cS 'del(.ledger_record.host)' "$tmp/host-good.json" >"$tmp/host-absent-raw.json"
assert_mutated "$tmp/host-good.json" "$tmp/host-absent-raw.json" "HOST ledger host absent"
refresh_selected_identity "$tmp/host-absent-raw.json" "$tmp/host-absent.json"
verify_file "$tmp/host-absent.json" fail "ledger host absent"

# End-to-end current producer contract: one strong verifier-selected row is
# passed as exact bytes with its canonical digest, the mechanical publisher
# emits an artifact-SHA-addressed body, and a current role-tagged marker
# dereferences through the landing verifier.
# Reuse the single fixture producer checkout created at the top, so the minted
# definition is read from real blobs at a real commit.
sha=$producer_sha
strong_log=$tmp/strong-validate.log
printf 'running 12 tests\ntest result: ok. 12 passed; 0 failed\n' >"$strong_log"
jq -cn --arg sha "$sha" --arg log "$strong_log" --arg cwd "$producer_repo" '{
  schema_version: 4,
  started_at: "2026-08-04T13:00:00Z",
  finished_at: "2026-08-04T13:02:00Z",
  host: "fixture-host",
  slot: "fixture-slot",
  profile: "full",
  selection_mode: "full",
  commit: $sha,
  tree: ("f" * 40),
  commit_anchored: true,
  tree_dirty: false,
  result: "pass",
  raw_result: "pass",
  exit_code: 0,
  executed_tests: 12,
  passed_tests: 12,
  filtered_tests: 3,
  checks: 2,
  gates_run: 2,
  gates_expected: 2,
  failures: 0,
  log_file: $log,
  cwd: $cwd,
  gates: [
    {name: "fmt", result: "pass", exit_code: 0},
    {name: "test", result: "pass", exit_code: 0}
  ]
}' | tr -d '\n' >"$tmp/strong-row.json"
"$receipt_digest" receipt-digest --sha "$sha" --canonical-row \
    <"$tmp/strong-row.json" >"$tmp/strong-canonical-row.json"
selected_digest=$(sha256sum "$tmp/strong-canonical-row.json" | awk '{print $1}')
strong_report=$(python3 "$publisher" \
    --repo rrnewton/hermit \
    --sha "$sha" \
    --selected-receipt-sha256 "$selected_digest" \
    --canonicalization 'serde_json::to_vec(HistoryRow)-v1' \
    --dry-run <"$tmp/strong-canonical-row.json")
artifact_digest=$(jq -r '.artifact_sha256' <<<"$strong_report")
artifact_path=$(jq -r '.path' <<<"$strong_report")
jq -jr '.artifact_body' <<<"$strong_report" >"$tmp/strong-artifact.json"
if [[ $(sha256sum "$tmp/strong-artifact.json" | awk '{print $1}') != "$artifact_digest" ]] || \
   [[ $artifact_path != "validation-receipts/rrnewton/hermit/$sha/$artifact_digest.json" ]]; then
    echo "FAIL: publisher did not bind exact artifact bytes to its digest-addressed path" >&2
    exit 1
fi
if ! jq -e --arg selected "$selected_digest" '
    .selected_receipt_identity.digest_algorithm == "sha256"
    and .selected_receipt_identity.canonicalization == "serde_json::to_vec(HistoryRow)-v1"
    and .selected_receipt_identity.digest == $selected
    and .ledger_record.checks == 2
    and .ledger_record.gates_run == 2
    and (.ledger_record.gates | length) == 2
' "$tmp/strong-artifact.json" >/dev/null; then
    echo "FAIL: artifact lost the verifier-selected strong row identity" >&2
    exit 1
fi
mkdir -p "$tmp/receipts/$receipt_commit/$(dirname "$artifact_path")"
cp "$tmp/strong-artifact.json" "$tmp/receipts/$receipt_commit/$artifact_path"
write_comments "$artifact_path" "$artifact_digest" '[coordinator, gpt-5.6-sol]'
if ! "$verifier" --sha "$sha" --comments "$tmp/comments.json" \
    --fixture-receipts "$tmp/receipts" \
    --producer-repo-checkout "$producer_repo" >/dev/null 2>&1; then
    echo "FAIL: strong-row -> artifact-digest -> marker chain was refused" >&2
    exit 1
fi

# Current landing evidence must carry the framework-owned pass count. Refresh
# the selected-row identity and outer artifact digest so refusal is attributable
# only to the missing field, not to stale identity bytes.
jq 'del(.ledger_record.passed_tests)' \
    "$tmp/strong-artifact.json" >"$tmp/strong-no-passed-raw.json"
assert_mutated "$tmp/strong-artifact.json" "$tmp/strong-no-passed-raw.json" \
    "COUNT current receipt omitted passed_tests"
refresh_selected_identity "$tmp/strong-no-passed-raw.json" \
    "$tmp/strong-no-passed.json"
verify_file "$tmp/strong-no-passed.json" fail \
    "current receipt omitted passed_tests" '[coordinator, gpt-5.6-sol]'

# A typed empty-manifest omission accounts for one planned node without adding
# an executed gate or a PASS. Unknown reasons and dependency skips remain red.
jq '
  .ledger_record.gates_expected = 3
  | .ledger_record.skipped_nodes = 1
  | .ledger_record.intentional_skipped_nodes = [{
      name: "privileged-e2e.manifest_applications",
      reason: "empty-manifest-bucket"
    }]
  | .ledger_record.dependency_skipped_nodes = []
  | .ledger_record.unaccounted_nodes = []
' "$tmp/strong-artifact.json" >"$tmp/typed-skip-raw.json"
refresh_selected_identity "$tmp/typed-skip-raw.json" "$tmp/typed-skip.json"
assert_mutated "$tmp/strong-artifact.json" "$tmp/typed-skip.json" \
    "typed intentional skip accounting"
verify_file "$tmp/typed-skip.json" pass \
    "typed empty-manifest skip accounts without becoming PASS" \
    '[coordinator, gpt-5.6-sol]'

jq '.ledger_record.intentional_skipped_nodes[0].reason = "looks-empty"' \
    "$tmp/typed-skip.json" >"$tmp/typed-skip-unknown-raw.json"
refresh_selected_identity "$tmp/typed-skip-unknown-raw.json" \
    "$tmp/typed-skip-unknown.json"
assert_mutated "$tmp/typed-skip.json" "$tmp/typed-skip-unknown.json" \
    "unknown intentional skip reason"
verify_file "$tmp/typed-skip-unknown.json" fail \
    "unknown intentional skip reason refused" '[coordinator, gpt-5.6-sol]'

jq '.ledger_record.dependency_skipped_nodes = ["downstream.node"]' \
    "$tmp/typed-skip.json" >"$tmp/typed-skip-dependent-raw.json"
refresh_selected_identity "$tmp/typed-skip-dependent-raw.json" \
    "$tmp/typed-skip-dependent.json"
assert_mutated "$tmp/typed-skip.json" "$tmp/typed-skip-dependent.json" \
    "dependency skip cannot hide behind intentional skip"
verify_file "$tmp/typed-skip-dependent.json" fail \
    "dependency skip remains non-qualifying" '[coordinator, gpt-5.6-sol]'

# Count-capable receipts additionally bind the per-node coverage obligation.
# Use a second exact head so the two positive controls represent two distinct
# legitimate landing authorizations rather than repeated parsing of one row.
# Make that second head a real same-map commit.
git -C "$producer_repo" -c user.name=t -c user.email=t@e commit \
    --allow-empty -qm 'schema5 producer fixture'
sha=$(git -C "$producer_repo" rev-parse HEAD)
make_receipt 12 "$tmp/schema5-base.json"
jq '.ledger_record.schema_version = 5
    | .ledger_record.producer = "hermit-validate-sh"
    | .ledger_record.admission = "ci-hub-validate-lock"
    | .ledger_record.concurrent_validates = 0
    | .ledger_record.concurrency_proof = "validate_lock_owner_ancestry"
    | .ledger_record.base_sha = ("1" * 40)
    | .ledger_record.base_tree = ("2" * 40)
    | .ledger_record.reverie_base_sha = ("3" * 40)
    | .ledger_record.reverie_base_tree = ("4" * 40)' \
    "$tmp/schema5-base.json" >"$tmp/schema5-missing-raw.json"
assert_mutated "$tmp/schema5-base.json" "$tmp/schema5-missing-raw.json" "COVERAGE schema5 missing coverage block"
refresh_selected_identity "$tmp/schema5-missing-raw.json" "$tmp/schema5-missing.json"
verify_file "$tmp/schema5-missing.json" fail "schema5 missing coverage"
jq '.ledger_record.coverage = {
      planned_test_nodes: 0, executed_test_nodes: 0,
      zero_executed_nodes: [], absent_nodes: []
    }' "$tmp/schema5-missing.json" >"$tmp/schema5-zero-planned-raw.json"
assert_mutated "$tmp/schema5-missing.json" "$tmp/schema5-zero-planned-raw.json" "COVERAGE schema5 zero planned nodes"
refresh_selected_identity "$tmp/schema5-zero-planned-raw.json" "$tmp/schema5-zero-planned.json"
verify_file "$tmp/schema5-zero-planned.json" fail "schema5 zero planned nodes"
jq '.ledger_record.coverage = {
      planned_test_nodes: 2, executed_test_nodes: 1,
      zero_executed_nodes: [], absent_nodes: ["test.missing"]
    }' "$tmp/schema5-missing.json" >"$tmp/schema5-absent-raw.json"
assert_mutated "$tmp/schema5-missing.json" "$tmp/schema5-absent-raw.json" "COVERAGE schema5 absent node"
refresh_selected_identity "$tmp/schema5-absent-raw.json" "$tmp/schema5-absent.json"
verify_file "$tmp/schema5-absent.json" fail "schema5 absent node"
jq '.ledger_record.coverage = {
      planned_test_nodes: 2, executed_test_nodes: 2,
      zero_executed_nodes: []
    }' "$tmp/schema5-missing.json" >"$tmp/schema5-no-absent-list-raw.json"
assert_mutated "$tmp/schema5-missing.json" \
    "$tmp/schema5-no-absent-list-raw.json" \
    "COVERAGE schema5 omitted absent_nodes list"
refresh_selected_identity "$tmp/schema5-no-absent-list-raw.json" \
    "$tmp/schema5-no-absent-list.json"
verify_file "$tmp/schema5-no-absent-list.json" fail \
    "schema5 omitted absent_nodes list"
jq '.ledger_record.coverage = {
      planned_test_nodes: 2, executed_test_nodes: 2,
      absent_nodes: []
    }' "$tmp/schema5-missing.json" >"$tmp/schema5-no-zero-list-raw.json"
assert_mutated "$tmp/schema5-missing.json" \
    "$tmp/schema5-no-zero-list-raw.json" \
    "COVERAGE schema5 omitted zero_executed_nodes list"
refresh_selected_identity "$tmp/schema5-no-zero-list-raw.json" \
    "$tmp/schema5-no-zero-list.json"
verify_file "$tmp/schema5-no-zero-list.json" fail \
    "schema5 omitted zero_executed_nodes list"
jq '.ledger_record.coverage = {
      planned_test_nodes: 2, executed_test_nodes: 2,
      zero_executed_nodes: [], absent_nodes: []
    }' "$tmp/schema5-missing.json" >"$tmp/schema5-valid-raw.json"
assert_mutated "$tmp/schema5-missing.json" "$tmp/schema5-valid-raw.json" "COVERAGE schema5 complete coverage (positive)"
refresh_selected_identity "$tmp/schema5-valid-raw.json" "$tmp/schema5-valid.json"
verify_file "$tmp/schema5-valid.json" pass "schema5 complete coverage"
jq 'del(.ledger_record.base_sha)' \
    "$tmp/schema5-valid.json" >"$tmp/schema5-no-base-raw.json"
assert_mutated "$tmp/schema5-valid.json" "$tmp/schema5-no-base-raw.json" \
    "BASE schema5 missing recorded base"
refresh_selected_identity "$tmp/schema5-no-base-raw.json" "$tmp/schema5-no-base.json"
verify_file "$tmp/schema5-no-base.json" fail "schema5 missing recorded base"

plant_root=$tmp
rm -rf -- "$plant_root"
if [[ -e $plant_root ]]; then
    echo "FAIL: receipt fixture plant was not deleted cleanly: $plant_root" >&2
    exit 1
fi
trap - EXIT

# Every mutant must be shown to have actually changed the receipt. A silently
# no-op mutation would otherwise be scored as "the guard refused it", i.e. the
# harness would report robustness it never tested.
printf 'MUTATION ANCHORS: %s\n' \
    "$([[ $mutation_anchor_failures -eq 0 ]] && echo "all $mutation_anchors_total mutants differed from their base" \
       || echo "$mutation_anchor_failures of $mutation_anchors_total MUTANT(S) DID NOT DIFFER -- results not believable")"
if [[ $mutation_anchor_failures -ne 0 ]]; then
    echo "FAIL: receipt-consumer bracket: mutation anchors" >&2
    exit 1
fi

echo "PASS: 2/2 legitimate exact-head landing receipts accepted; unregistered producer map 1/1 accepted (registration gate removed); 2/2 additional identity/compatibility receipts, ${#valid_role_tags[@]}/${#valid_role_tags[@]} valid and ${#invalid_role_tags[@]}/${#invalid_role_tags[@]} malformed role tags, derived-enum positive 1/1 and underivable-enum negatives 3/3, current-tagged identity omission, malformed legacy identity, tampered selected-row digest after outer rehash, current-tagged weak row, current receipt missing passed_tests, stale-head, forged, tampered, zero-executed, host-mismatch, host-absent, five incomplete schema5 coverage controls and 1/1 missing-base control refused; all ${mutation_anchors_total} mutation anchors changed canonical content; fixture plant deleted cleanly"
