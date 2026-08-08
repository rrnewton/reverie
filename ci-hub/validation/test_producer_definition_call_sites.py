#!/usr/bin/env python3
"""Call-site audit for the one producer-definition semantic verifier.

The registry is landing authority, so sharing its JSON is not enough: every
consumer must dereference it through the same shape/expiry/whole-map decision.
This test fails when a second production reader or a bypass appears.
"""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
CI_HUB = ROOT / "ci-hub"
VERIFIER = CI_HUB / "validation" / "verify_receipt.sh"
PUBLISHER = CI_HUB / "validation" / "publish_receipt.py"
ADAPTER = CI_HUB / "qualifying_receipt.py"
FINALIZER = CI_HUB / "validation" / "finalize_producer_transition.py"
RECEIPT_FINALIZER = CI_HUB / "validate" / "finalize_receipt.py"
ANCHOR = CI_HUB / "validate" / "anchor_select.py"
HISTORY_QUERY = CI_HUB / "history" / "query.py"
RUST = CI_HUB / "ci-hub.rs"
PROVISIONING = CI_HUB / "validation" / "test_verifier_provisioning.sh"


def production_sources() -> list[Path]:
    result = []
    for path in CI_HUB.rglob("*"):
        if not path.is_file() or path.suffix not in {".py", ".rs", ".sh"}:
            continue
        relative = path.relative_to(CI_HUB)
        if "tests" in relative.parts or path.name.startswith("test_"):
            continue
        result.append(path)
    return result


class ProducerDefinitionCallSiteAudit(unittest.TestCase):
    def test_only_immutable_verifier_reads_registry_or_transition_lifecycle(self):
        registry_readers = {
            path.relative_to(ROOT)
            for path in production_sources()
            if "producer-definition.json" in path.read_text(errors="replace")
        }
        self.assertEqual(
            registry_readers,
            {Path("ci-hub/validation/verify_receipt.sh")},
            "a second production registry reader would fork authority semantics",
        )

        lifecycle_readers = {
            path.relative_to(ROOT)
            for path in production_sources()
            if any(
                token in path.read_text(errors="replace")
                for token in (
                    "transition.finalize_after",
                    "transition.expires_at",
                    "transition.candidate",
                )
            )
        }
        self.assertEqual(
            lifecycle_readers,
            {Path("ci-hub/validation/verify_receipt.sh")},
            "transition shape/expiry/selection must have one implementation",
        )

    def test_publisher_and_adapter_delegate_map_semantics(self):
        source = PUBLISHER.read_text()
        self.assertNotIn("producer-definition.json", source)
        self.assertNotIn("finalize_after", source)
        self.assertNotIn("expires_at", source)
        self.assertIn("qualifying_receipt.resolve_producer_definition(row, sha)", source)

        adapter = ADAPTER.read_text()
        self.assertNotIn("producer-definition.json", adapter)
        self.assertIn('"--producer-definition-resolve"', adapter)
        self.assertIn('"--producer-definition-primary"', adapter)
        self.assertIn('"--producer-definition-check"', adapter)

    def test_receipt_and_reuse_paths_reach_same_verifier(self):
        verifier = VERIFIER.read_text()
        self.assertEqual(verifier.count("load_producer_definitions || exit $?"), 2)
        self.assertIn('or (.valid_commits | index($sha) != null)', verifier)

        rust = RUST.read_text()
        reuse = re.search(
            r"fn reuse_existing_immutable_receipt\(.*?\n}\n",
            rust,
            flags=re.DOTALL,
        )
        self.assertIsNotNone(reuse)
        self.assertIn("run_immutable_receipt_verifier", reuse.group(0))
        self.assertIn('root.join("ci-hub/validation/verify_receipt.sh")', rust)
        self.assertIn('root.join("ci-hub/validation/publish_receipt.py")', rust)
        self.assertIn('root.join("ci-hub/qualifying_receipt.py")', rust)
        self.assertIn("producer_definition_authority_key", rust)
        self.assertIn('"--producer-definition-allowed"', rust)

    def test_status_newest_green_finalizer_and_label_selection_are_wired(self):
        rust = RUST.read_text()
        self.assertIn("assess_canonical_receipts(root, &rows", rust)
        self.assertIn("producer_authoritative_history_rows", rust)
        self.assertIn("resolve_producer_definition_evidence(root, row, sha)", rust)

        finalizer = FINALIZER.read_text()
        self.assertNotIn("producer-definition.json", finalizer)
        self.assertNotIn("gh pr view", finalizer)
        self.assertNotIn("git fetch", finalizer)
        for token in (
            "--producer-definition-transition",
            "verify-landing",
            "--producer-definition-resolve",
            "--producer-definition-finalize",
            "--expected-registry-sha256",
            "registry_sha256",
            "merge_commit_oid",
            "refs/remotes/origin/main^{commit}",
        ):
            self.assertIn(token, finalizer)

    def test_every_python_semantic_green_consumer_has_a_producer_boundary(self):
        consumers = {
            path
            for path in production_sources()
            if re.search(
                r"qualifying_receipt\.(?:authoritative_)?row_qualif",
                path.read_text(errors="replace"),
            )
        }
        expected = {PUBLISHER, RECEIPT_FINALIZER, ANCHOR, HISTORY_QUERY}
        self.assertEqual(
            consumers,
            expected,
            "new semantic-green consumers must declare how they bind producer authority",
        )
        self.assertIn("authoritative_row_qualification", ANCHOR.read_text())
        self.assertIn("authoritative_row_qualifies", HISTORY_QUERY.read_text())
        self.assertIn("authoritative_row_qualifies", RECEIPT_FINALIZER.read_text())
        self.assertIn("resolve_producer_definition", PUBLISHER.read_text())

    def test_provisioned_gate_carries_the_same_verifier_and_registry(self):
        source = PROVISIONING.read_text()
        self.assertIn("ci-hub/validation/verify_receipt.sh", source)
        self.assertIn("ci-hub/validate/producer-definition.json", source)
        self.assertIn("PRODUCER_DEFINITION_REGISTRY", source)
        self.assertIn('bash "$verifier"', source)

    def test_no_consumer_opens_the_registry_directly(self):
        for relative in (
            "ci-hub/qualifying_receipt.py",
            "ci-hub/validation/publish_receipt.py",
            "ci-hub/validation/finalize_producer_transition.py",
            "ci-hub/ci-hub.rs",
            "ci-hub/lib/qualifying_receipt.rs",
            "ci-hub/lib/validate_status.rs",
        ):
            self.assertNotIn("producer-definition.json", (ROOT / relative).read_text())


if __name__ == "__main__":
    unittest.main()
