#!/usr/bin/env python3

import argparse
import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("publish_receipt.py")
SPEC = importlib.util.spec_from_file_location("publish_receipt", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class GitHubRouteTests(unittest.TestCase):
    def test_publication_uses_recorded_route_for_reads_creation_and_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            selected = root / "selected gh"
            capture = root / "calls.jsonl"
            proxy_capture = root / "proxy.txt"
            selected.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\nfrom pathlib import Path\n"
                "p=Path(os.environ['ROUTE_CAPTURE'])\n"
                "index=len(p.read_text().splitlines()) if p.exists() else 0\n"
                "body=sys.stdin.read() if '--input' in sys.argv else None\n"
                "with p.open('a') as f: f.write(json.dumps({'argv':sys.argv[1:], 'body':body})+'\\n')\n"
                "if index in (0,3): raise SystemExit(1)\n"
                "print(json.dumps({'commit' if index==4 else 'object': {'sha': ('f' if index==4 else 'e')*40}}))\n"
            )
            selected.chmod(0o755)
            proxy = root / "with-proxy"
            proxy.write_text('#!/bin/sh\nprintf "%s\\n" "$1" >> "$ROUTE_PROXY"\nexec "$@"\n')
            proxy.chmod(0o755)
            decoy = root / "gh"
            decoy.write_text("#!/bin/sh\nexit 99\n")
            decoy.chmod(0o755)
            environment = MODULE.gh_route.recorded_environment(
                {"github_executable": str(selected)},
                {"PATH": str(root), "ROUTE_CAPTURE": str(capture), "ROUTE_PROXY": str(proxy_capture)},
            )
            body = b'{"receipt":"exact bytes"}\n'
            with mock.patch.dict(os.environ, environment, clear=True):
                self.assertEqual(MODULE.publish("owner/repo", "receipts", "r.json", body), "f" * 40)
            calls = [json.loads(line) for line in capture.read_text().splitlines()]
            self.assertEqual([call["argv"] for call in calls], [
                ["api", "repos/owner/repo/git/ref/heads/receipts"],
                ["api", "repos/owner/repo/git/ref/heads/main"],
                ["api", "--method", "POST", "repos/owner/repo/git/refs", "-f", "ref=refs/heads/receipts", "-f", "sha=" + "e" * 40],
                ["api", "repos/owner/repo/contents/r.json?ref=" + "e" * 40],
                ["api", "--method", "PUT", "repos/owner/repo/contents/r.json", "--input", "-"],
            ])
            self.assertEqual(proxy_capture.read_text().splitlines(), [str(selected)] * 5)
            request = json.loads(calls[-1]["body"])
            self.assertEqual(base64.b64decode(request["content"]), body)
            self.assertEqual(request["branch"], "receipts")

    def test_missing_invalid_selection_and_missing_proxy_refuse(self):
        for selected in (None, "gh", "/nonexistent/explicit-gh"):
            with self.subTest(selected=selected), mock.patch.dict(os.environ, {}, clear=True):
                if selected is not None:
                    os.environ["FLEET_REAL_GH"] = selected
                with self.assertRaisesRegex(SystemExit, "FLEET_REAL_GH"):
                    MODULE.gh(["api", "user"])
        with mock.patch.dict(os.environ, {"FLEET_REAL_GH": sys.executable, "PATH": "/nonexistent"}, clear=True):
            with self.assertRaises(FileNotFoundError):
                MODULE.gh(["api", "user"])


def make_producer_checkout(root: Path) -> tuple[Path, str, dict[str, str]]:
    """A REAL one-commit git repo carrying Hermit producer files.

    The producer blobs are resolved with `git rev-parse <sha>:<path>`, so the
    mint-side tests use a genuine repository and a genuine commit sha rather
    than a stubbed resolver.  Deliberately no test-only override hook in
    publish_receipt.py: an env var that let a caller declare its own producer
    identity would be a forgery path straight through the guard this adds.
    """
    repo = root / "producer-checkout"
    (repo / ".github" / "workflows").mkdir(parents=True)
    (repo / "scripts").mkdir()
    (repo / "scripts" / "validate.rs").write_text("fn main() {}\n")
    (repo / ".github" / "workflows" / "ci-portable.yml").write_text("name: CI\n")
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@e",
        "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@e",
    }
    run = lambda *a: subprocess.run(  # noqa: E731
        ["git", "-C", str(repo), *a], check=True, capture_output=True, text=True, env=env
    )
    run("init", "-q")
    run("add", "-A")
    run("commit", "-qm", "producer fixture")
    sha = run("rev-parse", "HEAD").stdout.strip()
    definition = {
        relative: run("rev-parse", f"{sha}:{relative}").stdout.strip()
        for relative in ("scripts/validate.rs", ".github/workflows/ci-portable.yml")
    }
    return repo, sha, definition


