#!/usr/bin/env python3
"""Exercise issuer attribution and retirement for Hermit pull request 2680."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import pathlib
import sys


HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
REFUSAL_ID = 5420278252


def load_module(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


gate = load_module("refusal_gate_pr2680", ROOT / "ci-hub/bin/changes-requested-gate")
binding = load_module("approval_binding_pr2680", ROOT / "ci-hub/health/approval_binding.py")


def refusal_record() -> dict:
    payload = json.loads(
        (HERE / "hermit-census-cached-input-20260827T0240.json").read_text(
            encoding="utf-8"
        )
    )[0]
    prs = payload["data"]["repository"]["pullRequests"]["nodes"]
    pr = next(row for row in prs if row["number"] == 2680)
    return next(row for row in pr["comments"]["nodes"] if row["databaseId"] == REFUSAL_ID)


def current_row() -> dict:
    census = json.loads((HERE / "current-census.json").read_text(encoding="utf-8"))
    return next(
        row
        for row in census["rows"]
        if row["repository"] == "rrnewton/hermit"
        and row["pr"] == 2680
        and row["comment_id"] == REFUSAL_ID
    )


def withdrawal(agent: str, comment_id: int, refused_sha: str) -> dict:
    return {
        "id": comment_id,
        "body": (
            f"[hermit2, {agent}, unresolved, devbig014, role=reviewer]\n"
            f"CHANGES-REQUESTED-WITHDRAWN-AT: codex {refused_sha} BY {agent}\n"
            f"RETIRES #{REFUSAL_ID}"
        ),
    }


def binding_unanswered(records: list[dict]) -> list[tuple[str, str | None]]:
    unanswered: list[tuple[str, str | None]] = []
    binding.lane_shas(records, "codex", unanswered=unanswered)
    return unanswered


def gate_result(records: list[dict], head: str) -> tuple[int, str]:
    return gate.evaluate(
        [record["body"] for record in records],
        head,
        ids=[record["id"] for record in records],
    )


def main() -> int:
    source = refusal_record()
    refusal = {"id": source["databaseId"], "body": source["body"]}
    row = current_row()
    refused_sha = row["refused_sha"]
    head = row["head"]
    other = withdrawal("hermit-121", 9000000001, refused_sha)
    owner = withdrawal("hermit-016", 9000000002, refused_sha)

    issuer_from_gate = gate._issuer_of_comment(refusal["body"])
    issuer_from_binding = binding.comment_issuer(refusal["body"])
    unanswered = binding_unanswered([refusal])
    current_head_issuers = [issuer for sha, issuer in unanswered if sha == head]
    other_unanswered = binding_unanswered([refusal, other])
    owner_unanswered = binding_unanswered([refusal, owner])
    other_gate = gate_result([refusal, other], head)
    owner_gate = gate_result([refusal, owner], head)

    print(f"issuer_from_gate={issuer_from_gate}")
    print(f"issuer_from_binding={issuer_from_binding}")
    print(f"unanswered={unanswered}")
    print(f"current_head_issuers={current_head_issuers}")
    print(f"other_agent_unanswered={other_unanswered}")
    print(f"other_agent_gate_rc={other_gate[0]} reason={other_gate[1]}")
    print(f"issuer_unanswered={owner_unanswered}")
    print(f"issuer_gate_rc={owner_gate[0]} reason={owner_gate[1]}")

    assert issuer_from_gate == "hermit-016"
    assert issuer_from_binding == "hermit-016"
    assert unanswered == [(refused_sha, "hermit-016")]
    assert current_head_issuers == []
    assert other_unanswered == unanswered
    assert other_gate[0] == gate.BLOCKED
    assert "belongs to hermit-016" in other_gate[1]
    assert owner_unanswered == []
    assert owner_gate[0] == gate.OK
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
