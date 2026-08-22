#!/usr/bin/env bash
# One arm of the faithfulness test: census, run, census, gate, then virtual time
# granted per vCPU dettid inside the workload window against guest advancement.
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/vt.tsv
BASE="--no-virtualize-time --no-virtualize-metadata --max-timeslice 2000000"
K=${K:-1000000}
[ -f "$out" ] || printf "tag\tp\tseed\tadmit\twin_s\tg11\tg13\tvt_share0\tt11\tt13\tfirst\tother\tguest_share0\twarn\n" > "$out"
for spec in "$@"; do
  IFS=: read -r tag p seed <<<"$spec"
  ex="$BASE"
  [ "$p" != "none" ] && ex="$BASE --sched-heuristic StickyRandom --sched-sticky-random-param $p --sched-seed $seed"
  lg="$D/vt-$tag.log"; rm -f "$lg"
  ./admit.py capture "$D/b-$tag-b.json" >/dev/null 2>&1
  python3 hermit_run.py "$tag" --advance --no-strict --k "$K" --settle 25 --timeout 700 \
     --hermit-log info --hermit-log-file "$lg" --hermit-extra "$ex" >"$D/vo-$tag.txt" 2>&1 &
  rp=$!
  while kill -0 $rp 2>/dev/null; do
    if [ -f "$lg" ] && [ "$(stat -c%s "$lg" 2>/dev/null || echo 0)" -gt 4000000000 ]; then
      echo "SIZE-CAP: killed arm $tag" >> "$D/vo-$tag.txt"; kill -9 $rp 2>/dev/null; break
    fi
    sleep 2
  done
  wait $rp 2>/dev/null
  ./admit.py capture "$D/b-$tag-a.json" >/dev/null 2>&1
  ad=$(./admit.py compare "$D/b-$tag-b.json" "$D/b-$tag-a.json" >"$D/b-$tag.txt" 2>&1 && echo ADMIT || echo REJECT)
  o=$(cat "$D/vo-$tag.txt")
  st=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*start=\([^ ]*\) .*/\1/p')
  en=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*end=\(.*\)/\1/p')
  l=$(echo "$o" | grep -a "^ADVANCE R" | head -1)
  f=$(echo "$l" | sed -n 's/ADVANCE R\([01]\).*/\1/p')
  ot=$(echo "$l" | sed -n 's/.*other=\([0-9]*\).*/\1/p')
  ws=""; g11=""; g13=""; vs=""; t11=""; t13=""; gs=""
  if [ -n "$st" ]; then
    v=$(python3 vtime.py "$lg" "$st" "$en")
    g11=$(echo "$v" | sed -n "s/.*granted_s={[^}]*11: \([0-9.]*\).*/\1/p")
    g13=$(echo "$v" | sed -n "s/.*granted_s={[^}]*13: \([0-9.]*\).*/\1/p")
    t11=$(echo "$v" | sed -n "s/.*turns={[^}]*11: \([0-9]*\).*/\1/p")
    t13=$(echo "$v" | sed -n "s/.*turns={[^}]*13: \([0-9]*\).*/\1/p")
    ws=$(python3 -c "
from datetime import datetime
print('%.2f'%(datetime.fromisoformat('$en')-datetime.fromisoformat('$st')).total_seconds())")
    [ -n "$g11" ] && [ -n "$g13" ] && vs=$(python3 -c "print(f'{$g11/($g11+$g13)*100:.1f}')" 2>/dev/null)
  fi
  if [ -n "$f" ] && [ -n "$ot" ]; then
    gs=$(python3 -c "
K=$K; f=$f; o=$ot
c0,c1 = (K,o) if f==0 else (o,K)
print(f'{c0/(c0+c1)*100:.1f}')")
  fi
  wn=$(tr -d '\r' < "$D/h-$tag.capture" 2>/dev/null | grep -acE "soft lockup|csd:|rcu: INFO|workqueue lockup" || echo 0)
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
    "$tag" "$p" "$seed" "$ad" "${ws:-}" "${g11:-}" "${g13:-}" "${vs:-}" "${t11:-}" "${t13:-}" \
    "${f:-}" "${ot:-}" "${gs:-}" "$wn" >> "$out"
  rm -f "$lg" "$D/h-$tag.qcow2"
done
