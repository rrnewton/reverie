#!/usr/bin/env python3
"""Rebase-wrapper: soft-green is a confidence LEVEL; an absent judgement is REFUSED.

Verifies BOTH directions the owner named: a zero-conflict rebase is soft-greened
mechanically; a conflicted rebase WITHOUT a risk judgement is refused, never
defaulted to green. Also that the base's floor status is carried (a clean rebase
onto a sub-floor base is NOT landable), that a receipt at the PUSHED head Z is
carried (a push with NO receipt is NOT landable, and a receipt appearing later
flips Z eligible via the live re-check), and that the lander's `eligible` query
answers every direction.
"""
from __future__ import annotations

import importlib.util
import json
import subprocess
from pathlib import Path


PATH = Path(__file__).with_name("rebase_wrapper.py")
SPEC = importlib.util.spec_from_file_location("rebase_wrapper", PATH)
assert SPEC and SPEC.loader
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)

X = "a" * 40
Y = "b" * 40
Z = "c" * 40
Z2 = "d" * 40


def _cp(returncode=0, stdout="", stderr=""):
    return subprocess.CompletedProcess(args=[], returncode=returncode,
                                       stdout=stdout, stderr=stderr)


# --------------------------------------------------------------------------- #
# derive_verdict: the pure heart of the mechanism (both directions)            #
# --------------------------------------------------------------------------- #
def test_zero_conflict_is_soft_green_zero_conflict_and_landable() -> None:
    v = M.derive_verdict([], None, None, base_clears_floor=True, base_unmet=[])
    assert v["soft_green"] == M.SOFT_ZERO_CONFLICT
    assert v["risk_judgement"] == M.RISK_NA
    assert v["landable"] is True


def test_zero_conflict_but_base_below_floor_is_not_landable() -> None:
    unmet = [{"sha": "e" * 40, "kind": "merge-gate", "field": "merge-gate-v2"}]
    v = M.derive_verdict([], None, None, base_clears_floor=False, base_unmet=unmet)
    # Confidence LEVEL still records the zero-conflict bet ...
    assert v["soft_green"] == M.SOFT_ZERO_CONFLICT
    # ... but landability carries the base: a sub-floor base yields unlandable Z.
    assert v["landable"] is False
    assert "unlandable-base-below-floor" in v["landable_reason"]


def test_conflict_retained_is_soft_green_resolver_judged() -> None:
    v = M.derive_verdict(["src/lib.rs"], M.RISK_RETAIN, "trivial import reorder",
                         base_clears_floor=True, base_unmet=[])
    assert v["soft_green"] == M.SOFT_RESOLVER_JUDGED
    assert v["landable"] is True


def test_conflict_needs_full_validate_is_not_soft_green() -> None:
    v = M.derive_verdict(["src/lib.rs"], M.RISK_VALIDATE, "touches shared fixture",
                         base_clears_floor=True, base_unmet=[])
    assert v["soft_green"] is None
    assert v["landable"] is False
    assert "needs-full-validate" in v["landable_reason"]


def test_conflict_without_judgement_is_refused() -> None:
    for bad in (None, "", "green", "lgtm"):
        try:
            M.derive_verdict(["src/lib.rs"], bad, "some rationale",
                             base_clears_floor=True, base_unmet=[])
        except M.Refused:
            continue
        raise AssertionError(f"absent/invalid judgement {bad!r} must be REFUSED")


def test_conflict_with_judgement_but_no_rationale_is_refused() -> None:
    try:
        M.derive_verdict(["src/lib.rs"], M.RISK_RETAIN, "   ",
                         base_clears_floor=True, base_unmet=[])
    except M.Refused:
        return
    raise AssertionError("a bare judgement with no rationale must be REFUSED")


def test_soft_green_levels_are_distinguishable() -> None:
    assert M.SOFT_ZERO_CONFLICT != M.SOFT_RESOLVER_JUDGED


# --------------------------------------------------------------------------- #
# receipt at Z: the afternoon-cost gap. A push with NO receipt is NOT landable. #
# --------------------------------------------------------------------------- #
def test_zero_conflict_no_receipt_at_pushed_head_is_not_landable() -> None:
    v = M.derive_verdict([], None, None, base_clears_floor=True, base_unmet=[],
                         receipt_present=False, result=Z)
    # The confidence LEVEL still records the zero-conflict bet ...
    assert v["soft_green"] == M.SOFT_ZERO_CONFLICT
    # ... but with no receipt bound to the pushed head, Z is NOT landable.
    assert v["landable"] is False
    assert "no-receipt-at-pushed-head" in v["landable_reason"]


def test_zero_conflict_with_receipt_is_landable() -> None:
    v = M.derive_verdict([], None, None, base_clears_floor=True, base_unmet=[],
                         receipt_present=True, result=Z)
    assert v["landable"] is True
    assert "receipt bound at Z" in v["landable_reason"]


