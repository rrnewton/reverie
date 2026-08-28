#!/bin/bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# worktree-gc.sh — enforce the worktree artifact budget policy.
#
# A single heavy backend lane holds hundreds of GB of Rust build output
# (target/), so worktrees/ grows fast. This tool measures per-slot and total
# footprint, reports against a cap + per-slot budget, and prunes regenerable
# build artifacts in a strict safety-ordered escalation.
# See ai_docs/worktree-artifact-budget-policy.md.
#
# ACCOUNTING UNIT (the whole point — task align_worktree_gc_with):
#   This tool (du -sk) and scripts/allocate-worktree.rs (du -sb) BOTH measure in
#   APPARENT / PRE-COMPRESSION bytes. Plain du does NOT see btrfs zstd, so both
#   overstate real on-disk use by ~3.9x here (measured 2026-08-03: worktrees
#   du=644 GB apparent vs compsize disk=163 GB real). They therefore agree on
#   unit — the cap below is an APPARENT-du figure directly comparable to the
#   allocator's advisory, NOT a real-compsize figure. compsize (when available)
#   is printed only as the real-disk reference; it never gates the cap.
#
# SAFE BY DEFAULT: prints a report and changes nothing unless a prune/reclaim
# action flag is given. It NEVER discards unrecoverable work:
#   * a BUSY slot (any cargo/rustc/cc1/ld/make cwd'd under it) is skipped by all tiers
#   * tiers that remove full target/ or reclaim a slot require the worktree to be
#     CLEAN (no uncommitted changes) AND PUSHED (0 commits ahead of upstream)
#   * tier 1 only ever removes target/*/incremental/, which is never source or
#     commit data — cargo regenerates it on the next build
#
# Prune tiers (strict safety order):
#   1  incremental   target/*/incremental/ in any non-busy slot            (--prune-incremental)
#   2  idle-target   whole target/ of released/parked, clean, pushed slots (--prune-idle-targets)
#   3  reclaim       released, clean, pushed slots -> release-worktree      (--reclaim-released)
#
# Usage:
#   scripts/worktree-gc.sh                          # report only (default)
#   scripts/worktree-gc.sh --prune-incremental      # tier 1 (always-safe)
#   scripts/worktree-gc.sh --enforce                # escalate tiers until under cap
#   scripts/worktree-gc.sh --enforce --cap-gb 200 --slot-budget-gb 60
#
# Options:
#   --prune-incremental     remove target/*/incremental/ in non-busy slots
#   --prune-idle-targets    remove whole target/ of released+clean+pushed+idle slots
#   --reclaim-released      release-worktree --clean released+clean+pushed slots
#   --enforce               if total du > cap, run tiers 1..3 in order until under cap
#   --cap-gb N              global APPARENT-du cap in GB (default: derived, read
#                           from DEFAULT_DISK_CAP_GB in allocate-worktree.rs = 1425)
#   --slot-budget-gb N      per-slot APPARENT-du budget in GB, flagged in report
#                           (default 95 = derived per-tree figure; see below)
#   --include-active        allow tier-1 incremental prune in ACTIVE (registered) slots
#                           too (still skips BUSY slots). Off by default.
#   -h, --help              this help
set -uo pipefail

PRUNE_INCR=0
PRUNE_IDLE=0
RECLAIM=0
ENFORCE=0
INCLUDE_ACTIVE=0
# CAP_GB / SLOT_BUDGET_GB are DERIVED, not hand-picked, and are filled in AFTER
# the root is located (a stale hand-picked constant is exactly the 200-GB drift
# this task fixes). The cap's single source of truth is DEFAULT_DISK_CAP_GB in
# scripts/allocate-worktree.rs, read at runtime so the two tools cannot diverge.
#
# DERIVATION (recorded beside the value, same discipline as the allocator; ALL
# figures are APPARENT du / pre-compression — see ACCOUNTING UNIT above):
#   cap         = 95 GB/tree x 12 active x 1.25 headroom = 1425 GB apparent
#   slot budget = 95 GB/tree  (mean of the 5 heaviest fully-built trees measured
#                 2026-08-03: 54/66/87/110/152 GB ~= 94, taken as 95 for margin)
# Measured 2026-08-03 on a devserver: worktrees du-sb = 643.5 GB apparent; compsize
# disk = 163 GB real (ratio ~3.9:1); 16 active / 0 parked; fs free = 2330 GB.
# Empty = "use derived default"; --cap-gb / --slot-budget-gb override.
CAP_GB=""
SLOT_BUDGET_GB=""

