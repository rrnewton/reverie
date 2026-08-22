#!/usr/bin/env bash
# Turns granted to the two vCPU dettids per timeslice, bracketed to the workload window.
# Separate runs from the switch arms: reading turns needs --log info, which costs >=4x.
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
cd "$D"; out=$D/turnarms.tsv
[ -f "$out" ] || printf "tag\tts_ns\tplacement\twin_s\tt11_in\tt13_in\tvcpu_turns_per_s\tt3_in\tt11_all\tt13_all\n" > "$out"
NOVIRT="--no-virtualize-time --no-virtualize-metadata"
K=${K:-1000000}
for spec in "$@"; do
  IFS=: read -r tag ts pl <<<"$spec"
  ex="$NOVIRT"; [ "$ts" != "default" ] && ex="$NOVIRT --max-timeslice $ts"
  lg="$D/tt-$tag.log"; rm -f "$lg"
  o=$(python3 hermit_run.py "$tag" --preempt --no-strict --placement "$pl" --k $K \
        --settle 25 --timeout 900 --hermit-log info --hermit-log-file "$lg" \
        --hermit-extra "$ex" 2>&1)
  st=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*start=\([^ ]*\) .*/\1/p')
  en=$(echo "$o" | grep -a WINDOW_UTC | sed -n 's/.*end=\(.*\)/\1/p')
  w=""; i11=""; i13=""; i3=""; a11=""; a13=""
  if [ -n "$st" ]; then
    t=$(python3 turns.py "$lg" "$st" "$en")
    w=$(echo "$t" | sed -n 's/window=\([0-9.]*\)s.*/\1/p')
    i3=$(echo "$t" | sed -n "s/.*in_window={[^}]*3: \([0-9]*\).*/\1/p")
    i11=$(echo "$t" | sed -n "s/.*in_window={[^}]*11: \([0-9]*\).*/\1/p")
    i13=$(echo "$t" | sed -n "s/.*in_window={[^}]*13: \([0-9]*\).*/\1/p")
    a11=$(echo "$t" | sed -n "s/.*whole_run={[^}]*11: \([0-9]*\).*/\1/p")
    a13=$(echo "$t" | sed -n "s/.*whole_run={[^}]*13: \([0-9]*\).*/\1/p")
  fi
  rate=""
  [ -n "$w" ] && [ -n "$i11" ] && rate=$(python3 -c "print(f'{($i11+$i13)/$w:.2f}')" 2>/dev/null)
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$tag" "$ts" "$pl" "${w:-}" "${i11:-}" "${i13:-}" \
     "${rate:-}" "${i3:-}" "${a11:-}" "${a13:-}" >> "$out"
  rm -f "$lg" "$D/h-$tag.qcow2"
done
