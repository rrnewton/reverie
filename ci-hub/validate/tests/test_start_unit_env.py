#!/usr/bin/env python3
"""Network-free tests for the environment inherited by validate units."""

from __future__ import annotations

import contextlib
import io
import sys
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
        self.assertFalse(Path(env["E2E_RESULT_ROOT"]).is_relative_to(self.root / "hermit"))
        self.assertFalse(
            Path(env["SAFE_CI_DAG_RUNNER_LOG_DIR"]).is_relative_to(self.root / "hermit")
        )

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
        self.assertIn(f"E2E_RESULTS {artifact_root / 'e2e'}", output.getvalue())
        self.assertIn(
            f"RUNNER_LOGS {artifact_root / 'safe-ci-dag-runner'}",
            output.getvalue(),
        )

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


if __name__ == "__main__":
    unittest.main()
