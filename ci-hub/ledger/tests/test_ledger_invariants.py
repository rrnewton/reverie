#!/usr/bin/env python3
"""Invariant tests for the per-team/machine validation ledger.

WRITTEN BEFORE THE IMPLEMENTATION, ON PURPOSE. Schema:
`ai_docs/validate-ledger-team-machine-schema_20260807.md`. Section references
below (§2, §5, §7 …) point at it.

HOW THIS FILE IS MEANT TO FAIL
------------------------------
A TDD file whose every test dies on the same `ImportError` is indistinguishable
from a file with a typo in it, so this file is deliberately split in two:

* **Fixture/contract tests run and PASS today.** They assert things about the
  fixtures themselves — that the corpus is well formed, that it contains no
  owner path, that the legacy row arithmetic closes. If these fail, the test
  file is broken, not the implementation.
* **Implementation tests FAIL today**, each with a message naming the single
  invariant it exists to prove, via `needs_impl()`. They stop being red the
  moment `ci-hub/ledger/ledger.py` exists and behaves.

That split is the anti-vacuity property: a green fixture half plus a red
implementation half tells you the harness is sound and the feature is absent,
which a uniform wall of red cannot.

THE CONTRACT THE IMPLEMENTATION MUST PROVIDE (module `ledger`)
--------------------------------------------------------------
    shard_path(team, host, when)        -> "ledger/<team>/<host>/<YYYY>-<MM>.jsonl"
    parse_line(text)                    -> dict            (raises MalformedEvent)
    validate_event(event, shard_path)   -> list[str]       (violation codes, §7)
    union(paths)                        -> list[dict]      (deterministic, §5)
    fold(events)                        -> dict[run_id, dict]
    green(runs, host_pred=None)         -> commit | None   (§10)
    bisect_verdict(runs, commit)        -> "pass"|"fail"|"unknown"|"nodata"
    verify_append_only(old_text, new_text) -> list[str]

Run: python3 -m pytest ci-hub/ledger/tests/test_ledger_invariants.py -q
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

try:  # the implementation does not exist yet; that is the point
    import ledger as impl  # type: ignore
    IMPL_MISSING: str | None = None
except Exception as exc:  # noqa: BLE001 - any import failure means "absent"
    impl = None  # type: ignore
    IMPL_MISSING = f"{type(exc).__name__}: {exc}"


def needs_impl(invariant: str) -> None:
    """Fail naming the invariant, never a bare ImportError."""
    if impl is None:
        pytest.fail(
            f"IMPLEMENTATION ABSENT (ci-hub/ledger/ledger.py) — this test exists to prove:\n"
            f"    {invariant}\n"
            f"  import result: {IMPL_MISSING}"
        )


# --------------------------------------------------------------------------- fixtures
#
# Every fixture is privacy-clean BY CONSTRUCTION: no FQDN, no /home/<user>/.
# test_fixtures_contain_no_owner_path enforces that against this whole module,
# because a fixture that leaks an owner path would be committed into a tracked
# file and is exactly what §7 lint 3 refuses.

TEAM_A, TEAM_B = "hermit", "reverie-infra"
HOST_1, HOST_2 = "hosta", "hostb"
SHA_1 = "a" * 40
SHA_2 = "b" * 40
SHA_3 = "c" * 40


def ev(event_id, event_type="run.result", *, run_id=None, emitted_at="2026-08-07T02:00:00Z",
       team=TEAM_A, host=HOST_1, source="observed", **payload) -> dict:
    e = {
        "schema": "validate-ledger/v1",
        "event_id": event_id,
        "event_type": event_type,
        "emitted_at": emitted_at,
        "team": team,
        "host": host,
        "run_id": run_id or event_id,
        "producer": {"source": source, "tool": "ci-hub/validate-run", "tool_version": "1"},
    }
    e.update(payload)
    return e


def result(event_id, commit, outcome, **kw) -> dict:
    return ev(event_id, "run.result", commit=commit, outcome=outcome, **kw)


def write_shard(root: Path, team: str, host: str, month: str, events: list[dict]) -> Path:
    p = root / "ledger" / team / host / f"{month}.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("".join(json.dumps(e, sort_keys=True) + "\n" for e in events))
    return p


# A legacy-shaped corpus mirroring the MEASURED structure of the real ledger:
# 571 singleton runs + 36 two-row groups + one eleven-row chain = 654 rows / 608 runs.
# Scaled down but structurally identical, so the arithmetic test is real.
LEGACY_SINGLETONS = 5
LEGACY_PAIRS = 3
LEGACY_CHAIN_LEN = 11
LEGACY_ROWS = LEGACY_SINGLETONS + LEGACY_PAIRS * 2 + LEGACY_CHAIN_LEN
LEGACY_RUNS = LEGACY_SINGLETONS + LEGACY_PAIRS + 1


def legacy_rows() -> list[dict]:
    """Pre-migration rows in the OLD flat shape (no event_id, no event_type)."""
    rows = []
    for i in range(LEGACY_SINGLETONS):
        rows.append({"schema_version": 3, "host": HOST_1, "commit": SHA_1,
                     "started_at": f"2026-08-03T10:0{i}:00Z",
                     "finished_at": f"2026-08-03T10:0{i}:30Z", "result": "pass",
                     "cwd": "worktrees/kvm/hermit", "executed_tests": None})
    for i in range(LEGACY_PAIRS):  # base + one enrichment, the measured 36-pair shape
        base = {"schema_version": 3, "host": HOST_1, "commit": SHA_2,
                "started_at": f"2026-08-03T11:0{i}:00Z",
                "finished_at": f"2026-08-03T11:0{i}:30Z", "result": "pass",
                "cwd": "worktrees/kvm/hermit", "executed_tests": None, "coverage": None}
        rich = dict(base, schema_version=5, executed_tests=740,
                    coverage={"planned_test_nodes": 19, "executed_test_nodes": 19})
        rows += [base, rich]
    for i in range(LEGACY_CHAIN_LEN):  # the measured 11-row chain: depth is NOT 1
        # executed_tests goes null -> 740 ONCE and then holds. A value that kept
        # changing would be a CORRECTION, and the real data has zero of those
        # (0 of 37 groups change a set value), so the fixture must not invent any.
        rows.append({"schema_version": 3 + i // 6, "host": HOST_1, "commit": SHA_3,
                     "started_at": "2026-08-03T12:00:00Z",
                     "finished_at": "2026-08-03T12:05:00Z", "result": "fail",
                     "cwd": "worktrees/kvm/hermit",
                     "executed_tests": None if i < 6 else 740})
    return rows


# =========================================================================== #
# PART 1 — FIXTURE/CONTRACT TESTS. These RUN and PASS today.                  #
# =========================================================================== #

def test_fixtures_contain_no_owner_path_or_fqdn():
    """§7 lints 1 and 3, applied to this file's own fixtures.

    A fixture that leaked /home/<user>/ would be committed into a tracked file —
    the same defect class as portable_path_fix_ci. The test file must satisfy
    the rule it enforces.
    """
    # The needle is BUILT, not written literally: a literal would match this
    # very assertion and the check would fail on its own source. (It did.)
    needle = "/" + "home" + "/"
    text = Path(__file__).read_text().replace(needle + "<user>/", "")
    assert needle not in text, "fixture leaks an owner path"
    for row in legacy_rows():
        for value in row.values():
            assert needle not in str(value), f"legacy fixture row leaks an owner path: {value!r}"
    for host in (HOST_1, HOST_2):
        assert "." not in host, f"fixture host {host!r} is not a short name"
    for row in legacy_rows():
        assert not str(row["cwd"]).startswith("/"), "legacy fixture cwd must be repo-relative"


def test_legacy_fixture_reproduces_the_measured_group_arithmetic():
    """The corpus must have the shape the real ledger was measured to have.

    Measured: 571x1 + 36x2 + 1x11 = 654 rows over 608 runs. This fixture is the
    same shape scaled down, so a migration that collapses the 11-chain, or that
    treats a pair as a duplicate, fails here rather than in production.
    """
    rows = legacy_rows()
    assert len(rows) == LEGACY_ROWS == 5 + 6 + 11 == 22
    groups: dict[tuple, list[dict]] = {}
    for r in rows:
        groups.setdefault((r["host"], r["started_at"], r["finished_at"], r["commit"]), []).append(r)
    assert len(groups) == LEGACY_RUNS == 9
    sizes = sorted(len(v) for v in groups.values())
    assert sizes == [1] * 5 + [2] * 3 + [11]
    assert sum(sizes) == LEGACY_ROWS


def test_legacy_multi_row_groups_are_enrichments_not_corrections():
    """Every later row only ADDS; none changes an already-set value.

    Measured exhaustively on the real 654 rows: 0 of 37 groups change a set
    value. If that ever stops holding, the migration needs run.correct and this
    test is the thing that says so.
    """
    rows = legacy_rows()
    groups: dict[tuple, list[dict]] = {}
    for r in rows:
        groups.setdefault((r["host"], r["started_at"], r["finished_at"], r["commit"]), []).append(r)
    for key, members in groups.items():
        ordered = sorted(members, key=lambda r: r["schema_version"])
        for prev, cur in zip(ordered, ordered[1:]):
            for field, val in cur.items():
                if field == "schema_version":
                    continue
                if prev.get(field) is not None and prev.get(field) != val:
                    pytest.fail(f"group {key} field {field!r} CHANGES a set value: "
                                f"{prev.get(field)!r} -> {val!r}; that is a correction")


def test_event_fixture_builder_emits_a_well_formed_envelope():
    """§2: the envelope keys every event must carry."""
    e = result("01A", SHA_1, "pass")
    for k in ("schema", "event_id", "event_type", "emitted_at", "team", "host",
              "run_id", "producer"):
        assert k in e, f"envelope missing {k}"
    assert e["run_id"] == e["event_id"], "a run.result mints its own run_id"
    assert e["producer"]["source"] in ("observed", "reconstructed", "imported")


@pytest.mark.parametrize(
    "field",
    (
        "schema",
        "event_id",
        "event_type",
        "emitted_at",
        "team",
        "host",
        "run_id",
        "producer",
    ),
)
def test_shared_event_type_refuses_each_missing_envelope_field_by_name(field):
    needs_impl("every envelope field is required by the shared type (§2)")
    event = result("01A", SHA_1, "pass")
    del event[field]
    codes = impl.validate_event(event)
    assert any(field in code for code in codes), codes


def test_shared_event_type_refuses_unknown_event_type_by_name():
    needs_impl("event_type is the closed five-value vocabulary in §3")
    event = result("01A", SHA_1, "pass")
    event["event_type"] = "run.future"
    codes = impl.validate_event(event)
    assert any("run.future" in code and "event_type" in code for code in codes), codes


def test_shared_event_type_refuses_missing_result_outcome_by_name():
    needs_impl("run.result requires the normalized outcome in §4.1")
    event = result("01A", SHA_1, "pass")
    del event["outcome"]
    codes = impl.validate_event(event)
    assert any("run.result omitted outcome" in code for code in codes), codes


def copy_shared_checker_sources(destination: Path) -> None:
    repository = Path(__file__).resolve().parents[3]
    for relative in impl._SHARED_TYPE_SOURCE_RELATIVES:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes((repository / relative).read_bytes())
        target.chmod(
            0o755 if relative == impl._SHARED_TYPE_CHECKER_RELATIVE else 0o644
        )


def test_shared_checker_cache_key_is_source_content_not_checkout_path(tmp_path):
    needs_impl("the Rust checker cache key is independent of a private tool-root path")
    first_source = tmp_path / "run-a"
    second_source = tmp_path / "run-b"
    cache = tmp_path / "cache"
    copy_shared_checker_sources(first_source)
    copy_shared_checker_sources(second_source)

    first = impl._materialized_shared_type_checker(
        source_root=first_source, cache_root=cache
    )
    second = impl._materialized_shared_type_checker(
        source_root=second_source, cache_root=cache
    )

    assert first == second
    target = first.parents[2]
    for relative in impl._SHARED_TYPE_SOURCE_RELATIVES:
        cached = target / relative
        assert cached.read_bytes() == (first_source / relative).read_bytes()
        assert stat.S_IMODE(cached.stat().st_mode) == (
            0o555 if relative == impl._SHARED_TYPE_CHECKER_RELATIVE else 0o444
        )
        assert cached.stat().st_nlink == 1
    assert first.read_bytes().startswith(
        b"#!/usr/bin/env -S rust-script --force\n"
    )
    assert stat.S_IMODE(first.stat().st_mode) & 0o111


def test_shared_checker_cache_tamper_is_refused_not_repaired(tmp_path):
    needs_impl("tampered cached Rust checker bytes fail closed")
    source = tmp_path / "run"
    cache = tmp_path / "cache"
    copy_shared_checker_sources(source)
    checker = impl._materialized_shared_type_checker(
        source_root=source, cache_root=cache
    )
    tampered = checker.read_bytes() + b"\n// tampered\n"
    checker.chmod(0o755)
    checker.write_bytes(tampered)

    with pytest.raises(impl.SharedTypeCheckerUnavailable, match="cached checker source"):
        impl._materialized_shared_type_checker(source_root=source, cache_root=cache)
    assert checker.read_bytes() == tampered


def test_shared_checker_source_must_retain_tracked_executable_semantics(tmp_path):
    needs_impl("a nonexecutable source checker cannot be promoted by the cache")
    source = tmp_path / "run"
    copy_shared_checker_sources(source)
    (source / impl._SHARED_TYPE_CHECKER_RELATIVE).chmod(0o644)

    with pytest.raises(impl.SharedTypeCheckerUnavailable, match="source mode"):
        impl._materialized_shared_type_checker(
            source_root=source, cache_root=tmp_path / "cache"
        )


@pytest.mark.parametrize(
    "mutation,match",
    [
        ("hardlink", "link count"),
        ("extra-entry", "unexpected entries"),
    ],
)
def test_shared_checker_cache_shape_tamper_is_refused(
    tmp_path, mutation, match
):
    needs_impl("cached checker mode, links, and tree shape remain exact")
    source = tmp_path / "run"
    cache = tmp_path / "cache"
    copy_shared_checker_sources(source)
    checker = impl._materialized_shared_type_checker(
        source_root=source, cache_root=cache
    )
    if mutation == "hardlink":
        os.link(checker, tmp_path / "checker-hardlink")
    else:
        checker.parent.chmod(0o755)
        (checker.parent / "unexpected").write_text("tampered\n")
        checker.parent.chmod(0o555)

    with pytest.raises(impl.SharedTypeCheckerUnavailable, match=match):
        impl._materialized_shared_type_checker(source_root=source, cache_root=cache)


def test_shared_checker_dependency_change_mints_a_new_cache_key(tmp_path):
    needs_impl("a changed producer-owned Rust dependency cannot reuse a stale binary")
    first_source = tmp_path / "run-a"
    changed_source = tmp_path / "run-b"
    cache = tmp_path / "cache"
    copy_shared_checker_sources(first_source)
    copy_shared_checker_sources(changed_source)
    first = impl._materialized_shared_type_checker(
        source_root=first_source, cache_root=cache
    )
    dependency_relative = Path("ci-hub/lib/ledger_event.rs")
    dependency = changed_source / dependency_relative
    changed_bytes = dependency.read_bytes() + b"\n// source-identity fixture\n"
    dependency.write_bytes(changed_bytes)

    changed = impl._materialized_shared_type_checker(
        source_root=changed_source, cache_root=cache
    )

    assert changed != first
    assert (changed.parents[2] / dependency_relative).read_bytes() == changed_bytes
    assert (first.parents[2] / dependency_relative).read_bytes() != changed_bytes

    third_source = tmp_path / "run-c"
    copy_shared_checker_sources(third_source)
    third_dependency = third_source / dependency_relative
    third_bytes = third_dependency.read_bytes() + b"\n// second source identity\n"
    third_dependency.write_bytes(third_bytes)
    third = impl._materialized_shared_type_checker(
        source_root=third_source, cache_root=cache
    )
    assert third not in (first, changed)
    assert not first.parents[2].exists()
    assert changed.parents[2].is_dir()
    assert third.parents[2].is_dir()


def test_shared_checker_post_execution_tamper_discards_success(tmp_path, monkeypatch):
    needs_impl("a checker changed during execution cannot return a trusted success")
    cache = tmp_path / "cache"
    monkeypatch.setenv("CI_HUB_LEDGER_EVENT_CHECKER_CACHE", str(cache))
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER", raising=False)
    monkeypatch.delenv("DEV_HERMIT_TOOL_AUTHORITY", raising=False)

    def mutate_then_succeed(argv, **_kwargs):
        checker = Path(argv[1])
        checker.chmod(0o755)
        checker.write_bytes(checker.read_bytes() + b"\n// post-check tamper\n")
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(impl.subprocess, "run", mutate_then_succeed)
    codes = impl._shared_type_codes([result("01A", SHA_1, "pass")])
    assert len(codes) == 1 and len(codes[0]) == 1
    assert codes[0][0].startswith("shared-ledger-event-check-unavailable:")
    assert "cached checker source" in codes[0][0]


def test_shared_checker_runs_its_content_addressed_alias_without_force(
    tmp_path, monkeypatch
):
    needs_impl("a built checker is reused without a cargo run, and only by its bytes")
    # Through the --force shebang, every check ran cargo, which waits for the
    # host-wide Cargo locks; see _CONTENT_ADDRESSED_CHECKER_RUNNER. Without
    # --force, rust-script reuses the binary it built for this exact path, so the
    # path must be the content address of both sources.
    cache = tmp_path / "cache"
    monkeypatch.setenv("CI_HUB_LEDGER_EVENT_CHECKER_CACHE", str(cache))
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER", raising=False)
    monkeypatch.delenv("DEV_HERMIT_TOOL_AUTHORITY", raising=False)
    calls: list[list[str]] = []

    def record(argv, **_kwargs):
        calls.append(list(argv))
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(impl.subprocess, "run", record)
    assert impl._shared_type_codes([result("01A", SHA_1, "pass")]) == [[]]
    impl.prepare_shared_checker()

    repository = Path(__file__).resolve().parents[3]
    alias = impl._materialized_shared_type_checker(cache_root=cache)
    assert calls == [
        ["rust-script", str(alias)],
        ["rust-script", str(alias), "--help"],
    ]
    assert alias.parents[2].name == impl._stable_checker_digest(
        impl._stable_checker_source_bytes(repository)
    )
    assert alias.relative_to(alias.parents[2]) == impl._SHARED_TYPE_CHECKER_RELATIVE
    # A direct run of a checkout's checker still rebuilds a changed dependency.
    assert alias.read_bytes().startswith(b"#!/usr/bin/env -S rust-script --force\n")


@pytest.mark.parametrize(
    "name,value,message",
    [
        (
            "CI_HUB_LEDGER_EVENT_CHECKER",
            "/bin/true",
            "fixture checker override is forbidden in immutable tool runs",
        ),
        (
            "CI_HUB_LEDGER_EVENT_CHECKER_CACHE",
            "/fixture/cache",
            "fixture checker cache override is forbidden in immutable tool runs",
        ),
    ],
)
def test_immutable_tool_run_refuses_fixture_checker_override(
    monkeypatch, name, value, message
):
    needs_impl("immutable health cannot replace or redirect its Rust checker")
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER", raising=False)
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER_CACHE", raising=False)
    monkeypatch.setenv(name, value)
    monkeypatch.setenv("DEV_HERMIT_TOOL_AUTHORITY", "/fixture/authority")
    called = False

    def must_not_run(*_args, **_kwargs):
        nonlocal called
        called = True
        raise AssertionError("fixture override ran inside immutable tooling")

    monkeypatch.setattr(impl.subprocess, "run", must_not_run)
    codes = impl._shared_type_codes([result("01A", SHA_1, "pass")])
    assert not called
    assert codes == [[f"shared-ledger-event-check-unavailable: {message}"]]


def test_shared_checker_cache_path_symlink_is_refused(tmp_path):
    needs_impl("the checker cache never follows a substituted path component")
    source = tmp_path / "run"
    copy_shared_checker_sources(source)
    real_cache = tmp_path / "real-cache"
    real_cache.mkdir()
    cache_link = tmp_path / "cache-link"
    cache_link.symlink_to(real_cache, target_is_directory=True)

    with pytest.raises(impl.SharedTypeCheckerUnavailable, match="not a real directory"):
        impl._materialized_shared_type_checker(
            source_root=source, cache_root=cache_link
        )


def test_shared_checker_missing_source_and_materializer_failure_refuse(
    tmp_path, monkeypatch
):
    needs_impl("an incomplete checker source tree is unavailable, never skipped")
    source = tmp_path / "run"
    cache = tmp_path / "cache"
    checker_relative = impl._SHARED_TYPE_CHECKER_RELATIVE
    checker = source / checker_relative
    checker.parent.mkdir(parents=True)
    checker.write_bytes(
        (Path(__file__).resolve().parents[3] / checker_relative).read_bytes()
    )
    checker.chmod(0o755)
    with pytest.raises(impl.SharedTypeCheckerUnavailable, match="cannot read checker source"):
        impl._materialized_shared_type_checker(source_root=source, cache_root=cache)

    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER", raising=False)

    def unavailable():
        raise impl.SharedTypeCheckerUnavailable("fixture cache refusal")

    monkeypatch.setattr(
        impl, "_locked_materialized_shared_type_checker", unavailable
    )
    codes = impl._shared_type_codes([result("01A", SHA_1, "pass")])
    assert codes == [[
        "shared-ledger-event-check-unavailable: fixture cache refusal"
    ]]


def test_shard_fixtures_for_two_teams_and_two_machines_are_disjoint_paths(tmp_path):
    """§1: four shards, no shared file — the whole point of the layout."""
    paths = {
        write_shard(tmp_path, TEAM_A, HOST_1, "2026-08", [result("01A", SHA_1, "pass")]),
        write_shard(tmp_path, TEAM_A, HOST_2, "2026-08", [result("01B", SHA_1, "pass", host=HOST_2)]),
        write_shard(tmp_path, TEAM_B, HOST_1, "2026-08", [result("01C", SHA_1, "fail", team=TEAM_B)]),
        write_shard(tmp_path, TEAM_B, HOST_2, "2026-08", [result("01D", SHA_1, "pass", team=TEAM_B, host=HOST_2)]),
    }
    assert len(paths) == 4, "two teams x two machines must be four distinct files"


# =========================================================================== #
# PART 2 — IMPLEMENTATION TESTS. These FAIL today, each naming its invariant.  #
# =========================================================================== #

# ---- append-only -----------------------------------------------------------

def test_append_to_current_month_shard_is_allowed():
    needs_impl("appending a new line to the current month's shard is legal (§1)")
    old = json.dumps(result("01A", SHA_1, "pass")) + "\n"
    new = old + json.dumps(result("01B", SHA_1, "pass")) + "\n"
    assert impl.verify_append_only(old, new) == []


def test_rewriting_an_existing_line_is_detected():
    needs_impl("MUTATION: editing a line already committed is refused (§3, §7 lint 9)")
    old = json.dumps(result("01A", SHA_1, "pass")) + "\n"
    new = json.dumps(result("01A", SHA_1, "fail")) + "\n"   # outcome flipped in place
    assert "rewritten-line" in impl.verify_append_only(old, new)


def test_deleting_a_line_is_detected():
    needs_impl("MUTATION: removing a committed line is refused (§7 lint 9)")
    a, b = json.dumps(result("01A", SHA_1, "pass")), json.dumps(result("01B", SHA_1, "pass"))
    assert "deleted-line" in impl.verify_append_only(f"{a}\n{b}\n", f"{a}\n")


def test_appending_to_a_frozen_past_month_shard_is_rejected():
    needs_impl("a line whose emitted_at month != the shard's month is refused (§7 lint 8)")
    e = result("01A", SHA_1, "pass", emitted_at="2026-09-01T00:00:00Z")
    assert "frozen-shard-append" in impl.validate_event(e, f"ledger/hermit/{HOST_1}/2026-08.jsonl")


# ---- same-shard serialization & disjoint-machine merge ---------------------

def test_two_concurrent_producers_on_one_shard_serialize_without_tearing(tmp_path):
    needs_impl("two writers on the SAME shard produce whole lines only — no interleaving (§1)")
    path = write_shard(tmp_path, TEAM_A, HOST_1, "2026-08", [])
    impl.append_events(path, [result(f"01P{i}", SHA_1, "pass") for i in range(50)])
    impl.append_events(path, [result(f"01Q{i}", SHA_1, "pass") for i in range(50)])
    lines = path.read_text().splitlines()
    assert len(lines) == 100
    for line in lines:
        json.loads(line)  # every line must be complete JSON, none torn


def test_disjoint_machines_write_different_paths_and_union_keeps_both(tmp_path):
    needs_impl("two hosts never contend, and the union contains both (§1, §5)")
    p1 = write_shard(tmp_path, TEAM_A, HOST_1, "2026-08", [result("01A", SHA_1, "pass")])
    p2 = write_shard(tmp_path, TEAM_A, HOST_2, "2026-08", [result("01B", SHA_1, "pass", host=HOST_2)])
    assert p1 != p2
    assert {e["event_id"] for e in impl.union([p1, p2])} == {"01A", "01B"}


# ---- deterministic union ---------------------------------------------------

def test_union_is_independent_of_file_and_line_order(tmp_path):
    needs_impl("union output is byte-identical regardless of input order (§5)")
    evs = [result("01C", SHA_1, "pass", emitted_at="2026-08-07T03:00:00Z"),
           result("01A", SHA_1, "pass", emitted_at="2026-08-07T01:00:00Z"),
           result("01B", SHA_1, "pass", emitted_at="2026-08-07T02:00:00Z")]
    p1 = write_shard(tmp_path, TEAM_A, HOST_1, "2026-08", evs)
    p2 = write_shard(tmp_path / "replica", TEAM_A, HOST_1, "2026-08", list(reversed(evs)))
    assert impl.union([p1, p2]) == impl.union([p2, p1])


def test_union_orders_by_emitted_at_then_event_id(tmp_path):
    needs_impl("the total order is (emitted_at, event_id) — ULID breaks ties (§5.3)")
    same = "2026-08-07T02:00:00Z"
    p = write_shard(tmp_path, TEAM_A, HOST_1, "2026-08", [
        result("01Z", SHA_1, "pass", emitted_at=same),
        result("01A", SHA_1, "pass", emitted_at=same),
        result("01M", SHA_1, "pass", emitted_at="2026-08-07T01:00:00Z")])
    assert [e["event_id"] for e in impl.union([p])] == ["01M", "01A", "01Z"]


# ---- duplicate ids ---------------------------------------------------------

def test_identical_duplicate_event_id_dedupes_to_one(tmp_path):
    needs_impl("same event_id with an IDENTICAL body collapses to one event (§5.2)")
    e = result("01A", SHA_1, "pass")
    p1 = write_shard(tmp_path, TEAM_A, HOST_1, "2026-08", [e])
    p2 = write_shard(tmp_path / "replica", TEAM_A, HOST_1, "2026-08", [e])
    assert len(impl.union([p1, p2])) == 1


def test_conflicting_duplicate_event_id_is_a_hard_error(tmp_path):
    needs_impl("same event_id with a DIFFERENT body raises — never last-writer-wins (§5.2)")
    p1 = write_shard(tmp_path, TEAM_A, HOST_1, "2026-08", [result("01A", SHA_1, "pass")])
    p2 = write_shard(
        tmp_path / "replica", TEAM_A, HOST_1, "2026-08",
        [result("01A", SHA_1, "fail")],
    )
    with pytest.raises(impl.ConflictingEventId):
        impl.union([p1, p2])


def test_repeated_run_id_across_events_is_legal():
    needs_impl("run_id repeats by design — it is the join key, not a uniqueness key (§2)")
    evs = [result("01A", SHA_1, "pass"),
           ev("01B", "run.enrich", run_id="01A", enriches="01A", executed_tests=740)]
    assert set(impl.fold(evs)) == {"01A"}


# ---- correction / enrichment by reference ----------------------------------

def test_enrich_adds_a_key_that_was_absent():
    needs_impl("run.enrich supplies a field the result left null (§3)")
    folded = impl.fold([result("01A", SHA_1, "pass", executed_tests=None),
                        ev("01B", "run.enrich", run_id="01A", enriches="01A", executed_tests=740)])
    assert folded["01A"]["executed_tests"] == 740


def test_enrich_that_changes_a_set_value_is_rejected():
    needs_impl("run.enrich may only ADD; changing a set value is a correction (§3)")
    evs = [result("01A", SHA_1, "pass", executed_tests=100),
           ev("01B", "run.enrich", run_id="01A", enriches="01A", executed_tests=740)]
    assert "enrich-overwrites-set-value" in impl.validate_event(evs[1], None, prior=evs[0])


def test_correct_supersedes_and_preserves_the_original():
    needs_impl("run.correct changes a set value, records supersedes, keeps the original (§3)")
    original = result("01A", SHA_1, "fail")
    corr = ev("01C", "run.correct", run_id="01A", supersedes="01A",
              outcome="no_result", reason="runner lost", emitted_at="2026-08-07T04:00:00Z")
    folded = impl.fold([original, corr])
    assert folded["01A"]["outcome"] == "no_result"
    assert original in impl.union_events([original, corr]), "the original event must survive"


def test_dangling_enrich_or_correct_reference_is_rejected():
    needs_impl("an enrich/correct naming an unknown event_id is refused (§7 lint 5)")
    orphan = ev("01B", "run.enrich", run_id="01A", enriches="does-not-exist", executed_tests=1)
    assert "unresolvable-reference" in impl.validate_event(orphan, None, known_ids=set())


def test_eleven_deep_enrichment_chain_folds_correctly():
    """The real ledger contains an 11-row chain; depth 1 must not be assumed."""
    needs_impl("an 11-deep enrichment chain folds to one run with the last added value (§9)")
    evs = [result("01A", SHA_3, "fail", executed_tests=None)]
    for i in range(1, LEGACY_CHAIN_LEN):
        evs.append(ev(f"01A{i}", "run.enrich", run_id="01A", enriches=evs[-1]["event_id"],
                      emitted_at=f"2026-08-07T02:{i:02d}:00Z", executed_tests=100 + i))
    folded = impl.fold(evs)
    assert len(folded) == 1
    assert folded["01A"]["executed_tests"] == 100 + LEGACY_CHAIN_LEN - 1


# ---- malformed / truncated -------------------------------------------------

def test_malformed_json_line_is_reported_not_silently_skipped(tmp_path):
    needs_impl("a malformed line raises/reports — silent skip would hide data loss (§7 lint 4)")
    p = tmp_path / "ledger" / TEAM_A / HOST_1 / "2026-08.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(result("01A", SHA_1, "pass")) + "\n{ this is not json\n")
    with pytest.raises(impl.MalformedEvent):
        impl.union([p])


def test_truncated_final_line_without_newline_is_detected(tmp_path):
    needs_impl("a final line lacking its newline is flagged as truncated, not parsed as whole")
    p = tmp_path / "ledger" / TEAM_A / HOST_1 / "2026-08.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(result("01A", SHA_1, "pass"))
    p.write_text(body + "\n" + body[:-8])  # last line cut mid-object
    with pytest.raises(impl.MalformedEvent):
        impl.union([p])


# ---- schema versions -------------------------------------------------------

def test_unknown_optional_key_is_preserved_not_dropped():
    needs_impl("readers ignore-but-PRESERVE unknown keys; dropping them loses data (§8)")
    e = result("01A", SHA_1, "pass", some_future_field={"k": 1})
    assert impl.fold([e])["01A"]["some_future_field"] == {"k": 1}


def test_unknown_envelope_version_is_rejected():
    needs_impl("an unknown envelope version is refused rather than guessed at (§8)")
    e = dict(result("01A", SHA_1, "pass"), schema="validate-ledger/v99")
    assert "unknown-envelope-version" in impl.validate_event(e, None)


# ---- privacy ---------------------------------------------------------------

@pytest.mark.parametrize("bad_host", ["hosta.example.internal", "hosta.corp", "a.b"])
def test_fqdn_host_is_rejected(bad_host):
    needs_impl(f"a dotted host ({bad_host!r}) is refused — short names only (§7 lint 1)")
    assert "fqdn-host" in impl.validate_event(result("01A", SHA_1, "pass", host=bad_host), None)


def test_host_must_match_the_shard_path():
    needs_impl("host field disagreeing with the shard path is refused (§7 lint 2)")
    e = result("01A", SHA_1, "pass", host=HOST_2)
    assert "host-path-mismatch" in impl.validate_event(e, f"ledger/{TEAM_A}/{HOST_1}/2026-08.jsonl")


def test_team_must_match_the_shard_path():
    needs_impl("team field disagreeing with the shard path is refused (§7 lint 2)")
    e = result("01A", SHA_1, "pass", team=TEAM_B)
    assert "team-path-mismatch" in impl.validate_event(e, f"ledger/{TEAM_A}/{HOST_1}/2026-08.jsonl")


def test_owner_home_path_in_any_string_field_is_rejected():
    needs_impl("an owner home path anywhere in the event is refused (§7 lint 3, §4.2)")
    e = result("01A", SHA_1, "pass", workspace={"repo_relative": "/home/<user>/work/dev-hermit"})
    assert "owner-path" in impl.validate_event(e, None)


def test_raw_cwd_key_is_rejected_outright():
    needs_impl("the legacy raw `cwd` key is refused; §4.2 requires typed workspace{}")
    assert "raw-cwd-forbidden" in impl.validate_event(result("01A", SHA_1, "pass", cwd="x"), None)


# ---- queries: green / timeline / bisect / commit ranges --------------------

def test_green_uses_the_latest_run_per_commit_not_any_run():
    needs_impl("green = LATEST run per commit; an earlier pass does not make a commit green (§10)")
    runs = impl.fold([result("01A", SHA_1, "pass", emitted_at="2026-08-07T01:00:00Z"),
                      result("01B", SHA_1, "fail", emitted_at="2026-08-07T02:00:00Z")])
    assert impl.green(runs) is None


def test_green_excludes_reconstructed_provenance():
    needs_impl("a reconstructed row cannot establish green (§6) — 145 real rows are reconstructed")
    runs = impl.fold([result("01A", SHA_1, "pass", source="reconstructed")])
    assert impl.green(runs) is None


def test_bisect_returns_unknown_for_no_result_never_fail():
    needs_impl("no_result/timeout/incomplete bisect as UNKNOWN, not fail (§10)")
    runs = impl.fold([result("01A", SHA_1, "no_result")])
    assert impl.bisect_verdict(runs, SHA_1) == "unknown"


def test_absent_commit_reports_nodata_not_green_or_fail():
    needs_impl("a commit with no events is 'nodata' — absence is not a verdict (§10)")
    runs = impl.fold([result("01A", SHA_1, "pass")])
    assert impl.bisect_verdict(runs, SHA_2) == "nodata"


def test_timeline_preserves_belief_over_time_including_superseded_values():
    needs_impl("the timeline shows what was believed WHEN; corrections do not erase history (§10)")
    evs = [result("01A", SHA_1, "fail", emitted_at="2026-08-07T01:00:00Z"),
           ev("01C", "run.correct", run_id="01A", supersedes="01A", outcome="no_result",
              reason="runner lost", emitted_at="2026-08-07T02:00:00Z")]
    tl = impl.timeline(evs, run_id="01A")
    assert [x["event_type"] for x in tl] == ["run.result", "run.correct"]
    assert tl[0]["outcome"] == "fail", "the superseded value must remain visible in the timeline"


# ---- legacy migration ------------------------------------------------------

def test_legacy_corpus_migrates_without_loss_and_is_reversible():
    needs_impl("migration maps every legacy row to an event and replays back to the original (§9)")
    rows = legacy_rows()
    events = impl.migrate_legacy(rows, team=TEAM_A)
    assert len(events) == LEGACY_ROWS, "one event per legacy row — nothing collapsed"
    assert len(impl.fold(events)) == LEGACY_RUNS, "runs, not rows"
    assert impl.replay_legacy(events) == rows, "migration must be reversible"


def test_legacy_migration_emits_enrich_not_correct_for_multi_row_groups():
    needs_impl("legacy multi-row groups become run.enrich chains, never run.correct (§9)")
    events = impl.migrate_legacy(legacy_rows(), team=TEAM_A)
    assert not [e for e in events if e["event_type"] == "run.correct"]
    # Every non-first row in a group is an enrich event, whether or not it adds
    # a new value -- the row must survive migration either way (§9 no-loss).
    assert len([e for e in events if e["event_type"] == "run.enrich"]) == (
        LEGACY_PAIRS * 1 + (LEGACY_CHAIN_LEN - 1)
    )
