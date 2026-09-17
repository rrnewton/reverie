#!/usr/bin/env python3
"""Per-file bindings cannot prove anything about the SET; these tests do.

Two set-level failures survive a tree of individually perfect bindings, and they
fail in opposite ways:

* a DANGLING entry 404s, which reads as a broken site rather than as missing
  evidence;
* an UNLISTED file does not fail at all. Nothing 404s, nothing mis-verifies, and
  a reader who finds it cannot tell this run's evidence from a previous build's
  leftovers or from something never meant to be public.

The second is why "bind EVERY published evidence file" is a both-directions
requirement. Each test names the mutation that would break it; all were applied
and confirmed to fail.
"""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "compatibility-website"))

import evidence_artifact as artifact  # noqa: E402
import evidence_inventory as inventory  # noqa: E402


COMMIT = "a" * 40
GENERATOR = "ci-hub/compatibility-website"


class InventoryFixture(unittest.TestCase):
    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.addCleanup(self._directory.cleanup)
        self.root = Path(self._directory.name).resolve()
        self.evidence = self.root / "evidence"
        (self.evidence / "cell-a").mkdir(parents=True)
        (self.evidence / "cell-b").mkdir(parents=True)
        (self.evidence / "cell-a" / "stdout.txt").write_bytes(b"alpha\n")
        (self.evidence / "cell-a" / "stderr.txt").write_bytes(b"beta\n")
        (self.evidence / "cell-b" / "stdout.txt").write_bytes(b"gamma\n")

    def build(self) -> inventory.Inventory:
        return inventory.build_inventory(
            artifact_root=self.root,
            evidence_root=self.evidence,
            generator=GENERATOR,
            source_commit=COMMIT,
        )


class BuildTests(InventoryFixture):
    def test_every_file_under_the_evidence_root_is_bound(self) -> None:
        built = self.build()

        self.assertEqual(
            [entry.artifact_path for entry in built.entries],
            [
                "evidence/cell-a/stderr.txt",
                "evidence/cell-a/stdout.txt",
                "evidence/cell-b/stdout.txt",
            ],
        )

    def test_the_tree_is_walked_rather_than_taken_from_a_caller_list(self) -> None:
        """⚠️ A caller's list can only contain what the caller remembered.

        The unlisted-file failure IS the file the caller forgot, so discovering
        the set from the artifact is what makes "every" checkable rather than
        aspirational. Control: accept a list of paths instead of walking, and a
        file added behind the caller's back stops being noticed -- which is what
        the next test measures directly.
        """

        before = self.build()
        (self.evidence / "cell-b" / "surprise.txt").write_bytes(b"unlisted\n")
        after = self.build()

        self.assertEqual(len(after.entries), len(before.entries) + 1)
        self.assertIn(
            "evidence/cell-b/surprise.txt",
            [entry.artifact_path for entry in after.entries],
        )

    def test_an_unsafe_file_refuses_the_whole_build(self) -> None:
        """Control: skip the offending file instead of raising, and this fails.

        Skipping publishes an artifact whose inventory is silently incomplete --
        the unlisted-file failure arriving by a different route.
        """

        (self.evidence / "cell-a" / "link.txt").symlink_to(
            self.evidence / "cell-a" / "stdout.txt"
        )

        with self.assertRaises(inventory.InventoryRefused) as caught:
            self.build()

        self.assertIn("cannot bind published evidence", str(caught.exception))

    def test_provenance_is_required_and_exact(self) -> None:
        for generator, commit in (("", COMMIT), (GENERATOR, "abc"), (GENERATOR, "")):
            with self.subTest(generator=generator, commit=commit):
                with self.assertRaises(inventory.InventoryRefused):
                    inventory.build_inventory(
                        artifact_root=self.root,
                        evidence_root=self.evidence,
                        generator=generator,
                        source_commit=commit,
                    )

    def test_an_evidence_root_outside_the_artifact_is_refused(self) -> None:
        with self.assertRaises(inventory.InventoryRefused):
            inventory.build_inventory(
                artifact_root=self.root,
                evidence_root=Path("/etc"),
                generator=GENERATOR,
                source_commit=COMMIT,
            )


