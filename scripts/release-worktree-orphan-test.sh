#!/usr/bin/env bash
# Bracket coordinator-only orphan-residue finalization in disposable fixtures.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
suite_root="$(mktemp -d "${TMPDIR:-/tmp}/release-worktree-test.orphan.XXXXXX")"
trap 'rm -rf -- "$suite_root"' EXIT

fail() {
  echo "release-worktree-orphan-test: FAIL: $*" >&2
  exit 1
}

cat >"$suite_root/podman" <<'PY'
#!/usr/bin/env python3
import json
import sys

if sys.argv[1:] == ["ps", "-a", "--format", "json"]:
    print(json.dumps([]))
    raise SystemExit(0)
print(f"unsupported fake podman argv: {sys.argv[1:]}", file=sys.stderr)
raise SystemExit(2)
PY
chmod +x "$suite_root/podman"

make_fixture() {
  local name=$1
  fixture_root="$suite_root/$name"
  primary="$fixture_root/hermit"
  target="$fixture_root/worktrees/slots/$name/hermit"
  output="$fixture_root/finalize.out"
  mkdir -p \
    "$primary" "$fixture_root/reverie" "$fixture_root/liteinst2" \
    "$fixture_root/scripts" "$fixture_root/bin" "$fixture_root/worktrees/slots/$name" \
    "$fixture_root/ignored/ci-hub" "$fixture_root/proc" \
    "$fixture_root/cgroup" "$target/target/debug" \
    "$target/.safe-ci-dag-runner/profiles" "$target/.dagrun/profiles" \
    "$fixture_root/unrelated"
  : >"$fixture_root/.gitmodules"
  cp "$script_dir/check-worktree-registry.rs" "$fixture_root/scripts/"
  cp "$script_dir/worktree_registry.rs" "$fixture_root/scripts/"
  cp "$script_dir/agent-podman.rs" "$fixture_root/scripts/"
  cp "$script_dir/agent_snapshot.rs" "$fixture_root/scripts/"
  cp "$suite_root/podman" "$fixture_root/bin/podman"
  printf 'unrelated survives\n' >"$fixture_root/unrelated/sentinel"

  git -C "$fixture_root" init -q -b main
  git -C "$fixture_root" config user.email orphan-test@example.invalid
  git -C "$fixture_root" config user.name orphan-test
  git -C "$fixture_root" add .gitmodules
  git -C "$fixture_root" commit -q -m 'seed parent checkout'
  git -C "$primary" init -q -b main
  git -C "$primary" config user.email orphan-test@example.invalid
  git -C "$primary" config user.name orphan-test
  printf 'primary\n' >"$primary/seed"
  git -C "$primary" add seed
  git -C "$primary" commit -q -m 'seed product primary'
  git -C "$fixture_root/reverie" init -q -b main
  git -C "$fixture_root/liteinst2" init -q -b main

  printf 'object bytes\n' >"$target/target/debug/generated.o"
  printf 'Hermit compatibility results\nprofile\tfull\n' \
    >"$target/target/validate-results.txt"
  printf 'timestamp,git_sha,result\nnow,deadbeef,fail\n' \
    >"$target/.safe-ci-dag-runner/profiles/run.csv"
  printf 'timestamp,git_sha,result\nnow,feedface,fail\n' \
    >"$target/.dagrun/profiles/run.csv"

  cat >"$fixture_root/worktree-state.json" <<JSON
{
  "version": 3,
  "updated": "2026-08-28T00:00:00Z",
  "slots": {
    "$name": {
      "agents": [{"name":"fixture-$name","read_only":false,"task":"fixture-$name"}],
      "allocated": "2026-08-28T00:00:00Z",
      "updated": "2026-08-28T00:00:00Z",
      "hermit_branch":"codex-coord", "hermit_path":"worktrees/slots/$name/hermit",
      "reverie_branch":"-", "reverie_path":"worktrees/slots/$name/reverie",
      "liteinst2_branch":"-", "liteinst2_path":"worktrees/slots/$name/liteinst2",
      "task":"fixture-$name", "purpose":"orphan fixture", "status":"released"
    }
  }
}
JSON
  cat >"$fixture_root/worktrees/ACTIVE.md" <<MD
# Fixture registry
<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->
| Slot | Agent | HermitBranch | ReverieBranch | LiteInst2Branch | Task | Status | ReadOnly |
| --- | --- | --- | --- | --- | --- | --- | --- |
| $name | fixture-$name | codex-coord | - | - | fixture-$name | released | no |
<!-- END worktree-state -->
MD
  cat >"$fixture_root/ignored/ci-hub/agent-snapshot.json" <<JSON
{"schema_version":1,"captured_at":$(date +%s),"agents":[]}
JSON
  printf '[]\n' >"$fixture_root/systemd-units.json"
  : >"$fixture_root/tmux-panes"
  : >"$fixture_root/kernel-locks"
  : >"$fixture_root/mountinfo"
  printf '[]\n' >"$fixture_root/podman.json"
  cat >"$fixture_root/ignored/ci-hub/agent-containers.json" <<'JSON'
{"schema_version":1,"containers":{}}
JSON
}

