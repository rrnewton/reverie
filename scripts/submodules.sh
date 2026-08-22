#!/usr/bin/env bash
#
# Safe submodule init/update that NEVER detaches an attached primary checkout.
#
# THE CAVEAT THIS WRAPPER EXISTS FOR
# ----------------------------------
# The parent product submodules are declared `update = checkout` in .gitmodules,
# so the raw
#
#     git submodule update --init --recursive
#
# checks each one out at the parent's *pinned gitlink SHA in DETACHED HEAD*. Run
# in a primary checkout that was sitting on `main` (or, for liteinst2, its
# feature branch), that silently detaches it and violates the Primary Checkout
# Invariant ("both primaries always on latest main"). `make checkout-all` and
# `make init-hermit` both call that raw command.
#
# This wrapper does the equivalent init/update work but keeps every primary
# ATTACHED:
#   * missing submodule      -> init it, then attach to its default branch
#   * clean + detached       -> reattach to its default branch (only when HEAD is
#                               already reachable from origin/main, so no unique
#                               commit is ever orphaned)
#   * clean + on a branch    -> LEFT AS-IS (hermit/reverie expected on main;
#                               liteinst2 may legitimately be on a feature
#                               branch, which is preserved), fast-forwarded to
#                               its upstream unless --no-pull
#   * dirty                  -> WARNED and SKIPPED, never reset/cleaned/stashed
#
# It is deliberately NON-recursive for product initialization (heavy/optional
# nested submodules such as e9patch and SaBRe are provisioned on demand by
# scripts/checkout-optional-submodules.rs). With --with-agent-utils it makes the
# one explicit exception required by the parent symlink: initialize hermit
# first, then hermit's pinned agent-utils submodule. It NEVER edits .gitmodules.
#
# Usage:
#   scripts/submodules.sh [--no-pull] [--with-agent-utils] [--products a,b,c]
#
#   --no-pull            init/reattach only; do not fast-forward to upstream.
#   --with-agent-utils   also initialize hermit/agent-utils; the parent path
#                        agent-utils is a symlink to that one checkout.
#   --products LIST      comma-separated subset of hermit,reverie,liteinst2.
#
# Networked git runs through `with-proxy` when available; set
# SUBMODULES_DISABLE_PROXY=1 (or WITH_PROXY=<cmd>) to override.

set -euo pipefail

root=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")/.." rev-parse --show-toplevel)
cd "$root"

# Products that must stay attached, with the branch to reattach to when detached
# or freshly initialized. liteinst2's default is main, but an existing attached
# liteinst2 branch is preserved rather than forced.
default_products=(hermit reverie liteinst2)
declare -A default_branch=([hermit]=main [reverie]=main [liteinst2]=main)