class DeterminismTests(InventoryFixture):
    def test_rebuilding_from_the_same_bytes_is_byte_identical(self) -> None:
        """The task's rebuild property, applied to the set rather than a file."""

        first = self.build()
        second = self.build()

        self.assertEqual(
            inventory.canonical_bytes(first.to_json()),
            inventory.canonical_bytes(second.to_json()),
        )
        self.assertEqual(first.digest(), second.digest())

    def test_entries_are_stored_in_canonical_order(self) -> None:
        """⚠️ Path ORDER AND STRING ORDER GENUINELY DISAGREE, so the final sort
        is not redundant with walking a sorted tree.

        `sorted(Path)` compares part tuples; the stored `artifact_path` is a
        posix string. For `a/b`, `a-c` and `a.b` they differ -- Path gives
        a/b, a-c, a.b and string gives a-c, a.b, a/b, because "/" is 0x2F and
        sorts after "-" and ".". Without names that straddle that boundary the
        control passes with the sort removed, which is how I first wrote it.

        Control: drop `sorted(..., key=artifact_path)` from build_inventory and
        this fails.
        """

        (self.evidence / "a").mkdir()
        (self.evidence / "a" / "b").write_bytes(b"slash\n")
        (self.evidence / "a-c").write_bytes(b"hyphen\n")
        (self.evidence / "a.b").write_bytes(b"dot\n")

        built = self.build()
        paths = [entry.artifact_path for entry in built.entries]

        self.assertEqual(paths, sorted(paths))
        self.assertIn("evidence/a/b", paths)
        self.assertLess(paths.index("evidence/a-c"), paths.index("evidence/a/b"))

    def test_the_digest_changes_when_any_published_byte_changes(self) -> None:
        before = self.build().digest()
        (self.evidence / "cell-a" / "stdout.txt").write_bytes(b"alpha changed\n")

        self.assertNotEqual(self.build().digest(), before)

    def test_the_digest_does_not_cover_itself(self) -> None:
        """A digest inside its own body cannot be recomputed by a reader."""

        built = self.build()

        self.assertNotIn("inventory_sha256", built.body())
        self.assertIn("inventory_sha256", built.to_json())


class VerifyTests(InventoryFixture):
    def test_a_matching_set_verifies(self) -> None:
        inventory.verify_inventory(
            self.build(), artifact_root=self.root, evidence_root=self.evidence
        )

    def test_an_unlisted_file_is_refused(self) -> None:
        """⚠️ THE FAILURE THAT DOES NOT FAIL BY ITSELF.

        Every binding still verifies; the artifact simply publishes a file
        nobody bound. Control: check only that each entry verifies, and this
        passes while the artifact ships unbound bytes.
        """

        built = self.build()
        (self.evidence / "cell-b" / "leftover.txt").write_bytes(b"from a past build\n")

        with self.assertRaises(inventory.InventoryRefused) as caught:
            inventory.verify_inventory(
                built, artifact_root=self.root, evidence_root=self.evidence
            )

        self.assertIn("does not bind", str(caught.exception))
        self.assertIn("leftover.txt", str(caught.exception))

    def test_a_dangling_entry_is_refused(self) -> None:
        built = self.build()
        (self.evidence / "cell-b" / "stdout.txt").unlink()

        with self.assertRaises(inventory.InventoryRefused):
            inventory.verify_inventory(
                built, artifact_root=self.root, evidence_root=self.evidence
            )

    def test_a_tampered_file_is_refused(self) -> None:
        built = self.build()
        (self.evidence / "cell-a" / "stdout.txt").write_bytes(b"tampered\n")

        with self.assertRaises(inventory.InventoryRefused) as caught:
            inventory.verify_inventory(
                built, artifact_root=self.root, evidence_root=self.evidence
            )

        self.assertIn("does not verify", str(caught.exception))

    def test_an_unlisted_BROKEN_symlink_is_refused_rather_than_ignored(self) -> None:
        """⚠️ THE LINK HAS TO BE BROKEN OR THIS TEST PROVES NOTHING.

        `is_file()` FOLLOWS a symlink, so a link to an existing file is already
        caught by the `is_file()` half and `or path.is_symlink()` changes
        nothing. I wrote it against /etc/hostname first and the control passed --
        a vacuous assertion. Only a link whose target is absent needs the second
        half: `is_file()` is False and `is_symlink()` is True.

        Control: drop `or path.is_symlink()` and this fails.
        """

        built = self.build()
        (self.evidence / "cell-b" / "sneak.txt").symlink_to(
            self.evidence / "target-that-does-not-exist"
        )

        with self.assertRaises(inventory.InventoryRefused):
            inventory.verify_inventory(
                built, artifact_root=self.root, evidence_root=self.evidence
            )


