#!/usr/bin/env python3
"""RESULT-LEVEL detector: a GREEN must carry a NONZERO EXECUTED-TEST COUNT.

A success badge is a PROXY for "the tests passed". The badge and the fact it
claims are two independent things: a target can compile, the harness can run, and
ZERO tests can execute, yet the job still exits 0 and reports success. That is a
no-result wearing a success badge — the classic shape being `--features <x>`
gating that excludes the tests from compilation, so the binary runs 0 tests and
passes (verified by hermit-codex-rev planting a feature-gated fixture: build
succeeds, target runs, zero tests execute, reported SUCCESS). The SAME defect is
a `locally-validated` label with no backing run: a pass asserted with nothing
executed behind it.

The BINDING this detector observes (Proxy Binding review axis): it does not
authenticate WHO emitted the green, nor AND two independent facts. It reads the
executed count the test runner ITSELF printed into the log — evidence that can
exist only when tests actually ran — and refuses a green whose observed executed
count is zero. The count is the first-cause result; the badge is the proxy.

WHY THIS IS NOT THE SOURCE LINT (`lint-rust-error-string-proxies.py`): feature
gating EXCLUDES the tests from compilation, so there is no proxy-matching source
for a source lint to find. It is a BUILD-CONFIGURATION property producing a
RUNTIME outcome, observable only in the run's own output. A source lint straining
after it becomes slow, wrong, and — worst — LOOKS like the class is covered. This
detector is deliberately RESULT-LEVEL: it reads the emitted log, not the source.

FAIL-SAFE DIRECTION. The detector fires ONLY on POSITIVE evidence of zero-ness:
at least one test-runner banner is present AND the total executed across every
banner is 0. Absence of any banner yields ``None`` (unknown, not zero) so a green
whose log we could not parse for banners is NEVER downgraded — the detector can
only refuse a demonstrably inert green, it can never manufacture a no-result from
a green it merely failed to read. In the speculative-land classifier a downgrade
is to ``no_result`` (a hole to RE-DISPATCH), never to ``red`` (a revert): even a
false positive here is recoverable and can never revert a healthy tip.

KNOWN LIMITATION — this detector catches "produced EVIDENCE OF NOTHING", NOT
"produced NO EVIDENCE". An absent or zero-length log is no-evidence just as a
zero-test banner is, but only the latter is caught: an empty log yields ``None``
(no banner) and stays green. That is the SAME defect one layer out, and the
empty-log case is the MORE likely failure in practice — a job that dies before
emitting, a retention expiry, a fetch that 404s. This gap is deliberately left
UNCLOSED here (documented, not silent: a detector with an undocumented hole
converts "we do not check this" into "we checked and it passed").

The predicate that would CLOSE it: ``log absent OR zero bytes after the
validate-start offset => NOT a pass => no_result``. It MUST key on an observable
first-cause — ``stat()`` (file exists AND ``st_size > offset``) — NOT on the
empty string this classifier receives, because ``_read_local_output`` in
``protocol.py`` returns ``""`` on an OSError too: a genuinely empty/absent log
(no evidence) and a READ FAILURE of a log that did contain a Validation summary
are both ``""`` here and must not be conflated. Treating a read failure as
``no_result`` is still SAFE (re-dispatch, never revert) but would spuriously
re-dispatch real greens whenever a log read hiccups, so the closing predicate
belongs at the read site (which can stat the file) rather than in this
string-only function. NOT a legitimate-empty concern at the ``_classify_local``
layer: a genuine exit-0 validation driver always emits its ``Validation summary``
line, so empty output there is no-evidence, not a valid banner-less pass. (The
distinct, legitimate no-banner-but-NONEMPTY case — build-only / skipped-gate
summaries — is exactly why ``executed_test_count`` returns ``None`` for it,
UNKNOWN not zero; empty/absent is strictly stronger than no-banner.) At the
future log-FETCH consumer (GitHub job log / ``locally-validated`` label) the same
"absent/zero-length => no_result" predicate applies directly — that is where the
404 / retention-expiry cases live.
"""

from __future__ import annotations

import re
import sys

# libtest per-binary banner: `running 12 tests` (and `running 1 test`). This is
# the literal count of tests EXECUTED by that binary — the clean executed signal.
_RUNNING_RE = re.compile(r"\brunning (\d+) tests?\b")
# libtest summary line: `test result: ok. 12 passed; 0 failed; ...`. Only the
# `ok.` form carries a passing verdict; the passed count corroborates execution
# when a `running N` banner was truncated out of the captured window.
_RESULT_OK_RE = re.compile(r"\btest result: ok\. (\d+) passed\b")
# libtest summary tail: `... 154 filtered out; finished in 0.00s`. The count of
# tests the SELECTION excluded from this run. It is emitted on every `test
# result:` line (ok or not), so it is scanned independently of the `ok.` verdict.
_FILTERED_RE = re.compile(r"\b(\d+) filtered out\b")

