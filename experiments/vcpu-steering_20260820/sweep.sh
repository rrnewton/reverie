#!/usr/bin/env bash
# Replicates of the four pre-registered arms, then the graded lever.
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"
out=$D/sweep.tsv
printf "arm\trep\tfirst\tme\tother\tother_started\twall\n" > "$out"
run () {
  arm=$1; rep=$2; tag="s-$arm-$rep"
  t0=$(date +%s.%N)
  ./run.sh "$tag" "$arm" 200 3000000 >"$D/log-$tag.txt" 2>&1
  t1=$(date +%s.%N)
  line=$(grep -a "^ADVANCE R" "$D/ser-$tag.log" | head -1)
  if [ -z "$line" ]; then printf "%s\t%s\tNONE\t\t\t\t%.1f\n" "$arm" "$rep" "$(echo "$t1-$t0"|bc)" >>"$out"; return; fi
  f=$(echo "$line" | sed 's/ADVANCE R\([01]\).*/\1/')
  me=$(echo "$line" | sed 's/.*me=\([0-9]*\).*/\1/')
  ot=$(echo "$line" | sed 's/.*other=\([0-9]*\).*/\1/')
  os=$(echo "$line" | sed 's/.*other_started=\([0-9]*\).*/\1/')
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%.1f\n" "$arm" "$rep" "$f" "$me" "$ot" "$os" "$(echo "$t1-$t0"|bc)" >>"$out"
}
for rep in 1 2 3; do for arm in A B C D; do run "$arm" "$rep"; done; done
for rep in 1 2; do for arm in N1 N3 N5 N10 N19; do run "$arm" "$rep"; done; done
echo SWEEPDONE >> "$out"
