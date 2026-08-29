#!/usr/bin/env python3
"""MUTATION SUITE FOR GUARDS — a score, not an anecdote.

Planting a violation IS mutation testing. Done ad hoc it yields one data point:
"we planted a violation and it was caught". Done as a suite it yields a
COVERAGE MEASUREMENT: `killed / total`, plus the individual survivors — and every
survivor is a guard reporting health while not guarding.

THE BRACKET IS THREE PARTS, NOT TWO
-----------------------------------
  1. the mutant IS KILLED (the guard refuses it);
  2. THE LEGITIMATE POPULATION SURVIVES, COUNTED -- a guard that kills
     everything passes (1) perfectly and is useless, so a mutant is only scored
     once its guard's population control has been counted;
  3. the mutant CLEANS UP -- nothing planted may outlive the run.

HARD PRECONDITION, LEARNED THE EXPENSIVE WAY
--------------------------------------------
THE MUTANT MUST NOT BE ABLE TO TRIGGER THE LIVE HAZARD. Planting a
`locally-validated` label on a cold PR can satisfy merge-gate and AUTO-MERGE:
planting an AUTHORISATION is unsafe; planting an INERT artifact is safe. That
rule is enforced here rather than remembered -- every mutant declares a
`hazard` class, and `Mutant.__post_init__` REFUSES to construct an
authorisation-capable mutant. A precondition you have to remember is one you
will eventually forget at 3am.

USAGE
  python3 ci-hub/validate/mutation_suite.py            # run, print the score
  python3 ci-hub/validate/mutation_suite.py --json     # machine-readable
  python3 ci-hub/validate/mutation_suite.py --guard qualified_rows
Exit 0 iff every mutant was killed AND every population control held AND every
registered mutant actually RAN. Exit 1 on a real failure (survivor, skipped
probe, or a broken population control); exit 2 when nothing failed but some
registered guard could not be exercised here -- a clean score over guards that
never ran is the vacuity this suite exists to detect, so it does not read as 0.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import json
import tempfile
import subprocess
import os
from pathlib import Path
import sys
from typing import Any, Callable, Optional

HERE = Path(__file__).resolve().parent
CI_HUB = HERE.parent
PARENT = CI_HUB.parent
for p in (HERE, CI_HUB, CI_HUB / "landing", CI_HUB / "timeout_audit", CI_HUB / "ledger"):
    sys.path.insert(0, str(p))
import validate_rows as canonical_ledger  # noqa: E402
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

KILLED = "KILLED"
SURVIVED = "SURVIVED"
SKIPPED = "SKIPPED"
# A mutant that COULD NOT BE RUN for a declared structural reason -- the fixture
# is not on main yet, or the interpreter it needs (a built `hermit`) is absent.
# Distinct from SKIPPED, which means the probe RAISED and is therefore a defect
# in the suite. Both are uncounted; neither is ever a kill. The distinction
# matters because UNAVAILABLE is expected and actionable ("land the branch",
# "point HERMIT_BIN at a build") while SKIPPED is a bug.
UNAVAILABLE = "UNAVAILABLE"

# Hazard classes. Only INERT mutants may be constructed: an INERT mutant cannot
# authorise anything even if it leaked, which is the property that makes the
# suite safe to run unattended.
INERT = "inert"
AUTHORISATION = "authorisation"


class UnsafeMutant(Exception):
    """Refused: this mutant could trigger the live hazard it is testing."""


class FixtureUnavailable(Exception):
    """This mutant cannot run here, for a declared reason. NEVER a kill.

    Raised (not returned) so a probe physically cannot report "guard refused
    the mutant" when it never got as far as asking the guard.
    """


@dataclass
class Mutant:
    """One planted violation, and the guard that must refuse it."""

    id: str
    guard: str
    # What real defect this encodes -- the sentence a reader needs to know WHY.
    defect: str
    # Returns True when the guard REFUSED the mutant (i.e. the mutant is killed).
    probe: Callable[[], bool]
    hazard: str = INERT
    note: str = ""

    def __post_init__(self) -> None:
        if self.hazard != INERT:
            raise UnsafeMutant(
                f"{self.id}: hazard={self.hazard!r}. Planting an authorisation is "
                "unsafe -- exercise the CONSUMER with an inert fixture or a dry run "
                "instead. Refused at construction, not at review."
            )


@dataclass
class Population:
    """The positive control for a guard: legitimate items that must NOT be flagged."""

    guard: str
    describe: str
    # Returns (flagged, total). `flagged` must be 0 for the control to hold.
    count: Callable[[], tuple[int, int]]


@dataclass
class GuardReport:
    guard: str
    killed: list[str] = field(default_factory=list)
    survived: list[dict] = field(default_factory=list)
    skipped: list[dict] = field(default_factory=list)
    unavailable: list[dict] = field(default_factory=list)
    population: Optional[dict] = None


# --------------------------------------------------------------------------- probes
# Each probe returns True iff the guard REFUSED the mutant. Probes are tiny and
# call the REAL guard -- a probe that reimplemented the predicate would score
# itself, which is the failure this suite exists to detect.


# ------------------------------------------------------- hermit C-fixture probes
#
# The backend-parity C fixtures each carry their own negative bracket: compiling
# with -DHERMIT_TEST_ORACLE_NEGATIVE drops one satisfied check, so a working
# fixture exits 0 clean and 1 mutated. That is a real mutation and this is the
# subprocess probe mechanism the original catalogue lacked.
#
# *** THESE FIXTURES MUST BE RUN UNDER HERMIT, NEVER NATIVELY. ***
#
# This is not a preference. A hand audit of these fixtures ran them natively and
# concluded membarrier_query was BROKEN with an inert negative bracket. It is
# not broken. Natively it exits 1 BOTH clean and mutated -- because natively the
# host's real membarrier mask (1023 on kernel 6.18) is exactly the host-dependent
# value the fixture exists to reject. Under hermit, detcore normalizes the mask
# to its emulated set (31), so the fixture reads 0 clean and 1 mutated and
# discriminates perfectly.
#
# The general rule: a fixture whose contract is "hermit replaces this host value"
# CANNOT be bracketed on the host, because on the host the contract is genuinely
# violated. A native run of such a fixture yields a false BROKEN verdict. So when
# no hermit binary is available this probe reports UNAVAILABLE and declines to
# have an opinion, rather than guessing from a native run.

def _hermit_checkout() -> Path:
    """Which hermit checkout to read fixtures from.

    Defaults to the parent's primary. `HERMIT_CHECKOUT` overrides it, because the
    primary is a coordinator-owned integration surface that an ordinary agent must
    not update, and it is routinely a few commits behind main -- in which case a
    fixture that IS on main is simply absent from its working tree. Reporting that
    as "not on main" would be wrong, so the reason string names the checkout and
    its HEAD rather than guessing.
    """
    env = os.environ.get("HERMIT_CHECKOUT")
    return Path(env) if env else PARENT / "hermit"


def _fixture_path(name: str) -> Path:
    return _hermit_checkout() / "tests" / "backend-parity" / "fixtures" / name


# The bounded entry point every hermit run in this file goes through.
_SAFEHERMIT = PARENT / "bin" / "safehermit"


def _hermit_binary() -> Optional[Path]:
    """A built `hermit`, or None. `HERMIT_BIN` wins; then conventional builds."""
    env = os.environ.get("HERMIT_BIN")
    if env:
        p = Path(env)
        return p if p.is_file() and os.access(p, os.X_OK) else None
    for rel in ("hermit/target/debug/hermit", "hermit/target/release/hermit"):
        p = PARENT / rel
        if p.is_file() and os.access(p, os.X_OK):
            return p
    return None


def _run_c_fixture_under_hermit(source: Path, negative: bool) -> int:
    """Compile `source` (optionally mutated) and run it under hermit. Returns rc."""
    hermit = _hermit_binary()
    if hermit is None:
        raise FixtureUnavailable(
            "no built `hermit` found. Set HERMIT_BIN=/path/to/hermit, or build one "
            "(cargo build -p hermit --bin hermit). Refusing to run this fixture "
            "NATIVELY: for a fixture whose contract is 'hermit replaces this host "
            "value', a native run reports a FALSE broken verdict -- that exact "
            "mistake is why membarrier_query was recorded as broken when it works."
        )
    if not source.is_file():
        co = _hermit_checkout()
        head = "unknown"
        try:
            head = bounded_capture.git(
                ["rev-parse", "--short", "HEAD"], cwd=co
            ).stdout.strip() or "unknown"
        except Exception:
            pass
        raise FixtureUnavailable(
            f"{source.name} absent from checkout {co} (HEAD {head}). Either it is "
            "still on an unlanded branch, or this checkout is behind main -- the "
            "reason is deliberately not guessed. Point HERMIT_CHECKOUT at a "
            "checkout that has it; the mutant scores the moment it is reachable."
        )
    # NOT the default /tmp: hermit refuses to run a program under host /tmp
    # ("Hermit replaces guest /tmp with an isolated directory"), so a fixture
    # built there fails for a reason that has nothing to do with its contract --
    # and the NEGATIVE half would then be "killed" for that wrong reason while
    # the clean half looks broken. Build somewhere hermit will execute from.
    scratch = PARENT / "scratch" / "mutation-suite"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=str(scratch)) as td:
        exe = Path(td) / "fixture"
        cc = ["gcc", "-O1", "-o", str(exe), str(source)]
        if negative:
            cc.insert(2, "-DHERMIT_TEST_ORACLE_NEGATIVE")
        build = bounded_capture.capture(cc)
        if build.returncode != 0:
            raise FixtureUnavailable(
                f"cannot compile {source.name}: {build.stderr.strip()[:300]}")
        # --base-env=minimal keeps the host environment out of the comparison.
        #
        # Run hermit through bin/safehermit rather than bare. The existing
        # timeout=300 below reads like a bound and is not one: hermit's guest runs as
        # a namespace PID 1 with no SIGTERM handler, and the kernel DISCARDS
        # default-disposition signals to it, so the SIGKILL subprocess.run sends on
        # timeout cannot reap this process shape. safehermit's deadline is a cgroup
        # kill, which can.
        #
        # THE DEADLINE IS SET BELOW subprocess.run's SO IT FIRES FIRST. At 240s
        # safehermit cgroup-kills the run and returns 124 while python is still
        # waiting; if python's 300s fired first it would kill only the wrapper and
        # leave the transient unit running. The order matters more than either value.
        #
        # THE BYTE CAP IS NOT DERIVED FROM A HEALTHY RUN'S SIZE, because a healthy run
        # here writes 5 bytes (measured, 3 of 3 identical) and 3x of that is not a
        # cap, it is a tripwire on any diagnostic line. It is set so it CANNOT fire on
        # a good run -- 16 MiB is over three million times the measured output -- while
        # still bounding a wedged run: at the 5.32 MiB/s this hermit sustains when its
        # emitter is loud (measured on this box), 16 MiB is about three seconds of
        # runaway rather than the hours that filled the filesystem on 2026-08-17.
        proc = subprocess.run(
            [str(_SAFEHERMIT), "--sh-report", os.devnull,
             "--sh-deadline", "240", "--sh-max-log-bytes", str(16 * 1024 * 1024),
             str(hermit), "run", "--strict", "--base-env=minimal", "--", str(exe)],
            capture_output=True, text=True, timeout=300)
        return proc.returncode


def _c_fixture_mutants(mutants: list[Mutant], guard: str, fixture: str,
                       pins: str) -> None:
    """Register both halves of one C fixture's bracket as mutants.

    Both halves are mutants on purpose. "The mutated build fails" alone is
    satisfied by a fixture that fails on everything, which gets disabled within
    a day; "the clean build passes" alone is satisfied by one that fails on
    nothing. Only the pair says the oracle DISCRIMINATES.
    """
    src = _fixture_path(fixture)

    mutants.append(Mutant(
        f"{guard}/negative-bracket-fires", guard,
        f"{pins} -- with one contract check deliberately dropped "
        "(-DHERMIT_TEST_ORACLE_NEGATIVE) the fixture must FAIL; if it still "
        "passes, the fixture cannot detect the violation it claims to pin",
        lambda: _run_c_fixture_under_hermit(src, negative=True) != 0))

    mutants.append(Mutant(
        f"{guard}/clean-build-passes", guard,
        f"{pins} -- the UNMUTATED fixture must PASS under hermit; a fixture "
        "that fails on everything is as useless as one that fails on nothing",
        lambda: _run_c_fixture_under_hermit(src, negative=False) == 0))


def _catalogue() -> tuple[list[Mutant], list[Population]]:
    mutants: list[Mutant] = []
    pops: list[Population] = []

    # ---- guard: landing preflight -----------------------------------------
    try:
        import preflight as PF

        def m(mid, defect, fn, note=""):
            mutants.append(Mutant(mid, "landing-preflight", defect, fn, note=note))

        m("preflight/stale-sha",
          "four handed SHAs went stale in one night; one was quoted into agent "
          "instructions for hours after main advanced twice",
          lambda: PF.check_sha_is_current("a" * 40, "b" * 40).verdict == PF.REFUSE)
        m("preflight/unresolvable-head-is-not-pass",
          "an unanswerable check must not read as a satisfied one",
          lambda: not PF.check_sha_is_current("a" * 40, None).ok)
        m("preflight/zero-executed-tests",
          "--features gating: build ok, target ran, ZERO tests executed, SUCCESS reported",
          lambda: PF.check_green_carries_executed_tests(
              __file__, "a" * 40,
              typed_reader=lambda _path, _sha: (
                  1, '{"executed_tests":0}', ""
              )).verdict == PF.REFUSE)
        m("preflight/absent-typed-row",
          "no typed record is not a pass, even when human output still exists",
          lambda: PF.check_green_carries_executed_tests(
              "/no/such/typed-row.json", "a" * 40
          ).verdict == PF.REFUSE)
        m("preflight/unreadable-typed-row",
          "a reader failure must stay UNKNOWN and must never fall back to prose",
          lambda: PF.check_green_carries_executed_tests(
              __file__, "a" * 40,
              typed_reader=lambda _path, _sha: (2, "", "reader failed")
          ).verdict == PF.UNKNOWN)
        m("preflight/merged-flag-without-ancestry",
          "~12 PRs orphaned by force-push on 2026-08-03 were still flagged MERGED",
          lambda: PF.check_landed_by_ancestry(
              pr_state="MERGED", merge_commit_oid="c" * 40,
              is_ancestor=lambda s: False, fetched_fresh=True).verdict == PF.REFUSE)
        m("preflight/stale-remote-is-not-pass",
          "a stale ref answers about the past",
          lambda: not PF.check_landed_by_ancestry(
              pr_state="MERGED", merge_commit_oid="c" * 40,
              is_ancestor=lambda s: True, fetched_fresh=False).ok)
        m("preflight/reverie-patch-override",
          "an uncommitted [patch.\"...reverie.git\"] override riding along in a commit "
          "(live in worktrees/slots/250-delegate/hermit)",
          lambda: PF.check_no_uncommitted_patch_override(
              '+[patch."https://github.com/rrnewton/reverie.git"]\n'
              '+reverie = { path = "../reverie" }\n').verdict == PF.REFUSE)
        m("preflight/byte-identical-branch",
          "opening work that already exists verbatim on the remote (live on #355)",
          lambda: PF.check_no_byte_identical_branch(
              candidate_tree="t1", remote_trees={"fix/a": "t1"}).verdict == PF.REFUSE)

        pops.append(Population(
            "landing-preflight",
            "legitimate inputs that must NOT be refused",
            lambda: (
                sum(1 for ok in [
                    PF.check_sha_is_current("a" * 40, "a" * 40).ok,
                    PF.check_sha_is_current("A" * 40, "a" * 40).ok,   # case is not staleness
                    PF.check_green_carries_executed_tests(
                        __file__, "a" * 40,
                        typed_reader=lambda _path, _sha: (
                            0, '{"executed_tests":36}', ""
                        )).ok,
                    PF.check_landed_by_ancestry(
                        pr_state="MERGED", merge_commit_oid="c" * 40,
                        is_ancestor=lambda s: True, fetched_fresh=True).ok,
                    PF.check_no_uncommitted_patch_override("+fn x() {}\n").ok,
                    PF.check_no_byte_identical_branch(
                        candidate_tree="t9", remote_trees={"fix/a": "t1"}).ok,
                ] if not ok),
                6,
            )))
    except Exception as err:                                  # pragma: no cover
        mutants.append(Mutant("preflight/UNAVAILABLE", "landing-preflight",
                              f"import failed: {err}", lambda: False))

    # ---- guard: qualified-rows accessor ------------------------------------
    try:
        import qualified_rows as QR

        def _row(**over):
            r = {"commit": "a" * 40, "finished_at": "2026-08-04T10:00:00Z",
                 "result": "pass", "executed_tests": 10,
                 "gates_run": 5, "gates_expected": 5}
            r.update(over)
            return r

        mutants += [
            Mutant("qualified_rows/zero-executed", "qualified_rows",
                   "a zero-executed run is a no-result wearing a success badge",
                   lambda: not QR.is_qualified(_row(executed_tests=0))),
            Mutant("qualified_rows/partial-gates", "qualified_rows",
                   "a 5-of-6 partial run is incomplete, not a pass",
                   lambda: not QR.is_qualified(_row(gates_run=5, gates_expected=6))),
            Mutant("qualified_rows/bool-executed", "qualified_rows",
                   "True is an int in Python: `True > 0` would sneak a bool through",
                   lambda: not QR.is_qualified(_row(executed_tests=True))),
            Mutant("qualified_rows/not-pass", "qualified_rows",
                   "a non-pass row must never enter the green population",
                   lambda: not QR.is_qualified(_row(result="fail"))),
            Mutant("qualified_rows/no-event-time", "qualified_rows",
                   "ordering by file position instead of event time caused a false "
                   "13h schema-5 producer outage report",
                   lambda: not QR.is_qualified(_row(finished_at=None))),
        ]
        def _pop():
            rows = canonical_ledger.rows(root=PARENT)
            qual = [r for r in rows if QR.is_qualified(r)]
            # The control: a guard that killed everything would qualify 0.
            return (0 if qual else 1, len(rows))
        pops.append(Population("qualified_rows",
                               "real ledger rows still qualify (guard is not kill-everything)",
                               _pop))
    except Exception as err:                                  # pragma: no cover
        mutants.append(Mutant("qualified_rows/UNAVAILABLE", "qualified_rows",
                              f"import failed: {err}", lambda: False))

    # ---- guard: green-class provenance -------------------------------------
    try:
        import green_class as GC

        def _grow(**over):
            r = {"commit": "a" * 40}
            r.update(over)
            return r

        mutants += [
            Mutant("green_class/laundered-label", "green_class",
                   "stamping green_class=hard on an inherited row makes it read "
                   "byte-identical to a real green",
                   lambda: GC.derive_class(_grow(
                       green_class=GC.HARD, validated_head_sha="b" * 40,
                       inherited_from={"delta_kind": GC.DELTA_REBASE_ONLY,
                                       "branch_commits": 0}))[0] == GC.REFUSED),
            Mutant("green_class/soft-not-hard", "green_class",
                   "validation that ran on an ancestor is not a measurement here",
                   lambda: GC.derive_class(_grow(
                       validated_head_sha="b" * 40,
                       inherited_from={"delta_kind": GC.DELTA_REBASE_ONLY,
                                       "branch_commits": 0}))[0] != GC.HARD),
            Mutant("green_class/contradiction", "green_class",
                   "claims exact-head validation AND inheritance at once",
                   lambda: GC.derive_class(_grow(
                       validated_head_sha="a" * 40,
                       inherited_from={"delta_kind": GC.DELTA_REBASE_ONLY}))[0] == GC.REFUSED),
        ]
        pops.append(Population(
            "green_class",
            "a legacy row with no provenance fields still derives HARD",
            lambda: (0 if GC.derive_class({"commit": "a" * 40})[0] == GC.HARD else 1, 1)))
    except Exception as err:                                  # pragma: no cover
        mutants.append(Mutant("green_class/UNAVAILABLE", "green_class",
                              f"import failed: {err}", lambda: False))

    # ---- guard: check-outcome classifier -----------------------------------
    try:
        from check_outcome import CheckOutcome, classify_check

        mutants += [
            Mutant("check_outcome/in-flight-stale-success", "check_outcome",
                   "a re-run in flight can still carry the previous attempt's "
                   "conclusion; reading it as PASSED is a false green",
                   lambda: classify_check("in_progress", "success") is CheckOutcome.NO_RESULT),
            Mutant("check_outcome/cancelled-is-not-a-result", "check_outcome",
                   "a cancelled run carries no verdict",
                   lambda: classify_check("completed", "cancelled") is CheckOutcome.NO_RESULT),
        ]
        pops.append(Population(
            "check_outcome",
            "completed checks still resolve normally",
            lambda: (sum(1 for ok in [
                classify_check("completed", "success") is CheckOutcome.PASSED,
                classify_check("completed", "failure") is CheckOutcome.FAILED,
            ] if not ok), 2)))
    except Exception as err:                                  # pragma: no cover
        mutants.append(Mutant("check_outcome/UNAVAILABLE", "check_outcome",
                              f"import failed: {err}", lambda: False))

    # ---- guard: .gitmodules hazard ratchet ---------------------------------
    try:
        import gitmodules_lint as GL

        base = ('[submodule "x"]\n\tpath = x\n'
                '\turl = https://example.com/x.git\n\tupdate = checkout\n')

        def _haz(extra: str) -> bool:
            return bool(GL.lint_file.__wrapped__(extra)) if False else bool(
                [v for e in GL.parse_gitmodules(base + extra) for v in GL.hazards_in(e)])

        mutants += [
            Mutant("gitmodules/shallow", "gitmodules_lint",
                   "a shallow submodule makes cold-clone verify pass by removing the "
                   "history it would have checked",
                   lambda: _haz("\tshallow = true\n")),
            Mutant("gitmodules/update-none", "gitmodules_lint",
                   "update = none leaves an empty directory where a consumer expects a tree",
                   lambda: bool([v for e in GL.parse_gitmodules(
                       base.replace("update = checkout", "update = none"))
                       for v in GL.hazards_in(e)])),
            Mutant("gitmodules/branch-field", "gitmodules_lint",
                   "a branch field turns an exact gitlink into a moving target",
                   lambda: _haz("\tbranch = main\n")),
        ]

        def _gm_pop():
            paths = GL.discover(PARENT)
            flagged = sum(len(GL.lint_file(p)["violations"]) for p in paths)
            entries = sum(GL.lint_file(p)["entries"] for p in paths)
            return (flagged, entries)
        pops.append(Population("gitmodules_lint",
                               "real .gitmodules entries are not flagged", _gm_pop))
    except Exception as err:                                  # pragma: no cover
        mutants.append(Mutant("gitmodules/UNAVAILABLE", "gitmodules_lint",
                              f"import failed: {err}", lambda: False))

    # ---- guard: the LIVE zero-test chain (counter -> ledger consumer) -------
    # The owner recorded "KNOWN GAP: an EMPTY log still passes this check". Probed
    # against the LIVE chain rather than assumed: `executed_test_count` returns
    # None (not 0) for an empty log, so whether the gap is real depends entirely
    # on what the CONSUMER does with None. Testing the consumer is also the safe
    # pattern -- no artifact is planted anywhere near an authorisation.
    try:
        sys.path.insert(0, str(CI_HUB / "remediation"))
        import nonzero_result as NR
        import qualified_rows as QR2
        import anchor_select as AS2

        pred = json.loads((HERE / "qualifying-receipt.json").read_text())

        def _row_exec(v):
            return {"commit": "a" * 40, "finished_at": "2026-08-04T10:00:00Z",
                    "result": "pass", "executed_tests": v, "gates_run": 5,
                    "gates_expected": 5, "profile": "full", "selection_mode": "full",
                    "commit_anchored": True, "tree_dirty": False, "schema_version": 1}

        mutants += [
            Mutant("zero_test/empty-log-is-not-zero", "zero-test-chain",
                   "an empty log must not be read as a counted zero; the counter "
                   "must say 'no evidence' (None), not 'zero tests'",
                   lambda: NR.executed_test_count("") is None),
            Mutant("zero_test/counted-zero-is-detected", "zero-test-chain",
                   "the --features-gating shape: a real 'running 0 tests' banner",
                   lambda: NR.executed_test_count("running 0 tests\n") == 0),
            Mutant("zero_test/consumer-refuses-absent-count", "zero-test-chain",
                   "THE RECORDED GAP, tested at the consumer: a ledger row whose "
                   "executed_tests is null (what an empty log produces) must be "
                   "refused, not waved through for lack of a zero to find",
                   lambda: (not QR2.is_qualified(_row_exec(None)))
                   and (not AS2.row_qualifies(_row_exec(None), pred)[0])),
        ]
        pops.append(Population(
            "zero-test-chain",
            "a genuine count still passes the whole chain",
            lambda: (0 if (NR.executed_test_count("running 36 tests\n") == 36
                           and QR2.is_qualified(_row_exec(36))) else 1, 1)))
    except Exception as err:                                  # pragma: no cover
        mutants.append(Mutant("zero_test/UNAVAILABLE", "zero-test-chain",
                              f"probe unavailable: {err}", lambda: False))

    # ---- guard: the ancestry primitive (a KNOWN LEAK, probed deliberately) --
    try:
        import preflight as PF

        def _pr_head_form_survives() -> bool:
            """The DEFECTIVE form: ancestry tested on the PR HEAD.

            After a rebase replay the head is never an ancestor, so this form
            answers NOT-LANDED for a PR that HAS landed -- the read that scored
            79 unlanded when 46 had landed. The guard that must kill this mutant
            is "never test the head"; `check_landed_by_ancestry` structurally
            cannot, so the mutant is killed by construction. Recorded explicitly
            because the leak lives in ANY OTHER call site that still uses the
            head form, and this suite can only speak for this one.
            """
            asked: list[str] = []

            def spy(sha):
                asked.append(sha)
                return sha == "c" * 40

            PF.check_landed_by_ancestry(
                pr_state="MERGED", merge_commit_oid="c" * 40, pr_head="d" * 40,
                is_ancestor=spy, fetched_fresh=True)
            return asked == ["c" * 40]

        mutants.append(Mutant(
            "ancestry/pr-head-form", "ancestry-primitive",
            "testing ancestry on the PR head is ALWAYS false after a rebase replay; "
            "that misread 79 PRs as unlanded when 46 had landed",
            _pr_head_form_survives,
            note="kills only the preflight's call site; other call sites are NOT covered"))
    except Exception:                                         # pragma: no cover
        pass

    # ---- guards: the hermit contract fixtures ------------------------------
    #
    # Registered here so their can-it-fail property stops being a fact somebody
    # established by hand once. A hand verification decays silently: the next
    # edit to the fixture, to the parity seam, or to the syscall handler it
    # pins can make it vacuous, and nothing says so.
    #
    # Landing status is deliberately NOT hardcoded as a comment that will rot --
    # the probe resolves the real path and reports UNAVAILABLE with the reason
    # when it is absent, so this catalogue self-corrects as branches land.

    _c_fixture_mutants(
        mutants, "fixture/membarrier_query", "membarrier_query.c",
        "membarrier(CMD_QUERY) returns detcore's emulated command set (31), not "
        "the host kernel's mask (1023 here) -- a host capability value must not "
        "reach the guest")

    _c_fixture_mutants(
        mutants, "fixture/personality_domain", "personality_domain.c",
        "the personality(2) domain is fail-closed rather than reflecting host state")

    _c_fixture_mutants(
        mutants, "fixture/rlimit_identity", "rlimit_identity.c",
        "getrlimit/setrlimit report identical VALUES across backends -- one of "
        "only two parity fixtures that emit the value through the parity_probe.h "
        "seam instead of collapsing it to a boolean")

    _c_fixture_mutants(
        mutants, "fixture/sched_getaffinity_identity", "sched_getaffinity_identity.c",
        "sched_getaffinity reports an identical mask across backends rather than "
        "the host's CPU set")

    # The remaining three named guards are Rust/Python rather than C fixtures and
    # need their own probe shapes (cargo test; invoking the harness). Registered
    # as declared holes rather than omitted: an unregistered guard is invisible,
    # and invisibility is the decay this task exists to stop. Each raises
    # FixtureUnavailable with what it needs, so it is counted and named on every
    # run without ever being mistaken for a kill.
    def _todo(guard: str, need: str):
        def probe() -> bool:
            raise FixtureUnavailable(need)
        mutants.append(Mutant(
            f"{guard}/probe-not-implemented", guard,
            "registered so the gap is COUNTED AND NAMED on every run; an "
            "unregistered guard is an invisible one", probe))

    _todo("fixture/parity_mutation_harness",
          "needs a probe that invokes hermit/tests/backend-parity/parity_mutation.py "
          "and asserts its EXIT CODE (not its printed verdict -- a hand check of "
          "this harness first read rc=0 from a `tail` in the pipeline rather than "
          "from the harness). Absent from main; lives only on the local-only "
          "branch mutation-audit-fixtures.")
    _todo("fixture/file_contents_detlog_determinism",
          "needs a `cargo test` probe (minutes, and a hermit build). Pins that no "
          "raw host inode reaches ResourceID::FileContents. Absent from main; "
          "lives only on the local-only branch mutation-audit-fixtures.")
    _todo("fixture/register_file_hashing",
          "needs a `cargo test` probe plus a product-seam mutation in "
          "detcore/src/regdigest.rs. Absent from main; lives only on the "
          "local-only branch mutation-audit-fixtures.")

    return mutants, pops


# --------------------------------------------------------------------------- runner


def run(only_guard: Optional[str] = None) -> dict[str, Any]:
    mutants, pops = _catalogue()
    if only_guard:
        mutants = [m for m in mutants if m.guard == only_guard]
        pops = [p for p in pops if p.guard == only_guard]

    reports: dict[str, GuardReport] = {}
    for m in mutants:
        rep = reports.setdefault(m.guard, GuardReport(m.guard))
        try:
            killed = bool(m.probe())
        except FixtureUnavailable as err:
            # Declared, expected, and never a kill.
            rep.unavailable.append({"id": m.id, "reason": str(err)})
            continue
        except Exception as err:
            rep.skipped.append({"id": m.id, "error": f"{type(err).__name__}: {err}"})
            continue
        if killed:
            rep.killed.append(m.id)
        else:
            rep.survived.append({"id": m.id, "defect": m.defect, "note": m.note})

    for p in pops:
        rep = reports.setdefault(p.guard, GuardReport(p.guard))
        try:
            flagged, total = p.count()
            rep.population = {"describe": p.describe, "flagged": flagged,
                              "total": total, "holds": flagged == 0}
        except Exception as err:
            rep.population = {"describe": p.describe, "error": str(err), "holds": False}

    killed = sum(len(r.killed) for r in reports.values())
    survived = sum(len(r.survived) for r in reports.values())
    skipped = sum(len(r.skipped) for r in reports.values())
    unavailable = sum(len(r.unavailable) for r in reports.values())
    scored = killed + survived
    pops_total = sum(1 for r in reports.values() if r.population is not None)
    pops_holding = sum(1 for r in reports.values()
                       if r.population is not None and r.population.get("holds"))

    return {
        "schema_version": 1,
        "mutation_score": {
            "killed": killed,
            "total_scored": scored,
            "skipped": skipped,
            "unavailable": unavailable,
            "percent": round(100.0 * killed / scored, 1) if scored else None,
        },
        "population_controls": {"holding": pops_holding, "total": pops_total},
        "survivors": [s for r in reports.values() for s in r.survived],
        "guards": {
            g: {"killed": r.killed, "survived": r.survived, "skipped": r.skipped,
                "unavailable": r.unavailable, "population": r.population}
            for g, r in sorted(reports.items())
        },
    }


def render(rep: dict) -> str:
    s = rep["mutation_score"]
    out = ["MUTATION SUITE FOR GUARDS", ""]
    for g, d in rep["guards"].items():
        pop = d["population"]
        if pop is None:
            pop_s = "population control: NONE (guard scored on mutants only)"
        elif pop.get("holds"):
            pop_s = (f"population control HOLDS: {pop['flagged']} flagged of "
                     f"{pop['total']} legitimate")
        else:
            pop_s = f"population control FAILED: {pop}"
        out.append(f"  {g}: killed {len(d['killed'])}/"
                   f"{len(d['killed']) + len(d['survived'])}  |  {pop_s}")
        for sv in d["survived"]:
            out.append(f"      SURVIVED  {sv['id']} -- {sv['defect']}")
        for sk in d["skipped"]:
            out.append(f"      SKIPPED   {sk['id']} -- {sk['error']}")
        for un in d.get("unavailable", []):
            out.append(f"      UNAVAILABLE {un['id']} -- {un['reason']}")
    out += ["",
            f"MUTATION SCORE: {s['killed']}/{s['total_scored']}"
            + (f" ({s['percent']}%)" if s["percent"] is not None else ""),
            f"POPULATION CONTROLS: {rep['population_controls']['holding']}/"
            f"{rep['population_controls']['total']} holding"]
    if s["skipped"]:
        out.append(f"SKIPPED (uncounted, NOT killed): {s['skipped']}")
    if s.get("unavailable"):
        # Loud on purpose. A guard that cannot run is exactly as protective as a
        # guard that does not exist, and the whole point of registering these is
        # that the shortfall is stated on every run instead of being remembered.
        out.append(
            f"UNAVAILABLE (uncounted, NOT killed): {s['unavailable']} "
            "-- registered guards that could not be exercised here; see reasons above")
    if rep["survivors"]:
        out.append("")
        out.append("SURVIVORS -- each is a guard reporting health while not guarding:")
        for sv in rep["survivors"]:
            out.append(f"  - {sv['id']}: {sv['defect']}")
    return "\n".join(out)


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--guard", help="run only one guard's mutants")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    rep = run(args.guard)
    print(json.dumps(rep, indent=2) if args.json else render(rep))

    s = rep["mutation_score"]
    failed = (s["killed"] != s["total_scored"] or s["skipped"] != 0
              or rep["population_controls"]["holding"]
              != rep["population_controls"]["total"])
    if failed:
        return 1
    # A clean score over guards that could not RUN is the exact vacuity this
    # suite exists to detect, so incomplete coverage gets its own exit code
    # rather than being folded into success. 0 means "no failures AND nothing
    # unexercised"; 2 means "no failures, but the denominator is short".
    # Callers that want a hard gate should treat 2 as failure.
    if s.get("unavailable"):
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