def test_resolver_retained_but_no_receipt_is_not_landable() -> None:
    v = M.derive_verdict(["src/lib.rs"], M.RISK_RETAIN, "trivial reorder",
                         base_clears_floor=True, base_unmet=[],
                         receipt_present=False, result=Z)
    assert v["soft_green"] == M.SOFT_RESOLVER_JUDGED
    assert v["landable"] is False
    assert "no-receipt-at-pushed-head" in v["landable_reason"]


def test_landable_reason_order_floor_before_receipt() -> None:
    # Both the floor and the receipt fail; the floor is named first (it makes the
    # base unusable regardless of any receipt on the derived Z).
    unmet = [{"sha": "e" * 40, "kind": "merge-gate", "field": "v2"}]
    r = M.landable_reason(M.SOFT_ZERO_CONFLICT, base_clears_floor=False,
                          base_unmet=unmet, receipt_present=False, result=Z)
    assert "unlandable-base-below-floor" in r


# --------------------------------------------------------------------------- #
# receipt_identity: pure map from validate-status report -> receipt or None    #
# --------------------------------------------------------------------------- #
def test_receipt_identity_validated_carries_what_it_verified() -> None:
    report = {"verdict": "VALIDATED", "qualifying_count": 1,
              "newest_qualifying": {"profile": "full", "selection_mode": "full",
                                    "result": "pass", "finished_at": "t",
                                    "slot": "lander2", "host": "testhost"}}
    r = M.receipt_identity(report, Z)
    assert r is not None
    assert r["sha"] == Z and r["verdict"] == "VALIDATED" and r["profile"] == "full"


def test_receipt_identity_not_validated_is_none() -> None:
    assert M.receipt_identity({"verdict": "NOT-VALIDATED", "qualifying_count": 0,
                               "newest_qualifying": None}, Z) is None
    # A VALIDATED verdict with no qualifying record is still no receipt.
    assert M.receipt_identity({"verdict": "VALIDATED", "qualifying_count": 0,
                               "newest_qualifying": None}, Z) is None
    assert M.receipt_identity(None, Z) is None


# --------------------------------------------------------------------------- #
# parse_conflicts                                                              #
# --------------------------------------------------------------------------- #
def test_parse_conflicts() -> None:
    assert M.parse_conflicts(None) == []
    assert M.parse_conflicts("none") == []
    assert M.parse_conflicts("  NONE ") == []
    assert M.parse_conflicts("a.rs,b.rs") == ["a.rs", "b.rs"]
    assert M.parse_conflicts("a.rs b.rs") == ["a.rs", "b.rs"]


# --------------------------------------------------------------------------- #
# store + latest-per-Z                                                         #
# --------------------------------------------------------------------------- #
def test_store_roundtrip_and_latest_wins(tmp_path) -> None:
    store = str(tmp_path / "rebase-records.jsonl")
    M.append_record(store, {"result": Z, "soft_green": None, "note": "first"})
    M.append_record(store, {"result": Z, "soft_green": M.SOFT_RESOLVER_JUDGED,
                            "note": "second"})
    recs = M.load_records(store)
    assert len(recs) == 2
    latest = M.latest_by_result(recs)
    # Re-record supersedes: the later line wins for the same Z.
    assert latest[Z]["note"] == "second"


# --------------------------------------------------------------------------- #
# do_record + do_eligible end-to-end (git stubbed out)                         #
# --------------------------------------------------------------------------- #
_STUB_RECEIPT = {"sha": Z, "verdict": "VALIDATED", "profile": "full",
                 "qualifying_count": 1}


def _args(argv, store, monkeypatch, clears=True, unmet=None, receipt=True):
    """Parse argv, point the store at tmp, and stub the floor + receipt checks +
    clock. `receipt=True` binds a validated receipt at Z; False/None binds none."""
    args = M.build_parser().parse_args(argv + ["--store", store, "--no-fetch"]
                                       if "record" == argv[0] else argv + ["--store", store])
    monkeypatch.setattr(M, "base_floor_status",
                        lambda *a, **k: {"ok": clears, "unmet": unmet or []})
    monkeypatch.setattr(M, "receipt_at",
                        lambda *a, **k: (_STUB_RECEIPT if receipt else None))
    monkeypatch.setattr(M, "utc_now", lambda: "2026-08-04T00:00:00Z")
    return args


def test_do_record_zero_conflict_then_eligible_query(tmp_path, monkeypatch) -> None:
    store = str(tmp_path / "s.jsonl")
    rc = M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z,
         "--conflicts", "none"], store, monkeypatch))
    assert rc == M.EXIT_OK
    rec = M.load_records(store)[-1]
    assert rec["soft_green"] == M.SOFT_ZERO_CONFLICT and rec["landable"] is True

    # Lander QUERIES: this exact head is eligible -> exit 0 (trust the frozen
    # floor + receipt snapshot; live re-checks are exercised separately).
    q = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor",
         "--no-recheck-receipt"])
    assert M.do_eligible(q) == M.EXIT_OK


