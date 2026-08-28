#!/usr/bin/env bash
# Bracket the safety contract of release-worktree.rs --clean using only
# disposable local repositories. In particular, an initialized submodule makes
# plain `git worktree remove` refuse even when both the parent and submodule are
# clean; the release script must get past that Git implementation detail only
# after its own dirty, durability, and registry checks pass.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
suite_root="$(mktemp -d "${TMPDIR:-/tmp}/release-worktree-test.XXXXXX")"
case "$suite_root" in
  "${TMPDIR:-/tmp}"/release-worktree-test.*) ;;
  *) echo "unsafe temporary test root: $suite_root" >&2; exit 1 ;;
esac
live_pid=
fence_pid=
locked_proc_dir=
fixture_count=0
cleanup() {
  if [[ -n ${locked_proc_dir:-} ]]; then
    chmod 700 "$locked_proc_dir" 2>/dev/null || true
  fi
  if [[ -n ${live_pid:-} ]] && kill -0 "$live_pid" 2>/dev/null; then
    kill "$live_pid"
    wait "$live_pid" 2>/dev/null || true
  fi
  if [[ -n ${fence_pid:-} ]] && kill -0 "$fence_pid" 2>/dev/null; then
    kill "$fence_pid"
    wait "$fence_pid" 2>/dev/null || true
  fi
  rm -rf -- "$suite_root"
}
trap cleanup EXIT

fail() {
  echo "release-worktree-test: FAIL: $*" >&2
  exit 1
}

# release-worktree uses `with-proxy git` for authoritative origin checks. The
# fixtures have file://-local origins, so shadow the fleet wrapper with a
# no-network adapter that preserves the exact git argv.
mkdir -p "$suite_root/bin"
RELEASE_TEST_REAL_GIT=$(command -v git)
export RELEASE_TEST_REAL_GIT
cat >"$suite_root/bin/with-proxy" <<'SH'
#!/usr/bin/env bash
if [[ -n ${RELEASE_TEST_RACE_TARGET:-} ]] && [[ " $* " == *' ls-remote '* ]]; then
  printf 'planted after the preliminary status\n' >"$RELEASE_TEST_RACE_TARGET"
fi
if [[ -n ${RELEASE_TEST_REASSIGN_STATE:-} ]] && [[ " $* " == *' ls-remote '* ]]; then
  cp "$RELEASE_TEST_REASSIGN_STATE" "$RELEASE_TEST_STATE_TARGET"
  cp "$RELEASE_TEST_REASSIGN_ACTIVE" "$RELEASE_TEST_ACTIVE_TARGET"
fi
if [[ -n ${RELEASE_TEST_LOCK_PATH:-} ]] && [[ " $* " == *' ls-remote '* ]]; then
  exec {lock_fd}>"$RELEASE_TEST_LOCK_PATH"
  if flock -n "$lock_fd"; then
    printf 'acquired\n' >"$RELEASE_TEST_LOCK_RESULT"
    flock -u "$lock_fd"
  else
    printf 'blocked\n' >"$RELEASE_TEST_LOCK_RESULT"
  fi
  exec {lock_fd}>&-
fi
exec "$@"
SH
cat >"$suite_root/bin/git" <<'SH'
#!/usr/bin/env bash
if [[ -n ${RELEASE_TEST_TRANSACTION_ADMIN:-} ]] \
    && [[ " $* " == *' worktree remove '* ]]; then
  if [[ -f "$RELEASE_TEST_TRANSACTION_ADMIN/release-worktree.in-progress" ]] \
      && [[ -d "$RELEASE_TEST_TRANSACTION_ADMIN/modules.release-worktree" ]] \
      && [[ ! -e "$RELEASE_TEST_TRANSACTION_ADMIN/modules" ]] \
      && [[ " $* " != *' --force '* ]]; then
    printf 'active-nonforce\n' >"$RELEASE_TEST_TRANSACTION_RESULT"
  else
    printf 'invalid\n' >"$RELEASE_TEST_TRANSACTION_RESULT"
  fi
fi
if [[ -n ${RELEASE_TEST_FORCE_ADMIN:-} ]] \
    && [[ " $* " == *' worktree remove '* ]]; then
  if [[ " $* " == *' --force '* ]] \
      && [[ ! -e "$RELEASE_TEST_FORCE_ADMIN/release-worktree.in-progress" ]] \
      && [[ ! -e "$RELEASE_TEST_FORCE_ADMIN/modules.release-worktree" ]]; then
    printf 'direct-force\n' >"$RELEASE_TEST_FORCE_RESULT"
  else
    printf 'invalid\n' >"$RELEASE_TEST_FORCE_RESULT"
  fi
fi
if [[ -n ${RELEASE_TEST_REMOVE_RACE_TARGET:-} ]] \
    && [[ " $* " == *' worktree remove '* ]]; then
  remove_path=${*: -1}
  printf 'planted at worktree remove invocation\n' \
    >"$remove_path/$RELEASE_TEST_REMOVE_RACE_TARGET"
fi
if [[ -n ${RELEASE_TEST_FENCE_PROCESS_TARGET:-} ]] \
    && [[ " $* " == *' worktree move '* ]] \
    && [[ ! -e $RELEASE_TEST_FENCE_PROCESS_PID ]]; then
  (cd "$RELEASE_TEST_FENCE_PROCESS_TARGET" && exec sleep 60) \
    </dev/null >"$RELEASE_TEST_FENCE_PROCESS_LOG" 2>&1 &
  process_pid=$!
  printf '%s\n' "$process_pid" >"$RELEASE_TEST_FENCE_PROCESS_PID"
  mkdir -p "$RELEASE_TEST_FENCE_PROC_ROOT/$process_pid/fd" \
    "$RELEASE_TEST_FENCE_PROC_ROOT/$process_pid/map_files"
  fenced_path=${*: -1}
  ln -s "$fenced_path" "$RELEASE_TEST_FENCE_PROC_ROOT/$process_pid/cwd"
  ln -s /bin/sleep "$RELEASE_TEST_FENCE_PROC_ROOT/$process_pid/exe"
  ln -s / "$RELEASE_TEST_FENCE_PROC_ROOT/$process_pid/root"
  : >"$RELEASE_TEST_FENCE_PROC_ROOT/$process_pid/maps"
fi
if [[ -n ${RELEASE_TEST_PODMAN_RACE_SOURCE:-} ]] \
    && [[ " $* " == *' worktree move '* ]]; then
  cp "$RELEASE_TEST_PODMAN_RACE_SOURCE" "$RELEASE_TEST_PODMAN_RACE_TARGET"
fi
if [[ -n ${RELEASE_TEST_CONTAINER_LOCK_PATH:-} ]] \
    && [[ " $* " == *' worktree move '* ]]; then
  exec {container_lock_fd}<>"$RELEASE_TEST_CONTAINER_LOCK_PATH"
  if flock -n "$container_lock_fd"; then
    printf 'acquired\n' >"$RELEASE_TEST_CONTAINER_LOCK_RESULT"
    flock -u "$container_lock_fd"
  else
    printf 'blocked\n' >"$RELEASE_TEST_CONTAINER_LOCK_RESULT"
  fi
  exec {container_lock_fd}>&-
fi
if [[ ${RELEASE_TEST_FORCE_SUBMODULE_MOVE_REFUSAL:-0} == 1 ]] \
    && [[ " $* " == *' worktree move '* ]]; then
  printf '%s\n' 'fatal: working trees containing submodules cannot be moved or removed' >&2
  exit 128
fi
if [[ ${RELEASE_TEST_NONSPECIFIC_MOVE_REFUSAL:-0} == 1 ]] \
    && [[ " $* " == *' worktree move '* ]]; then
  printf '%s\n' 'fatal: planted non-submodule move refusal' >&2
  exit 128
fi
if [[ ${RELEASE_TEST_FAKE_REPAIR_SUCCESS:-0} == 1 ]] \
    && [[ " $* " == *' worktree repair '* ]]; then
  printf 'zero-without-repair\n' >"$RELEASE_TEST_FAKE_REPAIR_RESULT"
  exit 0
fi
if [[ ${RELEASE_TEST_FAKE_REMOVE_SUCCESS:-0} == 1 ]] \
    && [[ " $* " == *' worktree remove '* ]]; then
  printf 'zero-without-removal\n' >"$RELEASE_TEST_FAKE_REMOVE_RESULT"
  exit 0
fi
exec "$RELEASE_TEST_REAL_GIT" "$@"
SH
cat >"$suite_root/bin/podman" <<'PY'
#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

state = Path(os.environ["RELEASE_TEST_PODMAN_STATE"])
args = sys.argv[1:]
if os.environ.get("RELEASE_TEST_PODMAN_FAIL") == "1":
    print("planted podman authority failure", file=sys.stderr)
    raise SystemExit(125)
if os.environ.get("RELEASE_TEST_PODMAN_MALFORMED") == "1":
    print("not-json")
    raise SystemExit(0)
containers = json.loads(state.read_text())
if args[:3] == ["ps", "-a", "--format"]:
    print(json.dumps(containers))
elif args[:2] == ["container", "inspect"] and len(args) == 3:
    if os.environ.get("RELEASE_TEST_PODMAN_INCOMPLETE") == "1":
        print("[]")
        raise SystemExit(0)
    container = next((row for row in containers if row["Id"] == args[2]), None)
    if container is None:
        raise SystemExit(1)
    print(json.dumps([{
        "Id": container["Id"],
        "Config": {"Labels": container.get("Labels")},
        "Mounts": container.get("Mounts", []),
    }]))
else:
    print(f"unsupported fake podman argv: {args}", file=sys.stderr)
    raise SystemExit(2)
PY
cat >"$suite_root/bin/slot-owner-authority.py" <<'PY'
#!/usr/bin/env python3
"""Fixture authority that takes the real shared registry lock."""

import fcntl
import json
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[1]
slot = sys.argv[sys.argv.index("--slot") + 1]
lock_path = root / "worktree-state.lock"
state_path = root / "worktree-state.json"
with lock_path.open("a+", encoding="utf-8") as lock:
    fcntl.flock(lock, fcntl.LOCK_SH)
    state = json.loads(state_path.read_text(encoding="utf-8"))
    row = state["slots"][slot]
    marker = os.environ.get("RELEASE_TEST_AUTHORITY_LOCK_MARKER")
    if marker:
        Path(marker).write_text("shared-acquired\n", encoding="utf-8")
    fcntl.flock(lock, fcntl.LOCK_UN)

sidecar = row["owner_sidecar"]
owner = next(agent for agent in row["agents"] if agent["read_only"] is False)
identity = {
    "source": sidecar["source"],
    "generation": sidecar["generation"],
    "supervisor_pid": sidecar["supervisor_pid"],
    "boot_id": sidecar["boot_id"],
    "start_ticks": sidecar["start_ticks"],
    "lease": sidecar.get("lease"),
}

