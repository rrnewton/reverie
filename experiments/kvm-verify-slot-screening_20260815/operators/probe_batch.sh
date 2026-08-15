#!/bin/bash
cd /home/newton/work/dev-hermit/worktrees/slot04/hermit
R=/home/newton/work/dev-hermit/ignored/kvm2-probe
: > $R/batch.jsonl
for id in $(python3 -c "
import json
for r in json.load(open('$R/../kvm2-screen/probe_sample.json')): print(r['id'])
"); do
  timeout 600 ./ci/test_harness.sh run --probe-disabled --test "$id" --mode verify --backend kvm \
      --results $R/one.jsonl > $R/last.log 2>&1
  if [[ -s $R/one.jsonl ]]; then cat $R/one.jsonl >> $R/batch.jsonl
  else echo "{\"test\":\"$id\",\"outcome\":\"NO-ROW\",\"note\":\"harness produced no results row\"}" >> $R/batch.jsonl; fi
  echo "done $id" >&2
done
