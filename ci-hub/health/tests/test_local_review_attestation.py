"""Ordinary local relay opponents; all provider/network records here are fixtures."""

from __future__ import annotations

import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


ab = load("local_review_test_authority", ROOT / "health/approval_binding.py")
tool = load("local_review_test_helper", ROOT / "bin/gh-merge-verified")
HEAD, BASE, TREE = "a" * 40, "b" * 40, "c" * 40
DIFF = "d" * 64


def artifact(path, value):
    raw = value.encode() if isinstance(value, str) else (json.dumps(value) + "\n").encode()
    path.write_bytes(raw)
    return {"path": str(path), "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}


@pytest.fixture
def case(tmp_path):
    report = artifact(tmp_path / "review.md", f"[unknown-agentname, role=reviewer]\nAPPROVED-AT: codex {HEAD}\nNo blocking finding.\n")
    completion = {
        "schema": 1, "head": HEAD, "base": BASE, "tree": TREE, "diff_sha256": DIFF,
        "reviewer": "independent-worker", "implementor": "implementation-worker",
        "report_sha256": report["sha256"], "verdict": "approve", "open_findings": [],
        "completed_at": "2026-09-22T00:00:00+00:00",
    }
    packet = {
        "schema": 1, "repository": "fixture/repo", "pr": 7,
        "head": HEAD, "base": BASE, "tree": TREE, "diff_sha256": DIFF,
        "family": "codex", "report": report,
        "triggers": {key: {"applies": False, "reason": "Independent complete-diff fixture assessment."} for key in ab.LOCAL_REVIEW_TRIGGERS},
    }

    def save_completion():
        completion["triggers_sha256"] = hashlib.sha256(json.dumps(packet["triggers"], sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        packet["completion"] = artifact(tmp_path / "completion.json", completion)
        capture = {
            "reviewer": completion["reviewer"], "observed_at": "2026-09-22T00:00:01Z",
            "message": f"Completed exact {HEAD}; report {report['sha256']}; completion {packet['completion']['sha256']}",
        }
        packet["provenance"] = {"kind": "attestor-observed-collaboration", "evidence": artifact(tmp_path / "capture.json", capture)}

    save_completion()
    source = {key: packet[key] for key in ("base", "head", "tree", "diff_sha256")}
    reads = []

    def read_json(path):
        reads.append(path)
        if path == "user":
            return {"login": "relay-bot"}
        assert path == "repos/fixture/repo/collaborators/relay-bot/permission"
        return {"permission": "write", "user": {"login": "relay-bot"}}

    def evaluate(**overrides):
        path = tmp_path / "packet.json"
        path.write_text(json.dumps(packet))
        return ab.local_review_attestation(path, **{
            "repo": "fixture/repo", "pr": 7, "head": HEAD,
            "source_identity": lambda base, head: source,
            "read_json": read_json, "critical_required": False, "labels": [],
            **overrides,
        })

    return packet, completion, save_completion, evaluate, source, reads


def test_genuine_local_relay_is_not_a_forged_public_record(case):
    packet, _, _, evaluate, _, reads = case
    result = evaluate()
    assert result["origin"] == "local-authenticated-relay"
    assert result["attestor"]["login"] == "relay-bot"
    assert result["technical_reviewer"] == "independent-worker"
    assert result["state"] == ab.BOUND_CURRENT
    assert result["provider_files_authenticated"] is False
    assert result["public_record"] is False
    assert not {"id", "user", "created_at", "html_url"} & result.keys()
    assert result["report"] == packet["report"]
    assert reads == ["user", "repos/fixture/repo/collaborators/relay-bot/permission"]


def test_same_authenticated_relay_or_unknown_fleet_name_is_not_self_review(case):
    _, completion, save, evaluate, _, _ = case
    completion["reviewer"] = "relay-bot"
    save()
    assert evaluate()["attestor"]["login"] == "relay-bot"


@pytest.mark.parametrize("field,value", [("head", "e" * 40), ("pr", 8), ("repository", "wrong/repo"), ("schema", True), ("extra", "forged")])
def test_wrong_scope_or_open_packet_refuses(case, field, value):
    packet, _, _, evaluate, _, _ = case
    packet[field] = value
    with pytest.raises(ab.ReviewRecordError):
        evaluate()


@pytest.mark.parametrize("field", ["base", "tree", "diff_sha256"])
def test_actual_full_source_must_match(case, field):
    _, _, _, evaluate, source, _ = case
    source[field] = "e" * len(source[field])
    with pytest.raises(ab.ReviewRecordError, match="source/diff"):
        evaluate()


def test_fixed_reviewed_base_is_not_compared_to_moving_main(case):
    packet, _, _, evaluate, _, _ = case
    result = evaluate()
    assert result["base"] == packet["base"]
    assert "target_tip" not in result


def test_modified_or_missing_report_is_not_a_pass(case):
    packet, _, _, evaluate, _, _ = case
    report = Path(packet["report"]["path"])
    original = report.read_bytes()
    report.write_bytes(original.replace(b"No blocking", b"One blocking"))
    with pytest.raises(ab.ReviewRecordError, match="declared size|digest"):
        evaluate()
    report.unlink()
    with pytest.raises(ab.ReviewRecordError, match="cannot read"):
        evaluate()


@pytest.mark.parametrize("field,value", [("open_findings", ["unresolved"]), ("verdict", "changes-requested"), ("reviewer", "implementation-worker"), ("report_sha256", "0" * 64), ("completed_at", "yesterday")])
def test_incomplete_or_nonindependent_completion_refuses(case, field, value):
    _, completion, save, evaluate, _, _ = case
    completion[field] = value
    save()
    with pytest.raises(ab.ReviewRecordError):
        evaluate()


@pytest.mark.parametrize("permission", ["read", "none", "", None])
def test_current_actor_must_be_privileged(case, permission):
    _, _, _, evaluate, _, _ = case
    def read(path):
        return {"login": "relay-bot"} if path == "user" else {"permission": permission, "user": {"login": "relay-bot"}}
    with pytest.raises(ab.ReviewRecordError, match="permission"):
        evaluate(read_json=read)


def test_packet_cannot_claim_an_authenticated_actor(case):
    packet, _, _, evaluate, _, _ = case
    packet["actor"] = "administrator"
    with pytest.raises(ab.ReviewRecordError, match="unexpected fields"):
        evaluate()


@pytest.mark.parametrize("options", [{"critical_required": True}, {"labels": ["post-facto-human-review"]}])
def test_core_or_sticky_dual_review_cannot_use_local(case, options):
    with pytest.raises(ab.ReviewRecordError, match="core/post-facto"):
        case[3](**options)


def test_trigger_assessment_is_closed_and_refuses_any_applicable_class(case):
    packet, _, _, evaluate, _, _ = case
    packet["triggers"]["new-determinization"]["applies"] = True
    with pytest.raises(ab.ReviewRecordError, match="triggered"):
        evaluate()


def test_trigger_assessment_cannot_change_after_independent_completion(case):
    packet, _, _, evaluate, _, _ = case
    packet["triggers"]["new-determinization"]["reason"] = "Reclassified after review."
    with pytest.raises(ab.ReviewRecordError, match="completion/source/report"):
        evaluate()


def test_native_terminal_is_literal_not_a_success_boolean(case, tmp_path):
    packet, _, _, evaluate, _, _ = case
    report = Path(packet["report"]["path"]).read_text()
    native = {"type": "result", "subtype": "success", "is_error": False,
              "session_id": "synthetic-native-session", "num_turns": 2,
              "modelUsage": {"gpt-test": {}}, "result": report}
    packet["provenance"] = {"kind": "native-result", "evidence": artifact(tmp_path / "native.json", native)}
    assert evaluate()["provider_identity"]["session_id"] == "synthetic-native-session"
    native["result"] += "Changed result."
    packet["provenance"]["evidence"] = artifact(tmp_path / "native.json", native)
    with pytest.raises(ab.ReviewRecordError, match="literal report"):
        evaluate()
    native["result"], native["is_error"] = report, True
    packet["provenance"]["evidence"] = artifact(tmp_path / "native.json", native)
    with pytest.raises(ab.ReviewRecordError, match="terminal result"):
        evaluate()


def test_missing_completion_capture_cannot_be_replaced_by_report_hash(case, tmp_path):
    packet, _, _, evaluate, _, _ = case
    packet["provenance"]["evidence"] = artifact(tmp_path / "capture.json", {"reviewer": "independent-worker", "message": HEAD, "observed_at": "now"})
    with pytest.raises(ab.ReviewRecordError, match="completion binding"):
        evaluate()


def record(body):
    return {"id": 900001, "body": body, "user": {"login": "reviewer"},
            "created_at": "2026-09-22T00:00:00Z", "updated_at": "2026-09-22T00:00:01Z",
            "_review_surface": "issue-comment", "_current_review_actor_login": "reviewer",
            "_current_review_actor_authorized": True, "_current_review_actor_permission": "write"}


def test_newer_public_refusal_still_blocks_local_input(monkeypatch):
    monkeypatch.setattr(tool, "_load_grammar", lambda: ab)
    monkeypatch.setattr(tool, "_github_review_records", lambda *args: [record(f"CHANGES-REQUESTED-AT: claude {HEAD}")])
    problems, local = tool.review_admission("7", "fixture/repo", HEAD, "not-even-read.json")
    assert problems == ["unresolved review objection in claude"]
    assert local is None


@pytest.mark.parametrize("records", [None, [record("APPROVED-AT: codex malformed")]])
def test_unreadable_public_state_never_becomes_no_objection(monkeypatch, records):
    monkeypatch.setattr(tool, "_load_grammar", lambda: ab)
    monkeypatch.setattr(tool, "_github_review_records", lambda *args: records)
    assert tool.review_admission("7", "fixture/repo", HEAD, "local.json") == (["__UNREADABLE__"], None)


def test_premerge_receipt_is_exclusive_and_file_and_directory_are_fsynced(tmp_path, monkeypatch):
    calls = []
    real_fsync = tool.os.fsync
    monkeypatch.setattr(tool.os, "fsync", lambda fd: (calls.append(fd), real_fsync(fd))[1])
    path = tmp_path / "admission.json"
    digest = tool.durable_new_receipt(path, {"phase": "premerge-admission", "merge_performed": False})
    original = path.read_bytes()
    assert hashlib.sha256(original).hexdigest() == digest
    assert len(calls) == 2
    with pytest.raises(SystemExit) as error:
        tool.durable_new_receipt(path, {"phase": "terminal"})
    assert error.value.code == 4
    assert path.read_bytes() == original
    terminal = tmp_path / "terminal.json"
    tool.write_receipt(terminal, {"local_review_admission": {"path": str(path), "sha256": digest}})
    assert path.read_bytes() == original
    assert json.loads(terminal.read_text())["local_review_admission"]["sha256"] == digest


def test_packet_mutation_before_merge_refuses(case, monkeypatch):
    result = case[3]()
    monkeypatch.setattr(tool, "_load_grammar", lambda: ab)
    Path(result["packet_path"]).write_text("{}")
    with pytest.raises(SystemExit) as error:
        tool.recheck_local_review(result)
    assert error.value.code == 6


@pytest.mark.parametrize("kind", ["fifo", "directory", "symlink"])
def test_nonregular_retained_report_refuses_without_blocking(case, kind, tmp_path):
    packet, _, _, evaluate, _, _ = case
    path = Path(packet["report"]["path"])
    original = path.read_bytes()
    path.unlink()
    if kind == "fifo":
        os.mkfifo(path)
    elif kind == "directory":
        path.mkdir()
    else:
        target = tmp_path / "other-report.md"
        target.write_bytes(original)
        path.symlink_to(target)
    with pytest.raises(ab.ReviewRecordError, match="regular|cannot read"):
        evaluate()


def test_packet_replaced_by_fifo_before_merge_refuses(case, monkeypatch):
    result = case[3]()
    monkeypatch.setattr(tool, "_load_grammar", lambda: ab)
    path = Path(result["packet_path"])
    path.unlink()
    os.mkfifo(path)
    with pytest.raises(SystemExit) as error:
        tool.recheck_local_review(result)
    assert error.value.code == 6
