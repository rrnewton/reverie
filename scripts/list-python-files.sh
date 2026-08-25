#!/usr/bin/env bash
# List the Python files the lint gate should compile, DERIVED FROM THE REPOSITORY.
#
# WHY THIS EXISTS
# ===============
# The py-compile gate ran `python3 -m py_compile scripts/*.py`. That is a path
# pattern, not a rule about the repository, so it checked 41 of the 360 tracked
# Python files outside the submodules and the 319 it missed were not exotic:
#
#   ci-hub/**                 254 files -- the entire CI hub: validate/, health/,
#                             landing/, ledger/, closure/, remediation/. The
#                             machinery that decides whether a change may land was
#                             not syntax-checked by the gate that claims to check
#                             this repository's Python.
#   compat-envelope/**        28 files.
#   ci-hub/bin/cap-log, gh,   Python tools with NO .py EXTENSION. `scripts/*.py`
#   review-attest, who-am-i   cannot see them by construction -- the same way
#   bin/hermit-repeat         `scripts/*.sh` could not see bin/safehermit.
#
# This mirrors scripts/list-shell-scripts.sh, which fixed the identical defect in
# the shellcheck gate; read its header for the longer argument. The point is that
# a new directory, and a new extensionless tool, are covered BY DEFAULT.
#
# SELECTION
# =========
# Tracked files only (`git ls-files`), so untracked local artifacts are never
# handed to the compiler. A file counts as Python if it ends in .py OR its first
# line is a python shebang. Both, because this repo has Python tools with neither
# an extension nor a conventional name.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

# EXCLUSIONS ARE NAMED AND JUSTIFIED, because an unexplained exclusion is the same
# defect as an unexplained omission -- it just looks deliberate.
#
#   experiments/, ai_docs/,   Dated one-off records, not tooling. Editing a dated
#   benchmarks/,              archive to satisfy a linter changes the record of
#   dev_transcripts/          what was actually run, which is the point of keeping
#                             it. Measured 2026-08-24: all of them compile today,
#                             so this exclusion is not hiding a finding -- it is
#                             refusing to make an archive a maintenance burden.
#   shmem_exec_obj/,          Separate projects staged in this tree with their own
#   crates-squat-staging/     build and their own conventions; the parent lint gate
#                             is not their owner.
# The dated-record rule is SHARED; see scripts/lint-exclusions.sh.
. "$(dirname "${BASH_SOURCE[0]}")/lint-exclusions.sh"

is_excluded() {
    lint_is_dated_record "$1" && return 0
    # NOTE: shmem_exec_obj/ and crates-squat-staging/ are NOT excluded here.
    # The nested-Cargo-workspace deviation is RUST ONLY -- Python in those
    # directories has no other gate to receive it. See lint-exclusions.sh.
    return 1
}

git ls-files -z | while IFS= read -r -d '' f; do
    [ -f "$f" ] || continue          # skip gitlinks (hermit, reverie, liteinst2, agent-utils)
    is_excluded "$f" && continue
    case "$f" in
        *.py) printf '%s\n' "$f"; continue ;;
    esac
    # Read only the first line; -I so a tracked binary can never be misread as text.
    if head -c 200 -- "$f" 2>/dev/null | head -1 | grep -qIE '^#!.*python'; then
        printf '%s\n' "$f"
    fi
done | sort
