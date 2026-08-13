#!/usr/bin/env python3
"""Report the expected gate runner from repository and host-local evidence.

Certification condition 9. On 2026-08-08 the gate-runner installation
and credentials were deleted; the user service stayed **enabled** and failed
203/EXEC for roughly two days without reporting the deletion, and the shortfall
presented as CI saturation. The runner was later restored and executed gate jobs
again -- but recovery is not a detector.

The repository runner list answers whether the expected runner is registered and
online.  Host-local evidence is evaluated only when this host carries a local
identity, installation, or service trace.  Otherwise a healthy expected runner
is reported as living on another host; missing local files are not evidence that
the remote runner is absent.

When there is local evidence, the load-bearing design constraint comes straight
from the incident: the service surface said `enabled` the whole time, and only
`_work` survived under the installation root. A monitor that checks service
state, or that treats "the directory exists" as installed, reproduces the
two-day blindness exactly. So the four host-local surfaces are checked
**separately** and reported **separately**:

1. `installation` -- the runner distribution is present AND the unit's ExecStart
   target is an executable file. `_work` alone is ABSENT, by name.
2. `credentials`  -- `.credentials`, `.credentials_rsaparams` and `.runner` are
   present, non-empty, and `.runner` parses (it is written UTF-8 **with BOM**).
3. `service`      -- enabled is necessary and NOT sufficient: an enabled unit
   that is failed, or whose last exec status is nonzero, is RED.
4. `registration` -- the agent id recorded in `.runner` is still registered with
   the repository, with its reported status.

Absence is NO_RESULT, never silence.  A missing or offline expected repository
registration always alerts.  On a host with local traces, any surface that
cannot be positively confirmed makes the aggregate NO_RESULT.  A separate note
path reports the truthful non-owning-host state and Actions-disabled state; it
does not turn either state into a runner failure.

Exit codes:
  0  selected gate condition is false (or the default aggregate is PRESENT)
  2  selected --gate-on condition is true (or default aggregate is NO_RESULT)
  3  usage/internal error
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field, asdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_INSTALL_ROOT = str(REPO_ROOT / "ci-runner" / "state-gate")
DEFAULT_SERVICE = "hermit-gate-runner.service"
DEFAULT_REPO = "rrnewton/hermit"
DEFAULT_RUNNER_NAME = "hermit-gate-newton"

sys.path.insert(0, str(Path(__file__).resolve().parent))
from unevaluated_head import OWNER_DISABLED_ACTIONS  # noqa: E402

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


@dataclass(frozen=True)
class ActionsState:
    enabled: bool | None
    directive: str | None = None
    queued_merge_gate_runs: int = 0
    zero_job_runs: int = 0
    admitted_job_runs: int = 0
    detail: str = ""


@dataclass(frozen=True)
class Evaluation:
    aggregate: str
    state: str
    summary: str
    surfaces: list[Surface]
    alert: bool
    notice: bool
    actions: ActionsState


def _run(argv: list[str], timeout: int = 20) -> tuple[int, str, str]:
    try:
        p = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout.strip(), p.stderr.strip()
    except FileNotFoundError:
        return 127, "", f"not found: {argv[0]}"
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout}s: {' '.join(argv[:2])}"


def _gh_api(repo: str, endpoint: str) -> tuple[object | None, str]:
    gh = shutil.which("gh")
    if not gh:
        return None, "gh is not available"
    argv = [gh, "api", f"repos/{repo}/{endpoint}"]
    if shutil.which("with-proxy"):
        argv = ["with-proxy"] + argv
    rc, out, err = _run(argv, timeout=60)
    if rc != 0 or not out:
        return None, (err or f"gh exited {rc}")[:300]
    try:
        return json.loads(out), ""
    except json.JSONDecodeError as exc:
        return None, f"GitHub response was not JSON: {exc}"


def repository_runners(repo: str, offline: bool) -> tuple[list[dict] | None, str]:
    if offline:
        return None, "runner inventory not evaluated (--offline)"
    payload, error = _gh_api(repo, "actions/runners?per_page=100")
    if not isinstance(payload, dict) or not isinstance(payload.get("runners"), list):
        return None, error or "repository runner list has no runners array"
    return [runner for runner in payload["runners"] if isinstance(runner, dict)], ""


def check_actions(repo: str, offline: bool) -> ActionsState:
    if offline:
        return ActionsState(None, detail="Actions policy not evaluated (--offline)")
    payload, error = _gh_api(repo, "actions/permissions")
    if not isinstance(payload, dict) or type(payload.get("enabled")) is not bool:
        return ActionsState(None, detail=error or "Actions permission has no enabled field")
    enabled = payload["enabled"]
    if enabled:
        return ActionsState(True)

    directive = OWNER_DISABLED_ACTIONS.get(repo)
    runs_payload, runs_error = _gh_api(repo, "actions/runs?status=queued&per_page=100")
    if not isinstance(runs_payload, dict) or not isinstance(runs_payload.get("workflow_runs"), list):
        return ActionsState(False, directive=directive,
                            detail=runs_error or "queued workflow list unavailable")
    merge_gate_runs = [
        run for run in runs_payload["workflow_runs"]
        if isinstance(run, dict) and run.get("name") == "Merge Gate"
        and isinstance(run.get("id"), int)
    ]
    zero_jobs = 0
    admitted = 0
    failures: list[str] = []
    for run in merge_gate_runs:
        jobs, jobs_error = _gh_api(
            repo, f"actions/runs/{run['id']}/jobs?filter=all&per_page=1"
        )
        if not isinstance(jobs, dict) or not isinstance(jobs.get("total_count"), int):
            failures.append(str(run["id"]))
        elif jobs["total_count"] == 0:
            zero_jobs += 1
        else:
            admitted += 1
    detail = ""
    if failures:
        detail = "job count unavailable for queued run(s) " + ",".join(failures)
    return ActionsState(
        False,
        directive=directive,
        queued_merge_gate_runs=len(merge_gate_runs),
        zero_job_runs=zero_jobs,
        admitted_job_runs=admitted,
        detail=detail,
    )


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


def check_registration(
    repo: str,
    identity: dict,
    offline: bool,
    runners: list[dict] | None = None,
    runners_error: str = "",
) -> Surface:
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

    if runners is None:
        runners, runners_error = repository_runners(repo, offline)
    if runners is None:
        evidence["error"] = runners_error
        return Surface("registration", NO_RESULT,
                       "could not read the repository runner list", evidence)

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


def expected_registration(
    repo: str,
    runner_name: str,
    runners: list[dict] | None,
    runners_error: str,
) -> Surface:
    evidence = {"repo": repo, "runner_name": runner_name}
    if runners is None:
        evidence["error"] = runners_error
        return Surface(
            "registration", NO_RESULT,
            f"could not read repository runner list: {runners_error}", evidence,
        )
    for runner in runners:
        if runner.get("name") != runner_name:
            continue
        labels = [
            label.get("name") for label in runner.get("labels", [])
            if isinstance(label, dict)
        ]
        evidence.update({"agent_id": runner.get("id"), "status": runner.get("status"),
                         "labels": labels})
        if "gate" not in labels:
            return Surface(
                "registration", ABSENT,
                f"expected runner {runner_name} is registered without the gate label",
                evidence,
            )
        if runner.get("status") != "online":
            return Surface(
                "registration", ABSENT,
                f"expected runner {runner_name} (id {runner.get('id')}) is registered but "
                f"{runner.get('status')}", evidence,
            )
        return Surface(
            "registration", PRESENT,
            f"expected runner {runner_name} (id {runner.get('id')}) is registered online",
            evidence,
        )
    return Surface(
        "registration", ABSENT,
        f"expected runner {runner_name} is not registered with {repo}", evidence,
    )


def actions_summary(actions: ActionsState) -> str:
    if actions.enabled is True:
        return "repository Actions are enabled"
    if actions.enabled is None:
        return f"Actions state is NO_RESULT: {actions.detail}"
    directive = f" by owner directive {actions.directive}" if actions.directive else ""
    if actions.queued_merge_gate_runs == 0:
        queue = "no queued Merge Gate runs"
    else:
        queue = (
            f"queued Merge Gate runs={actions.queued_merge_gate_runs}; "
            f"zero admitted jobs={actions.zero_job_runs}"
        )
        if actions.admitted_job_runs:
            queue += f"; runs with admitted jobs={actions.admitted_job_runs}"
    if actions.detail:
        queue += f"; {actions.detail}"
    expected = (
        "; zero-job queued runs are expected while disabled, neither pass nor failure"
        if actions.directive and actions.zero_job_runs else ""
    )
    return f"repository Actions are disabled{directive}; {queue}{expected}"


def evaluate(args) -> Evaluation:
    root = Path(args.install_root)
    service_surface, exec_start = check_service(args.service, not args.system_service)
    install_surface = check_installation(root, exec_start)
    credential_surface, identity = check_credentials(root)
    runners, runners_error = repository_runners(args.repo, args.offline)
    expected_surface = expected_registration(
        args.repo, args.runner_name, runners, runners_error
    )
    actions = check_actions(args.repo, args.offline)

    service_state = service_surface.evidence.get("UnitFileState")
    local_trace = bool(identity) or root.exists() or service_state not in (None, "", "not-found")
    host = socket.gethostname().split(".", 1)[0]

    if expected_surface.ok and not local_trace:
        summary = (
            f"PRESENT: {expected_surface.detail}; host {host} carries no local "
            "gate-runner identity, installation root, or installed service, so the four "
            f"host-local surfaces do not apply here; {actions_summary(actions)}"
        )
        return Evaluation(
            PRESENT, "non-owning-host", summary, [expected_surface],
            alert=False,
            notice=True,
            actions=actions,
        )

    if local_trace:
        registration_surface = check_registration(
            args.repo, identity, args.offline, runners, runners_error
        )
        surfaces = [
            install_surface, credential_surface, service_surface, registration_surface
        ]
        aggregate = PRESENT if all(surface.ok for surface in surfaces) else NO_RESULT
        summary = render_summary(aggregate, surfaces) + "; " + actions_summary(actions)
        return Evaluation(
            aggregate,
            "host-local-PRESENT" if aggregate == PRESENT else "host-local-NO_RESULT",
            summary,
            surfaces,
            alert=aggregate != PRESENT or not expected_surface.ok,
            notice=actions.enabled is not True,
            actions=actions,
        )

    summary = (
        f"NO_RESULT: {expected_surface.detail}; host {host} also carries no local "
        "gate-runner identity, installation root, or installed service, so the owning "
        f"host cannot be inferred; {actions_summary(actions)}"
    )
    return Evaluation(
        NO_RESULT, "registration-NO_RESULT", summary, [expected_surface],
        alert=True,
        notice=actions.enabled is not True,
        actions=actions,
    )


def render_summary(aggregate: str, surfaces: list[Surface]) -> str:
    """Render the single-line value consumed by tick-hub's ``{summary}``."""
    failed = [surface for surface in surfaces if not surface.ok]
    if aggregate == PRESENT:
        if failed:
            raise ValueError("PRESENT aggregate contains a non-PRESENT surface")
        return "PRESENT: installation, credentials, service, and registration confirmed"
    if not failed:
        raise ValueError(f"{aggregate} aggregate contains no non-PRESENT surface")
    findings = "; ".join(
        f"{surface.name}={surface.verdict}: {' '.join(surface.detail.split())}"
        for surface in failed
    )
    return f"{aggregate}: {findings}"


