#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.
#
# Bracket for the detached ledger publish launcher.
#
# TWO DEFECTS, BOTH BRACKETED IN BOTH DIRECTIONS AGAINST THE PRE-FIX SCRIPT.
# A fix whose before-state is not demonstrated is indistinguishable from a no-op,
# so every positive here has a matching assertion run against a reconstructed
# baseline launcher (BASELINE_SNIPPET below), which must FAIL the same check.
#
#   1. PATH. `gh` lives in $HOME/bin on this host; a `systemd --user` unit
#      inherits the user manager's PATH, which does not contain it. Publication
#      pushes through a git credential helper that shells out to `gh`, so the
#      sweep failed ONLY on the write path -- invisible to any read-only
#      reachability probe, and masked entirely when a human ran it from a shell
#      whose profile had already added $HOME/bin.
#
#   2. OUTPUT DESTINATION. The sweep's stdout/stderr went to the journal by
#      default. `journalctl --user` on this host answers "No journal files were
#      opened due to insufficient permissions" for EVERY user unit, because the
#      account is in none of adm/systemd-journal/wheel. The diagnostic was not
#      discarded -- it had no reachable audience, which from outside is
#      indistinguishable from being discarded.
#
# systemd-run is STUBBED so these assertions test the arguments the unit would
# receive, not the local systemd. Testing against real systemd would make the
# bracket a test of this host's manager state instead of the launcher's contract.
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
launcher=$here/../launch_publish_backlog.sh
work=$(mktemp -d "${TMPDIR:-/tmp}/launch-publish-test.XXXXXX")
tool_root=$(cd -- "$here/../../.." && pwd)
state_root=$work/state-root
trap 'rm -rf "$work"' EXIT
fails=0

note() { printf '  %-6s %s\n' "$1" "$2"; }
check() { # label  expected-substring  file
  if grep -Fq -- "$2" "$3"; then note PASS "$1"; else note FAIL "$1 -- expected to find: $2"; fails=$((fails+1)); fi
}
check_absent() { # label  forbidden-substring  file
  if grep -Fq -- "$2" "$3"; then note FAIL "$1 -- should NOT contain: $2"; fails=$((fails+1)); else note PASS "$1"; fi
}
check_rc() { # label expected actual
  if [[ $2 == "$3" ]]; then note PASS "$1"; else note FAIL "$1 -- expected rc=$2, got rc=$3"; fails=$((fails+1)); fi
}

# --- stub bin: a systemd-run that records its argv and a gh that merely exists.
mkdir -p "$work/bin" "$work/ghdir" "$state_root"
cat >"$work/bin/systemd-run" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$SYSTEMD_RUN_ARGV_SINK"
exit "${SYSTEMD_RUN_RC:-0}"
STUB
chmod +x "$work/bin/systemd-run"
printf '#!/usr/bin/env bash\nexit 0\n' > "$work/ghdir/gh"; chmod +x "$work/ghdir/gh"
mkdir -p "$work/home/bin"
cp "$work/ghdir/gh" "$work/home/bin/gh"
# `systemctl` must exist and report the unit inactive and not failed unless a
# test selects a prior failed result explicitly.
cat >"$work/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${SYSTEMCTL_ARGV_SINK:-/dev/null}"
case " $* " in
  *" is-active "*) exit "${SYSTEMCTL_ACTIVE_RC:-1}" ;;
  *" is-failed "*) exit "${SYSTEMCTL_FAILED_RC:-1}" ;;
  *" reset-failed "*) exit "${SYSTEMCTL_RESET_RC:-0}" ;;
  *" show "*"--property=Result"*) printf '%s\n' "${SYSTEMCTL_RESULT:-exit-code}"; exit 0 ;;
  *" show "*"--property=ExecMainStatus"*) printf '%s\n' "${SYSTEMCTL_STATUS:-2}"; exit 0 ;;
esac
exit 1
STUB
chmod +x "$work/bin/systemctl"
for command in bash dirname mkdir tee; do
  ln -s "$(command -v "$command")" "$work/bin/$command"