no_pull=0
with_agent_utils=0
products=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-pull) no_pull=1 ;;
        --with-agent-utils) with_agent_utils=1 ;;
        --products)
            shift
            [[ $# -gt 0 ]] || { echo "ERROR: --products needs an argument." >&2; exit 2; }
            IFS=',' read -r -a products <<<"$1"
            ;;
        -h|--help)
            sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "ERROR: unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

if [[ ${#products[@]} -eq 0 ]]; then
    products=("${default_products[@]}")
fi

# The only agent-utils checkout lives below hermit. If a caller selected a
# narrower product set, initialize/attach hermit before trying to initialize its
# nested submodule. Keep the ordering explicit so a fresh clone never follows a
# dangling parent agent-utils symlink and accidentally asks Git about the parent.
if [[ $with_agent_utils -eq 1 ]]; then
    has_hermit=0
    for product in "${products[@]}"; do
        if [[ "$product" == hermit ]]; then
            has_hermit=1
        fi
    done
    [[ $has_hermit -eq 1 ]] || products=(hermit "${products[@]}")
fi

# Resolve the network proxy once (with-proxy in Meta environments).
proxy=()
if [[ -z "${SUBMODULES_DISABLE_PROXY:-}" ]]; then
    proxy_cmd=$(command -v "${WITH_PROXY:-with-proxy}" 2>/dev/null || true)
    [[ -n "$proxy_cmd" ]] && proxy=("$proxy_cmd")
fi

warnings=0
declare -A summary=()

# is_ancestor <repo> <rev> <maybe-descendant-ref>: succeed when <rev> is an
# ancestor of (or equal to) the ref, i.e. reattaching cannot orphan a commit.
is_ancestor() {
    git -C "$1" merge-base --is-ancestor "$2" "$3" 2>/dev/null
}

# ff_to_upstream <repo> <branch>: fast-forward-only pull of <branch> from its
# upstream. Never rewrites history; a non-ff divergence is reported, not forced.
ff_to_upstream() {
    local repo=$1 branch=$2
    [[ $no_pull -eq 1 ]] && return 0
    local upstream
    upstream=$(git -C "$repo" rev-parse --abbrev-ref --symbolic-full-name "$branch@{upstream}" 2>/dev/null || true)
    [[ -n "$upstream" ]] || return 0
    local remote=${upstream%%/*}
    local remote_branch=${upstream#*/}
    "${proxy[@]}" git -C "$repo" fetch --quiet "$remote" "$remote_branch" || {
        echo "  WARNING: $repo: fetch $upstream failed; left at current HEAD." >&2
        warnings=$((warnings + 1))
        return 0
    }
    if ! git -C "$repo" merge --ff-only --quiet "$upstream" 2>/dev/null; then
        echo "  NOTE: $repo: $branch is not a fast-forward of $upstream; left unchanged (no rebase/merge)." >&2
    fi
}

# attach_default <repo> <product>: put a clean, detached (or fresh) checkout back
# on its default branch without discarding reachable history.
attach_default() {
    local repo=$1 product=$2
    local branch=${default_branch[$product]:-main}
    if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
        git -C "$repo" checkout --quiet "$branch"
    else
        "${proxy[@]}" git -C "$repo" fetch --quiet origin "$branch" || true
        git -C "$repo" checkout --quiet -b "$branch" --track "origin/$branch"
    fi
    ff_to_upstream "$repo" "$branch"
}

process_product() {
    local product=$1
    local repo="$root/$product"

    # 1. Missing -> init this one submodule (non-recursive), then attach.
    if [[ ! -e "$repo/.git" ]]; then
        echo "== $product: not initialized; init + attach =="
        "${proxy[@]}" git submodule update --init -- "$product"
        attach_default "$repo" "$product"
        summary[$product]="initialized -> $(git -C "$repo" symbolic-ref --short -q HEAD || echo DETACHED) @ $(git -C "$repo" rev-parse --short HEAD)"
        return
    fi

    # 2. Dirty -> preserve, never touch.
    if [[ -n "$(git -C "$repo" status --porcelain)" ]]; then
        echo "== $product: DIRTY; preserved and skipped ==" >&2
        summary[$product]="DIRTY (skipped) @ $(git -C "$repo" rev-parse --short HEAD)"
        warnings=$((warnings + 1))
        return
    fi

    local branch
    branch=$(git -C "$repo" symbolic-ref --short -q HEAD || true)

    if [[ -n "$branch" ]]; then
        # 3. Already attached: leave the branch as-is, ff-update it.
        local expected=${default_branch[$product]:-main}
        if [[ "$product" != "liteinst2" && "$branch" != "$expected" ]]; then
            echo "  WARNING: $product is on '$branch', not '$expected' (Primary Checkout Invariant); left as-is." >&2
            warnings=$((warnings + 1))
        fi
        ff_to_upstream "$repo" "$branch"
        summary[$product]="attached ($branch) @ $(git -C "$repo" rev-parse --short HEAD)"
        return
    fi

    # 4. Detached but clean: reattach only when nothing would be orphaned.
    local head
    head="$(git -C "$repo" rev-parse HEAD)"
    "${proxy[@]}" git -C "$repo" fetch --quiet origin main || true
    if is_ancestor "$repo" "$head" origin/main || git -C "$repo" branch --contains "$head" --format='%(refname)' | grep -q .; then
        attach_default "$repo" "$product"
        summary[$product]="reattached ($(git -C "$repo" symbolic-ref --short -q HEAD || echo DETACHED)) @ $(git -C "$repo" rev-parse --short HEAD)"
    else
        echo "  WARNING: $product is DETACHED at $head with commits not reachable from origin/main or any branch; left as-is to avoid orphaning work." >&2
        summary[$product]="DETACHED (preserved) @ $(git -C "$repo" rev-parse --short "$head")"
        warnings=$((warnings + 1))
    fi
}

for product in "${products[@]}"; do
    case "$product" in
        hermit|reverie|liteinst2) process_product "$product" ;;
        *) echo "ERROR: unknown product '$product' (expected hermit|reverie|liteinst2)." >&2; exit 2 ;;
    esac
done

if [[ $with_agent_utils -eq 1 ]]; then
    if [[ ! -e "$root/hermit/agent-utils/.git" ]]; then
        echo "== hermit/agent-utils: on-demand init =="
        "${proxy[@]}" git -C "$root/hermit" submodule update --init --checkout -- agent-utils
    fi
    if [[ ! -L "$root/agent-utils" ]] || [[ "$(readlink "$root/agent-utils")" != "hermit/agent-utils" ]]; then
        echo "ERROR: parent agent-utils must be the tracked symlink hermit/agent-utils." >&2
        exit 1
    fi
    summary[agent-utils]="hermit/agent-utils @ $(git -C "$root/agent-utils" rev-parse --short HEAD 2>/dev/null || echo missing)"
fi

echo
echo "Submodule attach summary:"
for product in "${products[@]}"; do
    printf '  %-11s %s\n' "$product" "${summary[$product]:-unchanged}"
done
[[ $with_agent_utils -eq 1 ]] && printf '  %-11s %s\n' "agent-utils" "${summary[agent-utils]:-unchanged}"

if [[ $warnings -gt 0 ]]; then
    echo
    echo "Completed with $warnings warning(s); dirty/divergent checkouts were preserved untouched." >&2
    exit 1
fi
echo
echo "All requested primaries are initialized and attached."
