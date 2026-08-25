#!/usr/bin/env bash
# Report as soon as ANY 2-vCPU chaos run reaches its PASS marker, or after 100 min.
end=$((SECONDS+6000))
while [ $SECONDS -lt $end ]; do
  for l in clean-v2-s1 tt-v2-s1 tt-v2-s999; do
    if grep -q 'HERMIT-QEMU-BUSYBOX-PASS' "logs/$l.console" 2>/dev/null; then
      echo "PASS REACHED: $l  bytes=$(stat -c %s logs/$l.console)"
      tail -1 "logs/$l.progress"; exit 0
    fi
  done
  sleep 30
done
echo "no PASS within window; state:"
for l in clean-v2-s1 tt-v2-s1 tt-v2-s999; do printf '%-12s %s\n' $l "$(tail -1 logs/$l.progress)"; done
