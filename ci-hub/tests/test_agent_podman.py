#!/usr/bin/env python3
"""Integration tests for ownership-safe agent Podman reconciliation."""

from __future__ import annotations

import json
import fcntl
import os
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts" / "agent-podman.rs"
sys.path.insert(0, str(ROOT / "agent-utils" / "py"))

from tick_hub.engine import (  # noqa: E402
    UnresolvedPlaceholderError,
    parse_kv_lines,
    render_emit,
)
from tick_hub.io import config_from_yaml  # noqa: E402


FAKE_PODMAN = r"""#!/usr/bin/env python3
import json
import fcntl
import os
import sys
import time
from pathlib import Path

state_path = Path(os.environ["FAKE_PODMAN_STATE"])
log_path = Path(os.environ["FAKE_PODMAN_LOG"])
containers = json.loads(state_path.read_text())
args = sys.argv[1:]

def save():
    state_path.write_text(json.dumps(containers))

def find(name):
    return next((item for item in containers if item["Id"] == name or name in item["Names"]), None)

if args[:3] == ["ps", "-a", "--format"]:
    print(json.dumps(containers))
elif args and args[0] in {"run", "create"}:
    cidfile = Path(args[args.index("--cidfile") + 1])
    lifecycle = Path(os.environ["DEV_HERMIT_CONTAINER_STATE"]).parent / "agent-container-lifecycle.lock"
    lifecycle.parent.mkdir(parents=True, exist_ok=True)
    with lifecycle.open("a+") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            held = False
            fcntl.flock(lock, fcntl.LOCK_UN)
        except BlockingIOError:
            held = True
    container_id = "f" * 64
    launch_labels = {}
    for index, argument in enumerate(args):
        if argument == "--label":
            key, value = args[index + 1].split("=", 1)
            launch_labels[key] = value
    containers.append({
        "Id": container_id,
        "Names": ["created"],
        "State": "created",
        "Labels": launch_labels,
        "Mounts": [],
    })
    cidfile.write_text(container_id + "\n")
    if early_ready := os.environ.get("FAKE_PODMAN_EARLY_CID_READY"):
        Path(early_ready).write_text("cid-before-engine-census\n")
        gate = Path(os.environ["FAKE_PODMAN_EARLY_CID_GATE"])
        while not gate.exists():
            time.sleep(0.01)
    with log_path.open("a") as log:
        log.write("lifecycle-held\n" if held else "lifecycle-open\n")
    save()
elif args[:2] == ["container", "inspect"]:
    item = find(args[2])
    if item is None:
        sys.exit(1)
    print(json.dumps([{
        "Id": item["Id"],
        "Config": {"Labels": item.get("Labels")},
        "Mounts": item.get("Mounts", []),
    }]))
elif args and args[0] == "top":
    item = find(args[1])
    print("HPID STATE")
    for offset in range(item.get("Zombies", 0)):
        print(f"{1000 + offset} Z")
elif args and args[0] == "inspect":
    item = find(args[-1])
    if item is None:
        sys.exit(1)
    template = args[args.index("--format") + 1]
    if template == "{{json .Config.Labels}}":
        print(json.dumps(item.get("Labels", {})))
    elif template == "{{.Id}}":
        print(item["Id"])
    elif template == "{{.State.Pid}}":
        print("0")
    else:
        sys.exit(2)
elif args and args[0] == "stop":
    item = find(args[-1])
    item["State"] = "stopped"
    with log_path.open("a") as log:
        log.write(f"stop {item['Id']}\n")
    save()
    if os.environ.get("FAKE_PODMAN_STOP_FAIL_AFTER_STOP") == "1":
        print("stop command timed out after container exited", file=sys.stderr)
        sys.exit(124)
elif args and args[0] == "rm":
    item = find(args[-1])
    with log_path.open("a") as log:
        log.write(f"rm {item['Id']}\n")
    containers.remove(item)
    save()
elif args[:2] == ["container", "exists"]:
    lifecycle = Path(os.environ["DEV_HERMIT_CONTAINER_STATE"]).parent / "agent-container-lifecycle.lock"
    with lifecycle.open("a+") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            held = False
            fcntl.flock(lock, fcntl.LOCK_UN)
        except BlockingIOError:
            held = True
    with log_path.open("a") as log:
        log.write("exists-lifecycle-held\n" if held else "exists-lifecycle-open\n")
    if os.environ.get("FAKE_PODMAN_EXISTS_FORCE_PRESENT") == "1":
        sys.exit(0)
    if counter_path := os.environ.get("FAKE_PODMAN_EXISTS_COUNTER"):
        counter = Path(counter_path)
        count = int(counter.read_text()) if counter.exists() else 0
        counter.write_text(str(count + 1))
        threshold = int(os.environ.get("FAKE_PODMAN_EXISTS_FAIL_AFTER", "999999"))
        if count >= threshold:
            print("planted container-exists authority failure", file=sys.stderr)
            sys.exit(125)
    sys.exit(0 if find(args[2]) else 1)
else:
    print(f"unsupported fake podman args: {args}", file=sys.stderr)
    sys.exit(2)
"""


