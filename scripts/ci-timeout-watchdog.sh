#!/usr/bin/env bash
# CI timeout watchdog (dependency-free shell sidecar).
#
# A bare "job timed out" from GitHub Actions is undiagnosable. This script runs
# as a background sidecar next to a long-running step. If that step is still
# running after <warn-after-seconds>, the watchdog emits a GitHub `::error::`
# annotation that names the step, the elapsed seconds, and a LIVE thread/wchan
# snapshot of the monitored process tree, then repeats every <repeat-seconds>
# until the wrapper kills it on successful step completion. The snapshot is read
# straight from /proc (status, wchan, stack) plus `ps` -- no external tools such
# as py-spy.
#
# Usage:
#   ci-timeout-watchdog.sh <step-name> [warn-after-seconds] [repeat-seconds] [root-pid]
#
# <root-pid> defaults to $PPID, i.e. the shell of the wrapping step, whose
# descendants are exactly the monitored command's process tree.
#
# The watchdog never fails the step: its own errors are swallowed and its exit
# status is ignored by the wrapper (which kills it and then reports the command
# result).
set -uo pipefail

step_name="${1:-unknown step}"
warn_after="${2:-510}"
repeat="${3:-30}"
root_pid="${4:-$PPID}"

start="$(date +%s)"

# List every pid in the subtree rooted at $1, excluding this watchdog ($$).
list_tree() {
  local pid="$1"
  [ "$pid" = "$$" ] && return 0
  [ -d "/proc/$pid" ] || return 0
  printf '%s\n' "$pid"
  local kids kid
  kids="$(ps -o pid= --ppid "$pid" 2>/dev/null || true)"
  for kid in $kids; do
    list_tree "$kid"
  done
}

# Build a human-readable snapshot of the monitored process tree.
snapshot_text() {
  local now elapsed
  now="$(date +%s)"
  elapsed=$(( now - start ))
  printf 'Watchdog: step "%s" still running after %ss (monitoring pid tree rooted at %s).\n' \
    "$step_name" "$elapsed" "$root_pid"

  local pids pid
  pids="$(list_tree "$root_pid" | sort -un)"
  if [ -z "$pids" ]; then
    printf 'No live descendant processes found under pid %s.\n' "$root_pid"
    return 0
  fi

  local pid comm state threads t tid tcomm tstate twchan
  for pid in $pids; do
    [ -d "/proc/$pid" ] || continue
    comm="$(tr -d '\000' < "/proc/$pid/comm" 2>/dev/null || echo '?')"
    state="$(awk '/^State:/ { $1=""; print }' "/proc/$pid/status" 2>/dev/null | sed 's/^ *//' || true)"
    threads="$(awk '/^Threads:/ { print $2 }' "/proc/$pid/status" 2>/dev/null || echo '?')"
    printf 'PID %s (%s) state=[%s] threads=%s\n' "$pid" "$comm" "$state" "$threads"
    for t in "/proc/$pid/task"/*; do
      [ -d "$t" ] || continue
      tid="$(basename "$t")"
      tcomm="$(tr -d '\000' < "$t/comm" 2>/dev/null || echo '?')"
      tstate="$(awk '{ print $3 }' "$t/stat" 2>/dev/null || echo '?')"
      twchan="$(cat "$t/wchan" 2>/dev/null || echo '?')"
      printf '  tid %s (%s) state=%s wchan=%s\n' "$tid" "$tcomm" "$tstate" "$twchan"
      if [ -r "$t/stack" ]; then
        sed 's/^/    stack: /' "$t/stack" 2>/dev/null || true
      fi
    done
  done

  printf -- '--- ps ---\n'
  ps -o pid,ppid,stat,wchan:32,etimes,comm -p "$(echo $pids | tr ' ' ',')" 2>/dev/null || true
}

# Escape a string for use inside a GitHub Actions workflow-command message.
gh_escape() {
  local s="$1"
  s="${s//'%'/'%25'}"
  s="${s//$'\r'/'%0D'}"
  s="${s//$'\n'/'%0A'}"
  printf '%s' "$s"
}

emit() {
  local text now elapsed
  text="$(snapshot_text 2>/dev/null || echo 'snapshot failed')"
  now="$(date +%s)"
  elapsed=$(( now - start ))
  # Full detail in the step log for humans reading the run.
  printf '::group::Timeout watchdog snapshot: %s (%ss)\n%s\n::endgroup::\n' \
    "$step_name" "$elapsed" "$text"
  # A single-line annotation that surfaces on the run summary.
  printf '::error title=Step "%s" exceeded %ss (approaching hard timeout)::%s\n' \
    "$step_name" "$elapsed" "$(gh_escape "$text")"
}

sleep "$warn_after" 2>/dev/null || exit 0
while :; do
  emit || true
  sleep "$repeat" 2>/dev/null || exit 0
done
