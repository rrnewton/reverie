#!/usr/bin/env bash
# Every failure of parent-main-write must SAY WHY, by inducing the failure --
# not by reading the source.
#
# THE DEFECT THIS EXISTS FOR. `scripts/parent-main-write` runs under
# `set -euo pipefail`. Its reversion guard builds the refusal message with
#
#     report+=$( { diff ... || true; } | sed -n 's/^> /      LOST: /p' | head -6 )
#
# `head -6` closes the pipe after six lines, `sed` dies of SIGPIPE, `pipefail`
# makes the substitution exit 141, and `set -e` aborts the script -- WHILE IT IS
# BUILDING THE VERY MESSAGE THAT EXPLAINS THE REFUSAL. The author guarded
# `diff`'s exit-1 with `|| true` and the comment above it says so; the `head`
# SIGPIPE was not guarded. Measured directly: the shape exits 141 after printing
# nothing.
#
# So the most safety-critical refusal in the file -- "this publish would drop
# content that landed on main after X" -- was the one that went silent, and only
# when there was a lot to say. Agents saw an attempt that did not work, with no
# explanation, and re-derived the cause by hand. Several did, today.
#
# WHAT IS ASSERTED. Each induced failure must produce a DISTINCT and ACCURATE
# message naming its own cause, and a successful publish must be unaffected.
# Distinctness is checked, not just non-emptiness: four failures that all say
# "refused" would satisfy a weaker test while leaving the reader exactly as
# stranded as silence did.
#
# Fully inert: a temp repo with a local bare origin. Never touches the real
# parent, the real lock, or the network.
set -u

TOOL="${1:-scripts/parent-main-write}"
[ -f "$TOOL" ] || { echo "FATAL: tool not found at '$TOOL'" >&2; exit 2; }
TOOL=$(cd "$(dirname "$TOOL")" && pwd)/$(basename "$TOOL")
CHECKER=$(dirname "$TOOL")/check_commit_attribution.py
[ -x "$CHECKER" ] || { echo "FATAL: checker not found at '$CHECKER'" >&2; exit 2; }
TRAILER='[coordinator, Test] [parent-writer-test, devbig999]'
TAGGED_PROBE=$(printf 'probe\n\n%s' "$TRAILER")

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/pmw-msg-XXXXXX") || exit 2
trap 'rm -rf "$ROOT"' EXIT

PASS=0; FAIL=0
declare -a FAILURES=()
declare -a SEEN_MSG=()

