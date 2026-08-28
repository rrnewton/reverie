#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
subject="$script_dir/release-worktree.rs"
registry_subject="$script_dir/check-worktree-registry.rs"
podman_subject="$script_dir/agent-podman.rs"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/release-worktree-test.scope.XXXXXX")"
trap 'rm -rf -- "$test_root"' EXIT

fail() {
  echo "release-worktree-target-scope-test: FAIL: $*" >&2
  exit 1
}

init_root() {
  local root=$1
  mkdir -p \
    "$root/scripts" \
    "$root/worktrees" \
    "$root/moved" \
    "$root/remotes" \
    "$root/bin" \
    "$root/ignored/ci-hub" \
    "$root/test-proc" \
    "$root/test-cgroup/fixture-owner"
  : >"$root/test-tmux-panes"
  : >"$root/test-cgroup/fixture-owner/cgroup.procs"
  printf '%s\n' 'populated 0' >"$root/test-cgroup/fixture-owner/cgroup.events"
  printf '{"schema_version":1,"captured_at":%s,"agents":[]}\n' \
    "$(date +%s)" >"$root/ignored/ci-hub/agent-snapshot.json"
  printf '%s\n' '{"schema_version":1,"containers":{}}' \
    >"$root/ignored/ci-hub/agent-containers.json"
  printf '%s\n' '[]' >"$root/podman.json"
  cat >"$root/bin/podman" <<'PY'
#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

rows = json.loads(Path(os.environ["RELEASE_TEST_PODMAN_STATE"]).read_text())
args = sys.argv[1:]
if args[:3] == ["ps", "-a", "--format"]:
    print(json.dumps(rows))
elif args[:2] == ["container", "inspect"] and len(args) == 3:
    row = next((candidate for candidate in rows if candidate["Id"] == args[2]), None)
    if row is None:
        raise SystemExit(1)
    print(json.dumps([row]))
else:
    print(f"unsupported fake podman argv: {args}", file=sys.stderr)
    raise SystemExit(2)
PY
  chmod +x "$root/bin/podman"
  : >"$root/.gitmodules"
  cat >"$root/scripts/check-worktree-registry.rs" <<EOF
#!/usr/bin/env bash
exec "$registry_subject" "\$@"
EOF
  cat >"$root/scripts/agent-podman.rs" <<EOF
#!/usr/bin/env bash
exec "$podman_subject" "\$@"
EOF
  chmod +x "$root/scripts/check-worktree-registry.rs"
  chmod +x "$root/scripts/agent-podman.rs"
  printf '%s\n' '# fixture ACTIVE' >"$root/worktrees/ACTIVE.md"
  for product in hermit reverie liteinst2; do
    git init -q "$root/$product"
    git -C "$root/$product" config user.email test@example.invalid
    git -C "$root/$product" config user.name test
    printf '%s\n' seed >"$root/$product/seed"
    git -C "$root/$product" add seed
    git -C "$root/$product" commit -qm seed
    git -C "$root/$product" branch -M main
    git init -q --bare "$root/remotes/$product.git"
    git -C "$root/$product" remote add origin "$root/remotes/$product.git"
    git -C "$root/$product" push -q -u origin main
  done
}

add_target() {
  local root=$1 product=$2 slot=$3
  mkdir -p "$root/worktrees/slots/$slot"
  git -C "$root/$product" worktree add -q --detach \
    "$root/worktrees/slots/$slot/$product" HEAD
}

add_missing_unrelated() {
  local root=$1 product=$2 name=$3
  local stale="$root/worktrees/slots/$name-$product"
  git -C "$root/$product" worktree add -q --detach "$stale" HEAD
  mv "$stale" "$root/moved/$name-$product"
  printf '%s\n' "$stale"
}

assert_registered() {
  local root=$1 product=$2 path=$3
  git -C "$root/$product" worktree list --porcelain \
    | grep -Fxq "worktree $path" \
    || fail "$product registry lost unrelated entry $path"
}