if os.environ.get("RELEASE_TEST_AUTHORITY_RENEW_LEASE") == "1":
    with lock_path.open("a+", encoding="utf-8") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        current = json.loads(state_path.read_text(encoding="utf-8"))
        lease = current["slots"][slot]["owner_sidecar"]["lease"]
        lease["renewed_at"] += 1
        lease["expires_at"] += 1
        state_path.write_text(
            json.dumps(current, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        fcntl.flock(lock, fcntl.LOCK_UN)

authority_state = os.environ.get("RELEASE_TEST_OWNER_AUTHORITY_STATE", "dead")
payload = {
    "schema_version": 1,
    "state": authority_state,
    "reason_code": f"fixture-{authority_state}",
    "detail": f"fixture owner is {authority_state}",
    "slot": slot,
    "agent": owner["name"],
    "owner_identity": identity,
    "may_recover": authority_state == "dead",
}
print(json.dumps(payload, sort_keys=True))
raise SystemExit({"dead": 0, "alive": 1}.get(authority_state, 2))
PY
chmod +x "$suite_root/bin/with-proxy"
chmod +x "$suite_root/bin/git"
chmod +x "$suite_root/bin/podman"
chmod +x "$suite_root/bin/slot-owner-authority.py"
export PATH="$suite_root/bin:$PATH"

configure_repo() {
  local repo=$1
  git -C "$repo" config user.email release-worktree-test@example.invalid
  git -C "$repo" config user.name release-worktree-test
}

write_state() {
  local slot=$1 branch=$2 recorded_path=$3
  cat >"$fixture_root/worktree-state.json" <<JSON
{
  "version": 3,
  "updated": "2026-08-28T00:00:00Z",
  "slots": {
    "$slot": {
      "agents": [{
        "name": "fixture-$slot",
        "read_only": false,
        "task": "fixture-$slot",
        "tmux_pane_id": "%lease-$slot",
        "cgroup_path": "/agent.slice/fixture-$slot.scope"
      }],
      "allocated": "2026-08-28T00:00:00Z",
      "updated": "2026-08-28T00:00:00Z",
      "hermit_branch": "$branch",
      "hermit_path": "$recorded_path",
      "reverie_branch": "-",
      "reverie_path": "worktrees/slots/$slot/reverie",
      "liteinst2_branch": "-",
      "liteinst2_path": "worktrees/slots/$slot/liteinst2",
      "task": "fixture-$slot",
      "status": "active"
    }
  }
}
JSON
  cat >"$fixture_root/worktrees/ACTIVE.md" <<MD
# Fixture registry
<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->
| Slot | Agent | HermitBranch | ReverieBranch | LiteInst2Branch | Task | Status | ReadOnly |
| --- | --- | --- | --- | --- | --- | --- | --- |
| $slot | fixture-$slot | $branch | - | - | fixture-$slot | active | no |
<!-- END worktree-state -->
MD
}

set_owner_schema() {
  local agents_json=$1 active_agent=$2
  python3 - "$fixture_root/worktree-state.json" \
    "$fixture_root/worktrees/ACTIVE.md" "$agents_json" "$active_agent" <<'PY'
import json
import pathlib
import sys

state_path, active_path, agents_json, active_agent = sys.argv[1:]
state = json.loads(pathlib.Path(state_path).read_text())
slot = next(iter(state["slots"]))
state["slots"][slot]["agents"] = json.loads(agents_json)
pathlib.Path(state_path).write_text(json.dumps(state, indent=2) + "\n")

lines = pathlib.Path(active_path).read_text().splitlines()
for index, line in enumerate(lines):
    if line.startswith(f"| {slot} |"):
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        cells[1] = active_agent
        lines[index] = "| " + " | ".join(cells) + " |"
pathlib.Path(active_path).write_text("\n".join(lines) + "\n")
PY
}

# Set globals fixture_root, primary, target, keep, target_branch, and output.
make_fixture() {
  local name=$1
  fixture_count=$((fixture_count + 1))
  fixture_root="$suite_root/$name"
  primary="$fixture_root/hermit"
  target="$fixture_root/worktrees/slots/$name/hermit"
  keep="$fixture_root/worktrees/slots/keep-$name/hermit"
  target_branch="published-$name"
  output="$fixture_root/release.out"

  mkdir -p \
    "$fixture_root/bin" \
    "$fixture_root/remotes" \
    "$fixture_root/reverie" \
    "$fixture_root/liteinst2" \
    "$fixture_root/cgroup" \
    "$fixture_root/ignored/ci-hub" \
    "$fixture_root/proc" \
    "$fixture_root/scripts" \
    "$fixture_root/worktrees" \
    "$fixture_root/unrelated"
  : >"$fixture_root/.gitmodules"
  cp "$script_dir/check-worktree-registry.rs" "$fixture_root/scripts/"
  cp "$script_dir/worktree_registry.rs" "$fixture_root/scripts/"
  cp "$script_dir/agent-podman.rs" "$fixture_root/scripts/"
  cp "$script_dir/agent_snapshot.rs" "$fixture_root/scripts/"
  cp "$suite_root/bin/podman" "$fixture_root/bin/podman"
  cp "$suite_root/bin/slot-owner-authority.py" \
    "$fixture_root/scripts/slot-owner-authority.py"
  printf 'must survive\n' >"$fixture_root/unrelated/sentinel.txt"
  cat >"$fixture_root/ignored/ci-hub/agent-snapshot.json" <<JSON
{
  "schema_version": 1,
  "captured_at": $(date +%s),
  "agents": []
}
JSON
  : >"$fixture_root/tmux-panes"
  printf '[]\n' >"$fixture_root/podman.json"
  cat >"$fixture_root/ignored/ci-hub/agent-containers.json" <<'JSON'
{
  "schema_version": 1,
  "containers": {}
}
JSON

  local leaf_origin="$fixture_root/remotes/leaf.git"
  local leaf_seed="$fixture_root/leaf-seed"
  git init -q --bare "$leaf_origin"
  git -C "$leaf_origin" symbolic-ref HEAD refs/heads/main
  git init -q -b main "$leaf_seed"
  configure_repo "$leaf_seed"
  printf 'leaf published content\n' >"$leaf_seed/leaf.txt"
  git -C "$leaf_seed" add leaf.txt
  git -C "$leaf_seed" commit -q -m 'seed leaf origin'
  git -C "$leaf_seed" remote add origin "$leaf_origin"
  git -C "$leaf_seed" push -q -u origin main

  local nested_origin="$fixture_root/remotes/nested.git"
  local nested_seed="$fixture_root/nested-seed"
  git init -q --bare "$nested_origin"
  git -C "$nested_origin" symbolic-ref HEAD refs/heads/main
  git init -q -b main "$nested_seed"
  configure_repo "$nested_seed"
  printf 'nested published content\n' >"$nested_seed/nested.txt"
  git -C "$nested_seed" -c protocol.file.allow=always submodule add -q \
    "$leaf_origin" deps/leaf
  git -C "$nested_seed" add nested.txt .gitmodules deps/leaf
  git -C "$nested_seed" commit -q -m 'seed nested origin'
  git -C "$nested_seed" remote add origin "$nested_origin"
  git -C "$nested_seed" push -q -u origin main

  local parent_origin="$fixture_root/remotes/hermit.git"
  git init -q --bare "$parent_origin"
  git -C "$parent_origin" symbolic-ref HEAD refs/heads/main
  git init -q -b main "$primary"
  configure_repo "$primary"
  printf 'parent published content\n' >"$primary/parent.txt"
  git -C "$primary" -c protocol.file.allow=always submodule add -q \
    "$nested_origin" deps/nested
  git -C "$primary" add parent.txt .gitmodules deps/nested
  git -C "$primary" commit -q -m 'seed parent origin with submodule'
  git -C "$primary" remote add origin "$parent_origin"
  git -C "$primary" push -q -u origin main

  git -C "$primary" worktree add -q -b "$target_branch" "$target" origin/main
  configure_repo "$target"
  git -C "$target" commit -q --allow-empty -m 'publish target branch'
  git -C "$target" push -q -u origin "HEAD:refs/heads/$target_branch"
  git -C "$target" -c protocol.file.allow=always submodule update -q --init --recursive

  git -C "$primary" worktree add -q -b "keep-$name" "$keep" origin/main

  write_state "$name" "$target_branch" "worktrees/slots/$name/hermit"
}

plant_named_watchman_fd() {
  local flags=$1 executable=${2:-/usr/local/bin/watchman}
  local pid=424242
  mkdir -p "$fixture_root/proc/$pid/fd" \
    "$fixture_root/proc/$pid/fdinfo" \
    "$fixture_root/proc/$pid/map_files"
  printf 'watchman\n' >"$fixture_root/proc/$pid/comm"
  printf '/usr/local/bin/watchman\0--foreground\0' \
    >"$fixture_root/proc/$pid/cmdline"
  : >"$fixture_root/proc/$pid/environ"
  printf '%s\n' \
    '424242 (watchman) S 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 123456 0' \
    >"$fixture_root/proc/$pid/stat"
  ln -s "$executable" "$fixture_root/proc/$pid/exe"
  ln -s / "$fixture_root/proc/$pid/root"
  ln -s / "$fixture_root/proc/$pid/cwd"
  ln -s "$target" "$fixture_root/proc/$pid/fd/7"
  printf 'pos:\t0\nflags:\t%s\n' "$flags" \
    >"$fixture_root/proc/$pid/fdinfo/7"
  : >"$fixture_root/proc/$pid/maps"
}

deinitialize_fixture_submodules() {
  git -C "$target" submodule deinit -q --all
  test "$(git -C "$target" submodule status --recursive | wc -l)" -eq 1 \
    || fail "deinitialized fixture did not retain exactly one top-level gitlink: $target"
  git -C "$target" submodule status --recursive | grep -Eq '^-' \
    || fail "fixture submodule remains initialized: $target"
  test -z "$(find "$target/deps/nested" -mindepth 1 -maxdepth 1 -print -quit)" \
    || fail "deinitialized fixture retained nested worktree content: $target/deps/nested"
}

run_release() {
  local slot=$1
  shift
  (cd "$fixture_root" && env \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    "$@" "$script_dir/release-worktree.rs" --slot "$slot" --clean) \
    >"$output" 2>&1
}

run_release_force() {
  local slot=$1
  (cd "$fixture_root" && env \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    "$script_dir/release-worktree.rs" \
    --slot "$slot" --clean --force) >"$output" 2>&1
}

add_versioned_owner_lease() {
  local slot=$1 now generation
  now=$(date +%s)
  generation=11111111-1111-1111-1111-111111111111:424242:123456
  python3 - "$fixture_root/worktree-state.json" "$slot" "$now" "$generation" <<'PY'
import json
import sys

path, slot, now, generation = sys.argv[1:]
now = int(now)
state = json.load(open(path, encoding="utf-8"))
row = state["slots"][slot]
owner = row["agents"][0]
row["owner_sidecar"] = {
    "schema_version": 1,
    "source": "orc-tmux-pane-supervisor-v1",
    "slot": slot,
    "agent": owner["name"],
    "task": row["task"],
    "recorded_at": "2026-08-11T00:00:00Z",
    "generation": generation,
    "supervisor_pid": 424242,
    "boot_id": "11111111-1111-1111-1111-111111111111",
    "start_ticks": 123456,
    "tmux_pane_id": owner["tmux_pane_id"],
    "cgroup_path": owner["cgroup_path"],
    "lease": {
        "schema_version": 1,
        "resource": f"slot/{slot}",
        "generation": generation,
        "issued_at": now,
        "renewed_at": now,
        "ttl_seconds": 86400,
        "expires_at": now + 86400,
        "state": "active",
        "last_transition_actor": owner["name"],
        "automatic_handoff": None,
    },
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(state, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

plant_expired_owner_lease() {
  local slot=$1 now
  now=$(date +%s)
  python3 - "$fixture_root/worktree-state.json" "$slot" "$now" <<'PY'
import json
import sys

path, slot, now = sys.argv[1:]
now = int(now)
state = json.load(open(path, encoding="utf-8"))
row = state["slots"][slot]
owner = next(agent for agent in row["agents"] if agent["read_only"] is False)
boot_id = "11111111-1111-1111-1111-111111111111"
pid = 999999999
start_ticks = 123456
generation = f"{boot_id}:{pid}:{start_ticks}"
row["owner_sidecar"] = {
    "schema_version": 1,
    "source": "orc-tmux-pane-supervisor-v1",
    "slot": slot,
    "agent": owner["name"],
    "task": row["task"],
    "recorded_at": "2026-08-11T00:00:00Z",
    "generation": generation,
    "supervisor_pid": pid,
    "boot_id": boot_id,
    "start_ticks": start_ticks,
    "tmux_pane_id": owner["tmux_pane_id"],
    "cgroup_path": owner["cgroup_path"],
    "lease": {
        "schema_version": 1,
        "resource": f"slot/{slot}",
        "generation": generation,
        "issued_at": now - 120,
        "renewed_at": now - 120,
        "ttl_seconds": 30,
        "expires_at": now - 90,
        "state": "active",
        "last_transition_actor": owner["name"],
        "automatic_handoff": None,
    },
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(state, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(generation)
PY
}

plant_versioned_live_owner() {
  local slot=$1 cwd=$2 owner
  owner="fixture-$slot"
  add_versioned_owner_lease "$slot"
  printf '%s\t%%lease-%s\t424242\n' "$owner" "$slot" >"$fixture_root/tmux-panes"
  mkdir -p \
    "$fixture_root/proc/424242/fd" \
    "$fixture_root/proc/424242/map_files" \
    "$fixture_root/cgroup/agent.slice/$owner.scope"
  ln -s "$cwd" "$fixture_root/proc/424242/cwd"
  ln -s /bin/sleep "$fixture_root/proc/424242/exe"
  ln -s / "$fixture_root/proc/424242/root"
  printf 'DG_AGENT_NAME=%s\0' "$owner" >"$fixture_root/proc/424242/environ"
  printf '0::/agent.slice/%s.scope\n' "$owner" >"$fixture_root/proc/424242/cgroup"
  : >"$fixture_root/proc/424242/maps"
  printf '424242\n' >"$fixture_root/cgroup/agent.slice/$owner.scope/cgroup.procs"
  printf 'populated 1\nfrozen 0\n' \
    >"$fixture_root/cgroup/agent.slice/$owner.scope/cgroup.events"
}

run_self_release() {
  local slot=$1 owner
  owner="fixture-$slot"
  (cd "$fixture_root" && env \
    DG_AGENT_NAME="$owner" \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    "$script_dir/release-worktree.rs" --slot "$slot" --clean \
      --request-release --actor "$owner") >"$output" 2>&1
}

run_stale_clear() {
  local slot=$1 generation
  shift
  generation=$(python3 - "$fixture_root/worktree-state.json" "$slot" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
print(state["slots"][sys.argv[2]]["owner_sidecar"]["generation"])
PY
  )
  (cd "$fixture_root" && timeout "${RELEASE_TEST_STALE_CLEAR_TIMEOUT:-60}s" env \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    "$@" \
    "$script_dir/release-worktree.rs" --slot "$slot" --clean \
      --stale-clear --expected-generation "$generation" \
      --actor allocator:test) >"$output" 2>&1
}

run_quarantined_handoff_recovery() {
  local slot=$1 generation
  shift
  generation=$(python3 - "$fixture_root/worktree-state.json" "$slot" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
print(state["slots"][sys.argv[2]]["owner_sidecar"]["generation"])
PY
  )
  (cd "$fixture_root" && timeout "${RELEASE_TEST_STALE_CLEAR_TIMEOUT:-60}s" env \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    "$@" \
    "$script_dir/release-worktree.rs" --slot "$slot" --clean \
      --stale-clear --recover-quarantined-handoff \
      --expected-generation "$generation" --actor allocator:test) \
    >"$output" 2>&1
}

run_stale_clear_all() {
  (cd "$fixture_root" && timeout "${RELEASE_TEST_STALE_CLEAR_TIMEOUT:-60}s" env \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    "$script_dir/release-worktree.rs" \
      --stale-clear-all --actor allocator:test) >"$output" 2>&1
}

run_release_recover() {
  local slot=$1
  shift
  (cd "$fixture_root" && env \
    GIT_ALLOW_PROTOCOL=file \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    "$@" \
    "$script_dir/release-worktree.rs" \
    --slot "$slot" --clean --recover-submodule-cleanup) >"$output" 2>&1
}

assert_unrelated_survives() {
  grep -Fxq 'must survive' "$fixture_root/unrelated/sentinel.txt" \
    || fail "unrelated sentinel was changed for $fixture_root"
  test -d "$keep" || fail "unrelated worktree path was removed: $keep"
  git -C "$keep" status --porcelain >/dev/null \
    || fail "unrelated worktree is no longer usable: $keep"
  git -C "$primary" worktree list --porcelain \
    | grep -Fxq "worktree $keep" \
    || fail "unrelated worktree registration was removed: $keep"
}

assert_target_retained() {
  local slot=$1
  test -d "$target" || fail "refused target was removed: $target"
  git -C "$primary" worktree list --porcelain \
    | grep -Fxq "worktree $target" \
    || fail "refused target registration was removed: $target"
  grep -Fq "\"$slot\"" "$fixture_root/worktree-state.json" \
    || fail "refused target was removed from registry state: $slot"
  assert_unrelated_survives
}

assert_bad_owners_refused() {
  local name=$1 agents_json=$2 active_agent=$3
  make_fixture "$name"
  set_owner_schema "$agents_json" "$active_agent"
  if "$fixture_root/scripts/check-worktree-registry.rs" --root "$fixture_root" \
      >"$fixture_root/check-owners.out" 2>&1; then
    fail "canonical verifier accepted malformed owners: $name"
  fi
  grep -Fq 'OWNERS' "$fixture_root/check-owners.out" \
    || fail "canonical verifier did not attribute malformed owners: $name"
  if run_release_force "$name"; then
    fail "release accepted malformed owners: $name"
  fi
  grep -Fq 'invalid slot ownership' "$output" \
    || fail "release did not expose malformed ownership: $name"
  assert_target_retained "$name"
}

# RELEASE_TEST_STALE_CLEAR_ONLY keeps a fast, explicit bracket for the lock
# ordering without changing the default full-suite population.
if [[ ${RELEASE_TEST_STALE_CLEAR_ONLY:-0} != 1 ]]; then

# NAMED WATCHER POSITIVE: only an exact Watchman foreground process with a
# stable O_RDONLY descriptor is tolerated. The exception must be disclosed in
# output rather than rendered as a clean process census.
make_fixture named-read-only-watchman
plant_named_watchman_fd 0100000
if ! run_release named-read-only-watchman; then
  cat "$output" >&2
  fail 'exact named Watchman with a read-only descriptor was refused'
fi
grep -Fq 'TOLERATED_READ_ONLY_WATCHER: pid 424242@123456 watcher=watchman exe=/usr/local/bin/watchman fd/7=' "$output" \
  || fail 'named watcher tolerance was not explicitly disclosed'
grep -Fq 'reason=exact named watcher with stable non-writable descriptor' "$output" \
  || fail 'named watcher tolerance omitted its reason'
test ! -e "$target" || fail 'named read-only Watchman prevented clean release'
assert_unrelated_survives

# NAMED WATCHER NEGATIVE: the same exact identity with an O_WRONLY descriptor
# is a mutator, not a reader. It must remain a blocker and retain the target.
make_fixture named-writable-watchman
plant_named_watchman_fd 0100001
if run_release named-writable-watchman; then
  fail 'named Watchman with a writable descriptor authorized release'
fi
grep -Fq 'named watcher watchman has writable flags=00100001' "$output" \
  || fail 'writable named watcher refusal did not identify its descriptor mode'
if grep -Fq 'TOLERATED_READ_ONLY_WATCHER' "$output"; then
  fail 'writable named watcher was also reported as tolerated'
fi
assert_target_retained named-writable-watchman

# IDENTITY NEGATIVE: a process named watchman but executing another binary is
# not in the named set, even when its descriptor is O_RDONLY.
make_fixture spoofed-read-only-watchman
plant_named_watchman_fd 0100000 /bin/sleep
if run_release spoofed-read-only-watchman; then
  fail 'spoofed Watchman comm with another executable authorized release'
fi
grep -Fq 'pid 424242 fd/7=' "$output" \
  || fail 'spoofed Watchman refusal omitted the live descriptor'
if grep -Fq 'TOLERATED_READ_ONLY_WATCHER' "$output"; then
  fail 'spoofed Watchman identity was reported as tolerated'
fi
assert_target_retained spoofed-read-only-watchman

# EVIDENCE NEGATIVE: an exact named watcher whose descriptor remains live but
# lacks fdinfo must fail closed rather than being assumed read-only.
make_fixture unverifiable-read-only-watchman
plant_named_watchman_fd 0100000
rm "$fixture_root/proc/424242/fdinfo/7"
if run_release unverifiable-read-only-watchman; then
  fail 'named Watchman without descriptor mode evidence authorized release'
fi
grep -Fq 'remained live but unverifiable after retries' "$output" \
  || fail 'unverifiable named watcher did not fail closed with a reason'
if grep -Fq 'TOLERATED_READ_ONLY_WATCHER' "$output"; then
  fail 'unverifiable named watcher was reported as tolerated'
fi
assert_target_retained unverifiable-read-only-watchman

# POSITIVE: the outer repository plus two recursively initialized levels are
# clean and exactly published. Production must enumerate and verify all three.
make_fixture clean-submodule
mkdir -p "$fixture_root/cgroup/agent.slice/fixture-clean-submodule.scope"
: >"$fixture_root/cgroup/agent.slice/fixture-clean-submodule.scope/cgroup.procs"
printf 'populated 0\nfrozen 0\n' \
  >"$fixture_root/cgroup/agent.slice/fixture-clean-submodule.scope/cgroup.events"
test -z "$(git -C "$target" status --porcelain)" \
  || fail 'positive parent fixture is dirty before release'
test -z "$(git -C "$target/deps/nested" status --porcelain)" \
  || fail 'positive nested fixture is dirty before release'
test -z "$(git -C "$target/deps/nested/deps/leaf" status --porcelain)" \
  || fail 'positive recursively nested fixture is dirty before release'
target_head="$(git -C "$target" rev-parse HEAD)"
remote_target_head="$(git -C "$target" ls-remote origin "refs/heads/$target_branch" | cut -f1)"
test "$target_head" = "$remote_target_head" \
  || fail 'positive parent HEAD is not exactly published'
nested_head="$(git -C "$target/deps/nested" rev-parse HEAD)"
remote_nested_head="$(git -C "$target/deps/nested" ls-remote origin refs/heads/main | cut -f1)"
test "$nested_head" = "$remote_nested_head" \
  || fail 'positive nested HEAD is not exactly published'
leaf_head="$(git -C "$target/deps/nested/deps/leaf" rev-parse HEAD)"
remote_leaf_head="$(git -C "$target/deps/nested/deps/leaf" ls-remote origin refs/heads/main | cut -f1)"
test "$leaf_head" = "$remote_leaf_head" \
  || fail 'positive recursively nested HEAD is not exactly published'
clean_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
transaction_result="$fixture_root/transaction-result"
container_lock_result="$fixture_root/container-lock-result"
if ! run_release clean-submodule \
    RELEASE_TEST_TRANSACTION_ADMIN="$clean_admin" \
    RELEASE_TEST_TRANSACTION_RESULT="$transaction_result" \
    RELEASE_TEST_CONTAINER_LOCK_PATH="$fixture_root/ignored/ci-hub/agent-container-lifecycle.lock" \
    RELEASE_TEST_CONTAINER_LOCK_RESULT="$container_lock_result"; then
  cat "$output" >&2
  fail 'clean, fully published parent+submodule worktree was refused'
fi
grep -Fq 'verified 3 outer/nested repository HEAD(s) on origin' "$output" \
  || fail 'production did not report all three recursive repository proofs'
grep -Fxq 'active-nonforce' "$transaction_result" \
  || fail 'positive cleanup did not carry its marker/quarantine into non-force removal'
grep -Fxq 'blocked' "$container_lock_result" \
  || fail 'path fence was not protected by the canonical container lifecycle lock'
test ! -e "$clean_admin" \
  || fail 'successful cleanup left its worktree admin transaction behind'
test ! -e "$target" || fail 'clean target path survived successful release'
if git -C "$primary" worktree list --porcelain | grep -Fxq "worktree $target"; then
  fail 'clean target registration survived successful release'
fi
if grep -Fq '"clean-submodule"' "$fixture_root/worktree-state.json"; then
  fail 'clean target survived in registry state after successful release'
fi
assert_unrelated_survives

# DEINITIALIZED-SUBMODULE POSITIVE: Git refuses to move this clean linked
# worktree solely because its exact worktree admin retains deinitialized
# submodule repositories. The narrow fallback must atomically fence the path,
# repair and verify the registration, quarantine only that admin, and still
# reach Git's ordinary non-force removal boundary.
make_fixture clean-deinitialized-submodule
deinitialize_fixture_submodules
deinitialized_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
test -d "$deinitialized_admin/modules" \
  || fail 'deinitialized positive lacks retained submodule admin'
deinitialized_transaction_result="$fixture_root/deinitialized-transaction-result"
if ! run_release clean-deinitialized-submodule \
    RELEASE_TEST_TRANSACTION_ADMIN="$deinitialized_admin" \
    RELEASE_TEST_TRANSACTION_RESULT="$deinitialized_transaction_result"; then
  cat "$output" >&2
  fail 'clean fully deinitialized submodule worktree was refused'
fi
grep -Fq 'exact submodule-refusal atomic rename + verified git worktree repair' "$output" \
  || fail 'deinitialized positive did not exercise the narrow atomic fallback'
grep -Fxq 'active-nonforce' "$deinitialized_transaction_result" \
  || fail 'deinitialized positive did not retain Git non-force removal authority'
test ! -e "$target" || fail 'deinitialized positive target survived release'
assert_unrelated_survives

# ATOMIC-FALLBACK CRASH/RECOVERY POSITIVE: crash after the no-replace rename
# but before `git worktree repair`. The schema-2 marker plus old registration
# must let explicit recovery restore the canonical path, repeat every proof,
# and complete through a fresh fallback transaction.
make_fixture deinitialized-fallback-rename-crash
deinitialize_fixture_submodules
fallback_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
fallback_rename_crash="$fixture_root/post-fallback-rename-crash"
: >"$fallback_rename_crash"
if run_release deinitialized-fallback-rename-crash \
    HERMIT_RELEASE_TEST_CRASH_AFTER_FALLBACK_RENAME="$fallback_rename_crash"; then
  fail 'atomic fallback rename crash injection unexpectedly completed cleanup'
fi
grep -Fxq 'post-atomic-path-fence-rename crash injected' "$fallback_rename_crash" \
  || fail 'atomic fallback did not reach the post-rename crash boundary'
test ! -e "$target" \
  || fail 'atomic fallback rename crash left the canonical path published'
fallback_fenced_paths=("$fixture_root/worktrees/slots/deinitialized-fallback-rename-crash"/.hermit.release-worktree-*)
if test "${#fallback_fenced_paths[@]}" -ne 1 \
    || test ! -d "${fallback_fenced_paths[0]}"; then
  fail 'atomic fallback rename crash did not retain one fenced target'
fi
grep -Fq '"schema_version":2' "$fallback_admin/release-worktree.path-fence.json" \
  || fail 'atomic fallback crash marker did not carry schema 2'
grep -Fq '"phase":"renamed"' "$fallback_admin/release-worktree.path-fence.json" \
  || fail 'atomic fallback crash marker did not bind the pre-repair phase'
git -C "$primary" worktree list --porcelain \
  | grep -Fxq "worktree $target" \
  || fail 'pre-repair crash unexpectedly changed Git registration'
if run_release deinitialized-fallback-rename-crash; then
  fail 'ordinary retry consumed the atomic fallback crash transaction'
fi
grep -Fq 'unfinished release journal' "$output" \
  || fail 'ordinary retry did not retain the atomic fallback journal'
fallback_recovery_result="$fixture_root/fallback-recovery-transaction-result"
if ! run_release_recover deinitialized-fallback-rename-crash \
    RELEASE_TEST_TRANSACTION_ADMIN="$fallback_admin" \
    RELEASE_TEST_TRANSACTION_RESULT="$fallback_recovery_result"; then
  cat "$output" >&2
  fail 'guarded atomic fallback recovery did not complete cleanup'
fi
grep -Fq 'recovered path-acquisition fence' "$output" \
  || fail 'atomic fallback recovery did not report canonical-path restoration'
grep -Fq 'exact submodule-refusal atomic rename + verified git worktree repair' "$output" \
  || fail 'atomic fallback recovery did not repeat a fresh exact fallback'
grep -Fxq 'active-nonforce' "$fallback_recovery_result" \
  || fail 'atomic fallback recovery did not reach Git non-force removal'
test ! -e "$target" || fail 'atomic fallback recovered target survived cleanup'
assert_unrelated_survives

# SUBMODULE-RESIDUE NEGATIVE: an ignored file inside a deinitialized gitlink is
# invisible to ordinary porcelain status. The fallback's physical emptiness
# proof must retain it rather than treating "not initialized" as "no work".
make_fixture deinitialized-ignored-residue
printf '/deps/nested/hidden.txt\n' >"$target/.gitignore"
git -C "$target" add .gitignore
git -C "$target" commit -q -m 'ignore planted deinitialized residue'
git -C "$target" push -q origin "HEAD:refs/heads/$target_branch"
deinitialize_fixture_submodules
printf 'precious ignored residue\n' >"$target/deps/nested/hidden.txt"
test -z "$(git -C "$target" status --porcelain)" \
  || fail 'ignored-residue fixture is visible to ordinary status'
if run_release deinitialized-ignored-residue; then
  fail 'ignored deinitialized-submodule residue was removed'
fi
grep -Fq 'deinitialized submodule path' "$output" \
  || fail 'ignored residue refusal did not come from the physical submodule proof'
grep -Fxq 'precious ignored residue' "$target/deps/nested/hidden.txt" \
  || fail 'ignored deinitialized-submodule residue was lost'
assert_target_retained deinitialized-ignored-residue

# INITIALIZED-SUBMODULE NEGATIVE: even the exact Git refusal string cannot
# broaden the fallback. The ordinary initialized cleanup has quarantined its
# admin, so the fallback must refuse that mismatched state.
make_fixture initialized-exact-move-refusal
if run_release initialized-exact-move-refusal \
    RELEASE_TEST_FORCE_SUBMODULE_MOVE_REFUSAL=1; then
  fail 'exact refusal string authorized fallback for initialized submodules'
fi
grep -Fq 'deinitialized submodule admin' "$output" \
  || fail 'initialized-submodule fallback refusal did not expose admin mismatch'
assert_target_retained initialized-exact-move-refusal

# ADMIN-IDENTITY NEGATIVE: a symlinked retained modules admin is not the exact
# directory proved by the linked-worktree transaction.
make_fixture deinitialized-admin-alias
deinitialize_fixture_submodules
aliased_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
mv "$aliased_admin/modules" "$aliased_admin/modules.planted-real"
ln -s modules.planted-real "$aliased_admin/modules"
if run_release deinitialized-admin-alias \
    RELEASE_TEST_FORCE_SUBMODULE_MOVE_REFUSAL=1; then
  fail 'symlinked deinitialized submodule admin authorized fallback'
fi
grep -Fq 'is not an exact directory' "$output" \
  || fail 'symlinked admin refusal did not come from exact admin identity'
assert_target_retained deinitialized-admin-alias

# EXACT-ERROR NEGATIVE: an arbitrary worktree-move failure must never enter
# the atomic fallback, even when the checkout is otherwise deinitialized.
make_fixture deinitialized-nonspecific-move-refusal
deinitialize_fixture_submodules
if run_release deinitialized-nonspecific-move-refusal \
    RELEASE_TEST_NONSPECIFIC_MOVE_REFUSAL=1; then
  fail 'non-submodule Git move error authorized atomic fallback'
fi
grep -Fq 'planted non-submodule move refusal' "$output" \
  || fail 'non-specific move refusal was not preserved verbatim'
if grep -Fq 'atomic rename + verified git worktree repair' "$output"; then
  fail 'non-specific move refusal entered the atomic fallback'
fi
assert_target_retained deinitialized-nonspecific-move-refusal

# REPAIR-POSTCONDITION NEGATIVE + RECOVERY POSITIVE: exit zero from `git
# worktree repair` is not authority. Retain the fenced transaction when the
# registration did not move, then let explicit recovery restore and retry it.
make_fixture deinitialized-false-repair-success
deinitialize_fixture_submodules
false_repair_result="$fixture_root/false-repair-result"
if run_release deinitialized-false-repair-success \
    RELEASE_TEST_FAKE_REPAIR_SUCCESS=1 \
    RELEASE_TEST_FAKE_REPAIR_RESULT="$false_repair_result"; then
  fail 'Git repair exit zero without registration repair authorized release'
fi
grep -Fxq 'zero-without-repair' "$false_repair_result" \
  || fail 'false-repair fixture did not intercept git worktree repair'
grep -Fq 'post-repair worktree registration mismatch' "$output" \
  || fail 'release trusted repair exit zero without registration evidence'
test ! -e "$target" \
  || fail 'false-repair transaction unexpectedly restored canonical path'
false_repair_fenced=("$fixture_root/worktrees/slots/deinitialized-false-repair-success"/.hermit.release-worktree-*)
if test "${#false_repair_fenced[@]}" -ne 1 || test ! -d "${false_repair_fenced[0]}"; then
  fail 'false-repair transaction did not retain one recoverable fenced path'
fi
if ! run_release_recover deinitialized-false-repair-success; then
  cat "$output" >&2
  fail 'explicit recovery could not restore false-repair transaction'
fi
test ! -e "$target" || fail 'false-repair recovered target survived cleanup'
assert_unrelated_survives

# FALLBACK PATH-RACE NEGATIVE: a process takes cwd before Git emits the exact
# refusal. The raw rename follows that cwd just like Git's move, and the same
# post-fence census must roll back through verified repair without signaling it.
make_fixture deinitialized-fallback-process-race
deinitialize_fixture_submodules
fallback_process_pid_file="$fixture_root/fallback-process.pid"
fallback_process_log="$fixture_root/fallback-process.log"
if run_release deinitialized-fallback-process-race \
    RELEASE_TEST_FENCE_PROCESS_TARGET="$target" \
    RELEASE_TEST_FENCE_PROCESS_PID="$fallback_process_pid_file" \
    RELEASE_TEST_FENCE_PROCESS_LOG="$fallback_process_log" \
    RELEASE_TEST_FENCE_PROC_ROOT="$fixture_root/proc"; then
  fail 'fallback process that acquired cwd was deleted'
fi
fence_pid=$(cat "$fallback_process_pid_file")
kill -0 "$fence_pid" 2>/dev/null \
  || fail 'atomic fallback signaled a process it did not own'
grep -Fq 'post-fence live process ownership' "$output" \
  || fail 'atomic fallback post-fence census missed racing cwd owner'
kill "$fence_pid"
wait "$fence_pid" 2>/dev/null || true
fence_pid=
assert_target_retained deinitialized-fallback-process-race

# NEGATIVE: a tracked modification in the parent worktree must still refuse.
make_fixture dirty-parent
printf 'dirty parent content\n' >>"$target/parent.txt"
if run_release dirty-parent; then
  fail 'dirty parent worktree was removed'
fi
grep -Fq 'refusing --clean with uncommitted work' "$output" \
  || fail 'dirty parent refusal did not come from the dirty-work guardrail'
grep -Fq 'dirty parent content' "$target/parent.txt" \
  || fail 'dirty parent content was lost'
assert_target_retained dirty-parent

# NEGATIVE: untracked content inside the initialized nested submodule must make
# the parent status dirty and refuse without losing the nested content.
make_fixture dirty-nested
printf 'untracked nested content\n' >"$target/deps/nested/untracked.txt"
if run_release dirty-nested; then
  fail 'worktree with untracked nested-submodule content was removed'
fi
grep -Fq 'refusing --clean with uncommitted work' "$output" \
  || fail 'dirty nested refusal did not come from the dirty-work guardrail'
grep -Fxq 'untracked nested content' "$target/deps/nested/untracked.txt" \
  || fail 'untracked nested-submodule content was lost'
assert_target_retained dirty-nested

# NEGATIVE: a clean commit made after the last push must still refuse on the
# origin-durability guardrail.
make_fixture unpushed-branch
git -C "$target" commit -q --allow-empty -m 'local-only commit'
local_only_head="$(git -C "$target" rev-parse HEAD)"
if run_release unpushed-branch; then
  fail 'clean worktree with an unpushed commit was removed'
fi
grep -Fq 'committed work not on origin' "$output" \
  || fail 'unpushed refusal did not come from the origin-durability guardrail'
test "$local_only_head" = "$(git -C "$target" rev-parse HEAD)" \
  || fail 'unpushed branch HEAD changed during refusal'
assert_target_retained unpushed-branch

# BRANCH-AUTHORITY NEGATIVE: publishing HEAD only through remote HEAD, a tag,
# and a custom ref is not a recoverable feature-branch handoff. None may stand
# in for refs/heads reachability.
make_fixture tag-only
git -C "$target" commit -q --allow-empty -m 'tag-only commit'
tag_only_head="$(git -C "$target" rev-parse HEAD)"
git -C "$target" tag tag-only-recovery
git -C "$target" push -q origin refs/tags/tag-only-recovery
git -C "$target" push -q origin HEAD:refs/recovery/tag-only
git -C "$fixture_root/remotes/hermit.git" update-ref --no-deref HEAD "$tag_only_head"
test "$tag_only_head" = "$(git -C "$target" ls-remote origin refs/tags/tag-only-recovery | cut -f1)" \
  || fail 'tag-only fixture did not publish its exact HEAD through a tag'
test "$tag_only_head" = "$(git -C "$target" ls-remote origin HEAD | cut -f1)" \
  || fail 'tag-only fixture did not advertise its exact commit through remote HEAD'
test "$tag_only_head" = "$(git -C "$target" ls-remote origin refs/recovery/tag-only | cut -f1)" \
  || fail 'tag-only fixture did not publish its exact HEAD through a custom ref'
if run_release tag-only; then
  fail 'non-branch remote reachability authorized worktree deletion'
fi
grep -Fq 'committed work not on origin' "$output" \
  || fail 'non-branch-only refusal did not come from branch durability authority'
assert_target_retained tag-only

# EXPLICIT-FORCE POSITIVE: the dangerous override remains available only when
# the user requests it. It reaches Git force directly, never by consuming the
# normal path's marker or quarantined submodule admin.
make_fixture explicit-force
git -C "$target" commit -q --allow-empty -m 'force-only local commit'
printf 'force-only dirty work\n' >"$target/force-only.txt"
force_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
force_result="$fixture_root/force-result"
if ! (cd "$fixture_root" && env \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    RELEASE_TEST_PODMAN_STATE="$fixture_root/podman.json" \
    RELEASE_TEST_FORCE_ADMIN="$force_admin" \
    RELEASE_TEST_FORCE_RESULT="$force_result" \
    "$script_dir/release-worktree.rs" --slot explicit-force --clean --force) \
    >"$output" 2>&1; then
  cat "$output" >&2
  fail 'explicit force did not override dirty and unpublished work'
fi
grep -Fxq 'direct-force' "$force_result" \
  || fail 'explicit force consumed a normal-cleanup transaction artifact'
test ! -e "$target" || fail 'explicit-force target survived requested deletion'
if grep -Fq '"explicit-force"' "$fixture_root/worktree-state.json"; then
  fail 'explicit-force target survived in registry state after deletion'
fi
assert_unrelated_survives

# NEGATIVE: the registry remains the authority for which slot may be released.
make_fixture registry-authority
if run_release not-registered; then
  fail 'unregistered slot name was accepted'
fi
grep -Fq 'slot not-registered is not registered in worktree-state.json' "$output" \
  || fail 'unregistered refusal did not come from the registry guardrail'
assert_target_retained registry-authority

# OWNER-SCHEMA NEGATIVES: the canonical verifier and releaser must both reject
# missing fields, duplicate names, no mutating owner, and placeholder names.
assert_bad_owners_refused owner-missing-fields '[{}]' '-'
assert_bad_owners_refused owner-duplicate \
  '[{"name":"dup","read_only":false},{"name":"dup","read_only":true}]' \
  'dup (+ro: dup)'
assert_bad_owners_refused owner-no-mutator \
  '[{"name":"reader","read_only":true}]' '- (+ro: reader)'
# TWO MUTATING OWNERS -- the state allocate-worktree.rs used to create by
# appending a claimant beside a RELEASED predecessor instead of replacing it
# (fixed 2026-08-08; bracketed in allocate-worktree-released-rebind-test.sh).
#
# This case guards the OTHER half of that fix. The refusal below is the check
# that caught the append bug in the first place, and repairing the producer must
# not be mistaken for a licence to relax the consumer: two mutating owners is
# still an invalid slot, whatever produced it, and release must still refuse to
# act on one. Note ACTIVE.md cannot show this state -- it renders only the FIRST
# mutating owner -- so this refusal is the only thing that surfaces it.
assert_bad_owners_refused owner-two-mutators \
  '[{"name":"first","read_only":false},{"name":"second","read_only":false}]' 'first'
assert_bad_owners_refused owner-invalid-name \
  '[{"name":"","read_only":false}]' ''

# REVIEW FIXTURE 1: the outer feature branch is clean and exactly pushed, but
# its initialized nested repository has a detached local-only commit. Production
# must verify the nested origin itself and retain its only copy.
make_fixture nested-undurable
configure_repo "$target/deps/nested"
git -C "$target/deps/nested" commit -q --allow-empty -m 'nested local-only commit'
nested_local_head="$(git -C "$target/deps/nested" rev-parse HEAD)"
git -C "$target" add deps/nested
git -C "$target" commit -q -m 'record unpublished nested commit'
git -C "$target" push -q origin "HEAD:refs/heads/$target_branch"
test -z "$(git -C "$target" status --porcelain)" \
  || fail 'nested-durability outer fixture is dirty'
if git -C "$target/deps/nested" ls-remote origin \
  | cut -f1 | grep -Fxq "$nested_local_head"; then
  fail 'nested-durability fixture is inert: local nested HEAD is already published'
fi
if run_release nested-undurable; then
  fail 'clean outer worktree with unpublished nested HEAD was removed'
fi
grep -Fq 'not reachable from any current origin branch' "$output" \
  || fail 'nested durability refusal did not come from recursive origin proof'
test "$nested_local_head" = "$(git -C "$target/deps/nested" rev-parse HEAD)" \
  || fail 'unpublished nested HEAD was lost'
assert_target_retained nested-undurable

# REVIEW FIXTURE 2: malformed committed .gitmodules makes Git inspection fail
# while the initialized nested repository holds untracked work. An inspection
# error must refuse rather than being interpreted as clean.
make_fixture malformed-inspection
printf '\n[submodule "unterminated"\n' >>"$target/.gitmodules"
git -C "$target" add .gitmodules
git -C "$target" commit -q --no-verify -m 'commit malformed submodule metadata'
git -C "$target" push -q origin "HEAD:refs/heads/$target_branch"
printf 'precious nested work\n' >"$target/deps/nested/precious.txt"
set +e
git -C "$target" status --porcelain >"$fixture_root/malformed-status.out" 2>&1
malformed_status_rc=$?
set -e
test "$malformed_status_rc" -ne 0 \
  || fail 'malformed inspection fixture is inert: git status unexpectedly succeeded'
if run_release_force malformed-inspection; then
  fail 'Git inspection failure was interpreted as authorization'
fi
grep -Eq 'could not enumerate initialized submodules|git .* status .* failed' "$output" \
  || fail 'malformed metadata refusal did not expose a fail-closed Git inspection'
grep -Fxq 'precious nested work' "$target/deps/nested/precious.txt" \
  || fail 'untracked work behind failed Git inspection was lost'
assert_target_retained malformed-inspection

# REVIEW FIXTURE 3: plant an untracked file from the legitimate ls-remote
# adapter after the preliminary statuses. The final recursive boundary must see
# it and refuse.
make_fixture cleanliness-race
if run_release cleanliness-race RELEASE_TEST_RACE_TARGET="$target/raced.txt"; then
  fail 'last-moment untracked write was deleted by force removal'
fi
grep -Fq 'final removal boundary refused' "$output" \
  || fail 'last-moment write did not reach the final removal boundary'
grep -Fxq 'planted after the preliminary status' "$target/raced.txt" \
  || fail 'last-moment write was lost'
assert_target_retained cleanliness-race

# REVIEW FIXTURE 3B: plant after every scripted status, at the actual worktree
# remove invocation. Ordinary cleanup must use Git's non-force boundary, which
# sees the new file and retains the worktree.
make_fixture remove-boundary-race
remove_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
test -d "$remove_admin/modules" \
  || fail 'remove-race fixture has no initialized submodule admin to quarantine'
if run_release remove-boundary-race \
    RELEASE_TEST_REMOVE_RACE_TARGET="remove-raced.txt"; then
  fail 'write planted at worktree remove invocation was deleted'
fi
grep -Fxq 'planted at worktree remove invocation' "$target/remove-raced.txt" \
  || fail 'remove-invocation write did not survive'
grep -Eq 'could not remove exact|modified or untracked files' "$output" \
  || fail 'remove-invocation write did not reach Git non-force refusal'
test -d "$remove_admin/modules" \
  || fail 'submodule admin was not restored after non-force removal refusal'
test ! -e "$remove_admin/modules.release-worktree" \
  || fail 'submodule-admin quarantine survived a handled removal refusal'
test -f "$remove_admin/release-worktree.in-progress" \
  || fail 'failed submodule cleanup lost its crash-recovery marker'
assert_target_retained remove-boundary-race

# GIT-SUCCESS POSTCONDITION NEGATIVE: exit zero is not removal authority. A
# fake Git that reports success while retaining both the fenced path and its
# registration must be observed, rolled back, and left journaled for recovery.
make_fixture false-remove-success
false_remove_result="$fixture_root/false-remove-result"
if run_release false-remove-success \
    RELEASE_TEST_FAKE_REMOVE_SUCCESS=1 \
    RELEASE_TEST_FAKE_REMOVE_RESULT="$false_remove_result"; then
  fail 'Git exit zero without removal authorized release'
fi
grep -Fxq 'zero-without-removal' "$false_remove_result" \
  || fail 'false-success fixture did not intercept Git removal'
grep -Fq 'Git removal postcondition failed for hermit' "$output" \
  || fail 'release trusted Git exit zero without observing its postcondition'
assert_target_retained false-remove-success

# PATH-FENCE RACE NEGATIVE: a process acquires cwd after every preliminary
# proof but immediately before Git atomically moves the target. Its live cwd
# follows the moved inode, so the post-move scan must observe it and roll back.
make_fixture path-fence-process-race
fence_process_pid_file="$fixture_root/fence-process.pid"
fence_process_log="$fixture_root/fence-process.log"
if run_release path-fence-process-race \
    RELEASE_TEST_FENCE_PROCESS_TARGET="$target" \
    RELEASE_TEST_FENCE_PROCESS_PID="$fence_process_pid_file" \
    RELEASE_TEST_FENCE_PROCESS_LOG="$fence_process_log" \
    RELEASE_TEST_FENCE_PROC_ROOT="$fixture_root/proc"; then
  fail 'process that acquired cwd at the path fence was deleted'
fi
fence_pid=$(cat "$fence_process_pid_file")
kill -0 "$fence_pid" 2>/dev/null \
  || fail 'path-fence release signaled a process it did not own'
grep -Fq 'post-fence live process ownership' "$output" \
  || fail 'post-move scan did not observe the racing cwd owner'
kill "$fence_pid"
wait "$fence_pid" 2>/dev/null || true
fence_pid=
assert_target_retained path-fence-process-race

# PATH-FENCE PODMAN RACE NEGATIVE: an out-of-protocol container appears at the
# move boundary after every preliminary Podman audit. The repeat audit over the
# original and fenced paths must still retain it.
make_fixture path-fence-podman-race
cat >"$fixture_root/podman-race.json" <<JSON
[{
  "Id": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
  "Names": ["fence-race-mount"],
  "State": "running",
  "Labels": null,
  "Mounts": [{"Source": "$target"}]
}]
JSON
if run_release path-fence-podman-race \
    RELEASE_TEST_PODMAN_RACE_SOURCE="$fixture_root/podman-race.json" \
    RELEASE_TEST_PODMAN_RACE_TARGET="$fixture_root/podman.json"; then
  fail 'container that acquired a bind at the path fence was deleted'
fi
grep -Fq 'target-overlapping-mount' "$output" \
  || grep -Fq 'cannot be resolved' "$output" \
  || fail 'post-move Podman audit did not observe or fail closed on the racing target mount'
assert_target_retained path-fence-podman-race

# PATH-FENCE DANGLING-ALIAS NEGATIVE: a direct Podman user can insert a bind
# through an alias at the move boundary. The alias becomes dangling when Git
# moves the target, but that must make evidence incomplete rather than erase
# the live mount's identity.
make_fixture path-fence-podman-alias-race
alias_source="$fixture_root/late-target-alias"
ln -s "$target" "$alias_source"
cat >"$fixture_root/podman-alias-race.json" <<JSON
[{
  "Id": "abababababababababababababababababababababababababababababababab",
  "Names": ["fence-alias-race-mount"],
  "State": "running",
  "Labels": null,
  "Mounts": [{"Source": "$alias_source"}]
}]
JSON
if run_release path-fence-podman-alias-race \
    RELEASE_TEST_PODMAN_RACE_SOURCE="$fixture_root/podman-alias-race.json" \
    RELEASE_TEST_PODMAN_RACE_TARGET="$fixture_root/podman.json"; then
  fail 'container with a dangling post-fence source alias authorized deletion'
fi
grep -Fq 'cannot be resolved' "$output" \
  || fail 'dangling post-fence mount alias did not fail closed'
assert_target_retained path-fence-podman-alias-race

# PATH-FENCE CONFIG-CHANGE NEGATIVE: even when both old and new sources are
# unrelated and resolvable, direct out-of-protocol configuration changes must
# invalidate the pre-fence observation instead of being accepted as a fresh,
# unrelated snapshot.
make_fixture path-fence-podman-config-race
mkdir -p "$fixture_root/unrelated-before" "$fixture_root/unrelated-after"
cat >"$fixture_root/podman.json" <<JSON
[{
  "Id": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
  "Names": ["changing-unrelated-mount"],
  "State": "running",
  "Labels": null,
  "Mounts": [{"Source": "$fixture_root/unrelated-before"}]
}]
JSON
cat >"$fixture_root/podman-config-race.json" <<JSON
[{
  "Id": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
  "Names": ["changing-unrelated-mount"],
  "State": "running",
  "Labels": null,
  "Mounts": [{"Source": "$fixture_root/unrelated-after"}]
}]
JSON
if run_release path-fence-podman-config-race \
    RELEASE_TEST_PODMAN_RACE_SOURCE="$fixture_root/podman-config-race.json" \
    RELEASE_TEST_PODMAN_RACE_TARGET="$fixture_root/podman.json"; then
  fail 'changed container mount configuration authorized deletion'
fi
grep -Fq 'container census/config/mount observation changed across the path fence' "$output" \
  || fail 'path-fence config mutation was not bound to the pre-fence observation'
assert_target_retained path-fence-podman-config-race

# REGISTRY-SPLIT CRASHES: worktree-state.json is authoritative and ACTIVE.md
# is a human-augmented projection. Each planted crash after the JSON rename but
# before the managed block rewrite must converge under explicit recovery while
# preserving content outside the markers.
make_fixture journal-arm-active-split
printf 'human recovery sentinel: journal-arm\n' >>"$fixture_root/worktrees/ACTIVE.md"
arm_split_evidence="$fixture_root/journal-arm-active-split"
: >"$arm_split_evidence"
if run_release journal-arm-active-split \
    HERMIT_RELEASE_TEST_CRASH_AFTER_JOURNAL_ARM_STATE="$arm_split_evidence"; then
  fail 'journal-arm state/ACTIVE split injection unexpectedly completed'
fi
grep -Fxq 'post-journal-arm state/ACTIVE split injected' "$arm_split_evidence" \
  || fail 'journal-arm split did not reach the exact persistence boundary'
grep -Fq '"status": "releasing"' "$fixture_root/worktree-state.json" \
  || fail 'journal-arm split did not durably arm JSON state'
grep -Fq '| journal-arm-active-split ' "$fixture_root/worktrees/ACTIVE.md" \
  || fail 'journal-arm split unexpectedly rewrote ACTIVE before the crash'
if ! run_release_recover journal-arm-active-split; then
  cat "$output" >&2
  fail 'explicit recovery did not reconcile the journal-arm ACTIVE split'
fi
grep -Fxq 'human recovery sentinel: journal-arm' "$fixture_root/worktrees/ACTIVE.md" \
  || fail 'journal-arm recovery overwrote human ACTIVE content'
assert_unrelated_survives

make_fixture journal-clear-active-split
printf 'human recovery sentinel: journal-clear\n' >>"$fixture_root/worktrees/ACTIVE.md"
clear_split_evidence="$fixture_root/journal-clear-active-split"
: >"$clear_split_evidence"
if run_release journal-clear-active-split \
    HERMIT_RELEASE_TEST_CRASH_AFTER_JOURNAL_CLEAR_STATE="$clear_split_evidence"; then
  fail 'journal-clear state/ACTIVE split injection unexpectedly completed'
fi
grep -Fxq 'post-journal-clear state/ACTIVE split injected' "$clear_split_evidence" \
  || fail 'journal-clear split did not reach the exact persistence boundary'
test ! -e "$target" || fail 'journal-clear split did not follow completed Git removal'
grep -Fq '"status": "active"' "$fixture_root/worktree-state.json" \
  || fail 'journal-clear split did not durably clear releasing state'
grep -Fq '| journal-clear-active-split ' "$fixture_root/worktrees/ACTIVE.md" \
  || fail 'journal-clear split unexpectedly removed the stale ACTIVE row'
if ! run_release_recover journal-clear-active-split; then
  cat "$output" >&2
  fail 'explicit recovery did not reconcile the journal-clear ACTIVE split'
fi
grep -Fxq 'human recovery sentinel: journal-clear' "$fixture_root/worktrees/ACTIVE.md" \
  || fail 'journal-clear recovery overwrote human ACTIVE content'
assert_unrelated_survives

make_fixture final-state-active-split
printf 'human recovery sentinel: final-state\n' >>"$fixture_root/worktrees/ACTIVE.md"
final_split_evidence="$fixture_root/final-state-active-split"
: >"$final_split_evidence"
if run_release final-state-active-split \
    HERMIT_RELEASE_TEST_CRASH_AFTER_FINAL_STATE="$final_split_evidence"; then
  fail 'final-state state/ACTIVE split injection unexpectedly completed'
fi
grep -Fxq 'post-final-state state/ACTIVE split injected' "$final_split_evidence" \
  || fail 'final-state split did not reach the exact persistence boundary'
if grep -Fq 'final-state-active-split' "$fixture_root/worktree-state.json"; then
  fail 'final-state split did not durably delete the slot JSON record'
fi
grep -Fq '| final-state-active-split ' "$fixture_root/worktrees/ACTIVE.md" \
  || fail 'final-state split unexpectedly removed the stale ACTIVE row'
# Terminal missing-slot recovery proves absence through every product worktree
# authority. Materialize the otherwise-unused fixture primaries for that proof.
for product in reverie liteinst2; do
  git -C "$fixture_root/$product" init -q -b main
  configure_repo "$fixture_root/$product"
  : >"$fixture_root/$product/.fixture"
  git -C "$fixture_root/$product" add .fixture
  git -C "$fixture_root/$product" commit -q -m 'fixture primary'
done
if ! run_release_recover final-state-active-split; then
  cat "$output" >&2
  fail 'explicit recovery did not reconcile the final-slot ACTIVE split'
fi
grep -Fxq 'human recovery sentinel: final-state' "$fixture_root/worktrees/ACTIVE.md" \
  || fail 'final-state recovery overwrote human ACTIVE content'
grep -Fq 'recovered completed release metadata' "$output" \
  || fail 'terminal missing-slot recovery did not report its proof'
assert_unrelated_survives

# CRASH/RECOVERY BRACKET: production performs a real recursive deinit and exact
# admin quarantine, then the fixture terminates before removal. Ordinary and
# force retries refuse both artifacts. The guarded recovery restores the exact
# quarantine, reinitializes, repeats every recursive branch proof, and reaches
# ordinary non-force removal.
make_fixture interrupted-deinit
unfinished_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
crash_evidence="$fixture_root/post-deinit-crash"
: >"$crash_evidence"
if run_release interrupted-deinit \
    HERMIT_RELEASE_TEST_CRASH_AFTER_DEINIT="$crash_evidence"; then
  fail 'post-deinit crash injection unexpectedly completed cleanup'
fi
grep -Fxq 'post-deinit crash injected' "$crash_evidence" \
  || fail 'production did not reach the post-deinit crash boundary'
test -f "$unfinished_admin/release-worktree.in-progress" \
  || fail 'real interrupted deinit did not retain its recovery marker'
test ! -e "$unfinished_admin/modules" \
  || fail 'post-deinit crash did not reach exact admin quarantine'
test -d "$unfinished_admin/modules.release-worktree" \
  || fail 'post-deinit crash lost the quarantined nested repository admin'
submodule_state="$(git -C "$target" submodule status deps/nested)"
[[ $submodule_state == -* ]] \
  || fail 'post-deinit crash fixture still has an initialized nested worktree'
if run_release_force interrupted-deinit; then
  fail 'force consumed a real interrupted submodule-cleanup transaction'
fi
grep -Fq 'unfinished release journal' "$output" \
  || fail 'ordinary retry did not refuse the real interrupted transaction'
assert_target_retained interrupted-deinit

recovery_transaction_result="$fixture_root/recovery-transaction-result"
if ! run_release_recover interrupted-deinit \
    RELEASE_TEST_TRANSACTION_ADMIN="$unfinished_admin" \
    RELEASE_TEST_TRANSACTION_RESULT="$recovery_transaction_result"; then
  cat "$output" >&2
  fail 'guarded recovery could not reinitialize, re-prove, and remove the target'
fi
grep -Fq 'recovered interrupted submodule cleanup' "$output" \
  || fail 'guarded recovery did not report successful recursive reinitialization'
grep -Fq 'verified 3 outer/nested repository HEAD(s) on origin' "$output" \
  || fail 'recovered cleanup did not repeat recursive durability proof'
grep -Fxq 'active-nonforce' "$recovery_transaction_result" \
  || fail 'recovered cleanup did not reach ordinary non-force removal'
test ! -e "$target" || fail 'recovered target survived successful cleanup'
assert_unrelated_survives

# PATH-FENCE CRASH/RECOVERY: terminate after the canonical path has moved and
# the Git registration plus exact journal marker bind the fenced nonce. Guarded
# recovery must move it back, restore/reinitialize submodules, repeat proofs,
# and reach a fresh fenced non-force removal.
make_fixture interrupted-path-fence
fence_admin="$(git -C "$target" rev-parse --path-format=absolute --git-dir)"
fence_crash_evidence="$fixture_root/post-path-fence-crash"
: >"$fence_crash_evidence"
if run_release interrupted-path-fence \
    HERMIT_RELEASE_TEST_CRASH_AFTER_PATH_FENCE="$fence_crash_evidence"; then
  fail 'post-path-fence crash injection unexpectedly completed cleanup'
fi
grep -Fxq 'post-path-fence crash injected' "$fence_crash_evidence" \
  || fail 'production did not reach the post-path-fence crash boundary'
test ! -e "$target" \
  || fail 'path-fence crash left the canonical target path published'
fenced_paths=("$fixture_root/worktrees/slots/interrupted-path-fence"/.hermit.release-worktree-*)
if test "${#fenced_paths[@]}" -ne 1 || test ! -d "${fenced_paths[0]}"; then
  fail 'path-fence crash did not retain one exact fenced target'
fi
grep -Fq '"status": "releasing"' "$fixture_root/worktree-state.json" \
  || fail 'path-fence crash did not retain releasing registry state'
test -f "$fence_admin/release-worktree.path-fence.json" \
  || fail 'path-fence crash lost its exact path marker'
if run_release_force interrupted-path-fence; then
  fail 'force consumed an interrupted path-fence transaction'
fi
grep -Fq 'unfinished release journal' "$output" \
  || fail 'ordinary retry did not refuse the path-fence journal'

fence_recovery_result="$fixture_root/fence-recovery-transaction-result"
if ! run_release_recover interrupted-path-fence \
    RELEASE_TEST_TRANSACTION_ADMIN="$fence_admin" \
    RELEASE_TEST_TRANSACTION_RESULT="$fence_recovery_result"; then
  cat "$output" >&2
  fail 'guarded path-fence recovery could not re-prove and remove the target'
fi
grep -Fq 'recovered path-acquisition fence' "$output" \
  || fail 'guarded recovery did not report the restored path fence'
grep -Fq 'recovered interrupted submodule cleanup' "$output" \
  || fail 'guarded path recovery did not restore recursive submodule state'
grep -Fxq 'active-nonforce' "$fence_recovery_result" \
  || fail 'path-fence recovery did not reach fresh ordinary non-force removal'
test ! -e "$target" || fail 'path-fence recovered target survived cleanup'
assert_unrelated_survives

# COMPLETED-REMOVE CRASH/RECOVERY: Git can remove the fenced worktree before
# the registry journal is advanced. Recovery may accept the absent paths only
# after proving that Git's own worktree registry contains neither path and the
# exact release process durably recorded successful Git removal.
make_fixture interrupted-after-remove
remove_crash_evidence="$fixture_root/post-git-remove-crash"
: >"$remove_crash_evidence"
if run_release interrupted-after-remove \
    HERMIT_RELEASE_TEST_CRASH_AFTER_GIT_REMOVE="$remove_crash_evidence"; then
  fail 'post-Git-remove crash injection unexpectedly completed cleanup'
fi
grep -Fxq 'post-git-remove crash injected' "$remove_crash_evidence" \
  || fail 'production did not reach the post-Git-remove crash boundary'
test ! -e "$target" \
  || fail 'post-Git-remove crash retained the canonical target'
grep -Fq '"status": "releasing"' "$fixture_root/worktree-state.json" \
  || fail 'post-Git-remove crash did not retain its release journal'
grep -Fq '"phase": "git-removal-complete"' "$fixture_root/worktree-state.json" \
  || fail 'post-Git-remove crash lacks causal completion evidence'
if run_release interrupted-after-remove; then
  fail 'ordinary retry consumed a completed-remove journal'
fi
grep -Fq 'unfinished release journal' "$output" \
  || fail 'ordinary retry did not refuse the completed-remove journal'
if ! run_release_recover interrupted-after-remove; then
  cat "$output" >&2
  fail 'guarded recovery did not reconcile the completed Git removal'
fi
grep -Fq 'recovered completed Git removal for hermit' "$output" \
  || fail 'completed-remove recovery did not report its Git registration proof'
test -e "$fixture_root/worktree-state.json" \
  || fail 'completed-remove recovery unexpectedly removed the registry file'
if grep -Fq 'interrupted-after-remove' "$fixture_root/worktree-state.json"; then
  fail 'completed-remove recovery retained the released slot record'
fi
assert_unrelated_survives

# ABSENCE IS NOT COMPLETION AUTHORITY: remove an armed target out of band before
# the release process records Git success. Identical path/registration absence
# without the causal phase must remain a refusal.
make_fixture absent-without-completion-evidence
absence_split_evidence="$fixture_root/absence-journal-arm"
: >"$absence_split_evidence"
if run_release absent-without-completion-evidence \
    HERMIT_RELEASE_TEST_CRASH_AFTER_JOURNAL_ARM_STATE="$absence_split_evidence"; then
  fail 'absence-authority fixture unexpectedly passed journal arm'
fi
git -C "$primary" worktree remove --force "$target"
if run_release_recover absent-without-completion-evidence; then
  fail 'mere target absence authorized completed-removal recovery'
fi
grep -Fq 'without durable git-removal-complete evidence' "$output" \
  || fail 'completed-removal recovery did not require causal phase evidence'
grep -Fq '"phase": "armed"' "$fixture_root/worktree-state.json" \
  || fail 'absence-authority refusal rewrote the armed journal'
assert_unrelated_survives

# REVIEW FIXTURE 4: the registered state key points at a symlink alias of an
# unrelated physical worktree. Exact canonical/state/registry binding must
# reject the alias and preserve the real checkout.
make_fixture real-target
real_target=$target
mkdir -p "$fixture_root/worktrees/slots/alias"
ln -s ../real-target/hermit "$fixture_root/worktrees/slots/alias/hermit"
write_state alias "$target_branch" 'worktrees/slots/alias/hermit'
target=$real_target
if run_release_force alias; then
  fail 'symlink alias removed an unrelated physical worktree'
fi
grep -Fq 'refusing symlink/path alias' "$output" \
  || fail 'symlink alias was not rejected by exact target binding'
assert_target_retained alias

# REGISTRY-GENERATION NEGATIVE: an out-of-protocol writer swaps both registry
# files to a second internally valid owner during ls-remote. Full-record binding
# must detect the change even though the canonical verifier accepts the new row.
make_fixture ownership-race
cp "$fixture_root/worktree-state.json" "$fixture_root/replacement-state.json"
cp "$fixture_root/worktrees/ACTIVE.md" "$fixture_root/replacement-ACTIVE.md"
sed -i 's/fixture-ownership-race/replacement-owner/g' \
  "$fixture_root/replacement-state.json" "$fixture_root/replacement-ACTIVE.md"
if run_release ownership-race \
    RELEASE_TEST_REASSIGN_STATE="$fixture_root/replacement-state.json" \
    RELEASE_TEST_STATE_TARGET="$fixture_root/worktree-state.json" \
    RELEASE_TEST_REASSIGN_ACTIVE="$fixture_root/replacement-ACTIVE.md" \
    RELEASE_TEST_ACTIVE_TARGET="$fixture_root/worktrees/ACTIVE.md"; then
  fail 'valid concurrent registry reassignment was deleted'
fi
grep -Fq 'authorization changed before removal' "$output" \
  || fail 'full slot-record generation did not detect registry reassignment'
assert_target_retained ownership-race

# REGISTRY-LOCK POSITIVE: every network child observes the canonical writer
# lock as held; no second allocator/releaser can enter the mutation interval.
make_fixture registry-lock
lock_result="$fixture_root/lock-result"
if ! run_release registry-lock \
    RELEASE_TEST_LOCK_PATH="$fixture_root/worktree-state.lock" \
    RELEASE_TEST_LOCK_RESULT="$lock_result"; then
  cat "$output" >&2
  fail 'clean release failed while probing registry lock'
fi
grep -Fxq 'blocked' "$lock_result" \
  || fail 'release did not hold canonical registry lock across network proof'
test ! -e "$target" || fail 'registry-lock positive target survived release'
assert_unrelated_survives

fi

# STALE-CLEAR LOCK-ORDER POSITIVE: the owner verifier takes the canonical
# shared registry lock. It must finish before release-worktree takes the writer
# lock, while the later cleanup remains serialized against a second writer.
make_fixture stale-clear-dead-owner
plant_expired_owner_lease stale-clear-dead-owner >/dev/null
authority_lock_result="$fixture_root/authority-lock-result"
concurrent_lock_result="$fixture_root/concurrent-lock-result"
if ! run_stale_clear stale-clear-dead-owner \
    RELEASE_TEST_AUTHORITY_LOCK_MARKER="$authority_lock_result" \
    RELEASE_TEST_LOCK_PATH="$fixture_root/worktree-state.lock" \
    RELEASE_TEST_LOCK_RESULT="$concurrent_lock_result"; then
  cat "$output" >&2
  fail 'expired dead-owner stale-clear did not complete'
fi
grep -Fxq 'shared-acquired' "$authority_lock_result" \
  || fail 'stale-clear owner authority never acquired the shared registry lock'
grep -Fxq 'blocked' "$concurrent_lock_result" \
  || fail 'stale-clear did not serialize its cleanup against a concurrent writer'
test ! -e "$target" || fail 'successful dead-owner stale-clear retained its target'
python3 - "$fixture_root/worktree-state.json" <<'PY' \
  || fail 'successful dead-owner stale-clear retained its active registry row or lost its audit history'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
assert "stale-clear-dead-owner" not in state["slots"]
assert any(
    row.get("resource") == "slot/stale-clear-dead-owner"
    for row in state.get("lease_history", [])
)
PY
assert_unrelated_survives

# STALE-CLEAR GENERATION/LEASE RACE NEGATIVE: a writer renews the exact lease
# after the guarded authority read and before release obtains the writer lock.
# The old verdict must not authorize cleanup of the changed row.
make_fixture stale-clear-renewed-race
plant_expired_owner_lease stale-clear-renewed-race >/dev/null
if run_stale_clear stale-clear-renewed-race \
    RELEASE_TEST_AUTHORITY_RENEW_LEASE=1; then
  fail 'pre-lock owner verdict authorized a changed lease'
fi
grep -Fq 'owner authority changed before stale-clear lock acquisition' "$output" \
  || fail 'stale-clear did not bind the owner verdict to the complete lease'
assert_target_retained stale-clear-renewed-race

# STALE-CLEAR LIVENESS NEGATIVES: fixing the lock order must not reinterpret an
# unavailable or live owner as dead. Both verdicts remain fail-closed and must
# return within the same bounded fixture command rather than hanging.
make_fixture stale-clear-indeterminate-owner
plant_expired_owner_lease stale-clear-indeterminate-owner >/dev/null
if run_stale_clear stale-clear-indeterminate-owner \
    RELEASE_TEST_OWNER_AUTHORITY_STATE=indeterminate; then
  fail 'indeterminate owner authority authorized stale-clear'
fi
grep -Fq 'owner authority is INDETERMINATE; stale-clear is fail-closed' "$output" \
  || fail 'stale-clear did not retain the indeterminate-owner refusal'
assert_target_retained stale-clear-indeterminate-owner

make_fixture stale-clear-live-owner
plant_expired_owner_lease stale-clear-live-owner >/dev/null
if run_stale_clear stale-clear-live-owner \
    RELEASE_TEST_OWNER_AUTHORITY_STATE=alive; then
  fail 'live ORC owner authorized stale-clear'
fi
grep -Fq 'owner authority is alive; live ORC owner refuses stale-clear' "$output" \
  || fail 'stale-clear did not retain the live-owner refusal'
assert_target_retained stale-clear-live-owner

# QUARANTINED-HANDOFF STATE NEGATIVE: the explicit recovery flag is not a
# second spelling for ordinary stale-clear and cannot act on an active lease.
make_fixture quarantined-handoff-active
plant_expired_owner_lease quarantined-handoff-active >/dev/null
if run_quarantined_handoff_recovery quarantined-handoff-active; then
  fail 'quarantined-handoff recovery accepted an active lease'
fi
grep -Fq 'requires slot quarantined-handoff-active state lease-quarantined with a quarantined lease' "$output" \
  || fail 'quarantined-handoff recovery did not enforce its exact state'
assert_target_retained quarantined-handoff-active

# QUARANTINED-HANDOFF POSITIVE AND NEGATIVE BRACKET. Ordinary stale-clear first
# records the dirty work and quarantines the lease. Neither ordinary
# stale-clear nor the all-slots reaper may cross that boundary. The explicit
# recovery retries every proof, refuses both dirty and clean-but-unpublished
# work, retains authority/CAS checks, and removes the slot only after the exact
# HEAD is remotely durable.
make_fixture quarantined-handoff-recovery
plant_expired_owner_lease quarantined-handoff-recovery >/dev/null
printf 'quarantined work must survive\n' >"$target/quarantined-work.txt"
if run_stale_clear quarantined-handoff-recovery; then
  fail 'dirty expired stale-clear did not quarantine the lease'
fi
grep -Fq 'lease remains quarantined' "$output" \
  || fail 'dirty stale-clear did not report quarantine'
python3 - "$fixture_root/worktree-state.json" <<'PY' \
  || fail 'quarantined handoff did not record an executable, integrity-bound continuation'
import hashlib
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
lease = state["slots"]["quarantined-handoff-recovery"]["owner_sidecar"]["lease"]
handoff = lease["automatic_handoff"]
assert lease["state"] == "quarantined"
assert state["slots"]["quarantined-handoff-recovery"]["status"] == "lease-quarantined"
assert handoff["complete"] is False
assert "--stale-clear --recover-quarantined-handoff" in handoff["continuation"]
recorded = handoff.pop("sha256")
actual = hashlib.sha256(
    json.dumps(handoff, separators=(",", ":"), sort_keys=True).encode()
).hexdigest()
assert recorded == actual, (recorded, actual)
PY
if run_stale_clear quarantined-handoff-recovery; then
  fail 'ordinary stale-clear crossed a quarantined lease'
fi
grep -Fq 'lease is quarantined; after resolving the recorded handoff' "$output" \
  || fail 'ordinary stale-clear did not preserve quarantine and name its exit path'
grep -Fq -- '--recover-quarantined-handoff' "$output" \
  || fail 'ordinary stale-clear refusal omitted the executable recovery flag'
if ! run_stale_clear_all; then
  cat "$output" >&2
  fail 'stale-clear-all failed while excluding a quarantined lease'
fi
grep -Fq 'candidates=0 cleared=0 refused=0' "$output" \
  || fail 'stale-clear-all selected a quarantined lease'
assert_target_retained quarantined-handoff-recovery
python3 - "$fixture_root/worktree-state.json" <<'PY'
import json
import pathlib
import sys
import time

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
lease = state["slots"]["quarantined-handoff-recovery"]["owner_sidecar"]["lease"]
lease["renewed_at"] = int(time.time())
lease["ttl_seconds"] = 600
lease["expires_at"] = lease["renewed_at"] + lease["ttl_seconds"]
path.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
PY
if run_quarantined_handoff_recovery quarantined-handoff-recovery; then
  fail 'quarantined recovery accepted an unexpired lease'
fi
grep -Fq 'lease is not expired' "$output" \
  || fail 'quarantined recovery weakened the expiry gate'
assert_target_retained quarantined-handoff-recovery
python3 - "$fixture_root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
lease = state["slots"]["quarantined-handoff-recovery"]["owner_sidecar"]["lease"]
lease["issued_at"] = 0
lease["renewed_at"] = 0
lease["ttl_seconds"] = 1
lease["expires_at"] = 1
path.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
PY
if run_quarantined_handoff_recovery quarantined-handoff-recovery; then
  fail 'quarantined recovery deleted dirty work'
fi
grep -Fq 'lease remains quarantined' "$output" \
  || fail 'dirty quarantined recovery did not stay quarantined'
grep -Fxq 'quarantined work must survive' "$target/quarantined-work.txt" \
  || fail 'dirty quarantined recovery lost the untracked file'
git -C "$target" add quarantined-work.txt
git -C "$target" commit -q -m 'preserve quarantined work'
if run_quarantined_handoff_recovery quarantined-handoff-recovery; then
  fail 'quarantined recovery deleted an unpublished commit'
fi
grep -Fq 'lease remains quarantined' "$output" \
  || fail 'unpublished quarantined recovery did not stay quarantined'
test -d "$target" || fail 'unpublished quarantined recovery removed the worktree'
git -C "$target" push -q
if run_quarantined_handoff_recovery quarantined-handoff-recovery \
    RELEASE_TEST_OWNER_AUTHORITY_STATE=indeterminate; then
  fail 'indeterminate authority recovered a quarantined handoff'
fi
grep -Fq 'owner authority is INDETERMINATE; stale-clear is fail-closed' "$output" \
  || fail 'quarantined recovery weakened indeterminate-owner refusal'
assert_target_retained quarantined-handoff-recovery
if run_quarantined_handoff_recovery quarantined-handoff-recovery \
    RELEASE_TEST_OWNER_AUTHORITY_STATE=alive; then
  fail 'live ORC owner recovered a quarantined handoff'
fi
grep -Fq 'owner authority is alive; live ORC owner refuses stale-clear' "$output" \
  || fail 'quarantined recovery weakened live-owner refusal'
assert_target_retained quarantined-handoff-recovery
if run_quarantined_handoff_recovery quarantined-handoff-recovery \
    RELEASE_TEST_AUTHORITY_RENEW_LEASE=1; then
  fail 'pre-lock authority recovered a changed quarantined lease'
fi
grep -Fq 'owner authority changed before stale-clear lock acquisition' "$output" \
  || fail 'quarantined recovery did not retain full-lease CAS binding'
assert_target_retained quarantined-handoff-recovery
if ! run_quarantined_handoff_recovery quarantined-handoff-recovery; then
  cat "$output" >&2
  fail 'clean remotely durable quarantined handoff could not recover'
fi
test ! -e "$target" || fail 'successful quarantined recovery retained its target'
python3 - "$fixture_root/worktree-state.json" <<'PY' \
  || fail 'successful quarantined recovery omitted attributable history'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
assert "quarantined-handoff-recovery" not in state["slots"]
record = next(
    row for row in state.get("lease_history", [])
    if row.get("resource") == "slot/quarantined-handoff-recovery"
)
assert record["recovered_quarantined_handoff"] is True
assert len(record["previous_handoff_sha256"]) == 64
assert len(record["handoff_sha256"]) == 64
PY
assert_unrelated_survives

# DOWNSTREAM PROCESS-USE NEGATIVE: recovery may make the handoff complete, but
# it must still enter the ordinary process scan and retain a target referenced
# by a live-process cwd.
make_fixture quarantined-handoff-process-use
plant_expired_owner_lease quarantined-handoff-process-use >/dev/null
git -C "$target" commit -q --allow-empty -m 'unpublished quarantine trigger'
if run_stale_clear quarantined-handoff-process-use; then
  fail 'unpublished stale-clear did not quarantine the process-use fixture'
fi
git -C "$target" push -q
mkdir -p \
  "$fixture_root/proc/31337/fd" \
  "$fixture_root/proc/31337/map_files"
ln -s "$target" "$fixture_root/proc/31337/cwd"
ln -s /bin/sleep "$fixture_root/proc/31337/exe"
ln -s / "$fixture_root/proc/31337/root"
: >"$fixture_root/proc/31337/maps"
if run_quarantined_handoff_recovery quarantined-handoff-process-use; then
  fail 'quarantined recovery bypassed ordinary process-use refusal'
fi
grep -Fq "pid 31337 cwd=$target" "$output" \
  || fail 'quarantined recovery did not report the downstream cwd reference'
assert_target_retained quarantined-handoff-process-use

# DOWNSTREAM REMOVAL-RACE NEGATIVE: a write after every handoff and durability
# proof must still be left to Git's non-force worktree-removal boundary.
make_fixture quarantined-handoff-remove-race
plant_expired_owner_lease quarantined-handoff-remove-race >/dev/null
git -C "$target" commit -q --allow-empty -m 'unpublished quarantine trigger'
if run_stale_clear quarantined-handoff-remove-race; then
  fail 'unpublished stale-clear did not quarantine the remove-race fixture'
fi
git -C "$target" push -q
if run_quarantined_handoff_recovery quarantined-handoff-remove-race \
    RELEASE_TEST_REMOVE_RACE_TARGET="recovery-raced.txt"; then
  fail 'quarantined recovery deleted a write planted at worktree removal'
fi
grep -Fxq 'planted at worktree remove invocation' "$target/recovery-raced.txt" \
  || fail 'quarantined recovery lost the removal-race write'
grep -Eq 'could not remove exact|modified or untracked files' "$output" \
  || fail 'quarantined recovery did not reach Git non-force removal refusal'
assert_target_retained quarantined-handoff-remove-race

# QUARANTINED-HANDOFF RECORD NEGATIVE: the exit requires the exact recorded
# handoff, including its digest. A hand-edited record must not authorize cleanup.
make_fixture quarantined-handoff-tampered
plant_expired_owner_lease quarantined-handoff-tampered >/dev/null
git -C "$target" commit -q --allow-empty -m 'unpublished quarantine trigger'
if run_stale_clear quarantined-handoff-tampered; then
  fail 'unpublished stale-clear did not quarantine the lease'
fi
git -C "$target" push -q
python3 - "$fixture_root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["quarantined-handoff-tampered"]["owner_sidecar"]["lease"]["automatic_handoff"]["sha256"] = "0" * 64
path.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
PY
if run_quarantined_handoff_recovery quarantined-handoff-tampered; then
  fail 'tampered automatic handoff authorized cleanup'
fi
grep -Fq 'automatic handoff sha256 does not match its recorded content' "$output" \
  || fail 'tampered automatic handoff did not fail at its digest'
assert_target_retained quarantined-handoff-tampered
python3 - "$fixture_root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["quarantined-handoff-tampered"]["owner_sidecar"]["lease"].pop("automatic_handoff")
path.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
PY
if run_quarantined_handoff_recovery quarantined-handoff-tampered; then
  fail 'missing automatic handoff authorized cleanup'
fi
grep -Fq 'owner_sidecar lease field automatic_handoff is missing' "$output" \
  || fail 'missing automatic handoff did not fail closed'
assert_target_retained quarantined-handoff-tampered

if [[ ${RELEASE_TEST_STALE_CLEAR_ONLY:-0} == 1 ]]; then
  echo "release-worktree-test: PASS ($fixture_count stale-clear and quarantined-handoff fixtures)"
  exit 0
fi

# SELF-RELEASE POSITIVE: ownership authorizes an exact live agent to act, but
# liveness alone is not use. A clean live owner with no target references may
# generate its handoff and release the slot while the owner process remains.
make_fixture self-release-unused
plant_versioned_live_owner self-release-unused /
if ! run_self_release self-release-unused; then
  cat "$output" >&2
  fail 'exact live owner could not self-release an unused clean slot'
fi
grep -Fq '"authority": "exact-live-owner-self-release"' \
  "$fixture_root/worktree-state.json" \
  || fail 'successful self-release omitted attributable lease history'
if grep -Fq '"self-release-unused"' "$fixture_root/worktree-state.json"; then
  fail 'successful self-release left an active registry row'
fi
test ! -e "$target" || fail 'self-release positive target survived release'
assert_unrelated_survives

# SELF-RELEASE USE NEGATIVE: the same exact owner is not allowed to erase work
# it still uses. The owner identity permits the request; cwd evidence remains a
# separate, load-bearing veto.
make_fixture self-release-in-use
plant_versioned_live_owner self-release-in-use "$target"
if run_self_release self-release-in-use; then
  fail 'exact live owner released a slot it still actively used'
fi
grep -Eq 'live process ownership|live processes use the slot' "$output" \
  || fail 'self-release did not keep actual-use liveness as a separate veto'
assert_target_retained self-release-in-use
rm "$fixture_root/proc/424242/cwd"
ln -s / "$fixture_root/proc/424242/cwd"
if ! run_self_release self-release-in-use; then
  cat "$output" >&2
  fail 'draining owner could not retry after ending its actual slot use'
fi
test ! -e "$target" || fail 'self-release retry target survived release'
assert_unrelated_survives

# PROCESS-OWNERSHIP NEGATIVE: a specifically captured child process has its cwd
# inside the exact target. Cleanup must refuse it and must never signal it.
make_fixture live-owner
(cd "$target" && exec sleep 60) &
live_pid=$!
for _ in 1 2 3 4 5; do
  [[ $(readlink "/proc/$live_pid/cwd" 2>/dev/null || true) == "$target" ]] && break
  sleep 0.05
done
mkdir -p "$fixture_root/proc/$live_pid"
ln -s "$target" "$fixture_root/proc/$live_pid/cwd"
ln -s /bin/sleep "$fixture_root/proc/$live_pid/exe"
if run_release_force live-owner; then
  fail 'worktree used by a live process was removed'
fi
grep -Eq 'live process ownership|live processes use the slot' "$output" \
  || fail 'live process did not trigger the ownership refusal'
kill -0 "$live_pid" 2>/dev/null \
  || fail 'release signaled a process it did not own'
kill "$live_pid"
wait "$live_pid" 2>/dev/null || true
live_pid=
assert_target_retained live-owner

# COOPERATIVE-OWNER NEGATIVE: a recorded live owner must bind through the fresh
# ORC snapshot, its exact tmux pane pid, and that pid's unified cgroup lease.
# Even explicit user force cannot recycle while the bound lease is nonempty.
make_fixture recorded-live-owner
python3 - "$fixture_root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["recorded-live-owner"]["agents"][0]["tmux_pane_id"] = "%424242"
path.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
PY
cat >"$fixture_root/ignored/ci-hub/agent-snapshot.json" <<JSON
{
  "schema_version": 1,
  "captured_at": $(date +%s),
  "agents": [{
    "name": "fixture-recorded-live-owner",
    "status": "working",
    "tmux_pane_id": "%424242"
  }]
}
JSON
printf 'fixture-recorded-live-owner\t%%424242\t424242\n' \
  >"$fixture_root/tmux-panes"
mkdir -p \
  "$fixture_root/proc/424242" \
  "$fixture_root/cgroup/agent.slice/fixture-recorded-live-owner.scope"
printf '0::/agent.slice/fixture-recorded-live-owner.scope\n' \
  >"$fixture_root/proc/424242/cgroup"
printf '424242\n' \
  >"$fixture_root/cgroup/agent.slice/fixture-recorded-live-owner.scope/cgroup.procs"
printf 'populated 1\nfrozen 0\n' \
  >"$fixture_root/cgroup/agent.slice/fixture-recorded-live-owner.scope/cgroup.events"
printf 'DG_AGENT_NAME=fixture-recorded-live-owner\0' \
  >"$fixture_root/proc/424242/environ"
if run_release_force recorded-live-owner; then
  fail 'exact live recorded-owner lease authorized worktree deletion'
fi
grep -Fq "recorded owner 'fixture-recorded-live-owner' remains live in pane %424242" "$output" \
  || fail 'live recorded owner did not bind to its exact pane and cgroup lease'
grep -Fq 'cgroup /agent.slice/fixture-recorded-live-owner.scope, members=1' "$output" \
  || fail 'live recorded owner refusal omitted its nonempty cgroup lease'
assert_target_retained recorded-live-owner

# LEASED-PANE NEGATIVE: a terminal snapshot may omit its pane and the tmux
# window may have been renamed. The durable pane lease itself must still veto
# cleanup; checking only the current window name would lose the incarnation.
make_fixture terminal-renamed-pane
cat >"$fixture_root/ignored/ci-hub/agent-snapshot.json" <<JSON
{
  "schema_version": 1,
  "captured_at": $(date +%s),
  "agents": [{
    "name": "fixture-terminal-renamed-pane",
    "status": "terminated"
  }]
}
JSON
printf 'renamed-window\t%%lease-terminal-renamed-pane\t434343\n' \
  >"$fixture_root/tmux-panes"
if run_release_force terminal-renamed-pane; then
  fail 'renamed durable pane lease authorized worktree deletion'
fi
grep -Fq 'leased tmux pane %lease-terminal-renamed-pane remains under another window identity' "$output" \
  || fail 'terminal owner proof did not check its durable pane identity directly'
assert_target_retained terminal-renamed-pane

# CONTAINER-OWNER NEGATIVE: ORC and tmux report the recorded owner absent, but
# its managed retained container still exists without any target mount. The
# canonical Podman authority, not a correlated process scan, must refuse it.
make_fixture recorded-owner-container
cat >"$fixture_root/podman.json" <<JSON
[{
  "Id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "Names": ["recorded-owner-container"],
  "State": "stopped",
  "Labels": {
    "io.dev-hermit.agent-podman": "v1",
    "io.dev-hermit.owner-agent": "fixture-recorded-owner-container",
    "io.dev-hermit.owner-invocation": "retired-invocation",
    "io.dev-hermit.owner-pane": "%retired",
    "io.dev-hermit.owner-task": "fixture-recorded-owner-container",
    "io.dev-hermit.lifetime": "task"
  },
  "Mounts": []
}]
JSON
cat >"$fixture_root/ignored/ci-hub/agent-containers.json" <<JSON
{
  "schema_version": 1,
  "containers": {
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": {
      "container_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "owner_agent": "fixture-recorded-owner-container",
      "owner_invocation": "retired-invocation",
      "owner_pane": "%retired",
      "task": "fixture-recorded-owner-container",
      "lifetime": "task",
      "updated_at": 1
    }
  }
}
JSON
if run_release recorded-owner-container; then
  fail 'ORC-absent recorded-owner container authorized deletion'
fi
grep -Fq 'recorded-owner-container' "$output" \
  || fail 'recorded-owner container refusal was not bound to Podman evidence'
assert_target_retained recorded-owner-container

# CONTAINER-MOUNT NEGATIVE: an unmanaged container owned by nobody in the slot
# still holds an ancestor bind mount. Effective mount overlap alone must refuse.
make_fixture unmanaged-target-mount
cat >"$fixture_root/podman.json" <<JSON
[{
  "Id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "Names": ["unmanaged-target-mount"],
  "State": "running",
  "Labels": null,
  "Mounts": [{"Source": "$fixture_root/worktrees/slots/unmanaged-target-mount"}]
}]
JSON
if run_release unmanaged-target-mount; then
  fail 'unmanaged target-mounted container authorized deletion'
fi
grep -Fq 'target-overlapping-mount' "$output" \
  || fail 'target-mounted container refusal omitted effective mount evidence'
assert_target_retained unmanaged-target-mount

# CONTAINER-AUTHORITY NEGATIVES: absence is not established by a failed,
# malformed, or incomplete engine query.
make_fixture podman-unavailable
if run_release podman-unavailable RELEASE_TEST_PODMAN_FAIL=1; then
  fail 'unavailable Podman authority authorized deletion'
fi
grep -Fq 'planted podman authority failure' "$output" \
  || fail 'Podman authority failure did not fail closed'
assert_target_retained podman-unavailable

make_fixture podman-malformed
if run_release podman-malformed RELEASE_TEST_PODMAN_MALFORMED=1; then
  fail 'malformed Podman authority authorized deletion'
fi
grep -Fq 'parse podman ps JSON' "$output" \
  || fail 'malformed Podman authority did not fail closed'
assert_target_retained podman-malformed

make_fixture podman-incomplete
cat >"$fixture_root/podman.json" <<JSON
[{"Id":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","Names":["incomplete"],"State":"running","Labels":null,"Mounts":[]}]
JSON
if run_release podman-incomplete RELEASE_TEST_PODMAN_INCOMPLETE=1; then
  fail 'incomplete Podman inspect authorized deletion'
fi
grep -Fq 'podman inspect returned 0 rows' "$output" \
  || fail 'incomplete Podman inspection did not fail closed'
assert_target_retained podman-incomplete

# OWNER-LEASE NEGATIVES: legacy state without an exact lease cannot authorize
# cleanup, and an empty direct cgroup.procs is irrelevant when descendants keep
# the recorded subtree populated.
make_fixture owner-lease-missing
python3 - "$fixture_root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
owner = next(iter(state["slots"].values()))["agents"][0]
owner.pop("tmux_pane_id")
owner.pop("cgroup_path")
path.write_text(json.dumps(state, indent=2) + "\n")
PY
if run_release owner-lease-missing; then
  fail 'legacy owner without pane/cgroup lease authorized deletion'
fi
grep -Fq 'lacks tmux_pane_id lease data' "$output" \
  || fail 'legacy owner lease refusal omitted migration instruction'
assert_target_retained owner-lease-missing

make_fixture owner-subtree-populated
mkdir -p "$fixture_root/cgroup/agent.slice/fixture-owner-subtree-populated.scope"
: >"$fixture_root/cgroup/agent.slice/fixture-owner-subtree-populated.scope/cgroup.procs"
printf 'populated 1\nfrozen 0\n' \
  >"$fixture_root/cgroup/agent.slice/fixture-owner-subtree-populated.scope/cgroup.events"
if run_release owner-subtree-populated; then
  fail 'populated owner cgroup subtree authorized deletion'
fi
grep -Fq 'retains populated cgroup subtree' "$output" \
  || fail 'descendant-populated owner lease was not observed through cgroup.events'
assert_target_retained owner-subtree-populated

# TARGET-REFERENCE NEGATIVE: cwd/exe/root/maps are all unrelated, while one
# readable open file descriptor alone references the target. It must refuse.
make_fixture open-fd-owner
mkdir -p \
  "$fixture_root/proc/31337/fd" \
  "$fixture_root/proc/31337/map_files"
ln -s / "$fixture_root/proc/31337/cwd"
ln -s /bin/sleep "$fixture_root/proc/31337/exe"
ln -s / "$fixture_root/proc/31337/root"
ln -s "$target/parent.txt" "$fixture_root/proc/31337/fd/7"
printf 'DG_AGENT_NAME=unrelated-fd-holder\0' >"$fixture_root/proc/31337/environ"
: >"$fixture_root/proc/31337/maps"
if run_release_force open-fd-owner; then
  fail 'open-fd-only target ownership authorized worktree deletion'
fi
grep -Fq "pid 31337 fd/7=$target/parent.txt" "$output" \
  || fail 'open-fd-only target reference was not observed'
assert_target_retained open-fd-owner

# OWNER-AUTHORITY NEGATIVE: an unavailable canonical ORC snapshot is not
# absence evidence, even when explicit force is requested.
make_fixture owner-authority-unavailable
rm "$fixture_root/ignored/ci-hub/agent-snapshot.json"
if run_release_force owner-authority-unavailable; then
  fail 'unavailable cooperative-owner authority authorized deletion'
fi
grep -Fq 'canonical ORC owner snapshot unavailable' "$output" \
  || fail 'missing ORC snapshot did not fail owner resolution closed'
assert_target_retained owner-authority-unavailable

# OWNER-AUTHORITY AMBIGUITY NEGATIVE: a pane id cannot identify two processes.
# A malformed/ambiguous canonical tmux query is uncertainty, never absence.
make_fixture owner-authority-ambiguous
cat >"$fixture_root/ignored/ci-hub/agent-snapshot.json" <<JSON
{
  "schema_version": 1,
  "captured_at": $(date +%s),
  "agents": [{
    "name": "fixture-owner-authority-ambiguous",
    "status": "working",
    "tmux_pane_id": "%99"
  }]
}
JSON
printf 'fixture-owner-authority-ambiguous\t%%99\t99991\nother-agent\t%%99\t99992\n' \
  >"$fixture_root/tmux-panes"
if run_release_force owner-authority-ambiguous; then
  fail 'ambiguous cooperative-owner authority authorized deletion'
fi
grep -Fq 'canonical tmux pane query duplicated %99' "$output" \
  || fail 'ambiguous tmux owner query did not fail closed'
assert_target_retained owner-authority-ambiguous

# LEGACY-OWNER NEGATIVE: an ORC-live registry owner without a canonical pane
# identity cannot be proven absent. It must be migrated/recycled, not guessed
# away from a correlated status or name.
make_fixture legacy-owner-unresolved
cat >"$fixture_root/ignored/ci-hub/agent-snapshot.json" <<JSON
{
  "schema_version": 1,
  "captured_at": $(date +%s),
  "agents": [{
    "name": "fixture-legacy-owner-unresolved",
    "status": "working"
  }]
}
JSON
if run_release_force legacy-owner-unresolved; then
  fail 'unresolved legacy owner authorized deletion'
fi
grep -Fq "recorded live owner 'fixture-legacy-owner-unresolved' has no canonical tmux pane identity" "$output" \
  || fail 'unresolved legacy owner did not fail cooperative resolution closed'
assert_target_retained legacy-owner-unresolved

# UNRELATED-PROTECTED POSITIVE: a system service with a different agent name
# and system.slice cgroup may have a protected process-evidence namespace. A
# fresh ORC/tmux query has already established every recorded owner's absence,
# so unrelated PermissionDenied evidence is not a global same-UID veto.
make_fixture unrelated-protected-service
protected_pid_dir="$fixture_root/proc/525252"
locked_proc_dir="$protected_pid_dir/map_files"
mkdir -p \
  "$protected_pid_dir/fd" \
  "$locked_proc_dir" \
  "$fixture_root/cgroup/system.slice/protected-systemd.service"
ln -s / "$protected_pid_dir/cwd"
ln -s /usr/lib/systemd/systemd "$protected_pid_dir/exe"
ln -s / "$protected_pid_dir/root"
printf 'DG_AGENT_NAME=systemd-helper\0' >"$protected_pid_dir/environ"
printf '0::/system.slice/protected-systemd.service\n' >"$protected_pid_dir/cgroup"
printf '525252\n' \
  >"$fixture_root/cgroup/system.slice/protected-systemd.service/cgroup.procs"
: >"$protected_pid_dir/maps"
chmod 000 "$locked_proc_dir"
cat >"$fixture_root/podman.json" <<JSON
[{
  "Id": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
  "Names": ["unrelated-container"],
  "State": "running",
  "Labels": null,
  "Mounts": [{"Source": "$fixture_root/unrelated"}]
}]
JSON
if ls -A "$locked_proc_dir" >/dev/null 2>&1; then
  chmod 700 "$locked_proc_dir"
  locked_proc_dir=
  fail 'protected-service fixture is inert: map_files remained readable'
fi
if ! run_release unrelated-protected-service; then
  chmod 700 "$locked_proc_dir"
  locked_proc_dir=
  cat "$output" >&2
  fail 'unrelated protected system service globally vetoed safe cleanup'
fi
chmod 700 "$locked_proc_dir"
locked_proc_dir=
test -d "$protected_pid_dir" \
  || fail 'release modified unrelated protected process evidence'
test ! -e "$target" || fail 'unrelated-protected positive target survived release'
assert_unrelated_survives

# TOKEN NEGATIVE: path-like slot text must be rejected before state/path lookup.
make_fixture valid-token
if run_release '../valid-token'; then
  fail 'path-like slot token was accepted'
fi
grep -Fq "invalid slot name: '../valid-token'" "$output" \
  || fail 'invalid slot token did not reach the token guard'
assert_target_retained valid-token

printf 'release-worktree-test: PASS (%d fixtures: clean/recovery/force positives plus planted refusals; unrelated path+worktree survived)\n' \
  "$fixture_count"
