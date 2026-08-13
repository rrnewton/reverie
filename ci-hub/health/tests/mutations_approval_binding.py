#!/usr/bin/env python3
"""The enumerated mutation set for approval_binding.py, replayable by anyone.

WHY THIS FILE EXISTS. The three-state parser fix (16df25e9) claimed "7
mutations, 7 killed" and enumerated them NOWHERE. hermit-bpf-post, reviewing
it, could not verify that claim and had to derive its own nine mutations from
scratch. Claude-family reviewers are the scarcest resource in this fleet and
that cost one of them an hour rebuilding evidence the author already had. An
unenumerated mutation claim is an assertion, not a result.

WHAT IT CHECKS THAT AN AD-HOC HARNESS DOES NOT. Each mutation must not merely
FAIL the suite -- it must fail it through a NAMED assertion, and this runner
verifies the expected case name is among the failures. That distinction is the
whole point: a row-key deletion used to kill via a raw KeyError traceback that
died before the summary line, which is a real kill with a worthless report. A
reader cannot tell a deliberate catch from an unrelated crash, so "the suite
went red" is not sufficient evidence that the bracket works.

THREE PROPERTIES, EACH LEARNED FROM A HARNESS THAT LIED TODAY:

  1. NEVER MUTATES THE REAL TREE. Every mutation is applied to a COPY in a
     temp directory. An earlier harness was killed by a timeout mid-mutation,
     left the source defective, and its "pristine" snapshot was then taken
     FROM THE DEFECTIVE FILE -- so two later mutations reported failures
     belonging to the contaminated baseline. Nothing here can contaminate
     anything, because the original is never written to.

  2. ANCHORED APPLY. A mutation asserts its INTENDED SITE changed, not merely
     that the file changed. `if role not in ROLES:` appeared in two functions
     and a non-global replace hit the wrong one, reporting SURVIVED for a
     defect never introduced where it mattered.

  3. NAMED KILL REQUIRED. A mutation that kills only via traceback is reported
     as UNNAMED-KILL, not as a pass. That is this file's own subject.

Usage:
  mutations_approval_binding.py            # replay all, report per mutation
  mutations_approval_binding.py --list     # just enumerate them
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
HEALTH = HERE.parent
TARGET = HEALTH / "approval_binding.py"
SUITE = HERE / "test_approval_binding_grammar.py"

# anchor: text locating the FUNCTION, so a same-text line elsewhere is not hit.
# expect: substring of the case name that MUST appear in a FAIL line.
MUTATIONS = [
    {
        "id": "M1",
        "why": "classify_lane maps a rejection back onto NEVER",
        "anchor": "def classify_lane",
        "old": "    if verdict.rejected_sha is not None:",
        "new": "    if False:",
        "expect": "DIRECTION 1: a rejection at head is distinguishable from silence",
    },
    {
        "id": "M2",
        "why": "the scan stops capturing which SHA the rejection named",
        "anchor": "def lane_verdict",
        "old": "            rejected_sha = rejection_here or None",
        "new": "            rejected_sha = None",
        "expect": "the rejection reports WHICH head it named",
    },
    {
        "id": "M3",
        "why": "found.clear() removed -- DIRECTION 2, revocation broken",
        "anchor": "def lane_verdict",
        "old": "            found.clear()",
        "new": "            pass",
        "expect": "revokes an earlier exact-head approval",
    },
    {
        "id": "M4",
        "why": "a later approval no longer drops the superseded rejection",
        "anchor": "def lane_verdict",
        "old": "                rejected_sha = None\n                rejected_idx = -1",
        "new": "                pass",
        "expect": "a later approval re-establishes authority and drops the stale rejection",
    },
    {
        "id": "M5",
        "why": "classify_approval leaks the new vocabulary into the GATE seam",
        "anchor": "def classify_approval",
        "old": "    if not comment_shas:\n        return UNDETERMINED",
        "new": "    if not comment_shas:\n        return NEVER",
        "expect": "no binding line stays UNDETERMINED",
    },
    {
        "id": "M6",
        "why": "rows stop carrying lane_state (value blanked)",
        "anchor": "def lane_row",
        "old": '        "lane_state": classify_lane(head, verdict),',
        "new": '        "lane_state": None,',
        "expect": "the emitted row carries BOTH the gate state and the lane state",
    },
    {
        "id": "M6b",
        "why": "rows stop carrying lane_state (key DELETED) -- the raw-traceback case",
        "anchor": "def lane_row",
        "old": '        "lane_state": classify_lane(head, verdict),\n',
        "new": "",
        "expect": "the emitted row carries exactly the expected keys",
    },
    {
        "id": "M7",
        "why": "REJECTED_SUPERSEDED collapsed into REJECTED_CURRENT",
        "anchor": "def classify_lane",
        "old": "            if verdict.rejected_sha == head",
        "new": "            if True",
        "expect": "a rejection at an older head is REJECTED_SUPERSEDED",
    },
    {
        "id": "M8",
        "why": "a rejection for the OTHER lane wrongly revokes this one",
        "anchor": "def lane_verdict",
        "old": "            if current is not None and current.group(1).lower() == lane:",
        "new": "            if current is not None:",
        "expect": "a rejection for the other lane leaves this lane bound",
    },
]


def apply_mutation(path: Path, mutation: dict) -> None:
    """Anchored replace. Raises if the INTENDED site did not change."""
    text = path.read_text()
    index = text.index(mutation["anchor"])
    head, tail = text[:index], text[index:]
    out = tail.replace(mutation["old"], mutation["new"], 1)
    if out == tail:
        raise AssertionError(f"{mutation['id']}: intended site not mutated")
    path.write_text(head + out)


def run_suite(root: Path) -> tuple[int, str]:
    """Run the suite against the module IN ``root``, named explicitly.

    APPROVAL_BINDING_TOOL is passed rather than relying on cwd. The suite
    resolves a RELATIVE default path, so a runner that forgets to set either
    one silently loads the REAL module and every mutation reports SURVIVED --
    which is exactly what the first draft of this file did, reporting 9/9
    survived for mutations known to kill. An absolute, explicit target cannot
    drift with the working directory.
    """

    env = dict(os.environ)
    env["APPROVAL_BINDING_TOOL"] = str(root / "ci-hub/health/approval_binding.py")
    done = subprocess.run(
        [sys.executable, str(root / "ci-hub/health/tests/test_approval_binding_grammar.py")],
        capture_output=True,
        text=True,
        timeout=120,
        cwd=str(root),
        env=env,
    )
    return done.returncode, done.stdout + done.stderr


# A mutation whose ONLY purpose is to prove the runner is wired to the copy it
# mutated. If this survives, the harness is reading some other file and every
# verdict below is meaningless -- a green baseline does NOT detect that, because
# the unmutated real module is green too.
WIRING_CANARY = {
    "id": "CANARY",
    "why": "runner wiring control -- must always be killed",
    "anchor": "def classify_approval",
    "old": "    if not comment_shas:\n        return UNDETERMINED",
    "new": "    if not comment_shas:\n        return BOUND_CURRENT",
    "expect": "no binding line stays UNDETERMINED",
}

# THE SECOND DIRECTION OF THE BRACKET. Making row-key mutations produce a named
# FAIL is only half the requirement; the other half is that an UNRELATED crash
# must STILL read as a crash and must never be absorbed into the mutation-kill
# path. This injects a fault with nothing to do with any assertion. It must be
# reported UNNAMED-KILL -- never KILLED, which would mean the runner had started
# dressing crashes up as deliberate catches.
CRASH_CONTROL = {
    "id": "CRASH",
    "why": "unrelated-crash control -- must report UNNAMED-KILL, never KILLED",
    "anchor": "def lane_verdict",
    "old": "    if malformed is None:",
    "new": "    raise RuntimeError('unrelated harness fault')\n    if malformed is None:",
    "expect": "(none -- this must not be caught by any named assertion)",
}


def replay(mutation: dict) -> tuple[str, str]:
    """Return (verdict, detail). Never touches the real tree."""
    with tempfile.TemporaryDirectory(prefix=f"mut-{mutation['id']}-") as tmp:
        root = Path(tmp)
        (root / "ci-hub/health/tests").mkdir(parents=True)
        shutil.copy(TARGET, root / "ci-hub/health/approval_binding.py")
        shutil.copy(SUITE, root / "ci-hub/health/tests/test_approval_binding_grammar.py")
        try:
            apply_mutation(root / "ci-hub/health/approval_binding.py", mutation)
        except (AssertionError, ValueError) as error:
            return "NOT-APPLIED", str(error)[:120]
        code, output = run_suite(root)
        if code == 0:
            return "SURVIVED", "suite stayed green"
        failures = [
            line.strip() for line in output.splitlines() if line.strip().startswith("FAIL")
        ]
        if not failures:
            # Red, but with nothing named: exactly the defect this file exists
            # for. Reported distinctly, never as a pass.
            first = next((l for l in output.splitlines() if l.strip()), "")
            return "UNNAMED-KILL", f"no FAIL line; suite died with: {first[:90]}"
        if not any(mutation["expect"] in line for line in failures):
            return "WRONG-CATCH", f"expected {mutation['expect']!r}; got {failures[0][:90]}"
        return "KILLED", f"{len(failures)} named FAIL(s)"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="enumerate without running")
    args = parser.parse_args()

    if args.list:
        for mutation in MUTATIONS:
            print(f"{mutation['id']:5} {mutation['why']}")
        return 0

    # A baseline that is not green makes every result below meaningless.
    with tempfile.TemporaryDirectory(prefix="mut-baseline-") as tmp:
        root = Path(tmp)
        (root / "ci-hub/health/tests").mkdir(parents=True)
        shutil.copy(TARGET, root / "ci-hub/health/approval_binding.py")
        shutil.copy(SUITE, root / "ci-hub/health/tests/test_approval_binding_grammar.py")
        code, output = run_suite(root)
    if code != 0:
        print("BASELINE IS NOT GREEN -- every mutation result would be meaningless.")
        print(output[-800:])
        return 2
    print(f"baseline green: {output.strip().splitlines()[-1]}")

    # WIRING CONTROL, before any real verdict is reported. A green baseline
    # proves the suite passes on SOME module; it does not prove the suite reads
    # the module this runner mutates. Only a mutation that must fail can.
    canary_verdict, canary_detail = replay(WIRING_CANARY)
    if canary_verdict != "KILLED":
        print(f"WIRING CONTROL FAILED: canary {canary_verdict} ({canary_detail}).")
        print("The runner is not reading the module it mutates; results suppressed.")
        return 2
    print(f"wiring control: canary KILLED ({canary_detail})")

    crash_verdict, crash_detail = replay(CRASH_CONTROL)
    if crash_verdict != "UNNAMED-KILL":
        print(f"CRASH CONTROL FAILED: expected UNNAMED-KILL, got {crash_verdict}.")
        print(f"  {crash_detail}")
        print("An unrelated crash is being absorbed into the mutation-kill path.")
        return 2
    print(f"crash control: unrelated fault reported UNNAMED-KILL ({crash_detail})")

    bad = 0
    for mutation in MUTATIONS:
        verdict, detail = replay(mutation)
        if verdict != "KILLED":
            bad += 1
        print(f"  {mutation['id']:5} {verdict:13} {mutation['why']}")
        print(f"        {detail}")
    print(
        f"mutations: {len(MUTATIONS) - bad} killed by a NAMED assertion, "
        f"{bad} not"
    )
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
