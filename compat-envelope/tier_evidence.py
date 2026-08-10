#!/usr/bin/env python3
"""A tier claim must carry evidence for EVERY component it names.

WHAT WAS WRONG. `check-scorecard-tier.py` validates the tier VOCABULARY: each row
must carry a value from a known set, and a raw pass is promoted to "qualified
green" iff that value is one of the two qualifying tiers (lines 108-115). It
never reads an evidence column. So `comparison_tier` is a self-declared label
with nothing binding it to the comparison actually performed -- the label-as-cache
failure. Reproduced on 2026-08-07 against main: four planted rows all claiming
`full-stdout-info-stack-heap`, one of them carrying neither stdout nor INFO,
scored `qualified_green=4/4` at rc=0.

This module is the missing half and is deliberately SEPARATE: the vocabulary
check reads only the tier column and can run on any file; this one reads the
evidence columns and needs the schema. Neither subsumes the other, the same
split `check_cell_comparison.py` documents against `check_scorecard_schema.py`.

Base green is bound to the existing comparison verdict: stdout equality and a
nonempty INFO-log comparison. `stack_parity` and `heap_parity` are optional RUN
MODE fields, despite their historical names: true means the corresponding flag
was passed, false means it was not, and blank means historical unknown. They do
not authorize or veto base green. A separate highest-grade query combines base
green with both stronger-mode fields true.

Exit codes:
  0  every enumerated row's tier claim is fully evidenced
  1  at least one claim is not
  2  REFUSED -- the population or the vocabulary could not be established
"""

from __future__ import annotations

import argparse
import csv
import datetime as _dt
import importlib.util
import sys
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load(name: str, filename: str):
    """Import a sibling module, including hyphenated ones that `import` cannot name."""
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    if spec is None or spec.loader is None:
        raise PopulationError(f"cannot load {filename}; the tier vocabulary is unavailable")
    module = importlib.util.module_from_spec(spec)
    # Register BEFORE exec: a module absent from sys.modules cannot resolve its own
    # `__module__` during @dataclass processing, so a dataclass in a sibling loaded
    # this way dies with a bare AttributeError. Costs nothing; avoids a trap that
    # only appears when this file is imported rather than run.
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class PopulationError(RuntimeError):
    """The set to be checked, or the vocabulary to check it against, is unavailable."""


_tier = _load("_check_scorecard_tier", "check-scorecard-tier.py")
_cadence = _load("_spot_check_cadence", "spot-check-cadence.py")
_strict = _load("_strict_verdict", "strict_verdict.py")

FULL = _tier.FULL
SPOT_CHECK = _tier.SPOT_CHECK
QUALIFYING = _tier.QUALIFYING

#: Per-row evidence class for a base-green component.
EVERY_RUN, CADENCED = "every-run", "cadenced"

#: What each qualifying tier claims. Keys are `strict_verdict.STRICT_COMPONENTS`.
TIER_CLAIMS = {
    FULL: {"stdout": EVERY_RUN, "info_log": EVERY_RUN},
    SPOT_CHECK: {"stdout": EVERY_RUN, "info_log": EVERY_RUN},
}

#: Where a per-run base-green component's evidence lives on the row. Optional
#: stack/heap MODE fields are deliberately absent from this verdict map.
ROW_EVIDENCE = {
    "stdout": "stdout_parity",
    "info_log": "compared_log_messages",
}

#: A recorded comparison of zero records is not evidence that a comparison happened.
_EMPTY_COUNTS = {"0|0", "0/0", "0"}
_BLANKS = {"", "-", "n/a", "none", "null"}

#: Default debt register, consulted ONLY when `--baseline` names it. Running this
#: file bare keeps its uncompromising verdict: every unevidenced claim is a
#: violation and the exit code is 1. See `load_baseline` for why the gate differs.
DEFAULT_BASELINE = HERE / "tier-evidence-baseline.json"