write_state() {
  local root=$1 slot=$2 hbranch=$3 rbranch=$4 lbranch=$5
  local hpath=${6:-worktrees/slots/$slot/hermit}
  local rpath=${7:-worktrees/slots/$slot/reverie}
  local lpath=${8:-worktrees/slots/$slot/liteinst2}
  printf '%s\n' \
    '{' \
    '  "version": 3,' \
    '  "slots": {' \
    "    \"$slot\": {" \
    '      "agents": [{"name": "fixture-owner", "read_only": false, "tmux_pane_id": "%fixture", "cgroup_path": "/fixture-owner"}],' \
    "      \"hermit_branch\": \"$hbranch\"," \
    "      \"hermit_path\": \"$hpath\"," \
    "      \"reverie_branch\": \"$rbranch\"," \
    "      \"reverie_path\": \"$rpath\"," \
    "      \"liteinst2_branch\": \"$lbranch\"," \
    "      \"liteinst2_path\": \"$lpath\"," \
    '      "task": "fixture", "status": "active"' \
    '    }' \
    '  }' \
    '}' >"$root/worktree-state.json"
  printf '%s\n' \
    '# fixture ACTIVE' \
    '<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->' \
    '| Slot | Agents / tasks | Hermit branch | Reverie branch | LiteInst2 branch | Task | Status | Shared |' \
    '| --- | --- | --- | --- | --- | --- | --- | --- |' \
    "| $slot | fixture-owner | $hbranch | $rbranch | $lbranch | fixture | active | no |" \
    '<!-- END worktree-state -->' >"$root/worktrees/ACTIVE.md"
}

add_unrelated_drift() {
  local root=$1 name=${2:-unrelated}
  add_target "$root" hermit "$name"
  python3 - "$root/worktree-state.json" "$root/worktrees/ACTIVE.md" "$name" <<'PY'
import json
import pathlib
import sys

state_path, active_path = map(pathlib.Path, sys.argv[1:3])
name = sys.argv[3]
state = json.loads(state_path.read_text())
state["slots"][name] = {
    "agents": [{"name": "unrelated-owner", "read_only": False}],
    "hermit_branch": "wrong-branch",
    "hermit_path": f"worktrees/slots/{name}/hermit",
    "reverie_branch": "-",
    "reverie_path": f"worktrees/slots/{name}/reverie",
    "liteinst2_branch": "-",
    "liteinst2_path": f"worktrees/slots/{name}/liteinst2",
    "task": "unrelated-task",
    "status": "active",
}
state_path.write_text(json.dumps(state, indent=2) + "\n")
active = active_path.read_text()
row = f"| {name} | unrelated-owner | wrong-branch | - | - | unrelated-task | active | no |\n"
active_path.write_text(active.replace(
    "<!-- END worktree-state -->", row + "<!-- END worktree-state -->"
))
PY
}

run_release() {
  local root=$1 slot=$2
  (
    cd "$root"
    HERMIT_RELEASE_TEST_PROC_ROOT="$root/test-proc" \
      HERMIT_RELEASE_TEST_CGROUP_ROOT="$root/test-cgroup" \
      HERMIT_RELEASE_TEST_TMUX_PANES="$root/test-tmux-panes" \
      HERMIT_RELEASE_TEST_PODMAN_BIN="$root/bin/podman" \
      RELEASE_TEST_PODMAN_STATE="$root/podman.json" \
      "$subject" --slot "$slot" --clean
  )
}

run_recovery() {
  local root=$1 slot=$2
  (
    cd "$root"
    HERMIT_RELEASE_TEST_PROC_ROOT="$root/test-proc" \
      HERMIT_RELEASE_TEST_CGROUP_ROOT="$root/test-cgroup" \
      HERMIT_RELEASE_TEST_TMUX_PANES="$root/test-tmux-panes" \
      HERMIT_RELEASE_TEST_PODMAN_BIN="$root/bin/podman" \
      RELEASE_TEST_PODMAN_STATE="$root/podman.json" \
      "$subject" --slot "$slot" --clean \
        --recover-submodule-cleanup
  )
}

