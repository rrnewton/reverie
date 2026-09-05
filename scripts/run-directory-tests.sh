#!/usr/bin/env bash
# Run every test file under scripts/tests/ and ci-hub/health/tests/.
#
# ⚠️ NOTHING RAN THESE. Measured 2026-09-05: the Makefile's `py-unittest` line is
# the only scheduled Python discovery and it is `unittest discover -s scripts`,
# which cannot descend into either directory -- neither has an __init__.py, so
# discovery reports "Start directory is not importable" and silently walks past.
# Between them that is 58 Python files carrying 698 test cases, plus 4 shell
# tests, none of which had ever executed in a scheduled context. A test nothing
# runs cannot fail, so an unrun suite is indistinguishable from a passing one.
#
# ⚠️ AND `unittest discover` IS THE WRONG INSTRUMENT, NOT MERELY THE WRONG ROOT.
# TWENTY of those 58 files contain no reference to unittest at all: they are
# standalone scripts with a main() and an `if __name__ == "__main__"` entry.
# Discovery would import each, collect ZERO tests, and report success -- so
# adding __init__.py and widening the root would schedule 38 files, silently
# collect nothing from the other 20, and make the whole directory read as
# covered. That manufactures the exact shape this exists to remove. All 58 run
# correctly when EXECUTED, which is what this does.
#
# Both directories are live: 207 and 37 commits before the day this was written,
# and 52 files added to ci-hub/health/tests in the preceding thirty days.
#
# ⚠️ RUN THIS FROM A POPULATED CHECKOUT. Four of these files need the submodules
# present and fail in a bare detached worktree that has none --
# test_approval_binding_grammar.py, test_approval_binding.sh, test_bounded_gate.py
# and test_merge_gate_authority.py. Measured 2026-09-05: the same runner gave 58
# passed / 0 unwaived failures in the parent checkout and 54 passed / 4 in a
# worktree without submodules. That is a property of those tests, not of this
# runner, and it is recorded here so the next person does not read a bare-worktree
# run as four regressions. `make lint` runs in a populated checkout.
#
# ⚠️ AND DO NOT WAIVE A FILE THAT ONLY SOMETIMES FAILS. test_unpushed_change_clone_commits.py
# failed once during this work and then passed 5 of 5 on re-run; it inspects
# clone commits in a tree other agents are writing to. It is NOT on the list
# below. Every entry there was confirmed to fail three times out of three.
set -uo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT" || exit 1

TEST_DIRS=(scripts/tests ci-hub/health/tests)

# ⚠️ A RATCHET, NOT AN AMNESTY -- and the check below is what keeps it one. Each
# entry is a file that fails today, with the reason. A waived file that starts
# PASSING makes this script fail, so it cannot quietly stay on the list once it
# is fixed and the list can only shrink by being corrected. Same shape as the
# ALLOWLIST in hermit/scripts/check-checker-scheduling.rs.
#
# ⚠️ THESE ARE NOT KNOWN-BAD TESTS. At least one is reporting a real product
# defect that has been sitting unread precisely because nothing ran the file.
# Waiving here buys the other 698 cases their first ever scheduled run; it does
# not bless the three.
WAIVED_REASONS=(
    "ci-hub/health/tests/test_gate_variant_space.py|NOT a test bug: an enumeration over 72 gate cells reporting a REAL defect in ci-hub/bin/changes-requested-gate -- 'silent-or-false: 2', both being FALSE REFUSAL cells where a withdrawal should not have cleared a refusal. Needs its own row; do not fix it by editing this test."
    "ci-hub/health/tests/test_withdrawal_scope.py|1 of 4 cases: test_one_withdrawal_clearing_two_reviewers_refusals_is_reported expects OK and gets 1, against its own words 'the verdict is unchanged; only the report is'. Last touched 2026-08-25."
    "scripts/tests/test-parent-main-write-failure-messages.sh|8 of 9 assertions pass; the failing one is 'stale local main: message does not name the cause (want: not the freshly fetched origin/main)'. A refusal message that does not name its cause, which is a wording defect in parent-main-write rather than in this bracket."
)

waived_reason() {
    local entry
    for entry in "${WAIVED_REASONS[@]}"; do
        if [ "${entry%%|*}" = "$1" ]; then
            printf '%s' "${entry#*|}"
            return 0
        fi
    done
    return 1
}

test_files() {
    git ls-files "${TEST_DIRS[@]}" \
        | grep -E '/(test_[^/]*\.py|test[_-][^/]*\.sh)$' \
        | sort
}

run_one() {
    case "$1" in
        *.sh) timeout 600 bash "$1" ;;
        *)    timeout 600 python3 "$1" ;;
    esac
}

ran=0 passed=0 failed=0 waived_failing=0
declare -a failures=() unexpected_passes=()

while IFS= read -r file; do
    [ -n "$file" ] || continue
    ran=$((ran + 1))
    out=$(run_one "$file" 2>&1)
    rc=$?
    if reason=$(waived_reason "$file"); then
        if [ "$rc" -eq 0 ]; then
            # The ratchet. A waived file that passes must leave the list.
            unexpected_passes+=("$file")
        else
            waived_failing=$((waived_failing + 1))
            printf 'WAIVED FAILURE  %s\n    %s\n' "$file" "$reason"
        fi
        continue
    fi
    if [ "$rc" -eq 0 ]; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        failures+=("$file")
        printf 'FAIL rc=%s  %s\n' "$rc" "$file"
        printf '%s\n' "$out" | tail -15 | sed 's/^/    /'
    fi
done < <(test_files)

# ⚠️ COUNTS TRAVEL WITH THEIR DENOMINATOR. "N passed" alone cannot distinguish a
# healthy suite from one whose file list silently shrank to nothing.
printf 'directory tests: %s file(s) run across %s: %s passed, %s failed, %s waived\n' \
    "$ran" "${TEST_DIRS[*]}" "$passed" "$failed" "$waived_failing"

if [ "$ran" -eq 0 ]; then
    printf 'directory tests: REFUSING -- no test files were found under %s. An empty\n' "${TEST_DIRS[*]}" >&2
    printf '  run is not a pass; either the directories moved or the pattern stopped matching.\n' >&2
    exit 1
fi

if [ "${#unexpected_passes[@]}" -ne 0 ]; then
    printf 'directory tests: REFUSING -- %s waived file(s) now PASS and must be removed\n' \
        "${#unexpected_passes[@]}" >&2
    printf '  from WAIVED_REASONS in %s:\n' "${BASH_SOURCE[0]}" >&2
    printf '    %s\n' "${unexpected_passes[@]}" >&2
    printf '  The waiver list is a ratchet: it shrinks by being corrected, never by drifting.\n' >&2
    exit 1
fi

if [ "$failed" -ne 0 ]; then
    printf 'directory tests: %s unwaived failure(s): %s\n' "$failed" "${failures[*]}" >&2
    exit 1
fi
exit 0
