#!/usr/bin/env bash
# Within-RUN comparison: vCPU scheduler-turn rate during the workload window vs
# during the rest of the same resume (which is idle settle).
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/rate.tsv
printf "tag\tloaded_cpu\ttotal_s\twin_s\tt11_in\tt13_in\tt11_all\tt13_all\n" > "$out"
for spec in 0:a 0:b 1:a 1:b; do
  c=${spec%%:*}; r=${spec##*:}; tag="rt$c$r"
  lg="$D/tw-$tag.log"; rm -f "$lg"
  t0=$(date +%s.%N)
  o=$(python3 hermit_run.py "$tag" --orient $c --k 500000 --settle 25 --timeout 600 \
        --hermit-log info --hermit-log-file "$lg" 2>&1)
  t1=$(date +%s.%N)
  st=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*start=\([^ ]*\) .*/\1/p')
  en=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*end=\(.*\)/\1/p')
  if [ -n "$st" ]; then
    t=$(python3 turns.py "$lg" "$st" "$en")
    w=$(echo "$t" | sed -n 's/window=\([0-9.]*\)s.*/\1/p')
    i11=$(echo "$t" | sed -n "s/.*in_window={[^}]*11: \([0-9]*\).*/\1/p")
    i13=$(echo "$t" | sed -n "s/.*in_window={[^}]*13: \([0-9]*\).*/\1/p")
    a11=$(echo "$t" | sed -n "s/.*whole_run={[^}]*11: \([0-9]*\).*/\1/p")
    a13=$(echo "$t" | sed -n "s/.*whole_run={[^}]*13: \([0-9]*\).*/\1/p")
  fi
  printf "%s\t%s\t%.2f\t%s\t%s\t%s\t%s\t%s\n" "$tag" "$c" \
    "$(echo "$t1-$t0"|bc)" "${w:-}" "${i11:-}" "${i13:-}" "${a11:-}" "${a13:-}" >> "$out"
  rm -f "$lg" "$D/h-$tag.qcow2"
done
echo RATEDONE >> "$out"