# Positive: exact targets disappear while unrelated prunable admin entries in
# all three primaries remain. This brackets the original fleet-wide incident.
root="$test_root/exact"
init_root "$root"
for product in hermit reverie liteinst2; do
  add_target "$root" "$product" target
  eval "stale_$product=\$(add_missing_unrelated \"$root\" \"$product\" unrelated)"
done
write_state "$root" target detached detached detached
run_release "$root" target >/dev/null
[[ ! -e "$root/worktrees/slots/target" ]] || fail 'exact target slot remains'
jq -e '.slots.target == null' "$root/worktree-state.json" >/dev/null \
  || fail 'released slot remains in state'
for product in hermit reverie liteinst2; do
  eval "stale=\$stale_$product"
  assert_registered "$root" "$product" "$stale"
done

# Positive: ordinary cleanup is scoped to its exact slot, so unrelated branch
# drift cannot deadlock reclaim. The unrelated checkout and row must survive
# byte-for-byte; scope is not permission to repair or absorb them.
root="$test_root/ordinary-unrelated-drift"
init_root "$root"
add_target "$root" hermit target
write_state "$root" target detached - -
add_unrelated_drift "$root"
unrelated_before="$(git -C "$root/worktrees/slots/unrelated/hermit" rev-parse HEAD)"
run_release "$root" target >"$root/release.out" 2>&1
grep -Fq 'release cleanup scope: slot=target; unrelated registry rows are not inspected' \
  "$root/release.out" \
  || fail 'ordinary cleanup did not report its exact slot boundary'
[[ ! -e "$root/worktrees/slots/target" ]] \
  || fail 'ordinary scoped cleanup left the exact target slot'
[[ -e "$root/worktrees/slots/unrelated/hermit" ]] \
  || fail 'ordinary scoped cleanup touched the unrelated drifted checkout'
[[ "$(git -C "$root/worktrees/slots/unrelated/hermit" rev-parse HEAD)" == "$unrelated_before" ]] \
  || fail 'ordinary scoped cleanup moved the unrelated drifted checkout'
jq -e '.slots.unrelated.hermit_branch == "wrong-branch"' \
  "$root/worktree-state.json" >/dev/null \
  || fail 'ordinary scoped cleanup rewrote the unrelated drifted registry row'

# Positive: products recorded as '-' are outside the release authority and
# their unrelated stale registry entries remain untouched.
root="$test_root/dash"
init_root "$root"
add_target "$root" hermit target
stale_reverie="$(add_missing_unrelated "$root" reverie unrelated)"
stale_liteinst2="$(add_missing_unrelated "$root" liteinst2 unrelated)"
write_state "$root" target detached - -
run_release "$root" target >/dev/null
assert_registered "$root" reverie "$stale_reverie"
assert_registered "$root" liteinst2 "$stale_liteinst2"

# Negative: an allocated-but-missing target is refused before another product,
# state, ACTIVE, or an unrelated stale registry entry changes.
root="$test_root/missing"
init_root "$root"
add_target "$root" hermit target
add_target "$root" reverie target
add_target "$root" liteinst2 target
stale_hermit="$(add_missing_unrelated "$root" hermit unrelated)"
mv "$root/worktrees/slots/target/reverie" "$root/moved/target-reverie"
write_state "$root" target detached detached detached
state_before="$(sha256sum "$root/worktree-state.json")"
active_before="$(sha256sum "$root/worktrees/ACTIVE.md")"
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'allocated missing target was accepted'
fi
grep -Eq 'scoped clean preflight failed: canonical registry verifier refused cleanup: .*DRIFT slot=target reverie recorded=detached actual=-' "$root/release.out" \
  || {
    cat "$root/release.out" >&2
    fail 'missing-target refusal was not explicit'
  }
