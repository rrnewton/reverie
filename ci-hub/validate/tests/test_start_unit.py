from __future__ import annotations

import contextlib
import io
import json
import subprocess
import sys
import tempfile
import pathlib
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import start_unit  # noqa: E402


SHA = "a" * 40


def completed(command: list[str], rc: int = 0, stdout: str = "", stderr: str = ""):
    return subprocess.CompletedProcess(command, rc, stdout, stderr)


class FakeRun:
    def __init__(self, checkout: Path) -> None:
        self.checkout = checkout
        self.commands: list[list[str]] = []
        self.dirty = ""
        self.admission_rc = 0
        self.herdr_status_rc = 0
        # Fresh-checkout controls: `fresh` is what `mktemp -d` hands back,
        # `fresh_complete` decides whether the tree carries what validate needs,
        # and `ledger_rows` is what the canonical reader returns.
        self.fresh: Path | None = None
        self.fresh_complete = True
        self.ledger_rows: list[str] | None = None
        self.removed: list[str] = []
        # What `git ls-files` reports as TRACKED inside the fresh checkout.
        # Empty is the real-world case: hermit tracks zero .jsonl files.
        self.fresh_tracked_jsonl = ""
        # Relative path of a receipt the RUN writes inside its own temp
        # checkout. Planted when the fresh tree is created, because that is the
        # first moment the directory exists.
        self.plant_receipt: str | None = None

    def __call__(self, command: list[str], **_kwargs: object):
        self.commands.append(command)
        if command[:2] == ["mktemp", "-d"]:
            self.fresh = Path(command[2].replace("XXXXXXXX", "abcd1234"))
            self.fresh.mkdir(parents=True, exist_ok=True)
            (self.fresh / "scripts").mkdir(parents=True, exist_ok=True)
            (self.fresh / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
            if self.fresh_complete:
                dep = self.fresh / "agent-utils/rs/safe-ci-dag-runner"
                dep.mkdir(parents=True, exist_ok=True)
                (dep / "Cargo.toml").write_text("[package]\n")
            if self.plant_receipt is not None:
                receipt = self.fresh / self.plant_receipt
                receipt.parent.mkdir(parents=True, exist_ok=True)
                receipt.write_text(f'{{"commit": "{SHA}", "cwd": "{self.fresh}"}}\n')
            return completed(command, stdout=f"{self.fresh}\n")
        if self.fresh is not None and command[:4] == ["git", "-C", str(self.fresh), "rev-parse"]:
            return completed(command, stdout=f"{SHA}\n")
        if command[:3] == ["git", "-C", str(self.checkout)] and command[3] in {"worktree", "submodule"}:
            if command[3:5] == ["worktree", "remove"]:
                self.removed.append(command[-1])
            return completed(command)
        if self.fresh is not None and command[:4] == ["git", "-C", str(self.fresh), "submodule"]:
            return completed(command)
        if self.fresh is not None and command[:4] == ["git", "-C", str(self.fresh), "ls-files"]:
            # A planted receipt is untracked by construction, so git lists nothing.
            return completed(command, stdout=self.fresh_tracked_jsonl)
        if command[0].endswith("ci-hub") and command[1:3] == ["ledger", "qualified-rows"]:
            rows = self.ledger_rows
            if rows is None:
                rows = [f'{{"commit": "{SHA}", "cwd": "{self.fresh}"}}']
            return completed(command, stdout="\n".join(rows) + ("\n" if rows else ""))
        if command[:4] == ["git", "-C", str(self.checkout), "rev-parse"]:
            if command[-1] == "--show-toplevel":
                return completed(command, stdout=f"{self.checkout}\n")
            return completed(command, stdout=f"{SHA}\n")
        if command[:4] == ["git", "-C", str(self.checkout), "status"]:
            return completed(command, stdout=self.dirty)
        if command[0].endswith("preflight_validate.py"):
            return completed(
                command,
                rc=self.admission_rc,
                stderr="stale base" if self.admission_rc else "",
            )
        if command[0] == "systemd-run" and "herdr" in command:
            herdr = command[command.index("herdr") :]
            if herdr == ["herdr", "status", "--json"]:
                return completed(
                    command,
                    rc=self.herdr_status_rc,
                    stdout=json.dumps({"server": {"running": True}}),
                    stderr="jail denied" if self.herdr_status_rc else "",
                )
            if herdr == ["herdr", "server"]:
                return completed(command, stdout="Running as unit: ci-hub-herdr.service\n")
            if herdr == ["herdr", "workspace", "list"]:
                return completed(
                    command,
                    stdout=json.dumps(
                        {
                            "result": {
                                "workspaces": [
                                    {
                                        "workspace_id": "wV",
                                        "label": "validate-hermit",
                                    }
                                ]
                            }
                        }
                    ),
                )
            if herdr[:3] == ["herdr", "tab", "create"]:
                return completed(
                    command,
                    stdout=json.dumps(
                        {
                            "result": {
                                "root_pane": {"pane_id": "wV:p2"},
                                "tab": {"tab_id": "wV:t2"},
                            }
                        }
                    ),
                )
            if herdr[:3] == ["herdr", "pane", "rename"]:
                return completed(command)
            if herdr[:3] == ["herdr", "pane", "run"]:
                return completed(command)
            raise AssertionError(command)
        if command[0] == "systemd-run" and "validate-lock" in command:
            return completed(command, stdout="Running as unit: validate-test.service\n")
        if command[:3] == ["systemctl", "--user", "show"]:
            return completed(
                command,
                stdout=(
                    "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nResult=success\n"
                ),
            )
        raise AssertionError(command)


class StartUnitTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.original_host_tmp_root = start_unit.HOST_TMP_ROOT
        # Most unit fixtures live below the host's real /tmp. Model a separate
        # host-temp subtree so ordinary positive-path tests remain meaningful;
        # the planted real-/tmp negative restores the production constant.
        start_unit.HOST_TMP_ROOT = self.root / "modeled-host-tmp"
        self.checkout = self.root / "hermit"
        self.checkout.mkdir()
        (self.checkout / "scripts").mkdir()
        (self.checkout / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
        (self.root / "ci-hub/validate").mkdir(parents=True)
        self.fake = FakeRun(self.checkout)
        self.environment = {"HOME": "/home/test", "PATH": "/usr/bin:/bin"}

    def tearDown(self) -> None:
        start_unit.HOST_TMP_ROOT = self.original_host_tmp_root
        self.temporary.cleanup()

    def invoke(self, extra: list[str] | None = None) -> tuple[int, str, str]:
        out = io.StringIO()
        err = io.StringIO()
        argv = [
            "--checkout",
            str(self.checkout),
            "--agent",
            "hermit-test",
            "--target",
            SHA,
            "--unit",
            "validate-test",
            "--log",
            str(self.root / "run.log"),
            *(extra or []),
        ]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                argv,
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )
        return rc, out.getvalue(), err.getvalue()

    def test_positive_launch_routes_systemd_service_through_validate_lock(self) -> None:
        rc, output, error = self.invoke(["--pr", "123", "--", "full", "--ignore-cache"])

        self.assertEqual(0, rc, error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        self.assertIn(str(self.root / "ci-hub/ci-hub"), systemd)
        lock = systemd.index("validate-lock")
        self.assertEqual(["validate-lock", "run"], systemd[lock : lock + 2])
        self.assertEqual(
            [
                "/usr/bin/env",
                "PR_NUMBER=123",
                "with-proxy",
                "./scripts/validate.rs",
                "full",
                "--ignore-cache",
            ],
            systemd[-6:],
        )
        pane_run = next(command for command in self.fake.commands if "pane_watch.py" in " ".join(command))
        self.assertIn("herdr", pane_run)
        self.assertNotIn("scripts/validate.rs", pane_run)
        self.assertFalse(any(command[0] == "herdr" for command in self.fake.commands))
        self.assertIn("HANDLE", output)
        self.assertIn("PANE workspace=wV tab=wV:t2 pane=wV:p2", output)
        self.assertIn("FINISHED", output)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("completed", record["state"])
        self.assertEqual("observer-only", record["pane_role"])

    def test_dry_run_is_non_mutating_but_exposes_exact_command(self) -> None:
        rc, output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))
        self.assertIn("WOULD-START", output)
        self.assertIn("PANE-PLAN workspace=validate-hermit role=observer-only", output)
        self.assertIn("ci-hub validate-lock run", output)
        self.assertFalse((self.root / "run.log").exists())

    def test_dirty_checkout_is_refused_before_admission(self) -> None:
        self.fake.dirty = " M scripts/validate.rs\n"

        rc, _output, error = self.invoke()

        self.assertEqual(2, rc)
        self.assertIn("checkout is dirty", error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_real_host_tmp_source_is_refused_without_admission_or_ledger_write(self) -> None:
        """Plant the production hazard: this fixture itself is below real /tmp."""
        start_unit.HOST_TMP_ROOT = Path("/tmp").resolve()
        ledger = self.root / "ignored/validate-run-ledger.jsonl"
        record = self.root / "ignored/validate/runs/validate-test.json"

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("source checkout resolves beneath host /tmp", error)
        self.assertIn("canonical non-/tmp dev-hermit parent", error)
        self.assertFalse(any(command[:2] == ["mktemp", "-d"] for command in self.fake.commands))
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))
        self.assertFalse(ledger.exists(), "refusal must not mutate the validation ledger")
        self.assertFalse(record.exists(), "refusal must not create a service record")
        self.assertFalse((self.root / "run.log").exists())

    def test_canonical_non_tmp_parent_is_accepted_by_placement_guard(self) -> None:
        canonical = self.root / "canonical-workspace" / "dev-hermit"

        self.assertEqual(
            canonical.resolve(),
            start_unit.require_guest_visible_root(canonical, role="source checkout"),
        )

    def test_tmpfoo_sibling_is_not_mistaken_for_host_tmp(self) -> None:
        start_unit.HOST_TMP_ROOT = Path("/tmp").resolve()
        tmpfoo = start_unit.HOST_TMP_ROOT.with_name("tmpfoo") / "dev-hermit"

        self.assertEqual(
            tmpfoo.resolve(),
            start_unit.require_guest_visible_root(tmpfoo, role="source checkout"),
        )

    def test_symlink_to_host_tmp_is_refused_after_canonicalization(self) -> None:
        hidden = start_unit.HOST_TMP_ROOT / "hidden-hermit"
        hidden.mkdir(parents=True)
        (hidden / "scripts").mkdir()
        (hidden / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
        checkout_link = self.root / "apparently-safe-checkout"
        checkout_link.symlink_to(hidden, target_is_directory=True)
        self.checkout = checkout_link
        self.fake.checkout = hidden.resolve()

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn(str(hidden.resolve()), error)
        self.assertFalse(self.fake.commands, "canonicalization refusal must precede git/admission")
        self.assertFalse((self.root / "ignored/validate-run-ledger.jsonl").exists())

    def test_fresh_parent_symlinked_into_host_tmp_is_refused_before_mktemp(self) -> None:
        hidden_parent = start_unit.HOST_TMP_ROOT / "fresh-parent"
        hidden_parent.mkdir(parents=True)
        (self.root / "ignored").symlink_to(hidden_parent, target_is_directory=True)

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("fresh-checkout parent resolves beneath host /tmp", error)
        self.assertFalse(any(command[:2] == ["mktemp", "-d"] for command in self.fake.commands))
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))
        self.assertFalse((hidden_parent / "validate-run-ledger.jsonl").exists())

    def test_stale_head_is_refused_before_systemd_admission(self) -> None:
        self.fake.admission_rc = 2

        rc, _output, error = self.invoke()

        self.assertEqual(2, rc)
        self.assertIn("validation admission refused", error)
        self.assertIn("stale base", error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_visibility_failure_refuses_before_validation_service(self) -> None:
        self.fake.herdr_status_rc = 1

        rc, _output, error = self.invoke()

        self.assertEqual(2, rc)
        self.assertIn("Herdr server did not become ready", error)
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            )
        )

    def test_attach_waits_on_existing_handle_without_relaunching(self) -> None:
        record = self.root / "ignored/validate/runs/validate-test.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "state": "running",
                "unit": "validate-test.service",
                "target": SHA,
                "checkout": str(self.checkout),
                "log": str(self.root / "run.log"),
                "workspace_id": "wV",
                "tab_id": "wV:t2",
                "pane_id": "wV:p2",
            },
        )
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(0, rc, err.getvalue())
        self.assertIn("ATTACHED", out.getvalue())
        self.assertIn("FINISHED", out.getvalue())
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    # ---- fresh temp-dir checkout is the DEFAULT (owner directive #3) --------

    def _working_directory(self) -> str:
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        return systemd[systemd.index("--working-directory") + 1]

    def test_default_validates_a_fresh_checkout_not_the_slot_tree(self) -> None:
        """The DEFAULT must validate the commit, not the tree that claims to be at it.

        `validate_checkout` already refuses a dirty tree, so this is not about
        uncommitted files. It is about the 5.1 GB of IGNORED state (build output,
        caches, materialized submodules) that `git status --porcelain=v1` cannot
        see and that has twice been measured deciding a verdict.
        """
        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIsNotNone(self.fake.fresh)
        self.assertEqual(str(self.fake.fresh), self._working_directory())
        self.assertNotEqual(str(self.checkout), self._working_directory())

    def test_in_place_opt_out_still_validates_the_slot_tree(self) -> None:
        rc, _output, error = self.invoke(["--in-place", "--", "full"])

        self.assertEqual(0, rc, error)
        self.assertEqual(str(self.checkout), self._working_directory())
        self.assertFalse(
            any(command[:2] == ["mktemp", "-d"] for command in self.fake.commands),
            "opt-out must not build a temp checkout at all",
        )

    def test_incomplete_fresh_checkout_refuses_before_admission(self) -> None:
        """A fresh worktree starts with EMPTY submodules, agent-utils among them.

        Launching anyway reproduces the measured 0.045s exit that reads like a
        fast pass. The tree must be PROVEN usable, and an unusable one must abort
        the launch rather than becoming a quick green.
        """
        self.fake.fresh_complete = False

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("safe-ci-dag-runner", error)
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            ),
            "nothing may be admitted from an unusable tree",
        )

    # ---- REQUIREMENT (a): the row must be canonically readable BEFORE delete --

    def test_receipt_is_reread_from_the_canonical_ledger_before_cleanup(self) -> None:
        rc, output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIn("RECEIPT-CANONICAL", output)
        # The read must happen BEFORE the removal, or a missing row would be
        # indistinguishable from a deleted one.
        kinds = [
            "read" if command[1:3] == ["ledger", "qualified-rows"] else "remove"
            for command in self.fake.commands
            if command[1:3] == ["ledger", "qualified-rows"]
            or command[3:5] == ["worktree", "remove"]
        ]
        self.assertEqual(["read", "remove"], kinds)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_uncanonical_receipt_refuses_and_RETAINS_the_temp_checkout(self) -> None:
        """The negative that matters: an invisible green must be impossible.

        A temp-dir validate whose receipt lands only in the temp checkout's own
        ledger, followed by deletion, manufactures a green nobody can dereference
        — strictly worse than validating in place. The audit measured this exact
        shape: 111 validate.rs fallback rows in two per-checkout ledgers that
        default consumers discover ZERO of.

        THE FIXTURE NOW PLANTS THE HAZARD IT DESCRIBES. It previously only
        emptied the canonical ledger, which models "the canonical lookup found
        nothing" — true of this case AND of an ordinary early failure. Those
        need opposite dispositions, so the fixture has to distinguish them.
        """
        self.fake.ledger_rows = []
        self.plant_orphan_receipt()

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("RECEIPT-NOT-CANONICAL", error)
        self.assertIn("ORPHANED-RECEIPT", error)
        self.assertEqual([], self.fake.removed, "evidence must not be destroyed")

    def plant_orphan_receipt(self, rel: str = ".hermit-validate-ledger.jsonl") -> None:
        """Have the run write its receipt inside its own temp checkout."""
        self.fake.plant_receipt = rel

    def test_ordinary_early_failure_leaves_NO_retained_tree(self) -> None:
        """The reclaim half. The Rust driver is fail-fast: its first gate can abort
        in seconds, so this is the COMMON outcome, and it used to strand a 26 MB
        worktree every time. Nothing was produced, so nothing is preserved."""
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("NO-RECEIPT-PRODUCED", error)
        self.assertNotIn("ORPHANED-RECEIPT", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_a_receipt_under_an_UNANTICIPATED_name_is_still_retained(self) -> None:
        """Why the test is SHAPE, not a whitelist of ledger filenames.

        A whitelist fails in the dangerous direction: a receipt written under a
        name nobody enumerated reads as "no evidence" and gets deleted. Any
        untracked *.jsonl in the tree counts.
        """
        self.fake.ledger_rows = []
        self.plant_orphan_receipt("ci/some-unanticipated-receipt.jsonl")

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("ORPHANED-RECEIPT", error)
        self.assertEqual([], self.fake.removed)

    def test_a_TRACKED_jsonl_is_not_mistaken_for_a_produced_receipt(self) -> None:
        """Attribution is by tracked-ness. A .jsonl that git tracks at this
        commit came from the checkout, not from the run, and must not pin a
        26 MB tree forever."""
        self.fake.ledger_rows = []
        self.plant_orphan_receipt("fixtures/committed.jsonl")
        self.fake.fresh_tracked_jsonl = "fixtures/committed.jsonl\n"

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("NO-RECEIPT-PRODUCED", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_a_scan_that_cannot_look_retains_rather_than_reclaims(self) -> None:
        """Fail-closed. A NO-RESULT is not a negative: if the tree cannot be
        inspected we do not know whether evidence is in it, so it stays."""
        self.fake.ledger_rows = []
        original = start_unit.orphaned_receipt_locations

        def explode(*_a: object, **_k: object) -> list[str]:
            raise OSError("permission denied")

        start_unit.orphaned_receipt_locations = explode
        self.addCleanup(setattr, start_unit, "orphaned_receipt_locations", original)

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("ORPHANED-RECEIPT", error)
        self.assertIn("scan failed", error)
        self.assertEqual([], self.fake.removed)

    def test_a_row_for_the_same_sha_from_a_DIFFERENT_run_does_not_satisfy_it(self) -> None:
        """Identity binding, not a correlated proxy.

        Matching on the commit alone would accept some other run of the same SHA
        — including an in-place run, or a stale row from hours earlier. The row
        must carry this exact temp checkout's cwd.
        """
        self.fake.ledger_rows = [f'{{"commit": "{SHA}", "cwd": "/some/other/checkout"}}']

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("RECEIPT-NOT-CANONICAL", error)
        # The property under test is IDENTITY BINDING -- a foreign row must not
        # satisfy the check. Retention is a separate question: this run left no
        # receipt in its own tree, so there is nothing to preserve and the tree
        # is reclaimed. Asserting retention here conflated the two.
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)


class LibunwindEnvTest(unittest.TestCase):
    """The unit must carry libunwind's pkg-config path when it exists in-repo.

    Without it `unwind-sys`'s build.rs panics and every workspace-building DAG
    lane fails with an environment fault that reads exactly like a product red.
    """

    def _build(self, root):
        return start_unit.build_systemd_command(
            root=root,
            checkout=root,
            target="0" * 40,
            agent="a",
            unit="validate-a",
            log=pathlib.Path("/tmp/x.log"),
            pr=None,
            validate_args=["full"],
            wait=1,
            hold=1,
            child_deadline=1,
            environment={"HOME": "/h", "PATH": "/usr/bin"},
        )

    def test_sets_pkg_config_path_when_the_pc_file_is_present(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            pc = root / "ignored/lu-parity/usr/lib64/pkgconfig"
            pc.mkdir(parents=True)
            (pc / "libunwind-ptrace.pc").write_text("")
            # Pin RUNTIME_CANDIDATES like the two probe tests below do. The real
            # tuple is `~`-relative, so leaving it in place makes the runtime-dir
            # assertion depend on whether this HOST has a libunwind tree: with one
            # the probe picks it and the assertion holds, without one the probe
            # correctly falls back to link_dir and the assertion fails. The
            # fixture must supply the alternative it is asserting the existence
            # of, rather than borrowing it from $HOME.
            runtime = root / "runtime"
            runtime.mkdir()
            (runtime / "libunwind-ptrace.so.0").write_text("")
            original = start_unit.RUNTIME_CANDIDATES
            start_unit.RUNTIME_CANDIDATES = (str(runtime),)
            try:
                cmd = self._build(root)
            finally:
                start_unit.RUNTIME_CANDIDATES = original
            self.assertIn(f"PKG_CONFIG_PATH={pc}", cmd)
            # LINK dir is lu-parity, which carries the .pc and the static
            # libunwind-ptrace.a.
            self.assertIn(f"LIBRARY_PATH={pc.parent}", cmd)
            # RUNTIME dir must NOT be lu-parity: it has no libunwind-ptrace.so*,
            # so the loader would fail with
            # `libunwind-ptrace.so.0: cannot open shared object file`.
            ld = [c for c in cmd if c.startswith("LD_LIBRARY_PATH=")]
            self.assertEqual(len(ld), 1)
            self.assertNotIn(str(pc.parent), ld[0])
            self.assertIn(str(runtime), ld[0])

    def test_absent_pc_file_leaves_the_unit_untouched(self):
        with tempfile.TemporaryDirectory() as tmp:
            cmd = self._build(pathlib.Path(tmp))
            self.assertFalse([c for c in cmd if "PKG_CONFIG_PATH=" in c])
            self.assertFalse([c for c in cmd if "LD_LIBRARY_PATH=" in c])
            self.assertFalse([c for c in cmd if c.startswith("LIBRARY_PATH=")])

    def test_runtime_dir_prefers_a_candidate_carrying_the_shared_ptrace_lib(self):
        """lu-parity ships libunwind-ptrace.a but no .so; the loader needs the .so."""
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            link_dir = root / "link"
            link_dir.mkdir()
            (link_dir / "libunwind-ptrace.a").write_text("")
            good = root / "runtime"
            good.mkdir()
            (good / "libunwind-ptrace.so.0").write_text("")
            original = start_unit.RUNTIME_CANDIDATES
            start_unit.RUNTIME_CANDIDATES = (str(good),)
            try:
                self.assertEqual(start_unit._libunwind_runtime_dir(root, link_dir), good)
            finally:
                start_unit.RUNTIME_CANDIDATES = original

    def test_runtime_dir_falls_back_to_the_link_dir_when_no_candidate_exists(self):
        """Unknown host: behave exactly as before rather than inventing a path."""
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            link_dir = root / "link"
            link_dir.mkdir()
            original = start_unit.RUNTIME_CANDIDATES
            start_unit.RUNTIME_CANDIDATES = (str(root / "absent"),)
            try:
                self.assertEqual(start_unit._libunwind_runtime_dir(root, link_dir), link_dir)
            finally:
                start_unit.RUNTIME_CANDIDATES = original


if __name__ == "__main__":
    unittest.main()
