#!/usr/bin/env bash
# One arm: census, run, census, gate. Switches unlogged; turns from a separate logged run.
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"
NOVIRT="--no-virtualize-time --no-virtualize-metadata"
K=${K:-1000000}
out=$D/arms.tsv
[ -f "$out" ] || printf "arm\tplacement\tts_ns\tk\tadmit\twall\tinv0\tinv1\tinv_total\tguest_warn\n" > "$out"
arm () {
  tag=$1; ts=$2; pl=$3
  ex="$NOVIRT"; [ "$ts" != "default" ] && ex="$NOVIRT --max-timeslice $ts"
  ./admit.py capture "$D/adm-$tag-before.json" >/dev/null 2>&1
  o=$(python3 hermit_run.py "$tag" --preempt --no-strict --placement "$pl" --k $K \
        --settle 25 --timeout 600 --hermit-extra "$ex" 2>&1)
  ./admit.py capture "$D/adm-$tag-after.json" >/dev/null 2>&1
  ad=$(./admit.py compare "$D/adm-$tag-before.json" "$D/adm-$tag-after.json" >"$D/adm-$tag.txt" 2>&1 \
         && echo ADMIT || echo REJECT)
  w=$(echo "$o" | sed -n 's/PREEMPT-RUN wall=\([0-9.]*\)s.*/\1/p')
  i0=$(echo "$o" | sed -n 's/.*PREEMPT cpu=0 .*involuntary=\([0-9]*\).*/\1/p')
  i1=$(echo "$o" | sed -n 's/.*PREEMPT cpu=1 .*involuntary=\([0-9]*\).*/\1/p')
  gw=$(tr -d '\r' < "$D/h-$tag.capture" 2>/dev/null | grep -acE "soft lockup|csd:|rcu: INFO|workqueue lockup" || echo 0)
  tot=""; [ -n "$i0" ] && [ -n "$i1" ] && tot=$((i0+i1))
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$tag" "$pl" "$ts" "$K" "$ad" "${w:-}" \
     "${i0:-}" "${i1:-}" "${tot:-}" "$gw" >> "$out"
  rm -f "$D/h-$tag.qcow2"
}
for spec in "$@"; do
  IFS=: read -r tg ts pl <<<"$spec"; arm "$tg" "$ts" "$pl"
done
