#!/usr/bin/env python3
"""Regenerate experiments/INDEX.md from what is ON DISK.

WHY THIS EXISTS
---------------
A measurement that cannot be found costs exactly as much as one never taken —
and more, because the second measurement may disagree with the first and nobody
will know to reconcile them. This index is DERIVED, never hand-filed: run it and
the index reflects reality. Nobody has to remember to update a database.

WHAT IT MINES, per experiment directory:
  date      the _YYYYMMDD slug suffix, else the directory mtime
  what      metadata.json "question", else the README's first heading
  headline  the first result-bearing line under a Verdict/Result/Finding heading
  how       provenance markers found in the text (observed / measured / recorded
            / sampled / computed / estimated / modelled) — the distinction that
            went unrecorded when 21.8x and 2.2x turned out to be the same
            quantity measured two ways
  who       metadata.json agent/author, else the directory's first git author
  where     the path

Directories whose metadata.json status is SCHEMA_ONLY_NO_EXPERIMENTS_RUN are
excluded only while they contain no experiment.json file. They define how a
future measurement will be recorded, but contain no record to index. Once any
record exists the directory fails open into the index, even if its status is
stale or the record is malformed.

Rows are pipe-delimited so `grep` finds them and `awk -F'|'` parses them.

USAGE
  python3 experiments/build-index.py            # rewrite experiments/INDEX.md
  python3 experiments/build-index.py --check    # exit 1 if the index is stale
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
INDEX = ROOT / "INDEX.md"

# Provenance markers, most specific first. The point of recording these is that
# "computed" and "observed" are different epistemic claims about the same number.
HOW_MARKERS = [
    ("recorded", r"\brecorded\b|\bcgroup-recorded\b|\breceipt\b"),
    ("observed", r"\bobserved\b|\bmeasured\b|\bempiric"),
    ("sampled", r"\bsampl|\bpolled\b|\bpolling\b"),
    ("computed", r"\bcomputed\b|\bderived\b|\bcalculat"),
    ("estimated", r"\bestimate|\bmodell?ed\b|\bprojected\b"),
]

RESULT_HEADS = re.compile(
    r"^#+\s*(verdict|result|results|finding|findings|answer|conclusion|headline|summary)\b",
    re.I,
)
# A line that actually carries a number with a unit or a ratio — the thing a
# future searcher greps for.
NUMERIC = re.compile(r"\d[\d,.]*\s*(%|x\b|s\b|ms\b|MiB|GiB|MB|GB|KiB|cores?|CPU-s|/)", re.I)


def slug_date(name: str, path: Path) -> str:
    m = re.search(r"_(\d{8})$", name)
    if m:
        s = m.group(1)
        return f"{s[:4]}-{s[4:6]}-{s[6:]}"
    m = re.search(r"_(\d{4})$", name)  # e.g. debian_reproducible_builds_2026
    if m:
        return m.group(1)
    try:
        return dt.datetime.fromtimestamp(path.stat().st_mtime).strftime("%Y-%m-%d")
    except OSError:
        return "unknown"


def read(p: Path, limit: int = 60000) -> str:
    try:
        return p.read_text(errors="replace")[:limit]
    except OSError:
        return ""


def clean(s: str, n: int = 190) -> str:
    s = re.sub(r"[|\n\r\t]+", " ", s)
    s = re.sub(r"[*`#>_\[\]]+", "", s)
    s = re.sub(r"\s{2,}", " ", s).strip()
    return (s[: n - 1] + "…") if len(s) > n else s


def extract_what(d: Path) -> str:
    meta = d / "metadata.json"
    if meta.exists():
        try:
            j = json.loads(read(meta))
            for k in ("question", "purpose", "title", "experiment", "description"):
                if isinstance(j.get(k), str) and j[k].strip():
                    return clean(j[k])
        except (json.JSONDecodeError, ValueError):
            pass
    rd = read(d / "README.md", 4000)
    for line in rd.splitlines():
        if line.startswith("#"):
            return clean(line.lstrip("# "))
    return ""


def extract_headline(d: Path) -> str:
    rd = read(d / "README.md")
    if not rd:
        return ""
    lines = rd.splitlines()
    # 1) first numeric line under a Verdict/Result/... heading
    for i, line in enumerate(lines):
        if RESULT_HEADS.match(line):
            for cand in lines[i + 1 : i + 14]:
                if cand.strip() and NUMERIC.search(cand):
                    return clean(cand)
            for cand in lines[i + 1 : i + 6]:
                if cand.strip() and not cand.startswith("#"):
                    return clean(cand)
    # 2) otherwise the first numeric prose line in the doc
    for line in lines:
        t = line.strip()
        if t and not t.startswith(("#", "|", "-", "*", "```")) and NUMERIC.search(t):
            return clean(t)
    return ""


def extract_how(d: Path) -> str:
    blob = (read(d / "README.md", 20000) + " " + read(d / "metadata.json", 8000)).lower()
    hits = [name for name, pat in HOW_MARKERS if re.search(pat, blob)]
    return ",".join(hits) if hits else "unstated"


def extract_who(d: Path) -> str:
    meta = d / "metadata.json"
    if meta.exists():
        try:
            j = json.loads(read(meta))
            for k in ("agent", "author", "who", "operator"):
                if isinstance(j.get(k), str) and j[k].strip():
                    return clean(j[k], 40)
        except (json.JSONDecodeError, ValueError):
            pass
    try:
        out = subprocess.run(
            ["git", "log", "--diff-filter=A", "--format=%an", "-1", "--", d.name],
            cwd=ROOT, capture_output=True, text=True, timeout=15,
        ).stdout.strip()
        if out:
            return clean(out, 40)
    except (OSError, subprocess.SubprocessError):
        pass
    return "unknown"


def artifacts(d: Path) -> str:
    have = [n for n in ("README.md", "metadata.json", "results.csv") if (d / n).exists()]
    return "+".join(x.replace(".md", "").replace(".json", "").replace(".csv", "") for x in have) or "none"


def is_schema_only(d: Path) -> bool:
    """Return whether metadata marks a directory with no target records."""
    meta = d / "metadata.json"
    if not meta.exists():
        return False
    try:
        value = json.loads(read(meta))
    except (json.JSONDecodeError, ValueError):
        return False
    if not (
        isinstance(value, dict)
        and value.get("status") == "SCHEMA_ONLY_NO_EXPERIMENTS_RUN"
    ):
        return False
    return not any(d.rglob("experiment.json"))


def build_rows() -> list[tuple[str, ...]]:
    rows = []
    for d in sorted([p for p in ROOT.iterdir() if p.is_dir()], key=lambda p: p.name):
        if is_schema_only(d):
            continue
        rows.append((
            slug_date(d.name, d),
            d.name,
            extract_what(d) or "(undocumented)",
            extract_headline(d) or "(no headline result in README)",
            extract_how(d),
            extract_who(d),
            artifacts(d),
        ))
    rows.sort(key=lambda r: (r[0], r[1]), reverse=True)
    return rows


HEADER = """# Index of measurements taken