# safe-ci-dag-runner streams every child stdout line prefixed `[<node.tag>] `
# (node.tag = `group.job`) into the shared validate LOG_FILE at verbosity >= 2,
# and emits a per-node terminal `[<node.tag>] ✓ PASS ...` / `✗ FAIL ...` line at
# verbosity >= 1. This prefix is how per-node aggregation attributes a libtest
# banner to the DAG node that produced it.
_NODE_PREFIX_RE = re.compile(r"^\[([^\]]+)\] ?(.*)$")
# The terminal marker appears at the START of the terminal line's body (right
# after the `[tag] ` prefix); requiring startswith keeps indented guest stdout
# (e.g. `[tag]     ✓ PASS printed by a test`) from ever forging the signal.
_PASS_TERMINAL = "✓ PASS"  # ✓ PASS
_FAIL_TERMINAL = "✗ FAIL"  # ✗ FAIL


def executed_test_count(output: str) -> int | None:
    """Total tests EXECUTED as reported by the run's own test-runner banners.

    Returns the summed executed count when at least one banner is present, or
    ``None`` when the output carries no recognizable test-runner banner at all.
    ``None`` means UNKNOWN (we cannot see how many ran), which is distinct from
    ``0`` (banners present, and every one executed zero tests). A validate run
    that built dozens of crates prints one banner per test binary — crates with
    no unit tests print `running 0 tests` — so the sum is zero only when EVERY
    banner ran zero tests, i.e. the whole run executed nothing.
    """
    if not output:
        return None
    running = [int(m) for m in _RUNNING_RE.findall(output)]
    if running:
        return sum(running)
    passed = [int(m) for m in _RESULT_OK_RE.findall(output)]
    if passed:
        return sum(passed)
    return None


def filtered_test_count(output: str) -> int | None:
    """Total tests FILTERED OUT (excluded by name/selection) per the run's own
    `test result:` summary lines.

    Returns the summed filtered-out count when at least one `test result:` line
    is present, or ``None`` when no such line is seen (UNKNOWN). This is the
    companion of :func:`executed_test_count`: together they tell a zero-executed
    run's two distinct shapes apart. ``executed==0 and filtered==0`` is an
    EMPTY TARGET (no tests exist — the classic `--features`-gated build that
    compiles the tests out); ``executed==0 and filtered>0`` is FILTERED TO EMPTY
    (tests exist but the selection excluded every one). Both read as `ok` and
    verify nothing, but the cause — and the fix — differ, so the count is carried
    rather than collapsed. A nonzero-executed run with ``filtered>0`` is a real
    pass over a NARROWED scope (the `1 passed; 154 filtered out` trap): still a
    pass, but the consumer must see the scope it did not cover.
    """
    if not output:
        return None
    filtered = [int(m) for m in _FILTERED_RE.findall(output)]
    if filtered:
        return sum(filtered)
    return None


def is_zero_test_green(output: str) -> bool:
    """Whether a passing run executed a DEMONSTRABLY zero test count.

    True only on positive evidence: banners are present and their total executed
    count is 0. Unknown (no banner) and any nonzero count both return False, so
    this only refuses a green that is provably inert — never one it could not
    read. Caller supplies the output of a run that already reported success; this
    answers whether that success is backed by any executed test.
    """
    return executed_test_count(output) == 0