def test_do_record_conflict_without_judgement_refused(tmp_path, monkeypatch) -> None:
    store = str(tmp_path / "s.jsonl")
    try:
        M.do_record(_args(
            ["record", "--source", X, "--base", Y, "--result", Z,
             "--conflicts", "a.rs"], store, monkeypatch))
    except M.Refused:
        # And nothing eligible was written.
        assert M.load_records(store) == []
        return
    raise AssertionError("conflicted record with no judgement must be REFUSED")


def test_eligible_result_not_landable_exits_refused(tmp_path, monkeypatch) -> None:
    store = str(tmp_path / "s.jsonl")
    # needs-full-validate => recorded but not landable.
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "a.rs", "--risk-judgement", M.RISK_VALIDATE, "--rationale", "fixture"],
        store, monkeypatch))
    q = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor",
         "--no-recheck-receipt"])
    assert M.do_eligible(q) == M.EXIT_REFUSED


def test_eligible_unknown_head_is_refused(tmp_path) -> None:
    store = str(tmp_path / "empty.jsonl")
    q = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor",
         "--no-recheck-receipt"])
    try:
        M.do_eligible(q)
    except M.Refused:
        return
    raise AssertionError("querying an unrecorded head must be REFUSED")


def test_eligible_recheck_floor_demotes_stale_record(tmp_path, monkeypatch) -> None:
    """A floor added AFTER recording must demote a previously-landable head."""
    store = str(tmp_path / "s.jsonl")
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, clears=True))
    assert M.load_records(store)[-1]["landable"] is True
    # Now a new floor is live; the base no longer clears it.
    monkeypatch.setattr(M.gate_floors, "load_floors", lambda p: [{"sha": "f" * 40}])
    monkeypatch.setattr(M.gate_floors, "clears_all",
                        lambda floors, co, base: {
                            "ok": False,
                            "unmet": [{"sha": "f" * 40, "kind": "merge-gate",
                                       "field": "v3"}]})
    # Trust the frozen receipt so the ONLY demoting factor is the new floor.
    q = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-receipt"])
    assert M.do_eligible(q) == M.EXIT_REFUSED


def test_do_record_no_receipt_is_not_landable(tmp_path, monkeypatch) -> None:
    """A clean rebase whose pushed head Z has NO receipt yet is recorded VISIBLY
    with receipt_at_Z=null and NOT-LANDABLE -- the afternoon-cost gap, closed."""
    store = str(tmp_path / "s.jsonl")
    rc = M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, receipt=False))
    assert rc == M.EXIT_OK
    rec = M.load_records(store)[-1]
    assert rec["soft_green"] == M.SOFT_ZERO_CONFLICT   # the bet is still recorded
    assert rec["receipt_at_Z"] is None                  # ... but null is VISIBLE
    assert rec["landable"] is False
    q = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor",
         "--no-recheck-receipt"])
    assert M.do_eligible(q) == M.EXIT_REFUSED


def test_eligible_live_receipt_flips_head_landable(tmp_path, monkeypatch) -> None:
    """The mailbox-free promotion: a head recorded with NO receipt becomes eligible
    once a receipt is bound live to the exact pushed Z -- no re-record needed."""
    store = str(tmp_path / "s.jsonl")
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, receipt=False))
    # Frozen snapshot: still not eligible.
    q0 = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor",
         "--no-recheck-receipt"])
    assert M.do_eligible(q0) == M.EXIT_REFUSED
    # Validate at Z now completes; the live re-check dereferences the authority.
    # The live path is `receipt_status` (tri-state), NOT the record-time snapshot
    # helper `receipt_at` -- stub the authority the re-check actually calls.
    monkeypatch.setattr(M, "receipt_status", lambda z: {
        "status": M.RECEIPT_VALIDATED, "identity": _STUB_RECEIPT, "detail": ""})
    q1 = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor"])
    assert M.do_eligible(q1) == M.EXIT_OK


def test_eligible_live_receipt_revocation_demotes(tmp_path, monkeypatch) -> None:
    """Symmetric: a head recorded landable is demoted if the live receipt vanishes
    (e.g. the ledger record was superseded / the push rewrote Z again)."""
    store = str(tmp_path / "s.jsonl")
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, receipt=True))
    monkeypatch.setattr(M, "receipt_at", lambda z: None)
    q = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor"])
    assert M.do_eligible(q) == M.EXIT_REFUSED


