#!/usr/bin/env bash
# Bracketed tests for the shared-index guard in .githooks/pre-commit.
#
# WHY THIS FILE EXISTS. The guard was deleted on 2026-08-15 by 4acd425b
# "Remove parent Git serialization blockers" and NOTHING WENT RED for ten days.
# Its own experiment harness could not catch that: race_harness.sh:191 extracts
# the function with `awk '/^shared_index_guard\(\) \{/,/^\}$/'` and then calls
# what it extracted, so once the function was gone the harness built a hook that
# referenced an undefined name instead of reporting the absence. A guard whose
# only check greps for its own source cannot report its own deletion.
#
# So this suite asserts BEHAVIOUR through a real `git commit`, and the negative
# case is the concrete incident: one agent's bare commit taking another agent's
# staged path.
#
# Both halves matter. A guard that refuses everything gets switched off, which
# is worse than none because it also trains people to ignore it -- so the
# pathspec form, the two escape hatches, and the clean-index case are asserted
# just as hard as the refusal.
#
# Everything runs in a throwaway repo under a tmpdir; nothing touches the shared
# parent, which would be this test performing the very operation it polices.

set -uo pipefail

HOOKS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOK="$HOOKS_DIR/pre-commit"
[ -f "$HOOK" ] || { echo "FATAL: hook missing at $HOOK"; exit 1; }

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  PASS  $1"; }
bad() { fail=$((fail+1)); echo "  FAIL  $1"; [ -n "${2:-}" ] && echo "        $2"; }

new_repo() {
  local d; d=$(mktemp -d)
  git init -q -b main "$d"
  mkdir -p "$d/.githooks"
  cp "$HOOK" "$d/.githooks/pre-commit"; chmod +x "$d/.githooks/pre-commit"
  git -C "$d" config core.hooksPath .githooks
  git -C "$d" config user.name "fixture agent"
  git -C "$d" config user.email fixture@example.com
  printf 'seed\n' > "$d/mine.txt"
  printf 'seed\n' > "$d/theirs.txt"
  git -C "$d" add mine.txt theirs.txt
  git -C "$d" commit -q --no-verify -m seed
  printf '%s\n' "$d"
}
tip() { git -C "$1" rev-parse HEAD; }

# Stage one path as "me" and one as a neighbour, reproducing the shared index.
stage_both() {
  printf 'mine\n'   >> "$1/mine.txt";   git -C "$1" add mine.txt
  printf 'theirs\n' >> "$1/theirs.txt"; git -C "$1" add theirs.txt
}

# ------------------------------------------------------------------ NEGATIVE 1
# The incident itself: a bare commit takes a path its author never staged.
r=$(new_repo); stage_both "$r"; before=$(tip "$r")
git -C "$r" commit -m "bare" > /tmp/sig1.$$ 2>&1
rc=$?
if [ "$rc" -ne 0 ] && [ "$(tip "$r")" = "$before" ]; then
  ok "a bare commit over a shared index is refused and the ref does not move"
else
  bad "the bare commit was ACCEPTED (rc=$rc)" "$(git -C "$r" show --name-only --format= HEAD | tr '\n' ' ')"
fi

# ------------------------------------------------------------------ NEGATIVE 2
# The refusal must name the remedy, not merely say no.
if grep -q 'commit -m .* -- <your paths>' /tmp/sig1.$$; then
  ok "the refusal names the pathspec form that would work"
else
  bad "refusal did not tell the author what to do instead"
fi
if grep -q 'theirs.txt' /tmp/sig1.$$; then
  ok "the refusal lists the staged paths, so the author can see whose they are"
else
  bad "refusal did not show the staged paths"
fi
rm -f /tmp/sig1.$$

# ------------------------------------------------------------------ NEGATIVE 3
# `-a` stages every tracked modification and passes no pathspec, which is the
# commonest way the sweep actually happens.
before=$(tip "$r")
printf 'more\n' >> "$r/theirs.txt"
git -C "$r" commit -am "bare via -a" > /dev/null 2>&1
if [ "$(tip "$r")" = "$before" ]; then
  ok "commit -am is refused too, not just an explicit bare commit"
else
  bad "commit -am swept the tree"
fi

# ------------------------------------------------------------------- POSITIVE 1
# THE HALF THAT KEEPS THE GUARD SWITCHED ON. The same work, named explicitly,
# must commit only the named path and leave the bystander's staged file alone.
before=$(tip "$r")
git -C "$r" commit -q -m "pathspec" -- mine.txt > /dev/null 2>&1
rc=$?
took=$(git -C "$r" show --name-only --format= HEAD | grep -c 'mine.txt')
foreign=$(git -C "$r" show --name-only --format= HEAD | grep -c 'theirs.txt')
if [ "$rc" -eq 0 ] && [ "$(tip "$r")" != "$before" ] && [ "$took" -eq 1 ] && [ "$foreign" -eq 0 ]; then
  ok "the pathspec form is allowed and takes only the named path"
else
  bad "pathspec commit misbehaved (rc=$rc took=$took foreign=$foreign)"
fi
if [ -n "$(git -C "$r" diff --cached --name-only)" ]; then
  ok "the bystander's staged path survives the pathspec commit"
else
  bad "the pathspec commit consumed the bystander's staged path"
fi

# ------------------------------------------------------------------- POSITIVE 2
# A clean index has nothing to sweep, so a bare commit there is ordinary work.
r2=$(new_repo); before=$(tip "$r2")
printf 'x\n' >> "$r2/mine.txt"; git -C "$r2" add mine.txt
# Only one author's path is staged, but the guard is deliberately about the
# FORM, not about guessing ownership -- it cannot know whose path this is. This
# case therefore asserts the escape hatch, which is the sanctioned way through.
HERMIT_SHARED_INDEX_GUARD=warn git -C "$r2" commit -q -m "warned" > /dev/null 2>&1
if [ "$(tip "$r2")" != "$before" ]; then
  ok "HERMIT_SHARED_INDEX_GUARD=warn reports and allows"
else
  bad "warn mode blocked, so the escape hatch does not work"
fi

# ------------------------------------------------------------------- POSITIVE 3
before=$(tip "$r2")
printf 'y\n' >> "$r2/mine.txt"; git -C "$r2" add mine.txt
out=$(HERMIT_SHARED_INDEX_GUARD=off git -C "$r2" commit -m "silenced" 2>&1)
if [ "$(tip "$r2")" != "$before" ] && ! printf '%s' "$out" | grep -q 'SHARED INDEX'; then
  ok "HERMIT_SHARED_INDEX_GUARD=off allows and stays silent"
else
  bad "off mode did not silence the guard"
fi

# ------------------------------------------------------------------- POSITIVE 4
# Nothing staged at all: the guard must not invent a reason to refuse.
r3=$(new_repo); before=$(tip "$r3")
git -C "$r3" commit -q --allow-empty -m "empty" > /dev/null 2>&1
if [ "$(tip "$r3")" != "$before" ]; then
  ok "an empty index is not treated as a shared-index hazard"
else
  bad "the guard refused a commit with nothing staged"
fi

rm -rf "$r" "$r2" "$r3"
echo
echo "shared-index-guard: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
