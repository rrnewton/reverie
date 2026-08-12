#!/usr/bin/env python3
"""Selection and persistence policy for the recurring `Periodic showcase`.

The owner wants a periodic showcase of what NEWLY works, not manufactured
novelty. That makes selection the whole problem, and selection is a pure
function of two things -- the compat scorecard history and what was shown last
time -- so it lives here, in a plain script with real tests, rather than inside
an ORC workflow body that can only be exercised by running the fleet.

The workflow (.orc/plugins/periodic-showcase/index.ts) is deliberately thin:
poll `select`, and if it reports a delta, wake the coordinator with the
instruction this script generated, then call `record-shown`. Everything a test
would want to assert is therefore reachable without ORC, without a coordinator,
and without sending a message to anybody.

    select        decide whether a showcase is due; print the instruction
    record-shown  persist what was actually shown, so it is not repeated
    state         print the persisted state

Exit codes are the interface the workflow branches on:
    0  a showcase is due; stdout is the coordinator instruction
    1  nothing newly works -- report that plainly, do NOT fabricate a demo
    2  no usable scorecard history (cannot decide; not the same as "nothing new")
    3  usage or internal error
"""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path
import sys
from typing import TextIO


EXIT_SHOWCASE_DUE = 0
EXIT_NOTHING_NEW = 1
EXIT_NO_HISTORY = 2
EXIT_ERROR = 3

# Every scorecard the compat envelope writes. Each is optional: a missing file is
# not an error, it is one fewer source. Ordered so the broadest corpus is scanned
# first purely for stable, reproducible tie-breaking.
SCORECARDS = (
    Path("compat-envelope/fullcorpus-scorecard.csv"),
    Path("compat-envelope/scorecard.csv"),
    Path("compat-envelope/e9patch-scorecard.csv"),
    Path("compat-envelope/reverie-scorecard.csv"),
)
STATE_PATH = Path("ignored/periodic-showcase-state.json")

# `pass` is the only outcome meaning the cell ran and worked. Everything else
# observed in the corpus (fail, diverge, skip, unavailable, gap) is not-green.
GREEN = "pass"

# NOT ALL NOT-GREEN OUTCOMES MEAN THE SAME THING, and conflating them is how a
# showcase manufactures novelty.
#
#   fail / diverge  -> the cell RAN and got the wrong answer. `X -> pass` here is
#                      a genuine capability gain: the same build now behaves.
#   unavailable / skip / gap
#                   -> the cell never ran. `X -> pass` may mean nothing changed
#                      in Hermit at all -- most often the backend was simply
#                      COMPILED IN this time. The demo-presentation-cycle
#                      evidence says so in capitals: "DO NOT MISREAD
#                      'unavailable' AS 'BROKEN' ... That is NOT COMPILED IN,
#                      not broken" (a build without --features third-party-backends
#                      reports e9patch and sabre unavailable while both work).
#
# Measured on the current corpus (2284 rows, 1786 cells, 4 scorecards): there is
# no `fail -> pass` anywhere; the ONLY newly-green transition is
# `unavailable -> pass` (6 sabre cells). So the weak class is not hypothetical --
# it is currently the only thing a naive selector would ever find, and it would
# announce a build-flag change as a capability win.
RAN_AND_WAS_WRONG = frozenset({"fail", "diverge"})
NEVER_RAN = frozenset({"unavailable", "skip", "gap"})
CAPABILITY = "capability"
COVERAGE = "coverage"

# Demos that already exist and may be reused for a given backend, so the
# generated instruction can point at one instead of inviting a new artifact.
# "If something is good enough to be a permanent demo, PITCH IT rather than
# adding it" -- demo-presentation-cycle.
DEMO_HINTS = {
    "ptrace": "hermit/demos/01-deterministic-run.sh",
    "dbi": "hermit/demos/01-deterministic-run.sh",
    "sabre": "hermit/demos/01-deterministic-run.sh",
    "e9patch": "hermit/demos/01-deterministic-run.sh",
    "liteinst": "hermit/demos/01-deterministic-run.sh",
    "kvm": "hermit/demos/05-qemu-boot.py",
}


def cell_key(row: dict[str, str]) -> str:
    """Stable identity of one compat-matrix cell, independent of run."""
    return "{}|{}|{}".format(
        row.get("test_id", ""), row.get("test_mode", ""), row.get("backend", "")
    )


def run_order(row: dict[str, str]) -> tuple:
    """Sort key placing runs in recorded chronological order.

    `run_utc` is written as `@<epoch>`; sort it numerically when it parses that
    way and lexically otherwise, so a future format change degrades to a stable
    order instead of an exception.
    """
    raw = str(row.get("run_utc", "")).lstrip("@")
    try:
        return (0, float(raw), str(row.get("run_id", "")))
    except ValueError:
        return (1, 0.0, str(row.get("run_utc", "")) + str(row.get("run_id", "")))


