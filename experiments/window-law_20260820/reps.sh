#!/bin/bash
D=/home/newton/work/dev-hermit/experiments/window-law_20260820
cd "$D"
for r in b c; do
  rm -f "$D/disk-w0$r.qcow2"
  ./tickprobe.py resume --tag "w0$r" --command "order; racer_p0; echo FIN\$((20+3))" \
     --expect FIN23 --hermit --hermit-extra "--max-timeslice 2000000" --timeout 1500 >/dev/null 2>&1
  echo "== w0$r: $(grep -o 'switches=[0-9]*' r-w0$r.capture 2>/dev/null|head -1) $(grep -o 'lost=[0-9]*' r-w0$r.capture 2>/dev/null|head -1)$(grep -q 'write\" lock' r-w0$r.err 2>/dev/null && echo ' LOCK-VOID')"
done
echo REPSDONE