run_finalize() {
  local name=$1
  shift
  (cd "$fixture_root" && env \
    HERMIT_RELEASE_TEST_PROC_ROOT="$fixture_root/proc" \
    HERMIT_RELEASE_TEST_CGROUP_ROOT="$fixture_root/cgroup" \
    HERMIT_RELEASE_TEST_TMUX_PANES="$fixture_root/tmux-panes" \
    HERMIT_RELEASE_TEST_PODMAN_BIN="$fixture_root/bin/podman" \
    HERMIT_RELEASE_TEST_SYSTEMD_UNITS="$fixture_root/systemd-units.json" \
    HERMIT_RELEASE_TEST_LOCKS="$fixture_root/kernel-locks" \
    HERMIT_RELEASE_TEST_MOUNTINFO="$fixture_root/mountinfo" \
    DEV_HERMIT_CONTAINER_STATE="$fixture_root/ignored/ci-hub/agent-containers.json" \
    "$@" "$script_dir/release-worktree.rs" \
      --slot "$name" --clean --coordinator-finalize-orphan-residue \
      --orphan-recovery-note "fixture recovery for $name") >"$output" 2>&1
}

assert_retained() {
  local name=$1
  test -d "$target" || fail "$name refusal removed residue"
  grep -Fq "\"$name\"" "$fixture_root/worktree-state.json" \
    || fail "$name refusal removed registry state"
  grep -Fxq 'unrelated survives' "$fixture_root/unrelated/sentinel" \
    || fail "$name changed unrelated content"
}

# FALSE-ASCENT NEGATIVE + POSITIVE FINALIZATION: checker must identify the
# parent checkout proxy; the explicit finalizer then copies evidence, fences,
# hashes, removes generated residue, and drops only the released row.
make_fixture orphan-positive
cat >"$fixture_root/systemd-units.json" <<JSON
[{"unit":"orphan-positive-validate.service","active":"failed","details":"WorkingDirectory=$target"}]
JSON
if "$fixture_root/scripts/check-worktree-registry.rs" --root "$fixture_root" \
    >"$fixture_root/check.out" 2>&1; then
  fail 'checker accepted parent-ascent orphan residue'
fi
grep -Fq "actual=unreadable:git top-level $fixture_root does not equal requested checkout $target" \
  "$fixture_root/check.out" || fail 'checker did not expose exact parent ascent'
if ! run_finalize orphan-positive; then
  cat "$output" >&2
  fail 'qualifying released orphan residue was refused'
fi
test ! -e "$fixture_root/worktrees/slots/orphan-positive" \
  || fail 'positive orphan slot directory survived'
if grep -Fq 'orphan-positive' "$fixture_root/worktree-state.json"; then
  fail 'positive orphan row survived registry finalization'
fi
evidence_dir=$(find "$fixture_root/ignored/worktree-recovery/orphan-positive" \
  -mindepth 1 -maxdepth 1 -type d -print -quit)
test -n "$evidence_dir" || fail 'positive finalization created no evidence directory'
test -f "$evidence_dir/manifest.json" || fail 'positive finalization omitted manifest'
test -f "$evidence_dir/completion.json" || fail 'positive finalization omitted completion'
cmp "$evidence_dir/hermit/target/validate-results.txt" \
  <(printf 'Hermit compatibility results\nprofile\tfull\n') \
  || fail 'validate evidence copy changed bytes'
cmp "$evidence_dir/hermit/.safe-ci-dag-runner/profiles/run.csv" \
  <(printf 'timestamp,git_sha,result\nnow,deadbeef,fail\n') \
  || fail 'profile evidence copy changed bytes'
jq -e '.targets[0].inventory.files | length == 4 and
       (map(select(.copied == true)) | length == 3)' "$evidence_dir/manifest.json" >/dev/null \
  || fail 'manifest did not bind all files and copied evidence count'
cmp "$evidence_dir/hermit/.dagrun/profiles/run.csv" \
  <(printf 'timestamp,git_sha,result\nnow,feedface,fail\n') \
  || fail 'renamed profile evidence copy changed bytes'
grep -Fxq 'unrelated survives' "$fixture_root/unrelated/sentinel" \
  || fail 'positive finalization changed unrelated content'
