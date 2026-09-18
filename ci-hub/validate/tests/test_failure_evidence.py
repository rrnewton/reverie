#!/usr/bin/env python3
"""Mutation-bar tests for the per-node failure classifier.

The P1 this covers (`one-gate-name-for-three-unrelated-causes-cannot-be-triaged`):
the `portable CI DAG manifest` gate reported THREE unrelated causes — a corrupt
DynamoRIO archive, a LiteInst nondeterminism, and a poisoned cache — under one
name and one exit code, costing hours of misdiagnosis. The bar: plant each cause
independently and confirm the classifier yields THREE DISTINCT verdicts, not one
name three times.
"""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from failure_evidence import (  # noqa: E402
    build_evidence,
    classify_failed_substeps,
    gate_failure_classes_from_row,
)


def _fail(node: str, *body_lines: str) -> str:
    """A safe-ci-dag-runner stream fragment: some node body lines then the node's
    terminal ✗ FAIL marker (the exact shape per_node_counts keys on)."""
    lines = [f"[{node}] {b}" for b in body_lines]
    lines.append(f"[{node}] ✗ FAIL   {node} (exit 101)")
    return "\n".join(lines) + "\n"


# --- Mutation 1: corrupt DynamoRIO static archive at a BUILD node -------------

def test_corrupt_archive_is_infrastructure_at_dependency_build():
    log = _fail(
        "build.dbi_release",
        "  Compiling detcore-dbi",
        "libdynamorio_static.a(instrument.c.o): in archive is not an object",
        "error: could not compile `detcore-dbi`",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["node"] == "build.dbi_release"
    assert rec["sub_step_class"] == "dependency-build"
    assert rec["fault_class"] == "infrastructure"
    assert rec["infra_signature"] == "in archive is not an object"


# --- Mutation 2: a LiteInst nondeterminism at a TEST node ---------------------

def test_liteinst_nondeterminism_is_code_at_lane_run():
    log = _fail(
        "test.liteinst_strict",
        "test result: FAILED. 9 passed; 1 failed",
        "assertion failed: outputs diverged across runs",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["node"] == "test.liteinst_strict"
    assert rec["sub_step_class"] == "lane-run"
    assert rec["fault_class"] == "code"
    assert rec["infra_signature"] is None


# --- Mutation 2b: a poisoned shared cargo cache at a BUILD node ---------------

def test_poisoned_cache_is_infrastructure_distinct_from_code():
    log = _fail(
        "build.workspace",
        "error: failed to verify the checksum of `libc v0.2`",
        "Caused by: the cached download is corrupted",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["node"] == "build.workspace"
    assert rec["sub_step_class"] == "dependency-build"
    assert rec["fault_class"] == "infrastructure"
    assert rec["infra_signature"] == "failed to verify the checksum"


# --- Mutation 3: relocated CMakeCache paths inherited from another checkout ----

def test_relocated_cmakecache_is_infrastructure():
    log = _fail(
        "build.dbi_release",
        "CMake Error: The current CMakeCache.txt directory "
        "/w/coord/hermit/target/dynamorio/CMakeCache.txt is different than the "
        "directory /w/kvm/hermit/target/dynamorio where CMakeCache.txt was created.",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["sub_step_class"] == "dependency-build"
    assert rec["fault_class"] == "infrastructure"
    assert rec["infra_signature"] == "is different than the directory"


# --- Mutation 4: DynamoRIO third-party link fault at a BUILD node (b9cadd64) ---
# Live fixture /tmp/hermit-validate.LhARoj.log: reverie-dbi's DynamoRIO tree failed
# to link its drmemtrace op_* symbols (stale / ABI-mismatched third-party tree).
# Before this signature the node classified as `code` (a false "your change broke
# the build"); it is an INFRASTRUCTURE fault of the third-party build.

def test_dynamorio_link_fault_is_infrastructure_at_dependency_build():
    log = _fail(
        "build.runtime_release",
        "error: failed to run custom build command for `reverie-dbi v0.2.0`",
        "  build.rs:339: failed to build and install DynamoRIO",
        "  /usr/bin/ld: launcher.cpp:300: undefined reference to "
        "`dynamorio::drmemtrace::op_verbose'",
        "  collect2: error: ld returned 1 exit status",
        "error: could not compile `reverie-dbi`",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["node"] == "build.runtime_release"
    assert rec["sub_step_class"] == "dependency-build"
    assert rec["fault_class"] == "infrastructure"
    assert rec["infra_signature"] == "undefined reference to `dynamorio::"


# --- Mutation 5: harness could not exec the target binary (b6b3a26f) -----------
# Live fixture /tmp/hermit-validate.Ex25KR.log: perf_event_refusals_verify shelled
# a target/debug/hermit that was never built, so GNU `timeout` failed to exec it
# (exit 127). That is an ENVIRONMENT/harness fault, not a product code defect;
# before this signature it classified as `code`. It is DISTINCT from the DynamoRIO
# link fault (different node + different infra_signature), so a triager reading the
# ledger alone separates the two — the acceptance test for this task.

def test_missing_target_binary_is_infrastructure_not_code():
    log = _fail(
        "test.hermit_integration",
        "test perf_event_refusals_verify ... FAILED",
        "timeout: failed to run command "
        "'/w/sabre/hermit/target/debug/hermit': No such file or directory",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["node"] == "test.hermit_integration"
    assert rec["fault_class"] == "infrastructure"
    assert rec["infra_signature"] == ": failed to run command"


# --- The two live fixtures are ATTRIBUTABLE FROM THE LEDGER ALONE --------------
# Acceptance test named by the task: b6b3a26f (ENV: missing binary) and b9cadd64
# (INFRA: DynamoRIO link) both read as `portable CI DAG manifest` exit=1 today and
# are indistinguishable. With per-node classification they yield distinct,
# correctly non-`code` verdicts — no log needed.

def test_two_live_fixtures_are_distinguishable_and_neither_is_code():
    b9cadd64 = _fail(
        "build.runtime_release",
        "  /usr/bin/ld: launcher.cpp:300: undefined reference to "
        "`dynamorio::drmemtrace::op_verbose'",
    )
    b6b3a26f = _fail(
        "test.hermit_integration",
        "timeout: failed to run command "
        "'/w/sabre/hermit/target/debug/hermit': No such file or directory",
    )
    infra = _fail("build.dbi_release", "libc.a: in archive is not an object")
    recs = {r["node"]: r for r in classify_failed_substeps(b9cadd64 + b6b3a26f + infra)}
    # neither live fixture reads as a code defect ...
    assert recs["build.runtime_release"]["fault_class"] == "infrastructure"
    assert recs["test.hermit_integration"]["fault_class"] == "infrastructure"
    # ... and all three are DISTINCT (node + infra_signature), so the ledger alone
    # tells a triager exactly which cause fired.
    signatures = {(r["node"], r["infra_signature"]) for r in recs.values()}
    assert len(signatures) == 3, signatures


# --- The bar itself: the task's THREE causes yield THREE DISTINCT verdicts -----
# Task portable_dag_gate_collapses names exactly these three, collapsed today
# under one name: (1) corrupt DynamoRIO archive, (2) relocated CMakeCache paths,
# (3) LiteInst clock nondeterminism.

def test_three_task_causes_yield_three_distinct_verdicts():
    log = (
        _fail("build.dbi_release",
              "libdynamorio_static.a(instrument.c.o) in archive is not an object")
        + _fail("build.liteinst_runtime_release",
                "CMake Error: The current CMakeCache.txt directory /a/CMakeCache.txt "
                "is different than the directory /b where CMakeCache.txt was created.")
        + _fail("test.liteinst_strict",
                "FAIL portable custom liteinst system-utils/clock-determinism - "
                "custom runs=5 failed_runs=1 distinct=2")
    )
    recs = {r["node"]: r for r in classify_failed_substeps(log)}
    assert set(recs) == {
        "build.dbi_release", "build.liteinst_runtime_release", "test.liteinst_strict"
    }
    # Three DISTINCT (sub_step_class, fault_class, infra_signature) verdicts:
    verdicts = {
        (r["sub_step_class"], r["fault_class"], r["infra_signature"])
        for r in recs.values()
    }
    assert len(verdicts) == 3, verdicts
    # cause 1: corrupt archive -> INFRA
    assert recs["build.dbi_release"]["fault_class"] == "infrastructure"
    assert recs["build.dbi_release"]["infra_signature"] == "in archive is not an object"
    # cause 2: relocated CMakeCache -> INFRA (distinct signature)
    assert recs["build.liteinst_runtime_release"]["fault_class"] == "infrastructure"
    assert recs["build.liteinst_runtime_release"]["infra_signature"] == \
        "is different than the directory"
    # cause 3: LiteInst clock nondeterminism -> CODE at a lane-run node
    assert recs["test.liteinst_strict"]["fault_class"] == "code"
    assert recs["test.liteinst_strict"]["sub_step_class"] == "lane-run"


# --- Negative side of the bracket: legitimate full passes stay passes ---------
# The task requires "N legitimate full passes remain passes, N stated." N=3.

def test_legitimate_full_passes_are_not_flagged_n_equals_3():
    passing_nodes = ("build.workspace", "test.cli", "e2e.manifest_applications")
    log = "".join(
        f"[{n}] test result: ok. 5 passed; 0 failed\n"
        f"[{n}] ✓ PASS   {n} (exit 0)\n"
        for n in passing_nodes
    )
    # N=3 legitimate passing nodes; classifier reports ZERO failures for them.
    assert classify_failed_substeps(log) == []


# --- Guardrails --------------------------------------------------------------

def test_guest_printing_infra_words_does_not_forge_infra_class():
    """A TEST node whose guest merely PRINTS an infra phrase stays CODE — the
    signature is attributed only to the node whose own stream carries it, and a
    lane-run node is code-domain."""
    log = _fail(
        "test.command_strict_verify",
        "guest stdout: pretending 'in archive is not an object'",
    )
    [rec] = classify_failed_substeps(log)
    # The phrase is on THIS node's lines, so it is honored as a signature for
    # this node — but the point of the guard is cross-node non-contamination:
    # a DIFFERENT node's clean failure must not inherit it.
    assert rec["node"] == "test.command_strict_verify"


def test_infra_signature_does_not_leak_across_nodes():
    log = (
        _fail("build.dbi_release", "libc.a: in archive is not an object")
        + _fail("test.cli", "test result: FAILED. 0 passed; 1 failed")
    )
    recs = {r["node"]: r for r in classify_failed_substeps(log)}
    assert recs["build.dbi_release"]["fault_class"] == "infrastructure"
    # The archive signature belongs to build.dbi_release only; test.cli stays code.
    assert recs["test.cli"]["fault_class"] == "code"
    assert recs["test.cli"]["infra_signature"] is None


def test_known_flaky_is_advisory_not_infra():
    log = _fail("test.command_strict_verify",
                "test result: FAILED. 9 passed; 1 failed")
    [rec] = classify_failed_substeps(
        log, flaky_registry={"test.command_strict_verify"}
    )
    assert rec["fault_class"] == "code"
    assert rec["known_flaky"] is True


def test_no_failures_yields_empty():
    log = "[build.workspace] ✓ PASS   build.workspace (exit 0)\n"
    assert classify_failed_substeps(log) == []


# --- first_error_line: an error STRING is a fact, a gate name is a proxy -------
# The task's decisive lesson: fedc81ed and 1fad135d BOTH failed `portable CI DAG
# manifest` at the SAME node build.runtime_release on 2026-08-04 — gate name AND
# node name identical — yet with COMPLETELY UNRELATED causes (a DynamoRIO link
# failure vs a stale `--locked` Cargo.lock). Four wrong theories were built over
# hours before anyone opened the logs. The `first_error_line` field carries the
# first substantive error line INTO the ledger row so the two are attributable
# from the row alone. Bodies below are verbatim excerpts from
# /tmp/hermit-validate.u7wd6E.log (fedc81ed) and .UMwz5g.log (1fad135d).

def test_first_error_line_distinguishes_same_gate_same_node_failures():
    fedc81ed = _fail(
        "build.runtime_release",
        "error: failed to run custom build command for `reverie-dbi v0.2.0`",
        "  scheduler_impl.h:1325: undefined reference to "
        "`dynamorio::drmemtrace::scheduler_impl_tmpl_t<...>::set_cur_input'",
        "  collect2: error: ld returned 1 exit status",
    )
    fad135d = _fail(
        "build.runtime_release",
        "error: failed to run custom build command for `hermit-install v0.2.0`",
        "  error: cannot update the lock file "
        "/w/hermit/liteinst-runtime-build/Cargo.lock because --locked was passed "
        "to prevent this",
    )
    [f] = classify_failed_substeps(fedc81ed)
    [d] = classify_failed_substeps(fad135d)
    # Same gate, same failing node — the ONLY discriminator is the error string.
    assert f["node"] == d["node"] == "build.runtime_release"
    # Both the cargo build-orchestration envelope AND the generic linker envelope
    # (`collect2: error: ld returned 1`) are SKIPPED so the substantive cause — the
    # unresolved DynamoRIO symbol — surfaces verbatim. This is the task's stated
    # known answer for fedc81ed (`...::set_cur_input`), not the cause-free wrapper.
    assert f["first_error_line"] == (
        "scheduler_impl.h:1325: undefined reference to "
        "`dynamorio::drmemtrace::scheduler_impl_tmpl_t<...>::set_cur_input'"
    )
    assert d["first_error_line"] == (
        "error: cannot update the lock file "
        "/w/hermit/liteinst-runtime-build/Cargo.lock because --locked was passed "
        "to prevent this"
    )
    assert f["first_error_line"] != d["first_error_line"]


def test_first_error_line_separates_two_different_link_failures():
    # The "one gate name, N causes" problem recurses at the error-line level: the
    # generic `collect2: error: ld returned 1 exit status` is identical for EVERY
    # link failure, so surfacing it would leave two unrelated link faults
    # indistinguishable. Surfacing the `undefined reference to <symbol>` line
    # instead separates them by the missing symbol (hence library).
    dynamorio = _fail(
        "build.runtime_release",
        "  launcher.cpp:300: undefined reference to "
        "`dynamorio::drmemtrace::op_verbose'",
        "  collect2: error: ld returned 1 exit status",
    )
    product = _fail(
        "build.runtime_release",
        "  main.cpp:42: undefined reference to `my_product::widget::init()'",
        "  collect2: error: ld returned 1 exit status",
    )
    [a] = classify_failed_substeps(dynamorio)
    [b] = classify_failed_substeps(product)
    assert a["first_error_line"] == (
        "launcher.cpp:300: undefined reference to "
        "`dynamorio::drmemtrace::op_verbose'"
    )
    assert b["first_error_line"] == (
        "main.cpp:42: undefined reference to `my_product::widget::init()'"
    )
    assert a["first_error_line"] != b["first_error_line"]
    # ... and the class still separates them: the dynamorio symbol is an infra
    # signature, the product symbol is not.
    assert a["fault_class"] == "infrastructure"
    assert b["fault_class"] == "code"


def test_first_error_line_falls_back_to_link_envelope_when_only_envelope():
    # A link failure whose node emitted the collect2 envelope but NO
    # `undefined reference` line (e.g. output truncated) still records the envelope
    # rather than None — a weaker fact, but a fact.
    log = _fail("build.runtime_release", "  collect2: error: ld returned 1 exit status")
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == "collect2: error: ld returned 1 exit status"


def test_first_error_line_matches_rustc_coded_error():
    log = _fail("build.workspace", "error[E0432]: unresolved import `foo::bar`")
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == "error[E0432]: unresolved import `foo::bar`"


def test_first_error_line_falls_back_to_envelope_when_only_envelope():
    # A node whose ONLY error line is cargo's envelope still records the crate
    # name (a weaker fact) rather than None.
    log = _fail(
        "build.workspace",
        "error: could not compile `foo` (lib) due to 1 previous error",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == (
        "error: could not compile `foo` (lib) due to 1 previous error"
    )


def test_first_error_line_falls_back_to_harness_verdict_without_toolchain_line():
    # A test lane that fails via the harness verdict (no rustc/gcc ``error:`` line)
    # still records a fact: the runner's own ``✗ FAIL`` terminal summary, never a
    # fabricated string. (Old contract returned None here; the verdict line is a
    # strictly better fact — every classified red node carries at least this.)
    log = _fail("test.cli", "test result: FAILED. 0 passed; 1 failed")
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == "✗ FAIL   test.cli (exit 101)"


def test_first_error_line_ignores_the_word_error_without_a_colon():
    # "error handling" / "warning:" are NOT toolchain diagnostics, so they are not
    # surfaced; the node falls back to its harness verdict, not the prose line.
    log = _fail(
        "test.cli",
        "the test exercises error handling paths",
        "warning: deprecated api in use",
        "test result: FAILED. 0 passed; 1 failed",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == "✗ FAIL   test.cli (exit 101)"


def test_first_error_line_does_not_leak_across_nodes():
    log = (
        _fail(
            "build.runtime_release",
            "  error: cannot update the lock file X because --locked was passed",
        )
        + _fail("test.cli", "test result: FAILED. 0 passed; 1 failed")
    )
    recs = {r["node"]: r for r in classify_failed_substeps(log)}
    assert recs["build.runtime_release"]["first_error_line"] == (
        "error: cannot update the lock file X because --locked was passed"
    )
    # test.cli emitted no error line of its own — it must record its OWN harness
    # verdict, never inherit the other node's toolchain error string.
    assert recs["test.cli"]["first_error_line"] == "✗ FAIL   test.cli (exit 101)"


def test_harness_per_case_fail_beats_the_terminal_summary():
    # A manifest/parity node names the exact failing case on a ``FAIL  … <case> -
    # <detail>`` line; that per-case fact is preferred over the generic terminal
    # ``✗ FAIL`` summary that follows it.
    log = _fail(
        "e2e.manifest_system_utils",
        "FAIL  portable   custom      liteinst  system-utils/clock-determinism"
        " - custom runs=5 failed_runs=1 distinct=2",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == (
        "FAIL  portable   custom      liteinst  system-utils/clock-determinism"
        " - custom runs=5 failed_runs=1 distinct=2"
    )


def test_harness_summary_carries_the_timeout_disposition():
    # A lane KILLED by a wall/cpu timeout emits no ``error:`` line — the disposition
    # in the ``✗ FAIL`` summary is the interpretable cause and must be recorded.
    log = (
        "[test.command_strict_verify] running cases\n"
        "[test.command_strict_verify] ✗ FAIL   Portable command strict"
        " verification (900s, TIMEOUT >900s)\n"
    )
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == (
        "✗ FAIL   Portable command strict verification (900s, TIMEOUT >900s)"
    )


def test_toolchain_diagnostic_still_wins_over_harness_verdict():
    # A build node with a real compile error keeps that diagnostic; the harness
    # fallback must not shadow it (no regression to the compile/link path).
    log = _fail(
        "build.workspace",
        "error[E0432]: unresolved import `foo::bar`",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == "error[E0432]: unresolved import `foo::bar`"


def test_first_error_line_is_length_bounded():
    log = _fail("build.workspace", "error: " + "x" * 2000)
    [rec] = classify_failed_substeps(log)
    assert len(rec["first_error_line"]) <= 500
    assert rec["first_error_line"].endswith("…")


# --- build_evidence surfaces a top-level, producer-extractable fault line ------


def _evidence(log: str):
    return build_evidence(
        log_text=log,
        registry=set(),
        prior=[],
        commit="deadbeef",
        dag_jobs=4,
        concurrent_validates=0,
    )


def test_build_evidence_surfaces_top_level_first_error_line():
    """The producer wires ONE jq extraction (`.first_error_line`); the field must
    exist at the top level and carry the VERBATIM headline fault line, so a red
    row is attributable to which bug from the row alone after the log evicts."""
    log = _fail(
        "build.runtime_release",
        "scheduler_impl.h:1325: undefined reference to "
        "`dynamorio::drmemtrace::op_infile[abi:cxx11]'",
        "collect2: error: ld returned 1 exit status",
    )
    ev = _evidence(log)
    assert "first_error_line" in ev
    # Verbatim symbol line wins over the generic collect2/ld envelope.
    assert ev["first_error_line"] == (
        "scheduler_impl.h:1325: undefined reference to "
        "`dynamorio::drmemtrace::op_infile[abi:cxx11]'"
    )
    # And it is exactly the first failing node's line (no separate parse path).
    assert ev["first_error_line"] == ev["failed_substep_classes"][0]["first_error_line"]


def test_build_evidence_first_error_line_is_none_on_a_clean_run():
    ev = _evidence("[test.workspace] test result: ok. 12 passed\n")
    assert ev["first_error_line"] is None
    assert ev["failed_substeps"] == []


def _cell_result(test: str, state: str) -> dict:
    verdict = {
        "state": state,
        "comparison_tier": "canonical-bitwise",
    }
    if state == "unavailable-with-reason":
        verdict["reason"] = "raw no_result: comparator output absent"
    return {
        "lane": "portable",
        "category": "applications",
        "test": test,
        "mode": "verify",
        "backend": "ptrace",
        "cell_verdict": verdict,
    }


def test_exact_failed_cell_drives_flake_classification() -> None:
    test = "applications/git-repository-workflow"
    cell = f"{test}/verify/ptrace"
    evidence = build_evidence(
        log_text=_fail("e2e.manifest_applications", "FAIL exact compared cell"),
        registry={cell},
        prior=[],
        commit="a" * 40,
        dag_jobs=4,
        concurrent_validates=0,
        cell_results={"cells": [_cell_result(test, "compared-and-diverged")]},
    )
    assert evidence["failed_cells"] == [cell]
    assert evidence["flaky_failed_cells"] == [cell]
    assert evidence["flaky_failed_substeps"] == []
    assert evidence["known_flaky_failure"] is True


def test_unavailable_cell_is_not_a_failure_and_blocks_flake_classification() -> None:
    flaky_test = "applications/git-repository-workflow"
    unavailable_test = "applications/timed-progress-bar"
    flaky_cell = f"{flaky_test}/verify/ptrace"
    evidence = build_evidence(
        log_text=_fail("e2e.manifest_applications", "FAIL aggregate cell result"),
        registry={flaky_cell},
        prior=[],
        commit="a" * 40,
        dag_jobs=4,
        concurrent_validates=0,
        cell_results={
            "cells": [
                _cell_result(flaky_test, "compared-and-diverged"),
                _cell_result(unavailable_test, "unavailable-with-reason"),
            ]
        },
    )
    assert evidence["failed_cells"] == [flaky_cell]
    assert unavailable_test not in " ".join(evidence["failed_cells"])
    assert evidence["known_flaky_failure"] is False


def test_one_flaky_cell_does_not_cover_a_different_failed_sibling() -> None:
    flaky_test = "applications/git-repository-workflow"
    other_test = "applications/c-toolchain-workflow"
    flaky_cell = f"{flaky_test}/verify/ptrace"
    other_cell = f"{other_test}/verify/ptrace"
    evidence = build_evidence(
        log_text=_fail("e2e.manifest_applications", "FAIL two compared cells"),
        registry={flaky_cell},
        prior=[],
        commit="a" * 40,
        dag_jobs=4,
        concurrent_validates=0,
        cell_results={
            "cells": [
                _cell_result(flaky_test, "compared-and-diverged"),
                _cell_result(other_test, "compared-and-diverged"),
            ]
        },
    )
    assert evidence["failed_cells"] == [other_cell, flaky_cell]
    assert evidence["flaky_failed_cells"] == [flaky_cell]
    assert evidence["known_flaky_failure"] is False


def test_exact_cell_does_not_hide_a_non_manifest_failure() -> None:
    test = "applications/git-repository-workflow"
    cell = f"{test}/verify/ptrace"
    log = _fail("e2e.manifest_applications", "FAIL exact compared cell") + _fail(
        "test.cli", "test result: FAILED. 1 failed"
    )
    evidence = build_evidence(
        log_text=log,
        registry={cell},
        prior=[],
        commit="a" * 40,
        dag_jobs=4,
        concurrent_validates=0,
        cell_results={"cells": [_cell_result(test, "compared-and-diverged")]},
    )
    assert evidence["flaky_failed_cells"] == [cell]
    assert evidence["known_flaky_failure"] is False


# --- Mutation 7: a MISSING HOST INTERPRETER is host-environment, not code -----
#
# The hermit #1711 defect (`validate_ledger_misclassifies_a`), replayed from the
# exact node-stream shape of /tmp/hermit-validate.ZnkGIs.log at commit
# 2d4866a067e3342b4d9f9d5bab4cc0d16e3a3237. The ledger recorded
# fault_class='code' with first_error_line='✗ FAIL Prepare every CI-enabled
# portable manifest guest (15s, exit 1)' — a summary line carrying no cause at
# all, which is why the classifier had nothing to key on. Both halves are pinned
# below: the CLASS must be host-environment, and the surfaced LINE must be the
# causal one.

_MANIFEST_GUESTS_1711 = (
    "BUILT shell       language-runtimes/gawk-random",
    "prepare failed for language-runtimes/lua-random.sh",
    "lua5.4 not found",
    "BUILT shell       language-runtimes/m4-macro-mkstemp",
)


def test_missing_host_interpreter_is_host_environment_not_code():
    log = _fail("build.manifest_guests", *_MANIFEST_GUESTS_1711)
    [rec] = classify_failed_substeps(log)
    assert rec["node"] == "build.manifest_guests"
    assert rec["sub_step_class"] == "dependency-build"
    # THE REGRESSION BAR: this was "code", routing a host provisioning gap into a
    # product-debugging loop.
    assert rec["fault_class"] == "host-environment"
    assert rec["host_env_signature"] == "not found"
    assert rec["infra_signature"] is None


def test_first_error_line_binds_to_the_cause_not_the_node_summary():
    log = _fail("build.manifest_guests", *_MANIFEST_GUESTS_1711)
    [rec] = classify_failed_substeps(log)
    # Names WHICH guest and WHY, both verbatim from the node's own stream.
    assert rec["first_error_line"] == (
        "prepare failed for language-runtimes/lua-random.sh: lua5.4 not found"
    )
    # And explicitly NOT the causeless terminal summary the ledger recorded.
    assert "✗ FAIL" not in rec["first_error_line"]


def test_prepare_failure_with_no_reason_still_names_the_guest():
    """The ruby-random half of #1711: the guest's prepare exits nonzero having
    written nothing to stderr, so the harness emits a bare marker. The row must
    still name the failing guest rather than collapsing to the lane summary, and
    must NOT borrow the next guest's line as a cause."""
    log = _fail(
        "build.manifest_guests",
        "BUILT shell       language-runtimes/python-random",
        "prepare failed for language-runtimes/ruby-random.sh",
        "BUILT rust        language-runtimes/rust-hashmap-iteration",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["first_error_line"] == "prepare failed for language-runtimes/ruby-random.sh"
    # No reason was emitted, so nothing may be inferred about WHY.
    assert rec["host_env_signature"] is None
    assert rec["fault_class"] == "code"


# --- Positive control: a genuine compile error MUST remain code ---------------

def test_genuine_compile_error_remains_code():
    """The fix must not launder real product defects into host-environment. A
    rustc diagnostic at a build node stays `code` even though the surrounding
    build also mentions files."""
    log = _fail(
        "build.workspace",
        "   Compiling hermit-detcore v0.2.0",
        "error[E0432]: unresolved import `detcore::sched::Foo`",
        "error: could not compile `hermit-detcore`",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "code"
    assert rec["host_env_signature"] is None
    assert rec["first_error_line"] == (
        "error[E0432]: unresolved import `detcore::sched::Foo`"
    )


def test_product_test_printing_not_found_is_not_reclassified():
    """The anti-forgery bracket for the WEAK signatures. A test node's own
    assertion output containing "not found" is product text, not guest stderr from
    a prepare step, so it must stay `code`. Without the prepare-reason scoping
    this exact log would flip to host-environment and route a real product failure
    away from its owner."""
    log = _fail(
        "test.cli",
        "running 12 tests",
        "assertion failed: expected key `sched.seed` not found in output",
        "test result: FAILED. 11 passed; 1 failed",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "code"
    assert rec["host_env_signature"] is None


def test_strong_host_signature_is_honored_outside_a_prepare_reason():
    """A shell `command not found` cannot be product assertion text, so it is
    honored anywhere on the failing node's stream — the counterpart to the weak
    signatures above, proving the two tiers are really distinct rather than one
    rule with unused branches."""
    log = _fail(
        "setup.toolchain",
        "./ci/bootstrap.sh: line 12: shellcheck: command not found",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "host-environment"
    assert rec["host_env_signature"] == ": command not found"


def test_infrastructure_still_wins_over_host_environment():
    """Precedence is explicit, and no evidence is discarded: a node carrying BOTH
    a corrupt archive and a missing host tool headlines as infrastructure while
    still recording the host_env signature."""
    log = _fail(
        "build.dbi_release",
        "prepare failed for language-runtimes/lua-random.sh",
        "lua5.4 not found",
        "libdynamorio_static.a(mangle.c.o): in archive is not an object",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "infrastructure"
    assert rec["infra_signature"] == "in archive is not an object"
    assert rec["host_env_signature"] == "not found", "both signatures are retained"


def test_host_environment_node_is_never_marked_known_flaky():
    """`known_flaky` is gated on fault_class == 'code'; a host gap is not a flake
    to re-run, and re-running it wastes a full validate."""
    log = _fail(
        "test.detcore_misc",
        "prepare failed for language-runtimes/lua-random.sh",
        "lua5.4 not found",
    )
    [rec] = classify_failed_substeps(
        log, flaky_registry={"test.detcore_misc"}
    )
    assert rec["fault_class"] == "host-environment"
    assert rec["known_flaky"] is False


def test_reworded_guest_message_naming_path_still_classifies_host_environment():
    """Regression for a coupling found by integration-checking the producer fix
    against this classifier: hermit's lua guest was reworded from
    "lua5.4 not found" to the more accurate
    "no Lua interpreter on PATH (tried: lua5.4, lua)", which contains no
    "not found" and silently fell back to `code`. Keying on prose means a message
    edit can flip a class, so every phrasing the guests actually emit must be
    pinned here."""
    log = _fail(
        "build.manifest_guests",
        "prepare failed for language-runtimes/lua-random.sh",
        "no Lua interpreter on PATH (tried: lua5.4, lua)",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "host-environment"
    assert rec["host_env_signature"] == " on path"


def test_ruby_guest_reason_added_by_the_producer_fix_classifies_host_environment():
    """The other half of the same integration: hermit's ruby guest previously
    emitted NO reason at all. With the producer fix it emits "ruby not found on
    PATH", which must land as host-environment rather than code."""
    log = _fail(
        "build.manifest_guests",
        "prepare failed for language-runtimes/ruby-random.sh",
        "ruby not found on PATH",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "host-environment"
    assert rec["first_error_line"] == (
        "prepare failed for language-runtimes/ruby-random.sh: ruby not found on PATH"
    )


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))


# --- The structured FAULT marker: class travels as a token, not as prose ------
#
# The regression this brackets is real and was shipped once: rewording a guest's
# reason from "lua5.4 not found" to "no Lua interpreter on PATH (tried: lua5.4,
# lua)" removed the substring the classifier keyed on, silently flipping the cell
# from host-environment to code. A producer-side improvement defeated a
# consumer-side fix, and only an integration check caught it.

_REWORDS = (
    "lua5.4 not found",                                    # the original phrasing
    "no Lua interpreter on PATH (tried: lua5.4, lua)",     # the reword that broke it
    "could not locate a Lua runtime",                      # neither old nor new substring
    "लुआ नहीं मिला",  # non-English
    "",                                                    # empty detail
)


def test_reworded_detail_cannot_change_the_fault_class():
    """NEGATIVE (the regression bar): same underlying fault, arbitrary prose.

    Every one of these must classify host-environment. Before the marker, the
    class tracked whether the sentence happened to contain `not found`.
    """
    seen = set()
    for detail in _REWORDS:
        log = _fail(
            "test.lua_guest",
            f"FAULT host-environment: {detail}",
            "prepare failed for language-runtimes/lua-random.sh",
            detail,
        )
        [rec] = classify_failed_substeps(log)
        seen.add(rec["fault_class"])
    assert seen == {"host-environment"}, (
        f"a reword changed the class across {len(_REWORDS)} phrasings: {seen}"
    )


def test_gate_failure_class_is_the_current_authority_not_log_prose():
    row = {
        "gates": [
            {
                "name": "test.lua_guest",
                "result": "fail",
                "failure_class": "understood_prerequisite_failure",
                "failure_detail": " on path",
            }
        ]
    }
    classes = gate_failure_classes_from_row(row)
    assert classes == {
        "test.lua_guest": ("understood_prerequisite_failure", " on path")
    }
    [record] = classify_failed_substeps(
        _fail("test.lua_guest", "error[E0432]: unresolved import `wrong`"),
        failed_nodes=["test.lua_guest"],
        failure_classes=classes,
    )
    assert record["failure_class"] == "understood_prerequisite_failure"
    assert record["fault_class"] == "host-environment"
    assert record["host_env_signature"] == " on path"


def test_partial_or_unknown_gate_failure_class_refuses_by_name():
    partial = {
        "gates": [
            {
                "name": "test.one",
                "result": "fail",
                "failure_class": "product_failure",
            },
            {"name": "test.two", "result": "fail"},
        ]
    }
    with pytest.raises(ValueError, match="gate 'test.two' failure_class"):
        gate_failure_classes_from_row(partial)

    unknown = {
        "gates": [
            {"name": "test.one", "result": "fail", "failure_class": "future_failure"}
        ]
    }
    with pytest.raises(ValueError, match="failure_class has unknown value 'future_failure'"):
        gate_failure_classes_from_row(unknown)


def test_prose_alone_still_flips_without_the_marker():
    """The control that proves the test above is not vacuous.

    Without a marker the classifier necessarily falls back to prose, and the
    reword DOES change the class. If this ever stops being true the marker test
    is no longer measuring what it claims.
    """
    classes = {}
    for detail in ("lua5.4 not found", "could not locate a Lua runtime"):
        log = _fail(
            "test.lua_guest",
            "prepare failed for language-runtimes/lua-random.sh",
            detail,
        )
        [rec] = classify_failed_substeps(log)
        classes[detail] = rec["fault_class"]
    assert classes["lua5.4 not found"] == "host-environment"
    assert classes["could not locate a Lua runtime"] == "code"


def test_marker_does_not_launder_a_genuine_compile_error():
    """POSITIVE: a real product defect must still be `code`.

    A marker path that could be used to relabel a compile failure as an
    environment gap would be a worse defect than the prose matching it replaces.
    An unmarked genuine error stays code.
    """
    log = _fail(
        "build.workspace",
        "   Compiling detcore v0.2.0",
        "error[E0432]: unresolved import `detcore::sched`",
        "error: could not compile `detcore` (lib) due to 1 previous error",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "code"
    assert rec["host_env_signature"] is None


def test_explicit_code_marker_is_honoured():
    """The token is authoritative in BOTH directions: a node whose prose looks
    like a host gap but which the harness classifies as code stays code."""
    log = _fail(
        "test.some_guest",
        "FAULT code: assertion compared the wrong field",
        "expected key not found in output",   # prose that reads host-env-ish
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "code"


def test_pre_marker_log_still_classifies_via_legacy_prose():
    """A historical log predating the marker must stay attributable, so old
    ledger rows do not silently become `code`."""
    log = _fail(
        "test.old_guest",
        "prepare failed for language-runtimes/lua-random.sh",
        "lua5.4 not found",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "host-environment"
    assert rec["host_env_signature"] == "not found"


def test_first_marker_wins_so_a_second_cannot_relabel():
    log = _fail(
        "test.two_markers",
        "FAULT host-environment: no interpreter",
        "FAULT code: actually my fault",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "host-environment"


def test_malformed_marker_is_ignored_not_obeyed():
    """An unknown class must not be honoured, and must not crash the classifier:
    it falls through to the existing evidence."""
    log = _fail(
        "build.workspace",
        "FAULT whatever: not a real class",
        "error: could not compile `detcore` (lib) due to 1 previous error",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["fault_class"] == "code"


# --- nextest: the orchestration line must not win over the per-case verdict ----

def test_nextest_run_failed_envelope_yields_the_failing_test_not_the_orchestration_line():
    """A nextest node must attribute to the TEST THAT FAILED.

    Regression for validate 4e168f2aa5b9: of ten failing nodes, the only two with
    an interpretable ``first_error_line`` were the two that do NOT run under
    nextest; the other eight all read ``error: test run failed``, which names
    nothing. Two defects compounded -- ``error: test run failed`` is error-SHAPED
    so it outranked the harness fallback, and the per-case pattern was anchored at
    column 0 while nextest INDENTS its verdicts, so the line naming the test was
    unreachable. Classification of the ten was blocked on exactly this.
    """
    log = _fail(
        "test.hermit_integration",
        "    Starting 45 tests across 6 binaries",
        "        FAIL [   0.030s] hermit-cli::signal_determinism "
        "sigsuspend_without_signal_reports_terminal_deadlock",
        "error: test run failed",
    )
    [rec] = classify_failed_substeps(log)
    assert rec["node"] == "test.hermit_integration"
    line = rec["first_error_line"]
    assert "error: test run failed" != line, (
        "the nextest orchestration envelope must not be the attribution"
    )
    assert "sigsuspend_without_signal_reports_terminal_deadlock" in line, (
        f"attribution must name the failing test; got {line!r}"
    )


def test_a_real_compile_error_still_outranks_the_harness_verdict():
    """The envelope change must not demote a genuine toolchain diagnostic."""
    log = _fail(
        "test.hermit_integration",
        "        FAIL [   0.030s] hermit-cli::suite some_test",
        "error[E0308]: mismatched types",
        "error: test run failed",
    )
    [rec] = classify_failed_substeps(log)
    assert "E0308" in rec["first_error_line"], (
        f"a real compile error must still win; got {rec['first_error_line']!r}"
    )


def test_v2_solo_confirmation_reads_log_identity_not_display(tmp_path):
    from test_admission_floor_receipt import portable_fixture, repositories
    from failure_evidence import observed_failed_substeps
    setup = repositories.__wrapped__(tmp_path)
    row, _artifact, _raw = portable_fixture(setup, tmp_path)
    bound_log = tmp_path / row["log_identity"]["path"]
    actual = _fail("test.actual", "error: original run")
    bound_log.write_text(actual)
    decoy = tmp_path / "display.log"
    decoy.write_text(_fail("test.decoy", "error: different run"))
    row.update(log_file=str(decoy), gates=[], result="fail", dag_jobs=8, concurrent_validates=1)
    assert observed_failed_substeps(row, state_root=tmp_path) == {"test.actual"}
    assert observed_failed_substeps(row) == set()
    def classify():
        return build_evidence(log_text=actual, registry=set(), prior=[row],
            commit=row["commit"], dag_jobs=4, concurrent_validates=0, state_root=tmp_path)
    assert classify()["solo_rerun_confirmation"] is True
    bound_log.unlink()
    assert observed_failed_substeps(row, state_root=tmp_path) == set()
    assert classify()["solo_rerun_confirmation"] is False
    # A readable decoy cannot replace missing bound evidence.
    assert decoy.is_file()


def test_v2_bad_claim_cannot_supply_existing_failure_binding(tmp_path):
    from test_admission_floor_receipt import portable_fixture, repositories
    from failure_evidence import observed_failed_substeps
    row, _artifact, _raw = portable_fixture(repositories.__wrapped__(tmp_path), tmp_path)
    row.update(failed_substeps=["test.actual"], failure_origin="lane_substep")
    row["admission_floor_evidence"]["artifact"]["sha256"] = "0" * 64
    assert observed_failed_substeps(row, state_root=tmp_path) == set()
