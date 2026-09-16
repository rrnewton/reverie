#!/usr/bin/env bash
# Bracket gate_runner_presence.py BY DELETION, not by inspection.
#
# The 2026-08-08 incident: the installation and credentials were deleted, the
# user service stayed ENABLED and failed 203/EXEC for ~2 days, and nothing
# reported it. So the decisive case here is not "a missing directory is
# noticed" -- it is "an ENABLED service over a DELETED installation is
# reported", which is the exact state systemctl called healthy.
#
# Everything happens under a scratch probe root and a scratch user unit with a
# distinct name. The live hermit-gate-runner.service and its installation are
# never read for mutation and never touched.
#
# Usage: test_gate_runner_presence.sh [--keep]
set -euo pipefail

DETECTOR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/gate_runner_presence.py"
PROBE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/gate-runner-probe-XXXXXX")"
UNIT="gate-runner-presence-probe.service"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_PATH="$UNIT_DIR/$UNIT"
LIVE_UNIT="hermit-gate-runner.service"

pass=0; fail=0
cleanup() {
    systemctl --user disable --now "$UNIT" >/dev/null 2>&1 || true
    rm -f "$UNIT_PATH"
    # A unit left in `not-found failed` stays listed until it is reset, so
    # reset-failed is part of leaving no residue, not an afterthought.
    systemctl --user reset-failed "$UNIT" >/dev/null 2>&1 || true
    systemctl --user daemon-reload >/dev/null 2>&1 || true
    [[ ${1:-} == keep ]] || rm -rf "$PROBE_ROOT"
}
trap 'cleanup' EXIT

# Refuse to run if the scratch unit name would collide with the live one.
if [[ $UNIT == "$LIVE_UNIT" ]]; then
    echo "REFUSED: scratch unit name collides with the live gate runner" >&2
    exit 3
fi

expect() { # expect <label> <expected-rc> <expected-substring> <args...>
    local label=$1 want_rc=$2 want_text=$3; shift 3
    local out rc
    out=$(python3 "$DETECTOR" "$@" 2>&1) && rc=0 || rc=$?
    if [[ $rc == "$want_rc" ]] && grep -qi -- "$want_text" <<< "$out"; then
        echo "  PASS  $label (rc=$rc)"
        pass=$((pass + 1))
    else
        echo "  FAIL  $label: wanted rc=$want_rc containing '$want_text', got rc=$rc"
        sed 's/^/        /' <<< "$out"
        fail=$((fail + 1))
    fi
}

install_scratch_runner() {
    rm -rf "$PROBE_ROOT"
    mkdir -p "$PROBE_ROOT/bin" "$PROBE_ROOT/externals" "$PROBE_ROOT/_work"
    printf '#!/bin/sh\nsleep 3600\n' > "$PROBE_ROOT/run.sh"
    printf '#!/bin/sh\nexit 0\n' > "$PROBE_ROOT/config.sh"
    chmod +x "$PROBE_ROOT/run.sh" "$PROBE_ROOT/config.sh"
    printf 'scratch\n' > "$PROBE_ROOT/bin/placeholder"
    printf 'scratch\n' > "$PROBE_ROOT/externals/placeholder"
    printf '{"scratch":true}\n' > "$PROBE_ROOT/.credentials"
    printf '{"scratch":true}\n' > "$PROBE_ROOT/.credentials_rsaparams"
    # Written WITH a BOM, exactly as config.sh writes the real one.
    printf '\xef\xbb\xbf{"agentId":999999,"agentName":"scratch-probe-runner","gitHubUrl":"https://github.com/rrnewton/hermit"}\n' \
        > "$PROBE_ROOT/.runner"
}

install_scratch_unit() {
    mkdir -p "$UNIT_DIR"
    cat > "$UNIT_PATH" <<UNITEOF
[Unit]
Description=Scratch probe unit for gate_runner_presence brackets (safe to delete)
[Service]
Type=simple
ExecStart=$PROBE_ROOT/run.sh
[Install]
WantedBy=default.target
UNITEOF
    systemctl --user daemon-reload
    systemctl --user enable --now "$UNIT" >/dev/null 2>&1 || true
}

