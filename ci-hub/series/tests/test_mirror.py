"""Direct publication transport tests with real temporary Git repositories.

The default fast suite uses explicit Rust/checker/compiler fixture doubles.
The separate --real-finalized-provider JSON integration invocation uses freshly
source-bound native helpers, the actual canonical reader and combined writer.
Its compiler transport reuses those executables without creating a build target
inside temporary STATE. Neither suite executes a Hermit guest.
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest import mock

MODULE = Path(__file__).resolve().parents[1] / "mirror.py"
SPEC = importlib.util.spec_from_file_location("series_mirror", MODULE)
assert SPEC is not None and SPEC.loader is not None
mirror = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mirror)
series = mirror.series
HELP_FIXTURE = (
    "if sys.argv[1:] == ['--help']:\n"
    " assert 'DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD' not in os.environ\n"
    " assert 'DEV_HERMIT_TEST_LEDGER_ROOT' not in os.environ\n"
    " raise SystemExit(0)\n"
)


def bounded_fixture_command(command: list[str]) -> list[str]:
    """Install the existing checker/query doubles inside the real bounded worker.

    Production re-exec cannot carry an in-process runpy patch across exec. The
    fixture keeps that same bootstrap inside the actual cleanup domain instead;
    mirror still verifies its deadline/cgroup context before acquiring a lock.
    The original subprocess harness has a 30-second ceiling.
    """
    deadline = time.monotonic_ns() + 30_000_000_000
    return [str(mirror.TOOL_ROOT / "scripts/hermit-box-run"), "--passthrough",
            "--inherited-deadline-ns", str(deadline), "--diag-fd", "2", "--",
            *command, "--lifecycle-deadline-ns", str(deadline), "--bounded-worker"]


def row(identifier: str, outcome: str = "passed") -> dict:
    return {
        "schema": "stress-series/v1", "event_id": identifier,
        "event_type": "series.observation", "emitted_at": "2026-09-21T12:00:00Z",
        "team": "hermit", "host": "fixture", "run_id": identifier,
        "producer": "fixture", "series": {
            "cell": "c-programs/hello/verify/ptrace", "tree": "a" * 40,
            "run_index": 0, "outcome": outcome,
        },
    }


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()

def _apply_case(row: dict, case: dict) -> dict:
    """Apply a release-payload-cases.json case: drop the pair and every absent
    key, then set exactly the case's fields."""
    for key in ("release_builder", "e2e_payload", *case.get("absent", ())):
        row.pop(key, None)
    row.update(case["fields"])
    return row



class DirectLedgerFixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="direct-ledger-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        config = self.root / "gitconfig"
        config.write_text(
            '[user]\n name = Fixture\n email = fixture@example.invalid\n'
            '[init]\n defaultBranch = main\n'
            f'[url "{self.remote}"]\n insteadOf = {mirror.REMOTE_URL}\n'
        )
        self.enterContext(mock.patch.dict(os.environ, {
            "GIT_CONFIG_GLOBAL": str(config), "GIT_CONFIG_NOSYSTEM": "1",
            "WITH_PROXY": "/usr/bin/env", "PYTHONDONTWRITEBYTECODE": "1",
        }))
        for key in (series.LEDGER_ROOT_ENV, "DEV_HERMIT_TOOL_AUTHORITY", "CARGO_TARGET_DIR"):
            self.enterContext(mock.patch.dict(os.environ))
            os.environ.pop(key, None)
        self.enterContext(mock.patch.object(
            series, "_run_shared_checker",
            return_value=subprocess.CompletedProcess(["schema-fixture"], 0, "", ""),
        ))
        self.enterContext(mock.patch.object(mirror.publisher, "_series_store", return_value=series))
        subprocess.run(["git", "init", "--bare", "-q", str(self.remote)], check=True)
        seed = self.root / "seed"
        subprocess.run(["git", "init", "-q", str(seed)], check=True)
        (seed / "series/hermit/fixture").mkdir(parents=True)
        (seed / "series/hermit/fixture/2026-09.jsonl").write_text("")
        (seed / "scorecard").mkdir()
        (seed / "scorecard/cells.json").write_text('{"legacy":"preserve"}\n')
        (seed / "scorecard/SCORECARD.md").write_text("legacy scorecard\n")
        git(seed, "add", "series", "scorecard")
        git(seed, "commit", "-qm", "seed existing ledger")
        git(seed, "remote", "add", "origin", str(self.remote))
        git(seed, "push", "-q", "origin", "main")
        self.parent = self.root / "parent"
        subprocess.run(["git", "init", "-q", str(self.parent)], check=True)
        (self.parent / ".gitmodules").write_text(
            '[submodule "hermit_test_ledger"]\n path = hermit_test_ledger\n'
            f' url = {mirror.REMOTE_URL}\n'
        )
        (self.parent / ".gitignore").write_text("ignored/\nhermit_test_ledger/\n")
        (self.parent / "series/hermit/fixture").mkdir(parents=True)
        (self.parent / "series/hermit/fixture/2026-09.jsonl").write_text(json.dumps(row("historical-parent")) + "\n")
        git(self.parent, "add", ".gitmodules", ".gitignore", "series")
        git(self.parent, "commit", "-qm", "parent history before migration")
        self.parent_commit = git(self.parent, "rev-parse", "HEAD")
        self.ledger = self.parent / "hermit_test_ledger"
        subprocess.run(["git", "clone", "-q", mirror.REMOTE_URL, str(self.ledger)], check=True)
        self.source = self.root / "hermit"
        subprocess.run(["git", "init", "-q", str(self.source)], check=True)
        (self.source / "measured-source").write_text("measured checkout\n")
        (self.source / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "nightly-2025-01-01"\n')
        git(self.source, "add", "measured-source", "rust-toolchain.toml")
        git(self.source, "commit", "-qm", "measured source")
        self.target = git(self.source, "rev-parse", "HEAD")
        self.tool = self.root / "tool-hermit"
        subprocess.run(["git", "init", "-q", str(self.tool)], check=True)
        tool = self.tool / "ci/compat-envelope/scorecard.rs"
        tool.parent.mkdir(parents=True)
        (self.tool / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "nightly-2026-07-12"\n')
        compiler = self.root / "bin/rust-script"
        compiler.parent.mkdir()
        compiler.write_text(
            "#!/usr/bin/python3\nimport os, sys\n"
            "assert sys.argv[1:3] == ['--force', '--toolchain']\n"
            "assert sys.argv[3] == 'nightly-2026-07-12-x86_64-unknown-linux-gnu'\n"
            "assert os.environ['RUSTUP_TOOLCHAIN'] == sys.argv[3]\n"
            "assert os.environ['RUSTUP_AUTO_INSTALL'] == '0'\n"
            "os.execv(sys.argv[4], sys.argv[4:])\n"
        )
        compiler.chmod(0o755)
        cargo = compiler.with_name("cargo")
        cargo.write_text(
            "#!/usr/bin/python3\nimport os, sys\n"
            f"assert sys.argv[1:] == {['run', '--quiet', '-p', 'hermit-manifest-plan', '--manifest-path', str(self.tool / 'Cargo.toml'), '--target-dir', str(self.parent / 'target'), '--', '--help']!r}\n"
            "assert os.environ['RUSTUP_TOOLCHAIN'] == 'nightly-2026-07-12-x86_64-unknown-linux-gnu'\n"
            "assert os.environ['RUSTUP_AUTO_INSTALL'] == '0'\n"
            "assert 'DEV_HERMIT_TEST_LEDGER_ROOT' not in os.environ\n"
            "assert 'DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD' not in os.environ\n"
        )
        cargo.chmod(0o755)
        self.enterContext(mock.patch.dict(os.environ, {
            "PATH": str(compiler.parent) + os.pathsep + os.environ["PATH"],
            "RUSTUP_TOOLCHAIN": "nightly-2025-01-01",
            "RUSTUP_AUTO_INSTALL": "1",
        }))
        # A Python fixture child exercises inherited authority and output routing.
        tool.write_text("#!/usr/bin/python3\nimport os, pathlib, sys\n"
                        + HELP_FIXTURE +
                        "fd=int(os.environ['DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD'])\n"
                        "os.fstat(fd)\n"
                        "p=pathlib.Path(os.environ['DEV_HERMIT_TEST_LEDGER_ROOT'])/'scorecard'\n"
                        "assert (p/'cells.json').read_text() == '{\"legacy\":\"preserve\"}\\n'\n"
                        "(p/'SCORECARD.md').write_text('derived from existing history\\n')\n")
        tool.chmod(0o755)
        git(self.tool, "add", "ci", "rust-toolchain.toml")
        git(self.tool, "commit", "-qm", "fixture scorecard tool")

    def append(self, identifier: str, outcome: str = "passed") -> Path:
        path = series.spool_dir(self.parent) / f"{identifier}.jsonl"
        series.append_cell_rows(self.parent, path, [row(identifier, outcome)])
        return path

    def publish(self, **kwargs):
        return mirror.mirror(
            self.parent, self.source, self.target, mirror.REMOTE_URL,
            net_wrapper="/usr/bin/env", tool_checkout=self.tool, **kwargs,
        )

    def remote_rows(self):
        payload = git(self.remote, "show", "main:series/hermit/fixture/2026-09.jsonl")
        return [json.loads(line) for line in payload.splitlines()]

    def test_direct_publication_preserves_parent_and_legacy_history(self):
        self.append("run-one")
        report = self.publish()
        self.assertEqual("published", report["state"], report)
        self.assertEqual(mirror.REMOTE_URL, report["repository"])
        git(self.remote, "merge-base", "--is-ancestor", report["series_commit"], "main")
        self.assertEqual([row("run-one")], self.remote_rows())
        self.assertEqual(self.parent_commit, git(self.parent, "rev-parse", "HEAD"))
        self.assertEqual("", git(self.source, "status", "--porcelain"))
        self.assertEqual(git(self.tool, "rev-parse", "HEAD"), report["scorecard"]["tool_hermit_commit"])
        self.assertNotEqual(self.target, report["scorecard"]["tool_hermit_commit"])
        self.assertFalse((self.source / "target").exists())
        self.assertEqual('{"legacy":"preserve"}', git(self.remote, "show", "main:scorecard/cells.json"))
        self.assertEqual([], list(series.spool_dir(self.parent).glob("*.jsonl")))
        self.assertTrue(list((series.spool_dir(self.parent) / "published").glob("*.jsonl")))
        again = self.publish()
        self.assertEqual([row("run-one")], self.remote_rows())
        self.assertEqual(report["series_commit"], report["source_commit"])
        self.assertEqual(0, again["raw_publication"]["published"])
        linked = [line for line in git(self.ledger, "worktree", "list", "--porcelain").splitlines()
                  if line.startswith("worktree ")]
        self.assertEqual(2, len(linked), "repeated publication reuses one linked writer")

    def test_read_only_tool_uses_mutable_state_target_and_its_own_compiler(self):
        self.append("immutable-tool")
        source = self.tool / "ci/compat-envelope/scorecard.rs"
        source.write_text(source.read_text() +
            "target=pathlib.Path(os.environ['CARGO_TARGET_DIR'])\n"
            f"assert target == pathlib.Path({str(self.parent / 'target')!r})\n"
            "target.mkdir(exist_ok=True)\n"
            "(target/'fixture-build-output').write_text('mutable output\\n')\n")
        git(self.tool, "add", "ci")
        git(self.tool, "commit", "-qm", "read-only tool build transport fixture")
        self.tool.chmod(0o555)
        self.addCleanup(self.tool.chmod, 0o755)
        report = self.publish()
        self.assertEqual(report["state"], "published", report)
        self.assertTrue((self.parent / "target/fixture-build-output").is_file())
        self.assertFalse((self.tool / "target").exists())
        self.assertFalse((self.source / "target").exists())
        self.assertEqual(os.environ["RUSTUP_TOOLCHAIN"], "nightly-2025-01-01")
        self.assertEqual(os.environ["RUSTUP_AUTO_INSTALL"], "1")
        self.assertEqual(git(self.tool, "status", "--porcelain"), "")

    def test_explicit_existing_target_reaches_both_scorecard_preparation_commands(self):
        target = self.root / "existing-target"
        target.mkdir()
        cargo = self.root / "bin/cargo"
        cargo.write_text(cargo.read_text().replace(str(self.parent / "target"), str(target)))
        compiler = self.root / "bin/rust-script"
        compiler.write_text(compiler.read_text().replace(
            "import os, sys\n", "import os, sys\n"
            f"assert os.environ['CARGO_TARGET_DIR'] == {str(target)!r}\n"
            f"open({str(target / 'prepared')!r}, 'w').write('existing target only')\n"))
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target)}):
            tool_commit, compiler_name = mirror.prepare_scorecard(
                self.source, self.target, tool_checkout=self.tool,
                state_root=self.parent, net_wrapper="/usr/bin/env")
        self.assertEqual(git(self.tool, "rev-parse", "HEAD"), tool_commit)
        self.assertEqual("nightly-2026-07-12-x86_64-unknown-linux-gnu", compiler_name)
        self.assertEqual("existing target only", (target / "prepared").read_text())
        self.assertFalse((self.parent / "target").exists())
        self.assertFalse((self.tool / "target").exists())

    def test_invalid_explicit_target_refuses_before_compiler_execution(self):
        ordinary_file = self.root / "not-a-target"
        ordinary_file.write_text("preserve")
        for value in ("", "relative-target", str(self.root / "missing-target"), str(ordinary_file)):
            with self.subTest(value=value), mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": value}), \
                    mock.patch.object(mirror, "_network") as compiler, \
                    self.assertRaisesRegex(mirror.MirrorRefused, "explicit CARGO_TARGET_DIR"):
                mirror.prepare_scorecard(self.source, self.target, tool_checkout=self.tool,
                                         state_root=self.parent, net_wrapper="/usr/bin/env")
            compiler.assert_not_called()
        self.assertEqual("preserve", ordinary_file.read_text())
        self.assertFalse((self.root / "missing-target").exists())

    def test_unavailable_tool_compiler_keeps_published_rows_and_failed_derivation(self):
        self.append("compiler-unavailable")
        with mock.patch.object(
            mirror.rust_toolchain, "selected_toolchain",
            side_effect=mirror.rust_toolchain.ToolchainRefused("declared compiler unavailable"),
        ):
            report = self.publish()
        self.assertEqual(report["state"], "derived-failed")
        self.assertIn("declared compiler unavailable", report["scorecard"]["error"])
        self.assertEqual(self.remote_rows(), [row("compiler-unavailable")])
        self.assertEqual(git(self.remote, "show", "main:scorecard/SCORECARD.md"), "legacy scorecard")

    def test_two_concurrent_appenders_and_publishers_preserve_both_events_once(self):
        barrier = threading.Barrier(2)
        def append_and_publish(name):
            barrier.wait()
            self.append(name)
            return self.publish(publish_only=True)
        with ThreadPoolExecutor(max_workers=2) as pool:
            reports = list(pool.map(append_and_publish, ["run-a", "run-b"]))
        self.assertEqual([row("run-a"), row("run-b")], self.remote_rows())
        for report in reports:
            git(self.remote, "merge-base", "--is-ancestor", report["commit"], "main")
        self.assertEqual(self.parent_commit, git(self.parent, "rev-parse", "HEAD"))

    def test_slow_concurrent_compiler_preparation_does_not_hold_publication_lock(self):
        ready = threading.Barrier(3)
        release = threading.Event()
        original = mirror._network

        def slow_prepare(command, **kwargs):
            if command[0] == "rust-script" and command[-1] == "--help":
                self.assertNotIn(mirror.LOCK_FD_ENV, kwargs["environment"])
                self.assertNotIn(series.LEDGER_ROOT_ENV, kwargs["environment"])
                self.assertEqual(kwargs.get("pass_fds", ()), ())
                ready.wait(15)
                self.assertTrue(release.wait(15), "compiler preparation was not released")
            return original(command, **kwargs)

        for name in ("prepared-a", "prepared-b"):
            self.append(name)
        with mock.patch.object(mirror, "_network", side_effect=slow_prepare), \
                ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(self.publish) for _ in range(2)]
            try:
                ready.wait(15)
                # Both genuine fixture child preparations are still blocked.
                # The real writer lock must remain available to another owner.
                with mirror.publication_writer(self.parent, net_wrapper="/usr/bin/env"):
                    self.assertTrue(all(not future.done() for future in futures))
                    self.assertEqual([], self.remote_rows())
            finally:
                release.set()
            reports = [future.result(timeout=30) for future in futures]
        self.assertTrue(all(report["state"] == "published" for report in reports), reports)
        self.assertEqual([row("prepared-a"), row("prepared-b")], self.remote_rows())

    def test_compiler_preparation_failure_preserves_raw_publication_and_retry(self):
        self.append("prepare-refused")
        original = mirror._network

        def refuse_prepare(command, **kwargs):
            if command[-1] == "--help":
                raise mirror.MirrorRefused("actual compiler preparation failed")
            return original(command, **kwargs)

        with mock.patch.object(mirror, "_network", side_effect=refuse_prepare):
            report = self.publish()
        self.assertEqual("derived-failed", report["state"])
        self.assertIn("compiler preparation failed", report["scorecard"]["error"])
        self.assertEqual([row("prepare-refused")], self.remote_rows())
        self.assertEqual("legacy scorecard", git(self.remote, "show", "main:scorecard/SCORECARD.md"))
        self.assertEqual("published", self.publish()["state"])
        self.assertEqual([row("prepare-refused")], self.remote_rows())

    def test_checker_preparation_failure_keeps_spool_without_taking_writer_lock(self):
        spool = self.append("checker-prepare-refused")
        original = spool.read_bytes()
        with mock.patch.object(series, "_run_shared_checker", return_value=
                               subprocess.CompletedProcess(["checker-fixture"], 2, "", "compile failed")), \
                mock.patch.object(mirror, "publication_writer",
                                  side_effect=AssertionError("failed preparation took writer lock")):
            with self.assertRaisesRegex(series.SeriesError, "cannot prepare.*compile failed"):
                self.publish()
        self.assertEqual(original, spool.read_bytes())
        self.assertEqual([], self.remote_rows())
        self.assertEqual("published", self.publish()["state"])
        self.assertEqual([row("checker-prepare-refused")], self.remote_rows())

    def test_legacy_export_does_not_relabel_or_rewrite_the_existing_scorecard(self):
        pending = self.append("unrelated-pending-result")
        pending_bytes = pending.read_bytes()
        seed = self.root / "seed"
        old_metadata = '{"scorecard_hermit_commit":"preserve the original source"}\n'
        (seed / "latest.json").write_text(old_metadata)
        git(seed, "add", "latest.json")
        git(seed, "commit", "-qm", "existing scorecard identity")
        git(seed, "push", "-q", "origin", "main")
        cells = self.source / "ci/compat-envelope/cells.json"
        cells.parent.mkdir(parents=True)
        cells.write_text('{"exact":"legacy source bytes"}\n')
        git(self.source, "add", "ci")
        git(self.source, "commit", "-qm", "legacy source document")
        self.target = git(self.source, "rev-parse", "HEAD")
        blob = git(self.source, "rev-parse", "HEAD:ci/compat-envelope/cells.json")
        tool = self.tool / "ci/compat-envelope/scorecard.rs"
        tool.write_text("#!/usr/bin/python3\nimport os, pathlib, sys\n"
                        + HELP_FIXTURE +
                        "assert sys.argv[1:] == ['export-legacy']\n"
                        "os.fstat(int(os.environ['DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD']))\n"
                        "p=pathlib.Path(os.environ['DEV_HERMIT_TEST_LEDGER_ROOT'])/'scorecard/legacy'\n"
                        "p.mkdir()\n"
                        f"(p/{(blob+'.json')!r}).write_bytes(pathlib.Path('ci/compat-envelope/cells.json').read_bytes())\n"
                        f"(p/{(blob+'.identity.json')!r}).write_text('{{}}\\n')\n")
        git(self.tool, "add", "ci")
        git(self.tool, "commit", "-qm", "legacy export transport fixture")
        report = self.publish(scorecard_command="export-legacy")
        self.assertEqual("published", report["legacy_export"]["state"], report)
        self.assertNotIn("scorecard", report)
        self.assertNotIn("rows", report)
        self.assertEqual(pending_bytes, pending.read_bytes())
        self.assertEqual([], self.remote_rows())
        self.assertEqual("legacy scorecard", git(self.remote, "show", "main:scorecard/SCORECARD.md"))
        self.assertEqual('{"legacy":"preserve"}', git(self.remote, "show", "main:scorecard/cells.json"))
        self.assertEqual(f"scorecard/legacy/{blob}.identity.json\nscorecard/legacy/{blob}.json",
                         git(self.remote, "diff-tree", "--no-commit-id", "--name-only", "-r", "main"))
        self.assertEqual(old_metadata.strip(), git(self.remote, "show", "main:latest.json"))

    def test_rejected_push_retains_spool_and_named_writer_commit(self):
        batch = self.append("reject")
        hook = self.remote / "hooks/pre-receive"
        hook.write_text("#!/bin/sh\necho deliberate-push-refusal >&2\nexit 1\n")
        hook.chmod(0o755)
        with self.assertRaisesRegex(mirror.MirrorRefused, "spool retained"):
            self.publish(publish_only=True, attempts=1)
        self.assertEqual([row("reject")], [json.loads(line) for line in batch.read_text().splitlines()])
        self.assertEqual([], self.remote_rows())
        writer = self.parent / mirror.WRITER_RELATIVE
        self.assertNotEqual(git(self.remote, "rev-parse", "main"), git(writer, "rev-parse", "HEAD"))
        refused_commit = git(writer, "rev-parse", "HEAD")
        hook.unlink()
        recovered = self.publish(publish_only=True)
        self.assertEqual([row("reject")], self.remote_rows())
        self.assertEqual(refused_commit, git(writer, "rev-parse", recovered["raw_publication"]["recovered_ref"]))
        self.assertFalse(batch.exists())

    def test_one_rejected_push_retries_append_without_duplicate(self):
        self.append("retry")
        marker = self.remote / "rejected-once"
        hook = self.remote / "hooks/pre-receive"
        hook.write_text(f"#!/bin/sh\nif [ ! -e '{marker}' ]; then touch '{marker}'; exit 1; fi\n")
        hook.chmod(0o755)
        report = self.publish(publish_only=True)
        self.assertEqual(2, report["raw_publication"]["attempts"])
        self.assertEqual([row("retry")], self.remote_rows())

    def test_recovery_refuses_a_changed_spooled_body_without_reset(self):
        batch = self.append("pending")
        hook = self.remote / "hooks/pre-receive"
        hook.write_text("#!/bin/sh\nexit 1\n")
        hook.chmod(0o755)
        with self.assertRaises(mirror.MirrorRefused):
            self.publish(publish_only=True, attempts=1)
        writer = self.parent / mirror.WRITER_RELATIVE
        refused = git(writer, "rev-parse", "HEAD")
        batch.write_text(json.dumps(row("pending", "diverged")) + "\n")
        hook.unlink()
        with self.assertRaisesRegex(mirror.MirrorRefused, "unchanged spooled append"):
            self.publish(publish_only=True)
        self.assertEqual(refused, git(writer, "rev-parse", "HEAD"))
        self.assertEqual([], self.remote_rows())
        self.assertTrue(batch.exists())

    def test_recovery_refuses_rewriting_a_prior_published_event(self):
        self.append("existing")
        self.publish(publish_only=True)
        writer = self.parent / mirror.WRITER_RELATIVE
        shard = writer / "series/hermit/fixture/2026-09.jsonl"
        shard.write_text(json.dumps(row("existing", "diverged")) + "\n")
        git(writer, "add", "series")
        git(writer, "commit", "-qm", "unpublished prior-event rewrite opponent")
        refused = git(writer, "rev-parse", "HEAD")
        with self.assertRaisesRegex(mirror.MirrorRefused, "unchanged spooled append"):
            self.publish(publish_only=True)
        self.assertEqual(refused, git(writer, "rev-parse", "HEAD"))
        self.assertEqual([row("existing")], self.remote_rows())

    def test_same_id_different_body_is_refused_and_original_survives(self):
        self.append("identity")
        self.publish(publish_only=True)
        with self.assertRaisesRegex(series.SeriesError, "different row"):
            self.append("identity", "diverged")
        self.assertEqual([row("identity")], self.remote_rows())

    def test_derived_failure_keeps_raw_publication_and_legacy_bytes(self):
        self.append("raw-survives")
        with mock.patch.object(mirror, "build_scorecard", side_effect=mirror.MirrorRefused("derived refusal")):
            report = self.publish()
        self.assertEqual("derived-failed", report["state"])
        self.assertEqual([row("raw-survives")], self.remote_rows())
        git(self.remote, "merge-base", "--is-ancestor", report["commit"], "main")
        self.assertEqual('{"legacy":"preserve"}', git(self.remote, "show", "main:scorecard/cells.json"))

    def test_repository_identity_disambiguates_old_parent_commits(self):
        old = series.published_snapshot_at(self.parent, self.parent_commit, repository=series.PARENT_REPOSITORY)
        self.assertEqual([row("historical-parent")], old.rows)
        current = series.published_snapshot_at(self.parent, "origin/main", repository=mirror.REMOTE_URL)
        self.assertEqual([], current.rows)
        self.assertEqual(mirror.REMOTE_URL, current.repository)
        with self.assertRaises(series.SeriesError):
            series.published_snapshot_at(self.parent, self.parent_commit, repository=mirror.REMOTE_URL)
        with self.assertRaises(series.SeriesError):
            series.published_repository(self.parent, "https://example.invalid/other.git")

    def test_remote_identity_mismatch_refuses_before_publication(self):
        self.append("keep")
        git(self.ledger, "config", "remote.origin.url", "https://example.invalid/other.git")
        with self.assertRaisesRegex(series.SeriesError, "unexpected origin"):
            self.publish(publish_only=True)
        self.assertEqual([], self.remote_rows())
        self.assertTrue(list(series.spool_dir(self.parent).glob("*.jsonl")))

    def test_measured_head_move_refuses_derived_delivery_after_raw_publication(self):
        self.append("head-move")
        run = mirror._run
        renders = []

        def move_after_scorecard(command, **kwargs):
            result = run(command, **kwargs)
            # The renderer is run directly, never through the network wrapper.
            if (command[0] == "rust-script" and str(self.tool / "ci/compat-envelope/scorecard.rs") in command
                    and command[-1] != "--help"):
                renders.append(command)
                git(self.source, "commit", "--allow-empty", "-qm", "concurrent source change")
            return result

        with mock.patch.object(mirror, "_run", side_effect=move_after_scorecard):
            report = self.publish()
        self.assertEqual(1, len(renders))
        self.assertEqual("derived-failed", report["state"])
        self.assertIn("source HEAD changed", report["scorecard"]["error"])
        self.assertEqual([row("head-move")], self.remote_rows())
        # The refused render stayed in its discarded copy.
        writer = self.parent / mirror.WRITER_RELATIVE
        self.assertEqual("legacy scorecard\n", (writer / "scorecard/SCORECARD.md").read_text())
        self.assertEqual("", git(writer, "status", "--porcelain"))

    def test_unknown_dirty_writer_is_retained_without_reset(self):
        self.publish(publish_only=True)
        writer = self.parent / mirror.WRITER_RELATIVE
        unknown = writer / "unpublished-work"
        unknown.write_bytes(b"preserve this work\n")
        batch = self.append("pending")
        with self.assertRaisesRegex(mirror.MirrorRefused, "unpublished work"):
            self.publish(publish_only=True)
        self.assertEqual(b"preserve this work\n", unknown.read_bytes())
        self.assertTrue(batch.exists())
        self.assertEqual([], self.remote_rows())

    def test_dirty_tool_inputs_refuse_without_mislabeling_the_raw_publication(self):
        helper = self.tool / "ci/manifest-plan/src/lib.rs"
        helper.parent.mkdir(parents=True)
        helper.write_text("// committed helper\n")
        verdict = self.tool / "hermit-cli/src/canonical_verdict.rs"
        verdict.parent.mkdir(parents=True)
        verdict.write_text("// committed verdict semantics\n")
        git(self.tool, "add", "ci/manifest-plan", "hermit-cli")
        git(self.tool, "commit", "-qm", "fixture helper input")
        for changed, staged in ((helper, False), (helper, True), (verdict, False)):
            identifier = f"dirty-tool-{changed.stem}-{staged}"
            with self.subTest(path=changed, staged=staged):
                changed.write_text("// changed compiler input\n")
                if staged:
                    git(self.tool, "add", str(changed.relative_to(self.tool)))
                self.append(identifier)
                report = self.publish()
                self.assertEqual("derived-failed", report["state"])
                self.assertIn("tool inputs have uncommitted changes", report["scorecard"]["error"])
                self.assertNotIn("tool_hermit_commit", report)
                self.assertEqual("legacy scorecard", git(self.remote, "show", "main:scorecard/SCORECARD.md"))
                git(self.tool, "restore", "--staged", "--worktree", "--", "ci/manifest-plan", "hermit-cli")
        self.assertCountEqual([row("dirty-tool-lib-False"), row("dirty-tool-lib-True"),
                              row("dirty-tool-canonical_verdict-False")], self.remote_rows())
        # Source catalogue/history belongs to the measured checkout, not the
        # compiler input closure: unrelated data dirt must not block delivery.
        (self.tool / "ci/compat-envelope/cells.json").write_text("unrelated data\n")
        self.assertEqual("published", self.publish()["state"])

    def test_normal_completion_records_real_ledger_commit_without_changing_verdict(self):
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/validate"))
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub"))
        import start_unit
        import run_registry

        # This launches the production finalizer/CLI and an executable Python
        # scorecard fixture. The row checker and canonical producer query are
        # doubles; this verifies Git delivery, not finalized proof validity.
        bootstrap = (
            "import runpy,sys,subprocess;"
            "expected_run_id=sys.argv.pop(1);"
            f"sys.path[:0]=[{str(MODULE.parent)!r},{str(mirror.TOOL_ROOT / 'ci-hub/ledger')!r}];"
            "import series,publisher,validate_rows;"
            "series._run_shared_checker=lambda *a: subprocess.CompletedProcess(a,0,'','');"
            "publisher._series_store=lambda:series;"
            "r={'repo':'hermit','run_id':expected_run_id,"
            "'record_id':'fixture-final','commit':sys.argv[sys.argv.index('--target')+1],"
            "'cwd':sys.argv[sys.argv.index('--source-checkout')+1],'started_at':'2026-09-21T12:00:00Z','finished_at':'2026-09-21T12:10:00Z'};"
            "validate_rows.rows=lambda **k:[r];"
            "validate_rows.canonical_finalized_row=lambda **k:r;"
            "sys.argv=sys.argv[1:];runpy.run_path(sys.argv[0],run_name='__main__')"
        )
        calls = []

        def native_run(command, **kwargs):
            calls.append(command)
            return subprocess.run(
                bounded_fixture_command([sys.executable, "-B", "-c", bootstrap,
                    expected_run_id, *command[1:], "--tool-checkout", str(self.tool)]),
                cwd=kwargs.get("cwd"), capture_output=True, text=True, timeout=30,
            )

        # Since 68e56d4c002af5dbe9ed055caa186cb89a8acfd5 ("Require explicit bot
        # GitHub routing") the finalizer restores the launch's GitHub executable
        # from the run record and refuses without one. Publication never needs
        # GitHub, so the record names a stub that fails loudly if called, and the
        # caller's own FLEET_REAL_GH is removed so it cannot supply the route.
        no_gh = self.root / "no-gh"
        no_gh.write_text("#!/bin/sh\necho 'GitHub must not be called' >&2\nexit 97\n")
        no_gh.chmod(0o755)
        for code, verdict in [(0, "VALIDATED"), (start_unit.EXIT_FAILED, "FAILED")]:
            with self.subTest(verdict=verdict):
                expected_run_id = "validate-" + verdict.lower()
                self.append(verdict)
                record_path = self.parent / f"{verdict}.json"
                record = {
                    "schema_version": run_registry.SCHEMA_VERSION,
                    "producer": "normal-completion-publication-fixture",
                    "repo": "rrnewton/hermit", "checkout": str(self.source),
                    "target": self.target, "canonical_verdict": verdict,
                    "unit": expected_run_id + ".service",
                    "wrapper_exit_code": code, "e2e_result_root": str(self.root / "results"),
                    "github_executable": str(no_gh),
                }
                run_registry.write_record(record_path, record)
                with mock.patch.dict(os.environ):
                    os.environ.pop("FLEET_REAL_GH", None)
                    result = start_unit.finalize_ledger_mirror(
                        self.parent, mirror.TOOL_ROOT, record_path, record=record,
                        run=native_run,
                    )
                self.assertTrue(start_unit.cell_ledger_published(result), result)
                self.assertEqual(verdict, result["canonical_verdict"])
                self.assertEqual(code, result["wrapper_exit_code"])
                commit = result["ledger_mirror"]["commit"]
                git(self.remote, "merge-base", "--is-ancestor", commit, "main")
                self.assertEqual(result, run_registry.read_record(record_path))
                self.assertIn("--results", calls[-1])
                self.assertEqual(expected_run_id,
                                 calls[-1][calls[-1].index("--finalized-run-id") + 1])
        self.assertEqual({"VALIDATED", "FAILED"}, {row["event_id"] for row in self.remote_rows()})
        self.assertEqual("", git(self.source, "status", "--porcelain"))

    def test_finalized_run_record_counts_only_that_runs_quarantined_inputs(self):
        # The run record is where a validate's refused inputs are counted.
        # start_unit copies the mirror's raw_publication into ledger_mirror and
        # names the run by the same suffix-free unit it exported as E2E_RUN_ID,
        # which is the --run-id the validate gave append-cells.
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/validate"))
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub"))
        import start_unit
        import run_registry

        expected_run_id = "validate-quarantine"
        bootstrap = (
            "import runpy,sys,subprocess;"
            f"sys.path[:0]=[{str(MODULE.parent)!r},{str(mirror.TOOL_ROOT / 'ci-hub/ledger')!r}];"
            "import series,publisher,validate_rows;"
            "series._run_shared_checker=lambda *a: subprocess.CompletedProcess(a,0,'','');"
            "publisher._series_store=lambda:series;"
            f"r={{'repo':'hermit','run_id':{expected_run_id!r},"
            "'record_id':'fixture-final','commit':sys.argv[sys.argv.index('--target')+1],"
            "'cwd':sys.argv[sys.argv.index('--source-checkout')+1],'started_at':'2026-09-21T12:00:00Z','finished_at':'2026-09-21T12:10:00Z'};"
            "validate_rows.rows=lambda **k:[r];"
            "validate_rows.canonical_finalized_row=lambda **k:r;"
            "sys.argv=sys.argv[1:];runpy.run_path(sys.argv[0],run_name='__main__')"
        )

        def native_run(command, **kwargs):
            return subprocess.run(
                bounded_fixture_command([sys.executable, "-B", "-c", bootstrap,
                                         *command[1:], "--tool-checkout", str(self.tool)]),
                cwd=kwargs.get("cwd"), env=kwargs.get("env"), capture_output=True, text=True,
                timeout=30,
            )

        # Publication never needs GitHub; a call would fail this test loudly.
        no_gh = self.root / "no-gh"
        no_gh.write_text("#!/bin/sh\necho 'GitHub must not be called' >&2\nexit 97\n")
        no_gh.chmod(0o755)
        self.append("QUARANTINED")
        mine = self.quarantine(expected_run_id, b'{"schema": 4, "truncated')
        other = self.quarantine("validate-other", b"another run's refusal")
        series.append_quarantine(self.parent, expected_run_id, [mine])
        series.append_quarantine(self.parent, "validate-other", [other])
        record_path = self.parent / "quarantine-run.json"
        record = {
            "schema_version": run_registry.SCHEMA_VERSION,
            "producer": "normal-completion-publication-fixture",
            "repo": "rrnewton/hermit", "checkout": str(self.source),
            "target": self.target, "canonical_verdict": "VALIDATED",
            "unit": expected_run_id + ".service", "wrapper_exit_code": 0,
            "e2e_result_root": str(self.root / "results"),
            "github_executable": str(no_gh),
        }
        run_registry.write_record(record_path, record)
        with mock.patch.dict(os.environ):
            os.environ.pop("FLEET_REAL_GH", None)
            result = start_unit.finalize_ledger_mirror(
                self.parent, mirror.TOOL_ROOT, record_path, record=record, run=native_run,
            )
        self.assertTrue(start_unit.cell_ledger_published(result), result)
        self.assertEqual(result, run_registry.read_record(record_path))
        counted = result["ledger_mirror"]["raw_publication"]["quarantine"]
        self.assertIsNone(counted["error"], counted)
        self.assertEqual(2, counted["published"])
        self.assertEqual(0, counted["pending_records"])
        # Violating: the other run's refusal is published too but is not this
        # run's, so it must not be counted against it.
        self.assertEqual({"run_id": expected_run_id, "published": 1, "withheld": 0,
                          "unpublished": 0, "records": 1}, counted["run"])
        self.assertEqual({mine["event_id"], other["event_id"]},
                         {item["event_id"] for item in self.remote_quarantine()})

    def spool_refused_series_batch(self) -> Path:
        """A series batch the publisher refuses before touching Git."""
        bogus = dict(row("zz-bogus"), emitted_at="bogus")
        path = series.spool_dir(self.parent) / "zz-bogus.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(bogus, sort_keys=True) + "\n")
        return path

    def test_refused_series_batch_still_publishes_the_quarantine_and_the_record_counts_it(self):
        # A refused series batch used to raise before the quarantine publish
        # ran, stranding every refused input on this host and leaving the run
        # record with state=failed and no count.
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/validate"))
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub"))
        import start_unit
        import run_registry

        expected_run_id = "validate-refused-series"
        bootstrap = (
            "import runpy,sys,subprocess;"
            f"sys.path[:0]=[{str(MODULE.parent)!r},{str(mirror.TOOL_ROOT / 'ci-hub/ledger')!r}];"
            "import series,publisher,validate_rows;"
            "series._run_shared_checker=lambda *a: subprocess.CompletedProcess(a,0,'','');"
            "publisher._series_store=lambda:series;"
            f"r={{'repo':'hermit','run_id':{expected_run_id!r},"
            "'record_id':'fixture-final','commit':sys.argv[sys.argv.index('--target')+1],"
            "'cwd':sys.argv[sys.argv.index('--source-checkout')+1],'started_at':'2026-09-21T12:00:00Z'};"
            "validate_rows.rows=lambda **k:[r];"
            "validate_rows.canonical_finalized_row=lambda **k:r;"
            "sys.argv=sys.argv[1:];runpy.run_path(sys.argv[0],run_name='__main__')"
        )

        def native_run(command, **kwargs):
            return subprocess.run(
                bounded_fixture_command([sys.executable, "-B", "-c", bootstrap,
                                         *command[1:], "--tool-checkout", str(self.tool)]),
                cwd=kwargs.get("cwd"), env=kwargs.get("env"), capture_output=True, text=True,
                timeout=30,
            )

        no_gh = self.root / "no-gh"
        no_gh.write_text("#!/bin/sh\necho 'GitHub must not be called' >&2\nexit 97\n")
        no_gh.chmod(0o755)
        bogus = self.spool_refused_series_batch()
        refused_batch = bogus.read_bytes()
        mine = self.quarantine(expected_run_id, b'{"schema": 4, "truncated')
        series.append_quarantine(self.parent, expected_run_id, [mine])
        record_path = self.parent / "refused-series-run.json"
        record = {
            "schema_version": run_registry.SCHEMA_VERSION,
            "producer": "normal-completion-publication-fixture",
            "repo": "rrnewton/hermit", "checkout": str(self.source),
            "target": self.target, "canonical_verdict": "VALIDATED",
            "unit": expected_run_id + ".service", "wrapper_exit_code": 0,
            "e2e_result_root": str(self.root / "results"),
            "github_executable": str(no_gh),
        }
        run_registry.write_record(record_path, record)
        with mock.patch.dict(os.environ):
            os.environ.pop("FLEET_REAL_GH", None)
            result = start_unit.finalize_ledger_mirror(
                self.parent, mirror.TOOL_ROOT, record_path, record=record, run=native_run,
            )
        mirrored = result["ledger_mirror"]
        # The series failure is still a failure, and the verdict is unchanged.
        self.assertEqual("failed", mirrored["state"], mirrored)
        self.assertIn("month must be YYYY-MM", mirrored["detail"])
        self.assertEqual("VALIDATED", result["canonical_verdict"])
        self.assertEqual(refused_batch, bogus.read_bytes(), "the refused batch stays spooled")
        self.assertEqual([], self.remote_rows())
        # The refused input landed in the ledger anyway ...
        self.assertEqual([series.portable_quarantine_record(mine)], self.remote_quarantine())
        # ... and the durable run record counts it.
        counted = mirrored["raw_publication"]["quarantine"]
        self.assertIsNone(counted["error"], counted)
        self.assertEqual((1, 0), (counted["published"], counted["pending_records"]))
        self.assertEqual({"run_id": expected_run_id, "published": 1, "withheld": 0,
                          "unpublished": 0, "records": 1}, counted["run"])
        self.assertEqual(result, run_registry.read_record(record_path))

    def test_a_failure_reports_quarantine_count_the_record_refuses_keeps_the_failure(self):
        # The run record's schema types a failed mirror's quarantine count
        # strictly, but only its readers check it: the writer stores whatever
        # it is given. An unfit count copied in as-is would make every strict
        # reader refuse the whole record. So it is checked before the copy; the
        # failure is still recorded, and the refusal is named in its detail.
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/validate"))
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub"))
        import start_unit
        import run_registry

        run_id = "validate-unfit-count"
        series.append_quarantine(self.parent, run_id, [self.quarantine(run_id, b"not json")])
        fits = mirror.publish_quarantine(self.parent, self.parent, None, net_wrapper="",
                                         attempts=1, run_id=run_id, blocked="fixture")
        self.assertEqual(1, fits["run"]["unpublished"], fits)
        no_gh = self.root / "no-gh"
        no_gh.write_text("#!/bin/sh\necho 'GitHub must not be called' >&2\nexit 97\n")
        no_gh.chmod(0o755)
        cases = [
            ("fits", fits),
            ("negative count", {**fits, "published": -1}),
            ("missing corrupt_files", {k: v for k, v in fits.items() if k != "corrupt_files"}),
            ("run with a boolean count", {**fits, "run": {**fits["run"], "unpublished": True}}),
        ]
        for index, (name, quarantine) in enumerate(cases):
            with self.subTest(name):
                failure = {"state": "failed", "repository": mirror.REMOTE_URL,
                           "error": "MirrorRefused: fixture series failure",
                           "raw_publication": {"quarantine": quarantine}}

                def failed_mirror(command, **kwargs):
                    return subprocess.CompletedProcess(
                        command, 1, json.dumps(failure, sort_keys=True) + "\n",
                        "ledger publication: FAILED: fixture series failure\n")

                record_path = self.parent / f"unfit-count-{index}.json"
                record = {
                    "schema_version": run_registry.SCHEMA_VERSION,
                    "producer": "normal-completion-publication-fixture",
                    "repo": "rrnewton/hermit", "checkout": str(self.source),
                    "target": self.target, "canonical_verdict": "VALIDATED",
                    "unit": run_id + ".service", "wrapper_exit_code": 0,
                    "e2e_result_root": str(self.root / "results"),
                    "github_executable": str(no_gh),
                }
                run_registry.write_record(record_path, record)
                with mock.patch.dict(os.environ):
                    os.environ.pop("FLEET_REAL_GH", None)
                    result = start_unit.finalize_ledger_mirror(
                        self.parent, mirror.TOOL_ROOT, record_path, record=record,
                        run=failed_mirror,
                    )
                self.assertEqual(result, run_registry.read_record(record_path))
                mirrored = result["ledger_mirror"]
                self.assertEqual("failed", mirrored["state"], mirrored)
                self.assertIn("fixture series failure", mirrored["detail"])
                self.assertEqual("VALIDATED", result["canonical_verdict"])
                # What was written is what the record's own schema accepts.
                run_registry._ledger_mirror(mirrored)
                if name == "fits":
                    self.assertEqual({"quarantine": fits}, mirrored["raw_publication"])
                    self.assertNotIn("quarantine outcome not recorded", mirrored["detail"])
                else:
                    self.assertNotIn("raw_publication", mirrored)
                    self.assertIn("quarantine outcome not recorded: "
                                  "validation-run-handle-ledger_mirror.raw_publication",
                                  mirrored["detail"])

    def test_failed_series_push_keeps_its_commit_and_still_publishes_the_quarantine(self):
        hook = self.remote / "hooks/pre-receive"
        hook.write_text(
            "#!/bin/sh\nwhile read old new ref; do\n"
            " if git diff --name-only \"$old\" \"$new\" | grep -q '^series/'; then\n"
            "  echo deliberate-series-refusal >&2; exit 1\n fi\ndone\nexit 0\n"
        )
        hook.chmod(0o755)
        batch = self.append("run-push-refused")
        record = self.quarantine("run-push-refused", b"not json")
        series.append_quarantine(self.parent, "run-push-refused", [record])
        writer = self.parent / mirror.WRITER_RELATIVE
        with self.assertRaises(mirror.SeriesPublicationFailed) as caught:
            self.publish(publish_only=True, attempts=1)
        self.assertIn("deliberate-series-refusal", str(caught.exception))
        quarantine = caught.exception.quarantine
        self.assertEqual((1, None, 0), (quarantine["published"], quarantine["error"],
                                        quarantine["pending_records"]))
        self.assertEqual([series.portable_quarantine_record(record)], self.remote_quarantine())
        self.assertEqual([], self.remote_rows())
        self.assertTrue(batch.exists(), "the series batch stays spooled for the retry")
        # The refused series commit is named by a durable recovery ref before
        # the writer moved to published main to publish the quarantine.
        [recovery] = git(writer, "for-each-ref", "--format=%(refname)",
                         "refs/heads/recovery/").splitlines()
        refused = recovery.removeprefix("refs/heads/recovery/cell-series-")
        self.assertIn("series/hermit/fixture/2026-09.jsonl",
                      git(writer, "show", "--name-only", "--format=", refused))
        hook.unlink()
        retried = self.publish(publish_only=True)
        self.assertEqual([row("run-push-refused")], self.remote_rows())
        self.assertEqual(0, retried["raw_publication"]["quarantine"]["published"])
        self.assertFalse(batch.exists())

    def test_scheduled_backlog_publishes_the_quarantine_past_a_refused_series_batch(self):
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/ledger"))
        import publish_backlog

        bootstrap = (
            "import runpy,sys,subprocess;"
            f"sys.path[:0]=[{str(MODULE.parent)!r},{str(mirror.TOOL_ROOT / 'ci-hub/ledger')!r}];"
            "import series,publisher;"
            "series._run_shared_checker=lambda *a: subprocess.CompletedProcess(a,0,'','');"
            "publisher._series_store=lambda:series;"
            "sys.argv=sys.argv[1:];runpy.run_path(sys.argv[0],run_name='__main__')"
        )
        native_run = subprocess.run
        commands = []

        def sweep_child(command, **kwargs):
            if str(MODULE) in command:
                commands.append(command)
                command = bounded_fixture_command(
                    [sys.executable, "-B", "-c", bootstrap, *command[1:]])
            return native_run(command, **kwargs)

        bogus = self.spool_refused_series_batch()
        record = self.quarantine("run-behind-bogus", b"not json")
        series.append_quarantine(self.parent, "run-behind-bogus", [record])
        self.assertEqual({"series", "series-quarantine"},
                         set(publish_backlog.pending_stores(self.parent)))
        with mock.patch.object(publish_backlog, "STATE_ROOT", self.parent), \
             mock.patch.object(publish_backlog.subprocess, "run", side_effect=sweep_child):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                failed = publish_backlog.main(["--net-wrapper", "/usr/bin/env", "--timeout-seconds", "30"])
        sweep = json.loads(output.getvalue())
        reports = {report["store"]: report for report in sweep["reports"]}
        # The series store still fails, loudly, on its refused batch ...
        self.assertEqual(1, failed)
        self.assertEqual(1, sweep["failed_stores"])
        self.assertEqual(1, reports["series"]["rc"])
        self.assertEqual("failed", reports["series"]["state"])
        self.assertIn("month must be YYYY-MM", reports["series"]["error"])
        self.assertTrue(bogus.exists())
        # ... yet its own failure report shows the refused input published
        # after the refused batch, not stranded behind it ...
        self.assertEqual(1, reports["series"]["raw_publication"]["quarantine"]["published"])
        # ... and the quarantine store, run through the quarantine-only mode
        # that never reads the series spool, succeeds with nothing left over.
        self.assertEqual((0, 0), (reports["series-quarantine"]["rc"],
                                  reports["series-quarantine"]["published"]))
        self.assertIn("--quarantine-only", commands[-1])
        self.assertNotIn("--quarantine-only", commands[0])
        self.assertEqual([series.portable_quarantine_record(record)], self.remote_quarantine())
        self.assertEqual({"series": series.spool_dir(self.parent)},
                         publish_backlog.pending_stores(self.parent))

    def test_quarantine_only_publication_never_publishes_series_rows(self):
        batch = self.append("row-must-wait")
        record = self.quarantine("run-q-only", b"not json")
        series.append_quarantine(self.parent, "run-q-only", [record])
        report = self.publish(publish_only=True, quarantine_only=True)
        self.assertEqual("quarantine only", report["raw_publication"]["skipped"])
        self.assertEqual(1, report["raw_publication"]["quarantine"]["published"])
        self.assertEqual([], self.remote_rows())
        self.assertTrue(batch.exists())
        with self.assertRaisesRegex(mirror.MirrorRefused, "requires --publish-only"):
            self.publish(quarantine_only=True)

    def test_a_failure_before_publication_still_reports_the_runs_local_count(self):
        record = self.quarantine("run-early", b"not json")
        series.append_quarantine(self.parent, "run-early", [record])
        failure = mirror.failure_report(
            self.parent, mirror.MirrorRefused("another ledger publisher still holds the lock"),
            run_id="run-early", count=True)
        quarantine = failure["raw_publication"]["quarantine"]
        self.assertEqual("failed", failure["state"])
        self.assertTrue(quarantine["error"].startswith("not attempted: MirrorRefused"), quarantine)
        self.assertEqual({"run_id": "run-early", "published": None, "withheld": None,
                          "unpublished": 1, "records": 1}, quarantine["run"])
        self.assertEqual(1, quarantine["pending_records"])
        # No count is invented for a caller that did not ask for one.
        self.assertIsNone(mirror.failure_report(
            self.parent, mirror.MirrorRefused("x"), run_id=None, count=False))

    def test_missing_finalized_run_identity_preserves_verdict_and_pending_spool(self):
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/validate"))
        import start_unit
        import run_registry

        spool = self.append("unbound-finalized")
        original = spool.read_bytes()
        for unit in (None, "", " "):
            with self.subTest(unit=unit):
                record_path = self.parent / "missing-unit.json"
                record = {"schema_version": run_registry.SCHEMA_VERSION,
                          "repo": "rrnewton/hermit", "checkout": str(self.source),
                          "target": self.target, "unit": unit, "canonical_verdict": "FAILED",
                          "wrapper_exit_code": start_unit.EXIT_FAILED,
                          "state": "completed", "e2e_result_root": str(self.root / "results")}
                run_registry.write_record(record_path, record)
                child = mock.Mock(side_effect=AssertionError("unbound publication must not launch"))
                result = start_unit.finalize_ledger_mirror(
                    self.parent, mirror.TOOL_ROOT, record_path, record=record, run=child)
                self.assertEqual("failed", result["ledger_mirror"]["state"])
                self.assertIn("canonical run unit", result["ledger_mirror"]["detail"])
                self.assertEqual("FAILED", result["canonical_verdict"])
                self.assertEqual(start_unit.EXIT_FAILED, result["wrapper_exit_code"])
                self.assertEqual(original, spool.read_bytes())
                child.assert_not_called()

    def test_finalized_run_uses_held_canonical_row_and_published_snapshot(self):
        self.append("finalized-cell")
        row_bytes = json.dumps({"run_id": "run-1", "record_id": "record-1",
                                "started_at": "2026-09-21T12:00:00Z",
                                "finished_at": "2026-09-21T12:10:00Z"}).encode()
        calls = []
        original = mirror._run
        writer = self.parent / mirror.WRITER_RELATIVE

        def inspect(command, **kwargs):
            if (command[0] == "rust-script" and str(self.tool / "ci/compat-envelope/scorecard.rs") in command
                    and command[-1] != "--help"):
                calls.append(command)
                # Measured code gets a throwaway ledger copy, no routes and no credentials.
                ledger = Path(kwargs["environment"][series.LEDGER_ROOT_ENV])
                self.assertNotEqual(writer.resolve(), ledger.resolve())
                self.assertTrue(ledger.parent.name.startswith(mirror.RENDER_COPY_PREFIX), ledger)
                self.assertEqual(writer.parent, ledger.parent.parent)
                self.assertEqual("true", kwargs["environment"]["CARGO_NET_OFFLINE"])
                self.assertEqual(mirror.RENDERER_WITHHELD_ENV, kwargs["withhold"])
                self.assertEqual(command[:5], [
                    "rust-script", "--force", "--toolchain",
                    "nightly-2026-07-12-x86_64-unknown-linux-gnu",
                    str(self.tool / "ci/compat-envelope/scorecard.rs"),
                ])
                self.assertEqual("project-and-observe-results", command[5])
                row_path = Path(command[command.index("--finalized-row") + 1])
                snapshot_path = Path(command[command.index("--snapshot") + 1])
                self.assertEqual(row_bytes, row_path.read_bytes())
                self.assertEqual(mirror.hashlib.sha256(row_bytes).hexdigest(),
                                 command[command.index("--finalized-row-sha256") + 1])
                snapshot = json.loads(snapshot_path.read_bytes())
                self.assertEqual(mirror.REMOTE_URL, snapshot["source"]["repository"])
                self.assertEqual(["finalized-cell"], [row["event_id"] for row in snapshot["rows"]])
                self.assertEqual(series.SCORECARD_SNAPSHOT_SCHEMA, snapshot["schema"])
                self.assertEqual(series.SCORECARD_SNAPSHOT_SOURCE, snapshot["source"]["path"])
                self.assertEqual(1, snapshot["source"]["published_rows"])
                self.assertEqual([], snapshot["source"]["local_sources"])
                encoded = snapshot_path.read_bytes()
                digest = mirror.hashlib.sha256(encoded).hexdigest()
                self.assertEqual(snapshot, series._validate_snapshot_payload(encoded, expected_sha256=digest))
                for field in ("schema", "path", "published_rows"):
                    malformed = json.loads(encoded)
                    del (malformed if field == "schema" else malformed["source"])[field]
                    bad = series._snapshot_bytes(malformed)
                    with self.subTest(missing_wire_field=field), self.assertRaises(series.SeriesError):
                        series._validate_snapshot_payload(bad, expected_sha256=mirror.hashlib.sha256(bad).hexdigest())
                with self.assertRaisesRegex(series.SeriesError, "digest mismatch"):
                    series._validate_snapshot_payload(encoded, expected_sha256="0" * 64)
                self.assertEqual(str(self.parent), command[command.index("--state-root") + 1])
            return original(command, **kwargs)

        with mock.patch.object(mirror, "finalized_producer_row", return_value=row_bytes) as query, \
                mock.patch.object(mirror, "_run", side_effect=inspect):
            report = mirror.mirror(self.parent, self.source, self.target, mirror.REMOTE_URL,
                                   net_wrapper="/usr/bin/env", tool_checkout=self.tool,
                                   results=self.root / "results", finalized_run_id="run-1")
        self.assertEqual("published", report["state"])
        self.assertEqual(2, query.call_count)
        self.assertEqual(1, len(calls))
        self.assertFalse(Path(calls[0][calls[0].index("--finalized-row") + 1]).exists())
        self.assertEqual([], list(writer.parent.glob(mirror.RENDER_COPY_PREFIX + "*")))

    def test_canonical_run_change_refuses_derived_commit_without_losing_rows(self):
        self.append("finalized-cell")
        before = git(self.remote, "rev-parse", "main:scorecard/cells.json")
        rows = [json.dumps({"started_at": "2026-09-21T12:00:00Z", "finished_at": "2026-09-21T12:10:00Z",
                            "record_id": record}).encode()
                for record in ["original", "correction"]]
        with mock.patch.object(mirror, "finalized_producer_row", side_effect=rows):
            report = mirror.mirror(self.parent, self.source, self.target, mirror.REMOTE_URL,
                                   net_wrapper="/usr/bin/env", tool_checkout=self.tool,
                                   results=self.root / "results", finalized_run_id="run-1")
        self.assertEqual("derived-failed", report["state"])
        self.assertIn("changed during", report["scorecard"]["error"])
        self.assertEqual(before, git(self.remote, "rev-parse", "main:scorecard/cells.json"))
        self.assertEqual(["finalized-cell"], [row["event_id"] for row in self.remote_rows()])
        # The renderer ran in a scratch copy; a refused render copies nothing
        # into the writer and leaves no copy behind.
        writer = self.parent / mirror.WRITER_RELATIVE
        self.assertEqual("", git(writer, "status", "--porcelain"))
        self.assertEqual([], list(writer.parent.glob(mirror.RENDER_COPY_PREFIX + "*")))

    def portable_fixture(self):
        # The fast suite models canonical selection and the native writer. Exact
        # transport and real Git are exercised here; this is not schema-10
        # native or cross-host authority qualification.
        fixtures = mirror.TOOL_ROOT / "ci-hub/validate/tests/fixtures/schema10"
        selected = json.loads((fixtures / "ordinary-only-row.json").read_bytes())
        selected.update(repo="hermit", record_id="portable-fixture", commit=self.target,
                        cwd=str(self.source.resolve()), started_at="2026-09-21T12:00:00Z",
                        finished_at="2026-09-21T12:10:00Z")
        for role, ref in mirror.finalized_proof.row_references(selected).items():
            path = self.parent / ref["path"]
            path.parent.mkdir(parents=True, exist_ok=True)
            suffix = "plan.json" if role == "plan" else role + ".jsonl"
            path.write_bytes((fixtures / ("ordinary-only-" + suffix)).read_bytes())
        source = self.parent / "ignored/ci-hub/ledger-publisher-clone"
        if not source.exists():  # a test may prepare several terminals
            source.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            git(source, "remote", "add", "origin", mirror.finalized_proof.PARENT_REPOSITORY)
        selection = {
            "row": selected,
            "terminal": {"repository": "rrnewton/hermit", "run_id": selected["run_id"],
                         "record_id": selected["record_id"], "event_id": "portable-terminal",
                         "event_sha256": "1" * 64, "hermit_commit": self.target},
            "parent": {"repository": mirror.finalized_proof.PARENT_REPOSITORY,
                       "commit": "2" * 40, "shard": "ledger/hermit/fixture/2026-09.jsonl",
                       "blob_oid": "3" * 40, "event_line_sha256": "4" * 64},
        }
        return mirror._row_bytes(selected), selection

    def test_portable_parent_source_binding_refuses_dirty_transitive_inputs(self):
        inputs = ("ci-hub/qualifying_receipt.py", "ci-hub/admission_paths.py",
                  "ci-hub/validate/gate_completeness.py", "ci-hub/validate/flaky-cells.json",
                  "ci-hub/health/ledger_spool_backlog.py", "scripts/check_commit_attribution.py")
        for relative in inputs:
            path = self.parent / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("committed source fixture\\n")
        git(self.parent, "add", "--", *inputs)
        git(self.parent, "commit", "-qm", "synthetic source closure")
        head = git(self.parent, "rev-parse", "HEAD")
        with mock.patch.object(mirror, "TOOL_ROOT", self.parent):
            self.assertEqual(head, mirror._parent_tool_commit())
            for relative in inputs:
                path = self.parent / relative
                before = path.read_bytes()
                path.write_bytes(before + b"dirty source fixture\\n")
                with self.subTest(relative=relative), self.assertRaisesRegex(mirror.MirrorRefused, "uncommitted"):
                    mirror._parent_tool_commit()
                path.write_bytes(before)
            # Source identity does not impose whole-repository cleanliness.
            (self.parent / "unrelated-note").write_text("unrelated fixture\\n")
            self.assertEqual(head, mirror._parent_tool_commit())

    def test_portable_proof_follows_parent_then_cell_lock_and_same_derived_commit(self):
        encoded, selection = self.portable_fixture()
        self.append("portable-cell")
        order = []
        actual_publish, actual_writer = mirror.publisher.publish, mirror.publication_writer
        namespace = mock.MagicMock()

        def publish(*args, **kwargs):
            if kwargs.get("store") == "ledger":
                order.append("parent-publish")
                self.assertEqual(self.parent, kwargs["source_root"])
                self.assertEqual(mirror.TOOL_ROOT / "scripts/parent-main-write", kwargs["serialized_writer"])
                return {"published": 1}
            # The refused-input spool is published after the series rows, in
            # the same held cell lock; it is recorded separately so the order
            # of both is asserted rather than counted as one.
            order.append("quarantine-publish" if kwargs.get("store") == "series-quarantine"
                         else "raw-publish")
            return actual_publish(*args, **kwargs)

        @contextlib.contextmanager
        def writer(*args, **kwargs):
            order.append("cell-lock")
            with actual_writer(*args, **kwargs) as held:
                yield held

        with mock.patch.object(mirror, "finalized_producer_row", return_value=encoded) as live, \
                mock.patch.object(mirror, "_parent_tool_commit", return_value="8" * 40), \
                mock.patch.object(mirror, "_select_published_terminal", return_value=(namespace, selection)) as committed, \
                mock.patch.object(mirror.publisher, "publish", side_effect=publish), \
                mock.patch.object(mirror, "publication_writer", side_effect=writer):
            report = self.publish(results=self.root / "results", finalized_run_id=selection["row"]["run_id"])
        self.assertEqual("published", report["state"], report)
        self.assertEqual(["parent-publish", "cell-lock", "raw-publish", "quarantine-publish"], order)
        self.assertEqual(2, live.call_count)
        self.assertEqual(2, committed.call_count)
        artifacts, _ = mirror.finalized_proof.capture_artifacts(self.parent, selection["row"])
        manifest = mirror.finalized_proof.make_manifest(selection, artifacts, parent_commit="8" * 40,
                                                       hermit_tool_commit=git(self.tool, "rev-parse", "HEAD"),
                                                       snapshot=mirror._published_snapshot(self.parent / mirror.WRITER_RELATIVE))
        path = mirror.finalized_proof.carrier_path(manifest)
        public = json.loads(git(self.remote, "show", "main:" + path))
        self.assertEqual(selection["terminal"], public["terminal"])
        self.assertEqual(artifacts, public["artifacts"])
        self.assertIn(path, report["scorecard"]["files"])
        self.assertEqual(report["commit"], git(self.remote, "rev-parse", "main"))
        self.assertNotIn("row", public)

    def test_parent_proof_unavailable_preserves_raw_without_derived_credit(self):
        encoded, selection = self.portable_fixture()
        self.append("unavailable-parent")
        before = git(self.remote, "rev-parse", "main:scorecard/cells.json")
        with mock.patch.object(mirror, "finalized_producer_row", return_value=encoded), \
                mock.patch.object(mirror, "prepare_portable_proof", side_effect=mirror.MirrorRefused("canonical terminal unpublished")), \
                mock.patch.object(mirror, "build_scorecard") as native:
            report = self.publish(results=self.root / "results", finalized_run_id=selection["row"]["run_id"])
        self.assertEqual("derived-failed", report["state"])
        self.assertIn("unpublished", report["scorecard"]["error"])
        native.assert_not_called()
        self.assertEqual(before, git(self.remote, "rev-parse", "main:scorecard/cells.json"))
        self.assertEqual(["unavailable-parent"], [r["event_id"] for r in self.remote_rows()])
        self.assertEqual("", git(self.remote, "ls-tree", "-r", "main", "--", "scorecard/finalized"))

    def test_non_cargo_release_payload_never_reaches_the_scorecard(self):
        # A Buck full run observes the release E2E payload. Its cells must not
        # become scorecard observations; raw rows still publish unchanged. A row
        # whose builder and payload disagree names no builder and is refused too.
        cases = json.loads((Path(__file__).resolve().parents[2] / "validate" / "tests"
                            / "fixtures" / "release-payload-cases.json").read_text())["cases"]
        self.assertEqual(7, sum(case["expected"] is not None for case in cases))
        for index, case in enumerate(cases):
            encoded = json.dumps(_apply_case({"started_at": "2026-09-21T12:00:00Z",
                                              "finished_at": "2026-09-21T12:10:00Z"}, case)).encode()
            if case["expected"] != "cargo":
                with self.subTest(case=case["label"]):
                    event = f"payload-{index}"
                    self.append(event)
                    before = git(self.remote, "rev-parse", "main:scorecard/cells.json")
                    with mock.patch.object(mirror, "finalized_producer_row", return_value=encoded), \
                            mock.patch.object(mirror, "prepare_portable_proof") as portable, \
                            mock.patch.object(mirror, "build_scorecard") as native:
                        report = self.publish(results=self.root / "results", finalized_run_id=event)
                    self.assertEqual("derived-failed", report["state"])
                    self.assertIn("non-Cargo release payload", report["scorecard"]["error"])
                    portable.assert_not_called()
                    native.assert_not_called()
                    self.assertEqual(before, git(self.remote, "rev-parse", "main:scorecard/cells.json"))
                    self.assertIn(event, [r["event_id"] for r in self.remote_rows()])
                continue
            # Direction control: a pairless row dated before the cutoff, or the
            # exact Cargo pair, proceeds.
            with self.subTest(case=case["label"]):
                with mock.patch.object(mirror, "finalized_producer_row", return_value=encoded), \
                        mock.patch.object(mirror, "prepare_portable_proof", return_value=None), \
                        mock.patch.object(mirror, "build_scorecard",
                                          side_effect=mirror.MirrorRefused("reached scorecard")) as native:
                    report = self.publish(results=self.root / "results", finalized_run_id=f"cargo-{index}")
                native.assert_called_once()
                self.assertEqual("reached scorecard", report["scorecard"]["error"])

    def test_strict_writer_failure_cannot_publish_portable_carrier(self):
        self.append("native-refusal")
        encoded = json.dumps({"started_at": "2026-09-21T12:00:00Z",
                              "finished_at": "2026-09-21T12:10:00Z"}).encode()
        with mock.patch.object(mirror, "finalized_producer_row", return_value=encoded), \
                mock.patch.object(mirror, "prepare_portable_proof", return_value={"prepared": True}), \
                mock.patch.object(mirror, "build_scorecard", side_effect=mirror.MirrorRefused("strict native refusal")), \
                mock.patch.object(mirror, "finish_portable_proof") as finish:
            report = self.publish(results=self.root / "results", finalized_run_id="native-refusal")
        self.assertEqual("derived-failed", report["state"])
        finish.assert_not_called()
        self.assertEqual(["native-refusal"], [r["event_id"] for r in self.remote_rows()])
        self.assertEqual("", git(self.remote, "ls-tree", "-r", "main", "--", "scorecard/finalized"))

    def derived_fixture(self, **row_updates):
        _encoded, selection = self.portable_fixture()
        selected = selection["row"]
        selected.update(schema_version=5, cell_results=None, result="fail", validation_complete=False)
        # Row facts are part of the derived binding, so apply them before it.
        selected.update(row_updates)
        if "run_id" in row_updates:
            selection["terminal"]["run_id"] = row_updates["run_id"]
        cell = {"lane": "portable", "category": "c-programs", "test": "fixture",
                "mode": "verify", "backend": "ptrace"}
        selected["raw_result_input_census_v1"] = {
            "schema": 1, "run_id": selected["run_id"], "hermit_sha": selected["commit"],
            "files": [{"path": "portable/results.jsonl", "bytes": 20, "sha256": "a" * 64,
                       "rows": [{"line": 1, "cell": cell, "attempt": 1}]}],
        }
        value = {"schema": 1, "authority": mirror.finalized_proof.DERIVED_AUTHORITY,
                 **mirror.finalized_proof.derived_row_binding(selected), "attempts": [{
                     "input_path": "portable/results.jsonl", "input_line": 1, "cell": cell,
                     "attempt": 1, "source_row_sha256": "b" * 64, "outcome": "FAIL",
                     "result": None, "failure_class": None, "ordinary_verdict": {}, "backend_parity": None}]}
        encoded = mirror.finalized_proof.canonical_bytes(value) + b"\n"
        prepared = {"selection": selection, "source": self.parent, "artifacts": None,
                    "objects": None, "parent_tool_commit": "8" * 40, "derived": True}
        return prepared, encoded

    def test_native_derived_emission_is_explicit_and_failed_child_cannot_stage(self):
        prepared, encoded = self.derived_fixture()
        row_before = mirror._row_bytes(prepared["selection"]["row"])
        calls = []

        def native(command, **kwargs):
            calls.append(command)
            self.assertEqual("compatibility-website-derived-census", command[5])
            self.assertEqual(str(mirror.TOOL_ROOT / "ci-hub/ci-hub.rs"), command[4])
            self.assertEqual(str(self.parent / "target"), kwargs["environment"]["CARGO_TARGET_DIR"])
            self.assertEqual(row_before, Path(command[command.index("--row-json") + 1]).read_bytes())
            Path(command[command.index("--output-json") + 1]).write_bytes(encoded)
            return subprocess.CompletedProcess(command, 0, "", "")

        with mock.patch.object(mirror, "_parent_tool_commit", return_value="8" * 40), \
                mock.patch.object(mirror, "_committed_tool_source", return_value="9" * 40), \
                mock.patch.object(mirror.rust_toolchain, "selected_toolchain", return_value="fixture-toolchain"), \
                mock.patch.object(mirror, "_network", side_effect=native):
            self.assertEqual("9" * 40, mirror.emit_derived_portable_proof(
                prepared, self.parent, self.root / "results", self.tool, net_wrapper="fixture"))
        self.assertEqual(1, len(calls))
        self.assertEqual(2, prepared["artifacts"]["schema"])
        self.assertEqual(row_before, mirror._row_bytes(prepared["selection"]["row"]))
        for status in (126, 127):
            prepared.update(artifacts=None, objects=None)
            with mock.patch.object(mirror, "_parent_tool_commit", return_value="8" * 40), \
                    mock.patch.object(mirror, "_committed_tool_source", return_value="9" * 40), \
                    mock.patch.object(mirror.rust_toolchain, "selected_toolchain", return_value="fixture-toolchain"), \
                    mock.patch.object(mirror, "_network", side_effect=mirror.MirrorRefused(f"native exit {status}")), \
                    self.assertRaises(mirror.MirrorRefused):
                mirror.emit_derived_portable_proof(prepared, self.parent, self.root / "results", self.tool,
                                                    net_wrapper="fixture")
            self.assertIsNone(prepared["artifacts"])

    def test_native_derived_emission_preserves_explicit_existing_target(self):
        prepared, encoded = self.derived_fixture()
        target = self.root / "existing-target"
        target.mkdir()
        calls = []

        def native(command, **kwargs):
            calls.append(command)
            self.assertEqual(str(target), kwargs["environment"]["CARGO_TARGET_DIR"])
            self.assertEqual(str(mirror.TOOL_ROOT / "ci-hub/ci-hub.rs"), command[4])
            self.assertEqual("compatibility-website-derived-census", command[5])
            Path(command[command.index("--output-json") + 1]).write_bytes(encoded)
            return subprocess.CompletedProcess(command, 0, "", "")

        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target)}), \
                mock.patch.object(mirror, "_parent_tool_commit", return_value="8" * 40), \
                mock.patch.object(mirror, "_committed_tool_source", return_value="9" * 40), \
                mock.patch.object(mirror.rust_toolchain, "selected_toolchain", return_value="fixture-toolchain"), \
                mock.patch.object(mirror, "_network", side_effect=native):
            self.assertEqual("9" * 40, mirror.emit_derived_portable_proof(
                prepared, self.parent, self.root / "results", self.tool, net_wrapper="fixture"))
        self.assertEqual(1, len(calls))
        self.assertEqual(2, prepared["artifacts"]["schema"])
        self.assertFalse((self.parent / "target").exists())

    def test_proof_only_attachment_preserves_published_projection_and_requires_new_operands(self):
        # Native/parent authorities are explicit doubles. This tests real Git
        # delivery and preservation, not the native comparison verifier.
        self.append("existing-finalized-cell")
        original = self.publish()
        self.assertEqual("published", original["state"], original)
        before = {path: git(self.remote, "rev-parse", "main:" + path)
                  for path in ("latest.json", "scorecard/cells.json", "scorecard/SCORECARD.md")}
        prepared, encoded = self.derived_fixture()
        selected = prepared["selection"]
        native_calls = []

        def native(value, *args, **kwargs):
            native_calls.append(value)
            value["artifacts"], value["objects"] = mirror.finalized_proof.capture_derived_cells(selected["row"], encoded)
            return git(self.tool, "rev-parse", "HEAD")

        with mock.patch.object(mirror, "finalized_producer_row", return_value=mirror._row_bytes(selected["row"])), \
                mock.patch.object(mirror, "prepare_portable_proof", return_value=prepared), \
                mock.patch.object(mirror, "_parent_tool_commit", return_value="8" * 40), \
                mock.patch.object(mirror, "_select_published_terminal", return_value=(mock.MagicMock(), selected)), \
                mock.patch.object(mirror, "emit_derived_portable_proof", side_effect=native), \
                mock.patch.object(mirror, "build_scorecard") as scorecard:
            report = self.publish(results=self.root / "results", finalized_run_id=selected["row"]["run_id"],
                                  portable_proof_only=True)
        self.assertEqual("published", report["state"], report)
        self.assertIn("portable_proof", report)
        self.assertNotIn("scorecard", report)
        self.assertEqual(1, len(native_calls))
        scorecard.assert_not_called()
        self.assertEqual(before, {path: git(self.remote, "rev-parse", "main:" + path) for path in before})
        self.assertEqual(original["commit"], report["existing_scorecard"]["commit"])
        self.assertEqual(["existing-finalized-cell"], [value["event_id"] for value in self.remote_rows()])
        self.assertEqual("fail", selected["row"]["result"])
        writer = self.parent / mirror.WRITER_RELATIVE
        snapshot = mirror._published_snapshot(writer)
        metadata = json.loads((writer / "latest.json").read_bytes())
        for field, value in (("source_tree", "0" * 40), ("rows_sha256", "0" * 64),
                             ("scorecard_hermit_commit", "0" * 40)):
            changed = dict(metadata, **{field: value})
            (writer / "latest.json").write_text(json.dumps(changed))
            with self.subTest(field=field), self.assertRaises(mirror.MirrorRefused):
                mirror.published_scorecard_binding(writer, snapshot, self.target)
        (writer / "latest.json").write_text(json.dumps(metadata))
        (writer / "scorecard/cells.json").write_text("changed")
        with self.assertRaises(mirror.MirrorRefused):
            mirror.published_scorecard_binding(writer, snapshot, self.target)

    def test_proof_only_attachment_after_raw_append_keeps_both_source_bindings(self):
        # As above, native authority is an explicit double; Git, the historical
        # source binding and the current-series carrier transaction are real.
        self.append("frozen-scorecard-cell")
        original = self.publish()
        self.assertEqual("published", original["state"], original)
        before = {path: git(self.remote, "rev-parse", "main:" + path)
                  for path in ("latest.json", "scorecard/cells.json", "scorecard/SCORECARD.md")}
        self.append("later-raw-cell")
        appended = self.publish(publish_only=True)
        self.assertEqual("published", appended["state"], appended)
        self.assertNotEqual(original["commit"], appended["commit"])
        current_rows = self.remote_rows()
        self.assertEqual({"frozen-scorecard-cell", "later-raw-cell"},
                         {value["event_id"] for value in current_rows})
        prepared, encoded = self.derived_fixture()
        selected = prepared["selection"]

        def native(value, *args, **kwargs):
            value["artifacts"], value["objects"] = mirror.finalized_proof.capture_derived_cells(selected["row"], encoded)
            return git(self.tool, "rev-parse", "HEAD")

        with mock.patch.object(mirror, "finalized_producer_row", return_value=mirror._row_bytes(selected["row"])), \
                mock.patch.object(mirror, "prepare_portable_proof", return_value=prepared), \
                mock.patch.object(mirror, "_parent_tool_commit", return_value="8" * 40), \
                mock.patch.object(mirror, "_select_published_terminal", return_value=(mock.MagicMock(), selected)), \
                mock.patch.object(mirror, "emit_derived_portable_proof", side_effect=native), \
                mock.patch.object(mirror, "build_scorecard") as scorecard:
            report = self.publish(results=self.root / "results", finalized_run_id=selected["row"]["run_id"],
                                  portable_proof_only=True)
        self.assertEqual("published", report["state"], report)
        scorecard.assert_not_called()
        self.assertEqual(before, {path: git(self.remote, "rev-parse", "main:" + path) for path in before})
        self.assertEqual(current_rows, self.remote_rows())
        self.assertEqual(appended["commit"], report["series_commit"])
        carrier_paths = [path for path in report["portable_proof"]["files"]
                         if path.startswith("scorecard/finalized/runs/")]
        self.assertEqual(1, len(carrier_paths))
        carrier = json.loads(git(self.remote, "show", "main:" + carrier_paths[0]))
        self.assertEqual(appended["commit"], carrier["series"]["commit"])
        self.assertEqual(selected["terminal"], carrier["terminal"])
        self.assertEqual(5, selected["row"]["schema_version"])
        self.assertEqual("fail", selected["row"]["result"])
        self.assertFalse(selected["row"]["validation_complete"])
        self.assertIsNone(selected["row"]["cell_results"])

    def test_historical_scorecard_refuses_unbound_source_and_changed_original_events(self):
        self.append("original-cell")
        self.assertEqual("published", self.publish()["state"])
        self.append("later-cell")
        self.assertEqual("published", self.publish(publish_only=True)["state"])
        writer = self.parent / mirror.WRITER_RELATIVE
        snapshot = mirror._published_snapshot(writer)
        metadata_path = writer / "latest.json"
        encoded = metadata_path.read_bytes()
        metadata = json.loads(encoded)
        mirror.published_scorecard_binding(writer, snapshot, self.target)
        for field, value in (("source_tree", "0" * 40), ("rows_sha256", "0" * 64),
                             ("rows", metadata["rows"] + 1), ("series_commit", "0" * 40)):
            metadata_path.write_text(json.dumps(dict(metadata, **{field: value})))
            with self.subTest(field=field), self.assertRaises(mirror.MirrorRefused):
                mirror.published_scorecard_binding(writer, snapshot, self.target)
        # Even a real commit with identical source bytes is not authority when
        # it is outside the admitted publication's ancestry.
        orphan = git(writer, "commit-tree", git(writer, "rev-parse", metadata["source_commit"] + "^{tree}"),
                     "-m", "unrelated source with identical tree")
        metadata_path.write_text(json.dumps(dict(metadata, source_commit=orphan, series_commit=orphan)))
        with self.assertRaises(mirror.MirrorRefused):
            mirror.published_scorecard_binding(writer, snapshot, self.target)
        # Naming the current source must not silently rebind the old digest.
        metadata_path.write_text(json.dumps(dict(metadata, source_commit=snapshot["source"]["commit"],
                                                series_commit=snapshot["source"]["commit"])))
        with self.assertRaisesRegex(mirror.MirrorRefused, "immutable raw source"):
            mirror.published_scorecard_binding(writer, snapshot, self.target)
        metadata_path.write_bytes(encoded)
        scorecard = writer / "scorecard/cells.json"
        scorecard_bytes = scorecard.read_bytes()
        scorecard.write_bytes(scorecard_bytes + b" ")
        with self.assertRaisesRegex(mirror.MirrorRefused, "bytes differ"):
            mirror.published_scorecard_binding(writer, snapshot, self.target)
        scorecard.write_bytes(scorecard_bytes)
        shard = writer / "series/hermit/fixture/2026-09.jsonl"
        for original_rows in ([row("original-cell", "diverged")], []):
            shard.write_text("".join(json.dumps(value) + "\n" for value in [*original_rows, row("later-cell")]))
            git(writer, "add", "series")
            git(writer, "commit", "-qm", "invalid mutation of historical cohort")
            with self.subTest(original_rows=original_rows), \
                    self.assertRaisesRegex(mirror.MirrorRefused, "changed or removed"):
                mirror.published_scorecard_binding(writer, mirror._published_snapshot(writer), self.target)

    def test_historical_scorecard_preserves_json_types_in_original_events(self):
        original = dict(row("original-cell"), extra={"flag": True, "count": 1})
        series.append_cell_rows(self.parent, series.spool_dir(self.parent) / "original.jsonl", [original])
        self.assertEqual("published", self.publish()["state"])
        writer = self.parent / mirror.WRITER_RELATIVE
        mirror.published_scorecard_binding(writer, mirror._published_snapshot(writer), self.target)
        shard = writer / "series/hermit/fixture/2026-09.jsonl"
        for extra in ({"flag": 1, "count": 1}, {"flag": True, "count": 1.0}):
            changed = dict(original, extra=extra)
            self.assertEqual(original, changed, "Python equality alone misses this mutation")
            self.assertNotEqual(series._canonical_row_json(original), series._canonical_row_json(changed))
            shard.write_text(json.dumps(changed) + "\n")
            git(writer, "add", "series")
            git(writer, "commit", "-qm", "invalid JSON type mutation of historical event")
            with self.subTest(extra=extra), self.assertRaisesRegex(mirror.MirrorRefused, "changed or removed"):
                mirror.published_scorecard_binding(writer, mirror._published_snapshot(writer), self.target)

    # --- refused series inputs (series quarantine) -------------------------

    QUARANTINE_SHARD = "quarantine/series/hermit/fixture/2026-09.jsonl"

    def quarantine(self, run_id: str, raw: bytes, *, emitted_at: str = "2026-09-21T12:00:00Z") -> dict:
        return series.quarantine_record(
            stage="projection", run_id=run_id, producer="validate", tree="a" * 40,
            emitted_at=emitted_at, error="MalformedRow", reason="fixture refusal",
            inputs=[series.quarantine_input(1, raw)], host="fixture",
        )

    def remote_quarantine(self) -> list[dict]:
        shown = subprocess.run(["git", "-C", str(self.remote), "show", f"main:{self.QUARANTINE_SHARD}"],
                               capture_output=True, text=True, check=False)
        if shown.returncode != 0:
            return []
        return [json.loads(line) for line in shown.stdout.split("\n") if line.strip()]

    def refuse_quarantine_pushes(self) -> Path:
        hook = self.remote / "hooks/pre-receive"
        hook.write_text(
            "#!/bin/sh\nwhile read old new ref; do\n"
            " if git diff --name-only \"$old\" \"$new\" | grep -q '^quarantine/'; then\n"
            "  echo deliberate-quarantine-refusal >&2; exit 1\n fi\ndone\nexit 0\n"
        )
        hook.chmod(0o755)
        return hook

    def test_quarantine_publishes_beside_the_series_and_is_counted(self):
        needle = series._OWNER_PATH_NEEDLE
        plain = self.quarantine("run-q", b'{"schema": 4, "truncated')
        owner = self.quarantine("run-q", json.dumps({"cwd": needle + "alice/work"}).encode())
        self.append("run-q")
        retained = series.append_quarantine(self.parent, "run-q", [plain, owner])
        self.assertEqual(2, retained["written"])
        local = retained["path"]
        self.assertEqual(series.quarantine_dir(self.parent), local.parent)
        exact = local.read_bytes()

        report = self.publish(publish_only=True)
        quarantine = report["raw_publication"]["quarantine"]
        self.assertIsNone(quarantine["error"], quarantine)
        self.assertEqual(2, quarantine["published"])
        self.assertEqual(0, quarantine["pending_records"])
        self.assertEqual(0, quarantine["held_records"])
        self.assertEqual([row("run-q")], self.remote_rows(), "the series rows publish as before")
        published = self.remote_quarantine()
        expected = series.union_events([series.portable_quarantine_record(plain),
                                        series.portable_quarantine_record(owner)])
        self.assertEqual(expected, published)
        self.assertNotIn(needle, git(self.remote, "show", f"main:{self.QUARANTINE_SHARD}"))
        series.validate_quarantine_records(published, public=True)
        # Nothing is read as series data: the series tree is unchanged.
        self.assertEqual("series/hermit/fixture/2026-09.jsonl",
                         git(self.remote, "ls-tree", "-r", "--name-only", "main", "series"))
        # The exact local copy waits in published/ until the ledger reads it.
        self.assertFalse(local.exists())
        self.assertEqual(exact, (local.parent / "published" / local.name).read_bytes())

        writer = self.parent / mirror.WRITER_RELATIVE
        counted = mirror.publish_quarantine(self.parent, self.ledger, writer,
                                            net_wrapper="/usr/bin/env", attempts=1, run_id="run-q")
        self.assertEqual({"run_id": "run-q", "published": 2, "withheld": 0, "unpublished": 0,
                          "records": 2}, counted["run"])
        self.assertFalse((local.parent / "published" / local.name).exists(), "reaped once readable")

        # A replay of the same refusals publishes nothing new.
        replay = [self.quarantine("run-q", b'{"schema": 4, "truncated', emitted_at="2026-10-02T12:00:00Z")]
        replayed = series.append_quarantine(self.parent, "run-q", replay)
        self.assertIsNone(replayed["published_check_error"])
        self.assertEqual((0, 1), (replayed["written"], replayed["duplicates"]))
        again = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual((0, None), (again["published"], again["error"]))
        self.assertEqual(expected, self.remote_quarantine())

    def test_a_replayed_refusal_published_once_even_when_the_writer_could_not_dedupe(self):
        first = self.quarantine("run-d", b"not json")
        series.append_quarantine(self.parent, "run-d", [first])
        self.publish(publish_only=True)
        self.publish(publish_only=True)  # reaps the local published/ copy
        self.assertEqual([], list((series.quarantine_dir(self.parent) / "published").glob("*.jsonl")))
        later = self.quarantine("run-d", b"not json", emitted_at="2026-10-02T12:00:00Z")
        with mock.patch.object(series, "published_quarantine_at",
                               side_effect=series.SeriesError("ledger unreadable")):
            retained = series.append_quarantine(self.parent, "run-d", [later])
        self.assertEqual("ledger unreadable", retained["published_check_error"])
        self.assertEqual(1, retained["written"])
        report = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual((0, 1, None), (report["published"], report["already_published"], report["error"]))
        self.assertEqual([series.portable_quarantine_record(first)], self.remote_quarantine())
        self.assertFalse(git(self.remote, "ls-tree", "-r", "--name-only", "main",
                             "quarantine/series/hermit/fixture/2026-10.jsonl"))

    def test_quarantine_push_failure_is_recorded_and_the_spool_retried(self):
        self.append("run-f")
        record = self.quarantine("run-f", b"not json")
        local = series.append_quarantine(self.parent, "run-f", [record])["path"]
        exact = local.read_bytes()
        hook = self.refuse_quarantine_pushes()
        report = self.publish(publish_only=True, attempts=1)
        quarantine = report["raw_publication"]["quarantine"]
        self.assertEqual([row("run-f")], self.remote_rows(), "a quarantine failure never undoes the series")
        self.assertIn("PublishRefused", quarantine["error"])
        self.assertEqual((0, 1), (quarantine["published"], quarantine["pending_records"]))
        self.assertEqual(exact, local.read_bytes())
        self.assertEqual([], self.remote_quarantine())
        writer = self.parent / mirror.WRITER_RELATIVE
        self.assertEqual(git(writer, "rev-parse", "refs/remotes/origin/main"), git(writer, "rev-parse", "HEAD"))
        self.assertEqual("", git(writer, "status", "--porcelain"))
        hook.unlink()
        retried = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual((1, None, 0), (retried["published"], retried["error"], retried["pending_records"]))
        self.assertEqual([series.portable_quarantine_record(record)], self.remote_quarantine())

    def test_withheld_quarantine_is_published_as_a_stub_and_held_locally(self):
        needle = series._OWNER_PATH_NEEDLE
        raw = needle.encode() + b"alice/\xff"
        record = self.quarantine("run-w", raw)
        local = series.append_quarantine(self.parent, "run-w", [record])["path"]
        report = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual((1, None), (report["published"], report["error"]))
        [stub] = self.remote_quarantine()
        self.assertEqual("owner-path", stub["withheld"]["reason"])
        self.assertEqual((None, None), (stub["inputs"][0]["raw"], stub["inputs"][0]["raw_base64"]))
        self.assertEqual(hashlib.sha256(raw).hexdigest(), stub["inputs"][0]["raw_sha256"])
        writer = self.parent / mirror.WRITER_RELATIVE
        counted = mirror.publish_quarantine(self.parent, self.ledger, writer,
                                            net_wrapper="/usr/bin/env", attempts=1, run_id="run-w")
        self.assertEqual(1, counted["held_records"])
        self.assertEqual(1, counted["run"]["withheld"])
        held = series.quarantine_dir(self.parent) / series.QUARANTINE_HELD_SUBDIR / local.name
        self.assertEqual([record], series._read_quarantine_file(held), "the only exact copy is kept")
        self.assertEqual([], list((series.quarantine_dir(self.parent) / "published").glob("*.jsonl")))

    def test_append_never_waits_on_a_publisher_holding_the_quarantine_lock(self):
        # The publisher holds the exclusive quarantine lock across its network
        # push. An append used to wait for it (at most 120 s) and lose the
        # refused input it held in memory on timeout.
        directory = series.quarantine_dir(self.parent)
        directory.mkdir(parents=True, exist_ok=True)
        record = self.quarantine("run-busy", b"not json")
        with series.series_spool_transition_lock(directory, exclusive=True, timeout_seconds=5), \
             mock.patch.object(series, "QUARANTINE_LOCK_TIMEOUT_SECONDS", 0.5):
            started = time.monotonic()
            retained = series.append_quarantine(self.parent, "run-busy", [record])
            elapsed = time.monotonic() - started
        self.assertEqual(1, retained["written"])
        self.assertLess(elapsed, 5.0)
        self.assertEqual([record], series._read_quarantine_file(retained["path"]))
        self.assertEqual([], list(directory.glob(".*.partial")), "no partial file is left behind")
        # Two appends never share a file, so neither can tear the other.
        second = series.append_quarantine(
            self.parent, "run-busy", [self.quarantine("run-busy", b"other")])["path"]
        self.assertNotEqual(retained["path"], second)

    def test_an_append_during_publication_survives_the_drain_and_publishes_next(self):
        first = self.quarantine("run-a", b"first")
        series.append_quarantine(self.parent, "run-a", [first])
        late = self.quarantine("run-late", b"late")
        original = mirror.publisher._store_snapshot
        appended: list[Path] = []

        def snapshot_then_append(store, spool, *args, **kwargs):
            result = original(store, spool, *args, **kwargs)
            if store == "series-quarantine" and not appended:
                appended.append(series.append_quarantine(self.parent, "run-late", [late])["path"])
            return result

        with mock.patch.object(mirror.publisher, "_store_snapshot", side_effect=snapshot_then_append):
            report = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual((1, None, 1), (report["published"], report["error"], report["pending_records"]))
        self.assertEqual([series.portable_quarantine_record(first)], self.remote_quarantine())
        [late_path] = appended
        self.assertTrue(late_path.exists(), "a file written after the snapshot is not drained")
        again = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual((1, 0), (again["published"], again["pending_records"]))
        self.assertEqual(series.union_events([series.portable_quarantine_record(first),
                                              series.portable_quarantine_record(late)]),
                         self.remote_quarantine())

    def test_a_truncated_quarantine_file_is_set_aside_and_the_others_publish(self):
        directory = series.quarantine_dir(self.parent)
        directory.mkdir(parents=True, exist_ok=True)
        whole = json.dumps(self.quarantine("run-trunc", b"x"), sort_keys=True)
        truncated = directory / "run-trunc.1-1-00000000.jsonl"
        truncated.write_text(whole[: len(whole) // 2])
        exact = truncated.read_bytes()
        (directory / "published").mkdir()
        stale = directory / "published" / "run-old.1-1-00000000.jsonl"
        stale.write_bytes(b"\xff\xfe not utf-8\n")
        # An append beside the unreadable files still works ...
        good = self.quarantine("run-good", b"not json")
        self.assertEqual(1, series.append_quarantine(self.parent, "run-good", [good])["written"])
        # ... and publication moves them aside instead of refusing the store.
        report = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual((1, None, 0), (report["published"], report["error"], report["pending_records"]))
        self.assertEqual(["pending--run-trunc.1-1-00000000.jsonl",
                          "published--run-old.1-1-00000000.jsonl"], report["set_aside"])
        self.assertEqual(2, report["corrupt_files"])
        self.assertEqual([series.portable_quarantine_record(good)], self.remote_quarantine())
        corrupt = directory / series.QUARANTINE_CORRUPT_SUBDIR
        self.assertEqual(exact, (corrupt / "pending--run-trunc.1-1-00000000.jsonl").read_bytes(),
                         "set aside byte for byte, never rewritten")
        self.assertFalse(truncated.exists() or stale.exists())
        # Nothing republishes or deletes corrupt/.
        later = self.publish(publish_only=True)["raw_publication"]["quarantine"]
        self.assertEqual(([], 2), (later["set_aside"], later["corrupt_files"]))

    def test_recovery_republishes_an_unpushed_quarantine_append_and_refuses_others(self):
        self.publish(publish_only=True)
        writer = self.parent / mirror.WRITER_RELATIVE
        spooled = self.quarantine("run-r", b"not json")
        series.append_quarantine(self.parent, "run-r", [spooled])
        shard = writer / self.QUARANTINE_SHARD

        def commit(records):
            shard.parent.mkdir(parents=True, exist_ok=True)
            shard.write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in records))
            git(writer, "add", "quarantine")
            git(writer, "commit", "-qm", "series-quarantine: append event(s)")
            return git(writer, "rev-parse", "HEAD")

        # Violating: the unpushed commit adds a record that is not spooled here.
        foreign = self.quarantine("run-other", b"never spooled")
        refused = commit([series.portable_quarantine_record(foreign)])
        with self.assertRaisesRegex(mirror.MirrorRefused, "unchanged spooled quarantine append") as caught:
            self.publish(publish_only=True)
        self.assertEqual(refused, git(writer, "rev-parse", "HEAD"))
        self.assertEqual([], self.remote_quarantine())
        # Publishing would move the writer off work it cannot account for, so
        # the quarantine is not attempted; it is still counted.
        blocked = caught.exception.quarantine
        self.assertTrue(blocked["error"].startswith("not attempted: "), blocked)
        self.assertEqual((0, 1), (blocked["published"], blocked["pending_records"]))
        git(writer, "reset", "-q", "--hard", "refs/remotes/origin/main")

        # Qualifying: a crash between commit and push left exactly the spooled records.
        crashed = commit([series.portable_quarantine_record(spooled)])
        report = self.publish(publish_only=True)
        self.assertEqual(f"refs/heads/recovery/cell-series-{crashed}",
                         report["raw_publication"]["recovered_ref"])
        self.assertIsNone(report["raw_publication"]["quarantine"]["error"])
        self.assertEqual([series.portable_quarantine_record(spooled)], self.remote_quarantine())

    def test_scheduled_backlog_publishes_a_quarantine_only_backlog(self):
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/ledger"))
        import publish_backlog

        bootstrap = (
            "import runpy,sys,subprocess;"
            f"sys.path[:0]=[{str(MODULE.parent)!r},{str(mirror.TOOL_ROOT / 'ci-hub/ledger')!r}];"
            "import series,publisher;"
            "series._run_shared_checker=lambda *a: subprocess.CompletedProcess(a,0,'','');"
            "publisher._series_store=lambda:series;"
            "sys.argv=sys.argv[1:];runpy.run_path(sys.argv[0],run_name='__main__')"
        )
        native_run = subprocess.run

        def sweep_child(command, **kwargs):
            if str(MODULE) in command:
                command = bounded_fixture_command(
                    [sys.executable, "-B", "-c", bootstrap, *command[1:]])
            return native_run(command, **kwargs)

        # Every row of the run was refused: no series batch triggers a publish.
        record = self.quarantine("run-only-refused", b"not json")
        series.append_quarantine(self.parent, "run-only-refused", [record])
        self.assertEqual({"series-quarantine": series.quarantine_dir(self.parent)},
                         publish_backlog.pending_stores(self.parent))
        hook = self.refuse_quarantine_pushes()
        with mock.patch.object(publish_backlog, "STATE_ROOT", self.parent), \
             mock.patch.object(publish_backlog.subprocess, "run", side_effect=sweep_child):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                failed = publish_backlog.main(["--net-wrapper", "/usr/bin/env", "--timeout-seconds", "30"])
            failure = json.loads(output.getvalue())
            # The mirror exits 0 (its series had nothing to publish); the store
            # is judged by its own quarantine report.
            self.assertEqual(1, failed)
            self.assertEqual(1, failure["failed_stores"])
            [store] = failure["reports"]
            self.assertEqual(("series-quarantine", 1), (store["store"], store["rc"]))
            self.assertIn("PublishRefused", store["error"])
            self.assertEqual([], self.remote_quarantine())
            hook.unlink()
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                passed = publish_backlog.main(["--net-wrapper", "/usr/bin/env", "--timeout-seconds", "30"])
            delivered = json.loads(output.getvalue())
        self.assertEqual(0, passed)
        self.assertEqual((0, 1), (delivered["failed_stores"], delivered["published"]))
        self.assertEqual([series.portable_quarantine_record(record)], self.remote_quarantine())
        self.assertEqual({}, publish_backlog.pending_stores(self.parent))

    def test_quarantine_outcome_never_reads_a_missing_report_as_published(self):
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/ledger"))
        import publish_backlog

        self.assertEqual({"published": 2}, publish_backlog.quarantine_outcome(
            {"raw_publication": {"quarantine": {"published": 2, "error": None}}}))
        self.assertEqual(1, publish_backlog.quarantine_outcome({"raw_publication": {}})["rc"])
        self.assertEqual(1, publish_backlog.quarantine_outcome(
            {"raw_publication": {"quarantine": {"published": 0, "error": "push refused"}}})["rc"])

    def test_scheduled_backlog_retries_failed_push_without_another_validation(self):
        sys.path.insert(0, str(mirror.TOOL_ROOT / "ci-hub/ledger"))
        import publish_backlog

        # Execute the real sweep, mirror CLI and Git transport. Only the row
        # checker is a fixture double, as in the normal-completion test above.
        bootstrap = (
            "import runpy,sys,subprocess;"
            f"sys.path[:0]=[{str(MODULE.parent)!r},{str(mirror.TOOL_ROOT / 'ci-hub/ledger')!r}];"
            "import series,publisher;"
            "series._run_shared_checker=lambda *a: subprocess.CompletedProcess(a,0,'','');"
            "publisher._series_store=lambda:series;"
            "sys.argv=sys.argv[1:];runpy.run_path(sys.argv[0],run_name='__main__')"
        )
        native_run = subprocess.run

        def sweep_child(command, **kwargs):
            if str(MODULE) in command:
                command = bounded_fixture_command(
                    [sys.executable, "-B", "-c", bootstrap, *command[1:]])
            return native_run(command, **kwargs)

        batch = self.append("completed-before-push-failure", "diverged")
        expected = batch.read_bytes()
        hook = self.remote / "hooks/pre-receive"
        hook.write_text("#!/bin/sh\necho deliberate-push-refusal >&2\nexit 1\n")
        hook.chmod(0o755)
        with mock.patch.object(publish_backlog, "STATE_ROOT", self.parent), \
             mock.patch.object(publish_backlog.subprocess, "run", side_effect=sweep_child):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                failed = publish_backlog.main(["--net-wrapper", "/usr/bin/env", "--timeout-seconds", "30"])
            failure = json.loads(output.getvalue())
            self.assertEqual(1, failed)
            self.assertEqual(1, failure["failed_stores"])
            self.assertNotEqual(0, failure["reports"][0]["rc"])
            self.assertEqual(expected, batch.read_bytes())
            self.assertEqual([], self.remote_rows())
            writer = self.parent / mirror.WRITER_RELATIVE
            refused_commit = git(writer, "rev-parse", "HEAD")
            hook.unlink()
            # No new append, validation or measured result is produced here.
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                passed = publish_backlog.main(["--net-wrapper", "/usr/bin/env", "--timeout-seconds", "30"])
            delivered = json.loads(output.getvalue())
        self.assertEqual(0, passed)
        self.assertEqual(0, delivered["failed_stores"])
        self.assertEqual(1, delivered["published"])
        self.assertEqual([row("completed-before-push-failure", "diverged")], self.remote_rows())
        git(self.remote, "merge-base", "--is-ancestor", delivered["reports"][0]["commit"], "main")
        self.assertFalse(batch.exists())
        recovery = delivered["reports"][0]["raw_publication"]["recovered_ref"]
        self.assertEqual(f"refs/heads/recovery/cell-series-{refused_commit}", recovery)
        self.assertEqual(refused_commit, git(writer, "rev-parse", recovery))

    # --- Scorecard renderer, head order and proof independence ------------

    def set_tool(self, body: str, checkout: Path | None = None) -> str:
        """Replace a fixture scorecard renderer and commit it; return its HEAD."""
        checkout = checkout or self.tool
        tool = checkout / "ci/compat-envelope/scorecard.rs"
        tool.parent.mkdir(parents=True, exist_ok=True)
        tool.write_text("#!/usr/bin/python3\nimport os, pathlib, subprocess, sys\n" + HELP_FIXTURE
                        + "fd=int(os.environ['DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD'])\nos.fstat(fd)\n"
                        + "ledger=pathlib.Path(os.environ['DEV_HERMIT_TEST_LEDGER_ROOT'])\n"
                        + "p=ledger/'scorecard'\n" + body)
        tool.chmod(0o755)
        git(checkout, "add", "ci")
        git(checkout, "commit", "-qm", "fixture renderer")
        return git(checkout, "rev-parse", "HEAD")

    def use_head_renderer(self) -> None:
        # The renderer runs in the measured checkout; it names the head it rendered.
        self.set_tool("head=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()\n"
                      "(p/'SCORECARD.md').write_text('rendered at '+head+'\\n')\n")

    def source_commit(self, message: str, when: str, *, base: str | None = None) -> str:
        """Commit in the measured repository at WHEN, optionally on top of BASE."""
        if base is not None:
            git(self.source, "checkout", "-q", "--detach", base)
        environment = {**os.environ, "GIT_COMMITTER_DATE": when, "GIT_AUTHOR_DATE": when}
        subprocess.run(["git", "-C", str(self.source), "commit", "--allow-empty", "-qm", message],
                       check=True, env=environment)
        return git(self.source, "rev-parse", "HEAD")

    def publish_at(self, commit: str, **kwargs):
        git(self.source, "checkout", "-q", "--detach", commit)
        self.target = commit
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            report = self.publish(**kwargs)
        return report, stderr.getvalue()

    def remote_file(self, path: str) -> str | None:
        found = subprocess.run(["git", "-C", str(self.remote), "show", f"main:{path}"],
                               capture_output=True, text=True, check=False)
        return found.stdout if found.returncode == 0 else None

    def remote_latest(self) -> dict | None:
        text = self.remote_file("latest.json")
        return None if text is None else json.loads(text)

    def test_parser_leaves_renderer_to_the_measured_checkout(self):
        args = mirror.build_parser().parse_args(
            ["--parent", "/p", "--source-checkout", "/s", "--target", "0" * 40])
        self.assertIsNone(args.tool_checkout)
        explicit = mirror.build_parser().parse_args(
            ["--parent", "/p", "--source-checkout", "/s", "--target", "0" * 40, "--tool-checkout", "/t"])
        self.assertEqual(Path("/t"), explicit.tool_checkout)

    def test_measured_checkout_renders_its_own_scorecard_by_default(self):
        # https://github.com/rrnewton/dev-hermit/issues/392: the measured
        # SCORECARD.md and scorecard.rs both differ from the pinned tool.
        self.set_tool("if pathlib.Path('SCORECARD.md').read_text() != 'pinned prose\\n':\n"
                      "    sys.exit('SCORECARD.md is stale for this renderer')\n"
                      "(p/'SCORECARD.md').write_text('rendered by pinned renderer\\n')\n")
        (self.source / "SCORECARD.md").write_text("measured prose\n")
        (self.source / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "nightly-2026-07-12"\n')
        git(self.source, "add", "SCORECARD.md", "rust-toolchain.toml")
        git(self.source, "commit", "-qm", "measured prose")
        measured = self.set_tool("assert pathlib.Path('SCORECARD.md').read_text() == 'measured prose\\n'\n"
                                 "(p/'SCORECARD.md').write_text('rendered by measured renderer\\n')\n",
                                 checkout=self.source)
        cargo = self.root / "bin/cargo"
        cargo.write_text(
            "#!/usr/bin/python3\nimport os, sys\n"
            "manifest = sys.argv[sys.argv.index('--manifest-path') + 1]\n"
            f"assert manifest in {[str(self.tool / 'Cargo.toml'), str(self.source / 'Cargo.toml')]!r}, manifest\n"
            "open(os.environ['CARGO_FIXTURE_LOG'], 'a').write(manifest + '\\n')\n"
            "assert os.environ['RUSTUP_TOOLCHAIN'] == 'nightly-2026-07-12-x86_64-unknown-linux-gnu'\n")
        log = self.root / "cargo.log"
        self.enterContext(mock.patch.dict(os.environ, {"CARGO_FIXTURE_LOG": str(log)}))
        self.append("pinned-renderer")
        pinned, _ = self.publish_at(measured)
        self.assertEqual("derived-failed", pinned["state"])
        self.assertIn("SCORECARD.md is stale", pinned["scorecard"]["error"])
        self.assertEqual("legacy scorecard\n", self.remote_file("scorecard/SCORECARD.md"))
        self.append("measured-renderer")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            report = mirror.mirror(self.parent, self.source, measured, mirror.REMOTE_URL,
                                   net_wrapper="/usr/bin/env")
        self.assertEqual("published", report["state"], (report, stderr.getvalue()))
        self.assertEqual(measured, report["scorecard"]["tool_hermit_commit"])
        self.assertEqual("rendered by measured renderer\n", self.remote_file("scorecard/SCORECARD.md"))
        self.assertEqual(measured, self.remote_latest()["scorecard_tool_hermit_commit"])
        self.assertEqual(str(self.source / "Cargo.toml"), log.read_text().splitlines()[-1])

    def test_renderer_writes_outside_its_output_are_refused_before_push(self):
        # The renderer is measured code: it may write exactly SCORECARD.md and cells.json.
        self.set_tool("(p/'SCORECARD.md').write_text('rendered\\n')\n"
                      "(p/'finalized/runs').mkdir(parents=True)\n"
                      "(p/'finalized/runs/forged.json').write_text('{}\\n')\n")
        self.append("forged-path")
        report, _ = self.publish_at(self.target)
        self.assertEqual("derived-failed", report["state"])
        self.assertEqual("failed", report["scorecard"]["state"])
        self.assertIn("changed ledger paths outside its generated output", report["scorecard"]["error"])
        self.assertIn("scorecard/finalized/runs/forged.json", report["scorecard"]["error"])
        self.assertEqual("", git(self.remote, "ls-tree", "-r", "main", "--", "scorecard/finalized"))
        self.assertEqual("legacy scorecard\n", self.remote_file("scorecard/SCORECARD.md"))
        self.assertIsNone(self.remote_latest())
        self.assertEqual([row("forged-path")], self.remote_rows())

    def test_renderer_commit_in_its_ledger_copy_is_refused_before_push(self):
        # The ledger the renderer is given is a throwaway copy: a commit there,
        # or a push from it, reaches neither the writer nor the remote.
        self.set_tool("(p/'SCORECARD.md').write_text('rendered\\n')\n"
                      "subprocess.run(['git','-C',str(ledger),'commit','--allow-empty','-qm','renderer forged commit'],"
                      "check=True)\n"
                      "pushed=subprocess.run(['git','-C',str(ledger),'push','origin','HEAD:refs/heads/forged'],"
                      "capture_output=True)\n"
                      "assert pushed.returncode != 0, pushed\n")
        self.append("forged-commit")
        report, _ = self.publish_at(self.target)
        self.assertEqual("derived-failed", report["state"])
        self.assertIn("moved the HEAD of its ledger copy; nothing was copied", report["scorecard"]["error"])
        self.assertNotIn("renderer forged commit", git(self.remote, "log", "--format=%s", "--all"))
        self.assertIsNone(self.remote_latest())
        self.assertEqual([row("forged-commit")], self.remote_rows())
        writer = self.parent / mirror.WRITER_RELATIVE
        self.assertEqual(report["series_commit"], git(writer, "rev-parse", "HEAD"))
        self.assertEqual("", git(writer, "status", "--porcelain"))
        self.assertEqual([], list(writer.parent.glob(mirror.RENDER_COPY_PREFIX + "*")))

    def test_scorecard_head_only_moves_forward_along_ancestry(self):
        self.use_head_renderer()
        root = self.target
        first, _ = self.publish_at(root)
        self.assertEqual("published", first["state"], first)
        latest = self.remote_latest()
        self.assertEqual(root, latest["scorecard_hermit_commit"])
        self.assertEqual(int(git(self.source, "show", "-s", "--format=%ct", root)),
                         latest[mirror.SCORECARD_HEAD_TIME_FIELD])
        child = self.source_commit("descendant", "2030-01-01T00:00:00Z")
        advanced, _ = self.publish_at(child)
        self.assertEqual("published", advanced["scorecard"]["state"], advanced)
        self.assertEqual(child, self.remote_latest()["scorecard_hermit_commit"])
        self.assertEqual(f"rendered at {child}\n", self.remote_file("scorecard/SCORECARD.md"))
        before = {path: git(self.remote, "rev-parse", "main:" + path)
                  for path in ("latest.json", "scorecard/SCORECARD.md", "scorecard/cells.json")}
        self.append("late-older-run")
        with mock.patch.object(mirror, "build_scorecard", wraps=mirror.build_scorecard) as renderer:
            older, stderr = self.publish_at(root)
        renderer.assert_not_called()
        self.assertEqual("published", older["state"], older)
        self.assertEqual({"state": "superseded", "hermit_commit": root, "current_hermit_commit": child},
                         {key: older["scorecard"][key] for key in ("state", "hermit_commit", "current_hermit_commit")})
        self.assertIn("ancestor of the published head", older["scorecard"]["detail"])
        self.assertIn("SCORECARD SUPERSEDED", stderr)
        self.assertEqual(before, {path: git(self.remote, "rev-parse", "main:" + path) for path in before})
        self.assertIn(row("late-older-run"), self.remote_rows())
        self.assertEqual(older["commit"], git(self.remote, "rev-parse", "main"))

    def test_divergent_scorecard_heads_order_by_committer_time(self):
        self.use_head_renderer()
        root = self.target
        middle = self.source_commit("branch middle", "2030-01-01T00:00:00Z", base=root)
        older = self.source_commit("branch older", "2029-01-01T00:00:00Z", base=root)
        newer = self.source_commit("branch newer", "2031-01-01T00:00:00Z", base=root)
        tied = self.source_commit("branch tied", "2031-01-01T00:00:00Z", base=root)
        self.assertEqual("published", self.publish_at(middle)[0]["scorecard"]["state"])
        for commit, expected in ((older, "superseded"), (newer, "published"), (tied, "superseded")):
            with self.subTest(commit=commit):
                head = self.remote_latest()["scorecard_hermit_commit"]
                report, _ = self.publish_at(commit)
                self.assertEqual(expected, report["scorecard"]["state"], report)
                self.assertEqual(head if expected == "superseded" else commit,
                                 self.remote_latest()["scorecard_hermit_commit"])
                if expected == "superseded":
                    self.assertIn("divergent history", report["scorecard"]["detail"])
        self.assertEqual(f"rendered at {newer}\n", self.remote_file("scorecard/SCORECARD.md"))

    def metadata_repository(self, head: str, recorded_time=None) -> Path:
        repository = self.root / f"metadata-{head[:8]}-{recorded_time}"
        subprocess.run(["git", "init", "-q", str(repository)], check=True)
        value = {"schema": mirror.METADATA_SCHEMA, "scorecard_hermit_commit": head}
        if recorded_time is not None:
            value[mirror.SCORECARD_HEAD_TIME_FIELD] = recorded_time
        (repository / "latest.json").write_text(json.dumps(value))
        git(repository, "add", "latest.json")
        git(repository, "commit", "-qm", "metadata")
        return repository

    def test_unreadable_published_head_is_ordered_only_by_its_recorded_time(self):
        target_time = int(git(self.source, "show", "-s", "--format=%ct", self.target))
        unknown = "f" * 40
        for recorded, expected in ((target_time - 1, "advance"), (target_time, "superseded"),
                                   (target_time + 1, "superseded")):
            with self.subTest(recorded=recorded):
                order = mirror.scorecard_order(self.metadata_repository(unknown, recorded), self.source, self.target)
                self.assertEqual(expected, order["state"])
                self.assertEqual(unknown, order["current"])
        with self.assertRaisesRegex(mirror.MirrorRefused, "records no scorecard_hermit_commit_time"):
            mirror.scorecard_order(self.metadata_repository(unknown), self.source, self.target)
        # A readable head whose recorded time disagrees with the commit is refused.
        with self.assertRaisesRegex(mirror.MirrorRefused, "records commit time"):
            mirror.scorecard_order(self.metadata_repository(self.target, target_time + 5), self.source, self.target)
        # A shallow measured checkout cannot decide ancestry.
        shallow = self.root / "shallow-source"
        self.source_commit("second", "2030-01-01T00:00:00Z")
        subprocess.run(["git", "clone", "-q", "--depth", "1", f"file://{self.source}", str(shallow)], check=True)
        with self.assertRaisesRegex(mirror.MirrorRefused, "shallow"):
            mirror.scorecard_order(self.metadata_repository(self.target), shallow, git(shallow, "rev-parse", "HEAD"))

    def test_write_metadata_refuses_every_backward_or_stale_order(self):
        self.use_head_renderer()
        self.publish_at(self.target)
        child = self.source_commit("descendant", "2030-01-01T00:00:00Z")
        writer = self.parent / mirror.WRITER_RELATIVE
        snapshot = mirror._published_snapshot(writer)
        order = mirror.scorecard_order(writer, self.source, child)
        self.assertEqual("advance", order["state"])
        refused = (({**order, "state": "superseded"}, child, "may not move"),
                   (order, self.source_commit("other", "2031-01-01T00:00:00Z"), "may not move"),
                   ({**order, "current": "e" * 40}, child, "changed after it was ordered"))
        for bad, target, message in refused:
            with self.subTest(message=message, target=target), self.assertRaisesRegex(mirror.MirrorRefused, message):
                mirror.write_metadata(writer, snapshot, target, snapshot["source"]["commit"], {}, "9" * 40, bad)
        self.assertEqual("", git(writer, "status", "--porcelain"))
        mirror.write_metadata(writer, snapshot, child, snapshot["source"]["commit"], {}, "9" * 40, order)
        written = json.loads((writer / "latest.json").read_text())
        self.assertEqual(child, written["scorecard_hermit_commit"])
        self.assertEqual(order["hermit_commit_time"], written[mirror.SCORECARD_HEAD_TIME_FIELD])
        git(writer, "checkout", "--", "latest.json")

    def proof_doubles(self, prepared, encoded, calls=None):
        selected = prepared["selection"]

        def native(value, parent, results, tool_checkout, **kwargs):
            if calls is not None:
                calls.append(tool_checkout)
            value["artifacts"], value["objects"] = mirror.finalized_proof.capture_derived_cells(selected["row"], encoded)
            return git(self.tool, "rev-parse", "HEAD")

        stack = contextlib.ExitStack()
        stack.enter_context(mock.patch.object(mirror, "finalized_producer_row",
                                              return_value=mirror._row_bytes(selected["row"])))
        stack.enter_context(mock.patch.object(mirror, "prepare_portable_proof", return_value=prepared))
        stack.enter_context(mock.patch.object(mirror, "_parent_tool_commit", return_value="8" * 40))
        stack.enter_context(mock.patch.object(mirror, "_select_published_terminal",
                                              return_value=(mock.MagicMock(), selected)))
        stack.enter_context(mock.patch.object(mirror, "emit_derived_portable_proof", side_effect=native))
        return stack

    def carrier_paths(self) -> list[str]:
        listed = git(self.remote, "ls-tree", "-r", "--name-only", "main", "--", "scorecard/finalized/runs")
        return listed.splitlines()

    def test_failed_scorecard_still_publishes_the_derived_proof_and_stays_failed(self):
        # The renderer writes, then fails: its partial output is discarded with
        # its ledger copy, and only the carrier is committed beside the rows.
        self.set_tool("(p/'SCORECARD.md').write_text('partial render\\n')\nsys.exit('renderer exploded')\n")
        prepared, encoded = self.derived_fixture()
        calls = []
        self.append("proof-despite-scorecard")
        with self.proof_doubles(prepared, encoded, calls):
            report, stderr = self.publish_at(self.target, results=self.root / "results",
                                             finalized_run_id=prepared["selection"]["row"]["run_id"])
        self.assertEqual("derived-failed", report["state"], report)
        self.assertEqual("failed", report["scorecard"]["state"])
        self.assertIn("renderer exploded", report["scorecard"]["error"])
        proof = report["portable_proof"]
        self.assertEqual("published", proof["state"], proof)
        self.assertEqual([None], calls, "the derived census uses the parent's pinned provider")
        self.assertEqual(report["commit"], proof["commit"])
        self.assertEqual(report["commit"], git(self.remote, "rev-parse", "main"))
        carriers = self.carrier_paths()
        self.assertEqual(1, len(carriers))
        self.assertIn(carriers[0], proof["files"])
        carrier = json.loads(self.remote_file(carriers[0]))
        self.assertEqual(prepared["selection"]["terminal"], carrier["terminal"])
        self.assertEqual(report["series_commit"], carrier["series"]["commit"])
        self.assertIsNone(self.remote_latest())
        self.assertEqual("legacy scorecard\n", self.remote_file("scorecard/SCORECARD.md"))
        self.assertEqual([row("proof-despite-scorecard")], self.remote_rows())
        self.assertIn("SCORECARD FAILED", stderr)
        self.assertIn("FINALIZED PROOF PUBLISHED", stderr)
        writer = self.parent / mirror.WRITER_RELATIVE
        self.assertEqual("legacy scorecard\n", (writer / "scorecard/SCORECARD.md").read_text())
        self.assertEqual({}, mirror._worktree_changes(writer))

    def test_failed_scorecard_cannot_leave_files_where_the_carrier_is_written(self):
        # A forged carrier in the renderer's own copy is discarded with it.
        self.set_tool("(p/'finalized/runs').mkdir(parents=True)\n"
                      "(p/'finalized/runs/forged.json').write_text('{}\\n')\nsys.exit('renderer exploded')\n")
        prepared, encoded = self.derived_fixture()
        self.append("forged-in-copy")
        with self.proof_doubles(prepared, encoded):
            report, _ = self.publish_at(self.target, results=self.root / "results",
                                        finalized_run_id=prepared["selection"]["row"]["run_id"])
        self.assertEqual("derived-failed", report["state"])
        self.assertEqual("published", report["portable_proof"]["state"], report)
        carriers = self.carrier_paths()
        self.assertEqual(1, len(carriers))
        self.assertNotIn("scorecard/finalized/runs/forged.json", carriers)
        # One planted in the real writer (reached past the copy) blocks the carrier.
        writer = self.parent / mirror.WRITER_RELATIVE
        self.set_tool(f"w=pathlib.Path({str(writer)!r})/'scorecard/finalized/runs'\n"
                      "w.mkdir(parents=True, exist_ok=True)\n"
                      "(w/'forged.json').write_text('{}\\n')\nsys.exit('renderer exploded')\n")
        again, encoded_again = self.derived_fixture(run_id="another-run")
        again["selection"]["terminal"]["event_id"] = "another-terminal"
        self.append("forged-in-writer")
        with self.proof_doubles(again, encoded_again):
            report, stderr = self.publish_at(self.target, results=self.root / "results",
                                             finalized_run_id="another-run")
        self.assertEqual("derived-failed", report["state"])
        self.assertEqual("failed", report["portable_proof"]["state"])
        self.assertIn("under the finalized proof directory", report["portable_proof"]["error"])
        self.assertEqual(carriers, self.carrier_paths())
        self.assertIn("finalized proof failed", stderr)
        self.assertEqual({"scorecard/finalized/runs/forged.json": "??"}, mirror._worktree_changes(writer))

    def test_superseded_run_publishes_its_derived_proof_without_moving_the_head(self):
        self.use_head_renderer()
        root = self.target
        child = self.source_commit("descendant", "2030-01-01T00:00:00Z")
        self.publish_at(child)
        before = {path: git(self.remote, "rev-parse", "main:" + path)
                  for path in ("latest.json", "scorecard/SCORECARD.md")}
        self.target = root
        prepared, encoded = self.derived_fixture()
        self.append("superseded-with-proof")
        with self.proof_doubles(prepared, encoded):
            report, _ = self.publish_at(root, results=self.root / "results",
                                        finalized_run_id=prepared["selection"]["row"]["run_id"])
        self.assertEqual("published", report["state"], report)
        self.assertEqual("superseded", report["scorecard"]["state"])
        self.assertEqual("published", report["portable_proof"]["state"])
        self.assertEqual(1, len(self.carrier_paths()))
        self.assertEqual(before, {path: git(self.remote, "rev-parse", "main:" + path) for path in before})

    def test_unbound_proof_only_attaches_without_touching_any_scorecard(self):
        # No scorecard was ever published: latest.json does not exist.
        self.append("never-scored")
        self.assertEqual("published", self.publish(publish_only=True)["state"])
        prepared, encoded = self.derived_fixture(commit_anchored=True, tree_dirty=False)
        run_id = prepared["selection"]["row"]["run_id"]
        with self.proof_doubles(prepared, encoded), \
                mock.patch.object(mirror, "build_scorecard") as scorecard:
            report, stderr = self.publish_at(self.target, results=self.root / "results",
                                             finalized_run_id=run_id, portable_proof_only=True)
        scorecard.assert_not_called()
        self.assertEqual("published", report["state"], report)
        self.assertEqual({"state": "unbound", "latest_hermit_commit": None,
                          "commit": report["series_commit"], "retry_of": None}, report["existing_scorecard"])
        self.assertEqual("published", report["portable_proof"]["state"])
        self.assertEqual(report["commit"], report["portable_proof"]["commit"])
        carriers = self.carrier_paths()
        self.assertEqual(1, len(carriers))
        self.assertIsNone(self.remote_latest())
        self.assertEqual("legacy scorecard\n", self.remote_file("scorecard/SCORECARD.md"))
        self.assertIn("without a scorecard", stderr)
        published = git(self.remote, "rev-parse", "main")
        files = {path: self.remote_file(path) for path in report["portable_proof"]["files"]}

        def attach(prepared_again, encoded_again, emitted=None):
            with self.proof_doubles(prepared_again, encoded_again, emitted):
                return self.publish_at(self.target, results=self.root / "results",
                                       finalized_run_id=prepared_again["selection"]["row"]["run_id"],
                                       portable_proof_only=True)

        # An exact retry of the same terminal reproduces the published carrier
        # byte for byte and reports unchanged; nothing is pushed.
        retry, retry_stderr = attach(*self.derived_fixture(commit_anchored=True, tree_dirty=False))
        self.assertEqual("unchanged", retry["state"], retry)
        self.assertEqual("unchanged", retry["portable_proof"]["state"], retry)
        self.assertEqual(carriers[0], retry["existing_scorecard"]["retry_of"])
        self.assertEqual(published, retry["portable_proof"]["commit"])
        self.assertEqual(report["portable_proof"]["files"], retry["portable_proof"]["files"])
        self.assertEqual(published, git(self.remote, "rev-parse", "main"))
        self.assertEqual(files, {path: self.remote_file(path) for path in files})
        self.assertIn("FINALIZED PROOF UNCHANGED", retry_stderr)
        # The same terminal with different operands is refused, as is a second
        # carrier for the same run or for the same event under another run.
        changed, changed_encoded = self.derived_fixture(commit_anchored=True, tree_dirty=False)
        changed_encoded = changed_encoded.replace(b'"b' + b"b" * 63 + b'"', b'"c' + b"c" * 63 + b'"')
        refusals = (("changed operands", (changed, changed_encoded), "retry changes the same terminal or proof operands", 1),
                    ("same event", self.derived_fixture(commit_anchored=True, tree_dirty=False, run_id="another-run"),
                     "refusing a second carrier", 0))
        for label, (again, encoded_again), message, emissions in refusals:
            with self.subTest(label):
                emitted = []
                refused, _ = attach(again, encoded_again, emitted)
                self.assertEqual("derived-failed", refused["state"])
                self.assertIn(message, refused["portable_proof"]["error"])
                self.assertEqual(emissions, len(emitted))
                self.assertEqual(published, git(self.remote, "rev-parse", "main"))
                self.assertEqual(carriers, self.carrier_paths())
        other_event, other_encoded = self.derived_fixture(commit_anchored=True, tree_dirty=False)
        other_event["selection"]["terminal"]["event_id"] = "another-terminal"
        emitted = []
        refused, _ = attach(other_event, other_encoded, emitted)
        self.assertIn("refusing a second carrier for run", refused["portable_proof"]["error"])
        self.assertEqual([], emitted)
        self.assertEqual(published, git(self.remote, "rev-parse", "main"))

    def test_unbound_retry_refuses_to_complete_a_published_carrier(self):
        # A retry may only reproduce a complete published carrier; it never
        # adds a file the published carrier is missing.
        self.append("incomplete")
        self.publish(publish_only=True)
        prepared, encoded = self.derived_fixture(commit_anchored=True, tree_dirty=False)
        with self.proof_doubles(prepared, encoded):
            first, _ = self.publish_at(self.target, results=self.root / "results",
                                       finalized_run_id=prepared["selection"]["row"]["run_id"],
                                       portable_proof_only=True)
        self.assertEqual("published", first["portable_proof"]["state"], first)
        objects = [path for path in first["portable_proof"]["files"] if "/objects/" in path]
        self.assertEqual(1, len(objects))
        clone = self.root / "remove-object"
        subprocess.run(["git", "clone", "-q", str(self.remote), str(clone)], check=True)
        git(clone, "rm", "-q", objects[0])
        git(clone, "commit", "-qm", "fixture: drop the carrier object")
        git(clone, "push", "-q", "origin", "HEAD:main")
        before = git(self.remote, "rev-parse", "main")
        again, encoded_again = self.derived_fixture(commit_anchored=True, tree_dirty=False)
        with self.proof_doubles(again, encoded_again):
            refused, _ = self.publish_at(self.target, results=self.root / "results",
                                         finalized_run_id=again["selection"]["row"]["run_id"],
                                         portable_proof_only=True)
        self.assertEqual("derived-failed", refused["state"], refused)
        self.assertIn("the published carrier is incomplete", refused["portable_proof"]["error"])
        self.assertIn(objects[0], refused["portable_proof"]["error"])
        self.assertEqual(before, git(self.remote, "rev-parse", "main"))

    def test_unbound_proof_only_leaves_another_commits_scorecard_in_place(self):
        root = self.target
        self.append("scored-elsewhere")
        self.assertEqual("published", self.publish_at(root)[0]["state"])
        before = {path: git(self.remote, "rev-parse", "main:" + path)
                  for path in ("latest.json", "scorecard/SCORECARD.md", "scorecard/cells.json")}
        self.target = self.source_commit("unscored run", "2030-01-01T00:00:00Z")
        prepared, encoded = self.derived_fixture(commit_anchored=True, tree_dirty=False)
        with self.proof_doubles(prepared, encoded):
            report, _ = self.publish_at(self.target, results=self.root / "results",
                                        finalized_run_id=prepared["selection"]["row"]["run_id"],
                                        portable_proof_only=True)
        self.assertEqual("published", report["state"], report)
        self.assertEqual(root, report["existing_scorecard"]["latest_hermit_commit"])
        self.assertEqual(self.target, report["portable_proof"]["hermit_commit"])
        self.assertEqual(before, {path: git(self.remote, "rev-parse", "main:" + path) for path in before})
        self.assertEqual(1, len(self.carrier_paths()))

    def test_unbound_proof_only_requires_a_commit_anchored_clean_run(self):
        self.append("unqualified")
        self.publish(publish_only=True)
        for label, updates in (("not anchored", {"commit_anchored": None}),
                               ("anchor false", {"commit_anchored": False}),
                               ("dirty tree", {"commit_anchored": True, "tree_dirty": True}),
                               ("dirty unknown", {"commit_anchored": True, "tree_dirty": None})):
            with self.subTest(label):
                prepared, encoded = self.derived_fixture(**updates)
                emitted = []
                with self.proof_doubles(prepared, encoded, emitted):
                    report, _ = self.publish_at(self.target, results=self.root / "results",
                                                finalized_run_id=prepared["selection"]["row"]["run_id"],
                                                portable_proof_only=True)
                self.assertEqual("derived-failed", report["state"])
                self.assertIn("requires a commit-anchored run of a clean tree", report["portable_proof"]["error"])
                self.assertEqual([], emitted)
                self.assertEqual([], self.carrier_paths())


    # --- Round-2 review: https://github.com/rrnewton/dev-hermit/issues/394 ---

    def test_renderer_commits_in_the_real_writer_never_reach_the_remote(self):
        # F1: a renderer that reaches past its copy and commits in the real
        # writer must not ride underneath the derived carrier's commit, whether
        # it then fails or claims success.
        writer = self.parent / mirror.WRITER_RELATIVE
        forged_latest = json.dumps({"schema": mirror.METADATA_SCHEMA, "scorecard_hermit_commit": "f" * 40,
                                    mirror.SCORECARD_HEAD_TIME_FIELD: 4102444800}) + "\n"
        forgeries = {
            "empty commit": "",
            "forged latest.json": f"(w/'latest.json').write_text({forged_latest!r})\n",
            "forged carrier": "(w/'scorecard/finalized/runs').mkdir(parents=True, exist_ok=True)\n"
                              "(w/'scorecard/finalized/runs/forged.json').write_text('{}\\n')\n",
        }
        number = 0
        for label, forge in forgeries.items():
            for exploded in (True, False):
                number += 1
                with self.subTest(label, exploded=exploded):
                    ending = ("sys.exit('renderer exploded')\n" if exploded
                              else "(p/'SCORECARD.md').write_text('rendered\\n')\n")
                    self.set_tool(f"w=pathlib.Path({str(writer)!r})\n" + forge
                                  + "subprocess.run(['git','-C',str(w),'add','-A'],check=True)\n"
                                  + f"subprocess.run(['git','-C',str(w),'commit','--allow-empty','-qm',"
                                  + f"'renderer forged {number}'],check=True)\n" + ending)
                    prepared, encoded = self.derived_fixture(run_id=f"forging-run-{number}")
                    prepared["selection"]["terminal"]["event_id"] = f"forging-terminal-{number}"
                    self.append(f"forged-{number}")
                    with self.proof_doubles(prepared, encoded):
                        report, stderr = self.publish_at(self.target, results=self.root / "results",
                                                         finalized_run_id=f"forging-run-{number}")
                    self.assertEqual("derived-failed", report["state"], report)
                    self.assertEqual("failed", report["scorecard"]["state"])
                    if not exploded:
                        self.assertIn("the ledger writer HEAD moved while the scorecard tool ran",
                                      report["scorecard"]["error"])
                    self.assertEqual("failed", report["portable_proof"]["state"], report)
                    self.assertIn(f"is not the published series commit {report['series_commit']}",
                                  report["portable_proof"]["error"])
                    self.assertIn("writer retained for recovery", report["portable_proof"]["error"])
                    # The remote holds this run's rows and nothing else.
                    self.assertEqual(report["series_commit"], git(self.remote, "rev-parse", "main"))
                    self.assertIn(row(f"forged-{number}"), self.remote_rows())
                    self.assertNotIn("renderer forged", git(self.remote, "log", "--all", "--format=%s"))
                    self.assertIsNone(self.remote_latest())
                    self.assertEqual([], self.carrier_paths())
                    self.assertIn("SCORECARD FAILED", stderr)
                    # The forged commit is retained in the writer, directly on the series commit.
                    forged = git(writer, "rev-parse", "HEAD")
                    self.assertEqual(f"renderer forged {number}", git(writer, "log", "-1", "--format=%s", forged))
                    self.assertEqual([forged, report["series_commit"]],
                                     git(writer, "rev-list", "--parents", "-n", "1", forged).split())
                    # Operator recovery for the next subtest: drop the forged commit.
                    git(writer, "checkout", "-q", "--detach", report["series_commit"])

    def test_renderer_writes_in_the_real_writer_withhold_the_scorecard(self):
        # F1/F4: a renderer that claims success after writing past its copy
        # into the real writer's files (without committing) publishes no
        # scorecard, even where its write would be overwritten by the copy;
        # the derived carrier still publishes and the write stays in the writer.
        writer = self.parent / mirror.WRITER_RELATIVE
        legacy = self.remote_file("scorecard/SCORECARD.md")
        self.assertEqual("legacy scorecard\n", legacy)
        writes = {
            "stray file": ("stray.txt", "(w/'stray.txt').write_text('stray\\n')\n", "??"),
            "generated path": ("scorecard/SCORECARD.md",
                               "(w/'scorecard/SCORECARD.md').write_text('forged in the writer\\n')\n", "M"),
        }
        for number, (label, (path, forge, status)) in enumerate(writes.items(), start=1):
            with self.subTest(label):
                self.set_tool(f"w=pathlib.Path({str(writer)!r})\n" + forge
                              + "(p/'SCORECARD.md').write_text('rendered\\n')\n")
                prepared, encoded = self.derived_fixture(run_id=f"writing-run-{number}")
                prepared["selection"]["terminal"]["event_id"] = f"writing-terminal-{number}"
                before = self.carrier_paths()
                self.append(f"writing-{number}")
                with self.proof_doubles(prepared, encoded):
                    report, stderr = self.publish_at(self.target, results=self.root / "results",
                                                     finalized_run_id=f"writing-run-{number}")
                self.assertEqual("derived-failed", report["state"], report)
                self.assertEqual("failed", report["scorecard"]["state"])
                self.assertIn("the ledger writer changed while the scorecard tool ran; writer retained "
                              f"for recovery: {path}", report["scorecard"]["error"])
                self.assertEqual("published", report["portable_proof"]["state"], report)
                self.assertEqual(report["portable_proof"]["commit"], git(self.remote, "rev-parse", "main"))
                self.assertEqual(1, len(self.carrier_paths()) - len(before))
                # Neither the write nor the rendered scorecard reached the remote.
                self.assertIsNone(self.remote_latest())
                self.assertEqual(legacy, self.remote_file("scorecard/SCORECARD.md"))
                self.assertIsNone(self.remote_file("stray.txt"))
                self.assertIn("SCORECARD FAILED", stderr)
                self.assertEqual({path: status}, {key: value.strip() for key, value
                                                  in mirror._worktree_changes(writer).items()})
                # Operator recovery for the next subtest.
                git(writer, "checkout", "-q", "--", ".")
                git(writer, "clean", "-qfd")

    def test_commit_and_push_only_pushes_one_commit_directly_on_its_base(self):
        # F1 (second line of defence): every publication commit is checked
        # against the base it was computed from before anything is pushed.
        self.append("commit-base")
        self.assertEqual("published", self.publish(publish_only=True)["state"])
        writer = self.parent / mirror.WRITER_RELATIVE
        base = git(writer, "rev-parse", "HEAD")
        self.assertEqual(base, git(self.remote, "rev-parse", "main"))
        real_git = mirror._git

        def attempt(name, prepare=None, during_commit=None):
            (writer / name).write_text(name + "\n")
            if prepare is not None:
                prepare()

            def interfering(repo, *args, **kwargs):
                if during_commit is not None and "commit" in args:
                    during_commit()
                return real_git(repo, *args, **kwargs)

            with mock.patch.object(mirror, "_git", side_effect=interfering):
                return mirror._commit_and_push(writer, [name], "fixture publication",
                                               net_wrapper="/usr/bin/env", base=base)

        def stage(name):
            (writer / name).write_text("smuggled\n")
            git(writer, "add", name)

        refusals = (
            ("moved head", {"prepare": lambda: git(writer, "commit", "--allow-empty", "-qm", "forged underneath")},
             "is not the publication base"),
            ("extra parent", {"during_commit": lambda: git(writer, "update-ref", "HEAD", git(
                writer, "commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "raced underneath"))},
             "does not sit directly on the publication base"),
            ("extra path", {"during_commit": lambda: stage("smuggled-during-commit")},
             "changes paths other than this publication"),
            ("pre-staged path", {"prepare": lambda: stage("smuggled-before")},
             "index holds paths outside this publication"),
        )
        for label, hooks, message in refusals:
            with self.subTest(label):
                with self.assertRaisesRegex(mirror.MirrorRefused, message):
                    attempt(label.replace(" ", "-") + ".txt", **hooks)
                self.assertEqual(base, git(self.remote, "rev-parse", "main"))
                git(writer, "reset", "-q", "--hard", base)
                git(writer, "clean", "-qfd")
        commit = attempt("qualifying.txt")
        self.assertEqual(commit, git(self.remote, "rev-parse", "main"))
        self.assertEqual([commit, base], git(self.remote, "rev-list", "--parents", "-n", "1", commit).split())
        self.assertEqual("qualifying.txt",
                         git(self.remote, "diff-tree", "--no-commit-id", "--name-only", "-r", base, commit))

    def test_renderer_gets_a_ledger_copy_without_routes_credentials_or_the_wrapper(self):
        # F4: the renderer is measured code. It sees a throwaway ledger copy
        # (its origin URL intact for scorecard.rs, its push URL dead), the real
        # publication lock, no proxy or credential variables, none of the Git
        # configuration that carries the host's proxy and credential helper,
        # and is not run through the network wrapper.
        writer = self.parent / mirror.WRITER_RELATIVE
        dump = self.root / "renderer-view.json"
        self.set_tool(
            "import json\n"
            "def config(*args):\n"
            " return subprocess.run(['git','-C',str(ledger),'config',*args],"
            "capture_output=True,text=True).stdout.strip()\n"
            "lock=ledger/'.git'/'ci-hub-series-publication.lock'\n"
            "json.dump({'env': dict(os.environ), 'ledger': str(ledger), 'url': config('--get','remote.origin.url'),"
            " 'pushurl': config('--get','remote.origin.pushurl'),"
            " 'config': config('--list'), 'lock': os.path.realpath(lock),"
            " 'same_lock': os.path.samestat(os.fstat(fd), os.stat(lock))}, open("
            + repr(str(dump)) + ", 'w'))\n"
            "(p/'SCORECARD.md').write_text('rendered in a copy\\n')\n")
        # The publisher's own Git configuration names a proxy and a credential
        # helper, in its global file and through the environment. The fixture's
        # remote mapping stays, so the publisher's own fetch and push still work.
        global_config = self.root / "publisher-gitconfig"
        global_config.write_text((self.root / "gitconfig").read_text()
                                 + "[http]\n\tproxy = http://global.must-not-reach.invalid:1\n"
                                 "\tproxysslcert = /must-not-reach/x509.pem\n"
                                 "\tproxysslkey = /must-not-reach/x509.pem\n"
                                 "[credential]\n\thelper = !echo must-not-reach-the-renderer\n")
        system_config = self.root / "publisher-system-gitconfig"
        system_config.write_text("[http]\n\tproxy = http://system.must-not-reach.invalid:1\n")
        ambient = {name: "must-not-reach-the-renderer" for name in mirror.RENDERER_WITHHELD_ENV}
        ambient.update({
            "GIT_CONFIG_GLOBAL": str(global_config),
            "GIT_CONFIG_SYSTEM": str(system_config), "GIT_CONFIG_NOSYSTEM": "0",
            "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "http.proxy",
            "GIT_CONFIG_VALUE_0": "http://count.must-not-reach.invalid:1",
            "GIT_CONFIG_PARAMETERS": "'credential.helper'='!echo parameters-must-not-reach'",
        })
        log = self.root / "wrapper.log"
        wrapper = self.root / "bin/logging-wrapper"
        wrapper.write_text("#!/usr/bin/python3\nimport json, os, sys\n"
                           f"open({str(log)!r}, 'a').write(json.dumps(sys.argv[1:]) + '\\n')\n"
                           "os.execvp(sys.argv[1], sys.argv[1:])\n")
        wrapper.chmod(0o755)
        self.append("isolated-render")
        with mock.patch.dict(os.environ, ambient):
            report = mirror.mirror(self.parent, self.source, self.target, mirror.REMOTE_URL,
                                   net_wrapper=str(wrapper), tool_checkout=self.tool)
        self.assertEqual("published", report["state"], report)
        seen = json.loads(dump.read_text())
        self.assertEqual([], sorted(set(mirror.RENDERER_WITHHELD_ENV) & set(seen["env"])))
        self.assertEqual("true", seen["env"]["CARGO_NET_OFFLINE"])
        self.assertEqual("0", seen["env"]["GIT_TERMINAL_PROMPT"])
        # Git on the fleet hosts may itself add request headers on its command
        # line; no key may name a proxy or its client certificate (http.proxy,
        # http.proxysslcert, http.proxysslkey, as ~/.gitconfig on devbig014
        # sets), a client key or certificate, or a credential helper, and
        # nothing the publisher configured may appear.
        config_keys = [line.split("=", 1)[0].lower() for line in seen["config"].splitlines()]
        self.assertEqual([], [key for key in config_keys if key.startswith("credential.")
                              or key.rsplit(".", 1)[-1].startswith("proxy")
                              or key.rsplit(".", 1)[-1] in ("sslkey", "sslcert")], seen["config"])
        self.assertNotIn("must-not-reach", seen["config"])
        self.assertEqual(os.devnull, seen["env"]["GIT_CONFIG_GLOBAL"])
        self.assertEqual("1", seen["env"]["GIT_CONFIG_NOSYSTEM"])
        ledger = Path(seen["ledger"])
        self.assertNotEqual(writer.resolve(), ledger.resolve())
        self.assertTrue(ledger.parent.name.startswith(mirror.RENDER_COPY_PREFIX), ledger)
        self.assertEqual(writer.parent.resolve(), ledger.parent.parent.resolve())
        self.assertEqual(mirror.REMOTE_URL, seen["url"])
        self.assertTrue(seen["pushurl"].startswith(str(ledger)), seen["pushurl"])
        self.assertTrue(seen["same_lock"])
        common = Path(git(writer, "rev-parse", "--path-format=absolute", "--git-common-dir"))
        self.assertEqual(str((common / mirror.PUBLICATION_LOCK).resolve()), seen["lock"])
        self.assertFalse(ledger.exists(), "the copy is removed after the render")
        self.assertEqual([], list(writer.parent.glob(mirror.RENDER_COPY_PREFIX + "*")))
        invocations = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertTrue(any("push" in argv for argv in invocations), invocations)
        tool = str(self.tool / "ci/compat-envelope/scorecard.rs")
        self.assertTrue(any(tool in argv for argv in invocations), "prepare_scorecard uses the wrapper")
        self.assertEqual([], [argv for argv in invocations if tool in argv and argv[-1] != "--help"])
        self.assertEqual("rendered in a copy\n", self.remote_file("scorecard/SCORECARD.md"))
        self.assertEqual("", git(writer, "status", "--porcelain"))

    def test_stale_render_copy_from_a_killed_publisher_is_removed(self):
        writer = self.parent / mirror.WRITER_RELATIVE
        self.append("before-stale")
        self.assertEqual("published", self.publish()["state"])
        stale = writer.parent / (mirror.RENDER_COPY_PREFIX + "killed")
        (stale / "hermit_test_ledger").mkdir(parents=True)
        outside = writer.parent / "unrelated-render-state"
        outside.mkdir()
        self.append("after-stale")
        self.assertEqual("published", self.publish()["state"])
        self.assertFalse(stale.exists())
        self.assertTrue(outside.exists())

    def test_shallow_measured_checkout_still_publishes_one_derived_carrier(self):
        # F2: a head order that cannot be decided withholds the scorecard, not
        # the proof. The published head is the root; the measured checkout is
        # a depth-1 clone of a descendant, so ancestry cannot be decided.
        self.use_head_renderer()
        self.append("scored-root")
        self.assertEqual("published", self.publish_at(self.target)[0]["scorecard"]["state"])
        before = {path: git(self.remote, "rev-parse", "main:" + path)
                  for path in ("latest.json", "scorecard/SCORECARD.md", "scorecard/cells.json")}
        child = self.source_commit("descendant", "2030-01-01T00:00:00Z")
        shallow = self.root / "shallow-hermit"
        subprocess.run(["git", "clone", "-q", "--depth", "1", f"file://{self.source}", str(shallow)], check=True)
        self.assertEqual(child, git(shallow, "rev-parse", "HEAD"))
        self.source = shallow
        self.target = child
        prepared, encoded = self.derived_fixture()
        self.append("shallow-run")
        with self.proof_doubles(prepared, encoded), \
                mock.patch.object(mirror, "build_scorecard", wraps=mirror.build_scorecard) as renderer:
            report, stderr = self.publish_at(child, results=self.root / "results",
                                             finalized_run_id=prepared["selection"]["row"]["run_id"])
        renderer.assert_not_called()
        self.assertEqual("derived-failed", report["state"], report)
        self.assertEqual("failed", report["scorecard"]["state"])
        self.assertIn("scorecard head order cannot be decided: measured checkout is shallow",
                      report["scorecard"]["error"])
        self.assertEqual("published", report["portable_proof"]["state"], report)
        self.assertEqual(report["commit"], report["portable_proof"]["commit"])
        self.assertEqual(report["commit"], git(self.remote, "rev-parse", "main"))
        self.assertEqual(1, len(self.carrier_paths()))
        self.assertEqual(before, {path: git(self.remote, "rev-parse", "main:" + path) for path in before})
        self.assertIn(row("shallow-run"), self.remote_rows())
        self.assertIn("SCORECARD FAILED", stderr)
        self.assertIn("finalized proof published", stderr)

    def test_unordered_published_head_still_publishes_one_derived_carrier(self):
        # F2: latest.json names a head this checkout cannot read and records
        # no commit time (the pre-ordering format), so no order exists.
        clone = self.root / "seed-latest"
        subprocess.run(["git", "clone", "-q", str(self.remote), str(clone)], check=True)
        (clone / "latest.json").write_text(json.dumps(
            {"schema": mirror.METADATA_SCHEMA, "scorecard_hermit_commit": "e" * 40}, indent=2) + "\n")
        git(clone, "add", "latest.json")
        git(clone, "commit", "-qm", "fixture: an unreadable head with no recorded time")
        git(clone, "push", "-q", "origin", "HEAD:main")
        before = self.remote_file("latest.json")
        prepared, encoded = self.derived_fixture()
        self.append("unordered-run")
        with self.proof_doubles(prepared, encoded), \
                mock.patch.object(mirror, "build_scorecard", wraps=mirror.build_scorecard) as renderer:
            report, stderr = self.publish_at(self.target, results=self.root / "results",
                                             finalized_run_id=prepared["selection"]["row"]["run_id"])
        renderer.assert_not_called()
        self.assertEqual("derived-failed", report["state"], report)
        self.assertIn("scorecard head order cannot be decided", report["scorecard"]["error"])
        self.assertIn(f"records no {mirror.SCORECARD_HEAD_TIME_FIELD}", report["scorecard"]["error"])
        self.assertEqual("published", report["portable_proof"]["state"], report)
        self.assertEqual(report["commit"], git(self.remote, "rev-parse", "main"))
        self.assertEqual(1, len(self.carrier_paths()))
        self.assertEqual(before, self.remote_file("latest.json"))
        self.assertEqual("legacy scorecard\n", self.remote_file("scorecard/SCORECARD.md"))
        self.assertIn("SCORECARD FAILED", stderr)


def real_finalized_suite(provider_path: Path) -> unittest.TestSuite:
    """Run the expensive native transaction contract separately from doubles.

    The provider records prewarmed executable and source digests. Compilation
    happens outside this test and outside the 60-second publication lock. The
    measured repository below is small contract data, not a product checkout or
    an admitted validation. All temporary repositories remain under evidence_root.
    """
    import ledger
    import validate_rows

    provider = json.loads(provider_path.read_text())

    class RealFinalizedTransactions(unittest.TestCase):
        def checked_provider(self):
            self.assertEqual(provider["schema"], "finalized-concurrency-provider/v1")
            for name, digest in provider["source_files"].items():
                self.assertEqual(hashlib.sha256(Path(name).read_bytes()).hexdigest(), digest, name)
            for name, value in provider["executables"].items():
                path = Path(value["path"])
                self.assertTrue(path.is_absolute() and path.is_file(), name)
                self.assertTrue(os.access(path, os.X_OK), name)
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), value["sha256"], name)
            self.assertEqual(git(self.tool, "rev-parse", "HEAD"), provider["hermit_commit"])
            self.assertEqual(mirror._committed_tool_source(self.tool), provider["hermit_commit"])

        def command(self, argv, *, cwd=None, timeout=45):
            result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True,
                                    timeout=timeout, check=False)
            self.calls.append({"argv": list(map(str, argv)), "cwd": str(cwd),
                               "returncode": result.returncode,
                               "stdout": result.stdout, "stderr": result.stderr})
            self.assertEqual(result.returncode, 0, self.calls[-1])
            return result.stdout

        def setUp(self):
            self.tool = Path(provider["hermit_root"]).resolve()
            self.checked_provider()
            self.calls = []
            self.trace = []
            self.root = Path(tempfile.mkdtemp(
                prefix="finalized-transactions-", dir=provider["evidence_root"]))
            self.addCleanup(self.retain_evidence)
            self.parent = self.root / "state"
            self.source = self.parent / "hermit"
            self.remote = self.root / "remote.git"
            self.ledger = self.parent / "hermit_test_ledger"
            config = self.root / "gitconfig"
            config.write_text(
                '[user]\n name = Fixture\n email = fixture@example.invalid\n'
                '[init]\n defaultBranch = main\n'
                f'[url "{self.remote}"]\n insteadOf = {mirror.REMOTE_URL}\n')
            environment = {
                "GIT_CONFIG_GLOBAL": str(config), "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_ALLOW_PROTOCOL": "file",
                "WITH_PROXY": "/usr/bin/env", "PYTHONDONTWRITEBYTECODE": "1",
                "RUSTUP_TOOLCHAIN": provider["toolchain"], "RUSTUP_AUTO_INSTALL": "0",
                "CI_HUB_LEDGER_EVENT_CHECKER": provider["executables"]["ledger"]["path"],
            }
            self.enterContext(mock.patch.dict(os.environ, environment))
            for key in tuple(os.environ):
                if key.startswith("GIT_") and key not in environment:
                    os.environ.pop(key)
            for key in (series.LEDGER_ROOT_ENV, "DEV_HERMIT_TOOL_AUTHORITY",
                        mirror.LOCK_FD_ENV):
                os.environ.pop(key, None)
            temporary = self.root / "temporary"
            temporary.mkdir()
            os.environ["TMPDIR"] = str(temporary)
            self.enterContext(mock.patch.object(tempfile, "tempdir", str(temporary)))
            self.command(["git", "init", "--bare", "-q", str(self.remote)])
            self.command(["git", "init", "-q", str(self.parent)])
            (self.parent / ".gitmodules").write_text(
                '[submodule "hermit_test_ledger"]\n path = hermit_test_ledger\n'
                f' url = {mirror.REMOTE_URL}\n')
            (self.parent / ".gitignore").write_text("ignored/\nhermit/\nhermit_test_ledger/\n")
            self.command(["git", "init", "-q", str(self.source)])
            self.install_transport()
            self.make_source_fixture()
            self.make_ledger_fixture()
            self.assertEqual(series.shared_parent_root(self.parent), self.parent)
            self.assertEqual(series.ledger_root(self.parent), self.ledger)
            self.assertEqual(series.spool_dir(self.parent),
                             self.parent / "ignored/ci-hub/series-spool")
            self.inputs = {name: self.make_run(name, second) for name, second in
                           (("finalized-a", "00"), ("finalized-b", "01"))}
            git(self.parent, "add", ".gitmodules", ".gitignore", "ledger")
            git(self.parent, "commit", "-qm", "two failed partial finalized fixture records")
            self.parent_head = git(self.parent, "rev-parse", "HEAD")
            self.canonical_before = {
                name: mirror.finalized_producer_row(self.parent, name, self.target, self.source)
                for name in self.inputs
            }

        def write_json(self, path, value):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n")

        def install_transport(self):
            """Dispatch only exact compiler argv to real, verified native bodies."""
            directory = self.root / "bin"
            directory.mkdir()
            script_map = {
                str(self.tool / "ci/compat-envelope/scorecard.rs"):
                    provider["executables"]["scorecard"]["path"],
                str(MODULE.parent / "check-series-rows.rs"):
                    provider["executables"]["series"]["path"],
            }
            compiler = directory / "rust-script"
            compiler.write_text(
                "#!/usr/bin/python3\nimport os, sys\n"
                f"mapping = {script_map!r}\n"
                f"assert sys.argv[1:4] == ['--force', '--toolchain', {provider['toolchain']!r}]\n"
                "assert os.environ['RUSTUP_AUTO_INSTALL'] == '0'\n"
                "if sys.argv[5:] == ['--help']:\n"
                " assert 'DEV_HERMIT_TEST_LEDGER_ROOT' not in os.environ\n"
                " assert 'DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD' not in os.environ\n"
                "binary = mapping[sys.argv[4]]\n"
                "os.execv('/usr/bin/timeout', ['timeout', '--kill-after=5s', '45s', binary, *sys.argv[5:]])\n")
            compiler.chmod(0o755)
            cargo = directory / "cargo"
            expected = ["run", "--quiet", "-p", "hermit-manifest-plan", "--manifest-path",
                        str(self.tool / "Cargo.toml"), "--target-dir", str(self.parent / "target"),
                        "--", "--root", str(self.source), "--format", "matrix-json"]
            cargo.write_text(
                "#!/usr/bin/python3\nimport os, sys\n"
                f"assert sys.argv[1:] in [{expected!r}, {expected[:9] + ['--help']!r}], sys.argv\n"
                "if sys.argv[10:] == ['--help']:\n"
                " assert 'DEV_HERMIT_TEST_LEDGER_ROOT' not in os.environ\n"
                " assert 'DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD' not in os.environ\n"
                f"binary = {provider['executables']['manifest']['path']!r}\n"
                "os.execv('/usr/bin/timeout', ['timeout', '--kill-after=5s', '45s', binary, *sys.argv[10:]])\n")
            cargo.chmod(0o755)
            checker = directory / "ledger-checker"
            checker.write_text(
                "#!/usr/bin/python3\nimport os\n"
                f"binary = {provider['executables']['ledger']['path']!r}\n"
                "os.execv('/usr/bin/timeout', ['timeout', '--kill-after=5s', '45s', binary])\n")
            checker.chmod(0o755)
            os.environ["CI_HUB_LEDGER_EVENT_CHECKER"] = str(checker)
            os.environ["PATH"] = str(directory) + os.pathsep + os.environ["PATH"]
            os.environ["CARGO_TARGET_DIR"] = str(self.parent / "target")

        def make_source_fixture(self):
            """One declared cell; native plan/catalogue generation stays real."""
            self.cell = {"lane": "portable", "category": "fixture",
                         "test": "fixture/no-result", "mode": "verify", "backend": "ptrace"}
            backends = ["ptrace", "dbt", "kvm", "sabre", "liteinst"]
            modes = {}
            for mode in ("verify", "replay", "chaos", "naked", "custom"):
                enabled = ["ptrace"] if mode == "verify" else []
                all_backends = ["native"] if mode == "naked" else backends
                modes[mode] = {
                    "ci": mode == "verify", "backends_enabled": enabled,
                    "backends_disabled": {b: "Synthetic transaction fixture does not execute guests"
                                          for b in all_backends if b not in enabled},
                }
                if mode != "verify":
                    modes[mode]["ci_disabled_reason"] = "Synthetic transaction fixture has one declared cell"
            manifests = self.source / "tests/e2e/manifests"
            self.write_json(manifests / "defaults.yaml", {
                "schema": 3, "timeout_seconds": 15, "cpu_timeout_seconds": 10})
            self.write_json(manifests / "fixture.yaml", {
                "schema": 3, "bucket": "fixture", "test": [{
                    "id": self.cell["test"], "description": "No guest is executed by this contract fixture",
                    "lane": "portable", "requires": [], "occasional": False,
                    "program": "tests/fixture.sh", "observation": {
                        "status": True, "stdout": True, "stderr": True, "artifacts": []},
                    "modes": modes,
                }]})
            (self.source / "tests/fixture.sh").write_text("#!/bin/sh\nexit 75\n")
            inventory = "tests/e2e/manifests/inventory/test-files.json"
            paths = ["tests/fixture.sh", "tests/e2e/manifests/defaults.yaml",
                     "tests/e2e/manifests/fixture.yaml", inventory]
            self.write_json(self.source / inventory, {"schema": 2, "files": [
                {"path": p, "disposition": "manifest-test" if p == "tests/fixture.sh" else "support",
                 "runner": "transaction-fixture", "why": "Synthetic data, no guest execution"}
                for p in sorted(paths)]})
            self.write_json(self.source / "ci/ci-reason-baseline.json", {
                "schema": 1, "unreasoned_ci_false_cells": []})
            self.write_json(self.source / "ci/matrix-symmetry-baseline.json", {
                "schema": 1, "asymmetric_manifest_tests": [], "backend_private_guest_files": []})
            self.write_json(self.source / "ci/expected-e2e-plan.json", {"cells": [self.cell]})
            (self.source / "detcore").mkdir()
            (self.source / "detcore/fixture-input").write_text("Transaction contract data only\n")
            (self.source / "ci/compat-envelope").mkdir()
            git(self.source, "add", "tests", "ci", "detcore")
            git(self.source, "commit", "-qm", "synthetic measured input for ledger transaction")
            self.command([provider["executables"]["scorecard"]["path"], "update"], cwd=self.source)
            git(self.source, "add", "SCORECARD.md", "ci/compat-envelope/cells.json")
            git(self.source, "commit", "-qm", "native generated fixture catalogue")
            self.target = git(self.source, "rev-parse", "HEAD")
            git(self.source, "update-ref", "refs/remotes/origin/main", self.target)
            self.assertEqual(git(self.source, "status", "--porcelain"), "")

        def make_ledger_fixture(self):
            seed = self.root / "seed"
            self.command(["git", "init", "-q", str(seed)])
            catalogue = json.loads((self.source / "ci/compat-envelope/cells.json").read_text())
            self.assertEqual(catalogue["schema"], 9)
            cells = [{**cell, "observations": []} for cell in catalogue["cells"]]
            self.write_json(seed / "scorecard/cells.json", {"schema": 8, "cells": cells})
            (seed / "scorecard/SCORECARD.md").write_text("Empty transaction fixture history\n")
            (seed / "series/hermit/fixture").mkdir(parents=True)
            (seed / "series/hermit/fixture/2026-09.jsonl").write_text("")
            (seed / "retained-before.txt").write_text("Existing unrelated ledger data\n")
            git(seed, "add", "scorecard", "series", "retained-before.txt")
            git(seed, "commit", "-qm", "seed existing ledger data")
            git(seed, "remote", "add", "origin", str(self.remote))
            git(seed, "push", "-q", "origin", "main")
            self.seed = git(seed, "rev-parse", "HEAD")
            self.command(["git", "clone", "-q", mirror.REMOTE_URL, str(self.ledger)])
            self.assertEqual(git(self.ledger, "remote", "get-url", "--push", "origin"), str(self.remote))

        def make_run(self, name, second):
            report = {
                "verified": False, "bitwise_parity": False, "verdict": "no_result",
                "no_result_reason": {"kind": "not_run"}, "infrastructure_error": None,
                "comparison": None, "compared_log_messages": None, "runtime": None,
                "guest_exit_code": None, "guest_signal": None,
                "first_divergent_scheduler_turn": None, "first_divergent_virtual_nanoseconds": None,
                "first_divergent_record": None, "first_divergent_syscall": None,
                "first_divergent_left_message": None, "first_divergent_right_message": None,
            }
            report_bytes = json.dumps(report, sort_keys=True, separators=(",", ":"))
            invocation = {"argv": ["hermit", "run", "--verify", "fixture"],
                          "guest_argv": ["fixture"], "env": {"LC_ALL": "C"}, "cwd": "/repo",
                          "shell_command": "cd /repo && env LC_ALL=C hermit run --verify fixture"}
            raw = {**self.cell, **invocation, "schema": 4, "run_id": name, "attempt": 1,
                   "hermit_sha": self.target, "source_tree_dirty": False,
                   "binary_sha256": hashlib.sha256(b"synthetic unexecuted binary fixture").hexdigest(),
                   "test_sha256": hashlib.sha256(b"fixture").hexdigest(),
                   "machine_shortname": "fixture", "kernel_version": "fixture-no-execution",
                   "host_capabilities": {
                       capability: {"present": False, "evidence": "Synthetic fixture — no hardware probe"}
                       for capability in ("cpuid-faulting", "kvm")},
                   "classification": "required", "outcome": "ERROR",
                   "result": None, "failure_class": "no_result", "timeout_seconds": 15,
                   "execution_cpu_timeout_seconds": 10, "execution_wall_timeout_seconds": 15,
                   "log_level": "info", "effective_args": invocation["argv"][1:], "relaxations": [],
                   "attempts": [{**invocation, "index": "1", "outcome": "ERROR",
                                 "error_kind": "incomplete-verification-evidence", "status": 75,
                                 "signal": None, "timed_out": False,
                                 "verification_report": report_bytes,
                                 "verification_report_sha256": hashlib.sha256(report_bytes.encode()).hexdigest()}]}
            results = self.parent / "ignored/results" / name
            self.write_json(results / "results.jsonl", raw)
            data = (results / "results.jsonl").read_bytes()
            stamp = f"2026-09-21T12:00:{second}Z"
            row = {"schema_version": 5, "repo": "hermit", "producer": "hermit-validate-rs",
                   "host": "fixture", "run_id": name, "record_id": name,
                   "commit": self.target, "tree_dirty": False, "commit_anchored": True,
                   "cwd": "{{WORKSPACE_ROOT}}/hermit", "started_at": stamp, "finished_at": stamp,
                   "result": "fail", "raw_result": "fail", "executed_tests": None,
                   "passed_tests": None, "gates_expected": 3, "gates_run": 1,
                   "raw_result_input_census_v1": {
                       "schema": 1, "run_id": name, "hermit_sha": self.target, "files": [{
                           "path": "results.jsonl", "bytes": len(data),
                           "sha256": hashlib.sha256(data).hexdigest(),
                           "rows": [{"line": 1, "cell": self.cell, "attempt": 1}]}]}}
            event = validate_rows._event_for_row(row, self.parent)
            path = self.parent / "ledger/hermit/fixture/2026-09.jsonl"
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("a") as stream:
                stream.write(json.dumps(event, sort_keys=True) + "\n")
            projected = series.rows_from_cell_results(
                [raw], parent=self.parent, producer="validate", run_id=name, tree=self.target,
                emitted_at=stamp, hermit_checkout=self.source)
            self.assertEqual(len(projected), 1)
            return {"results": results, "raw": raw, "series": projected, "row": row}

        def append(self, name):
            path = series.spool_dir(self.parent) / f"{name}.jsonl"
            series.append_cell_rows(self.parent, path, self.inputs[name]["series"])

        def publish(self, name):
            return mirror.mirror(self.parent, self.source, self.target, mirror.REMOTE_URL,
                                 net_wrapper="/usr/bin/env", tool_checkout=self.tool,
                                 results=self.inputs[name]["results"], finalized_run_id=name)

        def retain_evidence(self):
            self.write_json(self.root / "execution.json", {
                "provider": str(provider_path), "target": getattr(self, "target", None),
                "calls": self.calls, "lock_trace": self.trace,
                "reports": getattr(self, "reports", None),
                "scope": "synthetic schema5 failed/partial records; no admitted run or guest execution",
            })
            print(f"real finalized transaction evidence: {self.root}", file=sys.stderr)

        def test_contended_finalized_transactions_preserve_raw_rows_and_derived_history(self):
            a_at_writer = threading.Event()
            b_contended = threading.Event()
            preparations_ready = threading.Barrier(3)
            release_preparations = threading.Event()
            local = threading.local()
            real_flock, real_network, real_run = mirror.fcntl.flock, mirror._network, mirror._run
            lock_path = self.ledger / ".git" / mirror.PUBLICATION_LOCK

            def observed_flock(fd, operation):
                watched = (operation == mirror.fcntl.LOCK_EX | mirror.fcntl.LOCK_NB
                           and getattr(local, "name", None) is not None and lock_path.exists()
                           and os.fstat(fd).st_ino == lock_path.stat().st_ino
                           and os.fstat(fd).st_dev == lock_path.stat().st_dev)
                try:
                    value = real_flock(fd, operation)
                except BlockingIOError:
                    if watched:
                        self.trace.append({"run": local.name, "event": "contended", "fd": fd,
                                           "inode": os.fstat(fd).st_ino, "time": time.monotonic_ns()})
                        if local.name == "finalized-b":
                            b_contended.set()
                    raise
                if watched:
                    self.trace.append({"run": local.name, "event": "acquired", "fd": fd,
                                       "inode": os.fstat(fd).st_ino, "time": time.monotonic_ns()})
                return value

            def observed_network(command, **kwargs):
                if (command[0] == "rust-script" and command[-1] == "--help"
                        and not getattr(local, "prepared", False)):
                    local.prepared = True
                    self.assertNotIn(mirror.LOCK_FD_ENV, kwargs["environment"])
                    self.assertNotIn(series.LEDGER_ROOT_ENV, kwargs["environment"])
                    self.assertEqual(kwargs.get("pass_fds", ()), ())
                    preparations_ready.wait(15)
                    self.assertTrue(release_preparations.wait(15))
                    result = real_network(command, **kwargs)
                    # Keep the existing A-writer/B-contention opponent after
                    # both real native preparations have run outside the lock.
                    if local.name == "finalized-b":
                        self.assertTrue(a_at_writer.wait(15), "A did not reach the real writer")
                    return result
                if command[0] == "git" and any(part in ("fetch", "push") for part in command):
                    self.assertEqual(git(kwargs["cwd"], "remote", "get-url", "--push", "origin"), str(self.remote))
                    self.assertEqual(os.environ["GIT_ALLOW_PROTOCOL"], "file")
                self.assertNotIn("project-and-observe-results", command, "the renderer never gets the wrapper")
                return real_network(command, **kwargs)

            def observed_run(command, **kwargs):
                # The renderer runs directly (not through _network), in its ledger copy.
                if command[0] == "rust-script" and "project-and-observe-results" in command:
                    selected = json.loads(Path(command[command.index("--finalized-row") + 1]).read_text())
                    self.assertEqual(selected["run_id"], local.name)
                    for option, suffix in (("--snapshot", "snapshot"), ("--finalized-row", "row")):
                        content = Path(command[command.index(option) + 1]).read_bytes()
                        (self.root / f"{local.name}-{suffix}.json").write_bytes(content)
                    if local.name == "finalized-a":
                        a_at_writer.set()
                        self.assertTrue(b_contended.wait(15), "B never contended on A's actual lock")
                return real_run(command, **kwargs)

            def publish(name):
                local.name = name
                self.append(name)
                return self.publish(name)

            with mock.patch.object(mirror.fcntl, "flock", side_effect=observed_flock), \
                    mock.patch.object(mirror, "_network", side_effect=observed_network), \
                    mock.patch.object(mirror, "_run", side_effect=observed_run), \
                    ThreadPoolExecutor(max_workers=2) as pool:
                futures = [pool.submit(publish, name) for name in self.inputs]
                try:
                    preparations_ready.wait(15)
                    with mirror.publication_writer(self.parent, net_wrapper="/usr/bin/env"):
                        self.assertTrue(all(not future.done() for future in futures))
                        self.trace.append({"run": "preparation-observer",
                                           "event": "lock_available_during_both_preparations",
                                           "time": time.monotonic_ns()})
                finally:
                    release_preparations.set()
                self.reports = [future.result(timeout=90) for future in futures]
            for report in self.reports:
                self.assertEqual(report["state"], "published", report)
                self.assertEqual(report["scorecard"]["state"], "published", report)
                git(self.remote, "merge-base", "--is-ancestor", report["commit"], "main")
            acquired = {entry["run"]: entry for entry in self.trace if entry["event"] == "acquired"}
            self.assertEqual(set(acquired), set(self.inputs))
            blocked = next(entry for entry in self.trace if entry["run"] == "finalized-b"
                           and entry["event"] == "contended")
            self.assertNotEqual(acquired["finalized-a"]["fd"], blocked["fd"])
            self.assertEqual(acquired["finalized-a"]["inode"], blocked["inode"])
            self.assertLess(acquired["finalized-a"]["time"], blocked["time"])
            self.assertLess(blocked["time"], acquired["finalized-b"]["time"])
            raw_rows = [json.loads(line) for line in git(
                self.remote, "show", "main:series/hermit/fixture/2026-09.jsonl").splitlines()]
            expected = [value["series"][0] for value in self.inputs.values()]
            self.assertEqual(sorted(raw_rows, key=lambda item: item["event_id"]),
                             sorted(expected, key=lambda item: item["event_id"]))
            cells = json.loads(git(self.remote, "show", "main:scorecard/cells.json"))
            cell = next(cell for cell in cells["cells"] if all(cell[k] == v for k, v in self.cell.items()))
            observations = cell["observations"]
            invocations = [invocation for observation in observations for invocation in observation["invocations"]]
            self.assertEqual({item["run_id"] for item in invocations}, set(self.inputs))
            self.assertEqual(len(invocations), 2)
            self.assertTrue(all(item.get("result") is None for item in invocations))
            self.assertTrue(all(not item.get("canonical_comparisons") and not item.get("backend_parity_comparisons")
                                for item in observations), "no-result fixture must not earn comparison credit")
            self.assertEqual(cells["projection"]["rows_read"], 2)
            metadata = json.loads(git(self.remote, "show", "main:latest.json"))
            self.assertEqual(metadata["rows"], 2)
            self.assertEqual(metadata["source_commit"], cells["projection"]["source_commit"])
            for path, digest in metadata["scorecard"].items():
                content = subprocess.check_output(["git", "-C", str(self.remote), "show", f"main:{path}"])
                self.assertEqual(hashlib.sha256(content).hexdigest(), digest, path)
            self.assertEqual(git(self.remote, "show", "main:retained-before.txt"), "Existing unrelated ledger data")
            git(self.remote, "merge-base", "--is-ancestor", self.seed, "main")
            self.assertEqual(git(self.parent, "rev-parse", "HEAD"), self.parent_head)
            self.assertEqual(git(self.source, "status", "--porcelain"), "")
            for name in self.inputs:
                canonical = mirror.finalized_producer_row(self.parent, name, self.target, self.source)
                self.assertEqual(canonical, self.canonical_before[name])
                row = json.loads(canonical)
                self.assertEqual((row["result"], row["raw_result"], row["executed_tests"],
                                  row["gates_run"], row["gates_expected"]), ("fail", "fail", None, 1, 3))
            self.assertFalse((self.parent / "target").exists(), "native transport must reuse the existing target")
            self.assertEqual(len([line for line in git(self.ledger, "worktree", "list", "--porcelain").splitlines()
                                  if line.startswith("worktree ")]), 2)
            # An actual changed raw input must fail against the retained census,
            # leaving the two completed publications and canonical records intact.
            head = git(self.remote, "rev-parse", "main")
            raw_path = self.inputs["finalized-a"]["results"] / "results.jsonl"
            raw_path.write_bytes(raw_path.read_bytes() + b"\n")
            refused = self.publish("finalized-a")
            self.assertEqual(refused["state"], "derived-failed", refused)
            self.assertIn("census", refused["scorecard"]["error"])
            self.assertEqual(git(self.remote, "rev-parse", "main"), head)
            self.reports.append(refused)
            self.checked_provider()

    return unittest.defaultTestLoader.loadTestsFromTestCase(RealFinalizedTransactions)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--real-finalized-provider":
        result = unittest.TextTestRunner(verbosity=2).run(real_finalized_suite(Path(sys.argv[2]).resolve()))
        raise SystemExit(not result.wasSuccessful())
    unittest.main()
