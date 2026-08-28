#!/usr/bin/env bash
# Bracketed test: a freshly allocated slot must come up ABLE TO BUILD.
#
# WHAT BROKE. `git worktree add` does not populate submodules and the allocator
# never asked it to. hermit declares two (`agent-utils`, `third-party/rr`), so a
# new slot's hermit worktree arrived with an EMPTY `agent-utils/`. Self-tests in
# it then died in under a second on a missing dependency, which reads as a
# PRODUCT failure rather than an incomplete checkout. agent(hermit-106) hit this
# on 2026-08-27 and lost three runs to it before checking why.
#
# Measured the same day across the 61 slot hermit worktrees on disk: 27 (44%)
# had an empty `agent-utils`, 50 (81%) an empty `third-party/rr`, and only 11
# had both. The populated ones were populated BY HAND. A slot that looks
# allocated and cannot build is worse than no slot.
#
# WHAT THIS ASSERTS, and why each half is here:
#
#   POPULATED  a fresh allocation leaves agent-utils non-empty and at the
#              recorded commit. Without this the change could be a no-op.
#   BEFORE     the SAME fixture against the pre-change allocator leaves it
#              EMPTY. Without this the first assertion says nothing about the
#              change.
#   PRESERVED  re-running allocation to adopt the slot does NOT disturb an
#              agent-utils the owner already moved to a different commit. This
#              is the one that could destroy work: adoption is expected to be
#              re-run routinely, and a blind re-init would silently revert an
#              owner's checkout.
#   ADVISORY   a submodule that cannot be initialized does not fail the
#              allocation, and says so on stderr. The slot is still allocated.
#
# Entirely local: the fixture's submodule source is a path on disk, so nothing
# here needs the network or the proxy.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
root="$(mktemp -d "${TMPDIR:-/tmp}/allocate-submodule-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

pass=0
fail=0
note() { printf '%s\n' "$*"; }
ok()   { printf '  PASS  %s\n' "$*"; pass=$((pass + 1)); }
bad()  { printf '  FAIL  %s\n' "$*"; fail=$((fail + 1)); }

note "Fixture root: $root"
note ""

# The submodule's upstream, a plain local repository.
upstream="$root/agent-utils-upstream"
git init -q -b main "$upstream"
git -C "$upstream" config user.email test@example.invalid
git -C "$upstream" config user.name test
printf 'first\n' >"$upstream/marker.txt"
git -C "$upstream" add marker.txt
git -C "$upstream" commit -q -m 'seed agent-utils'
printf 'second\n' >"$upstream/later.txt"
git -C "$upstream" add later.txt
git -C "$upstream" commit -q -m 'a later agent-utils commit'
later_sha="$(git -C "$upstream" rev-parse HEAD)"
git -C "$upstream" checkout -q HEAD~1
first_sha="$(git -C "$upstream" rev-parse HEAD)"

build_fixture() { # <allocator-source>
  rm -rf "$root/tree"
  mkdir -p "$root/tree/scripts" "$root/tree/worktrees" "$root/tree/ignored/ci-hub"
  cp "$1" "$root/tree/scripts/allocate-worktree.rs"
  chmod +x "$root/tree/scripts/allocate-worktree.rs"
  cp "$script_dir/check-worktree-registry.rs" "$root/tree/scripts/"
  cp "$script_dir/worktree_registry.rs" "$root/tree/scripts/"
  cp "$script_dir/agent_snapshot.rs" "$root/tree/scripts/"
  : >"$root/tree/.gitmodules"
  git -C "$root/tree" init -q -b main
  git -C "$root/tree" config user.email test@example.invalid
  git -C "$root/tree" config user.name test
  touch "$root/tree/parent-seed"
  git -C "$root/tree" add parent-seed .gitmodules
  git -C "$root/tree" commit -q -m 'seed parent checkout'
  for product in hermit reverie liteinst2; do
    mkdir -p "$root/tree/$product"
    git -C "$root/tree/$product" init -q -b main
    git -C "$root/tree/$product" config user.email test@example.invalid
    git -C "$root/tree/$product" config user.name test
    git -C "$root/tree/$product" config protocol.file.allow always
    touch "$root/tree/$product/seed"
    git -C "$root/tree/$product" add seed
    git -C "$root/tree/$product" commit -q -m 'seed product primary'
  done
  # Only hermit declares agent-utils, exactly as the real tree does.
  git -C "$root/tree/hermit" -c protocol.file.allow=always \
    submodule add -q "$upstream" agent-utils
  git -C "$root/tree/hermit" -C . add .gitmodules agent-utils
  git -C "$root/tree/hermit" commit -q -m 'add agent-utils submodule'
}

allocate() { # <agent> <slot>
  ( cd "$root/tree" && ./scripts/allocate-worktree.rs \
      --agent "$1" --slot "$2" --product hermit \
      --task "submodule-test" --purpose "bracket submodule initialization" \
  ) >"$root/out.$2" 2>"$root/err.$2" || true
}

entries() { # <dir>
  ls -A "$1" 2>/dev/null | wc -l | tr -d ' '
}

# ------------------------------------------------------------------ CASE 1 ---
note "CASE 1  POPULATED -- a fresh allocation leaves agent-utils usable"
build_fixture "$script_dir/allocate-worktree.rs"
allocate "hermit-sub" "subslot"
au="$root/tree/worktrees/slots/subslot/hermit/agent-utils"
if [[ "$(entries "$au")" -gt 0 ]]; then
  ok "C1: agent-utils is populated ($(entries "$au") entries)"
else
  bad "C1: agent-utils is EMPTY after allocation"
  sed -n '1,15p' "$root/err.subslot" >&2 || true