def claim_identity(file_name: str, row: dict) -> str:
    """Stable identity of a tier claim. Deliberately NOT the line number.

    Rows are appended and rewritten, so a line number would make the register go
    stale on every collector run and would silently re-key one row's debt onto
    another. `(file, test_id, test_mode, backend, tier)` is what the claim IS.
    """
    return "|".join((
        file_name,
        (row.get("test_id") or "").strip(),
        (row.get("test_mode") or "").strip(),
        (row.get("backend") or "").strip(),
        (row.get(_tier.REQUIRED) or "").strip(),
    ))


def load_baseline(path: Path) -> dict[str, str]:
    """Read the debt register: identity -> why it is not evidenced yet.

    WHY A REGISTER RATHER THAN A RED. Wiring this checker with no register turns
    every consumer red on day one for debt that predates the wiring, which is how
    a gate gets reverted instead of adopted. WHY IT IS NOT A MUTE BUTTON: a
    registered claim is still counted and still printed on every run; only the
    exit code is withheld. And the register is a RATCHET, enforced in `check`:

      * a claim NOT registered and not evidenced          -> violation, rc 1
      * a registered claim that is NOW evidenced          -> STALE, rc 1
      * a registered claim whose row no longer exists     -> STALE, rc 1

    So the register can only shrink. It cannot absorb a new unevidenced claim, and
    it cannot outlive the debt it records -- which is what separates it from the
    `SKIP` it replaces.
    """
    if not path.exists():
        raise PopulationError(f"baseline {path} does not exist")
    import json
    data = json.loads(path.read_text(encoding="utf-8"))
    entries = data.get("unevidenced_claims")
    if not isinstance(entries, list):
        raise PopulationError(f"baseline {path}: 'unevidenced_claims' must be a list")
    register: dict[str, str] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise PopulationError(f"baseline {path}: each entry must be an object")
        missing = [k for k in ("file", "test_id", "test_mode", "backend", "tier", "why")
                   if not str(entry.get(k) or "").strip()]
        if missing:
            raise PopulationError(
                f"baseline {path}: entry {entry!r} is missing {missing}; an entry "
                f"without a reason is an excuse, not a record")
        register["|".join(str(entry[k]).strip() for k in
                          ("file", "test_id", "test_mode", "backend", "tier"))] = entry["why"]
    return register


def _blank(value) -> bool:
    return (value or "").strip().lower() in _BLANKS


def is_dirty_sha(sha: str) -> bool:
    """A receipt SHA that does not identify a reproducible tree."""
    s = (sha or "").strip()
    return not s or s.lower().endswith("-dirty") or "dirty" in s.lower()


def row_component_evidenced(row: dict, header: list[str], component: str) -> tuple[bool, str]:
    """(ok, reason). Distinguishes a missing COLUMN from a blank VALUE, deliberately:
    the first is a schema that cannot express the claim, the second is a producer
    that did not make the measurement. Collapsing them hides which one to fix."""
    column = ROW_EVIDENCE[component]
    if column not in header:
        return False, f"schema-cannot-express:{component} (no {column!r} column)"
    value = (row.get(column) or "").strip()
    if _blank(value):
        return False, f"missing:{component} ({column} is blank)"
    if component == "info_log" and value in _EMPTY_COUNTS:
        return False, f"empty-comparison:{component} ({column}={value!r} compared nothing)"
    return True, ""


@dataclass
class Violation:
    file: str
    line: int
    test_id: str
    backend: str
    tier: str
    reasons: list[str]

    def render(self) -> str:
        return (f"{self.file}:{self.line}: {self.backend}/{self.test_id} "
                f"claims {self.tier} but {'; '.join(self.reasons)}")