**A measurement that cannot be found costs exactly as much as one never taken** — and more,
because a second measurement may disagree with the first and nobody will know to reconcile
them.

This file is **DERIVED, not hand-filed**. Regenerate it:

```
python3 experiments/build-index.py          # rewrite
python3 experiments/build-index.py --check  # exit 1 if stale
```

**Find a measurement:** `grep -i <term> experiments/INDEX.md`, or
`awk -F'|' '$5 ~ /observed/' experiments/INDEX.md` to filter by provenance.

**`how` is the load-bearing column.** `21.8x` and `2.2x` were once the same quantity
measured two ways — one computed, one observed — and the disagreement went unnoticed for
hours because neither was recorded with its method. `unstated` means the artifact does not
say; treat such a number as unqualified until you check the source.

**Adding a measurement:** write it into `experiments/<name>_YYYYMMDD/` with a `README.md`
(a `## Verdict` or `## Result` heading, and put the number in the first line under it) and
a `metadata.json` carrying at least `question` and `agent`. Then regenerate. Do not edit
the table by hand — it will be overwritten.

## Other places measurements live (this index covers `experiments/` ONLY)

Search these too before re-deriving a number — an index that silently omits half the
corpus reproduces the problem it solves:

| surface | what is there | how to search |
| --- | --- | --- |
| `ai_docs/*.md` | analysis artifacts; many carry measured tables (per-node CPU/wall, ledger stats, corpus counts) | `grep -ril <term> ai_docs/` |
| `ledger/<team>/<host>/<YYYY-MM>.jsonl` | canonical per-machine validate events: wall, CPU, result, counts | `ci-hub ledger qualified-rows`; readers use `ci-hub/ledger/validate_rows.py`, never a raw shard |
| ci-hub history store | per-DAG-node CPU/wall distributions | `python3 ci-hub/history/query.py node-cpu-budgets` |
| task notes | measurements posted but never filed to disk — **the lossy surface** | `tg` search |

## Known-LOST measurements

Recorded here so a future searcher gets a hit instead of silence, and so nobody
"remembers" a number and cites it as measured.

| what | value quoted | when | status |
| --- | --- | --- | --- |
| per-process `cc1plus` peak RSS | ~522 MiB | 2026-08-04 | **UNRECOVERABLE.** Quoted in three dispatches, never written to a task or artifact. Verified 2026-08-05: no artifact under `experiments/` or `ai_docs/` contains it (all `522` hits are unrelated — dpkg file lists and regex benchmarks at 522 MB/s). **Do not cite it as measured. Re-measure and file it.** |

"""


def render(rows) -> str:
    out = [HEADER, f"## Index ({len(rows)} experiment directories)\n",
           "date | slug | what | headline | how | who | artifacts",
           "--- | --- | --- | --- | --- | --- | ---"]
    for r in rows:
        out.append(" | ".join(r))
    out.append("")
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="exit 1 if INDEX.md is stale")
    args = ap.parse_args()
    text = render(build_rows())
    if args.check:
        cur = INDEX.read_text(errors="replace") if INDEX.exists() else ""
        if cur != text:
            print("experiments/INDEX.md is STALE — run: python3 experiments/build-index.py",
                  file=sys.stderr)
            return 1
        print("experiments/INDEX.md is up to date")
        return 0
    INDEX.write_text(text)
    print(f"wrote {INDEX} ({len(text.splitlines())} lines)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
