#!/usr/bin/env bash
# 2x20 factorial: does the inside-guest `yes | head` divergence reproduce under
# --verify-strict, and does it depend on max_timeslice?
#
# ARM A  --max-timeslice=disabled   (the historical corpus configuration)
# ARM B  default max_timeslice      (the flag omitted)
# Both arms: --verify --verify-strict, --no-virtualize-cpuid, same guest argv.
#
# WHAT EACH ARM DECIDES
#   divergence in A only   -> a CONFIGURATION finding: the corpus row runs with a
#                             determinism mechanism switched off.
#   divergence in BOTH     -> a product defect on the north star.
#   divergence in NEITHER  -> the legacy comparator was over-sensitive, and the
#                             historical failure data overstates divergence.
#
# RATE, NOT VERDICT. Every sample's exit status, comparator verdict, and package
# activity is recorded whether it passes or not.
set -uo pipefail

BIN=${BIN:?set BIN to the hermit-strict binary}
OUT=${OUT:?set OUT to the results directory}
N=${N:-20}
mkdir -p "$OUT"

# Package activity is audited PER SAMPLE because only 1 of 6 historical failures
# was package-induced and it was invisible until someone looked. Line counts on
# both the live log and its rotation, so a rotation mid-run cannot hide activity.
pkgcount() {
  local a b
  a=$(wc -l < /var/log/dnf.rpm.log 2>/dev/null || echo 0)
  b=$(wc -l < /var/log/dnf.rpm.log.1 2>/dev/null || echo 0)
  echo "$((a + b))"
}

run_arm() {
  local arm=$1; shift
  local -a extra=("$@")
  for i in $(seq 1 "$N"); do
    local tag="$arm-$(printf '%02d' "$i")"
    local json="$OUT/$tag.verify.json"
    local pkg_before pkg_after start end rc
    pkg_before=$(pkgcount)
    start=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    timeout 300 "$BIN" --log=info run --strict --verify --verify-strict \
      --verify-json "$json" --no-virtualize-cpuid "${extra[@]}" \
      -- bash -c 'set -eu; yes hermit | head -n 3' \
      </dev/null >"$OUT/$tag.stdout" 2>"$OUT/$tag.stderr"
    rc=$?
    end=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    pkg_after=$(pkgcount)
    printf '%s\trc=%s\tstart=%s\tend=%s\tpkg_delta=%s\n' \
      "$tag" "$rc" "$start" "$end" "$((pkg_after - pkg_before))" >> "$OUT/index.tsv"
  done
}

: > "$OUT/index.tsv"
run_arm A --max-timeslice=disabled
run_arm B
echo "done: $(wc -l < "$OUT/index.tsv") samples"
