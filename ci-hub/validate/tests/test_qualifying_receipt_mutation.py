#!/usr/bin/env python3
"""MUTATION proof for the ONE shared qualifying-receipt predicate.

Task `one-shared-qualifying-receipt-predicate-five-consumers-bypass-the-registry`.
Several consumers used to each fail-close on their own inline schema check; the fix
is ONE datum (`ci-hub/validate/qualifying-receipt.json`) that they all READ. The
strong proof is the mutation the finding sweep used: change the shared datum and
confirm EVERY consumer's answer MOVES. Any consumer whose answer does not move is
still bypassing the registry.

This harness drives each consumer in its OWN process so the
`QUALIFYING_RECEIPT_PREDICATE` env override (never the live file) is read fresh --
the predicate is cached process-wide, so an in-process import would leak the live
value across scenarios. Shared run-predicate scenarios assert panel unanimity;
the producer-map case separately asserts the established local-run versus
immutable-receipt policy:

  * POSITIVE (live): a genuine schema-5 full green qualifies in EVERY consumer.
  * MUTATION (executed_tests_min 1 -> 999999): that same green is REJECTED by
    EVERY consumer -> all panel legs move together on one datum edit.
  * COVERAGE negative (live): the ee303899 shape (executed=427 but 15 absent
    nodes) is REFUSED by EVERY consumer -> the per-node coverage clause is
    present in all of them (the DRIFT-4 gap query.py used to have).
  * COVERAGE-SCHEMA negative (live): a schema-4 row carrying a non-null
    coverage object is REFUSED by EVERY consumer; grandfathering cannot launder
    evidence its schema predates.
  * ZERO-exec negative (live): executed_tests==0 is refused by EVERY consumer.

Consumers driven (nine certifier sites, eight panel legs, three languages):
  - Rust `validate_status.rs`   via `ci-hub validate-status --sha S`, with the
    canonical ledger adapter pointed at an isolated test root.
  - Rust `history_queries.rs`   routes through the SAME `assess()` ->
    `is_clean_full_pass` -> `row_qualifies` as the leg above; one predicate path,
    so the Rust leg covers both (its own unit tests exercise history_queries).
  - Python `query.py`           via its `_row_full_pass`.
  - Python `publish_receipt.py` via `selected_record` shared-policy consistency.
  - Python `anchor_select.py`   via real ledger/ancestry anchor selection.
  - Python `finalize_receipt.py` via its schema-5 idempotency consumer.
  - Python `main_soft_green.py` via its canonical exact-main state consumer.
  - Rust `receipt-digest --require-canonical-qualifying`, the current landing
    receipt boundary.
  - bash/jq `verify_receipt.sh` via its receipt gate (`--fixture-receipts`).
"""

from __future__ import annotations

import copy
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

CI_HUB = Path(__file__).resolve().parents[2]
REPO_ROOT = CI_HUB.parent
LIVE_PREDICATE = CI_HUB / "validate" / "qualifying-receipt.json"
PRODUCER_REGISTRY = CI_HUB / "validate" / "producer-definition.json"
PRODUCER_REPO = REPO_ROOT / "hermit"
CI_HUB_RS = CI_HUB / "ci-hub.rs"
LEDGER_DIR = CI_HUB / "ledger"
QUERY_PY = CI_HUB / "history" / "query.py"
PUBLISH_PY = CI_HUB / "validation" / "publish_receipt.py"
ANCHOR_PY = CI_HUB / "validate" / "anchor_select.py"
FINALIZE_PY = CI_HUB / "validate" / "finalize_receipt.py"
MAIN_SOFT_PY = CI_HUB / "landing" / "main_soft_green.py"
VERIFY_SH = CI_HUB / "validation" / "verify_receipt.sh"

sys.path.insert(0, str(CI_HUB))
sys.path.insert(0, str(LEDGER_DIR))
import qualifying_receipt  # noqa: E402
import validate_rows  # noqa: E402

SHA = json.loads(PRODUCER_REGISTRY.read_text())["registered_at"]
RECEIPT_COMMIT = "b" * 40  # the parent commit a receipt was committed at
REPO = "rrnewton/hermit"
_MISSING = object()


def _green_row(sha: str) -> dict:
    """A genuine schema-5 clean full-coverage PASS carrying counts + coverage."""
    return {
        "commit": sha,
        "commit_anchored": True,
        "tree_dirty": False,
        "selection_mode": "full",
        "profile": "full",
        "result": "pass",
        "raw_result": "pass",
        "exit_code": 0,
        "failures": 0,
        "executed_tests": 740,
        "passed_tests": 740,
        "filtered_tests": 3,
        "schema_version": 5,
        "repo": "hermit",
        "tree": "b" * 40,
        "started_at": "2026-08-05T00:00:00Z",
        "finished_at": "2026-08-05T00:10:00Z",
        "host": "test-host",
        "slot": "fixture-slot",
        "log_file": "/tmp/qrp-fixture.log",
        "cwd": str(PRODUCER_REPO),
        "checks": 2,
        "gates_run": 2,
        "gates_expected": 2,
        "gates": [
            {"name": "fmt", "result": "pass", "exit_code": 0},
            {"name": "test", "result": "pass", "exit_code": 0},
        ],
        "coverage": {
            "planned_test_nodes": 4,
            "executed_test_nodes": 4,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        },
        "producer": "hermit-validate-sh",
        "admission": "ci-hub-validate-lock",
        "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
        "base_sha": "1" * 40,
        "base_tree": "2" * 40,
        "reverie_base_sha": "3" * 40,
        "reverie_base_tree": "4" * 40,
    }


def _forged_unadmitted_row(sha: str) -> dict:
    """The audit's exact bypass shape: authoritative-looking but unadmitted."""
    row = _green_row(sha)
    row.update(
        producer="unregistered-forged-writer",
        admission="none",
        concurrent_validates=None,
        concurrency_proof=None,
    )
    return row


def _grandfathered_schema4_row(sha: str) -> dict:
    """A counted schema-4 green that predates per-node/admission evidence."""
    row = _green_row(sha)
    row["schema_version"] = 4
    row.pop("coverage")
    row.pop("producer")
    row.pop("admission")
    row.pop("concurrent_validates")
    row.pop("concurrency_proof")
    return row


def _schema4_with_coverage_row(sha: str) -> dict:
    """Invalid cross-version claim: schema 4 carrying schema-5 coverage."""
    row = _grandfathered_schema4_row(sha)
    row["coverage"] = _green_row(sha)["coverage"]
    return row


def _ee303899_row(sha: str) -> dict:
    """Coverage-incomplete PASS: healthy executed count, 15 absent nodes."""
    row = _green_row(sha)
    row["executed_tests"] = 427
    row["passed_tests"] = 427
    row["filtered_tests"] = 0
    row["coverage"] = {
        "planned_test_nodes": 19,
        "executed_test_nodes": 4,
        "zero_executed_nodes": [],
        "absent_nodes": [f"node-{i}" for i in range(15)],
    }
    return row


def _incomplete_outer_graph_row(sha: str) -> dict:
    """A pass-shaped row that executed only 75 of its 76 planned outer nodes."""
    row = _green_row(sha)
    row["checks"] = 75
    row["gates_run"] = 75
    row["gates_expected"] = 76
    row["gates"] = [
        {"name": f"node-{index}", "result": "pass", "exit_code": 0}
        for index in range(75)
    ]
    return row


def _zero_exec_row(sha: str) -> dict:
    """A demonstrated zero-test run: never a full green at any schema."""
    row = _green_row(sha)
    row["executed_tests"] = 0
    row["passed_tests"] = 0
    return row


def _schema6_cell_row(
    sha: str,
    state: str,
    *,
    mode: str = "verify",
    virtualize_time: bool = True,
) -> dict:
    """A schema-6 row with exactly one identified cell and one closed verdict."""
    row = _green_row(sha)
    row["schema_version"] = 6
    row["record_id"] = "schema6-test"
    row["run_id"] = "schema6-test"
    identity = {
        "lane": "strict",
        "category": "record-replay",
        "test": "hello",
        "mode": mode,
        "backend": "ptrace",
    }
    if state in {"performs-no-comparison-by-design", "unavailable-with-reason"}:
        verdict = {
            "state": state,
            "comparison_tier": "declared-but-unverifiable",
            "reason": "exit-status gate" if state.startswith("performs") else "raw no_result: comparator output absent",
        }
    else:
        verdict = {
            "state": state,
            "comparison_tier": "canonical-bitwise",
            "comparison": {
                "strictness": "canonical",
                "display_name": "BitwiseInfoV1",
                "compare_logs": True,
                "compare_io_buffers": True,
                "log_scope": "info",
                "record_envelope": "all_records_v1",
                "virtualize_time": virtualize_time,
                "strip_lines": False,
                "canonicalize_addresses": True,
                "full_trace": True,
                "exact_remainder": True,
                "stripped_prefixes": ["real-wall-clock-prefix/v1"],
                "canonicalizations": ["host-address-to-first-appearance-ordinal/v1"],
                "ignore_lines": False,
                "skip_commit": False,
                "skip_detlog": False,
            },
            "bitwise_parity": state == "compared-and-matched",
            "compared_log_messages": {"left": 1, "right": 1},
        }
    selected = [identity]
    row["cell_results"] = {
        "run_id": "schema6-test",
        "hermit_sha": sha,
        "source_tree_dirty": False,
        "selected_count": 1,
        "recorded_count": 1,
        "population_sha256": qualifying_receipt.cell_population_sha256(selected),
        "artifact": {"path": "target/cells.jsonl", "sha256": "0" * 64, "row_count": 1},
        "selected": selected,
        "cells": [{**identity, "cell_verdict": verdict}],
    }
    return row


