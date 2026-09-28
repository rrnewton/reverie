#!/bin/bash
# usage: campaign.sh <outdir> <maxruns> <want_failures> <filter>
D=/home/newton/work/dev-hermit/ignored/liteinst-lane-claude/exit688/empirical
OUT=$1; MAX=$2; WANT=$3; FILTER=$4
mkdir -p $OUT
echo "campaign start $(date -Is) loadavg=$(cat /proc/loadavg) filter=$FILTER" >> $OUT/summary.txt
fails=0
cd $D/src/reverie-ptrace
for i in $(seq 1 $MAX); do
  la=$(cat /proc/loadavg)
  start=$(date +%s.%N)
  nice -n 10 timeout 300 $D/bin/rp-probe1 --test-threads=4 --show-output $FILTER > $OUT/run-$i.log 2>&1; rc=$?
  end=$(date +%s.%N)
  n688=$(grep -c "has no retained preceding EXIT event status" $OUT/run-$i.log)
  nes=$(grep -c "PROBE_EXITSTOP_GETEVENT_ESRCH=1" $OUT/run-$i.log)
  nfail=$(grep -E "^test .* FAILED$" $OUT/run-$i.log | wc -l)
  echo "run=$i rc=$rc wall=$(echo "$end - $start" | bc) failed_tests=$nfail n688_lines=$n688 esrch_lines=$nes loadavg=$la" >> $OUT/summary.txt
  if [ "$rc" -ne 0 ] && [ "$n688" -gt 0 ]; then fails=$((fails+1)); fi
  if [ "$rc" -eq 0 ] && [ "$nes" -eq 0 ]; then gzip -q $OUT/run-$i.log; fi
  # orphan check: test binaries of ours reparented to init
  for p in $(pgrep -u $(id -u) -f "$D/bin/rp-probe1" ); do :; done
  if [ "$fails" -ge "$WANT" ]; then break; fi
done
echo "campaign end $(date -Is) runs=$i n688_failing_runs=$fails loadavg=$(cat /proc/loadavg)" >> $OUT/summary.txt
