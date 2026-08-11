#!/usr/bin/env python3
"""Regression checks for target-repository binding in PR pin preparation."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKOUT_HELPER = ROOT / "scripts/checkout-hermit-pr-latest-reverie.sh"
HERMIT = ROOT / "hermit"
CHECKER = HERMIT / "scripts/check-reverie-pin.rs"


def _print_pin_invocations(source: str) -> list[str]:
    return [line.strip() for line in source.splitlines() if "--print-pin" in line]


def _invocation_binds_target_repo(invocation: str) -> bool:
    return '--repo "$repo"' in invocation


def _has_complete_task_plumbing(source: str) -> bool:
    return (
        "--task <stable-task-id>" in source
        and '--task "$task"' in source
        and source.count('-m "Task: $task"') == 2
    )


def _validator_uses_requested_agent(source: str) -> bool:
    validate_block = source.split('"$ci_hub" validate-run', 1)[-1]
    return '--agent "$agent"' in validate_block and "--agent hermit-ptw" not in source


def test_checkout_helper_binds_print_pin_to_target_repo() -> None:
    source = CHECKOUT_HELPER.read_text()
    invocations = _print_pin_invocations(source)

    assert len(invocations) == 1
    assert _invocation_binds_target_repo(invocations[0])
    assert not _invocation_binds_target_repo(
        invocations[0].replace('--repo "$repo"', "")
    )


def test_checkout_helper_threads_task_trailers_through_every_commit() -> None:
    source = CHECKOUT_HELPER.read_text()

    assert _has_complete_task_plumbing(source)
    assert not _has_complete_task_plumbing(
        source.replace('-m "Task: $task"', "", 1)
    )
    assert not _has_complete_task_plumbing(source.replace('--task "$task"', "", 1))


def test_checkout_helper_binds_validation_to_requested_agent() -> None:
    source = CHECKOUT_HELPER.read_text()
    mutated = source.replace(
        '--agent "$agent" \\\n    --target "$candidate"',
        '--agent hermit-ptw \\\n    --target "$candidate"',
    )

    assert _validator_uses_requested_agent(source)
    assert mutated != source
    assert not _validator_uses_requested_agent(mutated)


def test_checker_repo_binding_works_from_parent_cwd() -> None:
    # Exercise the checker's real, network-free API from outside the Hermit
    # checkout.  The explicit --repo binding must expose the tracked pin; an
    # unbound invocation from the parent must not accidentally expose that pin.
    lock = (HERMIT / "Cargo.lock").read_text()
    pins = set(
        re.findall(
            r"github\.com/rrnewton/reverie(?:\.git)?\?rev=([0-9a-f]{40})",
            lock,
        )
    )
    assert len(pins) == 1
    pin = pins.pop()
    bound = subprocess.run(
        [str(CHECKER), "--repo", str(HERMIT), "--print-pin"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    unbound = subprocess.run(
        [str(CHECKER), "--print-pin"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )

    assert bound.returncode == 0, bound.stderr
    assert bound.stdout.strip() == pin
    assert unbound.stdout.strip() != pin
