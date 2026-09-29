#!/usr/bin/env python3
"""Tests for nonzero_result: aggregate counts + per-node coverage parsing.

These cover the NEW per_node_counts()/--per-node surface added for the
schema-5 validation-receipt coverage bind. The pre-existing executed/filtered
extractor behavior is exercised where per-node aggregation depends on it, so the
"one parser, no divergence" invariant is regression-guarded from both sides.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
# --import-mode=importlib does not put this directory on sys.path, so the
# sibling module import below needs it inserted explicitly.
sys.path.insert(0, str(HERE))

import nonzero_result as nz  # noqa: E402

MODULE = HERE / "nonzero_result.py"


# --- aggregate extractor sanity (single-source guard) -----------------------

def test_executed_none_when_no_banner():
    assert nz.executed_test_count("nothing here\nno banners\n") is None


def test_executed_zero_distinct_from_none():
    # Banner present, zero passed -> 0 (demonstrated), NOT None (unknown).
    assert nz.executed_test_count("test result: ok. 0 passed; 5 filtered out") == 0


# --- per_node_counts --------------------------------------------------------

def test_per_node_thirteen_passing_nodes_all_positive():
    """N=13 test nodes, each with a positive-passed banner + terminal PASS.

    Every node has executed>0 and a terminal -> none in zero_executed, none
    absent -> a manifest of exactly these 13 would be SATISFIED.
    """
    lines = []
    tags = [f"test.node{i:02d}" for i in range(13)]
    for i, t in enumerate(tags):
        n = i + 1
        lines.append(f"[{t}] running {n} tests")
        lines.append(f"[{t}] test result: ok. {n} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s")
        lines.append(f"[{t}] ✓ PASS   node {t} ({n}s)  [test result: ok. {n} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out]")
    nodes = nz.per_node_counts("\n".join(lines))
    assert len(nodes) == 13
    for i, t in enumerate(tags):
        assert nodes[t]["executed"] == i + 1
        assert nodes[t]["banner_count"] == 1  # terminal-embedded banner NOT counted
        assert nodes[t]["terminal"] == "pass"


def test_per_node_terminal_embedded_banner_not_double_counted():
    """The terminal `✓ PASS` line embeds `[test result: ok. 40 passed]`; it must
    NOT add to the node's executed sum (only the streamed banner counts)."""
    log = (
        "[test.solo] running 40 tests\n"
        "[test.solo] test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.0s\n"
        "[test.solo] ✓ PASS   solo (2s)  [test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out]\n"
    )
    nodes = nz.per_node_counts(log)
    assert nodes["test.solo"]["executed"] == 40  # not 80
    assert nodes["test.solo"]["banner_count"] == 1
    assert nodes["test.solo"]["terminal"] == "pass"


def test_per_node_multibanner_aggregates_not_banner_level_refusal():
    """A node whose FIRST banner is `0 passed; 213 filtered out` but whose SECOND
    banner is `40 passed` aggregates to 40 -> NODE not inert (proves node-level
    aggregation, not banner-level over-refusal)."""
    log = (
        "[test.multi] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 213 filtered out; finished in 0.0s\n"
        "[test.multi] test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.5s\n"
        "[test.multi] ✓ PASS   multi (1s)\n"
    )
    nodes = nz.per_node_counts(log)
    assert nodes["test.multi"]["executed"] == 40
    assert nodes["test.multi"]["filtered"] == 213
    assert nodes["test.multi"]["banner_count"] == 2


def test_per_node_banner_zero_is_inert():
    """A node with a single `0 passed` banner and terminal PASS: banner_count>=1
    and executed==0 -> the caller will flag it zero-executed."""
    log = (
        "[test.inert] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.0s\n"
        "[test.inert] ✓ PASS   inert (1s)  [test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out]\n"
    )
    nodes = nz.per_node_counts(log)
    assert nodes["test.inert"]["banner_count"] == 1
    assert nodes["test.inert"]["executed"] == 0
    assert nodes["test.inert"]["terminal"] == "pass"


