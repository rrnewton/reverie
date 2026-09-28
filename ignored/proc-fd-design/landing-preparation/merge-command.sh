#!/usr/bin/env bash
# Prepared only. Run after root releases the exact comments, current source and merge.
set -u
cd /home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917 || exit 1
landing_output=/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/proc-fd-design/landing-preparation/merge-1
if [ -e "$landing_output" ]; then
    printf '%s\n' 'Refusing to overwrite an existing merge attempt.' >&2
    exit 1
fi
mkdir "$landing_output" || exit 1
with-proxy /home/newton/work/dev-hermit/ci-hub/bin/gh-merge-verified \
    565 --repo rrnewton/reverie \
    --run-current-main-copy \
    --expect-head 696f0476aa46cf29e31b947a89379d80b4542ce3 \
    --receipt "$landing_output/receipt.json" \
    -- --rebase > "$landing_output/stdout" 2> "$landing_output/stderr"
merge_status=$?
printf '%s\n' "$merge_status" > "$landing_output/exit-code.txt"
cat "$landing_output/stdout"
cat "$landing_output/stderr" >&2
exit "$merge_status"