while (($#)); do
    case "$1" in
        --prune-incremental) PRUNE_INCR=1 ;;
        --prune-idle-targets) PRUNE_IDLE=1 ;;
        --reclaim-released) RECLAIM=1 ;;
        --enforce) ENFORCE=1 ;;
        --cap-gb) CAP_GB=${2:?need N}; shift ;;
        --slot-budget-gb) SLOT_BUDGET_GB=${2:?need N}; shift ;;
        --include-active) INCLUDE_ACTIVE=1 ;;
        -h | --help) sed -n '/^# worktree-gc\.sh —/,/^#   -h, --help/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "worktree-gc.sh: unknown argument: $1" >&2; exit 64 ;;
    esac
    shift
done

# --- Locate dev-hermit root (.gitmodules + hermit/ + reverie/ + liteinst2/). ---
find_root() {
    local dir; dir=$(pwd)
    while [[ $dir != / ]]; do
        if [[ -f $dir/.gitmodules && -d $dir/hermit && -d $dir/reverie && -d $dir/liteinst2 ]]; then
            echo "$dir"; return 0
        fi
        dir=$(dirname "$dir")
    done
    echo "worktree-gc.sh: could not locate dev-hermit root" >&2; exit 1
}
ROOT=$(find_root)
WT="$ROOT/worktrees/slots"
STATE="$ROOT/worktree-state.json"
[[ -d $WT ]] || { echo "no worktrees/slots/ dir at $WT"; exit 0; }

# Fill DERIVED defaults now that ROOT is known. Single-source the cap from the
# allocator's constant so the two tools can never drift (see DERIVATION above).
derive_cap_gb() {
    local rs="$ROOT/scripts/allocate-worktree.rs" v
    v=$(grep -oP 'const\s+DEFAULT_DISK_CAP_GB:\s*u64\s*=\s*\K[0-9]+' "$rs" 2>/dev/null | head -1)
    [[ -n $v ]] && echo "$v" || echo 1425   # fallback = value derived 2026-08-03
}
CAP_SRC="derived from allocate-worktree.rs"
if [[ -z $CAP_GB ]]; then CAP_GB=$(derive_cap_gb); else CAP_SRC="--cap-gb override"; fi
: "${SLOT_BUDGET_GB:=95}"
# Idle threshold for the orphan-candidate flag (same heuristic as the allocator).
LANGUISH_HOURS=${HERMIT_WORKTREE_LANGUISH_HOURS:-24}

# du of a path in GB (integer), 0 if absent.
du_gb() {
    [[ -e $1 ]] || { echo 0; return; }
    local kb; kb=$(du -sk "$1" 2>/dev/null | cut -f1); echo $(( (kb + 524288) / 1048576 ))
}

# A slot is BUSY if any build process is cwd'd under it.
slot_busy() {
    local slot_dir=$1 pid cwd
    for pid in $(pgrep -x 'cargo|rustc|cc1|cc1plus|ld|make|ld.lld|lld' 2>/dev/null); do
        cwd=$(readlink "/proc/$pid/cwd" 2>/dev/null) || continue
        [[ $cwd == "$slot_dir"* ]] && return 0
    done
    return 1
}

