#!/usr/bin/env python3
"""Keep the retired validator-file registry out of validation decisions.

The hand-registered producer-definition registry and its transition lifecycle
were removed (https://github.com/rrnewton/dev-hermit/issues/414). A receipt's
producer map is recorded provenance; nothing may compare it to a registered
map. Local status, history, finalization, and main state follow the completed
run.
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
REGISTRY = CI_HUB / "validate" / "producer-definition.json"
RECEIPT_FINALIZER = CI_HUB / "validate" / "finalize_receipt.py"
ANCHOR = CI_HUB / "validate" / "anchor_select.py"
HISTORY_QUERY = CI_HUB / "history" / "query.py"
MAIN_SOFT_GREEN = CI_HUB / "landing" / "main_soft_green.py"
RUST = CI_HUB / "ci-hub.rs"
PROVISIONING = CI_HUB / "validation" / "test_verifier_provisioning.sh"


def production_sources() -> list[Path]:
    result = []
    for path in CI_HUB.rglob("*"):
        if not path.is_file() or path.suffix not in {".py", ".rs", ".sh"}:
            continue
        relative = path.relative_to(CI_HUB)
        # Both naming conventions are used for executable test harnesses. A
        # hyphenated shell harness is no more a production authority reader
        # than a test_*.py file; counting it hid the actual 1-reader invariant.
        if "tests" in relative.parts or path.name.startswith(("test_", "test-")):
            continue
        result.append(path)
    return result


class ProducerDefinitionCallSiteAudit(unittest.TestCase):
    def test_no_production_source_reads_registry_or_transition_lifecycle(self):
        self.assertFalse(REGISTRY.exists(), "the producer registry was retired")
        self.assertFalse(FINALIZER.exists(), "the transition finalizer was retired")
        registry_readers = {
            path.relative_to(ROOT)
            for path in production_sources()
            if any(
                token in path.read_text(errors="replace")
                for token in ("producer-definition.json", "PRODUCER_DEFINITION_REGISTRY")
            )
        }
        self.assertEqual(
            registry_readers,
            set(),
            "a registered producer map must not gate receipt verification",
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
            set(),
            "the producer transition lifecycle was retired",
        )

    def test_publisher_and_adapter_delegate_map_semantics(self):
        source = PUBLISHER.read_text()
        self.assertNotIn("producer-definition.json", source)
        self.assertNotIn("finalize_after", source)
        self.assertNotIn("expires_at", source)
        self.assertNotIn("resolve_producer_definition", source)
        self.assertNotIn("--observe-producer-definition", source)
        self.assertIn("qualifying_receipt.observe_producer_definition(row, sha)", source)

        adapter = ADAPTER.read_text()
        self.assertNotIn("producer-definition.json", adapter)
        self.assertNotIn("def resolve_producer_definition", adapter)
        self.assertNotIn("def registered_producer_definition", adapter)
        self.assertNotIn("def producer_definition_allowed", adapter)
        self.assertNotIn("--producer-definition-row", adapter)
        self.assertNotIn('"--producer-definition-resolve"', adapter)
        self.assertIn('"--producer-definition-observe"', adapter)
        self.assertNotIn('"--producer-definition-primary"', adapter)
        self.assertNotIn('"--producer-definition-check"', adapter)

    def test_historical_immutable_receipts_still_use_one_verifier(self):
        verifier = VERIFIER.read_text()
        self.assertEqual(verifier.count("load_producer_definitions"), 0)
        for token in (
            "--producer-definition-resolve",
            "--producer-definition-primary",
            "--producer-definition-check",
            "--producer-definition-transition",
            "--producer-definition-finalize",
        ):
            self.assertNotIn(token, verifier)

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
        self.assertNotIn("producer_definition_authority_key", rust)
        self.assertNotIn("producer_authoritative_history_rows", rust)
        self.assertNotIn("resolve_producer_definition_evidence", rust)

    def test_status_history_and_finalization_do_not_use_the_registry(self):
        rust = RUST.read_text()
        self.assertIn("assess_canonical_receipts(state_root, &rows", rust)
        self.assertNotIn("producer_authoritative_history_rows", rust)
        self.assertNotIn("resolve_producer_definition_evidence", rust)

        for path in (RECEIPT_FINALIZER, ANCHOR, HISTORY_QUERY, MAIN_SOFT_GREEN):
            self.assertNotIn(
                "resolve_producer_definition(",
                path.read_text(),
                f"{path.name} must not freeze validation on registered file hashes",
            )


    def test_python_green_consumers_share_the_run_predicate(self):
        consumers = {
            path
            for path in production_sources()
            if re.search(
                r"qualifying_receipt\.(?:authoritative_)?row_qualif",
                path.read_text(errors="replace"),
            )
        }
        expected = {
            PUBLISHER,
            RECEIPT_FINALIZER,
            ANCHOR,
            HISTORY_QUERY,
            MAIN_SOFT_GREEN,
        }
        self.assertEqual(
            consumers,
            expected,
            "new semantic-green consumers must use the shared run predicate",
        )
        self.assertIn("authoritative_row_qualification", ANCHOR.read_text())
        self.assertIn("authoritative_row_qualifies", HISTORY_QUERY.read_text())
        self.assertIn("authoritative_row_qualifies", RECEIPT_FINALIZER.read_text())
        self.assertNotIn("resolve_producer_definition", PUBLISHER.read_text())

    def test_provisioned_gate_carries_the_same_verifier(self):
        source = PROVISIONING.read_text()
        self.assertIn("ci-hub/validation/verify_receipt.sh", source)
        self.assertIn('bash "$verifier"', source)

    def test_no_consumer_opens_the_registry_directly(self):
        for relative in (
            "ci-hub/qualifying_receipt.py",
            "ci-hub/validation/publish_receipt.py",
            "ci-hub/validation/verify_receipt.sh",
            "ci-hub/ci-hub.rs",
            "ci-hub/lib/qualifying_receipt.rs",
            "ci-hub/lib/validate_status.rs",
        ):
            self.assertNotIn("producer-definition.json", (ROOT / relative).read_text())


if __name__ == "__main__":
    unittest.main()
