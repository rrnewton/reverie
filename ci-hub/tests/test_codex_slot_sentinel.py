#!/usr/bin/env python3
"""Hermetic integration tests for Codex slot-sentinel allocation and release."""

from __future__ import annotations

import copy
import json
import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
SENTINEL = ROOT / "scripts" / "codex-slot-sentinel.rs"
ALLOCATOR = ROOT / "scripts" / "allocate-worktree.rs"
RELEASER = ROOT / "scripts" / "release-worktree.rs"
WORKTREE_REGISTRY = ROOT / "scripts" / "worktree_registry.rs"


FAKE_SYSTEMD = r"""#!/usr/bin/env python3
import json
import os
import shutil
import sys
from pathlib import Path

root = Path(os.environ["HERMIT_SENTINEL_TEST_ROOT"])
state_path = Path(os.environ["FAKE_SYSTEMD_STATE"])
state = json.loads(state_path.read_text())
arguments = sys.argv[1:]


def save():
    state_path.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")


def unit_not_found(unit):
    return {
        "Id": unit,
        "LoadState": "not-found",
        "ActiveState": "inactive",
        "SubState": "dead",
        "InvocationID": "",
        "MainPID": "0",
        "ControlGroup": "",
        "Environment": "",
        "WorkingDirectory": "",
        "Transient": "no",
        "Type": "",
        "Restart": "no",
        "KillMode": "control-group",
    }


def remove_runtime(unit_state):
    pid = str(unit_state["MainPID"])
    shutil.rmtree(root / "proc" / pid, ignore_errors=True)
    cgroup = root / "cgroup" / unit_state["ControlGroup"].lstrip("/")
    shutil.rmtree(cgroup, ignore_errors=True)


program = Path(sys.argv[0]).name
if program == "systemctl":
    if arguments[:2] == ["--user", "show"] and len(arguments) >= 3:
        unit = arguments[2]
        requested = [item.removeprefix("--property=") for item in arguments[3:]]
        expected = [
            "Id", "LoadState", "ActiveState", "SubState", "InvocationID",
            "MainPID", "ControlGroup", "Environment", "WorkingDirectory",
            "Transient", "Type", "Restart", "KillMode",
        ]
        if requested != expected:
            print(f"unexpected property query: {arguments}", file=sys.stderr)
            sys.exit(64)
        record = state["units"].get(unit, unit_not_found(unit))
        for key in expected:
            print(f"{key}={record[key]}")
    elif len(arguments) == 4 and arguments[:3] == ["--user", "stop", "--"]:
        unit = arguments[3]
        record = state["units"].get(unit)
        if record is None:
            print(f"unit not found: {unit}", file=sys.stderr)
            sys.exit(5)
        registry_path = root / "worktree-state.json"
        journal = None
        slot_exists = None
        if registry_path.exists():
            registry = json.loads(registry_path.read_text())
            for slot, slot_state in registry.get("slots", {}).items():
                if slot_state.get("coordinator_lease", {}).get("unit") == unit:
                    journal = slot_state.get("coordinator_lease_revocation")
                    slot_exists = (root / "worktrees" / "slots" / slot).is_dir()
                    break
        state.setdefault("stop_log", []).append(arguments)
        state.setdefault("stop_observations", []).append({
            "unit": unit,
            "journal": journal,
            "slot_exists": slot_exists,
        })
        remove_runtime(record)
        record["ActiveState"] = "inactive"
        record["SubState"] = "dead"
        record["MainPID"] = "0"
        save()
    else:
        print(f"unsupported fake systemctl arguments: {arguments}", file=sys.stderr)
        sys.exit(64)
elif program == "systemd-run":
    state.setdefault("run_log", []).append(arguments)
    if "--collect" in arguments:
        save()
        print("--collect is forbidden for durable recovery", file=sys.stderr)
        sys.exit(65)
    failures = int(state.get("run_failures", 0))
    if failures:
        state["run_failures"] = failures - 1
    if arguments[:2] != ["--user", "--quiet"] or arguments[-2:] != ["/usr/bin/sleep", "infinity"]:
        save()
        print(f"unexpected fake systemd-run arguments: {arguments}", file=sys.stderr)
        sys.exit(64)
    unit = next(item.split("=", 1)[1] for item in arguments if item.startswith("--unit="))
    working = next(
        item.split("=", 1)[1]
        for item in arguments
        if item.startswith("--working-directory=")
    )
    environment = next(
        item.split("=", 1)[1] for item in arguments if item.startswith("--setenv=")
    )
    required = {
        "--property=Type=exec",
        "--property=Restart=no",
        "--property=KillMode=control-group",
    }
    if not required.issubset(arguments):
        save()
        print("missing service policy", file=sys.stderr)
        sys.exit(64)
    pid = int(state.get("next_pid", 41000))
    state["next_pid"] = pid + 1
    starttime = 9000000 + pid
    invocation = f"{pid:032x}"
    cgroup = f"/user.slice/test.slice/{unit}"
    record = {
        "Id": unit,
        "LoadState": "loaded",
        "ActiveState": "active",
        "SubState": "running",
        "InvocationID": invocation,
        "MainPID": str(pid),
        "ControlGroup": cgroup,
        "Environment": environment,
        "WorkingDirectory": working,
        "Transient": "yes",
        "Type": "exec",
        "Restart": "no",
        "KillMode": "control-group",
        "StartTime": starttime,
    }
    state["units"][unit] = record
    proc = root / "proc" / str(pid)
    proc.mkdir(parents=True, exist_ok=True)
    fields = ["S"] + ["0"] * 18 + [str(starttime)]
    (proc / "stat").write_text(f"{pid} (fixture sentinel) {' '.join(fields)}\n")
    (proc / "cgroup").write_text(f"0::{cgroup}\n")
    (proc / "cwd").symlink_to(working)
    group = root / "cgroup" / cgroup.lstrip("/")
    group.mkdir(parents=True, exist_ok=True)
    (group / "cgroup.procs").write_text(f"{pid}\n")
    (group / "cgroup.events").write_text("populated 1\n")
    save()
    if failures:
        print("planted systemd-run failure after exact unit creation", file=sys.stderr)
        sys.exit(55)
else:
    print(f"unsupported fake systemd program: {program}", file=sys.stderr)
    sys.exit(64)
"""