class ReceiptTests(unittest.TestCase):
    SHA = "a" * 40

    def row(
        self,
        root: Path,
        *,
        weak: bool = False,
        executed: int = 12,
        host: str = "test-host",
        sha: str | None = None,
        cwd: Path | None = None,
    ):
        log = root / "validate.log"
        log.write_text("running 12 tests\ntest result: ok. 12 passed; 0 failed\n")
        return {
            "schema_version": 4,
            "started_at": "2026-08-04T12:00:00Z",
            "finished_at": ("2026-08-04T12:02:00Z" if weak else "2026-08-04T12:01:00Z"),
            "commit": sha or self.SHA,
            "profile": "full",
            "selection_mode": "full",
            "commit_anchored": True,
            "tree_dirty": False,
            "result": "pass",
            "raw_result": "pass",
            "exit_code": 0,
            "checks": 0 if weak else 2,
            "gates_run": 0 if weak else 2,
            "gates_expected": 6 if weak else 2,
            "gates": (
                []
                if weak
                else [
                    {"name": "fmt", "result": "pass", "exit_code": 0},
                    {"name": "test", "result": "pass", "exit_code": 0},
                ]
            ),
            "failures": 0,
            "executed_tests": 0 if weak else executed,
            "passed_tests": 0 if weak else executed,
            "filtered_tests": 0 if weak else 3,
            "host": host,
            "slot": "fixture-slot",
            "repo": "hermit",
            "tree": "b" * 40,
            "log_file": str(log),
            # Producer binding resolves `git rev-parse <commit>:<path>` here, so
            # a row destined for build_receipt must name a real checkout.
            **({"cwd": str(cwd)} if cwd is not None else {}),
        }

    @staticmethod
    def canonical_row(row: dict) -> bytes:
        return json.dumps(row, separators=(",", ":")).encode()

    def args(
        self,
        root: Path,
        digest: str,
        *,
        dry_run: bool = True,
        sha: str | None = None,
    ):
        return argparse.Namespace(
            repo="rrnewton/hermit",
            sha=sha or self.SHA,
            selected_receipt_sha256=digest,
            canonicalization=MODULE.RECEIPT_CANONICALIZATION,
            receipt_repo=MODULE.RECEIPT_REPO,
            receipt_branch=MODULE.RECEIPT_BRANCH,
            state_root=root,
            dry_run=dry_run,
        )

    def test_selected_record_and_artifact_bind_exact_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            repo, sha, expected = make_producer_checkout(Path(directory))
            root = Path(directory)
            row = self.row(root)
            # The producer blobs are resolved at THIS sha inside THIS checkout,
            # so the row must point at the real fixture repo (producer binding).
            row["commit"] = sha
            row["cwd"] = str(repo)
            canonical_row = self.canonical_row(row)
            digest = hashlib.sha256(canonical_row).hexdigest()
            selected = MODULE.selected_record(
                canonical_row,
                sha=sha,
                expected_digest=digest,
                canonicalization=MODULE.RECEIPT_CANONICALIZATION,
            )
            durable = MODULE.preserve_log(root / "ledger.jsonl", sha, selected)
            receipt, body, artifact_digest = MODULE.build_receipt(
                "rrnewton/hermit",
                sha,
                selected,
                durable,
                selected_digest=digest,
                canonicalization=MODULE.RECEIPT_CANONICALIZATION,
            )
            observed_report = MODULE.execute(
                self.args(root, digest, sha=sha),
                canonical_row,
            )
            self.assertEqual(receipt["ledger_record"]["executed_tests"], 12)
            self.assertEqual(receipt["commit"], sha)
            self.assertEqual(receipt["selected_receipt_identity"]["digest"], digest)
            # Producer binding is minted, and from the validated commit.
            self.assertEqual(receipt["producer"]["resolved_from"], str(repo))
            self.assertEqual(receipt["producer"]["definition"], expected)
            self.assertNotIn("valid_commits", receipt["producer"])
            self.assertEqual(len(receipt["log_sha256"]), 64)
            self.assertTrue(Path(receipt["durable_log_file"]).is_file())
            self.assertEqual(hashlib.sha256(body).hexdigest(), artifact_digest)
            observed = json.loads(observed_report["artifact_body"])["producer"]
            self.assertEqual(str(repo), observed["resolved_from"])
            self.assertEqual(expected, observed["definition"])
            self.assertEqual("legacy-selected-paths", observed["coverage_status"])
            self.assertNotIn("valid_commits", observed)

    def test_execute_preserves_evidence_under_explicit_state_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_root = root / "state"
            state_root.mkdir()
            tool_root = root / "tool"
            tool_script = tool_root / "ci-hub/validation/publish_receipt.py"
            repo, sha, expected = make_producer_checkout(root)
            row = self.row(root, sha=sha, cwd=repo)
            canonical_row = self.canonical_row(row)
            digest = hashlib.sha256(canonical_row).hexdigest()

            with (
                mock.patch.object(MODULE, "__file__", str(tool_script)),
            ):
                report = MODULE.execute(
                    self.args(state_root, digest, sha=sha), canonical_row
                )

            durable_log = Path(json.loads(report["artifact_body"])["durable_log_file"])
            self.assertTrue(
                durable_log.is_relative_to(state_root / "ignored/validation-evidence")
            )
            self.assertTrue(durable_log.is_file())
            self.assertFalse((tool_root / "ignored/validation-evidence").exists())

    def test_preserved_log_identity_includes_exact_content(self):
        """A post-run audit suffix must not collide with an immutable prefix."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            row = self.row(root)
            evidence = root / "evidence"

            prefix = MODULE.preserve_log(evidence, self.SHA, row)
            prefix_bytes = Path(row["log_file"]).read_bytes()
            Path(row["log_file"]).write_bytes(prefix_bytes + b"binding audit suffix\n")
            complete = MODULE.preserve_log(evidence, self.SHA, row)

            self.assertNotEqual(prefix, complete)
            self.assertEqual(prefix.read_bytes(), prefix_bytes)
            self.assertEqual(complete.read_bytes(), prefix_bytes + b"binding audit suffix\n")
            self.assertIn(hashlib.sha256(prefix_bytes).hexdigest(), prefix.name)
            self.assertIn(hashlib.sha256(complete.read_bytes()).hexdigest(), complete.name)

            # Re-publishing the same exact bytes is idempotent and reuses the
            # same immutable local evidence path.
            self.assertEqual(complete, MODULE.preserve_log(evidence, self.SHA, row))

    def test_a_dead_cwd_resolves_from_a_durable_repository(self):
        """THE REGRESSION, and the 58% of the ledger that predates it.

        Directive #3 made validate run in a temp checkout that is deleted after
        the run, so every new receipt's `cwd` is dead on arrival. But measured
        2026-08-08, 72 of 124 qualified rows ALREADY pointed at a directory that
        no longer existed, and only 2 of those were temp checkouts -- the rest
        were reclaimed slots. The consumer needs an object store holding the
        commit, not the directory the run happened to execute in.
        """
        with tempfile.TemporaryDirectory() as directory:
            repo, sha, expected = make_producer_checkout(Path(directory))
            row = self.row(Path(directory))
            row["commit"] = sha
            # Dead on arrival: exactly what a deleted temp checkout leaves.
            row["cwd"] = str(Path(directory) / "validate-fresh-deleted")
            durable = Path(directory) / "durable.log"
            durable.write_text("log\n")
            with mock.patch.dict(os.environ, {
                "PRODUCER_DEFINITION_REPO": str(repo),
            }):
                receipt, _, _ = MODULE.build_receipt(
                    "rrnewton/hermit", sha, row, durable,
                    selected_digest=hashlib.sha256(self.canonical_row(row)).hexdigest(),
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )
            self.assertEqual(str(repo), receipt["producer"]["resolved_from"])
            definition = receipt["producer"]["definition"]
            self.assertEqual(sorted(definition), sorted(expected))
            for blob in definition.values():
                self.assertRegex(blob, r"^[0-9a-f]{40}$")

    def test_a_live_cwd_is_still_preferred_over_the_fallback(self):
        """The recorded cwd is the most specific answer and stays first, so this
        is a fallback rather than a replacement."""
        with tempfile.TemporaryDirectory() as directory:
            repo, sha, expected = make_producer_checkout(Path(directory))
            other, _, _ = make_producer_checkout(Path(directory) / "other")
            row = self.row(Path(directory))
            row["commit"] = sha
            row["cwd"] = str(repo)
            durable = Path(directory) / "durable.log"
            durable.write_text("log\n")
            with mock.patch.dict(os.environ, {
                "PRODUCER_DEFINITION_REPO": str(other),
            }):
                receipt, _, _ = MODULE.build_receipt(
                    "rrnewton/hermit", sha, row, durable,
                    selected_digest=hashlib.sha256(self.canonical_row(row)).hexdigest(),
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )
            self.assertEqual(str(repo), receipt["producer"]["resolved_from"])

    def test_unresolvable_anywhere_is_REFUSED_not_silently_skipped(self):
        """THE NEGATIVE THAT MATTERS. A label step that quietly applies nothing
        is the fail-open shape wearing a green tick. If no repository holds the
        commit, minting must abort and say what it tried."""
        with tempfile.TemporaryDirectory() as directory:
            repo, sha, expected = make_producer_checkout(Path(directory))
            empty = Path(directory) / "empty-repo"
            empty.mkdir()
            subprocess.run(["git", "init", "-q", str(empty)], check=True)
            row = self.row(Path(directory))
            row["commit"] = sha
            row["cwd"] = str(Path(directory) / "validate-fresh-deleted")
            durable = Path(directory) / "durable.log"
            durable.write_text("log\n")
            with mock.patch.dict(os.environ, {
                "PRODUCER_DEFINITION_REPO": str(empty),
            }):
                with self.assertRaises(SystemExit) as caught:
                    MODULE.build_receipt(
                        "rrnewton/hermit", sha, row, durable,
                        selected_digest=hashlib.sha256(self.canonical_row(row)).hexdigest(),
                        canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                    )
            self.assertNotEqual(0, caught.exception.code)

    def test_receipt_carries_the_producing_definition(self):
        # Mint-side half of the producer binding: the receipt must name WHICH
        # check definition produced it, and each blob must be the one git
        # resolves at the validated commit -- not a value the producer chose.
        with tempfile.TemporaryDirectory() as directory:
            repo, sha, expected = make_producer_checkout(Path(directory))
            row = self.row(Path(directory))
            row["commit"] = sha
            row["cwd"] = str(repo)
            durable = Path(directory) / "durable.log"
            durable.write_text("log\n")
            receipt, _, _ = MODULE.build_receipt(
                "rrnewton/hermit", sha, row, durable,
                selected_digest=hashlib.sha256(self.canonical_row(row)).hexdigest(),
                canonicalization=MODULE.RECEIPT_CANONICALIZATION,
            )
            definition = receipt["producer"]["definition"]
            self.assertEqual(sorted(definition), sorted(expected))
            for relative, blob in definition.items():
                expected = subprocess.run(
                    ["git", "-C", str(repo), "rev-parse", f"{sha}:{relative}"],
                    capture_output=True, text=True, check=True,
                ).stdout.strip()
                self.assertEqual(blob, expected, relative)
                self.assertRegex(blob, r"^[0-9a-f]{40}$")

    def test_receipt_without_resolvable_producer_is_refused(self):
        # Fail closed at MINT time too: a receipt that cannot name its producer
        # must not be created, because a producer-less receipt is precisely what
        # the consumer refuses. Two ways to be unresolvable, both fatal.
        with tempfile.TemporaryDirectory() as directory:
            repo, sha, expected = make_producer_checkout(Path(directory))
            durable = Path(directory) / "durable.log"
            durable.write_text("log\n")

            no_cwd = self.row(Path(directory))
            no_cwd["commit"] = sha
            with self.assertRaises(SystemExit):
                MODULE.build_receipt(
                    "rrnewton/hermit", sha, no_cwd, durable,
                    selected_digest="0" * 64,
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )

            # cwd is a real repo, but the commit is not in it -- the blobs of a
            # commit this checkout never had cannot be vouched for.
            foreign = self.row(Path(directory))
            foreign["commit"] = "a" * 40
            foreign["cwd"] = str(repo)
            with self.assertRaises(SystemExit):
                MODULE.build_receipt(
                    "rrnewton/hermit", "a" * 40, foreign, durable,
                    selected_digest="0" * 64,
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )

    def test_run_id_binds_host_same_sha_and_started_at(self):
        # Host-in-identity (Req2): two runs of the SAME sha at the SAME started_at
        # on DIFFERENT hosts must mint DISTINCT run_ids -- before host was in the
        # identity there was no collision guard between them at all.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, sha, expected = make_producer_checkout(root)
            durable = root / "durable.log"
            durable.write_text("log\n")
            row_a = self.row(root, host="host-a")
            row_a["commit"] = sha
            row_a["cwd"] = str(repo)
            row_b = dict(row_a)
            row_b["host"] = "host-b"
            self.assertEqual(row_a["started_at"], row_b["started_at"])
            digest_a = hashlib.sha256(self.canonical_row(row_a)).hexdigest()
            digest_b = hashlib.sha256(self.canonical_row(row_b)).hexdigest()
            receipt_a, _, _ = MODULE.build_receipt(
                "rrnewton/hermit",
                sha,
                row_a,
                durable,
                selected_digest=digest_a,
                canonicalization=MODULE.RECEIPT_CANONICALIZATION,
            )
            receipt_b, _, _ = MODULE.build_receipt(
                "rrnewton/hermit",
                sha,
                row_b,
                durable,
                selected_digest=digest_b,
                canonicalization=MODULE.RECEIPT_CANONICALIZATION,
            )
            self.assertNotEqual(receipt_a["run_id"], receipt_b["run_id"])
            self.assertTrue(receipt_a["run_id"].endswith("@host-a"))
            self.assertTrue(receipt_b["run_id"].endswith("@host-b"))

    def test_hostless_row_is_refused_for_publish(self):
        # The publish guard requires host present so a receipt can never be minted
        # with an identity that omits where it was produced.
        with tempfile.TemporaryDirectory() as directory:
            row = self.row(Path(directory))
            del row["host"]
            canonical_row = self.canonical_row(row)
            with self.assertRaises(SystemExit):
                MODULE.selected_record(
                    canonical_row,
                    sha=self.SHA,
                    expected_digest=hashlib.sha256(canonical_row).hexdigest(),
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )

    def test_zero_executed_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            row = self.row(Path(directory), executed=0)
            canonical_row = self.canonical_row(row)
            with self.assertRaises(SystemExit):
                MODULE.selected_record(
                    canonical_row,
                    sha=self.SHA,
                    expected_digest=hashlib.sha256(canonical_row).hexdigest(),
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )

    def test_wrong_head_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            row = self.row(Path(directory))
            canonical_row = self.canonical_row(row)
            with self.assertRaises(SystemExit):
                MODULE.selected_record(
                    canonical_row,
                    sha="b" * 40,
                    expected_digest=hashlib.sha256(canonical_row).hexdigest(),
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )

    def test_count_capable_row_requires_coverage(self):
        with tempfile.TemporaryDirectory() as directory:
            row = self.row(Path(directory))
            # PRE-EXISTING BREAKAGE, fixed in passing: 19a219f moved the
            # count-schema boundary into the shared qualifying-receipt predicate
            # and deleted publish_receipt.COUNTS_SCHEMA, but left this reference
            # behind, so this test had been erroring (not failing) ever since.
            # Read it from the one canonical source, as that refactor intended.
            row["schema_version"] = MODULE.qualifying_receipt.active()["counts_schema"]
            row.update({
                "base_sha": "1" * 40,
                "base_tree": "2" * 40,
                "reverie_base_sha": "3" * 40,
                "reverie_base_tree": "4" * 40,
                "producer": "hermit-validate-sh",
                "admission": "ci-hub-validate-lock",
                "concurrent_validates": 0,
                "concurrency_proof": "validate_lock_owner_ancestry",
            })
            canonical_row = self.canonical_row(row)
            with self.assertRaises(SystemExit):
                MODULE.selected_record(
                    canonical_row,
                    sha=self.SHA,
                    expected_digest=hashlib.sha256(canonical_row).hexdigest(),
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )
            row["coverage"] = {
                "planned_test_nodes": 3,
                "executed_test_nodes": 3,
                "zero_executed_nodes": [],
                "absent_nodes": [],
            }
            canonical_row = self.canonical_row(row)
            self.assertEqual(
                MODULE.selected_record(
                    canonical_row,
                    sha=self.SHA,
                    expected_digest=hashlib.sha256(canonical_row).hexdigest(),
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                ),
                row,
            )

    def test_tampered_or_mismatched_selected_digest_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            row = self.row(Path(directory))
            canonical_row = self.canonical_row(row)
            digest = hashlib.sha256(canonical_row).hexdigest()
            with self.assertRaises(SystemExit):
                MODULE.selected_record(
                    canonical_row,
                    sha=self.SHA,
                    expected_digest="0" * 64,
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )
            tampered = canonical_row.replace(
                b'"executed_tests":12', b'"executed_tests":1'
            )
            with self.assertRaises(SystemExit):
                MODULE.selected_record(
                    tampered,
                    sha=self.SHA,
                    expected_digest=digest,
                    canonicalization=MODULE.RECEIPT_CANONICALIZATION,
                )

    def test_strong_plus_newer_weak_publishes_selected_strong_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, sha, expected = make_producer_checkout(root)
            strong = self.row(root, sha=sha, cwd=repo)
            weak = self.row(root, weak=True, sha=sha, cwd=repo)
            (root / "ledger.jsonl").write_text(
                json.dumps(strong) + "\n" + json.dumps(weak) + "\n"
            )
            canonical_strong = self.canonical_row(strong)
            digest = hashlib.sha256(canonical_strong).hexdigest()
            report = MODULE.execute(self.args(root, digest, sha=sha), canonical_strong)
            artifact = json.loads(report["artifact_body"])
            self.assertEqual(report["action"], "would-publish")
            self.assertEqual(report["receipt_identity_sha256"], digest)
            self.assertEqual(report["receipt_repository"], MODULE.RECEIPT_REPO)
            self.assertEqual(report["receipt_branch"], MODULE.RECEIPT_BRANCH)
            self.assertTrue(
                report["path"].endswith(f"/{report['artifact_sha256']}.json")
            )
            self.assertEqual(artifact["ledger_record"], strong)
            self.assertNotEqual(artifact["ledger_record"], weak)
            self.assertEqual(
                hashlib.sha256(report["artifact_body"].encode()).hexdigest(),
                report["artifact_sha256"],
            )

    def test_direct_execution_can_publish_but_cannot_bind_a_pr(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, sha, expected = make_producer_checkout(root)
            row = self.row(root, sha=sha, cwd=repo)
            canonical_row = self.canonical_row(row)
            digest = hashlib.sha256(canonical_row).hexdigest()
            with (
                mock.patch.object(MODULE, "publish", return_value="e" * 40) as publish,
                mock.patch.object(MODULE, "gh") as gh,
            ):
                report = MODULE.execute(
                    self.args(root, digest, dry_run=False, sha=sha), canonical_row
                )
            publish.assert_called_once()
            gh.assert_not_called()
            self.assertEqual(report["action"], "published")
            self.assertEqual(report["receipt_commit"], "e" * 40)

        source = SCRIPT.read_text()
        self.assertNotIn("qualifying_row", source)
        self.assertNotIn("read_rows", source)
        self.assertNotIn("--add-label", source)
        self.assertNotIn('"pr", "comment"', source)
        self.assertFalse(hasattr(MODULE, "bind_pr"))

    def test_existing_receipt_is_read_from_the_returned_commit(self):
        body = b'{"receipt":"same bytes"}\n'
        head = "e" * 40
        responses = [
            subprocess.CompletedProcess(
                [], 0, stdout=json.dumps({"object": {"sha": head}}), stderr=""
            ),
            subprocess.CompletedProcess(
                [],
                0,
                stdout=json.dumps({"content": base64.b64encode(body).decode()}),
                stderr="",
            ),
        ]
        with mock.patch.object(MODULE, "gh", side_effect=responses) as gh:
            self.assertEqual(
                MODULE.publish("rrnewton/dev-hermit", "validation-receipts", "receipt.json", body),
                head,
            )
        self.assertEqual(
            gh.call_args_list[1],
            mock.call(
                [
                    "api",
                    f"repos/rrnewton/dev-hermit/contents/receipt.json?ref={head}",
                ],
                check=False,
            ),
        )

    def test_new_receipt_body_is_sent_on_stdin(self):
        body = b"x" * 200_000
        head = "e" * 40
        created = "f" * 40
        responses = [
            subprocess.CompletedProcess(
                [], 0, stdout=json.dumps({"object": {"sha": head}}), stderr=""
            ),
            subprocess.CompletedProcess([], 1, stdout="", stderr="not found"),
            subprocess.CompletedProcess(
                [], 0, stdout=json.dumps({"commit": {"sha": created}}), stderr=""
            ),
        ]
        with mock.patch.object(MODULE, "gh", side_effect=responses) as gh:
            self.assertEqual(
                MODULE.publish("rrnewton/dev-hermit", "validation-receipts", "receipt.json", body),
                created,
            )
        put = gh.call_args_list[2]
        self.assertEqual(
            put.args[0],
            [
                "api",
                "--method",
                "PUT",
                "repos/rrnewton/dev-hermit/contents/receipt.json",
                "--input",
                "-",
            ],
        )
        request = json.loads(put.kwargs["input_body"])
        self.assertEqual(base64.b64decode(request["content"]), body)
        self.assertEqual(request["branch"], "validation-receipts")


class ReverieReceiptTests(unittest.TestCase):
    # Immutable canonical bytes of the real September 14 run, not a synthetic
    # row supplied with Hermit's absent base/producer fields to make it pass.
    ORIGINAL = SCRIPT.parent / "fixtures/reverie-run150.json"
    SHA = "8de90a5b93af7c08b76d973bb0e5e5ea0adc156e"
    DIGEST = "c6037137812d4d188afdf694ada914b749b6888813c73a111e600eaf4cfd3c58"
    REPO = "rrnewton/reverie"

    @staticmethod
    def hub(*args: str, body: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(SCRIPT.parent.parent / "ci-hub"), "receipt-digest", *args],
            input=body, text=True, capture_output=True,
            env={**os.environ, "CI_HUB_SKIP_SOFT_MAIN_WARNING": "1"}, timeout=180,
        )

    def select(self, body: bytes, *, repo: str = REPO):
        return MODULE.selected_record(
            body, sha=self.SHA, expected_digest=hashlib.sha256(body).hexdigest(),
            canonicalization=MODULE.RECEIPT_CANONICALIZATION, repo=repo,
        )

    def test_original_completed_run_retains_exact_canonical_bytes(self):
        body = self.ORIGINAL.read_bytes()
        self.assertEqual(len(body), 3164)
        self.assertEqual(hashlib.sha256(body).hexdigest(), self.DIGEST)
        selected = self.select(body)
        self.assertEqual(selected, json.loads(body))
        self.assertNotIn("base_sha", selected)

    def test_wrong_repository_and_noncanonical_bytes_are_refused(self):
        body = self.ORIGINAL.read_bytes()
        for repo in ("rrnewton/hermit", "attacker/reverie"):
            with self.subTest(repo=repo), self.assertRaises(SystemExit):
                self.select(body, repo=repo)
        rendered = json.dumps(json.loads(body), indent=2).encode()
        with self.assertRaisesRegex(SystemExit, "exact canonical HistoryRow bytes"):
            self.select(rendered)

    def test_unsupported_policy_producer_and_failed_count_are_refused(self):
        body = self.ORIGINAL.read_bytes()
        for old, new in (
            (b"reverie-local-validation/v3", b"reverie-local-validation/v999"),
            (b"ci-hub-reverie-validate-run", b"reverie-validate-sh"),
            (b'"passed_tests":1546', b'"passed_tests":1545'),
        ):
            with self.subTest(old=old):
                self.assertIn(old, body)
                with self.assertRaisesRegex(SystemExit, "canonical qualification"):
                    self.select(body.replace(old, new))

    @staticmethod
    def git(repo: Path, *args: str) -> str:
        result = subprocess.run(
            ["git", "-C", str(repo), *args], text=True, capture_output=True,
            env={
                **os.environ,
                "GIT_AUTHOR_NAME": "receipt-test", "GIT_AUTHOR_EMAIL": "test@example.com",
                "GIT_COMMITTER_NAME": "receipt-test", "GIT_COMMITTER_EMAIL": "test@example.com",
            },
        )
        if result.returncode:
            raise AssertionError(result.stderr)
        return result.stdout.strip()

    def fixture(self, root: Path) -> tuple[Path, str, dict]:
        repo = root / "reverie-producer"
        repo.mkdir()
        (repo / "validate.sh").write_text("#!/bin/bash\nexit 0\n")
        self.git(repo, "init", "-q")
        self.git(repo, "add", "validate.sh")
        self.git(repo, "commit", "-qm", "Reverie receipt producer fixture")
        sha = self.git(repo, "rev-parse", "HEAD")
        row = json.loads(self.ORIGINAL.read_bytes())
        row.update(commit=sha, cwd=str(repo), tree=self.git(repo, "rev-parse", "HEAD^{tree}"))
        row["validate_lock_authority"]["holder"]["target"] = sha
        log = root / "validate.log"
        log.write_text("portable receipt fixture log\n")
        row["log_file"] = str(log)
        return repo, sha, row

    def verify(
        self, root: Path, repo: Path, sha: str, receipt: dict, *, tamper_artifact: bool = False,
    ):
        body = MODULE.canonical(receipt)
        digest = hashlib.sha256(body).hexdigest()
        receipt_commit = "b" * 40
        relative = f"validation-receipts/{self.REPO}/{sha}/{digest}.json"
        path = root / "receipts" / receipt_commit / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body + (b" " if tamper_artifact else b""))
        comments = root / "comments.json"
        comments.write_text(json.dumps([[{
            "user": {"login": "rrnewton"},
            "body": "[Human]\n"
            f"<!-- locally-validated-receipt commit={receipt_commit} path={relative} sha256={digest} -->",
        }]]))
        return subprocess.run(
            [str(SCRIPT.with_name("verify_receipt.sh")), "--repo", self.REPO,
             "--sha", sha, "--comments", str(comments),
             "--fixture-receipts", str(root / "receipts"),
             "--producer-repo-checkout", str(repo)],
            text=True, capture_output=True,
            env={**os.environ, "CI_HUB_SKIP_SOFT_MAIN_WARNING": "1"}, timeout=180,
        )

    def test_publish_and_immutable_readback_bind_actual_reverie_entrypoint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, sha, row = self.fixture(root)
            canonical = self.hub("--sha", sha, "--canonical-row", body=json.dumps(row))
            self.assertEqual(canonical.returncode, 0, canonical.stderr)
            body = canonical.stdout.encode()
            args = ReceiptTests().args(root, hashlib.sha256(body).hexdigest(), sha=sha)
            args.repo = self.REPO
            # No fixture Hermit registry and no resolver mock: actual git blobs
            # are used by both minting and immutable verification.
            report = MODULE.execute(args, body)
            receipt = json.loads(report["artifact_body"])
            self.assertEqual(receipt["ledger_record"], row)
            self.assertEqual(receipt["producer"]["definition"], {
                "validate.sh": self.git(repo, "rev-parse", f"{sha}:validate.sh"),
            })
            self.assertEqual(receipt["producer"]["coverage_status"], "legacy-selected-paths")
            self.assertEqual(receipt["producer"]["paths"], ["validate.sh"])
            verified = self.verify(root, repo, sha, receipt)
            self.assertEqual(verified.returncode, 0, verified.stderr)
            for change in ("blob", "coverage", "repository", "identity"):
                mutant = copy.deepcopy(receipt)
                if change == "blob":
                    mutant["producer"]["definition"]["validate.sh"] = "a" * 40
                elif change == "coverage":
                    mutant["producer"]["coverage_status"] = "complete"
                elif change == "repository":
                    mutant["repository"] = "rrnewton/hermit"
                else:
                    mutant["selected_receipt_identity"]["digest"] = "0" * 64
                with self.subTest(change=change):
                    refused = self.verify(root, repo, sha, mutant)
                    self.assertNotEqual(refused.returncode, 0, refused.stdout)
                    self.assertIn("typed published receipt refusal", refused.stderr)
            self.assertNotEqual(
                self.verify(root, repo, sha, receipt, tamper_artifact=True).returncode, 0,
            )
            # Recompute the selected identity for semantic mutations: refusal
            # must come from the existing policy, not an incidental stale hash.
            for field, value in (
                ("producer", "reverie-validate-sh"),
                ("reverie_validation_policy", "reverie-local-validation/v999"),
                ("schema_version", 4),
                ("repo", "rrnewton/hermit"),
                ("passed_tests", 1545),
            ):
                mutant = copy.deepcopy(receipt)
                mutant["ledger_record"][field] = value
                canonical = self.hub(
                    "--sha", sha, "--canonical-row", body=json.dumps(mutant["ledger_record"]),
                )
                self.assertEqual(canonical.returncode, 0, canonical.stderr)
                mutant["selected_receipt_identity"]["digest"] = hashlib.sha256(
                    canonical.stdout.encode(),
                ).hexdigest()
                with self.subTest(field=field):
                    refused = self.verify(root, repo, sha, mutant)
                    self.assertNotEqual(refused.returncode, 0, refused.stdout)
                    self.assertIn("typed published receipt refusal", refused.stderr)

    def test_missing_or_nonfile_entrypoint_cannot_produce_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, _, row = self.fixture(root)
            self.git(repo, "rm", "validate.sh")
            self.git(repo, "commit", "-qm", "Missing producer entrypoint")
            sha = self.git(repo, "rev-parse", "HEAD")
            with self.assertRaises(SystemExit):
                MODULE.producer_definition(row, sha)
            (repo / "validate.sh").mkdir()
            (repo / "validate.sh" / "child").write_text("not an entrypoint\n")
            self.git(repo, "add", "validate.sh")
            self.git(repo, "commit", "-qm", "Directory is not a producer entrypoint")
            sha = self.git(repo, "rev-parse", "HEAD")
            with self.assertRaises(SystemExit):
                MODULE.producer_definition(row, sha)


if __name__ == "__main__":
    unittest.main()