# --------------------------------------------------------------------------- #
# Fix A: canonical store resolution -- a COPY must not diverge onto its own store #
# --------------------------------------------------------------------------- #
def test_default_store_env_override_wins(monkeypatch, tmp_path) -> None:
    """CI_HUB_REBASE_STORE pins ONE shared path regardless of __file__/parent."""
    target = str(tmp_path / "shared" / "rebase-records.jsonl")
    monkeypatch.setenv("CI_HUB_REBASE_STORE", target)
    assert M.default_store() == __import__("os").path.abspath(target)


def test_default_store_anchored_to_parent_env_not_file(monkeypatch, tmp_path) -> None:
    """With no explicit store, the path anchors to DEV_HERMIT_PARENT -- so a copy
    of the wrapper with a DIFFERENT __file__ still resolves the SAME store. This is
    the divergence dbi flagged (scratch/slot copy -> its own store), closed."""
    monkeypatch.delenv("CI_HUB_REBASE_STORE", raising=False)
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(tmp_path))
    got = M.default_store()
    assert got == str(tmp_path / "ignored" / "rebase-records.jsonl")
    # parent_root honours the env over __file__ (the copy-independence guarantee).
    assert M.parent_root() == str(tmp_path)


def test_default_store_falls_back_to_file_when_no_env(monkeypatch) -> None:
    monkeypatch.delenv("CI_HUB_REBASE_STORE", raising=False)
    monkeypatch.delenv("DEV_HERMIT_PARENT", raising=False)
    # No env -> three levels up from the module file (dev-hermit parent).
    assert M.default_store().endswith("/ignored/rebase-records.jsonl")


# --------------------------------------------------------------------------- #
# receipt_status: an authority FAILURE is UNKNOWN, never ABSENT                 #
# --------------------------------------------------------------------------- #
def test_receipt_status_validated(monkeypatch) -> None:
    report = {"verdict": "VALIDATED", "qualifying_count": 1,
              "newest_qualifying": {"profile": "full", "selection_mode": "full",
                                    "result": "pass", "finished_at": "t",
                                    "slot": "s", "host": "h"}}
    monkeypatch.setattr(M, "_run", lambda *a, **k: _cp(0, json.dumps(report)))
    rs = M.receipt_status(Z)
    assert rs["status"] == M.RECEIPT_VALIDATED and rs["identity"]["sha"] == Z


def test_receipt_status_absent_when_authority_answers_not_validated(monkeypatch) -> None:
    report = {"verdict": "NOT-VALIDATED", "qualifying_count": 0,
              "newest_qualifying": None}
    monkeypatch.setattr(M, "_run", lambda *a, **k: _cp(1, json.dumps(report)))
    rs = M.receipt_status(Z)
    assert rs["status"] == M.RECEIPT_ABSENT and rs["identity"] is None


def test_receipt_status_unknown_on_authority_failure(monkeypatch) -> None:
    """The load-bearing distinction: could-not-reach is UNKNOWN, never ABSENT."""
    # (a) tool cannot be invoked.
    def boom(*a, **k):
        raise OSError("validate-status not found")
    monkeypatch.setattr(M, "_run", boom)
    assert M.receipt_status(Z)["status"] == M.RECEIPT_UNKNOWN
    # (b) tool ran but emitted no parseable JSON.
    monkeypatch.setattr(M, "_run", lambda *a, **k: _cp(3, "traceback: kaboom"))
    assert M.receipt_status(Z)["status"] == M.RECEIPT_UNKNOWN


# --------------------------------------------------------------------------- #
# CLOSURE BAR: a planted eligible head SURVIVES a validate-status failure       #
# --------------------------------------------------------------------------- #
def _unknown_receipt(_z):
    return {"status": M.RECEIPT_UNKNOWN, "identity": None,
            "detail": "validate-status unreachable"}


def test_closure_bar_targeted_head_survives_validate_status_failure(
        tmp_path, monkeypatch, capsys) -> None:
    """Plant a would-be-eligible head (soft-green, base clears, receipt bound), then
    make the receipt authority FAIL on the live re-check. The head must NOT vanish:
    it stays VISIBLE as receipt-unknown and non-landable -- the invisible-failure
    class must not be rebuilt inside the fix for it."""
    store = str(tmp_path / "s.jsonl")
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, receipt=True))
    capsys.readouterr()  # drain the RECORDED line so the eligible JSON parses alone
    monkeypatch.setattr(M, "receipt_status", _unknown_receipt)
    q = M.build_parser().parse_args(
        ["eligible", "--result", Z, "--store", store, "--no-recheck-floor",
         "--json"])
    rc = M.do_eligible(q)
    out = json.loads(capsys.readouterr().out)
    assert rc == M.EXIT_REFUSED                    # not landed on an unknown ...
    assert out["result"] == Z                      # ... but the head is PRESENT ...
    assert out["eligible"] is False
    assert out["receipt_state"] == M.RECEIPT_UNKNOWN   # ... and VISIBLE as unknown
    assert "receipt-unknown" in out["reason"]


