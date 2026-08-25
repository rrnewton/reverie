#!/usr/bin/env bash
# Apply the zero-warnings rule to rust-scripts, which cargo cannot see.
#
# WHY THIS EXISTS
# ---------------
# Every line of Rust in this project is warnings-as-errors. `cargo clippy
# --workspace -- -D warnings` cannot enforce that on a rust-script, because
# cargo never compiles one: it is a standalone file with an inline manifest,
# reached only through the `rust-script` shebang. So the rule was unenforced on
# this whole population BY CONSTRUCTION, and no amount of discipline would have
# caught it -- a dead_code warning in ci-hub/lib/qualifying_receipt.rs reached
# the owner because nothing was looking.
#
# WHY IT COMPILES RATHER THAN RUNS
# --------------------------------
# `rust-script --package` emits the generated Cargo package WITHOUT compiling or
# running it; this then runs `cargo check` over that manifest. Running each
# script instead would be wrong twice: several have real side effects (they read
# the ledger, write artifacts, take locks), and their exit codes mean something
# already -- `check-ledger-parses.rs` exits non-zero when it FINDS unparseable
# rows, which is it working, not a warning. A gate must not confuse "did not
# compile" with "compiled and reported a finding".
#
# WHY IT IS SHELL
# ---------------
# A gate whose job is to catch Rust that will not compile must not itself
# require compiling Rust first.
#
# SCOPE
# -----
# Every tracked file whose first line invokes `rust-script`. Pass paths as
# arguments to check a subset.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root" || exit 2

if ! command -v rust-script >/dev/null 2>&1; then
  echo "lint-rust-scripts: rust-script is not on PATH" >&2
  exit 2
fi

# Frozen historical artifacts. `experiments/` and `benchmarks/` are dated
# records of what was run at the time; they are not maintained code and holding
# them to today's lint would either freeze the lint or rewrite the record.
#
# ⚠️ THE EXCLUSION IS COUNTED AND PRINTED, NEVER SILENT. A gate that quietly
# skips part of its population reads as "covered everything" when it did not,
# which is the shape of defect this gate was created to close.
excluded_re='^(experiments|benchmarks)/'

count_only=0
if [ "${1:-}" = "--count" ]; then
  count_only=1
  shift
fi

if [ "$#" -gt 0 ]; then
  scripts=("$@")
  skipped=0
else
  mapfile -t all < <(
    git ls-files '*.rs' |
      while IFS= read -r f; do
        [ -f "$f" ] || continue
        head -1 "$f" | grep -q 'rust-script' && printf '%s\n' "$f"
      done
  )
  scripts=()
  skipped=0
  for f in "${all[@]}"; do
    if [[ "$f" =~ $excluded_re ]]; then
      skipped=$((skipped + 1))
    else
      scripts+=("$f")
    fi
  done
  if [ "$skipped" -gt 0 ] && [ "$count_only" -eq 0 ]; then
    echo "lint-rust-scripts: skipping $skipped frozen script(s) under experiments/ and benchmarks/"
  fi
fi

if [ "${#scripts[@]}" -eq 0 ]; then
  # Not silently green. An empty population means the discovery broke, and a
  # gate that passes because it found nothing to check is worse than no gate.
  echo "lint-rust-scripts: found NO rust-scripts; discovery is broken" >&2
  exit 2
fi

# The count the lint target prints beside the result. `make lint` documents that
# a gate reporting OK cannot be trusted unless it also says how much it checked.
if [ "$count_only" -eq 1 ]; then
  printf '%s\n' "${#scripts[@]}"
  exit 0
fi

# The intrinsic half of the rule. Scripts under ci-hub/ and compat-envelope/
# carry a CLOSED, NAMED `#![deny(...)]` set so the property travels with the
# file instead of depending on this gate being remembered. Checked here because
# an intrinsic property that can silently go missing is not intrinsic.
#
# The set is enumerated on purpose: `deny(warnings)` would let a future
# toolchain's new lint stop these scripts RUNNING, since they compile at
# invocation. This gate's own `-D warnings` pass is what catches new lints,
# where a failure blocks a commit rather than an execution.
missing_deny=0
for script in "${scripts[@]}"; do
  case "$script" in
    ci-hub/* | compat-envelope/*) ;;
    *) continue ;;
  esac
  if ! grep -q '^#!\[deny(' "$script"; then
    echo "lint-rust-scripts: $script is missing its #![deny(...)] lint set" >&2
    missing_deny=$((missing_deny + 1))
  fi
done

failed=$missing_deny
checked=0
for script in "${scripts[@]}"; do
  pkg="$(rust-script --package "$script" 2>/dev/null | tail -1)"
  if [ -z "$pkg" ] || [ ! -f "$pkg/Cargo.toml" ]; then
    echo "lint-rust-scripts: FAILED to generate a package for $script" >&2
    failed=$((failed + 1))
    continue
  fi
  out="$(RUSTFLAGS="-D warnings" cargo check --quiet --manifest-path "$pkg/Cargo.toml" 2>&1)"
  rc=$?
  checked=$((checked + 1))
  if [ "$rc" -ne 0 ]; then
    echo "=== $script ===" >&2
    printf '%s\n' "$out" >&2
    failed=$((failed + 1))
  fi
done

echo "lint-rust-scripts: checked $checked script(s) under -D warnings, $failed with findings"
[ "$failed" -eq 0 ] || exit 1
