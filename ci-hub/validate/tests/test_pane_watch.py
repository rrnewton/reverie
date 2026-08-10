from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import pane_watch  # noqa: E402
import run_registry  # noqa: E402


class PaneWatchTest(unittest.TestCase):
    def test_collected_killed_unit_uses_durable_inner_exit_not_success_placeholder(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text(
                "validate-lock: supervisor received signal 15\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=50.197s cpu=0.2s "
                "cpu_user=0.1s cpu_system=0.1s exit=143\n"
            )
            run_registry.write_record(
                record,
                {
                    "schema_version": 1,
                    "state": "running",
                    "target": "a" * 40,
                    "log": str(log),
                },
            )
            output = io.StringIO()
            with mock.patch.object(pane_watch, "service_properties", return_value=None):
                with contextlib.redirect_stdout(output):
                    rc = pane_watch.main(
                        [
                            "--unit",
                            "validate-test.service",
                            "--target",
                            "a" * 40,
                            "--checkout",
                            str(root),
                            "--log",
                            str(log),
                            "--record",
                            str(record),
                            "--appearance-seconds",
                            "0",
                        ]
                    )

            self.assertEqual(143, rc)
            durable = run_registry.read_record(record)
            self.assertEqual("killed", durable["state"])
            self.assertEqual("terminated", durable["result"])
            self.assertEqual(143, durable["exit_code"])
            self.assertEqual(15, durable["signal"])
            self.assertIn("state=killed result=terminated exit=143", output.getvalue())

    def test_boxing_proof_is_bound_to_descendant_process_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            proc = Path(temporary)
            fixtures = {
                10: (1, "/user.slice/3pai_sandbox.slice/validate.service"),
                11: (10, "/user.slice/safe-ci-dag-runner/safe-ci-node.scope"),
                12: (1, "/user.slice/safe-ci-unrelated.scope"),
            }
            for pid, (ppid, cgroup) in fixtures.items():
                directory = proc / str(pid)
                directory.mkdir()
                (directory / "stat").write_text(
                    f"{pid} (fixture) S {ppid} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
                )
                (directory / "cgroup").write_text(f"0::{cgroup}\n")

            self.assertEqual(
                {"/user.slice/safe-ci-dag-runner/safe-ci-node.scope"},
                pane_watch.safe_ci_cgroups(10, proc),
            )

    def test_durable_handle_round_trip_preserves_observer_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "runs/validate-test.json"
            run_registry.write_record(
                path,
                {
                    "schema_version": 1,
                    "state": "running",
                    "target": "a" * 40,
                },
            )
            updated = run_registry.update_record(
                path,
                state="completed",
                observed_safe_ci_cgroups=["/safe-ci-test.scope"],
            )

            self.assertEqual(updated, run_registry.read_record(path))
            self.assertEqual("completed", updated["state"])
            self.assertEqual(["/safe-ci-test.scope"], updated["observed_safe_ci_cgroups"])

    def test_concurrent_final_updates_do_not_erase_each_other(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "runs/validate-test.json"
            run_registry.write_record(path, {"schema_version": 1, "state": "running"})
            barrier = threading.Barrier(3)

            def update(**fields: object) -> None:
                barrier.wait()
                run_registry.update_record(path, **fields)

            caller = threading.Thread(target=update, kwargs={"exit_code": 0})
            observer = threading.Thread(
                target=update,
                kwargs={"observed_safe_ci_cgroups": ["/safe-ci-test.scope"]},
            )
            caller.start()
            observer.start()
            barrier.wait()
            caller.join()
            observer.join()

            durable = run_registry.read_record(path)
            self.assertEqual(0, durable["exit_code"])
            self.assertEqual(["/safe-ci-test.scope"], durable["observed_safe_ci_cgroups"])


if __name__ == "__main__":
    unittest.main()
