#!/usr/bin/env python3
"""Network-free tests for the environment inherited by validate units."""

from __future__ import annotations

import contextlib
import inspect
import fcntl
import io
import threading
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import run_registry  # noqa: E402
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
        child_deadline=3600,
        environment=environment,
    )


class StartUnitEnvTest(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile

        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.base_env = {"HOME": "/home/test", "PATH": "/usr/bin"}

    def test_frozen_private_tool_compiler_does_not_replace_measured_environment(self) -> None:
        measured = "nightly-2026-06-01-x86_64-unknown-linux-gnu"
        driver = "nightly-2026-07-12-x86_64-unknown-linux-gnu"
        command = su.build_systemd_command(
            root=self.root / "launcher-tool",
            checkout=self.root / "historical-hermit",
            target=TARGET, agent="test-agent", unit="validate-test.service",
            record=self.root / "record.json", log=self.root / "out.log", pr=None,
            validate_args=["quick", "--ignore-cache"], wait=1, hold=1,
            child_deadline=4800, environment=self.base_env,
            frozen_validate=True, frozen_result=self.root / "frozen.jsonl",
            unit_tool_prefix=["private-tool-bootstrap"],
            driver_rust_toolchain=driver, measured_rust_toolchain=measured,
        )
        shell = next(
            i for i, value in enumerate(command)
            if value == "/bin/sh" and "exec with-proxy rust-script" in command[i + 2]
        )
        proxy = self.root / "with-proxy"
        proxy.write_text(
            f"#!{sys.executable}\nimport json,os,sys\n"
            "print(json.dumps([sys.argv[1:], os.environ['RUSTUP_TOOLCHAIN']]))\n"
        )
        proxy.chmod(0o755)
        actual_tool = self.root / "unit-private-tool"
        result = subprocess.run(
            command[shell:], check=True, capture_output=True, text=True,
            env={**os.environ, **setenvs(command), "PATH": str(self.root),
                 "DEV_HERMIT_TOOL_ROOT": str(actual_tool)}, timeout=5,
        )
        argv, inherited = json.loads(result.stdout)
        self.assertEqual(
            ["rust-script", "--force", "--toolchain", driver,
             str(actual_tool / "hermit/scripts/validate.rs"), "quick", "--ignore-cache"],
            argv,
        )
        self.assertEqual(measured, inherited)
        self.assertEqual("0", setenvs(command)["RUSTUP_AUTO_INSTALL"])

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
        self.assertEqual(env["HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS"], "3239")

    def test_default_uses_the_controller_admission_policy(self) -> None:
        command = build(self.root, dict(self.base_env))
        lock = command.index("validate-lock")
        child = command.index("--", lock)
        self.assertNotIn("--max", command[lock:child])

    def test_explicit_max_reaches_validate_lock(self) -> None:
        command = su.build_systemd_command(
            root=self.root,
            checkout=self.root / "hermit",
            target=TARGET,
            agent="test-agent",
            unit="validate-test.service",
            record=self.root / "ignored/validate/runs/validate-test.json",
            log=self.root / "out.log",
            pr=None,
            validate_args=["full"],
            wait=1,
            hold=1,
            child_deadline=3600,
            environment=self.base_env,
            max_validates=1,
        )
        lock = command.index("validate-lock")
        child = command.index("--", lock)
        self.assertEqual(command[lock:child].count("--max"), 1)
        maximum = command.index("--max", lock, child)
        self.assertEqual(command[maximum + 1], "1")

    def test_no_wait_reaches_validate_lock_without_zeroing_the_wait_bound(self) -> None:
        command = su.build_systemd_command(
            root=self.root,
            checkout=self.root / "hermit",
            target=TARGET,
            agent="test-agent",
            unit="validate-test.service",
            record=self.root / "ignored/validate/runs/validate-test.json",
            log=self.root / "out.log",
            pr=None,
            validate_args=["full"],
            wait=7200,
            hold=1,
            child_deadline=3600,
            environment=self.base_env,
            no_wait=True,
            skip_if_recorded=True,
        )
        lock = command.index("validate-lock")
        child = command.index("--", lock)
        self.assertIn("--no-wait", command[lock:child])
        self.assertIn("--skip-if-recorded", command[lock:child])
        wait = command.index("--wait", lock, child)
        self.assertEqual(command[wait + 1], "7200")

    def test_large_child_deadline_does_not_widen_hermit_run(self) -> None:
        command = su.build_systemd_command(
            root=self.root,
            checkout=self.root / "hermit",
            target=TARGET,
            agent="test-agent",
            unit="validate-test.service",
            record=self.root / "ignored/validate/runs/validate-test.json",
            log=self.root / "out.log",
            pr=None,
            validate_args=["full"],
            wait=1,
            hold=1,
            child_deadline=7200,
            environment={
                **self.base_env,
                "HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS": "99999",
            },
        )

        env = setenvs(command)
        self.assertEqual(env["HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS"], "4200")
        self.assertEqual(command[command.index("--child-deadline") + 1], "4800")

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

    def test_parent_and_tool_roots_are_propagated(self) -> None:
        """State and executable roots both reach the validation driver.

        The ledger producer deliberately does not consult this variable: it uses
        one fixed adapter path even when DEV_HERMIT_PARENT is absent. Other
        state belongs under DEV_HERMIT_PARENT, while receipt finalization and
        ci-hub helpers execute from DEV_HERMIT_TOOL_ROOT. They are equal for an
        ordinary checkout and deliberately differ for a linked tooling checkout.

        Asserting the exact path, not just presence: pointing at the wrong root
        would make those other authorities unreachable.
        """
        env = setenvs(build(self.root, dict(self.base_env)))
        self.assertIn(
            "DEV_HERMIT_PARENT", env, "the unit cannot reach parent-owned authorities"
        )
        self.assertEqual(env["DEV_HERMIT_PARENT"], str(self.root))
        self.assertEqual(env["DEV_HERMIT_TOOL_ROOT"], str(self.root))

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
            child_deadline=3600,
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
            child_deadline=3600,
            environment=dict(self.base_env),
            make_validate=True,
            allow_owner_validate_symlink=True,
        )
        joined = " ".join(command)
        self.assertIn(
            "VALIDATE_SKIP_INNER_DIRTY_WORKING_TREE_AND_REBASE_FRESHNESS_CHECKS=1",
            joined,
        )
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


