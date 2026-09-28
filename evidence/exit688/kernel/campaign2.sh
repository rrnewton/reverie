#!/bin/bash
cd /home/newton/work/dev-hermit/ignored/liteinst-lane-claude/exit688/kernel
N=${N:-200}
mkdir -p campaign2
echo "start $(date -Is) loadavg=$(cat /proc/loadavg)" > campaign2/meta.txt
for s in S3a S3b S3c S3x; do
  echo "$s pre $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign2/meta.txt
  timeout 900 nice -n 10 ./ptx $s $N > campaign2/$s.out 2> campaign2/$s.err; rc=$?
  echo "$s post rc=$rc $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign2/meta.txt
done
for m in sc sw ic iw; do
  echo "S6$m pre $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign2/meta.txt
  timeout 900 nice -n 10 ./ptx6 $m $N > campaign2/S6$m.out 2> campaign2/S6$m.err; rc=$?
  echo "S6$m post rc=$rc $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign2/meta.txt
done
echo "end $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign2/meta.txt