if ! run_finalize orphan-positive; then
  cat "$output" >&2
  fail 'completed orphan finalization was not idempotent'
fi
grep -Fq 'already finalized' "$output" || fail 'idempotent replay was not reported'

# REGISTRATION NEGATIVE: an actual product worktree is normal-release territory.
make_fixture orphan-registered
rm -rf -- "$target"
git -C "$primary" worktree add -q -b registered-orphan "$target" main
if run_finalize orphan-registered; then
  fail 'orphan mode removed a registered product worktree'
fi
grep -Fq 'expected zero registrations' "$output" \
  || fail 'registered-worktree refusal omitted registration authority'
git -C "$primary" worktree list --porcelain | grep -Fxq "worktree $target" \
  || fail 'registered-worktree refusal removed physical registration'

# MATERIAL-WORK NEGATIVE: anything outside the generated allowlist is retained.
make_fixture orphan-source
mkdir -p "$target/src"
printf 'precious source\n' >"$target/src/lib.rs"
if run_finalize orphan-source; then
  fail 'orphan mode deleted source-like residue'
fi
grep -Fq 'non-generated top-level entry' "$output" \
  || fail 'source-like refusal did not come from generated allowlist'
assert_retained orphan-source

# PATH-ALIAS NEGATIVE: a lexical alias in registry state is not canonical
# authority even when it resolves to the same residue directory.
make_fixture orphan-path-alias
python3 - "$fixture_root/worktree-state.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
state = json.loads(path.read_text())
state["slots"]["orphan-path-alias"]["hermit_path"] = \
    "worktrees/slots/orphan-path-alias/./hermit"
path.write_text(json.dumps(state, indent=2) + "\n")
PY
if run_finalize orphan-path-alias; then
  fail 'aliased registry path authorized orphan deletion'
fi
grep -Fq "records noncanonical hermit_path; expected exact path 'worktrees/slots/orphan-path-alias/hermit'" \
  "$output" || fail 'path-alias refusal omitted exact canonical authority'
assert_retained orphan-path-alias

# CARGO ARTIFACT MARKER POSITIVE: Cargo's exact target-local lock marker is
# generated residue even though it has no conventional artifact extension.
make_fixture orphan-cargo-artifact-lock
printf 'cargo artifact lock\n' >"$target/target/debug/.cargo-artifact-lock"
if ! run_finalize orphan-cargo-artifact-lock; then
  cat "$output" >&2
  fail 'exact target-local Cargo artifact marker was refused'
fi
test ! -e "$fixture_root/worktrees/slots/orphan-cargo-artifact-lock" \
  || fail 'Cargo artifact marker positive retained the slot directory'

# ADJACENT UNKNOWN-FILE NEGATIVE: classification is the exact marker name,
# not a prefix that authorizes neighboring files in the generated directory.
make_fixture orphan-unknown-file
printf 'preserve me\n' >"$target/target/debug/.cargo-artifact-lock.backup"
if run_finalize orphan-unknown-file; then
  fail 'unrecognized nested file authorized orphan deletion'
fi
grep -Fq 'contains unrecognized generated file' "$output" \
  || fail 'unknown-file refusal omitted recursive allowlist authority'
assert_retained orphan-unknown-file

# NESTED-MOUNT NEGATIVE: a mountinfo row is direct authority that another
# filesystem remains rooted below the candidate residue. This is a plain text
# fixture; no mount is created.
make_fixture orphan-nested-mount
printf '29 23 0:25 / %s rw,relatime - tmpfs tmpfs rw\n' \
  "$target/target" >"$fixture_root/mountinfo"
if run_finalize orphan-nested-mount; then
  fail 'nested mount authority authorized orphan deletion'
fi
grep -Fq "nested mount $target/target remains inside orphan residue $target" "$output" \
  || fail 'nested-mount refusal omitted exact mount authority'
assert_retained orphan-nested-mount

# LIVE-UNIT NEGATIVE: inactive failed history is allowed above; a live unit is
# a current consumer and must retain the residue.
make_fixture orphan-live-unit
cat >"$fixture_root/systemd-units.json" <<JSON
[{"unit":"orphan-live-unit.service","active":"active","details":"WorkingDirectory=$target"}]
JSON
if run_finalize orphan-live-unit; then
  fail 'live systemd unit authorized orphan deletion'
fi
grep -Fq 'live systemd unit orphan-live-unit.service' "$output" \
  || fail 'live-unit refusal omitted exact unit'
assert_retained orphan-live-unit

