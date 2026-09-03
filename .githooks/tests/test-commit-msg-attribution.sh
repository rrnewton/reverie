#!/usr/bin/env bash
# Bracketed tests for the canonical attribution gate in .githooks/commit-msg.
#
# WHY THIS FILE EXISTS. On 2026-08-15, 4acd425b "Remove parent Git serialization
# blockers" deleted this gate from both .githooks/commit-msg and
# .githooks/dev-hermit-hook-dispatch. Attribution serializes nothing, so it was
# collateral rather than the target -- and the same commit left the
# canonical-disclosure section of .githooks/hygiene-policy.md
# standing, so the policy went on mandating a trailer nothing checked.
#
# NOTHING WENT RED. A guard that stops enforcing emits no error and leaves a
# clean-looking history; the only symptom is the count of commits that would
# have been refused. It took 10 days and 107 such commits on parent main before
# anyone noticed. That silence is the defect this file removes: from here on,
# deleting the gate fails a test by name instead of passing quietly.
#
# Both halves matter. A gate that never fires is useless, and a gate that fires
# on compliant work gets switched off -- which is worse than none, because it
# also teaches people to ignore it. So the positive case is not decoration.
#
# Everything runs in a throwaway repo under a tmpdir. Fixture commits use a
# reserved example.com identity, which validate_author_identity() permits
# precisely so tests need not borrow the owner's name.

set -uo pipefail

HOOKS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$HOOKS_DIR/.." && pwd)"
HOOK="$HOOKS_DIR/commit-msg"
CHECKER="$ROOT/scripts/check_commit_attribution.py"
DEMO="$ROOT/scripts/check-demo-review.sh"
[ -f "$HOOK" ]    || { echo "FATAL: hook missing at $HOOK"; exit 1; }
[ -f "$CHECKER" ] || { echo "FATAL: checker missing at $CHECKER"; exit 1; }

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  PASS  $1"; }
bad() { fail=$((fail+1)); echo "  FAIL  $1"; [ -n "${2:-}" ] && echo "        $2"; }

new_repo() {
  local d; d=$(mktemp -d)
  git init -q -b main "$d"
  mkdir -p "$d/.githooks" "$d/scripts"
  cp "$HOOK" "$d/.githooks/commit-msg";      chmod +x "$d/.githooks/commit-msg"
  cp "$CHECKER" "$d/scripts/"
  [ -f "$DEMO" ] && cp "$DEMO" "$d/scripts/"
  git -C "$d" config core.hooksPath .githooks
  git -C "$d" config user.name "fixture agent"
  git -C "$d" config user.email fixture@example.com
  printf 'seed\n' > "$d/f.txt"
  git -C "$d" add f.txt
  git -C "$d" commit -q --no-verify -m seed
  printf '%s\n' "$d"
}
tip() { git -C "$1" rev-parse HEAD; }

# Commit $2 as the message in repo $1; echo the exit status. Output to $3.
try_commit() {
  local d="$1" msg="$2" out="$3"
  printf 'change\n' >> "$d/f.txt"
  printf '%s\n' "$msg" > "$d/msg.txt"
  DG_AGENT_NAME=fixture-agent HERMIT_COMMIT_MACHINE=devbig014 \
    git -C "$d" commit -F "$d/msg.txt" -- f.txt > "$out" 2>&1
  echo $?
}

TRAILER='[impl agent, opus-5] [hermit2, devbig014]'
DISCLOSURE='[hermit2, fixture-agent, unresolved, devbig014, role=impl]'

# ------------------------------------------------------------------ NEGATIVE 1
# The exact regression 4acd425b introduced: an unattributed commit is accepted.
r=$(new_repo); before=$(tip "$r")
rc=$(try_commit "$r" "no attribution here" /tmp/cma1.$$)
if [ "$rc" -ne 0 ] && [ "$(tip "$r")" = "$before" ]; then
  ok "an unattributed commit is refused and the ref does not move"
else
  bad "unattributed commit was ACCEPTED (rc=$rc)" "$(head -2 /tmp/cma1.$$)"
fi

# ------------------------------------------------------------------ NEGATIVE 2
# ASSERT THE REFUSAL, NOT ONLY THE WORDING -- but the wording is the whole
# remedy here, so name the required format without pinning the full sentence.
if grep -q 'devbigNNN' /tmp/cma1.$$; then
  ok "the refusal names the required disclosure format"
else
  bad "refusal did not tell the author what to write" "$(head -2 /tmp/cma1.$$)"
fi
rm -f /tmp/cma1.$$