def labels(agent: str, invocation: str, lifetime: str = "agent") -> dict[str, str]:
    return {
        "io.dev-hermit.agent-podman": "v1",
        "io.dev-hermit.owner-agent": agent,
        "io.dev-hermit.owner-invocation": invocation,
        "io.dev-hermit.owner-pane": "%1",
        "io.dev-hermit.owner-task": "fixture-task",
        "io.dev-hermit.lifetime": lifetime,
    }


def container(
    container_id: str,
    name: str,
    container_labels: dict[str, str] | None,
    *,
    zombies: int = 0,
    mounts: list[dict[str, str]] | None = None,
) -> dict[str, Any]:
    return {
        "Id": container_id,
        "Names": [name],
        "State": "running",
        "Labels": container_labels,
        "Zombies": zombies,
        "Mounts": mounts or [],
    }


def ownership(
    container_id: str, agent: str, invocation: str, lifetime: str = "agent"
) -> dict[str, Any]:
    return {
        "container_id": container_id,
        "owner_agent": agent,
        "owner_invocation": invocation,
        "owner_pane": "%1",
        "task": "fixture-task",
        "lifetime": lifetime,
        "updated_at": 1,
    }


class AgentPodmanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="agent-podman-test-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.fake_podman = self.directory / "podman"
        self.fake_podman.write_text(textwrap.dedent(FAKE_PODMAN))
        self.fake_podman.chmod(0o755)
        self.podman_state = self.directory / "podman.json"
        self.podman_log = self.directory / "podman.log"
        self.podman_log.write_text("")
        self.agents = self.directory / "agents.json"
        self.invocations = self.directory / "invocations.json"
        self.registry = self.directory / "ownership.json"
        self.environment = os.environ | {
            "AGENT_PODMAN_BIN": str(self.fake_podman),
            "FAKE_PODMAN_STATE": str(self.podman_state),
            "FAKE_PODMAN_LOG": str(self.podman_log),
            "DEV_HERMIT_CONTAINER_STATE": str(self.registry),
            "DG_AGENT_NAME": "hermit-new",
            "META_3PAI_INVOCATION_ID": "inv-new",
            "TMUX_PANE": "%9",
        }

    def run_tool(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["rust-script", str(TOOL), *arguments],
            cwd=ROOT,
            env=self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            check=False,
        )

    def write_live_state(
        self, agents: list[dict[str, Any]], invocations: dict[str, list[str]]
    ) -> None:
        """Write the census in the ENVELOPE form, which is the only form accepted.

        ⚠️ THIS USED TO WRITE A BARE ARRAY, AND FIVE TESTS DIED ON IT WITHOUT EVER
        REACHING THEIR ASSERTIONS. `load_live_agents` has refused an undated census
        since the guard landed -- a snapshot with no `captured_at` skipped the
        staleness check entirely and was treated as ageless, and it decides which
        agents are absent, so `reconcile --apply` would remove a live agent's
        containers on it.

        The guard is right and these fixtures were stale: BOTH real producers write
        the envelope -- `persist_snapshot` in
        `ci-hub/health/agent-container-reconcile-bridge.rs` and
        `_persist_agent_snapshot` in `ci-hub/health/operational_health.py` both emit
        `schema_version` / `captured_at` / `agents`. The bare array was a shape no
        producer ever wrote, so these tests were exercising an input that cannot
        occur, and failing before the behaviour they name.

        (`ci-hub active-work --agent-snapshot` DOES want a bare array. That is a
        different tool reading the same file, and it is why the two shapes get
        confused; it is not a reason for this one to accept the other's.)
        """
        self.agents.write_text(
            json.dumps(
                {"schema_version": 1, "captured_at": time.time(), "agents": agents}
            )
        )
        self.invocations.write_text(json.dumps(invocations))

    # A census must contain someone live or it is refused, so a test about ONE
    # container's owner supplies an unrelated live agent. That is also the realistic
    # state: a fleet with nobody alive at all is not a condition these tests are
    # about, and it was only ever incidental to them.
    OTHER_LIVE_AGENT = [{"name": "hermit-bystander", "status": "running", "tmux_pane_id": "%7"}]

    def reconcile(self, *, apply: bool = True) -> subprocess.CompletedProcess[str]:
        arguments = [
            "reconcile",
            "--agents-json",
            str(self.agents),
            "--live-invocations",
            str(self.invocations),
            "--json",
        ]
        if apply:
            arguments.append("--apply")
        return self.run_tool(*arguments)

    # ⚠️ EMPTY IS NOT STALE. A census with a CURRENT timestamp and nobody live in it
    # passes the staleness and undated guards and then tells the reclaim predicate
    # that no agent owns anything -- which authorises removing every managed
    # container. Measured 2026-08-26 before the guard: a fresh `agents: []` returned
    # rc=0 `state=ok`, and it was harmless only because the box happened to hold zero
    # managed containers that minute.
    def test_a_fresh_but_empty_census_is_refused_rather_than_read_as_everyone_gone(
        self,
    ) -> None:
        self.podman_state.write_text(
            json.dumps([container("a" * 64, "retired", labels("hermit-old", "inv-old"))])
        )
        self.write_live_state([], {})

        result = self.reconcile()

        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("no live agents", result.stderr + result.stdout)
        self.assertEqual(self.podman_log.read_text(), "", "a refused census removed something")

    # The half a raw `array.is_empty()` check would miss: entries are present, so the
    # array is not empty, but every one is terminal and the derived live set is.
    def test_a_census_of_only_terminal_agents_is_refused_too(self) -> None:
        self.podman_state.write_text(
            json.dumps([container("a" * 64, "retired", labels("hermit-old", "inv-old"))])
        )
        self.write_live_state(
            [{"name": "hermit-901", "status": "exited"}, {"name": "hermit-902", "status": "dead"}],
            {},
        )

        result = self.reconcile()

        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("no live agents", result.stderr + result.stdout)
        self.assertEqual(self.podman_log.read_text(), "", "a refused census removed something")

    # THE NEGATIVE CONTROL, and it is the one that matters: without it the guard could
    # be refusing everything and the two cases above would still pass.
    def test_one_live_agent_is_enough_for_the_census_to_be_accepted(self) -> None:
        self.podman_state.write_text(json.dumps([]))
        self.write_live_state(
            [
                {"name": "hermit-901", "status": "running", "tmux_pane_id": "%1"},
                {"name": "hermit-902", "status": "dead"},
            ],
            {},
        )

        result = self.reconcile()

        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertNotIn("no live agents", result.stderr + result.stdout)

    def test_retired_agent_fixture_is_gracefully_reclaimed(self) -> None:
        self.podman_state.write_text(
            json.dumps([container("a" * 64, "retired", labels("hermit-old", "inv-old"), zombies=2)])
        )
        self.write_live_state(self.OTHER_LIVE_AGENT, {})

        result = self.reconcile()

        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        report = json.loads(result.stdout)
        self.assertEqual(report["reclaimed"], 1)
        self.assertEqual(report["zombies"], 2)
        self.assertEqual(
            self.podman_log.read_text().splitlines(),
            [f"stop {'a' * 64}", f"rm {'a' * 64}"],
        )
        self.assertEqual(json.loads(self.podman_state.read_text()), [])

    def test_remove_continues_when_stop_times_out_after_container_exits(self) -> None:
        container_id = "e" * 64
        self.podman_state.write_text(
            json.dumps(
                [container(container_id, "slow-stop", labels("hermit-old", "inv-old"))]
            )
        )
        self.write_live_state(self.OTHER_LIVE_AGENT, {})
        self.environment["FAKE_PODMAN_STOP_FAIL_AFTER_STOP"] = "1"

        result = self.reconcile()

        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(json.loads(result.stdout)["reclaimed"], 1)
        self.assertEqual(
            self.podman_log.read_text().splitlines(),
            [f"stop {container_id}", f"rm {container_id}"],
        )
        self.assertEqual(json.loads(self.podman_state.read_text()), [])

    def test_no_managed_container_reconcile_bootstraps_empty_authority(self) -> None:
        self.podman_state.write_text("[]")
        self.write_live_state(self.OTHER_LIVE_AGENT, {})

        result = self.reconcile()

        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(
            json.loads(self.registry.read_text()),
            {"schema_version": 1, "containers": {}},
        )

    def test_task_retained_fixture_requires_explicit_transfer(self) -> None:
        container_id = "b" * 64
        self.podman_state.write_text(
            json.dumps([container(container_id, "retained", labels("hermit-old", "inv-old", "task"))])
        )
        self.write_live_state(self.OTHER_LIVE_AGENT, {})

        before = self.reconcile()

        self.assertEqual(before.returncode, 1)
        self.assertEqual(json.loads(before.stdout)["transfer_required"], 1)
        self.assertEqual(self.podman_log.read_text(), "")

        transfer = self.run_tool(
            "transfer", "retained", "--task", "fixture-task", "--lifetime", "agent"
        )
        self.assertEqual(transfer.returncode, 0, transfer.stderr + transfer.stdout)
        self.write_live_state(
            [{"name": "hermit-new", "status": "busy", "tmux_pane_id": "%9"}],
            {"hermit-new": ["inv-new"]},
        )

        after = self.reconcile()

        self.assertEqual(after.returncode, 0, after.stderr + after.stdout)
        report = json.loads(after.stdout)
        self.assertEqual(report["live"], 1)
        self.assertEqual(report["reclaimed"], 0)
        self.assertEqual(self.podman_log.read_text(), "")

        # Transfer advances the durable current-owner registry while the
        # container retains its immutable creation-owner labels. The release
        # audit must follow the transfer authority, not reject or reuse the
        # stale creation owner as a proxy.
        current_owner = self.release_audit(
            "--owner",
            "hermit-new",
            "--target",
            str(self.directory / "unused"),
        )
        self.assertEqual(current_owner.returncode, 1)
        self.assertEqual(json.loads(current_owner.stdout)["owner_matches"], 1)

        former_owner = self.release_audit(
            "--owner",
            "hermit-old",
            "--target",
            str(self.directory / "unused"),
        )
        self.assertEqual(former_owner.returncode, 0, former_owner.stderr)
        self.assertEqual(json.loads(former_owner.stdout)["owner_matches"], 0)

    def test_live_and_unmanaged_containers_are_never_removed(self) -> None:
        self.podman_state.write_text(
            json.dumps(
                [
                    container("c" * 64, "live", labels("hermit-new", "inv-new")),
                    container("d" * 64, "legacy", None, zombies=3),
                ]
            )
        )
        self.write_live_state(
            [{"name": "hermit-new", "status": "busy", "tmux_pane_id": "%9"}],
            {"hermit-new": ["inv-new"]},
        )

        result = self.reconcile()

        self.assertEqual(result.returncode, 1)
        report = json.loads(result.stdout)
        self.assertEqual(report["live"], 1)
        self.assertEqual(report["unmanaged"], 1)
        self.assertEqual(report["reclaimed"], 0)
        self.assertEqual(self.podman_log.read_text(), "")
        self.assertEqual(len(json.loads(self.podman_state.read_text())), 2)

    def test_operational_failure_emits_renderable_tick_summary_and_stays_nonzero(
        self,
    ) -> None:
        """The pre-report error path must feed, not bypass, tick-hub capture."""
        self.podman_state.write_text("[]")
        missing_snapshot = self.directory / "missing-agents.json"

        result = self.run_tool(
            "reconcile", "--agents-json", str(missing_snapshot)
        )

        self.assertEqual(result.returncode, 1)
        captured = parse_kv_lines(result.stdout)
        self.assertEqual(captured["state"], "error")
        self.assertIn("could not complete", captured["summary"])
        config = config_from_yaml(
            (ROOT / "ci-hub" / "health" / "tick-hub.yaml").read_text()
        )
        lifecycle = config.by_name()["agent_container_lifecycle"]
        rendered = render_emit(lifecycle.emit, captured)
        self.assertIn(captured["summary"], rendered)
        self.assertNotIn("{summary}", rendered)

    def test_tick_still_refuses_lifecycle_emission_without_summary(self) -> None:
        """Do not weaken the renderer to hide a future producer regression."""
        config = config_from_yaml(
            (ROOT / "ci-hub" / "health" / "tick-hub.yaml").read_text()
        )
        lifecycle = config.by_name()["agent_container_lifecycle"]

        with self.assertRaisesRegex(
            UnresolvedPlaceholderError,
            r"refusing emission with unresolved placeholder\(s\): \{summary\}",
        ):
            render_emit(lifecycle.emit, {"state": "error"})

    def test_quickstart_is_pure_without_podman_or_state(self) -> None:
        self.environment["AGENT_PODMAN_BIN"] = str(self.directory / "missing-podman")
        result = self.run_tool("quickstart")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("never automatic deletion targets", result.stdout)
        self.assertFalse(self.registry.exists())

    def test_create_holds_lifecycle_fence_until_registration(self) -> None:
        self.podman_state.write_text("[]")

        result = self.run_tool("create", "--task", "fixture-task", "--", "image")

        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(
            self.podman_log.read_text().splitlines(),
            [
                "lifecycle-held",
                "exists-lifecycle-held",
                "exists-lifecycle-held",
            ],
        )
        registry = json.loads(self.registry.read_text())
        self.assertEqual(
            registry["containers"]["f" * 64]["owner_agent"], "hermit-new"
        )

    def test_create_retains_fence_from_early_cid_until_exact_engine_visibility(self) -> None:
        self.podman_state.write_text("[]")
        ready = self.directory / "early-cid-ready"
        gate = self.directory / "early-cid-gate"
        self.environment["FAKE_PODMAN_EARLY_CID_READY"] = str(ready)
        self.environment["FAKE_PODMAN_EARLY_CID_GATE"] = str(gate)
        process = subprocess.Popen(
            [
                "rust-script",
                str(TOOL),
                "create",
                "--task",
                "fixture-task",
                "--",
                "image",
            ],
            cwd=ROOT,
            env=self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.addCleanup(lambda: process.poll() is None and process.kill())
        for _ in range(500):
            if ready.exists() and self.registry.exists():
                break
            time.sleep(0.01)
        self.assertTrue(ready.exists(), "fake Podman never published its early cidfile")
        self.assertEqual(json.loads(self.podman_state.read_text()), [])
        self.assertIn("f" * 64, json.loads(self.registry.read_text())["containers"])

        lifecycle = self.registry.parent / "agent-container-lifecycle.lock"
        with lifecycle.open("a+") as lock:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

        gate.touch()
        stdout, stderr = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, stderr + stdout)

    def test_cleanup_query_error_retains_registry_under_lifecycle_fence(self) -> None:
        self.podman_state.write_text("[]")
        counter = self.directory / "exists-counter"
        self.environment["FAKE_PODMAN_EXISTS_COUNTER"] = str(counter)
        self.environment["FAKE_PODMAN_EXISTS_FAIL_AFTER"] = "1"

        result = self.run_tool("create", "--task", "fixture-task", "--", "image")

        self.assertEqual(result.returncode, 1)
        self.assertIn("container exists was inconclusive", result.stderr)
        registry = json.loads(self.registry.read_text())
        self.assertIn("f" * 64, registry["containers"])
        self.assertEqual(
            self.podman_log.read_text().splitlines()[-1],
            "exists-lifecycle-held",
        )

    def release_audit(
        self, *arguments: str, lock_operation: int = fcntl.LOCK_EX
    ) -> subprocess.CompletedProcess[str]:
        token = "fixture-release-token"
        lifecycle = self.registry.parent / "agent-container-lifecycle.lock"
        with lifecycle.open("w+") as lock:
            lock.write(token + "\n")
            lock.flush()
            os.fsync(lock.fileno())
            fcntl.flock(lock, lock_operation)
            return self.run_tool(
                "release-audit", "--fence-token", token, *arguments, "--json"
            )

    def test_release_audit_brackets_owner_mount_and_unrelated_cases(self) -> None:
        target = self.directory / "worktrees" / "slot01" / "hermit"
        target.parent.mkdir(parents=True)
        unrelated = self.directory / "elsewhere"
        unrelated.mkdir()
        alias = self.directory / "slot-parent-alias"
        alias.symlink_to(target.parent, target_is_directory=True)
        owner_id = "a" * 64
        self.registry.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "containers": {
                        owner_id: ownership(owner_id, "hermit-old", "inv-old")
                    },
                }
            )
        )
        self.podman_state.write_text(
            json.dumps(
                [
                    container(
                        owner_id,
                        "owner-without-mount",
                        labels("hermit-old", "inv-old"),
                    ),
                    container(
                        "b" * 64,
                        "unmanaged-target-mount",
                        None,
                        mounts=[{"Source": str(target.parent)}],
                    ),
                    container(
                        "c" * 64,
                        "unrelated",
                        None,
                        mounts=[{"Source": str(unrelated)}],
                    ),
                    container(
                        "d" * 64,
                        "symlink-target-mount",
                        None,
                        mounts=[{"Source": str(alias)}],
                    ),
                ]
            )
        )

        refused = self.release_audit(
            "--owner", "hermit-old", "--target", str(target)
        )
        self.assertEqual(refused.returncode, 1, refused.stderr + refused.stdout)
        report = json.loads(refused.stdout)
        self.assertEqual(report["owner_matches"], 1)
        self.assertEqual(report["target_mounts"], 2)
        self.assertEqual(len(report["details"]), 3)

        accepted = self.release_audit(
            "--owner", "different-owner", "--target", str(self.directory / "unused")
        )
        self.assertEqual(accepted.returncode, 0, accepted.stderr + accepted.stdout)
        self.assertEqual(json.loads(accepted.stdout)["inspected"], 4)

    def test_release_audit_refuses_malformed_or_unresolvable_mount_sources(self) -> None:
        target = self.directory / "worktrees" / "slot01" / "hermit"
        target.mkdir(parents=True)
        unrelated = self.directory / "unrelated"
        unrelated.mkdir()
        self.registry.write_text(
            json.dumps({"schema_version": 1, "containers": {}})
        )

        malformed = [
            ({}, "has no Source field"),
            ({"Source": {"concealed": str(target)}}, "is not a string"),
            ({"Source": 17}, "is not a string"),
        ]
        for index, (mount, expected) in enumerate(malformed):
            with self.subTest(mount=mount):
                self.podman_state.write_text(
                    json.dumps(
                        [
                            container(
                                str(index + 1) * 64,
                                "malformed-mount-source",
                                None,
                                mounts=[mount],
                            )
                        ]
                    )
                )
                refused = self.release_audit(
                    "--owner", "unrelated-owner", "--target", str(target)
                )
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(expected, refused.stderr)

        # Empty sources are a legitimate Podman schema value, while a resolved
        # absolute unrelated source is positive evidence that the parser is not
        # simply refusing every mount entry.
        self.podman_state.write_text(
            json.dumps(
                [
                    container(
                        "4" * 64,
                        "well-shaped-unrelated-mounts",
                        None,
                        mounts=[{"Source": ""}, {"Source": str(unrelated)}],
                    )
                ]
            )
        )
        accepted = self.release_audit(
            "--owner", "unrelated-owner", "--target", str(target)
        )
        self.assertEqual(accepted.returncode, 0, accepted.stderr + accepted.stdout)
        observation = json.loads(accepted.stdout)["container_observation"]
        self.assertEqual(observation[0]["mount_sources"], ["", str(unrelated)])

        # The path fence can make an alias dangling while the kernel mount still
        # refers to the moved inode. Losing resolution must refuse, not erase
        # the pre-fence identity and report target_mounts=0.
        alias = self.directory / "late-alias"
        alias.symlink_to(target, target_is_directory=True)
        fenced = target.parent / ".hermit.release-worktree-fixture"
        target.rename(fenced)
        self.podman_state.write_text(
            json.dumps(
                [
                    container(
                        "5" * 64,
                        "dangling-alias-mount",
                        None,
                        mounts=[{"Source": str(alias)}],
                    )
                ]
            )
        )
        dangling = self.release_audit(
            "--owner",
            "unrelated-owner",
            "--target",
            str(target),
            "--target",
            str(fenced),
        )
        self.assertEqual(dangling.returncode, 1, dangling.stdout)
        self.assertIn("cannot be resolved", dangling.stderr)

    def test_release_audit_requires_exclusive_fence_and_registry_authority(self) -> None:
        self.podman_state.write_text("[]")

        missing_registry = self.release_audit("--owner", "hermit-old")
        self.assertEqual(missing_registry.returncode, 1)
        self.assertIn("release ownership registry is unavailable", missing_registry.stderr)

        self.registry.write_text(
            json.dumps({"schema_version": 1, "containers": {}})
        )
        shared_fence = self.release_audit(
            "--owner", "hermit-old", lock_operation=fcntl.LOCK_SH
        )
        self.assertEqual(shared_fence.returncode, 1)
        self.assertIn("independently held exclusive lifecycle fence", shared_fence.stderr)

        managed_id = "9" * 64
        self.podman_state.write_text(
            json.dumps(
                [
                    container(
                        managed_id,
                        "managed-without-current-owner",
                        labels("former-owner", "former-invocation"),
                    )
                ]
            )
        )
        missing_entry = self.release_audit("--owner", "current-owner")
        self.assertEqual(missing_entry.returncode, 1)
        self.assertIn("has no durable current-owner registry entry", missing_entry.stderr)

        self.podman_state.write_text("[]")
        self.registry.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "containers": {
                        managed_id: ownership(
                            managed_id, "current-owner", "current-invocation"
                        )
                    },
                }
            )
        )
        registry_only = self.release_audit("--owner", "current-owner")
        self.assertEqual(registry_only.returncode, 0, registry_only.stderr)
        self.assertEqual(
            json.loads(registry_only.stdout)["stale_registry_entries_removed"], 1
        )
        self.assertEqual(json.loads(self.registry.read_text())["containers"], {})

        self.registry.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "containers": {
                        managed_id: ownership(
                            managed_id, "current-owner", "current-invocation"
                        )
                    },
                }
            )
        )
        counter = self.directory / "release-exists-counter"
        self.environment["FAKE_PODMAN_EXISTS_COUNTER"] = str(counter)
        self.environment["FAKE_PODMAN_EXISTS_FAIL_AFTER"] = "0"
        uncertain_registry_only = self.release_audit("--owner", "current-owner")
        self.assertEqual(uncertain_registry_only.returncode, 1)
        self.assertIn("uncertain engine existence", uncertain_registry_only.stderr)
        self.assertIn(
            managed_id, json.loads(self.registry.read_text())["containers"]
        )

        tampered = ownership("8" * 64, "current-owner", "current-invocation")
        self.registry.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "containers": {managed_id: tampered},
                }
            )
        )
        tampered_binding = self.release_audit("--owner", "current-owner")
        self.assertEqual(tampered_binding.returncode, 1)
        self.assertIn("does not bind payload ID", tampered_binding.stderr)
        self.assertIn(
            managed_id, json.loads(self.registry.read_text())["containers"]
        )

        self.registry.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "containers": {
                        managed_id: ownership(
                            managed_id, "current-owner", "current-invocation"
                        )
                    },
                }
            )
        )
        self.environment["FAKE_PODMAN_EXISTS_FORCE_PRESENT"] = "1"
        exists_without_census = self.release_audit("--owner", "current-owner")
        self.assertEqual(exists_without_census.returncode, 1)
        self.assertIn(
            "exists but is absent from the exact engine census",
            exists_without_census.stderr,
        )
        self.assertIn(
            managed_id, json.loads(self.registry.read_text())["containers"]
        )


if __name__ == "__main__":
    unittest.main()
