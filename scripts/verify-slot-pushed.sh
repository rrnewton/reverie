#!/bin/bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# verify-slot-pushed.sh — authoritative "is every slot's committed work on origin?"
# check, verified via `git ls-remote` (with-proxy). This is the structural
# guardrail behind push-on-recycle: run it periodically (cron) to catch drift
# outside the recycle path, or against one slot as a standalone pre-recycle check.
#
# A product child is AT RISK when it is on a branch whose HEAD is not durable on
# origin: origin lacks the branch, or origin's tip does not contain HEAD (local
# has commits missing from the remote). Uncommitted changes are also flagged.
# Detached checkouts parked at a pinned gitlink are safe (nothing to push).
#
# Exit status: 0 = all clear, 1 = at-risk work found (unpushed/uncommitted),
# 2 = usage error. Suitable as a cron gate or a pre-recycle assertion.
#
# Usage:
#   scripts/verify-slot-pushed.sh                 # sweep every slot (report + exit code)
#   scripts/verify-slot-pushed.sh --slot sabre    # one slot (pre-recycle check)
#   scripts/verify-slot-pushed.sh --quiet         # only print at-risk rows
set -uo pipefail

ONE_SLOT=""
QUIET=0
while (($#)); do
    case "$1" in
        --slot) ONE_SLOT=${2:?need SLOT}; shift ;;
        --quiet) QUIET=1 ;;
        -h | --help) sed -n '3,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "verify-slot-pushed.sh: unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

# Locate the dev-hermit root from cwd first, then from the script's own location
# (so cron/absolute-path invocations work regardless of cwd).
find_root() {
    local start dir
    for start in "$(pwd)" "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; do
        dir=$start
        while [[ $dir != / ]]; do
            if [[ -f $dir/.gitmodules && -d $dir/hermit && -d $dir/reverie && -d $dir/liteinst2 ]]; then
                echo "$dir"; return 0
            fi
            dir=$(dirname "$dir")
        done
    done
    echo "verify-slot-pushed.sh: could not locate dev-hermit root" >&2; exit 2
}
ROOT=$(find_root)
WT="$ROOT/worktrees/slots"
[[ -d $WT ]] || { echo "no worktrees/slots/ dir"; exit 0; }

# Is this worktree's HEAD durable on origin? echoes "safe" or "RISK:<reason>".
child_state() {
    local d=$1
    [[ -d $d/.git || -f $d/.git ]] || { echo "absent"; return; }
    local dirtyn; dirtyn=$(git -C "$d" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
    local branch; branch=$(git -C "$d" symbolic-ref --quiet --short HEAD 2>/dev/null || echo "")
    if [[ -z $branch ]]; then
        # Detached: parked at a pinned gitlink. Only uncommitted work is at risk.
        [[ $dirtyn != 0 ]] && { echo "RISK:detached with $dirtyn uncommitted change(s)"; return; }
        echo "safe:detached"; return
    fi
    local lhead; lhead=$(git -C "$d" rev-parse HEAD 2>/dev/null)
    local risk=""
    # Fast path: HEAD already on origin/main (no unique commits) -> durable.
    if git -C "$d" merge-base --is-ancestor "$lhead" origin/main 2>/dev/null; then
        :  # every commit is on origin's mainline
    else
        local rsha; rsha=$(with-proxy git -C "$d" ls-remote origin "refs/heads/$branch" 2>/dev/null | cut -f1)
        if [[ -z $rsha ]]; then
            risk="branch '$branch' not on origin"
        elif [[ $rsha != "$lhead" ]]; then
            if git -C "$d" merge-base --is-ancestor "$lhead" "$rsha" 2>/dev/null; then
                :  # origin strictly ahead; HEAD durable
            else
                risk="HEAD ${lhead:0:12} not on origin/$branch (remote ${rsha:0:12})"
            fi
        fi
    fi
    [[ $dirtyn != 0 ]] && risk="${risk:+$risk; }$dirtyn uncommitted change(s)"
    [[ -n $risk ]] && echo "RISK:$risk|$branch" || echo "safe:$branch"
}

if [[ -n $ONE_SLOT ]]; then
    SLOTS=("$WT/$ONE_SLOT")
    [[ -d ${SLOTS[0]} ]] || { echo "slot '$ONE_SLOT' not found under $WT" >&2; exit 2; }
else
    mapfile -t SLOTS < <(find "$WT" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort)
fi

risk_count=0
printf '%-22s %-10s %-8s %s\n' SLOT CHILD STATE DETAIL
for slot_dir in "${SLOTS[@]}"; do
    slot=$(basename "$slot_dir")
    for c in hermit reverie liteinst2 rs; do
        d="$slot_dir/$c"
        [[ -d $d/.git || -f $d/.git ]] || continue
        st=$(child_state "$d")
        if [[ $st == RISK:* ]]; then
            risk_count=$((risk_count+1))
            detail=${st#RISK:}
            printf '%-22s %-10s %-8s %s\n' "$slot" "$c" "AT-RISK" "${detail%%|*}"
        elif ((QUIET == 0)); then
            printf '%-22s %-10s %-8s %s\n' "$slot" "$c" "ok" "${st#safe:}"
        fi
    done
done

echo "-----"
if ((risk_count == 0)); then
    echo "✓ all committed work durable on origin (0 at-risk)"
    exit 0
else
    echo "✗ $risk_count child worktree(s) AT RISK — push before recycling."
    echo "  fix: scripts/release-worktree.rs --slot <slot> --push   (push-then-remove)"
    exit 1
fi
