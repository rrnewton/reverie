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
        network = mirror._network

        def move_after_scorecard(command, **kwargs):
            result = network(command, **kwargs)
            if str(self.tool / "ci/compat-envelope/scorecard.rs") in command and command[-1] != "--help":
                git(self.source, "commit", "--allow-empty", "-qm", "concurrent source change")
            return result

        with mock.patch.object(mirror, "_network", side_effect=move_after_scorecard):
            report = self.publish()
        self.assertEqual("derived-failed", report["state"])
        self.assertIn("source HEAD changed", report["scorecard"]["error"])
        self.assertEqual([row("head-move")], self.remote_rows())
        self.assertTrue((self.parent / mirror.WRITER_RELATIVE / "scorecard/SCORECARD.md").exists())

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
            "'cwd':sys.argv[sys.argv.index('--source-checkout')+1],'started_at':'2026-09-21T12:00:00Z'};"
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
                }
                run_registry.write_record(record_path, record)
                result = start_unit.finalize_ledger_mirror(
                    self.parent, mirror.TOOL_ROOT, record_path, record=record, run=native_run,
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
                                "started_at": "2026-09-21T12:00:00Z"}).encode()
        calls = []
        original = mirror._network

        def inspect(command, **kwargs):
            if str(self.tool / "ci/compat-envelope/scorecard.rs") in command and command[-1] != "--help":
                calls.append(command)
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
                mock.patch.object(mirror, "_network", side_effect=inspect):
            report = mirror.mirror(self.parent, self.source, self.target, mirror.REMOTE_URL,
                                   net_wrapper="/usr/bin/env", tool_checkout=self.tool,
                                   results=self.root / "results", finalized_run_id="run-1")
        self.assertEqual("published", report["state"])
        self.assertEqual(2, query.call_count)
        self.assertEqual(1, len(calls))
        self.assertFalse(Path(calls[0][calls[0].index("--finalized-row") + 1]).exists())

    def test_canonical_run_change_refuses_derived_commit_without_losing_rows(self):
        self.append("finalized-cell")
        before = git(self.remote, "rev-parse", "main:scorecard/cells.json")
        rows = [json.dumps({"started_at": "2026-09-21T12:00:00Z", "record_id": record}).encode()
                for record in ["original", "correction"]]
        with mock.patch.object(mirror, "finalized_producer_row", side_effect=rows):
            report = mirror.mirror(self.parent, self.source, self.target, mirror.REMOTE_URL,
                                   net_wrapper="/usr/bin/env", tool_checkout=self.tool,
                                   results=self.root / "results", finalized_run_id="run-1")
        self.assertEqual("derived-failed", report["state"])
        self.assertIn("changed during", report["scorecard"]["error"])
        self.assertEqual(before, git(self.remote, "rev-parse", "main:scorecard/cells.json"))
        self.assertEqual(["finalized-cell"], [row["event_id"] for row in self.remote_rows()])
        self.assertTrue(git(self.parent / mirror.WRITER_RELATIVE, "status", "--porcelain"))

    def portable_fixture(self):
        # The fast suite models canonical selection and the native writer. Exact
        # transport and real Git are exercised here; this is not schema-10
        # native or cross-host authority qualification.
        fixtures = mirror.TOOL_ROOT / "ci-hub/validate/tests/fixtures/schema10"
        selected = json.loads((fixtures / "ordinary-only-row.json").read_bytes())
        selected.update(repo="hermit", record_id="portable-fixture", commit=self.target,
                        cwd=str(self.source.resolve()), started_at="2026-09-21T12:00:00Z")
        for role, ref in mirror.finalized_proof.row_references(selected).items():
            path = self.parent / ref["path"]
            path.parent.mkdir(parents=True, exist_ok=True)
            suffix = "plan.json" if role == "plan" else role + ".jsonl"
            path.write_bytes((fixtures / ("ordinary-only-" + suffix)).read_bytes())
        source = self.parent / "ignored/ci-hub/ledger-publisher-clone"
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
            order.append("raw-publish")
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
        self.assertEqual(["parent-publish", "cell-lock", "raw-publish"], order)
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

    def test_strict_writer_failure_cannot_publish_portable_carrier(self):
        self.append("native-refusal")
        encoded = json.dumps({"started_at": "2026-09-21T12:00:00Z"}).encode()
        with mock.patch.object(mirror, "finalized_producer_row", return_value=encoded), \
                mock.patch.object(mirror, "prepare_portable_proof", return_value={"prepared": True}), \
                mock.patch.object(mirror, "build_scorecard", side_effect=mirror.MirrorRefused("strict native refusal")), \
                mock.patch.object(mirror, "finish_portable_proof") as finish:
            report = self.publish(results=self.root / "results", finalized_run_id="native-refusal")
        self.assertEqual("derived-failed", report["state"])
        finish.assert_not_called()
        self.assertEqual(["native-refusal"], [r["event_id"] for r in self.remote_rows()])
        self.assertEqual("", git(self.remote, "ls-tree", "-r", "main", "--", "scorecard/finalized"))

    def derived_fixture(self):
        _encoded, selection = self.portable_fixture()
        selected = selection["row"]
        selected.update(schema_version=5, cell_results=None, result="fail", validation_complete=False)
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
                    "objects": None, "parent_tool_commit": "8" * 40}
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
            real_flock, real_network = mirror.fcntl.flock, mirror._network
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
                if "project-and-observe-results" in command:
                    selected = json.loads(Path(command[command.index("--finalized-row") + 1]).read_text())
                    self.assertEqual(selected["run_id"], local.name)
                    for option, suffix in (("--snapshot", "snapshot"), ("--finalized-row", "row")):
                        content = Path(command[command.index(option) + 1]).read_bytes()
                        (self.root / f"{local.name}-{suffix}.json").write_bytes(content)
                    if local.name == "finalized-a":
                        a_at_writer.set()
                        self.assertTrue(b_contended.wait(15), "B never contended on A's actual lock")
                return real_network(command, **kwargs)

            def publish(name):
                local.name = name
                self.append(name)
                return self.publish(name)

            with mock.patch.object(mirror.fcntl, "flock", side_effect=observed_flock), \
                    mock.patch.object(mirror, "_network", side_effect=observed_network), \
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
