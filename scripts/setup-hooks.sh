#!/usr/bin/env bash
# Install repository-local hooks through one common dispatcher implementation.
# Every registered product worktree shares one of the common Git directories
# below, so configuring the parent + initialized submodules covers present and
# future slots on this machine.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
cd "$root"

mode=install
case "${1:-}" in
  '') ;;
  --check) mode=check ;;
  *) echo "usage: scripts/setup-hooks.sh [--check]" >&2; exit 2 ;;
esac

dispatcher="$root/.githooks/dev-hermit-hook-dispatch"
[ -x "$dispatcher" ] || { echo "missing executable dispatcher: $dispatcher" >&2; exit 1; }
managed_marker='# dev-hermit-managed-hook-dispatch-v1'
grep -Fqx "$managed_marker" "$dispatcher" || {
  echo "hook install REFUSED: dispatcher lacks managed marker: $dispatcher" >&2
  exit 1
}

# Install every client-side hook name up front. Each installed regular file is
# a self-contained copy: a symlink to this worktree would dangle as soon as a
# disposable linked worktree is removed, and Git silently treats a dangling
# hook as success. The dispatcher delegates to a
# tracked <repo>/.githooks/<name> when one exists, so adding a tracked hook later
# does not require replacing or regenerating this local composite directory.
hooks=(
  applypatch-msg pre-applypatch post-applypatch pre-commit pre-merge-commit
  prepare-commit-msg commit-msg post-commit pre-rebase post-checkout post-merge
  pre-push post-rewrite sendemail-validate fsmonitor-watchman
  reference-transaction post-index-change
)

repos=("$root")
while read -r _ path; do
  repo="$root/$path"
  if [ -e "$repo/.git" ] && git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
    repos+=("$repo")
  fi
done < <(git config -f .gitmodules --get-regexp '^submodule\..*\.path$' 2>/dev/null || true)

installed=0
for repo in "${repos[@]}"; do
  common=$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)
  composite="$common/dev-hermit-hooks"
  if [ "$mode" = check ]; then
    configured=$(git -C "$repo" config --get core.hooksPath 2>/dev/null || true)
    [ "$configured" = "$composite" ] || {
      echo "hook check REFUSED: $repo uses ${configured:-<unset>}, expected $composite" >&2
      exit 1
    }
  else
    mkdir -p "$composite"
  fi

  for hook in "${hooks[@]}"; do
    target="$composite/$hook"
    if [ "$mode" = check ]; then
      if [ ! -f "$target" ] || [ -L "$target" ] || [ ! -x "$target" ] || \
          ! cmp -s -- "$dispatcher" "$target"; then
        echo "hook check REFUSED: $target is not an executable installed copy of $dispatcher" >&2
        exit 1
      fi
    else
      if [ -e "$target" ] && [ ! -L "$target" ] && \
          ! grep -Fqx "$managed_marker" "$target"; then
        echo "hook install REFUSED: unmanaged file exists at $target" >&2
        exit 1
      fi
      temporary=$(mktemp "$composite/.${hook}.XXXXXX")
      if ! install -m 0755 "$dispatcher" "$temporary"; then
        rm -f -- "$temporary"
        exit 1
      fi
      mv -f -- "$temporary" "$target"
    fi
  done

  if [ "$mode" = install ]; then
    git -C "$repo" config core.hooksPath "$composite"

    # MAKE THE SAFE PUSH THE SHORT ONE.
    #
    # `git push origin HEAD:main 2>&1 | tail -2; rc=$?` reads TAIL's exit status,
    # so a REJECTED push reports rc=0 and an unlanded change gets recorded as
    # merged. Four agents hit that in one night against a warning that was
    # already written down -- documentation is not the fix, and a correct tool
    # nobody reaches for loses to plain `git push` every time. So the tool gets
    # a caller in the tree, here, where hooks are already provisioned.
    #
    # `git pushv <remote> <src>:<dst>` runs the push with no pipe and then asks
    # the REMOTE what the ref points at. The path is resolved at run time from
    # the superproject root so this works from submodules and contains no
    # absolute path.
    # This is Git's shell alias and is expanded only when the alias runs.
    # shellcheck disable=SC2016
    git -C "$repo" config alias.pushv '!f() { r=$(git rev-parse --show-superproject-working-tree 2>/dev/null); [ -n "$r" ] || r=$(git rev-parse --show-toplevel); t="$r/ci-hub/bin/git-push-verified"; if [ -x "$t" ]; then "$t" "$@"; else echo "git pushv: tool missing at $t; falling back is NOT safe -- verify the remote by hand" >&2; return 1; fi; }; f'
  fi
  installed=$((installed + 1))
done

echo "repository hooks: mode=$mode repositories=$installed"
echo "Repository-local .githooks remain active through durable composite dispatcher copies."
echo "Policy: .githooks/hygiene-policy.md"
echo "Override an oversized-but-intended file with: HERMIT_HYGIENE_OVERRIDE=1 git commit ..."
echo "Every push polls GitHub current-main workflow truth; RED warns but does not block a fix."
echo "Every commit warns on stale primaries/gitlinks; make checkout-fresh publishes a coherent clean snapshot."
echo "Shared-branch rewinds are REFUSED unless you declare the tip: HERMIT_RESET_EXPECT=<sha> git reset ..."
echo "Use 'git pushv <remote> <src>:<dst>' to push: it verifies the REMOTE moved, because a rejected push piped to tail reports success."