@dataclass
class Report:
    root: str
    files: list[str] = field(default_factory=list)
    rows: int = 0
    claims: int = 0
    upheld: int = 0
    violations: list[Violation] = field(default_factory=list)
    #: Unevidenced claims that the debt register accounts for. Counted and printed
    #: on every run; they do not set the exit code.
    registered: list[Violation] = field(default_factory=list)
    #: Register entries that no longer describe live debt -- the claim became
    #: evidenced, or its row is gone. These DO set the exit code, so the register
    #: cannot outlive its reason.
    stale: list[str] = field(default_factory=list)
    baseline: str | None = None

    def render(self) -> str:
        out = ["tier-evidence check (does a tier claim carry every component it names?)",
               f"  resolved root : {self.root}",
               f"  population    : {len(self.files)} scorecard(s) enumerated"]
        out += [f"      {f}" for f in self.files]
        out.append("")
        out.append(f"  qualifying tier claims : {self.claims} of {self.rows} rows")
        out.append(f"  fully evidenced        : {self.upheld} of {self.claims}")
        out.append(f"  NOT evidenced          : "
                   f"{len(self.violations) + len(self.registered)} of {self.claims}")
        if self.baseline is not None:
            out.append(f"  debt register          : {self.baseline}")
            out.append(f"      registered debt    : {len(self.registered)} "
                       f"(counted above; does not set the exit code)")
            out.append(f"      UNREGISTERED        : {len(self.violations)} "
                       f"(these fail the gate)")
            out.append(f"      stale entries       : {len(self.stale)} "
                       f"(register describes debt that is gone; these fail the gate)")
        return "\n".join(out)


def check_ledger_receipt(key, ledger: dict, now: _dt.datetime,
                         cadence_days: int) -> tuple[bool, str]:
    """Cadenced stack/heap evidence: a receipt must exist, be CLEAN, and be CURRENT.

    Order matters. Dirtiness is tested BEFORE age: a `-dirty` receipt is not stale
    evidence, it is evidence about no identifiable tree, and reporting it as STALE
    would imply it had once been valid.
    """
    receipt = ledger.get(key)
    if not receipt:
        return False, "cadence:NEVER (no spot-check receipt on record)"
    if is_dirty_sha(receipt.get("hermit_sha", "")):
        return False, (f"dirty-receipt (hermit_sha="
                       f"{receipt.get('hermit_sha','') or '<blank>'!r} does not identify a tree)")
    state, age = _cadence.age_state(receipt.get("spot_check_utc", ""), now, cadence_days)
    if state != _cadence.CURRENT:
        return False, f"cadence:{state} (age={age} d, cadence={cadence_days} d)"
    return True, ""


def check_row(row: dict, header: list[str], key, ledger, now, cadence_days):
    """Reasons this row's tier claim is not fully evidenced; empty list == upheld."""
    tier = (row.get(_tier.REQUIRED) or "").strip()
    claims = TIER_CLAIMS.get(tier)
    if claims is None:
        return []          # not a qualifying tier: the vocabulary gate owns it
    reasons = []
    for component in _strict.STRICT_COMPONENTS:
        cadence_class = claims.get(component)
        if cadence_class == EVERY_RUN:
            ok, why = row_component_evidenced(row, header, component)
            if not ok:
                reasons.append(why)
        elif cadence_class == CADENCED:
            ok, why = check_ledger_receipt(key, ledger, now, cadence_days)
            if not ok and why not in reasons:
                reasons.append(why)
    return reasons


