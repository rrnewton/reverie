#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "red-cell-register.py"
SPEC = importlib.util.spec_from_file_location("red_cell_register", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
register = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = register
SPEC.loader.exec_module(register)


def command(cwd: Path, *arguments: str, env: dict[str, str] | None = None) -> str:
    completed = subprocess.run(
        list(arguments),
        cwd=cwd,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(completed.stderr or completed.stdout)
    return completed.stdout.strip()


def cell(
    test: str,
    *,
    status: str,
    measurement: str,
    reason: str,
) -> dict[str, object]:
    return {
        "lane": "portable",
        "category": "applications",
        "test": test,
        "mode": "verify",
        "backend": "ptrace",
        "status": status,
        "measurement": measurement,
        "not_selected_by_full_reason": {"result": "test", "reason": reason},
        "observations": [],
    }


class RedCellRegisterTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.repo = Path(self.temporary.name)
        command(self.repo, "git", "init", "-q")
        command(self.repo, "git", "config", "user.name", "Test")
        command(self.repo, "git", "config", "user.email", "test@example.com")
        (self.repo / "ci/compat-envelope").mkdir(parents=True)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_reason_field_migration_accepts_each_schema_and_refuses_conflict(self) -> None:
        current = cell(
            "applications/current",
            status="red",
            measurement="never-measured",
            reason="current reason",
        )
        self.assertEqual(
            {"result": "test", "reason": "current reason"},
            register.reason_value(current),
        )

        legacy = dict(current)
        legacy["ci_disabled_reason"] = legacy.pop("not_selected_by_full_reason")
        self.assertEqual(
            {"result": "test", "reason": "current reason"},
            register.reason_value(legacy),
        )

        conflicting = dict(current)
        conflicting["ci_disabled_reason"] = {"result": "test", "reason": "old"}
        with self.assertRaisesRegex(
            register.RegisterError,
            "conflicting not_selected_by_full_reason and ci_disabled_reason values",
        ):
            register.reason_value(conflicting)

    def commit(self, date: str, cells: list[dict[str, object]]) -> str:
        path = self.repo / register.CELL_PATH
        path.write_text(json.dumps({"cells": cells}) + "\n", encoding="utf-8")
        command(self.repo, "git", "add", register.CELL_PATH)
        env = dict(os.environ)
        env["GIT_AUTHOR_DATE"] = date
        env["GIT_COMMITTER_DATE"] = date
        command(self.repo, "git", "commit", "-q", "-m", date, env=env)
        return command(self.repo, "git", "rev-parse", "HEAD")

    def test_dates_current_values_not_old_values(self) -> None:
        first = self.commit(
            "2026-01-01T00:00:00Z",
            [
                cell("applications/a", status="red", measurement="never-measured", reason="a1"),
                cell("applications/b", status="red", measurement="never-measured", reason="b1"),
            ],
        )
        second = self.commit(
            "2026-01-02T00:00:00Z",
            [
                cell(
                    "applications/a",
                    status="green",
                    measurement="measured-and-passed",
                    reason="a1",
                ),
                cell(
                    "applications/b",
                    status="red",
                    measurement="measured-and-passed",
                    reason="b2",
                ),
            ],
        )
        third = self.commit(
            "2026-01-03T00:00:00Z",
            [
                cell("applications/a", status="red", measurement="diverged", reason="a3"),
                cell(
                    "applications/b",
                    status="red",
                    measurement="measured-and-passed",
                    reason="b2",
                ),
            ],
        )
        current = register.load_cells(self.repo, "HEAD")["cells"]
        dates = register.derive_dates(self.repo, "HEAD", current)

        a = dates[register.cell_key(current[0])]
        self.assertEqual(third, a["status"].commit)
        self.assertEqual(third, a["measurement"].commit)
        self.assertEqual(third, a["reason"].commit)

        b = dates[register.cell_key(current[1])]
        self.assertEqual(first, b["status"].commit)
        self.assertEqual(second, b["measurement"].commit)
        self.assertEqual(second, b["reason"].commit)

    def test_current_work_requires_current_claim_data(self) -> None:
        target = cell(
            "applications/kvm-python-examples",
            status="red",
            measurement="diverged",
            reason="comparison diverged",
        )
        active = {
            "state": "ok",
            "claimed": [
                {
                    "id": "debug-kvm-python-examples",
                    "title": "Debug applications/kvm-python-examples",
                    "owner": "hermit-002",
                }
            ],
        }
        value = register.current_work_for(
            target,
            active,
            {"debug-kvm-python-examples": "applications/kvm-python-examples/verify/ptrace"},
            None,
        )
        self.assertEqual("current claimed task mentions this cell", value["state"])
        self.assertEqual("exact cell id", value["tasks"][0]["match"])

        self.assertIsNone(
            register.task_matches(
                target,
                "debug-kvm-python-examples-extra",
                "applications/kvm-python-examples-extra/verify/ptrace",
            )
        )

        unknown = register.current_work_for(
            target,
            {"state": "degraded"},
            {},
            "ORC agent snapshot missing",
        )
        self.assertEqual("unknown", unknown["state"])
        self.assertEqual("ORC agent snapshot missing", unknown["reason"])

    def test_observation_event_ids_are_unique_and_sorted(self) -> None:
        target = cell(
            "applications/a",
            status="red",
            measurement="measured-and-passed",
            reason="not selected",
        )
        target["observations"] = [
            {"event_ids": ["series-b", "series-a"]},
            {"event_ids": ["series-b"]},
        ]
        self.assertEqual(
            ["series-a", "series-b"],
            register.observation_event_ids(target),
        )


if __name__ == "__main__":
    unittest.main()
