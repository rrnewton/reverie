"""Static guard: production ledger consumers use one shared boundary."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
CI_HUB = ROOT / "ci-hub"

RETIRED_TOKENS = (
    "validate-run-" + "ledger.jsonl",
    "HERMIT_" + "VALIDATE_LEDGER",
    "REVERIE_" + "VALIDATE_LEDGER",
    "DISCOVERED parent " + "ledger",
    "ci/validate-" + "ledger",
)

# Every production module that consumes receipt rows. Each must either import
# the canonical adapter directly or invoke the Rust command whose default loader
# delegates to that adapter.
CONSUMERS = (
    "health/pr_status.py",
    "history/query.py",
    "history/validate_time_ratchet.py",
    "lib/validate_status.rs",
    "remediation/protocol.py",
    "validate/aggregate.py",
    "validate/anchor_select.py",
    "validate/attribute_reds.py",
    "validate/failure_evidence.py",
    "validate/finalize_receipt.py",
    "validate/green_class.py",
    "validate/mutation_suite.py",
    "validate/qualified_rows.py",
    "validate/start_unit.py",
    "validate/totality.py",
    "validate/wall_cpu_ratchet.py",
    "main-metrics/wall_series.py",
)


def test_no_production_source_names_a_retired_ledger_location() -> None:
    offenders = []
    for path in CI_HUB.rglob("*"):
        if (
            not path.is_file()
            or "tests" in path.parts
            or path.name.startswith("test_")
            or path.suffix not in {".py", ".rs", ".sh"}
        ):
            continue
        text = path.read_text(errors="replace")
        found = [token for token in RETIRED_TOKENS if token in text]
        if found:
            offenders.append(f"{path.relative_to(ROOT)}: {found}")
    assert not offenders, "retired ledger locations remain:\n" + "\n".join(offenders)


def test_every_declared_consumer_delegates_to_the_canonical_boundary() -> None:
    missing = []
    for rel in CONSUMERS:
        text = (CI_HUB / rel).read_text()
        if rel == "lib/validate_status.rs":
            # Rust's loader lives in ci-hub.rs; the library declares only the
            # logical ledger root and never a physical receipt file.
            ok = 'pub const LEDGER_REL: &str = "ledger"' in text
        elif rel in {"validate/start_unit.py"}:
            ok = '"ledger", "qualified-rows"' in text
        elif rel in {"history/validate_time_ratchet.py", "main-metrics/wall_series.py"}:
            ok = "qualified" in text and "DEFAULT_LEDGER" in text
        else:
            ok = "canonical_ledger" in text or "qualified-rows" in text
        if not ok:
            missing.append(rel)
    assert not missing, "consumer bypasses canonical ledger adapter: " + ", ".join(missing)


def test_rust_front_controller_executes_the_canonical_adapter() -> None:
    text = (CI_HUB / "ci-hub.rs").read_text()
    assert 'root.join("ci-hub/ledger/validate_rows.py")' in text
    assert '.arg("rows")' in text
