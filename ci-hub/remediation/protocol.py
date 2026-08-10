#!/usr/bin/env python3
"""Arm and watch mandatory verification after a speculative/admin land."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from datetime import datetime, timezone
import re
import select
import shutil
import signal
import socket
import subprocess
import sys
import time
import uuid
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub"))
sys.path.insert(0, str(ROOT / "ci-hub/history"))
sys.path.insert(0, str(ROOT / "ci-hub/ledger"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import obligations
from check_outcome import CheckOutcome, classify_check, select_latest_workflow_attempts
from check_outcome import FAIL_CONCLUSIONS as _RED_CONCLUSIONS
from nonzero_result import is_zero_test_green
import validate_rows as canonical_ledger

DEFAULT_REPO = "rrnewton/hermit"
DEFAULT_DECISION_WAITS_PATH = ROOT / "ci-hub/remediation/decision-waits.json"
DECISION_WAIT_STATE = "AWAITING-DECISION"
PARENT_REPO = "rrnewton/dev-hermit"
PARENT_TOOLING_WORKFLOW = "Dev-hermit operational tooling"
PARENT_TOOLING_WORKFLOW_FILE = ".github/workflows/dev-hermit-ci.yml"
PARENT_PORTABILITY_WORKFLOW = "Portability"
PARENT_PORTABILITY_WORKFLOW_FILE = ".github/workflows/portability.yml"
PARENT_DEMO_REVIEW_WORKFLOW = "Demo review gate"
PARENT_DEMO_REVIEW_WORKFLOW_FILE = ".github/workflows/demo-review-gate.yml"
DEFAULT_WORKFLOW = "CI (GitHub-managed portable)"
DEFAULT_WORKFLOW_FILE = ".github/workflows/ci-portable.yml"
PRIVILEGED_WORKFLOW = "CI (privileged)"
PRIVILEGED_WORKFLOW_FILE = ".github/workflows/ci-privileged.yml"
REVERIE_REPO = "rrnewton/reverie"
REVERIE_WORKFLOW = "Rust"
REVERIE_WORKFLOW_FILE = ".github/workflows/ci.yml"
# agent-utils is a first-class checked-out submodule (`update = checkout`) and
# owner tooling directives land there directly on main, so their landing
# ancestry is verifiable exactly like the other three. It deliberately gets NO
# entry in `_CURRENT_VERIFICATION_POLICY_VERSION`: see `resolve_repo_source`.
AGENT_UTILS_REPO = "rrnewton/agent-utils"
VERIFICATION_POLICY_SCHEMA_VERSION = 2
_CURRENT_VERIFICATION_POLICY_VERSION = {
    # v3 makes the owner-authorized Hermit hosted authority the portable job;
    # retain v2 below so already-open obligations keep their frozen 2-job policy.
    DEFAULT_REPO: 3,
    REVERIE_REPO: VERIFICATION_POLICY_SCHEMA_VERSION,
    # The parent has no validate.sh producer. Its distinct authority is the
    # exact-head set of deterministic parent-tooling, path-policy, and demo
    # review jobs below; absence of any member is a no-result, never a skip.
    PARENT_REPO: 1,
}
# Each tuple is (exact workflow path, exact workflow name, exact job name).
# The positive count is persisted beside this complete set and must equal its
# cardinality.  Workflow-level conclusions are deliberately absent: only the
# exact-SHA jobs below are hosted authority.
_VERSIONED_REPO_GITHUB_JOBS = {
    (DEFAULT_REPO, 2): (
        (
            DEFAULT_WORKFLOW_FILE,
            DEFAULT_WORKFLOW,
            "Regular tests (GitHub-managed portable)",
        ),
        (
            PRIVILEGED_WORKFLOW_FILE,
            PRIVILEGED_WORKFLOW,
            "Privileged capability and E2E tests",
        ),
    ),
    (DEFAULT_REPO, 3): (
        (
            DEFAULT_WORKFLOW_FILE,
            DEFAULT_WORKFLOW,
            "Regular tests (GitHub-managed portable)",
        ),
    ),
    (REVERIE_REPO, 2): (
        (
            REVERIE_WORKFLOW_FILE,
            REVERIE_WORKFLOW,
            "Regular tests (GitHub-hosted)",
        ),
        (
            REVERIE_WORKFLOW_FILE,
            REVERIE_WORKFLOW,
            "Host-dependent tests (self-hosted)",
        ),
    ),
    (PARENT_REPO, 1): (
        (
            PARENT_TOOLING_WORKFLOW_FILE,
            PARENT_TOOLING_WORKFLOW,
            "Parent tooling shard",
        ),
        (
            PARENT_TOOLING_WORKFLOW_FILE,
            PARENT_TOOLING_WORKFLOW,
            "ci-hub bounded operations shard",
        ),
        (
            PARENT_PORTABILITY_WORKFLOW_FILE,
            PARENT_PORTABILITY_WORKFLOW,
            "Reject owner-specific build paths",
        ),
        (
            PARENT_DEMO_REVIEW_WORKFLOW_FILE,
            PARENT_DEMO_REVIEW_WORKFLOW,
            "Demo-touching commits require a green-demo attestation",
        ),
    ),
}
_DEFAULT_REPO_SOURCES = {
    DEFAULT_REPO: ROOT / "hermit",
    REVERIE_REPO: ROOT / "reverie",
    PARENT_REPO: ROOT,
    AGENT_UTILS_REPO: ROOT / "agent-utils",
}
DEFAULT_POLL_SECONDS = 15
DEFAULT_GITHUB_WAIT_SECONDS = 120
DEFAULT_NETWORK_TIMEOUT = float(
    os.environ.get("CI_HUB_REMEDIATION_NETWORK_TIMEOUT", "120")
)
# BOUND INVERSION -- the reason `watch --once --gate` used to be SIGKILLed.
#
# tick-hub runs each gate under a 30s guillotine
# (agent-utils/py/tick_hub/probes.py DEFAULT_GATE_TIMEOUT_SECS = 30) and, on
# expiry, kills the process group and DISCARDS the captured stdout. Meanwhile
# `watch` polls every unresolved obligation serially, each poll making network
# calls bounded at DEFAULT_NETWORK_TIMEOUT = 120s -- FOUR TIMES the whole outer
# budget, ten call sites, five obligations in the measured run. One slow GitHub
# call therefore blew the outer bound on its own, the process was killed
# mid-loop, and every obligation already polled was thrown away: the tick saw
# no fields at all and could not distinguish that from "nothing is wrong".
#
# The fix is a WALL BUDGET the gate honours ITSELF, so it always returns under
# its own power with a typed result instead of being killed with none. Measured
# 2026-08-07 on the shared dev host: the work is ~1.7s of user CPU; the tail is blocking
# network wait (a 31.9s run showed 6144 voluntary context switches against ~600
# on fast runs, with flat CPU). So the budget is not a guess about how long the
# work takes -- it is the outer bound minus measured startup and the
# print_status tail, and the gate reports how much of the plan it completed.
DEFAULT_WATCH_GATE_BUDGET_SECS = float(
    os.environ.get("CI_HUB_WATCH_GATE_BUDGET", "20")
)
DEFAULT_WATCH_CANCEL_WAIT_SECS = float(
    os.environ.get("CI_HUB_WATCH_CANCEL_WAIT", "10")
)
# The operational-health taxonomy reserves 3 for an unavailable answer.  A
# watcher that exhausted its bound did not complete the census, so it is a
# NO-RESULT -- neither CLEAR nor a product/remediation failure.
WATCH_EXIT_NO_RESULT = 3
TERMINAL_VERIFICATION_STATES = frozenset(("green", "red", "error"))
# Only a genuine failing ANSWER remediates. A no_result (a cancelled hosted run,
# an OOM-killed local validate, a lost runner, a network error reaching GitHub)
# is the ABSENCE of an answer, never a failing one: it is a hole to RE-DISPATCH,
# never a red to revert on. Misreading a no_result as red once put an automated
# revert of a healthy main tip one step away (task
# obligation-path-must-consume-no-result-taxonomy).
REMEDIATION_STATES = frozenset(("red",))
# A local validate exit code is not a truth value. Exit 0 is the only passing
# answer; a validate process the ENVIRONMENT killed — OOM/SIGKILL (137),
# SIGTERM (143), or any signal (a negative subprocess returncode) — never
# delivered a verdict at all. That is a no_result, exactly like a cancelled
# hosted run.
_LOCAL_INFRA_EXITS = frozenset((137, 143))
DEFAULT_LOCAL_REDISPATCH_LIMIT = int(
    os.environ.get("CI_HUB_LOCAL_REDISPATCH_LIMIT", "2")
)
LAUNCH_REGISTRATION_TIMEOUT = float(
    os.environ.get("CI_HUB_LAUNCH_REGISTRATION_TIMEOUT", "10")
)
# Optional flags this producer would like to hand the target's validate driver.
# Each is passed ONLY when that driver says it accepts it.  An obligation runs
# the driver AT THE LANDED COMMIT, which can predate a flag by minutes:
# hermit 649cee3a landed EIGHT MINUTES before 36a84ddce added `--no-label-pr`,
# and passing it unconditionally made that obligation permanently
# undischargeable -- the driver exited on `unknown option` in ~5s, before a
# single test ran, so no re-run could ever have helped.
# ADD FUTURE FLAGS HERE AND NOTHING ELSE CHANGES: the probe is generic, so the
# next flag added to the driver does not need another fix in this file.
OPTIONAL_DRIVER_FLAGS: tuple[str, ...] = ("--no-label-pr",)
DRIVER_PROBE_TIMEOUT = float(os.environ.get("CI_HUB_DRIVER_PROBE_TIMEOUT", "120"))
LOCAL_RECEIPT_AUTHORITY = ROOT / "ci-hub/ci-hub"
LOCAL_RECEIPT_CANONICALIZATION = "serde_json::to_vec(HistoryRow)-v1"
LOCAL_RECEIPT_COUNT_DERIVATION = (
    "selected_tests=executed_tests;discovered_tests=executed_tests+filtered_tests"
)
LOCAL_SCHEMA4_COVERAGE_BASIS = "legacy-schema4-full-gates-and-aggregate-counts"
LOCAL_DECLARED_COVERAGE_BASIS = "declared-per-node"
LOCAL_SCHEMA4_COVERAGE_STATUS = "grandfathered-unknown"
LOCAL_DECLARED_COVERAGE_STATUS = "satisfied"
LOCAL_POLICY_SKIP_AUTHORITY = "ci-hub-local-receipt-policy-v1"

# A clean nonzero exit is NOT automatically a failing answer. Tonight three
# separate ENVIRONMENTAL failures — a sandbox EPERM on a re-validate, a
# BpfJailer denial of a `.o.d` write inside a DAG step, and a cold-build linker
# flake with zero tests run — each exited clean-nonzero and read as a product
# RED, and each was one automated step from reverting a healthy tip. The
# genuine failure was in the HARNESS, not the landed code.
#
# THE DERIVED DISCRIMINATOR (task cancellation_taxonomy_distinguish_self). The
# earlier attempt enumerated environmental root causes and defaulted every other
# nonzero exit to RED. That is precisely "a hardcoded list of a growing set":
# each NEW environmental wording we had not yet listed fell through to red and
# became one automated step from reverting a healthy tip — the exact failure
# mode that bit us five times tonight. So we DERIVE instead of enumerate, and we
# put the UNKNOWN on the safe side:
#
#   only a genuine, product-attributable failing TEST VERDICT makes the LOCAL
#   leg red. Every other nonzero exit — a build/link error, a sandbox denial, a
#   proxy drop, a disk-full, or an unrecognised failure we have never seen — is
#   a no_result: re-dispatch and (if it reproduces) fix the box, never revert
#   the tip on a local leg alone.
#
# This is sound, not merely cautious: a speculative land already COMPILED and
# TESTED the tip before arming, so a nonzero re-validate that is NOT a fresh
# failing test is overwhelmingly the environment (cold cache, sandbox, flake),
# not a regression in the landed code. And a real regression the environment did
# NOT cause surfaces on the GitHub leg too, which is authoritative and reverts on
# its own. The only "list" that remains is _TEST_FAILURE_MARKERS, and it lives
# in the SAFE direction: a marker we forget only downgrades a would-be red to a
# re-dispatch (recoverable), it can never manufacture a revert. _INFRA_SIGNATURE
# is retained ONLY to LABEL which box to fix; it no longer decides red vs
# no_result, so a missing category costs a precise log line, never a false red.
_TEST_FAILURE_MARKERS = (
    "test result: failed",  # cargo: printed only when >0 tests failed
    "error: test failed",  # cargo test-harness wrapper on a failing suite
    "failures:",  # cargo lists the failing test names under this header
    "tests failed",
    "assertion failed",
    "assertionerror",
    "panicked at",  # a rust test panic
)
_TEST_FAILURE_COUNT_RE = re.compile(r"\b([1-9]\d*)\s+failed\b")  # pytest "3 failed"
# safe-ci-dag-runner's OWN closing summary, e.g.
#   safe-ci-dag-runner: FAIL - 8 passed, 1 failed, 0 aborted, 39 skipped in 234.1s
# Its "N failed" counts DAG NODES, not tests, so _TEST_FAILURE_COUNT_RE must not
# read it as a test verdict.  A node can be "failed" because it TIMED OUT, was
# cancelled, or was killed -- none of which is a product test failure, and the
# measured instance was a node frozen in do_signal_stop for ~210s that completes
# in 54-58s unfrozen.  Matched as a whole line so only the aggregate is removed;
# genuine runner output on every other line is still read.
_DAG_RUNNER_AGGREGATE_RE = re.compile(
    r"(?mi)^[^\n]*\bsafe-ci-dag-runner:\s*(?:fail|ok|pass)\b[^\n]*$"
)
# These forms bind the failure to a product test suite rather than to the DAG's
# aggregate ``N failed`` node count.  They are intentionally narrower than
# _has_test_failures: when an unrelated build marker is also present, only a
# concrete named-test or test-runner verdict is strong enough to win.
_NAMED_TEST_FAILURE_RE = re.compile(r"(?mi)^test\s+.+?\s+\.\.\.\s+failed\s*$")
_PYTEST_FAILURE_SUMMARY_RE = re.compile(
    r"(?mi)^=+[^\n]*\b[1-9]\d*\s+failed\b[^\n]*=+\s*$"
)
# A BUILD-PHASE failure — a cargo build-script panic or a "failed to run custom
# build command" — is NOT a product test verdict, even though the DAG runner
# renders it with the very same "N failed" / "panicked at" vocabulary a failing
# test uses. It is a hole to re-dispatch (a cold-toolchain build-script flake
# reproduces every cold run), never a tip to revert. It is recognised ahead of
# ambiguous panic/count markers, but not ahead of an independent explicit test-
# runner verdict. Incident: a reverie-dbi/build.rs:339 cold-build panic,
# rendered "0 passed, 1 failed ... panicked at .../build.rs", was read as a
# test-failure and armed a revert of a healthy tip (obligation
# 20260804-025419-0f891e43). A build.rs panic means the crate never built, so no
# product test in that node could have run; a genuine regression the build did not
# cause still reverts via the authoritative GitHub leg, which compiles cleanly.
_BUILD_SCRIPT_FAILURE_MARKERS = (
    "failed to run custom build command for",  # cargo build-script failure line
)
_BUILD_SCRIPT_PANIC_RE = re.compile(r"panicked at [^\n]*build\.rs")
# Build helpers can fail after the DAG node has started but before the named
# product test runs.  The DAG summary still renders these as ``N failed``, which
# is test-looking vocabulary; the concrete operation is the binding.  These are
# the two verbatim DynamoRIO failures carried by obligation 3801a7df.
_BUILD_TOOL_FAILURE_MARKERS = (
    "failed to build and install dynamorio",
    "failed to configure dynamorio",
)
# An inner-MemoryMax OOM inside a boxed DAG node — the safe-ci-dag-runner reaped a
# step because it crossed its cgroup memory.max — is an ENVIRONMENTAL cap breach,
# not a product test verdict, even when its victim happens to be a test process and
# the node then surfaces "N failed"/"panicked at". It reproduces every run at the
# same cap, so it is a hole to re-dispatch (and fix-forward by RAISING the cap),
# never a tip to revert. It is recognised BEFORE the build-script and test-verdict
# markers so neither shared vocabulary can manufacture a false red, and because
# "raise this node's cap" is the actionable label even when the OOM also panicked a
# build.rs. BLAST-RADIUS CAVEAT (see the task note): the runner does not set
# memory.oom.group, so the kernel may kill an innocent process INSIDE the breaching
# node rather than the allocator that crossed the cap — attribution is sound at
# DAG-NODE granularity, NOT per-process. Do not blame the named victim; the reason
# points at the node's cap, which is the correct fix. Exact runner strings:
# model.rs `OOM-KILLED (hit inner MemoryMax; N oom_kill event(s))` and scheduler.rs
# `MEMORY CAP HIT: OOM-killed at its inner cgroup MemoryMax`.
_INNER_MEMORYMAX_OOM_MARKERS = (
    "oom-killed (hit inner memorymax",  # model.rs step reason
    "oom-killed at its inner cgroup memorymax",  # scheduler.rs cap-hit banner
)
# Diagnostic-only labels for a no_result. Grouped by CAUSE, but NON-load-bearing:
# they name which box to fix in the log, they do NOT gate red vs no_result, so
# this set can be incomplete without ever producing a false red.
_INFRA_SIGNATURE_CATEGORIES: tuple[tuple[str, tuple[str, ...]], ...] = (
    (
        "sandbox-denied",  # 3pai BpfJailer / seccomp / permission denial
        (
            "operation not permitted",
            "permission denied",
            "eperm",
            "eacces",
            "bpfjailer",
            "blocked by seccomp",
            "seccomp",
        ),
    ),
    (
        "network-proxy",  # the required proxy / network dropped mid-fetch
        (
            "could not resolve host",
            "connection reset by peer",
            "connection timed out",
            "failed to connect",
            "proxyconnect",
            "temporary failure in name resolution",
            "network is unreachable",
        ),
    ),
    (
        "disk-exhausted",  # ran out of disk / inodes / memory under load
        (
            "no space left on device",
            "disk quota exceeded",
            "cannot allocate memory",
        ),
    ),
    (
        "cold-build-flake",  # a cold-toolchain link/archive race, zero tests run
        (
            "collect2: error",
            "ld returned 1 exit status",
            "ld: cannot",
            "error adding symbols",
            "clang: error: unable to execute command",
        ),
    ),
)


def _has_test_failures(output: str) -> bool:
    """Whether the validate output shows at least one genuine test failure.

    Over-inclusive on purpose: a false positive here only forces a genuine RED
    (revertable), never swallows one, so unusual failure formatting fails safe.

    THE ONE THING IT MUST NOT COUNT is safe-ci-dag-runner's own aggregate, whose
    "N failed" counts DAG NODES.  A node is "failed" when it times out, is
    cancelled, or is killed, so counting it here manufactures a PRODUCT red from
    an infrastructure event.  Measured 2026-08-08 on obligation
    20260809-012006-649cee3a8339-dad6e4: a log with ZERO test-failure markers
    classified ("red", "test-failure") purely on the literal text "1 failed",
    where the failing node was a TIMEOUT caused by a frozen cgroup.
    The sibling _has_explicit_product_test_failure already documents excluding
    this exact text "because it may count DAG nodes"; this catch-all was
    re-admitting what the strong predicate deliberately threw out.

    Stripping is line-scoped, so this stays over-inclusive everywhere else: a
    genuine "3 failed" from a test runner on any other line still counts.
    """
    lowered = output.lower()
    if any(marker in lowered for marker in _TEST_FAILURE_MARKERS):
        return True
    without_node_aggregate = _DAG_RUNNER_AGGREGATE_RE.sub("", lowered)
    return _TEST_FAILURE_COUNT_RE.search(without_node_aggregate) is not None


def _has_explicit_product_test_failure(output: str) -> bool:
    """Whether a test runner itself emitted a strong failing verdict.

    Raw combined logs can contain failures from independent DAG nodes.  A
    DynamoRIO/build marker therefore cannot globally mask an explicit libtest,
    named-test, or pytest verdict elsewhere in the same run.  Bare aggregate
    ``N failed`` text is deliberately excluded because it may count DAG nodes.
    The persisted canonical FAILED/3 receipt remains the final typed authority.
    """
    lowered = output.lower()
    return (
        "test result: failed" in lowered
        or _NAMED_TEST_FAILURE_RE.search(output) is not None
        or _PYTEST_FAILURE_SUMMARY_RE.search(output) is not None
    )


def _infra_signature(output: str) -> str | None:
    """Best-effort ROOT-CAUSE label for a no_result, or None if unrecognised.

    Diagnostic only: it decides nothing. A None just yields a generic
    "non-test-failure" label; it never turns a no_result into a red.
    """
    lowered = output.lower()
    for category, needles in _INFRA_SIGNATURE_CATEGORIES:
        if any(needle in lowered for needle in needles):
            return category
    return None


def _is_inner_memorymax_oom(output: str) -> bool:
    """Whether a boxed DAG node was OOM-reaped at its inner cgroup memory.max.

    A cap breach is environmental (raise the cap), not a product test verdict —
    even when the reaped victim is a test process and the node then renders
    "N failed"/"panicked at". Recognising it keeps that shared vocabulary from
    manufacturing a false red; attribution is sound only at DAG-NODE granularity.
    """
    lowered = output.lower()
    return any(marker in lowered for marker in _INNER_MEMORYMAX_OOM_MARKERS)


def _is_build_phase_failure(output: str) -> bool:
    """Whether the failure originates in the BUILD phase, not a product test.

    A cargo build-script panic (``panicked at .../build.rs``) or a cargo
    "failed to run custom build command" is a build-layer flake, not a test
    verdict — the DAG runner just renders it with the same vocabulary. Recognising
    it here keeps that shared vocabulary from ever manufacturing a false red.
    """
    lowered = output.lower()
    if any(marker in lowered for marker in _BUILD_SCRIPT_FAILURE_MARKERS):
        return True
    return _BUILD_SCRIPT_PANIC_RE.search(lowered) is not None


def _is_build_tool_failure(output: str) -> bool:
    """Whether a named external build/configure step failed before tests ran."""
    lowered = output.lower()
    return any(marker in lowered for marker in _BUILD_TOOL_FAILURE_MARKERS)


def _classify_local(exit_code: int, output: str = "") -> tuple[str, str]:
    """Map a local validate (exit_code, output) to (state, human reason).

    The discriminator is DERIVED, not enumerated: only a genuine failing test
    VERDICT is red; every other nonzero exit — build/link error, sandbox denial,
    proxy drop, disk-full, or an unrecognised failure — is a no_result to
    re-dispatch, so an unknown failure mode can never manufacture a revert.
    """
    if exit_code == 0:
        # A clean exit is a PROXY for "the tests passed"; the executed count the
        # run itself printed is the fact. A validate that exited 0 having run a
        # DEMONSTRABLY zero test count (every libtest banner reported 0 — the
        # `--features` gating shape that compiles the target but excludes its
        # tests) is a no-result wearing a success badge, not a pass. It is a hole
        # to RE-DISPATCH (never a red to revert): downgrading here can only turn a
        # provably inert green into a recoverable re-dispatch, never manufacture a
        # revert. Absence of any banner stays green — is_zero_test_green fires only
        # on positive evidence of zero-ness, so an unreadable/banner-less log is
        # never downgraded.
        if is_zero_test_green(output):
            return "no_result", "zero-test-green"
        return "green", "clean exit"
    if exit_code < 0 or exit_code in _LOCAL_INFRA_EXITS:
        return "no_result", f"environment-killed (exit={exit_code})"
    if _is_inner_memorymax_oom(output):
        # A boxed node reaped at its inner cgroup memory.max is an environmental
        # cap breach (fix-forward = raise the cap), NOT a failing test verdict —
        # even when the reaped victim is a test process and the node then rendered
        # "N failed"/"panicked at". Recognised BEFORE build-script/test markers so
        # neither shared vocabulary can manufacture a revert: re-dispatch, never revert.
        return "no_result", "non-test-failure:inner-memorymax-oom"
    if _has_explicit_product_test_failure(output):
        # Combined DAG output may also mention an unrelated build-node failure.
        # A concrete test-runner verdict is the stronger causal binding and must
        # not be globally masked by that build marker.  Canonical FAILED/3
        # receipt verification still decides whether this provisional red may
        # drive remediation.
        return "red", "test-failure"
    if _is_build_phase_failure(output):
        # A build-script panic / "failed to run custom build command" surfaced
        # through the DAG runner's "N failed" / "panicked at" summary is a
        # build-layer flake, NOT a failing test verdict: re-dispatch, never revert.
        return "no_result", "non-test-failure:build-script"
    if _is_build_tool_failure(output):
        # The runner's aggregate ``N failed`` counts DAG nodes, not product
        # tests.  Without an explicit product-test verdict, the named
        # configure/install operation binds the observed failure to the build
        # layer, so remediation must repair/re-dispatch the box.
        return "no_result", "non-test-failure:build-tool"
    if _has_test_failures(output):
        return "red", "test-failure"
    # Nonzero, but no product test rendered a failing verdict: the failure came
    # from the build/harness/sandbox layer (or is simply unrecognised). Either
    # way it is a hole to re-dispatch, never a tip to revert. GitHub stays
    # authoritative for a genuine regression the environment did not cause.
    category = _infra_signature(output) or "unclassified"
    return "no_result", f"non-test-failure:{category}"


def _local_state(exit_code: int, output: str = "") -> str:
    return _classify_local(exit_code, output)[0]


def _read_local_output(log_path: Path, offset: int, *, cap: int = 64_000_000) -> str:
    """Best-effort read of this validate run's output for classification.

    The validate subprocess streams to the same log the watcher opened, so the
    output from `offset` (captured just before validate started) to end is this
    run's. A read failure yields "" (no test-failure verdict visible), which
    leaves a nonzero exit as a no_result to re-dispatch — the safe state that
    never manufactures a revert from an output we could not even read.
    """
    try:
        with log_path.open("r", encoding="utf-8", errors="replace") as handle:
            handle.seek(max(0, offset))
            return handle.read(cap)
    except OSError:
        return ""


class ProtocolError(RuntimeError):
    """The dual-verification protocol could not be armed or polled."""


def _local_receipt_problem(
    report: object,
    *,
    repo: str,
    sha: str,
    returncode: int,
) -> str | None:
    """Validate the envelope returned by the sole receipt authority.

    The semantic predicate itself deliberately remains single-sourced in
    ``ci-hub validate-status``.  This consumer proves that it invoked that
    authority for the exact SHA and received a non-vacuous dereferenced
    qualifying record instead of trusting ``validate.sh``'s process exit.
    """
    if returncode != 0:
        return f"canonical verifier exited {returncode}"
    if repo != DEFAULT_REPO:
        return (
            f"canonical local receipt authority is bound to {DEFAULT_REPO}, not {repo}"
        )
    if not isinstance(report, Mapping):
        return "canonical verifier report is not an object"
    if type(report.get("schema_version")) is not int or report["schema_version"] != 1:
        return "canonical verifier report schema is unsupported"
    if report.get("repo") != repo:
        return "canonical verifier report is not bound to the repository"
    if report.get("sha") != sha:
        return "canonical verifier report is not bound to the landed SHA"
    if (
        report.get("verdict") != "VALIDATED"
        or type(report.get("exit_code")) is not int
        or report["exit_code"] != 0
    ):
        return "canonical verifier did not return VALIDATED/0"
    qualifying_count = report.get("qualifying_count")
    if type(qualifying_count) is not int or qualifying_count <= 0:
        return "canonical verifier reported no qualifying counted receipt"
    disqualified_count = report.get("disqualified_count")
    if type(disqualified_count) is not int or disqualified_count < 0:
        return "canonical verifier reported an invalid disqualified count"
    newest = report.get("newest_qualifying")
    if not isinstance(newest, Mapping) or not newest:
        return "canonical verifier did not dereference a qualifying receipt"
    qualifying_receipts = report.get("qualifying_receipts")
    if (
        not isinstance(qualifying_receipts, list)
        or len(qualifying_receipts) != qualifying_count
        or newest not in qualifying_receipts
    ):
        return "canonical verifier did not carry its complete qualifying receipt set"
    if newest.get("repo") != repo:
        return "qualifying receipt is not repository-bound"
    if newest.get("sha") != sha or newest.get("commit") != sha:
        return "qualifying receipt is not exact-SHA-bound"
    schema_version = newest.get("schema_version")
    if type(schema_version) is not int or schema_version < 4:
        return "qualifying receipt schema is unsupported"
    tree = newest.get("tree")
    if not isinstance(tree, str) or not obligations.SHA_RE.fullmatch(tree):
        return "qualifying receipt has no exact tree identity"
    if (
        newest.get("commit_anchored") is not True
        or newest.get("tree_dirty") is not False
    ):
        return "qualifying receipt is not clean and commit-anchored"
    if newest.get("profile") != "full" or newest.get("selection_mode") != "full":
        return "qualifying receipt is not full/full"
    if (
        newest.get("result") != "pass"
        or newest.get("raw_result") != "pass"
        or type(newest.get("exit_code")) is not int
        or newest["exit_code"] != 0
        or type(newest.get("failures")) is not int
        or newest["failures"] != 0
    ):
        return "qualifying receipt does not carry a zero-failure pass"

    checks = newest.get("checks")
    gates_run = newest.get("gates_run")
    gates_expected = newest.get("gates_expected")
    gates = newest.get("gates")
    if (
        type(checks) is not int
        or type(gates_run) is not int
        or type(gates_expected) is not int
        or gates_expected <= 0
        or gates_run < gates_expected
        or checks != gates_run
        or not isinstance(gates, list)
        or gates_run != len(gates)
    ):
        return "qualifying receipt has inconsistent gate coverage"
    for gate in gates:
        if (
            not isinstance(gate, Mapping)
            or not isinstance(gate.get("name"), str)
            or not gate["name"].strip()
            or gate.get("result") != "pass"
            or type(gate.get("exit_code")) is not int
            or gate["exit_code"] != 0
        ):
            return "qualifying receipt has a non-passing or unidentified gate"

    executed = newest.get("executed_tests")
    filtered = newest.get("filtered_tests")
    selected = newest.get("selected_tests")
    discovered = newest.get("discovered_tests")
    if (
        type(executed) is not int
        or type(filtered) is not int
        or type(selected) is not int
        or type(discovered) is not int
        or executed <= 0
        or filtered < 0
        or selected != executed
        or discovered != executed + filtered
        or newest.get("count_derivation") != LOCAL_RECEIPT_COUNT_DERIVATION
    ):
        return "qualifying receipt has invalid or unbound test counts"

    coverage = newest.get("coverage")
    coverage_basis = newest.get("coverage_basis")
    if coverage_basis == LOCAL_SCHEMA4_COVERAGE_BASIS:
        if (
            schema_version != 4
            or coverage is not None
            or newest.get("coverage_satisfied") is not None
            or newest.get("coverage_status") != LOCAL_SCHEMA4_COVERAGE_STATUS
        ):
            return "legacy coverage basis is not bound to a schema-4 receipt"
    elif coverage_basis == LOCAL_DECLARED_COVERAGE_BASIS:
        if (
            newest.get("coverage_satisfied") is not True
            or newest.get("coverage_status") != LOCAL_DECLARED_COVERAGE_STATUS
            or not isinstance(coverage, Mapping)
        ):
            return "declared coverage basis has no coverage record"
        if (
            type(coverage.get("planned_test_nodes")) is not int
            or coverage["planned_test_nodes"] <= 0
            or coverage.get("zero_executed_nodes") != []
            or coverage.get("absent_nodes") != []
        ):
            return "declared per-node coverage is incomplete"
    else:
        return "qualifying receipt has an unsupported coverage basis"

    for field in ("finished_at", "host", "slot", "log_file"):
        if not isinstance(newest.get(field), str) or not newest[field].strip():
            return f"qualifying receipt has no durable {field}"
    identity = newest.get("receipt_identity")
    if not isinstance(identity, Mapping):
        return "qualifying receipt has no receipt identity"
    digest = identity.get("digest")
    if (
        identity.get("digest_algorithm") != "sha256"
        or identity.get("canonicalization") != LOCAL_RECEIPT_CANONICALIZATION
        or not isinstance(digest, str)
        or re.fullmatch(r"[0-9a-f]{64}", digest) is None
    ):
        return "qualifying receipt has an invalid canonical digest"
    identity_tuple = identity.get("tuple")
    expected_tuple = {
        "repo": repo,
        "sha": sha,
        "tree": tree,
        "finished_at": newest["finished_at"],
        "host": newest["host"],
        "slot": newest["slot"],
        "log_file": newest["log_file"],
    }
    if identity_tuple != expected_tuple:
        return "qualifying receipt identity tuple does not match its evidence"
    if not isinstance(report.get("ledger"), str) or not report["ledger"]:
        return "canonical verifier report has no ledger provenance"
    return None


def _local_failed_receipt_problem(
    report: object,
    *,
    repo: str,
    sha: str,
    returncode: int,
) -> str | None:
    """Validate the canonical authority's distinct known-failing answer."""
    if returncode != 3:
        return f"canonical verifier did not exit 3 for FAILED (exit={returncode})"
    if repo != DEFAULT_REPO:
        return (
            f"canonical local receipt authority is bound to {DEFAULT_REPO}, not {repo}"
        )
    if not isinstance(report, Mapping):
        return "canonical failed report is not an object"
    if type(report.get("schema_version")) is not int or report["schema_version"] != 1:
        return "canonical failed report schema is unsupported"
    if report.get("repo") != repo or report.get("sha") != sha:
        return "canonical failed report is not bound to the repository and SHA"
    if report.get("verdict") != "FAILED" or report.get("exit_code") != 3:
        return "canonical verifier did not return FAILED/3"
    if report.get("qualifying_count") != 0 or report.get("newest_qualifying") is not None:
        return "canonical failed report also claims a qualifying receipt"
    if report.get("qualifying_receipts") != []:
        return "canonical failed report has a nonempty qualifying receipt set"
    failed_count = report.get("failed_record_count")
    if type(failed_count) is not int or failed_count <= 0:
        return "canonical failed report has no counted failing receipt"
    if not isinstance(report.get("ledger"), str) or not report["ledger"]:
        return "canonical failed report has no ledger provenance"
    return None


