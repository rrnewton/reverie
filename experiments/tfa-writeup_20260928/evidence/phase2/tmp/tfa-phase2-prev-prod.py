# (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

from __future__ import annotations

import base64
import errno
import functools
import hashlib
import json
import os
import pwd
import re
import select
import shlex
import shutil
import signal
import stat
import subprocess
import tempfile
import time
import unittest
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass, replace
from pathlib import Path
from typing import ParamSpec, TypeVar
from unittest.mock import Mock, patch

import testinfra.tfa.hermit as hermit_module
import testinfra.tfa.manifest as manifest_module
import testinfra.tfa.pipeline as pipeline_module
from security.frameworks.python.exec.subprocess import TrustedSubprocessWithList
from testinfra.tfa.hermit import (
    controller_evidence,
    HermitError,
    safehermit_evidence,
    safehermit_runtime_paths,
)
from testinfra.tfa.manifest import Case, load_manifest, Manifest
from testinfra.tfa.orchestrator import run_investigation
from testinfra.tfa.pipeline import (
    _apply_candidate_locked,
    _diagnose_candidate_locked,
    _recover_candidate_transaction,
    _reject_active_candidate,
    diagnose_candidate,
)
from testinfra.tfa.process import (
    _pinned_safehermit_allowed,
    codex_arguments,
    codex_review_arguments,
    CONTAINMENT,
    diagnostic_environment,
    ProcessError,
    publish_codex_report,
    run_process,
    session_id_from_jsonl,
)
from testinfra.tfa.report import CandidateSummary, seal_candidate_report
from testinfra.tfa.state import (
    checkout_lease,
    Lease,
    Ledger,
    seal_working_directory,
    Stage,
    StateError,
    verify_working_directory,
)
from testinfra.tfa.tests.test_process_migration_helper import _production_probe


def _owned_process_cgroups() -> set[Path]:
    relative = Path("/proc/self/cgroup").read_text().strip().removeprefix("0::/")
    parent = Path("/sys/fs/cgroup") / relative
    prefix = f"tfa-{os.getpid()}-"
    return {path for path in parent.iterdir() if path.name.startswith(prefix)}


def _hermit_binary() -> str:
    return str(Path(__file__).with_name("hermit-test-bin").resolve(strict=True))


_HERMIT_TEST_LOCK = Path("/tmp/tfa-phase2/production-hermit-namespace.json")
_HERMIT_TEST_SCHEDULER_LOCK = Path(
    "/tmp/tfa-phase2/production-hermit-test-scheduler.json"
)
_HANDOFF_FAILURE_ROOT = Path("/tmp/tfa-phase2/production-test-failures")
_WORKTREE_SOURCE_ENV = "TFA_TEST_WORKTREE_SOURCE_ROOT"
_VERIFY_SPACE_ROOT_ENV = "TFA_TEST_VERIFY_SPACE_ROOT"
_PROC_ROOT = Path("/proc")
_CHECKOUT_SERVICE_ARGUMENTS = (
    ("/usr/local/bin/hg_monitor", "--hgmonitortask-path"),
    ("/usr/local/bin/chataccd", "--mount-point"),
)
_P = ParamSpec("_P")
_R = TypeVar("_R")


@dataclass(frozen=True)
class _CheckoutServiceIdentity:
    pid: int
    descriptor: int
    uid: int
    start_time: int
    arguments: tuple[bytes, ...]
    cgroup: str
    marker: str
    checkout: bytes


def _write_controller_candidate(
    case_dir: Path, case_id: str, attempt_number: int, patch_text: str
) -> tuple[Path, CandidateSummary]:
    output = case_dir / "agent-output"
    output.mkdir(parents=True, exist_ok=True)
    patch_path = output / "patch.diff"
    patch_path.write_text(patch_text)
    hypotheses = [
        {
            "rank": 1,
            "claim": "controller transaction",
            "supporting": "candidate bytes",
            "contradicting": "none",
            "next_experiment": "controller apply",
        },
        {
            "rank": 2,
            "claim": "no source change",
            "supporting": "baseline",
            "contradicting": "candidate patch",
            "next_experiment": "controller reject",
        },
    ]
    (output / "hypotheses.json").write_text(json.dumps(hypotheses))
    validations = [
        {
            "kind": kind,
            "argv": ["controller-only"],
            "status": "FAIL",
            "passed": 0,
            "total": 1,
            "duration_seconds": 0,
            "artifacts": [],
        }
        for kind in ("ORIGINAL_FAIL", "ORIGINAL_PASS", "FRESH_STRESS")
    ]
    (output / "validation.json").write_text(json.dumps(validations))
    report = output / "report.json"
    report.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "qualification": "candidate-only",
                "case_id": case_id,
                "reproduction": {
                    "pass_reproduced": True,
                    "fail_reproduced": True,
                },
                "hypotheses": hypotheses,
                "selected_hypothesis_rank": 1,
                "root_cause": "Controller transaction test invariant.",
                "patch_path": str(patch_path),
                "original_fail_now_passes": False,
                "original_pass_still_passes": False,
                "validations": validations,
            }
        )
    )
    diagnostic = case_dir / "control" / "diagnostic-agent"
    diagnostic.mkdir(parents=True, exist_ok=True)
    (diagnostic / "final-message.json").write_text(report.read_text())
    attempt = (
        case_dir / "control" / "candidate-attempts" / f"attempt-{attempt_number:03d}"
    )
    attempt.mkdir(parents=True)
    candidate = seal_candidate_report(report, attempt / "candidate-seal.json", case_id)
    approved = attempt / "approved-patch.diff"
    approved.write_text(patch_text)
    approved.chmod(0o400)
    (attempt / "candidate-approval.json").write_text("{}\n")
    (attempt / "pre-apply-review.json").write_text("{}\n")
    return attempt, candidate


def _synthetic_candidate_approval(
    _case: Case,
    _case_dir: Path,
    _checkout: Path,
    attempt: Path,
    *_rest: object,
) -> tuple[Path, tuple[str, ...]]:
    patch_path = attempt / "approved-patch.diff"
    if not patch_path.is_file():
        patch_path.write_bytes((_case_dir / "agent-output" / "patch.diff").read_bytes())
        patch_path.chmod(0o400)
        (attempt / "candidate-approval.json").write_text("{}\n")
        (attempt / "pre-apply-review.json").write_text("{}\n")
    return patch_path, pipeline_module._candidate_patch_paths(patch_path)


def _serialized_nested_hermit(function: Callable[_P, _R]) -> Callable[_P, _R]:
    @functools.wraps(function)
    def locked(*args: _P.args, **kwargs: _P.kwargs) -> _R:
        with (
            Lease(_HERMIT_TEST_SCHEDULER_LOCK, wait=True),
            _disposable_btrfs_verify_root(),
        ):
            return function(*args, **kwargs)

    return locked


def _fixture_command(executable: str, arguments: list[str]) -> None:
    result = TrustedSubprocessWithList.run(
        executable=executable,
        cmd_args=arguments,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        raise AssertionError(
            f"TFA integration fixture command failed: {executable} "
            f"{arguments!r}; stdout={result.stdout!r}; stderr={result.stderr!r}"
        )


@contextmanager
def _disposable_btrfs_verify_root() -> Iterator[Path]:
    Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="tfa-production-btrfs-", dir="/tmp/tfa-phase2"))
    image = root / "verify.btrfs"
    mount = root / "mount"
    mount.mkdir()
    with image.open("wb") as output:
        output.truncate(128 * 1024 * 1024)
    mounted = False
    prior = os.environ.get(_VERIFY_SPACE_ROOT_ENV)
    try:
        _fixture_command("/usr/sbin/mkfs.btrfs", ["-q", "-f", str(image)])
        _fixture_command(
            "/usr/bin/sudo", ["-n", "mount", "-o", "loop", str(image), str(mount)]
        )
        mounted = True
        _fixture_command(
            "/usr/bin/sudo", ["-n", "btrfs", "quota", "enable", str(mount)]
        )
        _fixture_command(
            "/usr/bin/sudo",
            ["-n", "chown", f"{os.getuid()}:{os.getgid()}", str(mount)],
        )
        verify_root = mount / "safehermit"
        verify_root.mkdir(mode=0o700)
        os.environ[_VERIFY_SPACE_ROOT_ENV] = str(verify_root)
        yield verify_root
    finally:
        if prior is None:
            os.environ.pop(_VERIFY_SPACE_ROOT_ENV, None)
        else:
            os.environ[_VERIFY_SPACE_ROOT_ENV] = prior
        if mounted:
            _fixture_command("/usr/bin/sudo", ["-n", "umount", str(mount)])
        shutil.rmtree(root)


def _serialized_controller_handoff(function: Callable[_P, _R]) -> Callable[_P, _R]:
    @functools.wraps(function)
    def locked(*args: _P.args, **kwargs: _P.kwargs) -> _R:
        with _disposable_worktree_source() as source_root:
            with Lease(source_root.parent / "controller-handoff.lock", wait=True):
                _reconcile_worktree_journals()
                return function(*args, **kwargs)

    return locked


def _worktree_source_root() -> Path:
    value = os.environ.get(_WORKTREE_SOURCE_ENV)
    if value is None:
        raise AssertionError("disposable TFA worktree source is not configured")
    path = Path(value)
    if not path.is_absolute():
        raise AssertionError("disposable TFA worktree source is not absolute")
    return path.resolve()


