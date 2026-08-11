#!/usr/bin/env bash
set -euo pipefail

# Prepare, validate, and optionally publish non-rewriting Reverie pin bumps.
# TARGET_SHA is resolved from Reverie main at execution time because that branch
# is the synchronization barrier. An explicitly supplied SHA is only an
# expectation guard; the command refuses it if main has already moved.

usage() {
  cat <<'EOF'
Usage:
  checkout-hermit-pr-latest-reverie.sh \
    --repo <clean-hermit-worktree> --model <MODEL> --agent <team-slug> \
    --task <stable-task-id> \
    [--push] <pr> [<pr> ...]

The command resolves latest Reverie main itself. Optional
--target-sha <40-hex> asserts an expected tip and fails if it is stale.

The operation is fail-closed and sequential per PR:
  fetch -> verify PR state -> merge current Hermit main -> run the checker's
  derived --update-to-latest mutation ->
  commit -> full validate at the exact commit -> non-force push (with --push).

Without --push, the validated candidate remains local. Evidence is written to
scratch/pin-bump-evidence/. The command launches a durable user-systemd unit and
returns its unit/log immediately. A failed PR stops before later PRs run.
EOF
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

note() {
  printf '[pin-bump] %s\n' "$*"
}

repo=
target=${TARGET_SHA:-}
model=${LANDER_MODEL:-}
agent=${DG_AGENT_NAME:-}
task=
push=false
foreground=false
prs=()
while (($#)); do
  case "$1" in
    --target-sha)
      (($# >= 2)) || die "--target-sha requires a SHA"
      [[ -z $target || $target == "$2" ]] || \
        die "TARGET_SHA and --target-sha disagree"
      target=$2
      shift 2
      ;;
    --repo)
      (($# >= 2)) || die "--repo requires a path"
      repo=$2
      shift 2
      ;;
    --model)
      (($# >= 2)) || die "--model requires a model name"
      model=$2
      shift 2
      ;;
    --agent)
      (($# >= 2)) || die "--agent requires a team slug"
      agent=$2
      shift 2
      ;;
    --task)
      (($# >= 2)) || die "--task requires a stable task id"
      task=$2
      shift 2
      ;;
    --push)
      push=true
      shift
      ;;
    --foreground)
      foreground=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --*)
      die "unknown option: $1"
      ;;
    *)
      [[ $1 =~ ^[0-9]+$ ]] || die "PR must be numeric: $1"
      prs+=("$1")
      shift
      ;;
  esac
done

[[ -z $target || $target =~ ^[0-9a-f]{40}$ ]] || \
  die "--target-sha (or TARGET_SHA) must be a full lowercase 40-hex SHA"
[[ -n $repo ]] || die "--repo is required"
[[ $model =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || \
  die "--model (or LANDER_MODEL) is required and must be a tag-safe model name"
[[ $agent =~ ^[a-z0-9][a-z0-9-]*$ ]] || \
  die "--agent (or DG_AGENT_NAME) is required and must be a lowercase team slug"
[[ $task =~ ^[a-z0-9][a-z0-9._-]*$ ]] || \
  die "--task is required and must be a stable task id"
((${#prs[@]} > 0)) || die "at least one PR is required"
repo=$(realpath "$repo")
[[ -d $repo/.git || -f $repo/.git ]] || die "not a Git worktree: $repo"
workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
attribution_checker="$workspace/scripts/check_commit_attribution.py"
[[ -x $attribution_checker ]] || die "missing attribution checker: $attribution_checker"
machine=$("$attribution_checker" --detect-machine) || \
  die "could not derive the commit-attribution machine"
commit_attribution="[impl agent, $model] [$agent, $machine]"

command -v gh >/dev/null || die "gh is required"
command -v jq >/dev/null || die "jq is required"
command -v with-proxy >/dev/null || die "with-proxy is required"

ci_hub="$workspace/ci-hub/ci-hub"
[[ -x $ci_hub ]] || die "missing ci-hub executable: $ci_hub"
evidence_dir="$workspace/scratch/pin-bump-evidence"
mkdir -p "$evidence_dir"

resolved_target=$(with-proxy git ls-remote \
  https://github.com/rrnewton/reverie.git refs/heads/main | awk '{print $1}')
[[ $resolved_target =~ ^[0-9a-f]{40}$ ]] || die "could not resolve Reverie main"
[[ -z $target || $target == "$resolved_target" ]] || \
  die "expected Reverie main $target, but latest is $resolved_target"
target=$resolved_target

if ! $foreground; then
  unit="hermit-pin-bump-${target:0:8}-$(date +%s)"
  launcher_log="$evidence_dir/$unit.log"
  relaunch=(
    "$0" --foreground --target-sha "$target" --repo "$repo"
    --model "$model" --agent "$agent" --task "$task"
  )
  $push && relaunch+=(--push)
  relaunch+=("${prs[@]}")
  systemd-run --user \
    --unit "$unit" \
    --working-directory "$workspace" \
    --collect \
    --setenv "HOME=$HOME" \
    --setenv "PATH=$PATH" \
    --setenv "HERMIT_COMMIT_MACHINE=$machine" \
    --property "StandardOutput=append:$launcher_log" \
    --property "StandardError=append:$launcher_log" \
    "${relaunch[@]}"
  printf 'STARTED_UNIT=%s\nLOG=%s\nTARGET_SHA=%s\nPRS=%s\n' \
    "$unit" "$launcher_log" "$target" "${prs[*]}"
  exit 0
fi

[[ -z $(git -C "$repo" status --porcelain) ]] || die "worktree is dirty: $repo"
[[ $(git -C "$repo" remote get-url origin) == *rrnewton/hermit* ]] || \
  die "origin is not rrnewton/hermit"

note "fetching authoritative tips"
with-proxy git -C "$repo" fetch origin main
git -C "$repo" show refs/remotes/origin/main:scripts/check-reverie-pin.rs | \
  grep -F -- '--update-to-latest' >/dev/null || \
  die "current Hermit main lacks the authoritative pin updater; land #1591 first"

for pr in "${prs[@]}"; do
  operation_started=$(date +%s)
  short_target=${target:0:12}
  # Branch refs are shared by every worktree in the clone. A preserved failed
  # attempt must not make all later clean slots falsely report a collision.
  branch="pin-bump/pr-${pr}-${short_target}-${agent}-$$"
  note "PR #$pr: reading fresh metadata"

  metadata=$(with-proxy gh pr view "$pr" -R rrnewton/hermit \
    --json state,isDraft,baseRefName,headRefName,headRefOid,headRepositoryOwner)
  state=$(jq -r '.state' <<<"$metadata")
  base=$(jq -r '.baseRefName' <<<"$metadata")
  head_ref=$(jq -r '.headRefName' <<<"$metadata")
  old_head=$(jq -r '.headRefOid' <<<"$metadata")
  head_owner=$(jq -r '.headRepositoryOwner.login' <<<"$metadata")
  [[ $state == OPEN ]] || die "PR #$pr is not open"
  [[ $base == main ]] || die "PR #$pr base is $base, not main"
  [[ $head_owner == rrnewton ]] || die "PR #$pr is from fork owner $head_owner"
  [[ $old_head =~ ^[0-9a-f]{40}$ ]] || die "PR #$pr has invalid head SHA"

  # Each candidate gets the freshest Hermit base. The target Reverie tip must
  # remain fixed for the full batch; any movement stops before another PR.
  with-proxy git -C "$repo" fetch origin main
  reverie_now=$(with-proxy git ls-remote https://github.com/rrnewton/reverie.git refs/heads/main | awk '{print $1}')
  [[ $reverie_now == "$target" ]] || \
    die "Reverie main moved during batch ($target -> $reverie_now)"
  with-proxy git -C "$repo" fetch origin \
    "refs/heads/$head_ref:refs/remotes/origin/pin-bump/$pr"
  fetched_head=$(git -C "$repo" rev-parse "refs/remotes/origin/pin-bump/$pr")
  [[ $fetched_head == "$old_head" ]] || die "PR #$pr head moved during fetch"
  git -C "$repo" show-ref --verify --quiet "refs/heads/$branch" && \
    die "local branch already exists: $branch"

  git -C "$repo" switch --detach "$old_head"
  git -C "$repo" switch -c "$branch"
  if ! git -C "$repo" merge --no-ff \
    -m "Merge current Hermit main into PR #$pr" \
    -m "Task: $task" \
    -m "$commit_attribution" refs/remotes/origin/main; then
    git -C "$repo" merge --abort || true
    die "PR #$pr does not merge cleanly with current Hermit main"
  fi
  pin_base=$(git -C "$repo" rev-parse HEAD)

  # check-reverie-pin.rs is the single authority for BOTH the derived Cargo
  # metadata domain and its mutation. A compatibility path with a hand-written
  # file list would recreate the drift this command exists to prevent.
  "$repo/scripts/check-reverie-pin.rs" --help | \
    grep -F -- '--update-to-latest' >/dev/null || \
    die "PR #$pr: current Hermit main lacks the authoritative pin updater; land #1591 first"
  note "PR #$pr: deriving and updating every Reverie pin site via the checker"
  (
    cd "$repo"
    with-proxy ./scripts/check-reverie-pin.rs --update-to-latest
  )
  updated_pin=$("$repo/scripts/check-reverie-pin.rs" --repo "$repo" --print-pin)
  [[ $updated_pin == "$target" ]] || \
    die "PR #$pr: checker updated to $updated_pin, expected $target"

  unexpected=$(git -C "$repo" diff --name-only "$pin_base" -- | \
    rg -v '(^|/)Cargo\.(toml|lock)$' || true)
  [[ -z $unexpected ]] || die "PR #$pr: pin generation changed unexpected paths: $unexpected"
  git -C "$repo" diff --check "$pin_base" --

  change_report="$evidence_dir/pr-${pr}-${short_target}-changes.txt"
  {
    printf 'PR=%s\nTARGET_SHA=%s\nPIN_BASE=%s\n' "$pr" "$target" "$pin_base"
    git -C "$repo" diff --name-status "$pin_base" --
    git -C "$repo" diff --stat "$pin_base" --
  } | tee "$change_report"

  bad_pins=$(git -C "$repo" grep -nE \
    'github\.com/rrnewton/reverie[^"#]*(rev = "|\?rev=)[0-9a-f]{7,40}' \
    -- '*Cargo.toml' '*Cargo.lock' | rg -v "$target" || true)
  [[ -z $bad_pins ]] || die "PR #$pr: stale/mixed Reverie pins remain: $bad_pins"
  (cd "$repo" && with-proxy ./scripts/check-reverie-pin.rs)

  # validate.rs compiles the tracked safe-ci-dag-runner path dependency. New
  # product slots do not necessarily materialize nested submodules, so prepare
  # the exact gitlink rather than depending on cache history from an older run.
  note "PR #$pr: initializing the exact agent-utils validation dependency"
  with-proxy git -C "$repo" submodule update --init --checkout -- agent-utils
  [[ -f $repo/agent-utils/rs/safe-ci-dag-runner/Cargo.toml ]] || \
    die "PR #$pr: safe-ci-dag-runner dependency is not materialized"

  mapfile -t pin_paths < <(git -C "$repo" diff --name-only "$pin_base" -- \
    | rg '(^|/)Cargo\.(toml|lock)$')
  if ((${#pin_paths[@]} > 0)); then
    git -C "$repo" add -- "${pin_paths[@]}"
    git -C "$repo" diff --cached --quiet && \
      die "PR #$pr: pin paths changed but produced no staged update"
    git -C "$repo" commit -m "build: bump Reverie pin to $short_target" \
      -m "Task: $task" \
      -m "$commit_attribution"
    candidate=$(git -C "$repo" rev-parse HEAD)
  else
    note "PR #$pr: Reverie pin already current at $target; no bump commit needed"
    candidate=$pin_base
  fi
  git -C "$repo" merge-base --is-ancestor "$old_head" "$candidate" || \
    die "PR #$pr candidate rewrites original head"

  log="$evidence_dir/pr-${pr}-${candidate}.log"
  note "PR #$pr: full validate at exact head $candidate (log $log)"
  validate_started=$(date +%s)
  set +e
  CARGO_BUILD_JOBS=2 THIRD_PARTY_BUILD_JOBS=2 "$ci_hub" validate-run \
    --checkout "$repo" \
    --agent "$agent" \
    --target "$candidate" \
    --pr "$pr" \
    --in-place \
    --log "$log" \
    --wait 7200 \
    --hold 1200 \
    --child-deadline 3600 \
    -- full
  validate_rc=$?
  set -e
  validate_finished=$(date +%s)
  validate_wall=$((validate_finished - validate_started))
  operation_wall=$((validate_finished - operation_started))
  printf 'PR=%s\nTARGET_SHA=%s\nCANDIDATE_SHA=%s\nVALIDATE_RC=%s\nVALIDATE_WALL_SECONDS=%s\nBUMP_PLUS_VALIDATE_WALL_SECONDS=%s\nLOG=%s\n' \
    "$pr" "$target" "$candidate" "$validate_rc" "$validate_wall" \
    "$operation_wall" "$log" | tee "$evidence_dir/pr-${pr}-${candidate}.result"
  ((validate_rc == 0)) || die "PR #$pr validation failed (rc=$validate_rc); not pushed"

  if $push; then
    remote_now=$(with-proxy git -C "$repo" ls-remote origin "refs/heads/$head_ref" | awk '{print $1}')
    [[ $remote_now == "$old_head" ]] || die "PR #$pr head moved during validation; not pushed"
    with-proxy git -C "$repo" push origin \
      "$candidate:refs/heads/$head_ref"
    published=$(with-proxy gh pr view "$pr" -R rrnewton/hermit --json headRefOid --jq '.headRefOid')
    [[ $published == "$candidate" ]] || die "PR #$pr published head mismatch"
    note "PR #$pr: pushed exact validated head $candidate without force"
  else
    note "PR #$pr: validated exact local head $candidate; --push not supplied"
  fi
done