def _persisted_local_receipt_problem(
    evidence: object, *, repo: str, sha: str
) -> str | None:
    if not isinstance(evidence, Mapping) or evidence.get("state") != "verified":
        return "persisted local receipt is not verified evidence"
    command = evidence.get("command")
    expected_command = [
        str(LOCAL_RECEIPT_AUTHORITY),
        "validate-status",
        "--sha",
        sha,
        "--repo",
        repo,
        "--json",
    ]
    if command != expected_command or evidence.get("repo") != repo:
        return "persisted local receipt is not bound to the requested repo and SHA"
    report = evidence.get("report")
    returncode = evidence.get("returncode")
    if type(returncode) is not int:
        return "persisted local receipt has no integer verifier return code"
    report_problem = _local_receipt_problem(
        report, repo=repo, sha=sha, returncode=returncode
    )
    if report_problem is not None:
        return report_problem
    if not isinstance(report, Mapping):
        return "persisted local receipt has no canonical verifier report"
    canonical = json.dumps(report, sort_keys=True, separators=(",", ":")).encode()
    if evidence.get("report_sha256") != hashlib.sha256(canonical).hexdigest():
        return "persisted local receipt report hash does not match its cached report"
    return None


def _persisted_local_failed_receipt_problem(
    evidence: object, *, repo: str, sha: str
) -> str | None:
    if not isinstance(evidence, Mapping) or evidence.get("state") != "failed":
        return "persisted local receipt is not canonical failed evidence"
    command = evidence.get("command")
    expected_command = [
        str(LOCAL_RECEIPT_AUTHORITY),
        "validate-status",
        "--sha",
        sha,
        "--repo",
        repo,
        "--json",
    ]
    if command != expected_command or evidence.get("repo") != repo:
        return "persisted failed receipt is not bound to the requested repo and SHA"
    report = evidence.get("report")
    returncode = evidence.get("returncode")
    if type(returncode) is not int:
        return "persisted failed receipt has no integer verifier return code"
    report_problem = _local_failed_receipt_problem(
        report, repo=repo, sha=sha, returncode=returncode
    )
    if report_problem is not None:
        return report_problem
    assert isinstance(report, Mapping)
    canonical = json.dumps(report, sort_keys=True, separators=(",", ":")).encode()
    if evidence.get("report_sha256") != hashlib.sha256(canonical).hexdigest():
        return "persisted failed receipt report hash does not match its cached report"
    return None


def _dereference_current_local_receipt(
    repo: str, sha: str
) -> tuple[bool, dict[str, Any]]:
    """Call the one canonical exact-SHA local receipt verifier."""
    return verify_local_receipt(repo, sha)


def _compare_persisted_local_receipt(
    evidence: object, *, repo: str, sha: str
) -> tuple[bool, dict[str, Any], str | None]:
    """Compare cached evidence with a fresh canonical selected receipt.

    ``report_sha256`` only detects corruption of the cached outer report.  It is
    not keyed authority: a writer can change that report and recompute the hash.
    Every green/durability consumer therefore dereferences ``validate-status``
    again and requires the exact selected receipt object (including its
    canonical digest and bound fields) to match the persisted selection.
    """
    persisted_problem = _persisted_local_receipt_problem(evidence, repo=repo, sha=sha)
    current_verified, current = _dereference_current_local_receipt(repo, sha)
    if persisted_problem is not None:
        return False, current, persisted_problem
    if not current_verified:
        return (
            False,
            current,
            "fresh canonical local receipt refused: "
            f"{current.get('reason') or 'unknown reason'}",
        )
    current_problem = _persisted_local_receipt_problem(current, repo=repo, sha=sha)
    if current_problem is not None:
        return (
            False,
            current,
            f"fresh canonical local receipt is invalid: {current_problem}",
        )

    assert isinstance(evidence, Mapping)
    persisted_report = evidence["report"]
    current_report = current["report"]
    assert isinstance(persisted_report, Mapping)
    assert isinstance(current_report, Mapping)
    persisted_selected = persisted_report.get("newest_qualifying")
    current_receipts = current_report.get("qualifying_receipts")
    if not isinstance(persisted_selected, Mapping) or not isinstance(
        current_receipts, list
    ):
        return False, current, "selected receipt comparison has no receipt object"
    current_selected = next(
        (
            candidate
            for candidate in current_receipts
            if isinstance(candidate, Mapping) and candidate == persisted_selected
        ),
        None,
    )
    if current_selected is None:
        return (
            False,
            current,
            "persisted selected receipt is absent from the fresh canonical "
            "qualifying receipt set",
        )
    persisted_identity = persisted_selected.get("receipt_identity")
    current_identity = current_selected.get("receipt_identity")
    if not isinstance(persisted_identity, Mapping) or not isinstance(
        current_identity, Mapping
    ):
        return False, current, "selected receipt comparison has no canonical digest"
    if persisted_identity.get("digest") != current_identity.get("digest"):
        return False, current, "selected receipt canonical digest changed"
    return True, current, None


def _persisted_local_receipt_valid(evidence: object, *, repo: str, sha: str) -> bool:
    matches, _current, _problem = _compare_persisted_local_receipt(
        evidence, repo=repo, sha=sha
    )
    return matches


def verify_local_receipt(
    repo: str,
    sha: str,
) -> tuple[bool, dict[str, Any]]:
    """Dereference and persist the canonical counted exact-SHA receipt verdict."""
    if not obligations.SHA_RE.fullmatch(sha):
        raise ProtocolError(f"invalid local receipt SHA {sha!r}")
    command = [
        str(LOCAL_RECEIPT_AUTHORITY),
        "validate-status",
        "--sha",
        sha,
        "--repo",
        repo,
        "--json",
    ]
    checked_at = obligations.utc_now()
    try:
        result = _run(tuple(command), check=False, timeout=DEFAULT_NETWORK_TIMEOUT)
    except ProtocolError as error:
        return False, {
            "state": "error",
            "authority": "ci-hub-validate-status",
            "repo": repo,
            "command": command,
            "checked_at": checked_at,
            "returncode": None,
            "report": None,
            "report_sha256": None,
            "reason": str(error),
        }
    try:
        parsed = json.loads(result.stdout)
    except json.JSONDecodeError:
        parsed = None
    problem = _local_receipt_problem(
        parsed, repo=repo, sha=sha, returncode=result.returncode
    )
    failed_problem = _local_failed_receipt_problem(
        parsed, repo=repo, sha=sha, returncode=result.returncode
    )
    canonical = (
        json.dumps(parsed, sort_keys=True, separators=(",", ":")).encode()
        if isinstance(parsed, Mapping)
        else None
    )
    if problem is None:
        evidence_state = "verified"
        evidence_reason = None
    elif failed_problem is None:
        evidence_state = "failed"
        evidence_reason = "canonical verifier returned FAILED/3"
    else:
        evidence_state = "refused"
        evidence_reason = problem
    evidence = {
        "state": evidence_state,
        "authority": "ci-hub-validate-status",
        "repo": repo,
        "command": command,
        "checked_at": checked_at,
        "returncode": result.returncode,
        "report": dict(parsed) if isinstance(parsed, Mapping) else None,
        "report_sha256": hashlib.sha256(canonical).hexdigest() if canonical else None,
        "reason": evidence_reason,
    }
    return problem is None, evidence


def _local_policy_skip_patch(repo: str) -> dict[str, Any]:
    """Persist that a registered hosted-only repo has no local producer.

    This is not an authorization skip: the local leg remains ``no_result`` and
    only the repository's non-vacuous exact-head hosted policy can turn the
    obligation green.  Keeping the fact explicit prevents a missing local
    receipt from being mistaken for either a pass or an unclassified hole.
    """
    if repo not in {REVERIE_REPO, PARENT_REPO}:
        raise ProtocolError(f"no local receipt skip policy exists for {repo}")
    parent = repo == PARENT_REPO
    recorded_at = obligations.utc_now()
    return {
        "state": "no_result",
        "started_at": None,
        "finished_at": None,
        "pid": None,
        "launch_token": None,
        "registered_at": None,
        "receipt_verification": None,
        "classification_reason": (
            "local-receipt-not-applicable-hosted-authority-required"
            if parent
            else "local-receipt-policy-skipped"
        ),
        "policy_skip": {
            "schema_version": 1,
            "authority": LOCAL_POLICY_SKIP_AUTHORITY,
            "repo": repo,
            "canonical_repo": DEFAULT_REPO,
            "outcome": "not_applicable" if parent else "skipped",
            "reason": (
                "parent-has-no-local-validate-producer;"
                "registered-exact-head-hosted-authority-required"
                if parent
                else "canonical-local-receipt-authority-unsupported-repository"
            ),
            "recorded_at": recorded_at,
        },
    }


def _local_policy_skip_valid(record: Mapping[str, Any]) -> bool:
    local = record.get("local")
    repo = record.get("repo")
    if repo == PARENT_REPO:
        classification_reason = "local-receipt-not-applicable-hosted-authority-required"
        outcome = "not_applicable"
        reason = (
            "parent-has-no-local-validate-producer;"
            "registered-exact-head-hosted-authority-required"
        )
    elif repo == REVERIE_REPO:
        classification_reason = "local-receipt-policy-skipped"
        outcome = "skipped"
        reason = "canonical-local-receipt-authority-unsupported-repository"
    else:
        return False
    if (
        not isinstance(local, Mapping)
        or local.get("state") != "no_result"
        or local.get("classification_reason") != classification_reason
    ):
        return False
    policy_skip = local.get("policy_skip")
    return (
        isinstance(policy_skip, Mapping)
        and type(policy_skip.get("schema_version")) is int
        and policy_skip["schema_version"] == 1
        and policy_skip.get("authority") == LOCAL_POLICY_SKIP_AUTHORITY
        and policy_skip.get("repo") == repo
        and policy_skip.get("canonical_repo") == DEFAULT_REPO
        and policy_skip.get("outcome") == outcome
        and policy_skip.get("reason") == reason
        and isinstance(policy_skip.get("recorded_at"), str)
        and bool(policy_skip["recorded_at"].strip())
        and local.get("pid") is None
        and local.get("launch_token") is None
        and local.get("registered_at") is None
        and local.get("started_at") is None
        and local.get("finished_at") is None
        and local.get("receipt_verification") is None
    )