class ParseTests(InventoryFixture):
    def test_a_well_formed_inventory_round_trips(self) -> None:
        built = self.build()

        parsed = inventory.parse_inventory(json.loads(json.dumps(built.to_json())))

        self.assertEqual(parsed.to_json(), built.to_json())

    def test_a_forged_inventory_digest_is_refused(self) -> None:
        value = self.build().to_json()
        value["inventory_sha256"] = "0" * 64

        with self.assertRaises(inventory.InventoryRefused) as caught:
            inventory.parse_inventory(value)

        self.assertIn("does not cover its own body", str(caught.exception))

    def test_an_entry_removed_after_signing_is_refused(self) -> None:
        """⚠️ The digest is what makes the SET tamper-evident.

        Control: stop comparing the recorded digest and an attacker -- or a
        buggy writer -- can drop an entry and the inventory still parses.
        """

        value = self.build().to_json()
        value["entries"] = value["entries"][:-1]

        with self.assertRaises(inventory.InventoryRefused):
            inventory.parse_inventory(value)

    def test_duplicate_paths_are_refused(self) -> None:
        built = self.build()
        value = built.to_json()
        value["entries"] = [value["entries"][0], value["entries"][0]]
        value["inventory_sha256"] = hashlib.sha256(
            inventory.canonical_bytes(
                {
                    "schema": value["schema"],
                    "provenance": value["provenance"],
                    "entries": value["entries"],
                }
            )
        ).hexdigest()

        with self.assertRaises(inventory.InventoryRefused) as caught:
            inventory.parse_inventory(value)

        self.assertIn("same artifact path twice", str(caught.exception))

    def test_unsorted_entries_are_refused(self) -> None:
        """Order is part of the contract, because the digest depends on it."""

        value = self.build().to_json()
        value["entries"] = list(reversed(value["entries"]))
        value["inventory_sha256"] = hashlib.sha256(
            inventory.canonical_bytes(
                {
                    "schema": value["schema"],
                    "provenance": value["provenance"],
                    "entries": value["entries"],
                }
            )
        ).hexdigest()

        with self.assertRaises(inventory.InventoryRefused) as caught:
            inventory.parse_inventory(value)

        self.assertIn("canonical order", str(caught.exception))

    def test_an_unknown_or_missing_field_is_refused(self) -> None:
        for mutate in (
            lambda v: v.update({"public_url": "http://x/"}),
            lambda v: v.pop("provenance"),
            lambda v: v.pop("inventory_sha256"),
            lambda v: v.update({"schema": 99}),
        ):
            with self.subTest(mutate=mutate):
                value = self.build().to_json()
                mutate(value)
                with self.assertRaises(inventory.InventoryRefused):
                    inventory.parse_inventory(value)

    def test_a_legacy_entry_with_no_schema_is_refused(self) -> None:
        """The backfill refusal must survive being wrapped in an inventory."""

        value = self.build().to_json()
        del value["entries"][0]["schema"]

        with self.assertRaises(Exception) as caught:
            inventory.parse_inventory(value)

        self.assertIn("predates the binding", str(caught.exception))


class CompositionTests(InventoryFixture):
    def test_the_four_modules_compose_end_to_end(self) -> None:
        """redact, bind, decide, place, inventory -- in that order.

        The digest an inventory certifies must cover REDACTED bytes. Building
        over raw bytes would publish host identity under a digest asserting it
        was checked.
        """

        import evidence_redaction as redaction

        workspace = "/home/someone/work/dev-hermit"
        raw = f"run 1: {workspace}/ignored/x\n".encode()
        published = redaction.redact_and_bind(raw, workspace_root=workspace)
        (self.evidence / "cell-a" / "stdout.txt").write_bytes(published.data)

        built = self.build()
        inventory.verify_inventory(
            built, artifact_root=self.root, evidence_root=self.evidence
        )

        entry = next(
            item
            for item in built.entries
            if item.artifact_path == "evidence/cell-a/stdout.txt"
        )
        self.assertEqual(entry.sha256, published.sha256)
        self.assertNotIn(
            b"/home/someone", (self.root / entry.artifact_path).read_bytes()
        )

    def test_a_page_may_link_only_to_inventoried_paths(self) -> None:
        built = self.build()

        self.assertEqual(
            sorted(inventory.published_paths(built)),
            sorted(entry.artifact_path for entry in built.entries),
        )


if __name__ == "__main__":
    unittest.main()
