#!/usr/bin/env python3
"""Run the deterministic ci-hub Python suites with counted coverage floors."""

from __future__ import annotations

import argparse
import importlib.metadata
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Sequence


REPO_ROOT = Path(__file__).resolve().parents[2]
REQUIREMENTS = Path(__file__).with_name("requirements-workflow.txt")
DEFAULT_SUITES = (
    ("ci-hub/directives/tests", 9),
    ("ci-hub/health/tests", 92),
    ("ci-hub/history/tests", 43),
    ("ci-hub/lib/tests", 6),
    ("ci-hub/remediation/tests", 87),
    ("ci-hub/runners/tests", 51),
    # Include both the legacy root-level validation tests and the nested suite.
    # Pointing only at validate/tests left six root modules outside the counted
    # gate. Preserve the existing conservative execution floor; importlib mode
    # and the real-path bracket below are what make the expanded surface
    # load-bearing without treating environment-declared skips as execution.
    ("ci-hub/validate", 123),
    ("ci-hub/tests", 66),
    # compat-envelope's Python tests had no CI caller of their own: their runner
    # is `compat-envelope/tests/run-all.sh`, reachable only through `make lint`,
    # and no workflow runs `make lint`. So the guards protecting the scorecard --
    # including the 18 tier-evidence tests written for a checker that itself had
    # zero call sites -- were committed, passing, and never executed by CI. Same
    # defect one level up. 145 discovered / 145 passing when added.
    ("compat-envelope", 120),
    # The 214x6x5 compatibility artifact shipped tools/analyze_repeat_stability.py
    # AND verify_artifact.py with NO CALLER: `grep -r` across the parent found
    # nothing invoking either, so a regeneration drift or a stale republished CSV
    # could not be detected by anything. Registering the bracket here is what
    # makes the correction a correction rather than a second uncalled tool -- the
    # same gap that let safehermit pass all its own tests while being invoked by
    # nothing. The bracket also invokes verify_artifact.py end to end.
    ("experiments/compat_repeat_sweep_external_214x6x5_20260810/tools", 7),
)


@dataclass(frozen=True)
class Suite:
    path: Path
    floor: int


@dataclass
class SuiteCounts:
    collected: list[str] = field(default_factory=list)
    executed: set[str] = field(default_factory=set)
    skipped: set[str] = field(default_factory=set)
    reported: set[str] = field(default_factory=set)


def _parse_suite(value: str) -> Suite:
    try:
        raw_path, raw_floor = value.rsplit("=", 1)
        floor = int(raw_floor)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            f"suite must have PATH=POSITIVE_FLOOR form: {value!r}"
        ) from error
    if not raw_path or floor < 1:
        raise argparse.ArgumentTypeError(
            f"suite must have PATH=POSITIVE_FLOOR form: {value!r}"
        )
    path = Path(raw_path)
    if not path.is_absolute():
        path = (Path.cwd() / path).resolve()
    else:
        path = path.resolve()
    return Suite(path=path, floor=floor)


def _default_suites() -> list[Suite]:
    return [Suite((REPO_ROOT / path).resolve(), floor) for path, floor in DEFAULT_SUITES]


class CountedCoverage:
    """Bind collected and call-phase item counts to their suite roots."""

    def __init__(self, suites: Sequence[Suite]) -> None:
        self.suites = list(suites)
        self.counts = {suite.path: SuiteCounts() for suite in suites}
        self.unbound_items: list[str] = []
        self._suite_by_nodeid: dict[str, Path] = {}

    def _suite_for(self, item: object) -> Suite | None:
        item_path = Path(item.path).resolve()  # type: ignore[attr-defined]
        for suite in self.suites:
            if item_path.is_relative_to(suite.path):
                return suite
        return None

    def pytest_collection_finish(self, session: object) -> None:
        for item in session.items:  # type: ignore[attr-defined]
            suite = self._suite_for(item)
            if suite is None:
                self.unbound_items.append(item.nodeid)
                continue
            self.counts[suite.path].collected.append(item.nodeid)
            self._suite_by_nodeid[item.nodeid] = suite.path

    def pytest_runtest_logreport(self, report: object) -> None:
        """Account for an environment-declared skip instead of losing it.

        A test skipped during setup never reaches the call hook, so it would
        otherwise be indistinguishable from a test that was collected and then
        silently dropped. Counting it here is what lets the unexplained
        remainder below mean ONLY "collected and then not run for a reason
        nobody declared".
        """
        path = self._suite_by_nodeid.get(getattr(report, "nodeid", ""))
        if path is None:
            return
        # ANY report is an outcome pytest already surfaces -- pass, fail, error,
        # skip, xfail. Those are accounted for. What must not happen silently is
        # a collected test that produces NO report at all.
        self.counts[path].reported.add(report.nodeid)  # type: ignore[attr-defined]
        if getattr(report, "when", None) == "setup" and getattr(report, "skipped", False):
            self.counts[path].skipped.add(report.nodeid)  # type: ignore[attr-defined]

    def pytest_runtest_call(self, item: object):
        suite = self._suite_for(item)
        if suite is not None:
            # Entering the call hook is observable execution. Setup-only skips
            # and collection-only successes deliberately do not count.
            self.counts[suite.path].executed.add(item.nodeid)  # type: ignore[attr-defined]
        yield


