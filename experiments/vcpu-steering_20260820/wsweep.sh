#!/usr/bin/env bash
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/wsweep.tsv
printf "arm\ttag\twin_s\tt11_in\tt13_in\tt11_all\tt13_all\tfirst\tother\n" > "$out"
run () {
  arm=$1; tag=$2; shift 2
  lg="$D/tw-$tag.log"; rm -f "$lg"
  o=$(python3 hermit_run.py "$tag" --k 500000 --settle 25 --timeout 600 \
        --hermit-log info --hermit-log-file "$lg" "$@" 2>&1)
  st=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*start=\([^ ]*\) .*/\1/p')
  en=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*end=\(.*\)/\1/p')
  l=$(grep -a "^ADVANCE R" "$D/h-$tag.capture" 2>/dev/null | head -1)
  if [ -n "$st" ]; then
    t=$(python3 turns.py "$lg" "$st" "$en")
    w=$(echo "$t" | sed -n 's/window=\([0-9.]*\)s.*/\1/p')
    i11=$(echo "$t" | sed -n "s/.*in_window={[^}]*11: \([0-9]*\).*/\1/p")
    i13=$(echo "$t" | sed -n "s/.*in_window={[^}]*13: \([0-9]*\).*/\1/p")
    a11=$(echo "$t" | sed -n "s/.*whole_run={[^}]*11: \([0-9]*\).*/\1/p")
    a13=$(echo "$t" | sed -n "s/.*whole_run={[^}]*13: \([0-9]*\).*/\1/p")
  fi
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$arm" "$tag" "${w:-}" "${i11:-}" "${i13:-}" \
    "${a11:-}" "${a13:-}" \
    "$(echo "$l"|sed -n 's/ADVANCE R\([01]\).*/\1/p')" \
    "$(echo "$l"|sed -n 's/.*other=\([0-9]*\).*/\1/p')" >> "$out"
  rm -f "$lg" "$D/h-$tag.qcow2"
}
for r in 2 3; do run "only-cpu0" "w0-$r" --orient 0; done
for r in 2 3; do run "only-cpu1" "w1-$r" --orient 1; done
for r in 1 2 3; do run "both-strict" "wb$r"; done
for s in 1 2; do run "both-random-s$s" "wr$s" --hermit-extra "--sched-heuristic Random --sched-seed $s"; done
echo WSWEEPDONE >> "$out"