# Read and validate one exact registry snapshot before any pruning decision.
# Re-running the Rust checker once per slot made a 178-row report invoke the
# authority 178 times; keeping the accepted statuses in memory is both faster
# and prevents one report from mixing different registry revisions.
declare -A REGISTRY_STATUS
if [[ -f $STATE ]]; then
    if ! registry_rows=$(python3 - "$STATE" "$ROOT" <<'PY'
import sys
from pathlib import Path
state, root = Path(sys.argv[1]), Path(sys.argv[2])
sys.path.insert(0, str(root / "ci-hub" / "lib"))
from worktree_registry import RegistrySchemaError, read_registry
try:
    document = read_registry(
        state, checker=root / "scripts" / "check-worktree-registry.rs"
    )
except RegistrySchemaError as error:
    print(f"worktree-gc: {error}", file=sys.stderr)
    raise SystemExit(2)
for name, row in document["slots"].items():
    print(f"{name}\t{row['status']}")
PY
    ); then
        echo "worktree-gc: COULD NOT READ worktree registry; refusing to classify or prune" >&2
        exit 2
    fi
    while IFS=$'\t' read -r registry_slot registry_status; do
        [[ -n $registry_slot ]] || continue
        REGISTRY_STATUS["$registry_slot"]=$registry_status
    done <<<"$registry_rows"
fi

# Registered status of a slot ("active"/"released"/"" if unregistered).
slot_state() {
    printf '%s\n' "${REGISTRY_STATUS[$1]:-}"
}

# git worktree clean (no uncommitted changes)?
wt_clean() {
    [[ -d $1 ]] || return 0
    [[ -z $(git -C "$1" status --porcelain 2>/dev/null) ]]
}

# git worktree fully pushed (upstream exists AND 0 commits ahead)?
wt_pushed() {
    [[ -d $1 ]] || return 0
    git -C "$1" rev-parse --abbrev-ref '@{upstream}' >/dev/null 2>&1 || return 1
    [[ $(git -C "$1" rev-list --count '@{upstream}..HEAD' 2>/dev/null || echo 1) == 0 ]]
}

# All product children of a slot clean + pushed?
slot_safe_to_drop() {
    local slot_dir=$1 c
    for c in hermit reverie liteinst2; do
        [[ -d $slot_dir/$c ]] || continue
        wt_clean "$slot_dir/$c" || return 1
        wt_pushed "$slot_dir/$c" || return 1
    done
    return 0
}

# Idle hours = age of the newest SOURCE file under a slot (ignoring build/VCS
# churn) — the real "when was this slot last worked" signal. This is the only
# available orphan proxy: worktree-state.json carries NO agent-liveness / PID /
# heartbeat, and its 'updated' field only moves when allocate-worktree runs, so
# an ACTIVE slot whose owning agent has DIED looks identical to a live one and is
# fully protected from every prune tier. Idle-past-threshold + active = the
# worktree form of orphaned work: capacity held with no signal. Report-only —
# GC never auto-reclaims an active slot. Returns -1 if no source file is found.
slot_idle_hours() {
    local dir=$1 newest now
    newest=$(find "$dir" \( -name target -o -name .git -o -name node_modules \) -prune \
        -o -type f -printf '%T@\n' 2>/dev/null | cut -d. -f1 | sort -rn | head -1)
    [[ -z $newest ]] && { echo -1; return; }
    now=$(date +%s)
    echo $(( (now - newest) / 3600 ))
}

# ------------------------------------------------------------------ report ----
mapfile -t SLOTS < <(find "$WT" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort)
echo "worktree-gc: root=$ROOT  cap=${CAP_GB}GB ($CAP_SRC)  per-slot-budget=${SLOT_BUDGET_GB}GB  [all figures APPARENT du / pre-compression]"
if command -v compsize >/dev/null 2>&1; then
    real=$(compsize "$WT" 2>/dev/null | awk '/^TOTAL/{print $3}')
    [[ -n $real ]] && echo "real on-disk (compsize): $real  (du overstates via zstd+reflink)"
