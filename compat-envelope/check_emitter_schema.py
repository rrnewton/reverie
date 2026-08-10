#!/usr/bin/env python3
"""Refuse an EMITTER that would write a row shape its target file cannot hold.

WHY THIS EXISTS, AND WHY IT IS THE EMIT SIDE.
`check_cell_comparison.py` refuses a cell carrying a comparison verdict with no
reference. That closes the CHECK side: bad data already written is caught when
the checker runs. It does nothing to stop an emitter WRITING it. This closes the
emit side, and it also covers a class neither landed checker looks at:
ROW WIDTH vs HEADER WIDTH.

Schema skew is quiet in the worst way. An emitter that declares 36 columns and
appends into a 33-column file writes three fields past the last column. Nothing
errors. A reader lines the values up by index, so every column after the
divergence is read from the wrong place -- `ref_output_hash` comes back holding
whatever sits at that offset -- and the file still parses. The 19-vs-20 dbi
parity skew that redded hermit CI from a parent-only change was this same shape.

TWO REAL EXPOSURES MEASURED AT origin/main ON 2026-08-07, both caught below:
  * collect-envelope.rs declares 36 and defaults to scorecard.csv (33).
  * collect-reverie-compat.rs declares 34 and ALSO defaults to scorecard.csv.
    It is correct when its wrapper passes `--csv reverie-scorecard.csv` (34==34),
    which is why a by-hand audit pairing each emitter with "its" scorecard called
    it matching. The DEFAULT invocation is the exposure. Pair an emitter with the
    file it will actually open, not the one its name suggests.

DERIVED, NEVER HARDCODED. The pairs come from scanning the emitters, because a
hand-kept list of a growing set is the recurring defect this repository keeps
paying for -- and it is exactly how the second exposure above went unlisted.

AN EMITTER IT CANNOT PARSE IS A REFUSAL, NOT A SKIP. Dropping an emitter it
cannot understand would shrink the population silently and make the report look
best precisely where the tool understands least. Same rule as a dropped row
shrinking a denominator: unmeasurable is reported, never omitted.
"""

from __future__ import annotations

import argparse
import csv as csvmod
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# One pattern per emitter language. A file matching none is UNPARSED -> refused.
HEADER_PATTERNS = (
    r'const\s+HEADER\s*:\s*&str\s*=\s*"([^"]+)"',   # rust
    r'^HDR="([^"]+)"',                               # shell
    r'^HEADER="([^"]+)"',                            # shell
)
TARGET_PATTERNS = (
    r'const\s+DEFAULT_CSV_TEMPLATE\s*:\s*&str\s*=\s*"([^"]+)"',
    r'csv\.unwrap_or_else\(\|\|\s*here\.join\("([^"]+)"\)\)',
    r'csv\.unwrap_or_else\(\|\|\s*PathBuf::from\("([^"]+)"\)\)',
    r'let\s+mut\s+csv\s*=\s*PathBuf::from\("([^"]+)"\)',
    r'^CSV="?\$\{?[A-Z_]+:-([^"}\s]+)',
    r'^OUT="?([^"\s]*scorecard\.csv)',
    r'^\s*CSV=\$\{CSV:-\$\{?here\}?/([^"\s}]+)\}',
    r'([a-z0-9-]*scorecard\.csv)"?\s*$',
)


def first_match(text: str, patterns) -> str | None:
    for p in patterns:
        m = re.search(p, text, re.M)
        if m:
            return m.group(1)
    return None


def discover(root: Path) -> list[Path]:
    """Every emitter in the directory, found by glob on an ABSOLUTE root.

    Resolved from this file's own location, never the invocation directory: a
    cwd-sensitive population is a defect this repo has already been bitten by.
    """
    return sorted(p for p in root.glob("collect-*") if p.suffix in (".rs", ".sh"))


def check_widths(path: Path) -> list[str]:
    """Every data row must have exactly as many fields as the header."""
    problems = []
    with path.open(newline="") as fh:
        rows = list(csvmod.reader(fh))
    if not rows:
        return [f"{path.name}: EMPTY -- no header, so nothing can be validated"]
    width = len(rows[0])
    bad = [(i, len(r)) for i, r in enumerate(rows[1:], start=2) if len(r) != width]
    for line, got in bad[:5]:
        problems.append(f"{path.name}:{line}: {got} fields, header has {width}")
    if len(bad) > 5:
        problems.append(f"{path.name}: ... and {len(bad) - 5} more width mismatches")
    return problems


SCHEMA = "scorecard-schema.json"


def expected_header(schema: dict, variant: str) -> list[str] | None:
    """The one true column list for a variant: CORE + its extras, core order kept.

    Derived from the single definition, never restated. A variant that omits a
    core column says so explicitly in the definition, so the omission is a
    recorded decision rather than a silent difference.
    """
    v = schema["variants"].get(variant)
    if v is None:
        return None
    # The definition states each variant's ACTUAL order. It does not reconstruct
    # it as core+extras, because one real variant (reverie) inserts an extra
    # MID-CORE -- reconstructing would make the definition disagree with the file
    # and report a difference it could not name. Truth first; the core-prefix
    # RULE is then checked separately and reported as its own violation.
    return list(v["columns"])


