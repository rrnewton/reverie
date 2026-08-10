#!/usr/bin/env python3
"""Both directions for the retirement tool, with counts.

The property under test is that a qualifying tier cannot outlive the evidence for
it, and that demoting one destroys nothing else. A hand-edited row is what created
the problem this tool exists for, so the tool must be provably mechanical: same
inputs, same output, no list of names inside it.
"""

from __future__ import annotations

import datetime as _dt
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("ru", HERE / "retire_unevidenced_tier_claims.py")
ru = importlib.util.module_from_spec(_spec)
sys.modules["ru"] = ru
_spec.loader.exec_module(ru)

FULL = ru._evidence.FULL
NOW = _dt.datetime(2026, 8, 8, tzinfo=_dt.timezone.utc)

# `stack_parity`/`heap_parity` are optional invocation modes, not verdicts.
HEADER = ("test_id,test_mode,backend,reason,stdout_parity,compared_log_messages,"
          "stack_parity,heap_parity,comparison_tier")


def row(name, *, tier=FULL, stdout="", info="150|150", stack="", heap="", reason=""):
    return f"{name},verify,ptrace,{reason},{stdout},{info},{stack},{heap},{tier}"


def root_with(rows):
    tmp = Path(tempfile.mkdtemp())
    (tmp / "probe-scorecard.csv").write_text("\n".join((HEADER, *rows)) + "\n")
    return tmp


def retire(tmp, *, apply):
    return ru.retire(tmp / "probe-scorecard.csv", apply=apply, now=NOW,
                     cadence_days=14, ledger={})


def read(tmp):
    import csv
    return list(csv.DictReader((tmp / "probe-scorecard.csv").open(newline="")))


class DemotesOnlyWhatItMust(unittest.TestCase):
    def test_an_unevidenced_qualifying_claim_is_demoted(self):
        tmp = root_with([row("g")])
        self.assertEqual(retire(tmp, apply=True), 1)
        self.assertEqual(read(tmp)[0]["comparison_tier"], ru.DEMOTED_TIER)

    def test_a_FULLY_EVIDENCED_claim_is_left_alone(self):
        """The positive control. Without it, a tool that demoted everything would
        pass every other test in this file."""
        tmp = root_with([row("g", stdout="1", stack="1", heap="1")])
        self.assertEqual(retire(tmp, apply=True), 0)
        self.assertEqual(read(tmp)[0]["comparison_tier"], FULL)

    def test_an_already_unqualified_row_is_left_alone(self):
        tmp = root_with([row("g", tier="legacy-unqualified")])
        self.assertEqual(retire(tmp, apply=True), 0)

    def test_it_is_idempotent(self):
        tmp = root_with([row("g"), row("h")])
        self.assertEqual(retire(tmp, apply=True), 2)
        self.assertEqual(retire(tmp, apply=True), 0)
        self.assertEqual(retire(tmp, apply=False), 0)

    def test_check_mode_reports_without_writing(self):
        tmp = root_with([row("g")])
        self.assertEqual(retire(tmp, apply=False), 1)
        self.assertEqual(read(tmp)[0]["comparison_tier"], FULL, "check mode wrote")

    def test_every_other_field_is_byte_preserved(self):
        """Demoting must not launder any other value -- the historical observation
        is the thing being preserved."""
        tmp = root_with([row("g", info="150|150", stack="", heap="")])
        before = read(tmp)[0]
        retire(tmp, apply=True)
        after = read(tmp)[0]
        changed = [k for k in before if (before[k] or "") != (after[k] or "")]
        self.assertEqual(sorted(changed), ["comparison_tier", "reason"])
        self.assertEqual(after["compared_log_messages"], "150|150")

    def test_the_row_records_WHY_it_was_demoted(self):
        tmp = root_with([row("g")])
        retire(tmp, apply=True)
        reason = read(tmp)[0]["reason"]
        self.assertIn("demoted to legacy-unqualified", reason)
        for component in ("stdout",):
            self.assertIn(component, reason)

    def test_an_existing_reason_is_appended_to_not_replaced(self):
        tmp = root_with([row("g", reason="pre-existing note")])
        retire(tmp, apply=True)
        self.assertTrue(read(tmp)[0]["reason"].startswith("pre-existing note; "))

    def test_the_criterion_is_computed_not_a_hardcoded_list(self):
        """No test id may appear in the tool. Hard-coding the six would be another
        unreproducible artifact, which is the defect being removed."""
        source = (HERE / "retire_unevidenced_tier_claims.py").read_text()
        self.assertNotIn("name_to_handle", source)
        self.assertNotIn("print_memaddrs", source)
        self.assertNotIn("heapy", source)


class SurvivesIntoValidateEnvelope(unittest.TestCase):
    """The gate must run this, and it must run it in --check mode: validation
    reports and refuses, it never silently rewrites published data."""

    SCRIPT = HERE / "validate-envelope.sh"

    def test_the_gate_invokes_check_mode_and_not_apply(self):
        text = self.SCRIPT.read_text(encoding="utf-8")
        needle = 'python3 "${here}/retire_unevidenced_tier_claims.py" --root "${here}"'
        self.assertIn(needle, text, "the gate no longer invokes the retirement check")
        # Executable lines only. A comment naming `--apply` as the remedy is
        # documentation; matching it would be the grep-hit-as-instance mistake.
        executable = [line for line in text.splitlines()
                      if line.strip() and not line.lstrip().startswith("#")]
        offending = [line for line in executable
                     if "retire_unevidenced_tier_claims.py" in line and "--apply" in line]
        self.assertEqual(offending, [],
                         "validation must never rewrite the published scorecards")

    def test_the_gates_own_command_refuses_a_planted_claim(self):
        import subprocess
        tmp = root_with([row("planted")])
        command = ('python3 "${here}/retire_unevidenced_tier_claims.py" --root "${here}"'
                   .replace("${here}/retire_unevidenced_tier_claims.py",
                            str(HERE / "retire_unevidenced_tier_claims.py"))
                   .replace('"${here}"', str(tmp)))
        result = subprocess.run(command, shell=True, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("would demote 1", result.stdout)


class ShippedData(unittest.TestCase):
    def test_no_published_row_carries_a_tier_it_cannot_evidence(self):
        """Green after the retirement, and this keeps it green: it fails the moment
        a qualifying tier reappears without the evidence to back it.

        Reads the COMMITTED blobs, not the working tree. "Published" means
        committed -- and the working copy of `scorecard.csv` is chronically dirty
        from a separate appending-producer defect, so checking it would report
        that producer's bug as this gate's. CI checks out committed content, so
        the two agree there; this only makes the local run mean the same thing.
        """
        import csv, datetime, subprocess
        names = [p.name for p in ru._tier.scorecards(HERE)]
        self.assertTrue(names, "no scorecards to check")
        now = datetime.datetime.now(datetime.timezone.utc)
        tmp = Path(tempfile.mkdtemp())
        for name in names:
            blob = subprocess.run(
                ["git", "show", f"HEAD:compat-envelope/{name}"],
                cwd=HERE.parent, capture_output=True, text=True)
            self.assertEqual(blob.returncode, 0, f"cannot read committed {name}")
            (tmp / name).write_text(blob.stdout)
        total = sum(ru.retire(p, apply=False, now=now, cadence_days=14, ledger={})
                    for p in ru._tier.scorecards(tmp))
        self.assertEqual(total, 0, "a committed row claims a tier it cannot evidence")


if __name__ == "__main__":
    unittest.main(verbosity=2)
