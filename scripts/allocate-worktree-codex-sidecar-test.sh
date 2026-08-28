#!/usr/bin/env bash
# Regression for re-adopting an existing Codex sentinel to publish its owner sidecar.
#
# Inert: the allocator, registry, sentinel verifier, and product repositories all
# live in one temporary fixture. The live registry, systemd user manager, and
# worktrees are never consulted or changed.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
root="$(mktemp -d "${TMPDIR:-/tmp}/allocate-codex-sidecar.XXXXXX")"
trap 'rm -rf "$root"' EXIT

mkdir -p "$root/scripts" "$root/hermit" "$root/reverie" "$root/liteinst2" \
  "$root/worktrees/slots/codex-fixture" "$root/ignored/ci-hub"
touch "$root/.gitmodules"
cp "${ALLOC_SRC:-$script_dir/allocate-worktree.rs}" "$root/scripts/allocate-worktree.rs"
cp "$script_dir/worktree_registry.rs" "$root/scripts/"
chmod +x "$root/scripts/allocate-worktree.rs"

# The allocator asks this verifier to prove the exact already-recorded sentinel.
# Echoing the same lease as live isolates the regression to the mutation branch
# after verification, which is where the pre-fix panic occurred.
cat >"$root/scripts/codex-slot-sentinel.rs" <<'PY'
#!/usr/bin/env python3
import json
import sys

if len(sys.argv) != 4 or sys.argv[1:3] != ["verify", "--lease-json"]:
    raise SystemExit("fixture supports only verify --lease-json")
print(json.dumps({"state": "live", "lease": json.loads(sys.argv[3])}))
PY
chmod +x "$root/scripts/codex-slot-sentinel.rs"

git -C "$root" init -q -b main
git -C "$root" config user.email test@example.invalid
git -C "$root" config user.name test
git -C "$root" add .gitmodules
git -C "$root" commit -q -m seed
for product in hermit reverie liteinst2; do
  git -C "$root/$product" init -q -b main
  git -C "$root/$product" config user.email test@example.invalid
  git -C "$root/$product" config user.name test
  touch "$root/$product/seed"
  git -C "$root/$product" add seed
  git -C "$root/$product" commit -q -m seed
done
git -C "$root/hermit" worktree add -q --detach \
  "$root/worktrees/slots/codex-fixture/hermit" HEAD

fixture_pid=$$
fixture_start_ticks="$(awk '{print $22}' "/proc/$fixture_pid/stat")"
python3 - "$root/worktree-state.json" "$root/worktrees/slots/codex-fixture" \
  "$fixture_pid" "$fixture_start_ticks" <<'PY'
import json
import sys

path, worktree, pid, start_ticks = sys.argv[1:]
lease = {
    "schema_version": 1,
    "source": "codex-systemd-sentinel-v1",
    "slot": "codex-fixture",
    "generation": "fixture-generation",
    "nonce": "fixture-nonce",
    "unit": "dev-hermit-slot-codex-fixture-fixture.service",
    "working_directory": worktree,
    "main_pid": int(pid),
    "main_pid_starttime": int(start_ticks),
    "cgroup_path": "/fixture/codex-fixture",
    "invocation_id": "fixture-invocation",
}
state = {
    "version": 3,
    "updated": "2026-08-28T00:00:00Z",
    "slots": {
        "codex-fixture": {
            "status": "active",
            "agents": [{
                "name": "codex-fixture",
                "task": "codex-sidecar-regression",
                "read_only": False,
            }],
            "hermit_path": "worktrees/slots/codex-fixture/hermit",
            "reverie_path": "worktrees/slots/codex-fixture/reverie",
            "liteinst2_path": "worktrees/slots/codex-fixture/liteinst2",
            "hermit_branch": "detached",
            "reverie_branch": "-",
            "liteinst2_branch": "-",
            "coordinator_lease": lease,
        }
    }
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(state, output, indent=2)
    output.write("\n")
PY

if ! (cd "$root" && ./scripts/allocate-worktree.rs \
  --agent codex-fixture \
  --slot codex-fixture \
  --product hermit \
  --task codex-sidecar-regression \
  --purpose "prove existing sentinel sidecar adoption" \
  --codex-systemd-sentinel) >"$root/allocator.out" 2>&1; then
  sed 's/^/  /' "$root/allocator.out"
  echo "FAIL: existing Codex sentinel re-adoption was refused"
  exit 1
fi

python3 - "$root/worktree-state.json" "$fixture_pid" "$fixture_start_ticks" <<'PY'
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))["slots"]["codex-fixture"]
pid, start_ticks = map(int, sys.argv[2:])
sidecar = row.get("owner_sidecar")
assert isinstance(sidecar, dict), "Codex re-adoption did not publish owner_sidecar"
assert sidecar["schema_version"] == 1
assert sidecar["source"] == "codex-systemd-sentinel-v1"
assert sidecar["slot"] == "codex-fixture"
assert sidecar["agent"] == "codex-fixture"
assert sidecar["task"] == "codex-sidecar-regression"
assert sidecar["supervisor_pid"] == pid
assert sidecar["start_ticks"] == start_ticks
assert sidecar["cgroup_path"] == "/fixture/codex-fixture"
assert sidecar["lease"]["schema_version"] == 1
assert sidecar["lease"]["resource"] == "slot/codex-fixture"
assert sidecar["lease"]["generation"] == sidecar["generation"]
assert sidecar["lease"]["ttl_seconds"] > 0
assert sidecar["lease"]["expires_at"] == sidecar["lease"]["renewed_at"] + sidecar["lease"]["ttl_seconds"]
assert sidecar["lease"]["state"] == "active"
assert row["coordinator_lease"]["generation"] == "fixture-generation"
assert row["task"] == "codex-sidecar-regression"
assert row["purpose"] == "prove existing sentinel sidecar adoption"
print("PASS: existing Codex sentinel re-adoption published its exact owner sidecar")
PY

if ! grep -Fq \
  '| codex-fixture | codex-fixture | detached | - | - | codex-sidecar-regression | active | no |' \
  "$root/worktrees/ACTIVE.md"; then
  echo "FAIL: ACTIVE.md omitted the sentinel-backed owner task"
  sed 's/^/  /' "$root/worktrees/ACTIVE.md"
  exit 1
fi
