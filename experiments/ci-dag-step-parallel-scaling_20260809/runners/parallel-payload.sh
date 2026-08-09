#!/usr/bin/env bash
# Fixed-total-work CPU-bound STRONG-SCALING payload.
#
# TOTAL hash units are split into exactly $1 chunks, one forked worker per chunk. Forks therefore
# scale with the WIDTH, not with the unit count, so per-unit process-spawn cost cannot masquerade
# as a scaling regression at high widths (it did in v1: eff_cores floored at ~58/316 because
# xargs -I{} forked once per unit). Total work is constant across widths, so wall(width) is a
# genuine strong-scaling curve and speedup = wall(1)/wall(w) is meaningful.
set -euo pipefail
P=${1:?usage: parallel-payload.sh <workers>}
PAYLOAD=${PAYLOAD:-/tmp/scaling-payload.bin}
TOTAL=${TOTAL:-576}
pids=()
for ((c=0; c<P; c++)); do
  # Chunk c takes units c, c+P, c+2P, ... -> ceil/floor split, total is exactly TOTAL.
  n=$(( TOTAL / P + ( c < TOTAL % P ? 1 : 0 ) ))
  if (( n > 0 )); then
    ( for ((i=0;i<n;i++)); do sha256sum "$PAYLOAD" >/dev/null; done ) &
    pids+=($!)
  fi
done
for p in "${pids[@]}"; do wait "$p"; done
