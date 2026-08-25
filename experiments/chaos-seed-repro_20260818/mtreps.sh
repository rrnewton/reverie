#!/usr/bin/env bash
H=/home/newton/work/dev-hermit/hermit/target/release/hermit
cd /home/newton/work/dev-hermit/experiments/chaos-seed-repro_20260818
for i in $(seq 1 5); do
  timeout 900 $H --log error run --strict --chaos --seed 1 \
    --record-preemptions-to rec/mt2-s1-r$i.json -- ./spin6 6 >/dev/null 2>rec/mt2-s1-r$i.err
done
echo "done"; sha256sum rec/mt2-s1-r*.json | awk '{print $1}' | sort | uniq -c
