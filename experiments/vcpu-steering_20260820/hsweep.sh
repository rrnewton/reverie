#!/usr/bin/env bash
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/hsweep.tsv
printf "arm\ttag\tfirst\tother\tother_started\twin\tv0_ticks\tv1_ticks\n" > "$out"
run () {
  arm=$1; tag=$2; shift 2
  python3 hermit_run.py "$tag" --k 30000000 --settle 25 --timeout 900 "$@" > "$D/hlog-$tag.txt" 2>&1
  l=$(grep -a "^ADVANCE R" "$D/h-$tag.capture" 2>/dev/null | head -1)
  w=$(grep -a "^HOSTCPU" "$D/hlog-$tag.txt" | head -1)
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$arm" "$tag" \
    "$(echo "$l"|sed -n 's/ADVANCE R\([01]\).*/\1/p')" \
    "$(echo "$l"|sed -n 's/.*other=\([0-9]*\).*/\1/p')" \
    "$(echo "$l"|sed -n 's/.*other_started=\([0-9]*\).*/\1/p')" \
    "$(echo "$w"|sed -n 's/.*window=\([0-9.]*\)s.*/\1/p')" \
    "$(echo "$w"|sed -n 's/.*v0_ticks=\([0-9-]*\).*/\1/p')" \
    "$(echo "$w"|sed -n 's/.*v1_ticks=\([0-9-]*\).*/\1/p')" >> "$out"
}
for r in 1 2 3 4; do run "strict" "b$r"; done
for s in 1 2 3 7 11 42; do
  run "chaos-slow-s$s" "c$s" --hermit-extra "--chaos --seed $s --chaos-per-thread-slowdown --chaos-slowdown-max-factor 8"
done
echo HSWEEPDONE >> "$out"
