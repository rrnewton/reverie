#!/usr/bin/env bash
set -euo pipefail

# Safe probes must be pure: print usage and exit 0 before any git/filesystem work.
for arg in "$@"; do
    case "$arg" in
        -h | --help)
            cat <<'EOF'
check-parent-gitmodules.sh — verify .gitmodules matches the pinned parent submodule policy

USAGE:
  scripts/check-parent-gitmodules.sh            check four pinned gitlinks and agent-utils symlink
  scripts/check-parent-gitmodules.sh -h|--help  show this help and exit (no side effects)

Exit 0 when policy holds, 1 on any mismatch. Takes no other arguments.
EOF
            exit 0
            ;;
    esac
done

root=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")/.." rev-parse --show-toplevel)
cd "$root"

expected=(
    "hermit|hermit|https://github.com/rrnewton/hermit.git|checkout"
    "reverie|reverie|https://github.com/rrnewton/reverie.git|checkout"
    "liteinst2|liteinst2|https://github.com/rrnewton/liteinst2.git|checkout"
    "hermit_test_ledger|hermit_test_ledger|https://github.com/rrnewton/hermit_test_ledger.git|checkout"
)

mapfile -t configured < <(git config -f .gitmodules --get-regexp '^submodule\..*\.path$')
if [[ ${#configured[@]} -ne ${#expected[@]} ]]; then
    echo "ERROR: .gitmodules must contain exactly ${#expected[@]} parent submodules." >&2
    exit 1
fi

for spec in "${expected[@]}"; do
    IFS='|' read -r name path url expected_update <<<"$spec"
    actual_path=$(git config -f .gitmodules --get "submodule.$name.path" || true)
    actual_url=$(git config -f .gitmodules --get "submodule.$name.url" || true)
    update=$(git config -f .gitmodules --get "submodule.$name.update" || true)
    mode=$(git ls-files --stage -- "$path" | awk 'NR == 1 { print $1 }')

    [[ "$actual_path" == "$path" ]] || {
        echo "ERROR: submodule $name path is '$actual_path', expected '$path'." >&2
        exit 1
    }
    [[ "$actual_url" == "$url" ]] || {
        echo "ERROR: submodule $name URL is '$actual_url', expected '$url'." >&2
        exit 1
    }
    [[ "$update" == "$expected_update" ]] || {
        echo "ERROR: submodule $name must set update = $expected_update." >&2
        exit 1
    }
    [[ "$mode" == 160000 ]] || {
        echo "ERROR: $path is not recorded as a gitlink." >&2
        exit 1
    }
done

agent_utils_mode=$(git ls-files --stage -- agent-utils | awk 'NR == 1 { print $1 }')
agent_utils_target=$(git cat-file -p :agent-utils 2>/dev/null || true)

[[ "$agent_utils_mode" == 120000 ]] || {
    echo "ERROR: agent-utils must be recorded as a symlink, not a parent gitlink." >&2
    exit 1
}
[[ "$agent_utils_target" == "hermit/agent-utils" ]] || {
    echo "ERROR: agent-utils must point exactly to hermit/agent-utils, got '$agent_utils_target'." >&2
    exit 1
}
[[ -L agent-utils ]] || {
    echo "ERROR: the agent-utils working-tree path is not a symlink." >&2
    exit 1
}
[[ "$(readlink agent-utils)" == "hermit/agent-utils" ]] || {
    echo "ERROR: the agent-utils working-tree symlink does not point to hermit/agent-utils." >&2
    exit 1
}

echo "Parent submodule policy is valid: four pinned gitlinks plus agent-utils -> hermit/agent-utils."