def bind_local_receipt_authority(
    obligation_id: str, store_path: Path
) -> dict[str, Any]:
    """Bind the local leg to its repository's canonical receipt authority."""
    record = obligations.get_record(obligation_id, store_path)
    repo, sha = str(record["repo"]), str(record["landed_sha"])
    if repo != DEFAULT_REPO:
        if _local_policy_skip_valid(record):
            return record
        return obligations.transition(
            obligation_id,
            "local-policy-skipped",
            {"local": _local_policy_skip_patch(repo)},
            store_path,
        )
    local = record.get("local")
    if not isinstance(local, Mapping) or local.get("state") not in {"green", "red"}:
        return record
    if local.get("state") == "red":
        # Pre-binding legacy rows may have no receipt observation at all.  This
        # migration handles the concrete bug class here: a row that DID call the
        # authority and persisted its refused/failed answer.
        if not isinstance(local.get("receipt_verification"), Mapping):
            return record
        # A raw validate exit and test-looking DAG summary are not a durable red.
        # Re-dereference the canonical authority: only its counted FAILED/3
        # answer may preserve red.  Every refusal/error is a no-result hole.
        _accepted, current = _dereference_current_local_receipt(repo, sha)
        if _persisted_local_failed_receipt_problem(current, repo=repo, sha=sha) is None:
            if local.get("receipt_verification") == current:
                return record
            return obligations.transition(
                obligation_id,
                "local-failure-confirmed",
                {
                    "local": {
                        "state": "red",
                        "receipt_verification": current,
                        "classification_reason": "canonical-receipt-failed",
                    }
                },
                store_path,
            )
        if _persisted_local_receipt_problem(current, repo=repo, sha=sha) is None:
            return obligations.transition(
                obligation_id,
                "local-failure-superseded",
                {
                    "local": {
                        "state": "green",
                        "receipt_verification": current,
                        "classification_reason": "canonical-receipt-validated",
                    }
                },
                store_path,
            )
        return obligations.transition(
            obligation_id,
            "local-receipt-refused",
            {
                "local": {
                    "state": "no_result",
                    "receipt_verification": current,
                    "classification_reason": (
                        "canonical-receipt-refused:"
                        f"{current.get('reason') or 'unknown reason'}"
                    ),
                }
            },
            store_path,
        )
    matches, current, comparison_problem = _compare_persisted_local_receipt(
        local.get("receipt_verification"), repo=repo, sha=sha
    )
    if matches:
        return record
    patch: dict[str, Any] = {
        "receipt_verification": current,
        "state": "no_result",
        "classification_reason": (
            "persisted-receipt-refused:"
            f"{comparison_problem or 'unknown persisted/live mismatch'}"
        ),
    }
    return obligations.transition(
        obligation_id,
        "local-persisted-receipt-refused",
        {"local": patch},
        store_path,
    )


def verification_policy_for_repo(repo: str) -> dict[str, Any]:
    """Return the only supported exact-SHA verification policy for ``repo``.

    Repository identity is load-bearing: a workflow file/name from another
    repository is not a missing result, but an invalid verification request.
    Keep the policy versioned and persist it with each obligation so consumers
    observe the binding instead of inferring it from a global default.
    """
    try:
        schema_version = _CURRENT_VERIFICATION_POLICY_VERSION[repo]
        required_jobs = _VERSIONED_REPO_GITHUB_JOBS[(repo, schema_version)]
    except KeyError as error:
        supported = ", ".join(sorted(_CURRENT_VERIFICATION_POLICY_VERSION))
        raise ProtocolError(
            f"unsupported post-land verification repository {repo!r}; "
            f"supported repositories: {supported}"
        ) from error
    return {
        "schema_version": schema_version,
        "repo": repo,
        "github": {
            "required_jobs": [
                {
                    "workflow_file": workflow_file,
                    "workflow_name": workflow_name,
                    "job_name": job_name,
                }
                for workflow_file, workflow_name, job_name in required_jobs
            ],
            "required_positive_count": len(required_jobs),
        },
    }


def validate_verification_policy(
    policy: Mapping[str, Any],
) -> dict[str, Any]:
    repo = policy.get("repo")
    if not isinstance(repo, str):
        raise ProtocolError("verification policy has no repository identity")
    schema_version = policy.get("schema_version")
    if type(schema_version) is not int:
        raise ProtocolError("verification policy has no integer schema version")
    try:
        required_jobs = _VERSIONED_REPO_GITHUB_JOBS[(repo, schema_version)]
    except KeyError as error:
        raise ProtocolError(
            f"unsupported verification policy version {schema_version} for {repo!r}"
        ) from error
    expected = {
        "schema_version": schema_version,
        "repo": repo,
        "github": {
            "required_jobs": [
                {
                    "workflow_file": workflow_file,
                    "workflow_name": workflow_name,
                    "job_name": job_name,
                }
                for workflow_file, workflow_name, job_name in required_jobs
            ],
            "required_positive_count": len(required_jobs),
        },
    }
    if policy != expected:
        raise ProtocolError(f"invalid verification policy for {repo!r}")
    return expected


def _verification_policy_from_record(record: Mapping[str, Any]) -> dict[str, Any]:
    repo = record.get("repo")
    if not isinstance(repo, str):
        raise ProtocolError("obligation has no repository identity")
    persisted = record.get("verification_policy")
    if persisted is None:
        # Pre-policy legacy events are migrated once, append-only, using the
        # repository's current registered policy. New opened events never take
        # this path because create_obligation stores their policy atomically.
        return verification_policy_for_repo(repo)
    if not isinstance(persisted, Mapping):
        raise ProtocolError("obligation verification policy is not an object")
    policy = validate_verification_policy(persisted)
    if policy["repo"] != repo:
        raise ProtocolError(
            f"obligation {record.get('obligation_id', '<unknown>')} has a "
            f"verification policy that does not match {repo!r}"
        )
    return policy


