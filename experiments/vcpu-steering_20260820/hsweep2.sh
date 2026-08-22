#!/usr/bin/env bash
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/hsweep2.tsv
printf "arm\ttag\tk\tfirst\tother\tother_started\twin\tv0_ticks\tv1_ticks\tcsd\n" > "$out"
run () {
  arm=$1; tag=$2; k=$3; shift 3
  python3 hermit_run.py "$tag" --k "$k" --settle 25 --timeout 300 "$@" > "$D/hlog-$tag.txt" 2>&1
  l=$(grep -a "^ADVANCE R" "$D/h-$tag.capture" 2>/dev/null | head -1)
  w=$(grep -a "^HOSTCPU" "$D/hlog-$tag.txt" | head -1)
  csd=$(tr -d '\r' < "$D/h-$tag.capture" 2>/dev/null | grep -ac "non-responsive CSD" || echo 0)
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$arm" "$tag" "$k" \
    "$(echo "$l"|sed -n 's/ADVANCE R\([01]\).*/\1/p')" \
    "$(echo "$l"|sed -n 's/.*other=\([0-9]*\).*/\1/p')" \
    "$(echo "$l"|sed -n 's/.*other_started=\([0-9]*\).*/\1/p')" \
    "$(echo "$w"|sed -n 's/.*window=\([0-9.]*\)s.*/\1/p')" \
    "$(echo "$w"|sed -n 's/.*v0_ticks=\([0-9-]*\).*/\1/p')" \
    "$(echo "$w"|sed -n 's/.*v1_ticks=\([0-9-]*\).*/\1/p')" "$csd" >> "$out"
}
for r in 5 6 7 8 9 10; do run "strict" "b$r" 30000000; done
for s in 1 2 3; do run "random-s$s" "r$s" 30000000 \
   --hermit-extra "--sched-heuristic Random --sched-seed $s"; done
for s in 1 2 3; do run "sticky09-s$s" "k$s" 30000000 \
   --hermit-extra "--sched-heuristic StickyRandom --sched-sticky-random-param 0.9 --sched-seed $s"; done
echo HSWEEP2DONE >> "$out"