def _schema8_reader_policy() -> dict:
    predicate = copy.deepcopy(qualifying_receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 8
    return predicate


def _schema8_cell_row(
    sha: str,
    state: str = "compared-and-matched",
    *,
    path: str = "full",
) -> dict:
    """A schema-8 row with its validate path and retained-result subset."""
    row = _schema6_cell_row(sha, state)
    row["schema_version"] = 8
    row["record_id"] = "schema8-test"
    row["run_id"] = "schema8-test"
    row["profile"] = path
    row["selection_mode"] = "full"
    row["cell_results"]["run_id"] = "schema8-test"
    row["cell_results"]["path"] = path
    row["cell_results"]["artifact"]["path"] = (
        "ignored/validate/artifacts/schema8-test/cells.jsonl"
    )
    return row


def _make_stripped(verdict: dict) -> None:
    verdict["comparison"].update(
        strictness="stripped",
        display_name="Stripped",
        compare_logs=False,
        compare_io_buffers=False,
        log_scope="deterministic",
        strip_lines=True,
        canonicalize_addresses=False,
        full_trace=False,
        exact_remainder=False,
        stripped_prefixes=[
            "real-wall-clock-prefix/v1",
            "unsafe-numeric-address-and-path-normalization/v1",
        ],
        canonicalizations=[],
    )


def _missing_coverage_list_row(sha: str, field: str) -> dict:
    """A schema-5 row that omits one condition rather than reporting []."""
    row = _green_row(sha)
    del row["coverage"][field]
    return row


def _overflow_coverage_count_row(sha: str) -> dict:
    """A Python integer that cannot deserialize into Rust's u64 authority."""
    row = _green_row(sha)
    row["coverage"]["planned_test_nodes"] = 1 << 64
    return row


def _executed_coverage_count_row(sha: str, value: object = _MISSING) -> dict:
    """Set or omit the diagnostic node count without changing any obligation."""
    row = _green_row(sha)
    if value is _MISSING:
        del row["coverage"]["executed_test_nodes"]
    else:
        row["coverage"]["executed_test_nodes"] = value
    return row


def _malformed_coverage_list_row(sha: str, field: str) -> dict:
    """A present failure list whose element cannot deserialize as a node name."""
    row = _green_row(sha)
    row["coverage"][field] = [7]
    return row


def _soft_rebase_only_row(sha: str) -> dict:
    """A fully shaped inherited receipt that the hard-only policy refuses."""
    row = _green_row(sha)
    row.update(
        validated_head_sha="c" * 40,
        inherited_from={
            "delta_kind": "rebase-only",
            "upstream_commits": 0,
            "branch_commits": 0,
            "patch_identical": True,
            "force_full_paths": [],
        },
        green_class="soft-rebase-only",
    )
    return row


def _tightened_predicate(dst: Path) -> Path:
    """A copy of the live predicate with executed_tests_min raised so far that
    the genuine green (executed=740) can no longer clear it. NEVER touches the
    live file -- mirrors the sweep's copy-override method."""
    live = json.loads(LIVE_PREDICATE.read_text())
    live["require"]["executed_tests_min"] = 999999
    dst.write_text(json.dumps(live))
    return dst


def _soft_enabled_predicate(dst: Path) -> Path:
    """A test-only policy that makes provenance typing load-bearing.

    The live hard-only policy rejects every soft row regardless of provenance,
    which would make malformed soft fixtures vacuous.  Widening a COPY lets the
    panel prove that all consumers accept well-typed soft evidence and refuse
    the same bool/non-string mutations.
    """
    live = json.loads(LIVE_PREDICATE.read_text())
    live["accepts_green_class"] = [
        "hard",
        "soft-rebase-only",
        "soft-upstream-delta",
        "soft-force-full-touched",
    ]
    dst.write_text(json.dumps(live))
    return dst


def _env(predicate: Path | None) -> dict:
    env = dict(os.environ)
    env["PRODUCER_DEFINITION_REGISTRY"] = str(PRODUCER_REGISTRY)
    env["PRODUCER_DEFINITION_REPO"] = str(PRODUCER_REPO)
    if predicate is not None:
        env["QUALIFYING_RECEIPT_PREDICATE"] = str(predicate)
    else:
        env.pop("QUALIFYING_RECEIPT_PREDICATE", None)
    return env


def _canonical_ledger_env(root: Path, predicate: Path | None) -> dict:
    """Point the real canonical adapter at one isolated fixture root.

    Production deliberately has no ``validate-status --ledger`` escape hatch:
    every consumer reads the canonical shard union.  The adapter's existing
    test-only root keeps this end-to-end CLI leg on that same code path without
    making a fixture file a second production authority.
    """
    env = _env(predicate)
    env["HERMIT_VALIDATE_STOP_TEST_MODE"] = "1"
    env["CI_HUB_VALIDATE_LEDGER_TEST_ROOT"] = str(root)
    return env


def _record_canonical_fixture(root: Path, row: dict) -> None:
    """Journal one HistoryRow through the canonical event writer."""
    validate_rows.record(row, root=root)


def _spool_without_cell_results_check(root: Path, row: dict) -> None:
    """Inject a future row while retaining the production writer refusal."""
    event = validate_rows._event_for_row(copy.deepcopy(row), root)
    validate_rows.spool(
        validate_rows.live_spool_dir(root), event["team"], event["host"], [event]
    )


class QualifyingReceiptMutationTest(unittest.TestCase):
    """One test method per scenario; each asserts the full consumer panel."""

    @classmethod
    def setUpClass(cls) -> None:
        if shutil.which("rust-script") is None:
            raise unittest.SkipTest("rust-script unavailable; Rust consumer leg cannot run")
        if shutil.which("jq") is None:
            raise unittest.SkipTest("jq unavailable; verify_receipt.sh leg cannot run")
        subprocess.run(
            ["git", "-C", str(PRODUCER_REPO), "cat-file", "-e", f"{SHA}^{{commit}}"],
            check=True,
            capture_output=True,
        )
        # Warm the rust-script cache once so per-scenario Rust calls are fast and
        # a compile failure surfaces here rather than as a spurious 'reject'.
        proc = subprocess.run(
            ["rust-script", "--force", str(CI_HUB_RS), "validate-status", "--help"],
            cwd=REPO_ROOT, capture_output=True, text=True, timeout=600,
        )
        if proc.returncode != 0:
            raise unittest.SkipTest(f"ci-hub.rs did not build: {proc.stderr[-800:]}")

    # -- individual consumer drivers: each returns True iff the consumer ACCEPTS.

    def _run_rust_status(
        self, root: Path, target_sha: str, predicate: Path | None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "rust-script",
                "--force",
                str(CI_HUB_RS),
                "validate-status",
                "--sha",
                target_sha,
                "--json",
            ],
            cwd=REPO_ROOT,
            env=_canonical_ledger_env(root, predicate),
            capture_output=True,
            text=True,
            timeout=600,
        )

    def _rust_report(self, row: dict, predicate: Path | None) -> dict:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _record_canonical_fixture(root, row)
            proc = self._run_rust_status(root, row["commit"], predicate)
        self.assertEqual(proc.returncode in (0,) or proc.stdout.strip().startswith("{"), True,
                         f"validate-status produced no report: {proc.stderr[-800:]}")
        return json.loads(proc.stdout)

    def _rust_accepts(self, row: dict, predicate: Path | None) -> bool:
        return self._rust_report(row, predicate)["qualifying_count"] >= 1

    def _query_accepts(self, row: dict, predicate: Path | None) -> bool:
        driver = (
            "import json,sys;"
            f"sys.path[:0]=[{str(CI_HUB)!r},{str(CI_HUB / 'history')!r}];"
            "import query;"
            "row=json.load(sys.stdin);"
            "print('ACCEPT' if query._row_full_pass(row) else 'REJECT')"
        )
        return self._python_accepts(driver, row, predicate)

    def _publish_accepts(self, row: dict, predicate: Path | None) -> bool:
        target_sha = row["commit"]
        driver = (
            "import hashlib,json,sys;"
            f"sys.path[:0]=[{str(CI_HUB)!r},{str(CI_HUB / 'validation')!r}];"
            "import publish_receipt;"
            "raw=sys.stdin.buffer.read();"
            "row=json.loads(raw);"
            "digest=hashlib.sha256(raw).hexdigest();"
            "\ntry:\n"
            f"    selected=publish_receipt.selected_record(raw, sha={target_sha!r}, expected_digest=digest, canonicalization=publish_receipt.RECEIPT_CANONICALIZATION); publish_receipt.producer_definition(selected, {target_sha!r}); print('ACCEPT')\n"
            "except SystemExit:\n"
            "    print('REJECT')\n"
        )
        return self._python_accepts(driver, row, predicate)

    def _anchor_accepts(self, row: dict, predicate: Path | None) -> bool:
        # Exercise the actual anchor-selection consumer rather than calling its
        # row helper directly: load a ledger, resolve a real local commit, prove
        # ancestry, and require that the row survives into the selected anchor.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            checkout = root / "checkout"
            ledger = root / "ledger.jsonl"
            source = Path(row.get("cwd") or PRODUCER_REPO)
            subprocess.run(
                ["git", "clone", "-q", "--shared", str(source), str(checkout)],
                check=True,
                capture_output=True,
                text=True,
            )
            anchor_sha = row["commit"]
            subprocess.run(
                ["git", "-C", str(checkout), "checkout", "-q", "--detach", anchor_sha],
                check=True,
                capture_output=True,
                text=True,
            )
            anchor_row = json.loads(json.dumps(row))
            anchor_row["commit"] = anchor_sha
            ledger.write_text(json.dumps(anchor_row) + "\n")
            predicate_path = predicate or LIVE_PREDICATE
            driver = (
                "import pathlib,sys;"
                f"sys.path[:0]=[{str(CI_HUB)!r},{str(ANCHOR_PY.parent)!r}];"
                "import anchor_select;"
                "report=anchor_select.select_anchor("
                f"checkout=pathlib.Path({str(checkout)!r}),"
                f"ledger_path=pathlib.Path({str(ledger)!r}),"
                f"predicate_path=pathlib.Path({str(predicate_path)!r}),"
                "target_ref='HEAD',apply_floor=False);"
                "print('ACCEPT' if report.get('anchor') else 'REJECT')"
            )
            proc = subprocess.run(
                [sys.executable, "-c", driver],
                env=_env(predicate),
                capture_output=True,
                text=True,
                timeout=60,
            )
        out = proc.stdout.strip().splitlines()
        self.assertTrue(
            out and out[-1] in ("ACCEPT", "REJECT"),
            f"anchor selection gave no verdict: rc={proc.returncode} "
            f"stdout={proc.stdout!r} stderr={proc.stderr[-800:]}",
        )
        return out[-1] == "ACCEPT"

    def _finalizer_accepts(self, row: dict, predicate: Path | None) -> bool:
        driver = (
            "import json,sys;"
            f"sys.path[:0]=[{str(CI_HUB)!r},{str(FINALIZE_PY.parent)!r}];"
            "import finalize_receipt;"
            "row=json.load(sys.stdin);"
            "print('ACCEPT' if finalize_receipt._has_satisfied_schema5(row) else 'REJECT')"
        )
        return self._python_accepts(driver, row, predicate)

    def _main_soft_accepts(self, row: dict, predicate: Path | None) -> bool:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _record_canonical_fixture(root, row)
            driver = (
                "import pathlib,sys;"
                f"sys.path[:0]=[{str(CI_HUB)!r},{str(MAIN_SOFT_PY.parent)!r}];"
                "import main_soft_green;"
                f"state,_=main_soft_green.canonical_state(pathlib.Path({str(REPO_ROOT)!r}),{row['commit']!r});"
                "print('ACCEPT' if state == 'HARD_GREEN' else 'REJECT')"
            )
            proc = subprocess.run(
                [sys.executable, "-c", driver],
                env=_canonical_ledger_env(root, predicate),
                capture_output=True,
                text=True,
                timeout=60,
            )
        out = proc.stdout.strip().splitlines()
        self.assertTrue(
            out and out[-1] in ("ACCEPT", "REJECT"),
            f"main-soft-green consumer gave no verdict: rc={proc.returncode} "
            f"stdout={proc.stdout!r} stderr={proc.stderr[-800:]}",
        )
        return out[-1] == "ACCEPT"

    def _canonical_digest_accepts(self, row: dict, predicate: Path | None) -> bool:
        proc = subprocess.run(
            [
                "rust-script",
                "--force",
                str(CI_HUB_RS),
                "receipt-digest",
                "--sha",
                row["commit"],
                "--require-canonical-qualifying",
            ],
            input=json.dumps(row),
            cwd=REPO_ROOT,
            env=_env(predicate),
            capture_output=True,
            text=True,
            timeout=600,
        )
        return proc.returncode == 0

    def _python_accepts(self, driver: str, row: dict, predicate: Path | None) -> bool:
        proc = subprocess.run(
            [sys.executable, "-c", driver],
            input=json.dumps(row), env=_env(predicate),
            capture_output=True, text=True, timeout=60,
        )
        out = proc.stdout.strip().splitlines()
        self.assertTrue(out and out[-1] in ("ACCEPT", "REJECT"),
                        f"python consumer gave no verdict: rc={proc.returncode} "
                        f"stdout={proc.stdout!r} stderr={proc.stderr[-800:]}")
        return out[-1] == "ACCEPT"

    def _verify_sh_accepts(self, row: dict, predicate: Path | None) -> bool:
        target_sha = row["commit"]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            registry = json.loads(PRODUCER_REGISTRY.read_text())
            producer = {
                "resolved_from": "/fixture/worktree",
                "definition": registry["registered"],
                "coverage_status": registry["registered_coverage_status"],
                "paths": sorted(registry["registered"]),
            }
            if "registered_valid_commits" in registry:
                producer["valid_commits"] = registry["registered_valid_commits"]
            receipt = {
                "schema_version": 1,
                "repository": REPO,
                "commit": target_sha,
                "run_id": target_sha + "@" + row["started_at"] + "@" + row["host"],
                "log_sha256": "c" * 64,
                "source_log_file": row["log_file"],
                "durable_log_file": "/durable" + row["log_file"],
                # This suite mutates the QUALIFYING-RECEIPT predicate, so the
                # producer binding must be satisfied and held constant here --
                # otherwise every row would be refused for the wrong reason and
                # the predicate leg would prove nothing. Read from the live
                # registration so this fixture follows a producer advance
                # instead of silently rotting into a false REJECT.
                "producer": producer,
                "ledger_record": row,
            }
            blob = json.dumps(receipt, sort_keys=True).encode()
            digest = hashlib.sha256(blob).hexdigest()
            rel_path = f"validation-receipts/{REPO}/{target_sha}/{digest}.json"
            receipt_file = root / RECEIPT_COMMIT / rel_path
            receipt_file.parent.mkdir(parents=True, exist_ok=True)
            receipt_file.write_bytes(blob)
            marker = (
                f"<!-- locally-validated-receipt commit={RECEIPT_COMMIT} "
                f"path={rel_path} sha256={digest} -->"
            )
            comments = [[{"user": {"login": "rrnewton"},
                          "body": "[impl agent, ci-hub]\n" + marker}]]
            comments_file = root / "comments.json"
            comments_file.write_text(json.dumps(comments))
            proc = subprocess.run(
                ["bash", str(VERIFY_SH), "--sha", target_sha, "--comments", str(comments_file),
                 "--repo", REPO, "--fixture-receipts", str(root),
                 # Keep this leg on the same pinned local authority source as
                 # the other five; ambient gh auth must not decide the panel.
                 "--producer-repo-checkout", str(PRODUCER_REPO)],
                env=_env(predicate), capture_output=True, text=True, timeout=60,
            )
        # exit 0 = a qualifying receipt was found; exit 1 = none; exit 2 = usage /
        # malformed predicate (a hard config error, not a verdict).
        self.assertIn(proc.returncode, (0, 1),
                      f"verify_receipt.sh errored: rc={proc.returncode} {proc.stderr[-800:]}")
        return proc.returncode == 0

    def _panel(self, row: dict, predicate: Path | None) -> dict[str, bool]:
        return {
            "validate_status.rs": self._rust_accepts(row, predicate),
            "query.py": self._query_accepts(row, predicate),
            "publish_receipt.py": self._publish_accepts(row, predicate),
            "anchor_select.py": self._anchor_accepts(row, predicate),
            "finalize_receipt.py": self._finalizer_accepts(row, predicate),
            "main_soft_green.py": self._main_soft_accepts(row, predicate),
            "receipt-digest canonical": self._canonical_digest_accepts(row, predicate),
            "verify_receipt.sh": self._verify_sh_accepts(row, predicate),
        }

    @staticmethod
    def _passed_count_panel(schema: int, panel: dict[str, bool]) -> dict[str, bool]:
        """Exclude the schema-5-only finalizer when exercising schema 4."""
        if schema >= 5:
            return panel
        return {
            name: accepted
            for name, accepted in panel.items()
            if name != "finalize_receipt.py"
        }

    # -- scenarios: each asserts the complete expected consumer panel.

    def test_live_accepts_genuine_green_unanimously(self) -> None:
        """POSITIVE control: a real schema-5 full green is accepted everywhere."""
        panel = self._panel(_green_row(SHA), predicate=None)
        print(
            "\nADMISSION POSITIVE: "
            f"legs={sum(panel.values())}/{len(panel)} certifier-sites=9/9 panel={panel}"
        )
        self.assertTrue(all(panel.values()),
                        f"a genuine green must be accepted by every consumer: {panel}")

    def test_live_refuses_forged_unadmitted_row_per_leg(self) -> None:
        """The audit's 6/6 accept bypass must invert independently on every leg."""
        panel = self._panel(_forged_unadmitted_row(SHA), predicate=None)
        refused = sum(not accepted for accepted in panel.values())
        print(
            "\nADMISSION NEGATIVE: "
            f"refused={refused}/{len(panel)} legs certifier-sites="
            f"{9 if refused == len(panel) else 0}/9 panel={panel}"
        )
        self.assertEqual(
            refused,
            len(panel),
            f"every consumer leg must refuse forged admission provenance: {panel}",
        )

    def test_schema4_with_nonnull_coverage_is_refused_per_leg(self) -> None:
        """A grandfathered schema cannot claim evidence its version predates."""
        row = _schema4_with_coverage_row(SHA)
        panel = self._panel(row, predicate=None)
        refused = sum(not accepted for accepted in panel.values())
        verdict = qualifying_receipt.coverage_schema_verdict(
            row, qualifying_receipt.active()
        )
        print(
            "\nCOVERAGE SCHEMA NEGATIVE: "
            f"refused={refused}/{len(panel)} legs certifier-sites="
            f"{9 if refused == len(panel) else 0}/9 status={verdict.value} "
            f"panel={panel}"
        )
        self.assertIs(
            verdict,
            qualifying_receipt.CoverageSchemaVerdict.LEGACY_CLAIMS_COVERAGE,
        )
        self.assertEqual(
            refused,
            len(panel),
            f"schema-4 plus non-null coverage must be refused everywhere: {panel}",
        )

    def test_changed_producer_map_preserves_local_and_immutable_authority(self) -> None:
        """Local run authority and immutable producer registration are distinct."""
        with tempfile.TemporaryDirectory() as tmp:
            checkout = Path(tmp) / "unregistered-producer"
            subprocess.run(
                ["git", "clone", "-q", "--shared", str(PRODUCER_REPO), str(checkout)],
                check=True,
                capture_output=True,
                text=True,
            )
            subprocess.run(
                ["git", "-C", str(checkout), "checkout", "-q", "--detach", SHA],
                check=True,
                capture_output=True,
                text=True,
            )
            # Mutate a path from the LIVE registered whole map.  `validate.sh`
            # used to be such a path, but the Rust producer cutover removed it;
            # continuing to edit that retired path made this fixture vacuous.
            registered_paths = sorted(
                json.loads(PRODUCER_REGISTRY.read_text())["registered"]
            )
            self.assertTrue(registered_paths, "producer registry has no paths")
            producer_path = registered_paths[0]
            with (checkout / producer_path).open("a") as producer:
                producer.write("\n# unregistered producer mutation\n")
            git_env = {
                **os.environ,
                "GIT_AUTHOR_NAME": "producer-mutation-test",
                "GIT_AUTHOR_EMAIL": "test@example.invalid",
                "GIT_COMMITTER_NAME": "producer-mutation-test",
                "GIT_COMMITTER_EMAIL": "test@example.invalid",
            }
            subprocess.run(
                ["git", "-C", str(checkout), "add", producer_path],
                check=True,
                env=git_env,
            )
            subprocess.run(
                ["git", "-C", str(checkout), "commit", "-qm", "unregistered producer"],
                check=True,
                env=git_env,
            )
            unregistered = subprocess.run(
                ["git", "-C", str(checkout), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            row = _green_row(unregistered)
            row["cwd"] = str(checkout)
            row["tree"] = subprocess.run(
                ["git", "-C", str(checkout), "rev-parse", "HEAD^{tree}"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            # Since 341f043979a3e5bdd2e1e827b422871716765e57, local
            # authority follows the completed run, not registered file hashes.
            # Immutable receipt publication/verification retain that registry.
            # Keep every leg explicit: neither policy may silently absorb the
            # other (see validation/test_local_producer_authority.py).
            expected = {
                "validate_status.rs": True,
                "query.py": True,
                "publish_receipt.py": False,
                "anchor_select.py": True,
                "finalize_receipt.py": True,
                "main_soft_green.py": True,
                "receipt-digest canonical": True,
                "verify_receipt.sh": False,
            }
            panel = self._panel(row, predicate=None)
            print(f"\nCHANGED PRODUCER MAP: panel={panel}")
            self.assertEqual(panel, expected)

            # Removing the file-map prerequisite never admits a failed run,
            # an unadmitted run, or an unknown writer. Exercise each separately
            # against this same real changed commit, with all eight consumers.
            mutations = {
                "failed-run": {
                    "result": "fail",
                    "raw_result": "fail",
                    "exit_code": 1,
                    "failures": 1,
                    "passed_tests": row["executed_tests"] - 1,
                },
                "unadmitted-run": {"admission": "none"},
                "unknown-writer": {"producer": "unregistered-forged-writer"},
            }
            for condition, fields in mutations.items():
                with self.subTest(condition=condition):
                    refused_row = copy.deepcopy(row)
                    refused_row.update(fields)
                    refused = self._panel(refused_row, predicate=None)
                    print(f"\nCHANGED PRODUCER MAP {condition}: panel={refused}")
                    self.assertEqual(
                        refused,
                        {name: False for name in expected},
                        f"{condition} must still be refused by every consumer",
                    )

    def test_each_admission_condition_is_load_bearing_per_leg(self) -> None:
        """Mutate one condition at a time so no composite refusal is vacuous."""
        mutations = {
            "registered-producer": ("producer", "unregistered-forged-writer"),
            "canonical-admission": ("admission", "none"),
            "bounded-concurrency": ("concurrent_validates", 6),
            "owner-ancestry": ("concurrency_proof", "self-declared"),
            "recorded-base": ("base_sha", None),
            "recorded-base-tree": ("base_tree", None),
            "recorded-reverie-base": ("reverie_base_sha", None),
            "recorded-reverie-tree": ("reverie_base_tree", None),
        }
        for condition, (field, value) in mutations.items():
            with self.subTest(condition=condition):
                row = _green_row(SHA)
                row[field] = value
                panel = self._panel(row, predicate=None)
                refused = sum(not accepted for accepted in panel.values())
                print(
                    f"\nADMISSION CLAUSE {condition}: refused={refused}/{len(panel)} "
                    f"legs certifier-sites={9 if refused == len(panel) else 0}/9 "
                    f"panel={panel}"
                )
                self.assertEqual(
                    refused,
                    len(panel),
                    f"{condition} must be load-bearing in every leg: {panel}",
                )

    def test_five_concurrent_validates_are_accepted_with_measured_proof(self) -> None:
        row = _green_row(SHA)
        row["concurrent_validates"] = 5
        row["concurrency_proof"] = (
            "validate_lock_owner_ancestry+live_flock_registry_cpu_delta"
        )
        # The removed exact-zero rule rejects this row, proving that the test
        # reaches the policy collision rather than merely observing two runs
        # that happened not to overlap.
        self.assertNotEqual(
            row["concurrent_validates"],
            qualifying_receipt.active()["admission"]["required_concurrent_validates"],
        )
        panel = self._panel(row, predicate=None)
        self.assertTrue(all(panel.values()), f"five admitted peers must qualify: {panel}")

    def test_schema4_remains_grandfathered_without_consumer_regression(self) -> None:
        """Admission tightening must not revoke counted schema-4 authority.

        ``finalize_receipt`` is intentionally false here: its consumer is an
        idempotency question named ``_has_satisfied_schema5``, not a schema-4
        landing verdict. That was its baseline before admission tightened.
        """
        row = _grandfathered_schema4_row(SHA)
        panel = self._panel(row, predicate=None)
        verdict = qualifying_receipt.admission_verdict(row, qualifying_receipt.active())
        authority = {name: accepted for name, accepted in panel.items()
                     if name != "finalize_receipt.py"}
        print(
            "\nADMISSION GRANDFATHER: "
            f"authority-accepted={sum(authority.values())}/{len(authority)} legs "
            "finalizer-schema5-idempotency=false "
            f"status={verdict.value} panel={panel}"
        )
        self.assertIs(
            verdict,
            qualifying_receipt.AdmissionVerdict.GRANDFATHERED_UNKNOWN,
        )
        self.assertTrue(
            all(authority.values()),
            f"schema-4 grandfather must remain accepted by authority legs: {panel}",
        )
        self.assertFalse(
            panel["finalize_receipt.py"],
            "schema-4 is not an already-finalized schema-5 row",
        )

    def test_mutation_rejects_green_unanimously(self) -> None:
        """THE mutation proof: raise executed_tests_min in a COPY and every
        consumer's answer must move from accept to reject -- one datum edit, all
        answers move. Any consumer still accepting is bypassing the registry."""
        with tempfile.TemporaryDirectory() as tmp:
            tightened = _tightened_predicate(Path(tmp) / "tight.json")
            live = self._panel(_green_row(SHA), predicate=None)
            self.assertTrue(all(live.values()), f"live must accept first: {live}")
            mutated = self._panel(_green_row(SHA), predicate=tightened)
        self.assertFalse(any(mutated.values()),
                         f"every consumer must reject under the tightened datum: {mutated}")

    def test_live_rejects_incomplete_coverage_unanimously(self) -> None:
        """DRIFT-4 closed: the ee303899 shape (executed>0 but 15 absent nodes) is
        REFUSED everywhere -- the per-node coverage clause is present in every
        consumer, including query.py which historically lacked it."""
        panel = self._panel(_ee303899_row(SHA), predicate=None)
        self.assertFalse(any(panel.values()),
                         f"incomplete coverage must be rejected by every consumer: {panel}")

    def test_live_rejects_incomplete_outer_graph_unanimously(self) -> None:
        """A 75-of-76 run cannot qualify by retaining complete test-node data."""
        panel = self._panel(_incomplete_outer_graph_row(SHA), predicate=None)
        self.assertFalse(
            any(panel.values()),
            f"incomplete outer graph must be rejected by every consumer: {panel}",
        )

    def test_live_rejects_unreported_coverage_lists_unanimously(self) -> None:
        """Missing is UNKNOWN, not empty, in all eight consumer legs."""
        for field in ("zero_executed_nodes", "absent_nodes"):
            with self.subTest(field=field):
                panel = self._panel(
                    _missing_coverage_list_row(SHA, field), predicate=None
                )
                self.assertFalse(
                    any(panel.values()),
                    f"unreported {field} must be rejected by every consumer: {panel}",
                )

    def test_live_rejects_out_of_u64_coverage_unanimously(self) -> None:
        """Python's unbounded int must not accept a count Rust cannot carry."""
        panel = self._panel(_overflow_coverage_count_row(SHA), predicate=None)
        self.assertFalse(
            any(panel.values()),
            f"planned_test_nodes above u64 must be rejected everywhere: {panel}",
        )

    def test_live_rejects_malformed_executed_node_count_unanimously(self) -> None:
        """A present diagnostic count must deserialize as Rust u64 everywhere."""
        malformed = (
            ("string", "4"),
            ("float", 1.5),
            ("bool", True),
            ("negative", -1),
            ("above-u64", 1 << 64),
            ("null", None),
        )
        for name, value in malformed:
            with self.subTest(name=name):
                panel = self._panel(
                    _executed_coverage_count_row(SHA, value), predicate=None
                )
                self.assertFalse(
                    any(panel.values()),
                    "malformed executed_test_nodes "
                    f"({name}) must be rejected everywhere: {panel}",
                )

    def test_live_accepts_rust_compatible_executed_node_counts_unanimously(self) -> None:
        """Missing defaults to zero; the full inclusive u64 domain is valid."""
        for name, value in (
            ("missing", _MISSING),
            ("zero", 0),
            ("u64-max", (1 << 64) - 1),
        ):
            with self.subTest(name=name):
                panel = self._panel(
                    _executed_coverage_count_row(SHA, value), predicate=None
                )
                self.assertTrue(
                    all(panel.values()),
                    "Rust-compatible executed_test_nodes "
                    f"({name}) must be accepted everywhere: {panel}",
                )

    def test_live_rejects_non_string_failure_entries_unanimously(self) -> None:
        """Present-but-malformed name lists are unavailable, never failures."""
        for field in ("zero_executed_nodes", "absent_nodes"):
            with self.subTest(field=field):
                panel = self._panel(
                    _malformed_coverage_list_row(SHA, field), predicate=None
                )
                self.assertFalse(
                    any(panel.values()),
                    f"non-string {field} entry must be rejected everywhere: {panel}",
                )

    def test_live_rejects_zero_execution_unanimously(self) -> None:
        """The surviving zero-execution floor: executed_tests==0 is refused by all
        consumers (executed_tests is diagnostic only, but a run that executed
        NOTHING is never a green at any schema)."""
        panel = self._panel(_zero_exec_row(SHA), predicate=None)
        self.assertFalse(any(panel.values()),
                         f"a zero-execution run must be rejected by every consumer: {panel}")

    def test_schema4_and_newer_passed_count_contract_directly(self) -> None:
        """Both shared predicates use one reason for every current-row defect."""
        predicate = qualifying_receipt.active()
        self.assertEqual(predicate["passed_tests"]["applies_at_schema_min"], 4)
        self.assertEqual(
            predicate["count_representation"],
            qualifying_receipt.COUNT_REPRESENTATION,
        )
        for schema, base in (
            (4, _grandfathered_schema4_row(SHA)),
            (5, _green_row(SHA)),
        ):
            with self.subTest(schema=schema, shape="exact"):
                self.assertEqual(
                    qualifying_receipt.row_qualification(base, SHA, predicate),
                    (True, "qualifies"),
                )
            for label, value in (
                ("missing", _MISSING),
                ("null", None),
                ("lower", 739),
                ("higher", 741),
                ("negative", -1),
                ("string", "740"),
                ("boolean", True),
                ("float", 740.0),
                ("above-i64", 1 << 63),
            ):
                with self.subTest(schema=schema, shape=label):
                    row = copy.deepcopy(base)
                    if value is _MISSING:
                        row.pop("passed_tests")
                    else:
                        row["passed_tests"] = value
                    self.assertEqual(
                        qualifying_receipt.row_qualification(row, SHA, predicate),
                        (False, qualifying_receipt.PASSED_TESTS_REFUSAL_REASON),
                    )

            for label, value, matching_passed in (
                ("missing-executed", _MISSING, 740),
                ("null-executed", None, 740),
                ("boolean-executed", True, 1),
                ("float-executed", 740.0, 740.0),
                ("above-i64-executed", 1 << 63, 1 << 63),
            ):
                with self.subTest(schema=schema, shape=label):
                    row = copy.deepcopy(base)
                    if value is _MISSING:
                        row.pop("executed_tests")
                    else:
                        row["executed_tests"] = value
                    row["passed_tests"] = matching_passed
                    self.assertEqual(
                        qualifying_receipt.row_qualification(row, SHA, predicate),
                        (False, qualifying_receipt.PASSED_TESTS_REFUSAL_REASON),
                    )

        old = _grandfathered_schema4_row(SHA)
        old["schema_version"] = 3
        self.assertEqual(
            qualifying_receipt.row_qualification(old, SHA, predicate),
            (True, "qualifies"),
            "pre-4 exact passed_tests must retain its qualification semantics",
        )
        old.pop("passed_tests")
        self.assertEqual(
            qualifying_receipt.row_qualification(old, SHA, predicate),
            (True, "qualifies"),
            "pre-4 missing passed_tests must retain its qualification semantics",
        )
        old["passed_tests"] = None
        self.assertEqual(
            qualifying_receipt.row_qualification(old, SHA, predicate),
            (True, "qualifies"),
            "pre-4 null passed_tests must retain its qualification semantics",
        )
        old["passed_tests"] = 739
        self.assertEqual(
            qualifying_receipt.row_qualification(old, SHA, predicate),
            (False, qualifying_receipt.PASSED_TESTS_REFUSAL_REASON),
            "pre-4 contradictory passed_tests must retain its refusal",
        )

    def test_count_representation_declaration_is_fixed(self) -> None:
        """The canonical i64 field set is a format contract, not a threshold."""
        predicate = copy.deepcopy(qualifying_receipt.active())
        for label, mutation in (
            ("wrong width", {"signed_bits": 32}),
            (
                "missing filtered field",
                {"fields": ["executed_tests", "passed_tests"]},
            ),
            ("late application", {"applies_at_schema_min": 5}),
        ):
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                drifted = copy.deepcopy(predicate)
                drifted["count_representation"].update(mutation)
                path = Path(tmp) / "predicate.json"
                path.write_text(json.dumps(drifted))
                with mock.patch.dict(
                    os.environ,
                    {qualifying_receipt.PREDICATE_ENV: str(path)},
                ):
                    with self.assertRaisesRegex(
                        RuntimeError,
                        "count_representation must be",
                    ):
                        qualifying_receipt.load()

    def test_current_schema_out_of_i64_counts_refuse_unanimously(self) -> None:
        """Schema 4/5 consumers refuse JSON integers Rust cannot deserialize."""
        overflow = qualifying_receipt.I64_MAX + 1
        expected_reasons = {
            "executed_tests": qualifying_receipt.PASSED_TESTS_REFUSAL_REASON,
            "passed_tests": qualifying_receipt.PASSED_TESTS_REFUSAL_REASON,
            "filtered_tests": qualifying_receipt.FILTERED_TESTS_I64_REFUSAL_REASON,
        }
        for schema, base in (
            (4, _grandfathered_schema4_row(SHA)),
            (5, _green_row(SHA)),
        ):
            maximum = copy.deepcopy(base)
            for field in expected_reasons:
                maximum[field] = qualifying_receipt.I64_MAX
            self.assertEqual(
                qualifying_receipt.row_qualification(
                    maximum, SHA, qualifying_receipt.active()
                ),
                (True, "qualifies"),
            )
            maximum_panel = self._passed_count_panel(
                schema, self._panel(maximum, predicate=None)
            )
            accepted = sum(maximum_panel.values())
            self.assertEqual(
                accepted,
                len(maximum_panel),
                f"schema-{schema} I64_MAX counts must be accepted by every "
                f"applicable consumer: {accepted}/{len(maximum_panel)} "
                f"{maximum_panel}",
            )

            for field, expected_reason in expected_reasons.items():
                with self.subTest(schema=schema, field=field):
                    row = copy.deepcopy(base)
                    row[field] = overflow
                    self.assertEqual(
                        qualifying_receipt.row_qualification(
                            row, SHA, qualifying_receipt.active()
                        ),
                        (False, expected_reason),
                    )
                    panel = self._passed_count_panel(
                        schema, self._panel(row, predicate=None)
                    )
                    self.assertFalse(
                        any(panel.values()),
                        f"schema-{schema} over-i64 {field} must be refused "
                        f"everywhere: {panel}",
                    )

    def test_current_schema_missing_passed_count_refuses_unanimously(self) -> None:
        """Missing and null counts refuse schema 4 and 5 in every consumer."""
        for schema, base in (
            (4, _grandfathered_schema4_row(SHA)),
            (5, _green_row(SHA)),
        ):
            exact = self._passed_count_panel(
                schema, self._panel(base, predicate=None)
            )
            self.assertTrue(
                all(exact.values()),
                f"schema-{schema} exact passed/executed counts must qualify: {exact}",
            )
            for shape in ("missing", "null"):
                with self.subTest(schema=schema, shape=shape):
                    row = copy.deepcopy(base)
                    if shape == "missing":
                        row.pop("passed_tests")
                    else:
                        row["passed_tests"] = None
                    panel = self._passed_count_panel(
                        schema, self._panel(row, predicate=None)
                    )
                    self.assertFalse(
                        any(panel.values()),
                        f"schema-{schema} {shape} passed_tests must refuse everywhere: {panel}",
                    )

    def test_malformed_or_mismatched_passed_count_refuses_unanimously(self) -> None:
        """Schema 4 and 5 consumers refuse mismatched and malformed counts."""
        for schema, base in (
            (4, _grandfathered_schema4_row(SHA)),
            (5, _green_row(SHA)),
        ):
            for label, value in (("mismatch", 739), ("malformed", "740")):
                with self.subTest(schema=schema, shape=label):
                    contradictory = copy.deepcopy(base)
                    contradictory["passed_tests"] = value
                    panel = self._passed_count_panel(
                        schema, self._panel(contradictory, predicate=None)
                    )
                    self.assertFalse(
                        any(panel.values()),
                        f"schema-{schema} {label} passed_tests must be refused "
                        f"everywhere: {panel}",
                    )

    def test_live_accepts_hard_and_rejects_soft_rebase_only_unanimously(self) -> None:
        """The class policy moves every consumer, including the Rust authority."""
        hard = self._panel(_green_row(SHA), predicate=None)
        soft = self._panel(_soft_rebase_only_row(SHA), predicate=None)
        self.assertTrue(all(hard.values()), f"hard exact-head row must pass: {hard}")
        self.assertFalse(
            any(soft.values()),
            f"hard-only policy must refuse soft-rebase-only everywhere: {soft}",
        )

    def test_typed_soft_provenance_agrees_across_every_consumer(self) -> None:
        """Rust and Python must derive the same class from the same JSON types.

        ``bool`` is an ``int`` subclass only in Python.  Likewise, a non-string
        item made Python's diagnostic join throw while Rust treated any
        nonempty array as force-full evidence.  With soft classes enabled in a
        predicate COPY, these mutations directly control acceptance rather than
        being hidden behind the production hard-only policy.
        """
        with tempfile.TemporaryDirectory() as tmp:
            predicate = _soft_enabled_predicate(Path(tmp) / "soft.json")

            valid_rebase = _soft_rebase_only_row(SHA)
            valid_force_full = _soft_rebase_only_row(SHA)
            valid_force_full["inherited_from"] = {
                "delta_kind": "rebase-plus-upstream",
                "upstream_commits": 2,
                "branch_commits": 0,
                "patch_identical": True,
                "force_full_paths": ["ci/run-node.sh"],
            }
            valid_force_full["green_class"] = "soft-force-full-touched"
            for name, row in (
                ("rebase-only", valid_rebase),
                ("force-full", valid_force_full),
            ):
                with self.subTest(positive=name):
                    panel = self._panel(row, predicate)
                    self.assertTrue(
                        all(panel.values()),
                        f"well-typed {name} provenance must agree as accepted: {panel}",
                    )

            malformed: list[tuple[str, dict]] = []
            row = _soft_rebase_only_row(SHA)
            row["validated_head_sha"] = 7
            malformed.append(("validated_head_sha=7", row))

            for value in (False, True):
                row = _soft_rebase_only_row(SHA)
                row["inherited_from"]["branch_commits"] = value
                malformed.append((f"branch_commits={value!r}", row))

            row = _soft_rebase_only_row(SHA)
            row["inherited_from"]["upstream_commits"] = False
            malformed.append(("rebase upstream_commits=False", row))

            row = _soft_rebase_only_row(SHA)
            row["inherited_from"].update(
                delta_kind="rebase-plus-upstream",
                upstream_commits=1 << 64,
            )
            row["green_class"] = "soft-upstream-delta"
            malformed.append(("upstream_commits exceeds u64", row))

            row = _soft_rebase_only_row(SHA)
            row["inherited_from"].update(
                delta_kind="rebase-plus-upstream",
                upstream_commits=True,
            )
            row["green_class"] = "soft-upstream-delta"
            malformed.append(("upstream_commits=True", row))

            row = _soft_rebase_only_row(SHA)
            row["inherited_from"]["force_full_paths"] = None
            malformed.append(("force_full_paths=null", row))

            row = _soft_rebase_only_row(SHA)
            row["inherited_from"].update(
                delta_kind="rebase-plus-upstream",
                upstream_commits=1,
                force_full_paths=["ci/run-node.sh", 7],
            )
            row["green_class"] = "soft-force-full-touched"
            malformed.append(("force_full_paths has non-string", row))

            for name, row in malformed:
                with self.subTest(negative=name):
                    panel = self._panel(row, predicate)
                    self.assertFalse(
                        any(panel.values()),
                        f"malformed {name} must fail closed identically: {panel}",
                    )

    def test_explicit_null_provenance_is_refused_unanimously(self) -> None:
        """Absent legacy fields default hard; present null carries no fact."""
        for field in ("validated_head_sha", "inherited_from", "green_class"):
            with self.subTest(field=field):
                row = _green_row(SHA)
                row[field] = None
                panel = self._panel(row, predicate=None)
                self.assertFalse(
                    any(panel.values()),
                    f"explicit-null {field} must be refused everywhere: {panel}",
                )

    def test_cell_verdict_is_exactly_the_four_closed_states(self) -> None:
        predicate = qualifying_receipt.active()
        for state in (
            "compared-and-matched",
            "compared-and-diverged",
            "performs-no-comparison-by-design",
            "unavailable-with-reason",
        ):
            with self.subTest(state=state):
                self.assertIs(
                    qualifying_receipt.cell_results_verdict(
                        _schema6_cell_row(SHA, state), predicate
                    ),
                    qualifying_receipt.CellResultsVerdict.SATISFIED,
                )

    def test_current_verifier_receipt_qualifies_but_weakened_or_foreign_shapes_do_not(
        self,
    ) -> None:
        predicate = qualifying_receipt.active()
        current = _schema6_cell_row(SHA, "compared-and-matched")
        self.assertEqual(
            qualifying_receipt.row_qualification(current, SHA, predicate),
            (True, "qualifies"),
        )
        self.assertIs(
            qualifying_receipt.cell_results_verdict(current, predicate),
            qualifying_receipt.CellResultsVerdict.SATISFIED,
        )

        replay = _schema6_cell_row(
            SHA,
            "compared-and-matched",
            mode="replay",
            virtualize_time=False,
        )
        # This fixture has the schema7 wire shape, independent of deployed support.
        replay["schema_version"] = 7
        self.assertEqual(
            qualifying_receipt.row_qualification(replay, SHA, predicate),
            (True, "qualifies"),
        )
        self.assertTrue(
            qualifying_receipt.cell_scorecard_projection(replay, predicate)["cells"][0][
                "scorecard_green"
            ]
        )

        for mode in ("verify", "chaos"):
            with self.subTest(mode=mode, virtualize_time=False):
                wrong_time_policy = _schema6_cell_row(
                    SHA,
                    "compared-and-matched",
                    mode=mode,
                    virtualize_time=False,
                )
                wrong_time_policy["schema_version"] = 7
                self.assertIs(
                    qualifying_receipt.cell_results_verdict(
                        wrong_time_policy, predicate
                    ),
                    qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
                )

        unknown_mode = _schema6_cell_row(
            SHA,
            "compared-and-matched",
            mode="unknown",
            virtualize_time=False,
        )
        unknown_mode["schema_version"] = 7
        self.assertIs(
            qualifying_receipt.cell_results_verdict(unknown_mode, predicate),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
        )

        unknown_reasoned_mode = _schema6_cell_row(
            SHA,
            "unavailable-with-reason",
            mode="unknown",
        )
        unknown_reasoned_mode["schema_version"] = 7
        self.assertIs(
            qualifying_receipt.cell_results_verdict(unknown_reasoned_mode, predicate),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
        )

        missing_mode = copy.deepcopy(replay)
        del missing_mode["cell_results"]["selected"][0]["mode"]
        del missing_mode["cell_results"]["cells"][0]["mode"]
        self.assertIs(
            qualifying_receipt.cell_results_verdict(missing_mode, predicate),
            qualifying_receipt.CellResultsVerdict.MALFORMED,
        )

        mutations = {
            "output buffers not compared": ("compare_io_buffers", False),
            "wrong display name": ("display_name", "Stripped"),
            "caller-defined record envelope": ("record_envelope", "caller_defined"),
            "virtual time disabled": ("virtualize_time", False),
            "extra stripped prefix": (
                "stripped_prefixes",
                ["real-wall-clock-prefix/v1", "unreviewed-prefix"],
            ),
            "missing address canonicalization": ("canonicalizations", []),
        }
        for name, (field, value) in mutations.items():
            with self.subTest(malformed=name):
                malformed = copy.deepcopy(current)
                malformed["cell_results"]["cells"][0]["cell_verdict"]["comparison"][field] = value
                self.assertEqual(
                    qualifying_receipt.row_qualification(malformed, SHA, predicate),
                    (False, "cell_results cell-verdict-invalid"),
                )
                self.assertIs(
                    qualifying_receipt.cell_results_verdict(malformed, predicate),
                    qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
                )

        missing = copy.deepcopy(current)
        del missing["cell_results"]["cells"][0]["cell_verdict"]["comparison"][
            "display_name"
        ]
        self.assertEqual(
            qualifying_receipt.row_qualification(missing, SHA, predicate),
            (False, "cell_results cell-verdict-invalid"),
        )

        explicit_nulls = copy.deepcopy(current)
        comparison = explicit_nulls["cell_results"]["cells"][0]["cell_verdict"][
            "comparison"
        ]
        for field in (
            "compare_io_buffers",
            "display_name",
            "record_envelope",
            "virtualize_time",
        ):
            comparison[field] = None
        self.assertEqual(
            qualifying_receipt.row_qualification(explicit_nulls, SHA, predicate),
            (False, "cell_results cell-verdict-invalid"),
        )

        foreign = copy.deepcopy(current)
        foreign["cell_results"]["cells"][0]["cell_verdict"]["comparison"][
            "unrecognized"
        ] = True
        self.assertEqual(
            qualifying_receipt.row_qualification(foreign, SHA, predicate),
            (False, "cell_results cell-verdict-invalid"),
        )
        self.assertIs(
            qualifying_receipt.cell_results_verdict(foreign, predicate),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
        )

    def test_schema_version_bounds_cell_results_authority(self) -> None:
        predicate = qualifying_receipt.active()
        current = _schema6_cell_row(SHA, "compared-and-matched")
        current["schema_version"] = 7
        self.assertEqual(
            qualifying_receipt.row_qualification(current, SHA, predicate),
            (True, "qualifies"),
        )

        newer = copy.deepcopy(current)
        newer["schema_version"] = predicate["cell_results"]["supported_schema_max"] + 1
        newer["cell_results"]["future_field"] = True
        self.assertEqual(
            qualifying_receipt.row_qualification(newer, SHA, predicate),
            (False, "cell_results schema-unavailable"),
        )
        self.assertIs(
            qualifying_receipt.cell_results_verdict(newer, predicate),
            qualifying_receipt.CellResultsVerdict.SCHEMA_UNAVAILABLE,
        )

        malformed_supported = copy.deepcopy(current)
        del malformed_supported["cell_results"]["cells"][0]["cell_verdict"][
            "comparison"
        ]["display_name"]
        self.assertEqual(
            qualifying_receipt.row_qualification(malformed_supported, SHA, predicate),
            (False, "cell_results cell-verdict-invalid"),
        )
        self.assertIs(
            qualifying_receipt.cell_results_verdict(malformed_supported, predicate),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
        )

    def test_schema8_version_bounds_cell_results_authority(self) -> None:
        predicate = _schema8_reader_policy()
        current = _schema8_cell_row(SHA)
        self.assertEqual(predicate["cell_results"]["supported_schema_max"], 8)
        self.assertEqual(
            qualifying_receipt.row_qualification(current, SHA, predicate),
            (True, "qualifies"),
        )

        schema7 = _schema6_cell_row(SHA, "compared-and-matched")
        schema7["schema_version"] = 7
        self.assertEqual(
            qualifying_receipt.row_qualification(schema7, SHA, predicate),
            (True, "qualifies"),
        )

        newer = copy.deepcopy(current)
        newer["schema_version"] += 1
        newer["cell_results"]["future_field"] = True
        self.assertEqual(
            qualifying_receipt.row_qualification(newer, SHA, predicate),
            (False, "cell_results schema-unavailable"),
        )
        self.assertIs(
            qualifying_receipt.cell_results_verdict(newer, predicate),
            qualifying_receipt.CellResultsVerdict.SCHEMA_UNAVAILABLE,
        )

        malformed_supported = copy.deepcopy(current)
        del malformed_supported["cell_results"]["cells"][0]["cell_verdict"][
            "comparison"
        ]["display_name"]
        self.assertEqual(
            qualifying_receipt.row_qualification(malformed_supported, SHA, predicate),
            (False, "cell_results cell-verdict-invalid"),
        )
        self.assertIs(
            qualifying_receipt.cell_results_verdict(malformed_supported, predicate),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
        )

    def test_schema_policy_maximum_alone_does_not_grant_a_new_schema_authority(self) -> None:
        predicate = _schema8_reader_policy()
        predicate["cell_results"]["supported_schema_max"] = 9
        for schema in (9, 10):
            with self.subTest(schema=schema):
                row = _schema8_cell_row(SHA)
                row["schema_version"] = schema
                self.assertIs(
                    qualifying_receipt.cell_results_verdict(row, predicate),
                    qualifying_receipt.CellResultsVerdict.SCHEMA_UNAVAILABLE,
                )

    def test_schema8_path_population_and_retained_subset_are_distinct(self) -> None:
        predicate = _schema8_reader_policy()
        current = _schema8_cell_row(SHA)
        second = copy.deepcopy(current["cell_results"]["selected"][0])
        second["test"] = "second"
        current["cell_results"]["selected"].append(second)
        current["cell_results"]["selected_count"] = 2
        current["cell_results"]["population_sha256"] = (
            qualifying_receipt.cell_population_sha256(
                current["cell_results"]["selected"]
            )
        )

        self.assertIs(
            qualifying_receipt.cell_results_verdict(current, predicate),
            qualifying_receipt.CellResultsVerdict.SATISFIED,
        )
        self.assertEqual(
            qualifying_receipt.row_qualification(current, SHA, predicate),
            (False, "cell_results incomplete for full validation"),
        )

        outside = copy.deepcopy(current)
        outside["cell_results"]["cells"][0]["test"] = "outside"
        self.assertIs(
            qualifying_receipt.cell_results_verdict(outside, predicate),
            qualifying_receipt.CellResultsVerdict.POPULATION_MISMATCH,
        )

        duplicate = copy.deepcopy(current)
        duplicate["cell_results"]["cells"].append(
            copy.deepcopy(duplicate["cell_results"]["cells"][0])
        )
        duplicate["cell_results"]["recorded_count"] = 2
        duplicate["cell_results"]["artifact"]["row_count"] = 2
        self.assertIs(
            qualifying_receipt.cell_results_verdict(duplicate, predicate),
            qualifying_receipt.CellResultsVerdict.POPULATION_MISMATCH,
        )

        mutated_selected = copy.deepcopy(current)
        mutated_selected["cell_results"]["selected"][0]["test"] = "mutated"
        self.assertIs(
            qualifying_receipt.cell_results_verdict(mutated_selected, predicate),
            qualifying_receipt.CellResultsVerdict.POPULATION_MISMATCH,
        )

    def test_schema8_full_population_refusal_precedes_policy_window(self) -> None:
        for maximum in (7, 8):
            for empty in (False, True):
                with self.subTest(maximum=maximum, empty=empty):
                    predicate = _schema8_reader_policy()
                    predicate["cell_results"]["supported_schema_max"] = maximum
                    row = _schema8_cell_row(SHA)
                    if empty:
                        row["cell_results"].update(
                            selected=[], cells=[], selected_count=0, recorded_count=0
                        )
                        row["cell_results"]["artifact"]["row_count"] = 0
                    else:
                        second = copy.deepcopy(row["cell_results"]["selected"][0])
                        second["test"] = "second"
                        row["cell_results"]["selected"].append(second)
                        row["cell_results"]["selected_count"] = 2
                    row["cell_results"]["population_sha256"] = (
                        qualifying_receipt.cell_population_sha256(
                            row["cell_results"]["selected"]
                        )
                    )
                    self.assertEqual(
                        qualifying_receipt.row_qualification(row, SHA, predicate),
                        (False, "cell_results incomplete for full validation"),
                    )

    def test_schema8_path_must_match_the_enclosing_validation_row(self) -> None:
        predicate = _schema8_reader_policy()
        for path in ("quick", "full", "super"):
            with self.subTest(path=path):
                row = _schema8_cell_row(SHA, path=path)
                self.assertIs(
                    qualifying_receipt.cell_results_verdict(row, predicate),
                    qualifying_receipt.CellResultsVerdict.SATISFIED,
                )

        substituted = _schema8_cell_row(SHA, path="quick")
        substituted["cell_results"]["path"] = "super"
        self.assertIs(
            qualifying_receipt.cell_results_verdict(substituted, predicate),
            qualifying_receipt.CellResultsVerdict.PATH_MISMATCH,
        )

        selective = _schema8_cell_row(SHA)
        selective["selection_mode"] = "selective"
        self.assertIs(
            qualifying_receipt.cell_results_verdict(selective, predicate),
            qualifying_receipt.CellResultsVerdict.PATH_MISMATCH,
        )

    def test_schema8_explicit_empty_population_is_readable_but_not_full_green(self) -> None:
        predicate = _schema8_reader_policy()
        empty = _schema8_cell_row(SHA, path="super")
        empty["cell_results"]["selected_count"] = 0
        empty["cell_results"]["recorded_count"] = 0
        empty["cell_results"]["selected"] = []
        empty["cell_results"]["cells"] = []
        empty["cell_results"]["artifact"]["row_count"] = 0
        empty["cell_results"]["population_sha256"] = (
            qualifying_receipt.cell_population_sha256([])
        )
        self.assertIs(
            qualifying_receipt.cell_results_verdict(empty, predicate),
            qualifying_receipt.CellResultsVerdict.SATISFIED,
        )
        projection = qualifying_receipt.cell_scorecard_projection(empty, predicate)
        self.assertTrue(projection["eligible"])
        self.assertEqual(projection["path"], "super")
        self.assertEqual(projection["selected"], [])
        self.assertEqual(projection["cells"], [])

        empty["profile"] = "full"
        empty["cell_results"]["path"] = "full"
        self.assertEqual(
            qualifying_receipt.row_qualification(empty, SHA, predicate),
            (False, "cell_results incomplete for full validation"),
        )

    def test_validate_status_cli_keeps_newer_shape_readable_without_authority(
        self,
    ) -> None:
        predicate = qualifying_receipt.active()
        newer = _schema6_cell_row(SHA, "compared-and-matched")
        newer["schema_version"] = predicate["cell_results"]["supported_schema_max"] + 1
        newer["record_id"] = "future-shape"
        newer["run_id"] = "future-shape"
        newer["toolchain"] = "rustc-test"
        newer["cell_results"]["run_id"] = "future-shape"
        newer["cell_results"]["cells"][0]["cell_verdict"]["comparison"][
            "future_field"
        ] = True

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _record_canonical_fixture(root, _green_row(SHA))
            with self.assertRaisesRegex(ValueError, "cell_results schema-unavailable"):
                _record_canonical_fixture(root, newer)
            _spool_without_cell_results_check(root, newer)
            proc = self._run_rust_status(root, SHA, predicate=None)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        report = json.loads(proc.stdout)
        self.assertEqual(report["verdict"], "VALIDATED")
        self.assertEqual(report["qualifying_count"], 1)
        self.assertEqual(report["disqualified_count"], 1)
        self.assertNotIn("LEDGER RECORDS UNREADABLE", proc.stderr)

        failed = _schema6_cell_row(SHA, "compared-and-matched")
        failed["schema_version"] = 7
        failed["cell_results"]["artifact"]["path"] = (
            "ignored/validate/artifacts/schema6-test/cells.jsonl"
        )
        failed.update(result="fail", raw_result="fail", exit_code=1, failures=1)
        failed.update(dag_jobs=4, known_flaky_failure=False, solo_rerun_confirmation=False)
        failed["gates"][1].update(
            result="fail",
            exit_code=1,
            real_seconds=1,
            failure_origin="outer_gate",
            failed_substeps=[],
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _spool_without_cell_results_check(root, failed)
            supported_proc = self._run_rust_status(root, SHA, predicate=None)
        self.assertEqual(supported_proc.returncode, 3, supported_proc.stderr)
        supported_report = json.loads(supported_proc.stdout)
        self.assertEqual(supported_report["verdict"], "FAILED")
        self.assertEqual(supported_report["failed_record_count"], 1)

        newer_failed = copy.deepcopy(failed)
        newer_failed["schema_version"] = predicate["cell_results"]["supported_schema_max"] + 1
        newer_failed["cell_results"]["cells"][0]["cell_verdict"]["comparison"][
            "future_field"
        ] = True
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _spool_without_cell_results_check(root, newer_failed)
            newer_proc = self._run_rust_status(root, SHA, predicate=None)
        self.assertEqual(newer_proc.returncode, 4, newer_proc.stderr)
        newer_report = json.loads(newer_proc.stdout)
        self.assertEqual(newer_report["verdict"], "NEEDS-RERUN")
        self.assertEqual(newer_report["failed_record_count"], 0)
        self.assertEqual(newer_report["withheld_nonpass_record_count"], 1)
        self.assertNotIn("LEDGER RECORDS UNREADABLE", newer_proc.stderr)

        malformed_supported = copy.deepcopy(newer)
        malformed_supported["schema_version"] = 7
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _spool_without_cell_results_check(root, malformed_supported)
            malformed_proc = self._run_rust_status(root, SHA, predicate=None)
        self.assertEqual(malformed_proc.returncode, 2)
        self.assertEqual(malformed_proc.stdout, "")
        self.assertIn("schema-7 row has malformed cell_results", malformed_proc.stderr)
        self.assertNotIn("LEDGER RECORDS UNREADABLE", malformed_proc.stderr)

    def test_tier_vocabulary_is_exactly_the_four_settled_names(self) -> None:
        self.assertEqual(
            qualifying_receipt._KNOWN_COMPARISON_TIERS,
            {
                "canonical-bitwise",
                "exit-and-stream-equality",
                "execution-only-self-consistent",
                "declared-but-unverifiable",
            },
        )
        for retired in (
            "full-stdout-info-stack-heap",
            "stdout-info-stack-heap-spot-check",
            "legacy-unqualified",
            "unqualified-no-comparison",
            "unqualified-stdout-only",
            "unqualified-self-verify-only",
            "unqualified-tool-count-only",
        ):
            with self.subTest(retired=retired):
                row = _schema6_cell_row(SHA, "compared-and-matched")
                row["cell_results"]["cells"][0]["cell_verdict"][
                    "comparison_tier"
                ] = retired
                self.assertIs(
                    qualifying_receipt.cell_results_verdict(
                        row, qualifying_receipt.active()
                    ),
                    qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
                )

    def test_raw_no_result_is_cause_evidence_not_a_fifth_verdict(self) -> None:
        row = _schema6_cell_row(SHA, "unavailable-with-reason")
        row["cell_results"]["cells"][0]["cell_verdict"]["state"] = "no_result"
        self.assertIs(
            qualifying_receipt.cell_results_verdict(
                row, qualifying_receipt.active()
            ),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
        )

    def test_compared_verdict_requires_nullable_log_count_key(self) -> None:
        row = _schema6_cell_row(SHA, "compared-and-matched")
        del row["cell_results"]["cells"][0]["cell_verdict"]["compared_log_messages"]
        self.assertIs(
            qualifying_receipt.cell_results_verdict(
                row, qualifying_receipt.active()
            ),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
        )

    def test_week_one_cell_evidence_projects_to_scorecard_without_exit_defaults(self) -> None:
        predicate = qualifying_receipt.active()
        expected = {
            "compared-and-matched": True,
            "compared-and-diverged": False,
            "performs-no-comparison-by-design": False,
            "unavailable-with-reason": False,
        }
        for state, green in expected.items():
            with self.subTest(state=state):
                row = _schema6_cell_row(SHA, state)
                projection = qualifying_receipt.cell_scorecard_projection(row, predicate)
                self.assertTrue(projection["eligible"])
                self.assertIs(projection["cell_results_satisfied"], True)
                self.assertEqual(projection["cells"][0]["state"], state)
                self.assertIs(projection["cells"][0]["scorecard_green"], green)
                self.assertEqual(projection["green_count"], int(green))

        weak = _schema6_cell_row(SHA, "compared-and-matched")
        weak_verdict = weak["cell_results"]["cells"][0]["cell_verdict"]
        weak_verdict["comparison_tier"] = "exit-and-stream-equality"
        _make_stripped(weak_verdict)
        weak_verdict["bitwise_parity"] = False
        weak_verdict["compared_log_messages"] = None
        projection = qualifying_receipt.cell_scorecard_projection(weak, predicate)
        self.assertTrue(projection["eligible"])
        self.assertFalse(projection["cells"][0]["scorecard_green"])
        self.assertEqual(
            projection["cells"][0]["comparison_tier"],
            "exit-and-stream-equality",
        )

        missing = _schema6_cell_row(SHA, "compared-and-matched")
        missing["exit_code"] = 0
        del missing["cell_results"]["cells"][0]["cell_verdict"]
        projection = qualifying_receipt.cell_scorecard_projection(missing, predicate)
        self.assertFalse(projection["eligible"])
        self.assertIs(projection["cell_results_satisfied"], False)
        self.assertIsNone(projection["cells"])

    def test_matched_tiers_remain_distinct_in_receipt_and_projection(self) -> None:
        predicate = qualifying_receipt.active()
        canonical = _schema6_cell_row(SHA, "compared-and-matched")
        weak = copy.deepcopy(canonical)
        weak_verdict = weak["cell_results"]["cells"][0]["cell_verdict"]
        weak_verdict["comparison_tier"] = "exit-and-stream-equality"
        _make_stripped(weak_verdict)
        weak_verdict["bitwise_parity"] = False
        weak_verdict["compared_log_messages"] = None

        canonical_projection = qualifying_receipt.cell_scorecard_projection(
            canonical, predicate
        )
        weak_projection = qualifying_receipt.cell_scorecard_projection(weak, predicate)
        self.assertEqual(
            canonical_projection["cells"][0]["comparison_tier"],
            "canonical-bitwise",
        )
        self.assertEqual(
            weak_projection["cells"][0]["comparison_tier"],
            "exit-and-stream-equality",
        )
        self.assertTrue(canonical_projection["cells"][0]["scorecard_green"])
        self.assertFalse(weak_projection["cells"][0]["scorecard_green"])
        self.assertNotEqual(canonical_projection["cells"], weak_projection["cells"])

    def test_weak_tiers_do_not_become_canonical_comparison_evidence(self) -> None:
        predicate = qualifying_receipt.active()
        laundered = _schema6_cell_row(SHA, "compared-and-matched")
        verdict = laundered["cell_results"]["cells"][0]["cell_verdict"]
        verdict["comparison_tier"] = "execution-only-self-consistent"
        self.assertIs(
            qualifying_receipt.cell_results_verdict(laundered, predicate),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
            "WITHOUT CONVERTING EITHER WEAK TIER INTO CANONICAL COMPARISON EVIDENCE",
        )

        measured_weak = copy.deepcopy(laundered)
        measured_verdict = measured_weak["cell_results"]["cells"][0][
            "cell_verdict"
        ]
        _make_stripped(measured_verdict)
        measured_verdict["bitwise_parity"] = False
        measured_verdict["compared_log_messages"] = None
        projection = qualifying_receipt.cell_scorecard_projection(
            measured_weak, predicate
        )
        self.assertTrue(projection["eligible"])
        self.assertEqual(
            projection["cells"][0]["comparison_tier"],
            "execution-only-self-consistent",
        )
        self.assertFalse(projection["cells"][0]["scorecard_green"])

        declared_laundered = _schema6_cell_row(SHA, "compared-and-matched")
        declared_laundered["cell_results"]["cells"][0]["cell_verdict"][
            "comparison_tier"
        ] = "declared-but-unverifiable"
        self.assertIs(
            qualifying_receipt.cell_results_verdict(
                declared_laundered, predicate
            ),
            qualifying_receipt.CellResultsVerdict.VERDICT_INVALID,
            "WITHOUT CONVERTING EITHER WEAK TIER INTO CANONICAL COMPARISON EVIDENCE",
        )
        declared = _schema6_cell_row(SHA, "unavailable-with-reason")
        declared_projection = qualifying_receipt.cell_scorecard_projection(
            declared, predicate
        )
        self.assertTrue(declared_projection["eligible"])
        self.assertEqual(
            declared_projection["cells"][0]["comparison_tier"],
            "declared-but-unverifiable",
        )
        self.assertFalse(declared_projection["cells"][0]["scorecard_green"])

    def test_tiers_classify_cells_not_backends_or_permanent_capabilities(self) -> None:
        predicate = qualifying_receipt.active()
        mixed = _schema6_cell_row(SHA, "compared-and-matched")
        canonical_cell = mixed["cell_results"]["cells"][0]
        canonical_cell["backend"] = "e9patch"
        canonical_cell["test"] = "canonical-cell"
        weak_cell = copy.deepcopy(canonical_cell)
        weak_cell["test"] = "broad-path-cell"
        weak_verdict = weak_cell["cell_verdict"]
        weak_verdict["comparison_tier"] = "execution-only-self-consistent"
        _make_stripped(weak_verdict)
        weak_verdict["bitwise_parity"] = False
        weak_verdict["compared_log_messages"] = None
        selected = [
            {key: cell[key] for key in qualifying_receipt._CELL_IDENTITY_KEYS}
            for cell in (canonical_cell, weak_cell)
        ]
        mixed["cell_results"]["selected"] = selected
        mixed["cell_results"]["cells"] = [canonical_cell, weak_cell]
        mixed["cell_results"]["selected_count"] = 2
        mixed["cell_results"]["recorded_count"] = 2
        mixed["cell_results"]["population_sha256"] = (
            qualifying_receipt.cell_population_sha256(selected)
        )
        mixed["cell_results"]["artifact"]["row_count"] = 2
        mixed_projection = qualifying_receipt.cell_scorecard_projection(
            mixed, predicate
        )
        self.assertTrue(mixed_projection["eligible"])
        self.assertEqual(mixed_projection["green_count"], 1)
        self.assertEqual(
            [cell["comparison_tier"] for cell in mixed_projection["cells"]],
            ["canonical-bitwise", "execution-only-self-consistent"],
        )

        current = _schema6_cell_row(SHA, "unavailable-with-reason")
        current_cell = current["cell_results"]["cells"][0]
        current_cell["backend"] = "dbt"
        current["cell_results"]["selected"][0]["backend"] = "dbt"
        current["cell_results"]["population_sha256"] = (
            qualifying_receipt.cell_population_sha256(
                current["cell_results"]["selected"]
            )
        )

        future = _schema6_cell_row(SHA, "compared-and-matched")
        future["run_id"] = "schema6-dbt-with-detlog"
        future["record_id"] = "schema6-dbt-with-detlog"
        future["cell_results"]["run_id"] = "schema6-dbt-with-detlog"
        future_cell = future["cell_results"]["cells"][0]
        future_cell["backend"] = "dbt"
        future["cell_results"]["selected"][0]["backend"] = "dbt"
        future["cell_results"]["population_sha256"] = (
            qualifying_receipt.cell_population_sha256(
                future["cell_results"]["selected"]
            )
        )

        current_projection = qualifying_receipt.cell_scorecard_projection(
            current, predicate
        )
        future_projection = qualifying_receipt.cell_scorecard_projection(
            future, predicate
        )
        self.assertEqual(
            current_projection["cells"][0]["comparison_tier"],
            "declared-but-unverifiable",
        )
        self.assertFalse(current_projection["cells"][0]["scorecard_green"])
        self.assertEqual(
            future_projection["cells"][0]["comparison_tier"],
            "canonical-bitwise",
        )
        self.assertTrue(future_projection["cells"][0]["scorecard_green"])

    def test_grandfathered_whole_run_never_satisfies_a_scorecard_cell(self) -> None:
        legacy = _green_row(SHA)
        proc = subprocess.run(
            [sys.executable, str(CI_HUB / "qualifying_receipt.py"), "--sha", SHA, "--json"],
            input=json.dumps(legacy),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        report = json.loads(proc.stdout)
        self.assertTrue(report["accepted"])
        self.assertIsNone(report["cell_results_satisfied"])
        projection = report["cell_scorecard"]
        self.assertFalse(projection["eligible"])
        self.assertIsNone(projection["cell_results_satisfied"])
        self.assertEqual(projection["status"], "grandfathered-unknown")
        self.assertIsNone(projection["green_count"])
        self.assertIsNone(projection["cells"])

        genuine = qualifying_receipt.cell_scorecard_projection(
            _schema6_cell_row(SHA, "compared-and-matched"),
            qualifying_receipt.active(),
        )
        self.assertTrue(genuine["eligible"])
        self.assertIs(genuine["cell_results_satisfied"], True)
        self.assertTrue(genuine["cells"][0]["scorecard_green"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
