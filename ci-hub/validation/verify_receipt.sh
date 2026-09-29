#!/usr/bin/env bash
# Verify that an evidence comment resolves to an immutable, counted receipt.
set -uo pipefail

receipt_repo=rrnewton/dev-hermit
receipt_branch=validation-receipts
repo=rrnewton/hermit
sha=
comments_file=
fixture_root=
current_base=
current_reverie_base=
repo_checkout=
reverie_checkout=
producer_repo_checkout=
producer_definition_mode=

resolve_producer_record_from_checkout() {
    local checkout=$1 target_sha=$2
    observe_producer_paths_from_checkout "$checkout" "$target_sha" '["validate.sh"]'
}

observe_primary_producer_record_from_checkout() {
    local checkout=$1 target_sha=$2
    if [[ $repo == rrnewton/reverie ]]; then
        observe_producer_paths_from_checkout "$checkout" "$target_sha" '["validate.sh"]'
        return $?
    fi
    # Hermit validation-producer files recorded in a receipt for provenance.
    # This list is not an authority: paths absent at the commit are skipped and
    # nothing compares the recorded map to a registered one, so Hermit main can
    # move without an edit here.
    observe_producer_paths_from_checkout "$checkout" "$target_sha" '[
        ".config/nextest.toml",
        ".github/workflows/ci-portable.yml",
        "ci/manifest-plan/src/bin/nextest-cpu-wrapper.rs",
        "ci/manifest-plan/src/ledger.rs",
        "ci/manifest-plan/src/lib.rs",
        "ci/manifest-plan/src/nextest_binaries.rs",
        "ci/manifest-plan/src/nextest_cpu.rs",
        "ci/manifest-plan/src/runner.rs",
        "ci/manifest-plan/src/service_result.rs",
        "ci/manifest-plan/src/timeouts.rs",
        "ci/nextest-binaries.rs",
        "ci/nextest-test-results.rs",
        "ci/nextest-timeout-config.rs",
        "ci/run-nextest-counted.sh",
        "hermit-cli/src/canonical_verdict.rs",
        "scripts/lib/safe_ci_scope.rs",
        "scripts/lib/validate_cell_results.rs",
        "scripts/lib/validate_classification.rs",
        "scripts/lib/validate_history.rs",
        "scripts/lib/validate_plan.rs",
        "scripts/lib/validate_receipt.rs",
        "scripts/lib/validate_runtime.rs",
        "scripts/validate.rs"
    ]'
}

observe_producer_paths_from_checkout() {
    local checkout=$1 target_sha=$2 paths=$3 checked_definition relative blob
    # Reverie's canonical policy qualifies the complete run separately. This
    # records its actual entrypoint at the exact commit, not a claim that one
    # path contains the whole validation implementation.
    # A path that is not a blob at the commit is skipped; a commit where no
    # listed path is a blob (or that the checkout cannot read) returns 1.
    checked_definition='{}'
    while IFS= read -r relative; do
        blob=$(git -C "$checkout" rev-parse "$target_sha:$relative" 2>/dev/null) || continue
        [[ $blob =~ ^[0-9a-f]{40}$ ]] || continue
        [[ $(git -C "$checkout" cat-file -t "$blob" 2>/dev/null) == blob ]] || continue
        checked_definition=$(jq -cn \
            --argjson definition "$checked_definition" \
            --arg relative "$relative" --arg blob "$blob" \
            '$definition + {($relative): $blob}') || return 2
    done < <(jq -r '.[]' <<<"$paths")
    [[ $checked_definition != '{}' ]] || return 1
    jq -cn --argjson definition "$checked_definition" \
        --arg resolved_from "$checkout" \
        '{definition: $definition, coverage_status: "legacy-selected-paths",
          paths: ($definition | keys), resolved_from: $resolved_from}'
}

resolve_producer_record_from_github() {
    local target_repo=$1 target_sha=$2 blob
    blob=$("${gh_cmd[@]}" api \
        "repos/$target_repo/contents/validate.sh?ref=$target_sha" \
        --jq 'select(.type == "file") | .sha' 2>/dev/null) || return 2
    [[ $blob =~ ^[0-9a-f]{40}$ ]] || return 1
    jq -cn --arg blob "$blob" --arg resolved_from "github:$target_repo@$target_sha" \
        '{definition: {"validate.sh": $blob}, coverage_status: "legacy-selected-paths",
          paths: ["validate.sh"], resolved_from: $resolved_from}'
}

