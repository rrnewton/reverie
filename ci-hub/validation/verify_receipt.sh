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
    if [[ $repo == rrnewton/reverie ]]; then
        observe_producer_paths_from_checkout "$checkout" "$target_sha" '["validate.sh"]'
        return $?
    fi
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
                '$record
                 | {definition, coverage_status, paths}
                 + (if has("valid_commits") then {valid_commits} else {} end)
                 + {resolved_from: $resolved_from}'
            return 0
        fi
    done < <(jq -c '.[]' <<<"$p_producer_records")
    return 1
}

observe_primary_producer_record_from_checkout() {
    local checkout=$1 target_sha=$2 paths
    if [[ $repo == rrnewton/reverie ]]; then
        observe_producer_paths_from_checkout "$checkout" "$target_sha" '["validate.sh"]'
        return $?
    fi
    paths=$(jq -c '.paths' <<<"$p_primary_record") || return 2
    observe_producer_paths_from_checkout "$checkout" "$target_sha" "$paths"
}

observe_producer_paths_from_checkout() {
    local checkout=$1 target_sha=$2 paths=$3 checked_definition relative blob
    # Reverie's canonical policy qualifies the complete run separately. This
    # records its actual entrypoint at the exact commit, not a claim that one
    # path contains the whole validation implementation.
    checked_definition='{}'
    while IFS= read -r relative; do
        blob=$(git -C "$checkout" rev-parse "$target_sha:$relative" 2>/dev/null) || return 1
        [[ $blob =~ ^[0-9a-f]{40}$ ]] || return 1
        [[ $(git -C "$checkout" cat-file -t "$blob" 2>/dev/null) == blob ]] || return 1
        checked_definition=$(jq -cn \
            --argjson definition "$checked_definition" \
            --arg relative "$relative" --arg blob "$blob" \
            '$definition + {($relative): $blob}') || return 2
    done < <(jq -r '.[]' <<<"$paths")
    jq -cn --argjson definition "$checked_definition" \
        --argjson paths "$paths" \
        --arg resolved_from "$checkout" \
        '{definition: $definition, coverage_status: "legacy-selected-paths",
          paths: $paths, resolved_from: $resolved_from}'
}

resolve_producer_record_from_github() {
    local target_repo=$1 target_sha=$2 producer_record expected_definition
    local checked_definition relative blob saw_error=0
    if [[ $repo == rrnewton/reverie ]]; then
        blob=$("${gh_cmd[@]}" api \
            "repos/$target_repo/contents/validate.sh?ref=$target_sha" \
            --jq 'select(.type == "file") | .sha' 2>/dev/null) || return 2
        [[ $blob =~ ^[0-9a-f]{40}$ ]] || return 1
        jq -cn --arg blob "$blob" --arg resolved_from "github:$target_repo@$target_sha" \
            '{definition: {"validate.sh": $blob}, coverage_status: "legacy-selected-paths",
              paths: ["validate.sh"], resolved_from: $resolved_from}'
        return $?
    fi
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
                '$record
                 | {definition, coverage_status, paths}
                 + (if has("valid_commits") then {valid_commits} else {} end)
                 + {resolved_from: $resolved_from}'
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
  --producer-repo-checkout DIR  Product object store containing exact --sha
  --producer-definition-primary  Print the validated primary map and exit
  --producer-definition-allowed  Print all currently allowed exact maps and exit
  --producer-definition-check FILE  Accept FILE only if it is one allowed whole map
  --producer-definition-resolve  Derive and check --sha from --repo-checkout
  --producer-definition-observe  Derive the primary path set at --sha without registration
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
        --producer-definition-observe) producer_definition_mode=observe; shift ;;
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
    if [[ $repo == rrnewton/reverie ]]; then
        case "$producer_definition_mode" in
            resolve|observe) ;;
            *) printf 'Reverie producer evidence has no Hermit registration mode\n' >&2; exit 2 ;;
        esac
    elif [[ $repo == rrnewton/hermit ]]; then
        load_producer_definitions || exit $?
    else
        printf 'unsupported producer repository: %s\n' "$repo" >&2
        exit 2
    fi
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
    if [[ $producer_definition_mode == observe ]]; then
        if [[ -n $producer_definition_landed_replay ]] || \
           [[ -n $producer_definition_expected_registry_sha256 ]] || \
           [[ ! $sha =~ ^[0-9a-f]{40}$ ]] || [[ -z $repo_checkout ]] || \
           [[ ! -d $repo_checkout ]]; then
            usage
            exit 2
        fi
        observe_primary_producer_record_from_checkout "$repo_checkout" "$sha"
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
# Resolve the producer authority through the same helper used by the publisher.
# Hermit's registry comes from this immutable parent commit, never the PR under
# test. Reverie records its actual validate.sh blob with selected-path coverage.
if [[ $repo == rrnewton/hermit ]]; then
    load_producer_definitions || exit $?
elif [[ $repo != rrnewton/reverie ]]; then
    printf 'unsupported producer repository: %s\n' "$repo" >&2
    exit 2
fi
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
        --expected-producer-record "$expected_producer_file"
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
