#!/usr/bin/env python3
"""A published evidence link must verify for a reader, not only for its producer.

The task's acceptance names the negatives explicitly: forged digest, forged size,
forged path, traversal, symlink, truncation, malformed or unknown schema, and an
attempted legacy-row backfill. There is one test per negative and each names the
mutation that would break it; every one of those was applied and confirmed to
fail.

⚠️ THE BACKFILL NEGATIVE IS THE ODD ONE AND THE MOST IMPORTANT. A forged digest
fails the first time anyone verifies it. A backfilled binding VERIFIES PERFECTLY
-- it is internally consistent and merely describes bytes that were never that
row's evidence. Nothing downstream can catch it, so it has to be refused here.
"""

from __future__ import annotations

import hashlib
import os
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "compatibility-website"))

import evidence_artifact as artifact  # noqa: E402


PAYLOAD = b"::   run 1: {{WORKSPACE_ROOT}}/ignored/x\n"


class ArtifactFixture(unittest.TestCase):
    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.addCleanup(self._directory.cleanup)
        # resolve(): macOS and some CI roots hand back a symlinked /tmp, and the
        # module refuses a link anywhere in the chain -- correctly. Resolving the
        # ROOT keeps the fixture honest without weakening that.
        self.root = Path(self._directory.name).resolve()
        (self.root / "cells" / "verify-1").mkdir(parents=True)
        self.evidence = self.root / "cells" / "verify-1" / "stdout.txt"
        self.evidence.write_bytes(PAYLOAD)


class PathSafetyTests(ArtifactFixture):
    def test_a_file_inside_the_artifact_binds_to_a_portable_path(self) -> None:
        relative = artifact.artifact_relative_path(
            artifact_root=self.root, path=self.evidence
        )

        self.assertEqual(relative, "cells/verify-1/stdout.txt")
        self.assertFalse(relative.startswith("/"))

    def test_traversal_is_refused(self) -> None:
        """Control: drop the `..` check and this fails.

        Note which check catches it: `relative_to` succeeds, because
        `<root>/cells/../../etc/passwd` is still LEXICALLY under the root as a
        string. The `..` scan is what refuses it, and that is why the two checks
        are separate rather than one containment test.
        """

        with self.assertRaises(artifact.ArtifactRefused) as caught:
            artifact.artifact_relative_path(
                artifact_root=self.root,
                path=self.root / "cells" / ".." / ".." / "etc" / "passwd",
            )

        self.assertIn("traverses upward", str(caught.exception))

    def test_an_absolute_path_outside_the_artifact_is_refused(self) -> None:
        with self.assertRaises(artifact.ArtifactRefused):
            artifact.artifact_relative_path(
                artifact_root=self.root, path=Path("/etc/hostname")
            )

    def test_a_symlinked_leaf_is_refused(self) -> None:
        """Control: remove the per-component symlink walk and this fails."""

        link = self.root / "cells" / "verify-1" / "linked.txt"
        link.symlink_to(self.evidence)

        with self.assertRaises(artifact.ArtifactRefused) as caught:
            artifact.artifact_relative_path(artifact_root=self.root, path=link)

        self.assertIn("symlink", str(caught.exception))

    def test_a_symlinked_PARENT_is_refused(self) -> None:
        """⚠️ `is_symlink()` on the leaf says nothing about its parents.

        A link partway up relocates the whole subtree, and the leaf is a perfectly
        ordinary file. Control: check only the final component and this fails
        while the leaf test above still passes.
        """

        (self.root / "real").mkdir()
        (self.root / "real" / "stdout.txt").write_bytes(PAYLOAD)
        (self.root / "cells" / "aliased").symlink_to(self.root / "real")

        with self.assertRaises(artifact.ArtifactRefused) as caught:
            artifact.artifact_relative_path(
                artifact_root=self.root,
                path=self.root / "cells" / "aliased" / "stdout.txt",
            )

        self.assertIn("symlink", str(caught.exception))

    def test_a_symlink_pointing_outside_is_refused(self) -> None:
        """⚠️ `resolve()` FOLLOWS the link, so a resolve-only check passes it.

        Control: keep only the resolved-containment check and drop the symlink
        walk -- this still fails, but the leaf test above starts passing wrongly,
        which is why both checks exist.
        """

        outside = Path(self._directory.name).resolve().parent / f"outside-{os.getpid()}"
        outside.write_bytes(b"not evidence\n")
        self.addCleanup(outside.unlink, missing_ok=True)
        link = self.root / "cells" / "verify-1" / "escape.txt"
        link.symlink_to(outside)

        with self.assertRaises(artifact.ArtifactRefused):
            artifact.artifact_relative_path(artifact_root=self.root, path=link)

    def test_a_directory_is_not_evidence(self) -> None:
        with self.assertRaises(artifact.ArtifactRefused):
            artifact.artifact_relative_path(
                artifact_root=self.root, path=self.root / "cells"
            )

    def test_a_relative_artifact_root_is_refused(self) -> None:
        with self.assertRaises(artifact.ArtifactRefused):
            artifact.artifact_relative_path(
                artifact_root=Path("relative/root"), path=self.evidence
            )