usage() {
    cat >&2 <<'EOF'
Usage: verify-local-validation-receipt.sh --sha SHA --comments FILE [options]

Options:
  --repo OWNER/REPO       PR repository (default: rrnewton/hermit)
  --fixture-receipts DIR  Read DIR/<receipt-commit>/<receipt-path> instead of GitHub
  --current-base SHA      Fresh Hermit main tip at the final merge boundary
  --current-reverie-base SHA  Fresh Reverie main tip at that same boundary
  --repo-checkout DIR     Hermit object store containing --current-base
  --reverie-checkout DIR  Reverie object store containing --current-reverie-base
  --producer-repo-checkout DIR  Product object store containing exact --sha
  --producer-definition-observe  Print the producer file blobs at --sha in --repo-checkout
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sha) sha=${2:-}; shift 2 ;;
        --comments) comments_file=${2:-}; shift 2 ;;
        --repo) repo=${2:-}; shift 2 ;;
        --fixture-receipts) fixture_root=${2:-}; shift 2 ;;
        --current-base) current_base=${2:-}; shift 2 ;;
        --current-reverie-base) current_reverie_base=${2:-}; shift 2 ;;
        --repo-checkout) repo_checkout=${2:-}; shift 2 ;;
        --reverie-checkout) reverie_checkout=${2:-}; shift 2 ;;
        --producer-repo-checkout) producer_repo_checkout=${2:-}; shift 2 ;;
        --producer-definition-observe) producer_definition_mode=observe; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; usage; exit 2 ;;
    esac
done

if [[ -n $producer_definition_mode ]]; then
    if [[ -n $comments_file || -n $current_base || -n $current_reverie_base || \
          -n $reverie_checkout || -n $producer_repo_checkout ]]; then
        printf 'producer-definition helper mode cannot be combined with receipt verification\n' >&2
        exit 2
    fi
    if [[ $repo != rrnewton/hermit && $repo != rrnewton/reverie ]]; then
        printf 'unsupported producer repository: %s\n' "$repo" >&2
        exit 2
    fi
    if [[ ! $sha =~ ^[0-9a-f]{40}$ ]] || [[ -z $repo_checkout ]] || \
       [[ ! -d $repo_checkout ]]; then
        usage
        exit 2
    fi
    observe_primary_producer_record_from_checkout "$repo_checkout" "$sha"
    exit $?
fi

if [[ ! $sha =~ ^[0-9a-f]{40}$ ]] || [[ -z $comments_file || ! -r $comments_file ]]; then
    usage
    exit 2
fi

boundary_values=("$current_base" "$current_reverie_base" "$repo_checkout" "$reverie_checkout")
boundary_count=0
for value in "${boundary_values[@]}"; do [[ -n $value ]] && ((boundary_count+=1)); done
if ((boundary_count != 0 && boundary_count != 4)); then
    printf 'merge-boundary verification requires all four base/checkouts arguments\n' >&2
    exit 2
fi
if ((boundary_count == 4)) && \
   { [[ ! $current_base =~ ^[0-9a-f]{40}$ ]] || \
     [[ ! $current_reverie_base =~ ^[0-9a-f]{40}$ ]] || \
     [[ ! -d $repo_checkout ]] || [[ ! -d $reverie_checkout ]]; }; then
    printf 'merge-boundary base/checkouts are malformed or unavailable\n' >&2
    exit 2
fi
if [[ -n $producer_repo_checkout && ! -d $producer_repo_checkout ]]; then
    printf 'producer repository checkout is unavailable: %s\n' "$producer_repo_checkout" >&2
    exit 2
fi
boundary_digest_args=()
if ((boundary_count == 4)); then
    boundary_digest_args=(
        --current-base "$current_base"
        --current-reverie-base "$current_reverie_base"
        --repo-checkout "$repo_checkout"
        --reverie-checkout "$reverie_checkout"
    )
fi

