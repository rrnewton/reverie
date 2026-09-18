"""Admission context integrity, transport and exact-row binding controls.

Synthetic unit contexts are not authenticated admission receipts. Real Git/CLI
controls below exercise the facts the finalizer can independently observe.
"""
from copy import deepcopy
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import finalize_receipt as fr
import qualifying_receipt as qr
from admission_paths import AdmissionPaths, read_admission_log


def context():
    return {
        "contract": "ci-hub-admission-floor/v1",
        "observed_at": "2026-09-18T15:00:01Z",
        "target_sha": "a" * 40, "target_tree": "b" * 40,
        "host": "fixture-host", "run_id": "actual-run-id",
        "started_at": "2026-09-18T15:00:00Z", "log_file": "/fixture/durable.log",
        "authority": {
            "slot": 1, "kind": "validate", "target": "a" * 40,
            "host": "fixture-host", "owner_host": "fixture-host",
            "owner_boot_id": "fixture-boot", "owner_pid": 42,
            "owner_start_ticks": (1 << 64) - 1, "response_sha256": "1" * 64,
        },
        "floor": {"kind": "current-main", "sha": "c" * 40,
                  "tree": "d" * 40, "observed_main_sha": "c" * 40},
        "canonical_plan_sha256": "2" * 64, "execution_plan_sha256": "3" * 64,
        "pin_bindings": [{"node": "pre.reverie_pin", "base_sha": "c" * 40,
                          "canonical_command_sha256": "4" * 64,
                          "execution_command_sha256": "5" * 64}],
    }


def raw_json(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False,
                      separators=(",", ":"), allow_nan=False).encode("utf-8")