echo "probe root: $PROBE_ROOT"
echo "scratch unit: $UNIT (live $LIVE_UNIT is never touched)"

# ---------------------------------------------------------------- untouched
# Measure the healthy hierarchy FIRST, before any mutation has been applied to
# it. Ordering matters: a later probe's side effect must not decide an earlier
# probe's verdict.
echo
echo "[1] untouched scratch installation, enabled+running unit"
install_scratch_runner
install_scratch_unit
expect "healthy scratch runner is NOT flagged (installation+credentials+service)" \
    2 "registration" --install-root "$PROBE_ROOT" --service "$UNIT" --offline
expect "  ... and its three local surfaces are all PRESENT" \
    2 "PRESENT   service" --install-root "$PROBE_ROOT" --service "$UNIT" --offline

# ------------------------------------------------------------------- deletion
echo
echo "[2] THE INCIDENT: delete the installation, leave the unit ENABLED"
rm -rf "${PROBE_ROOT:?}"/bin "${PROBE_ROOT:?}"/externals \
       "${PROBE_ROOT:?}"/run.sh "${PROBE_ROOT:?}"/config.sh \
       "${PROBE_ROOT:?}"/.credentials "${PROBE_ROOT:?}"/.credentials_rsaparams \
       "${PROBE_ROOT:?}"/.runner
# _work survives, exactly as it did on 2026-08-08.
enabled_state=$(systemctl --user show "$UNIT" -p UnitFileState --value 2>/dev/null || echo unknown)
echo "  systemctl still reports UnitFileState=${enabled_state}  <-- the two-day blind spot"
expect "deleted installation is REPORTED despite the unit being enabled" \
    2 "ABSENT    installation" --install-root "$PROBE_ROOT" --service "$UNIT" --offline
expect "  ... and it names the _work-only deletion shape" \
    2 "only _work" --install-root "$PROBE_ROOT" --service "$UNIT" --offline
expect "deleted credentials are reported SEPARATELY from installation" \
    2 "ABSENT    credentials" --install-root "$PROBE_ROOT" --service "$UNIT" --offline

echo
echo "[3] enabled-but-failed service is RED (enabled is necessary, not sufficient)"
# RESTART, not start: the unit is still active from before the deletion, and a
# process already exec'd does not care that its binary is gone. `start` on an
# active unit is a no-op, so it never re-execs and never reproduces 203/EXEC --
# the first version of this bracket made exactly that mistake and reported the
# service surface green for the wrong reason.
systemctl --user restart "$UNIT" >/dev/null 2>&1 || true
sleep 2
still_enabled=$(systemctl --user show "$UNIT" -p UnitFileState --value 2>/dev/null || echo unknown)
result=$(systemctl --user show "$UNIT" -p Result --value 2>/dev/null || echo unknown)
status=$(systemctl --user show "$UNIT" -p ExecMainStatus --value 2>/dev/null || echo unknown)
echo "  UnitFileState=${still_enabled} Result=${result} ExecMainStatus=${status}"
if [[ $status != 203 ]]; then
    echo "  NOTE: expected ExecMainStatus=203 (the incident's exec failure), got '${status}'"
fi
expect "an ENABLED unit that cannot exec is not called healthy" \
    2 "ABSENT    service" --install-root "$PROBE_ROOT" --service "$UNIT" --offline
expect "  ... and the report says ENABLED explicitly, so the tell is legible" \
    2 "is ENABLED but" --install-root "$PROBE_ROOT" --service "$UNIT" --offline

# -------------------------------------------------------------- registration
echo
echo "[4] registration surface is independent of the local ones"
install_scratch_runner   # local surfaces healthy again, bogus agent id 999999
expect "an unregistered agent id is reported ABSENT" \
    2 "is NOT registered" --install-root "$PROBE_ROOT" --service "$UNIT"
expect "--offline yields NO_RESULT for registration, never an assumed pass" \
    2 "NO_RESULT registration" --install-root "$PROBE_ROOT" --service "$UNIT" --offline

