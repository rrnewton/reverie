#!/bin/bash
# Run N demo-5-shaped boots under one instrument, concurrently, into
# FIXED-WIDTH run-dir names. Fixed width is not cosmetic: prior work
# (experiments/detcore-partial-determinism_20260819) showed one extra character
# in the output pathname is worth 10 ns of guest-visible logical time, so a
# varying-width name is a varying INPUT and would manufacture a divergence.
#
# $1 = prefix (2 chars), $2 = count, rest = extra hermit flags.
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
PREFIX=$1; shift
N=$1; shift
export RUST_LOG="warn,detcore=info,reverie_ptrace::task=info"
for i in $(seq -w 1 "$N"); do
  rm -rf "$HERE/runs/$PREFIX$i"
  "$HERE/boot_one.sh" "$HERE/runs/$PREFIX$i" "$@" &
done
wait
echo "ARM $PREFIX DONE flags=$*"
