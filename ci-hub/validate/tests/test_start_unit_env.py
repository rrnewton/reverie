#!/usr/bin/env python3
"""Network-free tests for the environment inherited by validate units."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import start_unit as su  # noqa: E402


TARGET = "9" * 40


def setenvs(command: list[str]) -> dict[str, str]:
    """Extract the KEY=VALUE pairs systemd-run is told to set."""
    out: dict[str, str] = {}
    for flag, value in zip(command, command[1:]):
        if flag == "--setenv" and "=" in value:
            key, _, val = value.partition("=")
            out[key] = val
    return out


def build(root: Path, environment: dict[str, str]) -> list[str]:
    return su.build_systemd_command(
        root=root,
        checkout=root / "hermit",
        target=TARGET,
        agent="test-agent",
        unit="validate-test.service",
        record=root / "ignored/validate/runs/validate-test.json",
        log=root / "out.log",
        pr=None,
        validate_args=["full"],
        wait=1,
        hold=1,
        child_deadline=1,
        environment=environment,
    )


class StartUnitEnvTest(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile

        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.base_env = {"HOME": "/home/test", "PATH": "/usr/bin"}

    def test_parent_libunwind_copy_is_not_injected(self) -> None:
        """Validation must build against the product's documented host dependency.

        The old fallback added a private directory to ``LIBRARY_PATH``. That
        selected ``libunwind-ptrace.so`` even though the installed development
        package selects its static archive. The resulting Hermit binary only ran
        while the parent-supplied ``LD_LIBRARY_PATH`` survived; LiteInst's clean
        activation probe then terminated before its handshake. A retained
        parent copy must no longer change the product build.
        """
        libdir = self.root / "ignored/lu-parity/usr/lib64"
        (libdir / "pkgconfig").mkdir(parents=True)
        (libdir / "pkgconfig/libunwind-ptrace.pc").write_text("Name: libunwind-ptrace\n")
        (libdir / "libunwind-ptrace.so.0").write_text("")
        env = setenvs(build(self.root, dict(self.base_env)))
        for var in ("PKG_CONFIG_PATH", "LIBRARY_PATH", "LD_LIBRARY_PATH"):
            self.assertNotIn(var, env, f"{var} must not redirect the product build")

    # -- the pre-existing contract must survive ------------------------------

    def test_home_and_path_still_propagate(self) -> None:
        env = setenvs(build(self.root, dict(self.base_env)))
        self.assertEqual(env["HOME"], "/home/test")
        self.assertEqual(env["PATH"], "/usr/bin")
        self.assertEqual(env["CI_HUB_VALIDATE_PRODUCER"], "systemd-user-v1")

    def test_e2e_results_and_runner_logs_survive_checkout_removal(self) -> None:
        ambient = {
            **self.base_env,
            "E2E_RESULT_ROOT": "/tmp/caller-chosen-results",
            "SAFE_CI_DAG_RUNNER_LOG_DIR": "/tmp/caller-chosen-logs",
        }
        env = setenvs(build(self.root, ambient))
        artifact_root = self.root / "ignored/validate/artifacts/validate-test.service"
        self.assertEqual(env["E2E_RESULT_ROOT"], str(artifact_root / "e2e"))
        self.assertEqual(
            env["SAFE_CI_DAG_RUNNER_LOG_DIR"],
            str(artifact_root / "safe-ci-dag-runner"),
        )
        self.assertEqual(
            env["DAGRUN_LOG_DIR"],
            str(artifact_root / "safe-ci-dag-runner"),
        )
        self.assertFalse(Path(env["E2E_RESULT_ROOT"]).is_relative_to(self.root / "hermit"))
        self.assertFalse(
            Path(env["SAFE_CI_DAG_RUNNER_LOG_DIR"]).is_relative_to(self.root / "hermit")
        )
        self.assertFalse(Path(env["DAGRUN_LOG_DIR"]).is_relative_to(self.root / "hermit"))

    def test_console_report_names_both_retained_paths(self) -> None:
        artifact_root = self.root / "ignored/validate/artifacts/validate-test"
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            su.emit_report(
                {
                    "event": "handle",
                    "unit": "validate-test.service",
                    "target": TARGET,
                    "state": "running",
                    "e2e_result_root": str(artifact_root / "e2e"),
                    "safe_ci_dag_runner_log_dir": str(
                        artifact_root / "safe-ci-dag-runner"
                    ),
                },
                json_output=False,
            )
        # Neither directory exists in this fixture, and a path that was never
        # created is not an artifact -- naming it sends the reader looking for
        # output that cannot be there. Both lines are therefore suppressed; the
        # populated and empty cases are asserted below.
        self.assertNotIn("E2E_RESULTS", output.getvalue())
        self.assertNotIn("RUNNER_LOGS", output.getvalue())

        # ...and the report still names them once they exist.
        (artifact_root / "e2e").mkdir(parents=True)
        (artifact_root / "safe-ci-dag-runner").mkdir(parents=True)
        (artifact_root / "e2e" / "row.json").write_text("{}\n")
        present = io.StringIO()
        with contextlib.redirect_stdout(present):
            su.emit_report(
                {
                    "event": "handle",
                    "unit": "validate-test.service",
                    "target": TARGET,
                    "state": "running",
                    "e2e_result_root": str(artifact_root / "e2e"),
                    "safe_ci_dag_runner_log_dir": str(
                        artifact_root / "safe-ci-dag-runner"
                    ),
                },
                json_output=False,
            )
        self.assertIn(
            f"E2E_RESULTS {artifact_root / 'e2e'} [1 entry]",
            present.getvalue(),
        )
        self.assertIn(
            f"RUNNER_LOGS {artifact_root / 'safe-ci-dag-runner'} [empty]",
            present.getvalue(),
        )

    def _emit_runner_logs(self, path, *, json_output=False) -> str:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            su.emit_report(
                {
                    "event": "handle",
                    "unit": "validate-test.service",
                    "target": TARGET,
                    "state": "running",
                    "safe_ci_dag_runner_log_dir": str(path),
                },
                json_output=json_output,
            )
        return output.getvalue()

    def test_retained_path_state_distinguishes_the_three_cases(self) -> None:
        """Absent, empty and populated must not read identically.

        This is the defect: the line was printed on the truthiness of a computed
        path string, which is always true, so all three printed the same.
        """
        absent = self.root / "never-created"
        empty = self.root / "empty-logs"
        empty.mkdir(parents=True)
        populated = self.root / "real-logs"
        populated.mkdir(parents=True)
        (populated / "node.log").write_text("a log line\n")

        self.assertEqual(su.retained_path_state(str(absent)), "absent")
        self.assertEqual(su.retained_path_state(str(empty)), "empty")
        self.assertEqual(su.retained_path_state(str(populated)), "1 entry")
        (populated / "second.log").write_text("another\n")
        self.assertEqual(su.retained_path_state(str(populated)), "2 entries")

    def test_console_line_names_the_state_both_ways(self) -> None:
        """A populated run reports populated; an absent one says so."""
        populated = self.root / "real-logs-2"
        populated.mkdir(parents=True)
        (populated / "node.log").write_text("x\n")
        absent = self.root / "never-created-2"

        empty = self.root / "empty-logs-2"
        empty.mkdir(parents=True)

        good = self._emit_runner_logs(populated)
        self.assertIn(f"RUNNER_LOGS {populated} [1 entry]", good)

        # EMPTY IS STILL REPORTED. The run created the directory and wrote
        # nothing into it, which is a fact the reader wants.
        blank = self._emit_runner_logs(empty)
        self.assertIn(f"RUNNER_LOGS {empty} [empty]", blank)

        # ABSENT IS SUPPRESSED, and that is the change: for a run that refused
        # before starting, this path never existed, and printing it sent the
        # owner hunting for output that could not be there.
        bad = self._emit_runner_logs(absent)
        self.assertNotIn("RUNNER_LOGS", bad)

        # The three cases must still be distinguishable from one another.
        self.assertNotEqual(good, blank)
        self.assertNotEqual(good, bad)
        self.assertNotEqual(blank, bad)

    def test_json_receipt_carries_the_state_too(self) -> None:
        """The JSON receipt is what machines read, so it needs the distinction."""
        empty = self.root / "empty-logs-json"
        empty.mkdir(parents=True)
        payload = json.loads(self._emit_runner_logs(empty, json_output=True))
        self.assertEqual(payload["safe_ci_dag_runner_log_dir_state"], "empty")
        # Additive: the original key keeps its exact meaning.
        self.assertEqual(payload["safe_ci_dag_runner_log_dir"], str(empty))

    def test_state_never_raises_and_never_fails_the_run(self) -> None:
        """An unusable path is a reporting gap, not a product failure."""
        not_a_dir = self.root / "a-file"
        not_a_dir.write_text("not a directory\n")
        self.assertEqual(su.retained_path_state(str(not_a_dir)), "not a directory")
        # A path that cannot even be stat'd must still return, not raise.
        self.assertIsInstance(su.retained_path_state("\0invalid"), str)

    def test_canonical_run_record_is_passed_to_validate_lock(self) -> None:
        command = build(self.root, dict(self.base_env))
        index = command.index("--run-record")
        self.assertEqual(
            command[index + 1],
            str(self.root / "ignored/validate/runs/validate-test.json"),
        )

    def test_missing_home_or_path_is_refused(self) -> None:
        for missing in ("HOME", "PATH"):
            env = dict(self.base_env)
            del env[missing]
            with self.assertRaises(ValueError):
                build(self.root, env)

    # -- parent-owned validation authorities remain reachable -----------------

    def test_parent_root_is_propagated_for_shared_authorities(self) -> None:
        """DEV_HERMIT_PARENT reaches consumers that still need the workspace root.

        The ledger producer deliberately does not consult this variable: it uses
        one fixed adapter path even when DEV_HERMIT_PARENT is absent. Other
        parent-owned operations (receipt finalization, durable logs and ci-hub
        helpers) still need the workspace root, so the admitted unit propagates
        it explicitly.

        Asserting the exact path, not just presence: pointing at the wrong root
        would make those other authorities unreachable.
        """
        env = setenvs(build(self.root, dict(self.base_env)))
        self.assertIn(
            "DEV_HERMIT_PARENT", env, "the unit cannot reach parent-owned authorities"
        )
        self.assertEqual(env["DEV_HERMIT_PARENT"], str(self.root))

    def test_parent_ledger_is_set_even_without_libunwind(self) -> None:
        # The libunwind block is conditional; the ledger anchor must not be
        # accidentally nested inside it, or receipts vanish on hosts that have
        # libunwind installed system-wide.
        env = setenvs(build(self.root, dict(self.base_env)))
        self.assertEqual(env["DEV_HERMIT_PARENT"], str(self.root))

    def test_make_validate_entrypoint_is_literal_and_admitted(self) -> None:
        command = su.build_systemd_command(
            root=self.root,
            checkout=self.root / "hermit",
            target=TARGET,
            agent="owner-checkout-watch",
            unit="validate-owner.service",
            record=self.root / "ignored/validate/runs/validate-owner.json",
            log=self.root / "owner.log",
            pr=None,
            validate_args=[],
            wait=1,
            hold=1,
            child_deadline=1,
            environment=dict(self.base_env),
            make_validate=True,
        )
        joined = " ".join(command)
        self.assertIn("validate-lock run", joined)
        self.assertIn("with-proxy make validate", joined)
        self.assertNotIn("./scripts/validate.rs", joined)

    def test_owner_symlink_exception_keeps_literal_make_and_marks_dirty_run(self) -> None:
        command = su.build_systemd_command(
            root=self.root,
            checkout=self.root / "hermit",
            target=TARGET,
            agent="owner-checkout-watch",
            unit="validate-owner.service",
            record=self.root / "ignored/validate/runs/validate-owner.json",
            log=self.root / "owner.log",
            pr=None,
            validate_args=[],
            wait=1,
            hold=1,
            child_deadline=1,
            environment=dict(self.base_env),
            make_validate=True,
            allow_owner_validate_symlink=True,
        )
        joined = " ".join(command)
        self.assertIn("VALIDATE_RUN_ON_DIRTY_TREE=1", joined)
        self.assertIn("with-proxy make validate", joined)


class RelayInnerRefusalTest(unittest.TestCase):
    """The wrapper must pass the inner run's words through, not summarise.

    THE DEFECT THIS LOCKS DOWN: validate refused with exit 2, named the gate it
    hit, gave the commit distance and printed its bypass flag -- and the
    wrapper replaced all of that with "CANONICAL-VERDICT-UNAVAILABLE: no receipt for
    commit". Reporting the ABSENCE of a receipt where a REASON exists describes
    a consequence as if it were the cause.
    """

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def test_relays_the_inner_text_verbatim(self) -> None:
        log = self.root / "run.log"
        inner = (
            "validate: REFUSED (exit 2) rebase-freshness: HEAD is 37\n"
            "commits behind origin/main. Rebase first, or pass\n"
            "--allow-stale-base to proceed.\n"
        )
        log.write_text(inner)
        sink = io.StringIO()
        self.assertTrue(su.relay_inner_output(log, out=sink))
        shown = sink.getvalue()
        # VERBATIM: every line the run wrote is present and unaltered.
        for line in inner.strip().splitlines():
            self.assertIn(line, shown)
        self.assertIn("--allow-stale-base", shown)

    def test_reports_nothing_when_there_is_nothing_to_relay(self) -> None:
        empty = self.root / "empty.log"
        empty.write_text("   \n")
        self.assertFalse(su.relay_inner_output(empty, out=io.StringIO()))
        # A log that does not exist must not raise; a validate cannot fail
        # because its own output could not be echoed.
        self.assertFalse(
            su.relay_inner_output(self.root / "missing.log", out=io.StringIO())
        )

    def test_a_long_log_is_tailed_and_says_so(self) -> None:
        log = self.root / "big.log"
        log.write_text("filler\n" * 20000 + "THE REFUSAL LINE\n")
        sink = io.StringIO()
        self.assertTrue(su.relay_inner_output(log, out=sink, limit=200))
        shown = sink.getvalue()
        self.assertIn("THE REFUSAL LINE", shown)
        self.assertIn("(tail)", shown)


class LogTailTest(unittest.TestCase):
    """The caller who started a validate should be able to watch it."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def test_streams_only_what_is_new(self) -> None:
        log = self.root / "unit.log"
        log.write_text("first\n")
        sink = io.StringIO()
        tail = su.LogTail(log, out=sink)
        tail.pump()
        self.assertEqual(sink.getvalue(), "first\n")
        # Appending streams only the appended bytes -- no re-printing.
        with log.open("a") as handle:
            handle.write("second\n")
        tail.pump()
        self.assertEqual(sink.getvalue(), "first\nsecond\n")
        # A poll with nothing new adds nothing.
        tail.pump()
        self.assertEqual(sink.getvalue(), "first\nsecond\n")

    def test_a_missing_log_is_survivable(self) -> None:
        sink = io.StringIO()
        su.LogTail(self.root / "not-there.log", out=sink).pump()
        self.assertEqual(sink.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