echo
echo "[5] a missing unit is ABSENT, not a crash"
expect "unknown service name is reported" \
    2 "service" --install-root "$PROBE_ROOT" --service "definitely-not-a-unit.service" --offline

# ------------------------------------------------------------------- cadence
echo
echo "[6] cadence: stale or missing records are NO_RESULT at admission"
record="$PROBE_ROOT/presence.json"
expect "no record at all fails closed" \
    2 "never checked" --admission-check --record "$record"
printf '{"checked_at": 1, "aggregate": "PRESENT", "surfaces": {}}\n' > "$record"
expect "a PRESENT record older than the cadence fails closed" \
    2 "stale is NO_RESULT" --admission-check --record "$record" --max-age 900
python3 - "$record" <<'PY'
import json, sys, time
json.dump({"checked_at": int(time.time()), "aggregate": "PRESENT", "surfaces": {}},
          open(sys.argv[1], "w"))
PY
expect "a fresh PRESENT record admits" \
    0 "within the 900s cadence" --admission-check --record "$record" --max-age 900
printf '{"checked_at": %s, "aggregate": "NO_RESULT", "surfaces": {}}\n' "$(date +%s)" > "$record"
expect "a fresh NO_RESULT record still fails closed" \
    2 "last recorded verdict was NO_RESULT" --admission-check --record "$record"

# ------------------------------------------------- the tick-hub reporting contract
# The probe worked for this whole session while the tick showed nothing, because
# tick-hub resolves `{summary}` from stdout `key=value` lines and the probe
# emitted none. These brackets cover the CONTRACT between probe and engine,
# which the surface brackets above never touched: a gate that has never emitted
# has never been validated.
echo
echo "[7] tick-hub reporting contract: summary= must render AND must report"

harvest() { # harvest <args...> -- echo the parse_kv_lines field set, sorted
    python3 - "$DETECTOR" "$@" <<'PY'
import subprocess, sys
out = subprocess.run([sys.executable, *sys.argv[1:]], capture_output=True,
                     text=True).stdout
fields = {}
for line in out.splitlines():                 # mirrors tick_hub.engine.parse_kv_lines
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    if "=" in line:
        key, _, value = line.partition("=")
        if key.strip():
            fields[key.strip()] = value.strip()
print(",".join(sorted(fields)))
PY
}

# Present direction, on the inert scratch fixture -- never a planted real runner.
install_scratch_runner
install_scratch_unit
expect "summary names ONLY the surface that is not PRESENT" \
    2 "summary=NO_RESULT on 1 of 4 surfaces -- registration" \
    --install-root "$PROBE_ROOT" --service "$UNIT" --offline

got=$(harvest --install-root "$PROBE_ROOT" --service "$UNIT" --offline)
if [[ $got == "summary" ]]; then
    echo "  PASS  stdout harvests EXACTLY one field and it is summary"
    pass=$((pass + 1))
else
    echo "  FAIL  stdout harvests '$got', wanted exactly 'summary'"
    fail=$((fail + 1))
fi

# Absent direction: same probe, a root that does not exist.
# The scratch unit is still enabled+running here, so `service` is legitimately
# PRESENT: the summary must name the three that are NOT, and must NOT claim four.
# That it reads "1 of 4 ... registration" above and "3 of 4 ..." here is the
# evidence that it reports state rather than rendering a constant.
expect "summary reports the absence, naming exactly the three failing surfaces" \
    2 "summary=NO_RESULT on 3 of 4 surfaces -- installation ABSENT, credentials ABSENT, registration NO_RESULT" \
    --install-root "$PROBE_ROOT/does-not-exist" --service "$UNIT" --offline