fi
printf '%-24s %8s %8s %5s %5s %5s %7s  %s\n' SLOT du_GB incr_GB busy clean pushed idle_h status
total=0
orphan_gb=0; orphan_list=""
declare -A SLOT_GB SLOT_BUSY SLOT_STATE
for slot_dir in "${SLOTS[@]}"; do
    slot=$(basename "$slot_dir")
    gb=$(du_gb "$slot_dir"); total=$((total+gb))
    incr=0
    while IFS= read -r d; do incr=$((incr + $(du_gb "$d"))); done \
        < <(find "$slot_dir" -type d -path '*/target/*/incremental' 2>/dev/null)
    busy=no; slot_busy "$slot_dir" && busy=YES
    st=$(slot_state "$slot"); [[ -z $st ]] && st="unreg"
    SLOT_GB[$slot]=$gb; SLOT_BUSY[$slot]=$busy; SLOT_STATE[$slot]=$st
    flag=""; (( gb > SLOT_BUDGET_GB )) && flag=" <<OVER-BUDGET"
    wt_all_clean=yes
    for c in hermit reverie liteinst2; do wt_clean "$slot_dir/$c" || wt_all_clean=no; done
    ps=no; slot_safe_to_drop "$slot_dir" && ps=yes
    idle=$(slot_idle_hours "$slot_dir"); idle_disp=$idle; [[ $idle == -1 ]] && idle_disp="?"
    # Orphan candidate: registered active but idle past the threshold and NOT busy
    # (a live agent building would show busy or a fresh source mtime). Report-only.
    if [[ $st == active && $busy == no && $idle -ge $LANGUISH_HOURS ]]; then
        flag+=" <<ORPHAN? active+idle ${idle}h"
        orphan_gb=$((orphan_gb+gb)); orphan_list+="${slot}(${idle}h,${gb}GB) "
    fi
    printf '%-24s %8s %8s %5s %5s %5s %7s  %s%s\n' \
        "$slot" "$gb" "$incr" "$busy" "$wt_all_clean" "$ps" "$idle_disp" "$st" "$flag"
done
echo "-----"
echo "TOTAL du: ${total}GB apparent  (cap ${CAP_GB}GB apparent)$( ((total>CAP_GB)) && echo '  *** OVER CAP ***')"
# Advisory registry-consistency check: call the canonical verifier rather than
# re-deriving branch/ownership state here. Never gates GC; a FAIL points at the
# single-writer reconciler.
if [[ -x "$ROOT/scripts/check-worktree-registry.rs" ]]; then
    if ! "$ROOT/scripts/check-worktree-registry.rs" --root "$ROOT" >/dev/null 2>&1; then
        echo "NOTE: worktree registry has drift (recorded vs actual branch). Run"
        echo "      scripts/allocate-worktree.rs --repair to reconcile (advisory)."
    fi
fi
if [[ -n $orphan_list ]]; then
    echo "ORPHAN CANDIDATES: ${orphan_gb}GB apparent in active+idle(>${LANGUISH_HOURS}h)+not-busy slots: ${orphan_list}"
    echo "  These hold capacity with NO liveness signal (state has no PID/heartbeat)."
    echo "  Likely a LARGER reclaim than any threshold change — but GC will NOT touch"
    echo "  active slots. Coordinator: confirm the owning agent is dead, then land/park"
    echo "  its work and run scripts/release-worktree.rs --slot <slot>."
fi
echo

# ---------------------------------------------------------------- actions ----
# Decide which tiers to run. --enforce escalates only while over cap.
run_incr=$PRUNE_INCR; run_idle=$PRUNE_IDLE; run_reclaim=$RECLAIM
if (( ENFORCE )); then
    (( total > CAP_GB )) && { run_incr=1; run_idle=1; run_reclaim=1; } \
        || echo "enforce: total ${total}GB already within cap ${CAP_GB}GB; no pruning needed."
fi

reclaimed=0
recompute_total() { total=0; for s in "${!SLOT_GB[@]}"; do total=$((total+${SLOT_GB[$s]})); done; }

# Tier 1: incremental caches (always regenerable).
if (( run_incr )); then
    echo "== tier 1: prune target/*/incremental/ =="
    for slot_dir in "${SLOTS[@]}"; do
        slot=$(basename "$slot_dir")
        (( ENFORCE )) && (( total <= CAP_GB )) && break
        if [[ ${SLOT_BUSY[$slot]} == YES ]]; then echo "  skip $slot (busy)"; continue; fi
        if [[ ${SLOT_STATE[$slot]} == active && $INCLUDE_ACTIVE == 0 ]]; then
            echo "  skip $slot (active; pass --include-active to prune its incremental)"; continue
        fi
        while IFS= read -r d; do
            [[ -d $d ]] || continue
            g=$(du_gb "$d")
            echo "  rm  $d  (${g}GB, regenerable)"
            rm -rf "$d" && { SLOT_GB[$slot]=$(( ${SLOT_GB[$slot]} - g )); reclaimed=$((reclaimed+g)); }
        done < <(find "$slot_dir" -type d -path '*/target/*/incremental' 2>/dev/null)
        recompute_total
    done
    echo
