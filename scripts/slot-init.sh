#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT_DIR

# Canonical nested layout (one slot per agent):
#   worktrees/slots/<slot>/hermit   -> Hermit worktree  (from the hermit/ primary)
#   worktrees/slots/<slot>/reverie  -> Reverie worktree (from the reverie/ primary)
#   worktrees/slots/<slot>/liteinst2 -> LiteInst2 worktree (from the liteinst2/ primary)
#
# PREFER scripts/allocate-worktree.rs: it is registry-aware (updates
# worktree-state.json + worktrees/ACTIVE.md, enforces one-owner-per-slot and
# one-slot-per-agent). This shell helper only creates detached worktrees and
# does NOT touch the registry; use it for quick manual scaffolding.
readonly WORKTREES="$ROOT_DIR/worktrees/slots"

function usage {
    cat <<'EOF'
Usage: ./scripts/slot-init.sh SLOT [PRODUCT] [START_POINT]

Create one or more nested product worktrees for a slot:

  worktrees/slots/<slot>/hermit   Hermit worktree  (from the hermit/ primary)
  worktrees/slots/<slot>/reverie  Reverie worktree (from the reverie/ primary)
  worktrees/slots/<slot>/liteinst2 LiteInst2 worktree (from the liteinst2/ primary)

  SLOT         Named token ([a-z][a-z0-9-]*, e.g. kvm) or slotNN (e.g. slot01).
  PRODUCT      hermit | reverie | liteinst2 | both | all
               (default: all; both means Hermit + Reverie).
  START_POINT  Commit or branch to base the detached worktree on
               (default: the primary checkout's current HEAD).

The worktree is created detached at START_POINT. Create a task-specific feature
branch inside the slot before editing, and register it in worktrees/ACTIVE.md.

PREFER scripts/allocate-worktree.rs (registry-aware) for real agent work.

Example:
  ./scripts/slot-init.sh slot01
  ./scripts/slot-init.sh kvm hermit
  ./scripts/slot-init.sh dbi reverie main
EOF
}

if (($# < 1)) || (($# > 3)) || [[ ${1:-} == "-h" || ${1:-} == "--help" ]]; then
    usage
    [[ $# -ge 1 && ${1:-} != "-h" && ${1:-} != "--help" ]] && exit 2
    exit 0
fi

readonly SLOT=${1:-}
if [[ ! $SLOT =~ ^(slot[0-9]{2,}|[a-z][a-z0-9-]*)$ ]]; then
    echo "Invalid SLOT: '$SLOT' (expected slotNN or a named token like kvm)" >&2
    usage >&2
    exit 2
fi

readonly PRODUCT=${2:-all}
case "$PRODUCT" in
    hermit | reverie | liteinst2 | both | all) ;;
    *)
        echo "Invalid PRODUCT: '$PRODUCT' (expected hermit, reverie, liteinst2, both, or all)" >&2
        usage >&2
        exit 2
        ;;
esac

readonly START_POINT_OVERRIDE=${3:-}

# add_worktree PRIMARY_DIR PRODUCT_SUBDIR
#   Create worktrees/slots/<slot>/<product> as a detached worktree of the primary repo.
function add_worktree {
    local primary_dir=$1
    local product_subdir=$2
    local slot_dir="$WORKTREES/$SLOT/$product_subdir"

    if [[ ! -d $primary_dir ]]; then
        echo "Missing primary checkout: $primary_dir" >&2
        exit 1
    fi
    if [[ -e $slot_dir ]]; then
        echo "Refusing to overwrite occupied slot: $slot_dir" >&2
        exit 1
    fi

    local start_point=$START_POINT_OVERRIDE
    if [[ -z $start_point ]]; then
        start_point=$(git -C "$primary_dir" rev-parse --abbrev-ref HEAD)
    fi

    mkdir -p "$WORKTREES/$SLOT"
    git -C "$primary_dir" worktree add --detach "$slot_dir" "$start_point"

    if ! git -C "$slot_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "Worktree creation failed: $slot_dir" >&2
        exit 1
    fi
    echo "Initialized $slot_dir (detached at $start_point)"
}

if [[ $PRODUCT == hermit || $PRODUCT == both || $PRODUCT == all ]]; then
    add_worktree "$ROOT_DIR/hermit" "hermit"
fi
if [[ $PRODUCT == reverie || $PRODUCT == both || $PRODUCT == all ]]; then
    add_worktree "$ROOT_DIR/reverie" "reverie"
fi
if [[ $PRODUCT == liteinst2 || $PRODUCT == all ]]; then
    add_worktree "$ROOT_DIR/liteinst2" "liteinst2"
fi