# A detail containing `=` must not become a field. This is the defect the
# missing summary was masking: gate_runner_presence.py renders
# "... is ENABLED but ActiveState={active}", and splitting on the FIRST `=`
# would key a field on the whole leading sentence. Bracketed against the
# REAL renderer (report_lines), not a copy of its format strings, and against
# an all-PRESENT set that is unreachable offline because registration needs
# GitHub -- so the positive direction is proven without planting a live runner.
python3 - "$DETECTOR" <<'PY' && { echo "  PASS  hostile '=' detail yields only summary; all-PRESENT summarises as PRESENT"; pass=$((pass + 1)); } || { echo "  FAIL  reporting contract broken on hostile detail or all-PRESENT set"; fail=$((fail + 1)); }
import importlib.util, sys
spec = importlib.util.spec_from_file_location("grp", sys.argv[1])
m = importlib.util.module_from_spec(spec)
sys.modules["grp"] = m           # @dataclass resolves via sys.modules[__module__]
spec.loader.exec_module(m)

def harvest(lines):              # mirrors tick_hub.engine.parse_kv_lines
    out = {}
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" in line:
            key, _, value = line.partition("=")
            if key.strip():
                out[key.strip()] = value.strip()
    return out

hostile = [m.Surface("installation", m.PRESENT, "ok"),
           m.Surface("credentials", m.PRESENT, "ok"),
           m.Surface("service", m.ABSENT, "svc is ENABLED but ActiveState=inactive"),
           m.Surface("registration", m.NO_RESULT, "offline")]
fields = harvest(m.report_lines(m.NO_RESULT, "svc", "/root", hostile))
assert set(fields) == {"summary"}, f"leaked fields: {sorted(fields)}"
assert "ActiveState" not in "".join(fields), fields
assert "service ABSENT" in fields["summary"], fields["summary"]

allp = [m.Surface(n, m.PRESENT, "ok") for n in
        ("installation", "credentials", "service", "registration")]
got = harvest(m.report_lines(m.PRESENT, "svc", "/root", allp))["summary"]
assert got == "PRESENT: all 4 surfaces PRESENT", got
PY

# The detector executes from an immutable tool snapshot while the installation
# lives in the canonical state root. Exercise its real argument parser from a
# different source path; the surface checks themselves are bracketed above.
python3 - "$DETECTOR" <<'ROOT_SELECTION_PY' && { echo "  PASS  canonical state default, standalone fallback, and explicit override"; pass=$((pass + 1)); } || { echo "  FAIL  installation root selection"; fail=$((fail + 1)); }
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import shutil
import sys
import tempfile
from unittest import mock

with tempfile.TemporaryDirectory(prefix="gate-runner-roots-") as temporary:
    root = Path(temporary)
    tool = root / "tool snapshot"
    detector = tool / "ci-hub" / "health" / "gate_runner_presence.py"
    detector.parent.mkdir(parents=True)
    shutil.copy2(sys.argv[1], detector)
    state = root / "operational state"
    override = root / "explicit installation"
    for label, state_root, arguments, expected in (
        ("canonical state", state, [], state / "ci-runner" / "state-gate"),
        ("standalone fallback", None, [], tool / "ci-runner" / "state-gate"),
        ("explicit override", state, ["--install-root", str(override)], override),
    ):
        environment = dict(os.environ)
        environment.pop("DEV_HERMIT_PARENT", None)
        if state_root is not None:
            environment["DEV_HERMIT_PARENT"] = str(state_root)
        with mock.patch.dict(os.environ, environment, clear=True):
            spec = importlib.util.spec_from_file_location("grp_roots", detector)
            module = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = module
            spec.loader.exec_module(module)
            assert module.REPO_ROOT == tool, module.REPO_ROOT
            with mock.patch.object(sys, "argv", [str(detector), *arguments]), \
                    mock.patch.object(module, "evaluate", return_value=(module.NO_RESULT, [])) as evaluate, \
                    mock.patch.object(module, "write_record") as write_record, \
                    contextlib.redirect_stdout(io.StringIO()):
                assert module.main() == 2
            evaluate.assert_called_once()
            actual = Path(evaluate.call_args.args[0].install_root)
            assert actual == expected, f"{label}: {actual} != {expected}"
            write_record.assert_not_called()
ROOT_SELECTION_PY

echo
echo "brackets: ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]]
