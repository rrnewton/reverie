#!/usr/bin/env python3
"""An unreadable producer definition is a READER fault, never an evidence verdict.

Measured 2026-08-11 on devbig014: an actor holding the identical 1423-row shard,
with both ledger rows for the SHA present, reported `NOT-VALIDATED, 1
non-qualifying` purely because `hermit/` was an uninitialized submodule.
Initializing the submodule flipped the same receipt to VALIDATED with no other
change. The directory EXISTED, so it passed the `is_dir()` candidate filter,
failed resolution with rc 1, and was reported as unresolvable evidence.

That collapse is the defect: "the receipt does not qualify" blames the fleet's
evidence, while "I cannot read the definition" blames this checkout. An actor in
that state silently disbelieves every valid receipt in the fleet and never
suspects itself.

Bracketed BOTH ways here, because a guard that only ever fires is as useless as
one that never does:
  - reader incomplete  -> ProducerDefinitionUndereferenceable, CLI rc 3
  - definition readable but the map is not allowed -> plain
    ProducerDefinitionError, CLI rc 1, i.e. a real verdict about the receipt
"""

from pathlib import Path
import json
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
ADAPTER = ROOT / "ci-hub" / "qualifying_receipt.py"

sys.path.insert(0, str(ROOT / "ci-hub"))
import qualifying_receipt as qr  # noqa: E402


SHA = "a" * 40


def _git(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )


class UndereferenceableProducerDefinitionTest(unittest.TestCase):
    def test_empty_submodule_directory_is_a_reader_fault_not_a_verdict(self) -> None:
        """The exact observed shape: a real directory with no object store."""
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "hermit"
            empty.mkdir()
            self.assertTrue(empty.is_dir(), "fixture must reproduce is_dir()==True")
            with self.assertRaises(qr.ProducerDefinitionUndereferenceable) as caught:
                qr.resolve_producer_definition({}, SHA, repositories=[empty])
            detail = str(caught.exception)
            # The message must accuse the checkout, not the receipt, and must
            # say the receipt was never judged.
            self.assertIn("THIS CHECKOUT", detail)
            self.assertIn("has not been judged", detail)
            self.assertIn("uninitialized submodule", detail)

    def test_reader_fault_is_a_subtype_so_existing_consumers_still_fail_closed(
        self,
    ) -> None:
        """Consumers that already catch ProducerDefinitionError keep working."""
        self.assertTrue(
            issubclass(
                qr.ProducerDefinitionUndereferenceable, qr.ProducerDefinitionError
            )
        )

    def test_real_repo_without_the_commit_is_also_a_reader_fault(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp) / "repo"
            repo.mkdir()
            _git("init", "-q", ".", cwd=repo)
            with self.assertRaises(qr.ProducerDefinitionUndereferenceable):
                qr.resolve_producer_definition({}, SHA, repositories=[repo])

    def test_cli_returns_3_and_says_the_receipt_was_not_judged(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "hermit"
            empty.mkdir()
            result = subprocess.run(
                [
                    sys.executable,
                    str(ADAPTER),
                    "--producer-definition-row",
                    "--sha",
                    SHA,
                    "--repo-checkout",
                    str(empty),
                ],
                input="{}",
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(
                result.returncode,
                3,
                f"rc must be 3 (reader), never 1 (verdict): {result.stdout}{result.stderr}",
            )
            report = json.loads(result.stdout)
            self.assertEqual(
                report["producer_definition_status"],
                "undereferenceable-reader-incomplete",
            )
            self.assertIs(report["receipt_judged"], False)
            self.assertIs(report["accepted"], False)

    def test_rust_consumer_routes_rc3_to_a_loud_error_not_a_disqualified_row(
        self,
    ) -> None:
        """The collapse lived in the Rust caller, so pin its branch shape.

        `Some(1) => Ok(None)` feeds `disqualified`, which is the evidence
        verdict. rc 3 must not reach that bucket.
        """
        # LOCATION UPDATED 2026-08-25, CONCERN UNCHANGED. The rc is now read in
        # ci-hub/lib/qualifying_receipt.rs, which ci-hub.rs reaches through
        # `mod qualifying_receipt`; the three literals this test used to pin were
        # in neither file any more, so it was failing on a stale ADDRESS while the
        # collapse it guards had moved. The assertions below pin the SAME concern
        # -- rc 3 must not reach the verdict bucket -- at the site that now decides
        # it. They are deliberately NOT re-pointed at a bool: a tri-state is what
        # makes the distinction expressible at all.
        lib = (ROOT / "ci-hub" / "lib" / "qualifying_receipt.rs").read_text()
        rust = (ROOT / "ci-hub" / "ci-hub.rs").read_text()

        # rc 3 is a reader fault and must be typed as one.
        self.assertIn("Some(3) =>", lib)
        self.assertIn("BoundaryOutcome::Indeterminate", lib)

        # The evidence verdict must still exist, so this did not simply delete
        # the disqualification path.
        self.assertIn("Some(1) => return BoundaryOutcome::Refused", lib)

        # The two blocking states must be REPORTED differently at the consumer,
        # or the type distinction dies one layer up -- which is exactly how the
        # original collapse happened.
        self.assertIn("CANNOT DEREFERENCE PRODUCER DEFINITION", rust)
        self.assertIn("BoundaryOutcome::Refused", rust)
        self.assertIn("BoundaryOutcome::Indeterminate", rust)


if __name__ == "__main__":
    unittest.main()
