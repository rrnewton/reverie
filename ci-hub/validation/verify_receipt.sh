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
producer_definition_input=
producer_definition_landed_replay=
producer_definition_expected_registry_sha256=

# This is the ONE semantic dereferencer for producer-definition authority.
# Receipt verification calls it in-process; the mechanical Python publisher
# calls the two narrow CLI modes below. No other consumer may parse transition
# shape, expiry, provenance, or whole-map membership independently.
load_producer_definitions() {
    local registry_shape transition_finalize transition_expires
    local canonical_finalize canonical_expires finalize_epoch expires_epoch now_epoch
    local transition_active=false transition_finalizable=false
    producer_registry=${PRODUCER_DEFINITION_REGISTRY:-}
    if [[ -z $producer_registry ]]; then
        script_dir=${script_dir:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
        producer_registry="$script_dir/../validate/producer-definition.json"
    fi
    if [[ ! -r $producer_registry ]]; then
        printf 'producer-definition registry unreadable: %s\n' "$producer_registry" >&2
        return 2
    fi
    p_registry_sha256=$(sha256sum "$producer_registry" 2>/dev/null | awk '{print $1}') || \
        p_registry_sha256=
    if [[ ! $p_registry_sha256 =~ ^[0-9a-f]{64}$ ]]; then
        printf 'cannot digest producer-definition registry: %s\n' "$producer_registry" >&2
        return 2
    fi
    # A malformed/empty registration or transition is a deploy defect -> 2.
    # Critical top-level fields and every transition field are exact-shaped;
    # documentation keys are the only extensible surface and start with `_`.
    registry_shape=$(jq -ce '
        def oid:
          type == "string" and test("^[0-9a-f]{40}$");
        def producer_path:
          type == "string"
          and test("^[A-Za-z0-9._/-]+$")
          and (startswith("/") | not)
          and (split("/") | all(. != "" and . != "." and . != ".."));
        def producer_map:
          type == "object"
          and length > 0
          and (to_entries | all((.key | producer_path) and (.value | oid)));
        def canonical_utc:
          type == "string"
          and test("^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]Z$");
        def oid_list:
          . as $items
          | type == "array"
          and length > 0
          and (unique | length) == length
          and (sort == $items)
          and all(oid);
        def evidence($definition; $status; $valid_commits):
          {definition: $definition, coverage_status: $status,
           paths: ($definition | keys | sort)}
          + if $valid_commits == null then {}
            else {valid_commits: $valid_commits}
            end;
        . as $registry
        | ([keys[] | select(startswith("_") | not)] | sort) as $critical
        | select(
            (["registered", "registered_at", "registered_coverage_status"] - $critical | length) == 0
            and ($critical - ["registered", "registered_at", "registered_coverage_status",
                              "registered_valid_commits", "legacy", "transition"] | length) == 0
          )
        | select(.registered_at | oid)
        | select(.registered_coverage_status == "legacy-selected-paths"
                 or .registered_coverage_status == "complete")
        | select(.registered | producer_map)
        | .registered as $primary
        | .registered_coverage_status as $primary_status
        | select(
            if $primary_status == "legacy-selected-paths" then
              (.registered_valid_commits | oid_list)
              and (.registered_valid_commits | index($registry.registered_at)) != null
            else
              (has("registered_valid_commits") | not)
            end
          )
        | (.registered_valid_commits // null) as $primary_valid_commits
        | select(
            if has("legacy") then
              (.legacy | type) == "array"
              and (.legacy | length) > 0
              and (.legacy | all(
                . as $legacy
                | (keys | sort)
                  == (["coverage_status", "definition", "id", "registered_at", "valid_commits"] | sort)
                and ($legacy.id | type == "string" and test("^[a-z0-9][a-z0-9-]*$"))
                and ($legacy.registered_at | oid)
                and ($legacy.coverage_status == "legacy-selected-paths"
                     or $legacy.coverage_status == "complete")
                and ($legacy.definition | producer_map)
                and ($legacy.valid_commits | oid_list)
                and ($legacy.valid_commits | index($legacy.registered_at)) != null
                and $legacy.definition != $primary
              ))
              and ([.legacy[].id] | unique | length) == (.legacy | length)
              and ([.legacy[].definition | tojson] | unique | length) == (.legacy | length)
              and ([.legacy[].valid_commits[]] | unique | length)
                    == ([.legacy[].valid_commits[]] | length)
              and ($primary_valid_commits == null
                   or (([.legacy[].valid_commits[]] + $primary_valid_commits | unique | length)
                       == ([.legacy[].valid_commits[]] + $primary_valid_commits | length)))
            else true end
          )
        | [(.legacy // [])[]
            | evidence(.definition; .coverage_status; .valid_commits)
              + {id: .id, registered_at: .registered_at}] as $legacy_records
        | if has("transition") then
            .transition as $transition
            | select(($transition | type) == "object")
            | select(
                ($transition | keys | sort)
                == (["added_paths", "candidate", "candidate_coverage_status", "expires_at", "finalize_after", "id", "provenance", "registered_at"] | sort)
              )
            | select($transition.registered_at | oid)
            | select(($transition.provenance | type) == "object")
            | select(
                ($transition.provenance | keys | sort)
                == (["head", "pull_request", "repository"] | sort)
              )
            | select($transition.provenance.repository == "rrnewton/hermit")
            | select(
                $transition.provenance.pull_request
                | type == "number" and . == floor and . > 0
              )
            | select(
                $transition.provenance.head == $transition.registered_at
                and ($transition.provenance.head | oid)
              )
            | select(
                $transition.id
                == (($transition.provenance.repository | gsub("/"; "-"))
                    + "-pr-" + ($transition.provenance.pull_request | tostring))
              )
            | select($transition.finalize_after | canonical_utc)
            | select($transition.expires_at | canonical_utc)
            | select($transition.candidate_coverage_status == "complete")
            | select(
                ($transition.added_paths | type) == "array"
                and ($transition.added_paths | unique | length)
                    == ($transition.added_paths | length)
                and ($transition.added_paths | all(producer_path))
              )
            | select($transition.candidate | producer_map)
            | select(
                ($transition.added_paths | all(. as $path | $primary | has($path) | not))
              )
            | select(
                ($transition.candidate | keys | sort)
                == (($primary | keys) + $transition.added_paths | unique | sort)
              )
            | select($transition.candidate != $primary)
            | select($legacy_records | all(.definition != $transition.candidate))
            | {
                registry: $registry,
                primary_record: evidence($primary; $primary_status; $primary_valid_commits),
                legacy_records: $legacy_records,
                transition: true,
                transition_record: {
                  id: $transition.id,
                  registered_at: $transition.registered_at,
                  provenance: $transition.provenance,
                  added_paths: $transition.added_paths,
                  finalize_after: $transition.finalize_after,
                  expires_at: $transition.expires_at,
                  candidate_record: evidence(
                    $transition.candidate;
                    $transition.candidate_coverage_status;
                    null
                  )
                }
              }
          else
            {
              registry: $registry,
              primary_record: evidence($primary; $primary_status; $primary_valid_commits),
              legacy_records: $legacy_records,
              transition: false
            }
          end
      ' "$producer_registry" 2>/dev/null) || registry_shape=
    if [[ -z $registry_shape ]]; then
        printf 'malformed producer-definition registry: %s\n' "$producer_registry" >&2
        return 2
    fi
    p_registry=$(jq -c '.registry' <<<"$registry_shape")
    p_primary_record=$(jq -c '.primary_record' <<<"$registry_shape")
    p_primary=$(jq -c '.primary_record.definition' <<<"$registry_shape")
    p_producer_records=$(jq -c '[.primary_record] + .legacy_records' <<<"$registry_shape")
    p_transition_record=null
    if [[ $(jq -r '.transition' <<<"$registry_shape") == true ]]; then
        transition_finalize=$(jq -r '.transition_record.finalize_after' <<<"$registry_shape")
        transition_expires=$(jq -r '.transition_record.expires_at' <<<"$registry_shape")
        canonical_finalize=$(date -u -d "$transition_finalize" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null) || canonical_finalize=
        canonical_expires=$(date -u -d "$transition_expires" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null) || canonical_expires=
        finalize_epoch=$(date -u -d "$transition_finalize" +%s 2>/dev/null) || finalize_epoch=
        expires_epoch=$(date -u -d "$transition_expires" +%s 2>/dev/null) || expires_epoch=
        now_epoch=$(date -u +%s 2>/dev/null) || now_epoch=
        if [[ $canonical_finalize != "$transition_finalize" ]] || \
           [[ $canonical_expires != "$transition_expires" ]] || \
           [[ ! $finalize_epoch =~ ^[0-9]+$ ]] || \
           [[ ! $expires_epoch =~ ^[0-9]+$ ]] || \
           [[ ! $now_epoch =~ ^[0-9]+$ ]] || \
           ((finalize_epoch >= expires_epoch)); then
            printf 'malformed producer-definition registry: %s\n' "$producer_registry" >&2
            return 2
        fi
        if ((now_epoch < expires_epoch)); then
            transition_active=true
            if ((now_epoch >= finalize_epoch)); then
                transition_finalizable=true
            fi
            p_producer_records=$(jq -c \
                --argjson candidate "$(jq -c '.transition_record.candidate_record' <<<"$registry_shape")" \
                '. + [$candidate]' \
                <<<"$p_producer_records")
        fi
        p_transition_record=$(jq -c \
            --argjson active "$transition_active" \
            --argjson finalizable "$transition_finalizable" \
            --arg registry_sha256 "$p_registry_sha256" \
            '.transition_record + {active: $active, finalizable: $finalizable,
                                   registry_sha256: $registry_sha256}' \
            <<<"$registry_shape")
    fi
}

resolve_producer_record_from_checkout() {
    local checkout=$1 target_sha=$2 producer_record expected_definition
    local checked_definition relative blob
    while IFS= read -r producer_record; do
        if ! jq -e --arg sha "$target_sha" '
            (has("valid_commits") | not)
            or (.valid_commits | index($sha) != null)
          ' <<<"$producer_record" >/dev/null; then
            continue
        fi
        expected_definition=$(jq -c '.definition' <<<"$producer_record") || return 2
        checked_definition='{}'
        while IFS= read -r relative; do
            blob=$(git -C "$checkout" rev-parse "$target_sha:$relative" 2>/dev/null) || {
                checked_definition=; break;
            }
            [[ $blob =~ ^[0-9a-f]{40}$ ]] || { checked_definition=; break; }
            checked_definition=$(jq -cn \
                --argjson definition "$checked_definition" \
                --arg relative "$relative" --arg blob "$blob" \
                '$definition + {($relative): $blob}') || return 2
        done < <(jq -r 'keys[]' <<<"$expected_definition")
        if [[ -n $checked_definition ]] && \
           jq -en --argjson actual "$checked_definition" \
               --argjson expected "$expected_definition" \
               '$actual == $expected' >/dev/null; then
            jq -cn --argjson record "$producer_record" \
                --arg resolved_from "$checkout" \
                '$record + {resolved_from: $resolved_from}'
            return 0
        fi
    done < <(jq -c '.[]' <<<"$p_producer_records")
    return 1
}

resolve_producer_record_from_github() {
    local target_repo=$1 target_sha=$2 producer_record expected_definition
    local checked_definition relative blob saw_error=0
    while IFS= read -r producer_record; do
        if ! jq -e --arg sha "$target_sha" '
            (has("valid_commits") | not)
            or (.valid_commits | index($sha) != null)
          ' <<<"$producer_record" >/dev/null; then
            continue
        fi
        expected_definition=$(jq -c '.definition' <<<"$producer_record") || return 2
        checked_definition='{}'
        while IFS= read -r relative; do
            blob=$("${gh_cmd[@]}" api \
                "repos/$target_repo/contents/$relative?ref=$target_sha" \
                --jq .sha 2>/dev/null) || {
                saw_error=1; checked_definition=; break;
            }
            [[ $blob =~ ^[0-9a-f]{40}$ ]] || {
                saw_error=1; checked_definition=; break;
            }
            checked_definition=$(jq -cn \
                --argjson definition "$checked_definition" \
                --arg relative "$relative" --arg blob "$blob" \
                '$definition + {($relative): $blob}') || return 2
        done < <(jq -r 'keys[]' <<<"$expected_definition")
        if [[ -n $checked_definition ]] && \
           jq -en --argjson actual "$checked_definition" \
               --argjson expected "$expected_definition" \
               '$actual == $expected' >/dev/null; then
            jq -cn --argjson record "$producer_record" \
                --arg resolved_from "github:$target_repo@$target_sha" \
                '$record + {resolved_from: $resolved_from}'
            return 0
        fi
    done < <(jq -c '.[]' <<<"$p_producer_records")
    ((saw_error == 0)) || return 2
    return 1
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
  --producer-repo-checkout DIR  Hermit object store containing exact --sha
  --producer-definition-primary  Print the validated primary map and exit
  --producer-definition-allowed  Print all currently allowed exact maps and exit
  --producer-definition-check FILE  Accept FILE only if it is one allowed whole map
  --producer-definition-resolve  Derive and check --sha from --repo-checkout
  --producer-definition-transition  Print the validated transition record and lifecycle
  --producer-definition-finalize  Print a finalized registry for --landed-replay
  --landed-replay SHA     Rebase-merge replay proven by the transition finalizer
  --expected-registry-sha256 HEX  CAS-bind finalization to inspected registry bytes
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
        --producer-definition-primary) producer_definition_mode=primary; shift ;;
        --producer-definition-allowed) producer_definition_mode=allowed; shift ;;
        --producer-definition-check)
            producer_definition_mode=check; producer_definition_input=${2:-}; shift 2 ;;
        --producer-definition-resolve) producer_definition_mode=resolve; shift ;;
        --producer-definition-transition) producer_definition_mode=transition; shift ;;
        --producer-definition-finalize) producer_definition_mode=finalize; shift ;;
        --landed-replay) producer_definition_landed_replay=${2:-}; shift 2 ;;
        --expected-registry-sha256)
            producer_definition_expected_registry_sha256=${2:-}; shift 2 ;;
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
    load_producer_definitions || exit $?
    if [[ $producer_definition_mode == primary ]]; then
        [[ -z $sha && -z $repo_checkout && -z $producer_definition_landed_replay &&
           -z $producer_definition_expected_registry_sha256 ]] || exit 2
        printf '%s\n' "$p_primary"
        exit 0
    fi
    if [[ $producer_definition_mode == allowed ]]; then
        [[ -z $sha && -z $repo_checkout && -z $producer_definition_landed_replay &&
           -z $producer_definition_expected_registry_sha256 ]] || exit 2
        printf '%s\n' "$p_producer_records"
        exit 0
    fi
    if [[ $producer_definition_mode == transition ]]; then
        [[ -z $sha && -z $repo_checkout && -z $producer_definition_landed_replay &&
           -z $producer_definition_expected_registry_sha256 ]] || exit 2
        [[ $p_transition_record != null ]] || exit 1
        printf '%s\n' "$p_transition_record"
        exit 0
    fi
    if [[ $producer_definition_mode == finalize ]]; then
        if [[ -n $sha || -n $repo_checkout ]] || \
           [[ ! $producer_definition_landed_replay =~ ^[0-9a-f]{40}$ ]] || \
           [[ ! $producer_definition_expected_registry_sha256 =~ ^[0-9a-f]{64}$ ]] || \
           [[ $p_transition_record == null ]] || \
           [[ $(jq -r '.active and .finalizable' <<<"$p_transition_record") != true ]]; then
            exit 2
        fi
        [[ $producer_definition_expected_registry_sha256 == "$p_registry_sha256" ]] || exit 1
        jq -S \
            --arg replay "$producer_definition_landed_replay" \
            --argjson primary "$p_primary_record" \
            --argjson transition "$p_transition_record" '
              . as $registry
              | (($registry.legacy // []) + [{
                  id: ("pre-" + $transition.id),
                  registered_at: $registry.registered_at,
                  coverage_status: $primary.coverage_status,
                  valid_commits: ($primary.valid_commits // [$registry.registered_at]),
                  definition: $primary.definition
                }]) as $legacy
              | $registry
              | .registered_at = $replay
              | .registered_coverage_status = $transition.candidate_record.coverage_status
              | .registered = $transition.candidate_record.definition
              | .legacy = $legacy
              | del(.registered_valid_commits, .transition)
            ' <<<"$p_registry"
        exit 0
    fi
    if [[ $producer_definition_mode == resolve ]]; then
        if [[ -n $producer_definition_landed_replay ]] || \
           [[ -n $producer_definition_expected_registry_sha256 ]] || \
           [[ ! $sha =~ ^[0-9a-f]{40}$ ]] || [[ -z $repo_checkout ]] || \
           [[ ! -d $repo_checkout ]]; then
            usage
            exit 2
        fi
        resolve_producer_record_from_checkout "$repo_checkout" "$sha"
        exit $?
    fi
    if [[ $producer_definition_mode != check ]] || \
       [[ -z $producer_definition_input ]] || \
       [[ -n $repo_checkout || -n $producer_definition_landed_replay ||
          -n $producer_definition_expected_registry_sha256 ]] || \
       { [[ -n $sha ]] && [[ ! $sha =~ ^[0-9a-f]{40}$ ]]; } || \
       { [[ $producer_definition_input != - ]] && [[ ! -r $producer_definition_input ]]; }; then
        usage
        exit 2
    fi
    if [[ $producer_definition_input == - ]]; then
        producer_definition_input=/dev/stdin
    fi
    checked_definition=$(jq -ce '
        . as $definition
        | select(
            type == "object"
            and length > 0
            and (to_entries | all(
              (.key | type == "string")
              and (.value | type == "string" and test("^[0-9a-f]{40}$"))
            ))
          )
        | $definition
      ' "$producer_definition_input" 2>/dev/null) || \
        checked_definition=
    if [[ -z $checked_definition ]]; then
        exit 1
    fi
    jq -en --arg sha "$sha" --argjson definition "$checked_definition" \
        --argjson records "$p_producer_records" '
          $records | any(
            .definition == $definition
            and ((has("valid_commits") | not)
                 or ($sha != "" and (.valid_commits | index($sha) != null)))
          )
        ' \
        >/dev/null
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

owner=${repo%%/*}
mapfile -t candidates < <(jq -r --arg owner "$owner" '
    # Historical ci-hub comments used a service actor in the model slot. Keep
    # that exact tag readable; new comments use one of the four AGENTS.md role
    # forms, with a nonempty model for automated roles.
    def accepted_role_tag:
      . == "[impl agent, ci-hub]"
      or . == "[Human]"
      or (
        (capture("^\\[(?<role>impl agent|adversarial-reviewer agent|coordinator), (?<model>[^]]+)\\]$")? // {})
        | ((.model // "") | test("\\S"))
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

# Resolve the single shared qualifying-receipt predicate from this immutable
# parent commit. An override is used only by the cross-consumer mutation test.
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
predicate_file=${QUALIFYING_RECEIPT_PREDICATE:-}
if [[ -z $predicate_file ]]; then
    predicate_file="$script_dir/../validate/qualifying-receipt.json"
fi
if [[ ! -r $predicate_file ]]; then
    printf 'qualifying-receipt predicate unreadable: %s\n' "$predicate_file" >&2
    exit 2
fi
p_counts_schema=$(jq -r '.counts_schema' "$predicate_file" 2>/dev/null)
p_exec_min=$(jq -r '.require.executed_tests_min' "$predicate_file" 2>/dev/null)
p_cov_schema=$(jq -r '.coverage.applies_at_schema_min' "$predicate_file" 2>/dev/null)
p_cov_pernode=$(jq -r '.coverage.per_node' "$predicate_file" 2>/dev/null)
p_gate_filtered=$(jq -r '(.gate_filtered_tests // false)' "$predicate_file" 2>/dev/null)
p_failures_max=$(jq -r '.require.failures_max' "$predicate_file" 2>/dev/null)
p_profile=$(jq -r '.require.profile' "$predicate_file" 2>/dev/null)
p_selection=$(jq -r '.require.selection_mode' "$predicate_file" 2>/dev/null)
p_result=$(jq -r '.require.result' "$predicate_file" 2>/dev/null)
p_commit_anchored=$(jq -r '.require.commit_anchored' "$predicate_file" 2>/dev/null)
p_tree_dirty=$(jq -r '.require.tree_dirty' "$predicate_file" 2>/dev/null)
for _v in "$p_counts_schema" "$p_exec_min" "$p_cov_schema" "$p_cov_pernode" \
          "$p_gate_filtered" "$p_failures_max" "$p_profile" "$p_selection" \
          "$p_result" "$p_commit_anchored" "$p_tree_dirty"; do
    if [[ -z $_v || $_v == null ]]; then
        printf 'malformed qualifying-receipt predicate: %s\n' "$predicate_file" >&2
        exit 2
    fi
done

# Resolve the producer authority through the same helper used by the publisher.
# The registry comes from this immutable parent commit, never the PR under test.
load_producer_definitions || exit $?
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
        printf 'target commit has no registered producer definition: %s@%s\n' \
            "$repo" "$sha" >&2
        exit 1
    fi
    printf 'cannot resolve target commit producer definition: %s@%s\n' \
        "$repo" "$sha" >&2
    exit 2
fi

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

    # The artifact digest binds the wrapper bytes, not the claimed selected
    # ledger row. Re-run the shared semantic predicate on that embedded row and
    # recompute its named Rust canonicalization before trusting the identity.
    # A writer cannot repair a forged selected digest merely by recomputing the
    # outer artifact digest/path. One Rust invocation owns BOTH decisions; a
    # separate Python qualifier here made the authorities drift and doubled the
    # process cost for every candidate.
    row_file=$(mktemp)
    if ! jq -c '.ledger_record' "$receipt_file" >"$row_file" 2>/dev/null; then
        rm -f -- "$row_file" "$receipt_file"
        continue
    fi
    computed_digest=$("$script_dir/../ci-hub" receipt-digest --sha "$sha" \
        --require-qualifying "${boundary_digest_args[@]}" \
        <"$row_file" 2>/dev/null) || computed_digest=
    if [[ ! $computed_digest =~ ^[0-9a-f]{64}$ ]]; then
        rm -f -- "$row_file" "$receipt_file"
        continue
    fi
    identity_present=$(jq -r 'has("selected_receipt_identity")' "$receipt_file" 2>/dev/null)
    if [[ $identity_present == true ]]; then
        identity_algorithm=$(jq -r '.selected_receipt_identity.digest_algorithm // ""' "$receipt_file")
        identity_canonicalization=$(jq -r '.selected_receipt_identity.canonicalization // ""' "$receipt_file")
        identity_digest=$(jq -r '.selected_receipt_identity.digest // ""' "$receipt_file")
        if [[ $identity_algorithm != sha256 ]] || \
           [[ $identity_canonicalization != 'serde_json::to_vec(HistoryRow)-v1' ]] || \
           [[ ! $identity_digest =~ ^[0-9a-f]{64}$ ]] || \
           [[ $computed_digest != "$identity_digest" ]]; then
            rm -f -- "$row_file" "$receipt_file"
            continue
        fi
    elif [[ $role_class != legacy-service ]]; then
        rm -f -- "$row_file" "$receipt_file"
        continue
    fi
    rm -f -- "$row_file"

    receipt_definition=$(jq -c '.producer.definition // null' "$receipt_file" 2>/dev/null) || \
        receipt_definition=null
    producer_record=$(jq -cn \
        --argjson target "$target_producer_record" \
        --argjson definition "$receipt_definition" \
        '$target | select(.definition == $definition)' 2>/dev/null) || producer_record=
    if [[ -z $producer_record ]]; then
        rm -f -- "$receipt_file"
        continue
    fi

    if jq -e \
        --arg sha "$sha" --arg repo "$repo" --arg role_class "$role_class" \
        --arg req_profile "$p_profile" \
        --arg req_selection "$p_selection" \
        --arg req_result "$p_result" \
        --argjson req_commit_anchored "$p_commit_anchored" \
        --argjson req_tree_dirty "$p_tree_dirty" \
        --argjson req_failures_max "$p_failures_max" \
        --argjson exec_min "$p_exec_min" \
        --argjson counts_schema "$p_counts_schema" \
        --argjson cov_schema "$p_cov_schema" \
        --argjson cov_pernode "$p_cov_pernode" \
        --argjson gate_filtered "$p_gate_filtered" \
        --argjson producer_record "$producer_record" '
        def integer:
          if type == "number" then . == floor else false end;
        def nonempty_string:
          if type == "string" then test("\\S") else false end;
        def selected_identity_valid:
          .digest_algorithm == "sha256"
          and .canonicalization == "serde_json::to_vec(HistoryRow)-v1"
          and (.digest | test("^[0-9a-f]{64}$"));
        def current_structural_row($sha; $repo):
          .ledger_record as $row
          | $repo == "rrnewton/hermit"
          and ($row.schema_version | integer and . >= 4)
          and (
            $row.repo == "hermit"
            or $row.repo == "rrnewton/hermit"
            or ($row.schema_version == 4 and $row.repo == null)
          )
          and $row.commit == $sha
          and ($row.tree | type == "string" and test("^[0-9A-Fa-f]{40}$"))
          and $row.raw_result == "pass"
          and $row.exit_code == 0
          and ($row.checks | integer and . > 0)
          and ($row.gates_run | integer and . >= $row.gates_expected)
          and ($row.gates_expected | integer and . > 0)
          and $row.checks == $row.gates_run
          and ($row.gates | type == "array" and length == $row.gates_run)
          and all($row.gates[];
            (.name | nonempty_string)
            and .result == "pass"
            and .exit_code == 0
          )
          and ($row.executed_tests | integer)
          and ($row.filtered_tests | integer and . >= 0)
          and ($row.started_at | nonempty_string)
          and ($row.finished_at | nonempty_string)
          and ($row.host | nonempty_string)
          and ($row.slot | nonempty_string)
          and ($row.log_file | nonempty_string);
        .schema_version == 1
        and .repository == $repo
        and .commit == $sha
        # PRODUCER BINDING: the receipt must name the check definition that
        # produced it, and that definition must be one registered exact whole
        # map: the primary, or the one unexpired transition candidate. Never
        # union fields across maps. A receipt cannot drop/add a file to escape
        # the comparison, and an absent block is never a free pass.
        and (.producer.definition == $producer_record.definition)
        and (
          if $producer_record.coverage_status == "complete" then
            .producer.coverage_status == "complete"
            and .producer.paths == $producer_record.paths
          else
            ((.producer.coverage_status // "legacy-selected-paths")
             == "legacy-selected-paths")
            and ((.producer.paths // $producer_record.paths)
                 == $producer_record.paths)
          end
        )
        and (.ledger_record.host | nonempty_string)
        and (.run_id == ($sha + "@" + .ledger_record.started_at + "@" + .ledger_record.host))
        and (.log_sha256 | test("^[0-9a-f]{64}$"))
        and .ledger_record.commit == $sha
        and .ledger_record.profile == $req_profile
        and .ledger_record.selection_mode == $req_selection
        and .ledger_record.commit_anchored == $req_commit_anchored
        and .ledger_record.tree_dirty == $req_tree_dirty
        and .ledger_record.result == $req_result
        and ((.ledger_record.failures // 0) <= $req_failures_max)
        and .ledger_record.log_file == .source_log_file
        and (.durable_log_file | startswith("/"))
        and (.ledger_record.executed_tests != 0)
        and (
          (.ledger_record.schema_version // 0) as $schema
          | ($schema >= $counts_schema) as $count_capable
          | ((.ledger_record.executed_tests | integer)
             and (.ledger_record.filtered_tests | integer)) as $counts_present
          | (.ledger_record.executed_tests | integer and . >= $exec_min) as $executed_ok
          | (if $gate_filtered then (.ledger_record.filtered_tests == 0) else true end) as $filtered_ok
          | $filtered_ok and (
              if $count_capable then
                $executed_ok and (
                  if ($cov_pernode and ($schema >= $cov_schema)) then
                    (.ledger_record.coverage.planned_test_nodes > 0
                     and .ledger_record.coverage.zero_executed_nodes == []
                     and .ledger_record.coverage.absent_nodes == [])
                  else true end)
              elif $counts_present then
                $executed_ok
              else false end)
        )
        and (
          if $role_class == "legacy-service" and (has("selected_receipt_identity") | not)
            then true
            else (.selected_receipt_identity | selected_identity_valid)
            end
        )
        and ($role_class == "legacy-service" or current_structural_row($sha; $repo))
    ' "$receipt_file" >/dev/null; then
        producer_status=$(jq -r '.coverage_status' <<<"$producer_record")
        producer_paths=$(jq -r '.paths | join(",")' <<<"$producer_record")
        producer_valid_commits=$(jq -r '
            if has("valid_commits") then (.valid_commits | join(","))
            else "unbounded" end
          ' <<<"$producer_record")
        printf 'receipt_commit=%s receipt_path=%s receipt_sha256=%s producer_coverage_status=%s producer_paths=%s producer_valid_commits=%s\n' \
            "$receipt_commit" "$receipt_path" "$receipt_digest" \
            "$producer_status" "$producer_paths" "$producer_valid_commits"
        rm -f -- "$receipt_file"
        exit 0
    fi
    rm -f -- "$receipt_file"
done

printf 'no immutable counted local-validation receipt for exact head %s\n' "$sha" >&2
exit 1
