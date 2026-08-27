#!/usr/bin/env python3
"""Exercise the retirement routes for Reverie pull request 503's old refusal."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib
import sys


ROOT = pathlib.Path(__file__).resolve().parents[3]


def load_module(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


gate = load_module("refusal_gate_pr503", ROOT / "ci-hub/bin/changes-requested-gate")
binding = load_module("approval_binding_pr503", ROOT / "ci-hub/health/approval_binding.py")

REFUSAL_ID = 5437464601
WITHDRAWAL_ID = 5438402110
REFUSED_HEAD = "c73b7e4e0a29bfe1f17a4b387b847052e527eac2"
CURRENT_HEAD = "e3ccb71500f5cd13d45eba849100f2aae4265bc0"
REFUSAL = (
    f"CHANGES-REQUESTED-AT: codex {REFUSED_HEAD}\n\n"
    "role=adversarial-reviewer lane=codex"
)
ORDINARY_WITHDRAWAL = (
    "[hermit2, hermit-131, unresolved, devbig014, role=reviewer]\n"
    f"CHANGES-REQUESTED-WITHDRAWN-AT: codex {REFUSED_HEAD} BY hermit-131"
)
EXACT_WITHDRAWAL = ORDINARY_WITHDRAWAL + f"\nRETIRES #{REFUSAL_ID}"


def approval_binding_state(withdrawal: str) -> list[tuple[str, str | None]]:
    unanswered: list[tuple[str, str | None]] = []
    binding.lane_shas(
        [
            {"id": REFUSAL_ID, "body": REFUSAL},
            {"id": WITHDRAWAL_ID, "body": withdrawal},
        ],
        "codex",
        unanswered=unanswered,
    )
    return unanswered


def main() -> int:
    ordinary = approval_binding_state(ORDINARY_WITHDRAWAL)
    exact = approval_binding_state(EXACT_WITHDRAWAL)
    with_ids = gate.evaluate(
        [REFUSAL, EXACT_WITHDRAWAL],
        CURRENT_HEAD,
        ids=[REFUSAL_ID, WITHDRAWAL_ID],
    )
    without_ids = gate.evaluate([REFUSAL, EXACT_WITHDRAWAL], CURRENT_HEAD)

    print(f"ordinary_withdrawal_unanswered={ordinary}")
    print(f"exact_retirement_unanswered={exact}")
    print(f"gate_with_ids_rc={with_ids[0]} reason={with_ids[1]}")
    print(f"gate_without_ids_rc={without_ids[0]} reason={without_ids[1]}")

    assert ordinary == [(REFUSED_HEAD, None)]
    assert exact == []
    assert with_ids[0] == gate.OK
    assert without_ids[0] == gate.BLOCKED
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