# LIVE-OWNER NEGATIVE: a fresh ORC snapshot is a direct owner authority, so a
# recorded live owner must retain the residue before any cleanup is armed.
make_fixture orphan-live-owner
cat >"$fixture_root/ignored/ci-hub/agent-snapshot.json" <<JSON
{"schema_version":1,"captured_at":$(date +%s),"agents":[{"name":"fixture-orphan-live-owner","status":"working"}]}
JSON
if run_finalize orphan-live-owner; then
  fail 'live ORC owner authorized orphan deletion'
fi
grep -Fq "recorded orphan owner 'fixture-orphan-live-owner' is still live in ORC" "$output" \
  || fail 'live-owner refusal omitted exact owner authority'
assert_retained orphan-live-owner

# KERNEL-LOCK NEGATIVE: the inode/device tuple in /proc/locks binds a live
# kernel consumer to the exact residue file, independent of process labels.
make_fixture orphan-live-lock
python3 - "$target/target/debug/generated.o" "$fixture_root/kernel-locks" <<'PY'
import os
import pathlib
import sys

source = os.stat(sys.argv[1])
pathlib.Path(sys.argv[2]).write_text(
    f"1: FLOCK ADVISORY WRITE 4242 "
    f"{os.major(source.st_dev):x}:{os.minor(source.st_dev):x}:{source.st_ino} 0 EOF\n",
    encoding="utf-8",
)
PY
if run_finalize orphan-live-lock; then
  fail 'live kernel lock authorized orphan deletion'
fi
grep -Fq 'live kernel lock references orphan residue' "$output" \
  || fail 'live-lock refusal omitted inode/device authority'
assert_retained orphan-live-lock

# JOURNAL/CRASH POSITIVE: crash after the atomic path fence must leave the
# evidence and journal replayable; an exact retry completes without recopying
# or trusting mere absence.
make_fixture orphan-crash-fence
crash_marker="$fixture_root/crash-after-fence"
: >"$crash_marker"
if run_finalize orphan-crash-fence \
    HERMIT_RELEASE_TEST_CRASH_AFTER_ORPHAN_FENCE="$crash_marker"; then
  fail 'fence crash injection unexpectedly completed'
fi
grep -Fxq 'post-orphan-fence crash injected' "$crash_marker" \
  || fail 'fence crash injection did not reach its boundary'
grep -Fq '"phase": "fenced"' "$fixture_root/worktree-state.json" \
  || fail 'fence crash did not durably record phase'
test ! -e "$target" || fail 'fence crash retained canonical path'
find "$fixture_root/worktrees/slots/orphan-crash-fence" -maxdepth 1 \
  -name '.hermit.orphan-residue-*' -type d | grep -q . \
  || fail 'fence crash left no exact fenced residue'
if (cd "$fixture_root" && "$script_dir/release-worktree.rs" \
    --slot orphan-crash-fence --clean) >"$output" 2>&1; then
  fail 'ordinary cleanup consumed an unfinished orphan journal'
fi
grep -Fq 'rerun ./scripts/release-worktree.rs --slot orphan-crash-fence --clean --coordinator-finalize-orphan-residue with the exact original --orphan-recovery-note "fixture recovery for orphan-crash-fence"' \
  "$output" || fail 'orphan-journal diagnostic pointed at the wrong recovery command'
if ! run_finalize orphan-crash-fence; then
  cat "$output" >&2
  fail 'journaled fence recovery failed'
fi
test ! -e "$fixture_root/worktrees/slots/orphan-crash-fence" \
  || fail 'fence recovery retained slot directory'

# EVIDENCE-TAMPER NEGATIVE: copied evidence is authority, not a cache.
make_fixture orphan-evidence-tamper
evidence_marker="$fixture_root/crash-after-evidence"
: >"$evidence_marker"
if run_finalize orphan-evidence-tamper \
    HERMIT_RELEASE_TEST_CRASH_AFTER_ORPHAN_EVIDENCE="$evidence_marker"; then
  fail 'evidence crash injection unexpectedly completed'
fi
tamper_dir=$(find "$fixture_root/ignored/worktree-recovery/orphan-evidence-tamper" \
  -mindepth 1 -maxdepth 1 -type d -print -quit)
printf 'tampered\n' >"$tamper_dir/hermit/target/validate-results.txt"
if run_finalize orphan-evidence-tamper; then
  fail 'tampered evidence copy authorized residue deletion'
fi
grep -Fq 'does not match its manifest' "$output" \
  || fail 'evidence tamper refusal omitted manifest binding'
assert_retained orphan-evidence-tamper

echo 'release-worktree-orphan-test: PASS (exact-root ascent refused; exact Cargo artifact marker and released generated residue cleaned; idempotent and fence-crash recovery; marker-lookalike/path-alias/unknown-file/nested-mount/registration/source/live-unit/live-owner/live-lock/evidence-tamper negatives retained)'