def _expected_pytest_version() -> str:
    requirement = REQUIREMENTS.read_text().strip()
    prefix = "pytest=="
    if "\n" in requirement or not requirement.startswith(prefix):
        raise ValueError(f"expected exactly one pytest==VERSION pin in {REQUIREMENTS}")
    version = requirement.removeprefix(prefix)
    if not version:
        raise ValueError(f"empty pytest version pin in {REQUIREMENTS}")
    return version


def _verify_pytest_version() -> bool:
    try:
        expected = _expected_pytest_version()
    except (OSError, ValueError) as error:
        print(f"REFUSED: invalid pytest requirement: {error}", file=sys.stderr)
        return False
    try:
        actual = importlib.metadata.version("pytest")
    except importlib.metadata.PackageNotFoundError:
        actual = "not installed"
    if actual == expected:
        return True
    print(
        "REFUSED: pytest version mismatch: "
        f"expected={expected} actual={actual}",
        file=sys.stderr,
    )
    return False


def run(suites: Sequence[Suite]) -> int:
    if not _verify_pytest_version():
        return 2

    import pytest

    missing = [str(suite.path) for suite in suites if not suite.path.is_dir()]
    if missing:
        for path in missing:
            print(f"REFUSED: suite directory does not exist: {path}", file=sys.stderr)
        return 2

    # Decorate the unbound function only after the exact pytest package has
    # been verified. A bound method cannot carry pluggy's hook metadata.
    pytest.hookimpl(hookwrapper=True)(CountedCoverage.pytest_runtest_call)
    plugin = CountedCoverage(suites)
    pytest_status = int(
        pytest.main(
            [
                "--import-mode=importlib",
                "--strict-config",
                "--strict-markers",
                # Make every skip STATE ITSELF. Without this the summary reports
                # a bare count, and an unnamed skip is indistinguishable from a
                # test that silently stopped running -- the same shape as an
                # absent authority reading as green. Measured 2026-08-08: the
                # green at b5cc56c9 carried "4 skipped" that nobody could
                # enumerate from the log, and two of the four turned out to be
                # coverage lost to step ordering rather than a decision anyone
                # had taken. A skip is a decision; it has to say so.
                "-rs",
                *(str(suite.path) for suite in suites),
            ],
            plugins=[plugin],
        )
    )

    floor_failed = False
    for suite in suites:
        counts = plugin.counts[suite.path]
        discovered = len(counts.collected)
        executed = len(counts.executed)
        skipped = len(counts.skipped)
        # THE FLOOR ALONE CANNOT SEE A SUITE THAT STOPS RUNNING. It compares
        # both numbers to a CONSTANT and never to each other, so a suite that
        # discovers 466 and executes 92 passes a floor of 92 while 374 tests
        # vanish. Measured 2026-08-24 across the nine default suites, 1058
        # tests could stop executing with every suite still reporting PASS.
        # health/tests was caught at discovered=466 executed=0 only because
        # zero happens to fall below its floor.
        #
        # So relate the two numbers. Every discovered test must be accounted
        # for: executed, or skipped for a declared reason. What is left is
        # collected-and-then-not-run -- a module-level sys.exit aborting the
        # session, a collection INTERNALERROR, an import failure from an
        # uninitialised submodule. One relation catches the whole class.
        #
        # SKIPS ARE ACCOUNTED FOR, NOT FORBIDDEN, deliberately. A bare
        # `executed == discovered` would red on every legitimate
        # environment-declared skip, and a check that reds on normal operation
        # is suppressed within a week.
        # UNEXPLAINED means VANISHED WITHOUT TRACE: collected, then no report of
        # any kind. A failure, an error or a skip is an outcome pytest already
        # reports and already fails the run on; counting those here would red
        # twice for one thing and would make this check the noisy one that gets
        # suppressed. Only silence counts.
        unexplained = discovered - len(counts.reported)
        meets_floor = discovered >= suite.floor and executed >= suite.floor
        passed_floor = meets_floor and unexplained <= 0
        status = "PASS" if passed_floor else "REFUSED"
        print(
            f"SUITE {status}: path={suite.path} floor={suite.floor} "
            f"discovered={discovered} executed={executed} "
            f"skipped={skipped} unexplained={unexplained}"
        )
        if unexplained > 0:
            print(
                f"REFUSED: {suite.path} discovered {discovered} test(s) and ran "
                f"{executed}; {unexplained} were collected and then not run for "
                "any declared reason",
                file=sys.stderr,
            )
        floor_failed |= not passed_floor

    if plugin.unbound_items:
        floor_failed = True
        for nodeid in plugin.unbound_items:
            print(f"REFUSED: collected item is not bound to a suite: {nodeid}", file=sys.stderr)

    if floor_failed:
        return 1
    return pytest_status


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--suite",
        action="append",
        type=_parse_suite,
        help="suite override in PATH=POSITIVE_FLOOR form; repeat for multiple suites",
    )
    args = parser.parse_args(argv)
    suites = args.suite if args.suite is not None else _default_suites()
    return run(suites)


if __name__ == "__main__":
    raise SystemExit(main())