class BindingTests(ArtifactFixture):
    def test_the_binding_describes_the_bytes_on_disk(self) -> None:
        bound = artifact.bind_published_file(
            artifact_root=self.root, path=self.evidence
        )

        self.assertEqual(bound.schema, artifact.EVIDENCE_BINDING_SCHEMA)
        self.assertEqual(bound.artifact_path, "cells/verify-1/stdout.txt")
        self.assertEqual(bound.sha256, hashlib.sha256(PAYLOAD).hexdigest())
        self.assertEqual(bound.byte_length, len(PAYLOAD))

    def test_binding_the_same_bytes_twice_is_deterministic(self) -> None:
        """The task requires rebuild from the same source bytes to be stable."""

        first = artifact.bind_published_file(artifact_root=self.root, path=self.evidence)
        second = artifact.bind_published_file(artifact_root=self.root, path=self.evidence)

        self.assertEqual(first.to_json(), second.to_json())

    def test_a_correct_binding_verifies(self) -> None:
        bound = artifact.bind_published_file(artifact_root=self.root, path=self.evidence)

        artifact.verify_binding(bound, artifact_root=self.root)


class ForgeryTests(ArtifactFixture):
    def setUp(self) -> None:
        super().setUp()
        self.bound = artifact.bind_published_file(
            artifact_root=self.root, path=self.evidence
        )

    def test_a_forged_digest_is_rejected(self) -> None:
        forged = artifact.EvidenceBinding(**{**self.bound.to_json(), "sha256": "0" * 64})

        with self.assertRaises(artifact.ArtifactRefused) as caught:
            artifact.verify_binding(forged, artifact_root=self.root)

        self.assertIn("recorded digest", str(caught.exception))

    def test_a_forged_size_is_rejected(self) -> None:
        forged = artifact.EvidenceBinding(
            **{**self.bound.to_json(), "byte_length": self.bound.byte_length + 1}
        )

        with self.assertRaises(artifact.ArtifactRefused):
            artifact.verify_binding(forged, artifact_root=self.root)

    def test_truncation_is_reported_AS_truncation(self) -> None:
        """⚠️ The digest alone would call this corruption.

        A short file with a mismatched digest is almost always a cut-off write,
        and reporting it as "digest mismatch" sends the reader looking for the
        wrong thing. Control: check the digest before the length, or drop the
        length check, and this fails.
        """

        self.evidence.write_bytes(PAYLOAD[: len(PAYLOAD) // 2])

        with self.assertRaises(artifact.ArtifactRefused) as caught:
            artifact.verify_binding(self.bound, artifact_root=self.root)

        self.assertIn("truncated", str(caught.exception))

    def test_a_forged_path_pointing_outside_is_rejected(self) -> None:
        forged = artifact.EvidenceBinding(
            **{**self.bound.to_json(), "artifact_path": "../outside.txt"}
        )

        with self.assertRaises(artifact.ArtifactRefused):
            artifact.verify_binding(forged, artifact_root=self.root)


class SchemaTests(unittest.TestCase):
    def valid(self) -> dict[str, object]:
        return {
            "schema": artifact.EVIDENCE_BINDING_SCHEMA,
            "artifact_path": "cells/verify-1/stdout.txt",
            "sha256": hashlib.sha256(PAYLOAD).hexdigest(),
            "byte_length": len(PAYLOAD),
            "media_type": "text/plain",
            "encoding": "binary",
        }

    def test_a_well_formed_binding_parses(self) -> None:
        parsed = artifact.parse_binding(self.valid())

        self.assertEqual(parsed.to_json(), self.valid())

    def test_a_legacy_row_with_no_schema_is_refused_not_backfilled(self) -> None:
        """⚠️ THE NEGATIVE WITH NO TECHNICAL SYMPTOM.

        A backfilled binding is internally consistent and verifies perfectly; it
        merely describes bytes that were never this row's evidence. Nothing
        downstream can detect it, so defaulting the schema here would publish
        fabricated provenance silently and permanently.

        Control: `value.get("schema", EVIDENCE_BINDING_SCHEMA)` and this fails.
        """

        legacy = self.valid()
        del legacy["schema"]

        with self.assertRaises(artifact.ArtifactRefused) as caught:
            artifact.parse_binding(legacy)

        self.assertIn("predates the binding", str(caught.exception))

    def test_an_unknown_schema_is_refused(self) -> None:
        with self.assertRaises(artifact.ArtifactRefused):
            artifact.parse_binding({**self.valid(), "schema": 99})

    def test_an_unknown_field_is_refused(self) -> None:
        """Control: compare with `<=` instead of `==` and this fails.

        A binding carrying a field this reader does not understand was written by
        something else, and publishing it would assert a contract nobody checked.
        """

        with self.assertRaises(artifact.ArtifactRefused):
            artifact.parse_binding({**self.valid(), "public_url": "http://x/"})

    def test_a_missing_field_is_refused(self) -> None:
        for field in ("artifact_path", "sha256", "byte_length", "media_type", "encoding"):
            with self.subTest(field=field):
                incomplete = self.valid()
                del incomplete[field]
                with self.assertRaises(artifact.ArtifactRefused):
                    artifact.parse_binding(incomplete)

    def test_a_non_canonical_or_absolute_path_is_refused(self) -> None:
        for path in ("/abs/x", "a/../b", "a//b", "./a", "a\\b", ""):
            with self.subTest(path=path):
                with self.assertRaises(artifact.ArtifactRefused):
                    artifact.parse_binding({**self.valid(), "artifact_path": path})

    def test_a_malformed_digest_is_refused(self) -> None:
        for sha in ("", "abc", "A" * 64, "g" * 64, 0):
            with self.subTest(sha=sha):
                with self.assertRaises(artifact.ArtifactRefused):
                    artifact.parse_binding({**self.valid(), "sha256": sha})

    def test_a_malformed_length_is_refused(self) -> None:
        for length in (-1, True, "10", None):
            with self.subTest(length=length):
                with self.assertRaises(artifact.ArtifactRefused):
                    artifact.parse_binding({**self.valid(), "byte_length": length})

    def test_an_unknown_media_type_or_encoding_is_refused(self) -> None:
        with self.assertRaises(artifact.ArtifactRefused):
            artifact.parse_binding({**self.valid(), "media_type": "text/html"})
        with self.assertRaises(artifact.ArtifactRefused):
            artifact.parse_binding({**self.valid(), "encoding": "gzip"})


class CompositionTests(ArtifactFixture):
    def test_redact_bind_decide_place_composes(self) -> None:
        """The four modules meet only here, so pin the order they meet in.

        Redaction must precede the digest, and the digest must describe the file
        actually placed in the artifact. A reader verifying the published binding
        must get the redacted bytes back.
        """

        sys.path.insert(0, str(ROOT / "ci-hub" / "compatibility-website"))
        import evidence_redaction as redaction

        workspace = "/home/someone/work/dev-hermit"
        raw = f"run 1: {workspace}/ignored/x\n".encode()
        published = redaction.redact_and_bind(raw, workspace_root=workspace)
        self.evidence.write_bytes(published.data)

        bound = artifact.bind_published_file(
            artifact_root=self.root, path=self.evidence
        )

        self.assertEqual(bound.sha256, published.sha256)
        self.assertEqual(bound.byte_length, published.byte_length)
        artifact.verify_binding(bound, artifact_root=self.root)
        self.assertNotIn(b"/home/someone", (self.root / bound.artifact_path).read_bytes())


if __name__ == "__main__":
    unittest.main()
