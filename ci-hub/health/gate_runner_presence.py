#!/usr/bin/env python3
"""Detect deletion of a self-hosted gate runner across four independent surfaces.

Certification condition 9. On 2026-08-08 the gate-runner installation
and credentials were deleted; the user service stayed **enabled** and failed
203/EXEC for roughly two days without reporting the deletion, and the shortfall
presented as CI saturation. The runner was later restored and executed gate jobs
again -- but recovery is not a detector.

The load-bearing design constraint comes straight from that incident: the
service surface said `enabled` the whole time, and only `_work` survived under
the installation root. A monitor that checks service state, or that treats "the
directory exists" as installed, reproduces the two-day blindness exactly. So the
four surfaces are checked **separately** and reported **separately**:

1. `installation` -- the runner distribution is present AND the unit's ExecStart
   target is an executable file. `_work` alone is ABSENT, by name.
2. `credentials`  -- `.credentials`, `.credentials_rsaparams` and `.runner` are
   present, non-empty, and `.runner` parses (it is written UTF-8 **with BOM**).
3. `service`      -- enabled is necessary and NOT sufficient: an enabled unit
   that is failed, or whose last exec status is nonzero, is RED.
4. `registration` -- the agent id recorded in `.runner` is still registered with
   the repository, with its reported status.

Absence is NO_RESULT, never silence: any surface that cannot be positively
confirmed -- including one that could not be evaluated at all -- makes the
aggregate NO_RESULT and exits nonzero, so an admission caller fails closed.

Exit codes:
  0  PRESENT    all four surfaces positively confirmed
  2  NO_RESULT  any surface absent, degraded, or unevaluable (fail closed)
  3  usage/internal error
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field, asdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
# Immutable tool snapshots hold code; the runner installation belongs to the
# canonical operational state root. Standalone invocations keep the repo default.
DEFAULT_INSTALL_ROOT = str(
    Path(os.environ.get("DEV_HERMIT_PARENT", REPO_ROOT)) / "ci-runner" / "state-gate"
)
DEFAULT_SERVICE = "hermit-gate-runner.service"
DEFAULT_REPO = "rrnewton/hermit"

# Cadence: how old a recorded PRESENT verdict may be before the record itself is
# treated as NO_RESULT. A monitor whose last answer is arbitrarily old cannot
# discharge "reporting deletion within a defined cadence".
DEFAULT_MAX_AGE_S = 900

# The distribution files. `_work` is deliberately excluded: it is what survived
# the 2026-08-08 deletion, so treating it as evidence of installation is the
# precise mistake this detector exists to refuse.
INSTALL_MARKERS = ("run.sh", "config.sh", "bin", "externals")
CREDENTIAL_MARKERS = (".credentials", ".credentials_rsaparams", ".runner")

PRESENT = "PRESENT"
ABSENT = "ABSENT"
NO_RESULT = "NO_RESULT"


@dataclass
class Surface:
    name: str
    verdict: str
    detail: str
    evidence: dict = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return self.verdict == PRESENT


def _run(argv: list[str], timeout: int = 20) -> tuple[int, str, str]:
    try:
        p = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout.strip(), p.stderr.strip()
    except FileNotFoundError:
        return 127, "", f"not found: {argv[0]}"
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout}s: {' '.join(argv[:2])}"


def _systemctl_show(service: str, user: bool, props: list[str]) -> tuple[dict, str]:
    argv = ["systemctl"] + (["--user"] if user else []) + ["show", service]
    for p in props:
        argv += ["-p", p]
    rc, out, err = _run(argv)
    if rc != 0 and not out:
        return {}, err or f"systemctl exited {rc}"
    values = {}
    for line in out.splitlines():
        key, _, val = line.partition("=")
        values[key] = val
    return values, ""


def check_installation(root: Path, exec_start: str | None) -> Surface:
    """The distribution, not the directory, and not `_work`."""
    if not root.exists():
        return Surface(
            "installation", ABSENT,
            f"installation root {root} does not exist",
            {"root": str(root)},
        )

    present, missing = [], []
    for marker in INSTALL_MARKERS:
        target = root / marker
        if target.exists() and (target.is_dir() or target.stat().st_size > 0):
            present.append(marker)
        else:
            missing.append(marker)

    survivors = sorted(p.name for p in root.iterdir()) if root.is_dir() else []
    evidence = {
        "root": str(root),
        "markers_present": present,
        "markers_missing": missing,
        "entries_found": survivors[:24],
    }

    if missing:
        # Name the incident shape explicitly when it recurs.
        only_work = set(survivors) <= {"_work", "_diag"}
        detail = f"missing runner distribution: {', '.join(missing)}"
        if only_work:
            detail += " -- only _work/_diag remain, the 2026-08-08 deletion shape"
        return Surface("installation", ABSENT, detail, evidence)

    # 203/EXEC is what the deleted runner actually produced, so the ExecStart
    # target being an executable file is part of THIS surface, not the service's.
    if exec_start:
        exe = Path(exec_start)
        evidence["exec_start"] = str(exe)
        if not exe.exists():
            return Surface(
                "installation", ABSENT,
                f"unit ExecStart target {exe} does not exist (the 203/EXEC shape)",
                evidence,
            )
        if not os.access(exe, os.X_OK):
            return Surface(
                "installation", ABSENT,
                f"unit ExecStart target {exe} is not executable (the 203/EXEC shape)",
                evidence,
            )

    return Surface(
        "installation", PRESENT,
        f"runner distribution present ({len(present)}/{len(INSTALL_MARKERS)} markers)",
        evidence,
    )


def check_credentials(root: Path) -> tuple[Surface, dict]:
    """Credential material present and `.runner` parseable. Never print secrets."""
    evidence: dict = {"root": str(root)}
    missing = []
    for marker in CREDENTIAL_MARKERS:
        target = root / marker
        if not target.exists() or target.stat().st_size == 0:
            missing.append(marker)
    evidence["missing"] = missing
    if missing:
        return (
            Surface("credentials", ABSENT,
                    f"missing or empty credential material: {', '.join(missing)}",
                    evidence),
            {},
        )

    # `.runner` is written UTF-8 WITH BOM by config.sh; a plain utf-8 load raises
    # and would otherwise be misreported as corrupt credentials.
    try:
        with open(root / ".runner", encoding="utf-8-sig") as fh:
            runner = json.load(fh)
    except (OSError, json.JSONDecodeError) as exc:
        evidence["parse_error"] = str(exc)
        return Surface("credentials", ABSENT, f".runner is unreadable: {exc}", evidence), {}

    agent_id = runner.get("agentId")
    agent_name = runner.get("agentName")
    evidence["agent_id"] = agent_id
    evidence["agent_name"] = agent_name
    if agent_id is None or not agent_name:
        return (
            Surface("credentials", ABSENT,
                    ".runner carries no agentId/agentName", evidence),
            {},
        )
    return (
        Surface("credentials", PRESENT,
                f"credential material present; identity {agent_name} (id {agent_id})",
                evidence),
        {"agent_id": agent_id, "agent_name": agent_name},
    )


def check_service(service: str, user: bool) -> tuple[Surface, str | None]:
    """Enabled is NECESSARY AND NOT SUFFICIENT. This is the crux of the incident."""
    props = ["UnitFileState", "ActiveState", "SubState", "Result",
             "ExecMainStatus", "NRestarts", "ExecStart"]
    values, err = _systemctl_show(service, user, props)
    if not values:
        return (
            Surface("service", NO_RESULT,
                    f"could not query {service}: {err}", {"service": service}),
            None,
        )

    exec_start = None
    raw_exec = values.get("ExecStart", "")
    if "path=" in raw_exec:
        exec_start = raw_exec.split("path=", 1)[1].split(";", 1)[0].strip()

    evidence = {
        "service": service,
        "scope": "user" if user else "system",
        "UnitFileState": values.get("UnitFileState", ""),
        "ActiveState": values.get("ActiveState", ""),
        "SubState": values.get("SubState", ""),
        "Result": values.get("Result", ""),
        "ExecMainStatus": values.get("ExecMainStatus", ""),
        "NRestarts": values.get("NRestarts", ""),
        "exec_start": exec_start,
    }

    unit_state = values.get("UnitFileState", "")
    active = values.get("ActiveState", "")
    result = values.get("Result", "")
    exec_status = values.get("ExecMainStatus", "0")

    if unit_state in ("", "not-found"):
        return Surface("service", ABSENT, f"{service} is not installed", evidence), exec_start
    if unit_state != "enabled":
        return (
            Surface("service", ABSENT,
                    f"{service} is {unit_state}, not enabled", evidence),
            exec_start,
        )
    # The 2026-08-08 shape: enabled, and failing. Enabled alone is never green.
    if active == "failed" or result not in ("", "success"):
        return (
            Surface("service", ABSENT,
                    f"{service} is ENABLED but {active}/{result} "
                    f"(exec status {exec_status}) -- enabled is not running",
                    evidence),
            exec_start,
        )
    if active != "active":
        return (
            Surface("service", ABSENT,
                    f"{service} is ENABLED but ActiveState={active}", evidence),
            exec_start,
        )
    try:
        if int(exec_status) != 0:
            return (
                Surface("service", ABSENT,
                        f"{service} is ENABLED and active but last exec status is "
                        f"{exec_status}", evidence),
                exec_start,
            )
    except ValueError:
        pass

    return (
        Surface("service", PRESENT,
                f"{service} enabled and active ({values.get('SubState','')})", evidence),
        exec_start,
    )


def check_registration(repo: str, identity: dict, offline: bool) -> Surface:
    """The recorded agent id is still registered with the repository."""
    evidence = {"repo": repo, "agent_id": identity.get("agent_id"),
                "agent_name": identity.get("agent_name")}
    if offline:
        return Surface("registration", NO_RESULT,
                       "registration not evaluated (--offline): absence is NO_RESULT, "
                       "not an assumed pass", evidence)
    if not identity:
        return Surface("registration", NO_RESULT,
                       "no local identity to look up (credentials surface failed first)",
                       evidence)

    gh = shutil.which("gh")
    if not gh:
        return Surface("registration", NO_RESULT, "gh is not available", evidence)

    argv = [gh, "api", f"repos/{repo}/actions/runners", "--paginate"]
    if shutil.which("with-proxy"):
        argv = ["with-proxy"] + argv
    rc, out, err = _run(argv, timeout=60)
    if rc != 0 or not out:
        evidence["error"] = (err or f"gh exited {rc}")[:300]
        return Surface("registration", NO_RESULT,
                       "could not read the repository runner list", evidence)
    try:
        payload = json.loads(out) if out.lstrip().startswith("{") else None
        runners = payload.get("runners", []) if payload else []
    except json.JSONDecodeError as exc:
        evidence["error"] = str(exc)
        return Surface("registration", NO_RESULT, "runner list was not JSON", evidence)

    evidence["registered_ids"] = sorted(r.get("id") for r in runners if "id" in r)
    for runner in runners:
        if runner.get("id") == identity.get("agent_id"):
            evidence["status"] = runner.get("status")
            evidence["name"] = runner.get("name")
            evidence["labels"] = [l.get("name") for l in runner.get("labels", [])]
            if runner.get("status") != "online":
                return Surface("registration", ABSENT,
                               f"runner id {runner.get('id')} is registered but "
                               f"{runner.get('status')}", evidence)
            return Surface("registration", PRESENT,
                           f"runner id {runner.get('id')} ({runner.get('name')}) is "
                           f"registered and online", evidence)

    return Surface("registration", ABSENT,
                   f"runner id {identity.get('agent_id')} "
                   f"({identity.get('agent_name')}) is NOT registered with {repo}",
                   evidence)


def evaluate(args) -> tuple[str, list[Surface]]:
    root = Path(args.install_root)
    service_surface, exec_start = check_service(args.service, not args.system_service)
    install_surface = check_installation(root, exec_start)
    credential_surface, identity = check_credentials(root)
    registration_surface = check_registration(args.repo, identity, args.offline)

    surfaces = [install_surface, credential_surface, service_surface, registration_surface]
    aggregate = PRESENT if all(s.ok for s in surfaces) else NO_RESULT
    return aggregate, surfaces


def write_record(path: Path, aggregate: str, surfaces: list[Surface]) -> None:
    record = {
        "checked_at": int(time.time()),
        "aggregate": aggregate,
        "surfaces": {s.name: {"verdict": s.verdict, "detail": s.detail} for s in surfaces},
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(record, indent=2) + "\n")


def read_cadence(path: Path, max_age: int) -> tuple[str, str]:
    """A recorded verdict older than the cadence is itself NO_RESULT."""
    if not path.exists():
        return NO_RESULT, f"no presence record at {path}: never checked"
    try:
        record = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        return NO_RESULT, f"presence record at {path} is unreadable: {exc}"
    age = int(time.time()) - int(record.get("checked_at", 0))
    if age > max_age:
        return NO_RESULT, (f"presence record is {age}s old, older than the {max_age}s "
                           f"cadence: stale is NO_RESULT, not PRESENT")
    if record.get("aggregate") != PRESENT:
        return NO_RESULT, f"last recorded verdict was {record.get('aggregate')} ({age}s ago)"
    return PRESENT, f"gate runner confirmed present {age}s ago, within the {max_age}s cadence"


def summary_line(aggregate: str, surfaces: list[Surface]) -> str:
    """The one machine-readable line tick-hub harvests for `{summary}`.

    tick-hub resolves an emit title's placeholders from the gate command's
    stdout `key=value` lines (agent-utils/py/tick_hub/engine.py:64,
    `parse_kv_lines`). This probe printed a human report and no `key=value` line
    at all, so `{summary}` never resolved and the configured action was replaced
    by NO-SIGNAL every cycle -- the probe worked the whole time while the tick
    showed nothing. The value must therefore carry the verdict AND which
    surfaces carry it: a summary that renders but does not report would restore
    emission while leaving the gate just as uninformative.
    """
    failing = [s for s in surfaces if not s.ok]
    if not failing:
        return f"{aggregate}: all {len(surfaces)} surfaces PRESENT"
    named = ", ".join(f"{s.name} {s.verdict}" for s in failing)
    text = f"{aggregate} on {len(failing)} of {len(surfaces)} surfaces -- {named}"
    if failing[0].detail:
        text += f"; first: {failing[0].detail}"
    # One line, whitespace-squeezed: parse_kv_lines is line-oriented, and a
    # title is not truncated downstream the way a detail field is.
    return " ".join(text.split())[:240]


def report_lines(aggregate: str, service: str, install_root, surfaces: list[Surface]) -> list[str]:
    """Every stdout line of the human report, as tick-hub will see them.

    Extracted so the probe/engine contract can be bracketed directly on hostile
    input -- a detail containing `=` -- instead of by duplicating these format
    strings in the test, which would assert the test's copy rather than this one.
    """
    lines = [f"# GATE RUNNER {aggregate} -- {service} @ {install_root}"]
    lines += [f"#   {s.verdict:<9} {s.name:<13} {s.detail}" for s in surfaces]
    if aggregate != PRESENT:
        failed = ", ".join(s.name for s in surfaces if not s.ok)
        lines.append(f"#   admission must treat this as NO_RESULT (surfaces: {failed})")
    lines.append(f"summary={summary_line(aggregate, surfaces)}")
    return lines


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--install-root", default=DEFAULT_INSTALL_ROOT)
    ap.add_argument("--service", default=DEFAULT_SERVICE)
    ap.add_argument("--system-service", action="store_true",
                    help="query the system manager instead of --user")
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--offline", action="store_true",
                    help="skip the GitHub surface; it becomes NO_RESULT, never a pass")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--record", help="write the verdict to this path for cadence checks")
    ap.add_argument("--admission-check", action="store_true",
                    help="read a recorded verdict and fail closed if stale or absent")
    ap.add_argument("--max-age", type=int, default=DEFAULT_MAX_AGE_S)
    args = ap.parse_args()

    if args.admission_check:
        if not args.record:
            print("--admission-check requires --record", file=sys.stderr)
            return 3
        verdict, detail = read_cadence(Path(args.record), args.max_age)
        payload = {"aggregate": verdict, "detail": detail, "mode": "admission"}
        if args.json:
            print(json.dumps(payload, indent=2))
        else:
            # Same contract as the full path below: report `#`-prefixed so it
            # cannot be harvested, verdict carried on one `summary=` line.
            print(f"# GATE RUNNER {verdict}: {detail}")
            print(f"summary={' '.join(f'{verdict}: {detail}'.split())[:240]}")
        return 0 if verdict == PRESENT else 2

    aggregate, surfaces = evaluate(args)

    if args.json:
        print(json.dumps({"aggregate": aggregate,
                          "surfaces": [asdict(s) for s in surfaces]}, indent=2))
    else:
        # The human report is `#`-prefixed because tick-hub harvests EVERY
        # unprefixed stdout line containing `=` into a field, splitting on the
        # FIRST `=`. A free-text detail such as "... is ENABLED but
        # ActiveState=inactive" would otherwise become a field whose key is the
        # whole leading sentence. Marking the report as comment lines -- which
        # parse_kv_lines documents as ignored -- makes the boundary structural,
        # so no future detail string can leak a field by containing `=`.
        for line in report_lines(aggregate, args.service, args.install_root, surfaces):
            print(line)

    if args.record:
        write_record(Path(args.record), aggregate, surfaces)

    return 0 if aggregate == PRESENT else 2


if __name__ == "__main__":
    sys.exit(main())