# ------------------------------------------------------------------ NEGATIVE 3
# A disclosure that is present but not first in the body must not satisfy the
# gate; otherwise any commit quoting the format later in its body would pass.
before=$(tip "$r")
rc=$(try_commit "$r" "$(printf 'subject\n\nintroductory prose\n\n%s\n\n%s' "$DISCLOSURE" "$TRAILER")" /tmp/cma3.$$)
if [ "$rc" -ne 0 ] && [ "$(tip "$r")" = "$before" ]; then
  ok "a disclosure that is not the first body line is refused"
else
  bad "a misplaced disclosure was accepted (rc=$rc)"
fi
rm -f /tmp/cma3.$$

# ------------------------------------------------------------------ NEGATIVE 4
# Once the first-body disclosure is authoritative, a second disclosure-looking
# line must not be silently ignored by the real commit hook.
before=$(tip "$r")
rc=$(try_commit "$r" "$(printf 'subject\n\n%s\n\nbody\n\n%s' "$DISCLOSURE" "$DISCLOSURE")" /tmp/cma-duplicate.$$)
if [ "$rc" -ne 0 ] && [ "$(tip "$r")" = "$before" ]; then
  ok "a second canonical disclosure is refused and the ref does not move"
else
  bad "a duplicate canonical disclosure was accepted (rc=$rc)"
fi
rm -f /tmp/cma-duplicate.$$

# ------------------------------------------------------------------ NEGATIVE 5
# The gate must fail closed. A missing checker is an unenforced policy, which is
# the state this whole file exists to prevent, so it cannot exit 0.
before=$(tip "$r"); mv "$r/scripts/check_commit_attribution.py" "$r/scripts/.away"
rc=$(try_commit "$r" "$(printf 'subject\n\n%s' "$DISCLOSURE")" /tmp/cma4.$$)
if [ "$rc" -ne 0 ] && [ "$(tip "$r")" = "$before" ]; then
  ok "a missing checker fails closed rather than silently passing"
else
  bad "missing checker allowed the commit (rc=$rc)"
fi
mv "$r/scripts/.away" "$r/scripts/check_commit_attribution.py"
rm -f /tmp/cma4.$$

# ------------------------------------------------------------------- POSITIVE 1
# The half that keeps the gate switched on: compliant work must sail through.
before=$(tip "$r")
rc=$(try_commit "$r" "$(printf 'subject\n\n%s\n\nValidation: pass' "$DISCLOSURE")" /tmp/cma5.$$)
if [ "$rc" -eq 0 ] && [ "$(tip "$r")" != "$before" ]; then
  ok "a commit beginning its body with the canonical disclosure is accepted"
else
  bad "a COMPLIANT commit was refused (rc=$rc)" "$(head -3 /tmp/cma5.$$)"
fi
rm -f /tmp/cma5.$$

# ------------------------------------------------------------------- POSITIVE 2
# Stored history and noninteractive writers still use the old final-line form.
# Compatibility is explicit and tested rather than an accidental regex overlap.
before=$(tip "$r")
rc=$(try_commit "$r" "$(printf 'subject\n\nbody\n\n%s' "$TRAILER")" /tmp/cma-legacy.$$)
if [ "$rc" -eq 0 ] && [ "$(tip "$r")" != "$before" ]; then
  ok "the explicit legacy final-line form remains accepted"
else
  bad "a legacy commit was refused (rc=$rc)" "$(head -3 /tmp/cma-legacy.$$)"
fi
rm -f /tmp/cma-legacy.$$

# ------------------------------------------------------------------- POSITIVE 3
# The dispatcher sets this after checking. Honouring it is what stops a parent
# commit being checked twice; ignoring it would double-report every refusal.
before=$(tip "$r")
printf 'change\n' >> "$r/f.txt"
printf 'no attribution here\n' > "$r/msg.txt"
HERMIT_MACHINE_ATTRIBUTION_CHECKED=1 \
  git -C "$r" commit -F "$r/msg.txt" -- f.txt > /tmp/cma6.$$ 2>&1
rc=$?
if [ "$rc" -eq 0 ] && [ "$(tip "$r")" != "$before" ]; then
  ok "HERMIT_MACHINE_ATTRIBUTION_CHECKED=1 suppresses the duplicate check"
else
  bad "the dispatcher's already-checked flag was ignored (rc=$rc)" "$(head -2 /tmp/cma6.$$)"
fi
rm -f /tmp/cma6.$$
rm -rf "$r"

echo
echo "commit-msg-attribution: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