ok()  { printf '  PASS  %s\n' "$*"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL  %s\n' "$*"; FAIL=$((FAIL + 1)); FAILURES+=("$*"); }

# A fresh repo whose "origin" is a local bare repo. Each case gets its own, so
# one case's lock or branch state cannot leak into the next.
new_repo() { # echoes the repo path
  local n="$ROOT/r$RANDOM$RANDOM" origin="$ROOT/o$RANDOM$RANDOM.git"
  git init -q --bare "$origin"
  git -C "$origin" symbolic-ref HEAD refs/heads/main
  git init -q -b main "$n"
  git -C "$n" config user.email pmw@test.local
  git -C "$n" config user.name "pmw bracket"
  mkdir -p "$n/scripts"
  cp "$TOOL" "$n/scripts/parent-main-write"; chmod +x "$n/scripts/parent-main-write"
  cp "$CHECKER" "$n/scripts/check_commit_attribution.py"; chmod +x "$n/scripts/check_commit_attribution.py"
  echo seed > "$n/seed.txt"
  git -C "$n" add -A
  git -C "$n" -c core.hooksPath=/dev/null commit -q -m seed -m "$TRAILER"
  git -C "$n" remote add origin "$origin"
  git -C "$n" push -q origin main
  git -C "$n" fetch -q origin
  printf '%s' "$n"
}

# Run a parent-main-write subcommand and report "<rc>|<combined output>".
run_pmw() { # <repo> <args...>
  local repo="$1"; shift
  local out rc
  # PER-REPO LOCK PATH, always. Without it the fixture takes the REAL
  # /tmp/dev-hermit-parent-main-<uid>.lock, contends with live agents on this
  # shared box, and reports their contention as this case's failure.
  out=$(cd "$repo" && HERMIT_PARENT_MAIN_LOCK_TIMEOUT=1 \
        HERMIT_PARENT_MAIN_LOCK_PATH="$repo/.pmw.lock" \
        HERMIT_COMMIT_MACHINE=devbig999 \
        ./scripts/parent-main-write "$@" 2>&1); rc=$?
  printf '%s|%s' "$rc" "$out"
}

# assert_explains <case-name> <expected-substring> <rc|out>
#
# Three properties per case: it FAILED, it said something, and what it said
# NAMES THIS cause. The third is the one that matters -- silence and a generic
# "refused" are equally useless to the reader.
assert_explains() {
  local name="$1" needle="$2" res="$3"
  local rc="${res%%|*}" out="${res#*|}"
  if [ "$rc" = 0 ]; then
    bad "$name: expected a failure, got rc=0"; return
  fi
  if [ -z "$(printf '%s' "$out" | tr -d '[:space:]')" ]; then
    bad "$name: FAILED SILENTLY (rc=$rc, no output at all)"; return
  fi
  if ! printf '%s' "$out" | grep -qiF -- "$needle"; then
    bad "$name: message does not name the cause (want '$needle')"
    printf '%s\n' "$out" | sed 's/^/          /' | head -4
    return
  fi
  ok "$name: rc=$rc and the message names it ('$needle')"
  SEEN_MSG+=("$(printf '%s' "$out" | tr -d '\n' | head -c 4000)")
}

echo "-- NEGATIVE: each failure mode must name ITS OWN cause --"

# 1. STALE LOCAL MAIN: origin moved, local did not.
repo=$(new_repo)
side=$(mktemp -d "$ROOT/side.XXXXXX"); git clone -q "$(git -C "$repo" remote get-url origin)" "$side/c"
git -C "$side/c" config user.email a@b.c; git -C "$side/c" config user.name a
echo more > "$side/c/other.txt"; git -C "$side/c" add -A
git -C "$side/c" -c core.hooksPath=/dev/null commit -q -m "landed elsewhere" -m "$TRAILER"
git -C "$side/c" push -q origin main
assert_explains "stale local main" "not the freshly fetched origin/main" \
  "$(run_pmw "$repo" commit -m "$TAGGED_PROBE" -- seed.txt)"

# 2. CONTENDED LOCK: someone else holds the serialized writer.
repo=$(new_repo)
lock="$ROOT/held.lock"; : > "$lock"
( exec 9>"$lock"; flock 9; sleep 12 ) & holder=$!
sleep 0.5
res=$(cd "$repo" && HERMIT_PARENT_MAIN_LOCK_PATH="$lock" HERMIT_PARENT_MAIN_LOCK_TIMEOUT=1 \
      ./scripts/parent-main-write sync 2>&1; printf '|rc=%s' $?)
out="${res%|rc=*}"
if printf '%s' "$out" | grep -qiF "another parent-main writer"; then
  ok "contended lock: names the competing writer"
else
  bad "contended lock: did not name lock contention"
  printf '%s\n' "$out" | sed 's/^/          /' | head -4
fi
kill "$holder" 2>/dev/null; wait "$holder" 2>/dev/null

# 3. DIRTY TREE blocking a sync fast-forward.
repo=$(new_repo)
side=$(mktemp -d "$ROOT/side.XXXXXX"); git clone -q "$(git -C "$repo" remote get-url origin)" "$side/c"
git -C "$side/c" config user.email a@b.c; git -C "$side/c" config user.name a
echo upstream > "$side/c/shared.txt"; git -C "$side/c" add -A
git -C "$side/c" -c core.hooksPath=/dev/null commit -q -m "upstream edits shared.txt" -m "$TRAILER"
git -C "$side/c" push -q origin main
echo "local uncommitted" > "$repo/shared.txt"
assert_explains "dirty tree blocks sync" "shared.txt" "$(run_pmw "$repo" sync)"

# 4. ★ THE REVERSION GUARD -- the case that went silent.
#    A commit that would drop MANY lines landed on main. >6 lost lines is what
#    makes `head -6` close the pipe early and kill `sed`.
repo=$(new_repo)
seq 1 5 > "$repo/data.txt"
git -C "$repo" add -A; git -C "$repo" -c core.hooksPath=/dev/null commit -q -m "base data" -m "$TRAILER"
git -C "$repo" push -q origin main
side=$(mktemp -d "$ROOT/side.XXXXXX"); git clone -q "$(git -C "$repo" remote get-url origin)" "$side/c"
git -C "$side/c" config user.email a@b.c; git -C "$side/c" config user.name a
seq 1 5000 > "$side/c/data.txt"        # 4995 lines land on main
git -C "$side/c" add -A; git -C "$side/c" -c core.hooksPath=/dev/null commit -q -m "many lines land on main" -m "$TRAILER"
git -C "$side/c" push -q origin main
git -C "$repo" fetch -q origin
# The commit must DESCEND from the fetched tip or `publish` refuses on ancestry
# before the reversion scan runs. This is the stale-COPY shape: rebased onto the
# tip, but carrying a working copy read against the OLD base, so it silently
# reverts the 55 lines that landed. Exactly what bit scorecard-fixer.
# The edit must MERGE CLEANLY against main's additions, or `git merge-file`
# reports a conflict, the `if` is false, and the guard never reaches its
# message-building path at all. Main appended lines 6..60 at the END, so a
# stale copy that edits LINE 1 and keeps only 1..5 merges cleanly and still
# drops all 55 landed lines.
git -C "$repo" reset -q --hard origin/main
printf 'mine\n2\n3\n4\n5\n' > "$repo/data.txt"
git -C "$repo" add -- data.txt
git -C "$repo" -c core.hooksPath=/dev/null commit -q -m "stale-copy rewrite of data.txt" -m "$TRAILER"
assert_explains "reversion guard, LARGE revert (4995 lost lines)" "would drop content" \
  "$(run_pmw "$repo" publish HEAD)"

# 4b. The truncation summary must say how much it did NOT show. A capped list
#     that does not admit it is capped reads as the whole story.
if printf '%s' "${SEEN_MSG[*]:-}" | grep -qE "more lost line"; then
  ok "large revert states how many lost lines were NOT shown"
else
  bad "large revert truncated its list silently (no '... and N more')"
fi

echo
echo "-- THE TRAP ITSELF: an UNEXPECTED abort must still name itself --"
# Induce a failure the tool does not anticipate, by injecting one. Without this
# the trap is untested and the promise "no silent failure" is unverified.
repo=$(new_repo)
sed 's|^publish_sha() {|publish_sha() {\n  if [ "${PMW_TEST_INJECT:-0}" = 1 ]; then false; fi|' \
  "$TOOL" > "$repo/scripts/parent-main-write"
chmod +x "$repo/scripts/parent-main-write"
echo change > "$repo/seed.txt"; git -C "$repo" add -- seed.txt
git -C "$repo" -c core.hooksPath=/dev/null commit -q -m "a real change" -m "$TRAILER"
out=$(cd "$repo" && PMW_TEST_INJECT=1 HERMIT_PARENT_MAIN_LOCK_TIMEOUT=1 \
      HERMIT_PARENT_MAIN_LOCK_PATH="$repo/.pmw.lock" \
      ./scripts/parent-main-write publish HEAD 2>&1); rc=$?
if [ "$rc" != 0 ] && printf '%s' "$out" | grep -qF "ABORTED at line"; then
  ok "an unexpected abort is LOCATED, not silent (rc=$rc)"
elif [ "$rc" = 0 ]; then
  bad "injected failure did not fail the publish -- injection missed"
else
  bad "an unexpected abort was SILENT (rc=$rc)"
  printf '%s\n' "$out" | sed 's/^/          /' | head -4
fi
# ADMIT CONTROL: the trap must not fire when nothing went wrong.
out=$(cd "$repo" && PMW_TEST_INJECT=0 HERMIT_PARENT_MAIN_LOCK_TIMEOUT=1 \
      HERMIT_PARENT_MAIN_LOCK_PATH="$repo/.pmw.lock" \
      ./scripts/parent-main-write publish HEAD 2>&1) || true
if printf '%s' "$out" | grep -qF "ABORTED at line"; then
  bad "the trap fired on a healthy run (false alarm)"
else
  ok "the trap stays quiet when nothing goes wrong"
fi

echo
echo "-- DISTINCTNESS: four causes must not collapse into one generic refusal --"
uniq_n=$(printf '%s\n' "${SEEN_MSG[@]:-}" | sort -u | grep -c . || true)
tot_n=${#SEEN_MSG[@]}
if [ "$tot_n" -gt 0 ] && [ "$uniq_n" -eq "$tot_n" ]; then
  ok "all $tot_n captured messages are distinct"
else
  bad "only $uniq_n distinct message(s) across $tot_n failures"
fi

echo
echo "-- POSITIVE: a legitimate publish must be unaffected --"
repo=$(new_repo)
echo change > "$repo/seed.txt"
git -C "$repo" add -- seed.txt
git -C "$repo" -c core.hooksPath=/dev/null commit -q -m "a real change" -m "$TRAILER"
res=$(run_pmw "$repo" publish HEAD)
rc="${res%%|*}"; out="${res#*|}"
if [ "$rc" = 0 ] && printf '%s' "$out" | grep -qF "PARENT_MAIN_WRITE published"; then
  ok "successful publish still succeeds and reports its receipt"
else
  bad "successful publish broke (rc=$rc)"
  printf '%s\n' "$out" | sed 's/^/          /' | head -6
fi

echo
echo "======================================================================"
echo "assertions: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  printf 'FAILED: %s\n' "${FAILURES[@]}"
  echo "RESULT: FAIL"
  exit 1
fi
echo "RESULT: PASS"