def read_scorecards(root: Path, sources=SCORECARDS) -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    for relative in sources:
        path = root / relative
        if not path.is_file():
            continue
        with path.open(newline="") as handle:
            for row in csv.DictReader(handle):
                if row.get("test_id") and row.get("backend"):
                    row["_source"] = str(relative)
                    rows.append(row)
    return rows


def newly_green_cells(rows: list[dict[str, str]]) -> list[dict[str, str]]:
    """Cells whose latest recorded outcome is green and whose previous was not.

    HISTORY IS SCOPED PER SCORECARD FILE, deliberately. 442 of 1786 cells
    (measured) appear in more than one scorecard, and the scorecards cover
    different corpora/lanes written at different times -- so interleaving them by
    timestamp invents transitions that only record which FILE was regenerated
    last. Comparing a cell against its own history in its own scorecard is the
    only comparison whose two sides are alike.

    A cell seen for the FIRST time is not "newly green": there is no prior
    observation to have improved on, so counting it would manufacture novelty out
    of a widened corpus -- the exact failure the owner objected to.
    """
    history: dict[tuple[str, str], list[dict[str, str]]] = {}
    for row in rows:
        history.setdefault((row.get("_source", ""), cell_key(row)), []).append(row)

    deltas = []
    for (source, key), entries in history.items():
        entries.sort(key=run_order)
        if len(entries) < 2:
            continue
        latest, previous = entries[-1], entries[-2]
        before = previous.get("outcome", "")
        if latest.get("outcome") != GREEN or before == GREEN:
            continue
        if before in RAN_AND_WAS_WRONG:
            kind = CAPABILITY
        elif before in NEVER_RAN:
            kind = COVERAGE
        else:
            # An outcome vocabulary we have not seen. Treat it as the weaker
            # class rather than silently promoting it to a capability claim.
            kind = COVERAGE
        deltas.append(
            {
                "key": key,
                "kind": kind,
                "test_id": latest.get("test_id", ""),
                "test_mode": latest.get("test_mode", ""),
                "backend": latest.get("backend", ""),
                "from_outcome": before,
                "to_outcome": latest.get("outcome", ""),
                "hermit_sha": latest.get("hermit_sha", ""),
                "reverie_sha": latest.get("reverie_sha", ""),
                "run_id": latest.get("run_id", ""),
                "run_utc": latest.get("run_utc", ""),
                "source": source,
                "demo_hint": DEMO_HINTS.get(latest.get("backend", ""), ""),
            }
        )
    # Capability gains first, then deterministic order: the same inputs must
    # always select the same cell, or the "already shown" check means nothing.
    deltas.sort(
        key=lambda d: (
            0 if d["kind"] == CAPABILITY else 1,
            d["backend"],
            d["test_id"],
            d["test_mode"],
        )
    )
    return deltas


def load_state(root: Path, state_path: Path = STATE_PATH) -> dict:
    path = root / state_path
    try:
        with path.open() as handle:
            state = json.load(handle)
    except (OSError, json.JSONDecodeError):
        return {"shown": []}
    if not isinstance(state, dict) or not isinstance(state.get("shown"), list):
        return {"shown": []}
    return state


def already_shown(state: dict) -> set[tuple[str, str]]:
    """(cell key, hermit SHA) pairs already showcased.

    Keyed on BOTH so the same cell going green again at a later SHA is a new
    showcase, while re-running the selector at the same tip is not.
    """
    pairs = set()
    for entry in state.get("shown", []):
        if isinstance(entry, dict):
            pairs.add((str(entry.get("key", "")), str(entry.get("hermit_sha", ""))))
    return pairs


def select(root: Path, sources=SCORECARDS, state_path: Path = STATE_PATH) -> tuple[int, dict]:
    rows = read_scorecards(root, sources)
    if not rows:
        return EXIT_NO_HISTORY, {"reason": "no scorecard rows found"}
    deltas = newly_green_cells(rows)
    if not deltas:
        return EXIT_NOTHING_NEW, {"reason": "no cell went from not-green to green"}
    seen = already_shown(load_state(root, state_path))
    fresh = [d for d in deltas if (d["key"], d["hermit_sha"]) not in seen]
    if not fresh:
        return EXIT_NOTHING_NEW, {
            "reason": "every newly-green cell was already showcased",
            "suppressed": len(deltas),
        }
    return EXIT_SHOWCASE_DUE, {"selected": fresh[0], "also_new": fresh[1:]}


