#!/bin/bash
# Runs every scenario N times; records loadavg before and after each.
cd /home/newton/work/dev-hermit/ignored/liteinst-lane-claude/exit688/kernel
N=${N:-250}
echo "start $(date -Is) loadavg=$(cat /proc/loadavg)" > campaign/meta.txt
for s in S1p S1f S2d S2h S3a S3b S3c S3x S3r S3p S4a S4b S4c S4d S4e; do
  echo "$s pre $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign/meta.txt
  timeout 900 nice -n 10 ./ptx $s $N > campaign/$s.out 2> campaign/$s.err; rc=$?
  echo "$s post rc=$rc $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign/meta.txt
done
echo "end $(date -Is) loadavg=$(cat /proc/loadavg)" >> campaign/meta.txt
