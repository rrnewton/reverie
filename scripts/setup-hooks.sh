#!/usr/bin/env bash
# Install the machine attribution hook without replacing repository-local hooks.
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
parent_reference_transaction="$root/scripts/parent-reference-transaction"
checker="$root/scripts/check_commit_attribution.py"
[ -x "$dispatcher" ] || { echo "missing executable dispatcher: $dispatcher" >&2; exit 1; }
[ -x "$parent_reference_transaction" ] || {
  echo "missing executable parent reference-transaction hook: $parent_reference_transaction" >&2
  exit 1
}
[ -x "$checker" ] || { echo "missing executable checker: $checker" >&2; exit 1; }
machine=$("$checker" --detect-machine)

# Install every client-side hook name up front. The dispatcher delegates to a
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
    source="$dispatcher"
    if [ "$repo" = "$root" ] && [ "$hook" = reference-transaction ]; then
      source="$parent_reference_transaction"
    fi
    if [ "$mode" = check ]; then
      if [ ! -L "$target" ] || [ "$(readlink -f -- "$target")" != "$source" ]; then
        echo "hook check REFUSED: $target is not bound to $source" >&2
        exit 1
      fi
    else
      if [ -e "$target" ] && [ ! -L "$target" ]; then
        echo "hook install REFUSED: unmanaged file exists at $target" >&2
        exit 1
      fi
      ln -sfn "$source" "$target"
    fi
  done

  if [ "$mode" = install ]; then
    git -C "$repo" config core.hooksPath "$composite"
  fi
  installed=$((installed + 1))
done

echo "machine commit hooks: mode=$mode machine=$machine repositories=$installed"
echo "Every commit must end: [<role>, <model>] [<team-slug>, $machine]"
echo "Repository-local .githooks remain active through the composite dispatcher."
echo "Policy: .githooks/hygiene-policy.md"
echo "Override an oversized-but-intended file with: HERMIT_HYGIENE_OVERRIDE=1 git commit ..."
echo "Every push polls GitHub current-main workflow truth; RED warns but does not block a fix."
echo "Every commit warns on stale primaries/gitlinks; make checkout-fresh publishes a coherent clean snapshot."
echo "Shared-branch rewinds are REFUSED unless you declare the tip: HERMIT_RESET_EXPECT=<sha> git reset ..."
echo "Parent main commits/pushes are REFUSED outside scripts/parent-main-write."
echo "Gitlink/hook repairs require its retained --audit-reason path."
