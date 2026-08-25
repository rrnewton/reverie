# Shared exclusion rule for the lint enumerators. Source, do not execute.
#
# WHY THIS EXISTS
# ===============
# Four enumerators each carried their own hand-maintained exclusion list, and no
# two agreed. Measured 2026-08-25 on dev-hermit main ffe9a3a4:
#
#   list-rust-sources.sh   experiments, benchmarks, dev_transcripts,
#                          ai_docs/archived, + a derived nested-workspace rule
#   list-shell-scripts.sh  experiments, ai_docs (all of it), demos/common.sh
#   list-python-files.sh   experiments, ai_docs (all of it), benchmarks,
#                          dev_transcripts, shmem_exec_obj, crates-squat-staging
#   lint-rust-scripts.sh   experiments, benchmarks
#
# They agreed on `experiments/` and nothing else. The reasons written beside those
# lists are properties of a DIRECTORY -- "dated record", "has its own Cargo
# workspace" -- but each gate applied them to its own LANGUAGE only. The visible
# consequence: a shell script inside a nested Cargo crate IS linted while the Rust
# file beside it is NOT. That is one duplicated rule drifting in N-1 places, which
# is the defect the enumerators were introduced to remove in the first place.
#
# ⚠️ THIS IS A REFACTOR, NOT A COVERAGE CHANGE, AND THAT WAS MEASURED FIRST.
# Before writing it, each gate's newly-covered set under this rule was enumerated
# and counted: 0 for rustfmt, 0 for shellcheck, 0 for python, 0 for the warnings
# gate. The four rules disagree ON PAPER, but every directory they disagree about
# happens to contain no files of the other languages. So nothing is newly checked
# today and nothing goes red. The value is that the NEXT file added to one of
# those directories is treated consistently instead of by whichever list was
# edited last.
#
# ⚠️ DEVIATIONS ARE NAMED, NOT ERASED. A shared rule that flattened the real
# per-language differences would be a different defect wearing a tidier shape.
# There are exactly two, and each has a mechanism behind it:
#
#   NESTED CARGO WORKSPACE -- RUST ONLY.  A .rs file under a directory owning a
#     Cargo.toml belongs to that workspace's own `cargo fmt`/`clippy`, so this
#     tree's Rust gates hand it over. Shell and Python in those same directories
#     have NO other gate to receive them, so they stay covered here. Measured:
#     12 shell scripts live in shmem_exec_obj/ and crates-squat-staging/, they
#     pass shellcheck today, and excluding them to "match the Rust rule" would
#     lose 12 passing checks and gain nothing.
#
#   demos/common.sh -- SHELL ONLY.  A sourced fragment rather than a script; it
#     has no shebang and no independent execution path.
#
# ai_docs IS SPLIT ON PURPOSE. `ai_docs/reference/` is LIVE TOOLING -- an
# executable staleness dashboard and an update driver -- and is checked.
# Everything else under ai_docs/ (archived/, evidence/, rescue-*/) is a dated
# capture of what was run, and editing a capture to satisfy a linter changes the
# record. Excluding `ai_docs/archived/` alone was too narrow: it would have
# covered `ai_docs/evidence/` had any file of that language existed there.

# A dated record: a capture of what was run, in every language. Editing one to
# satisfy a linter rewrites history rather than fixing code.
lint_is_dated_record() {
    case "$1" in
        ai_docs/reference/*) return 1 ;;   # live tooling, checked -- must precede the ai_docs case
        experiments/*|benchmarks/*|dev_transcripts/*|ai_docs/*) return 0 ;;
    esac
    return 1
}
