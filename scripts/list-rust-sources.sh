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
# ⚠️ THE "OWN WORKSPACE" EXCLUSION IS DERIVED, NOT A DIRECTORY LIST, AND THAT
# CHANGE HAS A MEASURED REASON. `shmem_exec_obj/` and `crates-squat-staging/` were
# excluded whole because their CONTENTS belong to nested Cargo workspaces. That is
# true and confirmed -- 19 and 25 tracked Cargo.toml respectively -- but the
# justification is per-FILE while the exclusion was per-DIRECTORY, so a top-level
# rust-script sitting BESIDE those crates was skipped too, despite belonging to no
# workspace and so having no other gate at all. Measured 2026-08-25:
# `crates-squat-staging/drip-squat.rs` is a live publisher tool, was outside every
# Rust gate, and was unformatted.
#
# Asking the repository the real question -- "does an ancestor directory own a
# Cargo.toml?" -- hands a file to another workspace's `cargo fmt` only when such a
# workspace actually exists to receive it, and a new nested crate is excluded BY
# DEFAULT without editing any list.
crate_dirs="$(git ls-files -- '*Cargo.toml' | while IFS= read -r t; do
    d=${t%/Cargo.toml}
    [ "$d" = "$t" ] && d=.
    printf '%s\n' "$d"
done | sort -u)"

owned_by_a_nested_workspace() {
    local dir=$1
    case "$dir" in */*) dir=${dir%/*} ;; *) return 1 ;; esac
    while [ -n "$dir" ] && [ "$dir" != "." ]; do
        printf '%s\n' "$crate_dirs" | grep -qxF "$dir" && return 0
        case "$dir" in */*) dir=${dir%/*} ;; *) dir="" ;; esac
    done
    return 1
}

# DATED RECORDS stay excluded, but by the narrowest path that is genuinely an
# archive. `ai_docs/` as a whole was too broad: `ai_docs/reference/` is LIVE
# TOOLING -- a staleness dashboard and an update driver, both executable -- and
# only `ai_docs/archived/` is a capture of what was run. Editing a capture to
# satisfy a formatter changes the record; editing live tooling does not.
# The dated-record rule is SHARED; see scripts/lint-exclusions.sh for why, and
# for the two named per-language deviations.
. "$(dirname "${BASH_SOURCE[0]}")/lint-exclusions.sh"

git ls-files -z -- '*.rs' | while IFS= read -r -d '' f; do
    [ -f "$f" ] || continue          # skip gitlinks (hermit, reverie, liteinst2, agent-utils)
    lint_is_dated_record "$f" && continue
    owned_by_a_nested_workspace "$f" && continue
    printf '%s\n' "$f"
done | sort