# The disclosure role enum has ONE definition: `ROLES` in `ci-hub/lib/disclosure_tag.py`.
# DERIVE it here; never restate it. A restatement drifts silently in the worst
# possible direction -- a role added to ROLES makes the emitter publish receipts
# this filter drops on their first line, and the refusal reads as "no receipt"
# rather than "unknown role". That is exactly the defect that broke the lander on
# 2026-08-11, and two copies cannot be held in sync by discipline because they land
# in different commits by different agents at different times.
#
# Parse the assignment rather than importing it: `ast` extraction has no import
# path, executes no module-level code, and cannot be perturbed by the module's
# other contents.
#
# FAIL CLOSED. An unreadable module, a missing or empty ROLES, or a token that is
# not a safe lowercase identifier is a DEPLOY DEFECT -> exit 2. Never widen to a
# permissive pattern on failure: accepting any role string would forge the
# authority this filter exists to bind, which is worse than refusing to land.
disclosure_roles_module=${DISCLOSURE_ROLES_MODULE:-}
if [[ -z $disclosure_roles_module ]]; then
    script_dir=${script_dir:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
    disclosure_roles_module="$script_dir/../lib/disclosure_tag.py"
fi
role_alternation=$(DISCLOSURE_ROLES_MODULE_PATH="$disclosure_roles_module" python3 - <<'PY'
import ast
import os
import re
import sys

path = os.environ["DISCLOSURE_ROLES_MODULE_PATH"]
try:
    with open(path, encoding="utf-8") as handle:
        tree = ast.parse(handle.read())
except (OSError, SyntaxError) as error:
    sys.stderr.write(f"cannot read disclosure role definition {path}: {error}\n")
    raise SystemExit(2)

found = None
for node in ast.walk(tree):
    if not isinstance(node, ast.Assign):
        continue
    if not any(isinstance(t, ast.Name) and t.id == "ROLES" for t in node.targets):
        continue
    value = node.value
    # Accept `ROLES = frozenset({...})` and a bare set/tuple/list literal.
    if (
        isinstance(value, ast.Call)
        and isinstance(value.func, ast.Name)
        and value.func.id in {"frozenset", "set"}
        and len(value.args) == 1
    ):
        value = value.args[0]
    try:
        found = ast.literal_eval(value)
    except ValueError:
        found = None
    break

if not found:
    sys.stderr.write(f"no usable ROLES definition in {path}\n")
    raise SystemExit(2)

token = re.compile(r"^[a-z][a-z0-9_-]*$")
roles = sorted(found)
for role in roles:
    if not isinstance(role, str) or not token.fullmatch(role):
        sys.stderr.write(f"unsafe role token in {path}: {role!r}\n")
        raise SystemExit(2)
print("|".join(roles))
PY
) || {
    printf 'disclosure role enum is underivable from %s\n' "$disclosure_roles_module" >&2
    exit 2
}
if [[ -z $role_alternation ]]; then
    printf 'disclosure role enum resolved empty from %s\n' "$disclosure_roles_module" >&2
    exit 2
fi

owner=${repo%%/*}
mapfile -t candidates < <(jq -r --arg owner "$owner" --arg role_alternation "$role_alternation" '
    # Historical ci-hub comments used a service actor or the former four-field
    # disclosure. Keep those exact artifacts readable. New comments use the
    # five-field prefix as the complete first line; the body starts on line two.
    # The model is a nonempty resolved token, self-describing `unknown-model`,
    # or the historical literal `unresolved`, and role is
    # the closed enum DERIVED from ROLES above and passed in as $role_alternation,
    # so adding a role there needs no edit here.
    def four_field_prefix: "^\\[[^,\\]]+, [^,\\]]+, [^,\\]]+, role=[^\\]]+\\]";
    def five_field_prefix:
      "^\\[[^,\\]]+, [^,\\]]+, [^,\\]]+, [^,\\]]+, role=(" + $role_alternation + ")\\]$";
    def legacy_role_tag:
      . == "[impl agent, ci-hub]"
      or . == "[Human]"
      or (
        (capture("^\\[(?<role>impl agent|adversarial-reviewer agent|coordinator), (?<model>[^]]+)\\]$")? // {})
        | ((.model // "") | test("\\S"))
      );
    def accepted_role_tag:
      legacy_role_tag
      or test(five_field_prefix)
      or (
        test(four_field_prefix)
        and (
          (sub(four_field_prefix; "") | sub("^ +"; "")) as $rest
          | ($rest == "" or ($rest | legacy_role_tag))
        )
      );
    [ .[][]?
      | select(.user.login == $owner)
      | (.body // "") as $body
      | ($body | split("\n")[0]) as $role_tag
      | select($role_tag | accepted_role_tag)
      | $body
      | split("\n")[]
      | capture("^<!-- locally-validated-receipt commit=(?<commit>[0-9a-f]{40}) path=(?<path>[^ ]+) sha256=(?<digest>[0-9a-f]{64}) -->$")?
      | . + {role_class: (if $role_tag == "[impl agent, ci-hub]" then "legacy-service" else "current" end)}
    ] | reverse[] | [.commit, .path, .digest, .role_class] | @tsv
' "$comments_file" 2>/dev/null)

gh_cmd=(gh)
if command -v with-proxy >/dev/null 2>&1; then
    gh_cmd=(with-proxy gh)
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# A Hermit receipt's producer map is recorded provenance, not compared to any
# registered map (https://github.com/rrnewton/dev-hermit/issues/414). Reverie
# records its actual validate.sh blob with selected-path coverage, and the
# receipt must still match that blob at the exact commit.
expected_producer_args=()
if [[ $repo == rrnewton/reverie ]]; then
    target_producer_status=0
    if [[ -n $producer_repo_checkout ]]; then
        target_producer_record=$(resolve_producer_record_from_checkout \
            "$producer_repo_checkout" "$sha") || target_producer_status=$?
    else
        target_producer_record=$(resolve_producer_record_from_github \
            "$repo" "$sha") || target_producer_status=$?
    fi
    if ((target_producer_status != 0)); then
        if ((target_producer_status == 1)); then
            printf 'target commit has no resolvable producer definition: %s@%s\n' \
                "$repo" "$sha" >&2
            exit 1
        fi
        printf 'cannot resolve target commit producer definition: %s@%s\n' \
            "$repo" "$sha" >&2
        exit 2
    fi

    expected_producer_file=$(mktemp)
    cleanup_expected_producer() {
        local status=$?
        rm -f -- "$expected_producer_file"
        exit "$status"
    }
    trap cleanup_expected_producer EXIT
    if ! jq -c '. + {resolved_from: "immutable-receipt-verifier"}' \
        <<<"$target_producer_record" >"$expected_producer_file"; then
        printf 'cannot encode target producer definition for typed receipt verification\n' >&2
        exit 2
    fi
    expected_producer_args=(--expected-producer-record "$expected_producer_file")
elif [[ $repo != rrnewton/hermit ]]; then
    printf 'unsupported producer repository: %s\n' "$repo" >&2
    exit 2
fi

last_typed_receipt_error=
for candidate in "${candidates[@]}"; do
    IFS=$'\t' read -r receipt_commit receipt_path receipt_digest role_class <<<"$candidate"
    expected_prefix="validation-receipts/${repo}/${sha}/"
    if [[ $receipt_path != "$expected_prefix"*.json ]] || \
       [[ ${receipt_path##*/} != "${receipt_digest}.json" ]]; then
        continue
    fi

    receipt_file=$(mktemp)
    if [[ -n $fixture_root ]]; then
        source_file="$fixture_root/$receipt_commit/$receipt_path"
        if [[ ! -f $source_file ]] || ! cp -- "$source_file" "$receipt_file"; then
            rm -f -- "$receipt_file"
            continue
        fi
    else
        comparison=$("${gh_cmd[@]}" api \
            "repos/${receipt_repo}/compare/${receipt_commit}...${receipt_branch}" \
            --jq .status 2>/dev/null) || comparison=
        if [[ $comparison != ahead && $comparison != identical ]]; then
            rm -f -- "$receipt_file"
            continue
        fi
        if ! "${gh_cmd[@]}" api \
            "repos/${receipt_repo}/contents/${receipt_path}?ref=${receipt_commit}" \
            --jq .content 2>/dev/null | tr -d '\n' | base64 --decode >"$receipt_file"; then
            rm -f -- "$receipt_file"
            continue
        fi
    fi

    actual_digest=$(sha256sum "$receipt_file" | awk '{print $1}')
    if [[ $actual_digest != "$receipt_digest" ]]; then
        rm -f -- "$receipt_file"
        continue
    fi

    receipt_digest_args=(
        receipt-digest
        --sha "$sha"
        --published-receipt
        --repository "$repo"
        "${expected_producer_args[@]}"
        --json
        "${boundary_digest_args[@]}"
    )
    if [[ $role_class == legacy-service ]]; then
        receipt_digest_args+=(--require-qualifying --allow-legacy-missing-identity)
    else
        receipt_digest_args+=(--require-canonical-qualifying)
    fi
    typed_receipt_error=$(mktemp)
    typed_receipt=$("$script_dir/../ci-hub" "${receipt_digest_args[@]}" \
        <"$receipt_file" 2>"$typed_receipt_error") || typed_receipt=
    if [[ -z $typed_receipt ]]; then
        last_typed_receipt_error=$(cat "$typed_receipt_error")
    fi
    rm -f -- "$typed_receipt_error"
    if [[ -n $typed_receipt ]]; then
        producer_status=$(jq -r '.producer_coverage_status // empty' <<<"$typed_receipt")
        producer_paths=$(jq -r '.producer_paths | join(",")' <<<"$typed_receipt" 2>/dev/null)
        producer_valid_commits=$(jq -r '
            if .producer_valid_commits == null then "unbounded"
            else (.producer_valid_commits | join(",")) end
          ' <<<"$typed_receipt" 2>/dev/null)
        if [[ -n $producer_status && -n $producer_paths && -n $producer_valid_commits ]]; then
            printf 'receipt_commit=%s receipt_path=%s receipt_sha256=%s producer_coverage_status=%s producer_paths=%s producer_valid_commits=%s\n' \
                "$receipt_commit" "$receipt_path" "$receipt_digest" \
                "$producer_status" "$producer_paths" "$producer_valid_commits"
            rm -f -- "$receipt_file"
            exit 0
        fi
    fi
    rm -f -- "$receipt_file"
done

if [[ -n $last_typed_receipt_error ]]; then
    printf 'typed published receipt refusal: %s\n' "$last_typed_receipt_error" >&2
fi
printf 'no immutable counted local-validation receipt for exact head %s\n' "$sha" >&2
exit 1