done

run_launcher() { # script  extra-PATH-dir  -> writes argv to $work/argv, stdout to $work/out
  local script=$1 extra=${2:-}
  : > "$work/argv"
  : > "$work/systemctl-argv"
  SYSTEMD_RUN_ARGV_SINK="$work/argv" \
  SYSTEMCTL_ARGV_SINK="$work/systemctl-argv" \
  DEV_HERMIT_PARENT="$state_root" \
  LEDGER_PUBLISH_GH_BIN="${extra:+$extra/gh}" \
    PATH="$work/bin${extra:+:$extra}:/usr/bin:/bin" \
    bash "$script" > "$work/out" 2>&1
}

run_operational_launcher() { # script -> records the detached exact-revision re-entry
  local script=$1
  : > "$work/argv"
  SYSTEMD_RUN_ARGV_SINK="$work/argv" \
  DEV_HERMIT_PARENT="$state_root" \
  DEV_HERMIT_OPERATIONAL_TOOL="$work/operational-tool" \
  DEV_HERMIT_TOOL_PARENT_SHA=0123456789abcdef0123456789abcdef01234567 \
  DEV_HERMIT_TOOL_CACHE="$work/tool-cache" \
  LEDGER_PUBLISH_GH_BIN="$work/ghdir/gh" \
    PATH="$work/bin:$work/ghdir:/usr/bin:/bin" \
    bash "$script" > "$work/out" 2>&1
}

run_installed_operational_launcher() { # script -> parent SHA plus installed stable bootstrap
  local script=$1
  : > "$work/argv"
  mkdir -p "$work/home/.local/libexec/dev-hermit"
  cp "$work/operational-tool" "$work/home/.local/libexec/dev-hermit/operational-tool"
  SYSTEMD_RUN_ARGV_SINK="$work/argv" \
  DEV_HERMIT_PARENT="$state_root" \
  DEV_HERMIT_TOOL_PARENT_SHA=0123456789abcdef0123456789abcdef01234567 \
  DEV_HERMIT_TOOL_CACHE="$work/tool-cache" \
  LEDGER_PUBLISH_GH_BIN="$work/ghdir/gh" \
  HOME="$work/home" \
    PATH="$work/bin:$work/ghdir:/usr/bin:/bin" \
    bash "$script" > "$work/out" 2>&1
}

run_home_gh_launcher() { # script -> PATH omits gh; only $HOME/bin supplies it
  local script=$1
  : > "$work/argv"
  SYSTEMD_RUN_ARGV_SINK="$work/argv" \
  DEV_HERMIT_PARENT="$state_root" \
  HOME="$work/home" \
    PATH="$work/bin" \
    bash "$script" > "$work/out" 2>&1
}

echo "== FIXED launcher, gh present =="
run_launcher "$launcher" "$work/ghdir"
check "launch reported"                 "state=launched"                       "$work/out"
check "1. PATH handed to the unit"      "--setenv=PATH="                       "$work/argv"
check "1. that PATH contains gh's dir"  "$work/ghdir"                          "$work/argv"
check "2. stdout captured in state root" "StandardOutput=append:$state_root/ignored/ci-hub" "$work/argv"
check "2. stderr captured in state root" "StandardError=append:$state_root/ignored/ci-hub" "$work/argv"
check "log path named to the caller"    "$state_root/ignored/ci-hub/ledger-publish-backlog.log" "$work/out"
check "tool cwd is immutable root"      "--working-directory=$tool_root"       "$work/argv"
check "state root is explicit"          "--setenv=DEV_HERMIT_PARENT=$state_root" "$work/argv"
check "tool root is explicit"           "--setenv=DEV_HERMIT_TOOL_ROOT=$tool_root" "$work/argv"
check "publisher code comes from tool"  "$tool_root/ci-hub/ledger/publish_backlog.py" "$work/argv"
check_absent "failed result remains queryable" "--collect"                        "$work/argv"