def test_closure_bar_reconciled_head_survives_as_receipt_unknown(
        tmp_path, monkeypatch, capsys) -> None:
    """List/reconcile mode: the head is an open PR; the receipt authority fails. It
    must land in the receipt-unknown bucket (VISIBLE), never be silently dropped
    from the population -- 'invisible != nothing-pending' includes 'unknown'."""
    store = str(tmp_path / "s.jsonl")
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, receipt=True))
    capsys.readouterr()  # drain the RECORDED line so the eligible JSON parses alone
    monkeypatch.setattr(M, "open_pushed_prs",
                        lambda repo: [{"number": 7, "headRefOid": Z,
                                       "headRefName": "feat", "url": "u"}])
    monkeypatch.setattr(M, "receipt_status", _unknown_receipt)
    q = M.build_parser().parse_args(
        ["eligible", "--store", store, "--no-recheck-floor", "--json"])
    assert M.do_eligible(q) == M.EXIT_OK
    out = json.loads(capsys.readouterr().out)
    assert out["summary"]["receipt-unknown"] == 1
    assert out["summary"]["eligible"] == 0
    assert out["receipt_unknown"][0]["result"] == Z      # present, not vanished
    assert out["reconciled"] is True


# --------------------------------------------------------------------------- #
# Fix B: reconcile against the LIVE open-PR population (invisible != nothing)   #
# --------------------------------------------------------------------------- #
def test_reconcile_unaccounted_open_pr_fires(tmp_path, monkeypatch, capsys) -> None:
    """An open pushed PR with NO record is UNACCOUNTED, not silently omitted."""
    store = str(tmp_path / "empty.jsonl")
    other = "e" * 40
    monkeypatch.setattr(M, "open_pushed_prs",
                        lambda repo: [{"number": 9, "headRefOid": other,
                                       "headRefName": "pushed-elsewhere",
                                       "url": "u"}])
    q = M.build_parser().parse_args(
        ["eligible", "--store", store, "--no-recheck-floor", "--json"])
    assert M.do_eligible(q) == M.EXIT_OK
    out = json.loads(capsys.readouterr().out)
    assert out["summary"]["unaccounted"] == 1
    assert out["summary"]["open_pushed_prs"] == 1
    assert out["unaccounted"][0]["result"] == other


def test_reconcile_recorded_landable_is_eligible_not_inert(
        tmp_path, monkeypatch, capsys) -> None:
    """The positive bracket: a recorded landable head that IS an open PR fires as
    ELIGIBLE -- the gate is live, not inert."""
    store = str(tmp_path / "s.jsonl")
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, receipt=True))
    capsys.readouterr()  # drain the RECORDED line so the eligible JSON parses alone
    monkeypatch.setattr(M, "open_pushed_prs",
                        lambda repo: [{"number": 3, "headRefOid": Z,
                                       "headRefName": "feat", "url": "u"}])
    monkeypatch.setattr(M, "receipt_status", lambda z: {
        "status": M.RECEIPT_VALIDATED, "identity": _STUB_RECEIPT, "detail": ""})
    q = M.build_parser().parse_args(
        ["eligible", "--store", store, "--no-recheck-floor", "--json"])
    assert M.do_eligible(q) == M.EXIT_OK
    out = json.loads(capsys.readouterr().out)
    assert out["summary"]["eligible"] == 1
    assert out["eligible"][0]["result"] == Z
    assert out["eligible"][0]["pr_number"] == 3


def test_reconcile_orphaned_head_is_recorded_not_open(
        tmp_path, monkeypatch, capsys) -> None:
    """100%-orphan-rate reality: a head we recorded whose PR moved (re-rebased /
    force-pushed away) is no open PR's head. It must be reported as
    recorded_not_open (superseded), NOT counted as eligible."""
    store = str(tmp_path / "s.jsonl")
    M.do_record(_args(
        ["record", "--source", X, "--base", Y, "--result", Z, "--conflicts",
         "none"], store, monkeypatch, receipt=True))
    capsys.readouterr()  # drain the RECORDED line so the eligible JSON parses alone
    # The live population has a DIFFERENT head (the orphaning re-rebase minted Z2).
    monkeypatch.setattr(M, "open_pushed_prs",
                        lambda repo: [{"number": 5, "headRefOid": Z2,
                                       "headRefName": "feat", "url": "u"}])
    q = M.build_parser().parse_args(
        ["eligible", "--store", store, "--no-recheck-floor", "--json"])
    assert M.do_eligible(q) == M.EXIT_OK
    out = json.loads(capsys.readouterr().out)
    assert Z in out["recorded_not_open"]           # our stale head, surfaced
    assert out["summary"]["eligible"] == 0         # never landed on a moved head
    assert out["summary"]["unaccounted"] == 1      # the new head Z2 is unaccounted