def write_record(path: Path, evaluation: Evaluation) -> None:
    record = {
        "checked_at": int(time.time()),
        "aggregate": evaluation.aggregate,
        "state": evaluation.state,
        "summary": evaluation.summary,
        "surfaces": {
            surface.name: {"verdict": surface.verdict, "detail": surface.detail}
            for surface in evaluation.surfaces
        },
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


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--install-root", default=DEFAULT_INSTALL_ROOT)
    ap.add_argument("--service", default=DEFAULT_SERVICE)
    ap.add_argument("--system-service", action="store_true",
                    help="query the system manager instead of --user")
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--runner-name", default=DEFAULT_RUNNER_NAME)
    ap.add_argument("--offline", action="store_true",
                    help="skip the GitHub surface; it becomes NO_RESULT, never a pass")
    ap.add_argument(
        "--gate-on", choices=("alert", "notice"),
        help="exit nonzero only for the selected tick-hub emission path",
    )
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
        print(json.dumps(payload, indent=2) if args.json
              else f"GATE RUNNER {verdict}: {detail}")
        return 0 if verdict == PRESENT else 2

    evaluation = evaluate(args)

    if args.json:
        print(json.dumps({
            "aggregate": evaluation.aggregate,
            "state": evaluation.state,
            "summary": evaluation.summary,
            "alert": evaluation.alert,
            "notice": evaluation.notice,
            "actions": asdict(evaluation.actions),
            "surfaces": [asdict(surface) for surface in evaluation.surfaces],
        }, indent=2))
    else:
        print(
            f"GATE RUNNER {evaluation.aggregate} state={evaluation.state} -- "
            f"{args.service} @ {args.install_root}"
        )
        for surface in evaluation.surfaces:
            print(f"  {surface.verdict:<9} {surface.name:<13} {surface.detail}")
        if evaluation.aggregate != PRESENT:
            failed = ", ".join(
                surface.name for surface in evaluation.surfaces if not surface.ok
            )
            print(f"  admission must treat this as NO_RESULT (surfaces: {failed})")
        # This command is the captured producer for gate_runner_presence.  Keep
        # the detailed human report above, and also publish the field its
        # reminder template requires.  Never substitute an empty value: an
        # absent field remains a loud, fail-closed tick-hub NO-SIGNAL.
        print(f"state={evaluation.state}")
        print(f"summary={evaluation.summary}")

    if args.record:
        write_record(Path(args.record), evaluation)

    if args.gate_on == "alert":
        return 2 if evaluation.alert else 0
    if args.gate_on == "notice":
        return 2 if evaluation.notice else 0
    return 0 if evaluation.aggregate == PRESENT else 2


if __name__ == "__main__":
    sys.exit(main())