echo "== FIXED launcher, previous detached command FAILED: report and retry =="
SYSTEMCTL_FAILED_RC=0 SYSTEMCTL_RESULT=exit-code SYSTEMCTL_STATUS=2 \
  run_launcher "$launcher" "$work/ghdir"
fixed_failed_rc=$?
check_rc "prior child failure makes the tick fail" 1 "$fixed_failed_rc"
check "terminal result reported"          "Result=exit-code ExecMainStatus=2" "$work/out"
check "failed child retried"              "state=relaunched"                 "$work/out"
check "failed unit reset after reading"   "reset-failed ledger-publish-backlog.service" "$work/systemctl-argv"
check "retry launched"                    "--unit=ledger-publish-backlog"     "$work/argv"

echo "== FIXED launcher, previous detached command still RUNNING: report it =="
SYSTEMCTL_ACTIVE_RC=0 run_launcher "$launcher" "$work/ghdir"
fixed_running_rc=$?
check_rc "still-running child makes the tick fail" 1 "$fixed_running_rc"
check "still-running child reported"       "state=running"                    "$work/out"
check "still-running spool retained"       "spool retained"                   "$work/out"
check_absent "no overlapping child launched" "--unit="                         "$work/argv"

echo "== FIXED launcher, gh only in HOME/bin outside systemd-manager PATH =="
run_home_gh_launcher "$launcher"
check "home gh launch reported"          "state=launched"                       "$work/out"
check "home gh directory handed to unit" "$work/home/bin"                       "$work/argv"

echo "== FIXED launcher, detached health-tick handoff =="
printf '#!/usr/bin/env bash\nexit 0\n' > "$work/operational-tool"
chmod +x "$work/operational-tool"
mkdir -p "$work/tool-cache"
run_operational_launcher "$launcher"
check "operational launch reported"       "state=launched"                       "$work/out"
check "stable bootstrap runs"             "$work/operational-tool"               "$work/argv"
check "exact parent revision retained"    "0123456789abcdef0123456789abcdef01234567" "$work/argv"
check "configured cache retained"         "--cache-root"                         "$work/argv"
check "publisher selected relatively"     "ci-hub/ledger/publish_backlog.py"      "$work/argv"
check "stable working directory"          "--working-directory=$state_root"       "$work/argv"
check_absent "private tool root not lent" "$tool_root/ci-hub/ledger/publish_backlog.py" "$work/argv"

echo "== FIXED launcher, health tick supplies SHA and installed stable bootstrap =="
run_installed_operational_launcher "$launcher"
check "installed bootstrap launch reported" "state=launched"                       "$work/out"
check "installed bootstrap runs"           "$work/home/.local/libexec/dev-hermit/operational-tool" "$work/argv"
check "installed bootstrap keeps SHA"      "0123456789abcdef0123456789abcdef01234567" "$work/argv"

echo "== FIXED launcher, incomplete detached handoff: refuse and retain =="
: > "$work/argv"
SYSTEMD_RUN_ARGV_SINK="$work/argv" \
DEV_HERMIT_PARENT="$state_root" \
DEV_HERMIT_OPERATIONAL_TOOL="$work/operational-tool" \
LEDGER_PUBLISH_GH_BIN="$work/ghdir/gh" \
  PATH="$work/bin:$work/ghdir:/usr/bin:/bin" \
  bash "$launcher" > "$work/out" 2>&1
check "incomplete handoff refused"        "state=refused"                        "$work/out"
check "refusal preserves spool"           "spool retained"                       "$work/out"
check_absent "incomplete handoff not launched" "--unit="                           "$work/argv"

echo "== FIXED launcher, gh ABSENT: must refuse rather than launch a doomed sweep =="
run_launcher "$launcher" ""
missing_gh_rc=$?
check "refused"                         "state=refused"                        "$work/out"
check "names the cause"                 "gh not found"                         "$work/out"
check_absent "did not launch"           "--unit="                              "$work/argv"
check_rc "missing gh is a gate failure" 1 "$missing_gh_rc"