def per_node_counts(output: str) -> dict[str, dict]:
    """Per-DAG-node executed/filtered/banner counts, keyed by `[<node.tag>]`.

    Returns ``{node_tag: {"executed": int, "filtered": int, "banner_count": int,
    "terminal": "pass"|"fail"|None}}`` for every ``[tag] ``-prefixed node seen in
    the safe-ci-dag-runner streamed log. This is the NODE-granular companion of
    :func:`executed_test_count`, single-sourcing the SAME ``passed``/``filtered``
    regexes so a per-node view can never drift from the aggregate one.

    Counting rules (see the module docstring for WHY node granularity is correct):

    * ``executed`` sums ``N passed`` across the node's ``test result: ok.``
      libtest banner lines; ``filtered`` sums ``N filtered out``. A node's banners
      are AGGREGATED before any zero-check, because a legitimate full run has
      zero-passed banners (empty crates, doctests, sharded filter-misses like
      ``1 passed; 213 filtered out``) absorbed inside a node whose total is
      positive. Only a NODE whose banners sum to zero is inert.
    * ``banner_count`` counts the node's ``test result:`` summary lines. A node
      with ``banner_count == 0`` emitted no libtest banner and is EXEMPT from the
      zero-executed obligation (legit shell / e2e / nextest node).
    * ``terminal`` records the node's ``✓ PASS`` / ``✗ FAIL`` runner line. The
      terminal line itself embeds a ``[test result: ok. ...]`` summary; it is
      DELIBERATELY excluded from banner counting so that summary is never
      double-counted against the banners streamed earlier.
    """
    nodes: dict[str, dict] = {}
    if not output:
        return nodes
    for raw in output.splitlines():
        m = _NODE_PREFIX_RE.match(raw)
        if not m:
            continue
        tag, rest = m.group(1), m.group(2)
        node = nodes.setdefault(
            tag, {"executed": 0, "filtered": 0, "banner_count": 0, "terminal": None}
        )
        stripped = rest.lstrip()
        # Terminal line first: it carries an embedded banner we must NOT count.
        if stripped.startswith(_PASS_TERMINAL):
            node["terminal"] = "pass"
            continue
        if stripped.startswith(_FAIL_TERMINAL):
            node["terminal"] = "fail"
            continue
        if "test result:" in rest:
            node["banner_count"] += 1
            pm = _RESULT_OK_RE.search(rest)
            if pm:
                node["executed"] += int(pm.group(1))
            fm = _FILTERED_RE.search(rest)
            if fm:
                node["filtered"] += int(fm.group(1))
    return nodes


def _json_literal(value: int | None) -> str:
    """A count as a JSON literal: an integer, or ``null`` for UNKNOWN (``None``).

    Chosen so a shell caller can splice the token straight into a JSONL record
    without a JSON parser: both forms are valid JSON values on their own.
    """
    return "null" if value is None else str(value)


def _ledger_fields(output: str) -> str:
    """The two count fields for a validate-run ledger row, single-sourced.

    Prints ``<executed_tests> <filtered_tests>`` as two space-separated JSON
    literals (an integer or ``null``) — the SAME extractor the aggregator and the
    landing predicate read, so a bash ledger writer never carries a second
    `running N tests` / `N filtered out` regex that could drift from this module.
    ``null`` is UNKNOWN (no banner in the log), which the reader treats fail-safe;
    ``0`` is a demonstrated zero, which the reader refuses as a green.
    """
    return f"{_json_literal(executed_test_count(output))} {_json_literal(filtered_test_count(output))}"


def main(argv: list[str] | None = None) -> int:
    """CLI so a non-Python ledger writer (Hermit's Rust driver) can call the ONE
    extractor instead of re-implementing the banner regexes in bash.

    ``nonzero_result.py --ledger-fields <log>`` prints the two JSON-literal counts
    for embedding in a ledger record; ``-`` reads the log from stdin. A missing or
    unreadable log prints ``null null`` (UNKNOWN, fail-safe) and still exits 0, so
    the writer never fabricates a zero and never fails the run over a read hiccup.

    ``nonzero_result.py --per-node <log>`` prints the per-DAG-node counts dict as
    JSON (``-`` reads the log from stdin). A missing/unreadable log prints ``{}``
    and exits 0, matching the fail-safe error handling of ``--ledger-fields``.
    """
    import argparse
    import json

    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument(
        "--ledger-fields",
        metavar="LOG",
        help="print `<executed_tests> <filtered_tests>` (JSON literals) for LOG (`-`=stdin)",
    )
    group.add_argument(
        "--per-node",
        metavar="LOG",
        help="print the per-node counts dict as JSON for LOG (`-`=stdin)",
    )
    args = parser.parse_args(argv)

    src = args.ledger_fields if args.ledger_fields is not None else args.per_node
    per_node_mode = args.per_node is not None
    if src == "-":
        output = sys.stdin.read()
    else:
        try:
            with open(src, errors="replace") as fh:
                output = fh.read()
        except OSError:
            # No evidence to read -> UNKNOWN/empty, never a fabricated result. The
            # writer emits the fail-safe token and still exits 0 (read hiccup is
            # never a run failure).
            print("{}" if per_node_mode else "null null")
            return 0
    if per_node_mode:
        print(json.dumps(per_node_counts(output), sort_keys=True))
    else:
        print(_ledger_fields(output))
    return 0


if __name__ == "__main__":
    sys.exit(main())