class ThreeOutcomesTest(unittest.TestCase):
    """REFUSED, FAILED and COULD-NOT-DETERMINE must never share an exit code.

    ⚠️ THIS EXISTS BECAUSE THEY DID. `CANONICAL-VERDICT-UNAVAILABLE` -- the run
    happened and its verdict could not be read -- returned 2, the same code as
    `--pr must be positive`. Three more post-run conditions did the same:
    WAIT-INTERRUPTED, ORPHANED-RECEIPT and NO-RECEIPT-PRODUCED all reported
    REFUSED for runs that had actually started.

    Retry and investigate are opposite reactions, so a caller that cannot tell
    them apart takes the wrong one half the time.
    """

    def test_the_three_outcomes_have_distinct_exit_codes(self) -> None:
        codes = {su.EXIT_REFUSED, su.EXIT_FAILED, su.EXIT_COULD_NOT_DETERMINE}
        self.assertEqual(3, len(codes), f"outcomes collapsed onto one code: {codes}")
        self.assertNotIn(su.EXIT_PASSED, codes)

    def test_a_refusal_cannot_be_written_without_a_remedy(self) -> None:
        # Positional and required: a future refusal cannot omit it by accident.
        params = inspect.signature(su.refuse).parameters
        self.assertIn("remedy", params)
        self.assertIs(inspect.Parameter.empty, params["remedy"].default)

    def test_refused_says_it_never_ran_and_names_a_remedy(self) -> None:
        sink = io.StringIO()
        rc = su.refuse("--pr must be positive", "pass a positive number", out=sink)
        self.assertEqual(su.EXIT_REFUSED, rc)
        self.assertIn("NEVER RAN", sink.getvalue())
        self.assertIn("remedy: pass a positive number", sink.getvalue())

    def test_could_not_determine_is_not_reported_as_a_failure(self) -> None:
        sink = io.StringIO()
        rc = su.could_not_determine("verdict unreadable", "recover the receipt", out=sink)
        self.assertEqual(su.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertNotEqual(su.EXIT_FAILED, rc)
        self.assertNotEqual(su.EXIT_REFUSED, rc)
        self.assertIn("NOT a failure", sink.getvalue())
        self.assertIn("remedy: recover the receipt", sink.getvalue())

    def test_each_outcome_describes_itself_differently(self) -> None:
        seen = {
            su.describe_outcome(code)
            for code in (
                su.EXIT_PASSED,
                su.EXIT_REFUSED,
                su.EXIT_FAILED,
                su.EXIT_COULD_NOT_DETERMINE,
            )
        }
        self.assertEqual(4, len(seen), f"two outcomes describe themselves alike: {seen}")
        self.assertIn("NEVER RAN", su.describe_outcome(su.EXIT_REFUSED))
        self.assertIn("RAN", su.describe_outcome(su.EXIT_FAILED))

    def test_an_unmapped_code_is_named_as_a_defect_not_guessed(self) -> None:
        # The negative control. Without it, describe_outcome could return a
        # plausible sentence for anything and the tests above would still pass.
        self.assertIn("UNCLASSIFIED", su.describe_outcome(99))

    def test_the_canonical_verdict_map_keeps_failed_separate_from_passed(self) -> None:
        self.assertEqual(su.EXIT_PASSED, su.canonical_verdict_exit_code("VALIDATED"))
        self.assertEqual(su.EXIT_FAILED, su.canonical_verdict_exit_code("FAILED"))


def _finishes(call, seconds: float = 5.0):
    """Run `call` in a thread; return (finished, result_or_error).

    ⚠️ THE BOUND IS ON THE TEST, NOT THE PRODUCT. Without it the two contended
    assertions below HANG under the exact mutation they exist to catch -- revert
    the non-blocking acquire and the call never returns, so the suite stops
    instead of going red. A test that hangs on regression is worse than one that
    fails: CI reports nothing and a human has to guess which test. Measured while
    writing this -- the mutant run was killed at 120s having produced no verdict.
    The product still contains no timeout anywhere.
    """
    box: dict = {}

    def target() -> None:
        try:
            box["value"] = call()
        except BaseException as exc:  # noqa: BLE001 -- recorded and asserted on
            box["error"] = exc
        box["finished"] = True

    thread = threading.Thread(target=target, daemon=True)
    thread.start()
    thread.join(seconds)
    return box.get("finished", False), box


class ScorecardInvocationLock(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.checkout = Path(self.temporary.name) / "checkout"
        self.checkout.mkdir()
        self.lock_path = (
            self.checkout / su.SCORECARD_INVOCATION_LOCK_RELATIVE
        )

    def _open_and_hold(self):
        self.lock_path.parent.mkdir(parents=True, exist_ok=True)
        lock = self.lock_path.open("a+")
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        return lock

    def _bounded_child(self, operation: str) -> dict:
        # These children only inspect local lock descriptors. The parent owns
        # the deadline, kill and reap even when the old helper admits a FIFO or
        # fails to make progress. No validation or guest runs here.
        program = (
            "import contextlib, fcntl, json, os, sys\n"
            "from pathlib import Path\n"
            "from unittest import mock\n"
            "sys.path.insert(0, sys.argv[1])\n"
            "import start_unit as su\n"
            "checkout = Path(sys.argv[2])\n"
            "lock_path = checkout / su.SCORECARD_INVOCATION_LOCK_RELATIVE\n"
            "try:\n"
            "    exec(sys.argv[3])\n"
            "except su.ScorecardInvocationLockRefused as error:\n"
            "    print(json.dumps({'refused': str(error)}))\n"
            "else:\n"
            "    print(json.dumps({'accepted': True}))\n"
        )
        child = subprocess.Popen(
            [sys.executable, "-c", program, str(Path(su.__file__).parent),
             str(self.checkout), operation],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        timed_out = False
        try:
            stdout, stderr = child.communicate(timeout=3)
        except subprocess.TimeoutExpired:
            timed_out = True
            child.kill()
            stdout, stderr = child.communicate(timeout=3)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=3)
        print(json.dumps({
            "control": self.id(), "pid": child.pid, "status": child.returncode,
            "timed_out": timed_out, "reaped": child.poll() is not None,
            "stdout": stdout, "stderr": stderr,
        }, sort_keys=True))
        self.assertFalse(timed_out, "lock control exceeded its external child deadline")
        self.assertEqual(child.returncode, 0, stderr)
        return json.loads(stdout)

    def test_helper_holds_the_exact_validate_invocation_lock(self) -> None:
        with su.scorecard_invocation_lock(self.checkout, timeout_seconds=0.1) as held:
            self.assertEqual(self.lock_path, held.path)
            held.verify()
            contender = self.lock_path.open("a+")
            try:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(contender.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                contender.close()
        successor = self._open_and_hold()
        successor.close()

    def test_contention_is_bounded_and_refuses_before_the_body(self) -> None:
        held = self._open_and_hold()
        body_ran = False
        started = time.monotonic()
        try:
            with self.assertRaisesRegex(
                su.ScorecardInvocationLockRefused,
                "remained held.*two-file write-back did not run",
            ):
                with su.scorecard_invocation_lock(
                    self.checkout, timeout_seconds=0.02, poll_seconds=0.005
                ):
                    body_ran = True
        finally:
            held.close()
        self.assertFalse(body_ran)
        self.assertLess(time.monotonic() - started, 0.5)

    def test_body_exception_still_releases_the_lock(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "body failed"):
            with su.scorecard_invocation_lock(self.checkout, timeout_seconds=0.1):
                raise RuntimeError("body failed")
        successor = self._open_and_hold()
        successor.close()

    def test_unresolvable_lock_path_refuses_instead_of_running(self) -> None:
        self.lock_path.parent.mkdir(parents=True)
        target = Path(self.temporary.name) / "foreign-lock"
        target.write_text("")
        self.lock_path.symlink_to(target)
        with self.assertRaisesRegex(
            su.ScorecardInvocationLockRefused, "cannot open.*validation invocation lock"
        ):
            with su.scorecard_invocation_lock(self.checkout, timeout_seconds=0):
                self.fail("a symlinked lock path must never admit the writer")

    def test_invalid_bounds_refuse_before_creating_a_lock(self) -> None:
        for timeout in (-1, float("inf"), True):
            with self.subTest(timeout=timeout), self.assertRaises(ValueError):
                with su.scorecard_invocation_lock(
                    self.checkout, timeout_seconds=timeout
                ):
                    pass
        self.assertFalse(self.lock_path.exists())

    def test_replaced_lock_while_waiting_refuses_the_obsolete_inode(self) -> None:
        result = self._bounded_child('''
lock_path.parent.mkdir(parents=True)
with contextlib.ExitStack() as cleanup:
    original = cleanup.enter_context(lock_path.open("a+"))
    fcntl.flock(original.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    replacements = []
    def replace_while_waiting(_seconds):
        assert not replacements
        os.rename(lock_path, lock_path.with_name("old.lock"))
        replacement = cleanup.enter_context(lock_path.open("a+"))
        fcntl.flock(replacement.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        replacements.append(replacement)
        fcntl.flock(original.fileno(), fcntl.LOCK_UN)
    with mock.patch.object(su.time, "sleep", side_effect=replace_while_waiting):
        try:
            with su.scorecard_invocation_lock(checkout, timeout_seconds=0.2):
                raise AssertionError("writer body entered after lock replacement")
        finally:
            assert len(replacements) == 1
            with lock_path.open("a+") as contender:
                try:
                    fcntl.flock(contender.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    pass
                else:
                    raise AssertionError("replacement lock was not held")
''')
        self.assertIn("pathname changed", result.get("refused", ""))

    def test_replaced_directory_while_waiting_refuses_the_obsolete_inode(self) -> None:
        result = self._bounded_child('''
lock_path.parent.mkdir(parents=True)
with contextlib.ExitStack() as cleanup:
    original = cleanup.enter_context(lock_path.open("a+"))
    fcntl.flock(original.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    replacements = []
    def replace_while_waiting(_seconds):
        assert not replacements
        os.rename(lock_path.parent, lock_path.parent.with_name("old-validation"))
        lock_path.parent.mkdir()
        replacement = cleanup.enter_context(lock_path.open("a+"))
        fcntl.flock(replacement.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        replacements.append(replacement)
        fcntl.flock(original.fileno(), fcntl.LOCK_UN)
    with mock.patch.object(su.time, "sleep", side_effect=replace_while_waiting):
        try:
            with su.scorecard_invocation_lock(checkout, timeout_seconds=0.2):
                raise AssertionError("writer body entered after directory replacement")
        finally:
            assert len(replacements) == 1
''')
        self.assertIn("directory changed", result.get("refused", ""))

    def test_nonregular_lock_refuses_before_the_body(self) -> None:
        self.lock_path.parent.mkdir(parents=True)
        os.mkfifo(self.lock_path)
        result = self._bounded_child('''
with su.scorecard_invocation_lock(checkout, timeout_seconds=0):
    raise AssertionError("writer body entered with a nonregular lock")
''')
        self.assertIn("not a regular file", result.get("refused", ""))
        self.assertTrue(self.lock_path.exists())

    def test_publication_check_refuses_a_replaced_lock(self) -> None:
        result = self._bounded_child('''
with su.scorecard_invocation_lock(checkout, timeout_seconds=0.1) as held:
    os.rename(lock_path, lock_path.with_name("old.lock"))
    lock_path.write_text("replacement")
    held.verify()
    raise AssertionError("publication must not proceed after lock replacement")
''')
        self.assertIn("pathname changed", result.get("refused", ""))
        self.assertEqual(self.lock_path.read_text(), "replacement")

    def test_context_exit_refuses_a_replaced_lock(self) -> None:
        result = self._bounded_child('''
with su.scorecard_invocation_lock(checkout, timeout_seconds=0.1):
    os.rename(lock_path, lock_path.with_name("old.lock"))
    lock_path.write_text("replacement")
''')
        self.assertIn("pathname changed", result.get("refused", ""))
        self.assertEqual(self.lock_path.read_text(), "replacement")


class RecordLockNeverWithholdsAVerdict(unittest.TestCase):
    """A bookkeeping write must not hold the caller's terminal.

    ⚠️ REPRODUCED BEFORE FIXING. attach() updates the run record twice, both AFTER
    the outcome is settled, and both took flock(LOCK_EX) with no bound. With
    another process holding that lock the wrapper printed its report and then
    blocked indefinitely -- idle in flock rather than spinning, which is why it
    read as a job still running. That held the owner's box. Same shape as the rest
    of tonight: a value computed and never delivered.
    """

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.record = self.dir / "r.json"
        self.record.write_text(json.dumps({"schema_version": 1, "a": 1}))

    def _hold(self):
        lock = (self.dir / ".r.json.lock").open("a+")
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        return lock

    def test_a_contended_bookkeeping_write_raises_instead_of_waiting(self) -> None:
        held = self._hold()
        try:
            finished, box = _finishes(
                lambda: run_registry.update_record(self.record, blocking=False, b=2)
            )
            self.assertTrue(finished, "contended non-blocking update DID NOT RETURN")
            self.assertIsInstance(box.get("error"), run_registry.RecordLocked)
        finally:
            held.close()

    def test_bookkeep_record_reports_and_returns_rather_than_raising(self) -> None:
        held = self._hold()
        try:
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                finished, box = _finishes(lambda: su.bookkeep_record(self.record, b=2))
            self.assertTrue(finished, "bookkeep_record DID NOT RETURN while contended")
            self.assertEqual({"b": 2}, box.get("value"))
            self.assertIn("RUN-RECORD NOT UPDATED", err.getvalue())
            self.assertIn("verdict below is unaffected", err.getvalue())
            self.assertIn("Remedy", err.getvalue())
        finally:
            held.close()

    def test_the_blocking_default_is_unchanged(self) -> None:
        # The load-bearing negative: writers that need exclusivity for CORRECTNESS
        # still block. A change making every writer non-blocking would pass every
        # other test here.
        for fn in (run_registry.exclusive_record, run_registry.update_record):
            self.assertIs(
                True, inspect.signature(fn).parameters["blocking"].default, fn.__name__
            )

    def test_an_uncontended_write_still_updates_the_record(self) -> None:
        run_registry.update_record(self.record, blocking=False, b=2)
        self.assertEqual(2, json.loads(self.record.read_text())["b"])

    def test_bookkeep_record_is_silent_when_the_lock_is_free(self) -> None:
        # Negative control: without it the assertion above passes on a build that
        # prints the contention notice unconditionally.
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            su.bookkeep_record(self.record, b=3)
        self.assertNotIn("RUN-RECORD NOT UPDATED", err.getvalue())
        self.assertEqual(3, json.loads(self.record.read_text())["b"])


if __name__ == "__main__":
    unittest.main()