echo "== FIXED launcher, systemd-run FAILS: its own error must be kept =="
SYSTEMD_RUN_RC=1 run_launcher "$launcher" "$work/ghdir"
launch_refused_rc=$?
check "refusal reported"                "state=refused"                        "$work/out"
check "systemd-run's output retained"   "systemd-run said:"                    "$work/out"
check_rc "launch refusal is a gate failure" 1 "$launch_refused_rc"

echo "== BASELINE (pre-fix) launcher: every positive above must FAIL here =="
# Reconstruct the invocation as it stood before this change: no --setenv, no
# StandardOutput/StandardError, and the launcher's own stderr discarded.
baseline=$work/baseline.sh
cp "$launcher" "$baseline"
python3 - "$baseline" <<'PY'
import re, sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
new_lookup = '''if [[ ${LEDGER_PUBLISH_GH_BIN+x} ]]; then
  GH_CANDIDATES=("$LEDGER_PUBLISH_GH_BIN")
else
  # The user manager's PATH is not the login-shell PATH. In particular, the
  # production host keeps gh in $HOME/bin while systemd --user omits that
  # directory. Resolve the known per-user locations explicitly before falling
  # back to PATH, then pass the selected directory to the transient unit.
  GH_CANDIDATES=(
    "$HOME/bin/gh"
    "${FLEET_REAL_GH:-}"
    "$(command -v gh 2>/dev/null || true)"
    "$HOME/.local/libexec/dev-hermit/gh"
  )
fi
GH_BIN=
for candidate in "${GH_CANDIDATES[@]}"; do
  if [[ -n $candidate && -f $candidate && -x $candidate ]]; then
    GH_BIN=$candidate
    break
  fi
done
'''
old_lookup = '''if [[ ${LEDGER_PUBLISH_GH_BIN+x} ]]; then
  GH_BIN=$LEDGER_PUBLISH_GH_BIN
else
  GH_BIN=$(command -v gh 2>/dev/null || true)
fi
'''
assert new_lookup in s, "baseline reconstruction lost the gh lookup anchor"
s = s.replace(new_lookup, old_lookup, 1)
new_operational = '''OPERATIONAL_TOOL=${DEV_HERMIT_OPERATIONAL_TOOL:-}
if [[ -z $OPERATIONAL_TOOL && -n ${DEV_HERMIT_TOOL_PARENT_SHA:-} ]]; then
  installed_operational_tool=$HOME/.local/libexec/dev-hermit/operational-tool
  if [[ ! -L $installed_operational_tool && -f $installed_operational_tool && \\
        -x $installed_operational_tool ]]; then
    OPERATIONAL_TOOL=$installed_operational_tool
  fi
fi
if [[ -n $OPERATIONAL_TOOL || -n ${DEV_HERMIT_TOOL_PARENT_SHA:-} ]]; then
  if [[ -z $OPERATIONAL_TOOL || ! -x $OPERATIONAL_TOOL || \\
        ! ${DEV_HERMIT_TOOL_PARENT_SHA:-} =~ ^[0-9a-f]{40}$ ]]; then
'''
old_operational = '''if [[ -n ${DEV_HERMIT_OPERATIONAL_TOOL:-} || -n ${DEV_HERMIT_TOOL_PARENT_SHA:-} ]]; then
  if [[ -z ${DEV_HERMIT_OPERATIONAL_TOOL:-} || \\
        ! -x ${DEV_HERMIT_OPERATIONAL_TOOL:-} || \\
        ! ${DEV_HERMIT_TOOL_PARENT_SHA:-} =~ ^[0-9a-f]{40}$ ]]; then
'''
assert new_operational in s, "baseline reconstruction lost the operational lookup anchor"
s = s.replace(new_operational, old_operational, 1)
s = s.replace('    "$OPERATIONAL_TOOL"\n    --run-current-tool',
              '    "$DEV_HERMIT_OPERATIONAL_TOOL"\n    --run-current-tool', 1)
