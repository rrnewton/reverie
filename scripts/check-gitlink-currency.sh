#!/usr/bin/env bash
# Judge whether the parent's SUBMODULE GITLINKS are CURRENT.
#
# WHY THIS EXISTS, AND WHY IT IS A NEW FILE RATHER THAN AN EDIT.
#
# On 2026-08-25 the parent pinned hermit at a commit 374 COMMITS BEHIND
# rrnewton/hermit:main, and every existing check was green. Nothing was wrong
# with those checks -- they were measuring other things:
#
#   hermit/scripts/check-reverie-pin.rs   judges hermit's REVERIE CARGO PIN
#                                         (`rev = ...` in tracked Cargo.toml /
#                                         Cargo.lock). Correct, current, and it
#                                         never reads a parent gitlink.
#   scripts/check-parent-gitmodules.sh    asserts the paths ARE gitlinks and use
#                                         the checkout policy. Structural, not
#                                         currency.
#   check-git-pin-uniformity.rs           asserts each git dependency is pinned
#                                         at exactly ONE revision -- UNIFORMITY,
#                                         not currency.
#
# ⚠️ THAT LAST ONE IS WHY THE GAP SURVIVED. Its output says "Scope: 21 tracked
# Cargo metadata file(s) PLUS tracked submodule gitlinks". Anyone asking "is
# something watching the gitlinks?" finds the word `gitlinks` in a green check
# and stops looking. The existing check is what made the missing check invisible.
# So: DO NOT fold this into pin-uniformity. It is doing its own job correctly.
#
# THE THRESHOLD, AND WHY IT IS THIS AND NOT ZERO.
#
# Requiring the pin to EQUAL the tip was tried and rejected by the owner on
# 2026-08-08 for the reverie Cargo pin, on the grounds that it makes the verdict
# "a property of WHEN you looked rather than of the pin". That reasoning applies
# here unchanged: main lands work continuously, so a pin is ALWAYS behind by the
# time you read it, and a tip-equality gate would be red permanently and
# therefore ignored.
#
# But ancestry ALONE is what permitted 374 commits. So the rule is ancestry PLUS
# a bounded lag, and the bound is chosen by REVIEWABILITY:
#
#   WARN  >  50 commits   a bump is still readable as a changelog; roughly a day
#                         of this repo's throughput (measured 2026-08-25: 374
#                         commits in ~83h on hermit, ~4.5/h average, peaking
#                         near 24/h). At this size a reviewer can still say what
#                         they are accepting.
#   FAIL  > 200 commits   past here a bump stops being reviewable and becomes a
#                         leap of faith. That is exactly how 374 accumulated
#                         without anyone deciding to accept it.
#
# Re-derive the churn before trusting those numbers; they are calibration, not
# law. Override with GITLINK_WARN_COMMITS / GITLINK_FAIL_COMMITS.
#
# THREE OUTCOMES, AND THE THIRD IS NOT A PASS. If the submodule's remote cannot
# be read, this reports INDETERMINATE and exits 2. It never treats an unanswered
# question as a current pin.

set -uo pipefail

WARN=${GITLINK_WARN_COMMITS:-50}
FAIL=${GITLINK_FAIL_COMMITS:-200}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root" || exit 2

echo "check-gitlink-currency: judging PARENT GITLINKS by ancestry and bounded lag."
echo "  Scope: tracked submodule gitlinks in this repository's HEAD commit."
echo "  NOT in scope: Cargo rev pins (hermit/scripts/check-reverie-pin.rs),"
echo "                gitlink structure (scripts/check-parent-gitmodules.sh),"
echo "                pin uniformity (check-git-pin-uniformity.rs)."
echo "  Thresholds: WARN >${WARN} commits behind, FAIL >${FAIL} commits behind."

rc=0
mapfile -t links < <(git ls-tree HEAD | awk '$2=="commit"{print $4}')
if [[ ${#links[@]} -eq 0 ]]; then
  echo "INDETERMINATE: no gitlinks found in HEAD -- this check evaluated nothing." >&2
  exit 2
fi

for sm in "${links[@]}"; do
  pin=$(git rev-parse "HEAD:$sm" 2>/dev/null) || {
    echo "  $sm: INDETERMINATE -- cannot read the gitlink" >&2; rc=2; continue; }
  if [[ ! -d $sm/.git && ! -f $sm/.git ]]; then
    echo "  $sm: INDETERMINATE -- submodule not initialised; cannot judge currency" >&2
    rc=2; continue
  fi
  main=$(git -C "$sm" rev-parse origin/main 2>/dev/null) || {
    echo "  $sm: INDETERMINATE -- cannot resolve origin/main in the submodule" >&2; rc=2; continue; }
  if ! git -C "$sm" cat-file -e "$pin^{commit}" 2>/dev/null; then
    echo "  $sm: INDETERMINATE -- pinned commit $pin is not present locally; fetch first" >&2
    rc=2; continue
  fi

  if ! git -C "$sm" merge-base --is-ancestor "$pin" "$main" 2>/dev/null; then
    echo "  $sm: FAIL -- OFF-MAIN. Pin $pin is NOT an ancestor of origin/main ($main)."
    echo "      A gitlink left on a feature branch is a different fault from a stale one:"
    echo "      the remedy is to move it back onto main, not to bump it."
    rc=1; continue
  fi

  behind=$(git -C "$sm" rev-list --count "$pin..$main")
  pindate=$(git -C "$sm" log -1 --format=%cd --date=format:'%Y-%m-%d %H:%M' "$pin")
  wt=$(git -C "$sm" rev-parse HEAD 2>/dev/null || echo unknown)
  note=""
  if [[ $wt != "$pin" && $wt != unknown ]]; then
    if git -C "$sm" merge-base --is-ancestor "$pin" "$wt" 2>/dev/null; then
      note="  [worktree is AHEAD of the pin at ${wt:0:12} -- an uncommitted bump; commit it as its own change]"
    else
      note="  [worktree ${wt:0:12} is not a descendant of the pin -- investigate separately]"
    fi
  fi

  if (( behind > FAIL )); then
    echo "  $sm: FAIL -- pin ${pin:0:12} ($pindate) is $behind commits behind origin/main (>$FAIL).$note"
    echo "      A bump this large is not cleanup. It needs its own change, its own review,"
    echo "      and a validate anyone can point at."
    rc=1
  elif (( behind > WARN )); then
    echo "  $sm: WARN -- pin ${pin:0:12} ($pindate) is $behind commits behind origin/main (>$WARN).$note"
    [[ $rc -eq 0 ]] && rc=0
  else
    echo "  $sm: OK -- pin ${pin:0:12} ($pindate) is $behind commits behind origin/main.$note"
  fi
done

case $rc in
  0) echo "check-gitlink-currency: OK -- every gitlink is on main and within the lag bound." ;;
  1) echo "check-gitlink-currency: FAILED -- at least one gitlink is off-main or past the lag bound." >&2 ;;
  2) echo "check-gitlink-currency: INDETERMINATE -- at least one gitlink could not be judged. THIS IS NOT A PASS." >&2 ;;
esac
exit $rc
