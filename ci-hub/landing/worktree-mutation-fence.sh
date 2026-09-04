#!/usr/bin/env bash
# Shared worktree-mutation ownership fence for land-pr and union-rebase.
#
# The land-lock supervisor outlives a hard-killed landing child. Process-local
# shell variables therefore cannot tell that supervisor whether the child moved
# the caller's checkout. This file records one exact post-mutation state through
# atomic rename. A restorer acts only when its token, checkout, entry state, and
# current state all match; any later external change is left untouched.

# Keep this exact set synchronized with ci-hub/lib/git_env.py. These selectors
# override even `git -C <checkout>` and are inherited by every Git subprocess.
# Sourcing the fence is the shell authority boundary used by both land-pr and
# standalone union-rebase, so clear them before either can inspect a checkout.
LANDER_GIT_REPOSITORY_ENV=(
  GIT_INDEX_FILE
  GIT_DIR
  GIT_WORK_TREE
  GIT_COMMON_DIR
  GIT_OBJECT_DIRECTORY
  GIT_ALTERNATE_OBJECT_DIRECTORIES
  GIT_NAMESPACE
  GIT_PREFIX
  GIT_INDEX_VERSION
)
unset "${LANDER_GIT_REPOSITORY_ENV[@]}"

lander_checkout_state() {
  local checkout=$1 ref head status
  ref=$(git -C "$checkout" symbolic-ref -q --short HEAD 2>/dev/null || echo DETACHED)
  head=$(git -C "$checkout" rev-parse -q --verify HEAD 2>/dev/null) || return 1
  status=$(git -C "$checkout" status --porcelain=v1 -z --untracked-files=all 2>/dev/null \
    | sha256sum | awk '{print $1}') || return 1
  printf '%s|%s|%s\n' "$ref" "$head" "$status"
}

lander_fence_arguments_are_valid() {
  local checkout=$1 entry_state=$2 fence=$3 token=$4
  [ -n "$checkout" ] && [ "${checkout#/}" != "$checkout" ] \
    && [ -n "$entry_state" ] && [ "${fence#/}" != "$fence" ] \
    && [[ $token =~ ^[0-9a-f]{32}$ ]] \
    && [[ $checkout != *$'\n'* ]] && [[ $entry_state != *$'\n'* ]] \
    && [[ $fence != *$'\n'* ]]
}

lander_read_mutation_fence() {
  local checkout=$1 entry_state=$2 fence=$3 token=$4
  LANDER_FENCE_OWNED_STATE=
  lander_fence_arguments_are_valid "$checkout" "$entry_state" "$fence" "$token" \
    || return 2
  [ -s "$fence" ] || return 1
  local -a rows=()
  mapfile -t rows <"$fence" || return 2
  [ "${#rows[@]}" -eq 5 ] \
    && [ "${rows[0]}" = land-pr-mutation-v1 ] \
    && [ "${rows[1]}" = "$token" ] \
    && [ "${rows[2]}" = "$checkout" ] \
    && [ "${rows[3]}" = "$entry_state" ] \
    && [ -n "${rows[4]}" ] || return 2
  LANDER_FENCE_OWNED_STATE=${rows[4]}
}

lander_record_checkout_mutation() {
  local checkout=$1 entry_state=$2 fence=$3 token=$4
  shift 4
  local current ref expected matched=0 temporary
  lander_fence_arguments_are_valid "$checkout" "$entry_state" "$fence" "$token" \
    || return 2
  current=$(lander_checkout_state "$checkout") || return 2
  [ "$current" != "$entry_state" ] || return 0
  ref=${current%%|*}
  for expected in "$@"; do
    [ "$ref" = "$expected" ] && matched=1
  done
  if [ "$matched" -ne 1 ]; then
    printf '%s\n' \
      "land-pr: refusing to claim unexpected checkout state '$ref' as this invocation's mutation" >&2
    return 2
  fi
  temporary=$(mktemp "${fence}.tmp.XXXXXXXX") || return 2
  chmod 600 "$temporary" || { rm -f -- "$temporary"; return 2; }
  if ! printf '%s\n' \
      land-pr-mutation-v1 "$token" "$checkout" "$entry_state" "$current" \
      >"$temporary"; then
    rm -f -- "$temporary"
    return 2
  fi
  mv -f -- "$temporary" "$fence" || { rm -f -- "$temporary"; return 2; }
  LANDER_FENCE_OWNED_STATE=$current
}

lander_clear_mutation_fence() {
  local checkout=$1 entry_state=$2 fence=$3 token=$4
  local rc=0
  lander_read_mutation_fence "$checkout" "$entry_state" "$fence" "$token" || rc=$?
  [ "$rc" -eq 1 ] && return 0
  [ "$rc" -eq 0 ] || return 2
  rm -f -- "$fence"
}

lander_restore_owned_checkout() {
  local checkout=$1 entry_ref=$2 entry_detached=$3 entry_state=$4
  local fence=$5 token=$6 clear_on_success=${7:-0}
  local rc=0 current entry_head restored
  lander_read_mutation_fence "$checkout" "$entry_state" "$fence" "$token" || rc=$?
  if [ "$rc" -eq 1 ]; then
    [ "$clear_on_success" -eq 1 ] && rm -f -- "$fence"
    return 0
  fi
  if [ "$rc" -ne 0 ]; then
    printf '%s\n' \
      "land-pr: WARNING: refusing checkout restoration: mutation fence is invalid at $fence" >&2
    return 0
  fi
  current=$(lander_checkout_state "$checkout") || current=UNREADABLE-BEFORE-RESTORE
  if [ "$current" = "$entry_state" ]; then
    [ "$clear_on_success" -eq 1 ] \
      && lander_clear_mutation_fence "$checkout" "$entry_state" "$fence" "$token"
    return 0
  fi
  if [ "$current" != "$LANDER_FENCE_OWNED_STATE" ]; then
    printf '%s\n' \
      "land-pr: WARNING: refusing to restore $checkout: checkout changed after the lander's last recorded mutation; current state is not lander-owned. Fence retained at $fence." >&2
    return 0
  fi
  entry_head=${entry_state#*|}
  entry_head=${entry_head%%|*}
  if [ "$entry_detached" -ne 1 ]; then
    restored=$(git -C "$checkout" rev-parse -q --verify "${entry_ref}^{commit}" 2>/dev/null) \
      || restored=
    if [ "$restored" != "$entry_head" ]; then
      printf '%s\n' \
        "land-pr: WARNING: refusing to restore $checkout: entry ref $entry_ref moved from $entry_head to ${restored:-unreadable}. Fence retained at $fence." >&2
      return 0
    fi
  fi
  if [ "$entry_detached" -eq 1 ]; then
    git -C "$checkout" checkout -q --detach "$entry_ref" 2>/dev/null
  else
    git -C "$checkout" checkout -q "$entry_ref" 2>/dev/null
  fi || {
    printf '%s\n' \
      "land-pr: WARNING: could not restore $checkout to $entry_ref. Fence retained at $fence." >&2
    return 0
  }
  restored=$(lander_checkout_state "$checkout") || restored=UNREADABLE-AFTER-RESTORE
  if [ "$restored" != "$entry_state" ]; then
    printf '%s\n' \
      "land-pr: WARNING: restored checkout state does not match its entry snapshot. Fence retained at $fence." >&2
    return 0
  fi
  [ "$clear_on_success" -eq 1 ] \
    && lander_clear_mutation_fence "$checkout" "$entry_state" "$fence" "$token"
}