failed_result = '''# A collected transient unit vanishes as soon as it exits. `systemctl show`
# then describes a synthetic not-found unit whose Result=success and
# ExecMainStatus=0 are ABSENCE, not evidence that the command passed. Keep a
# failed unit loaded until this next sweep reads its real terminal result. Reset
# it only after capturing that result so the same unit name can be retried.
PRIOR_FAILURE=
if systemctl --user is-failed --quiet "$UNIT.service" 2>/dev/null; then
  UNIT_RESULT=$(systemctl --user show "$UNIT.service" --property=Result --value \\
    2>/dev/null || true)
  UNIT_STATUS=$(systemctl --user show "$UNIT.service" --property=ExecMainStatus --value \\
    2>/dev/null || true)
  PRIOR_FAILURE="previous $UNIT failed with Result=${UNIT_RESULT:-unknown} ExecMainStatus=${UNIT_STATUS:-unknown}"
  if ! reset_error=$(systemctl --user reset-failed "$UNIT.service" 2>&1); then
    echo "state=refused summary=$PRIOR_FAILURE; could not reset it for retry; spool retained; systemctl said: ${reset_error:-<no output>}" \\
      | tee -a "$LOG" 2>/dev/null || true
    exit 1
  fi
fi

'''
assert failed_result in s, "baseline reconstruction lost the failed-result anchor"
s = s.replace(failed_result, 'PRIOR_FAILURE=\n', 1)
s = s.replace('systemd-run --user --quiet \\\n',
              'systemd-run --user --quiet --collect \\\n', 1)
for line in ('    --property="StandardOutput=append:$LOG" \\\n',
             '    --property="StandardError=append:$LOG" \\\n',
             '    --working-directory="$PUBLISH_WORKING_DIRECTORY" \\\n',
             '    --setenv="PATH=$UNIT_PATH" \\\n',
             '    --setenv="DEV_HERMIT_PARENT=$STATE_ROOT" \\\n',
             '    --setenv="DEV_HERMIT_TOOL_ROOT=$TOOL_ROOT" \\\n'):
    assert line in s, f"baseline reconstruction anchor missing: {line!r}"
    s = s.replace(line, '', 1)
p.write_text(s)
PY
run_launcher "$baseline" "$work/ghdir"
check        "baseline still launches"        "state=launched"    "$work/out"
check_absent "baseline hands NO PATH"         "--setenv=PATH="    "$work/argv"
check_absent "baseline captures NO stdout"    "StandardOutput="   "$work/argv"
check_absent "baseline captures NO stderr"    "StandardError="    "$work/argv"
check_absent "baseline carries NO state root" "DEV_HERMIT_PARENT" "$work/argv"
run_home_gh_launcher "$baseline"
check        "baseline cannot find HOME gh"   "state=refused"     "$work/out"
check_absent "baseline does not launch HOME gh" "--unit="          "$work/argv"
run_installed_operational_launcher "$baseline"
check        "baseline cannot recover installed bootstrap" "state=refused" "$work/out"
check_absent "baseline does not launch recovered bootstrap" "--unit="       "$work/argv"
SYSTEMCTL_FAILED_RC=0 SYSTEMCTL_RESULT=exit-code SYSTEMCTL_STATUS=2 \
  run_launcher "$baseline" "$work/ghdir"
baseline_failed_rc=$?
check_rc    "baseline hides prior child failure" 0 "$baseline_failed_rc"
check       "baseline calls the failed run launched" "state=launched" "$work/out"
check_absent "baseline omits terminal result" "ExecMainStatus=2" "$work/out"

echo
if [[ $fails -eq 0 ]]; then
  echo "test_launch_publish_backlog: PASS (fixed launcher hands the unit a gh-bearing PATH and readable output, re-enters the exact cached tool revision for detached work, retains and reports a detached failure before retrying it, refuses incomplete handoff and missing gh without dropping the spool, retains systemd-run's own error, and the reconstructed pre-fix launcher fails the original checks)"
  exit 0
fi
echo "test_launch_publish_backlog: FAIL ($fails check(s))"
exit 1
