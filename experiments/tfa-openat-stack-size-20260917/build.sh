#!/bin/bash
# Build and verify the openat/stack-size probe.
#
# Derived and verified 2026-09-17 by hermit-908. hermit-906, who originally built
# the probe, left no build command anywhere, so this recipe was re-derived from the
# two sources and then checked against the preserved evidence: it must reproduce
# BOTH arms, not just the failing one.
#
#   ./build.sh                 # build only
#   ./build.sh --verify        # build, then run all four arms and check them
#
# --verify needs the frozen ptrace hermit binary; override with HERMIT_BIN=...
set -uo pipefail
cd "$(dirname "$0")"

HERMIT_BIN="${HERMIT_BIN:-/home/newton/work/dev-hermit/ignored/tfa-hermit-binary-readiness/79da523137dbac400faca6130593361b58a01468/hermit}"
HERMIT_SHA_EXPECTED=000eeb5e59345b6c9c14bf194d6d5b392294a52d25d40209e553376f3a84764a
SAFEHERMIT="${SAFEHERMIT:-/home/newton/work/dev-hermit/bin/safehermit}"
OUT="${OUT:-$PWD/tfa_open_stack_probe}"

# THE BUILD COMMAND. The .S is assembled by the same driver invocation; there is no
# third source, no extra header and no extra link dependency.
gcc -O2 -g -o "$OUT" tfa_open_stack_probe.c tfa_open_stack_probe.S || exit 1
echo "built $OUT"
[ "${1:-}" = "--verify" ] || exit 0

# The probe needs one existing, openable file as its second argument. Any will do --
# see the note on path length in README.md. This makes the check self-contained.
TARGET="$PWD/.probe-target"; echo probe-target > "$TARGET"

fail=0
check () { # label expected_rc expected_stdout actual_rc actual_stdout
  if [ "$4" = "$2" ] && [ "$5" = "$3" ]; then echo "  PASS $1 (rc=$4, $5)"
  else echo "  FAIL $1: expected rc=$2 '$3', got rc=$4 '$5'"; fail=1; fi
}

echo "native arms (both must succeed):"
for off in 192 512; do
  o=$("$OUT" "$off" "$TARGET" 2>&1); rc=$?
  check "native/$off" 0 "openat raw_rc=3 errno=0" "$rc" "$o"
done

if [ ! -x "$HERMIT_BIN" ]; then
  echo "hermit arms SKIPPED: $HERMIT_BIN not present -- verification is INCOMPLETE"
  exit 2
fi
got_sha=$(sha256sum "$HERMIT_BIN" | cut -d' ' -f1)
if [ "$got_sha" != "$HERMIT_SHA_EXPECTED" ]; then
  echo "hermit arms SKIPPED: $HERMIT_BIN is sha256 $got_sha, not the frozen"
  echo "  $HERMIT_SHA_EXPECTED -- a different build is not this experiment"
  exit 2
fi

# Every agent-run hermit invocation goes through bin/safehermit (parent AGENTS.md).
echo "hermit arms (192 must REFUSE, 512 must SUCCEED -- both directions required):"
o=$("$SAFEHERMIT" "$HERMIT_BIN" run -- "$OUT" 192 "$TARGET" 2>/dev/null); rc=$?
check "hermit/192" 1 "openat raw_rc=-14 errno=14" "$rc" "$o"
o=$("$SAFEHERMIT" "$HERMIT_BIN" run -- "$OUT" 512 "$TARGET" 2>/dev/null); rc=$?
check "hermit/512" 0 "openat raw_rc=3 errno=0" "$rc" "$o"

[ "$fail" -eq 0 ] && echo "ALL FOUR ARMS REPRODUCE" || echo "VERIFICATION FAILED"
exit "$fail"
