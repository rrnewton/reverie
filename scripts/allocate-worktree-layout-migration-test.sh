#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(mktemp -d /tmp/allocate-layout-migration-test.XXXXXX)
trap 'rm -rf -- "$root"' EXIT

mkdir -p "$root/hermit" "$root/reverie" "$root/liteinst2" "$root/worktrees"
: >"$root/.gitmodules"
cat >"$root/worktree-state.json" <<'JSON'
{
  "version": 3,
  "slots": {
    "slot01": {
      "agents": [{"name": "fixture", "read_only": false}],
      "task": "fixture",
      "purpose": "layout migration fixture",
      "status": "released",
      "hermit_branch": "-",
      "reverie_branch": "-",
      "liteinst2_branch": "-",
      "hermit_path": "worktrees/slot01/hermit",
      "reverie_path": "worktrees/slot01/reverie",
      "liteinst2_path": "worktrees/slot01/liteinst2"
    }
  }
}
JSON
cat >"$root/worktrees/ACTIVE.md" <<'MD'
# Fixture registry

Human text before the managed block.
<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->
<!-- END worktree-state -->
Human text after the managed block.
MD

set +e
(cd "$root" && "$script_dir/allocate-worktree.rs" --migrate-layout --dry-run) \
  >"$root/dry-run.out" 2>&1
dry_rc=$?
set -e
[[ $dry_rc -eq 1 ]] || { cat "$root/dry-run.out" >&2; exit 1; }
jq -e '.slots.slot01.hermit_path == "worktrees/slot01/hermit"' \
  "$root/worktree-state.json" >/dev/null
grep -Fq '3 path cell(s) would change. No files written.' "$root/dry-run.out"

(cd "$root" && "$script_dir/allocate-worktree.rs" --migrate-layout) \
  >"$root/migrate.out" 2>&1
jq -e '.slots.slot01.hermit_path == "worktrees/slots/slot01/hermit" and
       .slots.slot01.reverie_path == "worktrees/slots/slot01/reverie" and
       .slots.slot01.liteinst2_path == "worktrees/slots/slot01/liteinst2"' \
  "$root/worktree-state.json" >/dev/null
grep -Fq 'Human text before the managed block.' "$root/worktrees/ACTIVE.md"
grep -Fq 'Human text after the managed block.' "$root/worktrees/ACTIVE.md"

(cd "$root" && "$script_dir/allocate-worktree.rs" --migrate-layout) \
  >"$root/idempotent.out" 2>&1
grep -Fq 'rewrote 0 path cell(s)' "$root/idempotent.out"

python3 - "$root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["slot01"]["hermit_path"] = "worktrees/slots/slot01/./hermit"
path.write_text(json.dumps(state, indent=2) + "\n")
PY
before=$(sha256sum "$root/worktree-state.json")
set +e
(cd "$root" && "$script_dir/allocate-worktree.rs" --migrate-layout) \
  >"$root/refuse.out" 2>&1
refuse_rc=$?
set -e
[[ $refuse_rc -ne 0 ]] || { cat "$root/refuse.out" >&2; exit 1; }
[[ $(sha256sum "$root/worktree-state.json") == "$before" ]] || {
  echo 'layout migration changed a registry containing a noncanonical alias' >&2
  exit 1
}
grep -Fq 'neither the exact legacy nor canonical spelling' "$root/refuse.out"

echo 'allocate-worktree-layout-migration-test: PASS'
