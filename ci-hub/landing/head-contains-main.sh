#!/usr/bin/env bash
# Sourced by land-pr.sh. Answers one question for a --no-rebase landing: does
# the head about to be merged CONTAIN the current target main, read from the
# canonical GitHub repository rather than from any local ref?
#
# WHY THIS EXISTS. Under --no-rebase the lander merges the head exactly as it
# was validated and approved, and `gh pr merge --rebase` then replays its
# commits server-side onto whatever main is by then. When the head is behind
# main, the tree that lands is one nobody validated or reviewed. Measured:
# https://github.com/rrnewton/reverie/pull/679 was queued with --no-rebase at
# head 169f1d0b9f9cf9abb3d78c13c1e61811ae43c1ac, one landing behind main
# (f5cbc2e2 had landed after the head's base), and landed as 547278b9 with no
# check that the head contained main.
#
# WHY NOT `origin/main`. The checkout's remote-tracking ref lives in a common
# git dir that other agents share, and a plain `git fetch origin` in a sibling
# worktree whose origin is a stale local mirror force-rewinds it (measured
# 2026-09-28: 895d214e -> 6297f715). A global url.insteadOf also rewrites the
# `.git` spelling of the reverie URL to that same stale mirror. A rewound or
# mirror-fed base makes a behind head look current, which is the exact failure
# being refused. So this fetches the canonical URL into a PRIVATE per-process
# ref, refuses if that URL is itself rewritten by insteadOf, and never touches
# refs/remotes/origin/*.
#
# Contract (stdout is exactly one verdict line; the caller must require BOTH
# the verdict word AND the status, so a missing helper -- which produces a
# shell error and no verdict -- can never read as a pass):
#   rc 0  "CONTAINS <main-sha>"      head contains current main
#   rc 1  "BEHIND <main-sha>"        main is not an ancestor of head
#   rc 2  "UNDETERMINED <reason>"    could not decide; callers must refuse

lander_contains_main_git_net() {
  if command -v with-proxy >/dev/null 2>&1; then
    with-proxy git "$@"
  else
    git "$@"
  fi
}

lander_head_contains_canonical_main() {
  local checkout=$1 url=$2 head=$3
  local expanded ref main_sha fetch_out rc
  if [ -z "$checkout" ] || [ -z "$url" ] || [ -z "$head" ]; then
    echo "UNDETERMINED usage: lander_head_contains_canonical_main CHECKOUT URL HEAD"
    return 2
  fi
  # A url.<base>.insteadOf rule would silently send this fetch somewhere else
  # (today, to a day-stale local mirror). `ls-remote --get-url` applies the
  # rewrite without contacting the remote, so an exact match proves none applies.
  expanded=$(git -C "$checkout" ls-remote --get-url "$url" 2>/dev/null) || expanded=
  if [ "$expanded" != "$url" ]; then
    echo "UNDETERMINED $url is rewritten by git url.insteadOf to '${expanded:-<unreadable>}'; refusing to read main from anywhere but the canonical repository"
    return 2
  fi
  ref="refs/land-pr/contains-main/$$-${RANDOM}${RANDOM}"
  fetch_out=$(lander_contains_main_git_net -C "$checkout" fetch -q --no-tags \
    --no-write-fetch-head --no-recurse-submodules \
    "$url" "+refs/heads/main:$ref" 2>&1)
  rc=$?
  if [ "$rc" -ne 0 ]; then
    git -C "$checkout" update-ref -d "$ref" >/dev/null 2>&1
    echo "UNDETERMINED could not fetch main from $url (rc=$rc): $(tr '\n' ' ' <<<"$fetch_out" | tail -c 200)"
    return 2
  fi
  main_sha=$(git -C "$checkout" rev-parse --verify -q "$ref^{commit}" 2>/dev/null) || main_sha=
  git -C "$checkout" update-ref -d "$ref" >/dev/null 2>&1
  if [[ ! $main_sha =~ ^[0-9a-f]+$ ]]; then
    echo "UNDETERMINED fetched $url main but could not resolve it to a commit"
    return 2
  fi
  if ! git -C "$checkout" rev-parse --verify -q "$head^{commit}" >/dev/null 2>&1; then
    echo "UNDETERMINED head $head is not present in $checkout, so its ancestry cannot be compared"
    return 2
  fi
  # `merge-base --is-ancestor` exits 1 for "not an ancestor" and another
  # non-zero status when it could not answer. Only 1 is a determined BEHIND.
  git -C "$checkout" merge-base --is-ancestor "$main_sha" "$head" >/dev/null 2>&1
  rc=$?
  case "$rc" in
    0) echo "CONTAINS $main_sha"; return 0 ;;
    1) echo "BEHIND $main_sha"; return 1 ;;
    *) echo "UNDETERMINED merge-base --is-ancestor $main_sha $head failed (rc=$rc)"; return 2 ;;
  esac
}