fi
if [[ -f "$au/marker.txt" ]]; then
  ok "C1: the submodule's content is actually checked out"
else
  bad "C1: agent-utils has no marker.txt -- not a real checkout"
fi
if [[ "$(git -C "$au" rev-parse HEAD 2>/dev/null)" == "$first_sha" ]]; then
  ok "C1: checked out at the commit the superproject records"
else
  bad "C1: agent-utils is at $(git -C "$au" rev-parse HEAD 2>/dev/null), expected $first_sha"
fi
if grep -q "initialized hermit submodule agent-utils" "$root/out.subslot"; then
  ok "C1: the allocator says it did it"
else
  bad "C1: allocation output does not report the initialization"
fi
note ""

# ------------------------------------------------------------------ CASE 2 ---
note "CASE 2  BEFORE -- the SAME fixture against the pre-change allocator"
note "        must leave agent-utils EMPTY."
# Not `git show ... | grep -q`: under pipefail, grep -q exits on its first match,
# git show takes SIGPIPE, and the PIPELINE reports failure -- so a revision that
# DOES contain the function tests as one that does not, and the A/B silently
# selects the post-change build as its own control.
before_src="$root/allocate-worktree.before.rs"
before_sha=""
while read -r sha; do
  [[ -n "$sha" ]] || continue
  git -C "$script_dir/.." show "$sha:scripts/allocate-worktree.rs" >"$before_src" 2>/dev/null || continue
  if grep -q "initialize_required_submodules" "$before_src"; then
    continue
  fi
  before_sha="$sha"
  break
done < <(git -C "$script_dir/.." log --format=%H -n 200 -- scripts/allocate-worktree.rs 2>/dev/null)

if [[ -n "$before_sha" ]]; then
  note "        pre-change allocator taken from $before_sha"
  build_fixture "$before_src"
  allocate "hermit-sub" "subslot"
  if [[ "$(entries "$root/tree/worktrees/slots/subslot/hermit/agent-utils")" -eq 0 ]]; then
    ok "C2: pre-change build left agent-utils EMPTY -- the defect, reproduced"
  else
    bad "C2: pre-change build unexpectedly populated agent-utils"
  fi
else
  bad "C2: no pre-change revision in the last 200 commits touching the file; the A/B did not run"
fi
note ""

# ------------------------------------------------------------------ CASE 3 ---
note "CASE 3  PRESERVED -- re-running allocation to adopt must NOT revert a"
note "        submodule the owner moved. Adoption is re-run routinely."
build_fixture "$script_dir/allocate-worktree.rs"
allocate "hermit-sub" "subslot"
au="$root/tree/worktrees/slots/subslot/hermit/agent-utils"
git -C "$au" -c protocol.file.allow=always fetch -q origin 2>/dev/null || true
git -C "$au" checkout -q "$later_sha" 2>/dev/null || true
moved="$(git -C "$au" rev-parse HEAD 2>/dev/null)"
if [[ "$moved" == "$later_sha" ]]; then
  allocate "hermit-sub" "subslot"
  if [[ "$(git -C "$au" rev-parse HEAD 2>/dev/null)" == "$later_sha" ]]; then
    ok "C3: re-allocation left the owner's checked-out commit alone"
  else
    bad "C3: re-allocation REVERTED the owner's submodule to $(git -C "$au" rev-parse HEAD)"
  fi
else
  bad "C3: could not move the fixture submodule; the preservation case did not run"
fi
note ""

# ------------------------------------------------------------------ CASE 4 ---
note "CASE 4  ADVISORY -- an uninitializable submodule must not fail the"
note "        allocation, and must say so."
build_fixture "$script_dir/allocate-worktree.rs"
# Breaking the URL alone is NOT enough, and finding that out is worth recording:
# git reuses the superproject's already-cloned `.git/modules/agent-utils`, so the
# init still succeeds offline. That is exactly the property that makes this cheap
# in production -- no network -- but it means a broken URL provokes nothing. To
# reach the failure path the local objects have to be gone too.
# Nor is editing .gitmodules enough on its own: `submodule add` also wrote the
# URL into the primary's own .git/config, and `submodule update --init` prefers
# THAT. All three have to go.
rm -rf "$root/tree/hermit/.git/modules/agent-utils"
git -C "$root/tree/hermit" config --unset submodule.agent-utils.url || true
git -C "$root/tree/hermit" config --file .gitmodules submodule.agent-utils.url \
  "$root/definitely-not-a-repository"
git -C "$root/tree/hermit" add .gitmodules
git -C "$root/tree/hermit" commit -q -m 'break the submodule url'
allocate "hermit-sub" "brokenslot"
if grep -q "allocated worktrees/slots/brokenslot" "$root/out.brokenslot"; then
  ok "C4: the allocation still succeeded"
else
  bad "C4: a failed submodule init blocked the allocation"
  sed -n '1,15p' "$root/err.brokenslot" >&2 || true
fi
if grep -q "could not initialize hermit submodule agent-utils" "$root/err.brokenslot"; then
  ok "C4: the failure is reported loudly rather than silently"
else
  bad "C4: the failure was silent -- that is the bug this replaces"
fi
if grep -q "submodule update --init agent-utils" "$root/err.brokenslot"; then
  ok "C4: the message carries the exact recovery command"
else
  bad "C4: no recovery command in the warning"
fi
note ""

note "======================================================================"
note "assertions: $pass passed, $fail failed"
if [[ "$fail" -ne 0 ]]; then
  note "RESULT: FAIL"
  exit 1
fi
note "RESULT: PASS"
