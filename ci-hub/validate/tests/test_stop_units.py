from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import stop_units  # noqa: E402
import run_registry  # noqa: E402


def completed(command: list[str], rc: int = 0, stdout: str = "", stderr: str = ""):
    return subprocess.CompletedProcess(command, rc, stdout, stderr)


class FakeSystemctl:
    def __init__(self, active: set[str] | None = None) -> None:
        self.active = (
            set(active)
            if active is not None
            else {"validate-a.service", "validate-b.scope"}
        )
        self.stops: list[str] = []

    def __call__(self, command: list[str], **_kwargs: object):
        if "list-units" in command:
            return completed(
                command,
                stdout=(
                    "validate-b.scope loaded active running B\n"
                    "validate-a.service loaded active running A\n"
                ),
            )
        if "is-active" in command:
            unit = command[-1]
            return completed(command, rc=0 if unit in self.active else 3)
        if "stop" in command:
            unit = command[-1]
            self.stops.append(unit)
            self.active.discard(unit)
            return completed(command)
        raise AssertionError(command)


class StopUnitsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.state_root = Path(self.temporary.name)
        self.actor = stop_units.Actor(agent="terminating-agent", pid=4242)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_handle(self, unit: str) -> Path:
        path = run_registry.record_path(self.state_root, unit)
        path.parent.mkdir(parents=True, exist_ok=True)
        run_registry.write_current_record(
            path,
            {
                "schema_version": run_registry.SCHEMA_VERSION,
                "kind": "validate",
                "state": "running",
                "unit": unit,
                "target": "a" * 40,
                "repo": "rrnewton/hermit",
                "checkout": "/tmp/checkout",
                "source_checkout": "/tmp/source",
                "temporary_checkout": True,
                "cargo_home": "/tmp/cargo",
                "log": "/tmp/validate.log",
                "agent": "running-agent",
                "pr": None,
                "started_at": "2026-08-31T12:00:00+00:00",
                "parent_checkout_head": "a" * 40,
                "producer": run_registry.PRODUCER,
                "admission": "ci-hub validate-lock",
                "pane_role": "observer-only",
                "validate_lock_child_deadline_seconds": 3600,
                "e2e_result_root": "/tmp/e2e",
                "safe_ci_dag_runner_log_dir": "/tmp/dag",
                "hermit_run_timeout_seconds": 3300,
                "process_identity": {
                    "pid": 99,
                    "start_ticks": 100,
                    "boot_id": "boot-one",
                },
            },
        )
        return path

    def invoke(self, arguments: list[str], fake: FakeSystemctl) -> int:
        return stop_units.main(
            [
                *arguments,
                "--reason",
                "superseded by an exact-head run",
                "--state-root",
                str(self.state_root),
            ],
            run=fake,
            actor_resolver=lambda _root: self.actor,
        )

    def test_reason_must_be_nonempty_single_line_text(self) -> None:
        for reason in ("", "   ", "first\nsecond", "reason\0suffix"):
            with self.subTest(reason=reason):
                with self.assertRaisesRegex(argparse.ArgumentTypeError, "must be"):
                    stop_units.nonempty_reason(reason)
        self.assertEqual("reason", stop_units.nonempty_reason("  reason  "))

    def test_all_stops_each_enumerated_unit_and_verifies_inactive(self) -> None:
        fake = FakeSystemctl()
        records = [self.write_handle(unit) for unit in sorted(fake.active)]

        self.assertEqual(0, self.invoke(["--all"], fake))
        self.assertEqual(["validate-a.service", "validate-b.scope"], fake.stops)
        self.assertEqual(set(), fake.active)
        for record in records:
            requests = run_registry.read_record(record)["termination_requests"]
            self.assertEqual(1, len(requests))
            self.assertEqual("terminating-agent", requests[0]["agent"])
            self.assertEqual(4242, requests[0]["pid"])
            self.assertEqual("superseded by an exact-head run", requests[0]["reason"])
            self.assertIn("confirmed_at", requests[0])

    def test_targeted_stop_never_accepts_an_unrelated_unit(self) -> None:
        fake = FakeSystemctl()

        self.assertEqual(
            2,
            self.invoke(["--unit", "github-runner.service"], fake),
        )
        self.assertEqual([], fake.stops)

    def test_inactive_unit_is_reported_without_a_stop(self) -> None:
        fake = FakeSystemctl(set())

        def fail_if_called(_root: Path) -> stop_units.Actor:
            raise AssertionError("inactive units do not need actor resolution")

        self.assertEqual(
            0,
            stop_units.main(
                [
                    "--unit",
                    "validate-old.service",
                    "--reason",
                    "superseded",
                    "--state-root",
                    str(self.state_root),
                ],
                run=fake,
                actor_resolver=fail_if_called,
            ),
        )
        self.assertEqual([], fake.stops)

    def test_active_unit_without_a_current_handle_is_refused_before_any_stop(
        self,
    ) -> None:
        fake = FakeSystemctl()
        self.write_handle("validate-a.service")

        self.assertEqual(2, self.invoke(["--all"], fake))
        self.assertEqual([], fake.stops)
        self.assertEqual(
            [],
            run_registry.read_record(
                run_registry.record_path(self.state_root, "validate-a.service")
            ).get("termination_requests", []),
        )


if __name__ == "__main__":
    unittest.main()
