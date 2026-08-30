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

# --- stub bin: a systemd-run that records its argv and a gh that merely exists.
mkdir -p "$work/bin" "$work/ghdir" "$state_root"
cat >"$work/bin/systemd-run" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$SYSTEMD_RUN_ARGV_SINK"
exit "${SYSTEMD_RUN_RC:-0}"
STUB
chmod +x "$work/bin/systemd-run"
printf '#!/usr/bin/env bash\nexit 0\n' > "$work/ghdir/gh"; chmod +x "$work/ghdir/gh"
# `systemctl` must exist and report the unit inactive, or the launcher short-circuits.
printf '#!/usr/bin/env bash\nexit 1\n' > "$work/bin/systemctl"; chmod +x "$work/bin/systemctl"

run_launcher() { # script  extra-PATH-dir  -> writes argv to $work/argv, stdout to $work/out
  local script=$1 extra=${2:-}
  : > "$work/argv"
  SYSTEMD_RUN_ARGV_SINK="$work/argv" \
  DEV_HERMIT_PARENT="$state_root" \
  LEDGER_PUBLISH_GH_BIN="${extra:+$extra/gh}" \
    PATH="$work/bin${extra:+:$extra}:/usr/bin:/bin" \
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

echo "== FIXED launcher, gh ABSENT: must refuse rather than launch a doomed sweep =="
run_launcher "$launcher" ""
check "refused"                         "state=refused"                        "$work/out"
check "names the cause"                 "gh not found"                         "$work/out"
check_absent "did not launch"           "--unit="                              "$work/argv"

echo "== FIXED launcher, systemd-run FAILS: its own error must be kept =="
SYSTEMD_RUN_RC=1 run_launcher "$launcher" "$work/ghdir"
check "refusal reported"                "state=refused"                        "$work/out"
check "systemd-run's output retained"   "systemd-run said:"                    "$work/out"

echo "== BASELINE (pre-fix) launcher: every positive above must FAIL here =="
# Reconstruct the invocation as it stood before this change: no --setenv, no
# StandardOutput/StandardError, and the launcher's own stderr discarded.
baseline=$work/baseline.sh
cp "$launcher" "$baseline"
python3 - "$baseline" <<'PY'
import re, sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
for line in ('    --property="StandardOutput=append:$LOG" \\\n',
             '    --property="StandardError=append:$LOG" \\\n',
             '    --working-directory="$TOOL_ROOT" \\\n',
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

echo
if [[ $fails -eq 0 ]]; then
  echo "test_launch_publish_backlog: PASS (fixed launcher hands the unit a gh-bearing PATH and a readable output file; refuses when gh is absent; retains systemd-run's own error; and the reconstructed pre-fix launcher fails all three, so none of these assertions is vacuous)"
  exit 0
fi
echo "test_launch_publish_backlog: FAIL ($fails check(s))"
exit 1
