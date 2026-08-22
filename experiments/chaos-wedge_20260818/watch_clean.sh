#!/usr/bin/env bash
# Block until the clean 2-vCPU chaos run exits, then report.
while pgrep -f "run.sh clean-v2-s1 " >/dev/null 2>&1; do sleep 30; done
grep rc= clean-v2-s1.txt 2>/dev/null
tail -3 logs/clean-v2-s1.progress
