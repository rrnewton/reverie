#!/usr/bin/env python3
"""Fail-closed brackets for producer-transition finalization orchestration."""

from __future__ import annotations

import argparse
import copy
import hashlib
import os
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
    "scripts/lib/validate_plan.rs": "820806e5a50d7044175ed8d0822b1fe979e2358d",
    "scripts/lib/validate_receipt.rs": "15fa8d4b4ecdf89bdeb6ce33f460de11cf43d79c",
    "scripts/lib/validate_runtime.rs": "721df0c7330851ea4cc09516d3ee6a4f680943b3",
    "scripts/validate.rs": "5c72a835bd766af63e35b9219eac077c1b927db7",
}
STAGED_PRIMARY = {
    **OUTGOING_PRIMARY,
    "scripts/validate.rs": "66681b61cda77c79a61634973f3d887bc424b428",
}


PR_3018_HEAD = "631131d42972398fdceabef7023a67116fb43d59"
PR_3018_MERGE = "ce1aca3aba3a4bb1db244e1ca2ab6903ea4bb7a8"
LIVE_CANDIDATE = {
    ".config/nextest.toml": "5989ccd378adc871597969880082fd3b36b3d170",
    ".github/workflows/ci-portable.yml": "172ecf07418f284dfabd3e919eec3ca934ce9b8a",
    "ci/manifest-plan/src/bin/nextest-cpu-wrapper.rs": "1f827317df1af81991743edb99ded0fc22094a5a",
    "ci/manifest-plan/src/ledger.rs": "0d737c4dc0d4facac7ebad0be2cda3cf16f69acb",
    "ci/manifest-plan/src/lib.rs": "506a5c5832661a31727ef918ad40fa6a3171b1b4",
    "ci/manifest-plan/src/nextest_binaries.rs": "aa10f24d085f3f59cb8b1f68d6a9063471b67a6a",
    "ci/manifest-plan/src/nextest_cpu.rs": "35e03425d8fed1f319fe2a2bbfce4879ba0a9fc1",
    "ci/manifest-plan/src/runner.rs": "40bd930a76340075dfe3c7913b2c5ee32239d908",
    "ci/manifest-plan/src/service_result.rs": "b138f1937c2af8269755cfeb3311c4f2dec3c8b8",
    "ci/manifest-plan/src/timeouts.rs": "d4cfe464f851df78ddf72e7980220bf32b66c609",
    "ci/nextest-binaries.rs": "fa5722934022d53723490675e983778735402693",
    "ci/nextest-test-results.rs": "d1a2f1141250267461bf64141c5368f5102782ff",
    "ci/nextest-timeout-config.rs": "a005fc8ba8b987923db21ea8a0b24daf4f58eccb",
    "ci/run-nextest-counted.sh": "7839b9d80b2563d1221690ef69d388fface5d325",
    "hermit-cli/src/canonical_verdict.rs": "7786342c9f284857cf8a5e97f8536d8b176438e4",
    "scripts/lib/safe_ci_scope.rs": "8f3f95f7fc56a3a9540545391e65c32101632f21",
    "scripts/lib/validate_cell_results.rs": "fe0f4c75299ada11d1c138421158425ee9d05b0e",
    "scripts/lib/validate_classification.rs": "5dff66ce5321e0bb7f13cb705ab6853fd5996872",
    "scripts/lib/validate_history.rs": "2fd9aeba20d8d05f5ff1318bb8d3b4949b5bac7d",
    "scripts/lib/validate_plan.rs": "cd5b3a686fa17fc8513b5f4f04398c4d8e5dea43",
    "scripts/lib/validate_receipt.rs": "15fa8d4b4ecdf89bdeb6ce33f460de11cf43d79c",
    "scripts/lib/validate_runtime.rs": "4e6e4e0ab5d73ef0780b4411e1978c11db57506a",
    "scripts/validate.rs": "33b4f778185b4d572c423291d5290be93763d937",
}
PR_3212_HEAD = "5066b0a5b73d54c2c6709c8344cec6f3eb84a917"
PR_3212_CANDIDATE = {
    ".config/nextest.toml": "b409b5170b8d6b9aecb1c35707aa1bd6f10045c0",
    ".github/workflows/ci-portable.yml": "a4a487afc421501837f4ac027ab83942cfe52e5e",
    "ci/manifest-plan/src/bin/nextest-cpu-wrapper.rs": "40561f929a199c853dd5c00f190d3542b08efae6",
    "ci/manifest-plan/src/ledger.rs": "0b36d76824b0b345ef9129f8d15f822eb38ac7f9",
    "ci/manifest-plan/src/lib.rs": "9e343ba1f12d04fc0cadde246a9c49dd74cb5600",
    "ci/manifest-plan/src/nextest_binaries.rs": "6d39d8ab9a225cecb044cbfbca7773513e32cfc6",
    "ci/manifest-plan/src/nextest_cpu.rs": "f06d6cd11c3c5bffbab0a7d9267b1ede4af49f76",
    "ci/manifest-plan/src/runner.rs": "a960679a6a61deb371071dc001a7d7d8bde347f4",
    "ci/manifest-plan/src/service_result.rs": "b138f1937c2af8269755cfeb3311c4f2dec3c8b8",
    "ci/manifest-plan/src/timeouts.rs": "65ecae4916d76749f0970b077e9399a3a4355776",
    "ci/nextest-binaries.rs": "6ca925dd2d7eac953e85f9c48796c8baee58220c",
    "ci/nextest-test-results.rs": "f5a10d1f2c42d6b0a4077dc7118293aa3183af11",
    "ci/nextest-timeout-config.rs": "74e194d8762b744d77a16479b76373ed3a400173",
    "ci/run-nextest-counted.sh": "9b29439ffbd6746c86235ca374d9eee19ecc9ea1",
    "hermit-cli/src/canonical_verdict.rs": "5d39822bf1423357679960246df31096da5d2549",
    "scripts/lib/safe_ci_scope.rs": "8f3f95f7fc56a3a9540545391e65c32101632f21",
    "scripts/lib/validate_cell_results.rs": "f7203f6264dbe9596b357f00905d2d528a79b481",
    "scripts/lib/validate_classification.rs": "00cc70dd305b05a817a1e28ad389d50177f72a3f",
    "scripts/lib/validate_history.rs": "ad3779e49e86947680a6f0c08466710c4e9817e9",
    "scripts/lib/validate_plan.rs": "700967d9658feeed252830e3ece377d7fedf8e2c",
    "scripts/lib/validate_receipt.rs": "f497e377622d5440e6ad904ff4b183aee2b5f833",
    "scripts/lib/validate_runtime.rs": "4e6e4e0ab5d73ef0780b4411e1978c11db57506a",
    "scripts/validate.rs": "457d150a400252a0e92a245fdadb1e71315be66a",
}
PR_3212_TRANSITION = {
    "id": "rrnewton-hermit-pr-3212",
    "registered_at": PR_3212_HEAD,
    "provenance": {
        "repository": "rrnewton/hermit",
        "pull_request": 3212,
        "head": PR_3212_HEAD,
    },
    "finalize_after": "2026-09-24T15:30:00Z",
    "expires_at": "2026-09-25T15:30:00Z",
    "candidate_coverage_status": "complete",
    "added_paths": [],
    "candidate": PR_3212_CANDIDATE,
}
LIVE_ADDED_PATHS = [
    "ci/manifest-plan/src/bin/nextest-cpu-wrapper.rs",
    "ci/manifest-plan/src/ledger.rs",
    "ci/manifest-plan/src/lib.rs",
    "ci/manifest-plan/src/nextest_binaries.rs",
    "ci/manifest-plan/src/nextest_cpu.rs",
    "ci/manifest-plan/src/runner.rs",
    "ci/manifest-plan/src/service_result.rs",
    "ci/manifest-plan/src/timeouts.rs",
    "ci/nextest-binaries.rs",
    "ci/nextest-test-results.rs",
    "ci/nextest-timeout-config.rs",
    "hermit-cli/src/canonical_verdict.rs",
    "scripts/lib/safe_ci_scope.rs",
    "scripts/lib/validate_cell_results.rs",
    "scripts/lib/validate_classification.rs",
]
STAGED_FINALIZE_AFTER = "2026-09-16T11:12:26Z"
STAGED_EXPIRES_AT = "2026-09-17T11:12:26Z"
PRE_STAGE_LEGACY_DIGEST = (
    "8e8cf5a49301a18eb5c696b8f8041fd89b7c5e2e83f5259ff1ca3452fe91946c"
)


