#!/bin/bash
D=/home/newton/work/dev-hermit/experiments/window-law_20260820
cd "$D"
for p in 5 15; do
  rm -f "$D/disk-w$p.qcow2"          # never inherit a locked image
  ./tickprobe.py resume --tag "w$p" --command "order; racer_p$p; echo FIN\$((20+3))" \
     --expect FIN23 --hermit --hermit-extra "--max-timeslice 2000000" --timeout 1500 >/dev/null 2>&1
  echo "== w$p: $(grep -o 'switches=[0-9]*' r-w$p.capture 2>/dev/null|head -1) $(grep -o 'lost=[0-9]*' r-w$p.capture 2>/dev/null|head -1)"
  if grep -q 'Failed to get "write" lock' "r-w$p.err" 2>/dev/null; then echo "   LOCK CONTENTION -- result void"; fi
done
echo WIN2DONE