def instruction(selected: dict, also_new: list[dict]) -> str:
    """The coordinator-facing wake body.

    Carries the evidence contract as literal text, because the wake is the only
    thing the doing agent is guaranteed to read.
    """
    demo = selected["demo_hint"] or "an existing demo under hermit/demos/"
    capability = selected["kind"] == CAPABILITY
    headline = (
        "Periodic showcase: a compatibility cell that RAN AND FAILED now passes."
        if capability
        else "Periodic showcase: a compatibility cell that never ran now passes "
        "-- VERIFY THE CAUSE BEFORE CLAIMING IT."
    )
    lines = [
        headline,
        "",
        "WHAT CHANGED (from the recorded compat scorecard, not from inference):",
        "  cell     {} [{}] on backend {}".format(
            selected["test_id"], selected["test_mode"], selected["backend"]
        ),
        "  outcome  {} -> {}  [{}]".format(
            selected["from_outcome"], selected["to_outcome"], selected["kind"]
        ),
        "  at       hermit {}".format(selected["hermit_sha"] or "(sha not recorded)"),
        "  reverie  {}".format(selected["reverie_sha"] or "(unchanged/not recorded)"),
        "  source   {} run {}".format(selected["source"], selected["run_id"]),
        "",
    ]
    if not capability:
        lines += [
            "READ THIS FIRST -- THE DELTA IS A COVERAGE CHANGE, NOT YET A CAPABILITY.",
            "The previous outcome was `{}`, which means the cell DID NOT RUN.".format(
                selected["from_outcome"]
            ),
            "The most common cause is a build-configuration change, not a Hermit fix:",
            "a build without `--features third-party-backends` reports sabre and",
            "e9patch as `unavailable` even though both work. Establish which it is",
            "BEFORE you present it, and say so either way:",
            "  - if Hermit/the backend genuinely gained the ability, showcase that",
            "  - if the cell merely got compiled in or enabled, say exactly that;",
            "    'we now measure this cell' is an honest, useful showcase and is NOT",
            "    a capability claim. Do not dress it up as one.",
            "",
        ]
    lines += [
        "WHAT TO DO: showcase THIS delta. Reuse {} if it exercises the cell;".format(demo),
        "do not invent a new demo and do not add files. If the cell turns out not to",
        "reproduce, say so plainly -- a refuted delta is a valid, useful showcase.",
        "",
        "PLAIN-LANGUAGE REQUIREMENT (before every command block, in prose):",
        "  - what the program under test actually does",
        "  - what Hermit and this backend are doing to it",
        "  - what NEWLY works that did not before",
        "  - what the observed output means, and why it matters",
        "  Shell is reproducible evidence, not the explanation.",
        "",
        "EVIDENCE CONTRACT (from demo-presentation-cycle; all of it binds):",
        "  - state the exact commit hash the demo was run against, plus backend and env",
        "  - every command copy-pastable and ACTUALLY RUN by you",
        "  - a snippet of REAL captured output for each; if you did not run it, it does",
        "    not go in -- no 'this should work', no reconstructed examples",
        "  - anything you tried that FAILED goes in too, with the error",
        "  - do not pad; an honest short list is the deliverable",
        "",
        "Post it as a task note on demo-presentation-cycle headed `DEMO-RUN @<UTC>`,",
        "tag it implemented, and leave that task in_progress.",
        "",
        "After the showcase is posted, record it so it is never repeated:",
        "  scripts/periodic_showcase.py record-shown --key '{}' --sha '{}'".format(
            selected["key"], selected["hermit_sha"]
        ),
    ]
    if also_new:
        lines += [
            "",
            "Also newly green this cycle ({}), held back for a later showcase:".format(
                len(also_new)
            ),
        ]
        lines += [
            "  {} [{}] on {} ({} -> pass)".format(
                d["test_id"], d["test_mode"], d["backend"], d["from_outcome"]
            )
            for d in also_new[:5]
        ]
    return "\n".join(lines)


def record_shown(root: Path, key: str, sha: str, state_path: Path = STATE_PATH) -> int:
    state = load_state(root, state_path)
    for entry in state["shown"]:
        if isinstance(entry, dict) and entry.get("key") == key and entry.get(
            "hermit_sha"
        ) == sha:
            return EXIT_SHOWCASE_DUE
    state["shown"].append({"key": key, "hermit_sha": sha})
    path = root / state_path
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)
    return EXIT_SHOWCASE_DUE


def default_root() -> Path:
    return Path(__file__).resolve().parent.parent


def main(argv: list[str] | None = None, out: TextIO = sys.stdout) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=default_root())
    subparsers = parser.add_subparsers(dest="command", required=True)
    chooser = subparsers.add_parser("select", description="decide whether a showcase is due", help="decide whether a showcase is due")
    chooser.add_argument("--json", action="store_true", help="emit the raw selection")
    recorder = subparsers.add_parser("record-shown", description="persist what was showcased", help="persist what was showcased")
    recorder.add_argument("--key", required=True)
    recorder.add_argument("--sha", required=True, default="")
    subparsers.add_parser("state", description="print the persisted state", help="print the persisted state")
    args = parser.parse_args(argv)
    root = args.root.resolve()

    if args.command == "record-shown":
        return record_shown(root, args.key, args.sha)
    if args.command == "state":
        print(json.dumps(load_state(root), indent=2, sort_keys=True), file=out)
        return EXIT_SHOWCASE_DUE

    code, payload = select(root)
    if args.json:
        print(json.dumps(payload, indent=2, sort_keys=True), file=out)
        return code
    if code == EXIT_SHOWCASE_DUE:
        print(instruction(payload["selected"], payload["also_new"]), file=out)
    else:
        print("no periodic showcase is due: " + payload["reason"], file=out)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