def check(root: Path, *, now: _dt.datetime, cadence_days: int, ledger_path: Path,
          baseline: dict[str, str] | None = None, baseline_path: Path | None = None) -> Report:
    found = _tier.scorecards(root)
    if not found:
        raise PopulationError(f"no *scorecard*.csv under {root}")
    ledger = _cadence.load_ledger(ledger_path) if ledger_path.exists() else {}
    report = Report(root=str(root), files=[p.name for p in found],
                    baseline=str(baseline_path) if baseline is not None else None)
    seen: set[str] = set()      # registered identities actually observed as debt
    upheld_ids: set[str] = set()
    for path in found:
        with path.open(newline="", encoding="utf-8") as handle:
            reader = csv.DictReader(handle)
            header = list(reader.fieldnames or [])
            for line, row in enumerate(reader, start=2):
                report.rows += 1
                tier = (row.get(_tier.REQUIRED) or "").strip()
                if tier not in QUALIFYING:
                    continue
                report.claims += 1
                key = (row.get("test_id", ""), row.get("test_mode", ""), row.get("backend", ""))
                reasons = check_row(row, header, key, ledger, now, cadence_days)
                identity = claim_identity(path.name, row)
                if reasons:
                    violation = Violation(
                        path.name, line, row.get("test_id", "?"),
                        row.get("backend", "?"), tier, reasons)
                    if baseline is not None and identity in baseline:
                        seen.add(identity)
                        report.registered.append(violation)
                    else:
                        report.violations.append(violation)
                else:
                    report.upheld += 1
                    upheld_ids.add(identity)
    if baseline is not None:
        # The ratchet. An entry is stale if its claim is now evidenced, or if no
        # row carries that claim any more. Either way the register is describing
        # debt that does not exist, and a register nobody has to prune is the
        # silence this whole mechanism exists to remove.
        for identity in sorted(set(baseline) - seen):
            why = ("the claim is now EVIDENCED -- delete this entry"
                   if identity in upheld_ids
                   else "no row carries this claim any more -- delete this entry")
            report.stale.append(f"{identity}: {why}")
    return report


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=HERE)
    ap.add_argument("--ledger", type=Path, default=None)
    ap.add_argument("--cadence-days", type=int, default=_cadence.CADENCE_DAYS)
    ap.add_argument("--now", default=None, help="ISO instant; defaults to real now")
    ap.add_argument(
        "--baseline", type=Path, nargs="?", const=DEFAULT_BASELINE, default=None,
        help="ratchet against a debt register of already-unevidenced claims. WITHOUT "
             "this flag the verdict is uncompromising: every unevidenced claim fails. "
             "WITH it, registered claims are still counted and printed but do not set "
             "the exit code, while an unregistered claim, or a register entry whose "
             "debt is gone, does. Bare default: " + str(DEFAULT_BASELINE))
    a = ap.parse_args(argv)

    now = _cadence._parse(a.now) or _dt.datetime.now(_dt.timezone.utc)
    ledger = a.ledger if a.ledger is not None else _cadence.LEDGER
    try:
        register = load_baseline(a.baseline) if a.baseline is not None else None
        report = check(a.root, now=now, cadence_days=a.cadence_days, ledger_path=ledger,
                       baseline=register, baseline_path=a.baseline)
    except PopulationError as error:
        print(f"tier-evidence: REFUSED: {error}", file=sys.stderr)
        return 2

    print(report.render())
    # Print the registered debt too. It does not fail the gate, but a number that
    # is never shown is a number nobody acts on -- the exact failure this replaces.
    if report.registered:
        print(f"\nREGISTERED DEBT: {len(report.registered)} claim(s) known to be "
              f"unevidenced and accepted for now:")
        for violation in report.registered[:20]:
            print(f"  {violation.render()}")
        if len(report.registered) > 20:
            print(f"  ... {len(report.registered) - 20} more")
    if report.stale:
        print(f"\nREFUSED: {len(report.stale)} debt-register entr(ies) no longer "
              f"describe live debt:", file=sys.stderr)
        for entry in report.stale[:20]:
            print(f"  {entry}", file=sys.stderr)
        print("The register is a ratchet: it may shrink, never persist past its reason.",
              file=sys.stderr)
    if report.violations:
        print(f"\nREFUSED: {len(report.violations)} tier claim(s) not supported by "
              f"their own evidence:", file=sys.stderr)
        for violation in report.violations[:20]:
            print(f"  {violation.render()}", file=sys.stderr)
        if len(report.violations) > 20:
            print(f"  ... {len(report.violations) - 20} more", file=sys.stderr)
        print("A tier names the components it compared. A row claiming one must carry "
              "evidence for every one of them, or drop to a lower tier or NO-RESULT.",
              file=sys.stderr)
    return 1 if (report.violations or report.stale) else 0


if __name__ == "__main__":
    raise SystemExit(main())