# --------------------------------------------------------------------------- #
# A2: durable, cross-host soft-green provenance                                 #
# --------------------------------------------------------------------------- #
def test_provenance_body_carries_only_nonderivable() -> None:
    rec = {"source_rev": X, "base": Y, "result": Z, "conflicts": [],
           "soft_green": M.SOFT_ZERO_CONFLICT, "risk_judgement": M.RISK_NA,
           "rationale": "", "resolver": "", "recorded_utc": "t",
           "base_clears_floor": True, "receipt_at_Z": {"sha": Z}}
    prov = json.loads(M.provenance_body(rec))
    assert prov["soft_green"] == M.SOFT_ZERO_CONFLICT and prov["result"] == Z
    # Live-authority fields must NOT be frozen into durable provenance.
    assert "base_clears_floor" not in prov and "receipt_at_Z" not in prov


def test_publish_provenance_refuses_null_soft_green() -> None:
    rec = {"result": Z, "soft_green": None}
    try:
        M.publish_provenance(rec)
    except M.RebaseError:
        return
    raise AssertionError("null soft-green carries no durable claim; must refuse")


def test_publish_provenance_content_addressed_immutable_path(monkeypatch) -> None:
    import hashlib
    captured = {}

    def fake_publish(repo, branch, path, body):
        captured.update(repo=repo, branch=branch, path=path, body=body)
        return "cafe" * 10
    monkeypatch.setattr(M.publish_receipt, "publish", fake_publish)
    rec = {"source_rev": X, "base": Y, "result": Z, "conflicts": [],
           "soft_green": M.SOFT_ZERO_CONFLICT, "risk_judgement": M.RISK_NA,
           "rationale": "", "resolver": "", "recorded_utc": "t"}
    info = M.publish_provenance(rec)
    digest = hashlib.sha256(M.provenance_body(rec)).hexdigest()
    assert captured["path"] == f"rebase-provenance/{Z}/{digest}.json"
    assert captured["branch"] == M.RECEIPT_BRANCH
    assert info["digest"] == digest


def test_fetch_provenance_picks_latest_by_time(monkeypatch) -> None:
    import base64
    old = {"result": Z, "soft_green": M.SOFT_RESOLVER_JUDGED,
           "risk_judgement": M.RISK_VALIDATE, "recorded_utc": "2026-08-04T00:00:00Z"}
    new = {"result": Z, "soft_green": M.SOFT_RESOLVER_JUDGED,
           "risk_judgement": M.RISK_RETAIN, "recorded_utc": "2026-08-04T02:00:00Z"}

    def fake_gh(argv, check=True):
        url = argv[-1]
        if url.endswith(f"rebase-provenance/{Z}?ref={M.RECEIPT_BRANCH}"):
            return _cp(0, json.dumps([
                {"type": "file", "path": f"rebase-provenance/{Z}/a.json"},
                {"type": "file", "path": f"rebase-provenance/{Z}/b.json"}]))
        blob = old if "a.json" in url else new
        return _cp(0, json.dumps(
            {"content": base64.b64encode(json.dumps(blob).encode()).decode()}))
    monkeypatch.setattr(M.publish_receipt, "gh", fake_gh)
    got = M.fetch_provenance(Z)
    assert got["risk_judgement"] == M.RISK_RETAIN   # the later upgrade wins


def test_fetch_provenance_missing_returns_none(monkeypatch) -> None:
    monkeypatch.setattr(M.publish_receipt, "gh", lambda argv, check=True: _cp(1, ""))
    assert M.fetch_provenance(Z) is None


def test_durable_provenance_recovers_unaccounted_cross_host(
        tmp_path, monkeypatch, capsys) -> None:
    """A2 cross-host close: this host's local store is EMPTY, but the head is an
    open PR whose soft-green provenance another host published. With
    --durable-provenance it is recovered and (base floor + receipt re-checked live)
    classified ELIGIBLE -- not left UNACCOUNTED forever."""
    store = str(tmp_path / "empty.jsonl")
    monkeypatch.setattr(M, "open_pushed_prs",
                        lambda repo: [{"number": 11, "headRefOid": Z,
                                       "headRefName": "feat", "url": "u"}])
    monkeypatch.setattr(M, "fetch_provenance", lambda z: {
        "source_rev": X, "base": Y, "result": Z, "conflicts": [],
        "soft_green": M.SOFT_ZERO_CONFLICT, "risk_judgement": M.RISK_NA,
        "rationale": "", "resolver": "", "recorded_utc": "t"})
    # Live gates still apply: floor clears, receipt validated.
    monkeypatch.setattr(M.gate_floors, "load_floors", lambda p: [])
    monkeypatch.setattr(M.gate_floors, "clears_all",
                        lambda floors, co, base: {"ok": True, "unmet": []})
    monkeypatch.setattr(M, "receipt_status", lambda z: {
        "status": M.RECEIPT_VALIDATED, "identity": _STUB_RECEIPT, "detail": ""})
    q = M.build_parser().parse_args(
        ["eligible", "--store", store, "--durable-provenance", "--json"])
    assert M.do_eligible(q) == M.EXIT_OK
    out = json.loads(capsys.readouterr().out)
    assert out["summary"]["unaccounted"] == 0       # recovered, not orphaned
    assert out["summary"]["eligible"] == 1
    assert out["eligible"][0]["result"] == Z


