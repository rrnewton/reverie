#!/bin/bash
cd /home/newton/work/dev-hermit/experiments/window-law_20260820
# Timeslice HELD at 2 ms in every run, so the switch count is constant and the
# ONLY variable is the race window. order reports switches; racer_pN reports lost.
for p in 0 5 15; do
  ./tickprobe.py resume --tag w$p --command "order; racer_p$p; echo FIN\$((20+3))" \
     --expect FIN23 --hermit --hermit-extra "--max-timeslice 2000000" --timeout 1800 >/dev/null 2>&1
  echo "== w$p done: $(grep -o 'switches=[0-9]*' r-w$p.capture 2>/dev/null|head -1) $(grep -o 'lost=[0-9]*' r-w$p.capture 2>/dev/null|head -1)"
done
echo WINDONE
