#!/usr/bin/env python3
"""Assemble a tfa.followup-ledger.v1 for one case from the stress tree.

Every replay directory with a receipt is listed; nothing is filtered by outcome.
The reverted-checkout negative control ran the rebuilt unfixed ELF, so it is an
unfixed-phase run.
"""

import json
import re
import sys
from pathlib import Path

E3 = Path(__file__).resolve().parents[1]
FAIL_SEED = {"thrift": 1, "asyncssl": 11, "httpcoro": 0}


def role(case, name):
    for prefix in ("fail-witness", "pass-witness", "discriminator", "fresh"):
        if name.startswith(prefix):
            return prefix
    if name.startswith("search-seed-"):
        return "discriminator"
    if name.startswith("revert-control-seed-"):
        seed = int(re.match(r"revert-control-seed-(\d+)", name).group(1))
        return "fail-witness" if seed == FAIL_SEED[case] else "discriminator"
    raise SystemExit(f"unknown replay kind: {name}")


def runs(case, dirs, keep=lambda name: True):
    listed, interrupted = [], []
    for base in (d for d in dirs if d.is_dir()):
        for run in sorted(p for p in base.iterdir() if p.is_dir()):
            if not keep(run.name):
                continue
            if (run / "receipt.json").exists():
                listed.append({"role": role(case, run.name), "run": str(run)})
            else:
                interrupted.append(str(run))
    return listed, interrupted


def main(case):
    stress = E3 / "stress" / case
    postfix = stress / "postfix"
    unfixed_sha = json.loads((E3 / "stress/prefix-elves.json").read_text())[case][
        "sha256"
    ]
    patched_sha = (postfix / "build-patched.elf.sha256").read_text().split()[0]
    is_revert = lambda name: name.startswith("revert-control-")
    unfixed, unfixed_interrupted = runs(
        case, [stress / d for d in ("prefix", "prefix-fresh", "prefix-extra")]
    )
    revert, revert_interrupted = runs(case, [postfix], is_revert)
    patched, patched_interrupted = runs(
        case, [postfix], lambda name: not is_revert(name)
    )
    ledger = {
        "schema": "tfa.followup-ledger.v1",
        "case_id": case,
        "candidate_report": str(
            E3 / "investigation" / case / "agent-output/controller-extracted-report.json"
        ),
        "phases": {
            "unfixed": {
                "guest_sha256": unfixed_sha,
                "runs": unfixed + revert,
                "interrupted": unfixed_interrupted + revert_interrupted,
            },
            "patched": {
                "guest_sha256": patched_sha,
                "runs": patched,
                "interrupted": patched_interrupted,
            },
        },
    }
    out = E3 / "followup" / case / "ledger.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("x") as stream:
        stream.write(json.dumps(ledger, indent=2, sort_keys=True) + "\n")
    print(out)


if __name__ == "__main__":
    main(sys.argv[1])
