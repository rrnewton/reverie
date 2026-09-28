#!/bin/bash
set -u

review_dir=/home/newton/work/dev-hermit/worktrees/slots/kvm-sendfile-lseek-lowword-20260923/ignored/recovery/sendfile-lseek-lowword-20260923
printf '%s\n' "$$" > "$review_dir/claude-exact-review-live.pid"
/usr/local/bin/claude \
  --model opus \
  --effort max \
  --permission-mode dontAsk \
  --allowedTools "Read,Grep,Glob,Bash" \
  --disallowedTools "Edit,Write,NotebookEdit,WebFetch,WebSearch" \
  --max-budget-usd 20 \
  --no-session-persistence \
  --output-format json \
  -p \
  < "$review_dir/claude-exact-review-prompt.md" \
  > "$review_dir/claude-exact-review-result.json" \
  2> "$review_dir/claude-exact-review.stderr"
review_rc=$?
printf '%s\n' "$review_rc" > "$review_dir/claude-exact-review.rc"
exit "$review_rc"