def bind_verification_policy(
    obligation_id: str,
    store_path: Path,
    *,
    requested_policy: Mapping[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Persist the repository policy, migrating legacy obligations append-only."""
    record = obligations.get_record(obligation_id, store_path)
    repo = record.get("repo")
    if not isinstance(repo, str):
        raise ProtocolError("obligation has no repository identity")
    requested = (
        None
        if requested_policy is None
        else validate_verification_policy(requested_policy)
    )
    if requested is not None and requested["repo"] != repo:
        raise ProtocolError(
            "requested verification policy repository does not match obligation"
        )
    if record.get("verification_policy") is None:
        policy = requested or verification_policy_for_repo(repo)
        record = obligations.transition(
            obligation_id,
            "verification-policy-bound",
            {"verification_policy": policy},
            store_path,
        )
    else:
        policy = _verification_policy_from_record(record)
        if requested is not None and requested != policy:
            raise ProtocolError(
                "existing open obligation uses a different verification policy"
            )
    return record, policy


def _record_policy_investigation(
    record: Mapping[str, Any], error: ProtocolError, store_path: Path
) -> dict[str, Any]:
    if (
        record.get("overall_state") == "investigation_required"
        and record.get("failure_source") == "verification_policy"
    ):
        return dict(record)
    now = obligations.utc_now()
    summary = f"invalid persisted verification policy: {error}"
    updated = obligations.transition(
        str(record["obligation_id"]),
        "verification-policy-error",
        {
            "overall_state": "investigation_required",
            "first_terminal_at": record.get("first_terminal_at") or now,
            "failure_source": "verification_policy",
            "failure_summary": summary,
            "recommendation": None,
            "alert": {"state": "raised", "raised_at": now},
            "remediation": {"state": "not_required"},
            "github": {
                "state": "error",
                "finished_at": now,
                "last_poll_error": str(error),
            },
        },
        store_path,
    )
    print(
        f"HARD WARNING: obligation {record['obligation_id']} policy invalid: {error}",
        file=sys.stderr,
        flush=True,
    )
    return updated


def _percentile(values: Sequence[float], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("percentile requires at least one value")
    index = max(0, min(len(ordered) - 1, math.ceil(len(ordered) * fraction) - 1))
    return ordered[index]


def estimate_local_validate_cost(ledger_path: Path | None = None) -> dict[str, Any]:
    samples: list[tuple[float, float]] = []
    if ledger_path is None:
        try:
            records = canonical_ledger.rows(root=ROOT)
        except (OSError, ValueError, json.JSONDecodeError):
            records = []
    elif ledger_path.exists():
        try:
            records = [
                json.loads(line)
                for line in ledger_path.read_text().splitlines()
                if line.strip()
            ]
        except (OSError, json.JSONDecodeError):
            records = []
    else:
        records = []
    for record in records:
        try:
            wall = float(record.get("real_seconds") or 0)
            cpu = float(record.get("user_seconds") or 0) + float(
                record.get("sys_seconds") or 0
            )
        except (TypeError, ValueError):
            continue
        if record.get("profile") == "full" and wall > 0 and cpu >= 0:
            samples.append((wall, cpu))
    samples = samples[-50:]
    if samples:
        wall = _percentile([x[0] for x in samples], 0.9)
        cpu = _percentile([x[1] for x in samples], 0.9)
        basis = (
            f"derived from p90 of the last {len(samples)} usable successful "
            "full-profile validate ledger row(s), window capped at 50, "
            "mixed host/cache states"
        )
        return {
            "kind": "derived",
            "wall_seconds": round(wall, 3),
            "cpu_seconds": round(cpu, 3),
            "basis": basis,
        }
    return {
        "kind": "unknown",
        "wall_seconds": None,
        "cpu_seconds": None,
        "basis": "not measured: no usable successful full-profile validate ledger rows",
    }


def _run(
    command: Sequence[str],
    *,
    cwd: Path | None = None,
    check: bool = False,
    capture_output: bool = True,
    env: Mapping[str, str] | None = None,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(command),
            cwd=cwd,
            check=check,
            capture_output=capture_output,
            text=True,
            env=dict(env) if env is not None else None,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        raise ProtocolError(
            f"command timed out after {timeout:.1f}s: {' '.join(command)}"
        ) from error
    except (OSError, subprocess.CalledProcessError) as error:
        if isinstance(error, subprocess.CalledProcessError):
            detail = (error.stderr or error.stdout or "").strip()
        else:
            detail = str(error)
        raise ProtocolError(f"command failed: {' '.join(command)}: {detail}") from error


_GITHUB_PATH_COMPONENT = re.compile(r"[A-Za-z0-9_.-]+")


def _github_repo_from_remote(remote: str) -> str | None:
    """Return ``owner/repo`` only for an exact, structurally valid GitHub URL.

    Git's common SSH remote is scp-like rather than a URI, so it gets one fully
    anchored parser.  URI forms use ``urlsplit`` and bind the hostname itself;
    substring searches incorrectly accepted hosts such as ``evilgithub.com``.
    """
    value = remote.strip()
    scp = re.fullmatch(
        r"git@(?P<host>[^:/\s]+):(?P<path>[^?#\s]+)",
        value,
    )
    if scp is not None:
        if scp.group("host").lower() != "github.com":
            return None
        repo_path = scp.group("path")
    else:
        try:
            parsed = urlsplit(value)
            port = parsed.port
        except ValueError:
            return None
        if parsed.scheme not in {"https", "ssh"}:
            return None
        if parsed.hostname is None or parsed.hostname.lower() != "github.com":
            return None
        if parsed.query or parsed.fragment or parsed.password is not None:
            return None
        if parsed.scheme == "https":
            if parsed.username is not None or port not in (None, 443):
                return None
        elif parsed.username != "git" or port not in (None, 22):
            return None
        repo_path = parsed.path.removeprefix("/")

    repo_path = repo_path.removesuffix("/").removesuffix(".git")
    parts = repo_path.split("/")
    if len(parts) != 2 or not all(
        _GITHUB_PATH_COMPONENT.fullmatch(part) for part in parts
    ):
        return None
    return "/".join(parts)


def resolve_repo_source(repo: str, source: Path | None) -> Path:
    """Resolve a repository-specific donor checkout and prove its origin binding."""
    # Landing ancestry is a narrower authority than post-land CI policy. Parent
    # tooling and agent-utils both land directly to main and therefore need
    # ancestry verification, but neither has a hosted/local verification
    # obligation policy. Keeping these allowlists separate avoids manufacturing
    # a zero-job policy that would make an unsupported obligation look green:
    # `AGENT_UTILS_REPO` is in `_DEFAULT_REPO_SOURCES` but NOT in
    # `_CURRENT_VERIFICATION_POLICY_VERSION`, so `verification_policy_for_repo`
    # still refuses it ("unsupported post-land verification repository").
    if repo not in _DEFAULT_REPO_SOURCES:
        supported = ", ".join(sorted(_DEFAULT_REPO_SOURCES))
        raise ProtocolError(
            f"unsupported landing verification repository {repo!r}; "
            f"supported repositories: {supported}"
        )
    candidate = source if source is not None else _DEFAULT_REPO_SOURCES[repo]
    candidate = candidate.expanduser().resolve()
    if not candidate.is_dir():
        raise ProtocolError(f"source checkout for {repo!r} is missing: {candidate}")
    result = _run(
        ("git", "-C", str(candidate), "remote", "get-url", "origin"),
        check=True,
    )
    origin = result.stdout.strip()
    observed = _github_repo_from_remote(origin)
    if observed != repo:
        raise ProtocolError(
            f"source checkout {candidate} origin is {origin or '<missing>'!r}, "
            f"not required repository {repo!r}"
        )
    return candidate


def _fetch_target(source: Path, target: str) -> str:
    target_ref = f"origin/{target}"
    _run(
        (
            "with-proxy",
            "git",
            "-C",
            str(source),
            "fetch",
            "origin",
            f"refs/heads/{target}:refs/remotes/{target_ref}",
        ),
        check=True,
        timeout=DEFAULT_NETWORK_TIMEOUT,
    )
    return target_ref


def _is_target_ancestor(source: Path, sha: str, target_ref: str) -> bool:
    ancestry = _run(
        (
            "git",
            "-C",
            str(source),
            "merge-base",
            "--is-ancestor",
            sha,
            target_ref,
        ),
        check=False,
    )
    if ancestry.returncode not in (0, 1):
        detail = (ancestry.stderr or ancestry.stdout or "").strip()
        raise ProtocolError(f"cannot compare {sha} with fetched {target_ref}: {detail}")
    return ancestry.returncode == 0


def _is_main_ancestor(source: Path, sha: str) -> bool:
    return _is_target_ancestor(source, sha, "origin/main")


def _query_pr_landing(repo: str, pr: int) -> tuple[str, str, str]:
    result = _run(
        (
            "with-proxy",
            "gh",
            "pr",
            "view",
            str(pr),
            "-R",
            repo,
            "--json",
            "state,headRefOid,mergeCommit",
        ),
        check=True,
        timeout=DEFAULT_NETWORK_TIMEOUT,
    )
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ProtocolError("gh pr view returned invalid JSON") from error
    if not isinstance(payload, Mapping):
        raise ProtocolError("gh pr view returned a non-object")
    state = str(payload.get("state") or "").upper()
    head = str(payload.get("headRefOid") or "").lower()
    merge_commit = payload.get("mergeCommit")
    replay = (
        str(merge_commit.get("oid") or "").lower()
        if isinstance(merge_commit, Mapping)
        else ""
    )
    return state, head, replay


def _resolve_pr_replay_sha(source: Path, repo: str, pr: int) -> tuple[str, str]:
    state, head, replay = _query_pr_landing(repo, pr)
    if state != "MERGED" or not obligations.SHA_RE.fullmatch(replay):
        raise ProtocolError(
            f"{repo}#{pr} has no merged replay SHA (state={state or 'unknown'})"
        )
    if not _is_main_ancestor(source, replay):
        raise ProtocolError(
            f"{repo}#{pr} reports replay SHA {replay}, but it is not reachable "
            "from fetched origin/main (the landing may have been orphaned)"
        )
    return head, replay


def resolve_landed_sha(
    source: Path,
    requested: str,
    *,
    repo: str | None = None,
    pr: int | None = None,
) -> str:
    """Resolve the commit that actually landed, not a pre-rebase PR head.

    In PR-aware mode the PR identity and GitHub's replay SHA are canonical. The
    requested head may have been rewritten by rebase merge and need not exist in
    the local object database.
    """
    if not source.is_dir():
        raise ProtocolError(f"Hermit source checkout is missing: {source}")
    _run(
        ("with-proxy", "git", "-C", str(source), "fetch", "origin", "main"),
        check=True,
        timeout=DEFAULT_NETWORK_TIMEOUT,
    )
    if pr is not None:
        if not repo:
            raise ProtocolError("a repository is required with a PR number")
        _, replay = _resolve_pr_replay_sha(source, repo, pr)
        return replay
    resolved = (
        _run(
            (
                "git",
                "-C",
                str(source),
                "rev-parse",
                "--verify",
                f"{requested}^{{commit}}",
            ),
            check=True,
        )
        .stdout.strip()
        .lower()
    )
    if not obligations.SHA_RE.fullmatch(resolved):
        raise ProtocolError(f"cannot resolve a full commit SHA from {requested!r}")
    if not _is_main_ancestor(source, resolved):
        raise ProtocolError(
            f"{resolved} is not reachable from fetched origin/main. A PR head is "
            "normally rewritten by rebase-merge; pass --pr so the verifier can "
            "resolve and check GitHub's replay SHA instead"
        )
    return resolved


def _print_landing_verdict(
    *,
    state: str,
    rc: int,
    reference: str,
    target_ref: str,
    json_output: bool,
    **details: object,
) -> int:
    payload = {
        "state": state,
        "rc": rc,
        "input": reference,
        "target": target_ref,
        **details,
    }
    if json_output:
        print(json.dumps(payload, sort_keys=True))
    else:
        fields = " ".join(
            f"{key}={value}" for key, value in payload.items() if key != "state"
        )
        print(f"{state.upper().replace('-', '_')} {fields}")
    return rc


def _resolve_raw_sha(source: Path, reference: str) -> str:
    rev_parse = (
        "git",
        "-C",
        str(source),
        "rev-parse",
        "--verify",
        f"{reference}^{{commit}}",
    )
    result = _run(rev_parse, check=False)
    if result.returncode != 0:
        _run(
            (
                "with-proxy",
                "git",
                "-C",
                str(source),
                "fetch",
                "--no-tags",
                "origin",
                reference,
            ),
            check=True,
            timeout=DEFAULT_NETWORK_TIMEOUT,
        )
        result = _run(rev_parse, check=True)
    commit = result.stdout.strip().lower()
    if not obligations.SHA_RE.fullmatch(commit):
        raise ProtocolError(f"cannot resolve full commit SHA from {reference!r}")
    return commit


def _commit_parents(source: Path, commit: str) -> list[str]:
    """Return the exact commit's parents from the local object database."""
    result = _run(
        (
            "git",
            "-C",
            str(source),
            "rev-list",
            "--parents",
            "-n",
            "1",
            commit,
        ),
        check=True,
    )
    fields = result.stdout.strip().lower().split()
    if not fields or fields[0] != commit:
        raise ProtocolError(f"cannot inspect commit parents for {commit}")
    parents = fields[1:]
    if any(not obligations.SHA_RE.fullmatch(parent) for parent in parents):
        raise ProtocolError(f"commit {commit} has malformed parent identity")
    return parents


def _commit_tree(source: Path, commit: str) -> str:
    """Return the exact tree identity carried by ``commit``."""
    result = _run(
        (
            "git",
            "-C",
            str(source),
            "rev-parse",
            "--verify",
            f"{commit}^{{tree}}",
        ),
        check=True,
    )
    tree = result.stdout.strip().lower()
    if not obligations.SHA_RE.fullmatch(tree):
        raise ProtocolError(f"cannot inspect tree identity for {commit}")
    return tree


def _resolve_claimed_oid(
    source: Path,
    claimed_oid: str,
    *,
    pr: int,
    pr_head: str,
) -> tuple[str, bool, bool]:
    """Return (full OID, resolves, object available for local ancestry).

    The freshly fetched target resolves abbreviations for commits already on the
    target. A pre-rebase PR head is normally absent from the target, so use
    GitHub's full head identity and fetch the immutable pull ref before checking
    its ancestry. Merely padding or prefix-matching a short hash is never treated
    as local object resolution.
    """
    abbreviated = claimed_oid.strip().lower()
    if not re.fullmatch(r"[0-9a-f]{7,40}", abbreviated):
        return "", False, False

    rev_parse = (
        "git",
        "-C",
        str(source),
        "rev-parse",
        "--verify",
        f"{abbreviated}^{{commit}}",
    )
    resolved = _run(rev_parse, check=False)
    if resolved.returncode == 0:
        full_oid = resolved.stdout.strip().lower()
        if obligations.SHA_RE.fullmatch(full_oid):
            return full_oid, True, True

    if obligations.SHA_RE.fullmatch(pr_head) and pr_head.startswith(abbreviated):
        fetched = _run(
            (
                "with-proxy",
                "git",
                "-C",
                str(source),
                "fetch",
                "--no-tags",
                "origin",
                f"pull/{pr}/head",
            ),
            check=False,
            timeout=DEFAULT_NETWORK_TIMEOUT,
        )
        available = False
        if fetched.returncode == 0:
            available = _run(
                (
                    "git",
                    "-C",
                    str(source),
                    "cat-file",
                    "-e",
                    f"{pr_head}^{{commit}}",
                ),
                check=False,
            ).returncode == 0
        return pr_head, True, available

    return "", False, False


def _claim_fields(
    *,
    item: str,
    claimed_oid: str,
    full_oid: str,
    resolves: bool,
    change_present: bool,
    claimed_ancestry_rc: int,
    target: str,
) -> dict[str, object]:
    fields: dict[str, object] = {
        "item": item,
        "claimed_oid": claimed_oid,
        "full_oid": full_oid or "unresolved",
        "resolves": resolves,
        "change_present_on_target": change_present,
        "claimed_ancestry_rc": claimed_ancestry_rc,
    }
    if target == "main":
        fields["change_present_on_main"] = change_present
    return fields


def verify_landing(args: argparse.Namespace) -> int:
    item = getattr(args, "item", None)
    claimed_oid = getattr(args, "claimed_oid", None)
    try:
        source = resolve_repo_source(args.repo, args.source)
        target_ref = _fetch_target(source, args.target)
        reference = args.reference.lower()
        if claimed_oid and not item:
            return _print_landing_verdict(
                state="unverifiable",
                rc=2,
                reference=args.reference,
                target_ref=target_ref,
                json_output=args.json,
                reason="--claimed-oid requires --item",
            )
        if re.fullmatch(r"[0-9a-f]{7,40}", reference):
            commit = _resolve_raw_sha(source, reference)
            is_ancestor = _is_target_ancestor(source, commit, target_ref)
            return _print_landing_verdict(
                state="landed" if is_ancestor else "not-landed",
                rc=0 if is_ancestor else 1,
                reference=args.reference,
                target_ref=target_ref,
                json_output=args.json,
                input_kind="sha",
                resolved_sha=commit,
                ancestry="ancestor" if is_ancestor else "not-ancestor",
                **_claim_fields(
                    item=item or args.reference,
                    claimed_oid=args.reference,
                    full_oid=commit,
                    resolves=True,
                    change_present=is_ancestor,
                    claimed_ancestry_rc=0 if is_ancestor else 1,
                    target=args.target,
                ),
            )

        if not args.reference.isdecimal() or int(args.reference) <= 0:
            return _print_landing_verdict(
                state="unverifiable",
                rc=2,
                reference=args.reference,
                target_ref=target_ref,
                json_output=args.json,
                reason="input must be a positive PR number or 7-40 character commit OID",
            )

        pr = int(args.reference)
        pr_state, head, replay = _query_pr_landing(args.repo, pr)
        if pr_state != "MERGED" or not obligations.SHA_RE.fullmatch(replay):
            return _print_landing_verdict(
                state="unverifiable",
                rc=2,
                reference=args.reference,
                target_ref=target_ref,
                json_output=args.json,
                input_kind="pr",
                repo=args.repo,
                pr=pr,
                pr_state=pr_state or "unknown",
                pr_head_sha=head or "unknown",
                reason="no mergeCommit.oid",
            )
        is_ancestor = _is_target_ancestor(source, replay, target_ref)
        claim_details: dict[str, object]
        if claimed_oid:
            full_oid, resolves, locally_available = _resolve_claimed_oid(
                source,
                claimed_oid,
                pr=pr,
                pr_head=head,
            )
            claimed_ancestry_rc = 2
            if locally_available:
                claimed_ancestry_rc = (
                    0 if _is_target_ancestor(source, full_oid, target_ref) else 1
                )
            claim_details = _claim_fields(
                item=item,
                claimed_oid=claimed_oid,
                full_oid=full_oid,
                resolves=resolves,
                change_present=is_ancestor,
                claimed_ancestry_rc=claimed_ancestry_rc,
                target=args.target,
            )
        else:
            claim_details = _claim_fields(
                item=item or f"{args.repo}#{pr}",
                claimed_oid=replay,
                full_oid=replay,
                resolves=True,
                change_present=is_ancestor,
                claimed_ancestry_rc=0 if is_ancestor else 1,
                target=args.target,
            )
        return _print_landing_verdict(
            state="landed" if is_ancestor else "not-landed",
            rc=0 if is_ancestor else 1,
            reference=args.reference,
            target_ref=target_ref,
            json_output=args.json,
            input_kind="pr",
            repo=args.repo,
            pr=pr,
            pr_state=pr_state,
            pr_head_sha=head or "unknown",
            resolved_sha=replay,
            merge_commit_oid=replay,
            ancestry="ancestor" if is_ancestor else "not-ancestor",
            **claim_details,
        )
    except ProtocolError as error:
        return _print_landing_verdict(
            state="unverifiable",
            rc=2,
            reference=args.reference,
            target_ref=f"origin/{args.target}",
            json_output=args.json,
            reason=str(error),
        )


def _required_github_jobs(
    policy: Mapping[str, Any],
) -> list[dict[str, str]]:
    validated = validate_verification_policy(policy)
    return [dict(job) for job in validated["github"]["required_jobs"]]


def _required_workflows(policy: Mapping[str, Any]) -> list[tuple[str, str]]:
    workflows: list[tuple[str, str]] = []
    for job in _required_github_jobs(policy):
        identity = (job["workflow_file"], job["workflow_name"])
        if identity not in workflows:
            workflows.append(identity)
    return workflows


def _parse_github_runs(
    output: str, sha: str, policy: Mapping[str, Any]
) -> list[dict[str, Any]]:
    policy = validate_verification_policy(policy)
    if not obligations.SHA_RE.fullmatch(sha):
        raise ProtocolError(f"invalid exact-SHA GitHub query {sha!r}")
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as error:
        raise ProtocolError("GitHub workflow-runs API returned invalid JSON") from error
    if not isinstance(payload, Mapping) or not isinstance(
        payload.get("workflow_runs"), list
    ):
        raise ProtocolError("GitHub workflow-runs API returned an invalid payload")
    raw_runs = payload["workflow_runs"]
    total_count = payload.get("total_count")
    if type(total_count) is not int or total_count != len(raw_runs):
        raise ProtocolError(
            "GitHub workflow-runs response is truncated or has an invalid exact count"
        )

    expected_by_file = dict(_required_workflows(policy))
    grouped: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for raw in raw_runs:
        if not isinstance(raw, Mapping):
            raise ProtocolError("GitHub workflow-runs response contains a non-object")
        run_sha = str(raw.get("head_sha") or "").lower()
        if run_sha != sha:
            raise ProtocolError(
                f"GitHub workflow run is for {run_sha or '<missing>'}, "
                f"expected exact SHA {sha}"
            )
        workflow_file = str(raw.get("path") or "")
        if workflow_file not in expected_by_file:
            continue
        workflow_name = str(raw.get("name") or "")
        expected_name = expected_by_file[workflow_file]
        if workflow_name != expected_name:
            raise ProtocolError(
                f"GitHub workflow {workflow_file!r} is named "
                f"{workflow_name or '<missing>'!r}, expected {expected_name!r}"
            )
        try:
            run_id = int(raw["id"])
        except (KeyError, TypeError, ValueError) as error:
            raise ProtocolError("GitHub workflow run has no integer id") from error
        normalized = {
            "databaseId": run_id,
            "headSha": run_sha,
            "workflowFile": workflow_file,
            "workflowName": workflow_name,
            "status": raw.get("status"),
            "conclusion": raw.get("conclusion"),
            "createdAt": raw.get("created_at"),
            "startedAt": raw.get("run_started_at"),
            "updatedAt": raw.get("updated_at"),
            "url": raw.get("html_url"),
            "event": raw.get("event"),
            "runAttempt": raw.get("run_attempt"),
        }
        grouped.setdefault((workflow_file, workflow_name), []).append(normalized)

    selected: list[dict[str, Any]] = []
    for identity in _required_workflows(policy):
        candidates = grouped.get(identity, [])
        if not candidates:
            continue
        attempts = select_latest_workflow_attempts(
            candidates,
            head_sha=sha,
            workflows=(identity[1],),
        )
        if attempts:
            selected.append(attempts[0])
    return selected


def _parse_github_jobs(
    output: str,
    *,
    run: Mapping[str, Any],
    sha: str,
) -> list[dict[str, Any]]:
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as error:
        raise ProtocolError("GitHub jobs API returned invalid JSON") from error
    if not isinstance(payload, Mapping) or not isinstance(payload.get("jobs"), list):
        raise ProtocolError("GitHub jobs API returned an invalid payload")
    raw_jobs = payload["jobs"]
    total_count = payload.get("total_count")
    if type(total_count) is not int or total_count != len(raw_jobs):
        raise ProtocolError(
            "GitHub jobs response is truncated or has an invalid exact count"
        )
    run_id = int(run["databaseId"])
    jobs: list[dict[str, Any]] = []
    seen_job_ids: set[int] = set()
    for raw in raw_jobs:
        if not isinstance(raw, Mapping):
            raise ProtocolError("GitHub jobs response contains a non-object")
        job_sha = str(raw.get("head_sha") or "").lower()
        if job_sha != sha:
            raise ProtocolError(
                f"GitHub job is for {job_sha or '<missing>'}, expected exact SHA {sha}"
            )
        observed_run_id = raw.get("run_id")
        if type(observed_run_id) is not int or observed_run_id != run_id:
            raise ProtocolError(
                f"GitHub job belongs to run {observed_run_id!r}, expected {run_id}"
            )
        raw_job_id = raw.get("id")
        job_id = raw_job_id if type(raw_job_id) is int and raw_job_id > 0 else None
        if job_id is not None:
            if job_id in seen_job_ids:
                raise ProtocolError(
                    f"GitHub jobs response repeats job identity ({run_id}, {job_id})"
                )
            seen_job_ids.add(job_id)
        jobs.append(
            {
                "id": job_id,
                "run_id": observed_run_id,
                "head_sha": job_sha,
                "name": str(raw.get("name") or ""),
                "status": raw.get("status"),
                "conclusion": raw.get("conclusion"),
                "started_at": raw.get("started_at"),
                "completed_at": raw.get("completed_at"),
                "html_url": raw.get("html_url"),
            }
        )
    return jobs


def github_runs(
    repo: str,
    sha: str,
    *,
    policy: Mapping[str, Any] | None = None,
) -> list[dict[str, Any]]:
    selected_policy = (
        verification_policy_for_repo(repo)
        if policy is None
        else validate_verification_policy(policy)
    )
    if selected_policy["repo"] != repo:
        raise ProtocolError(
            f"verification policy for {selected_policy['repo']!r} cannot query {repo!r}"
        )
    if not obligations.SHA_RE.fullmatch(sha):
        raise ProtocolError(f"invalid exact-SHA GitHub query {sha!r}")
    result = _run(
        (
            "with-proxy",
            "gh",
            "api",
            f"repos/{repo}/actions/runs?head_sha={sha}&per_page=100",
        ),
        check=True,
        timeout=DEFAULT_NETWORK_TIMEOUT,
    )
    runs = _parse_github_runs(result.stdout, sha, selected_policy)
    for run in runs:
        jobs_result = _run(
            (
                "with-proxy",
                "gh",
                "api",
                f"repos/{repo}/actions/runs/{run['databaseId']}/jobs?filter=latest&per_page=100",
            ),
            check=True,
            timeout=DEFAULT_NETWORK_TIMEOUT,
        )
        run["jobs"] = _parse_github_jobs(
            jobs_result.stdout,
            run=run,
            sha=sha,
        )
    return runs


def github_main_sha(repo: str) -> str:
    result = _run(
        ("with-proxy", "gh", "api", f"repos/{repo}/commits/main", "--jq", ".sha"),
        check=True,
        timeout=DEFAULT_NETWORK_TIMEOUT,
    )
    sha = result.stdout.strip().lower()
    if not obligations.SHA_RE.fullmatch(sha):
        raise ProtocolError(f"GitHub returned invalid main SHA {sha!r}")
    return sha


def _github_state(run: Mapping[str, Any]) -> str:
    status = str(run.get("status") or "").lower()
    if status in {"requested", "waiting", "queued", "pending"}:
        return "pending"
    if status == "in_progress":
        return "running"
    # A conclusion without a completed producer status is not a verdict.  In
    # particular, accepting ``status=None, conclusion=success`` would let a
    # partial/malformed API object manufacture a positive authority record.
    if status != "completed":
        return "no_result"
    outcome = classify_check(run.get("status"), run.get("conclusion"))
    if outcome is CheckOutcome.PASSED:
        return "green"
    if outcome is CheckOutcome.FAILED:
        return "red"
    return "no_result"


def _github_patch(
    runs: Sequence[Mapping[str, Any]], sha: str, policy: Mapping[str, Any]
) -> dict[str, Any]:
    policy = validate_verification_policy(policy)
    if not obligations.SHA_RE.fullmatch(sha):
        raise ProtocolError(f"invalid exact-SHA GitHub observation {sha!r}")
    expected_jobs = _required_github_jobs(policy)
    expected_count = policy["github"]["required_positive_count"]
    if expected_count != len(expected_jobs) or expected_count <= 0:
        raise ProtocolError("verification policy has a vacuous positive count")

    runs_by_workflow: dict[tuple[str, str], list[Mapping[str, Any]]] = {}
    run_workflows: dict[int, tuple[str, str]] = {}
    for run in runs:
        run_sha = str(run.get("headSha") or "").lower()
        if run_sha != sha:
            raise ProtocolError(
                f"GitHub run is for {run_sha or '<missing>'}, expected exact SHA {sha}"
            )
        identity = (
            str(run.get("workflowFile") or ""),
            str(run.get("workflowName") or ""),
        )
        if identity not in _required_workflows(policy):
            raise ProtocolError(f"unexpected GitHub workflow identity {identity!r}")
        run_id = run.get("databaseId")
        if type(run_id) is not int or run_id <= 0:
            raise ProtocolError("GitHub workflow evidence has no positive run id")
        previous_identity = run_workflows.get(run_id)
        if previous_identity is not None and previous_identity != identity:
            raise ProtocolError(
                f"GitHub run {run_id} is reused for workflow identities "
                f"{previous_identity!r} and {identity!r}"
            )
        run_workflows[run_id] = identity
        runs_by_workflow.setdefault(identity, []).append(run)

    observed_jobs: list[dict[str, Any]] = []
    run_ids: list[int] = []
    selected_job_identities: set[tuple[int, int]] = set()
    urls: list[str] = []
    reasons: list[str] = []
    for expected in expected_jobs:
        workflow_identity = (
            expected["workflow_file"],
            expected["workflow_name"],
        )
        workflow_runs = runs_by_workflow.get(workflow_identity, [])
        if len(workflow_runs) > 1:
            raise ProtocolError(
                f"duplicate latest workflow evidence for {workflow_identity!r}"
            )
        if not workflow_runs:
            observed_jobs.append(
                {
                    **expected,
                    "state": "no_result",
                    "reason": "no dereferenced workflow producer",
                }
            )
            reasons.append(
                f"no workflow producer {expected['workflow_file']} / "
                f"{expected['workflow_name']}"
            )
            continue
        run = workflow_runs[0]
        run_id = int(run["databaseId"])
        if run_id not in run_ids:
            run_ids.append(run_id)
        run_url = str(run.get("url") or "")
        if run_url and run_url not in urls:
            urls.append(run_url)
        jobs = run.get("jobs")
        if not isinstance(jobs, list):
            raise ProtocolError(f"GitHub run {run_id} has no dereferenced jobs list")
        matches = [job for job in jobs if job.get("name") == expected["job_name"]]
        if len(matches) > 1:
            raise ProtocolError(
                f"duplicate required GitHub job {expected['job_name']!r} in run {run_id}"
            )
        if not matches:
            run_state = _github_state(run)
            reason = f"required job missing from {run_state} workflow run"
            observed_jobs.append(
                {
                    **expected,
                    "run_id": run_id,
                    "state": "no_result",
                    "status": run.get("status"),
                    "reason": reason,
                }
            )
            reasons.append(f"{expected['job_name']}: {reason}")
            continue
        job = matches[0]
        job_sha = str(job.get("head_sha") or "").lower()
        if job_sha != sha:
            raise ProtocolError(
                f"GitHub job is for {job_sha or '<missing>'}, expected exact SHA {sha}"
            )
        job_run_id = job.get("run_id")
        if type(job_run_id) is not int or job_run_id <= 0 or job_run_id != run_id:
            raise ProtocolError(
                f"GitHub job {job.get('id')} is not bound to selected run {run_id}"
            )
        job_id = job.get("id")
        if type(job_id) is not int or job_id <= 0:
            reason = "required job has no positive dereferenced job identity"
            observed_jobs.append(
                {
                    **expected,
                    "run_id": run_id,
                    "state": "no_result",
                    "status": job.get("status"),
                    "conclusion": job.get("conclusion"),
                    "url": str(job.get("html_url") or ""),
                    "reason": reason,
                }
            )
            reasons.append(f"{expected['job_name']}: {reason}")
            continue
        job_identity = (run_id, job_id)
        if job_identity in selected_job_identities:
            raise ProtocolError(
                f"required GitHub evidence repeats job identity {job_identity!r}"
            )
        selected_job_identities.add(job_identity)
        state = _github_state(job)
        reason = None
        if state == "no_result":
            reason = (
                f"required job completed without a verdict "
                f"(conclusion={job.get('conclusion') or '<missing>'})"
            )
            reasons.append(f"{expected['job_name']}: {reason}")
        job_url = str(job.get("html_url") or "")
        if job_url and job_url not in urls:
            urls.append(job_url)
        observed_jobs.append(
            {
                **expected,
                "run_id": run_id,
                "job_id": job_id,
                "state": state,
                "status": job.get("status"),
                "conclusion": job.get("conclusion"),
                "url": job_url,
                "reason": reason,
            }
        )

    states = [job["state"] for job in observed_jobs]
    positive_count = sum(state == "green" for state in states)
    if "red" in states:
        state = "red"
    elif positive_count == expected_count and len(observed_jobs) == expected_count:
        state = "green"
    elif "running" in states:
        state = "running"
    elif "pending" in states:
        state = "pending"
    else:
        state = "no_result"
    started = [
        str(run.get("startedAt") or run.get("createdAt"))
        for run in runs
        if run.get("startedAt") or run.get("createdAt")
    ]
    finished = [str(run.get("updatedAt")) for run in runs if run.get("updatedAt")]
    return {
        "github": {
            "state": state,
            "started_at": min(started) if started else None,
            "finished_at": (
                max(finished)
                if finished
                and (state in TERMINAL_VERIFICATION_STATES or state == "no_result")
                else None
            ),
            "run_ids": run_ids,
            "urls": urls,
            "workflow_name": ", ".join(
                workflow_name for _, workflow_name in _required_workflows(policy)
            ),
            "event": ", ".join(
                sorted({str(run.get("event")) for run in runs if run.get("event")})
            )
            or None,
            "required_positive_count": expected_count,
            "positive_count": positive_count,
            "jobs": observed_jobs,
            "last_poll_error": "; ".join(reasons) or None,
        }
    }


def hosted_status(args: argparse.Namespace) -> int:
    """Dereference the repository's exact-head hosted landing authority."""
    sha = args.sha.lower()
    if not obligations.SHA_RE.fullmatch(sha):
        raise ProtocolError("--sha must be a full 40-character commit SHA")
    policy = verification_policy_for_repo(args.repo)
    runs = github_runs(args.repo, sha, policy=policy)
    evidence = _github_patch(runs, sha, policy)["github"]
    report = {
        "schema_version": 1,
        "authority": "github-actions-exact-head-jobs",
        "repo": args.repo,
        "sha": sha,
        "policy_schema_version": policy["schema_version"],
        **evidence,
    }
    if args.json:
        print(json.dumps(report, sort_keys=True))
    else:
        print(
            f"HOSTED {str(evidence['state']).upper()} {args.repo}@{sha} "
            f"positive={evidence['positive_count']}/"
            f"{evidence['required_positive_count']}"
        )
    return {"green": 0, "red": 3}.get(str(evidence["state"]), 4)


def ensure_github_verification(
    obligation_id: str,
    *,
    store_path: Path,
    wait_seconds: int = DEFAULT_GITHUB_WAIT_SECONDS,
    poll_seconds: int = 5,
    allow_dispatch: bool = True,
    sleep: Callable[[float], None] = time.sleep,
) -> dict[str, Any]:
    record, policy = bind_verification_policy(obligation_id, store_path)
    repo, sha = record["repo"], record["landed_sha"]
    deadline = time.monotonic() + wait_seconds
    dispatched = False
    latest_patch = _github_patch([], sha, policy)
    while True:
        runs = github_runs(repo, sha, policy=policy)
        latest_patch = _github_patch(runs, sha, policy)
        record = obligations.get_record(obligation_id, store_path)
        if record.get("github") != latest_patch["github"]:
            record = obligations.transition(
                obligation_id,
                "github-observed",
                latest_patch,
                store_path,
            )
        if latest_patch["github"]["state"] in {"green", "red"}:
            return evaluate_obligation(obligation_id, store_path=store_path)
        if time.monotonic() >= deadline:
            break
        if (
            allow_dispatch
            and not dispatched
            and deadline - time.monotonic() <= max(0, wait_seconds - 30)
        ):
            if github_main_sha(repo) == sha:
                for workflow_file, _ in _required_workflows(policy):
                    _run(
                        (
                            "with-proxy",
                            "gh",
                            "workflow",
                            "run",
                            workflow_file,
                            "-R",
                            repo,
                            "--ref",
                            "main",
                        ),
                        check=True,
                        timeout=DEFAULT_NETWORK_TIMEOUT,
                    )
                dispatched = True
        sleep(min(poll_seconds, max(0.0, deadline - time.monotonic())))

    active = _github_verification_in_flight(latest_patch["github"])
    if active:
        # A real queued/in-progress producer remains durable and watchable. It is
        # not collapsed into no_result just because the synchronous arm wait ended.
        return evaluate_obligation(obligation_id, store_path=store_path)
    summary = (
        f"required hosted job set did not produce {policy['github']['required_positive_count']} "
        f"exact-SHA positives for {sha} within {wait_seconds}s"
    )
    # A missing or unresolved GitHub result is NOT a failure: it leaves the leg in
    # no_result so a locally-green land is never reverted purely because its hosted
    # run was throttled/cancelled. A later poll or the re-dispatched run completes
    # verification; a genuine local red still alarms via the local leg.
    obligations.transition(
        obligation_id,
        "github-no-result",
        {
            "github": {
                "state": "no_result",
                "finished_at": None,
                "required_positive_count": policy["github"]["required_positive_count"],
                "positive_count": latest_patch["github"]["positive_count"],
                "jobs": latest_patch["github"]["jobs"],
                "last_poll_error": summary,
            }
        },
        store_path,
    )
    return evaluate_obligation(obligation_id, store_path=store_path)


def _failure_details(record: Mapping[str, Any]) -> tuple[str, str]:
    failed: list[tuple[str, Mapping[str, Any]]] = []
    for source in ("local", "github"):
        verification = record[source]
        if verification.get("state") in REMEDIATION_STATES:
            failed.append((source, verification))
    if not failed:
        raise ProtocolError("failure details requested for a non-failing obligation")
    failed.sort(key=lambda pair: str(pair[1].get("finished_at") or ""))
    source, verification = failed[0]
    if source == "local":
        summary = (
            f"local validate state={verification.get('state')} "
            f"exit={verification.get('exit_code')} log={verification.get('log_path')}"
        )
    else:
        expected_workflow = ", ".join(
            workflow_name
            for _, workflow_name in _required_workflows(
                _verification_policy_from_record(record)
            )
        )
        summary = (
            f"GitHub {verification.get('workflow_name') or expected_workflow} "
            f"state={verification.get('state')} urls={','.join(verification.get('urls') or [])}"
        )
    return source, summary


def remediation_recommendation(
    record: Mapping[str, Any], main_sha: str | None
) -> dict[str, str]:
    if main_sha == record["landed_sha"] or main_sha is None:
        return {
            "action": "revert",
            "reason": (
                "the failing speculative land is still the main tip; revert immediately to "
                "restore the last green base, then fix forward on a reviewed branch"
                if main_sha is not None
                else "main-tip identity is unavailable; conservatively prepare an immediate revert"
            ),
        }
    return {
        "action": "fix-forward",
        "reason": (
            f"main has advanced to {main_sha}; repair the current tip immediately rather than "
            "blindly reverting through later lands"
        ),
    }


def _legs_disagree(record: Mapping[str, Any]) -> bool:
    """One exact-SHA verifier is green while the other is genuinely red.

    This is a THIRD outcome, neither a confirmed regression nor a clean pass: the
    authorities contradict each other for the same commit. It is symmetric and
    never reaches the actuator; a passing authority cannot be silently overruled
    by a failing one from a different execution environment.
    """
    return {record["local"]["state"], record["github"]["state"]} == {
        "green",
        "red",
    }


def _local_red_uncorroborated(record: Mapping[str, Any]) -> bool:
    """A local red whose authoritative GitHub leg NEVER reported (no_result).

    Under admission-limited hosted CI, github == "no_result" (never admitted,
    cancelled below the concurrency cap, or superseded) is the COMMON case, not a
    rare hole. A local red beside it — even after the whole re-dispatch budget is
    spent, since a load/environment-dependent flake reproduces on the same
    loaded/cold box — has NO authoritative corroboration. It is therefore surfaced
    for a human to investigate (a genuine local-only regression, or an
    environmental/load artifact the hosted leg would have avoided), NEVER an
    auto-revert of a landed tip. This is the actuator-side residual of the
    no_result taxonomy: the classifier already keeps no_result distinct from red,
    and here the revert PATH refuses to act on a lone local signal.

    Gated on the spent re-dispatch budget so a still-re-dispatching local red
    (redispatch_count < limit) keeps re-validating rather than immediately
    raising an investigation alert.
    """
    return (
        record["local"]["state"] == "red"
        and record["github"]["state"] == "no_result"
        and int(record["local"].get("redispatch_count") or 0)
        >= DEFAULT_LOCAL_REDISPATCH_LIMIT
    )


def _remediation_ready(record: Mapping[str, Any]) -> bool:
    """Whether a failing answer is CONFIRMED enough to revert a landed tip.

    Sequencing comes first: never arm a revert while EITHER verifier is still
    ``starting`` or ``running``. A leg in flight may be the tool's OWN exoneration arriving — the
    incident that armed ``action=revert`` on a local red while the GitHub verify
    run (30873193855) was still executing (obligation 20260804-025419-0f891e43).
    A revert decision is never made on a partial picture. (``pending`` — a leg not
    yet dispatched — does not block the authoritative GitHub-red path below, which
    is the intended safe revert; only an active ``starting``/``running`` answer is
    waited on.)

    Once both legs have reported, ONLY an authoritative GitHub red arms a revert —
    hosted CI does not flake the way a loaded local box does, and cancelled/absent
    is already no_result — so it remediates at once. A local red is only
    PROVISIONAL and NEVER reverts alone: a single (or even re-dispatch-reproduced)
    local validate that failed on a loaded/cold box is a candidate flake, not an
    authoritative regression. Beside a GREEN hosted leg it is a DISAGREEMENT
    (see ``_legs_disagree``); beside a NO_RESULT hosted leg — the COMMON case
    under admission-limited hosted CI, where the authoritative run was never
    admitted / cancelled below the concurrency cap / superseded — it is an
    UNCORROBORATED local signal (see ``_local_red_uncorroborated``). Either way it
    is surfaced for a human to investigate, never an auto-revert of a landed tip.

    History: reverting a healthy tip on a lone local red is the exact hazard this
    guard forecloses — three such recommendations fired 2026-08-04 (e8a0d8d3,
    0f891e43 x2), all caught by humans reading evidence, not by the actuator. The
    classifiers (_classify_local, _github_state) were already conservative; this is
    the ACTUATOR refusing to act on a lone local signal at all. This is the
    post-mortem's own remedy — a local box cannot overrule the authoritative
    verifier — made mechanical.
    """
    local = record["local"]["state"]
    github = record["github"]["state"]
    if {local, github}.intersection({"starting", "running"}):
        return False
    if github == "red":
        return True
    # A local red is never sufficient to revert on its own. github == "green" is
    # a disagreement (_legs_disagree); github == "no_result" is uncorroborated
    # (_local_red_uncorroborated). Both route to investigation_required in
    # evaluate_obligation; neither arms an automated revert.
    return False


def evaluate_obligation(
    obligation_id: str,
    *,
    store_path: Path,
    main_sha: str | None = None,
) -> dict[str, Any]:
    record = obligations.get_record(obligation_id, store_path)
    remediation = record.get("remediation")
    if (
        record.get("overall_state") in obligations.CLOSED_STATES
        and isinstance(remediation, Mapping)
        and remediation.get("state") == "completed"
    ):
        # A completed repair is a terminal historical fact. Re-polling its
        # original red producers must not reopen the obligation and strand the
        # detached watcher inside a landing-lock cleanup domain. `satisfied`
        # records deliberately keep reconciling an in-flight peer so a later
        # green/red contradiction can still become investigation_required.
        return record
    try:
        # Evaluation is an authority consumer, so a legacy record must first
        # acquire its repository policy in the append-only ledger. Merely
        # deriving a default in memory would let a green satisfy an unbound fact.
        record, _ = bind_verification_policy(obligation_id, store_path)
    except ProtocolError as error:
        return _record_policy_investigation(record, error, store_path)
    # Older obligation rows may carry local.state=green solely from
    # validate.sh's process exit. Re-dereference them once through the counted
    # exact-SHA authority; a refusal becomes no_result, never a preserved green.
    record = bind_local_receipt_authority(obligation_id, store_path)
    states = (record["local"]["state"], record["github"]["state"])
    now = obligations.utc_now()
    first_terminal_at = record.get("first_terminal_at")
    if first_terminal_at is None and any(
        state in TERMINAL_VERIFICATION_STATES for state in states
    ):
        first_terminal_at = now

    if _legs_disagree(record):
        failed_source = "local" if record["local"]["state"] == "red" else "github"
        passed_source = "github" if failed_source == "local" else "local"
        summary = (
            f"{failed_source} verifier red but {passed_source} verifier green for "
            f"the same SHA {record['landed_sha']}; NOT actuating — investigate "
            "the contradictory exact-SHA authorities"
        )
        if record.get("overall_state") != "investigation_required":
            record = obligations.transition(
                obligation_id,
                "verification-disagreement",
                {
                    "overall_state": "investigation_required",
                    "first_terminal_at": first_terminal_at,
                    "failure_source": failed_source,
                    "failure_summary": summary,
                    "recommendation": None,
                    "alert": {
                        "state": "raised",
                        "raised_at": now,
                        "severity": "P0",
                        "action": "investigate",
                    },
                    "remediation": {"state": "not_required"},
                },
                store_path,
            )
            print(
                f"HARD WARNING: obligation {obligation_id} legs DISAGREE: {summary}",
                file=sys.stderr,
                flush=True,
            )
        return record

    # One exact-SHA green is authoritative. The other source may still be
    # pending, running, absent/no_result, or independently green; none is a
    # failing answer, so waiting for it would turn a supplemental source into an
    # AND gate. A known red was handled as DISAGREEMENT above.
    if "green" in states:
        if record.get("overall_state") != "satisfied":
            record = obligations.transition(
                obligation_id,
                "satisfied",
                {
                    "overall_state": "satisfied",
                    "first_terminal_at": first_terminal_at,
                    "satisfied_at": now,
                    "remediation": {"state": "not_required"},
                },
                store_path,
            )
        return record

    if _remediation_ready(record):
        source, summary = _failure_details(record)
        if main_sha is None:
            try:
                main_sha = github_main_sha(record["repo"])
            except ProtocolError:
                main_sha = None
        recommendation = remediation_recommendation(record, main_sha)
        raised_now = (
            record.get("overall_state") != "remediation_required"
            or record.get("failure_summary") != summary
        )
        if raised_now:
            record = obligations.transition(
                obligation_id,
                "remediation-required",
                {
                    "overall_state": "remediation_required",
                    "first_terminal_at": first_terminal_at,
                    "failure_source": source,
                    "failure_summary": summary,
                    "recommendation": recommendation,
                    "alert": {"state": "raised", "raised_at": now},
                    "remediation": {
                        "state": "recommended",
                        "kind": recommendation["action"],
                    },
                },
                store_path,
            )
        if raised_now:
            print(
                f"HARD WARNING: obligation {obligation_id} failed: {summary}; "
                f"recommendation={recommendation['action']} ({recommendation['reason']})",
                file=sys.stderr,
                flush=True,
            )
        return trigger_remediation(record, store_path=store_path)

    if _local_red_uncorroborated(record):
        summary = (
            "local validate red but the authoritative GitHub leg never reported "
            f"(no_result) for SHA {record['landed_sha']} after the re-dispatch "
            "budget was spent; NOT reverting on a lone local signal — investigate "
            "(genuine local-only regression, or a load/environment artifact the "
            "hosted leg was never admitted to corroborate under admission-limited CI)"
        )
        if record.get("overall_state") != "investigation_required":
            record = obligations.transition(
                obligation_id,
                "verification-uncorroborated-local-red",
                {
                    "overall_state": "investigation_required",
                    "first_terminal_at": first_terminal_at,
                    "failure_source": "local",
                    "failure_summary": summary,
                    "recommendation": None,
                    "alert": {"state": "raised", "raised_at": now},
                    "remediation": {"state": "not_required"},
                },
                store_path,
            )
            print(
                f"HARD WARNING: obligation {obligation_id} UNCORROBORATED local "
                f"red: {summary}",
                file=sys.stderr,
                flush=True,
            )
        return record

    if first_terminal_at != record.get("first_terminal_at"):
        record = obligations.transition(
            obligation_id,
            "verification-progress",
            {"first_terminal_at": first_terminal_at},
            store_path,
        )
    return record


def trigger_remediation(
    record: Mapping[str, Any], *, store_path: Path
) -> dict[str, Any]:
    """Persist an idempotent, executable remediation dispatch.

    The ORC heartbeat may send a best-effort wake, but the append-only dispatch
    is the authority. Every fresh lander can discover and acknowledge the same
    record without receiving that wake.
    """
    if record.get("overall_state") != "remediation_required":
        return dict(record)
    remediation = record.get("remediation")
    if isinstance(remediation, Mapping) and remediation.get("state") in {
        "triggered",
        "completed",
    }:
        return dict(record)
    recommendation = record.get("recommendation")
    action = (
        recommendation.get("action") if isinstance(recommendation, Mapping) else None
    )
    if action not in {"fix-forward", "revert"}:
        raise ProtocolError("cannot trigger remediation without a concrete action")
    now = obligations.utc_now()
    instruction = (
        f"Act immediately on obligation {record['obligation_id']}: {action} "
        f"{record['repo']}@{record['landed_sha']}. Failure: "
        f"{record.get('failure_summary') or 'see obligation record'}. "
        "After the repair lands, run resolve-obligation with its full SHA."
    )
    triggered = obligations.transition(
        str(record["obligation_id"]),
        "remediation-triggered",
        {
            "alert": {
                "state": "pending",
                "raised_at": now,
                "target": "hermit-lander",
            },
            "remediation": {
                "state": "triggered",
                "kind": action,
                "started_at": now,
                "dispatch": {
                    "state": "pending",
                    "target": "hermit-lander",
                    "requested_at": now,
                    "instruction": instruction,
                    "wake_attempt": 0,
                    "wake_id": None,
                    "wake_sent_at": None,
                    "acknowledged_at": None,
                    "acknowledged_by": None,
                    "acknowledged_session": None,
                },
            },
        },
        store_path,
    )
    print(
        f"REMEDIATION TRIGGERED: obligation={record['obligation_id']} "
        f"action={action} target=hermit-lander",
        file=sys.stderr,
        flush=True,
    )
    return triggered


def actionable_records(store_path: Path) -> list[dict[str, Any]]:
    """Return every remediation still owed, independent of notification state."""
    return [
        record
        for record in obligations.unresolved_records(store_path)
        if record.get("overall_state") == "remediation_required"
    ]


def record_wake_sent(
    *, store_path: Path, target: str, source: str
) -> list[dict[str, Any]]:
    """Record that notification was attempted, not that anybody handled it."""
    now = obligations.utc_now()
    wake_id = uuid.uuid4().hex
    updated: list[dict[str, Any]] = []
    for record in actionable_records(store_path):
        remediation = record.get("remediation") or {}
        dispatch = remediation.get("dispatch") or {}
        attempt = int(dispatch.get("wake_attempt") or 0) + 1
        already_acknowledged = dispatch.get("state") == "acknowledged"
        alert_patch = (
            {
                "state": "handled",
                "target": target,
                "wake_id": wake_id,
                "wake_sent_at": now,
            }
            if already_acknowledged
            else {
                "state": "sent_unacknowledged",
                "target": target,
                "wake_id": wake_id,
                "wake_sent_at": now,
            }
        )
        dispatch_patch: dict[str, Any] = {
            "state": "acknowledged" if already_acknowledged else "sent_unacknowledged",
            "target": target,
            "source": source,
            "wake_attempt": attempt,
            "wake_id": wake_id,
            "wake_sent_at": now,
        }
        if not already_acknowledged:
            dispatch_patch.update(
                acknowledged_at=None,
                acknowledged_by=None,
                acknowledged_session=None,
            )
        updated.append(
            obligations.transition(
                record["obligation_id"],
                "remediation-wake-sent",
                {
                    "alert": alert_patch,
                    "remediation": {"dispatch": dispatch_patch},
                },
                store_path,
            )
        )
    unacknowledged = sum(
        ((record.get("remediation") or {}).get("dispatch") or {}).get("state")
        == "sent_unacknowledged"
        for record in updated
    )
    print(
        f"WAKE RECORDED: wake_id={wake_id} target={target} count={len(updated)} "
        f"unacknowledged={unacknowledged} ids="
        + (",".join(record["obligation_id"] for record in updated) or "none")
    )
    return updated


def inherit_actionable(
    *,
    store_path: Path,
    agent: str,
    session: str,
    action_outcome: str | None = None,
    reason: str | None = None,
) -> list[dict[str, Any]]:
    """Discover inherited remediation and optionally record its disposition.

    A bare acknowledgement means only that the dispatch was received.  An
    explicit decline is a durable decision and therefore requires a reason in
    the obligation itself.  Execution may be recorded without a reason because
    the completed repair is separately proven by ``resolve-obligation``.
    """
    if action_outcome not in {None, "executed", "declined"}:
        raise ProtocolError(
            "--action-outcome must be either 'executed' or 'declined'"
        )
    normalized_reason = reason.strip() if reason is not None else None
    if action_outcome == "declined" and not normalized_reason:
        raise ProtocolError(
            "--reason is required when --action-outcome=declined"
        )
    if action_outcome != "declined" and reason is not None:
        raise ProtocolError(
            "--reason is only valid with --action-outcome=declined"
        )

    now = obligations.utc_now()
    inherited: list[dict[str, Any]] = []
    for record in actionable_records(store_path):
        remediation = record.get("remediation") or {}
        dispatch = remediation.get("dispatch") or {}
        acknowledgement = dispatch.get("acknowledgement") or {}
        same_disposition = action_outcome is None or (
            acknowledgement.get("outcome") == action_outcome
            and acknowledgement.get("reason") == normalized_reason
            and acknowledgement.get("recorded_by") == agent
            and acknowledgement.get("recorded_session") == session
        )
        if (
            dispatch.get("state") == "acknowledged"
            and dispatch.get("acknowledged_by") == agent
            and dispatch.get("acknowledged_session") == session
            and same_disposition
        ):
            inherited.append(record)
            continue
        dispatch_patch: dict[str, Any] = {
            "state": "acknowledged",
            "acknowledged_at": now,
            "acknowledged_by": agent,
            "acknowledged_session": session,
            "acknowledged_wake_id": dispatch.get("wake_id"),
        }
        if action_outcome is not None:
            dispatch_patch["acknowledgement"] = {
                "outcome": action_outcome,
                "reason": normalized_reason,
                "recorded_at": now,
                "recorded_by": agent,
                "recorded_session": session,
            }
        inherited.append(
            obligations.transition(
                record["obligation_id"],
                "remediation-inherited",
                {
                    "alert": {
                        "state": "handled",
                        "handled_at": now,
                        "handled_by": agent,
                        "handled_session": session,
                    },
                    "remediation": {
                        "dispatch": dispatch_patch
                    },
                },
                store_path,
            )
        )
    print(f"INHERITED REMEDIATION OBLIGATIONS: {len(inherited)}")
    for record in inherited:
        dispatch = (record.get("remediation") or {}).get("dispatch") or {}
        print(
            f"  {record['obligation_id']} {record['repo']}@{record['landed_sha']} "
            f"action={(record.get('recommendation') or {}).get('action', '-')}"
        )
        print(f"    {dispatch.get('instruction') or 'inspect the obligation record'}")
    return inherited


def _spawn_detached(arguments: Sequence[str], log_path: Path) -> int:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("ab", buffering=0) as output:
        process = subprocess.Popen(
            [
                "nohup",
                "setsid",
                "--fork",
                "--wait",
                "--",
                sys.executable,
                str(Path(__file__).resolve()),
                *arguments,
            ],
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            close_fds=True,
        )
    return process.pid


def _driver_advertises(help_text: str, flag: str) -> bool:
    """Whether `help_text` offers exactly `flag`, not merely contains its text.

    The boundary is `[\\w-]` rather than `\\b` because these tokens are
    hyphenated, so `\\b` would still match a longer neighbour. Containment
    over-matches on PREFIXES: a driver advertising only `--label-pr` contains
    the text `--label`, and a naive test would report that shorter flag as
    supported and pass it to a driver that rejects it -- reintroducing exactly
    the usage-error abort this probe exists to prevent.
    """
    return re.search(rf"(?<![\w-]){re.escape(flag)}(?![\w-])", help_text) is not None


def driver_accepted_flags(
    checkout: Path,
    wanted: Sequence[str] = OPTIONAL_DRIVER_FLAGS,
    *,
    timeout: float = DRIVER_PROBE_TIMEOUT,
) -> tuple[list[str], str]:
    """ASK the target's validate driver which of `wanted` it accepts.

    RECORDED, NOT DERIVED. A commit-date or ordering comparison is an inference
    about what a binary supports; `--help` is the binary answering for itself.
    This file was already burned once by the derived form, and re-running could
    never have detected it -- the mistake reproduces identically every time.

    UNKNOWN IS NOT SUPPORT. When the driver cannot be asked, the flag is
    OMITTED, because the two errors are not symmetric: omitting a supported
    flag degrades behaviour a little, while passing an unsupported one aborts
    the run on a usage error before any test executes. The reason string is
    returned so the caller can log it -- an omission must never be silent.

    Returns (accepted flags in `wanted` order, one-line human reason).
    """
    # Resolve before exec: `cwd=checkout` would otherwise re-resolve a relative
    # program path against the new directory and report "driver absent" for a
    # driver that is present.
    driver = (checkout / "scripts/validate.rs").resolve()
    if not driver.is_file():
        return [], f"driver absent at {driver}; omitting {' '.join(wanted)}"
    try:
        probe = _run(
            (str(driver), "--help"),
            cwd=checkout,
            check=False,
            capture_output=True,
            timeout=timeout,
        )
    except (ProtocolError, OSError) as error:
        return [], f"driver could not be asked ({type(error).__name__}); omitting"
    # Usage text is worth reading whatever the exit status: a driver that
    # prints its options and exits nonzero has still told us what it accepts.
    help_text = f"{probe.stdout or ''}\n{probe.stderr or ''}"
    if not help_text.strip():
        return [], "driver answered with no usage text; omitting"
    accepted = [flag for flag in wanted if _driver_advertises(help_text, flag)]
    omitted = [flag for flag in wanted if flag not in accepted]
    return accepted, (
        f"driver accepts [{' '.join(accepted) or 'none'}]"
        f"; not advertised [{' '.join(omitted) or 'none'}]"
    )


def _local_run(
    obligation_id: str,
    source: Path,
    store_path: Path,
    *,
    launch_token: str | None = None,
) -> int:
    if launch_token is not None and not _register_local_runner(
        obligation_id, launch_token, store_path
    ):
        print(
            f"ci-hub obligation={obligation_id} local launch token is stale; exiting",
            file=sys.stderr,
            flush=True,
        )
        return 0
    record = obligations.get_record(obligation_id, store_path)
    workspace = ROOT / "ignored/ci-hub/obligations" / obligation_id
    checkout = workspace / "hermit"
    log_path = Path(record["local"]["log_path"])
    cost = record["local"]["cost"]
    estimate = cost["estimate"]
    cost_path = Path(cost["record_path"])
    started = time.monotonic()
    exit_code = 2
    classification_reason = "verification setup did not complete"
    receipt_verification: dict[str, Any] = {
        "state": "not_checked",
        "authority": "ci-hub-validate-status",
        "reason": "validate.sh did not complete",
    }
    # A setup that never reaches validate.sh (clone/checkout/submodule failure)
    # is infrastructure, not a product verdict: leave the leg no_result so it is
    # re-dispatched, never reverted on.
    state = "no_result"
    print(
        f"ci-hub obligation={obligation_id} repo={record['repo']} sha={record['landed_sha']} "
        f"started_at={obligations.utc_now()}",
        flush=True,
    )
    try:
        workspace.mkdir(parents=True, exist_ok=True)
        # This workspace is obligation-scoped ignored scratch. A re-dispatched
        # local leg (a prior attempt that came back no_result) must be able to
        # reclaim its own checkout; a stale one is not another owner's work.
        if checkout.exists():
            shutil.rmtree(checkout, ignore_errors=True)
        _run(
            ("git", "clone", "--shared", "--no-checkout", str(source), str(checkout)),
            check=True,
            capture_output=False,
        )
        _run(
            ("git", "-C", str(checkout), "checkout", "--detach", record["landed_sha"]),
            check=True,
            capture_output=False,
        )
        _run(
            (
                "with-proxy",
                "git",
                "-C",
                str(checkout),
                "submodule",
                "update",
                "--init",
                "--recursive",
            ),
            check=True,
            capture_output=False,
            timeout=DEFAULT_NETWORK_TIMEOUT,
        )
        actual = _run(
            ("git", "-C", str(checkout), "rev-parse", "HEAD"), check=True
        ).stdout.strip()
        if actual != record["landed_sha"]:
            raise ProtocolError(
                f"isolated checkout is {actual}, expected {record['landed_sha']}"
            )
        environment = os.environ.copy()
        environment.update(
            {
                "CI_HUB_OBLIGATION_ID": obligation_id,
                "VALIDATE_LABEL_PR": "0",
            }
        )
        estimate_arguments = (
            (
                "--estimate-wall-seconds",
                str(estimate["wall_seconds"]),
                "--estimate-cpu-seconds",
                str(estimate["cpu_seconds"]),
            )
            if estimate["kind"] == "derived"
            else ("--estimate-unknown",)
        )
        # Ask the driver AT THIS COMMIT which optional flags it takes, rather
        # than assuming the set this parent happens to know about. Printed
        # BEFORE log_offset on purpose: the line belongs in the durable log for
        # audit, but must stay outside the slice `_classify_local` reads so a
        # probe note can never colour a product verdict.
        driver_flags, driver_probe_reason = driver_accepted_flags(checkout)
        print(
            f"ci-hub obligation={obligation_id} driver-probe: {driver_probe_reason}",
            flush=True,
        )
        # Everything from here streams to the same log the watcher opened, so
        # capture the offset now: the bytes after it are exactly this validate
        # run's output, used to tell a product test failure from an
        # environmental (harness-caused) one.
        log_offset = log_path.stat().st_size if log_path.exists() else 0
        result = _run(
            (
                str(ROOT / "ci-hub/bin/tool-cost"),
                "--tool",
                "speculative-land/local-validate",
                *estimate_arguments,
                "--basis",
                str(estimate["basis"]),
                "--actual-json",
                str(cost_path),
                "--",
                # A validation-driver exit is not admission evidence. Route this
                # producer through the same non-overridable boundary as every
                # manual validate so the receipt carries canonical lock-owner
                # ancestry and concurrency proof.  validate-run deliberately
                # defaults to a fresh checkout of the exact target; do not add
                # --in-place here.
                str(ROOT / "ci-hub/ci-hub"),
                "validate-run",
                "--checkout",
                str(checkout),
                "--agent",
                f"obligation-{obligation_id}",
                "--target",
                record["landed_sha"],
                "--log",
                str(log_path),
                "--",
                *driver_flags,
            ),
            cwd=ROOT,
            check=False,
            capture_output=False,
            env=environment,
        )
        exit_code = result.returncode
        output = _read_local_output(log_path, log_offset)
        state, classification_reason = _classify_local(exit_code, output)
        receipt_ok, receipt_verification = verify_local_receipt(
            record["repo"], record["landed_sha"]
        )
        if state == "green" and not receipt_ok:
            # A bare validation-driver rc=0 is only a proxy. The canonical ledger
            # verifier must dereference a counted clean full/full receipt for
            # this exact SHA before the local leg carries a green authority.
            state = "no_result"
            classification_reason = (
                "canonical-receipt-refused:"
                f"{receipt_verification.get('reason') or 'unknown reason'}"
            )
        # Log every classification so an environmental downgrade is auditable and
        # an unattributed zero-test-failure red surfaces a candidate missing
        # signature (task cancellation_taxonomy_distinguish_self).
        print(
            f"ci-hub obligation={obligation_id} local classification: "
            f"state={state} reason={classification_reason} exit={exit_code}",
            flush=True,
        )
        try:
            measured_cost = json.loads(cost_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ProtocolError(
                f"tool-cost result is unavailable: {cost_path}: {error}"
            ) from error
        measured_cost["record_path"] = str(cost_path)
    except ProtocolError as error:
        print(f"local verification setup failed: {error}", file=sys.stderr, flush=True)
        state = "no_result"
        classification_reason = f"verification-setup-failed:{error}"
        measured_cost = cost
    finished_at = obligations.utc_now()
    obligations.transition(
        obligation_id,
        "local-completed",
        {
            "local": {
                "state": state,
                "finished_at": finished_at,
                "exit_code": exit_code,
                "duration_seconds": round(time.monotonic() - started, 3),
                "log_path": str(log_path),
                "cost": measured_cost,
                "classification_reason": classification_reason,
                "receipt_verification": receipt_verification,
            }
        },
        store_path,
    )
    record = evaluate_obligation(obligation_id, store_path=store_path)
    return 0 if record["local"]["state"] == "green" else 1


def _pid_alive(raw_pid: object) -> bool:
    if not isinstance(raw_pid, int) or raw_pid <= 0:
        return False
    try:
        os.kill(raw_pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _process_start_ticks(pid: int) -> int:
    try:
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
        close = stat.rfind(")")
        if close < 0:
            raise ValueError("missing command terminator")
        fields = stat[close + 2 :].split()
        return int(fields[19])
    except (OSError, IndexError, ValueError) as error:
        raise ProtocolError(f"cannot read process identity for pid {pid}: {error}") from error


def _current_boot_id() -> str:
    try:
        return Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    except OSError as error:
        raise ProtocolError(f"cannot read host boot identity: {error}") from error


def _watcher_process_identity(pid: int) -> dict[str, object]:
    return {
        "host": socket.gethostname(),
        "boot_id": _current_boot_id(),
        "start_ticks": _process_start_ticks(pid),
    }


class LaunchBusy(ProtocolError):
    """Another live process owns this obligation's launch reconciliation."""


def _register_local_runner(
    obligation_id: str,
    launch_token: str,
    store_path: Path,
    *,
    pid: int | None = None,
) -> bool:
    runner_pid = os.getpid() if pid is None else pid
    return (
        obligations.transition_if_matches(
            obligation_id,
            "local-runner-registered",
            {
                "local": {
                    "state": "running",
                    "pid": runner_pid,
                    "registered_at": obligations.utc_now(),
                }
            },
            {
                "local": {
                    "state": "starting",
                    "launch_token": launch_token,
                }
            },
            store_path,
        )
        is not None
    )


# How long a speculative-land obligation may sit UNRESOLVED before its silence
# stops being a normal post-merge window and becomes a hole.
#
# ARGUED FROM THE THING IT WAITS ON, NOT FROM HISTORICAL OBLIGATION LIFETIMES.
# I tried the obvious source first and it is contaminated: of 15 distinct
# obligations in the store only 9 carry both opened_at and satisfied_at, and
# their median lifetime is 235004s (2.7 DAYS) with a max of 296782s -- because
# nothing was resolving them, not because resolution legitimately takes days.
# Deriving a bound from that would either make the gate inert (2.7d) or fire on
# 89% of real obligations (900s). A contaminated denominator is worse than none.
#
# The honest reference is what an obligation actually BLOCKS ON: hosted CI
# reaching a verdict. Measured 2026-08-08 over the last 80 runs on
# rrnewton/hermit main (75 completed+success):
#     CI (GitHub-managed portable)  n=9  median 849s  MAX 1491s   <- the authority
#     P0 Demo Gate                  n=4  median 504s  max  610s
#     CI (privileged)               n=8  median 129s  max  185s
#     overall p95 1208s, overall MAX 1491s
# 1800s clears the slowest observed legitimate completion by 309s (~21% margin),
# the same margin shape as the github-main pending bound. Overridable so a slower
# reality can be accommodated without a code change.
#
# THE SAMPLE IS SMALL (n=9 for the authority) and I am not going to pretend
# otherwise. The asymmetry is what makes 1800s safe to pick anyway: too low costs
# a wasted check, and a genuine FAILURE is unaffected -- it pages at rc 2 at any
# age, through `remediation`, never through this bound.
OBLIGATION_STALE_AFTER_SECS = float(
    os.environ.get("CI_HUB_OBLIGATION_STALE_SECS", "1800")
)


def _obligation_age_secs(record: Mapping[str, Any], now: datetime | None = None):
    """Seconds since the obligation opened, or None if that cannot be read."""
    opened = record.get("opened_at")
    if not isinstance(opened, str) or not opened:
        return None
    try:
        when = datetime.fromisoformat(opened.replace("Z", "+00:00"))
    except ValueError:
        return None
    moment = now or datetime.now(timezone.utc)
    return (moment - when).total_seconds()


def _stale_obligations(
    unresolved: Sequence[Mapping[str, Any]],
    *,
    stale_after: float | None = None,
    now: datetime | None = None,
) -> list:
    """Unresolved obligations that have outlived their normal window.

    An obligation whose `opened_at` cannot be read is treated as STALE. That is
    the fail-closed direction and it is the right one here: this bound exists to
    SUPPRESS a page, so anything it cannot age must not get the suppression.
    Note the asymmetry with the refire futility probe, which fails OPEN -- there
    the probe withheld an action, here it withholds an alarm.
    """
    bound = OBLIGATION_STALE_AFTER_SECS if stale_after is None else stale_after
    stale = []
    for record in unresolved:
        age = _obligation_age_secs(record, now)
        if age is None or age > bound:
            stale.append(record)
    return stale


def _register_watcher(
    obligation_id: str,
    launch_token: str,
    store_path: Path,
    *,
    pid: int | None = None,
) -> bool:
    watcher_pid = os.getpid() if pid is None else pid
    identity = _watcher_process_identity(watcher_pid)
    return (
        obligations.transition_if_matches(
            obligation_id,
            "watcher-registered",
            {
                "watcher": {
                    "state": "running",
                    "pid": watcher_pid,
                    "started_at": obligations.utc_now(),
                    "identity": identity,
                }
            },
            {
                "watcher": {
                    "state": "starting",
                    "launch_token": launch_token,
                }
            },
            store_path,
        )
        is not None
    )


def _complete_watcher(obligation_id: str, launch_token: str, store_path: Path) -> None:
    obligations.transition_if_matches(
        obligation_id,
        "watcher-completed",
        {
            "watcher": {
                "state": "completed",
                "pid": None,
                "exit_code": 0,
                "finished_at": obligations.utc_now(),
            }
        },
        {
            "watcher": {
                "state": "running",
                "launch_token": launch_token,
                "pid": os.getpid(),
            }
        },
        store_path,
    )


def _fail_watcher(
    obligation_id: str,
    launch_token: str,
    store_path: Path,
    error: BaseException,
) -> None:
    obligations.transition_if_matches(
        obligation_id,
        "watcher-failed",
        {
            "watcher": {
                "state": "failed",
                "pid": None,
                "exit_code": 2,
                "finished_at": obligations.utc_now(),
                "last_error": str(error),
            }
        },
        {
            "watcher": {
                "state": "running",
                "launch_token": launch_token,
                "pid": os.getpid(),
            }
        },
        store_path,
    )


def _local_launch_durable(record: Mapping[str, Any]) -> bool:
    local = record.get("local")
    if not isinstance(local, Mapping):
        return False
    if _local_policy_skip_valid(record):
        return True
    state = local.get("state")
    if state == "running":
        registered = bool(local.get("registered_at"))
        legacy_registered = local.get("launch_token") is None and bool(
            local.get("started_at")
        )
        return (registered or legacy_registered) and _pid_alive(local.get("pid"))
    terminal_is_registered = (
        all(
            isinstance(local.get(field), str) and bool(local[field].strip())
            for field in ("launch_token", "registered_at", "started_at", "finished_at")
        )
        and type(local.get("pid")) is int
        and local["pid"] > 0
    )
    if not terminal_is_registered:
        return False
    if state == "green":
        return (
            isinstance(record.get("repo"), str)
            and isinstance(record.get("landed_sha"), str)
            and _persisted_local_receipt_valid(
                local.get("receipt_verification"),
                repo=str(record["repo"]),
                sha=str(record["landed_sha"]),
            )
        )
    return state in {"red", "no_result", "error"}


def _watcher_launch_durable(record: Mapping[str, Any]) -> bool:
    watcher = record.get("watcher")
    if not isinstance(watcher, Mapping):
        return False
    state = watcher.get("state")
    # Compatibility for records written before watcher.state existed: a live,
    # timestamped PID is still observable producer evidence.
    if state in {None, "running"}:
        return bool(watcher.get("started_at")) and _pid_alive(watcher.get("pid"))
    if state == "cancelled":
        cancellation = watcher.get("cancellation")
        return (
            record.get("overall_state") in obligations.CLOSED_STATES
            and _watch_complete(record)
            and bool(watcher.get("started_at") and watcher.get("finished_at"))
            and watcher.get("pid") is None
            and isinstance(cancellation, Mapping)
            and cancellation.get("state") == "completed"
        )
    return (
        state == "completed"
        and bool(watcher.get("started_at") and watcher.get("finished_at"))
        and watcher.get("exit_code") == 0
        and _watch_complete(record)
    )


def _components_launch_durable(record: Mapping[str, Any]) -> bool:
    return _local_launch_durable(record) and _watcher_launch_durable(record)


def obligation_launch_durable(record: Mapping[str, Any]) -> bool:
    launch = record.get("launch")
    return (
        isinstance(launch, Mapping)
        and launch.get("state") == "armed"
        and _components_launch_durable(record)
    )


def _claim_obligation_launch(
    obligation_id: str, store_path: Path
) -> tuple[str, dict[str, Any], str | None]:
    for _ in range(4):
        record = obligations.get_record(obligation_id, store_path)
        if obligation_launch_durable(record):
            return "armed", record, None
        launch = record.get("launch")
        launch = launch if isinstance(launch, Mapping) else {}
        launcher_pid = launch.get("launcher_pid")
        if launch.get("state") == "launching" and _pid_alive(launcher_pid):
            raise LaunchBusy(
                f"obligation {obligation_id} launch is owned by live pid {launcher_pid}"
            )
        token = uuid.uuid4().hex
        claimed = obligations.transition_if_matches(
            obligation_id,
            "launch-claimed",
            {
                "launch": {
                    "state": "launching",
                    "token": token,
                    "launcher_pid": os.getpid(),
                    "attempt": int(launch.get("attempt") or 0) + 1,
                    "started_at": obligations.utc_now(),
                    "armed_at": None,
                    "last_error": None,
                }
            },
            {"event_id": record["event_id"]},
            store_path,
        )
        if claimed is not None:
            return "owner", claimed, token
    raise ProtocolError(f"could not atomically claim obligation {obligation_id}")


def _release_obligation_launch(
    obligation_id: str,
    launch_token: str,
    store_path: Path,
    error: BaseException,
) -> None:
    obligations.transition_if_matches(
        obligation_id,
        "launch-repairable",
        {
            "launch": {
                "state": "repairable",
                "launcher_pid": None,
                "last_error": str(error),
            }
        },
        {"launch": {"state": "launching", "token": launch_token}},
        store_path,
    )


def _wait_for_registration(
    obligation_id: str,
    component: str,
    launch_token: str,
    store_path: Path,
    *,
    timeout: float = LAUNCH_REGISTRATION_TIMEOUT,
    sleep: Callable[[float], None] = time.sleep,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    while True:
        record = obligations.get_record(obligation_id, store_path)
        observed = record.get(component)
        if not isinstance(observed, Mapping):
            raise ProtocolError(f"obligation {obligation_id} has no {component} state")
        if observed.get("launch_token") != launch_token:
            raise ProtocolError(
                f"obligation {obligation_id} {component} launch token was superseded"
            )
        if observed.get("state") in {
            "running",
            "completed",
            "green",
            "red",
            "no_result",
            "error",
        }:
            return record
        if time.monotonic() >= deadline:
            raise ProtocolError(
                f"obligation {obligation_id} {component} did not register within {timeout}s"
            )
        sleep(min(0.05, max(0.0, deadline - time.monotonic())))


def _ensure_local_launched(
    obligation_id: str, source: Path, store_path: Path
) -> dict[str, Any]:
    record = obligations.get_record(obligation_id, store_path)
    if record.get("repo") != DEFAULT_REPO:
        if _local_policy_skip_valid(record):
            return record
        skipped = obligations.transition_if_matches(
            obligation_id,
            "local-policy-skipped",
            {"local": _local_policy_skip_patch(str(record.get("repo")))},
            {"event_id": record["event_id"]},
            store_path,
        )
        if skipped is None:
            return _ensure_local_launched(obligation_id, source, store_path)
        return skipped
    if _local_launch_durable(record):
        return record
    workspace = ROOT / "ignored/ci-hub/obligations" / obligation_id
    local_log = workspace / "local-validate.log"
    local_cost = workspace / "local-validate-cost.json"
    cost = record.get("local", {}).get("cost") or {}
    estimate = cost.get("estimate") or estimate_local_validate_cost()
    launch_token = uuid.uuid4().hex
    prepared = obligations.transition_if_matches(
        obligation_id,
        "local-prepared",
        {
            "local": {
                "state": "starting",
                "started_at": obligations.utc_now(),
                "finished_at": None,
                "pid": None,
                "launch_token": launch_token,
                "registered_at": None,
                "log_path": str(local_log),
                "workspace": str(workspace / "hermit"),
                "source": str(source),
                "redispatch_count": int(
                    record.get("local", {}).get("redispatch_count") or 0
                ),
                "receipt_verification": None,
                "cost": {
                    "estimate": estimate,
                    "actual": None,
                    "record_path": str(local_cost),
                },
            }
        },
        {"event_id": record["event_id"]},
        store_path,
    )
    if prepared is None:
        return _ensure_local_launched(obligation_id, source, store_path)
    _spawn_detached(
        (
            "_local-run",
            obligation_id,
            "--launch-token",
            launch_token,
            "--source",
            str(source),
            "--store",
            str(store_path),
        ),
        local_log,
    )
    return _wait_for_registration(obligation_id, "local", launch_token, store_path)


def _ensure_watcher_launched(
    obligation_id: str, store_path: Path, poll_seconds: int
) -> dict[str, Any]:
    record = obligations.get_record(obligation_id, store_path)
    if _watcher_launch_durable(record):
        return record
    workspace = ROOT / "ignored/ci-hub/obligations" / obligation_id
    watcher_log = workspace / "watcher.log"
    launch_token = uuid.uuid4().hex
    prepared = obligations.transition_if_matches(
        obligation_id,
        "watcher-prepared",
        {
            "watcher": {
                "state": "starting",
                "pid": None,
                "launch_token": launch_token,
                "log_path": str(watcher_log),
                "started_at": None,
                "finished_at": None,
            }
        },
        {"event_id": record["event_id"]},
        store_path,
    )
    if prepared is None:
        return _ensure_watcher_launched(obligation_id, store_path, poll_seconds)
    _spawn_detached(
        (
            "watch",
            "--id",
            obligation_id,
            "--launch-token",
            launch_token,
            "--poll-seconds",
            str(poll_seconds),
            "--store",
            str(store_path),
        ),
        watcher_log,
    )
    return _wait_for_registration(obligation_id, "watcher", launch_token, store_path)


def resume_obligation_launch(
    obligation_id: str,
    *,
    source: Path,
    store_path: Path,
    github_wait_seconds: int,
    poll_seconds: int,
    allow_dispatch: bool,
) -> tuple[dict[str, Any], ProtocolError | None]:
    status, record, launch_token = _claim_obligation_launch(obligation_id, store_path)
    if status == "armed":
        return record, None
    assert launch_token is not None
    github_error: ProtocolError | None = None
    try:
        _ensure_local_launched(obligation_id, source, store_path)
        _ensure_watcher_launched(obligation_id, store_path, poll_seconds)
        try:
            ensure_github_verification(
                obligation_id,
                store_path=store_path,
                wait_seconds=github_wait_seconds,
                allow_dispatch=allow_dispatch,
            )
        except ProtocolError as error:
            github_error = error
            obligations.transition(
                obligation_id,
                "github-arm-error",
                {
                    "github": {
                        "state": "no_result",
                        "finished_at": None,
                        "last_poll_error": str(error),
                    }
                },
                store_path,
            )
            evaluate_obligation(obligation_id, store_path=store_path)
        record = obligations.get_record(obligation_id, store_path)
        if not _components_launch_durable(record):
            raise ProtocolError(
                f"obligation {obligation_id} verifier/watcher launch is not durable"
            )
        armed = obligations.transition_if_matches(
            obligation_id,
            "launch-armed",
            {
                "launch": {
                    "state": "armed",
                    "launcher_pid": None,
                    "armed_at": obligations.utc_now(),
                    "last_error": str(github_error) if github_error else None,
                }
            },
            {"launch": {"state": "launching", "token": launch_token}},
            store_path,
        )
        if armed is None:
            raise ProtocolError(
                f"obligation {obligation_id} lost its launch ownership token"
            )
        return armed, github_error
    except BaseException as error:
        _release_obligation_launch(obligation_id, launch_token, store_path, error)
        raise


def _github_verification_in_flight(github: object) -> bool:
    if not isinstance(github, Mapping) or github.get("state") not in {
        "pending",
        "running",
    }:
        return False
    run_ids = github.get("run_ids")
    jobs = github.get("jobs")
    dereferenced_run_ids = (
        {run_id for run_id in run_ids if type(run_id) is int and run_id > 0}
        if isinstance(run_ids, list)
        else set()
    )
    if not dereferenced_run_ids or not isinstance(jobs, list):
        return False
    for job in jobs:
        if not isinstance(job, Mapping):
            continue
        state = job.get("state")
        status = str(job.get("status") or "").lower()
        producer_status_matches = (
            state == "pending"
            and status in {"requested", "waiting", "queued", "pending"}
        ) or (state == "running" and status == "in_progress")
        if (
            producer_status_matches
            and type(job.get("run_id")) is int
            and job["run_id"] in dereferenced_run_ids
            and type(job.get("job_id")) is int
            and job["job_id"] > 0
        ):
            return True
    return False


def _verification_in_flight(record: Mapping[str, Any]) -> bool:
    local = record.get("local")
    local_registered = isinstance(local, Mapping) and (
        bool(local.get("registered_at"))
        or (local.get("launch_token") is None and bool(local.get("started_at")))
    )
    local_in_flight = (
        isinstance(local, Mapping)
        and local.get("state") == "running"
        and local_registered
        and isinstance(local.get("pid"), int)
        and local["pid"] > 0
        and _pid_alive(local["pid"])
    )
    return local_in_flight or _github_verification_in_flight(record.get("github"))


def _verification_state_needs_reconcile(record: Mapping[str, Any]) -> bool:
    """A producer-looking state without observable producer identity."""
    local = record.get("local")
    github = record.get("github")
    local_state = local.get("state") if isinstance(local, Mapping) else None
    github_state = github.get("state") if isinstance(github, Mapping) else None
    looks_active = local_state in {
        "pending",
        "starting",
        "running",
    } or github_state in {
        "pending",
        "running",
    }
    return looks_active and not _verification_in_flight(record)


def poll_obligation(obligation_id: str, store_path: Path) -> dict[str, Any]:
    record = obligations.get_record(obligation_id, store_path)
    if record["overall_state"] in obligations.CLOSED_STATES and not (
        record["overall_state"] == "satisfied"
        and (
            _verification_in_flight(record)
            or _verification_state_needs_reconcile(record)
        )
    ):
        return record
    if (
        record.get("overall_state") == "investigation_required"
        and record.get("failure_source") == "verification_policy"
    ):
        return record
    # A persisted exact-SHA hosted green is already sufficient authority.
    # Evaluate it before trying to fill a supplemental local no_result;
    # otherwise a burst of already-passing obligations needlessly launches
    # local validation and can exhaust the one-shot watcher's wall budget
    # before recording the satisfied transitions.  Keep polling the opposite
    # orientation (local green / hosted no_result), because a late hosted red is
    # an authoritative disagreement that must still be observed.  Active or
    # stale producer-looking states also take the reconciliation path below.
    if (
        record["github"]["state"] == "green"
        and record["local"]["state"] == "no_result"
        and not _verification_in_flight(record)
        and not _verification_state_needs_reconcile(record)
    ):
        record = evaluate_obligation(obligation_id, store_path=store_path)
        if record["overall_state"] == "satisfied":
            return record
    policy: dict[str, Any] | None = None
    try:
        record, policy = bind_verification_policy(obligation_id, store_path)
    except ProtocolError as error:
        return _record_policy_investigation(record, error, store_path)
    if (
        policy is not None
        and record["github"]["state"] not in TERMINAL_VERIFICATION_STATES
    ):
        try:
            runs = github_runs(record["repo"], record["landed_sha"], policy=policy)
            patch = _github_patch(runs, record["landed_sha"], policy)
            if record.get("github") != patch["github"]:
                record = obligations.transition(
                    obligation_id,
                    "github-polled",
                    patch,
                    store_path,
                )
        except ProtocolError as error:
            record = obligations.transition(
                obligation_id,
                "github-poll-error",
                {"github": {"last_poll_error": str(error)}},
                store_path,
            )
    if record["local"]["state"] in {"pending", "starting"}:
        launch = record.get("launch")
        launch = launch if isinstance(launch, Mapping) else {}
        if not (
            launch.get("state") == "launching"
            and _pid_alive(launch.get("launcher_pid"))
        ):
            record = obligations.transition(
                obligation_id,
                "local-producer-absent",
                {
                    "local": {
                        "state": "no_result",
                        "finished_at": obligations.utc_now(),
                        "exit_code": 2,
                        "classification_reason": (
                            "pending/starting state had no durable live producer"
                        ),
                    }
                },
                store_path,
            )
    if record["local"]["state"] == "running" and not _pid_alive(
        record["local"].get("pid")
    ):
        record = obligations.get_record(obligation_id, store_path)
        if record["local"]["state"] == "running":
            # A runner that vanished never returned a verdict: it is a hole to
            # re-dispatch, never a red to revert on.
            record = obligations.transition(
                obligation_id,
                "local-runner-lost",
                {
                    "local": {
                        "state": "no_result",
                        "finished_at": obligations.utc_now(),
                        "exit_code": 2,
                    }
                },
                store_path,
            )
    record = _maybe_redispatch_local(obligation_id, record, store_path)
    return evaluate_obligation(obligation_id, store_path=store_path)


def _maybe_redispatch_local(
    obligation_id: str, record: Mapping[str, Any], store_path: Path
) -> dict[str, Any]:
    """Re-run the local leg to fill a hole or reproduce a provisional red.

    A no_result (OOM/SIGKILL, a lost runner, a setup failure) is a hole; a lone
    local red not yet corroborated by GitHub is provisional. Both are re-dispatched
    — never reverted on — until the leg resolves green, GitHub corroborates a red,
    or the bounded budget is spent (a spent red is then confirmed by
    ``_remediation_ready``; a spent no_result stays an unresolved hole, still never
    a revert).
    """
    local = record["local"]
    if record.get("repo") != DEFAULT_REPO:
        if _local_policy_skip_valid(record):
            return dict(record)
        return obligations.transition(
            obligation_id,
            "local-policy-skipped",
            {"local": _local_policy_skip_patch(str(record.get("repo")))},
            store_path,
        )
    state = local.get("state")
    if state not in {"no_result", "red"}:
        return dict(record)
    if state == "red" and record["github"]["state"] == "red":
        return dict(record)  # GitHub already corroborates the red; let it remediate
    spent = int(local.get("redispatch_count") or 0)
    if spent >= DEFAULT_LOCAL_REDISPATCH_LIMIT:
        return dict(record)
    if _pid_alive(local.get("pid")):
        return dict(record)  # a runner is already producing the next answer
    source = local.get("source")
    if not source or not Path(source).is_dir():
        return dict(record)  # cannot re-run without the donor checkout
    admitted, admission_detail = _redispatch_watch_admission()
    if not admitted:
        # Do not consume a retry or even start the clone/submodule preparation.
        # That preparation took 28s in the live 069b532f redispatch and aged the
        # shared lock's bounded yield window past 90s before validate-run reached
        # the box chokepoint.  The downstream validate-lock check remains the
        # race-safe authority; this early check makes automation obey the same
        # free-boundary priority at its actual dispatch decision.
        print(
            f"LOCAL RE-DISPATCH YIELD: obligation={obligation_id} "
            f"reason={admission_detail}",
            file=sys.stderr,
            flush=True,
        )
        return dict(record)
    log_path = Path(
        local.get("log_path")
        or ROOT / "ignored/ci-hub/obligations" / obligation_id / "local-validate.log"
    )
    launch_token = uuid.uuid4().hex
    claimed = obligations.transition_if_matches(
        obligation_id,
        "local-redispatch-claimed",
        {
            "local": {
                "state": "starting",
                "pid": None,
                "launch_token": launch_token,
                "registered_at": None,
                "finished_at": None,
                "redispatch_count": spent + 1,
            }
        },
        {"event_id": record["event_id"]},
        store_path,
    )
    if claimed is None:
        return obligations.get_record(obligation_id, store_path)
    _spawn_detached(
        (
            "_local-run",
            obligation_id,
            "--launch-token",
            launch_token,
            "--source",
            str(source),
            "--store",
            str(store_path),
        ),
        log_path,
    )
    reason = (
        "reproduce a provisional local red"
        if state == "red"
        else "fill a local no_result hole"
    )
    print(
        f"LOCAL RE-DISPATCH: obligation={obligation_id} attempt={spent + 1} "
        f"reason={reason} token={launch_token}",
        file=sys.stderr,
        flush=True,
    )
    return _wait_for_registration(
        obligation_id,
        "local",
        launch_token,
        store_path,
    )


def _redispatch_watch_admission(
    *,
    release_path: Path | None = None,
    verifier: Path | None = None,
) -> tuple[bool, str]:
    """Ask the public owner-watch predicate before automatic redispatch.

    This does not duplicate admission semantics.  It only obtains the latest
    release boundary and calls ``owner-checkout-validate-watch.py``'s public
    three-state interface.  Missing release state is the documented bootstrap
    case; malformed/unreadable state and an UNKNOWN verifier answer fail closed.
    ``validate-lock run`` asks the same predicate again at the anchor boundary.
    """
    release_path = release_path or ROOT / ".validate-lock.released"
    verifier = verifier or ROOT / "scripts/owner-checkout-validate-watch.py"
    try:
        release_text = release_path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return True, "bootstrap-no-release-yet"
    except OSError as error:
        return False, f"release-record-unreadable:{error}"
    try:
        release = dict(
            line.split("=", 1)
            for line in release_text.splitlines()
            if line.strip()
        )
        released_at = release["released_at"]
        if not released_at.isdigit() or int(released_at) < 0:
            raise ValueError("released_at is not a non-negative epoch")
        released_by = release["released_by"]
        if not released_by:
            raise ValueError("released_by is empty")
    except (KeyError, ValueError) as error:
        return False, f"release-record-malformed:{error}"

    # A release by the priority watcher is itself the strongest possible proof
    # that it was admitted.  Its public cursor records acquisition before this
    # later release boundary, so querying strictly after the release would
    # otherwise manufacture starvation immediately after the watch got its turn.
    if released_by == "owner-checkout-watch":
        return True, "owner-watch-released-last"

    try:
        result = _run(
            (str(verifier), "--admission-status-since", released_at),
            check=False,
            timeout=5,
        )
        payload = json.loads(result.stdout)
    except (OSError, ProtocolError, json.JSONDecodeError) as error:
        return False, f"owner-watch-admission-unknown:{error}"
    state = payload.get("state") if isinstance(payload, Mapping) else None
    contract = (result.returncode, state)
    if contract == (0, "ADMITTED"):
        return True, "owner-watch-admitted-since-release"
    if contract == (1, "NOT_ADMITTED"):
        return False, "owner-watch-not-admitted-since-release"
    if contract == (2, "UNKNOWN"):
        return False, "owner-watch-admission-unknown"
    return False, f"owner-watch-admission-contract-mismatch:{contract!r}"


def _watch_complete(record: Mapping[str, Any]) -> bool:
    if (
        record.get("overall_state") == "investigation_required"
        and record.get("failure_source") == "verification_policy"
    ):
        return True
    if record["overall_state"] == "satisfied":
        return not _verification_in_flight(
            record
        ) and not _verification_state_needs_reconcile(record)
    if record["overall_state"] in obligations.CLOSED_STATES:
        return True
    return all(
        record[source]["state"] in TERMINAL_VERIFICATION_STATES
        for source in ("local", "github")
    )


def _poll_within_budget(
    records: Sequence[Mapping[str, Any]],
    store_path: Path,
    deadline: float | None,
) -> tuple[list[dict[str, Any]], int, bool]:
    """Poll obligations one at a time, stopping when the wall budget is spent.

    Returns ``(updated, planned, timed_out)``. Polling one at a time -- rather
    than the list comprehension this replaces -- is what lets a typed NO-RESULT
    retain its partial rows: the old form had to finish every obligation before
    anything could be reported, so an outer kill discarded the ones already done.

    Per-op wall and CPU go to stdout as ``#`` comment lines. That is deliberate:
    tick-hub's ``parse_kv_lines`` ignores ``#``, so the timing basis rides along
    in the captured output for a human without polluting the gate's fields. The
    flat-user/exploding-system split is the entire diagnostic for this gate and
    was previously invisible.
    """
    updated: list[dict[str, Any]] = []
    for record in records:
        if deadline is not None and time.monotonic() >= deadline:
            return updated, len(records), True
        op_wall = time.monotonic()
        op_cpu = time.process_time()
        updated.append(poll_obligation(record["obligation_id"], store_path))
        print(
            f"# poll {record['obligation_id']} "
            f"wall={time.monotonic() - op_wall:.2f}s "
            f"cpu={time.process_time() - op_cpu:.2f}s"
        )
        # A single blocking remote call can cross the deadline.  Checking only
        # before the next item made an over-budget final (or only) poll look
        # complete because there was no next iteration at which to notice.
        if deadline is not None and time.monotonic() >= deadline:
            return updated, len(records), True
    return updated, len(records), False


def watch(
    *,
    store_path: Path,
    obligation_id: str | None,
    once: bool,
    poll_seconds: int,
    budget_secs: float | None = None,
) -> int:
    started = time.monotonic()
    deadline = None if budget_secs is None else started + budget_secs
    while True:
        records = (
            [obligations.get_record(obligation_id, store_path)]
            if obligation_id
            else sorted(
                (
                    record
                    for record in obligations.latest_records(store_path).values()
                    if record.get("overall_state") not in obligations.CLOSED_STATES
                    or _verification_in_flight(record)
                    or _verification_state_needs_reconcile(record)
                ),
                key=lambda record: (
                    str(record.get("opened_at", "")),
                    record["obligation_id"],
                ),
            )
        )
        updated, planned, timed_out = _poll_within_budget(
            records, store_path, deadline
        )
        if timed_out:
            # TYPED, NON-CRASHING NO-RESULT. We stop under our own power and
            # report what we actually checked, so the consumer can distinguish
            # an unavailable answer from CLEAR and from a completed OPEN tick.
            elapsed_ms = round((time.monotonic() - started) * 1000)
            bound_ms = round((budget_secs or 0) * 1000)
            print(
                f"WATCH OBLIGATIONS: NO-RESULT checked={len(updated)} of "
                f"planned={planned} elapsed_ms={elapsed_ms} bound_ms={bound_ms}"
            )
            print("state=no-result")
            print("verdict=NO-RESULT")
            print("watch_status=no-result")
            print("watch_verdict=NO-RESULT")
            print(f"watch_planned={planned}")
            print(f"watch_checked={len(updated)}")
            print("watch_timed_out=true")
            print(f"elapsed_ms={elapsed_ms}")
            print(f"bound_ms={bound_ms}")
            print(
                "summary="
                f"NO-RESULT: watch-obligations timed out after {elapsed_ms}ms "
                f"against {bound_ms}ms; checked {len(updated)} of {planned}"
            )
            for record in updated:
                print(f"  {_summary_line(record)}")
            return WATCH_EXIT_NO_RESULT
        if once or all(_watch_complete(record) for record in updated):
            remediation = sum(
                record["overall_state"] == "remediation_required" for record in updated
            )
            unresolved = sum(
                record["overall_state"] not in obligations.CLOSED_STATES
                for record in updated
            )
            watch_verdict = (
                "REMEDIATION-REQUIRED"
                if remediation
                else "OPEN"
                if unresolved
                else "CLEAR"
            )
            print("watch_status=complete")
            print(f"watch_verdict={watch_verdict}")
            print(f"watch_planned={planned}")
            print(f"watch_checked={len(updated)}")
            print("watch_timed_out=false")
            print(f"elapsed_ms={round((time.monotonic() - started) * 1000)}")
            print(
                f"WATCH OBLIGATIONS: checked={len(updated)} "
                f"unresolved={unresolved} remediation_required={remediation}"
            )
            for record in updated:
                print(f"  {_summary_line(record)}")
            if any(
                record["overall_state"] == "remediation_required" for record in updated
            ):
                return 2
            return (
                1
                if any(
                    record["overall_state"] not in obligations.CLOSED_STATES
                    for record in updated
                )
                else 0
            )
        time.sleep(poll_seconds)


def _load_decision_waits(path: Path) -> dict[str, dict[str, str]]:
    """Load reporting-only human-decision blockers, bound to obligation identity.

    These annotations do not transition or discharge an obligation.  They only
    let the reporting layer distinguish "nothing is known" from "progress is
    waiting on this named decision".  Missing configuration means no
    de-escalation; malformed configuration refuses rather than hiding a page.
    """
    if not path.exists():
        return {}
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ProtocolError(
            f"cannot read decision-wait registry {path}: {error}"
        ) from error
    if not isinstance(document, Mapping) or document.get("schema_version") != 1:
        raise ProtocolError(f"decision-wait registry {path} must use schema_version=1")
    entries = document.get("obligations")
    if not isinstance(entries, Mapping):
        raise ProtocolError(f"decision-wait registry {path} has no obligations object")

    waits: dict[str, dict[str, str]] = {}
    for obligation_id, entry in entries.items():
        if not isinstance(obligation_id, str) or not obligation_id:
            raise ProtocolError(
                f"decision-wait registry {path} has an invalid obligation id"
            )
        if not isinstance(entry, Mapping):
            raise ProtocolError(
                f"decision-wait registry {path} entry {obligation_id!r} is not an object"
            )
        repo = entry.get("repo")
        landed_sha = entry.get("landed_sha")
        blocker = entry.get("blocker")
        if (
            entry.get("state") != DECISION_WAIT_STATE
            or not isinstance(repo, str)
            or not obligations.REPO_RE.fullmatch(repo)
            or not isinstance(landed_sha, str)
            or not obligations.SHA_RE.fullmatch(landed_sha)
            or not isinstance(blocker, str)
            or not blocker.strip()
            or "\n" in blocker
            or "\r" in blocker
        ):
            raise ProtocolError(
                f"decision-wait registry {path} entry {obligation_id!r} must bind "
                "state=AWAITING-DECISION, repo, landed_sha, and a one-line blocker"
            )
        waits[obligation_id] = {
            "state": DECISION_WAIT_STATE,
            "repo": repo,
            "landed_sha": landed_sha,
            "blocker": blocker.strip(),
        }
    return waits


def _bound_decision_wait(
    record: Mapping[str, Any], waits: Mapping[str, Mapping[str, str]]
) -> Mapping[str, str] | None:
    entry = waits.get(str(record.get("obligation_id", "")))
    if entry is None:
        return None
    if (
        entry.get("repo") != record.get("repo")
        or entry.get("landed_sha") != record.get("landed_sha")
    ):
        raise ProtocolError(
            "decision-wait identity mismatch for obligation "
            f"{record.get('obligation_id', '-')}: registry binds "
            f"{entry.get('repo')}@{entry.get('landed_sha')}, record is "
            f"{record.get('repo')}@{record.get('landed_sha')}"
        )
    return entry


def _summary_line(
    record: Mapping[str, Any], decision_wait: Mapping[str, str] | None = None
) -> str:
    recommendation = record.get("recommendation") or {}
    action = (
        recommendation.get("action", "-")
        if isinstance(recommendation, Mapping)
        else "-"
    )
    remediation = record.get("remediation") or {}
    remediation_state = (
        DECISION_WAIT_STATE
        if decision_wait is not None
        else remediation.get("state", "-")
        if isinstance(remediation, Mapping)
        else "-"
    )
    dispatch = remediation.get("dispatch") if isinstance(remediation, Mapping) else None
    dispatch_state = (
        dispatch.get("state", "-") if isinstance(dispatch, Mapping) else "-"
    )
    blocker = (
        f" blocker={json.dumps(decision_wait['blocker'])}"
        if decision_wait is not None
        else ""
    )
    return (
        f"{record['obligation_id']} {record['repo']}@{record['landed_sha'][:12]} "
        f"overall={record['overall_state']} local={record['local']['state']} "
        f"github={record['github']['state']} recommendation={action} "
        f"remediation={remediation_state} dispatch={dispatch_state}{blocker}"
    )


def print_status(
    store_path: Path,
    *,
    include_closed: bool,
    json_output: bool,
    gate: bool,
    actionable_only: bool = False,
    awaiting_decision_only: bool = False,
    decision_waits_path: Path = DEFAULT_DECISION_WAITS_PATH,
) -> int:
    if actionable_only and awaiting_decision_only:
        raise ProtocolError("--actionable and --awaiting-decision are mutually exclusive")
    decision_wait_registry = _load_decision_waits(decision_waits_path)
    records = list(obligations.latest_records(store_path).values())
    if actionable_only:
        records = [
            record
            for record in records
            if record.get("overall_state") == "remediation_required"
        ]
    if not include_closed:
        records = [
            record
            for record in records
            if record["overall_state"] not in obligations.CLOSED_STATES
        ]
    records.sort(key=lambda record: (record["opened_at"], record["obligation_id"]))
    unresolved = [
        record
        for record in records
        if record["overall_state"] not in obligations.CLOSED_STATES
    ]
    remediation = [
        record
        for record in unresolved
        if record["overall_state"] == "remediation_required"
    ]
    # An obligation that is merely RUNNING is the normal state for the minutes
    # after any merge -- local validation in flight, hosted CI not yet reporting.
    # Paging on it made this gate fire on every successful land, and a gate that
    # cries wolf on success is how a fleet learns to skim past the one that
    # matters. Only a SUSTAINED-unresolved obligation is a finding; `remediation`
    # above is unaffected and still pages at any age, because a failure is a
    # finding the moment it exists.
    stale = _stale_obligations(unresolved)
    decision_waits: dict[str, Mapping[str, str]] = {}
    for record in stale:
        # A real remediation result always wins over a reporting annotation.
        # The annotation describes why an otherwise-unclassified OPEN record is
        # waiting; it must never suppress a concrete fix-forward/revert action.
        if record.get("overall_state") != "remediation_required":
            wait = _bound_decision_wait(record, decision_wait_registry)
            if wait is not None:
                decision_waits[record["obligation_id"]] = wait
    stale_actionable = [
        record for record in stale if record["obligation_id"] not in decision_waits
    ]
    if awaiting_decision_only:
        records = [
            record for record in records if record["obligation_id"] in decision_waits
        ]
        unresolved = records
        remediation = []
        stale = records
        stale_actionable = []
    triggered = [
        record
        for record in remediation
        if (record.get("remediation") or {}).get("state") == "triggered"
    ]
    sent_unacknowledged = [
        record
        for record in remediation
        if ((record.get("remediation") or {}).get("dispatch") or {}).get("state")
        == "sent_unacknowledged"
    ]
    acknowledged = [
        record
        for record in remediation
        if ((record.get("remediation") or {}).get("dispatch") or {}).get("state")
        == "acknowledged"
    ]
    if json_output:
        print(json.dumps({"obligations": records}, sort_keys=True))
    elif gate:
        state = (
            "remediation-required"
            if remediation
            else "stale-open"
            if stale_actionable
            else "awaiting-decision"
            if decision_waits
            else "running"
            if unresolved
            else "clear"
        )
        print(f"state={state}")
        print(f"count={len(unresolved)}")
        print(f"remediation_count={len(remediation)}")
        print(f"stale_count={len(stale)}")
        print(f"awaiting_decision_count={len(decision_waits)}")
        print(f"stale_after_secs={int(OBLIGATION_STALE_AFTER_SECS)}")
        print(f"triggered_count={len(triggered)}")
        print(f"sent_unacknowledged_count={len(sent_unacknowledged)}")
        print(f"acknowledged_count={len(acknowledged)}")
        print(
            "ids="
            + (",".join(record["obligation_id"] for record in unresolved) or "none")
        )
        print(
            "summary="
            + (
                ";".join(
                    _summary_line(record, decision_waits.get(record["obligation_id"]))
                    for record in unresolved
                )
                if unresolved
                else "no-open-speculative-land-obligations"
            )
        )
    else:
        heading = (
            "Speculative-land obligations: REMEDIATION REQUIRED"
            if remediation
            else (
                "Speculative-land obligations: OPEN"
                if unresolved
                else "Speculative-land obligations: CLEAR"
            )
        )
        print(heading)
        for record in records:
            print(
                "  "
                + _summary_line(
                    record, decision_waits.get(record["obligation_id"])
                )
            )
            if record.get("failure_summary"):
                print(f"    failure: {record['failure_summary']}")
    # Primary view: rc 2 remediation-required (any age) | rc 1 unclassified
    # unresolved BEYOND the bound | rc 0 clear, fresh, or a named decision wait.
    # Decision-only view deliberately inverts the last class to rc 1 so tick-hub
    # emits the lower-severity NOTE without polling/redispatching a second time.
    if awaiting_decision_only:
        return 1 if decision_waits else 0
    return 2 if remediation else 1 if stale_actionable else 0


def recoverable_obligation(
    repo: str, sha: str, store_path: Path
) -> dict[str, Any] | None:
    """Newest same-SHA record whose launch/peer work still needs ownership."""
    matches = [
        record
        for record in obligations.latest_records(store_path).values()
        if record.get("repo") == repo
        and record.get("landed_sha") == sha
        and (
            record.get("overall_state") not in obligations.CLOSED_STATES
            or _verification_in_flight(record)
            or not obligation_launch_durable(record)
        )
    ]
    if not matches:
        return None
    return max(
        matches,
        key=lambda record: (
            str(record.get("updated_at") or ""),
            str(record.get("opened_at") or ""),
            str(record.get("obligation_id") or ""),
        ),
    )


def arm(args: argparse.Namespace) -> int:
    # Resolve repository policy before touching the append-only store or starting
    # either verifier. Unsupported repositories must never acquire an obligation
    # that no consumer can verify.
    raw_policy = getattr(args, "verification_policy_json", None)
    if raw_policy is None:
        policy = verification_policy_for_repo(args.repo)
    else:
        try:
            parsed_policy = json.loads(raw_policy)
        except json.JSONDecodeError as error:
            raise ProtocolError("--verification-policy-json is invalid JSON") from error
        if not isinstance(parsed_policy, Mapping):
            raise ProtocolError("--verification-policy-json must be an object")
        policy = validate_verification_policy(parsed_policy)
        if policy["repo"] != args.repo:
            raise ProtocolError(
                "--verification-policy-json repository does not match --repo"
            )
    store_path = args.store.expanduser().resolve()
    source = resolve_repo_source(args.repo, args.source)
    sha = resolve_landed_sha(source, args.sha, repo=args.repo, pr=args.pr)
    record = recoverable_obligation(args.repo, sha, store_path)
    if record is not None:
        bind_verification_policy(
            record["obligation_id"], store_path, requested_policy=policy
        )
        print(
            f"resuming obligation {record['obligation_id']} for {args.repo}@{sha}",
            file=sys.stderr,
        )
    else:
        try:
            record = obligations.create_obligation(
                repo=args.repo,
                landed_sha=sha,
                land_mode=args.land_mode,
                verification_scope="total",
                verification_policy=policy,
                actor=args.actor,
                path=store_path,
            )
        except obligations.DuplicateOpenObligation as error:
            record = error.record
            bind_verification_policy(
                record["obligation_id"], store_path, requested_policy=policy
            )
            print(str(error), file=sys.stderr)
    # Creation is not arming. Every path, including DuplicateOpen recovery,
    # enters the same claim/register reconciler and returns successfully only
    # after both the verifier and watcher have durable producer evidence.
    obligation_id = record["obligation_id"]
    record, github_error = resume_obligation_launch(
        obligation_id,
        source=source,
        store_path=store_path,
        github_wait_seconds=args.github_wait_seconds,
        poll_seconds=args.poll_seconds,
        allow_dispatch=not args.no_dispatch,
    )
    if not obligation_launch_durable(record):
        raise ProtocolError(f"obligation {obligation_id} did not durably arm")
    print(f"OPEN OBLIGATION: {obligation_id} {args.repo}@{sha}")
    print(
        f"  launch: state={record['launch']['state']} "
        f"armed_at={record['launch'].get('armed_at')}"
    )
    print(
        f"  local: state={record['local']['state']} pid={record['local'].get('pid')} "
        f"log={record['local'].get('log_path')}"
    )
    print(
        "  github: "
        f"state={record['github']['state']} "
        f"runs={','.join(map(str, record['github']['run_ids'])) or 'none'}"
    )
    print(
        f"  watcher: state={record['watcher']['state']} "
        f"pid={record['watcher'].get('pid')} log={record['watcher'].get('log_path')}"
    )
    return 2 if github_error else 0


def resolve_obligation(args: argparse.Namespace) -> int:
    ref = args.ref.lower()
    if not obligations.SHA_RE.fullmatch(ref):
        raise ProtocolError("--ref must be a full 40-character commit SHA")
    store_path = args.store.expanduser().resolve()
    record = obligations.get_record(args.id, store_path)
    if record.get("overall_state") != "remediation_required":
        raise ProtocolError(
            f"obligation {args.id!r} is not in remediation_required state"
        )
    event_id = record.get("event_id")
    if not isinstance(event_id, str) or not event_id:
        raise ProtocolError(f"obligation {args.id!r} has no durable event identity")
    recommendation = record.get("recommendation")
    durable_action = (
        recommendation.get("action")
        if isinstance(recommendation, Mapping)
        else None
    )
    if durable_action not in {"fix-forward", "revert"}:
        raise ProtocolError(
            f"obligation {args.id!r} has no concrete remediation recommendation"
        )
    remediation = record.get("remediation")
    if not isinstance(remediation, Mapping) or remediation.get("state") != "triggered":
        raise ProtocolError(f"obligation {args.id!r} remediation is not triggered")
    if remediation.get("kind") != durable_action:
        raise ProtocolError(
            f"obligation {args.id!r} durable remediation kind does not match its "
            "recommendation"
        )
    # A recommendation is a PREDICTION made at failure time, before anyone had
    # diagnosed the failure. The resolution is a STATEMENT OF WHAT WAS DONE.
    # Requiring them to agree meant the ledger could only record cases where the
    # first guess happened to be right, and left every other case with two
    # dishonest options: refuse to resolve a genuinely discharged obligation
    # (corrupting the ledger by omission), or record a repair that never
    # happened (corrupting it by commission).
    #
    # So a differing kind is ACCEPTED AND RECORDED AS DIFFERING -- never
    # silently coerced. Evidence is NOT relaxed: the proof obligations below key
    # on `args.kind`, the kind actually CLAIMED, so claiming `revert` still
    # requires proving a revert happened. What is gone is the requirement that
    # the claim match the prediction, not the requirement that it match reality.
    if args.kind != durable_action:
        print(
            f"resolution kind {args.kind!r} differs from durable recommendation "
            f"{durable_action!r}; recording the repair that actually happened",
            file=sys.stderr,
        )
    repo = record.get("repo")
    if not isinstance(repo, str):
        raise ProtocolError(f"obligation {args.id!r} has no repository identity")
    if ref == record.get("landed_sha"):
        raise ProtocolError("--ref must identify a repair commit, not the failed land")

    # Resolution is itself a load-bearing authority. Freshly fetch this
    # obligation's repository-bound main ref, prove the exact repair object
    # exists there, and carry that proof into the append-only transition.
    source = resolve_repo_source(repo, args.source)
    target_ref = _fetch_target(source, "main")
    target_tip = _resolve_raw_sha(source, target_ref)
    resolved = _resolve_raw_sha(source, ref)
    if resolved != ref:
        raise ProtocolError(f"--ref resolved to {resolved}, not exact repair SHA {ref}")
    if not _is_target_ancestor(source, resolved, target_tip):
        raise ProtocolError(
            f"repair {resolved} is not reachable from freshly fetched {repo}:main"
        )
    landed_sha = record.get("landed_sha")
    if not isinstance(landed_sha, str) or not obligations.SHA_RE.fullmatch(landed_sha):
        raise ProtocolError(f"obligation {args.id!r} has no exact landed SHA")
    if not _is_target_ancestor(source, landed_sha, resolved):
        raise ProtocolError(
            f"repair {resolved} does not descend from failed land {landed_sha}"
        )
    # `kind` is what was ACTUALLY done; `recommended_kind` is what was PREDICTED.
    # Both are recorded so a reader can see the two disagreeing rather than
    # having to infer it, and `durable_recommendation_matches` is now COMPUTED
    # instead of hardcoded True.
    kind_verification: dict[str, object] = {
        "kind": args.kind,
        "recommended_kind": durable_action,
        "durable_recommendation_matches": args.kind == durable_action,
    }
    # Keyed on the CLAIM, not the recommendation: a `revert` resolution must
    # prove a revert regardless of what was recommended, and a `fix-forward`
    # resolution cannot borrow revert's proof by having been recommended one.
    if args.kind == "revert":
        repair_parents = _commit_parents(source, resolved)
        if repair_parents != [landed_sha]:
            raise ProtocolError(
                "revert repair must be the direct single-parent child of the "
                f"failed land {landed_sha}"
            )
        failed_land_parents = _commit_parents(source, landed_sha)
        if len(failed_land_parents) != 1:
            raise ProtocolError(
                f"failed land {landed_sha} is not a single-parent commit; "
                "resolve it as a reviewed fix-forward"
            )
        failed_land_parent = failed_land_parents[0]
        repair_tree = _commit_tree(source, resolved)
        restored_tree = _commit_tree(source, failed_land_parent)
        if repair_tree != restored_tree:
            raise ProtocolError(
                f"revert repair {resolved} does not restore failed land parent "
                f"tree {restored_tree}"
            )
        kind_verification.update(
            {
                "repair_parent_sha": landed_sha,
                "failed_land_parent_sha": failed_land_parent,
                "repair_tree_sha": repair_tree,
                "failed_land_parent_tree_sha": restored_tree,
                "tree_restored": True,
            }
        )
    now = obligations.utc_now()
    updated = obligations.transition_if_matches(
        args.id,
        "remediated",
        {
            "overall_state": "remediated",
            "remediation": {
                "state": "completed",
                "kind": args.kind,
                "ref": ref,
                "started_at": args.started_at or now,
                "completed_at": now,
                "landing_verification": {
                    "authority": "fresh-repository-main-ancestry-v1",
                    "repo": repo,
                    "repair_sha": resolved,
                    "target_ref": target_ref,
                    "target_tip_sha": target_tip,
                    "repair_is_ancestor_of_target_tip": True,
                    "failed_land_sha": landed_sha,
                    "failed_land_is_ancestor_of_repair": True,
                    "kind_verification": kind_verification,
                    "source": str(source),
                    "checked_at": now,
                },
            },
        },
        {
            "event_id": event_id,
            "overall_state": "remediation_required",
            "recommendation": {"action": durable_action},
            "remediation": {"state": "triggered", "kind": durable_action},
        },
        store_path,
    )
    if updated is None:
        raise ProtocolError(
            f"obligation {args.id!r} changed while repair ancestry was verified"
        )
    return 0


def _wait_for_pidfd_exit(pidfd: int, wait_seconds: float) -> bool:
    poller = select.poll()
    poller.register(pidfd, select.POLLIN)
    return bool(poller.poll(max(0, round(wait_seconds * 1000))))


def cancel_watcher(args: argparse.Namespace) -> int:
    """Terminate one durably identified watcher without changing obligation truth."""
    reason = args.reason.strip()
    if not reason:
        raise ProtocolError("--reason must be nonempty")
    if args.wait_seconds <= 0:
        raise ProtocolError("--wait-seconds must be positive")

    store_path = args.store.expanduser().resolve()
    record = obligations.get_record(args.id, store_path)
    watcher = record.get("watcher")
    if not isinstance(watcher, Mapping):
        raise ProtocolError(f"obligation {args.id!r} has no watcher record")
    if watcher.get("state") in {"cancelled", "completed", "failed"}:
        print(
            f"WATCHER ALREADY TERMINAL: obligation={args.id} "
            f"state={watcher.get('state')}"
        )
        return 0
    if watcher.get("state") != "running":
        raise ProtocolError(
            f"obligation {args.id!r} watcher is not running "
            f"(state={watcher.get('state')!r})"
        )
    if (
        record.get("overall_state") not in obligations.CLOSED_STATES
        or not _watch_complete(record)
    ):
        raise ProtocolError(
            f"obligation {args.id!r} is not terminal; refusing to cancel its "
            "only continuous watcher"
        )

    pid = watcher.get("pid")
    launch_token = watcher.get("launch_token")
    identity = watcher.get("identity")
    if type(pid) is not int or pid <= 0 or not isinstance(launch_token, str):
        raise ProtocolError(f"obligation {args.id!r} watcher has no durable process owner")
    if not isinstance(identity, Mapping):
        raise ProtocolError(
            f"obligation {args.id!r} watcher predates exact process identities; "
            "refusing PID-only cancellation"
        )
    host = identity.get("host")
    boot_id = identity.get("boot_id")
    start_ticks = identity.get("start_ticks")
    if (
        not isinstance(host, str)
        or not isinstance(boot_id, str)
        or type(start_ticks) is not int
        or start_ticks <= 0
    ):
        raise ProtocolError(f"obligation {args.id!r} watcher identity is malformed")
    if host != socket.gethostname():
        raise ProtocolError(
            f"watcher belongs to host {host!r}, not current host {socket.gethostname()!r}"
        )
    if boot_id != _current_boot_id():
        raise ProtocolError("watcher belongs to an earlier host boot")

    try:
        pidfd = os.pidfd_open(pid, 0)
    except (AttributeError, OSError) as error:
        raise ProtocolError(f"cannot open pidfd for watcher {pid}: {error}") from error
    try:
        observed_ticks = _process_start_ticks(pid)
        if observed_ticks != start_ticks:
            raise ProtocolError(
                f"watcher pid {pid} was reused: recorded start_ticks={start_ticks} "
                f"observed={observed_ticks}"
            )
        requested_at = obligations.utc_now()
        attempt = int((watcher.get("cancellation") or {}).get("attempt") or 0) + 1
        requested = obligations.transition_if_matches(
            args.id,
            "watcher-cancel-requested",
            {
                "watcher": {
                    "cancellation": {
                        "state": "requested",
                        "attempt": attempt,
                        "requested_at": requested_at,
                        "requested_by": args.actor,
                        "reason": reason,
                    }
                }
            },
            {
                "event_id": record["event_id"],
                "watcher": {
                    "state": "running",
                    "pid": pid,
                    "launch_token": launch_token,
                    "identity": {
                        "host": host,
                        "boot_id": boot_id,
                        "start_ticks": start_ticks,
                    },
                },
            },
            store_path,
        )
        if requested is None:
            raise ProtocolError(
                f"obligation {args.id!r} changed while watcher identity was verified"
            )

        delivered_signal = "SIGTERM"
        exit_code: int | None = 128 + signal.SIGTERM
        try:
            signal.pidfd_send_signal(pidfd, signal.SIGTERM, None, 0)
        except ProcessLookupError:
            # The exact pidfd cannot retarget a reused PID. If the watcher won
            # the race and exited after the durable request, finish reconciling
            # its stale `running` row without pretending a signal was delivered.
            delivered_signal = "none-already-exited"
            exit_code = None
        except OSError as error:
            raise ProtocolError(
                f"cannot signal exact watcher {pid}@{start_ticks}: {error}"
            ) from error
        if not _wait_for_pidfd_exit(pidfd, args.wait_seconds):
            obligations.transition_if_matches(
                args.id,
                "watcher-cancel-timeout",
                {"watcher": {"cancellation": {"state": "timed_out"}}},
                {
                    "watcher": {
                        "state": "running",
                        "pid": pid,
                        "cancellation": {"requested_at": requested_at},
                    }
                },
                store_path,
            )
            raise ProtocolError(
                f"watcher {pid}@{start_ticks} did not exit after SIGTERM; "
                "no stronger signal was sent"
            )

        finished_at = obligations.utc_now()
        cancelled = obligations.transition_if_matches(
            args.id,
            "watcher-cancelled",
            {
                "watcher": {
                    "state": "cancelled",
                    "pid": None,
                    "exit_code": exit_code,
                    "finished_at": finished_at,
                    "cancellation": {
                        "state": "completed",
                        "completed_at": finished_at,
                        "signal": delivered_signal,
                    },
                }
            },
            {
                "watcher": {
                    "state": "running",
                    "pid": pid,
                    "launch_token": launch_token,
                    "identity": {
                        "host": host,
                        "boot_id": boot_id,
                        "start_ticks": start_ticks,
                    },
                    "cancellation": {"requested_at": requested_at},
                }
            },
            store_path,
        )
        if cancelled is None:
            latest = obligations.get_record(args.id, store_path)
            latest_state = (latest.get("watcher") or {}).get("state")
            if latest_state not in {
                "cancelled",
                "completed",
                "failed",
            }:
                raise ProtocolError(
                    f"obligation {args.id!r} changed before cancellation was recorded"
                )
            print(
                f"WATCHER TERMINAL: obligation={args.id} "
                f"identity={pid}@{start_ticks} state={latest_state}"
            )
            return 0
        print(
            f"WATCHER CANCELLED: obligation={args.id} "
            f"identity={pid}@{start_ticks} signal={delivered_signal}"
        )
        return 0
    finally:
        os.close(pidfd)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    arm_parser = subparsers.add_parser(
        "arm", help="arm dual verification for a landed SHA"
    )
    arm_parser.add_argument("sha")
    arm_parser.add_argument("--repo", default=DEFAULT_REPO)
    arm_parser.add_argument(
        "--verification-policy-json",
        help=argparse.SUPPRESS,
    )
    arm_parser.add_argument(
        "--pr",
        type=int,
        help="resolve a rebase-merged PR head to GitHub's replay SHA",
    )
    arm_parser.add_argument(
        "--source",
        type=Path,
        default=None,
        help="checkout whose origin matches --repo (defaults by supported repo)",
    )
    arm_parser.add_argument(
        "--land-mode", choices=("admin", "speculative"), default="speculative"
    )
    arm_parser.add_argument(
        "--actor", default=os.environ.get("AGENT", os.environ.get("USER", "unknown"))
    )
    arm_parser.add_argument(
        "--github-wait-seconds", type=int, default=DEFAULT_GITHUB_WAIT_SECONDS
    )
    arm_parser.add_argument("--poll-seconds", type=int, default=DEFAULT_POLL_SECONDS)
    arm_parser.add_argument("--no-dispatch", action="store_true")
    arm_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )

    verify_landing_parser = subparsers.add_parser(
        "verify-landing",
        aliases=["verify-landed-pr"],
        help="verify a PR replay SHA or commit SHA against a freshly fetched target",
    )
    verify_landing_parser.add_argument(
        "reference", help="positive PR number or commit OID (abbreviations accepted)"
    )
    verify_landing_parser.add_argument(
        "--item", help="human-readable item name included in claim-audit output"
    )
    verify_landing_parser.add_argument(
        "--claimed-oid",
        help="OID originally reported for a PR landing (requires --item)",
    )
    verify_landing_parser.add_argument("--repo", default=DEFAULT_REPO)
    verify_landing_parser.add_argument(
        "--source",
        type=Path,
        default=None,
        help="checkout whose origin matches --repo (defaults by supported repo)",
    )
    verify_landing_parser.add_argument("--target", default="main")
    verify_landing_parser.add_argument("--json", action="store_true")

    hosted_status_parser = subparsers.add_parser(
        "hosted-status",
        help="verify the registered exact-head GitHub job authority",
    )
    hosted_status_parser.add_argument("--repo", default=DEFAULT_REPO)
    hosted_status_parser.add_argument("--sha", required=True)
    hosted_status_parser.add_argument("--json", action="store_true")

    watch_parser = subparsers.add_parser(
        "watch", help="poll open obligations and record transitions"
    )
    watch_parser.add_argument("--id")
    watch_parser.add_argument("--launch-token", help=argparse.SUPPRESS)
    watch_parser.add_argument("--once", action="store_true")
    watch_parser.add_argument("--gate", action="store_true")
    watch_parser.add_argument("--poll-seconds", type=int, default=DEFAULT_POLL_SECONDS)
    watch_parser.add_argument(
        "--budget-secs",
        type=float,
        default=DEFAULT_WATCH_GATE_BUDGET_SECS,
        help=(
            "wall budget for a --once sweep; on expiry the gate returns a typed "
            "NO-RESULT under its own power instead of being killed by the "
            "outer tick guillotine (which discards all output)"
        ),
    )
    watch_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )

    status_parser = subparsers.add_parser("status", help="show unresolved obligations")
    status_parser.add_argument("--all", action="store_true")
    status_parser.add_argument(
        "--actionable", action="store_true", help="show only remediation still owed"
    )
    status_parser.add_argument(
        "--awaiting-decision",
        action="store_true",
        help="show only stale obligations with a named external decision blocker",
    )
    status_parser.add_argument("--json", action="store_true")
    status_parser.add_argument("--gate", action="store_true")
    status_parser.add_argument(
        "--decision-waits-file",
        type=Path,
        default=DEFAULT_DECISION_WAITS_PATH,
        help=argparse.SUPPRESS,
    )
    status_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )

    wake_parser = subparsers.add_parser(
        "wake-sent", help="record a best-effort wake as sent but unacknowledged"
    )
    wake_parser.add_argument("--target", required=True)
    wake_parser.add_argument("--source", default="unknown")
    wake_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )

    inherit_parser = subparsers.add_parser(
        "inherit", help="discover and acknowledge inherited remediation"
    )
    inherit_parser.add_argument("--agent", required=True)
    inherit_parser.add_argument(
        "--session", default=f"{socket.gethostname()}:{os.getpid()}"
    )
    inherit_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )
    inherit_parser.add_argument(
        "--action-outcome",
        choices=("executed", "declined"),
        help=(
            "record an evaluated recommendation; declined requires --reason, "
            "while omission records receipt only"
        ),
    )
    inherit_parser.add_argument(
        "--reason",
        help="durable refusal rationale; valid only for a declined action",
    )

    cancel_parser = subparsers.add_parser(
        "cancel-watcher",
        help="pidfd-cancel one durably identified watcher without closing its obligation",
    )
    cancel_parser.add_argument("id")
    cancel_parser.add_argument("--reason", required=True)
    cancel_parser.add_argument(
        "--actor", default=os.environ.get("AGENT", os.environ.get("USER", "unknown"))
    )
    cancel_parser.add_argument(
        "--wait-seconds", type=float, default=DEFAULT_WATCH_CANCEL_WAIT_SECS
    )
    cancel_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )

    resolve_parser = subparsers.add_parser(
        "resolve", help="record completed remediation"
    )
    resolve_parser.add_argument("id")
    resolve_parser.add_argument(
        "--kind", choices=("fix-forward", "revert"), required=True
    )
    resolve_parser.add_argument("--ref", required=True)
    resolve_parser.add_argument("--started-at")
    resolve_parser.add_argument(
        "--source",
        type=Path,
        default=None,
        help="checkout whose origin matches the obligation repository",
    )
    resolve_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )

    local_parser = subparsers.add_parser("_local-run", help=argparse.SUPPRESS)
    local_parser.add_argument("id")
    local_parser.add_argument("--launch-token", help=argparse.SUPPRESS)
    local_parser.add_argument("--source", type=Path, required=True)
    local_parser.add_argument("--store", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.command == "arm":
            if args.github_wait_seconds < 0 or args.poll_seconds <= 0:
                raise ProtocolError(
                    "wait must be non-negative and poll interval must be positive"
                )
            return arm(args)
        if args.command in ("verify-landing", "verify-landed-pr"):
            return verify_landing(args)
        if args.command == "hosted-status":
            return hosted_status(args)
        if args.command == "watch":
            if args.poll_seconds <= 0:
                raise ProtocolError("--poll-seconds must be positive")
            if args.launch_token is not None:
                if args.id is None:
                    raise ProtocolError("a launched watcher requires --id")
                if not _register_watcher(args.id, args.launch_token, args.store):
                    return 0
            try:
                result = watch(
                    store_path=args.store,
                    obligation_id=args.id,
                    once=args.once,
                    poll_seconds=args.poll_seconds,
                    # Only bound the ONE-SHOT gate path. A long-lived `watch`
                    # (no --once) is a deliberate foreground poll with no outer
                    # guillotine over it, and must not acquire one here.
                    budget_secs=args.budget_secs if args.once else None,
                )
            except BaseException as error:
                if args.launch_token is not None:
                    _fail_watcher(args.id, args.launch_token, args.store, error)
                raise
            else:
                if args.launch_token is not None:
                    _complete_watcher(args.id, args.launch_token, args.store)
            if args.gate:
                # A timed-out census has no domain answer. Do not replace its
                # typed NO-RESULT with print_status() over an unchanged store;
                # that was the laundering path that could emit CLEAR/exit 0.
                if result == WATCH_EXIT_NO_RESULT:
                    return result
                return print_status(
                    args.store, include_closed=False, json_output=False, gate=True
                )
            return result
        if args.command == "status":
            return print_status(
                args.store,
                include_closed=args.all,
                json_output=args.json,
                gate=args.gate,
                actionable_only=args.actionable,
                awaiting_decision_only=args.awaiting_decision,
                decision_waits_path=args.decision_waits_file,
            )
        if args.command == "wake-sent":
            record_wake_sent(
                store_path=args.store, target=args.target, source=args.source
            )
            return 0
        if args.command == "inherit":
            inherit_actionable(
                store_path=args.store,
                agent=args.agent,
                session=args.session,
                action_outcome=args.action_outcome,
                reason=args.reason,
            )
            return 0
        if args.command == "cancel-watcher":
            return cancel_watcher(args)
        if args.command == "resolve":
            return resolve_obligation(args)
        if args.command == "_local-run":
            return _local_run(
                args.id,
                args.source,
                args.store,
                launch_token=args.launch_token,
            )
    except (ProtocolError, obligations.StoreError) as error:
        print(f"ci-hub speculative-land: {error}", file=sys.stderr)
        return 2
    raise AssertionError(f"unhandled command {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
