#!/usr/bin/env bash
# INDEPENDENT VERIFICATION of the ancestry+monotonicity pin gate.
#
# Runs the REAL compiled checker through its REAL launcher against the REAL
# rrnewton/reverie remote, using REAL Reverie SHAs. No fixture remote, no stubs,
# no test harness -- so it shares nothing with the author's unit suite.
#
# Every fixture is an inert temp git repo. Nothing here can authorize anything:
# it plants pins and reads a verdict.
#
# Usage:  ./prove-refusals.sh /path/to/hermit-checkout-with-the-change
#
# Exit 0 iff all four cases produce the required verdict.
set -uo pipefail

HERMIT=$(cd -- "${1:?usage: prove-refusals.sh <hermit-checkout>}" && pwd)  # absolute: we cd into fixtures
RUNNER="$HERMIT/ci/run-reverie-pin-check.sh"
[[ -x $RUNNER ]] || { echo "no runner at $RUNNER" >&2; exit 2; }

# STATE WHICH CHECKER WAS ACTUALLY TESTED. Fleet rule 2026-08-08: never trust a
# moving reference in a shared checkout. A verifier extracted a checker with
# `git show FETCH_HEAD:...` from the shared hermit primary, another agent
# fetched concurrently, FETCH_HEAD moved, and it silently tested the OLD binary
# -- then reported 4 of 4 refusals FAILING on a gate that works. Nothing
# alerted it. So this proof refuses to be silent about its own subject: it
# prints the exact SHA and dirty state it measured, and warns when pointed at a
# shared primary whose HEAD another agent can move mid-run.
TESTED_SHA=$(git -C "$HERMIT" rev-parse HEAD 2>/dev/null || echo UNKNOWN)
TESTED_DIRTY=$(git -C "$HERMIT" status --porcelain 2>/dev/null | wc -l)
case $HERMIT in
  */worktrees/*) SHARED="no (slot worktree)" ;;
  *) SHARED="YES -- shared primary; HEAD can move under you mid-run" ;;
esac
echo "CHECKER UNDER TEST"
echo "  path      $HERMIT"
echo "  HEAD      $TESTED_SHA"
echo "  dirty     $TESTED_DIRTY file(s)$([[ $TESTED_DIRTY != 0 ]] && echo '  <-- results describe the WORKING TREE, not that commit')"
echo "  shared    $SHARED"
echo

# REAL Reverie commits. Ancestry is stable forever, so these do not rot as main
# advances -- which is itself a property of the new rule.
ANCESTOR_OLD=038e993926e45514264d30367b70df9b6ac3b9b8  # real, on main, old
ANCESTOR_MID=fb963d90dc6c5a136cfff23d3e898ab06f8cb265  # real, on main, newer
ANCESTOR_NEW=ab44bbf7351e2329a0de5270d22a02a0c8142f4a  # real, on main, newest of these
# REAL commit that main has NEVER contained: it lived only on the abandoned
# branch rename/dbi-to-dbt (PR #390) and was rebased away before merge. This is
# a genuine dead commit found in the wild, not a synthetic SHA -- and it is
# object-PRESENT in a blobless fetch of main, which is what makes it the
# interesting case.
OFF_HISTORY=88363a56870dcdd88b13b1309233706b691f6a41

manifest() {
  printf '[dependencies]\nreverie = { git = "https://github.com/rrnewton/reverie.git", rev = "%s" }\n' "$1"
}

# Build an inert repo whose BASE commit pins $1 and whose working tree pins $2.
plant() {
  local base=$1 candidate=$2 dir
  dir=$(mktemp -d "${TMPDIR:-/tmp}/pinproof.XXXXXX")
  git -C "$dir" init -q
  git -C "$dir" config user.email proof@inert.local
  git -C "$dir" config user.name "Pin Proof"
  manifest "$base" > "$dir/Cargo.toml"
  git -C "$dir" add Cargo.toml
  git -C "$dir" commit -qm base
  git -C "$dir" branch -f proofbase
  manifest "$candidate" > "$dir/Cargo.toml"
  git -C "$dir" add Cargo.toml
  printf '%s' "$dir"
}

pass=0; fail=0
check() { # name expected_rc base candidate
  local name=$1 want=$2 base=$3 cand=$4 dir rc out
  dir=$(plant "$base" "$cand")
  out=$(cd "$dir" && with-proxy "$RUNNER" --repo "$dir" --base-ref proofbase 2>&1)
  rc=$?
  local verdict; verdict=$(printf '%s' "$out" | grep -E 'REVERIE PIN LINT|Reverie pin is' | head -1)
  if [[ $rc == "$want" ]]; then
    printf '  [ok]   %-34s rc=%s  %s\n' "$name" "$rc" "${verdict:0:76}"
    pass=$((pass+1))
  else
    printf '  [FAIL] %-34s rc=%s want=%s  %s\n' "$name" "$rc" "$want" "${verdict:0:60}"
    fail=$((fail+1))
  fi
  rm -rf -- "$dir"
}

echo "REAL remote, REAL SHAs, inert fixtures. 0 = accepted, 1 = REFUSED."
echo
echo "POSITIVES (must be accepted -- these alone prove nothing):"
check "lagging pin"        0 "$ANCESTOR_OLD" "$ANCESTOR_MID"
check "equal to base"      0 "$ANCESTOR_NEW" "$ANCESTOR_NEW"
echo
echo "REFUSALS (the entire point):"
check "REGRESSED pin"      1 "$ANCESTOR_NEW" "$ANCESTOR_OLD"
check "OFF-HISTORY pin"    1 "$ANCESTOR_OLD" "$OFF_HISTORY"
echo
printf 'RESULT: %d passed, %d failed\n' "$pass" "$fail"
[[ $fail == 0 ]]
