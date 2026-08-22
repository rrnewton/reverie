#!/usr/bin/env bash
n=${1:-150}; secs=${2:-300}
for i in $(seq 1 $n); do timeout $secs bash -c 'while :; do :; done' & done
for i in $(seq 1 20); do timeout $secs bash -c 'while :; do sched_yield 2>/dev/null || true; done' & done
wait
