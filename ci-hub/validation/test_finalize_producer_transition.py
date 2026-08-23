#!/usr/bin/env python3
"""Fail-closed brackets for producer-transition finalization orchestration."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
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
LIVE_PRIMARY = {
    ".config/nextest.toml": "ec47795a8289a1f3ea62424b686217c942427b18",
    ".github/workflows/ci-portable.yml": "8f56ca222078f831e85c86ec743cb3feacb0381a",
    "ci/run-nextest-counted.sh": "5fcfc77bb7264f9631312ebb4f24f635e464364c",
    "scripts/lib/validate_history.rs": "0255c492791fba04aa9000efb18ec3f57dd6a8ad",
    "scripts/lib/validate_receipt.rs": "465f6c7e1f027ba81daf09f5011f84ebdd08deb0",
    "scripts/validate.rs": "4eb0538868c13f9d5545a09d31dbd80cbae7d6e0",
}
LIVE_CANDIDATE = {
    ".config/nextest.toml": "ec47795a8289a1f3ea62424b686217c942427b18",
    ".github/workflows/ci-portable.yml": "76f36a751302aaed0eb721970efaab5c915b7370",
    "ci/run-nextest-counted.sh": "1268ba21f7916f8b6269f059726c7edbce2ef0d2",
    "scripts/lib/validate_history.rs": "0255c492791fba04aa9000efb18ec3f57dd6a8ad",
    "scripts/lib/validate_plan.rs": "37fef2cd948c3bec1bf533bd5abf74c03d343ca6",
    "scripts/lib/validate_receipt.rs": "465f6c7e1f027ba81daf09f5011f84ebdd08deb0",
    "scripts/lib/validate_runtime.rs": "33fa5a529a1cac8adc4b19333fa6c627df8176e6",
    "scripts/validate.rs": "e91c0cd306d8a8cdd83fe3cd015fc037a7bfb41e",
}


def test_live_registry_replaces_expired_transition_without_advancing_primary():
    registry = json.loads(LIVE_REGISTRY.read_text())
    transition = registry["transition"]

    assert registry["registered_at"] == "1f4e430c135204591daa0c0cba84f77dae679a78"
    assert registry["registered_coverage_status"] == "complete"
    assert registry["registered"] == LIVE_PRIMARY

    assert transition["id"] == "rrnewton-hermit-pr-2375"
    assert transition["registered_at"] == "ca764b0266b789d4276ab73941318a0c6e5747a7"
    assert transition["provenance"] == {
        "repository": "rrnewton/hermit",
        "pull_request": 2375,
        "head": "ca764b0266b789d4276ab73941318a0c6e5747a7",
    }
    assert transition["candidate_coverage_status"] == "complete"
    assert transition["added_paths"] == [
        "scripts/lib/validate_plan.rs",
        "scripts/lib/validate_runtime.rs",
    ]
    assert transition["candidate"] == LIVE_CANDIDATE
    assert set(LIVE_CANDIDATE) == set(LIVE_PRIMARY) | set(transition["added_paths"])

    finalize_after = datetime.strptime(
        transition["finalize_after"], "%Y-%m-%dT%H:%M:%SZ"
    ).replace(tzinfo=timezone.utc)
    expires_at = datetime.strptime(
        transition["expires_at"], "%Y-%m-%dT%H:%M:%SZ"
    ).replace(tzinfo=timezone.utc)
    assert (expires_at - finalize_after).total_seconds() == 24 * 60 * 60


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