def evidence(value=None, path="/evicted/context.json"):
    value = context() if value is None else value
    raw = raw_json(value)
    return {"context": value, "artifact": {"path": str(path),
            "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}}


def row(value=None):
    value = evidence() if value is None else value
    c = value["context"]
    return {
        "schema_version": 5, "commit": c["target_sha"], "tree": c["target_tree"],
        "host": c["host"], "run_id": c["run_id"], "started_at": c["started_at"],
        "log_file": c["log_file"], "producer": "hermit-validate-sh",
        "admission": "ci-hub-validate-lock", "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
        "admission_floor_evidence": value,
    }


def test_absence_stays_legacy_and_present_invalid_refuses_even_legacy():
    r = row(); del r["admission_floor_evidence"]
    assert qr.admission_verdict(r, qr.active()) is qr.AdmissionVerdict.SATISFIED
    r["schema_version"] = 4
    assert qr.admission_verdict(r, qr.active()) is qr.AdmissionVerdict.GRANDFATHERED_UNKNOWN
    r["admission_floor_evidence"] = None
    assert qr.admission_verdict(r, qr.active()) is qr.AdmissionVerdict.ADMISSION_FLOOR_INVALID
    # The existing unrelated legacy coverage refusal remains load-bearing.
    r["coverage"] = {}
    assert qr.coverage_schema_verdict(r, qr.active()) is qr.CoverageSchemaVerdict.LEGACY_CLAIMS_COVERAGE


def test_failed_row_without_cells_retains_context_but_cannot_qualify():
    r = row(); r.update(result="fail", raw_result="fail", exit_code=1)
    before = deepcopy(r)
    assert qr.validate_admission_floor_evidence(r["admission_floor_evidence"], r)
    assert not qr.row_qualification(r, r["commit"], qr.active())[0]
    assert r == before and "cell_results" not in r
    clone = fr._clone_upgraded(r, {"base_sha": "f" * 40}, "e" * 64)
    assert clone["admission_floor_evidence"] == r["admission_floor_evidence"]
    assert clone["run_id"] == r["run_id"] != clone["record_id"]


@pytest.mark.parametrize("key", ["commit", "tree", "host", "run_id", "started_at", "log_file"])
def test_context_binds_each_independent_selected_row_field(key):
    r = row(); r[key] += "changed"
    assert qr.admission_verdict(r, qr.active()) is qr.AdmissionVerdict.ADMISSION_FLOOR_INVALID


@pytest.mark.parametrize("field", ["cell_results", "coverage", "test_results"])
def test_present_nested_run_id_must_match(field):
    r = row(); r[field] = {"run_id": r["run_id"]}
    assert qr.validate_admission_floor_evidence(r["admission_floor_evidence"], r)
    r[field]["run_id"] = "other"
    with pytest.raises(ValueError, match="run_id mismatch"):
        qr.validate_admission_floor_evidence(r["admission_floor_evidence"], r)


@pytest.mark.parametrize("field,value", [
    ("slot", True), ("slot", -1), ("slot", 1 << 64),
    ("owner_pid", False), ("owner_pid", 1), ("owner_pid", 1 << 32),
    ("owner_start_ticks", 0), ("owner_start_ticks", True),
    ("owner_start_ticks", 1 << 64), ("owner_pid", 42.0),
])
def test_authority_integer_bounds_and_bool_refusals(field, value):
    c = context(); c["authority"][field] = value
    with pytest.raises(ValueError, match="expected integer"):
        qr.admission_context_bytes(c)


@pytest.mark.parametrize("where", ["context", "authority", "floor", "binding", "artifact", "envelope"])
def test_unknown_keys_refuse_at_every_contract_object(where):
    e = evidence()
    obj = {"context": e["context"], "authority": e["context"]["authority"],
           "floor": e["context"]["floor"], "binding": e["context"]["pin_bindings"][0],
           "artifact": e["artifact"], "envelope": e}[where]
    obj["unknown"] = "not an extension point"
    with pytest.raises(ValueError):
        qr.validate_admission_floor_evidence(e)


@pytest.mark.parametrize("value", ["unknown", "2026-02-30T00:00:00Z", "2026-09-18T15:00:00+00:00", "2026-09-18T15:00:00.1Z"])
def test_new_context_timestamps_are_real_driver_utc_seconds(value):
    c = context(); c["started_at"] = value
    with pytest.raises(ValueError):
        qr.admission_context_bytes(c)


def test_inline_digest_survives_artifact_eviction_and_detects_mutation():
    e = evidence()
    assert qr.validate_admission_floor_evidence(e, row(e)) == raw_json(e["context"])
    for field, changed in [("sha256", "0" * 64), ("bytes", e["artifact"]["bytes"] + 1), ("bytes", True)]:
        bad = deepcopy(e); bad["artifact"][field] = changed
        with pytest.raises(ValueError):
            qr.validate_admission_floor_evidence(bad)
    e["context"]["host"] = "changed"
    with pytest.raises(ValueError):
        qr.validate_admission_floor_evidence(e)


def test_internal_authority_floor_pin_relations_refuse():
    for mutate in [
        lambda c: c["authority"].update(target="e" * 40),
        lambda c: c["authority"].update(owner_host="other"),
        lambda c: c["floor"].update(observed_main_sha="e" * 40),
        lambda c: c["pin_bindings"][0].update(base_sha="e" * 40),
        lambda c: c["pin_bindings"].append(deepcopy(c["pin_bindings"][0])),
        lambda c: c["pin_bindings"][0].update(node="test.other"),
        lambda c: c.update(contract="future-version"),
    ]:
        c = context(); mutate(c)
        with pytest.raises(ValueError):
            qr.admission_context_bytes(c)


def test_frozen_evidence_is_readable_but_remains_noncanonical():
    c = context(); c["authority"]["kind"] = "frozen-validate"
    c["floor"].update(kind="frozen-target", sha=c["target_sha"], tree=c["target_tree"])
    c["pin_bindings"][0]["base_sha"] = c["target_sha"]
    e = evidence(c)
    assert qr.validate_admission_floor_evidence(e, row(e))
    assert qr.admission_verdict(row(e), qr.active()) is qr.AdmissionVerdict.ADMISSION_NONCANONICAL


@pytest.mark.parametrize("variant", ["duplicate", "whitespace", "order", "float", "nan", "surrogate", "unknown", "newline"])
def test_artifact_raw_byte_contract_is_strict(tmp_path, variant):
    c = context(); raw = raw_json(c)
    if variant == "duplicate": raw = raw.replace(b'"slot":1', b'"slot":1,"slot":1')
    elif variant == "whitespace": raw = json.dumps(c, ensure_ascii=False).encode()
    elif variant == "order": raw = json.dumps(c, separators=(",", ":"), ensure_ascii=False).encode()
    elif variant == "float": raw = raw.replace(b'"slot":1', b'"slot":1.0')
    elif variant == "nan": raw = raw.replace(b'"slot":1', b'"slot":NaN')
    elif variant == "surrogate": raw = raw.replace(b'"actual-run-id"', b'"\\ud800"')
    elif variant == "unknown": raw = raw[:-1] + b',"unknown":0}'
    elif variant == "newline": raw += b"\n"
    path = tmp_path / "context.json"; path.write_bytes(raw)
    with pytest.raises(ValueError):
        fr.read_admission_context(str(path), hashlib.sha256(raw).hexdigest())


def test_regular_artifact_not_pipe_symlink_or_oversized(tmp_path):
    path = tmp_path / "context.json"; raw = raw_json(context()); path.write_bytes(raw)
    digest = hashlib.sha256(raw).hexdigest()
    assert fr.read_admission_context(str(path), digest) == evidence(path=path)
    with pytest.raises(ValueError, match="digest mismatch"):
        fr.read_admission_context(str(path), "0" * 64)
    link = tmp_path / "link"; link.symlink_to(path)
    pipe = tmp_path / "pipe"; os.mkfifo(pipe)
    for p in (link, pipe, tmp_path):
        with pytest.raises(ValueError):
            fr.read_admission_context(str(p), digest)
    path.write_bytes(b" " * (qr.ADMISSION_CONTEXT_MAX_BYTES + 1))
    with pytest.raises(ValueError, match="capacity"):
        fr.read_admission_context(str(path), digest)


@pytest.fixture
def repositories(tmp_path):
    def git(repo, *args):
        return subprocess.run(["git", "-C", str(repo), *args], check=True,
                              capture_output=True, text=True, env={**os.environ,
            "GIT_AUTHOR_NAME": "fixture", "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
            "GIT_COMMITTER_NAME": "fixture", "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        }).stdout.strip()
    h = tmp_path / "hermit"; r = tmp_path / "reverie"
    for p in (h, r): p.mkdir(); git(p, "init", "-q")
    (r / "source").write_text("reverie fixture\n"); git(r, "add", "source"); git(r, "commit", "-qm", "base")
    pin = git(r, "rev-parse", "HEAD")
    (h / "Cargo.lock").write_text(f'source = "git+https://github.com/rrnewton/reverie.git?rev={pin}#{pin}"\n')
    (h / "ci/dag").mkdir(parents=True)
    (h / "ci/dag/validate.json").write_text(json.dumps({"steps": [{"group": "test", "job": "only", "labels": ["full"], "deps": [], "result_manifests": [{"kind": "structured-test-results", "schema": 2, "owner": "test.only", "path_env": "DAGRUN_TEST_COUNTS_PATH"}]}]}))
    git(h, "add", "Cargo.lock", "ci/dag/validate.json"); git(h, "commit", "-qm", "floor")
    floor = git(h, "rev-parse", "HEAD"); git(h, "update-ref", "refs/remotes/origin/main", floor)
    (h / "branch").write_text("target\n"); git(h, "add", "branch"); git(h, "commit", "-qm", "target")
    target = git(h, "rev-parse", "HEAD")
    log = tmp_path / "dag.log"
    log.write_text("[test.only] test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n[test.only] ✓ PASS test.only\n")
    c = context(); c.update(target_sha=target, target_tree=git(h, "rev-parse", target+"^{tree}"), log_file=str(log))
    c["authority"]["target"] = target
    c["floor"].update(sha=floor, tree=git(h, "rev-parse", floor+"^{tree}"), observed_main_sha=floor)
    c["pin_bindings"][0]["base_sha"] = floor
    return h, r, git, c, log


def test_base_observation_captures_main_once_before_ref_moves(repositories, monkeypatch):
    h, r, git, c, _ = repositories; old = c["floor"]["sha"]
    real_git = fr._git; resolutions = []
    def move_after_resolve(checkout, *args):
        result = real_git(checkout, *args)
        if args == ("rev-parse", "refs/remotes/origin/main^{commit}"):
            resolutions.append(result)
            git(h, "update-ref", "refs/remotes/origin/main", c["target_sha"])
        return result
    monkeypatch.setattr(fr, "_git", move_after_resolve)
    fields = fr.build_base_evidence(str(h), c["target_sha"], str(r))
    assert resolutions == [old]
    assert git(h, "rev-parse", "origin/main") == c["target_sha"]
    assert fields["base_sha"] == old and fields["base_tree"] == c["floor"]["tree"]
    assert fields["reverie_base_sha"] == git(r, "rev-parse", "HEAD")
    assert fields["reverie_base_tree"] == git(r, "rev-parse", "HEAD^{tree}")
    observation = fields["base_observation"]
    assert observation["contract"] == "post-run-local-main-merge-base/v1"
    assert observation["observed_main_sha"] == old
    assert datetime.strptime(observation["observed_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc) <= datetime.now(timezone.utc)
    assert "admission_floor_evidence" not in fields


def test_real_emit_only_echoes_context_and_refuses_wrong_git_identity(repositories, tmp_path):
    h, r, git, c, log = repositories
    path = tmp_path / "context.json"; raw = raw_json(c); path.write_bytes(raw)
    e = evidence(c, path)
    command = [sys.executable, str(Path(fr.__file__)), "--emit-only", "--sha", c["target_sha"],
               "--log", str(log), "--hermit-checkout", str(h), "--reverie-checkout", str(r),
               "--admission-context", str(path), "--admission-context-sha256", e["artifact"]["sha256"]]
    passed = subprocess.run(command, capture_output=True, text=True)
    assert passed.returncode == 0, passed.stderr
    fields = json.loads(passed.stdout)
    assert fields["admission_floor_evidence"] == e
    assert fields["base_sha"] == c["floor"]["sha"]
    for key in ("target_tree", "log_file"):
        bad = deepcopy(c); bad[key] = "e" * 40 if key == "target_tree" else "/different/log"
        raw = raw_json(bad); path.write_bytes(raw); command[-1] = hashlib.sha256(raw).hexdigest()
        refused = subprocess.run(command, capture_output=True, text=True)
        assert refused.returncode == 2 and not refused.stdout
        assert "invalid admission floor" in refused.stderr


@pytest.mark.parametrize("flags", [[], ["--scan"], ["--select-candidate-sha256"], ["--emit-only", "--scan"]])
def test_context_transport_refuses_outside_explicit_emit_only(tmp_path, flags):
    command = [sys.executable, str(Path(fr.__file__)), "--hermit-checkout", str(tmp_path),
               "--sha", "a" * 40, "--admission-context", str(tmp_path/"unused"),
               "--admission-context-sha256", "b" * 64, *flags]
    result = subprocess.run(command, capture_output=True, text=True)
    assert result.returncode == 2 and "requires both context flags" in result.stderr


@pytest.mark.parametrize("flag", ["--admission-context", "--admission-context-sha256"])
def test_context_transport_requires_paired_flags(tmp_path, flag):
    result = subprocess.run([sys.executable, str(Path(fr.__file__)), "--emit-only",
                             "--hermit-checkout", str(tmp_path), flag, "one-only"],
                            capture_output=True, text=True)
    assert result.returncode == 2 and "requires both context flags" in result.stderr


@pytest.mark.parametrize("rewrite", ["replacement-tree", "replacement-ancestry", "graft-ancestry"])
def test_admission_checks_use_immutable_git_objects(repositories, rewrite):
    h, _r, git, c, log = repositories
    target = c["target_sha"]
    forged = deepcopy(c)
    if rewrite == "replacement-tree":
        replacement = git(h, "commit-tree", c["floor"]["tree"], "-p", c["floor"]["sha"], "-m", "different tree")
        git(h, "replace", target, replacement)
        forged["target_tree"] = c["floor"]["tree"]
        assert git(h, "rev-parse", target+"^{tree}") == forged["target_tree"] != c["target_tree"]
    else:
        orphan = git(h, "commit-tree", c["floor"]["tree"], "-m", "unrelated floor")
        forged["floor"].update(sha=orphan, observed_main_sha=orphan)
        forged["pin_bindings"][0]["base_sha"] = orphan
        if rewrite == "replacement-ancestry":
            git(h, "replace", "--graft", target, orphan)
        else:
            (h/".git/info/grafts").write_text(f"{target} {orphan}\n")
        # The ordinary mutable view accepts this false ancestry; the admission
        # path must judge the actual stored commit objects instead.
        assert git(h, "merge-base", "--is-ancestor", orphan, target) == ""
    with pytest.raises((ValueError, RuntimeError)):
        fr.verify_admission_context_git(evidence(forged), str(h), target, str(log))
    # Real target/floor evidence remains accepted in the same altered checkout.
    fr.verify_admission_context_git(evidence(c), str(h), target, str(log))

def complete_row(c, artifact_path):
    # Reuse the original finalizer's schema-5 fixture; real Git identities and
    # the context below replace only its deliberately synthetic base facts.
    from test_finalize_receipt import _counted_schema5_source
    r = _counted_schema5_source(c["target_sha"], c["log_file"])
    r.update(tree=c["target_tree"], host=c["host"], run_id=c["run_id"],
             started_at=c["started_at"], record_id="original-measurement",
             admission_floor_evidence=evidence(c, artifact_path))
    return r


def test_real_rust_delegation_refuses_present_bad_floor(repositories, tmp_path):
    h, reverie, _git, c, _log = repositories
    p = tmp_path / "context.json"; p.write_bytes(raw_json(c))
    r = complete_row(c, p)
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    assert len(fr.canonical_row_sha256(r, c["target_sha"], require_canonical_qualifying=True)) == 64
    r["admission_floor_evidence"]["artifact"]["bytes"] += 1
    with pytest.raises(RuntimeError, match="admission artifact does not match the inline context bytes"):
        fr.canonical_row_sha256(r, c["target_sha"], require_canonical_qualifying=True)
    # Rust rejects the malformed artifact during typed row decoding. Check
    # the detailed status at the actual delegated Python owner too.
    pred = qr.active()
    request = {"row": r, **{key: pred[key] for key in ("admission", "producer", "base", "coverage")}}
    result = subprocess.run([sys.executable, str(Path(qr.__file__)),
                             "--coverage-base-admission-only"],
                            input=json.dumps(request), capture_output=True, text=True)
    assert result.returncode == 1, result.stderr
    response = json.loads(result.stdout)
    assert response["admission_status"] == "admission-floor-invalid"
    assert response["admission_satisfied"] is False


def test_scan_binds_original_same_sha_row_and_preserves_context(repositories, tmp_path, monkeypatch):
    h, reverie, _git, c, _log = repositories
    monkeypatch.setenv("CI_HUB_LEDGER_FIXTURE_MODE", "1")
    p = tmp_path / "context.json"; p.write_bytes(raw_json(c))
    first = complete_row(c, p)
    second = deepcopy(first); second["run_id"] = "other-run"
    second["record_id"] = "other-measurement"
    # Same target SHA, but the original context cannot be borrowed by this row.
    ledger = tmp_path / "ledger.jsonl"
    ledger.write_text(json.dumps(first)+"\n"+json.dumps(second)+"\n")
    original = ledger.read_bytes()
    with pytest.raises(RuntimeError, match="cannot canonicalize receipt HistoryRow"):
        fr.canonical_row_sha256(second, c["target_sha"])
    assert ledger.read_bytes() == original
    selected = fr.canonical_row_sha256(first, c["target_sha"])
    passed = fr.scan_and_finalize(str(ledger), str(h), c["target_sha"], selected, str(reverie))
    assert passed["exit_code"] == 0 and passed["appended"] == 1
    rows = [json.loads(line) for line in ledger.read_text().splitlines()]
    assert rows[:2] == [first, second]
    clone = rows[2]
    assert clone["admission_floor_evidence"] == first["admission_floor_evidence"]
    assert clone["finalized_from"]["digest"] == selected
    assert clone["run_id"] == first["run_id"] != clone["record_id"]
    assert clone["base_observation"]["observed_main_sha"] == c["floor"]["sha"]
    assert len(fr.canonical_row_sha256(clone, c["target_sha"], require_canonical_qualifying=True)) == 64


def test_scan_refuses_retained_invalid_historical_digest_without_appending(tmp_path, monkeypatch):
    monkeypatch.setenv("CI_HUB_LEDGER_FIXTURE_MODE", "1")
    fixture = Path(__file__).parent / "tests/fixtures/admission-floor-v1/source4-mismatched-row.json"
    raw = fixture.read_bytes()
    digest = "03ce9d444b5a4a735beda68a4d0f35534b61fbc440884af801d141b5600345c2"
    assert hashlib.sha256(raw).hexdigest() == digest
    historical = json.loads(raw)
    assert historical["run_id"] != historical["admission_floor_evidence"]["context"]["run_id"]
    ledger = tmp_path / "historical.jsonl"
    ledger.write_bytes(raw + b"\n")
    before = ledger.read_bytes()
    # A real old reader emitted this digest before the original-row guard.
    # No new canonical ID may be minted, and even that retained selector must
    # fail through the actual scan without opening its embedded old paths.
    with pytest.raises(RuntimeError, match="cannot canonicalize receipt HistoryRow"):
        fr.canonical_row_sha256(historical, historical["commit"])
    with pytest.raises(fr.SelectedRowError, match="no canonical row"):
        fr.scan_and_finalize(str(ledger), str(tmp_path), historical["commit"], digest, str(tmp_path))
    assert ledger.read_bytes() == before


def test_scan_refuses_missing_or_modified_context_without_appending(repositories, tmp_path, monkeypatch):
    h, reverie, _git, c, _log = repositories
    monkeypatch.setenv("CI_HUB_LEDGER_FIXTURE_MODE", "1")
    p = tmp_path / "context.json"
    r = complete_row(c, p)
    ledger = tmp_path / "ledger.jsonl"; ledger.write_text(json.dumps(r)+"\n")
    before = ledger.read_bytes(); selected = fr.canonical_row_sha256(r, c["target_sha"])
    for payload in (None, raw_json(c)+b"\n"):
        if payload is not None: p.write_bytes(payload)
        result = fr.scan_and_finalize(str(ledger), str(h), c["target_sha"], selected, str(reverie))
        assert result["exit_code"] == 1 and result["reason"] == "invalid-admission-floor"
        assert result["appended"] == 0 and ledger.read_bytes() == before
    # Public inline readers still work after a host artifact has gone away.
    assert qr.validate_admission_floor_evidence(r["admission_floor_evidence"], r)


GOLDENS = {
    "basic.json": (1306, "2d1a311cb8f50dfeea1ca9f1ee66faaa4cc0610dffbcd5b25567d5461888744e"),
    "escaped.json": (1360, "c83e6e2ac71391f36a73efcea2601a486044f839e32378ca67166ae2fb6194fd"),
    "large-u64.json": (1349, "db58dffa2780d5d496b46b368af80caf32b479487f54da5f1869f991d16919b7"),
    "frozen.json": (1314, "1cf26ce5910f0bddf744fcad229f8b6e7edd5176223f752ed3fac75c531efb04"),
}


@pytest.mark.parametrize("name", GOLDENS)
def test_actual_rust_encoder_goldens_are_exact_python_bytes(name):
    p = Path(__file__).parent / "tests/fixtures/admission-floor-v1" / name
    raw = p.read_bytes(); length, digest = GOLDENS[name]
    assert len(raw) == length and hashlib.sha256(raw).hexdigest() == digest
    assert not raw.endswith(b"\n")
    c = json.loads(raw)
    assert qr.admission_context_bytes(c) == raw
    transported = fr.read_admission_context(str(p), digest)
    assert transported["context"] == c
    assert qr.validate_admission_floor_evidence(transported, row(transported)) == raw


@pytest.mark.parametrize("field", ["host", "run_id", "owner_boot_id"])
@pytest.mark.parametrize("value", [" ", "\t\n", "\x1c", "\u2003", "a\0b"])
def test_identity_strings_match_strict_cross_language_contract(field, value):
    c = context()
    if field == "owner_boot_id": c["authority"][field] = value
    else:
        c[field] = value
        if field == "host": c["authority"].update(host=value, owner_host=value)
    with pytest.raises(ValueError):
        qr.admission_context_bytes(c)


def test_absolute_retained_paths_and_derived_command_identity_required(tmp_path, monkeypatch):
    c = context(); c["log_file"] = "relative/run.log"
    with pytest.raises(ValueError, match="absolute"):
        qr.admission_context_bytes(c)
    e = evidence(); e["artifact"]["path"] = "context.json"
    with pytest.raises(ValueError, match="absolute"):
        qr.validate_admission_floor_evidence(e)
    monkeypatch.chdir(tmp_path); Path("context.json").write_bytes(raw_json(context()))
    with pytest.raises(ValueError, match="absolute"):
        fr.read_admission_context("context.json", evidence()["artifact"]["sha256"])
    c = context(); b = c["pin_bindings"][0]
    b["execution_command_sha256"] = b["canonical_command_sha256"]
    with pytest.raises(ValueError, match="derived floor"):
        qr.admission_context_bytes(c)


@pytest.mark.parametrize("bad", ["wrong-run-id", None, 42, {}, []],
                         ids=["mismatch", "null", "integer", "object", "array"])
def test_public_rust_qualifier_refuses_nested_coverage_run_id(repositories, tmp_path, bad):
    h, reverie, _git, c, _log = repositories
    path = tmp_path / "context.json"; path.write_bytes(raw_json(c))
    original = complete_row(c, path)
    original.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    assert len(fr.canonical_row_sha256(original, c["target_sha"], require_canonical_qualifying=True)) == 64
    matching = deepcopy(original); matching["coverage"]["run_id"] = c["run_id"]
    assert len(fr.canonical_row_sha256(matching, c["target_sha"], require_canonical_qualifying=True)) == 64
    broken = deepcopy(original); broken["coverage"]["run_id"] = bad
    pred = qr.active()
    request = {"row": broken, **{key: pred[key] for key in ("admission", "producer", "base", "coverage")}}
    child = subprocess.run([sys.executable, str(Path(qr.__file__)), "--coverage-base-admission-only"],
                           input=json.dumps(request), capture_output=True, text=True)
    assert child.returncode == 1, child.stderr
    report = json.loads(child.stdout)
    assert report["admission_status"] == "admission-floor-invalid"
    assert report["admission_satisfied"] is False
    with pytest.raises(RuntimeError, match="complete canonical qualifying predicate"):
        fr.canonical_row_sha256(broken, c["target_sha"], require_canonical_qualifying=True)


def digest_input(r, shape, digest):
    if shape == "row":
        return r, []
    if shape == "event":
        return {
            "schema": "validate-ledger/v1", "event_id": "fixture-result",
            "event_type": "run.result", "emitted_at": "2026-09-18T15:10:00Z",
            "team": "hermit", "host": r["host"], "run_id": r["run_id"],
            "producer": {"source": "observed", "tool": "fixture", "tool_version": "1"},
            "commit": r["commit"], "outcome": "pass", "legacy_row": r,
        }, ["--ledger-event"]
    assert shape == "published"
    return {
        "schema_version": 1, "repository": "rrnewton/hermit", "commit": r["commit"],
        "run_id": f'{r["commit"]}@{r["started_at"]}@{r["host"]}',
        "source_log_file": r["log_file"], "durable_log_file": "/retained/receipt.log",
        "log_sha256": "e" * 64,
        "producer": {"definition": {"scripts/validate.rs": "d" * 40},
                     "coverage_status": "legacy-selected-paths", "paths": ["scripts/validate.rs"],
                     "resolved_from": "/synthetic-producer-fixture"},
        "selected_receipt_identity": {"digest_algorithm": "sha256",
                                      "canonicalization": fr.RECEIPT_CANONICALIZATION,
                                      "digest": digest},
        "ledger_record": r,
    }, ["--published-receipt", "--repository", "rrnewton/hermit"]


@pytest.mark.parametrize("shape", ["row", "published", "event"])
@pytest.mark.parametrize("output", [[], ["--canonical-row"], ["--json"]],
                         ids=["digest", "canonical", "json"])
@pytest.mark.parametrize("qualifying", [[], ["--require-qualifying"], ["--require-canonical-qualifying"]],
                         ids=["plain", "shared", "canonical-qualifying"])
def test_real_receipt_digest_all_input_and_output_paths_check_original_row(
        repositories, tmp_path, shape, output, qualifying):
    h, reverie, _git, c, _log = repositories
    r = complete_row(c, tmp_path / "evicted-context.json")
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    digest = fr.canonical_row_sha256(r, c["target_sha"])
    r["coverage"]["run_id"] = r["run_id"]
    for malformed in (False, True):
        original = deepcopy(r)
        if malformed:
            original["coverage"]["run_id"] = None
        payload, flags = digest_input(original, shape, digest)
        command = [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
                   "receipt-digest", "--sha", r["commit"], *flags, *output, *qualifying]
        child = fr.bounded_capture.capture(command, input=json.dumps(payload))
        if malformed:
            assert child.returncode != 0, child.stdout
            assert child.stdout == "", "refused provenance must not emit a canonical row or digest"
        else:
            assert child.returncode == 0, child.stderr
            assert child.stdout.strip()


@pytest.mark.parametrize("second", ["wrong-run-id", "actual-run-id"])
def test_real_public_reader_keeps_legacy_duplicate_coverage_but_refuses_present_proof(
        repositories, tmp_path, second):
    h, reverie, _git, c, _log = repositories
    r = complete_row(c, tmp_path / "context.json")
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    r["coverage"]["run_id"] = r["run_id"]
    command = [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
               "receipt-digest", "--sha", r["commit"]]
    for present in (False, True):
        original = deepcopy(r)
        if not present:
            del original["admission_floor_evidence"]
        coverage = json.dumps(original["coverage"], separators=(",", ":"))
        raw = json.dumps(original, separators=(",", ":"))
        assert raw.count(coverage) == 1
        duplicate = coverage[:-1] + ',"run_id":' + json.dumps(second) + '}'
        raw = raw.replace(coverage, duplicate)
        child = fr.bounded_capture.capture(command, input=raw)
        if present:
            assert child.returncode != 0 and child.stdout == ""
        else:
            assert child.returncode == 0, child.stderr
            assert child.stdout.strip() == fr.canonical_row_sha256(original, r["commit"])


@pytest.mark.parametrize("bad_claim", ["row-binding", "artifact-bytes", "escaped-artifact", "duplicate", "escaped-duplicate"])
def test_real_validation_row_selects_good_failed_evidence_beside_bad_same_sha_row(repositories, tmp_path, bad_claim):
    _h, _reverie, _git, c, _log = repositories
    selected = complete_row(c, tmp_path / "evicted-context.json")
    selected.update(schema_version=7, result="fail", raw_result="fail", failures=1, exit_code=1,
                    cell_results={
                        "run_id": c["run_id"], "hermit_sha": c["target_sha"], "source_tree_dirty": False,
                        "selected_count": 1, "recorded_count": 0, "population_sha256": "a" * 64,
                        "artifact": {"path": "ignored/retained-cells.jsonl", "sha256": "b" * 64, "row_count": 0},
                        "selected": [{"lane": "portable", "category": "c-programs", "test": "hello",
                                      "mode": "verify", "backend": "ptrace"}], "cells": [],
                    })
    other = deepcopy(selected)
    other.update(record_id="unrelated-record", run_id="unrelated-run")
    other["cell_results"]["run_id"] = other["run_id"]
    command = [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"), "validation-row"]
    if bad_claim != "row-binding":
        other_context = deepcopy(c)
        other_context["run_id"] = other["run_id"]
        other["admission_floor_evidence"] = evidence(other_context, tmp_path / "other-context.json")
        positive = fr.bounded_capture.capture(
            command + ["--record-id", other["record_id"], "--run-id", other["run_id"]],
            input=json.dumps(other) + "\n")
        assert positive.returncode == 0, positive.stderr
    if bad_claim in ("artifact-bytes", "escaped-artifact"):
        other["admission_floor_evidence"]["artifact"]["bytes"] = 1
    other_raw = json.dumps(other)
    if bad_claim == "escaped-artifact":
        other_raw = other_raw.replace('"admission_floor_evidence":', r'"admission_\u0066loor_evidence":')
    elif bad_claim in ("duplicate", "escaped-duplicate"):
        key = (r'"admission_\u0066loor_evidence"' if bad_claim == "escaped-duplicate"
               else '"admission_floor_evidence"')
        other_raw = other_raw[:-1] + "," + key + ":" + json.dumps(other["admission_floor_evidence"]) + "}"
    raw = other_raw + "\n" + json.dumps(selected) + "\n"
    good = fr.bounded_capture.capture(command + ["--record-id", selected["record_id"], "--run-id", selected["run_id"]], input=raw)
    assert good.returncode == 0, good.stderr
    report = json.loads(good.stdout)
    assert report["input"]["row_count"] == 2
    assert report["row"]["result"] == "fail" and report["row"]["failures"] == 1
    assert report["row"]["cell_results"] == selected["cell_results"]
    assert report["row"]["admission_floor_evidence"] == selected["admission_floor_evidence"]
    bad = fr.bounded_capture.capture(command + ["--record-id", other["record_id"], "--run-id", other["run_id"]], input=raw)
    assert bad.returncode != 0 and bad.stdout == ""
    assert "cannot canonicalize the selected HistoryRow" in bad.stderr


def raw_duplicate_id(r, container, first, second):
    """Keep the actual duplicate tokens; Value/dict fixtures cannot prove this."""
    original = deepcopy(r)
    original.setdefault(container, {})["run_id"] = first
    encoded = json.dumps(original, separators=(",", ":"))
    nested = json.dumps(original[container], separators=(",", ":"))
    assert encoded.count(nested) == 1
    duplicate = nested[:-1] + ',"run_id":' + json.dumps(second) + '}'
    return encoded.replace(nested, duplicate)


@pytest.mark.parametrize("container", ["coverage", "cell_results", "test_results"])
@pytest.mark.parametrize("first,second", [("wrong", "actual-run-id"),
                                         ("actual-run-id", "wrong"),
                                         ("actual-run-id", "actual-run-id")],
                         ids=["wrong-first", "wrong-last", "equal"])
@pytest.mark.parametrize("shape", ["row", "event", "published"])
def test_public_raw_duplicate_ids_survive_each_receipt_input(
        repositories, tmp_path, container, first, second, shape):
    h, reverie, _git, c, _log = repositories
    r = complete_row(c, tmp_path / "context.json")
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    r.setdefault(container, {})["run_id"] = c["run_id"]
    for present in (False, True):
        original = deepcopy(r)
        if not present:
            del original["admission_floor_evidence"]
        # The published envelope must describe the actual legacy last-value
        # row; its original duplicate tokens still go to the public decoder.
        normalized = json.loads(raw_duplicate_id(original, container, first, second))
        digest_row = original if present else normalized
        digest = fr.canonical_row_sha256(digest_row, r["commit"])
        payload, flags = digest_input(original, shape, digest)
        row_text = json.dumps(original, separators=(",", ":"))
        encoded = json.dumps(payload, separators=(",", ":"))
        assert encoded.count(row_text) == 1
        raw = encoded.replace(row_text, raw_duplicate_id(original, container, first, second))
        command = [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
                   "receipt-digest", "--sha", r["commit"], *flags]
        child = fr.bounded_capture.capture(command, input=raw)
        if present:
            assert child.returncode != 0 and child.stdout == "", child.stdout
        else:
            normalized = json.loads(raw_duplicate_id(original, container, first, second))
            assert child.returncode == 0, child.stderr
            assert child.stdout.strip() == fr.canonical_row_sha256(normalized, r["commit"])


@pytest.mark.parametrize("where", ["row", "context", "artifact"])
def test_public_event_refuses_duplicate_admission_fields(repositories, tmp_path, where):
    h, reverie, _git, c, _log = repositories
    r = complete_row(c, tmp_path / "context.json")
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    digest = fr.canonical_row_sha256(r, r["commit"])
    payload, flags = digest_input(r, "event", digest)
    encoded = json.dumps(payload, separators=(",", ":"))
    if where == "row":
        key = "admission_floor_evidence"; value = r[key]
    elif where == "context":
        key = "contract"; value = c[key]
    else:
        key = "bytes"; value = r["admission_floor_evidence"]["artifact"][key]
    field = json.dumps(key) + ":" + json.dumps(value, separators=(",", ":"))
    assert encoded.count(field) == 1
    raw = encoded.replace(field, field + "," + field)
    child = fr.bounded_capture.capture(
        [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
         "receipt-digest", "--sha", r["commit"], *flags], input=raw)
    assert child.returncode != 0 and child.stdout == "", child.stdout


@pytest.mark.parametrize("container", ["coverage", "cell_results", "test_results"])
@pytest.mark.parametrize("first,second", [("wrong", "actual-run-id"),
                                         ("actual-run-id", "wrong"),
                                         ("actual-run-id", "actual-run-id")])
def test_python_public_input_refuses_duplicates_without_changing_legacy(
        repositories, tmp_path, container, first, second):
    h, reverie, _git, c, _log = repositories
    r = complete_row(c, tmp_path / "context.json")
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    for present in (False, True):
        original = deepcopy(r)
        if not present:
            del original["admission_floor_evidence"]
        raw = raw_duplicate_id(original, container, first, second)
        pred = qr.active()
        request = {"row": original, **{key: pred[key] for key in ("admission", "producer", "base", "coverage")}}
        encoded = json.dumps(request, separators=(",", ":"))
        row_text = json.dumps(original, separators=(",", ":"))
        assert encoded.count(row_text) == 1
        child = fr.bounded_capture.capture(
            [sys.executable, str(Path(qr.__file__)), "--coverage-base-admission-only"],
            input=encoded.replace(row_text, raw))
        report = json.loads(child.stdout)
        assert child.returncode == (1 if present else 0), child.stderr
        assert report["admission_status"] == ("admission-floor-invalid" if present else "satisfied")
        if present:
            assert report["refusal"] == "admission admission-floor-invalid"


def test_claimed_pair_transport_preserves_legacy_and_qualifying_identity():
    legacy = '{"future":1,"future":2,"coverage":{"run_id":"wrong","run_id":"right"}}'
    parsed = json.loads(legacy, object_pairs_hook=qr.AdmissionJsonObject)
    assert qr.admission_json_dumps(parsed) == json.dumps(json.loads(legacy), separators=(",", ":"))
    r = row(); raw = raw_duplicate_id(r, "coverage", "wrong", r["run_id"])
    parsed = json.loads(raw, object_pairs_hook=qr.AdmissionJsonObject)
    transported = qr.admission_json_dumps(parsed, sort_keys=True)
    assert '"run_id":"wrong","run_id":"actual-run-id"' in transported
    with pytest.raises(ValueError, match="duplicate selected row coverage.run_id"):
        qr.validate_admission_floor_evidence(parsed["admission_floor_evidence"], parsed)
    # Default dictionary encoding is visibly the old lossy route, not evidence.
    normalized = json.loads(json.dumps(parsed))
    assert qr.validate_admission_floor_evidence(normalized["admission_floor_evidence"], normalized)


@pytest.mark.parametrize("store", ["shard", "spool", "retained", "fixture"])
@pytest.mark.parametrize("order", ["bad-first", "good-first", "bad-only"])
def test_real_selected_scan_keeps_raw_duplicate_rows_separate(
        repositories, tmp_path, monkeypatch, store, order):
    h, reverie, _git, c, _log = repositories
    root = tmp_path / "state"; root.mkdir()
    artifact = tmp_path / "context.json"; artifact.write_bytes(raw_json(c))
    good = complete_row(c, artifact)
    good.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    good["coverage"]["run_id"] = c["run_id"]
    digest = fr.canonical_row_sha256(good, c["target_sha"])
    good_raw = json.dumps(good, separators=(",", ":"))
    bad_raw = raw_duplicate_id(good, "coverage", "wrong", c["run_id"])
    raws = {"bad-first": [bad_raw, good_raw], "good-first": [good_raw, bad_raw],
            "bad-only": [bad_raw]}[order]
    if store == "fixture":
        path = root / "rows.jsonl"
        path.write_text("\n".join(raws) + "\n")
        fixture = str(path)
    else:
        if store == "shard":
            path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
        else:
            pending = root / "ignored/ci-hub/validate-ledger-spool"
            path = pending / ("published" if store == "retained" else "") / "hermit__fixture-host__2026-09__fixture.jsonl"
        path.parent.mkdir(parents=True)
        events = []
        for index, raw in enumerate(raws):
            event, _flags = digest_input(good, "event", digest)
            event["event_id"] = f"fixture-{index}"
            encoded = json.dumps(event, separators=(",", ":"))
            assert encoded.count(good_raw) == 1
            events.append(encoded.replace(good_raw, raw))
        path.write_text("\n".join(events) + "\n")
        monkeypatch.setenv("DEV_HERMIT_PARENT", str(root))
        fixture = None
        # Legacy default view remains unchanged; the opt-in view must not
        # deduplicate a bad raw row with its normalized valid neighbour.
        assert len(fr.canonical_ledger.rows(root=root)) == 1
        guarded = fr.canonical_ledger.rows(root=root, preserve_admission=True)
        assert len(guarded) == len(raws)
    before = path.read_bytes()
    if order == "bad-only":
        with pytest.raises(fr.SelectedRowError, match="no canonical row"):
            fr.scan_and_finalize(fixture, str(h), c["target_sha"], digest, str(reverie), dry_run=True)
    else:
        result = fr.scan_and_finalize(fixture, str(h), c["target_sha"], digest, str(reverie), dry_run=True)
        assert result["reason"] == "would-mint" and result["satisfied"] is True
        assert result["appended"] == 0
    assert path.read_bytes() == before


def import_consumer(relative, name):
    import importlib.util
    path = Path(fr.__file__).resolve().parents[1] / relative
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    # Match Python's ordinary script-directory import resolution, without
    # leaving a fixture path installed for unrelated tests.
    prior_path = list(sys.path)
    sys.modules[name] = module
    try:
        sys.path.insert(0, str(path.parent))
        spec.loader.exec_module(module)
    finally:
        sys.path[:] = prior_path
    return module


@pytest.mark.parametrize("store", ["shard", "spool", "retained"])
@pytest.mark.parametrize("replacement", ["none", "bad", "good"])
def test_python_production_union_corrections_and_authority_readers(
        repositories, tmp_path, monkeypatch, store, replacement):
    h, reverie, _git, c, _log = repositories
    root = tmp_path / "state"; root.mkdir()
    artifact = root / "context.json"; artifact.write_bytes(raw_json(c))
    good = complete_row(c, artifact)
    good.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    good["coverage"]["run_id"] = c["run_id"]
    good_text = json.dumps(good, separators=(",", ":"))
    bad_text = raw_duplicate_id(good, "coverage", "wrong", c["run_id"])
    event, _flags = digest_input(good, "event", "0" * 64)
    event["event_id"] = "original-result"
    original_raw = bad_text if replacement == "good" else good_text
    if replacement == "none":
        original_raw = bad_text
    encoded = json.dumps(event, separators=(",", ":"))
    assert encoded.count(good_text) == 1
    lines = [encoded.replace(good_text, original_raw)]
    if replacement != "none":
        correction = deepcopy(event)
        correction.update(event_id="correction-result", event_type="run.correct",
                          supersedes=event["event_id"], reason="fixture raw-identity correction",
                          emitted_at="2026-09-18T15:11:00Z")
        del correction["outcome"]; del correction["commit"]
        encoded = json.dumps(correction, separators=(",", ":"))
        assert encoded.count(good_text) == 1
        lines.append(encoded.replace(good_text, bad_text if replacement == "bad" else good_text))
    if store == "shard":
        path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
    else:
        pending = root / "ignored/ci-hub/validate-ledger-spool"
        path = pending / ("published" if store == "retained" else "") / "hermit__fixture-host__2026-09__fixture.jsonl"
    path.parent.mkdir(parents=True)
    # Exercise the actual workspace-token restoration before admission/artifact
    # checks; the context file itself is the unchanged original byte evidence.
    path.write_text(("\n".join(lines) + "\n").replace(str(root), fr.canonical_ledger.WORKSPACE_TOKEN))
    before = path.read_bytes()
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(root))
    legacy = fr.canonical_ledger.rows(root=root)
    guarded = fr.canonical_ledger.rows(root=root, preserve_admission=True)
    assert len(legacy) == len(guarded) == 1
    assert qr.row_qualifies(legacy[0], c["target_sha"], qr.active())
    expected = replacement == "good"
    assert qr.row_qualifies(guarded[0], c["target_sha"], qr.active()) is expected
    assert guarded[0]["admission_floor_evidence"]["artifact"]["path"] == str(artifact)
    assert artifact.read_bytes() == raw_json(c)
    tool = Path(fr.__file__).resolve().parents[1] / "ledger/validate_rows.py"
    status = fr.bounded_capture.capture([sys.executable, str(tool), "status-input", "--preserve-admission"])
    assert status.returncode == 0, status.stderr
    transported = json.loads(status.stdout, object_pairs_hook=qr.AdmissionJsonObject)
    assert len(transported["records"]) == len(lines)
    assert qr.row_qualifies(transported["rows"][0], c["target_sha"], qr.active()) is expected
    anchor = import_consumer("validate/anchor_select.py", "admission_anchor_consumer")
    anchor_rows, malformed = anchor.load_ledger(root / "ledger")
    assert malformed == 0 and len(anchor_rows) == 1
    assert anchor.row_qualifies(anchor_rows[0], qr.active())[0] is expected
    history = import_consumer("history/query.py", "admission_history_consumer")
    indexed = history.load_ledger_index(str(root))
    assert len(indexed[c["target_sha"]]) == 1
    assert history._row_full_pass(indexed[c["target_sha"]][0]) is expected
    main = import_consumer("landing/main_soft_green.py", "admission_main_consumer")
    state, _detail = main.canonical_state(Path(fr.__file__).resolve().parents[2], c["target_sha"])
    assert state == ("HARD_GREEN" if expected else "NO_RESULT")
    assert path.read_bytes() == before


def test_fixture_python_and_selected_publisher_keep_original_duplicate_evidence(
        repositories, tmp_path):
    h, reverie, _git, c, _log = repositories
    r = complete_row(c, tmp_path / "evicted-context.json")
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    r["coverage"]["run_id"] = c["run_id"]
    good = json.dumps(r, separators=(",", ":"))
    bad = raw_duplicate_id(r, "coverage", "wrong", c["run_id"])
    ledger = tmp_path / "raw-rows.jsonl"; ledger.write_text(bad + "\n" + good + "\n")
    before = ledger.read_bytes()
    with ledger.open() as handle:
        rows = fr._load_rows(handle)
    assert len(rows) == 2
    assert [qr.row_qualifies(row, c["target_sha"], qr.active()) for row in rows] == [False, True]
    publisher = import_consumer("validation/publish_receipt.py", "admission_publisher_consumer")
    accepted = publisher.selected_record(good.encode(), sha=c["target_sha"],
        expected_digest=hashlib.sha256(good.encode()).hexdigest(), canonicalization=fr.RECEIPT_CANONICALIZATION)
    assert accepted == r
    with pytest.raises(SystemExit, match="does not satisfy the shared receipt predicate"):
        publisher.selected_record(bad.encode(), sha=c["target_sha"],
            expected_digest=hashlib.sha256(bad.encode()).hexdigest(), canonicalization=fr.RECEIPT_CANONICALIZATION)
    assert ledger.read_bytes() == before


def test_real_newest_green_default_cache_rejects_previous_decoder_policy(tmp_path):
    # Reuse the existing real Git/ledger fixture and ordinary CLI environment.
    # An old cache with the same inputs must miss without --no-cache or cleanup.
    authority = import_consumer("validation/test_local_producer_authority.py",
                                "admission_cache_authority_fixture")
    fixture = authority.LocalProducerAuthorityTests()
    repo, registry, shas = fixture.fixture(tmp_path)
    target = shas["candidate"]
    authority.run("git", "update-ref", "refs/remotes/origin/main", target, cwd=repo)
    positive = fixture.row(repo, target, "2026-08-08T12:10:00Z")
    positive["passed_tests"] = positive["executed_tests"]
    qualified = fr.bounded_capture.capture(
        [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
         "receipt-digest", "--sha", target, "--require-qualifying"], input=json.dumps(positive))
    assert qualified.returncode == 0 and len(qualified.stdout.strip()) == 64, qualified.stderr
    fixture.record(tmp_path, positive)
    cache = tmp_path / "newest-green-cache.json"
    args = ("newest-green", "--branch", "main", "--repo-dir", str(repo),
            "--cache", str(cache), "--no-fetch", "--json")
    first = fixture.hub(registry, *args)
    assert first.returncode == 0, first.stderr
    report = json.loads(first.stdout)
    assert report["cache_hit"] is False and report["report"]["green"]["sha"] == target
    original = json.loads(cache.read_text())
    assert original["schema_version"] == 9
    old = deepcopy(original)
    old["schema_version"] = 8
    old["report"]["green"]["sha"] = "0" * 40
    cache.write_text(json.dumps(old))
    refreshed = fixture.hub(registry, *args)
    assert refreshed.returncode == 0, refreshed.stderr
    report = json.loads(refreshed.stdout)
    assert report["cache_hit"] is False and report["report"]["green"]["sha"] == target
    replacement = json.loads(cache.read_text())
    assert replacement == original
    reused = fixture.hub(registry, *args)
    assert reused.returncode == 0, reused.stderr
    report = json.loads(reused.stdout)
    assert report["cache_hit"] is True and report["report"]["green"]["sha"] == target


@pytest.mark.parametrize("container", ["coverage", "cell_results", "test_results"])
def test_real_validate_status_preserves_bad_raw_row_and_good_same_sha(
        repositories, tmp_path, monkeypatch, container):
    h, reverie, _git, c, _log = repositories
    root = tmp_path / "state"; root.mkdir()
    good = complete_row(c, tmp_path / "evicted-context.json")
    good.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    if container == "cell_results":
        # A cell claim needs its supported schema and complete typed evidence;
        # schema 5's unrelated legacy-cell refusal must not mask this control.
        mutation = import_consumer("validate/tests/test_qualifying_receipt_mutation.py",
                                   "admission_status_cell_fixture")
        cells = mutation._schema6_cell_row(c["target_sha"], "compared-and-matched")["cell_results"]
        cells["run_id"] = c["run_id"]
        cells["artifact"]["path"] = "ignored/validate/artifacts/actual-run-id/cells.jsonl"
        good.update(schema_version=6, cell_results=cells)
    good.setdefault(container, {})["run_id"] = c["run_id"]
    assert len(fr.canonical_row_sha256(good, c["target_sha"], require_canonical_qualifying=True)) == 64
    good_text = json.dumps(good, separators=(",", ":"))
    bad_text = raw_duplicate_id(good, container, "wrong", c["run_id"])
    event, _flags = digest_input(good, "event", "0" * 64)
    encoded = json.dumps(event, separators=(",", ":"))
    assert encoded.count(good_text) == 1
    bad_event = encoded.replace(good_text, bad_text)
    path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
    path.parent.mkdir(parents=True)
    monkeypatch.setenv("HERMIT_VALIDATE_STOP_TEST_MODE", "1")
    monkeypatch.setenv("CI_HUB_VALIDATE_LEDGER_TEST_ROOT", str(root))
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(root))
    command = [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
               "validate-status", "--sha", c["target_sha"], "--repo", "rrnewton/hermit", "--json"]
    for include_good in (False, True):
        lines = [bad_event]
        if include_good:
            positive = deepcopy(event); positive["event_id"] += "-good"
            lines.append(json.dumps(positive, separators=(",", ":")))
        path.write_text("\n".join(lines) + "\n")
        before = path.read_bytes()
        result = fr.bounded_capture.capture(command)
        report = json.loads(result.stdout)
        assert report["qualifying_count"] == int(include_good), (result.stderr, report)
        assert (report["verdict"] == "VALIDATED") is include_good
        assert (result.returncode == 0) is include_good
        assert path.read_bytes() == before


@pytest.mark.parametrize("case", ["bad-only", "bad-beside-good", "frozen-only",
                                 "frozen-beside-good", "legacy", "failed-bad"])
def test_selected_receipt_copy_checks_original_claim_before_green_tuple(
        repositories, tmp_path, monkeypatch, case):
    h, reverie, _git, c, _log = repositories
    root = tmp_path / "state"; root.mkdir()
    good = complete_row(c, tmp_path / "evicted-context.json")
    good.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    good.update(cwd=str(h), finished_at="2026-09-18T15:10:00Z")
    good["coverage"]["run_id"] = c["run_id"]
    other = deepcopy(good)
    if case.startswith("frozen"):
        frozen = deepcopy(c)
        frozen["authority"]["kind"] = "frozen-validate"
        frozen["floor"].update(kind="frozen-target", sha=c["target_sha"], tree=c["target_tree"])
        frozen["pin_bindings"][0]["base_sha"] = c["target_sha"]
        other["admission_floor_evidence"] = evidence(frozen, tmp_path / "frozen.json")
        assert qr.admission_verdict(other, qr.active()) is qr.AdmissionVerdict.ADMISSION_NONCANONICAL
    elif case == "legacy":
        del other["admission_floor_evidence"]
    good_raw = json.dumps(good, separators=(",", ":"))
    other_raw = (raw_duplicate_id(other, "coverage", "wrong", c["run_id"])
                 if case.startswith("bad") or case == "failed-bad"
                 else json.dumps(other, separators=(",", ":")))
    raws = [good_raw, other_raw] if case.endswith("beside-good") else [other_raw]
    path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
    path.parent.mkdir(parents=True)
    lines = []
    for index, raw in enumerate(raws):
        event, _flags = digest_input(good, "event", "0" * 64)
        event["event_id"] = f"copy-fixture-{index}"
        encoded = json.dumps(event, separators=(",", ":"))
        assert encoded.count(good_raw) == 1
        lines.append(encoded.replace(good_raw, raw))
    path.write_text("\n".join(lines) + "\n")
    before = path.read_bytes()
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(root))
    start = import_consumer("validate/start_unit.py", "admission_start_unit_consumer")
    verdict = "FAILED" if case == "failed-bad" else "VALIDATED"
    rc = 3 if verdict == "FAILED" else 0
    identity = {key: good[field] for key, field in {
        "sha": "commit", "tree": "tree", "finished_at": "finished_at",
        "host": "host", "slot": "slot", "log_file": "log_file",
    }.items()}
    status = {"sha": c["target_sha"], "verdict": verdict, "exit_code": rc,
              "qualifying_count": int(rc == 0), "disqualified_count": int(rc != 0),
              "qualifying_receipts": [{"receipt_identity": {"tuple": identity}}]}
    calls = []

    def run(command, **kwargs):
        calls.append(command)
        if command[1] == "validate-status":
            # Supply the existing authority's response to isolate the copy
            # boundary. The canonical ledger transport below is real Python.
            return subprocess.CompletedProcess(command, rc, json.dumps(status), "")
        assert command[-2:] == ["rows", "--preserve-admission"]
        return subprocess.run(command, capture_output=True, text=True, **kwargs)

    arguments = (root, c["target_sha"], h, c["started_at"])
    options = {"repo": "rrnewton/hermit", "run": run,
               "tool_root": Path(fr.__file__).resolve().parents[2]}
    if case.endswith("only"):
        with pytest.raises(RuntimeError, match="no qualifying receipt belonging"):
            start.assert_row_readable_from_canonical_ledger(*arguments, **options)
    else:
        copied, actual_verdict, actual_rc = start.assert_row_readable_from_canonical_ledger(
            *arguments, **options)
        assert (actual_verdict, actual_rc) == (verdict, rc)
        parsed = json.loads(copied, object_pairs_hook=qr.AdmissionJsonObject)
        if case == "failed-bad":
            assert qr.admission_verdict(parsed, qr.active()) is qr.AdmissionVerdict.ADMISSION_FLOOR_INVALID
            assert '"run_id":"wrong","run_id":"actual-run-id"' in copied
        elif case == "legacy":
            assert parsed == other and "admission_floor_evidence" not in parsed
        else:
            assert parsed == good
            assert qr.admission_verdict(parsed, qr.active()) is qr.AdmissionVerdict.SATISFIED
    assert len(calls) == 2 and path.read_bytes() == before


@pytest.mark.parametrize("order", ["claimed-first", "claimed-last", "equal"])
def test_claimed_outer_duplicate_row_reaches_existing_envelope_checker(
        repositories, tmp_path, order):
    _h, _reverie, _git, c, _log = repositories
    claimed = complete_row(c, tmp_path / "evicted-context.json")
    legacy = deepcopy(claimed); del legacy["admission_floor_evidence"]
    root = tmp_path / "state"
    path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
    path.parent.mkdir(parents=True)
    checker = Path(fr.__file__).resolve().parents[1] / "ledger/check-ledger-events.rs"
    for present in (False, True):
        first, second = legacy, legacy
        if present:
            first, second = {"claimed-first": (claimed, legacy),
                             "claimed-last": (legacy, claimed),
                             "equal": (claimed, claimed)}[order]
        event, _flags = digest_input(legacy, "event", "0" * 64)
        encoded = json.dumps(event, separators=(",", ":"))
        field = '"legacy_row":' + json.dumps(legacy, separators=(",", ":"))
        duplicate = ','.join('"legacy_row":' + json.dumps(item, separators=(",", ":"))
                             for item in (first, second))
        assert encoded.count(field) == 1
        raw = encoded.replace(field, duplicate) + "\n"
        path.write_text(raw)
        # The existing Rust envelope type already rejects this exact input.
        direct = fr.bounded_capture.capture([str(checker)], input=raw)
        assert direct.returncode != 0 and "duplicate field `legacy_row`" in direct.stderr
        assert len(fr.canonical_ledger.rows(root=root)) == 1
        if present:
            with pytest.raises(ValueError, match="duplicate field `legacy_row`"):
                fr.canonical_ledger.rows(root=root, preserve_admission=True)
        else:
            # Absence retains the historical normalized view, rather than
            # extending the new contract to previously accepted old records.
            assert fr.canonical_ledger.rows(root=root, preserve_admission=True) == [legacy]
        assert path.read_text() == raw


@pytest.mark.parametrize("case,container", [
    ("current", None), ("legacy", None), ("bad-context", None), ("frozen", None),
    *[(case, container) for container in ("coverage", "cell_results", "test_results")
      for case in ("malformed-id", "wrong-first", "wrong-last", "equal")],
])
def test_real_qualified_rows_banked_frontdoor_preserves_present_claim(
        repositories, tmp_path, monkeypatch, case, container):
    # This is the existing timing population, formerly used by banked health.
    # It remains a separate front door from full canonical receipt qualification.
    h, reverie, _git, c, _log = repositories
    good = complete_row(c, tmp_path / "evicted-context.json")
    good.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    if container is not None:
        good.setdefault(container, {})["run_id"] = c["run_id"]
    candidate = deepcopy(good)
    if case == "legacy":
        del candidate["admission_floor_evidence"]
    elif case == "bad-context":
        other = deepcopy(c); other["run_id"] = "other-run"
        candidate["admission_floor_evidence"] = evidence(other)
    elif case == "frozen":
        frozen = deepcopy(c)
        frozen["authority"]["kind"] = "frozen-validate"
        frozen["floor"].update(kind="frozen-target", sha=c["target_sha"], tree=c["target_tree"])
        frozen["pin_bindings"][0]["base_sha"] = c["target_sha"]
        candidate["admission_floor_evidence"] = evidence(frozen)
    elif case == "malformed-id":
        candidate[container]["run_id"] = None
    encoded = json.dumps(candidate, separators=(",", ":"))
    if case in ("wrong-first", "wrong-last", "equal"):
        first, second = {"wrong-first": ("wrong", c["run_id"]),
                         "wrong-last": (c["run_id"], "wrong"),
                         "equal": (c["run_id"], c["run_id"])}[case]
        encoded = raw_duplicate_id(candidate, container, first, second)
    root = tmp_path / "state"
    path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
    path.parent.mkdir(parents=True)
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(root))
    monkeypatch.setenv("HERMIT_VALIDATE_STOP_TEST_MODE", "1")
    monkeypatch.setenv("CI_HUB_VALIDATE_LEDGER_TEST_ROOT", str(root))
    timing = import_consumer("validate/qualified_rows.py", "admission_qualified_consumer")
    tool = Path(fr.__file__).resolve().parents[1] / "ci-hub"
    event, _flags = digest_input(candidate, "event", "0" * 64)
    event["event_id"] = "candidate-result"
    candidate_text = json.dumps(candidate, separators=(",", ":"))
    event_text = json.dumps(event, separators=(",", ":"))
    assert event_text.count(candidate_text) == 1
    raw_event = event_text.replace(candidate_text, encoded)
    accepts = case in ("current", "legacy")
    for include_good in ((False,) if accepts else (False, True)):
        lines = [raw_event]
        raws = [encoded]
        if include_good:
            positive, _flags = digest_input(good, "event", "0" * 64)
            positive["event_id"] = "positive-neighbour"
            lines.append(json.dumps(positive, separators=(",", ":")))
            raws.append(json.dumps(good, separators=(",", ":")))
        path.write_text("\n".join(lines) + "\n")
        before = path.read_bytes()
        # Calls the ordinary executable -> ledger qualified-rows -> real union.
        # No mocked qualification result or replacement command supplies credit.
        expected = frozenset({c["target_sha"]}) if accepts or include_good else frozenset()
        qualified = fr.bounded_capture.capture([str(tool), "ledger", "qualified-rows"])
        assert qualified.returncode == 0, qualified.stderr
        actual = frozenset(json.loads(line)["commit"] for line in qualified.stdout.splitlines() if line.strip())
        assert actual == expected
        fixture = root / "rows.jsonl"; fixture.write_text("\n".join(raws) + "\n")
        fixture_before = fixture.read_bytes()
        loaded, malformed = timing.load_rows(fixture)
        assert malformed == 0 and len(loaded) == len(raws)
        assert [timing.is_qualified(item) for item in loaded] == [accepts] + ([True] if include_good else [])
        assert fixture.read_bytes() == fixture_before and path.read_bytes() == before


@pytest.mark.parametrize("case", ["valid", "bad-count", "missing-coverage", "failed-same-sha", "withheld-neighbour", "legacy-schema4"])
def test_real_health_banks_canonical_status_not_timing_population(
        repositories, tmp_path, monkeypatch, case):
    h, reverie, _git, c, _log = repositories
    good = complete_row(c, tmp_path / "evicted-context.json")
    good.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    candidate = deepcopy(good)
    if case == "bad-count":
        candidate["passed_tests"] = candidate["executed_tests"] - 1
    elif case == "missing-coverage":
        del candidate["coverage"]["absent_nodes"]
    elif case == "legacy-schema4":
        candidate["schema_version"] = 4
        del candidate["coverage"]
        del candidate["admission_floor_evidence"]
    rows = [candidate]
    if case in ("failed-same-sha", "withheld-neighbour"):
        other = deepcopy(good)
        other.update(result="fail", raw_result="fail", exit_code=1, failures=1,
                     record_id="failed-neighbour")
        other["gates"][0].update(result="fail", exit_code=1)
        if case == "failed-same-sha":
            other.update(dag_jobs=4, known_flaky_failure=False)
            other["gates"][0].update(failure_origin="outer_gate", failed_substeps=[])
        rows.append(other)
    root = tmp_path / "state"
    path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
    path.parent.mkdir(parents=True)
    lines = []
    for index, item in enumerate(rows):
        event, _flags = digest_input(item, "event", "0" * 64)
        event.update(event_id=f"canonical-health-{index}", outcome=item["result"])
        lines.append(json.dumps(event))
    path.write_text("\n".join(lines) + "\n"); before = path.read_bytes()
    for key, value in {"DEV_HERMIT_PARENT": str(root), "HERMIT_VALIDATE_STOP_TEST_MODE": "1",
                       "CI_HUB_VALIDATE_LEDGER_TEST_ROOT": str(root)}.items():
        monkeypatch.setenv(key, value)
    banked = import_consumer("health/pr_status.py", "canonical_health_consumer")
    timing = import_consumer("validate/qualified_rows.py", "canonical_health_timing")
    tool = Path(fr.__file__).resolve().parents[1] / "ci-hub"
    status = fr.bounded_capture.capture([str(tool), "validate-status", "--sha", c["target_sha"],
                                        "--repo", "rrnewton/hermit", "--json"])
    report = json.loads(status.stdout)
    expected = {"bad-count": (4, "NOT-VALIDATED", 0), "missing-coverage": (4, "NOT-VALIDATED", 0),
                "failed-same-sha": (3, "FAILED", 1)}.get(case, (0, "VALIDATED", 1))
    assert (status.returncode, report["verdict"], report["qualifying_count"]) == expected, report
    assert timing.is_qualified(candidate)
    actual = banked.banked_green_commits("rrnewton/hermit", [c["target_sha"]], ci_hub_bin=tool)
    assert actual == (frozenset({c["target_sha"]}) if expected[0] == 0 else frozenset())
    if case == "withheld-neighbour":
        assert report["withheld_nonpass_record_count"] == 1
    if case == "legacy-schema4":
        assert report["newest_qualifying"]["coverage_satisfied"] is None
    print(json.dumps({"case": case, "verdict": report["verdict"], "raw_exit": status.returncode,
                      "qualifying_count": report["qualifying_count"], "banked": sorted(actual)}), flush=True)
    assert path.read_bytes() == before


def test_real_health_keeps_original_reverie_receipt_authority(tmp_path, monkeypatch):
    fixture = Path(fr.__file__).resolve().parents[1] / "validation/fixtures/reverie-run150.json"
    raw = fixture.read_bytes()
    assert hashlib.sha256(raw).hexdigest() == "c6037137812d4d188afdf694ada914b749b6888813c73a111e600eaf4cfd3c58"
    original = json.loads(raw)
    root = tmp_path / "state"
    path = root / "ledger/reverie/devbig014/2026-09.jsonl"
    path.parent.mkdir(parents=True)
    # Normal historical-row event conversion preserves repository/identity and
    # replaces archived owner paths with the existing workspace tokens.
    event = fr.canonical_ledger._event_for_row(original, root)
    assert original == json.loads(raw)
    path.write_text(json.dumps(event) + "\n"); before = path.read_bytes()
    for key, value in {"DEV_HERMIT_PARENT": str(root), "HERMIT_VALIDATE_STOP_TEST_MODE": "1",
                       "CI_HUB_VALIDATE_LEDGER_TEST_ROOT": str(root)}.items():
        monkeypatch.setenv(key, value)
    tool = Path(fr.__file__).resolve().parents[1] / "ci-hub"
    result = fr.bounded_capture.capture([str(tool), "validate-status", "--sha", original["commit"],
                                        "--repo", "rrnewton/reverie", "--json"])
    assert result.stdout, (result.returncode, result.stderr)
    report = json.loads(result.stdout)
    assert (result.returncode, report["verdict"], report["qualifying_count"]) == (0, "VALIDATED", 1), report
    banked = import_consumer("health/pr_status.py", "canonical_reverie_health_consumer")
    assert banked.banked_green_commits("rrnewton/reverie", [original["commit"]], ci_hub_bin=tool) == frozenset({original["commit"]})
    assert fixture.read_bytes() == raw and path.read_bytes() == before


def test_real_health_unresolved_cell_obligation_overrides_positive_receipt(
        repositories, tmp_path, monkeypatch):
    h, reverie, git, c, _log = repositories
    good = complete_row(c, tmp_path / "evicted-context.json")
    good.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    # Reuse the existing typed canonical-divergence fixture and real Git
    # ancestry; no mocked obligation verdict or ancestry result supplies it.
    mutation = import_consumer("validate/tests/test_qualifying_receipt_mutation.py", "health_obligation_fixture")
    failed = mutation._schema6_cell_row(c["floor"]["sha"], "compared-and-diverged")
    failed.update(tree=c["floor"]["tree"], toolchain="fixture-rustc",
                  finished_at="2026-08-07T15:00:00Z", result="fail", raw_result="fail",
                  exit_code=1, failures=1, dag_jobs=4, known_flaky_failure=False)
    failed["gates"][0].update(result="fail", exit_code=1, failure_origin="outer_gate", failed_substeps=[])
    assert failed["finished_at"] < good["finished_at"]
    root = tmp_path / "state"; root.mkdir()
    (root / "hermit").symlink_to(h, target_is_directory=True)
    cells = failed["cell_results"]
    artifact = root / "ignored/validate/artifacts/schema6-test/cells.jsonl"
    artifact.parent.mkdir(parents=True)
    raw = (json.dumps({**cells["cells"][0], "run_id": cells["run_id"],
                       "hermit_sha": cells["hermit_sha"], "source_tree_dirty": False}) + "\n").encode()
    artifact.write_bytes(raw)
    cells["artifact"].update(path=str(artifact.relative_to(root)), sha256=hashlib.sha256(raw).hexdigest())
    # Exercise the existing retained-artifact check independently of the reader.
    mutation.validate_rows._require_cell_results(failed, root)
    assert git(h, "merge-base", c["floor"]["sha"], c["target_sha"]) == c["floor"]["sha"]
    path = root / "ledger/hermit/fixture-host/2026-09.jsonl"
    path.parent.mkdir(parents=True)
    lines = []
    for index, item in enumerate((good, failed)):
        event, _flags = digest_input(item, "event", "0" * 64)
        event.update(event_id=f"obligation-health-{index}", outcome=item["result"], emitted_at=item["finished_at"])
        lines.append(json.dumps(event))
    path.write_text("\n".join(lines) + "\n"); before = path.read_bytes()
    for key, value in {"DEV_HERMIT_PARENT": str(root), "HERMIT_VALIDATE_STOP_TEST_MODE": "1",
                       "CI_HUB_VALIDATE_LEDGER_TEST_ROOT": str(root)}.items():
        monkeypatch.setenv(key, value)
    tool = Path(fr.__file__).resolve().parents[1] / "ci-hub"
    result = fr.bounded_capture.capture([str(tool), "validate-status", "--sha", c["target_sha"],
                                        "--repo", "rrnewton/hermit", "--json"])
    assert result.stdout, (result.returncode, result.stderr)
    report = json.loads(result.stdout)
    assert (result.returncode, report["verdict"], report["qualifying_count"]) == (4, "NEEDS-RERUN", 1), report
    obligations = report["unresolved_failure_obligations"]
    assert len(obligations) == 1
    assert obligations[0]["failing_commit"] == c["floor"]["sha"]
    assert obligations[0]["observed_clean_legs_at_target"] == 0
    banked = import_consumer("health/pr_status.py", "canonical_obligation_health_consumer")
    assert banked.banked_green_commits("rrnewton/hermit", [c["target_sha"]], ci_hub_bin=tool) == frozenset()
    print(json.dumps({"case": "ancestor-obligation", "verdict": report["verdict"],
                      "qualifying_count": report["qualifying_count"], "obligations": obligations}), flush=True)
    assert path.read_bytes() == before and artifact.read_bytes() == raw


def portable_fixture(repositories, root):
    h, reverie, _git, original, original_log = repositories
    retained = root / "ignored/validate"
    retained.mkdir(parents=True)
    log = retained / "run.log"
    log.write_bytes(original_log.read_bytes())
    context_path = retained / "admission/context.json"
    context_path.parent.mkdir()
    c = deepcopy(original)
    c["log_file"] = str(log)
    r = complete_row(c, context_path)
    r.update(cwd=str(h), finished_at="2026-09-18T15:10:00Z")
    r.update(fr.build_base_evidence(str(h), c["target_sha"], str(reverie)))
    with AdmissionPaths(root) as paths:
        log_identity = paths.locator(str(log))
        artifact_locator = paths.locator(str(context_path))
    del c["log_file"]
    c.update(contract=qr.ADMISSION_CONTEXT_V2, log_identity=deepcopy(log_identity))
    raw = qr.admission_context_bytes(c)
    context_path.write_bytes(raw)
    r["log_identity"] = deepcopy(log_identity)
    r["admission_floor_evidence"] = {
        "context": c, "artifact": {"locator": artifact_locator,
            "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}}
    return r, context_path, raw


@pytest.mark.parametrize("name", ["basic", "escaped", "large-u64", "frozen"])
def test_v2_actual_rust_encoder_bytes_match_python(name):
    directory = Path(__file__).parent / "tests/fixtures/admission-floor-v2"
    provenance = json.loads((directory / "PROVENANCE.json").read_text())
    raw = (directory / f"{name}.json").read_bytes()
    binding = provenance["files"][f"{name}.json"]
    assert len(raw) == binding["bytes"]
    assert hashlib.sha256(raw).hexdigest() == binding["sha256"]
    assert qr.admission_context_bytes(json.loads(raw)) == raw


@pytest.mark.parametrize("path", ["", "/absolute", "a/", "a//b", ".", "a/../b",
    "a/./b", "a\\b", "a\nb", "a\x7fb", "a\x85b", "ignored/validate/home/user/log",
    "{{WORKSPACE_ROOT}}/x", "a/{{HOME}}/x"])
def test_v2_locator_refuses_noncanonical_identity(path):
    with pytest.raises(ValueError):
        qr.admission_workspace_locator({"scope": "workspace", "path": path})


@pytest.mark.parametrize("location", ["row", "row-inner", "context", "artifact"])
@pytest.mark.parametrize("first", ["different", "equal"])
def test_v2_raw_duplicate_log_identity_refuses(repositories, tmp_path, location, first):
    r, _file, _raw = portable_fixture(repositories, tmp_path)
    encoded = raw_json(r).decode()
    identity = raw_json(r["log_identity"]).decode()
    other = identity if first == "equal" else '{"scope":"workspace","path":"ignored/validate/other.log"}'
    if location == "row":
        needle = '"log_identity":' + identity
        position = encoded.rfind(needle)
        assert position >= 0
        encoded = encoded[:position] + needle.replace(identity, other) + ',' + encoded[position:]
    elif location == "row-inner":
        needle = '"log_identity":' + identity
        position = encoded.rfind(needle)
        duplicate = identity.replace('"path":', '"path":"ignored/validate/other.log","path":', 1)
        if first == "equal":
            duplicate = identity.replace('"path":', '"path":' + json.dumps(r["log_identity"]["path"]) + ',"path":', 1)
        encoded = encoded[:position] + encoded[position:].replace(identity, duplicate, 1)
    else:
        key = "log_identity" if location == "context" else "locator"
        needle = json.dumps(key) + ':' + identity
        if location == "artifact":
            identity = raw_json(r["admission_floor_evidence"]["artifact"]["locator"]).decode()
            other = identity if first == "equal" else other
            needle = '"locator":' + identity
        assert needle in encoded
        encoded = encoded.replace(needle, json.dumps(key) + ':' + other + ',' + needle, 1)
    loaded = json.loads(encoded, object_pairs_hook=qr.AdmissionJsonObject)
    with pytest.raises(ValueError, match="duplicate"):
        qr.validate_admission_floor_evidence(loaded["admission_floor_evidence"], loaded)


@pytest.mark.parametrize("other_root", [False, True])
def test_v2_real_writer_and_relocated_reader_preserve_exact_proof(repositories, tmp_path, other_root):
    r, context_path, raw = portable_fixture(repositories, tmp_path)
    original = deepcopy(r)
    result = fr.canonical_ledger.record(r, root=tmp_path)
    assert r == original
    recorded = (tmp_path / result["spool"]).read_bytes()
    event = json.loads(recorded)
    assert event["run_id"] == r["run_id"] != r["record_id"]
    root = tmp_path
    if other_root:
        root = tmp_path / "reader"
        shard = root / result["published_shard"]
        shard.parent.mkdir(parents=True)
        shard.write_bytes(recorded)
    [restored] = fr.canonical_ledger.rows(root=root, preserve_admission=True)
    assert qr.validate_admission_floor_evidence(restored["admission_floor_evidence"], restored) == raw
    assert qr.row_qualifies(restored, r["commit"], qr.active())
    assert restored["admission_floor_evidence"] == original["admission_floor_evidence"]
    assert restored["log_identity"] == original["log_identity"]
    assert restored["log_file"] == str(root / r["log_identity"]["path"])
    assert context_path.read_bytes() == raw
    assert (tmp_path / result["spool"]).read_bytes() == recorded
    # Public inline qualification has no artifact-survival requirement.
    context_path.unlink()
    assert qr.validate_admission_floor_evidence(restored["admission_floor_evidence"], restored) == raw
    with pytest.raises((OSError, ValueError)):
        fr.read_admission_context(str(root / r["admission_floor_evidence"]["artifact"]["locator"]["path"]),
                                  hashlib.sha256(raw).hexdigest(), state_root=root)


def test_v2_actual_emit_echo_and_root_log_binding(repositories, tmp_path, monkeypatch, capsys):
    r, path, raw = portable_fixture(repositories, tmp_path)
    h, reverie, *_ = repositories
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(tmp_path))
    argv = ["--emit-only", "--hermit-checkout", str(h), "--reverie-checkout", str(reverie),
            "--sha", r["commit"], "--log", r["log_file"], "--admission-context", str(path),
            "--admission-context-sha256", hashlib.sha256(raw).hexdigest(),
            "--admission-state-root", str(tmp_path)]
    assert fr.main(argv) == 0
    echoed = json.loads(capsys.readouterr().out)
    assert echoed["admission_floor_evidence"] == r["admission_floor_evidence"]
    wrong_log = tmp_path / "ignored/validate/other/run.log"
    wrong_log.parent.mkdir(); wrong_log.write_bytes(Path(r["log_file"]).read_bytes())
    wrong = list(argv); wrong[wrong.index("--log") + 1] = str(wrong_log)
    assert fr.main(wrong) == 2
    assert "logical identity" in capsys.readouterr().err
    wrong = list(argv); wrong[wrong.index("--admission-state-root") + 1] = str(h)
    assert fr.main(wrong) == 2
    assert "canonical ledger state root" in capsys.readouterr().err
    assert path.read_bytes() == raw


@pytest.mark.parametrize("kind", ["outside-retention", "directory-symlink", "file-symlink", "fifo", "changed-root"])
def test_v2_resolver_refuses_escaping_or_replaced_objects(tmp_path, kind):
    root = tmp_path / "state"; root.mkdir()
    retained = root / "ignored/validate"; retained.mkdir(parents=True)
    good = retained / "run.log"; good.write_bytes(b"retained log")
    with AdmissionPaths(root) as paths:
        identity = paths.locator(str(good))
        assert paths.read(identity) == b"retained log"
        if kind == "outside-retention":
            target = root / "secret"; target.write_bytes(b"private")
            identity = {"scope": "workspace", "path": "secret"}
        elif kind == "directory-symlink":
            outside = tmp_path / "outside"; outside.mkdir()
            (outside / "run.log").write_bytes(b"foreign")
            (retained / "link").symlink_to(outside, target_is_directory=True)
            identity = {"scope": "workspace", "path": "ignored/validate/link/run.log"}
        elif kind == "file-symlink":
            target = retained / "link"; target.symlink_to(good)
            identity = {"scope": "workspace", "path": "ignored/validate/link"}
        elif kind == "fifo":
            target = retained / "pipe"; os.mkfifo(target)
            identity = {"scope": "workspace", "path": "ignored/validate/pipe"}
        else:
            root.rename(tmp_path / "old-state"); root.mkdir()
        with pytest.raises((ValueError, OSError)):
            paths.read(identity)


def test_v2_writer_refuses_rewritten_proof_without_rehashing(repositories, tmp_path):
    r, path, raw = portable_fixture(repositories, tmp_path)
    c = r["admission_floor_evidence"]["context"]
    c["authority"]["owner_boot_id"] = str(tmp_path) + "/boot"
    # Construct an untrusted claim without using the producer encoder: an
    # owner path can already be refused there, before ledger redaction.
    changed = raw_json(c)
    r["admission_floor_evidence"]["artifact"].update(
        sha256=hashlib.sha256(changed).hexdigest(), bytes=len(changed))
    before = deepcopy(r)
    with pytest.raises(ValueError, match="token|rendering|owner path"):
        fr.canonical_ledger._event_for_row(r, tmp_path)
    assert r == before and path.read_bytes() == raw


def test_v2_publisher_uses_bound_log_and_never_display_fallback(repositories, tmp_path):
    r, _path, _raw = portable_fixture(repositories, tmp_path)
    publisher = import_consumer("validation/publish_receipt.py", "v2_publish_receipt")
    original = Path(r["log_file"]).read_bytes()
    decoy = tmp_path / "decoy.log"; decoy.write_bytes(b"wrong run")
    r["log_file"] = str(decoy)
    saved = publisher.preserve_log(tmp_path / "published", r["commit"], r, state_root=tmp_path)
    assert saved.read_bytes() == original
    (tmp_path / r["log_identity"]["path"]).unlink()
    with pytest.raises(SystemExit, match="bound log is unavailable"):
        publisher.preserve_log(tmp_path / "published", r["commit"], r, state_root=tmp_path)


@pytest.mark.parametrize("shape", ["row", "event", "published"])
@pytest.mark.parametrize("duplicate", [False, True])
def test_v2_public_original_row_and_raw_event(repositories, tmp_path, shape, duplicate):
    r, path, raw = portable_fixture(repositories, tmp_path)
    writer = fr.canonical_ledger.record(r, root=tmp_path)
    recorded = (tmp_path / writer["spool"]).read_bytes()
    digest = fr.canonical_row_sha256(r, r["commit"])
    payload, flags = digest_input(r, shape, digest)
    if shape == "event":
        # These are the real writer's original pre-expansion bytes, including
        # the independent schema-5 event run_id, not a hand-built envelope.
        encoded = recorded.decode().strip()
    else:
        encoded = json.dumps(payload, separators=(",", ":"))
    if duplicate:
        # Equal duplicates are ambiguous evidence too. Locate the independent
        # row identity (after its nested context) without normalizing JSON.
        prefix = '"log_identity":'
        position = encoded.rfind(prefix)
        assert position >= 0
        value_start = position + len(prefix)
        while encoded[value_start].isspace():
            value_start += 1
        value, end = json.JSONDecoder().raw_decode(encoded, value_start)
        assert value == r["log_identity"]
        field = encoded[position:end]
        encoded = encoded[:position] + field + ',' + encoded[position:]
    path.unlink()
    child = fr.bounded_capture.capture(
        [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
         "receipt-digest", "--sha", r["commit"], *flags,
         "--require-canonical-qualifying", "--canonical-row"], input=encoded)
    if duplicate:
        assert child.returncode != 0 and child.stdout == "", child.stdout
    else:
        assert child.returncode == 0, child.stderr
        decoded = json.loads(child.stdout)
        assert decoded["log_identity"] == r["log_identity"]
        assert raw_json(decoded["admission_floor_evidence"]["context"]) == raw
        assert decoded["admission_floor_evidence"] == r["admission_floor_evidence"]
    assert (tmp_path / writer["spool"]).read_bytes() == recorded


def test_v2_selected_copy_compares_log_identity_with_all_other_tuple_fields(repositories, tmp_path):
    r, _path, _raw = portable_fixture(repositories, tmp_path)
    start = import_consumer("validate/start_unit.py", "v2_selected_copy")
    selected = {key: r[field] for key, field in {
        "sha": "commit", "tree": "tree", "finished_at": "finished_at",
        "host": "host", "slot": "slot", "log_file": "log_file",
        "log_identity": "log_identity",
    }.items()}
    selected["log_file"] = "/different-reader-root/ignored/validate/run.log"
    receipt = {"receipt_identity": {"tuple": selected}}
    assert start.row_is_selected_qualifying_receipt(r, [receipt])
    for key in ("sha", "tree", "finished_at", "host", "slot", "log_identity"):
        wrong = deepcopy(receipt)
        wrong["receipt_identity"]["tuple"][key] = None
        assert not start.row_is_selected_qualifying_receipt(r, [wrong])
    malformed = deepcopy(r)
    malformed["log_identity"] = {"scope": "workspace", "path": "ignored/validate/other/run.log"}
    assert not start.row_is_selected_qualifying_receipt(malformed, [receipt])


def test_v2_local_scan_requires_original_artifact_and_refuses_missing_file(repositories, tmp_path, monkeypatch):
    r, path, raw = portable_fixture(repositories, tmp_path)
    h, reverie, *_ = repositories
    ledger = tmp_path / "selected.jsonl"
    ledger.write_text(json.dumps(r) + '\n')
    before = ledger.read_bytes()
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(tmp_path))
    digest = fr.canonical_row_sha256(r, r["commit"])
    result = fr.scan_and_finalize(str(ledger), str(h), r["commit"], digest, str(reverie), dry_run=True)
    assert result["satisfied"] is True, result
    assert result["appended"] == 0 and ledger.read_bytes() == before
    path.unlink()
    refused = fr.scan_and_finalize(str(ledger), str(h), r["commit"], digest, str(reverie), dry_run=True)
    assert refused["satisfied"] is False and refused["reason"] == "invalid-admission-floor"
    assert refused["appended"] == 0 and ledger.read_bytes() == before
    assert qr.validate_admission_floor_evidence(r["admission_floor_evidence"], r) == raw


@pytest.mark.parametrize("first", ["wrong", "equal"])
def test_v2_real_status_keeps_duplicate_log_identity_out_of_credit(repositories, tmp_path, monkeypatch, first):
    r, _path, _raw = portable_fixture(repositories, tmp_path)
    event = fr.canonical_ledger._event_for_row(r, tmp_path)
    encoded = json.dumps(event, separators=(",", ":"))
    prefix = '"log_identity":'
    position = encoded.rfind(prefix)
    assert position >= 0
    identity, end = json.JSONDecoder().raw_decode(encoded, position + len(prefix))
    assert identity == r["log_identity"]
    if first == "wrong":
        identity = {"scope": "workspace", "path": "ignored/validate/another/run.log"}
    bad = encoded[:position] + prefix + json.dumps(identity, separators=(",", ":")) + ',' + encoded[position:]
    ledger = tmp_path / "ledger/hermit/fixture-host/2026-09.jsonl"
    ledger.parent.mkdir(parents=True)
    monkeypatch.setenv("HERMIT_VALIDATE_STOP_TEST_MODE", "1")
    monkeypatch.setenv("CI_HUB_VALIDATE_LEDGER_TEST_ROOT", str(tmp_path))
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(tmp_path))
    command = [str(Path(fr.__file__).resolve().parents[1] / "ci-hub"),
               "validate-status", "--sha", r["commit"], "--repo", "rrnewton/hermit", "--json"]
    for include_good in (False, True):
        lines = [bad]
        if include_good:
            good_event = deepcopy(event); good_event["event_id"] += "-good"
            lines.append(json.dumps(good_event, separators=(",", ":")))
        ledger.write_text('\n'.join(lines) + '\n')
        before = ledger.read_bytes()
        result = fr.bounded_capture.capture(command)
        report = json.loads(result.stdout)
        assert report["qualifying_count"] == int(include_good), (result.stderr, report)
        assert (report["verdict"] == "VALIDATED") is include_good
        assert (result.returncode == 0) is include_good
        if include_good:
            chosen = report["qualifying_receipts"][0]
            assert chosen["log_identity"] == r["log_identity"]
            assert chosen["receipt_identity"]["tuple"]["log_identity"] == r["log_identity"]
        assert ledger.read_bytes() == before


def record_cli(source, root, *, metadata=False):
    environment = {**os.environ, 'DEV_HERMIT_PARENT': str(root)}
    environment.pop('CI_HUB_VALIDATE_RUN_NUMBER', None)
    environment.pop('CI_HUB_VALIDATE_BRANCH', None)
    if metadata:
        environment.update(CI_HUB_VALIDATE_RUN_NUMBER='701', CI_HUB_VALIDATE_BRANCH='codex/admitted')
    return subprocess.run([sys.executable, str(Path(fr.canonical_ledger.__file__)), 'record'],
                          input=source, capture_output=True, text=True, env=environment)


def spool_bytes(root):
    directory = fr.canonical_ledger.live_spool_dir(root)
    return {str(p.relative_to(root)): p.read_bytes() for p in directory.rglob('*') if p.is_file()}


def record_claim(repositories, root, field):
    r, path, raw = portable_fixture(repositories, root)
    if field in ('coverage', 'cell_results', 'test_results'):
        r['coverage']['run_id'] = r['run_id']
        r['test_results'] = {'run_id': r['run_id']}
    if field == 'cell_results':
        fixtures = import_consumer('ledger/tests/test_validate_rows.py', 'raw_record_cli_cells')
        fixtures.with_cell_results(r, root, run_id=r['run_id'])
    return r, path, raw


@pytest.mark.parametrize('field', ['run_id', 'log_identity', 'admission_floor_evidence', 'coverage', 'cell_results', 'test_results'])
@pytest.mark.parametrize('variant', ['wrong-first', 'equal', 'escaped-equal'])
def test_record_cli_preserves_original_claim_before_write(repositories, tmp_path, field, variant):
    r, path, raw = record_claim(repositories, tmp_path, field)
    encoded = raw_json(r).decode()
    good = record_cli(encoded, tmp_path)
    assert good.returncode == 0, good.stderr
    before = spool_bytes(tmp_path)
    assert before
    nested = field in ('coverage', 'cell_results', 'test_results')
    target = r[field] if nested else r
    key = 'run_id' if nested else field
    value = target[key]
    spelling = json.dumps(key)
    if variant == 'escaped-equal':
        spelling = '"' + key[:-1] + '\\u%04x' % ord(key[-1]) + '"'
    first = value
    if variant == 'wrong-first':
        first = 'wrong-run' if key == 'run_id' else None
    needle = json.dumps(key) + ':' + raw_json(value).decode()
    container = raw_json(target).decode()
    position = container.rfind(needle)
    assert position >= 0
    broken = container[:position] + spelling + ':' + raw_json(first).decode() + ',' + container[position:]
    if nested:
        needle = json.dumps(field) + ':' + container
        assert encoded.count(needle) == 1
        broken = encoded.replace(needle, json.dumps(field) + ':' + broken, 1)
    child = record_cli(broken, tmp_path)
    print(json.dumps({'field': field, 'variant': variant, 'raw_exit': child.returncode,
                      'stdout': child.stdout, 'stderr': child.stderr}), flush=True)
    assert child.returncode == 2 and child.stdout == '', child.stdout + child.stderr
    assert 'duplicate' in child.stderr
    assert spool_bytes(tmp_path) == before
    assert path.read_bytes() == raw


@pytest.mark.parametrize('claimed', [False, True])
def test_record_cli_keeps_valid_claim_and_legacy_display_normalization(repositories, tmp_path, claimed):
    r, path, raw = portable_fixture(repositories, tmp_path)
    if not claimed:
        del r['admission_floor_evidence']
        del r['log_identity']
    r.update(run_number='stale-invalid', branch='stale-invalid\nbranch')
    encoded = raw_json(r).decode()
    if not claimed:
        # Historical absence retains json.load's last-value dictionary view.
        encoded = encoded.replace('"run_id":', '"run_id":"legacy-earlier","run_id":', 1)
    child = record_cli(encoded, tmp_path, metadata=True)
    assert child.returncode == 0, child.stderr
    answer = json.loads(child.stdout)
    saved = json.loads((tmp_path / answer['spool']).read_text())['legacy_row']
    assert saved['run_number'] == 701 and saved['branch'] == 'codex/admitted'
    if claimed:
        assert saved['admission_floor_evidence'] == r['admission_floor_evidence']
        assert saved['log_identity'] == r['log_identity']
    else:
        assert 'admission_floor_evidence' not in saved
        assert saved['run_id'] == r['run_id']
    assert path.read_bytes() == raw


@pytest.mark.parametrize('component', ['context', 'artifact', 'log-identity'])
def test_record_rechecks_original_identity_after_enrichment(repositories, tmp_path, monkeypatch, component):
    r, path, raw = portable_fixture(repositories, tmp_path)
    before = deepcopy(r)
    original_enrichment = fr.canonical_ledger._enrich_failure_classification

    def rewrite_after_original_inspection(value, root, **kwargs):
        original_enrichment(value, root, **kwargs)
        evidence = value['admission_floor_evidence']
        if component == 'context':
            evidence['context']['authority']['owner_boot_id'] = 'different-valid-boot'
        elif component == 'artifact':
            evidence['artifact']['locator']['path'] = 'ignored/validate/other-context.json'
        else:
            identity = {'scope': 'workspace', 'path': 'ignored/validate/other.log'}
            value['log_identity'] = deepcopy(identity)
            evidence['context']['log_identity'] = identity
        # Even a self-consistent rewrite cannot replace the original admitted
        # identity while the writer attaches display/failure metadata.
        encoded = qr.admission_context_bytes(evidence['context'])
        evidence['artifact'].update(sha256=hashlib.sha256(encoded).hexdigest(), bytes=len(encoded))

    monkeypatch.setattr(fr.canonical_ledger, '_enrich_failure_classification', rewrite_after_original_inspection)
    with pytest.raises(ValueError, match='normalization changed admitted evidence identity'):
        fr.canonical_ledger.record(r, root=tmp_path)
    assert r == before
    assert spool_bytes(tmp_path) == {}
    assert path.read_bytes() == raw
