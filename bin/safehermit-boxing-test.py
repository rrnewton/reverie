#!/usr/bin/env python3
"""Acceptance tests for safehermit's already-boxed check and its in-place mode.

Needs neither systemd nor a Hermit binary. Each case builds a fake cgroup v2
tree and a fake /proc/self/cgroup, handed to safehermit through the test-only
SAFEHERMIT_TEST_CGROUP_ROOT and SAFEHERMIT_TEST_PROC_SELF_CGROUP. Fake
`systemd-run` and `systemctl` executables come first in PATH and record every
call, so a case can prove that the run was boxed (a non-probe systemd-run call
naming the run's unit) or that it ran in place (no systemd call at all). The
"hermit binary" is this Python interpreter running a small payload.

Negative cases matter as much as the positive one: a caller whose limits do not
qualify must still be boxed exactly as before.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
from typing import Callable


ROOT = Path(__file__).resolve().parent.parent
SAFEHERMIT = ROOT / "bin" / "safehermit"
PYTHON = sys.executable
RUN_TIMEOUT_SECONDS = 60.0
QUALIFYING = {"memory.max": "8589934592", "cpu.max": "400000 100000"}


def _write_executable(path: Path, body: str) -> None:
    path.write_text(textwrap.dedent(body).lstrip(), encoding="utf-8")
    path.chmod(0o755)


class Case:
    def __init__(self, base: Path) -> None:
        self.base = base
        self.calls = base / "calls.jsonl"
        self.tools = base / "tools"
        self.tools.mkdir()
        self.systemd_available = True
        self.cgroup_root = base / "cgroup"
        self.proc_file = base / "proc-self-cgroup"
        self.extra_env: dict[str, str] = {}

    def fake_systemd(self, available: bool = True) -> None:
        self.systemd_available = available
        if not available:
            _write_executable(self.tools / "systemd-run", """
                #!/bin/sh
                echo "fake systemd-run: unavailable" >&2
                exit 127
                """)
            return
        # Records the call, succeeds for capability probes, and otherwise runs
        # the command after `--` directly: enough for safehermit to take its
        # boxed path end to end without a user manager.
        _write_executable(self.tools / "systemd-run", f"""
            #!{PYTHON}
            import json, os, sys
            args = sys.argv[1:]
            with open({str(self.calls)!r}, "a", encoding="utf-8") as stream:
                stream.write(json.dumps(["systemd-run", *args]) + "\\n")
            command = args[args.index("--") + 1:]
            unit = next((a.split("=", 1)[1] for a in args if a.startswith("--unit=")), "")
            if unit.startswith("safehermit-probe-") and command == ["/bin/true"]:
                raise SystemExit(0)
            os.execvp(command[0], command)
            """)
        _write_executable(self.tools / "systemctl", f"""
            #!{PYTHON}
            import json, sys
            args = sys.argv[1:]
            with open({str(self.calls)!r}, "a", encoding="utf-8") as stream:
                stream.write(json.dumps(["systemctl", *args]) + "\\n")
            if "show" in args and "--value" in args:
                print("success")
            """)

    def cgroups(self, own: str, limits: dict[str, dict[str, str | None]],
                controllers: tuple[str, ...] = ("memory", "cpu")) -> None:
        """Build a cgroup tree along `own`; every level defaults to "max".

        A limit value of None removes that file, as when a controller is not
        enabled for the cgroup. The root gets only the files named under "/":
        the host's real root cgroup has no limit files at all.
        """
        self.cgroup_root.mkdir()
        (self.cgroup_root / "cgroup.controllers").write_text(" ".join(controllers) + "\n")
        for name, value in limits.get("/", {}).items():
            if value is not None:
                (self.cgroup_root / name).write_text(value + "\n")
        parts = [part for part in own.split("/") if part and part not in (".", "..")]
        for depth in range(1, len(parts) + 1):
            group = "/" + "/".join(parts[:depth])
            directory = self.cgroup_root.joinpath(*parts[:depth])
            directory.mkdir(exist_ok=True)
            files: dict[str, str | None] = {}
            if "memory" in controllers:
                files.update({"memory.max": "max", "memory.high": "max"})
            if "cpu" in controllers:
                files["cpu.max"] = "max 100000"
            files.update(limits.get(group, {}))
            for name, value in files.items():
                if value is not None:
                    (directory / name).write_text(value + "\n")
        self.proc_file.write_text(f"0::{own}\n")

    def env(self) -> dict[str, str]:
        env = os.environ.copy()
        for name in ("SAFEHERMIT_BOXING", "SAFEHERMIT_TEST_CGROUP_ROOT",
                     "SAFEHERMIT_TEST_PROC_SELF_CGROUP", "HERMIT_BIN"):
            env.pop(name, None)
        env.update({
            "PATH": f"{self.tools}:{env['PATH']}",
            "SAFEHERMIT_LOG_ROOT": str(self.base / "logs"),
            "SAFEHERMIT_TEST_CGROUP_ROOT": str(self.cgroup_root),
            "SAFEHERMIT_TEST_PROC_SELF_CGROUP": str(self.proc_file),
            "DEV_HERMIT_EXCLUDE_CPUS": "none",
        })
        env.update(self.extra_env)
        return env

    def run(self, payload: str, *sh_flags: str, stdin: str = "") -> "Result":
        argv = [str(SAFEHERMIT), *sh_flags, PYTHON, "-c", textwrap.dedent(payload)]
        started = time.monotonic()
        completed = subprocess.run(
            argv, input=stdin, capture_output=True, text=True, env=self.env(),
            cwd=self.base, timeout=RUN_TIMEOUT_SECONDS, check=False,
        )
        return Result(completed, time.monotonic() - started, self.calls)


class Result:
    def __init__(self, completed: subprocess.CompletedProcess[str], seconds: float,
                 calls: Path) -> None:
        self.rc = completed.returncode
        self.stdout = completed.stdout
        self.stderr = completed.stderr
        self.seconds = seconds
        self.calls = [json.loads(line) for line in calls.read_text().splitlines()] \
            if calls.exists() else []
        self.fields: dict[str, str] = {}
        for line in completed.stderr.splitlines():
            if not line.startswith("safehermit: ") or "=" not in line:
                continue
            key, value = line[len("safehermit: "):].split("=", 1)
            if key.replace(".", "").replace("_", "").isalnum() and key.islower():
                assert key not in self.fields, f"duplicate field {key}:\n{completed.stderr}"
                self.fields[key] = value

    def context(self) -> str:
        return (f"rc={self.rc} seconds={self.seconds:.1f}\nstdout:\n{self.stdout}\n"
                f"stderr:\n{self.stderr}\ncalls:\n{json.dumps(self.calls, indent=1)}")

    def run_units(self) -> list[list[str]]:
        """systemd-run calls that launched the run itself, not a probe."""
        return [call for call in self.calls if call[0] == "systemd-run"
                and any(arg == f"--unit=safehermit-{self.fields.get('run_id')}" for arg in call)]

    def assert_boxed(self, reason_fragment: str) -> None:
        boxing = self.fields.get("boxing", "")
        assert boxing.startswith("SYSTEMD_RUN:"), self.context()
        assert reason_fragment in boxing, (reason_fragment, self.context())
        units = self.run_units()
        assert len(units) == 1, self.context()
        assert "--property=MemoryMax=16G" in units[0], self.context()
        assert self.fields.get("bound.cgroup") == "APPLIED:MemoryMax=16G MemorySwapMax=0", self.context()
        assert self.fields.get("unit") == f"safehermit-{self.fields.get('run_id')}", self.context()
        assert "in_place.outcome" not in self.fields, self.context()

    def assert_in_place(self, group: str, memory: str, cpu: str) -> None:
        expected = f"IN_PLACE:already boxed by {group} ({memory}, {cpu}); running in place"
        assert self.fields.get("boxing", "").startswith(expected), (expected, self.context())
        assert self.calls == [], self.context()
        assert self.fields.get("unit") == "none", self.context()
        assert self.fields.get("bound.cgroup", "").startswith("NOT_APPLIED:in place"), self.context()
        assert self.fields.get("bound.bytes", "").startswith("APPLIED:"), self.context()
        assert "LETHAL" in self.fields["bound.bytes"], self.context()


def _pid_gone(pid: int, within: float) -> bool:
    deadline = time.monotonic() + within
    while time.monotonic() < deadline:
        try:
            with open(f"/proc/{pid}/stat", "rb") as stream:
                state = stream.read().rsplit(b")", 1)[1].split()[0]
            if state == b"Z":
                return True
        except FileNotFoundError:
            return True
        time.sleep(0.05)
    return False


# ---------------------------------------------------------------- required cases (a)-(e)

def test_a_all_limits_max_boxes(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/agent/leaf", {})
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    result.assert_boxed("no cgroup from /agent/leaf up to the root limits memory or CPU")


def test_b_finite_memory_without_cpu_controller_boxes(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/agent/leaf", {"/agent": {"memory.max": "8589934592"}},
                 controllers=("memory",))
    assert not (case.cgroup_root / "agent" / "cpu.max").exists()
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    result.assert_boxed("/agent limits memory (memory.max=8589934592), "
                        "but no cgroup from /agent/leaf up to the root has a CPU quota")


def test_c_finite_cpu_with_memory_max_boxes(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/agent/leaf", {"/agent": {"cpu.max": "200000 100000"}})
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    result.assert_boxed("/agent limits CPU (cpu.max=200000 100000), "
                        "but no cgroup from /agent/leaf up to the root limits memory")


def test_d_ancestor_with_both_limits_runs_in_place(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box/agent/leaf", {"/box": dict(QUALIFYING)})
    case.extra_env["BOXING_TEST_SENTINEL"] = "inherited"
    result = case.run(
        """
        import os, sys
        print(sys.stdin.read().strip(), os.environ.get("BOXING_TEST_SENTINEL"))
        print(open("/proc/self/oom_score_adj").read().strip())
        sys.stderr.write("payload stderr\\n")
        """,
        stdin="stdin passes\n",
    )
    assert result.rc == 0, result.context()
    assert result.stdout == "stdin passes inherited\n1000\n", result.context()
    assert "payload stderr\n" in result.stderr, result.context()
    result.assert_in_place("/box", "memory.max=8589934592", "cpu.max=400000 100000")
    assert "TEST OVERRIDE SAFEHERMIT_TEST_CGROUP_ROOT=" in result.fields["boxing"], result.context()
    assert result.fields.get("bound.wall") == (
        "APPLIED:3600s (in place: the supervisor SIGKILLs the run's whole process tree)"
    ), result.context()
    assert result.fields.get("in_place.outcome") == "exit", result.context()
    assert result.fields.get("in_place.oom_score_adj") == "1000", result.context()
    assert result.fields.get("in_place.cleanup", "").startswith("tree_empty=true "), result.context()
    assert result.fields.get("exit_code") == "0", result.context()
    log = Path(result.fields["log_file"]).read_text()
    assert log == "payload stderr\n", log


def test_e_force_override_boxes_a_qualifying_caller(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box/agent/leaf", {"/box": dict(QUALIFYING)})
    case.extra_env["SAFEHERMIT_BOXING"] = "force"
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    result.assert_boxed("SAFEHERMIT_BOXING=force, so the already-boxed check was skipped")


# ---------------------------------------------------------------- detection edges

def test_split_limits_across_two_cgroups_do_not_qualify(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box/agent", {"/box": {"memory.max": "8589934592"},
                                "/box/agent": {"cpu.max": "400000 100000"}})
    result = case.run("print('payload ran')")
    assert result.rc == 0, result.context()
    result.assert_boxed("but no single cgroup limits both")


def test_memory_high_alone_counts_as_a_memory_limit(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box/agent", {"/box/agent": {"memory.high": "4294967296",
                                               "cpu.max": "100000 100000"}})
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    result.assert_in_place("/box/agent", "memory.high=4294967296", "cpu.max=100000 100000")


def test_dot_dot_in_the_cgroup_path_is_refused(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    case.proc_file.write_text("0::/box/../box\n")
    result = case.run("print('payload ran')")
    assert result.rc == 0, result.context()
    result.assert_boxed("refusing the cgroup path '/box/../box'")


def test_unreadable_proc_file_is_not_boxed(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    case.proc_file.unlink()
    result = case.run("print('payload ran')")
    assert result.rc == 0, result.context()
    result.assert_boxed(f"cannot read {case.proc_file}")


def test_unboxed_without_systemd_is_reported_unboxed_as_before(case: Case) -> None:
    case.fake_systemd(available=False)
    case.cgroups("/agent", {})
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    assert result.fields.get("boxing", "").startswith("NONE:not already boxed: "), result.context()
    assert "the run is UNBOXED" in result.fields["boxing"], result.context()
    assert result.fields.get("bound.wall", "").startswith("NOT_APPLIED:systemd --user unavailable"), result.context()
    assert result.fields.get("bound.cgroup") == "NOT_APPLIED:systemd --user unavailable", result.context()


def test_cgroup_namespace_root_with_both_limits_runs_in_place(case: Case) -> None:
    # Inside a container with a private cgroup namespace, /proc/self/cgroup
    # says "0::/" and the container's limits sit on the mount root.
    case.fake_systemd()
    case.cgroups("/", {"/": dict(QUALIFYING)})
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    result.assert_in_place("/", "memory.max=8589934592", "cpu.max=400000 100000")


def test_host_root_cgroup_without_limit_files_boxes(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/", {})
    result = case.run("print('payload ran')")
    assert result.rc == 0 and result.stdout == "payload ran\n", result.context()
    result.assert_boxed("no cgroup from / up to the root limits memory or CPU")


def test_invalid_boxing_value_is_a_usage_error(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    case.extra_env["SAFEHERMIT_BOXING"] = "never"
    marker = case.base / "payload-ran"
    result = case.run(f"open({str(marker)!r}, 'w').close()")
    assert result.rc == 2, result.context()
    assert "SAFEHERMIT_BOXING must be 'auto' or 'force', got 'never'" in result.stderr, result.context()
    assert not marker.exists() and result.calls == [], result.context()


# ---------------------------------------------------------------- in-place protections

def test_in_place_exit_codes_and_signals_pass_through(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    for action, wanted in (("sys.exit(42)", 42),
                           ("os.kill(os.getpid(), signal.SIGABRT)", 134),
                           ("os.kill(os.getpid(), signal.SIGKILL)", 137)):
        result = case.run(
            "import os, resource, signal, sys\n"
            "resource.setrlimit(resource.RLIMIT_CORE, (0, 0))\n" + action
        )
        assert result.rc == wanted, (action, result.context())
        assert result.fields.get("runner_exit_code") == str(wanted), result.context()
        assert result.fields.get("exit_code") == str(wanted), result.context()
        assert result.fields.get("in_place.outcome") == "exit", result.context()
        assert result.calls == [], result.context()


def test_in_place_deadline_kills_a_setsid_grandchild(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    pid_file = case.base / "grandchild.pid"
    result = case.run(
        f"""
        import os, subprocess, time
        subprocess.Popen(["/bin/sh", "-c", "echo $$ > {pid_file}.tmp; mv {pid_file}.tmp {pid_file}; exec sleep 120"],
                         start_new_session=True)
        time.sleep(120)
        """,
        "--sh-deadline", "3s",
    )
    assert result.rc == 124, result.context()
    assert result.fields.get("in_place.outcome") == "timeout", result.context()
    assert "DEADLINE: the run reached 3s and its process tree was killed by the in-place supervisor" \
        in result.stderr, result.context()
    assert result.fields.get("bound.wall", "").startswith("APPLIED:3s = 3.0s (in place"), result.context()
    assert result.seconds < 45, result.context()
    grandchild = int(pid_file.read_text())
    assert _pid_gone(grandchild, 2.0), f"setsid grandchild {grandchild} survived\n{result.context()}"


def test_in_place_log_cap_kills_the_tree(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    result = case.run(
        """
        import os, time
        for _ in range(64):
            os.write(2, b"x" * 1023 + b"\\n")
        time.sleep(120)
        """,
        "--sh-max-log-bytes", "4096",
    )
    assert result.rc == 125, result.context()
    assert result.fields.get("truncated") == "true", result.context()
    assert result.fields.get("killed_at_cap") == "in-place-tree-killed", result.context()
    assert result.fields.get("in_place.outcome") == "cap-killed", result.context()
    assert result.seconds < 45, result.context()
    assert "the in-place supervisor was notified" in Path(result.fields["log_file"]).read_text()


def test_in_place_leftover_daemon_is_killed_when_the_run_exits(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    pid_file = case.base / "daemon.pid"
    result = case.run(
        f"""
        import os, subprocess, sys
        # Double fork plus setsid: the daemon's parent exits, so it is orphaned,
        # and it keeps stderr open, which would stall the capper forever.
        subprocess.Popen(["/bin/sh", "-c",
                          "setsid sh -c 'echo $$ > {pid_file}.tmp; mv {pid_file}.tmp {pid_file}; exec sleep 120' &"])
        import time
        while not os.path.exists({str(pid_file)!r}):
            time.sleep(0.05)
        sys.exit(7)
        """,
    )
    assert result.rc == 7, result.context()
    assert result.fields.get("in_place.outcome") == "exit", result.context()
    assert result.fields.get("in_place.cleanup", "").startswith("tree_empty=true "), result.context()
    assert result.seconds < 45, result.context()
    daemon = int(pid_file.read_text())
    assert _pid_gone(daemon, 2.0), f"orphaned daemon {daemon} survived\n{result.context()}"


def test_in_place_unparseable_deadline_is_a_usage_error(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    marker = case.base / "payload-ran"
    result = case.run(f"open({str(marker)!r}, 'w').close()", "--sh-deadline", "soon")
    assert result.rc == 2, result.context()
    assert "--sh-deadline 'soon' is not a positive time span" in result.stderr, result.context()
    assert not marker.exists(), result.context()


def test_in_place_run_dies_when_safehermit_is_killed(case: Case) -> None:
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    pid_file = case.base / "payload.pid"
    payload = (f"import os, time\nopen({str(pid_file) + '.tmp'!r}, 'w').write(str(os.getpid()))\n"
               f"os.rename({str(pid_file) + '.tmp'!r}, {str(pid_file)!r})\ntime.sleep(120)\n")
    with open(case.base / "out", "wb") as out, open(case.base / "err", "wb") as err:
        wrapper = subprocess.Popen([str(SAFEHERMIT), PYTHON, "-c", payload], env=case.env(),
                                   cwd=case.base, stdin=subprocess.DEVNULL, stdout=out, stderr=err)
        try:
            deadline = time.monotonic() + 30
            while not pid_file.exists():
                assert time.monotonic() < deadline, (case.base / "err").read_text()
                assert wrapper.poll() is None, (case.base / "err").read_text()
                time.sleep(0.05)
            payload_pid = int(pid_file.read_text())
            os.kill(wrapper.pid, signal.SIGKILL)
            wrapper.wait(timeout=10)
            assert _pid_gone(payload_pid, 15.0), (
                f"payload {payload_pid} outlived a SIGKILLed safehermit\n"
                + (case.base / "err").read_text())
        finally:
            if wrapper.poll() is None:
                wrapper.kill()
                wrapper.wait()


def test_in_place_cpu_placement_uses_sched_setaffinity(case: Case) -> None:
    current = sorted(os.sched_getaffinity(0))
    if len(current) < 2:
        print("    SKIP: fewer than two CPUs available to exclude one")
        return
    case.fake_systemd()
    case.cgroups("/box", {"/box": dict(QUALIFYING)})
    excluded = current[0]
    case.extra_env["DEV_HERMIT_EXCLUDE_CPUS"] = str(excluded)
    result = case.run("import os; print(sorted(os.sched_getaffinity(0)))")
    assert result.rc == 0, result.context()
    assert result.fields.get("placement.cpus", "").startswith("APPLIED:sched_setaffinity("), result.context()
    observed = json.loads(result.stdout)
    assert excluded not in observed and observed == current[1:], (observed, result.context())


TESTS: list[Callable[[Case], None]] = [
    test_a_all_limits_max_boxes,
    test_b_finite_memory_without_cpu_controller_boxes,
    test_c_finite_cpu_with_memory_max_boxes,
    test_d_ancestor_with_both_limits_runs_in_place,
    test_e_force_override_boxes_a_qualifying_caller,
    test_split_limits_across_two_cgroups_do_not_qualify,
    test_memory_high_alone_counts_as_a_memory_limit,
    test_dot_dot_in_the_cgroup_path_is_refused,
    test_unreadable_proc_file_is_not_boxed,
    test_unboxed_without_systemd_is_reported_unboxed_as_before,
    test_cgroup_namespace_root_with_both_limits_runs_in_place,
    test_host_root_cgroup_without_limit_files_boxes,
    test_invalid_boxing_value_is_a_usage_error,
    test_in_place_exit_codes_and_signals_pass_through,
    test_in_place_deadline_kills_a_setsid_grandchild,
    test_in_place_log_cap_kills_the_tree,
    test_in_place_leftover_daemon_is_killed_when_the_run_exits,
    test_in_place_unparseable_deadline_is_a_usage_error,
    test_in_place_run_dies_when_safehermit_is_killed,
    test_in_place_cpu_placement_uses_sched_setaffinity,
]


def main() -> int:
    selected = set(sys.argv[1:])
    unknown = selected - {test.__name__ for test in TESTS}
    if unknown:
        print(f"unknown tests: {sorted(unknown)}", file=sys.stderr)
        return 2
    passed = failed = 0
    for test in TESTS:
        if selected and test.__name__ not in selected:
            continue
        started = time.monotonic()
        with tempfile.TemporaryDirectory(prefix="safehermit-boxing-test-") as directory:
            try:
                test(Case(Path(directory)))
            except Exception as error:  # report every case, then fail
                failed += 1
                print(f"  FAIL  {test.__name__} ({time.monotonic() - started:.1f}s)\n{error}")
                continue
        passed += 1
        print(f"  PASS  {test.__name__} ({time.monotonic() - started:.1f}s)")
    print(f"{passed} passed, {failed} failed")
    return 0 if failed == 0 and passed > 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
