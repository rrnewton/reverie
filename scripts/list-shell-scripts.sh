#!/usr/bin/env bash
# List the shell scripts the lint gate should check, DERIVED FROM THE REPOSITORY.
#
# WHY THIS EXISTS
# ===============
# The shellcheck gate used to take a hand-maintained list of ten path patterns. A
# script added outside those patterns was unchecked forever and nothing said so: the
# gate passed, having looked at ten patterns rather than at the repository. That is
# not a hypothetical. Measured on 2026-08-20, the list covered 51 of 161 live shell
# scripts, and the 110 it missed included:
#
#   scripts/hermit-box-run          the cgroup-boxing wrapper wired at four call
#                                   sites and referenced by 31 tracked files. Missed
#                                   for exactly the reason bin/safehermit was: no
#                                   .sh extension, so `scripts/*.sh` skips it, and
#                                   unlike its neighbour scripts/parent-main-write
#                                   nobody added it by hand.
#   .githooks/tests/*.sh            the tests FOR the hooks the gate does check --
#                                   the hook was checked, its test was not.
#
# So the list did not merely miss new directories; it missed files inside directories
# it already claimed to cover. Coverage is now derived and the exclusions are named.
#
# SELECTION
# =========
# Tracked files only (`git ls-files`), so untracked local artifacts are never handed
# to the linter -- notably the 58 MB bin/hermit ELF that sits in working checkouts.
# (That sentence avoids starting a line with the tool's name on purpose: a comment
# beginning `# shellcheck ...` is parsed as a DIRECTIVE, and this file tripped SC1072
# on its own prose while being written.)
# A file counts as a shell script if it ends in .sh OR its first line is a
# bash/sh/dash/zsh/ksh shebang. Both, because this repo has scripts with neither an
# extension (scripts/parent-main-write, bin/safehermit) nor a conventional name.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

# EXCLUSIONS ARE NAMED AND JUSTIFIED, one per line, because an unexplained exclusion
# is the same defect as an unexplained omission -- it just looks deliberate.
#
#   experiments/, ai_docs/  Dated one-off records, not tooling. 243 and 2 scripts,
#                           carrying 111 findings between them. Editing a dated
#                           archive to satisfy a linter changes the record of what
#                           was actually run, which is the point of keeping it.
#   demos/common.sh         One SC2034 false positive: HERMIT_ANALYZE_TMP_FLAGS is
#                           set here and read by demos/04-schedule-bisection.sh:76,
#                           which shellcheck cannot see across a `source`. The fix is
#                           a one-line directive, but demos/** mechanically requires
#                           an adversarial green-demo review, and a reviewer running
#                           two QEMU demos to approve a comment is not a good trade
#                           to bundle into this change. Tracked separately; the other
#                           12 scripts under demos/ ARE covered.
# The dated-record rule is SHARED; see scripts/lint-exclusions.sh.
. "$(dirname "${BASH_SOURCE[0]}")/lint-exclusions.sh"

is_excluded() {
    lint_is_dated_record "$1" && return 0
    # SHELL-ONLY DEVIATION, named in lint-exclusions.sh: a sourced fragment,
    # not a script -- no shebang, no independent execution path.
    case "$1" in demos/common.sh) return 0 ;; esac
    return 1
}

git ls-files -z | while IFS= read -r -d '' f; do
    [ -f "$f" ] || continue          # skip gitlinks (hermit, reverie, liteinst2, agent-utils)
    is_excluded "$f" && continue
    case "$f" in
        *.sh) printf '%s\n' "$f"; continue ;;
    esac
    # Read only the first line; -I so a tracked binary can never be misread as text.
    if head -c 200 -- "$f" 2>/dev/null | head -1 |
         grep -qIE '^#!.*\b(bash|sh|dash|zsh|ksh)\b'; then
        printf '%s\n' "$f"
    fi
done | sort
