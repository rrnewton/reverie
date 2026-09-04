#!/usr/bin/env python3
"""Fail-closed brackets for producer-transition finalization orchestration."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import subprocess

import pytest


MODULE = Path(__file__).with_name("finalize_producer_transition.py")
SPEC = importlib.util.spec_from_file_location("finalize_producer_transition", MODULE)
assert SPEC is not None and SPEC.loader is not None
fpt = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fpt)

HEAD = "1" * 40
REPLAY = "2" * 40
MAIN = "3" * 40
FIXTURE_PR = 4242
CANDIDATE = {
    "definition": {
        ".github/workflows/ci-portable.yml": "4" * 40,
        "ci/validate_peer_snapshot.py": "5" * 40,
        "validate.sh": "6" * 40,
    },
    "coverage_status": "complete",
    "paths": [
        ".github/workflows/ci-portable.yml",
        "ci/validate_peer_snapshot.py",
        "validate.sh",
    ],
}
TRANSITION = {
    "id": f"rrnewton-hermit-pr-{FIXTURE_PR}",
    "registered_at": HEAD,
    "provenance": {
        "repository": "rrnewton/hermit",
        "pull_request": FIXTURE_PR,
        "head": HEAD,
    },
    "candidate_record": CANDIDATE,
    "added_paths": ["ci/validate_peer_snapshot.py"],
    "finalize_after": "2000-01-01T00:00:00Z",
    "expires_at": "2099-01-01T00:00:00Z",
    "active": True,
    "finalizable": True,
    "registry_sha256": "a" * 64,
}

LIVE_REGISTRY = MODULE.parents[1] / "validate" / "producer-definition.json"
OUTGOING_PRIMARY = {
    ".config/nextest.toml": "5754a6d6983000ded67562250cedcc6c1ce53cf5",
    ".github/workflows/ci-portable.yml": "78167c8dccfa757a5348d96d479f72b1c8e63a3d",
    "ci/run-nextest-counted.sh": "ca88efe6d3d270881ecbe7205373f642369ce53d",
    "scripts/lib/validate_history.rs": "fb5bda65ac9ceb16bd7ded72c4f8f1b1b1615bde",
    "scripts/lib/validate_plan.rs": "f7ddea328cef00ac37b5b2a6381e656e0d9c3821",
    "scripts/lib/validate_receipt.rs": "15fa8d4b4ecdf89bdeb6ce33f460de11cf43d79c",
    "scripts/lib/validate_runtime.rs": "721df0c7330851ea4cc09516d3ee6a4f680943b3",
    "scripts/validate.rs": "062d52b04e70c066d40bf5d89c7a7ee4f2a3c51e",
}
LIVE_PRIMARY = {
    **OUTGOING_PRIMARY,
    "scripts/lib/validate_plan.rs": "820806e5a50d7044175ed8d0822b1fe979e2358d",
    "scripts/validate.rs": "5c72a835bd766af63e35b9219eac077c1b927db7",
}
PR_2946_HEAD = "646c62fcd68ff88fb8b8bd9e8468b9eb8b3293c6"
LIVE_CANDIDATE = {
    **LIVE_PRIMARY,
    "scripts/validate.rs": "66681b61cda77c79a61634973f3d887bc424b428",
}


def test_live_registry_stages_pr_2946_without_advancing_primary():
    registry = json.loads(LIVE_REGISTRY.read_text())

    assert registry["registered_at"] == "2d3876bdf5d310eb4cd55e7a3613d3dbc39bdc5c"
    assert registry["registered_coverage_status"] == "complete"
    assert registry["registered"] == LIVE_PRIMARY
    assert registry["transition"] == {
        "id": "rrnewton-hermit-pr-2946",
        "registered_at": PR_2946_HEAD,
        "provenance": {
            "repository": "rrnewton/hermit",
            "pull_request": 2946,
            "head": PR_2946_HEAD,
        },
        "finalize_after": "2026-09-04T17:50:00Z",
        "expires_at": "2026-09-05T17:50:00Z",
        "candidate_coverage_status": "complete",
        "added_paths": [],
        "candidate": LIVE_CANDIDATE,
    }

    outgoing = [
        record
        for record in registry["legacy"]
        if record["id"] == "pre-rrnewton-hermit-pr-2944"
    ]
    assert outgoing == [
        {
            "id": "pre-rrnewton-hermit-pr-2944",
            "registered_at": "5a87208bb75fc1a52aebdc19694abb3d25efdee7",
            "coverage_status": "complete",
            "valid_commits": ["5a87208bb75fc1a52aebdc19694abb3d25efdee7"],
            "definition": OUTGOING_PRIMARY,
        }
    ]


def landing_payload(*, head: str = HEAD) -> dict[str, object]:
    return {
        "state": "landed",
        "rc": 0,
        "input": str(FIXTURE_PR),
        "target": "origin/main",
        "input_kind": "pr",
        "repo": "rrnewton/hermit",
        "pr": FIXTURE_PR,
        "pr_state": "MERGED",
        "pr_head_sha": head,
        "resolved_sha": REPLAY,
        "merge_commit_oid": REPLAY,
        "ancestry": "ancestor",
    }


def test_landing_authority_binds_registered_head_to_replay(monkeypatch, tmp_path):
    observed: list[list[str]] = []

    def fake_run(arguments, **_kwargs):
        observed.append(arguments)
        return subprocess.CompletedProcess(
            arguments, 0, json.dumps(landing_payload()), ""
        )

    monkeypatch.setattr(fpt, "run", fake_run)
    evidence = fpt.landing_evidence(Path("ci-hub"), tmp_path, TRANSITION)
    assert evidence["merge_commit_oid"] == REPLAY
    assert observed[0][1:4] == ["verify-landing", str(FIXTURE_PR), "--repo"]
    assert REPLAY != HEAD  # the proof consumes replay identity, not head ancestry


def test_landing_authority_refuses_a_different_dereferenced_head(monkeypatch, tmp_path):
    monkeypatch.setattr(
        fpt,
        "run",
        lambda arguments, **_kwargs: subprocess.CompletedProcess(
            arguments, 0, json.dumps(landing_payload(head="7" * 40)), ""
        ),
    )
    with pytest.raises(fpt.Refused, match="does not bind"):
        fpt.landing_evidence(Path("ci-hub"), tmp_path, TRANSITION)


def test_no_output_until_replay_and_fetched_main_both_match(monkeypatch, tmp_path):
    output = tmp_path / "finalized.json"
    args = argparse.Namespace(
        repo_checkout=tmp_path,
        verifier=Path("verifier"),
        ci_hub=Path("ci-hub"),
        output=output,
    )
    monkeypatch.setattr(fpt, "transition_evidence", lambda _verifier: TRANSITION)
    monkeypatch.setattr(
        fpt, "landing_evidence", lambda *_args: landing_payload()
    )
    monkeypatch.setattr(fpt, "fetched_main_tip", lambda _checkout: MAIN)

    crossed = json.loads(json.dumps(CANDIDATE))
    crossed["definition"]["ci/validate_peer_snapshot.py"] = "8" * 40

    def mismatched_map(_verifier, _checkout, sha):
        return CANDIDATE if sha == REPLAY else crossed

    monkeypatch.setattr(fpt, "resolved_map", mismatched_map)
    monkeypatch.setattr(
        fpt,
        "finalized_registry",
        lambda *_args: ({"registered": CANDIDATE["definition"]}, CANDIDATE),
    )
    with pytest.raises(fpt.Refused, match="freshly fetched main tip"):
        fpt.execute(args)
    assert not output.exists()


def test_positive_writes_only_canonically_reverified_proposal(monkeypatch, tmp_path):
    output = tmp_path / "finalized.json"
    registry = {
        "registered_at": REPLAY,
        "registered_coverage_status": "complete",
        "registered": CANDIDATE["definition"],
        "legacy": [
            {
                "id": f"pre-rrnewton-hermit-pr-{FIXTURE_PR}",
                "registered_at": "9" * 40,
                "coverage_status": "legacy-selected-paths",
                "valid_commits": ["9" * 40],
                "definition": {"validate.sh": "a" * 40},
            }
        ],
    }
    args = argparse.Namespace(
        repo_checkout=tmp_path,
        verifier=Path("verifier"),
        ci_hub=Path("ci-hub"),
        output=output,
    )
    monkeypatch.setattr(fpt, "transition_evidence", lambda _verifier: TRANSITION)
    monkeypatch.setattr(
        fpt, "landing_evidence", lambda *_args: landing_payload()
    )
    monkeypatch.setattr(fpt, "fetched_main_tip", lambda _checkout: MAIN)
    monkeypatch.setattr(fpt, "resolved_map", lambda *_args: CANDIDATE)
    monkeypatch.setattr(
        fpt, "finalized_registry", lambda *_args: (registry, CANDIDATE)
    )
    report = fpt.execute(args)
    assert report["action"] == "proposal-written"
    assert report["merge_commit_oid"] == REPLAY
    assert json.loads(output.read_text()) == registry


def test_concurrent_output_creation_is_never_overwritten(monkeypatch, tmp_path):
    output = tmp_path / "finalized.json"
    registry = {
        "registered_at": REPLAY,
        "registered_coverage_status": "complete",
        "registered": CANDIDATE["definition"],
    }
    args = argparse.Namespace(
        repo_checkout=tmp_path,
        verifier=Path("verifier"),
        ci_hub=Path("ci-hub"),
        output=output,
    )
    monkeypatch.setattr(fpt, "transition_evidence", lambda _verifier: TRANSITION)
    monkeypatch.setattr(
        fpt, "landing_evidence", lambda *_args: landing_payload()
    )
    monkeypatch.setattr(fpt, "fetched_main_tip", lambda _checkout: MAIN)
    monkeypatch.setattr(fpt, "resolved_map", lambda *_args: CANDIDATE)
    monkeypatch.setattr(
        fpt, "finalized_registry", lambda *_args: (registry, CANDIDATE)
    )

    def plant_concurrent_artifact(*_args):
        output.write_text("concurrent-update\n")

    monkeypatch.setattr(fpt, "require_same_active_transition", plant_concurrent_artifact)
    with pytest.raises(fpt.Refused, match="refusing to overwrite"):
        fpt.execute(args)
    assert output.read_text() == "concurrent-update\n"


@pytest.mark.parametrize("change", ["registry", "expiry"])
def test_write_boundary_refuses_registry_or_expiry_change(
    monkeypatch, tmp_path, change
):
    output = tmp_path / "finalized.json"
    registry = {
        "registered_at": REPLAY,
        "registered_coverage_status": "complete",
        "registered": CANDIDATE["definition"],
    }
    args = argparse.Namespace(
        repo_checkout=tmp_path,
        verifier=Path("verifier"),
        ci_hub=Path("ci-hub"),
        output=output,
    )
    changed = json.loads(json.dumps(TRANSITION))
    if change == "registry":
        changed["registry_sha256"] = "b" * 64
    else:
        changed["active"] = False
        changed["finalizable"] = False
    observations = iter((TRANSITION, changed))
    monkeypatch.setattr(
        fpt, "transition_evidence", lambda _verifier: next(observations)
    )
    monkeypatch.setattr(
        fpt, "landing_evidence", lambda *_args: landing_payload()
    )
    monkeypatch.setattr(fpt, "fetched_main_tip", lambda _checkout: MAIN)
    monkeypatch.setattr(fpt, "resolved_map", lambda *_args: CANDIDATE)
    monkeypatch.setattr(
        fpt, "finalized_registry", lambda *_args: (registry, CANDIDATE)
    )
    with pytest.raises(fpt.Refused, match="changed during proof"):
        fpt.execute(args)
    assert not output.exists()
