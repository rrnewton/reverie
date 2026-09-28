#!/bin/bash
# usage: campaign.sh LABEL BIN CAP NEED_SIGTRAP FILTER [taskset-cpus]
set -u
LABEL=$1; BIN=$2; CAP=$3; NEED=$4; FILTER=$5; CPUS=${6:-}
D=/home/newton/work/dev-hermit/ignored/liteinst-lane-claude/exit688/sigtrap
OUT=$D/runs/$LABEL; mkdir -p $OUT
cd $D/src/reverie-ptrace
echo "start $(date -Is) load=$(cat /proc/loadavg)" > $OUT/summary.txt
traps=0
for i in $(seq 1 $CAP); do
  pre="nice -n 10"
  [ -n "$CPUS" ] && pre="nice -n 10 taskset -c $CPUS"
  timeout 180 $pre $BIN --test-threads=4 --nocapture $FILTER > $OUT/run.log 2>&1; rc=$?
  n=$(grep -c PROBE_SIGTRAP_TERMINAL $OUT/run.log)
  f=$(grep -c "^test .* FAILED$" $OUT/run.log)
  echo "run=$i rc=$rc sigtrap=$n failed_tests=$f load=$(cut -d' ' -f1-3 /proc/loadavg)" >> $OUT/summary.txt
  grep -h "PROBE_OUTCOME" $OUT/run.log | sed "s/^/run=$i /" >> $OUT/outcomes.txt
  grep -h "^test .*\(ok\|FAILED\)$" $OUT/run.log | sed "s/^/run=$i /" >> $OUT/tests.txt
  if [ "$rc" -ne 0 ] || [ "$n" -gt 0 ]; then cp $OUT/run.log $OUT/run-$i.log; fi
  traps=$((traps + n))
  # kill orphans of our binary
  pkill -KILL -f "^$BIN" 2>/dev/null
  if [ "$traps" -ge "$NEED" ]; then break; fi
done
echo "end $(date -Is) runs=$i sigtrap_total=$traps load=$(cat /proc/loadavg)" >> $OUT/summary.txt