def test_per_node_no_banner_exempt():
    """A shell/e2e node: terminal PASS, no libtest banner -> banner_count==0
    (EXEMPT from zero-executed)."""
    log = "[test.shell] ✓ PASS   shell e2e node (3s)\n"
    nodes = nz.per_node_counts(log)
    assert nodes["test.shell"]["banner_count"] == 0
    assert nodes["test.shell"]["executed"] == 0
    assert nodes["test.shell"]["terminal"] == "pass"


def test_per_node_fail_terminal():
    log = "[test.bad] ✗ FAIL   bad node (3s)\n"
    nodes = nz.per_node_counts(log)
    assert nodes["test.bad"]["terminal"] == "fail"


def test_per_node_indented_guest_output_not_a_terminal():
    """An indented guest line that happens to print `✓ PASS` is NOT the runner's
    terminal line (startswith guard) and does not set terminal."""
    log = "[test.x] running 1 tests\n[test.x]     some guest text ✓ PASS printed by a test\n"
    nodes = nz.per_node_counts(log)
    assert nodes["test.x"]["terminal"] is None


def test_per_node_empty():
    assert nz.per_node_counts("") == {}


# --- CLI --------------------------------------------------------------------

def _run(*args, stdin=None):
    return subprocess.run(
        [sys.executable, str(MODULE), *args],
        input=stdin, capture_output=True, text=True,
    )


def test_cli_per_node_stdin():
    log = "[test.a] test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n[test.a] ✓ PASS a\n"
    proc = _run("--per-node", "-", stdin=log)
    assert proc.returncode == 0
    d = json.loads(proc.stdout)
    assert d["test.a"]["executed"] == 3
    assert d["test.a"]["terminal"] == "pass"


def test_cli_per_node_missing_file_prints_empty():
    proc = _run("--per-node", "/no/such/log/here.log")
    assert proc.returncode == 0
    assert proc.stdout.strip() == "{}"


def test_cli_ledger_fields_still_works():
    log = "running 5 tests\ntest result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out\n"
    proc = _run("--ledger-fields", "-", stdin=log)
    assert proc.returncode == 0
    assert proc.stdout.strip() == "5 2"


def test_cli_requires_a_mode():
    proc = _run()
    assert proc.returncode != 0


# --- positive control on any real full DAG logs on disk ---------------------

def test_real_logs_passing_banner_nodes_are_not_inert():
    """Positive control (criterion 2): on real DAG logs, every node that PASSED
    (terminal ✓ PASS) AND emitted >=1 libtest banner has passed-sum > 0 -- i.e.
    no passing node is the inert-green we refuse.

    Scope is deliberately PASSING nodes: a FAILED node legitimately shows
    `test result: FAILED. ... 0 ok-passed` and is caught by its ✗ FAIL terminal,
    not by the zero-executed clause. Non-libtest nodes (nextest/e2e) have
    banner_count==0 and are exempt. STATE N + filenames in the captured output.
    """
    import glob
    candidates = []
    for pat in ("/tmp/hermit-validate.*.log",
                str(HERE.parent.parent / "scratch" / "*.log"),
                str(HERE.parent.parent / "hermit" / "scratch" / "*.log")):
        candidates.extend(glob.glob(pat))
    checked = []
    for path in candidates:
        try:
            text = Path(path).read_text(errors="replace")
        except OSError:
            continue
        nodes = nz.per_node_counts(text)
        passing_banner_nodes = {
            t: n for t, n in nodes.items()
            if n["banner_count"] >= 1 and n["terminal"] == "pass"
        }
        if not passing_banner_nodes:
            continue
        checked.append((path, len(passing_banner_nodes)))
        inert = [t for t, n in passing_banner_nodes.items() if n["executed"] == 0]
        assert inert == [], f"{path}: PASSING banner nodes with 0 executed {inert}"
    print(f"\n[positive-control] real logs with passing [node] banners checked: {len(checked)}")
    for p, k in checked:
        print(f"  {p}  ({k} passing banner-emitting nodes)")
    if not checked:
        print("  (none found on disk)")
