#!/usr/bin/env bash
# The disk tool must validate one exact registry snapshot before classifying or
# pruning anything. A malformed status is an unreadable authority, not an
# unregistered slot that may enter a pruning tier.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d "${TMPDIR:-/tmp}/worktree-gc-registry-test.XXXXXX")
trap 'rm -rf -- "$root"' EXIT

mkdir -p "$root/scripts" "$root/ci-hub/lib" "$root/hermit" "$root/reverie" \
  "$root/liteinst2" "$root/worktrees/slots"
touch "$root/.gitmodules"
cp "$script_dir/worktree-gc.sh" "$root/scripts/"
cp "$script_dir/allocate-worktree.rs" "$root/scripts/"
cp "$script_dir/check-worktree-registry.rs" "$root/scripts/"
cp "$script_dir/worktree_registry.rs" "$root/scripts/"
cp "$script_dir/agent_snapshot.rs" "$root/scripts/"
cp "$script_dir/../ci-hub/lib/worktree_registry.py" "$root/ci-hub/lib/"

cat >"$root/worktree-state.json" <<'JSON'
{
  "version": 3,
  "updated": "2026-08-28T00:00:00Z",
  "slots": {}
}
JSON
(cd "$root" && ./scripts/worktree-gc.sh) >"$root/valid.out" 2>&1
grep -Fq 'TOTAL du:' "$root/valid.out"

cat >"$root/worktree-state.json" <<'JSON'
{
  "version": 3,
  "updated": "2026-08-28T00:00:00Z",
  "slots": {
    "slot01": {
      "agents": [{"name": "agent-01", "read_only": false, "task": "task-01"}],
      "hermit_path": "worktrees/slots/slot01/hermit",
      "hermit_branch": "detached",
      "reverie_path": "worktrees/slots/slot01/reverie",
      "reverie_branch": "-",
      "liteinst2_path": "worktrees/slots/slot01/liteinst2",
      "liteinst2_branch": "-"
    }
  }
}
JSON
if (cd "$root" && ./scripts/worktree-gc.sh --prune-incremental) \
    >"$root/invalid.out" 2>&1; then
  echo "worktree-gc accepted a registry row without status" >&2
  exit 1
fi
grep -Fq 'status' "$root/invalid.out" \
  || { echo "worktree-gc refusal did not name status" >&2; exit 1; }
grep -Fq 'refusing to classify or prune' "$root/invalid.out" \
  || { echo "worktree-gc did not refuse before pruning" >&2; exit 1; }
if grep -Eq '^[[:space:]]+rm[[:space:]]' "$root/invalid.out"; then
  echo "worktree-gc printed a prune action after registry refusal" >&2
  exit 1
fi

echo 'worktree-gc-registry-test: PASS (valid empty registry reported; missing status refused before every pruning tier)'