[[ -e "$root/worktrees/slots/target/hermit" ]] || fail 'preflight mutated an earlier target'
[[ "$(sha256sum "$root/worktree-state.json")" == "$state_before" ]] \
  || fail 'missing-target refusal changed state'
[[ "$(sha256sum "$root/worktrees/ACTIVE.md")" == "$active_before" ]] \
  || fail 'missing-target refusal changed ACTIVE'
assert_registered "$root" hermit "$stale_hermit"

# Negative: a recorded sibling path cannot widen the selected slot.
root="$test_root/mismatch"
init_root "$root"
add_target "$root" hermit target
add_target "$root" hermit sibling
write_state "$root" target detached - - worktrees/slots/sibling/hermit
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'mismatched recorded path was accepted'
fi
grep -Fq 'DRIFT slot=target hermit path recorded=Some("worktrees/slots/sibling/hermit") expected=worktrees/slots/target/hermit' "$root/release.out" \
  || {
    cat "$root/release.out" >&2
    fail 'mismatched-path refusal was not explicit'
  }
[[ -e "$root/worktrees/slots/sibling/hermit" ]] || fail 'sibling path was removed'
[[ -e "$root/worktrees/slots/target/hermit" ]] || fail 'target changed on mismatch refusal'

# Negative: the recorded checkout identity must match the exact registered
# child, not merely its path.
root="$test_root/identity"
init_root "$root"
add_target "$root" hermit target
write_state "$root" target wrong-branch - -
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'checkout identity mismatch was accepted'
fi
grep -Fq 'DRIFT slot=target hermit recorded=wrong-branch actual=detached:' "$root/release.out" \
  || fail 'checkout identity refusal was not explicit'
[[ -e "$root/worktrees/slots/target/hermit" ]] || fail 'identity refusal removed target'

# Negative: only a safe lexical slot token can influence filesystem paths.
root="$test_root/absolute-slot"
init_root "$root"
mkdir -p "$root/escape"
write_state "$root" target - - -
jq --arg slot "$root/escape" '.slots = {($slot): .slots.target}' \
  "$root/worktree-state.json" >"$root/state.tmp"
mv "$root/state.tmp" "$root/worktree-state.json"
if run_release "$root" "$root/escape" >"$root/release.out" 2>&1; then
  fail 'absolute slot name was accepted'
fi
grep -Eq 'invalid slot name' "$root/release.out" \
  || fail 'absolute-slot refusal was not explicit'
[[ -d "$root/escape" ]] || fail 'absolute-slot refusal removed external directory'

# Negative: only an explicit string '-' skips a product. Missing/non-string
# allocation data fails closed.
root="$test_root/missing-branch"
init_root "$root"
add_target "$root" hermit target
write_state "$root" target detached - -
jq 'del(.slots.target.hermit_branch)' "$root/worktree-state.json" >"$root/state.tmp"
mv "$root/state.tmp" "$root/worktree-state.json"
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'missing branch field was accepted'
fi
grep -Fq 'missing field `hermit_branch`' "$root/release.out" \
  || {
    cat "$root/release.out" >&2
    fail 'missing-branch refusal was not explicit'
  }
[[ -e "$root/worktrees/slots/target/hermit" ]] || fail 'missing-branch refusal removed target'

# Negative: a symlink at the canonical lexical path cannot alias a sibling
# registered worktree.
root="$test_root/symlink"
init_root "$root"
add_target "$root" hermit target
add_target "$root" hermit sibling
git -C "$root/hermit" worktree remove "$root/worktrees/slots/target/hermit"
ln -s "$root/worktrees/slots/sibling/hermit" "$root/worktrees/slots/target/hermit"
write_state "$root" target detached - -
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'symlink alias was accepted'
fi
grep -Eq 'refusing symlink/path alias' "$root/release.out" \
  || {
    cat "$root/release.out" >&2
    fail 'symlink refusal was not explicit'
  }
