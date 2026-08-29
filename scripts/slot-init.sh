#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.

set -euo pipefail

# Historical nested layout (one slot per agent):
#   worktrees/slots/<slot>/hermit   -> Hermit worktree  (from the hermit/ primary)
#   worktrees/slots/<slot>/reverie  -> Reverie worktree (from the reverie/ primary)
#   worktrees/slots/<slot>/liteinst2 -> LiteInst2 worktree (from the liteinst2/ primary)
#
function usage {
    cat <<'EOF'
Usage: ./scripts/slot-init.sh SLOT [PRODUCT] [START_POINT]

This legacy entrypoint no longer creates worktrees. wrkslots is the lifecycle
interface and records ownership, time-to-live, process evidence, and recovery.

  worktrees/slots/<slot>/hermit   Hermit worktree  (from the hermit/ primary)
  worktrees/slots/<slot>/reverie  Reverie worktree (from the reverie/ primary)
  worktrees/slots/<slot>/liteinst2 LiteInst2 worktree (from the liteinst2/ primary)

  SLOT         Named token ([a-z][a-z0-9-]*, e.g. kvm) or slotNN (e.g. slot01).
  PRODUCT      hermit | reverie | liteinst2 | both | all
               (default: all; both means Hermit + Reverie).
  START_POINT  Commit or branch to base the detached worktree on
               (default: the primary checkout's current HEAD).

Ask the coordinator to create the slot. A coordinator-delegated wrapper may use:
  ci-hub/bin/wrkslots create SLOT --slot-type agent --coordinator-authorized \
    --agent AGENT --task TASK --purpose TEXT --owner-pid PID \
    --coordinator-pid PID --repo hermit=hermit --branch hermit=BRANCH

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

printf '%s\n' \
    "slot-init: REFUSED -- this legacy helper cannot create slot '$SLOT'." \
    "state: REFUSED -- no worktree or lifecycle record was changed." \
    "remedy: ask the coordinator to create the $PRODUCT slot through ci-hub/bin/wrkslots; include the requested start point there if one is required." >&2
exit 3
