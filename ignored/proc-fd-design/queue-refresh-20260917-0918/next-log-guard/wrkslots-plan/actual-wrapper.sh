#!/usr/bin/env bash
# Run the committed wrkslots implementation while keeping one registry beside
# the primary dev-hermit checkout. Linked worktrees contain their own copy of
# this launcher, but must not create a second registry beneath themselves.

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
checkout_root=$(cd -- "$script_dir/../.." && pwd)
python_root="$checkout_root/hermit/agent-utils/py"

if [[ ! -f "$python_root/wrkslots/__main__.py" ]]; then
  printf '%s\n' \
    "REFUSED: the committed wrkslots package is missing at $python_root." \
    "state: REFUSED -- no worktree or lifecycle record was changed." \
    "remedy: initialize the hermit/agent-utils submodule in this checkout, then rerun ci-hub/bin/wrkslots." >&2
  exit 3
fi

project_root_supplied=0
for argument in "$@"; do
  case "$argument" in
    --project-root|--project-root=*) project_root_supplied=1 ;;
  esac
done

project_args=()
if ((project_root_supplied == 0)); then
  common_dir=$(git -C "$checkout_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || {
    printf '%s\n' \
      "REFUSED: cannot locate the shared dev-hermit Git directory from $checkout_root." \
      "state: REFUSED -- no worktree or lifecycle record was changed." \
      "remedy: run this command from a dev-hermit checkout, or pass --project-root PATH explicitly." >&2
    exit 3
  }
  if [[ $(basename -- "$common_dir") != .git ]]; then
    printf '%s\n' \
      "REFUSED: Git common directory $common_dir does not identify a primary dev-hermit checkout." \
      "state: REFUSED -- no worktree or lifecycle record was changed." \
      "remedy: pass --project-root PATH naming the project whose worktrees directory holds the shared slot state." >&2
    exit 3
  fi
  project_args=(--project-root "$(dirname -- "$common_dir")")
fi

if [[ -n ${PYTHONPATH:-} ]]; then
  export PYTHONPATH="$python_root:$PYTHONPATH"
else
  export PYTHONPATH="$python_root"
fi

refuse() {
  printf '%s\n' \
    "REFUSED: $1" \
    "state: REFUSED -- no worktree or lifecycle record was changed." >&2
  exit 3
}

refuse_unknown() {
  printf '%s\n' \
    "REFUSED: $1" \
    "state: UNKNOWN -- the host command did not complete, so inspect the wrkslots journal and registry before retrying." >&2
  exit 3
}

