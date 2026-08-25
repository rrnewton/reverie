#!/bin/bash
# Compare every run in a fixed-path keep dir against run 1, two ways:
#   (1) hermit log-diff -- the project's first-divergence tool. Compares DETLOG
#       and scheduler COMMIT records only, and by design excludes internal-IO
#       poll COMMITs and scheduler-committed-time detlogs.
#   (2) a RAW byte compare with only the wall-clock timestamp prefix removed,
#       which catches everything (1) is deliberately blind to. Where the run was
#       bounded with --stop-after-turn, hermit's own early-exit summary is cut
#       off first: that block dumps an unordered futex map and raw host pointers
#       from hermit's own address space and is not guest state.
# Also reports serial and snapshot digests, so the upstream log verdict and the
# downstream artifact verdict sit side by side.
set -u
H=/home/newton/work/dev-hermit/hermit/target/release/hermit
KEEP=$1
cd "$KEEP" || exit 1
for f in r*.log; do
  b=${f%.log}
  sed -E 's/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z //' "$f" \
    | awk '/Early exit during turn/{exit} {print}' > "$b.body"
done
printf '%-5s %-14s %-14s %-14s %-9s %s\n' run log-body serial snapshot rc logdiff-vs-r1
first=$(ls r*.body | head -1); first=${first%.body}
for f in r*.body; do
  b=${f%.body}
  if [ "$b" = "$first" ]; then verdict="(reference)"; else
    verdict=$($H log-diff "$first.log" "$b.log" --no-color 2>&1 | tail -1)
  fi
  printf '%-5s %-14s %-14s %-14s %-9s %s\n' "$b" \
    "$(sha256sum < "$f" | cut -c1-12)" \
    "$(sha256sum < "$b.serial" 2>/dev/null | cut -c1-12)" \
    "$(sha256sum < "$b.qcow2"  2>/dev/null | cut -c1-12)" \
    "$(cat "$b.rc" 2>/dev/null)" \
    "$verdict"
done
echo
echo "distinct log bodies : $(sha256sum r*.body   2>/dev/null | awk '{print $1}' | sort -u | wc -l) of $(ls r*.body | wc -l)"
echo "distinct serial     : $(sha256sum r*.serial 2>/dev/null | awk '{print $1}' | sort -u | wc -l) of $(ls r*.serial 2>/dev/null | wc -l)"
echo "distinct snapshots  : $(sha256sum r*.qcow2  2>/dev/null | awk '{print $1}' | sort -u | wc -l) of $(ls r*.qcow2 2>/dev/null | wc -l)"
