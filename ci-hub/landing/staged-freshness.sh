#!/usr/bin/env bash
# Refuse a parent commit whose staged blob is an OLDER copy of what is landed.
#
# WHY THIS EXISTS. Attribution is not sufficient to make a commit safe. On
# 2026-08-06, three of 59 paths staged in the shared parent index were stale
# pre-fix copies; committing them would have silently reverted e9d433c and
# 8117b39c/c6265f5 while looking like ordinary cleanup. The staging was not
# malicious or careless -- it was a post-rewrite RESTORATION taken from a
# snapshot captured before those fixes landed. Correct owner, correct intent,
# wrong vintage.
#
# So the safe-commit checklist is three independent things, not one:
#   1. explicit pathspec            -- guards BREADTH (never `git add -A`)
#   2. no foreign staged change     -- guards COLLISION on a single file, since
#      in that same path              `git commit -o P` commits the WORKING TREE
#                                     content of P, so a peer's staged change to
#                                     P rides along under your message
#   3. staged blob not older than   -- guards FRESHNESS. This script.
#      what is landed
# The in-flight four-call-site conversion and guard flip cover (1). Nothing
# covered (2) or (3).
#
# THE PREDICATE, per path:
#   staged := git rev-parse :<path>            (blob in the index)
#   landed := git rev-parse origin/main:<path> (blob at the tip of the remote)
#   staged == landed                  -> NOOP, committing it changes nothing
#   staged != landed, and staged matches
#     a HISTORICAL blob of that path
#     on origin/main                  -> STALE, REFUSE and name the commit that
#                                        superseded it
#   staged != landed, unmatched       -> NEW, allow
#
# The historical-blob walk is what separates "older copy" from "genuine new
# work". Both differ from the tip; only the stale one is a state main has
# already moved past. A check that refused everything differing from main would
# block all work and be disabled within a day, which is why the NEW case must
# pass cleanly and is bracketed in the self-test.
#
# FAILS LOUD. A stale path is named, with the superseding commit, and the exit
# status is non-zero. A silent skip would re-create the defect this prevents.
#
# It only READS. It never stages, resets, checks out, or commits, because the
# owner's staged content is the only copy of whatever they meant to restore.
#
# ---------------------------------------------------------------------------
# PRODUCING A COMMIT WHILE THE SHARED INDEX IS OCCUPIED (credit: hermit-det4)
#
# Do not touch the shared index at all. Two techniques, which compose:
#
#   (a) temp index + commit-tree -- build a tree in a private index file:
#         export GIT_INDEX_FILE=$(mktemp)
#         git read-tree HEAD
#         git update-index --add -- <your paths>
#         tree=$(git write-tree)
#         commit=$(git commit-tree "$tree" -p HEAD -m "msg")
#         git update-ref refs/heads/<branch> "$commit"
#         unset GIT_INDEX_FILE
#
#   (b) coordinator-provided wrkslots checkout -- its own index by construction:
#         ask the coordinator for worktrees/slots/<slot>
#         git -C worktrees/slots/<slot> cherry-pick <your commit>
#         # push that single commit and verify with ls-remote
#
# (b) additionally sidesteps a rewritten local main, because the commit reaches
# the remote on its own rather than depending on local main's state.
# ---------------------------------------------------------------------------
#
# Usage:
#   staged-freshness.sh [--root DIR] [--base REF] [PATH...]
# With no PATH, every staged path is checked.
# Exit: 0 all fresh (or no-op), 1 at least one stale path, 2 usage/environment error.

set -uo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT="${ROOT:-$(cd -- "$SCRIPT_DIR/../.." && pwd)}"
BASE="origin/main"
PATHS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?--root needs a value}"; shift 2 ;;
    --base) BASE="${2:?--base needs a value}"; shift 2 ;;
    -h|--help) sed -n '2,60p' "$0"; exit 0 ;;
    *) PATHS+=("$1"); shift ;;
  esac
done

cd "$ROOT" || { echo "staged-freshness: cannot cd $ROOT" >&2; exit 2; }

if ! git rev-parse --verify --quiet "$BASE" >/dev/null; then
  echo "staged-freshness: base ref '$BASE' does not resolve; fetch it first" >&2
  exit 2
fi

if [ "${#PATHS[@]}" -eq 0 ]; then
  mapfile -t PATHS < <(git diff --cached --name-only)
fi

[ "${#PATHS[@]}" -eq 0 ] && { echo "staged-freshness: nothing staged; nothing to check"; exit 0; }

stale=0 new=0 noop=0 added=0
declare -a STALE_REPORT=()

for p in "${PATHS[@]}"; do
  staged=$(git rev-parse ":$p" 2>/dev/null) || { echo "  SKIP     $p (not in the index)"; continue; }
  landed=$(git rev-parse "$BASE:$p" 2>/dev/null)

  if [ -z "$landed" ]; then
    # Absent from the base: a genuinely new file cannot revert anything.
    echo "  NEW-FILE $p"
    added=$((added + 1))
    continue
  fi

  if [ "$staged" = "$landed" ]; then
    echo "  NOOP     $p (staged blob == $BASE)"
    noop=$((noop + 1))
    continue
  fi

  # Does the staged blob match a version this path ALREADY HAD on the base? If
  # so the base has moved past it and committing it would walk that back.
  superseded_by=""
  while read -r sha; do
    [ -z "$sha" ] && continue
    hist=$(git rev-parse "$sha:$p" 2>/dev/null) || continue
    if [ "$hist" = "$staged" ]; then
      # The commit that CHANGED the path after this blob is the superseder.
      superseded_by=$(git log --format=%H "$sha..$BASE" -- "$p" | tail -1)
      break
    fi
  done < <(git log --format=%H "$BASE" -- "$p")

  if [ -n "$superseded_by" ]; then
    subject=$(git log -1 --format=%s "$superseded_by" 2>/dev/null)
    STALE_REPORT+=("  STALE    $p
             staged blob ${staged:0:12} is an OLDER copy already on $BASE
             superseded by ${superseded_by:0:12}  $subject
             committing it would revert that change")
    stale=$((stale + 1))
  else
    echo "  NEW      $p (differs from $BASE, matches no earlier version -- genuine work)"
    new=$((new + 1))
  fi
done

if [ "$stale" -gt 0 ]; then
  echo
  echo "staged-freshness: REFUSED -- $stale stale path(s) would revert landed work:" >&2
  for r in "${STALE_REPORT[@]}"; do echo "$r" >&2; done
  echo >&2
  echo "Do NOT reset or checkout these: the staged copy may be the only record of" >&2
  echo "what the owner meant to restore. Leave them staged and report them." >&2
  exit 1
fi

echo "staged-freshness: OK -- new=$new new-file=$added noop=$noop stale=0"
exit 0
