#!/bin/bash
set -u
HERE=/home/newton/work/dev-hermit/experiments/first-divergence_20260820
TAG=$1; shift
N=$1; shift
for i in $(seq -w 1 "$N"); do
  rm -rf "$HERE/keep/$TAG/r$i"
  "$HERE/boot5.sh" "$HERE/keep/$TAG/r$i" "$@"
done
echo "ARM5 $TAG DONE flags=$*"