@contextmanager
def _disposable_worktree_source() -> Iterator[Path]:
    Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
    root = Path(
        tempfile.mkdtemp(prefix="tfa-production-source-", dir="/tmp/tfa-phase2")
    )
    source = root / "source"
    repository = Path.cwd().resolve()
    revision = TrustedSubprocessWithList.run(
        executable="/usr/local/bin/sl",
        cmd_args=[
            "log",
            "-r",
            ".",
            "-T",
            "{node}\\n",
            "--reason",
            "resolve disposable TFA fixture revision - sl help log",
        ],
        cwd=repository,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if revision.returncode != 0 or not re.fullmatch(
        r"[0-9a-f]{40}\n?", revision.stdout
    ):
        shutil.rmtree(root)
        raise AssertionError(
            "TFA worktree integration requires a local Sapling source checkout: "
            f"cwd={repository}; returncode={revision.returncode}; "
            f"stdout={revision.stdout!r}; stderr={revision.stderr!r}"
        )
    cloned = TrustedSubprocessWithList.run(
        executable="/usr/local/bin/eden",
        cmd_args=[
            "clone",
            "--rev",
            revision.stdout.strip(),
            str(repository),
            str(source),
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if cloned.returncode != 0:
        shutil.rmtree(root)
        raise AssertionError(
            "TFA worktree integration requires a disposable Eden checkout: "
            + cloned.stderr
        )
    prior = os.environ.get(_WORKTREE_SOURCE_ENV)
    os.environ[_WORKTREE_SOURCE_ENV] = str(source)
    try:
        yield source
    finally:
        if prior is None:
            os.environ.pop(_WORKTREE_SOURCE_ENV, None)
        else:
            os.environ[_WORKTREE_SOURCE_ENV] = prior
        removed = TrustedSubprocessWithList.run(
            executable="/usr/local/bin/eden",
            cmd_args=["remove", "--yes", str(source)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        if removed.returncode != 0:
            raise AssertionError(
                "cannot retire disposable TFA source checkout: " + removed.stderr
            )
        if source in _eden_mounts():
            raise AssertionError("disposable TFA source checkout remains mounted")
        shutil.rmtree(root)


def _preserve_controller_handoff_failure(
    case_dir: Path,
    failure_root: Path = _HANDOFF_FAILURE_ROOT,
) -> Path:
    failure = failure_root / f"controller-handoff-{time.time_ns()}"
    failure.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(case_dir, failure)
    return failure


@contextmanager
def _preserving_production_directory(
    prefix: str,
    failure_label: str,
    failure_root: Path = _HANDOFF_FAILURE_ROOT,
) -> Iterator[Path]:
    root = Path(tempfile.mkdtemp(prefix=prefix, dir="/tmp/tfa-phase2"))
    try:
        yield root
    except Exception as error:
        failure = _preserve_controller_handoff_failure(
            root,
            failure_root / failure_label,
        )
        raise AssertionError(
            f"production evidence preserved at {failure}; retained root: {root}"
        ) from error
    else:
        shutil.rmtree(root)


def _report_redirect_prompt(marker: Path, report: Path, external: Path) -> str:
    source = "\n".join(
        (
            "import json, pathlib, secrets",
            f"marker = pathlib.Path({str(marker)!r})",
            f"report = pathlib.Path({str(report)!r})",
            f"external = pathlib.Path({str(external)!r})",
            "nonce = secrets.token_hex(24)",
            "marker.write_text(nonce)",
            "try:",
            "    report.unlink(missing_ok=True)",
            "    report.symlink_to(external)",
            "    link_result = 'symlink-succeeded'",
            "except OSError as error:",
            "    link_result = f'errno:{error.errno}'",
            "print(json.dumps({",
            "    'nonce': nonce,",
            "    'report_link': link_result,",
            "}, sort_keys=True))",
        )
    )
    command = f"/usr/local/bin/fbpython -c {shlex.quote(source)}"
    return (
        "Run this exact command with the shell tool exactly once. "
        "Return only its exact JSON object:\n"
        f"{command}"
    )


def _controller_handoff_source(
    agent_output: Path,
    forbidden: Path,
    probe_path: str,
    case_id: str,
) -> str:
    return "\n".join(
        (
            "import json, pathlib, secrets, socket",
            f"output = pathlib.Path({str(agent_output)!r})",
            f"forbidden = pathlib.Path({str(forbidden)!r})",
            "patch_path = output / 'patch.diff'",
            "nonce = secrets.token_hex(24)",
            "try:",
            "    forbidden.write_text(nonce)",
            "    checkout_write = 'succeeded'",
            "except OSError as error:",
            "    checkout_write = f'errno:{error.errno}'",
            "try:",
            "    socket.socket(socket.AF_INET, socket.SOCK_STREAM)",
            "    network = 'succeeded'",
            "except OSError as error:",
            "    network = f'errno:{error.errno}'",
            f"probe_path = {probe_path!r}",
            "patch_path.write_text(",
            ("    'diff --git a/' + probe_path + ' b/' + probe_path + '\\n'"),
            (
                "    'new file mode 100644\\n--- /dev/null\\n'"
                "    '+++ b/' + probe_path + '\\n'"
            ),
            "    '@@ -0,0 +1 @@\\n+' + nonce + '\\n'",
            ")",
            "hypotheses = [",
            "    {'rank': 1, 'claim': 'handoff', 'supporting': nonce,",
            "     'contradicting': 'none', 'next_experiment': 'apply'},",
            "    {'rank': 2, 'claim': 'direct edit',",
            "     'supporting': checkout_write + ',' + network,",
            "     'contradicting': 'sandbox',",
            "     'next_experiment': 'controller'},",
            "]",
            "hypotheses_path = output / 'hypotheses.json'",
            "hypotheses_path.write_text(json.dumps(hypotheses))",
            "proposed = [",
            "    {'kind': kind, 'argv': ['controller-only'],",
            "     'status': 'FAIL',",
            "     'passed': 0, 'total': total, 'duration_seconds': 0,",
            "     'artifacts': []}",
            "    for kind, total in (('ORIGINAL_FAIL', 1),",
            "        ('ORIGINAL_PASS', 1), ('FRESH_STRESS', 1))",
            "]",
            "(output / 'validation.json').write_text(json.dumps(proposed))",
            "reproduction = {'pass_reproduced': True,",
            "                'fail_reproduced': True}",
            "print(json.dumps({",
            "    'schema_version': 1, 'qualification': 'candidate-only',",
            f"    'case_id': {case_id!r},",
            "    'reproduction': reproduction,",
            "    'hypotheses': hypotheses, 'selected_hypothesis_rank': 1,",
            "    'root_cause': 'Controller-only validation handoff probe.',",
            "    'patch_path': str(patch_path),",
            "    'original_fail_now_passes': False,",
            "    'original_pass_still_passes': False,",
            "    'validations': proposed,",
            "}, sort_keys=True))",
        )
    )


def _controller_handoff_command(source: str) -> str:
    payload = base64.b64encode(source.encode()).decode("ascii")
    loader = (
        "import base64; "
        f"exec(compile(base64.b64decode({payload!r}), "
        "'<tfa-controller-handoff>', 'exec'))"
    )
    return f"/usr/local/bin/fbpython -c {shlex.quote(loader)}"


def _assert_process_cleanup(process_dir: Path) -> None:
    metadata = json.loads((process_dir / "metadata.json").read_text())
    if not metadata["containment_empty"] or Path(metadata["cgroup"]).exists():
        raise AssertionError("production process containment was not cleaned")


def _bounded_command_metadata(attempt: Path) -> tuple[dict[str, object], ...]:
    control = attempt / "bounded-space-control"
    return tuple(
        json.loads((entry / "metadata.json").read_text())
        for entry in sorted(control.iterdir())
        if entry.is_dir()
    )


def _processes_referencing(
    path: Path,
    proc_root: Path = _PROC_ROOT,
) -> tuple[tuple[int, tuple[str, ...], tuple[str, ...]], ...]:
    canonical = os.fsencode(os.path.abspath(path))
    matches: list[tuple[int, tuple[str, ...], tuple[str, ...]]] = []
    for entry in proc_root.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            arguments = (entry / "cmdline").read_bytes().split(b"\0")
        except OSError:
            arguments = []
        references: list[str] = []
        for argument in arguments:
            value = argument
            if argument.startswith(b"-") and b"=" in argument:
                value = argument.split(b"=", 1)[1]
            if value == canonical or value.startswith(canonical + b"/"):
                references.append(f"argv:{os.fsdecode(argument)}")
        for name in ("cwd", "root"):
            try:
                target = os.fsencode(os.readlink(entry / name))
            except OSError:
                continue
            normalized = target.removesuffix(b" (deleted)")
            if normalized == canonical or normalized.startswith(canonical + b"/"):
                references.append(f"{name}:{os.fsdecode(target)}")
        try:
            descriptors = tuple((entry / "fd").iterdir())
        except OSError:
            descriptors = ()
        for descriptor in descriptors:
            try:
                target = os.fsencode(os.readlink(descriptor))
            except OSError:
                continue
            normalized = target.removesuffix(b" (deleted)")
            if normalized == canonical or normalized.startswith(canonical + b"/"):
                references.append(f"fd:{descriptor.name}:{os.fsdecode(target)}")
        if references:
            matches.append(
                (
                    int(entry.name),
                    tuple(os.fsdecode(argument) for argument in arguments if argument),
                    tuple(references),
                )
            )
    return tuple(sorted(matches))


def _argument_value(arguments: tuple[bytes, ...], flag: bytes) -> bytes | None:
    for index, argument in enumerate(arguments):
        if argument == flag and index + 1 < len(arguments):
            return arguments[index + 1]
        prefix = flag + b"="
        if argument.startswith(prefix):
            return argument.removeprefix(prefix)
    return None


def _pidfd_exited(descriptor: int) -> bool:
    poller = select.poll()
    poller.register(descriptor, select.POLLIN)
    return bool(poller.poll(0))


def _process_start_time(value: str) -> int:
    closing = value.rfind(")")
    if closing < 0:
        raise ValueError("process stat has no command terminator")
    fields = value[closing + 2 :].split()
    if len(fields) < 20:
        raise ValueError("process stat is truncated")
    return int(fields[19])


def _read_checkout_service_identity(
    pid: int,
    descriptor: int,
    marker: str,
    checkout: bytes,
    proc_root: Path = _PROC_ROOT,
) -> _CheckoutServiceIdentity | None:
    if _pidfd_exited(descriptor):
        return None
    process = proc_root / str(pid)
    try:
        before = _process_start_time((process / "stat").read_text())
        status = (process / "status").read_text()
        arguments = tuple((process / "cmdline").read_bytes().split(b"\0"))
        cgroups = tuple(
            line.removeprefix("0::")
            for line in (process / "cgroup").read_text().splitlines()
            if line.startswith("0::/")
        )
        after = _process_start_time((process / "stat").read_text())
    except (OSError, ValueError):
        return None
    uid_line = next(
        (line for line in status.splitlines() if line.startswith("Uid:")), None
    )
    if uid_line is None or before != after or len(cgroups) != 1:
        return None
    uids = tuple(int(value) for value in uid_line.split()[1:])
    marker_bytes = os.fsencode(marker)
    flag = os.fsencode(dict(_CHECKOUT_SERVICE_ARGUMENTS)[marker])
    if (
        not uids
        or any(uid != os.getuid() for uid in uids)
        or marker_bytes not in arguments
        or _argument_value(arguments, flag) != checkout
        or _pidfd_exited(descriptor)
    ):
        return None
    return _CheckoutServiceIdentity(
        pid,
        descriptor,
        uids[0],
        before,
        arguments,
        cgroups[0],
        marker,
        checkout,
    )


def _open_checkout_services(
    path: Path,
    proc_root: Path = _PROC_ROOT,
) -> tuple[_CheckoutServiceIdentity, ...]:
    checkout = os.fsencode(os.path.abspath(path))
    matches: list[_CheckoutServiceIdentity] = []
    for entry in proc_root.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            descriptor = os.pidfd_open(int(entry.name))
        except (OSError, ValueError):
            continue
        identity = None
        for marker, _flag in _CHECKOUT_SERVICE_ARGUMENTS:
            identity = _read_checkout_service_identity(
                int(entry.name), descriptor, marker, checkout, proc_root
            )
            if identity is not None:
                break
        if identity is None:
            os.close(descriptor)
        else:
            matches.append(identity)
    return tuple(sorted(matches, key=lambda value: value.pid))


def _checkout_services(path: Path) -> tuple[tuple[int, str], ...]:
    identities = _open_checkout_services(path)
    try:
        return tuple((identity.pid, identity.marker) for identity in identities)
    finally:
        for identity in identities:
            os.close(identity.descriptor)


def _service_stop_command(
    identity: _CheckoutServiceIdentity,
) -> tuple[str, tuple[str, ...], str] | None:
    unit = identity.cgroup.rsplit("/", 1)[-1]
    if identity.marker == "/usr/local/bin/hg_monitor" and re.fullmatch(
        r"hg_monitor-[A-Za-z0-9_.-]+@[A-Za-z0-9_.:@-]+\.service", unit
    ):
        return (
            "/usr/bin/sudo",
            ("-n", "/usr/bin/systemctl"),
            unit,
        )
    if identity.marker == "/usr/local/bin/chataccd" and re.fullmatch(
        r"chataccd-mount@[A-Za-z0-9_.:@-]+\.service", unit
    ):
        return "/usr/bin/systemctl", ("--user",), unit
    return None


def _revalidate_checkout_service(
    identity: _CheckoutServiceIdentity,
    *,
    exited_is_retired: bool = False,
) -> bool:
    if _pidfd_exited(identity.descriptor):
        if exited_is_retired:
            return False
        raise AssertionError(f"checkout service identity exited: {identity.pid}")
    current = _read_checkout_service_identity(
        identity.pid,
        identity.descriptor,
        identity.marker,
        identity.checkout,
    )
    if current != identity:
        raise AssertionError(f"checkout service identity changed: {identity.pid}")
    return True


def _systemd_properties(
    executable: str,
    prefix: tuple[str, ...],
    unit: str,
) -> dict[str, str]:
    result = TrustedSubprocessWithList.run(
        executable=executable,
        cmd_args=[
            *prefix,
            "show",
            "--property=MainPID",
            "--property=ControlGroup",
            "--property=User",
            unit,
        ],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )


def _stop_managed_service(
    identity: _CheckoutServiceIdentity,
    command: tuple[str, tuple[str, ...], str],
) -> None:
    executable, prefix, unit = command
    _revalidate_checkout_service(identity)
    properties = _systemd_properties(executable, prefix, unit)
    username = pwd.getpwuid(identity.uid).pw_name
    expected_user = username if identity.marker.endswith("hg_monitor") else ""
    if properties != {
        "MainPID": str(identity.pid),
        "ControlGroup": identity.cgroup,
        "User": expected_user,
    }:
        raise AssertionError(f"systemd service identity changed: {unit}")
    _revalidate_checkout_service(identity)
    stopped = TrustedSubprocessWithList.run(
        executable=executable,
        cmd_args=[*prefix, "stop", unit],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if stopped.returncode != 0:
        raise AssertionError(stopped.stderr)


def _chataccd_environment_path(uid: int, unit: str) -> Path:
    instance = unit.removeprefix("chataccd-mount@").removesuffix(".service")
    if not re.fullmatch(r"[A-Za-z0-9_.:@-]+", instance):
        raise AssertionError("chataccd unit instance is invalid")
    runtime = Path(f"/run/user/{uid}")
    configured = os.environ.get("XDG_RUNTIME_DIR")
    if configured is not None and Path(configured).resolve() != runtime:
        raise AssertionError("chataccd runtime root is unexpected")
    return runtime / "chataccd" / instance / "mount-path"


def _remove_test_chataccd_environment(checkout: Path) -> None:
    instance = hashlib.sha256(os.fsencode(checkout.resolve())).hexdigest()[:16]
    unit = f"chataccd-mount@{instance}.service"
    path = _chataccd_environment_path(os.getuid(), unit)
    directory = path.parent
    if not os.path.lexists(directory):
        return
    metadata = directory.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid():
        raise AssertionError(f"chataccd environment directory changed: {directory}")
    entries = {entry.name: entry for entry in directory.iterdir()}
    if not entries or not set(entries).issubset({"mount-path", "startup.lock"}):
        raise AssertionError(f"chataccd environment contents changed: {directory}")
    for name, entry in entries.items():
        entry_metadata = entry.lstat()
        if (
            not stat.S_ISREG(entry_metadata.st_mode)
            or entry_metadata.st_uid != os.getuid()
            or entry_metadata.st_mode & 0o022
        ):
            raise AssertionError(f"chataccd environment entry changed: {entry}")
        if name == "mount-path" and entry.read_bytes() != (
            b"MOUNT_PATH=" + os.fsencode(checkout.resolve()) + b"\n"
        ):
            raise AssertionError(f"chataccd mount path changed: {entry}")
        if name == "startup.lock" and entry.stat().st_size != 0:
            raise AssertionError(f"chataccd startup lock changed: {entry}")
    for entry in entries.values():
        entry.unlink()
    directory.rmdir()
    _fsync_directory(directory.parent)


def _terminate_unmanaged_service(
    identity: _CheckoutServiceIdentity, timeout_seconds: float
) -> None:
    if not _revalidate_checkout_service(identity, exited_is_retired=True):
        return
    signal.pidfd_send_signal(identity.descriptor, signal.SIGTERM)
    deadline = time.monotonic() + timeout_seconds
    while not _pidfd_exited(identity.descriptor) and time.monotonic() < deadline:
        time.sleep(0.01)
    if not _pidfd_exited(identity.descriptor):
        signal.pidfd_send_signal(identity.descriptor, signal.SIGKILL)
    kill_deadline = time.monotonic() + 1
    while not _pidfd_exited(identity.descriptor) and time.monotonic() < kill_deadline:
        time.sleep(0.01)
    if not _pidfd_exited(identity.descriptor):
        raise AssertionError(f"checkout service did not retire: {identity.pid}")


def _retire_checkout_services(
    path: Path, timeout_seconds: float = 3
) -> tuple[int, ...]:
    identities = _open_checkout_services(path)
    try:
        managed_units: set[str] = set()
        for identity in identities:
            command = _service_stop_command(identity)
            if command is None:
                _terminate_unmanaged_service(identity, timeout_seconds)
                continue
            unit = command[2]
            if unit in managed_units:
                raise AssertionError(f"duplicate managed checkout service: {unit}")
            managed_units.add(unit)
            _stop_managed_service(identity, command)
        deadline = time.monotonic() + timeout_seconds
        while _checkout_services(path) and time.monotonic() < deadline:
            time.sleep(0.01)
        remaining = _checkout_services(path)
        if remaining:
            raise AssertionError(f"checkout services did not retire: {remaining!r}")
        return tuple(identity.pid for identity in identities)
    finally:
        for identity in identities:
            os.close(identity.descriptor)


def _drain_removed_worktree(path: Path, timeout_seconds: float) -> None:
    deadline = time.monotonic() + timeout_seconds
    while True:
        if os.path.lexists(path):
            raise AssertionError(f"removed worktree path reappeared: {path}")
        references = _processes_referencing(path)
        if not references:
            if os.path.lexists(path):
                raise AssertionError(f"removed worktree path reappeared: {path}")
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"removed worktree process references did not drain: {references!r}"
            )
        time.sleep(0.01)


def _registered_worktrees(source_root: Path) -> set[Path]:
    result = TrustedSubprocessWithList.run(
        executable="/usr/local/bin/sl",
        cmd_args=[
            "worktree",
            "list",
            "-Tjson",
            "--reason",
            "verify isolated TFA worktree retirement - sl help worktree",
        ],
        cwd=source_root,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return {Path(item["path"]).resolve() for item in json.loads(result.stdout)}


def _eden_mounts() -> set[Path]:
    result = TrustedSubprocessWithList.run(
        executable="/usr/local/bin/eden",
        cmd_args=["list", "--json"],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return {Path(path).resolve() for path in json.loads(result.stdout)}


def _verify_worktree_retired(
    source_root: Path,
    checkout: Path,
    timeout_seconds: float,
) -> None:
    canonical = checkout.resolve()
    deadline = time.monotonic() + timeout_seconds
    while True:
        registered = canonical in _registered_worktrees(source_root)
        mounted = canonical in _eden_mounts()
        if not registered and not mounted:
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                "worktree retirement incomplete: "
                f"registered={registered}, mounted={mounted}, path={canonical}"
            )
        time.sleep(0.1)


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _journal_payload_bytes(payload: dict[str, object]) -> bytes:
    return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()


def _worktree_journal_root() -> Path:
    root = _worktree_source_root().parent / "journals"
    root.mkdir(parents=True, mode=0o700, exist_ok=True)
    metadata = root.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise AssertionError("worktree journal root is not private")
    return root


def _worktree_journal_path(checkout: Path) -> Path:
    digest = hashlib.sha256(os.fsencode(checkout.resolve())).hexdigest()
    return _worktree_journal_root() / f"{digest}.json"


def _write_worktree_journal(
    source_root: Path,
    evidence_root: Path,
    checkout: Path,
    label: str,
) -> Path:
    source = source_root.resolve()
    evidence = evidence_root.resolve()
    target = checkout.resolve()
    if source != _worktree_source_root() or target.parent != evidence:
        raise AssertionError("worktree journal paths are outside their sealed roots")
    target.relative_to(Path("/tmp/tfa-phase2"))
    if not re.fullmatch(r"tfa-[A-Za-z0-9_.-]+", label):
        raise AssertionError("worktree journal label is invalid")
    payload: dict[str, object] = {
        "schema_version": 1,
        "source_root": str(source),
        "evidence_root": str(evidence),
        "checkout": str(target),
        "label": label,
    }
    body = _journal_payload_bytes(payload)
    document = {
        "payload": payload,
        "sha256": hashlib.sha256(body).hexdigest(),
    }
    encoded = json.dumps(document, sort_keys=True).encode() + b"\n"
    destination = _worktree_journal_path(target)
    temporary = destination.with_name(
        f".{destination.name}.{os.getpid()}.{time.time_ns()}.tmp"
    )
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as output:
            output.write(encoded)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, destination)
        _fsync_directory(destination.parent)
    finally:
        os.close(descriptor)
        if temporary.exists():
            temporary.unlink()
    return destination


def _read_worktree_journal(path: Path) -> dict[str, object]:
    metadata = path.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o600
    ):
        raise AssertionError(f"worktree journal metadata is invalid: {path}")
    document = json.loads(path.read_text())
    if set(document) != {"payload", "sha256"}:
        raise AssertionError(f"worktree journal envelope is invalid: {path}")
    payload = document["payload"]
    if not isinstance(payload, dict) or set(payload) != {
        "schema_version",
        "source_root",
        "evidence_root",
        "checkout",
        "label",
    }:
        raise AssertionError(f"worktree journal payload is invalid: {path}")
    if (
        document["sha256"]
        != hashlib.sha256(_journal_payload_bytes(payload)).hexdigest()
    ):
        raise AssertionError(f"worktree journal hash is invalid: {path}")
    source = Path(str(payload["source_root"])).resolve()
    evidence = Path(str(payload["evidence_root"])).resolve()
    checkout = Path(str(payload["checkout"])).resolve()
    label = str(payload["label"])
    if (
        payload["schema_version"] != 1
        or source != _worktree_source_root()
        or checkout.parent != evidence
        or not re.fullmatch(r"tfa-[A-Za-z0-9_.-]+", label)
        or path != _worktree_journal_path(checkout)
    ):
        raise AssertionError(f"worktree journal identity is invalid: {path}")
    checkout.relative_to(Path("/tmp/tfa-phase2"))
    return payload


def _remove_worktree_journal(path: Path) -> None:
    path.unlink()
    _fsync_directory(path.parent)


def _retire_registered_worktree(source_root: Path, checkout: Path) -> None:
    _retire_checkout_services(checkout)
    canonical = checkout.resolve()
    registered = canonical in _registered_worktrees(source_root)
    mounted = canonical in _eden_mounts()
    if registered:
        removed = TrustedSubprocessWithList.run(
            executable="/usr/local/bin/sl",
            cmd_args=[
                "worktree",
                "remove",
                str(checkout),
                "-y",
                "--reason",
                "remove isolated TFA production checkout - sl help worktree",
            ],
            cwd=source_root,
            capture_output=True,
            text=True,
            timeout=60,
        )
    elif mounted:
        removed = TrustedSubprocessWithList.run(
            executable="/usr/local/bin/eden",
            cmd_args=["remove", "--yes", str(checkout)],
            capture_output=True,
            text=True,
            timeout=60,
        )
    elif os.path.lexists(checkout):
        raise AssertionError(f"unregistered checkout path remains: {checkout}")
    else:
        removed = None
    if removed is not None and removed.returncode != 0:
        raise AssertionError(removed.stderr)
    _verify_worktree_retired(source_root, checkout, 10)
    _retire_checkout_services(checkout)
    _remove_test_chataccd_environment(checkout)
    _drain_removed_worktree(checkout, 10)


def _reconcile_worktree_journals() -> None:
    root = _worktree_journal_root()
    for journal in sorted(root.iterdir()):
        if journal.name.startswith(".") and journal.name.endswith(".tmp"):
            raise AssertionError(f"incomplete worktree journal remains: {journal}")
        payload = _read_worktree_journal(journal)
        source_root = Path(str(payload["source_root"]))
        checkout = Path(str(payload["checkout"]))
        _retire_registered_worktree(source_root, checkout)
        _remove_worktree_journal(journal)


@contextmanager
def _registered_worktree(
    source_root: Path,
    evidence_root: Path,
    checkout: Path,
    label: str,
) -> Iterator[None]:
    journal = _write_worktree_journal(source_root, evidence_root, checkout, label)
    body_error: BaseException | None = None
    try:
        added = TrustedSubprocessWithList.run(
            executable="/usr/local/bin/sl",
            cmd_args=[
                "worktree",
                "add",
                str(checkout),
                "-r",
                ".",
                "--label",
                label,
                "--reason",
                "create isolated TFA production checkout - sl help worktree",
            ],
            cwd=source_root,
            capture_output=True,
            text=True,
            timeout=60,
        )
        if added.returncode != 0:
            raise AssertionError(added.stderr)
        yield
    except (Exception, KeyboardInterrupt, SystemExit) as error:
        body_error = error
    cleanup_error: BaseException | None = None
    try:
        _retire_registered_worktree(source_root, checkout)
        _remove_worktree_journal(journal)
    except (Exception, KeyboardInterrupt, SystemExit) as error:
        cleanup_error = error
    if body_error is not None:
        if cleanup_error is not None:
            body_error.add_note(f"worktree cleanup also failed: {cleanup_error}")
        raise body_error.with_traceback(body_error.__traceback__)
    if cleanup_error is not None:
        raise cleanup_error


def _worktree_crash_body(
    root: Path,
    checkout: Path,
    ready: Path,
    label: str,
) -> None:
    with _registered_worktree(_worktree_source_root(), root, checkout, label):
        result = TrustedSubprocessWithList.run(
            executable="/usr/local/bin/sl",
            cmd_args=[
                "log",
                "-r",
                ".",
                "-T",
                "{node}\\n",
                "--reason",
                "start crashed TFA worktree services - sl help log",
            ],
            cwd=checkout,
            capture_output=True,
            text=True,
            timeout=60,
        )
        if result.returncode != 0:
            raise RuntimeError(result.stderr)
        instance = hashlib.sha256(os.fsencode(checkout.resolve())).hexdigest()[:16]
        unit = f"chataccd-mount@{instance}.service"
        environment = _chataccd_environment_path(os.getuid(), unit)
        environment.parent.mkdir(parents=True, mode=0o700)
        environment.write_bytes(b"MOUNT_PATH=" + os.fsencode(checkout) + b"\n")
        environment.chmod(0o600)
        started = TrustedSubprocessWithList.run(
            executable="/usr/bin/systemctl",
            cmd_args=["--user", "start", unit],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if started.returncode != 0:
            raise RuntimeError(started.stderr)
        ready.write_text("ready\n")
        time.sleep(300)


@contextmanager
def _removing_worktree(source_root: Path, checkout: Path) -> Iterator[None]:
    body_error: BaseException | None = None
    try:
        yield
    except (Exception, KeyboardInterrupt, SystemExit) as error:
        body_error = error
    cleanup_error: BaseException | None = None
    try:
        _retire_registered_worktree(source_root, checkout)
    except (Exception, KeyboardInterrupt, SystemExit) as error:
        cleanup_error = error
    if body_error is not None:
        if cleanup_error is not None:
            body_error.add_note(f"worktree cleanup also failed: {cleanup_error}")
        raise body_error.with_traceback(body_error.__traceback__)
    if cleanup_error is not None:
        raise cleanup_error


class ProductionContainmentIntegrationTest(unittest.TestCase):
    def test_controller_handoff_exact_command_creates_all_artifacts_once(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "agent-output"
            output.mkdir()
            source = _controller_handoff_source(
                output,
                root / "forbidden",
                "fbcode/tfa_controller_handoff_probe.txt",
                "asyncssl",
            )
            command = _controller_handoff_command(source)
            self.assertNotIn("\\n", command)
            result = TrustedSubprocessWithList.run(
                executable="/bin/bash",
                cmd_args=["-lc", command],
                cwd=root,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            value = json.loads(result.stdout)
            self.assertEqual(value["case_id"], "asyncssl")
            for name in ("hypotheses.json", "patch.diff", "validation.json"):
                artifact = output / name
                self.assertTrue(artifact.is_file(), name)
                self.assertGreater(artifact.stat().st_size, 0, name)
            patch_bytes = (output / "patch.diff").read_bytes()
            self.assertIn(b"diff --git a/", patch_bytes)
            self.assertNotIn(b"\\n", patch_bytes)

    @_serialized_controller_handoff
    def test_worktree_journal_is_atomic_sealed_and_private(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with _preserving_production_directory(
            "tfa-production-worktree-journal-", "worktree-journal"
        ) as root:
            evidence = root / "evidence"
            evidence.mkdir()
            checkout = evidence / "case-checkout"
            journal_root = root / "journals"
            journal_root.mkdir()
            with patch(
                "testinfra.tfa.tests.test_production_containment._worktree_journal_root",
                return_value=journal_root,
            ):
                journal = _write_worktree_journal(
                    _worktree_source_root(),
                    evidence,
                    checkout,
                    "tfa-journal-test",
                )
                original = journal.read_bytes()
                self.assertEqual(
                    _read_worktree_journal(journal)["checkout"], str(checkout)
                )
                journal.write_bytes(original.replace(b"journal-test", b"journal-evil"))
                with self.assertRaisesRegex(AssertionError, "hash is invalid"):
                    _read_worktree_journal(journal)
                journal.write_bytes(original)
                journal.chmod(0o644)
                with self.assertRaisesRegex(AssertionError, "metadata is invalid"):
                    _read_worktree_journal(journal)
                journal.chmod(0o600)
                incomplete = journal_root / ".incomplete.tmp"
                incomplete.write_text("incomplete\n")
                incomplete.chmod(0o600)
                with self.assertRaisesRegex(AssertionError, "incomplete"):
                    _reconcile_worktree_journals()
                incomplete.unlink()
                _remove_worktree_journal(journal)

    @_serialized_controller_handoff
    def test_killed_worktree_body_is_reconciled_by_independent_owner(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with _preserving_production_directory(
            "tfa-production-worktree-crash-", "worktree-crash"
        ) as root:
            checkout = root / "case-checkout"
            ready = root / "body-ready"
            body = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    "-c",
                    (
                        "import sys; from pathlib import Path; "
                        "from testinfra.tfa.tests.test_production_containment "
                        "import _worktree_crash_body; "
                        "root = Path(sys.argv[1]); "
                        "_worktree_crash_body(root, root / 'case-checkout', "
                        "Path(sys.argv[2]), sys.argv[3])"
                    ),
                    str(root),
                    str(ready),
                    f"tfa-worktree-crash-{os.getpid()}",
                ],
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 60
                while not ready.is_file():
                    if body.poll() is not None:
                        self.fail(f"worktree crash body exited {body.returncode}")
                    if time.monotonic() >= deadline:
                        self.fail("worktree crash body readiness timed out")
                    time.sleep(0.01)
                deadline = time.monotonic() + 10
                while not _checkout_services(checkout):
                    if time.monotonic() >= deadline:
                        self.fail("worktree crash service readiness timed out")
                    time.sleep(0.01)
                canonical = checkout.resolve()
                self.assertIn(canonical, _registered_worktrees(_worktree_source_root()))
                self.assertIn(canonical, _eden_mounts())
                journals = tuple(_worktree_journal_root().iterdir())
                self.assertEqual(len(journals), 1)

                os.kill(body.pid, signal.SIGKILL)
                self.assertEqual(body.wait(timeout=5), -signal.SIGKILL)
                self.assertTrue(ready.is_file())
                self.assertTrue(root.is_dir())

                _reconcile_worktree_journals()
                self.assertEqual(tuple(_worktree_journal_root().iterdir()), ())
                self.assertNotIn(
                    canonical, _registered_worktrees(_worktree_source_root())
                )
                self.assertNotIn(canonical, _eden_mounts())
                self.assertFalse(os.path.lexists(checkout))
                self.assertEqual(_checkout_services(checkout), ())
                self.assertEqual(_processes_referencing(checkout), ())
                self.assertTrue(ready.is_file())
            finally:
                if body.poll() is None:
                    os.killpg(body.pid, signal.SIGKILL)
                    body.wait(timeout=5)
                if tuple(_worktree_journal_root().iterdir()):
                    _reconcile_worktree_journals()

    def test_worktree_retirement_requires_registration_and_mount_absence(
        self,
    ) -> None:
        checkout = Path("/tmp/owned-checkout").resolve()
        for registered, mounted, label in (
            ({checkout}, set(), "registered=True, mounted=False"),
            (set(), {checkout}, "registered=False, mounted=True"),
        ):
            with (
                patch(
                    "testinfra.tfa.tests.test_production_containment._registered_worktrees",
                    return_value=registered,
                ),
                patch(
                    "testinfra.tfa.tests.test_production_containment._eden_mounts",
                    return_value=mounted,
                ),
                self.assertRaisesRegex(AssertionError, label),
            ):
                _verify_worktree_retired(Path("/tmp"), checkout, 0)

    def test_checkout_service_units_are_exact_and_fail_closed(self) -> None:
        descriptor = os.pidfd_open(os.getpid())
        identity = _CheckoutServiceIdentity(
            os.getpid(),
            descriptor,
            os.getuid(),
            1,
            (),
            "/system.slice/hg_monitor-newton@_tmp_case.service",
            "/usr/local/bin/hg_monitor",
            b"/tmp/case",
        )
        self.assertEqual(
            _service_stop_command(identity),
            (
                "/usr/bin/sudo",
                ("-n", "/usr/bin/systemctl"),
                "hg_monitor-newton@_tmp_case.service",
            ),
        )
        chataccd = replace(
            identity,
            cgroup="/user.slice/chataccd-mount@0123456789abcdef.service",
            marker="/usr/local/bin/chataccd",
        )
        self.assertEqual(
            _service_stop_command(chataccd),
            (
                "/usr/bin/systemctl",
                ("--user",),
                "chataccd-mount@0123456789abcdef.service",
            ),
        )
        self.assertIsNone(
            _service_stop_command(replace(identity, cgroup="/unrelated.service"))
        )
        denied = Mock(returncode=1, stderr="access denied")
        command = _service_stop_command(identity)
        self.assertIsNotNone(command)
        try:
            with (
                patch(
                    "testinfra.tfa.tests.test_production_containment._revalidate_checkout_service"
                ),
                patch(
                    "testinfra.tfa.tests.test_production_containment._systemd_properties",
                    return_value={
                        "MainPID": str(identity.pid),
                        "ControlGroup": identity.cgroup,
                        "User": pwd.getpwuid(os.getuid()).pw_name,
                    },
                ),
                patch.object(TrustedSubprocessWithList, "run", return_value=denied),
                self.assertRaisesRegex(AssertionError, "access denied"),
            ):
                _stop_managed_service(identity, command)
        finally:
            os.close(descriptor)

    def test_checkout_service_identity_drift_prevents_mutation(self) -> None:
        descriptor = os.pidfd_open(os.getpid())
        identity = _CheckoutServiceIdentity(
            os.getpid(),
            descriptor,
            os.getuid(),
            1,
            (),
            "/test.scope",
            "/usr/local/bin/hg_monitor",
            b"/tmp/case",
        )
        try:
            with (
                patch(
                    "testinfra.tfa.tests.test_production_containment._read_checkout_service_identity",
                    return_value=replace(identity, start_time=2),
                ),
                patch("signal.pidfd_send_signal") as send_signal,
                self.assertRaisesRegex(AssertionError, "identity changed"),
            ):
                _terminate_unmanaged_service(identity, 1)
            send_signal.assert_not_called()

            managed = replace(
                identity,
                cgroup="/system.slice/hg_monitor-newton@_tmp_case.service",
            )
            command = _service_stop_command(managed)
            self.assertIsNotNone(command)
            with (
                patch(
                    "testinfra.tfa.tests.test_production_containment._read_checkout_service_identity",
                    return_value=replace(managed, cgroup="/replacement.service"),
                ),
                patch.object(TrustedSubprocessWithList, "run") as run,
                self.assertRaisesRegex(AssertionError, "identity changed"),
            ):
                _stop_managed_service(managed, command)
            run.assert_not_called()

            with (
                patch(
                    "testinfra.tfa.tests.test_production_containment._revalidate_checkout_service",
                    return_value=True,
                ),
                patch(
                    "testinfra.tfa.tests.test_production_containment._systemd_properties",
                    return_value={
                        "MainPID": str(managed.pid + 1),
                        "ControlGroup": managed.cgroup,
                        "User": pwd.getpwuid(os.getuid()).pw_name,
                    },
                ),
                patch.object(TrustedSubprocessWithList, "run") as run,
                self.assertRaisesRegex(AssertionError, "systemd service identity"),
            ):
                _stop_managed_service(managed, command)
            run.assert_not_called()
        finally:
            os.close(descriptor)

    def test_checkout_service_retirement_is_exact_and_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "removed-worktree"
            service_ready = root / "service-ready"
            unrelated_ready = root / "unrelated-ready"
            source = (
                "import pathlib,sys,time; "
                "pathlib.Path(sys.argv[1]).write_text('ready\\n'); "
                "time.sleep(60)"
            )
            service = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    "-c",
                    source,
                    str(service_ready),
                    "/usr/local/bin/hg_monitor",
                    "--hgmonitortask-path",
                    str(checkout),
                ],
                start_new_session=True,
            )
            unrelated = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    "-c",
                    source,
                    str(unrelated_ready),
                    str(checkout),
                ],
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not service_ready.is_file() or not unrelated_ready.is_file():
                    if time.monotonic() >= deadline:
                        self.fail("checkout-service fixture readiness timed out")
                    time.sleep(0.01)
                expected = tuple(pid for pid, _marker in _checkout_services(checkout))
                self.assertIn(service.pid, expected)
                self.assertNotIn(unrelated.pid, expected)
                retired = _retire_checkout_services(checkout)
                self.assertIn(service.pid, retired)
                self.assertNotIn(unrelated.pid, retired)
                self.assertEqual(service.wait(timeout=5), -signal.SIGTERM)
                self.assertIsNone(unrelated.poll())
            finally:
                for process in (service, unrelated):
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)

    def test_removed_worktree_drain_rejects_persistent_reference(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "removed-worktree"
            ready = root / "ready"
            process = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    "-c",
                    (
                        "import pathlib,sys,time; "
                        "pathlib.Path(sys.argv[1]).write_text('ready\\n'); "
                        "time.sleep(60)"
                    ),
                    str(ready),
                    str(checkout),
                ],
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not ready.is_file():
                    if time.monotonic() >= deadline:
                        self.fail("persistent-reference readiness timed out")
                    time.sleep(0.01)
                with self.assertRaisesRegex(
                    AssertionError,
                    rf"process references did not drain: .*{process.pid}",
                ):
                    _drain_removed_worktree(checkout, 0.05)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            _drain_removed_worktree(checkout, 5)

    def test_removed_worktree_drain_rejects_encoded_argv_reference(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "removed-worktree"
            ready = root / "ready"
            process = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    "-c",
                    (
                        "import pathlib,sys,time; "
                        "pathlib.Path(sys.argv[1]).write_text('ready\\n'); "
                        "time.sleep(60)"
                    ),
                    str(ready),
                    f"--checkout={checkout}",
                ],
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not ready.is_file():
                    if time.monotonic() >= deadline:
                        self.fail("encoded-argv readiness timed out")
                    time.sleep(0.01)
                with self.assertRaisesRegex(
                    AssertionError,
                    rf"process references did not drain: .*{process.pid}.*argv:",
                ):
                    _drain_removed_worktree(checkout, 0.05)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            _drain_removed_worktree(checkout, 5)

    def test_removed_worktree_drain_rejects_deleted_cwd_reference(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "removed-worktree"
            checkout.mkdir()
            ready = root / "ready"
            process = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    "-c",
                    (
                        "import pathlib,sys,time; "
                        "pathlib.Path(sys.argv[1]).write_text('ready\\n'); "
                        "time.sleep(60)"
                    ),
                    str(ready),
                ],
                cwd=checkout,
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not ready.is_file():
                    if time.monotonic() >= deadline:
                        self.fail("deleted-cwd readiness timed out")
                    time.sleep(0.01)
                checkout.rmdir()
                with self.assertRaisesRegex(
                    AssertionError,
                    rf"process references did not drain: .*{process.pid}.*cwd:",
                ):
                    _drain_removed_worktree(checkout, 0.05)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            _drain_removed_worktree(checkout, 5)

    def test_removed_worktree_drain_rejects_deleted_open_fd_reference(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "removed-worktree"
            checkout.mkdir()
            held = checkout / "held"
            held.write_text("held\n")
            ready = root / "ready"
            script = (
                "import pathlib,time; "
                f"stream=open({str(held)!r}); "
                f"pathlib.Path({str(ready)!r}).write_text('ready\\n'); "
                "time.sleep(60)"
            )
            process = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=["-c", script],
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not ready.is_file():
                    if time.monotonic() >= deadline:
                        self.fail("open-fd readiness timed out")
                    time.sleep(0.01)
                held.unlink()
                checkout.rmdir()
                references = _processes_referencing(checkout)
                self.assertTrue(
                    any(
                        reference.startswith("fd:")
                        and str(held) in reference
                        and reference.endswith(" (deleted)")
                        for _, _, process_references in references
                        for reference in process_references
                    )
                )
                with self.assertRaisesRegex(
                    AssertionError,
                    rf"process references did not drain: .*fd:.*{re.escape(str(held))}",
                ):
                    _drain_removed_worktree(checkout, 0.05)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            _drain_removed_worktree(checkout, 5)

    def test_process_reference_scan_checks_process_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "removed-worktree"
            proc = root / "proc"
            process = proc / "123"
            (process / "fd").mkdir(parents=True)
            (process / "cmdline").write_bytes(b"worker\0")
            (process / "cwd").symlink_to("/")
            (process / "root").symlink_to(checkout)
            references = _processes_referencing(checkout, proc)
            self.assertEqual(references[0][0], 123)
            self.assertEqual(references[0][2], (f"root:{checkout}",))

    @_serialized_nested_hermit
    def test_removed_worktree_drain_rejects_real_deleted_process_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "removed-worktree"
            checkout.mkdir()
            ready = root / "ready"
            script = (
                "import os,sys,time; "
                "descriptor=os.open("
                "sys.argv[1],os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600); "
                "os.chroot(sys.argv[2]); os.chdir('/'); "
                "os.write(descriptor,f'{os.getpid()}\\n'.encode()); "
                "os.close(descriptor); time.sleep(60)"
            )
            process = TrustedSubprocessWithList.Popen(
                executable="/usr/bin/unshare",
                cmd_args=[
                    "--user",
                    "--map-root-user",
                    "--fork",
                    "/usr/local/bin/fbpython",
                    "-c",
                    script,
                    str(ready),
                    str(checkout),
                ],
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not ready.is_file():
                    if process.poll() is not None:
                        self.fail(f"process-root fixture exited {process.returncode}")
                    if time.monotonic() >= deadline:
                        self.fail("process-root readiness timed out")
                    time.sleep(0.01)
                child_pid = int(ready.read_text().strip())
                checkout.rmdir()
                references = _processes_referencing(checkout)
                child = [
                    reference for reference in references if reference[0] == child_pid
                ]
                self.assertEqual(len(child), 1)
                self.assertIn(
                    f"root:{checkout} (deleted)",
                    child[0][2],
                )
                with self.assertRaisesRegex(
                    AssertionError,
                    rf"process references did not drain: .*{child_pid}.*root:",
                ):
                    _drain_removed_worktree(checkout, 0.05)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            _drain_removed_worktree(checkout, 5)

    @_serialized_nested_hermit
    def test_runtime_recovery_preserves_mounted_interrupted_staging(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            buck_out = root / "checkout" / "buck-out"
            output = buck_out / "v2" / "art" / "target" / "__test__"
            output.mkdir(parents=True)
            elf = output / "test"
            elf.write_bytes(b"elf")
            tree = output / "__test__shared_libs_symlink_tree"
            tree.mkdir()
            source = root / "external" / "libexternal.so"
            source.parent.mkdir()
            source.write_bytes(b"external")
            (tree / source.name).symlink_to(source)
            evidence = root / "attempt" / "runtime.json"
            real_copy = pipeline_module._copy_pinned_runtime_source

            def fail_external_copy(
                source_path: Path, destination: Path, max_bytes: int
            ) -> tuple[dict[str, object], dict[str, object]]:
                if destination.name == source.name:
                    raise RuntimeError("injected external-copy failure")
                return real_copy(source_path, destination, max_bytes)

            with (
                patch(
                    "testinfra.tfa.pipeline._copy_pinned_runtime_source",
                    side_effect=fail_external_copy,
                ),
                self.assertRaisesRegex(RuntimeError, "external-copy failure"),
            ):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, evidence
                )
            transaction_path = evidence.with_name(
                "runtime-materialization-transaction.json"
            )
            interrupted = Path(str(json.loads(transaction_path.read_text())["staging"]))
            mount_target = interrupted / elf.parent.name
            mounted_source = root / "mounted-source"
            mounted_source.mkdir()
            sentinel = mounted_source / "outside-sentinel"
            sentinel.write_text("preserve")
            script = (
                "import ctypes,os,sys; from pathlib import Path; "
                "import testinfra.tfa.pipeline as pipeline; "
                "source=Path(sys.argv[1]); target=Path(sys.argv[2]); "
                "libc=ctypes.CDLL(None,use_errno=True); "
                "result=libc.mount(os.fsencode(source),os.fsencode(target),"
                "None,4096,None); "
                "result == 0 or (_ for _ in ()).throw("
                "OSError(ctypes.get_errno(),os.strerror(ctypes.get_errno()))); "
                "pipeline._materialize_external_runtime_links("
                "Path(sys.argv[3]),Path(sys.argv[4]),Path(sys.argv[5]))"
            )
            process = TrustedSubprocessWithList.Popen(
                executable="/usr/bin/unshare",
                cmd_args=[
                    "--user",
                    "--map-root-user",
                    "--mount",
                    "--fork",
                    "/usr/bin/env",
                    f"PYTHONPATH={Path(__file__).parents[3]}",
                    "/usr/local/bin/fbpython",
                    "-c",
                    script,
                    str(mounted_source),
                    str(mount_target),
                    str(elf),
                    str(buck_out),
                    str(evidence),
                ],
                start_new_session=True,
            )
            self.assertEqual(process.wait(timeout=30), 1)
            self.assertEqual(sentinel.read_text(), "preserve")
            transaction = json.loads(transaction_path.read_text())
            self.assertEqual(
                [record["path"] for record in transaction["preserved_staging"]],
                [str(interrupted)],
            )
            with self.assertRaisesRegex(RuntimeError, "requires explicit cleanup"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, evidence
                )
            self.assertEqual(sentinel.read_text(), "preserve")

    def test_post_diagnosis_failure_bundle_outlives_case_directory(self) -> None:
        with tempfile.TemporaryDirectory() as persistent_temporary:
            persistent = Path(persistent_temporary)
            with tempfile.TemporaryDirectory() as source_temporary:
                source = Path(source_temporary)
                case_dir = source / "ephemeral-case"
                case_dir.mkdir()
                (case_dir / "post-diagnosis.txt").write_text("failed assertion\n")
                failure = _preserve_controller_handoff_failure(
                    case_dir, persistent / "persistent-failures"
                )
            self.assertFalse(source.exists())
            self.assertEqual(
                (failure / "post-diagnosis.txt").read_text(), "failed assertion\n"
            )

    def test_worktree_cleanup_failure_preserves_evidence_outside_root(self) -> None:
        with tempfile.TemporaryDirectory() as persistent_temporary:
            persistent = Path(persistent_temporary)
            retained_root: Path | None = None
            with self.assertRaisesRegex(
                AssertionError, "production evidence preserved"
            ) as preserved:
                with _preserving_production_directory(
                    "tfa-production-forced-cleanup-",
                    "forced-cleanup",
                    persistent,
                ) as root:
                    retained_root = root
                    checkout = root / "case-checkout"
                    checkout.mkdir()
                    (root / "cleanup-evidence.txt").write_text("preserve me\n")
                    with (
                        patch.object(
                            TrustedSubprocessWithList,
                            "run",
                            return_value=Mock(
                                returncode=1,
                                stderr="forced remove failure",
                            ),
                        ),
                        _removing_worktree(root, checkout),
                    ):
                        raise ValueError("primary body failure")
            primary = preserved.exception.__cause__
            self.assertIsInstance(primary, ValueError)
            self.assertEqual(str(primary), "primary body failure")
            self.assertTrue(
                any(
                    "worktree cleanup also failed" in note
                    for note in getattr(primary, "__notes__", ())
                )
            )
            bundles = tuple((persistent / "forced-cleanup").iterdir())
            self.assertEqual(len(bundles), 1)
            self.assertEqual(
                (bundles[0] / "cleanup-evidence.txt").read_text(),
                "preserve me\n",
            )
            self.assertTrue((bundles[0] / "case-checkout").is_dir())
            self.assertIsNotNone(retained_root)
            if retained_root is not None:
                self.assertTrue(retained_root.is_dir())
                self.assertTrue((retained_root / "case-checkout").is_dir())
                shutil.rmtree(retained_root)

    def test_checkout_identity_lock_excludes_concurrent_controller(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-checkout-lock-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            checkout.mkdir()
            ready = root / "ready"
            release = root / "release"
            helper = Path(__file__).with_name("test_process_migration_helper.py")
            process = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    str(helper),
                    "--checkout-lock-probe",
                    str(checkout),
                    str(ready),
                    str(release),
                ],
            )
            try:
                deadline = time.monotonic() + 10
                while not ready.is_file():
                    if time.monotonic() >= deadline:
                        self.fail("checkout lock probe readiness timed out")
                    time.sleep(0.01)
                with self.assertRaisesRegex(StateError, "lease already held"):
                    with checkout_lease(checkout):
                        self.fail("concurrent checkout lease was admitted")
                release.write_text("release\n")
                self.assertEqual(process.wait(timeout=10), 0)
                with checkout_lease(checkout):
                    pass
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)

    def test_checkout_identity_lock_rejects_path_replacement(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-checkout-replacement-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            checkout.mkdir()
            moved = root / "moved-checkout"
            command_artifacts = root / "command-artifacts"
            lease = checkout_lease(checkout)
            lease.__enter__()
            try:
                checkout.rename(moved)
                checkout.mkdir()
                sentinel = checkout / "host-visible-sentinel"
                sentinel.write_text("unchanged\n")
                with self.assertRaisesRegex(StateError, "lease already held"):
                    with checkout_lease(checkout):
                        self.fail("replacement checkout acquired the path lease")
                with self.assertRaisesRegex(StateError, "identity changed"):
                    run_process(
                        "/usr/local/bin/fbpython",
                        (
                            "-c",
                            "from pathlib import Path; "
                            "Path('host-visible-sentinel').write_text('changed')",
                        ),
                        checkout,
                        command_artifacts,
                        10,
                    )
                self.assertEqual(sentinel.read_text(), "unchanged\n")
                self.assertFalse(command_artifacts.exists())
                sentinel.unlink()
                checkout.rmdir()

                checkout.symlink_to(moved, target_is_directory=True)
                with self.assertRaisesRegex(StateError, "canonical"):
                    with checkout_lease(checkout):
                        self.fail("symlink replacement acquired a checkout lease")
                with self.assertRaisesRegex(StateError, "identity changed"):
                    run_process(
                        "/usr/local/bin/fbpython",
                        ("-c", "pass"),
                        checkout,
                        command_artifacts,
                        10,
                    )
                self.assertFalse(command_artifacts.exists())
                checkout.unlink()

                with self.assertRaisesRegex(StateError, "disappeared"):
                    run_process(
                        "/usr/local/bin/fbpython",
                        ("-c", "pass"),
                        checkout,
                        command_artifacts,
                        10,
                    )
                self.assertFalse(command_artifacts.exists())
                moved.rename(checkout)
            finally:
                if checkout.is_symlink():
                    checkout.unlink()
                elif checkout.is_dir() and moved.is_dir():
                    sentinel = checkout / "host-visible-sentinel"
                    if sentinel.is_file():
                        sentinel.unlink()
                    checkout.rmdir()
                if not checkout.exists() and moved.is_dir():
                    moved.rename(checkout)
                lease.__exit__(None, None, None)

    @_serialized_controller_handoff
    @patch(
        "testinfra.tfa.pipeline._verify_candidate_approval",
        side_effect=_synthetic_candidate_approval,
    )
    def test_real_controller_transaction_rejects_and_retries_exact_paths(
        self,
        _approval: object,
    ) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        source_root = _worktree_source_root()
        with _preserving_production_directory(
            "tfa-production-controller-transaction-", "controller-transaction"
        ) as root:
            checkout = root / "case-checkout"

            def sl(*arguments: str):
                return TrustedSubprocessWithList.run(
                    executable="/usr/local/bin/sl",
                    cmd_args=list(arguments),
                    cwd=checkout,
                    capture_output=True,
                    text=True,
                    timeout=60,
                )

            with _registered_worktree(
                source_root,
                root,
                checkout,
                f"tfa-transaction-{os.getpid()}",
            ):
                revision = sl(
                    "log",
                    "-r",
                    ".",
                    "-T",
                    "{node}\\n",
                    "--reason",
                    "record transaction test base - sl help log",
                )
                self.assertEqual(revision.returncode, 0, revision.stderr)
                case = replace(
                    load_manifest(
                        Path(manifest_module.__file__).with_name("cases.v1.json")
                    ).case("asyncssl"),
                    source_revision=revision.stdout.strip(),
                )
                case_dir = root / "artifacts" / case.case_id
                ledger = Ledger(case_dir / "control" / "ledger.jsonl")
                ledger.append(Stage.VALIDATED_REPRO, {"test": True})
                ledger.append(Stage.INITIAL_COMMENT, {"test": True})
                ledger.append(Stage.INVESTIGATING, {"test": True})

                def assert_run_agent_recovery_blocked(
                    candidate_attempt: Path,
                ) -> None:
                    transaction_path = candidate_attempt / "transaction.json"
                    transaction_before = transaction_path.read_bytes()
                    report_before = (
                        case_dir / "agent-output" / "report.json"
                    ).read_bytes()
                    status_before = sl(
                        "status",
                        "--reason",
                        "seal transaction before blocked run-agent - sl help status",
                    ).stdout
                    with self.assertRaisesRegex(
                        RuntimeError, "run-case recovery is required"
                    ):
                        run_investigation(
                            Manifest(1, (case,)),
                            case.case_id,
                            root / "artifacts",
                            checkout,
                            root / "unused-schema.json",
                            10,
                        )
                    self.assertEqual(transaction_path.read_bytes(), transaction_before)
                    self.assertEqual(
                        (case_dir / "agent-output" / "report.json").read_bytes(),
                        report_before,
                    )
                    self.assertEqual(
                        sl(
                            "status",
                            "--reason",
                            "verify blocked run-agent left checkout unchanged"
                            " - sl help status",
                        ).stdout,
                        status_before,
                    )

                old_path = "fbcode/testinfra/example/PACKAGE"
                new_path = "fbcode/testinfra/example/PACKAGE.transaction-probe"
                moved = sl(
                    "mv",
                    old_path,
                    new_path,
                    "--reason",
                    "generate transaction rename patch - sl help mv",
                )
                self.assertEqual(moved.returncode, 0, moved.stderr)
                generated = sl(
                    "diff",
                    "--git",
                    "--reason",
                    "generate transaction rename patch - sl help diff",
                    "--",
                    old_path,
                    new_path,
                )
                self.assertEqual(generated.returncode, 0, generated.stderr)
                reverted = sl(
                    "revert",
                    "--no-backup",
                    "--reason",
                    "restore transaction test baseline - sl help revert",
                    "--",
                    old_path,
                    new_path,
                )
                self.assertEqual(reverted.returncode, 0, reverted.stderr)
                generated_destination = checkout / new_path
                if generated_destination.exists():
                    generated_destination.unlink()
                self.assertEqual(
                    sl(
                        "status",
                        "--reason",
                        "verify generated rename cleanup - sl help status",
                    ).stdout,
                    "",
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 1, generated.stdout
                )
                with checkout_lease(checkout):
                    _apply_candidate_locked(
                        case, case_dir, checkout, attempt, candidate
                    )
                    self.assertFalse((checkout / old_path).exists())
                    self.assertTrue((checkout / new_path).is_file())
                    _reject_active_candidate(
                        case, case_dir, checkout, "forced validation rejection"
                    )
                self.assertTrue((checkout / old_path).is_file())
                self.assertFalse((checkout / new_path).exists())
                self.assertEqual(
                    sl(
                        "status",
                        "--reason",
                        "verify rejected transaction cleanup - sl help status",
                    ).stdout,
                    "",
                )
                rejected = case_dir / "control" / "rejected-candidates" / "attempt-001"
                self.assertTrue((rejected / "rejection.json").is_file())
                for mandatory in (
                    "agent-report.json",
                    "agent-final-message.json",
                    "approved-patch.diff",
                    "candidate-approval.json",
                    "candidate-seal.json",
                    "pre-apply-review.json",
                    "transaction.json",
                    "active-candidate.json",
                    "handoff.json",
                ):
                    self.assertTrue((rejected / mandatory).is_file(), mandatory)
                archived_report = rejected / "agent-report.json"
                hidden_report = rejected / "agent-report.hidden"
                archived_report.rename(hidden_report)
                try:
                    with self.assertRaisesRegex(
                        RuntimeError, "cannot inspect evidence file"
                    ):
                        pipeline_module._verify_rejection_archive(
                            rejected / "rejection.json", rejected
                        )
                finally:
                    hidden_report.rename(archived_report)

                unsafe_patch = (
                    "diff --git a/--all b/--all\n"
                    "new file mode 100644\n--- /dev/null\n+++ b/--all\n"
                    "@@ -0,0 +1 @@\n+unsafe\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 2, unsafe_patch
                )
                with (
                    checkout_lease(checkout),
                    self.assertRaisesRegex(
                        RuntimeError, "unsafe candidate repository path"
                    ),
                ):
                    _apply_candidate_locked(
                        case, case_dir, checkout, attempt, candidate
                    )
                self.assertEqual(
                    sl(
                        "status",
                        "--reason",
                        "verify unsafe transaction cleanup - sl help status",
                    ).stdout,
                    "",
                )
                self.assertFalse((attempt / "transaction.json").exists())
                self.assertTrue((attempt / "candidate-seal.json").is_file())
                self.assertTrue((attempt / "approved-patch.diff").is_file())

                partial_path = "fbcode/tfa_transaction_partial.txt"
                partial_patch = (
                    f"diff --git a/{partial_path} b/{partial_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{partial_path}\n@@ -0,0 +1 @@\n+partial\n"
                    "@@ malformed @@\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 3, partial_patch
                )
                with (
                    checkout_lease(checkout),
                    self.assertRaisesRegex(RuntimeError, "could not apply"),
                ):
                    _apply_candidate_locked(
                        case, case_dir, checkout, attempt, candidate
                    )
                self.assertFalse((checkout / partial_path).exists())

                retry_path = "fbcode/tfa_transaction_retry.txt"
                retry_patch = (
                    f"diff --git a/{retry_path} b/{retry_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{retry_path}\n@@ -0,0 +1 @@\n+retry\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 4, retry_patch
                )
                with checkout_lease(checkout):
                    _apply_candidate_locked(
                        case, case_dir, checkout, attempt, candidate
                    )
                    self.assertEqual((checkout / retry_path).read_text(), "retry\n")
                    _reject_active_candidate(case, case_dir, checkout, "retry cleanup")
                self.assertEqual(
                    sl(
                        "status",
                        "--reason",
                        "verify retry transaction cleanup - sl help status",
                    ).stdout,
                    "",
                )

                recovery_path = "fbcode/tfa_transaction_recovery.txt"
                recovery_patch = (
                    f"diff --git a/{recovery_path} b/{recovery_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{recovery_path}\n@@ -0,0 +1 @@\n+recover\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 5, recovery_patch
                )
                original_write_transaction = pipeline_module._write_transaction
                crashed = False

                def crash_after_applied(
                    candidate_attempt: Path,
                    transaction: dict[str, object],
                    phase: str,
                ) -> dict[str, object]:
                    nonlocal crashed
                    value = original_write_transaction(
                        candidate_attempt, transaction, phase
                    )
                    if phase == "applied" and not crashed:
                        crashed = True
                        raise KeyboardInterrupt("fault after applied journal")
                    return value

                with checkout_lease(checkout):
                    with (
                        patch(
                            "testinfra.tfa.pipeline._write_transaction",
                            side_effect=crash_after_applied,
                        ),
                        self.assertRaisesRegex(KeyboardInterrupt, "applied journal"),
                    ):
                        _apply_candidate_locked(
                            case, case_dir, checkout, attempt, candidate
                        )
                    assert_run_agent_recovery_blocked(attempt)
                    recovered = _recover_candidate_transaction(case, case_dir, checkout)
                    self.assertEqual(recovered, candidate)
                    self.assertEqual(
                        (checkout / recovery_path).read_text(), "recover\n"
                    )
                    _reject_active_candidate(
                        case, case_dir, checkout, "recovery cleanup"
                    )
                self.assertEqual(
                    sl(
                        "status",
                        "--reason",
                        "verify recovered transaction cleanup - sl help status",
                    ).stdout,
                    "",
                )
                original_write_metadata = pipeline_module.write_metadata
                for attempt_number, boundary in (
                    (6, "handoff.json"),
                    (7, "active-candidate.json"),
                ):
                    boundary_path = (
                        f"fbcode/tfa_transaction_boundary_{attempt_number}.txt"
                    )
                    boundary_patch = (
                        f"diff --git a/{boundary_path} b/{boundary_path}\n"
                        "new file mode 100644\n--- /dev/null\n"
                        f"+++ b/{boundary_path}\n@@ -0,0 +1 @@\n+boundary\n"
                    )
                    attempt, candidate = _write_controller_candidate(
                        case_dir, case.case_id, attempt_number, boundary_patch
                    )
                    faulted = False

                    def crash_after_file_publish(
                        path: Path,
                        value: dict[str, object],
                        boundary_name: str = boundary,
                    ) -> None:
                        nonlocal faulted
                        original_write_metadata(path, value)
                        if path.name == boundary_name and not faulted:
                            faulted = True
                            raise KeyboardInterrupt(
                                f"fault after publishing {boundary_name}"
                            )

                    with checkout_lease(checkout):
                        with (
                            patch(
                                "testinfra.tfa.pipeline.write_metadata",
                                side_effect=crash_after_file_publish,
                            ),
                            self.assertRaisesRegex(
                                KeyboardInterrupt, "fault after publishing"
                            ),
                        ):
                            _apply_candidate_locked(
                                case, case_dir, checkout, attempt, candidate
                            )
                        recovered = _recover_candidate_transaction(
                            case, case_dir, checkout
                        )
                        self.assertEqual(recovered, candidate)
                        _reject_active_candidate(
                            case, case_dir, checkout, f"{boundary} recovery cleanup"
                        )

                applying_path = "fbcode/tfa_transaction_applying.txt"
                applying_patch = (
                    f"diff --git a/{applying_path} b/{applying_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{applying_path}\n@@ -0,0 +1 @@\n+applying\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 8, applying_patch
                )
                applying_faulted = False

                def crash_after_applying(
                    candidate_attempt: Path,
                    transaction: dict[str, object],
                    phase: str,
                ) -> dict[str, object]:
                    nonlocal applying_faulted
                    value = original_write_transaction(
                        candidate_attempt, transaction, phase
                    )
                    if phase == "applying" and not applying_faulted:
                        applying_faulted = True
                        raise KeyboardInterrupt("fault after applying journal")
                    return value

                with checkout_lease(checkout):
                    with (
                        patch(
                            "testinfra.tfa.pipeline._write_transaction",
                            side_effect=crash_after_applying,
                        ),
                        self.assertRaisesRegex(KeyboardInterrupt, "applying journal"),
                    ):
                        _apply_candidate_locked(
                            case, case_dir, checkout, attempt, candidate
                        )
                    assert_run_agent_recovery_blocked(attempt)
                    self.assertIsNone(
                        _recover_candidate_transaction(case, case_dir, checkout)
                    )
                self.assertFalse((checkout / applying_path).exists())

                diff_path = "fbcode/tfa_transaction_diff_boundary.txt"
                diff_patch = (
                    f"diff --git a/{diff_path} b/{diff_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{diff_path}\n@@ -0,0 +1 @@\n+diff\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 9, diff_patch
                )
                original_sl_diff = pipeline_module._sl_diff

                def crash_before_diff_record(
                    source_checkout: Path,
                    changed: tuple[str, ...],
                    artifact_dir: Path,
                ):
                    if artifact_dir.name == "applied-diff":
                        raise KeyboardInterrupt("fault before applied diff record")
                    return original_sl_diff(source_checkout, changed, artifact_dir)

                with checkout_lease(checkout):
                    with (
                        patch(
                            "testinfra.tfa.pipeline._sl_diff",
                            side_effect=crash_before_diff_record,
                        ),
                        self.assertRaisesRegex(KeyboardInterrupt, "applied diff"),
                    ):
                        _apply_candidate_locked(
                            case, case_dir, checkout, attempt, candidate
                        )
                    self.assertIsNone(
                        _recover_candidate_transaction(case, case_dir, checkout)
                    )
                self.assertFalse((checkout / diff_path).exists())

                publish_path = "fbcode/tfa_transaction_publish_boundary.txt"
                publish_patch = (
                    f"diff --git a/{publish_path} b/{publish_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{publish_path}\n@@ -0,0 +1 @@\n+publish\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 10, publish_patch
                )
                original_publish_directory = pipeline_module.publish_directory
                publish_faulted = False

                def crash_after_archive_publish(
                    staging: Path, destination: Path
                ) -> None:
                    nonlocal publish_faulted
                    original_publish_directory(staging, destination)
                    if not publish_faulted:
                        publish_faulted = True
                        raise KeyboardInterrupt("fault after archive publish")

                with checkout_lease(checkout):
                    _apply_candidate_locked(
                        case, case_dir, checkout, attempt, candidate
                    )
                    with (
                        patch(
                            "testinfra.tfa.pipeline.publish_directory",
                            side_effect=crash_after_archive_publish,
                        ),
                        self.assertRaisesRegex(KeyboardInterrupt, "archive publish"),
                    ):
                        _reject_active_candidate(
                            case, case_dir, checkout, "publish fault"
                        )
                    assert_run_agent_recovery_blocked(attempt)
                    self.assertIsNone(
                        _recover_candidate_transaction(case, case_dir, checkout)
                    )
                self.assertFalse((checkout / publish_path).exists())

                rejected_path = "fbcode/tfa_transaction_rejected_boundary.txt"
                rejected_patch = (
                    f"diff --git a/{rejected_path} b/{rejected_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{rejected_path}\n@@ -0,0 +1 @@\n+rejected\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 11, rejected_patch
                )
                rejected_faulted = False

                def crash_after_rejected_journal(
                    candidate_attempt: Path,
                    transaction: dict[str, object],
                    phase: str,
                ) -> dict[str, object]:
                    nonlocal rejected_faulted
                    value = original_write_transaction(
                        candidate_attempt, transaction, phase
                    )
                    if phase == "rejected" and not rejected_faulted:
                        rejected_faulted = True
                        raise KeyboardInterrupt("fault after rejected journal")
                    return value

                with checkout_lease(checkout):
                    _apply_candidate_locked(
                        case, case_dir, checkout, attempt, candidate
                    )
                    with (
                        patch(
                            "testinfra.tfa.pipeline._write_transaction",
                            side_effect=crash_after_rejected_journal,
                        ),
                        self.assertRaisesRegex(KeyboardInterrupt, "rejected journal"),
                    ):
                        _reject_active_candidate(
                            case, case_dir, checkout, "rejected fault"
                        )
                    self.assertIsNone(
                        _recover_candidate_transaction(case, case_dir, checkout)
                    )
                self.assertFalse((checkout / rejected_path).exists())

                revert_path = "fbcode/tfa_transaction_revert_boundary.txt"
                revert_patch = (
                    f"diff --git a/{revert_path} b/{revert_path}\n"
                    "new file mode 100644\n--- /dev/null\n"
                    f"+++ b/{revert_path}\n@@ -0,0 +1 @@\n+revert\n"
                )
                attempt, candidate = _write_controller_candidate(
                    case_dir, case.case_id, 12, revert_patch
                )
                original_revert_checkout = pipeline_module._revert_checkout
                revert_faulted = False

                def crash_after_revert(
                    rejected_case: Case,
                    rejected_checkout: Path,
                    artifact_dir: Path,
                    expected_paths: tuple[str, ...],
                    baseline_files: list[dict[str, object]],
                ) -> None:
                    nonlocal revert_faulted
                    original_revert_checkout(
                        rejected_case,
                        rejected_checkout,
                        artifact_dir,
                        expected_paths,
                        baseline_files,
                    )
                    if not revert_faulted:
                        revert_faulted = True
                        raise KeyboardInterrupt("fault after exact revert")

                with checkout_lease(checkout):
                    _apply_candidate_locked(
                        case, case_dir, checkout, attempt, candidate
                    )
                    with (
                        patch(
                            "testinfra.tfa.pipeline._revert_checkout",
                            side_effect=crash_after_revert,
                        ),
                        self.assertRaisesRegex(KeyboardInterrupt, "exact revert"),
                    ):
                        _reject_active_candidate(
                            case, case_dir, checkout, "revert fault"
                        )
                    self.assertIsNone(
                        _recover_candidate_transaction(case, case_dir, checkout)
                    )
                self.assertFalse((checkout / revert_path).exists())
                self.assertEqual(
                    sl(
                        "status",
                        "--reason",
                        "verify transaction fault cleanup - sl help status",
                    ).stdout,
                    "",
                )

    @_serialized_nested_hermit
    def test_nested_hermit_lock_serializes_independent_processes(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-hermit-lock-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            helper = Path(__file__).with_name("test_process_migration_helper.py")
            events = root / "events.jsonl"
            processes = [
                TrustedSubprocessWithList.Popen(
                    executable="/usr/local/bin/fbpython",
                    cmd_args=[
                        str(helper),
                        "--hermit-lock-probe",
                        str(root),
                        str(_HERMIT_TEST_LOCK),
                        str(events),
                        name,
                    ],
                )
                for name in ("one", "two")
            ]
            try:
                deadline = time.monotonic() + 10
                ready = tuple(root / f"ready-{name}" for name in ("one", "two"))
                while not all(path.is_file() for path in ready):
                    if time.monotonic() >= deadline:
                        self.fail("lock probe readiness timed out")
                    time.sleep(0.01)
                root.joinpath("start").write_text("start\n")
                self.assertEqual(
                    [process.wait(timeout=30) for process in processes], [0, 0]
                )
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=5)
            records = [json.loads(line) for line in events.read_text().splitlines()]
            self.assertEqual(
                [record["event"] for record in records],
                ["enter", "exit", "enter", "exit"],
            )
            self.assertEqual({record["name"] for record in records}, {"one", "two"})
            self.assertEqual(records[0]["name"], records[1]["name"])
            self.assertEqual(records[2]["name"], records[3]["name"])
            self.assertNotEqual(records[0]["name"], records[2]["name"])
            self.assertGreaterEqual(
                records[1]["time_ns"] - records[0]["time_ns"], 350_000_000
            )
            self.assertGreaterEqual(records[2]["time_ns"], records[1]["time_ns"])

    def test_cgroup_cleanup_covers_nested_descendants_on_every_outcome(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-containment-", dir="/tmp/tfa-phase2"
        ) as temporary:
            for scenario in ("success", "failure", "timeout"):
                with self.subTest(scenario=scenario):
                    value = _production_probe(scenario, Path(temporary) / scenario)
                    self.assertTrue(value["valid"], value)
                    self.assertEqual(value["scenario"], scenario)
                    self.assertTrue(value["nested_descendant"])
                    self.assertTrue(value["containment_empty"])
                    self.assertTrue(value["hierarchy_removed"])
                    self.assertTrue(value["ready"])
                    self.assertTrue(value["readiness_sealed"])
                    self.assertFalse(value["survived"])
                    self.assertEqual(value["containment"], CONTAINMENT)
                    process_group = value["process_group"]
                    self.assertIsInstance(process_group, int)
                    if scenario == "timeout":
                        with self.assertRaises(ProcessLookupError):
                            os.killpg(process_group, 0)

    def test_readiness_phase_has_a_bounded_startup_deadline(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-readiness-timeout-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(ProcessError, "readiness timed out"):
                run_process(
                    "/usr/local/bin/fbpython",
                    ("-c", "import time; time.sleep(30)"),
                    root,
                    root / "run",
                    30,
                    readiness_path=root / "never-ready",
                    startup_timeout_seconds=1,
                )
            metadata = json.loads((root / "run" / "metadata.json").read_text())
            self.assertFalse(Path(metadata["cgroup"]).exists())

    def test_fast_failure_keeps_host_identity_and_cleans_up(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-root-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            result = run_process(
                "/usr/local/bin/fbpython",
                (
                    "-c",
                    "import json,os,sys; "
                    "print(json.dumps({'uid':os.getuid(),'euid':os.geteuid()})); "
                    "sys.exit(7)",
                ),
                root,
                root / "run",
                10,
            )
            self.assertEqual(result.returncode, 7, result.stderr_path.read_text())
            self.assertEqual(
                json.loads(result.stdout_path.read_text()),
                {"euid": os.geteuid(), "uid": os.getuid()},
            )
            self.assertNotEqual(os.geteuid(), 0)
            self.assertEqual(result.containment, CONTAINMENT)
            self.assertFalse(
                Path(
                    json.loads((root / "run" / "metadata.json").read_text())["cgroup"]
                ).exists()
            )

    @_serialized_controller_handoff
    def test_real_codex_bootstraps_with_bound_agent_identity(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-codex-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            environment = diagnostic_environment(root / "logs")
            environment["META_EXEC_WRAPPER"] = "/bin/false"
            result = run_process(
                "/usr/local/bin/codex",
                ("--version",),
                root,
                root / "run",
                30,
                environment,
            )
            self.assertEqual(result.returncode, 0, result.stderr_path.read_text())
            self.assertIn("codex", result.stdout_path.read_text().lower())
            self.assertNotIn("META_CLAUDE_ROOT_ALLOWED", environment)
            self.assertNotIn("THRIFT_TLS_CL_CERT_PATH", environment)
            self.assertEqual(environment["META_EXEC_WRAPPER"], "/bin/false")
            metadata = json.loads((root / "run" / "metadata.json").read_text())
            self.assertTrue(metadata["containment_empty"])
            sentinel = Path(metadata["payload_sentinel"]["path"])
            status = Path(metadata["payload_status"]["path"])
            sentinel_pid, sentinel_cgroup = sentinel.read_text().split()
            self.assertTrue(sentinel_pid.isdigit())
            self.assertEqual(sentinel_cgroup, metadata["cgroup"])
            returncode, descendants, payload_pid = status.read_text().split()
            self.assertEqual((returncode, descendants), ("0", "0"))
            self.assertEqual(int(payload_pid), metadata["payload_pid"])
            self.assertEqual(sentinel.parent, root / "run")
            self.assertEqual(status.parent, root / "run")
            self.assertEqual(
                metadata["payload_runtime"]["kind"], "sealed-memfd-runtime"
            )
            self.assertEqual(metadata["outer_filesystem_isolation"], "none")
            self.assertEqual(metadata["outer_network_isolation"], "none")
            self.assertEqual(
                metadata["payload_isolation"], "codex-native-bootstrap-only"
            )
            self.assertFalse(Path(metadata["cgroup"]).exists())

    @_serialized_controller_handoff
    def test_real_codex_native_workspace_sandbox_and_tfa_cleanup(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with (
            tempfile.TemporaryDirectory(
                prefix="tfa-production-codex-native-", dir="/tmp/tfa-phase2"
            ) as temporary,
            tempfile.TemporaryDirectory(
                prefix="tfa-codex-native-forbidden-", dir="/dev/shm"
            ) as forbidden_temporary,
        ):
            root = Path(temporary)
            checkout = root / "checkout"
            checkout.mkdir()
            control = root / "control"
            control.mkdir()
            agent_process = control / "diagnostic-agent"
            agent_process.mkdir()
            agent_output = root / "agent-output"
            agent_output.mkdir()
            allowed = checkout / "tool-nonce"
            forbidden = Path(forbidden_temporary) / "host-visible-write"
            schema = control / "schema.json"
            report = agent_process / "final-message.json"
            schema.write_text(
                json.dumps(
                    {
                        "type": "object",
                        "additionalProperties": False,
                        "required": ["nonce", "forbidden", "network", "cgroup"],
                        "properties": {
                            "nonce": {"type": "string"},
                            "forbidden": {"type": "string"},
                            "network": {"type": "string"},
                            "cgroup": {"type": "string"},
                        },
                    }
                )
            )
            tool_source = "\n".join(
                (
                    "import errno, json, pathlib, secrets, socket",
                    f"allowed = pathlib.Path({str(allowed)!r})",
                    f"forbidden = pathlib.Path({str(forbidden)!r})",
                    "nonce = secrets.token_hex(24)",
                    "allowed.write_text(nonce)",
                    "try:",
                    "    forbidden.write_text(nonce)",
                    "    forbidden_result = 'private-write-succeeded'",
                    "except OSError as error:",
                    "    forbidden_result = f'errno:{error.errno}'",
                    "try:",
                    "    socket.socket(socket.AF_INET, socket.SOCK_STREAM)",
                    "    network_result = 'socket-succeeded'",
                    "except OSError as error:",
                    "    network_result = f'errno:{error.errno}'",
                    (
                        "relative = pathlib.Path('/proc/self/cgroup').read_text()"
                        ".strip().removeprefix('0::/')"
                    ),
                    (
                        "cgroup_probe = pathlib.Path('/sys/fs/cgroup') / relative / "
                        "'native-sandbox-probe'"
                    ),
                    "try:",
                    "    cgroup_probe.mkdir()",
                    "    cgroup_result = 'mkdir-succeeded'",
                    "except OSError as error:",
                    "    cgroup_result = f'errno:{error.errno}'",
                    (
                        "print(json.dumps({'nonce': nonce, "
                        "'forbidden': forbidden_result, 'network': network_result, "
                        "'cgroup': cgroup_result}, sort_keys=True))"
                    ),
                )
            )
            command = f"/usr/local/bin/fbpython -c {shlex.quote(tool_source)}"
            prompt = (
                "Run this exact command with the shell tool exactly once. "
                "Do not invent or alter its result. Return only the exact JSON object "
                f"printed by the command:\n{command}"
            )
            arguments = codex_arguments(prompt, checkout, schema, report, None)
            result = run_process(
                "/usr/local/bin/codex",
                arguments,
                checkout,
                agent_process,
                120,
                diagnostic_environment(root / "logs"),
            )
            self.assertEqual(result.returncode, 0, result.stderr_path.read_text())
            value = json.loads(report.read_text())
            self.assertRegex(value["nonce"], re.compile(r"^[0-9a-f]{48}$"))
            self.assertEqual(allowed.read_text(), value["nonce"])
            self.assertFalse(forbidden.exists())
            self.assertIn(
                value["forbidden"],
                {
                    "private-write-succeeded",
                    f"errno:{errno.EACCES}",
                    f"errno:{errno.ENOENT}",
                    f"errno:{errno.EROFS}",
                },
            )
            self.assertEqual(value["network"], f"errno:{errno.EPERM}")
            self.assertIn(
                value["cgroup"],
                {
                    f"errno:{errno.EACCES}",
                    f"errno:{errno.ENOENT}",
                    f"errno:{errno.EROFS}",
                },
            )
            metadata = json.loads((agent_process / "metadata.json").read_text())
            preflight_record = metadata["codex_preflight"]
            self.assertIsInstance(preflight_record, dict)
            preflight = json.loads(Path(preflight_record["path"]).read_text())
            self.assertEqual(preflight["executable"], "/usr/local/bin/codex")
            self.assertRegex(
                preflight["version"], r"^codex-cli [0-9]+\.[0-9]+\.[0-9]+$"
            )
            self.assertEqual(preflight["state"], "eligible")
            self.assertIsNone(preflight["blocker"])
            self.assertEqual(metadata["outer_filesystem_isolation"], "none")
            self.assertEqual(metadata["outer_network_isolation"], "none")
            self.assertEqual(
                metadata["payload_isolation"], "codex-native-workspace-write"
            )
            self.assertTrue(metadata["containment_empty"])
            self.assertFalse(Path(metadata["cgroup"]).exists())

    @_serialized_controller_handoff
    def test_real_codex_candidate_handoff_uses_controller_buck(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        source_root = _worktree_source_root()
        with _preserving_production_directory(
            "tfa-production-controller-handoff-", "controller-handoff"
        ) as root:
            checkout = root / "case-checkout"
            case_dir = root / "artifacts" / "handoff"
            with _registered_worktree(
                source_root,
                root,
                checkout,
                f"tfa-handoff-{os.getpid()}",
            ):
                revision = TrustedSubprocessWithList.run(
                    executable="/usr/local/bin/sl",
                    cmd_args=[
                        "log",
                        "-r",
                        ".",
                        "-T",
                        "{node}\\n",
                        "--reason",
                        "record production handoff base - sl help log",
                    ],
                    cwd=checkout,
                    capture_output=True,
                    text=True,
                    timeout=60,
                )
                self.assertEqual(revision.returncode, 0, revision.stderr)
                case = replace(
                    load_manifest(
                        Path(manifest_module.__file__).with_name("cases.v1.json")
                    ).case("asyncssl"),
                    source_revision=revision.stdout.strip(),
                )
                agent_output = case_dir / "agent-output"
                agent_process = case_dir / "control" / "diagnostic-agent"
                report = agent_output / "report.json"
                forbidden = checkout / "agent-must-not-write"
                probe_path = "fbcode/tfa_controller_handoff_probe.txt"
                applied = checkout / probe_path
                schema = source_root / "fbcode/testinfra/tfa/report.schema.v1.json"
                review_schema = (
                    source_root / "fbcode/testinfra/tfa/review.schema.v1.json"
                )
                source = _controller_handoff_source(
                    agent_output,
                    forbidden,
                    probe_path,
                    case.case_id,
                )
                command = _controller_handoff_command(source)
                prompt = (
                    "Run this exact command with the shell tool exactly once. "
                    "Return only its exact JSON object:\n"
                    f"{command}"
                )
                with (
                    patch("testinfra.tfa.pipeline.render_prompt", return_value=prompt),
                    checkout_lease(checkout),
                ):
                    candidate = _diagnose_candidate_locked(
                        case, case_dir, checkout, schema, 120, True, review_schema
                    )
                value = json.loads(report.read_text())
                nonce = value["hypotheses"][0]["supporting"]
                self.assertRegex(nonce, re.compile(r"^[0-9a-f]{48}$"))
                self.assertRegex(
                    value["hypotheses"][1]["supporting"],
                    rf"^errno:\d+,errno:{errno.EPERM}$",
                )
                self.assertFalse(forbidden.exists())
                metadata = json.loads((agent_process / "metadata.json").read_text())
                self.assertTrue(metadata["containment_empty"])
                self.assertFalse(Path(metadata["cgroup"]).exists())
                self.assertEqual(candidate.case_id, case.case_id)
                self.assertEqual(applied.read_text(), nonce + "\n")
                transaction = json.loads(
                    (
                        case_dir
                        / "control"
                        / "candidate-attempts"
                        / "attempt-001"
                        / "transaction.json"
                    ).read_text()
                )
                self.assertEqual(transaction["phase"], "validating")
                attempt = transaction["attempt"]
                approval = json.loads(
                    (Path(attempt) / "candidate-approval.json").read_text()
                )
                self.assertEqual(approval["changed_paths"], [probe_path])
                self.assertEqual(
                    approval["diff_sha256"],
                    hashlib.sha256(
                        (Path(attempt) / "approved-patch.diff").read_bytes()
                    ).hexdigest(),
                )
                applied_paths = tuple(transaction["expected_paths"])
                applied_review = pipeline_module._independent_diff_review(
                    checkout,
                    applied_paths,
                    case_dir,
                    review_schema,
                )
                applied_hash = applied_review["diff_sha256"]
                self.assertIsInstance(applied_hash, str)
                pipeline_module._verify_independent_diff_review(
                    case_dir,
                    applied_paths,
                    str(applied_hash),
                )
                applied_review_process = Path(str(applied_review["process_dir"]))
                _assert_process_cleanup(applied_review_process)
                with (
                    patch("testinfra.tfa.pipeline.render_prompt") as render,
                    self.assertRaisesRegex(
                        RuntimeError, "run-case recovery is required"
                    ),
                ):
                    diagnose_candidate(case, case_dir, checkout, schema, 120)
                render.assert_not_called()
                with checkout_lease(checkout):
                    recovered = _recover_candidate_transaction(case, case_dir, checkout)
                self.assertEqual(recovered, candidate)
                build = run_process(
                    "/usr/local/bin/buck",
                    ("build", "fbcode//testinfra/tfa:tfa_phase2_lib"),
                    checkout,
                    case_dir / "control" / "build",
                    600,
                )
                self.assertEqual(build.returncode, 0, build.stderr_path.read_text())
                build_metadata = json.loads(
                    (case_dir / "control" / "build" / "metadata.json").read_text()
                )
                self.assertTrue(build_metadata["containment_empty"])
                self.assertFalse(Path(build_metadata["cgroup"]).exists())

    @_serialized_controller_handoff
    def test_real_codex_final_message_cannot_be_redirected_by_tool(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with _preserving_production_directory(
            "tfa-production-codex-report-", "codex-report"
        ) as root:
            checkout = root / "checkout"
            checkout.mkdir()
            control = root / "control"
            agent_process = control / "diagnostic-agent"
            agent_process.mkdir(parents=True)
            agent_output = root / "agent-output"
            agent_output.mkdir()
            final_message = agent_process / "final-message.json"
            destination = agent_output / "report.json"
            external = root / "control-target"
            external.write_text("sentinel\n")
            schema = root / "schema.json"
            schema.write_text(
                json.dumps(
                    {
                        "type": "object",
                        "additionalProperties": False,
                        "required": ["nonce", "report_link"],
                        "properties": {
                            "nonce": {"type": "string"},
                            "report_link": {"type": "string"},
                        },
                    }
                )
            )

            fresh_marker = checkout / "fresh-nonce"
            fresh_prompt = _report_redirect_prompt(fresh_marker, destination, external)
            fresh = run_process(
                "/usr/local/bin/codex",
                codex_arguments(fresh_prompt, checkout, schema, final_message, None),
                checkout,
                agent_process,
                120,
                diagnostic_environment(agent_output / "logs"),
            )
            self.assertEqual(fresh.returncode, 0, fresh.stderr_path.read_text())
            fresh_value = json.loads(final_message.read_text())
            self.assertRegex(fresh_value["nonce"], re.compile(r"^[0-9a-f]{48}$"))
            self.assertEqual(fresh_marker.read_text(), fresh_value["nonce"])
            self.assertEqual(fresh_value["report_link"], "symlink-succeeded")
            self.assertTrue(destination.is_symlink())
            with self.assertRaises(ProcessError):
                publish_codex_report(final_message, destination)
            self.assertEqual(external.read_text(), "sentinel\n")
            _assert_process_cleanup(agent_process)

            session = session_id_from_jsonl(fresh.stdout_path)
            if session is None:
                self.fail("fresh Codex run did not emit a session ID")
            resume_marker = checkout / "resume-nonce"
            resume_prompt = _report_redirect_prompt(
                resume_marker, destination, external
            )
            resumed = run_process(
                "/usr/local/bin/codex",
                codex_arguments(
                    resume_prompt, checkout, schema, final_message, session
                ),
                checkout,
                agent_process,
                120,
                diagnostic_environment(agent_output / "logs"),
            )
            self.assertEqual(resumed.returncode, 0, resumed.stderr_path.read_text())
            resumed_value = json.loads(final_message.read_text())
            self.assertRegex(resumed_value["nonce"], re.compile(r"^[0-9a-f]{48}$"))
            self.assertEqual(resume_marker.read_text(), resumed_value["nonce"])
            self.assertEqual(resumed_value["report_link"], "symlink-succeeded")
            with self.assertRaises(ProcessError):
                publish_codex_report(final_message, destination)
            self.assertEqual(external.read_text(), "sentinel\n")
            _assert_process_cleanup(agent_process)

            expected_report = final_message.read_bytes()
            destination.unlink()
            publish_codex_report(final_message, destination)
            self.assertEqual(destination.read_bytes(), expected_report)
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o600)
            self.assertEqual(destination.stat().st_nlink, 1)

    @_serialized_controller_handoff
    def test_real_codex_reviewer_final_message_cannot_be_redirected(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with _preserving_production_directory(
            "tfa-production-reviewer-report-", "reviewer-report"
        ) as root:
            external = root / "control-target"
            external.write_text("sentinel\n")
            schema = root / "schema.json"
            schema.write_text(
                json.dumps(
                    {
                        "type": "object",
                        "additionalProperties": False,
                        "required": ["nonce", "report_link"],
                        "properties": {
                            "nonce": {"type": "string"},
                            "report_link": {"type": "string"},
                        },
                    }
                )
            )
            reviewer_output = root / "reviewer-output"
            reviewer_workspace = reviewer_output / "workspace"
            reviewer_workspace.mkdir(parents=True)
            reviewer_report = reviewer_output / "review.json"
            review_marker = reviewer_workspace / "review-nonce"
            review_prompt = _report_redirect_prompt(
                review_marker, reviewer_report, external
            )
            review_process = root / "control" / "review-process"
            review = run_process(
                "/usr/local/bin/codex",
                codex_review_arguments(
                    review_prompt, reviewer_workspace, schema, reviewer_report
                ),
                reviewer_workspace,
                review_process,
                120,
                diagnostic_environment(reviewer_output / "logs"),
            )
            self.assertEqual(review.returncode, 0, review.stderr_path.read_text())
            review_value = json.loads(reviewer_report.read_text())
            self.assertRegex(review_value["nonce"], re.compile(r"^[0-9a-f]{48}$"))
            self.assertEqual(review_marker.read_text(), review_value["nonce"])
            self.assertRegex(review_value["report_link"], r"^errno:\d+$")
            self.assertFalse(reviewer_report.is_symlink())
            self.assertEqual(external.read_text(), "sentinel\n")
            _assert_process_cleanup(review_process)

    def test_changed_runtime_bytes_fail_closed_before_launch(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-runtime-negative-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            original_read_bytes = Path.read_bytes
            for name in ("payload_wrapper.py", "process_supervisor.py"):
                with self.subTest(name=name):
                    artifact_dir = root / name
                    cgroups_before = _owned_process_cgroups()

                    def changed_bytes(path: Path, changed_name: str = name) -> bytes:
                        content = original_read_bytes(path)
                        return content + b"\n" if path.name == changed_name else content

                    with (
                        patch.object(Path, "read_bytes", changed_bytes),
                        self.assertRaisesRegex(
                            ProcessError, f"runtime identity differs: {name}"
                        ),
                    ):
                        run_process(
                            "/usr/local/bin/fbpython",
                            ("-c", "raise SystemExit('must not execute')"),
                            root,
                            artifact_dir,
                            10,
                        )
                    self.assertEqual(
                        json.loads((artifact_dir / "metadata.json").read_text())[
                            "state"
                        ],
                        "running",
                    )
                    self.assertEqual((artifact_dir / "stdout.txt").read_text(), "")
                    self.assertEqual(cgroups_before, _owned_process_cgroups())

    def test_nested_run_process_in_transient_scope_scrubs_exec_wrapper(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-transient-scope-", dir="/tmp/tfa-phase2"
        ) as temporary:
            helper = Path(__file__).with_name("test_process_migration_helper.py")
            completed = TrustedSubprocessWithList.run(
                executable="/usr/bin/systemd-run",
                cmd_args=[
                    "--user",
                    "--scope",
                    "--quiet",
                    "--collect",
                    "--same-dir",
                    str(helper),
                    "--transient-scope-probe",
                    temporary,
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            value = json.loads(completed.stdout.splitlines()[-1])
            self.assertTrue(value["valid"], value)
            self.assertIn(".scope", value["scope"])
            self.assertIsNone(value["meta_exec_wrapper"])

    @_serialized_nested_hermit
    def test_normal_verify_keeps_guest_and_controller_workdirs_clean(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-hermit-normal-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            guest_cwd = root / "guest-cwd"
            guest_seal = root / "guest-cwd.json"
            seal_working_directory(guest_cwd, guest_seal)
            attempt = root / "attempt"
            helper = Path(__file__).with_name("test_process_migration_helper.py")
            completed = TrustedSubprocessWithList.run(
                executable="/usr/bin/systemd-run",
                cmd_args=[
                    "--user",
                    "--quiet",
                    "--collect",
                    "--pipe",
                    "--wait",
                    "--expand-environment=no",
                    f"--setenv={_VERIFY_SPACE_ROOT_ENV}="
                    f"{os.environ[_VERIFY_SPACE_ROOT_ENV]}",
                    f"--working-directory={Path.cwd()}",
                    "--",
                    "/usr/local/bin/fbpython",
                    str(helper),
                    "--hermit-quota-probe",
                    "normal",
                    str(root),
                    str(guest_cwd),
                    _hermit_binary(),
                ],
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            receipt = json.loads((root / "hermit-quota-probe.json").read_text())
            self.assertEqual(
                receipt["returncode"], 0, (attempt / "wrapper/stderr.txt").read_text()
            )
            self.assertFalse(receipt["timed_out"])
            argv = tuple(receipt["argv"])
            verify_working_directory(guest_cwd, guest_seal)
            evidence = controller_evidence(attempt)
            self.assertEqual(evidence["outputs"], [])
            self.assertEqual(evidence["verify_summary_residue"], [])
            self.assertIn(f"--workdir={guest_cwd}", argv)
            self.assertIn("--timeout=20", argv)
            self.assertEqual(argv[argv.index("--") + 1], "/usr/bin/python3")
            metadata = json.loads((attempt / "wrapper/metadata.json").read_text())
            hermit_sha256 = hashlib.sha256(
                Path(_hermit_binary()).read_bytes()
            ).hexdigest()
            bounded_evidence = safehermit_evidence(
                argv,
                metadata,
                attempt / "safehermit.txt",
                hermit_sha256,
            )
            self.assertEqual(bounded_evidence["exit_code"], 0)
            verify_arg = next(
                token.removeprefix("--verify-log-dir=")
                for token in argv
                if token.startswith("--verify-log-dir=")
            )
            self.assertTrue(Path(verify_arg).parent.name.startswith("brs-tfa-verify-"))
            self.assertFalse(Path(verify_arg).parent.exists())
            verify_evidence = bounded_evidence["verify_output"]
            self.assertIsInstance(verify_evidence, dict)
            self.assertEqual(verify_evidence["phase"], "finalized")
            self.assertEqual(verify_evidence["report"]["returncode"], 0)
            self.assertEqual(verify_evidence["destroy"]["returncode"], 0)
            bounded_commands = _bounded_command_metadata(attempt)
            self.assertEqual(len(bounded_commands), 3)
            self.assertTrue(
                all(
                    metadata["containment_empty"] is True
                    and metadata["timed_out"] is False
                    and not Path(str(metadata["cgroup"])).exists()
                    for metadata in bounded_commands
                )
            )
            self.assertTrue((attempt / "verify").is_dir())
            self.assertEqual(Path(argv[0]), safehermit_runtime_paths(attempt)[0])
            self.assertTrue(
                _pinned_safehermit_allowed(argv[0], attempt / "controller-cwd")
            )
            bounded_run_space = safehermit_runtime_paths(attempt)[1]
            self.assertTrue(_pinned_safehermit_allowed(str(bounded_run_space), attempt))
            for runtime_input in safehermit_runtime_paths(attempt):
                with self.subTest(runtime_input=runtime_input.name):
                    original = runtime_input.read_bytes()
                    runtime_input.unlink()
                    runtime_input.write_bytes(original + b"\n# replaced\n")
                    runtime_input.chmod(0o500)
                    self.assertFalse(
                        _pinned_safehermit_allowed(argv[0], attempt / "controller-cwd")
                    )
                    self.assertFalse(
                        _pinned_safehermit_allowed(str(bounded_run_space), attempt)
                    )
                    with self.assertRaisesRegex(HermitError, "execution input differs"):
                        safehermit_evidence(
                            argv,
                            metadata,
                            attempt / "safehermit.txt",
                            hermit_sha256,
                        )
                    runtime_input.unlink()
                    runtime_input.write_bytes(original)
                    runtime_input.chmod(0o500)

    @_serialized_nested_hermit
    def test_forced_run2_kill_confines_and_seals_verify_residue(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-hermit-run2-", dir="/tmp/tfa-phase2"
        ) as temporary:
            root = Path(temporary)
            guest_cwd = root / "guest-cwd"
            guest_seal = root / "guest-cwd.json"
            seal_working_directory(guest_cwd, guest_seal)
            attempt = root / "attempt"
            controller_cwd = attempt / "controller-cwd"
            ready = controller_cwd / "run2"
            helper = Path(__file__).with_name("test_process_migration_helper.py")
            completed = TrustedSubprocessWithList.run(
                executable="/usr/bin/systemd-run",
                cmd_args=[
                    "--user",
                    "--quiet",
                    "--collect",
                    "--pipe",
                    "--wait",
                    "--expand-environment=no",
                    f"--setenv={_VERIFY_SPACE_ROOT_ENV}="
                    f"{os.environ[_VERIFY_SPACE_ROOT_ENV]}",
                    f"--working-directory={Path.cwd()}",
                    "--",
                    "/usr/local/bin/fbpython",
                    str(helper),
                    "--hermit-quota-probe",
                    "forced-run2",
                    str(root),
                    str(guest_cwd),
                    _hermit_binary(),
                ],
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            receipt = json.loads((root / "hermit-quota-probe.json").read_text())
            self.assertEqual(
                receipt["returncode"],
                124,
                (attempt / "wrapper/stderr.txt").read_text(),
            )
            self.assertFalse(receipt["timed_out"])
            self.assertTrue(ready.is_file())
            verify_working_directory(guest_cwd, guest_seal)
            evidence = controller_evidence(attempt)
            residues = evidence["verify_summary_residue"]
            self.assertIsInstance(residues, list)
            self.assertGreaterEqual(len(residues), 1)
            self.assertTrue(
                all(
                    Path(str(record["path"])).parent == controller_cwd
                    for record in residues
                )
            )
            self.assertTrue((attempt / "verify").is_dir())
            self.assertTrue(any((attempt / "verify").iterdir()))

    @_serialized_nested_hermit
    def test_killed_controller_recovers_every_verify_allocation_boundary(
        self,
    ) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        helper = Path(__file__).with_name("test_process_migration_helper.py")
        boundaries = (
            "create",
            "record",
            "launch",
            "report",
            "copy",
            "destroy",
            "final-write",
        )
        for boundary in boundaries:
            with (
                self.subTest(boundary=boundary),
                _preserving_production_directory(
                    f"tfa-production-hermit-crash-{boundary}-",
                    f"hermit-crash-{boundary}",
                ) as root,
            ):
                guest_cwd = root / "guest-cwd"
                guest_cwd.mkdir()
                crash = TrustedSubprocessWithList.run(
                    executable="/usr/bin/systemd-run",
                    cmd_args=[
                        "--user",
                        "--quiet",
                        "--collect",
                        "--pipe",
                        "--wait",
                        "--expand-environment=no",
                        f"--setenv={_VERIFY_SPACE_ROOT_ENV}="
                        f"{os.environ[_VERIFY_SPACE_ROOT_ENV]}",
                        f"--working-directory={Path.cwd()}",
                        "--",
                        "/usr/local/bin/fbpython",
                        str(helper),
                        "--hermit-quota-crash-probe",
                        boundary,
                        str(root),
                        str(guest_cwd),
                        _hermit_binary(),
                    ],
                    capture_output=True,
                    text=True,
                    timeout=60,
                )
                self.assertNotEqual(crash.returncode, 0, crash.stdout)
                ready_path = root / f"crash-ready-{boundary}.json"
                self.assertTrue(ready_path.is_file(), crash.stderr)
                ready = json.loads(ready_path.read_text())
                self.assertEqual(ready["boundary"], boundary)
                transaction = ready["transaction"]
                candidates = ready["candidates"]
                self.assertIsInstance(candidates, list)
                if transaction is None:
                    self.assertEqual(len(candidates), 1)
                    allocation = Path(candidates[0])
                else:
                    self.assertIsInstance(transaction, dict)
                    allocation = Path(transaction["verify_log_dir"]).parent
                self.assertEqual(
                    allocation.exists(),
                    boundary not in {"destroy", "final-write"},
                )

                recovery = TrustedSubprocessWithList.run(
                    executable="/usr/bin/systemd-run",
                    cmd_args=[
                        "--user",
                        "--quiet",
                        "--collect",
                        "--pipe",
                        "--wait",
                        "--expand-environment=no",
                        f"--setenv={_VERIFY_SPACE_ROOT_ENV}="
                        f"{os.environ[_VERIFY_SPACE_ROOT_ENV]}",
                        f"--working-directory={Path.cwd()}",
                        "--",
                        "/usr/local/bin/fbpython",
                        str(helper),
                        "--hermit-quota-recover",
                        boundary,
                        str(root),
                        str(guest_cwd),
                        _hermit_binary(),
                    ],
                    capture_output=True,
                    text=True,
                    timeout=60,
                )
                self.assertEqual(recovery.returncode, 0, recovery.stderr)
                recovered = json.loads((root / f"recovery-{boundary}.json").read_text())
                self.assertIn("was recovered", recovered["error"])
                self.assertFalse(allocation.exists())
                bounded_commands = _bounded_command_metadata(root / "attempt")
                self.assertGreaterEqual(len(bounded_commands), 1)
                self.assertTrue(
                    all(
                        metadata["containment_empty"] is True
                        and metadata["timed_out"] is False
                        and not Path(str(metadata["cgroup"])).exists()
                        for metadata in bounded_commands
                    )
                )
                if boundary == "create":
                    self.assertTrue(recovered["orphan_recovery"])
                    self.assertIsNone(recovered["transaction"])
                else:
                    self.assertFalse(recovered["orphan_recovery"])
                    self.assertEqual(recovered["transaction"]["phase"], "recovered")

    @_serialized_nested_hermit
    def test_inflight_bounded_commands_hold_recovery_until_quiescent(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        helper = Path(__file__).with_name("test_process_migration_helper.py")
        for boundary in ("create", "report", "destroy"):
            with (
                self.subTest(boundary=boundary),
                _preserving_production_directory(
                    f"tfa-production-hermit-inflight-{boundary}-",
                    f"hermit-inflight-{boundary}",
                ) as root,
            ):
                guest_cwd = root / "guest-cwd"
                guest_cwd.mkdir()
                ready_path = root / f"inflight-ready-{boundary}.json"
                release = root / f"inflight-release-{boundary}"
                controller = TrustedSubprocessWithList.Popen(
                    executable="/usr/local/bin/fbpython",
                    cmd_args=[
                        str(helper),
                        "--hermit-inflight-crash-probe",
                        boundary,
                        str(root),
                        str(guest_cwd),
                        _hermit_binary(),
                    ],
                    start_new_session=True,
                )
                recovery: subprocess.Popen[bytes] | None = None
                try:
                    deadline = time.monotonic() + 20
                    while not ready_path.is_file():
                        if controller.poll() is not None:
                            self.fail(
                                f"in-flight controller exited {controller.returncode}"
                            )
                        if time.monotonic() >= deadline:
                            self.fail("in-flight command readiness timed out")
                        time.sleep(0.01)
                    ready = json.loads(ready_path.read_text())
                    self.assertEqual(ready["operation"], boundary)
                    controller_pid = ready["controller_pid"]
                    worker_pid = ready["worker_pid"]
                    self.assertIsInstance(controller_pid, int)
                    self.assertIsInstance(worker_pid, int)
                    self.assertTrue(Path(f"/proc/{worker_pid}").exists())

                    os.kill(controller_pid, signal.SIGKILL)
                    self.assertNotEqual(controller.wait(timeout=5), 0)
                    recovery = TrustedSubprocessWithList.Popen(
                        executable="/usr/local/bin/fbpython",
                        cmd_args=[
                            str(helper),
                            "--hermit-quota-recover",
                            boundary,
                            str(root),
                            str(guest_cwd),
                            _hermit_binary(),
                        ],
                        start_new_session=True,
                    )
                    time.sleep(0.5)
                    self.assertIsNone(recovery.poll())
                    self.assertFalse((root / f"recovery-{boundary}.json").exists())
                    release.write_text("release\n")
                    self.assertEqual(recovery.wait(timeout=60), 0)
                    recovered = json.loads(
                        (root / f"recovery-{boundary}.json").read_text()
                    )
                    self.assertIn("was recovered", recovered["error"])

                    deadline = time.monotonic() + 5
                    while Path(f"/proc/{worker_pid}").exists():
                        if time.monotonic() >= deadline:
                            self.fail("bounded command worker did not exit")
                        time.sleep(0.01)
                    attempt = root / "attempt"
                    key = hashlib.sha256(
                        os.fsencode(attempt.resolve(strict=True))
                    ).hexdigest()[:24]
                    prefix = f"brs-tfa-verify-v2-{key}-"
                    bounded_root = hermit_module._safehermit_log_root(attempt)
                    self.assertFalse(
                        any(
                            path.name.startswith(prefix)
                            for path in bounded_root.iterdir()
                        )
                    )
                    metadata = _bounded_command_metadata(attempt)
                    self.assertGreaterEqual(len(metadata), 1)
                    self.assertTrue(
                        all(
                            value["containment_empty"] is True
                            and value["timed_out"] is False
                            and not Path(str(value["cgroup"])).exists()
                            for value in metadata
                        )
                    )
                finally:
                    release.write_text("release\n")
                    if controller.poll() is None:
                        os.kill(controller.pid, signal.SIGKILL)
                        controller.wait(timeout=5)
                    if recovery is not None and recovery.poll() is None:
                        recovery.wait(timeout=60)

    @_serialized_nested_hermit
    def test_worker_response_timeout_keeps_command_lock_until_exit(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        helper = Path(__file__).with_name("test_process_migration_helper.py")
        with _preserving_production_directory(
            "tfa-production-hermit-worker-timeout-",
            "hermit-worker-timeout",
        ) as root:
            guest_cwd = root / "guest-cwd"
            guest_cwd.mkdir()
            ready_path = root / "worker-timeout-ready.json"
            release = root / "worker-timeout-release"
            controller = TrustedSubprocessWithList.Popen(
                executable="/usr/local/bin/fbpython",
                cmd_args=[
                    str(helper),
                    "--hermit-worker-timeout-probe",
                    str(root),
                    str(guest_cwd),
                    _hermit_binary(),
                ],
                start_new_session=True,
            )
            lock_probe: subprocess.Popen[bytes] | None = None
            try:
                deadline = time.monotonic() + 10
                while not ready_path.is_file():
                    if controller.poll() is not None:
                        self.fail(f"timeout controller exited {controller.returncode}")
                    if time.monotonic() >= deadline:
                        self.fail("worker-timeout readiness timed out")
                    time.sleep(0.01)
                ready = json.loads(ready_path.read_text())
                self.assertEqual(ready["operation"], "create")
                worker_pid = ready["worker_pid"]
                self.assertIsInstance(worker_pid, int)
                time.sleep(1.5)
                self.assertIsNone(controller.poll())

                lock_started = root / "command-lock-started"
                lock_acquired = root / "command-lock-acquired"
                lock_probe = TrustedSubprocessWithList.Popen(
                    executable="/usr/local/bin/fbpython",
                    cmd_args=[
                        str(helper),
                        "--hermit-command-lock-probe",
                        str(lock_started),
                        str(lock_acquired),
                    ],
                    start_new_session=True,
                )
                deadline = time.monotonic() + 5
                while not lock_started.is_file():
                    if lock_probe.poll() is not None:
                        self.fail(f"command lock probe exited {lock_probe.returncode}")
                    if time.monotonic() >= deadline:
                        self.fail("command lock probe readiness timed out")
                    time.sleep(0.01)
                time.sleep(0.5)
                self.assertIsNone(lock_probe.poll())
                self.assertFalse(lock_acquired.exists())

                release.write_text("release\n")
                self.assertEqual(lock_probe.wait(timeout=30), 0)
                self.assertTrue(lock_acquired.is_file())
                self.assertNotEqual(controller.wait(timeout=60), 0)
                orphan_path = root / "attempt/verify-output-orphan-recovery.json"
                self.assertTrue(orphan_path.is_file())
                orphan = json.loads(orphan_path.read_text())
                self.assertTrue(
                    all(
                        not Path(value["path"]).exists()
                        for value in orphan["allocations"]
                    )
                )
                deadline = time.monotonic() + 5
                while Path(f"/proc/{worker_pid}").exists():
                    if time.monotonic() >= deadline:
                        self.fail("timed-out bounded command worker did not exit")
                    time.sleep(0.01)
                metadata = _bounded_command_metadata(root / "attempt")
                self.assertGreaterEqual(len(metadata), 2)
                self.assertTrue(
                    all(
                        value["containment_empty"] is True
                        and value["timed_out"] is False
                        and not Path(str(value["cgroup"])).exists()
                        for value in metadata
                    )
                )
            finally:
                release.write_text("release\n")
                if controller.poll() is None:
                    controller.wait(timeout=60)
                if lock_probe is not None and lock_probe.poll() is None:
                    lock_probe.wait(timeout=30)