FAKE_REGISTRY_CHECKER = r"""#!/usr/bin/env python3
import sys

if (
    len(sys.argv) not in (3, 5)
    or sys.argv[1] != "--root"
    or (len(sys.argv) == 5 and sys.argv[3] != "--slot")
):
    print(f"unexpected registry-check arguments: {sys.argv[1:]}", file=sys.stderr)
    sys.exit(64)
"""


FAKE_AGENT_PODMAN = r"""#!/usr/bin/env python3
import json
import sys

if not sys.argv[1:] or sys.argv[1] != "release-audit" or "--json" not in sys.argv:
    print(f"unexpected agent-podman arguments: {sys.argv[1:]}", file=sys.stderr)
    sys.exit(64)
print(json.dumps({"state": "ok", "inspected": 0, "container_observation": []}))
"""


class SentinelFixture:
    def __init__(self, *, prefix: str = "codex-slot-sentinel-test.") -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix=prefix)
        self.root = Path(self.temporary.name)
        for relative in [
            "hermit",
            "reverie",
            "liteinst2",
            "scripts",
            "worktrees/slots",
            "ignored/ci-hub",
            "proc",
            "cgroup",
            "bin",
        ]:
            (self.root / relative).mkdir(parents=True, exist_ok=True)
        (self.root / ".gitmodules").write_text("# disposable fixture\n")
        (self.root / "worktrees" / "ACTIVE.md").write_text("# Active fixture slots\n")

        self.sentinel = self.root / "scripts" / "codex-slot-sentinel.rs"
        shutil.copy2(SENTINEL, self.sentinel)
        shutil.copy2(WORKTREE_REGISTRY, self.root / "scripts" / "worktree_registry.rs")
        self.sentinel.chmod(0o755)
        self.systemctl = self.root / "bin" / "systemctl"
        self.systemd_run = self.root / "bin" / "systemd-run"
        for tool in [self.systemctl, self.systemd_run]:
            tool.write_text(textwrap.dedent(FAKE_SYSTEMD))
            tool.chmod(0o755)
        self.systemd_state = self.root / "fake-systemd.json"
        self.write_systemd({
            "units": {},
            "next_pid": 41000,
            "run_failures": 0,
            "run_log": [],
            "stop_log": [],
            "stop_observations": [],
        })

        checker = self.root / "scripts" / "check-worktree-registry.rs"
        checker.write_text(textwrap.dedent(FAKE_REGISTRY_CHECKER))
        checker.chmod(0o755)
        podman = self.root / "scripts" / "agent-podman.rs"
        podman.write_text(textwrap.dedent(FAKE_AGENT_PODMAN))
        podman.chmod(0o755)

        self.environment = dict(os.environ)
        for name in list(self.environment):
            if name.startswith("HERMIT_RELEASE_TEST_") or name.startswith(
                "HERMIT_SENTINEL_TEST_"
            ):
                self.environment.pop(name)
        self.environment.pop("AGENT_PODMAN_BIN", None)
        self.environment |= {
            "HERMIT_SENTINEL_TEST_ROOT": str(self.root),
            "HERMIT_SENTINEL_TEST_SYSTEMCTL": str(self.systemctl),
            "HERMIT_SENTINEL_TEST_SYSTEMD_RUN": str(self.systemd_run),
            "FAKE_SYSTEMD_STATE": str(self.systemd_state),
            "HERMIT_RELEASE_TEST_PROC_ROOT": str(self.root / "proc"),
            "HERMIT_RELEASE_TEST_CGROUP_ROOT": str(self.root / "cgroup"),
        }

    def cleanup(self) -> None:
        self.temporary.cleanup()

    def read_systemd(self) -> dict[str, Any]:
        return json.loads(self.systemd_state.read_text())

    def write_systemd(self, value: dict[str, Any]) -> None:
        self.systemd_state.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")

    def run_rust(
        self,
        script: Path,
        *arguments: str,
        environment: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["rust-script", str(script), *arguments],
            cwd=self.root,
            env=environment or self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=90,
            check=False,
        )

    def sentinel_command(self, *arguments: str) -> dict[str, Any]:
        result = self.run_rust(self.sentinel, *arguments)
        if result.returncode != 0:
            raise AssertionError(
                f"sentinel {' '.join(arguments)} failed ({result.returncode}):\n"
                f"stdout={result.stdout}\nstderr={result.stderr}"
            )
        return json.loads(result.stdout)

    def plan_and_launch(
        self, slot: str = "slot01", *, working_slot: str | None = None
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        working = self.root / "worktrees" / "slots" / (working_slot or slot)
        working.mkdir(parents=True, exist_ok=True)
        planned = self.sentinel_command(
            "plan", "--slot", slot, "--working-directory", str(working)
        )["plan"]
        lease = self.sentinel_command(
            "launch", "--plan-json", json.dumps(planned, separators=(",", ":"))
        )["lease"]
        return planned, lease

    def write_registry_slot(self, lease: dict[str, Any], slot: str = "slot01") -> None:
        (self.root / "worktrees" / "slots" / slot).mkdir(parents=True, exist_ok=True)
        state = {
            "version": 3,
            "updated": "fixture",
            "slots": {
                slot: {
                    "agents": [
                        {
                            "name": "hermit-coord-fixture",
                            "read_only": False,
                            "task": "sentinel-release-fixture",
                        }
                    ],
                    "allocated": "2026-08-05T00:00:00Z",
                    "updated": "2026-08-05T00:00:00Z",
                    "status": "active",
                    "task": "sentinel-release-fixture",
                    "purpose": "exercise exact coordinator revocation",
                    "hermit_path": f"worktrees/slots/{slot}/hermit",
                    "reverie_path": f"worktrees/slots/{slot}/reverie",
                    "liteinst2_path": f"worktrees/slots/{slot}/liteinst2",
                    "hermit_branch": "-",
                    "reverie_branch": "-",
                    "liteinst2_branch": "-",
                    "coordinator_lease": lease,
                }
            },
        }
        (self.root / "worktree-state.json").write_text(
            json.dumps(state, indent=2, sort_keys=True) + "\n"
        )

    def initialize_primary(self, product: str) -> None:
        primary = self.root / product
        subprocess.run(
            ["git", "init", "--quiet"], cwd=primary, check=True
        )
        (primary / "fixture.txt").write_text("fixture\n")
        subprocess.run(
            ["git", "add", "fixture.txt"], cwd=primary, check=True
        )
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Sentinel Fixture",
                "-c",
                "user.email=sentinel-fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
            cwd=primary,
            check=True,
        )

    def registry(self) -> dict[str, Any]:
        return json.loads((self.root / "worktree-state.json").read_text())

    def release(
        self, *extra: str, environment: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        return self.run_rust(
            RELEASER,
            "--slot",
            "slot01",
            "--clean",
            *extra,
            environment=environment,
        )

    def vanish_unit(self, lease: dict[str, Any]) -> None:
        state = self.read_systemd()
        record = state["units"].pop(lease["unit"])
        shutil.rmtree(self.root / "proc" / str(lease["main_pid"]), ignore_errors=True)
        shutil.rmtree(
            self.root / "cgroup" / record["ControlGroup"].lstrip("/"),
            ignore_errors=True,
        )
        self.write_systemd(state)


class CodexSlotSentinelTests(unittest.TestCase):
    def fixture(self, *, prefix: str = "codex-slot-sentinel-test.") -> SentinelFixture:
        fixture = SentinelFixture(prefix=prefix)
        self.addCleanup(fixture.cleanup)
        return fixture

    def assert_success(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(
            result.returncode,
            0,
            f"stdout={result.stdout}\nstderr={result.stderr}",
        )

    def test_plan_launch_recovery_verify_revoke_and_negative_bindings(self) -> None:
        fixture = self.fixture()
        plan, lease = fixture.plan_and_launch()

        recovered = fixture.sentinel_command(
            "launch", "--plan-json", json.dumps(plan, separators=(",", ":"))
        )
        self.assertEqual(recovered, {"state": "live", "lease": lease})
        verified = fixture.sentinel_command(
            "verify", "--lease-json", json.dumps(lease, separators=(",", ":"))
        )
        self.assertEqual(verified, {"state": "live", "lease": lease})

        baseline = fixture.read_systemd()
        wrong_nonce = (
            ("0" if lease["nonce"][0] != "0" else "1") + lease["nonce"][1:]
        )
        cases = {
            "missing-nonce": lambda record: record.__setitem__("Environment", ""),
            "wrong-nonce": lambda record: record.__setitem__(
                "Environment",
                f"DEV_HERMIT_SLOT_LEASE={lease['generation']}:{wrong_nonce}",
            ),
            "wrong-cgroup": lambda record: record.__setitem__(
                "ControlGroup", f"/user.slice/wrong/{lease['unit']}"
            ),
            "restarted-invocation": lambda record: record.__setitem__(
                "InvocationID", "f" * 32
            ),
            "vanished": None,
        }
        for label, mutate in cases.items():
            with self.subTest(label=label):
                planted = copy.deepcopy(baseline)
                fixture.write_systemd(planted)
                if label == "vanished":
                    planted["units"].pop(lease["unit"])
                else:
                    mutate(planted["units"][lease["unit"]])
                fixture.write_systemd(planted)
                refused = fixture.run_rust(
                    fixture.sentinel,
                    "verify",
                    "--lease-json",
                    json.dumps(lease, separators=(",", ":")),
                )
                self.assertNotEqual(refused.returncode, 0, refused.stdout)
                fixture.write_systemd(copy.deepcopy(baseline))

        revoked = fixture.sentinel_command(
            "revoke", "--lease-json", json.dumps(lease, separators=(",", ":"))
        )
        self.assertEqual(revoked["state"], "revoked")
        self.assertEqual(revoked["lease"], lease)
        proven = fixture.sentinel_command(
            "prove-revoked",
            "--lease-json",
            json.dumps(lease, separators=(",", ":")),
        )
        self.assertEqual(proven["state"], "revoked")
        self.assertEqual(proven["lease"], lease)

        systemd = fixture.read_systemd()
        self.assertEqual(len(systemd["run_log"]), 1, systemd["run_log"])
        self.assertNotIn("--collect", systemd["run_log"][0])
        self.assertEqual(
            systemd["stop_log"],
            [["--user", "stop", "--", lease["unit"]]],
        )

    def test_allocator_preserves_legacy_authority_and_recovers_failed_launch(self) -> None:
        fixture = self.fixture(prefix="release-worktree-test.")
        slot = "lander-linear"
        (fixture.root / "worktrees" / "slots" / slot).mkdir()
        historical = {
            "agents": [
                {
                    "name": "hermit-lander",
                    "read_only": False,
                    "task": "historic-owner-task",
                    "tmux_pane_id": "%77",
                    "cgroup_path": "/agent.slice/historic-owner.scope",
                    "owner_marker": "must-survive",
                }
            ],
            "allocated": "2026-07-31T01:02:03Z",
            "updated": "2026-08-01T04:05:06Z",
            "status": "active",
            "task": "historic-slot-task",
            "purpose": "historic purpose must remain exact",
            "hermit_path": f"worktrees/slots/{slot}/hermit",
            "reverie_path": f"worktrees/slots/{slot}/reverie",
            "liteinst2_path": f"worktrees/slots/{slot}/liteinst2",
            "hermit_branch": "-",
            "reverie_branch": "-",
            "liteinst2_branch": "-",
            "custom_marker": {"preserve": [1, 2, 3]},
        }
        registry = {"version": 3, "updated": "fixture", "slots": {slot: historical}}
        (fixture.root / "worktree-state.json").write_text(
            json.dumps(registry, indent=2, sort_keys=True) + "\n"
        )
        systemd = fixture.read_systemd()
        systemd["run_failures"] = 1
        fixture.write_systemd(systemd)
        thread_id = "codex-thread-must-not-own-historical-slot"
        environment = fixture.environment | {"CODEX_THREAD_ID": thread_id}
        arguments = [
            "--agent",
            "hermit-lander",
            "--slot",
            slot,
            "--codex-systemd-sentinel",
            "--recover-legacy-unbound-owner",
            "--recovery-note",
            "coordinator binding after legacy authority audit",
        ]

        failed = fixture.run_rust(ALLOCATOR, *arguments, environment=environment)
        self.assertNotEqual(failed.returncode, 0, failed.stdout)
        after_failure = fixture.registry()["slots"][slot]
        intent = after_failure["coordinator_lease_intent"]
        self.assertEqual(intent["phase"], "launch-planned")
        self.assertTrue(intent["legacy_recovery"])
        self.assertEqual(intent["historical_slot"], historical)
        self.assertNotIn("coordinator_lease", after_failure)
        first_plan = copy.deepcopy(intent["plan"])
        failed_systemd = fixture.read_systemd()
        self.assertIn(first_plan["unit"], failed_systemd["units"])
        self.assertEqual(
            failed_systemd["units"][first_plan["unit"]]["ActiveState"], "active"
        )

        durable_intent_state = fixture.registry()
        for label, (mutate, expected) in {
            "wrong-phase": (
                lambda value: value["coordinator_lease_intent"].__setitem__(
                    "phase", "launching"
                ),
                "launch intent does not match this exact binding request",
            ),
            "missing-history": (
                lambda value: value["coordinator_lease_intent"].__setitem__(
                    "historical_slot", None
                ),
                "launch intent does not match this exact binding request",
            ),
            "current-slot-drift": (
                lambda value: value.__setitem__(
                    "purpose", "tampered after durable intent"
                ),
                "drifted from the durable Codex launch snapshot",
            ),
        }.items():
            with self.subTest(malformed_intent=label):
                planted = copy.deepcopy(durable_intent_state)
                mutate(planted["slots"][slot])
                (fixture.root / "worktree-state.json").write_text(
                    json.dumps(planted, indent=2, sort_keys=True) + "\n"
                )
                refused = fixture.run_rust(
                    ALLOCATOR, *arguments, environment=environment
                )
                self.assertNotEqual(refused.returncode, 0, refused.stdout)
                self.assertIn(expected, refused.stderr)
                self.assertEqual(len(fixture.read_systemd()["run_log"]), 1)
        (fixture.root / "worktree-state.json").write_text(
            json.dumps(durable_intent_state, indent=2, sort_keys=True) + "\n"
        )

        recovered = fixture.run_rust(ALLOCATOR, *arguments, environment=environment)
        self.assert_success(recovered)
        final = fixture.registry()["slots"][slot]
        self.assertNotIn("coordinator_lease_intent", final)
        lease = final["coordinator_lease"]
        for key in [
            "schema_version",
            "source",
            "slot",
            "generation",
            "nonce",
            "unit",
            "working_directory",
        ]:
            self.assertEqual(lease[key], first_plan[key])
        history = final["coordinator_lease_history"]
        self.assertEqual(len(history), 1)
        self.assertEqual(history[0]["mode"], "binding-only-legacy-recovery")
        self.assertEqual(history[0]["historical_slot"], historical)
        for key in [
            "task",
            "purpose",
            "hermit_branch",
            "reverie_branch",
            "liteinst2_branch",
            "custom_marker",
        ]:
            self.assertEqual(final[key], historical[key])
        active_owner = final["agents"][0]
        self.assertEqual(
            active_owner,
            {
                key: value
                for key, value in historical["agents"][0].items()
                if key not in {"tmux_pane_id", "cgroup_path"}
            },
        )
        self.assertNotIn(thread_id, json.dumps(final, sort_keys=True))
        run_log = fixture.read_systemd()["run_log"]
        self.assertEqual(len(run_log), 1)
        self.assertNotIn("--collect", run_log[0])

    def test_allocator_refuses_dead_finalized_lease_before_product_mutation(self) -> None:
        fixture = self.fixture(prefix="release-worktree-test.")
        fixture.initialize_primary("hermit")
        _plan, lease = fixture.plan_and_launch()
        fixture.write_registry_slot(lease)
        fixture.vanish_unit(lease)
        product_path = fixture.root / "worktrees" / "slots" / "slot01" / "hermit"
        self.assertFalse(product_path.exists())

        refused = fixture.run_rust(
            ALLOCATOR,
            "--agent",
            "hermit-coord-fixture",
            "--slot",
            "slot01",
            "--product",
            "hermit",
            "--task",
            "sentinel-release-fixture",
            "--purpose",
            "exercise exact coordinator revocation",
            "--codex-systemd-sentinel",
        )

        self.assertNotEqual(refused.returncode, 0, refused.stdout)
        self.assertIn("existing Codex sentinel refused", refused.stderr)
        self.assertFalse(product_path.exists())
        registered = subprocess.run(
            ["git", "worktree", "list", "--porcelain"],
            cwd=fixture.root / "hermit",
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        ).stdout
        self.assertNotIn(str(product_path), registered)
        self.assertEqual(fixture.registry()["slots"]["slot01"]["coordinator_lease"], lease)

    def test_allocator_refuses_finalized_lease_bound_to_another_slot(self) -> None:
        fixture = self.fixture(prefix="release-worktree-test.")
        fixture.initialize_primary("hermit")
        _plan, lease = fixture.plan_and_launch("slot01", working_slot="other")
        fixture.write_registry_slot(lease)
        product_path = fixture.root / "worktrees" / "slots" / "slot01" / "hermit"

        refused = fixture.run_rust(
            ALLOCATOR,
            "--agent",
            "hermit-coord-fixture",
            "--slot",
            "slot01",
            "--product",
            "hermit",
            "--codex-systemd-sentinel",
        )

        self.assertNotEqual(refused.returncode, 0, refused.stdout)
        self.assertIn("does not bind exact canonical slot", refused.stderr)
        self.assertFalse(product_path.exists())
        self.assertEqual(fixture.read_systemd()["stop_log"], [])

    def test_allocator_intent_retry_cannot_create_a_new_product(self) -> None:
        fixture = self.fixture(prefix="release-worktree-test.")
        fixture.initialize_primary("hermit")
        fixture.initialize_primary("reverie")
        systemd = fixture.read_systemd()
        systemd["run_failures"] = 1
        fixture.write_systemd(systemd)
        initial = [
            "--agent",
            "hermit-coord-fixture",
            "--slot",
            "slot01",
            "--product",
            "hermit",
            "--codex-systemd-sentinel",
        ]
        failed = fixture.run_rust(ALLOCATOR, *initial)
        self.assertNotEqual(failed.returncode, 0, failed.stdout)
        state = fixture.registry()
        drifted = copy.deepcopy(state)
        drifted["slots"]["slot01"]["agents"][0]["read_only"] = True
        (fixture.root / "worktree-state.json").write_text(
            json.dumps(drifted, indent=2, sort_keys=True) + "\n"
        )
        drift_refused = fixture.run_rust(ALLOCATOR, *initial)
        self.assertNotEqual(drift_refused.returncode, 0, drift_refused.stdout)
        self.assertIn("drifted from the durable Codex launch snapshot", drift_refused.stderr)
        self.assertEqual(len(fixture.read_systemd()["run_log"]), 1)

        state["slots"]["slot01"]["coordinator_lease_intent"]["plan"][
            "working_directory"
        ] = str(fixture.root / "worktrees" / "slots" / "other")
        (fixture.root / "worktree-state.json").write_text(
            json.dumps(state, indent=2, sort_keys=True) + "\n"
        )
        new_product = fixture.root / "worktrees" / "slots" / "slot01" / "reverie"

        refused = fixture.run_rust(
            ALLOCATOR,
            "--agent",
            "hermit-coord-fixture",
            "--slot",
            "slot01",
            "--product",
            "reverie",
            "--codex-systemd-sentinel",
        )

        self.assertNotEqual(refused.returncode, 0, refused.stdout)
        self.assertIn("does not bind exact canonical slot", refused.stderr)
        self.assertFalse(new_product.exists())

    def test_release_revokes_before_removal_with_exact_durable_journal(self) -> None:
        fixture = self.fixture(prefix="release-worktree-test.")
        _plan, lease = fixture.plan_and_launch()
        fixture.write_registry_slot(lease)

        released = fixture.release()
        self.assert_success(released)
        self.assertNotIn("slot01", fixture.registry()["slots"])
        self.assertFalse((fixture.root / "worktrees" / "slots" / "slot01").exists())
        systemd = fixture.read_systemd()
        self.assertEqual(
            systemd["stop_log"],
            [["--user", "stop", "--", lease["unit"]]],
        )
        observation = systemd["stop_observations"][0]
        self.assertTrue(observation["slot_exists"])
        self.assertEqual(observation["journal"]["phase"], "stop-authorized")
        self.assertEqual(observation["journal"]["lease"], lease)
        self.assertEqual(len(observation["journal"]["transaction_nonce"]), 32)

    def test_release_recovers_crashes_after_revocation_arm_and_stop(self) -> None:
        for hook, stopped_before_recovery in [
            ("HERMIT_RELEASE_TEST_CRASH_AFTER_SENTINEL_REVOCATION_ARM", False),
            ("HERMIT_RELEASE_TEST_CRASH_AFTER_SENTINEL_STOP", True),
        ]:
            with self.subTest(hook=hook):
                fixture = self.fixture(prefix="release-worktree-test.")
                _plan, lease = fixture.plan_and_launch()
                fixture.write_registry_slot(lease)
                marker = fixture.root / f"{hook}.marker"
                marker.write_text("armed\n")
                crashed = fixture.release(
                    environment=fixture.environment | {hook: str(marker)}
                )
                self.assertNotEqual(crashed.returncode, 0, crashed.stdout)
                journal = fixture.registry()["slots"]["slot01"][
                    "coordinator_lease_revocation"
                ]
                self.assertEqual(journal["phase"], "stop-authorized")
                self.assertEqual(journal["lease"], lease)
                self.assertNotIn("proof", journal)
                systemd = fixture.read_systemd()
                self.assertEqual(
                    len(systemd["stop_log"]), 1 if stopped_before_recovery else 0
                )

                recovered = fixture.release("--recover-submodule-cleanup")
                self.assert_success(recovered)
                self.assertNotIn("slot01", fixture.registry()["slots"])
                systemd = fixture.read_systemd()
                self.assertEqual(
                    systemd["stop_log"],
                    [["--user", "stop", "--", lease["unit"]]],
                )

    def test_release_refuses_missing_or_restarted_sentinel_even_with_force(self) -> None:
        for condition in ["missing", "restarted"]:
            with self.subTest(condition=condition):
                fixture = self.fixture(prefix="release-worktree-test.")
                _plan, lease = fixture.plan_and_launch()
                fixture.write_registry_slot(lease)
                if condition == "missing":
                    fixture.vanish_unit(lease)
                else:
                    state = fixture.read_systemd()
                    state["units"][lease["unit"]]["InvocationID"] = "f" * 32
                    fixture.write_systemd(state)

                refused = fixture.release("--force")
                self.assertNotEqual(refused.returncode, 0, refused.stdout)
                slot = fixture.registry()["slots"]["slot01"]
                self.assertNotIn("coordinator_lease_revocation", slot)
                self.assertTrue((fixture.root / "worktrees" / "slots" / "slot01").is_dir())
                self.assertEqual(fixture.read_systemd()["stop_log"], [])
                self.assertIn("live Codex coordinator lease refused cleanup", refused.stderr)

    def test_release_refuses_sentinel_bound_to_another_working_directory(self) -> None:
        fixture = self.fixture(prefix="release-worktree-test.")
        _plan, lease = fixture.plan_and_launch("slot01", working_slot="other")
        fixture.write_registry_slot(lease)

        refused = fixture.release("--force")

        self.assertNotEqual(refused.returncode, 0, refused.stdout)
        self.assertIn("does not bind exact canonical slot", refused.stderr)
        slot = fixture.registry()["slots"]["slot01"]
        self.assertNotIn("coordinator_lease_revocation", slot)
        self.assertTrue((fixture.root / "worktrees" / "slots" / "slot01").is_dir())
        self.assertTrue((fixture.root / "worktrees" / "slots" / "other").is_dir())
        self.assertEqual(fixture.read_systemd()["stop_log"], [])


if __name__ == "__main__":
    unittest.main()