process_start_ticks() {
  local pid="$1" raw rest
  local -a fields
  raw=$(<"/proc/$pid/stat") || return 1
  rest="${raw##*) }"
  read -r -a fields <<<"$rest"
  ((${#fields[@]} > 19)) || return 1
  [[ ${fields[19]} =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "${fields[19]}"
}

command_name=""
global_value=0
end_global_options=0
help_requested=0
for argument in "$@"; do
  case "$argument" in
    -h|--h|--he|--hel|--help) help_requested=1 ;;
  esac
done
for argument in "$@"; do
  if ((global_value)); then
    global_value=0
    continue
  fi
  if ((end_global_options)); then
    command_name="$argument"
    break
  fi
  if [[ $argument == -- ]]; then
    end_global_options=1
    continue
  fi
  if [[ $argument != --* ]]; then
    command_name="$argument"
    break
  fi

  option_name="${argument%%=*}"
  option_value_inline=0
  [[ $argument == *=* ]] && option_value_inline=1
  option_matches=()
  for known_option in \
      --version --userguide --project-root --machine --wait-lock \
      --allow-existing-unregistered-worktrees --help; do
    [[ $known_option == "$option_name"* ]] && option_matches+=("$known_option")
  done
  ((${#option_matches[@]} == 1)) || break
  case "${option_matches[0]}" in
    --version|--userguide|--help) break ;;
    --project-root|--machine|--wait-lock)
      ((option_value_inline)) || global_value=1
      ;;
  esac
done

host_removal=0
if [[ $command_name == remove || $command_name == remove-validate-batch \
      || $command_name == recover-ownerless-validate-batch \
      || $command_name == recover ]]; then
  host_removal=1
fi

# Run the WHOLE removal or recovery command in one host-visible process. A bare
# recover may resume a frozen-checkout journal, so its arguments alone cannot
# prove that no removal will occur. Moving only the registered-liveness probe
# would leave the exact-owner and path/cgroup scans in the caller's potentially
# restricted PID namespace.
if ((help_requested == 0 && host_removal == 1)); then
  process_view_probe="$checkout_root/ci-hub/health/host_process_context.py"
  [[ -f $process_view_probe ]] || refuse \
    "the process-view authority is missing at $process_view_probe; removal requires fresh host process evidence"

  if [[ ${WRKSLOTS_REQUIRE_HOST_PROCESS_VIEW:-} == 1 ]]; then
    # The marker is not authority: a caller can set an environment variable.
    # Re-entry is accepted only when /proc itself proves the host process view.
    python3 "$process_view_probe" >/dev/null || refuse \
      "the removal process does not have a verified full process view; restricted or unreadable process evidence cannot authorize deletion"
    [[ ${WRKSLOTS_ORIGINAL_COORDINATOR_PID:-} =~ ^[1-9][0-9]*$ ]] || refuse \
      "the host process handoff did not bind the original coordinator PID"
    [[ ${WRKSLOTS_ORIGINAL_COORDINATOR_START_TICKS:-} =~ ^[0-9]+$ ]] || refuse \
      "the host process handoff did not bind the original coordinator generation"
    observed_start_ticks=$(process_start_ticks "$WRKSLOTS_ORIGINAL_COORDINATOR_PID") || refuse \
      "the original coordinator process exited before host-context removal began"
    [[ $observed_start_ticks == "$WRKSLOTS_ORIGINAL_COORDINATOR_START_TICKS" ]] || refuse \
      "the original coordinator process generation changed before host-context removal began"
    export WRKSLOTS_REMOVE_RUNNER_PID="$BASHPID"
    export WRKSLOTS_REMOVE_COORDINATOR_START_TICKS="$WRKSLOTS_ORIGINAL_COORDINATOR_START_TICKS"
    export WRKSLOTS_REMOVE_PROOF_FD=0

    # The host service is not descended from the requesting coordinator. The
    # inner command receives both identities: it records and rechecks the
    # original coordinator at the locked boundary, while separately proving
    # that this service process is in its invoking ancestry.
    rewritten=()
    replace_coordinator=0
    for argument in "$@"; do
      if ((replace_coordinator)); then
        rewritten+=("$WRKSLOTS_ORIGINAL_COORDINATOR_PID")
        replace_coordinator=0
      elif [[ $argument == --coordinator-p || $argument == --coordinator-pi || $argument == --coordinator-pid ]]; then
        rewritten+=("$argument")
        replace_coordinator=1
      elif [[ $argument == --coordinator-p=* || $argument == --coordinator-pi=* || $argument == --coordinator-pid=* ]]; then
        rewritten+=("${argument%%=*}=$WRKSLOTS_ORIGINAL_COORDINATOR_PID")
      else
        rewritten+=("$argument")
      fi
    done
    exec python3 -m wrkslots "${project_args[@]}" "${rewritten[@]}"
  fi

  coordinator_pid=""
  coordinator_count=0
  expect_coordinator=0
  for argument in "$@"; do
    if ((expect_coordinator)); then
      coordinator_pid="$argument"
      coordinator_count=$((coordinator_count + 1))
      expect_coordinator=0
    elif [[ $argument == --coordinator-p || $argument == --coordinator-pi || $argument == --coordinator-pid ]]; then
      expect_coordinator=1
    elif [[ $argument == --coordinator-p=* || $argument == --coordinator-pi=* || $argument == --coordinator-pid=* ]]; then
      coordinator_pid="${argument#*=}"
      coordinator_count=$((coordinator_count + 1))
    fi
  done
  ((expect_coordinator == 0 && coordinator_count == 1)) || refuse \
    "removal requires exactly one --coordinator-pid before it can enter the host process context"
  [[ $coordinator_pid =~ ^[1-9][0-9]*$ ]] || refuse \
    "removal coordinator PID must be a positive integer"
  coordinator_start_ticks=$(process_start_ticks "$coordinator_pid") || refuse \
    "cannot read the coordinator process generation for PID $coordinator_pid"

  current_pid="$BASHPID"
  caller_is_ancestor=0
  for _ in {1..256}; do
    if [[ $current_pid == "$coordinator_pid" ]]; then
      caller_is_ancestor=1
      break
    fi
    parent_pid=""
    while read -r key value _rest; do
      if [[ $key == PPid: ]]; then
        parent_pid="$value"
        break
      fi
    done <"/proc/$current_pid/status" || refuse \
      "cannot read process ancestry for launcher PID $current_pid"
    [[ $parent_pid =~ ^[1-9][0-9]*$ && $parent_pid != "$current_pid" ]] || break
    current_pid="$parent_pid"
  done
  ((caller_is_ancestor == 1)) || refuse \
    "coordinator PID $coordinator_pid is not in the invoking process ancestry"

  # NSpid binds the namespace-local coordinator to the PID the host service
  # will see. Re-entry checks that exact process generation before continuing.
  host_coordinator_pid=""
  while read -r key first_namespace_pid _rest; do
    if [[ $key == NSpid: ]]; then
      host_coordinator_pid="$first_namespace_pid"
      break
    fi
  done <"/proc/$coordinator_pid/status" || refuse \
    "cannot read the coordinator process identity for PID $coordinator_pid"
  [[ $host_coordinator_pid =~ ^[1-9][0-9]*$ ]] || refuse \
    "cannot bind coordinator PID $coordinator_pid to the host process view"
  observed_start_ticks=$(process_start_ticks "$coordinator_pid") || refuse \
    "the coordinator process exited during host-context handoff"
  [[ $observed_start_ticks == "$coordinator_start_ticks" ]] || refuse \
    "the coordinator process generation changed during host-context handoff"

  systemd_run=$(command -v systemd-run || true)
  [[ -n $systemd_run ]] || refuse \
    "systemd-run is unavailable; removal cannot establish fresh host process evidence"

  # User-manager services do not inherit the caller's environment. Preserve it
  # explicitly for Git credentials/configuration and other existing wrkslots
  # behavior, but never accept caller-supplied handoff evidence.
  setenv_args=()
  while IFS= read -r -d '' environment_entry; do
    environment_name="${environment_entry%%=*}"
    case "$environment_name" in
      WRKSLOTS_REQUIRE_HOST_PROCESS_VIEW|WRKSLOTS_ORIGINAL_COORDINATOR_PID|WRKSLOTS_ORIGINAL_COORDINATOR_START_TICKS|WRKSLOTS_REMOVE_RUNNER_PID|WRKSLOTS_REMOVE_COORDINATOR_START_TICKS|WRKSLOTS_REMOVE_PROOF_FD)
        continue
        ;;
    esac
    case "$environment_name" in
      [A-Za-z_]*)
        [[ $environment_name != *[!A-Za-z0-9_]* ]] || continue
        ;;
      *) continue ;;
    esac
    setenv_args+=("--setenv=$environment_entry")
  done < <(env -0)

  if "$systemd_run" --user --collect --quiet --pipe --wait \
      --property=PrivateUsers=no \
      --property=PrivateMounts=no \
      --property=ProtectProc=default \
      --property=ProcSubset=all \
      "--working-directory=$PWD" \
      "${setenv_args[@]}" \
      --setenv=WRKSLOTS_REQUIRE_HOST_PROCESS_VIEW=1 \
      "--setenv=WRKSLOTS_ORIGINAL_COORDINATOR_PID=$host_coordinator_pid" \
      "--setenv=WRKSLOTS_ORIGINAL_COORDINATOR_START_TICKS=$coordinator_start_ticks" \
      -- "$script_dir/wrkslots" "$@" \
      < <(python3 "$checkout_root/ci-hub/health/removal_handoff_writer.py"); then
    exit 0
  else
    status=$?
    case "$status" in
      2|3) exit "$status" ;;
      *) refuse_unknown \
        "the host process handoff or command failed with exit $status; removal did not fall back to the caller's process view" ;;
    esac
  fi
fi

exec python3 -m wrkslots "${project_args[@]}" "$@"
