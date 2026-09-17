#!/usr/bin/env python3
"""Decide whether a managed worktree may be removed, without reading its path.

The removal decision used to be ``checkout.name.startswith("validate-fresh-")``
in five separate places. A leaf-name prefix is not evidence: it is one rename
from deleting the wrong tree, the five copies could disagree with each other,
and nothing about it can be audited.

⚠️ THE RECORDED SLOT TYPE ALONE IS NOT SUFFICIENT EITHER, WHICH IS WHY THIS
MODULE ASKS TWO QUESTIONS RATHER THAN ONE. `wrkslots` has recorded a
creation-time ``slot_type`` all along -- 249 ``agent`` and 23 ``validate`` when
this was written -- and the design says a ``validate`` tree holds no authored
work and may be removed without salvage. MEASURED 2026-09-17, THAT PREMISE IS
FALSE FOR FOUR TREES: ``codex-liteinst-integration-01a0a13c``,
``scorecard-pages-validation2``, ``scorecard-pr2939-validation`` and
``validate-pinned-guest-paths-20260916`` are each recorded ``validate`` and hold
five unpushed commits between them, confirmed against a fresh fetch. Trusting
the type by itself would have deleted them; the leaf-name check this module
replaces spared them only by accident, because all of them are named something
other than ``validate-fresh-``.

So a tree is removable only when the registry says it is disposable AND the tree
itself holds no authored work. Either question answering "no" or "unknown"
protects the tree.

The authored-work half is deliberately computed live rather than read from the
registry's ``containing_remote_refs``: measured on the same day, that field was
``0`` for all twenty-three recorded validate trees including the ones whose
heads are genuinely on a remote, so it records creation-time state and never
catches up. Two git commands per tree cost about a second across twenty-five
trees, which is affordable for a question whose wrong answer destroys work.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Sequence

# A tree may be removed. Both questions answered, both favourably.
DISPOSABLE = "disposable"
# A tree must not be removed. `reason` always says which question refused.
PROTECTED = "protected"

# Recorded kinds. `wrkslots` spells these in SLOT_TYPES; they are repeated here
# rather than imported because wrkslots lives in a separate repository and this
# reader must keep working when that pin moves.
KIND_VALIDATE = "validate"
KIND_AGENT = "agent"


def _registry_files(root: Path) -> list[Path]:
    """Every per-machine active registry beside the control directory."""

    control = root / "worktrees"
    if not control.is_dir():
        return []
    # ACTIVE.global.lock and ACTIVE.<machine>.json.lock are not registries.
    return sorted(
        path
        for path in control.glob("ACTIVE.*.json")
        if path.suffix == ".json" and path.is_file()
    )


def _records(root: Path) -> Iterable[Mapping[str, Any]]:
    for path in _registry_files(root):
        try:
            document = json.loads(path.read_text())
        except (OSError, ValueError):
            # An unreadable registry must not silently become "no record", which
            # would read as UNKNOWN and protect. That is the safe direction, so
            # skipping is correct here; the caller's reason will say unregistered.
            continue
        slots = document.get("slots")
        if not isinstance(slots, list):
            continue
        for record in slots:
            if isinstance(record, Mapping):
                yield record


def recorded_kind(root: Path, tree: Path) -> str | None:
    """Return the registry's creation-time kind for `tree`, or None if absent.

    Matching is by the recorded checkout PATH, not by any name pattern, so a
    renamed leaf changes nothing and an unregistered tree answers None.
    """

    try:
        wanted = tree.resolve()
    except OSError:
        return None
    for record in _records(root):
        kind = record.get("slot_type")
        if not isinstance(kind, str):
            continue
        for checkout in record.get("checkouts") or ():
            if not isinstance(checkout, Mapping):
                continue
            recorded = checkout.get("path")
            if not isinstance(recorded, str):
                continue
            candidate = Path(recorded)
            if not candidate.is_absolute():
                candidate = root / candidate
            try:
                if candidate.resolve() == wanted:
                    return kind
            except OSError:
                continue
    return None


def authored_work(tree: Path, *, run: Callable[..., Any]) -> tuple[int, int] | None:
    """Return (dirty paths, commits on no remote ref), or None if unmeasurable.

    None is NOT "no authored work". A tree whose state cannot be read must
    protect, so the caller treats None as a refusal.
    """

    def count(args: Sequence[str]) -> int | None:
        result = run(["git", "-C", str(tree), *args], check=False)
        if getattr(result, "returncode", 1) != 0:
            return None
        return len([line for line in (result.stdout or "").splitlines() if line.strip()])

    dirty = count(["status", "--porcelain"])
    if dirty is None:
        return None
    unpushed = count(["rev-list", "HEAD", "--not", "--remotes"])
    if unpushed is None:
        return None
    return dirty, unpushed


def disposition(root: Path, tree: Path, *, run: Callable[..., Any]) -> tuple[str, str]:
    """Answer whether `tree` may be removed, and say why not when it may not.

    Never consults the tree's name. Every uncertainty resolves to PROTECTED.
    """

    kind = recorded_kind(root, tree)
    if kind is None:
        return PROTECTED, "no registry record: an unregistered tree is unknown, not disposable"
    if kind != KIND_VALIDATE:
        return PROTECTED, f"recorded slot_type is {kind!r}, not {KIND_VALIDATE!r}"

    measured = authored_work(tree, run=run)
    if measured is None:
        return PROTECTED, "authored-work state could not be read"
    dirty, unpushed = measured
    if unpushed:
        return (
            PROTECTED,
            f"recorded {KIND_VALIDATE} but holds {unpushed} commit(s) on no remote ref",
        )
    if dirty:
        return PROTECTED, f"recorded {KIND_VALIDATE} but has {dirty} uncommitted path(s)"
    return DISPOSABLE, f"recorded {KIND_VALIDATE} with no authored work"