def check_against_definition(root: Path) -> list[str]:
    """Enforce BOTH directions between the definition and the files on disk.

    definition -> file : a variant whose header does not equal core+extras FAILS.
    file -> definition : a scorecard present but UNDECLARED fails too, otherwise a
                         new scorecard could be added and escape the definition
                         entirely -- the growing-set hole this exists to close.
    """
    spath = root / SCHEMA
    if not spath.exists():
        return [f"{SCHEMA}: MISSING -- there is no single definition to derive from"]
    try:
        schema = json.loads(spath.read_text())
    except ValueError as e:
        return [f"{SCHEMA}: malformed ({e})"]
    for key in ("core", "variants"):
        if key not in schema:
            return [f"{SCHEMA}: missing required key {key!r}"]

    problems: list[str] = []
    on_disk = {p.name for p in root.glob("*scorecard*.csv")}
    declared = set(schema["variants"])

    for name in sorted(on_disk - declared):
        problems.append(
            f"{name}: UNDECLARED in {SCHEMA}. A scorecard the definition does not "
            f"know about cannot be validated; declare it or remove it."
        )
    for name in sorted(declared & on_disk):
        v = schema["variants"][name]
        # POLICY, separate from drift: a variant whose core columns are not a
        # prefix in core order breaks core-position indexing for every reader
        # that knows only the core.
        if v.get("core_prefix_safe") is False:
            problems.append(
                f"{name}: NOT CORE-PREFIX SAFE -- its core columns are not a prefix "
                f"in core order (omits {v.get('omits_core') or 'none'}; extras "
                f"{v.get('extra_columns') or 'none'} not all appended). A reader that "
                f"knows only the core cannot index core columns in this variant."
            )
        want = expected_header(schema, name)
        got = (root / name).read_text(errors="replace").split("\n", 1)[0].split(",")
        if want != got:
            problems.append(
                f"{name}: HEADER DIFFERS FROM THE DEFINITION -- definition derives "
                f"{len(want)} column(s), file has {len(got)}. "
                f"definition-only: {sorted(set(want) - set(got)) or 'none'}; "
                f"file-only: {sorted(set(got) - set(want)) or 'none'}. "
                f"Update {SCHEMA} and the emitter together; they derive from one source."
            )
    return problems


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, default=HERE,
                    help="directory holding the emitters and scorecards")
    args = ap.parse_args()
    root: Path = args.root.resolve()

    emitters = discover(root)
    if not emitters:
        print(f"REFUSE: no emitters found under {root} -- an empty population "
              f"cannot be a pass", file=sys.stderr)
        return 2

    problems: list[str] = []
    checked = unparsed = absent = 0

    for em in emitters:
        text = em.read_text(errors="replace")
        header = first_match(text, HEADER_PATTERNS)
        target = first_match(text, TARGET_PATTERNS)
        if header is None or target is None:
            unparsed += 1
            missing = "HEADER" if header is None else "default --csv target"
            problems.append(
                f"{em.name}: UNPARSED -- cannot derive its {missing}. Refused rather "
                f"than skipped: an emitter this tool cannot read is unverified, not clean."
            )
            continue
        # An unexpanded shell/format variable means the derivation FAILED. Calling
        # that "target absent" would file a mis-derived emitter under a benign
        # heading -- the silent-population-shrink this tool exists to prevent, and
        # I hit it here first: collect-fullcorpus.sh derived to a literal "$here/...".
        if any(ch in target for ch in "${}"):
            unparsed += 1
            problems.append(
                f"{em.name}: UNPARSED -- derived target {target!r} still contains an "
                f"unexpanded variable, so the real target is unknown. Refused rather "
                f"than reported absent."
            )
            continue
        tpath = (root / target) if not Path(target).is_absolute() else Path(target)
        if not tpath.exists():
            absent += 1
            print(f"  {em.name:<30} -> {target} (target absent; header declares "
                  f"{len(header.split(','))} columns) NOT YET WRITTEN")
            continue
        checked += 1
        file_header = tpath.read_text(errors="replace").split("\n", 1)[0]
        if header.strip() != file_header.strip():
            hs, fs = header.split(","), file_header.split(",")
            problems.append(
                f"{em.name}: SCHEMA SKEW against its DEFAULT target {tpath.name} -- "
                f"emitter declares {len(hs)} columns, file has {len(fs)}. "
                f"Emitter-only: {sorted(set(hs) - set(fs)) or 'none'}; "
                f"file-only: {sorted(set(fs) - set(hs)) or 'none'}."
            )
        else:
            print(f"  {em.name:<30} -> {tpath.name} ({len(hs := header.split(','))} cols) OK")

    for sc in sorted(root.glob("*scorecard*.csv")):
        problems.extend(check_widths(sc))
    problems.extend(check_against_definition(root))

    print(f"\npopulation: {len(emitters)} emitter(s) discovered "
          f"[{checked} checked against an existing target, {absent} target-absent, "
          f"{unparsed} unparsed]")
    if problems:
        print("\nREFUSED:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1
    print("OK: every emitter's declared header matches its default target, every "
          "scorecard row is header-width, and every scorecard matches the single "
          f"definition in {SCHEMA}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
