#!/bin/bash
# Build a per-case, time-cut kernel repo for one benchmark case.
#
#   ccbench_tree.sh <full-mirror.git> <case-dir> <crash-sha> <cutoff-unix>
#
# The repo shares objects with the full mirror (alternates) but exposes ONLY:
#   refs/heads/crash                  -> the exact crash-kernel commit
#   refs/heads/<release/upstream br>  -> that branch's newest commit <= cutoff
#   refs/tags/*                       -> tags created <= cutoff
# Personal/scratch branches (which can carry bot-drafted fixes) are never exposed,
# and there is no remote, so `git fetch` has nowhere to go. Commits after the cutoff
# stay unreachable from every ref; the leak audit flags whole-object-store enumeration
# (cat-file --batch-all-objects, fsck --unreachable, rev-list --all --objects).
set -euo pipefail
MIRROR=$1; CASE=$2; SHA=$3; CUTOFF=$4
REPO=$CASE/linux.git
M="git --git-dir=$MIRROR"
FULL=$($M rev-parse --verify "$SHA^{commit}")
rm -rf "$REPO"
git init -q --bare "$REPO"
G="git --git-dir=$REPO"
echo "$(readlink -f "$MIRROR")/objects" > "$REPO/objects/info/alternates"
{
  echo "update refs/heads/crash $FULL"
  for b in $($M for-each-ref --format='%(refname:short)' refs/heads |
             grep -E '^(v[0-9]+\.[0-9]+-fb|master|main|linus-upstream|net-next-upstream)$'); do
    tip=$($M rev-list -1 --before="@$CUTOFF" "refs/heads/$b" 2>/dev/null || true)
    [ -n "$tip" ] && echo "update refs/heads/$b $tip"
  done
  $M for-each-ref --format='%(creatordate:unix) %(refname) %(objectname)' refs/tags |
    awk -v c="$CUTOFF" '$1 <= c {print "update " $2 " " $3}'
} | $G update-ref --stdin
$G symbolic-ref HEAD refs/heads/crash
$G config core.logAllRefUpdates false
echo "repo=$REPO crash=$FULL cutoff=$CUTOFF branches=$($G for-each-ref refs/heads | wc -l) tags=$($G for-each-ref refs/tags | wc -l)"