def test_durable_provenance_off_leaves_head_unaccounted(
        tmp_path, monkeypatch, capsys) -> None:
    """Without --durable-provenance the same head stays UNACCOUNTED (network stays
    OFF the default hot path); the cross-host recovery is strictly opt-in."""
    store = str(tmp_path / "empty.jsonl")
    monkeypatch.setattr(M, "open_pushed_prs",
                        lambda repo: [{"number": 11, "headRefOid": Z,
                                       "headRefName": "feat", "url": "u"}])
    # fetch_provenance must NOT be called; make it explode if it is.
    monkeypatch.setattr(M, "fetch_provenance",
                        lambda z: (_ for _ in ()).throw(AssertionError("called")))
    q = M.build_parser().parse_args(
        ["eligible", "--store", store, "--no-recheck-floor", "--json"])
    assert M.do_eligible(q) == M.EXIT_OK
    out = json.loads(capsys.readouterr().out)
    assert out["summary"]["unaccounted"] == 1


# --------------------------------------------------------------------------- #
# Landing authority: hard green OR independently recomputed soft green         #
# --------------------------------------------------------------------------- #
def _git_run(repo: Path, *args: str) -> str:
    cp = subprocess.run(["git", "-C", str(repo), *args], text=True,
                        capture_output=True, check=True)
    return cp.stdout.strip()


def _commit_file(repo: Path, path: str, body: str, message: str) -> str:
    (repo / path).write_text(body)
    _git_run(repo, "add", "--", path)
    _git_run(repo, "commit", "-q", "-m", message)
    return _git_run(repo, "rev-parse", "HEAD")


def _init_repo(repo: Path) -> None:
    repo.mkdir()
    _git_run(repo, "init", "-q", "-b", "main")
    _git_run(repo, "config", "user.name", "fixture")
    _git_run(repo, "config", "user.email", "fixture@example.invalid")


def _clean_rebase_fixture(tmp_path: Path) -> tuple[Path, str, str, str]:
    repo = tmp_path / "repo"
    _init_repo(repo)
    _commit_file(repo, "common", "base\n", "base")
    _git_run(repo, "checkout", "-q", "-b", "feature")
    source = _commit_file(repo, "feature", "feature\n", "feature")
    _git_run(repo, "checkout", "-q", "main")
    base = _commit_file(repo, "main-only", "main\n", "main")
    _git_run(repo, "checkout", "-q", "feature")
    _git_run(repo, "rebase", "-q", "main")
    result = _git_run(repo, "rev-parse", "HEAD")
    return repo, source, base, result


def _authorization_args(repo: Path, store: Path, base: str, result: str):
    return M.build_parser().parse_args([
        "authorize", "--result", result, "--current-base", base,
        "--repo-checkout", str(repo), "--registry", str(repo / "floors.json"),
        "--store", str(store), "--json",
    ])


def _plant_zero_conflict(store: Path, source: str, base: str, result: str) -> None:
    M.append_record(str(store), {
        "source_rev": source, "base": base, "result": result,
        "conflicts": [], "soft_green": M.SOFT_ZERO_CONFLICT,
        "risk_judgement": M.RISK_NA, "base_clears_floor": True,
        "base_unmet_floors": [], "receipt_at_Z": None, "landable": False,
    })


def _stub_soft_authorities(monkeypatch) -> None:
    monkeypatch.setattr(M, "exact_head_status", lambda args: _cp(4, "NO_RESULT"))
    monkeypatch.setattr(M.gate_floors, "load_floors", lambda path: [])
    monkeypatch.setattr(M.gate_floors, "clears_all",
                        lambda floors, checkout, base: {"ok": True, "unmet": []})
    monkeypatch.setattr(M, "receipt_status", lambda source: {
        "status": M.RECEIPT_VALIDATED,
        "identity": {"sha": source, "verdict": "VALIDATED"}, "detail": ""})


def test_authorize_hard_green_without_rebase_record(
        tmp_path, monkeypatch, capsys) -> None:
    monkeypatch.setattr(M, "exact_head_status", lambda args: _cp(0, "hard green"))
    args = _authorization_args(tmp_path, tmp_path / "absent.jsonl", Y, Z)
    assert M.do_authorize(args) == M.EXIT_OK
    assert json.loads(capsys.readouterr().out)["state"] == "HARD_GREEN"


