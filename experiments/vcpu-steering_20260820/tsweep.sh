#!/usr/bin/env bash
# Turns as the independent variable. Logs are ~400 MB each, so each one is parsed
# and deleted immediately rather than accumulated.
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/tsweep.tsv
printf "arm\ttag\tmode\twall\tfirst\tother\tt3\tt11\tt13\ttotal\n" > "$out"
K=500000
parse () { grep -ao 'COMMIT turn [0-9_]*, dettid [0-9]*' "$1" | awk '{print $NF}' | sort -n | uniq -c \
             | awk -v d="$2" '$2==d{print $1}' | head -1; }
run () {
  arm=$1; tag=$2; mode=$3; shift 3
  lg="$D/tl-$tag.log"; rm -f "$lg"
  python3 hermit_run.py "$tag" --k $K --settle 25 --timeout 600 \
      --hermit-log info --hermit-log-file "$lg" "$@" > "$D/tlog-$tag.txt" 2>&1
  w=$(grep -a "wall=" "$D/tlog-$tag.txt" | head -1 | sed -n 's/.*wall=\([0-9.]*\)s.*/\1/p')
  l=$(grep -a "^ADVANCE R" "$D/h-$tag.capture" 2>/dev/null | head -1)
  t3=$(parse "$lg" 3); t11=$(parse "$lg" 11); t13=$(parse "$lg" 13)
  tot=$(grep -ac 'COMMIT turn' "$lg")
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$arm" "$tag" "$mode" "${w:-}" \
    "$(echo "$l"|sed -n 's/ADVANCE R\([01]\).*/\1/p')" \
    "$(echo "$l"|sed -n 's/.*other=\([0-9]*\).*/\1/p')" \
    "${t3:-0}" "${t11:-0}" "${t13:-0}" "$tot" >> "$out"
  rm -f "$lg" "$D/h-$tag.qcow2"
}
# orientation replicates: only ONE guest CPU has any work
for r in 2 3; do run "orient0" "o0-$r" "only-cpu0" --orient 0; done
for r in 2 3; do run "orient1" "o1-$r" "only-cpu1" --orient 1; done
# the acceptance table itself, both guest CPUs loaded
for r in 1 2 3 4; do run "strict" "t-b$r" "both"; done
for s in 1 2 3; do run "random-s$s" "t-r$s" "both" \
   --hermit-extra "--sched-heuristic Random --sched-seed $s"; done
echo TSWEEPDONE >> "$out"
