#!/usr/bin/env bash
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"
out=$D/sweep2.tsv
printf "arm\trep\tfirst\tother\tother_started\twin_wall\tv0_ticks\tv1_ticks\n" > "$out"
run () {
  arm=$1; rep=$2; tag="q-$arm-$rep"
  ./run.sh "$tag" "$arm" 200 100000000 >"$D/log-$tag.txt" 2>&1
  line=$(grep -a "^ADVANCE R" "$D/ser-$tag.log" | head -1)
  w=$(grep -a "^HOSTCPU WINDOW" "$D/log-$tag.txt" | head -1)
  f=$(echo "$line" | sed -n 's/ADVANCE R\([01]\).*/\1/p')
  ot=$(echo "$line" | sed -n 's/.*other=\([0-9]*\).*/\1/p')
  os=$(echo "$line" | sed -n 's/.*other_started=\([0-9]*\).*/\1/p')
  ww=$(echo "$w" | sed -n 's/.*wall=\([0-9.]*\)s.*/\1/p')
  a=$(echo "$w" | sed -n 's/.*v0_ticks=\([0-9-]*\).*/\1/p')
  b=$(echo "$w" | sed -n 's/.*v1_ticks=\([0-9-]*\).*/\1/p')
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$arm" "$rep" "${f:-NONE}" "${ot:-}" "${os:-}" "${ww:-}" "${a:-}" "${b:-}" >>"$out"
}
for rep in 1 2 3; do for arm in A B C D; do run "$arm" "$rep"; done; done
for rep in 1 2; do for arm in N1 N3 N5 N10 N19; do run "$arm" "$rep"; done; done
echo SWEEP2DONE >> "$out"