STAGED_TRANSITION = {
    "id": "rrnewton-hermit-pr-3018",
    "registered_at": PR_3018_HEAD,
    "provenance": {
        "repository": "rrnewton/hermit",
        "pull_request": 3018,
        "head": PR_3018_HEAD,
    },
    "candidate_coverage_status": "complete",
    "candidate": LIVE_CANDIDATE,
    "added_paths": LIVE_ADDED_PATHS,
    "finalize_after": STAGED_FINALIZE_AFTER,
    "expires_at": STAGED_EXPIRES_AT,
}

# Reconstruct the exact published pre-finalization fixture without requiring Git
# history or copying its 18 unchanged legacy records into a second source file.
# The independent digest freezes every field; current registry drift cannot
# silently change the fixture used for provenance and expiry refusal controls.
STAGED_REGISTRY_DIGEST = (
    "24def8be04dd3a1745568111e39baaaf6a52344826c1f2a52b42ee51a06ce32c"
)


def pre_finalized_stage_registry():
    registry = json.loads(LIVE_REGISTRY.read_text())
    assert registry["registered_at"] == PR_3018_MERGE
    assert registry["registered"] == LIVE_CANDIDATE
    assert registry.pop("transition") == PR_3212_TRANSITION
    registry.pop("_transition_2026_09_24_pr3212")
    assert registry["legacy"][-1] == {
        "id": "pre-rrnewton-hermit-pr-3018",
        "registered_at": "4d8f866102882b6eeabdf99cc7e81433cc3c95c5",
        "coverage_status": "complete",
        "valid_commits": ["4d8f866102882b6eeabdf99cc7e81433cc3c95c5"],
        "definition": STAGED_PRIMARY,
    }
    registry["registered_at"] = "4d8f866102882b6eeabdf99cc7e81433cc3c95c5"
    registry["registered"] = copy.deepcopy(STAGED_PRIMARY)
    registry["legacy"] = registry["legacy"][:-1]
    registry["transition"] = copy.deepcopy(STAGED_TRANSITION)
    assert (
        hashlib.sha256(
            json.dumps(registry, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        == STAGED_REGISTRY_DIGEST
    )
    return registry


def test_live_registry_finalizes_pr_3018_and_stages_exact_pr_3212_transition():
    registry = json.loads(LIVE_REGISTRY.read_text())

    assert registry["registered_at"] == PR_3018_MERGE
    assert registry["registered_coverage_status"] == "complete"
    assert registry["registered"] == LIVE_CANDIDATE

    assert registry["transition"] == PR_3212_TRANSITION
    assert "registered_valid_commits" not in registry
    assert len(registry["legacy"]) == 19
    assert (
        hashlib.sha256(
            json.dumps(
                registry["legacy"][:-1], sort_keys=True, separators=(",", ":")
            ).encode()
        ).hexdigest()
        == PRE_STAGE_LEGACY_DIGEST
    )

    outgoing = [
        record
        for record in registry["legacy"]
        if record["id"] == "pre-rrnewton-hermit-pr-2946"
    ]
    assert outgoing == [
        {
            "id": "pre-rrnewton-hermit-pr-2946",
            "registered_at": "2d3876bdf5d310eb4cd55e7a3613d3dbc39bdc5c",
            "coverage_status": "complete",
            "valid_commits": ["2d3876bdf5d310eb4cd55e7a3613d3dbc39bdc5c"],
            "definition": OUTGOING_PRIMARY,
        }
    ]

    # The nineteenth record is the outgoing primary, bounded to its old commit.
    assert pre_finalized_stage_registry()["transition"] == STAGED_TRANSITION


def check_live_definition(
    tmp_path, definition, *, registry=LIVE_REGISTRY, sha=PR_3018_MERGE
):
    definition_path = tmp_path / "definition.json"
    definition_path.write_text(json.dumps(definition))
    env = {**os.environ, "PRODUCER_DEFINITION_REGISTRY": str(registry)}
    return subprocess.run(
        [
            str(fpt.DEFAULT_VERIFIER),
            "--producer-definition-check",
            str(definition_path),
            "--sha",
            sha,
        ],
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def test_live_candidate_requires_every_path_and_one_whole_map(tmp_path):
    qualifying = check_live_definition(tmp_path, PR_3212_CANDIDATE, sha=PR_3212_HEAD)
    assert qualifying.returncode == 0, qualifying.stderr
    for missing in PR_3212_CANDIDATE:
        partial = {
            key: value for key, value in PR_3212_CANDIDATE.items() if key != missing
        }
        assert len(partial) == len(PR_3212_CANDIDATE) - 1
        assert (
            check_live_definition(tmp_path, partial, sha=PR_3212_HEAD).returncode == 1
        ), missing
    crossed = {
        **PR_3212_CANDIDATE,
        "scripts/validate.rs": LIVE_CANDIDATE["scripts/validate.rs"],
    }
    assert crossed != PR_3212_CANDIDATE
    assert check_live_definition(tmp_path, crossed, sha=PR_3212_HEAD).returncode == 1
    extra = {**PR_3212_CANDIDATE, "unregistered-producer.rs": "a" * 40}
    assert check_live_definition(tmp_path, extra, sha=PR_3212_HEAD).returncode == 1


def test_live_candidate_expiry_and_provenance_refuse_without_losing_primary(tmp_path):
    source = pre_finalized_stage_registry()
    wrong_head = copy.deepcopy(source)
    wrong_head["transition"]["provenance"]["head"] = "f" * 40
    malformed = tmp_path / "wrong-provenance.json"
    malformed.write_text(json.dumps(wrong_head))
    assert (
        check_live_definition(
            tmp_path, LIVE_CANDIDATE, registry=malformed, sha=PR_3018_HEAD
        ).returncode
        == 2
    )

    expired = copy.deepcopy(source)
    expired["transition"]["finalize_after"] = "2000-01-01T00:00:00Z"
    expired["transition"]["expires_at"] = "2000-01-02T00:00:00Z"
    expired_path = tmp_path / "expired.json"
    expired_path.write_text(json.dumps(expired))
    assert (
        check_live_definition(
            tmp_path, LIVE_CANDIDATE, registry=expired_path, sha=PR_3018_HEAD
        ).returncode
        == 1
    )
    assert (
        check_live_definition(
            tmp_path, STAGED_PRIMARY, registry=expired_path, sha=source["registered_at"]
        ).returncode
        == 0
    )


def test_live_legacy_definitions_keep_their_exact_commit_bounds(tmp_path):
    registry = json.loads(LIVE_REGISTRY.read_text())
    assert len(registry["legacy"]) == 19
    for record in registry["legacy"]:
        for sha in record["valid_commits"]:
            result = check_live_definition(tmp_path, record["definition"], sha=sha)
            assert result.returncode == 0, (record["id"], sha, result.stderr)
        assert (
            check_live_definition(tmp_path, record["definition"]).returncode == 1
        ), record["id"]


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
    monkeypatch.setattr(fpt, "landing_evidence", lambda *_args: landing_payload())
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
    monkeypatch.setattr(fpt, "landing_evidence", lambda *_args: landing_payload())
    monkeypatch.setattr(fpt, "fetched_main_tip", lambda _checkout: MAIN)
    monkeypatch.setattr(fpt, "resolved_map", lambda *_args: CANDIDATE)
    monkeypatch.setattr(fpt, "finalized_registry", lambda *_args: (registry, CANDIDATE))
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
    monkeypatch.setattr(fpt, "landing_evidence", lambda *_args: landing_payload())
    monkeypatch.setattr(fpt, "fetched_main_tip", lambda _checkout: MAIN)
    monkeypatch.setattr(fpt, "resolved_map", lambda *_args: CANDIDATE)
    monkeypatch.setattr(fpt, "finalized_registry", lambda *_args: (registry, CANDIDATE))

    def plant_concurrent_artifact(*_args):
        output.write_text("concurrent-update\n")

    monkeypatch.setattr(
        fpt, "require_same_active_transition", plant_concurrent_artifact
    )
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
    monkeypatch.setattr(fpt, "landing_evidence", lambda *_args: landing_payload())
    monkeypatch.setattr(fpt, "fetched_main_tip", lambda _checkout: MAIN)
    monkeypatch.setattr(fpt, "resolved_map", lambda *_args: CANDIDATE)
    monkeypatch.setattr(fpt, "finalized_registry", lambda *_args: (registry, CANDIDATE))
    with pytest.raises(fpt.Refused, match="changed during proof"):
        fpt.execute(args)
    assert not output.exists()
