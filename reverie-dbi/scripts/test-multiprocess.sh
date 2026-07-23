#!/usr/bin/env bash
# Multi-process (fork/exec) coordination smoke test.
#
# Runs a genuinely-forking shell pipeline under the DynamoRIO client with the
# cross-process coordinator enabled, then shows the per-process records the
# followed children dropped into the shared coordinator directory. Under the
# default -follow_children, DynamoRIO injects the client into every child, and
# each child writes its own `proc-<pid>` record (pid, ppid, syscalls, comm).
#
# The offline aggregation (deterministic process-tree reconstruction) is covered
# by `cargo test -p reverie-dbi` (coordinator unit tests) and, end-to-end, by
# `tests/multiprocess_coordinator.rs`.
set -euo pipefail

if [[ -z "${DYNAMORIO_HOME:-}" ]]; then
  echo "DYNAMORIO_HOME must point to a built DynamoRIO source tree" >&2
  exit 2
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# The client MUST be release-built: debug frames overflow DynamoRIO's ~56K
# client stack and crash even the baseline. Default to release here.
export PROFILE="${PROFILE:-release}"
client=$("$script_dir/build-client.sh" | tail -n 1)
if [[ -x "$DYNAMORIO_HOME/build/bin64/drrun" ]]; then
  drrun="$DYNAMORIO_HOME/build/bin64/drrun"
else
  drrun="$DYNAMORIO_HOME/install/bin64/drrun"
fi

coord=$(mktemp -d)
trap 'rm -rf "$coord"' EXIT

# `echo hi | cat` forks twice and execs /bin/echo and /usr/bin/cat, so the tree
# is bash -> {echo, cat}: three followed, coordinated processes.
REVERIE_DBI_COORD_DIR="$coord" \
  "$drrun" -disable_rseq -follow_children -c "$client" -- \
  /bin/bash -c '/bin/echo hi | cat'

echo "--- coordinator records in $coord ---"
records=$(find "$coord" -name 'proc-*' | wc -l)
for f in "$coord"/proc-*; do
  printf 'record: %s\n' "$(cat "$f")"
done

# bash + echo + cat.
[[ "$records" -ge 2 ]] || {
  echo "expected >= 2 coordinated processes, saw $records" >&2
  exit 1
}
echo "OK: $records processes coordinated through the REVERIE_DBI_COORD_DIR channel"