def test_authorize_clean_rebase_soft_green_recomputes_x_on_y(
        tmp_path, monkeypatch, capsys) -> None:
    repo, source, base, result = _clean_rebase_fixture(tmp_path)
    store = tmp_path / "records.jsonl"
    _plant_zero_conflict(store, source, base, result)
    _stub_soft_authorities(monkeypatch)
    assert M.do_authorize(_authorization_args(repo, store, base, result)) == M.EXIT_OK
    out = json.loads(capsys.readouterr().out)
    assert out["state"] == "SOFT_GREEN"
    assert out["source_rev"] == source and out["base"] == base


def test_authorize_absent_record_refuses(tmp_path, monkeypatch) -> None:
    _stub_soft_authorities(monkeypatch)
    args = _authorization_args(tmp_path, tmp_path / "absent.jsonl", Y, Z)
    try:
        M.do_authorize(args)
    except M.Refused as err:
        assert "no rebase record" in str(err)
        return
    raise AssertionError("an absent rebase record must refuse soft-green")


def test_authorize_soft_green_requires_live_validated_source(
        tmp_path, monkeypatch) -> None:
    repo, source, base, result = _clean_rebase_fixture(tmp_path)
    store = tmp_path / "records.jsonl"
    _plant_zero_conflict(store, source, base, result)
    _stub_soft_authorities(monkeypatch)
    monkeypatch.setattr(M, "receipt_status", lambda _source: {
        "status": M.RECEIPT_ABSENT, "identity": None,
        "detail": "NOT-VALIDATED"})
    try:
        M.do_authorize(_authorization_args(repo, store, base, result))
    except M.Refused as err:
        assert "source" in str(err) and "not live VALIDATED" in str(err)
        return
    raise AssertionError("soft-green inherited from a source without authority")


def test_authorize_soft_green_requires_recorded_base_to_be_current(
        tmp_path, monkeypatch) -> None:
    repo, source, base, result = _clean_rebase_fixture(tmp_path)
    _git_run(repo, "checkout", "-q", "main")
    newer_base = _commit_file(repo, "later", "later\n", "later main")
    store = tmp_path / "records.jsonl"
    _plant_zero_conflict(store, source, base, result)
    _stub_soft_authorities(monkeypatch)
    try:
        M.do_authorize(_authorization_args(repo, store, newer_base, result))
    except M.Refused as err:
        assert "is not current main" in str(err)
        return
    raise AssertionError("soft-green authorized against a superseded base")


def test_authorize_known_red_never_falls_through_to_soft_green(
        tmp_path, monkeypatch) -> None:
    repo, source, base, result = _clean_rebase_fixture(tmp_path)
    store = tmp_path / "records.jsonl"
    _plant_zero_conflict(store, source, base, result)
    _stub_soft_authorities(monkeypatch)
    monkeypatch.setattr(M, "exact_head_status", lambda args: _cp(3, "known red"))
    try:
        M.do_authorize(_authorization_args(repo, store, base, result))
    except M.Refused as err:
        assert "known red" in str(err)
        return
    raise AssertionError("a known-red exact head inherited soft-green")


def test_authorize_forged_record_refuses_on_recomputed_tree(
        tmp_path, monkeypatch) -> None:
    """Mutation control: without recomputation this forged row authorizes."""
    repo, source, base, _honest_result = _clean_rebase_fixture(tmp_path)
    _git_run(repo, "checkout", "-q", "-b", "forged", base)
    forged_result = _commit_file(repo, "forged", "not the feature\n", "forged")
    store = tmp_path / "records.jsonl"
    _plant_zero_conflict(store, source, base, forged_result)
    _stub_soft_authorities(monkeypatch)
    try:
        M.do_authorize(_authorization_args(repo, store, base, forged_result))
    except M.Refused as err:
        assert "recomputation produced tree" in str(err)
        return
    raise AssertionError("a fabricated row authorized an unrelated result")


def test_authorize_real_rebase_conflict_refuses(tmp_path, monkeypatch) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)
    _commit_file(repo, "shared", "base\n", "base")
    _git_run(repo, "checkout", "-q", "-b", "feature")
    source = _commit_file(repo, "shared", "feature\n", "feature")
    _git_run(repo, "checkout", "-q", "main")
    base = _commit_file(repo, "shared", "main\n", "main")
    _git_run(repo, "checkout", "-q", "-b", "claimed-result", base)
    result = _commit_file(repo, "shared", "manual resolution\n", "resolved")
    store = tmp_path / "records.jsonl"
    _plant_zero_conflict(store, source, base, result)
    _stub_soft_authorities(monkeypatch)
    try:
        M.do_authorize(_authorization_args(repo, store, base, result))
    except M.Refused as err:
        assert "recomputation conflicted" in str(err)
        return
    raise AssertionError("a genuinely conflicted rebase must refuse inheritance")


if __name__ == "__main__":
    import pytest
    raise SystemExit(pytest.main([__file__, "-v"]))