fi

# Tier 2: whole target/ of idle (released + clean + pushed + not busy) slots.
if (( run_idle )); then
    echo "== tier 2: prune whole target/ of released+clean+pushed idle slots =="
    for slot_dir in "${SLOTS[@]}"; do
        slot=$(basename "$slot_dir")
        (( ENFORCE )) && (( total <= CAP_GB )) && break
        [[ ${SLOT_BUSY[$slot]} == YES ]] && { echo "  skip $slot (busy)"; continue; }
        [[ ${SLOT_STATE[$slot]} == released ]] || { echo "  skip $slot (not released; state=${SLOT_STATE[$slot]})"; continue; }
        slot_safe_to_drop "$slot_dir" || { echo "  skip $slot (dirty or unpushed — refuse to touch)"; continue; }
        while IFS= read -r d; do
            [[ -d $d ]] || continue
            g=$(du_gb "$d")
            echo "  rm  $d  (${g}GB)"
            rm -rf "$d" && { SLOT_GB[$slot]=$(( ${SLOT_GB[$slot]} - g )); reclaimed=$((reclaimed+g)); }
        done < <(find "$slot_dir" -mindepth 2 -maxdepth 2 -type d -name target 2>/dev/null)
        recompute_total
    done
    echo
fi

# Tier 3: reclaim released+clean+pushed slots via release-worktree (push-then-remove).
if (( run_reclaim )); then
    echo "== tier 3: reclaim released+clean+pushed slots =="
    for slot_dir in "${SLOTS[@]}"; do
        slot=$(basename "$slot_dir")
        (( ENFORCE )) && (( total <= CAP_GB )) && break
        [[ ${SLOT_BUSY[$slot]} == YES ]] && { echo "  skip $slot (busy)"; continue; }
        [[ ${SLOT_STATE[$slot]} == released ]] || { echo "  skip $slot (not released)"; continue; }
        slot_safe_to_drop "$slot_dir" || { echo "  skip $slot (dirty or unpushed)"; continue; }
        echo "  reclaim $slot -> release-worktree.rs --slot $slot --clean"
        if "$ROOT/scripts/release-worktree.rs" --slot "$slot" --clean; then
            reclaimed=$((reclaimed + ${SLOT_GB[$slot]})); SLOT_GB[$slot]=0
        fi
        recompute_total
    done
    echo
fi

if (( PRUNE_INCR || PRUNE_IDLE || RECLAIM || ENFORCE )); then
    recompute_total
    echo "Done: reclaimed ~${reclaimed}GB du.  new TOTAL du: ${total}GB apparent (cap ${CAP_GB}GB apparent)."
    if (( ENFORCE )) && (( total > CAP_GB )); then
        # Everything reclaimable (released+clean+pushed) is exhausted; the residual
        # bulk is ACTIVE/busy work GC must not touch. Say so explicitly instead of
        # implying failure — enforcement did all it safely could, and churning
        # further over an all-active workspace is futile.
        active_gb=0
        for s in "${!SLOT_GB[@]}"; do
            [[ ${SLOT_STATE[$s]} == released ]] || active_gb=$((active_gb+${SLOT_GB[$s]}))
        done
        echo "enforce: still ${total}GB > ${CAP_GB}GB, but ~${active_gb}GB is in ACTIVE/busy"
        echo "         slots GC cannot safely reclaim. This is a COORDINATOR decision"
        echo "         (land/park active work; see any ORPHAN CANDIDATES above), not a"
        echo "         GC failure. No active or unrecoverable work was removed."
    fi
else
    echo "Report only. Re-run with --prune-incremental (safe) or --enforce to act."
fi
