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
# WHY IT RUNS --help
# ------------------
# `rust-script --package` plus `cargo check` proves that the generated package
# type-checks, but it does not prove the script can build and start. Every
# maintained script therefore runs its own `--help` path and must print usage.
# That is the narrow probe the owner requested: it proves compilation, linking,
# startup, and argument parsing. It does NOT claim to test ordinary behavior or
# prove that every help path is side-effect-free; `check-harness-help.py` carries
# the stronger purity checks for the entrypoints with measured exposure.
#
# `--force` matters for scripts that import local `#[path]` modules. rust-script's
# ordinary cache key follows the top-level file; a shared module can change while
# the cached binary remains stale. Every commit must compile the source present
# in that commit, not yesterday's cached binary.
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
if ! command -v timeout >/dev/null 2>&1; then
  echo "lint-rust-scripts: timeout is not on PATH" >&2
  exit 2
fi

smoke_timeout_seconds="${RUST_SCRIPT_SMOKE_TIMEOUT_SECONDS:-300}"
case "$smoke_timeout_seconds" in
  '' | *[!0-9]*)
    echo "lint-rust-scripts: RUST_SCRIPT_SMOKE_TIMEOUT_SECONDS must be a positive integer" >&2
    exit 2
    ;;
  0)
    echo "lint-rust-scripts: RUST_SCRIPT_SMOKE_TIMEOUT_SECONDS must be positive" >&2
    exit 2
    ;;
esac

# Frozen historical artifacts. `experiments/` and `benchmarks/` are dated
# records of what was run at the time; they are not maintained code and holding
# them to today's lint would either freeze the lint or rewrite the record.
#
# ⚠️ THE EXCLUSION IS COUNTED AND PRINTED, NEVER SILENT. A gate that quietly
# skips part of its population reads as "covered everything" when it did not,
# which is the shape of defect this gate was created to close.
# The dated-record rule is SHARED; see scripts/lint-exclusions.sh for why, and for
# the two named per-language deviations. This gate previously carried its own
# two-directory list, which was the narrowest of the four and agreed with none of
# them.
. "$(dirname "${BASH_SOURCE[0]}")/lint-exclusions.sh"

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
    if lint_is_dated_record "$f"; then
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
  out="$(RUSTFLAGS="-D warnings" timeout --signal=KILL "$smoke_timeout_seconds" \
    rust-script --force "$script" --help 2>&1)"
  rc=$?
  checked=$((checked + 1))
  if [ "$rc" -ne 0 ]; then
    echo "=== $script ===" >&2
    printf '%s\n' "$out" >&2
    failed=$((failed + 1))
  elif ! grep -qi 'usage' <<<"$out"; then
    echo "=== $script ===" >&2
    echo "lint-rust-scripts: --help exited 0 without printing usage" >&2
    printf '%s\n' "$out" >&2
    failed=$((failed + 1))
  else
    echo "lint-rust-scripts: OK $script --help"
  fi
done

echo "lint-rust-scripts: checked $checked script(s) by running --help under -D warnings, $failed with findings"
[ "$failed" -eq 0 ] || exit 1
