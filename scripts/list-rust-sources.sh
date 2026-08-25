#!/usr/bin/env bash
# List the Rust sources the lint gate should format-check, DERIVED FROM THE REPOSITORY.
#
# WHY THIS EXISTS
# ===============
# The rustfmt gate ran `rustfmt --check scripts/*.rs`. That is a path pattern, not
# a rule about the repository, so it checked 19 of the 36 Rust sources this
# repository actually owns. The 17 it missed were the tooling that matters most:
#
#   ci-hub/ci-hub.rs          the CI hub entry point
#   ci-hub/lib/*.rs           8 files: landing_lock, validate_lock, validate_status,
#                             lease, records, measured, qualifying_receipt,
#                             history_queries -- the landing-admission machinery
#   compat-envelope/*.rs      7 files: the scorecard renderer and the corpus
#                             collectors
#
# Eight of those 17 were unformatted when this rule was written, which is the
# ordinary consequence of never being checked. They were formatted first, in their
# own commit, so that switching the gate from a list to a rule did not turn a
# silent gap into a standing red.
#
# This mirrors scripts/list-shell-scripts.sh, which fixed the identical defect in
# the shellcheck gate. The point is that a new directory is covered BY DEFAULT.
#
# SELECTION
# =========
# Tracked *.rs files only (`git ls-files`), so untracked build output is never
# handed to rustfmt. No shebang rule here, unlike the shell and Python lists: a
# rust-script carries the .rs extension even when it is executable, so the
# extension alone is a complete rule for this language.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

# EXCLUSIONS ARE NAMED AND JUSTIFIED, because an unexplained exclusion is the same
# defect as an unexplained omission -- it just looks deliberate.
#
#   experiments/, ai_docs/,   Dated one-off records, not tooling. Editing a dated
#   benchmarks/               archive to satisfy a formatter changes the record of
#                             what was actually run.
#   shmem_exec_obj/           A separate Cargo project staged in this tree (98 .rs
#                             files, its own workspace, its own rustfmt settings,
#                             and tests/ui fixtures whose exact layout is the
#                             assertion). The parent lint gate is not its owner.
#   crates-squat-staging/     27 name-reservation stubs for crates.io, published
#                             from their own workspace. Same reason.
#
# Both excluded projects have their own Cargo workspace, so `cargo fmt` inside
# them is the right gate for their contents -- not this one.
is_excluded() {
    case "$1" in
        experiments/*|ai_docs/*|benchmarks/*)    return 0 ;;
        shmem_exec_obj/*|crates-squat-staging/*) return 0 ;;
    esac
    return 1
}

git ls-files -z -- '*.rs' | while IFS= read -r -d '' f; do
    [ -f "$f" ] || continue          # skip gitlinks (hermit, reverie, liteinst2, agent-utils)
    is_excluded "$f" && continue
    printf '%s\n' "$f"
done | sort