[[ -e "$root/worktrees/slots/sibling/hermit" ]] || fail 'symlink refusal removed sibling'

# Negative: a stale primary admin record and a replacement repository at the
# same path are not reciprocal identity evidence.
root="$test_root/replacement-repo"
init_root "$root"
add_target "$root" hermit target
mv "$root/worktrees/slots/target/hermit" "$root/moved/original-hermit"
git init -q "$root/worktrees/slots/target/hermit"
git -C "$root/worktrees/slots/target/hermit" config user.email test@example.invalid
git -C "$root/worktrees/slots/target/hermit" config user.name test
printf '%s\n' replacement >"$root/worktrees/slots/target/hermit/replacement"
git -C "$root/worktrees/slots/target/hermit" add replacement
git -C "$root/worktrees/slots/target/hermit" commit -qm replacement
git -C "$root/worktrees/slots/target/hermit" checkout -q --detach
write_state "$root" target detached - -
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'replacement repository at stale registered path was accepted'
fi
grep -Eq 'child common-dir .* does not match primary' "$root/release.out" \
  || {
    cat "$root/release.out" >&2
    fail 'replacement-repo refusal was not explicit'
  }
[[ -e "$root/worktrees/slots/target/hermit/replacement" ]] \
  || fail 'replacement-repo refusal removed replacement data'

# Negative: unexpected slot-root residue is discovered before the exact child
# is removed, keeping the operation retryable.
root="$test_root/residue"
init_root "$root"
add_target "$root" hermit target
printf '%s\n' sentinel >"$root/worktrees/slots/target/UNOWNED"
write_state "$root" target detached - -
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'unexpected slot residue was accepted'
fi
grep -Eq 'contains unexpected entry' "$root/release.out" \
  || fail 'slot residue refusal was not explicit'
[[ -e "$root/worktrees/slots/target/hermit" ]] || fail 'residue refusal removed target'

# Negative plus recovery: an exact registered but locked target retains the
# target and a durable armed journal. Ordinary retries refuse that transaction;
# explicit recovery resumes it after the lock is removed.
root="$test_root/locked"
init_root "$root"
add_target "$root" hermit target
write_state "$root" target detached - -
git -C "$root/hermit" worktree lock "$root/worktrees/slots/target/hermit"
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'locked target was removed'
fi
jq -e '.slots.target.status == "releasing" and
       .slots.target.release_journal.label == "hermit" and
       .slots.target.release_journal.phase == "armed"' \
  "$root/worktree-state.json" >/dev/null \
  || fail 'locked-target refusal did not retain its exact durable journal'
grep -Fq '| target | fixture-owner | detached | - | - | fixture | releasing | no |' \
  "$root/worktrees/ACTIVE.md" \
  || fail 'locked-target journal was not projected into ACTIVE'
[[ -e "$root/worktrees/slots/target/hermit" ]] || fail 'locked-target refusal removed target'
if run_release "$root" target >"$root/retry.out" 2>&1; then
  fail 'ordinary retry bypassed the unfinished locked-target journal'
fi
grep -Eq 'unfinished release journal' "$root/retry.out" \
  || fail 'ordinary locked-target retry refusal was not explicit'
git -C "$root/hermit" worktree unlock "$root/worktrees/slots/target/hermit"
add_unrelated_drift "$root"
run_recovery "$root" target >"$root/recovery.out" 2>&1
grep -Fq 'release cleanup scope: slot=target; unrelated registry rows are not inspected' \
  "$root/recovery.out" \
  || fail 'scoped recovery did not report its exact slot boundary'
jq -e '.slots.target == null' "$root/worktree-state.json" >/dev/null \
  || fail 'locked-target recovery did not complete release'
