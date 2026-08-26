#!/usr/bin/env python3
"""Detect refusals discharged by SHA MOTION rather than by an answer.

A refusal binds one sha. When the author pushes, it stops binding -- whether or
not the objection was addressed, and nothing records which. So an answered
finding and an abandoned one are indistinguishable afterwards. This pairs every
refusal with the withdrawals at the SAME sha; an excess at a NO-LONGER-CURRENT
sha is a CANDIDATE ORPHAN.

⚠️ CANDIDATES, NOT DEFECTS. This cannot tell answered from abandoned -- that it
cannot is the finding. It is also a LOWER BOUND: it is blind to prose refusals
carrying no marker.

Three outcomes per pull request: OK / CANDIDATE-ORPHANS / INDETERMINATE.
⚠️ AN INDETERMINATE IS NOT AN OK, and it is printed rather than counted away.

    ci-hub/health/orphaned_refusals.py rrnewton/hermit 2216 2670 ...
    ci-hub/health/orphaned_refusals.py --self-test
"""

import collections
import importlib.machinery
import importlib.util
import json
import pathlib
import subprocess
import sys
import time

# Resolve the gate as a SIBLING, never an absolute path. This previously read
# `/tmp/hermit-030-cihub/...` -- one agent's scratch directory, which exists on
# exactly one box and is reaped. Everywhere else the import raises, main()
# catches it, and every pull request reports INDETERMINATE. A detector that
# cannot run still prints a tidy summary line, so the failure reads as "nothing
# found" rather than "nothing looked".
_GATE = pathlib.Path(__file__).resolve().parent.parent / "bin" / "changes-requested-gate"
_loader = importlib.machinery.SourceFileLoader("crg", str(_GATE))
crg = importlib.util.module_from_spec(importlib.util.spec_from_loader("crg", _loader))
_loader.exec_module(crg)


def pair(bodies, head):
    """(refusals, withdrawals, orphans) keyed by sha prefix.

    Pure: no network, so the self-test drives exactly what production drives.
    """
    refusals, withdrawals = collections.Counter(), collections.Counter()
    for body in bodies:
        # ⚠️ markers() yields THREE fields (kind, lane, sha). Unpacking two
        # raises ValueError, which main() catches as INDETERMINATE -- so the
        # bug would have reported "cannot decide" on every pull request that
        # had any marker at all, which is every interesting one.
        for kind, _lane, sha in crg.markers([body]):
            if not sha:
                continue
            key = sha[:8]
            if "WITHDRAWN" in str(kind).upper():
                withdrawals[key] += 1
            elif "CHANGES" in str(kind).upper():
                refusals[key] += 1
    orphans = {
        sha: refusals[sha] - withdrawals.get(sha, 0)
        for sha in refusals
        if not head.startswith(sha) and refusals[sha] - withdrawals.get(sha, 0) > 0
    }
    return refusals, withdrawals, orphans


def _gh(args):
    proc = subprocess.run(["with-proxy", "gh"] + args, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip()[:160] or f"gh exited {proc.returncode}")
    return json.loads(proc.stdout)


def scan(repo, pr):
    data = _gh(["pr", "view", str(pr), "-R", repo, "--json", "comments,headRefOid"])
    head = data["headRefOid"]
    refusals, withdrawals, orphans = pair([c["body"] for c in data["comments"]], head)
    return head, refusals, withdrawals, orphans


def main(repo, prs, pace=2.5):
    total_orphans = total_prs = indeterminate = 0
    for index, pr in enumerate(prs):
        if index:
            time.sleep(pace)  # PACED: a burst is what tripped the secondary rate limit
        try:
            head, refusals, _withdrawals, orphans = scan(repo, pr)
        except Exception as exc:  # noqa: BLE001 -- reported, never silently skipped
            print(f"  #{pr:<5} INDETERMINATE -- {exc}")
            indeterminate += 1
            continue
        if orphans:
            total_prs += 1
            total_orphans += sum(orphans.values())
            detail = "  ".join(f"{s}:{n}" for s, n in sorted(orphans.items()))
            print(
                f"  #{pr:<5} CANDIDATE-ORPHANS {sum(orphans.values())}"
                f"  at dead shas [{detail}]"
                f"   (live at head {head[:8]}: {refusals.get(head[:8], 0)})"
            )
        else:
            print(f"  #{pr:<5} OK  (live refusals at head {head[:8]}: {refusals.get(head[:8], 0)})")
    print(
        f"  ---- scanned {len(prs)}  PRs with candidates: {total_prs}  "
        f"candidate orphans: {total_orphans}  INDETERMINATE: {indeterminate}"
    )
    return 0 if not indeterminate else 2


def self_test():
    head = "f" * 40
    dead, other = "a" * 40, "b" * 40

    # MUST FIND: a refusal at a sha that is no longer the head, never withdrawn.
    _r, _w, orphans = pair([f"CHANGES-REQUESTED-AT: claude {dead}"], head)
    assert orphans == {dead[:8]: 1}, orphans

    # MUST NOT FIND: withdrawn at the same sha -- answered, so discharged.
    _r, _w, orphans = pair(
        [
            f"CHANGES-REQUESTED-AT: claude {dead}",
            f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {dead}",
        ],
        head,
    )
    assert orphans == {}, orphans

    # MUST NOT FIND: a refusal at the CURRENT head is live, not orphaned.
    _r, _w, orphans = pair([f"CHANGES-REQUESTED-AT: claude {head}"], head)
    assert orphans == {}, orphans

    # ⚠️ A WITHDRAWAL AT A DIFFERENT SHA DOES NOT DISCHARGE IT. Pairing is
    # per-sha; cross-sha crediting is how a real orphan disappears.
    _r, _w, orphans = pair(
        [
            f"CHANGES-REQUESTED-AT: claude {dead}",
            f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {other}",
        ],
        head,
    )
    assert orphans == {dead[:8]: 1}, orphans

    # An approval is not a withdrawal, and must not discharge a refusal.
    _r, _w, orphans = pair(
        [f"CHANGES-REQUESTED-AT: claude {dead}", f"APPROVED-AT: claude {dead}"], head
    )
    assert orphans == {dead[:8]: 1}, orphans

    # CONTROL THAT MUST STAY EMPTY: no markers at all yields nothing, so a
    # passing suite above cannot be an artefact of everything matching.
    _r, _w, orphans = pair(["no markers here at all"], head)
    assert orphans == {}, orphans

    print("PASS: orphaned_refusals pairs per-sha, credits only withdrawals, and "
          "treats a live refusal and a cross-sha withdrawal as non-orphans")
    return 0


if __name__ == "__main__":
    if "--self-test" in sys.argv[1:]:
        sys.exit(self_test())
    if len(sys.argv) < 3:
        print(__doc__.strip(), file=sys.stderr)
        sys.exit(1)
    sys.exit(main(sys.argv[1], [int(x) for x in sys.argv[2:]]))
