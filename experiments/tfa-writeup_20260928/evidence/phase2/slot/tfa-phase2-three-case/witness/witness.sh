#!/bin/bash
# Usage: witness.sh --case C --template pass|fail --out <new attempt dir> [--seed N] [--note TEXT]
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
LT=/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/c62dfa3398b37794/testinfra/tfa/tests/__tfa_phase2_test__/tfa_phase2_test#link-tree
cd "$E3/witness" || exit 2
exec env TMPDIR="$E3/tmpdir" PYTHONPATH="$E3/witness:$E3/relocated:$LT" \
  /usr/local/fbcode/platform010/bin/python3.12 -W ignore "$E3/witness/run_witness.py" "$@"