[[ -e "$root/worktrees/slots/unrelated/hermit" ]] \
  || fail 'scoped recovery touched the unrelated drifted checkout'
jq -e '.slots.unrelated.hermit_branch == "wrong-branch"' \
  "$root/worktree-state.json" >/dev/null \
  || fail 'scoped recovery rewrote the unrelated drifted registry row'

# Two-fault negative: scoped recovery still refuses local-only work in its own
# target and retains the armed journal. Scope is not a force bypass.
root="$test_root/locked-unpublished"
init_root "$root"
add_target "$root" hermit target
write_state "$root" target detached - -
git -C "$root/hermit" worktree lock "$root/worktrees/slots/target/hermit"
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'locked unpublished fixture unexpectedly completed initial release'
fi
git -C "$root/hermit" worktree unlock "$root/worktrees/slots/target/hermit"
printf '%s\n' local-only >"$root/worktrees/slots/target/hermit/local-only"
git -C "$root/worktrees/slots/target/hermit" add local-only
git -C "$root/worktrees/slots/target/hermit" commit -qm 'local-only recovery fixture'
add_unrelated_drift "$root"
if run_recovery "$root" target >"$root/recovery.out" 2>&1; then
  fail 'scoped recovery discarded a local-only target commit'
fi
grep -Fq 'committed work not on origin' "$root/recovery.out" \
  || { cat "$root/recovery.out" >&2; fail 'local-only recovery refusal was not explicit'; }
jq -e '.slots.target.status == "releasing" and
       .slots.target.release_journal.phase == "armed"' \
  "$root/worktree-state.json" >/dev/null \
  || fail 'local-only refusal cleared the target recovery journal'
[[ -e "$root/worktrees/slots/target/hermit/local-only" ]] \
  || fail 'local-only refusal removed unpublished target content'
jq -e '.slots.unrelated.hermit_branch == "wrong-branch"' \
  "$root/worktree-state.json" >/dev/null \
  || fail 'local-only refusal changed unrelated drift state'

# Negative plus recovery: if a later exact product removal fails, completed
# removals are recorded per product and retry resumes the remaining target.
root="$test_root/partial-retry"
init_root "$root"
add_target "$root" hermit target
add_target "$root" reverie target
write_state "$root" target detached detached -
git -C "$root/reverie" worktree lock "$root/worktrees/slots/target/reverie"
if run_release "$root" target >"$root/release.out" 2>&1; then
  fail 'locked second target unexpectedly completed release'
fi
jq -e '.slots.target.hermit_branch == "-" and .slots.target.reverie_branch == "detached"' \
  "$root/worktree-state.json" >/dev/null \
  || fail 'partial release did not record exact per-product progress'
[[ ! -e "$root/worktrees/slots/target/hermit" ]] || fail 'first exact target was not removed'
[[ -e "$root/worktrees/slots/target/reverie" ]] || fail 'locked remaining target was removed'
jq -e '.slots.target.release_journal.label == "reverie" and
       .slots.target.release_journal.phase == "armed"' \
  "$root/worktree-state.json" >/dev/null \
  || fail 'partial release did not retain the remaining exact transaction'
git -C "$root/reverie" worktree unlock "$root/worktrees/slots/target/reverie"
run_recovery "$root" target >/dev/null
jq -e '.slots.target == null' "$root/worktree-state.json" >/dev/null \
  || fail 'retry did not complete slot release'
[[ ! -e "$root/worktrees/slots/target" ]] || fail 'retry left target slot directory'

if grep -nE 'worktree.*prune|\["worktree", "prune"\]' "$subject" >/dev/null; then
  fail 'release script still contains a worktree prune call'
fi

echo 'release-worktree-target-scope-test: PASS (5 positive; 10 negative; ordinary and recovery scope disclosed; local-only recovery refused; unrelated drift preserved)'
