#!/usr/bin/env bash
# One arm of the handoff workload: census, run, census, gate. Optionally with
# --log info so hermit's scheduler turns can be counted in the same window.
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/pp.tsv
NOVIRT="--no-virtualize-time --no-virtualize-metadata"
[ -f "$out" ] || printf "tag\tmode\tplacement\tts\tn\tadmit\twall\twin_s\tinv\tvol\tt11\tt13\tturns_per_s\tguest_warn\n" > "$out"
arm () {
  tag=$1; mode=$2; pl=$3; ts=$4; n=$5
  ex="$NOVIRT"; [ "$ts" != "default" ] && ex="$NOVIRT --max-timeslice $ts"
  lg=""; logargs=()
  if [ "$mode" = "turns" ]; then lg="$D/pp-$tag.log"; rm -f "$lg"
     logargs=(--hermit-log info --hermit-log-file "$lg"); fi
  ./admit.py capture "$D/a-$tag-b.json" >/dev/null 2>&1
  # --log info writes at a measured 25.5 GiB/h. The arm deletes its log on
  # completion, so a run that FINISHES is bounded by its own duration -- but a run
  # that hangs is not, and the 900s timeout would allow ~6.4 GiB. Cap it.
  python3 hermit_run.py "$tag" --pingpong --no-strict --placement "$pl" --k "$n" \
        --settle 25 --timeout 900 --hermit-extra "$ex" "${logargs[@]}" >"$D/o-$tag.txt" 2>&1 &
  runpid=$!
  while kill -0 $runpid 2>/dev/null; do
    if [ -n "$lg" ] && [ -f "$lg" ]; then
      sz=$(stat -c%s "$lg" 2>/dev/null || echo 0)
      if [ "$sz" -gt 4000000000 ]; then
        echo "SIZE-CAP: $lg exceeded 4 GB, killing arm $tag" >> "$D/o-$tag.txt"
        kill -9 $runpid 2>/dev/null; break
      fi
    fi
    sleep 2
  done
  wait $runpid 2>/dev/null
  o=$(cat "$D/o-$tag.txt")
  ./admit.py capture "$D/a-$tag-a.json" >/dev/null 2>&1
  ad=$(./admit.py compare "$D/a-$tag-b.json" "$D/a-$tag-a.json" >"$D/a-$tag.txt" 2>&1 && echo ADMIT || echo REJECT)
  w=$(echo "$o" | sed -n 's/PP-RUN wall=\([0-9.]*\)s.*/\1/p')
  inv=$(echo "$o" | grep -a "^PP side=" | sed -n 's/.*involuntary=\([0-9]*\).*/\1/p' | paste -sd+ | bc)
  vol=$(echo "$o" | grep -a "^PP side=" | sed -n 's/.*voluntary=\([0-9]*\).*/\1/p' | paste -sd+ | bc)
  st=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*start=\([^ ]*\) .*/\1/p')
  en=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*end=\(.*\)/\1/p')
  ws=""; t11=""; t13=""; rate=""
  if [ -n "$st" ]; then
    ws=$(python3 -c "
from datetime import datetime
print('%.2f' % (datetime.fromisoformat('$en')-datetime.fromisoformat('$st')).total_seconds())")
    if [ -n "$lg" ]; then
      t=$(python3 turns.py "$lg" "$st" "$en")
      t11=$(echo "$t" | sed -n "s/.*in_window={[^}]*11: \([0-9]*\).*/\1/p")
      t13=$(echo "$t" | sed -n "s/.*in_window={[^}]*13: \([0-9]*\).*/\1/p")
      [ -n "$t11" ] && rate=$(python3 -c "print(f'{($t11+$t13)/$ws:.2f}')" 2>/dev/null)
    fi
  fi
  gw=$(tr -d '\r' < "$D/h-$tag.capture" 2>/dev/null | grep -acE "soft lockup|csd:|rcu: INFO|workqueue lockup" || echo 0)
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
    "$tag" "$mode" "$pl" "$ts" "$n" "$ad" "${w:-}" "${ws:-}" "${inv:-}" "${vol:-}" \
    "${t11:-}" "${t13:-}" "${rate:-}" "$gw" >> "$out"
  rm -f "$lg" "$D/h-$tag.qcow2"
}
for spec in "$@"; do IFS=: read -r a b c d e <<<"$spec"; arm "$a" "$b" "$c" "$d" "$e"; done
