#!/usr/bin/env python3
"""Launch one detached, admitted Hermit or Reverie validation through ci-hub."""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import re
import secrets
import shlex
import shutil
import subprocess
import tempfile
import sys
import time
from contextlib import contextmanager
from collections.abc import Callable, Mapping, Sequence
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, TextIO

import pane_lifecycle
import pane_owner
import run_registry
import service_result


ROOT = Path(__file__).resolve().parents[2]
HOST_TMP_ROOT = Path("/tmp").resolve()
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
UNIT_RE = re.compile(r"^validate-[A-Za-z0-9_.@:-]+$")
Runner = Callable[..., subprocess.CompletedProcess[str]]
TERMINAL_STATES = frozenset(("failed", "inactive"))
# THE THREE OUTCOMES A CALLER MUST TELL APART, and one of them used to be
# invisible.
#
# REFUSED means the validate NEVER RAN -- a bad argument, a held lock, a missing
# precondition. FAILED means it ran and something was wrong. They demand opposite
# reactions: retry versus investigate.
#
# ⚠️ COULD-NOT-DETERMINE IS THE THIRD, AND IT WAS COLLAPSED INTO REFUSED. Before
# this, `CANONICAL-VERDICT-UNAVAILABLE` -- the run happened but its verdict could
# not be read back -- returned 2, the same code as "you passed --pr -1". A caller
# could not distinguish "your command was wrong" from "the run's result is
# unreadable", and the second one is not fixed by correcting a flag.
#
# 75 is EX_TEMPFAIL and is this project's established spelling for a third state:
# scripts/validate.rs reserves it as the only nonzero code that is not a product
# failure, and ci/lint-checks-node.sh already exits 75 for its setup precondition.
# Reused rather than inventing a fourth convention.
EXIT_PASSED = 0
EXIT_REFUSED = 2
EXIT_FAILED = 3
EXIT_REMEASURE = 4
EXIT_COULD_NOT_DETERMINE = 75
FROZEN_VALIDATE_KIND = "frozen-validate"
FROZEN_RESULT_ADMISSION = "frozen-validate"
CURRENT_MAIN_FETCH_TIMEOUT_SECONDS = 30
HERMIT_MIN_CLEANUP_GRACE_SECONDS = 60

CANONICAL_TERMINAL_VERDICTS = {"VALIDATED": 0, "FAILED": 3}
CANONICAL_REMEASUREMENT_VERDICTS = frozenset(
    ("TRUNCATED", "NEEDS-RERUN", "NO-RESULT", "NOT-VALIDATED")
)
SUPPORTED_REPOS = {
    "hermit": "rrnewton/hermit",
    "rrnewton/hermit": "rrnewton/hermit",
    "reverie": "rrnewton/reverie",
    "rrnewton/reverie": "rrnewton/reverie",
}


def hermit_run_timeout_seconds(child_deadline: int) -> int:
    """Derive Hermit's inner deadline from validate-lock's child deadline.

    Hermit's checked-in scope ladder reserves ``max(60s, run_timeout / 10)``
    after its in-process deadline so it can stop nodes and flush evidence. The
    canonical launcher used to pass only the outer 3600-second child deadline,
    leaving Hermit unbounded and allowing two attempts of a 2400-second DAG node
    to be killed by validate-lock before Hermit could record the retry result.

    Reserve the same grace from the enclosing deadline, plus one second for a
    strict INNER < OUTER ordering. Because the derived run timeout is smaller
    than the child deadline, Hermit's own cleanup grace cannot exceed the grace
    reserved here.
    """
    cleanup_grace = max(HERMIT_MIN_CLEANUP_GRACE_SECONDS, child_deadline // 10)
    run_timeout = child_deadline - cleanup_grace - 1
    if run_timeout <= 0:
        raise ValueError(
            f"Hermit child deadline {child_deadline}s cannot contain its required "
            f"{cleanup_grace}s cleanup grace plus a positive run timeout"
        )
    hermit_cleanup_grace = max(
        HERMIT_MIN_CLEANUP_GRACE_SECONDS, run_timeout // 10
    )
    if run_timeout + hermit_cleanup_grace >= child_deadline:
        raise ValueError(
            "derived Hermit run timeout does not fit strictly inside the "
            f"validate-lock child deadline: {run_timeout}s + "
            f"{hermit_cleanup_grace}s >= {child_deadline}s"
        )
    return run_timeout


def canonical_repo(value: str) -> str:
    try:
        return SUPPORTED_REPOS[value]
    except KeyError as error:
        raise ValueError(
            f"unsupported validation repository {value!r}; expected rrnewton/hermit or rrnewton/reverie"
        ) from error


def row_matches_repo(row: Mapping[str, Any], repo: str) -> bool:
    value = row.get("repo")
    if repo == "rrnewton/hermit":
        return value in (None, "hermit", repo)
    return value in ("reverie", repo)


def canonical_verdict_exit_code(verdict: str) -> int | None:
    terminal = CANONICAL_TERMINAL_VERDICTS.get(verdict)
    if terminal is not None:
        return terminal
    if verdict in CANONICAL_REMEASUREMENT_VERDICTS:
        return 4
    return None


def frozen_result_path(root: Path, unit: str) -> Path:
    """The durable, deliberately non-canonical record for one frozen validate."""
    return root / "ignored" / "validate" / "frozen" / f"{unit}.jsonl"


def frozen_checkout_parent(root: Path) -> Path:
    """Return a guest-visible worktree parent outside the dev-hermit boundary.

    Hermit's product front door correctly accepts only canonical current-tip
    validation. A frozen validation is still box-admitted, but its result must
    not enter that receipt path. Keeping its disposable checkout beside rather
    than beneath dev-hermit makes the product use its standalone result writer;
    ``HERMIT_VALIDATE_LEDGER`` below then points that writer at the durable,
    explicitly non-canonical file owned by this run.
    """
    candidate = (root.parent / f".{root.name}-frozen-validate").resolve()
    if candidate == root or root in candidate.parents:
        raise ValueError("frozen validation checkout parent must be outside dev-hermit")
    candidate.mkdir(parents=True, exist_ok=True)
    return require_guest_visible_root(candidate, role="frozen validation checkout parent")


def current_main_for_frozen_validate(
    checkout: Path, target: str, *, run: Runner
) -> str:
    """Require ``target`` to omit freshly fetched main and return that main SHA.

    A frozen validate is not a general freshness bypass. It is the explicit
    path for re-running a target after main has moved beyond it. A target that
    still contains current main belongs on the ordinary validating path, while
    an unavailable fetch or ancestry answer refuses instead of guessing.
    """
    refspec = "refs/heads/main:refs/remotes/origin/main"
    fetch = run(
        [
            "timeout",
            "--signal=TERM",
            "--kill-after=1s",
            f"{CURRENT_MAIN_FETCH_TIMEOUT_SECONDS}s",
            "with-proxy",
            "git",
            "-C",
            str(checkout),
            "fetch",
            "--quiet",
            "--no-tags",
            "origin",
            refspec,
        ],
        check=False,
    )
    if fetch.returncode != 0:
        detail = fetch.stderr.strip() or fetch.stdout.strip() or f"exit {fetch.returncode}"
        raise RuntimeError(
            f"cannot freshly fetch origin/main for frozen validation: {detail}"
        )
    current_main = checked_output(
        [
            "git",
            "-C",
            str(checkout),
            "rev-parse",
            "--verify",
            "refs/remotes/origin/main^{commit}",
        ],
        run=run,
        purpose="cannot resolve freshly fetched origin/main for frozen validation",
    )
    if SHA_RE.fullmatch(current_main) is None:
        raise RuntimeError(
            f"freshly fetched origin/main is not an exact lowercase 40-hex SHA: {current_main!r}"
        )
    contains = run(
        [
            "git",
            "-C",
            str(checkout),
            "merge-base",
            "--is-ancestor",
            "refs/remotes/origin/main",
            target,
        ],
        check=False,
    )
    if contains.returncode == 0:
        raise ValueError(
            f"--frozen-validate requires a target that does not contain freshly "
            f"fetched origin/main {current_main}; {target} is current enough for "
            "ordinary validate-run"
        )
    if contains.returncode != 1:
        detail = contains.stderr.strip() or contains.stdout.strip() or f"exit {contains.returncode}"
        raise RuntimeError(
            f"cannot establish whether {target} contains freshly fetched "
            f"origin/main {current_main}: {detail}"
        )
    return current_main


def mark_and_read_frozen_result(
    result_path: Path,
    target: str,
    cwd: Path,
    run_started_at: str,
    current_main_before_launch: str,
    *,
    repo: str,
) -> tuple[str, str, int]:
    """Bind one completed frozen run and make its non-current status explicit.

    The result lives outside the canonical ledger. Before reporting it, replace
    the producer's ordinary admission claim with the existing frozen-validate
    kind and record the exact main SHA the target did not contain. Even if this
    JSONL is later copied into a canonical shard, its non-canonical admission
    makes the shared qualifying-receipt predicate reject it.
    """
    try:
        lines = result_path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise RuntimeError(f"cannot read frozen validation result {result_path}: {error}") from error
    parsed: list[dict[str, Any]] = []
    matches: list[int] = []
    handle_start = parse_utc_timestamp(run_started_at, role="validate-run handle")
    for line in lines:
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as error:
            raise RuntimeError(
                f"frozen validation result {result_path} has malformed JSONL: {error}"
            ) from error
        if not isinstance(value, dict):
            raise RuntimeError(f"frozen validation result {result_path} has a non-object row")
        parsed.append(value)
        if (
            row_matches_repo(value, repo)
            and value.get("commit") == target
            and value.get("cwd") == str(cwd)
            and parse_utc_timestamp(value.get("started_at"), role="frozen validation row")
            >= handle_start
        ):
            matches.append(len(parsed) - 1)
    if len(matches) != 1:
        raise RuntimeError(
            f"expected exactly one frozen validation row for commit {target}, cwd {cwd}, "
            f"at or after {run_started_at}; found {len(matches)}"
        )

    selected = parsed[matches[0]]
    selected.update(
        {
            "admission": FROZEN_RESULT_ADMISSION,
            "measured_against_superseded_tip": True,
            "current_main_before_launch": current_main_before_launch,
            "target_contains_current_main_before_launch": False,
            "qualifying_receipt": False,
        }
    )
    result = selected.get("result")
    exit_code = selected.get("exit_code")
    if result == "pass" and exit_code == 0:
        wrapper_exit = EXIT_PASSED
    elif result == "fail" and isinstance(exit_code, int) and exit_code != 0:
        wrapper_exit = EXIT_FAILED
    elif result == "no_result":
        wrapper_exit = EXIT_REMEASURE
    else:
        raise RuntimeError(
            f"frozen validation row has inconsistent result={result!r} exit_code={exit_code!r}"
        )

    result_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        descriptor, temporary = tempfile.mkstemp(
            prefix=f".{result_path.name}.", dir=result_path.parent
        )
    except OSError as error:
        raise RuntimeError(
            f"cannot create an atomic rewrite beside frozen result {result_path}: {error}"
        ) from error
    try:
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
                for row in parsed:
                    json.dump(row, stream, separators=(",", ":"))
                    stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, result_path)
        except OSError as error:
            raise RuntimeError(
                f"cannot mark frozen validation result {result_path}: {error}"
            ) from error
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
    return json.dumps(selected, separators=(",", ":")), str(result), wrapper_exit



def refuse(reason: str, remedy: str, *, out: TextIO | None = None) -> int:
    """Report that the run NEVER STARTED, and how to make it start.

    ⚠️ `remedy` IS A REQUIRED PARAMETER ON PURPOSE. A refusal that names a
    condition and no fix reads as a dead end, and several chains stranded that way
    -- the caller cannot tell whether they are blocked or merely holding it wrong.
    Making it positional means a future refusal cannot be added without one.

    Says NEVER RAN in words because the distinction from FAILED is the whole
    point: there is no log to read and no result to investigate, so re-running
    after the remedy is the correct next action.
    """
    stream = out if out is not None else sys.stderr
    print(f"validate-run: REFUSED: {reason}", file=stream)
    print("  state:  REFUSED -- the validate NEVER RAN, so there is no log and no result.", file=stream)
    print(f"  remedy: {remedy}", file=stream)
    return EXIT_REFUSED


def could_not_determine(reason: str, remedy: str, *, out: TextIO | None = None) -> int:
    """The run is not being called passed OR failed, and that is the answer.

    Distinct from both. The validate may well have executed; what is missing is a
    readable verdict, so neither "retry" nor "investigate the failure" is right
    until the evidence is recoverable.
    """
    stream = out if out is not None else sys.stderr
    print(f"validate-run: COULD-NOT-DETERMINE: {reason}", file=stream)
    print("  state:  COULD-NOT-DETERMINE -- not a pass and NOT a failure; the verdict is unreadable.", file=stream)
    print(f"  remedy: {remedy}", file=stream)
    return EXIT_COULD_NOT_DETERMINE



def describe_outcome(exit_code: int) -> str:
    """One line saying which of the outcomes this is, and what to do about it.

    ⚠️ EXISTS BECAUSE THE NUMBER ALONE WAS AMBIGUOUS AT THE CALLER. Retry and
    investigate are opposite actions and the exit code was being asked to carry
    that distinction unaided.
    """
    if exit_code == EXIT_PASSED:
        return "PASSED -- the validate ran and the canonical verdict is VALIDATED"
    if exit_code == EXIT_FAILED:
        return (
            "FAILED -- the validate RAN and something was wrong. Investigate the log; "
            "do not simply retry"
        )
    if exit_code == EXIT_REFUSED:
        return (
            "REFUSED -- the validate NEVER RAN. There is no log and no result; fix the "
            "stated condition and re-run"
        )
    if exit_code == EXIT_COULD_NOT_DETERMINE:
        return (
            "COULD-NOT-DETERMINE -- neither a pass nor a failure; the verdict could not "
            "be read. Recover the evidence before concluding anything"
        )
    if exit_code == EXIT_REMEASURE:
        return "REMEASURE -- the canonical verdict asks for another measurement"
    return f"UNCLASSIFIED exit {exit_code} -- this is itself a defect; no outcome maps to it"


def parse_utc_timestamp(value: object, *, role: str) -> datetime:
    if not isinstance(value, str) or not value:
        raise RuntimeError(f"{role} has no usable timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise RuntimeError(f"{role} has malformed timestamp {value!r}") from error
    if parsed.tzinfo is None:
        raise RuntimeError(f"{role} timestamp has no timezone: {value!r}")
    return parsed.astimezone(timezone.utc)


def row_is_selected_qualifying_receipt(
    row: Mapping[str, Any], qualifying_receipts: object
) -> bool:
    """Bind a canonical VALIDATED verdict to this exact qualifying row.

    `validate-status` deliberately reports the complete qualifying set.  A
    same-SHA validate may finish after this run and become the newest receipt
    before this wrapper reads the ledger; requiring only that newest identity
    makes two successful concurrent runs race each other at publication.
    """
    if not isinstance(qualifying_receipts, list):
        return False
    fields = {
        "sha": "commit",
        "tree": "tree",
        "finished_at": "finished_at",
        "host": "host",
        "slot": "slot",
        "log_file": "log_file",
    }
    for receipt in qualifying_receipts:
        if not isinstance(receipt, dict):
            continue
        identity = receipt.get("receipt_identity")
        if not isinstance(identity, dict):
            continue
        selected = identity.get("tuple")
        if not isinstance(selected, dict):
            continue
        if all(
            selected_key in selected and selected[selected_key] == row.get(row_key)
            for selected_key, row_key in fields.items()
        ):
            return True
    return False


def require_guest_visible_root(path: Path, *, role: str) -> Path:
    """Refuse a program root hidden by Hermit's isolated guest ``/tmp``.

    Hermit deliberately replaces guest ``/tmp`` with an isolated directory.
    A fresh validation checkout below host ``/tmp`` therefore builds valid
    programs that Hermit then refuses to execute.  Resolve first so an
    apparently safe symlink cannot bypass the placement check.
    """
    resolved = path.resolve()
    try:
        resolved.relative_to(HOST_TMP_ROOT)
    except ValueError:
        return resolved
    raise ValueError(
        f"{role} resolves beneath host /tmp ({resolved}); Hermit isolates guest /tmp, "
        "so programs built there are not guest-visible. Use the canonical non-/tmp "
        "dev-hermit parent under your workspace root, or another non-/tmp checkout."
    )


def run_command(command: Sequence[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    """Run a command and capture its output, waiting for THE CHILD, not for EOF.

    ⚠️ `capture_output=True` GIVES THE CHILD A PIPE AND THEN READS TO EOF, and EOF
    arrives when the LAST HOLDER closes it -- not when the child exits. Any
    background grandchild that inherited it keeps the caller blocked after the
    child is already gone. git's `gc --auto` is the everyday example: it defaults
    on (gc.auto=6700) and detaches (gc.autoDetach=true), inheriting stderr.

    Reproduced 2026-08-25 with a child that exits immediately leaving one
    background grandchild: capture_output blocked indefinitely; capturing to a
    file returned in 0.0s with the same stdout and the same exit code. A regular
    file has no EOF-waiting semantics, so `wait()` on the child is the only thing
    that gates the return.

    ⚠️ NOT SOLVED WITH A TIMEOUT, and not by turning auto-gc off. A timeout makes
    an unbounded wait a bounded one and still discards the reason; disabling gc
    hides the mechanism and leaves the next background child to reproduce it. The
    defect is waiting on the wrong thing, so this waits on the right one.
    """
    # ⚠️ `check` IS A `subprocess.run` PARAMETER AND `Popen` REJECTS IT. Moving to
    # Popen (42d27103, to wait on the child rather than on EOF) kept forwarding
    # **kwargs straight through, so every caller that passed `check=` -- 7 sites
    # with check=False and the fresh-checkout helper with check=True -- began
    # raising `TypeError: Popen.__init__() got an unexpected keyword argument
    # 'check'`. That is every path through `checked_output`, which is reached
    # before anything else a launch does, so `ci-hub validate-run` could not start
    # a validation at all.
    #
    # Honour it here rather than deleting it at the call sites: the callers were
    # written against subprocess.run semantics and check=True is load-bearing in
    # create_fresh_checkout, where a failed `git worktree add` must raise instead
    # of yielding an empty tree that validates fast and green.
    check = bool(kwargs.pop("check", False))
    with tempfile.TemporaryFile("w+b") as out, tempfile.TemporaryFile("w+b") as err:
        process = subprocess.Popen(
            list(command), stdout=out, stderr=err, stdin=subprocess.DEVNULL, **kwargs
        )
        code = process.wait()
        out.seek(0)
        err.seek(0)
        completed = subprocess.CompletedProcess(
            list(command),
            code,
            out.read().decode("utf-8", "replace"),
            err.read().decode("utf-8", "replace"),
        )
        if check and code != 0:
            raise subprocess.CalledProcessError(
                code, list(command), completed.stdout, completed.stderr
            )
        return completed


def checked_output(
    command: Sequence[str], *, run: Runner, purpose: str
) -> str:
    result = run(list(command), check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"{purpose}: {detail}")
    return result.stdout.strip()


def sanitize_unit(raw: str) -> str:
    unit = raw.removesuffix(".service")
    if not UNIT_RE.fullmatch(unit):
        raise ValueError(
            f"invalid unit {raw!r}; expected validate- followed by letters, digits, or ._@:-"
        )
    return unit


def default_unit(agent: str, target: str) -> str:
    safe_agent = re.sub(r"[^A-Za-z0-9_.@:-]+", "-", agent).strip("-.")
    if not safe_agent:
        raise ValueError("--agent must contain at least one unit-safe character")
    return sanitize_unit(
        f"validate-{safe_agent}-{target[:12]}-{time.time_ns()}-"
        f"{os.getpid()}-{secrets.token_hex(4)}"
    )


def _validate_output_paths(root: Path, unit: str) -> tuple[Path, Path]:
    base = root / "ignored/validate/artifacts" / unit
    return base / "e2e", base / "safe-ci-dag-runner"


def prepare_runtime_root(parent: Path, unit: str) -> Path:
    """Create the host-side temporary/cache tree owned by one validate run."""
    runtime = parent / "validate-runtime" / unit
    runtime.mkdir(parents=True, mode=0o700, exist_ok=False)
    for name in ("tmp", "cache", "python-cache", "hermit-data"):
        (runtime / name).mkdir(mode=0o700)
    return runtime


def remove_runtime_root(runtime: Path | None, *, run: Runner) -> None:
    if runtime is not None:
        run(["rm", "-rf", str(runtime)], check=False)


def git_common_dir(source: Path, *, run: Runner) -> Path:
    return Path(
        checked_output(
            [
                "git",
                "-C",
                str(source),
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ],
            run=run,
            purpose="cannot resolve shared Git directory",
        )
    )


@contextmanager
def git_mutation_lock(source: Path, *, run: Runner):
    """Serialize worktree/fetch metadata mutation in the shared Git directory."""
    common = git_common_dir(source, run=run)
    common.mkdir(parents=True, exist_ok=True)
    path = common / "ci-hub-validate.lock"
    with path.open("a+") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


# The row lookup below goes through the canonical union command. This module
# never opens a ledger shard itself.


def prepare_fresh_checkout(
    source: Path,
    target: str,
    *,
    run: Runner,
    parent: Path,
    repo: str,
    independent_refs: bool = False,
) -> Path:
    """Materialize `target` into a private temp worktree and PROVE it is usable.

    WHY THIS IS THE DEFAULT. `validate_checkout` below refuses a dirty tree, so
    "no uncommitted changes" is already guaranteed — but `git status
    --porcelain=v1` says NOTHING ABOUT IGNORED FILES, and that is where the
    divergence lives. Measured 2026-08-08 on a slot at 393c6a765: `git status
    --porcelain=v1` reported 0 paths while the tree carried 5.1 GB of ignored
    build output, caches and materialized dependencies. A validate of that tree
    is a validate of the commit PLUS 5.1 GB of unrecorded local history, so the
    40-hex SHA on the receipt does not describe what actually ran.

    Two measured cases from the same day where ignored state DECIDED a verdict:
    an empty (gitignored) `hermit/agent-utils` made `scripts/validate.rs` die in
    0.045s in a way that reads like a fast pass; and a stale (gitignored)
    rust-script binary cache made a tree whose `git status` was empty fail
    `--self-test` on a mutation that had already been reverted.

    THE SECOND CASE IS ALSO THIS FUNCTION'S OWN FAILURE MODE. A fresh worktree
    starts with EMPTY submodules, `agent-utils` among them — so materializing
    the tree and launching without initializing them reproduces the 0.045s
    fake-pass BY CONSTRUCTION. That is why this returns only after proving the
    tree is usable, and raises otherwise: an unusable temp checkout must abort
    the launch, never quietly become a fast green.
    """
    parent = require_guest_visible_root(parent, role="fresh-checkout parent")
    fresh = require_guest_visible_root(
        Path(
            checked_output(
                ["mktemp", "-d", str(parent / "validate-fresh-XXXXXXXX")],
                run=run,
                purpose="cannot create temp checkout directory",
            )
        ),
        role="fresh checkout",
    )
    # Ordinary validation uses a worktree: it shares the source object store,
    # so this costs no object copy. Frozen validation needs independent refs.
    # Its target is deliberately behind the freshly observed origin/main, but
    # the target's own pre.reverie_pin gate uses origin/main as its historical
    # monotonicity floor. Leaving the live ref visible makes every old target
    # whose Reverie pin has since advanced fail before the test matrix starts.
    # A shared clone uses the same object store while keeping refs independent,
    # so its origin/main can name the commit being re-measured without changing
    # the source repository's ref for concurrent users.
    with git_mutation_lock(source, run=run):
        if independent_refs:
            origin_url = checked_output(
                ["git", "-C", str(source), "remote", "get-url", "origin"],
                run=run,
                purpose="cannot resolve source origin for frozen validation",
            )
            run(
                [
                    "git",
                    "clone",
                    "--shared",
                    "--no-checkout",
                    "--no-tags",
                    str(source),
                    str(fresh),
                ],
                check=True,
            )
            run(
                ["git", "-C", str(fresh), "remote", "set-url", "origin", origin_url],
                check=True,
            )
            run(["git", "-C", str(fresh), "checkout", "--detach", target], check=True)
            run(
                [
                    "git",
                    "-C",
                    str(fresh),
                    "update-ref",
                    "refs/remotes/origin/main",
                    target,
                ],
                check=True,
            )
        else:
            run(
                ["git", "-C", str(source), "worktree", "add", "--detach", str(fresh), target],
                check=True,
            )
        head = checked_output(
            ["git", "-C", str(fresh), "rev-parse", "HEAD^{commit}"],
            run=run,
            purpose="cannot resolve fresh checkout HEAD",
        )
        if head != target:
            raise RuntimeError(f"fresh checkout resolved to {head}, not requested target {target}")
        run(
            ["git", "-C", str(fresh), "submodule", "update", "--init", "--recursive"],
            check=False,
        )
    # PROVE usable, do not assume. Each of these is a thing whose absence has
    # produced a misleading fast exit rather than an honest failure.
    #
    # The DAG-runner crate that scripts/validate.rs takes as a path dependency
    # was renamed in agent-utils: rs/safe-ci-dag-runner became rs/dagrun. A
    # target is validatable if it carries EITHER, because both spellings are
    # still reachable -- current main pins the new name, while an older commit
    # or an unrebased PR head pins the old one, and a bisect walks across the
    # rename. Requiring one exact path made every target on the other side of
    # that boundary unlaunchable.
    #
    # The RUNNER EXECUTABLE is required too, and its absence is precisely what
    # took main down at 4b9a56bfc2. The crate-path check above proves the SOURCE
    # is present; it says nothing about whether ci/run-dag.sh can find something
    # to execute. When the rename landed without its hermit half, every lane
    # exited on "safe-ci-dag-runner not found" -- an environmental refusal that
    # produced no result at all. Proving the executable here turns that into an
    # honest, named precondition failure at launch instead of a lane that starts
    # and immediately gives up. Either spelling satisfies it, for the same
    # both-sides-of-the-rename reason as the crate path.
    alternatives: tuple[tuple[str, ...], ...] = (
        (
            ("scripts/validate.rs",),
            (
                "agent-utils/rs/dagrun/Cargo.toml",
                "agent-utils/rs/safe-ci-dag-runner/Cargo.toml",
            ),
            (
                "agent-utils/common/bin/dagrun",
                "agent-utils/common/bin/safe-ci-dag-runner",
                "agent-utils/py/bin/dagrun",
                "agent-utils/py/bin/safe-ci-dag-runner",
            ),
        )
        if repo == "rrnewton/hermit"
        else (("validate.sh",),)
    )
    missing = [
        " or ".join(group)
        for group in alternatives
        if not any((fresh / rel).exists() for rel in group)
    ]
    if missing:
        raise RuntimeError(
            f"fresh checkout {fresh} is missing {', '.join(missing)}; refusing to launch a run "
            "that would exit fast for an environmental reason and read like a pass "
            "(retained for inspection)"
        )
    return fresh


def remove_fresh_checkout(source: Path, fresh: Path, *, run: Runner) -> None:
    """Remove the temp worktree AND deregister it. Best-effort, never fatal."""
    # A frozen checkout is a standalone shared clone: `.git` is its own
    # directory and it has no registration in the source repository. Ordinary
    # fresh checkouts are worktrees, whose `.git` is a file naming the shared
    # common directory.
    if (fresh / ".git").is_dir():
        try:
            shutil.rmtree(fresh)
        except OSError as error:
            print(
                f"validate-run: could not remove isolated checkout {fresh}: {error}",
                file=sys.stderr,
            )
        return
    try:
        with git_mutation_lock(source, run=run):
            run(["git", "-C", str(source), "worktree", "remove", "--force", str(fresh)], check=False)
            run(["git", "-C", str(source), "worktree", "prune"], check=False)
    except RuntimeError as error:
        print(f"validate-run: could not lock shared Git metadata for cleanup: {error}", file=sys.stderr)


# Seeded from the shared cache rather than started empty, and seeded with these
# two subtrees rather than all of it. Both choices are measured, not aesthetic;
# the numbers are in `test_private_cargo_home.py` and in the commit message.
CARGO_SEED_SUBTREES: tuple[tuple[str, str], ...] = (
    ("registry", "registry"),
    ("git/db", "git/db"),
)


def prepare_private_cargo_home(parent: Path, *, run: Runner, shared: Path) -> Path:
    """Give this validate its own CARGO_HOME, seeded by reflink from the shared one.

    WHY. Cargo serialises every cache mutation on one lock file,
    `$CARGO_HOME/.package-cache`. Two validations may now coexist, and neither
    should share that lock with the other validation or with the ~18 agents
    running ordinary `cargo build` in their own slots. When a validate node
    loses that race it does not run slowly, it sits in `Blocking waiting for file
    lock on package cache` until its node budget kills it. Measured on devbig030:
    `privileged-build.privileged_tests` has a median of 0.05s across 21 passing
    runs and died at its 120s budget with nothing else in its output, and 9 of
    14 node timeouts across 78 retained logs show that message.

    This is invisible to every contention signal we had: a process blocked on
    flock consumes no CPU, so load average and all three PSI counters read
    clean while a node burns its whole budget. That is why these reds were
    repeatedly attributed to a busy box and rerun rather than read.

    WHY SEEDED, AND WHY ONLY THESE SUBTREES. An empty CARGO_HOME is correct but
    would re-fetch the git dependencies on every run. Copying all of `~/.cargo`
    is also correct and takes 218s here, because 45 of its 48 GB is 324 stale
    `git/checkouts/reverie-*` revisions that cargo never collects -- pure
    metadata cost for content the run does not need, since cargo recreates the
    one checkout it wants from `git/db` locally. Seeding `registry` + `git/db`
    is 10.4s and reproduces the warm behaviour: the reflink is copy-on-write on
    btrfs, so it also costs no real space.

    The private home is mandatory.  Missing shared seed data produces an empty
    private home; an allocation or copy error refuses the run instead of
    silently falling back to the shared package-cache lock.
    """
    private = Path(
        checked_output(
            ["mktemp", "-d", str(parent / "validate-cargo-XXXXXXXX")],
            run=run,
            purpose="cannot create private cargo home",
        )
    )
    if not shared.is_dir():
        return private

    package_lock = shared / ".package-cache"
    try:
        with package_lock.open("a+") as lock:
            # Cargo mutates registry and git/db while holding this lock.  A
            # shared lock lets two snapshot readers coexist while excluding a
            # concurrent Cargo writer, so the reflink is one coherent seed.
            fcntl.flock(lock.fileno(), fcntl.LOCK_SH)
            for relative, destination in CARGO_SEED_SUBTREES:
                source = shared / relative
                if not source.is_dir():
                    continue
                target = private / destination
                target.parent.mkdir(parents=True, exist_ok=True)
                result = run(
                    ["cp", "-a", "--reflink=auto", str(source), str(target)],
                    check=False,
                )
                if getattr(result, "returncode", 1) != 0:
                    raise RuntimeError(f"cannot seed {relative} into {private}")

            # Registry sources and credentials live in config; without it a
            # private home can resolve a different registry than the seed did.
            for name in ("config.toml", "config"):
                candidate = shared / name
                if candidate.is_file():
                    result = run(
                        ["cp", "-a", str(candidate), str(private / name)],
                        check=False,
                    )
                    if getattr(result, "returncode", 1) != 0:
                        raise RuntimeError(f"cannot seed {name} into {private}")
    except Exception:
        remove_private_cargo_home(private, run=run)
        raise
    return private


def remove_private_cargo_home(private: Path | None, *, run: Runner) -> None:
    """Drop the per-run cargo home. Best-effort, never fatal."""
    if private is None:
        return
    run(["rm", "-rf", str(private)], check=False)


def assert_row_readable_from_canonical_ledger(
    root: Path,
    target: str,
    cwd: Path,
    run_started_at: str,
    *,
    repo: str,
    run: Runner,
) -> tuple[str, str, int]:
    """REQUIREMENT (a). Fail unless this run's exact row is readable canonically.

    A validate that writes its receipt into a checkout-local ledger and then
    loses that directory MANUFACTURES INVISIBLE GREENS, and
    would be strictly worse than validating the slot in place. This is not a
    hypothetical failure mode — it is the measured one. The admission audit
    (`ci-hub-admission-control-audit`) reproduced exactly 111 `validate.rs`
    fallback rows sitting in two per-checkout ledgers that default consumers
    discover ZERO of, and a full green at PR #1635 head 291a2fd6 (862 executed,
    0 failed) read as NOT-VALIDATED because its record went to a per-checkout
    shard. A field recorded only where it does not survive the checkout is not
    recorded at all.

    So the row is re-read THROUGH THE CANONICAL READER before the wrapper
    reports a verdict. The binding is by IDENTITY rather than by a correlated
    proxy: the row must carry this exact 40-hex commit AND the `cwd` of this
    exact run checkout AND start no earlier than this handle. A row matching on
    commit and checkout alone could be an older run of the same SHA. A
    VALIDATED verdict additionally has to select that exact row as its newest
    canonical qualifying receipt; an older green cannot bless a new empty run.
    """
    handle_start = parse_utc_timestamp(run_started_at, role="validate-run handle")
    status = run(
        [
            str(root / "ci-hub/ci-hub"),
            "validate-status",
            "--sha",
            target,
            "--repo",
            repo,
            "--json",
        ],
        cwd=root,
        check=False,
    )
    try:
        report = json.loads(status.stdout)
        verdict = str(report["verdict"])
        reported_sha = str(report["sha"])
        reported_exit = int(report["exit_code"])
        qualifying_receipts = report.get("qualifying_receipts")
        receipt_count = int(report["qualifying_count"]) + int(
            report["disqualified_count"]
        )
    except (json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        detail = (
            status.stderr.strip()
            or status.stdout.strip()
            or f"exit {status.returncode}"
        )
        raise RuntimeError(f"canonical validate-status unreadable ({detail}; {error})") from error
    expected_exit = canonical_verdict_exit_code(verdict)
    if (
        expected_exit is None
        or status.returncode != expected_exit
        or reported_exit != expected_exit
        or reported_sha != target
    ):
        detail = status.stderr.strip() or f"exit {status.returncode}, sha {reported_sha}"
        raise RuntimeError(
            "canonical validate-status refused or was internally inconsistent "
            f"({detail}; verdict={verdict}, reported_exit={reported_exit}, "
            f"expected_exit={expected_exit})"
        )
    if receipt_count == 0:
        raise RuntimeError(
            f"canonical validate-status reports no receipt for commit {target}"
        )

    # `validate-status` is the verdict authority. Its JSON intentionally
    # summarizes a SHA, while this producer must additionally prove that the
    # receipt belongs to THIS run rather than an older run of the same SHA.
    # Re-read identity through the exact canonical-union adapter used by
    # validate-status; do not reconstruct a second pass/fail predicate here.
    rows = run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
        cwd=root,
        check=False,
    )
    if rows.returncode != 0:
        detail = rows.stderr.strip() or rows.stdout.strip() or f"exit {rows.returncode}"
        raise RuntimeError(f"canonical ledger union unreadable ({detail})")
    matched: list[tuple[str, Mapping[str, Any]]] = []
    for line in rows.stdout.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(row, dict):
            continue
        if (
            not row_matches_repo(row, repo)
            or row.get("commit") != target
            or row.get("cwd") != str(cwd)
        ):
            continue
        row_start = parse_utc_timestamp(row.get("started_at"), role="canonical ledger row")
        if row_start >= handle_start:
            matched.append((line, row))
    if not matched:
        raise RuntimeError(
            f"no row for commit {target} with cwd {cwd} at or after {run_started_at} "
            "is readable from the canonical ledger; an older row cannot bind this run"
        )
    if verdict == "VALIDATED":
        selected = [
            candidate
            for candidate in matched
            if row_is_selected_qualifying_receipt(candidate[1], qualifying_receipts)
        ]
        if not selected:
            raise RuntimeError(
                "canonical VALIDATED verdict contains no qualifying receipt belonging "
                "to this run; refusing to inherit another run's green"
            )
        matched = selected
    matched.sort(key=lambda candidate: str(candidate[1].get("finished_at") or ""))
    return matched[-1][0], verdict, expected_exit


# Directories a receipt is never written into and that are expensive to walk.
RECEIPT_SCAN_PRUNE = {".git", "target", "node_modules"}


def orphaned_receipt_locations(fresh: Path, *, run: Runner) -> list[str]:
    """Receipt files THIS RUN left inside the temp checkout, if any.

    The discriminator between two states the retention path used to conflate:

      * a receipt was produced but written where no consumer reads it -- the
        exact hazard requirement (a) exists to catch. The temp tree is then the
        ONLY trace the hazard occurred, so reclaiming it destroys the evidence.
        RETAIN.
      * the run failed before producing any receipt at all -- ordinary, and the
        common case, because the Rust validation driver is fail-fast and its first gate can
        abort in seconds. Nothing is preserved by keeping 26 MB of build tree.
        RECLAIM.

    THE TEST IS SHAPE, NOT NAME, and that choice is the safety-critical one. A
    whitelist of known ledger filenames fails in the DANGEROUS direction: a
    receipt written under a name nobody enumerated would read as "no evidence"
    and be deleted -- a disk-hygiene fix eating the evidence path. So this looks
    for any `*.jsonl` in the tree that git does not track. Measured before
    relying on it: hermit tracks ZERO `.jsonl` files, so anything found here was
    produced by this run. The tracked-ness check is kept regardless, so the
    property survives that changing.

    Reads no ledger CONTENT -- existence and tracked-ness are the whole test --
    so this module stays a non-reader under the ledger-reader allowlist.
    """
    candidates: list[str] = []
    for current, dirs, files in os.walk(fresh):
        dirs[:] = [d for d in dirs if d not in RECEIPT_SCAN_PRUNE]
        for name in files:
            if name.endswith(".jsonl"):
                candidates.append(str(Path(current, name).relative_to(fresh)))
    if not candidates:
        return []
    orphaned: list[str] = []
    for relative in candidates:
        candidate = fresh / relative
        owner_result = run(
            ["git", "-C", str(candidate.parent), "rev-parse", "--show-toplevel"],
            check=False,
        )
        if owner_result.returncode != 0:
            orphaned.append(relative)
            continue
        owner = Path(owner_result.stdout.strip())
        try:
            owner.relative_to(fresh)
            owner_relative = candidate.relative_to(owner)
        except ValueError:
            # A repository outside the disposable checkout cannot establish
            # that a file inside it was present at checkout time.
            orphaned.append(relative)
            continue
        tracked = run(
            [
                "git",
                "-C",
                str(owner),
                "ls-files",
                "--error-unmatch",
                "--",
                str(owner_relative),
            ],
            check=False,
        )
        if tracked.returncode != 0:
            orphaned.append(relative)
    return sorted(orphaned)


def owner_validate_symlink_is_exact(checkout: Path, dirty: str) -> bool:
    """The one owner-checkout exception; every property is deliberately fixed."""
    link = checkout / "validate"
    return (
        dirty.splitlines() == ["?? validate"]
        and link.is_symlink()
        and os.readlink(link) == "scripts/validate.rs"
    )


def validate_checkout(
    checkout: Path,
    target: str,
    *,
    repo: str,
    run: Runner,
    allow_owner_validate_symlink: bool = False,
) -> Path:
    checkout = require_guest_visible_root(checkout, role="source checkout")
    if not SHA_RE.fullmatch(target):
        raise ValueError("--target must be an exact lowercase 40-hex commit SHA")
    driver = "scripts/validate.rs" if repo == "rrnewton/hermit" else "validate.sh"
    if not (checkout / driver).is_file():
        raise ValueError(f"missing {driver} in checkout {checkout}")

    top = Path(
        checked_output(
            ["git", "-C", str(checkout), "rev-parse", "--show-toplevel"],
            run=run,
            purpose="cannot resolve checkout root",
        )
    ).resolve()
    if top != checkout:
        raise ValueError(f"--checkout must name the repository root ({top}), not {checkout}")

    head = checked_output(
        ["git", "-C", str(checkout), "rev-parse", "HEAD^{commit}"],
        run=run,
        purpose="cannot resolve checkout HEAD",
    )
    if head != target:
        raise ValueError(f"checkout HEAD is {head}, not requested exact target {target}")

    dirty = checked_output(
        ["git", "-C", str(checkout), "status", "--porcelain=v1"],
        run=run,
        purpose="cannot inspect checkout cleanliness",
    )
    if dirty and not (
        allow_owner_validate_symlink
        and owner_validate_symlink_is_exact(checkout, dirty)
    ):
        first = dirty.splitlines()[0]
        raise ValueError(f"checkout is dirty ({first}); refusing unrepeatable validation")
    return checkout


def preflight(
    root: Path, checkout: Path, target: str, *, repo: str, run: Runner
) -> None:
    if repo == "rrnewton/reverie":
        result = run(
            [
                "git",
                "-C",
                str(checkout),
                "merge-base",
                "--is-ancestor",
                "refs/remotes/origin/main",
                target,
            ],
            check=False,
        )
        if result.returncode == 1:
            raise RuntimeError(
                "validation admission refused target: Reverie head does not contain current origin/main"
            )
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
            raise RuntimeError(f"cannot establish Reverie current-main ancestry: {detail}")
        return
    command = [
        str(root / "ci-hub/validate/preflight_validate.py"),
        "--head",
        target,
        "--repo-checkout",
        str(checkout),
    ]
    result = run(command, cwd=root, check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"validation admission refused target: {detail}")


def build_systemd_command(
    *,
    root: Path,
    checkout: Path,
    target: str,
    agent: str,
    unit: str,
    record: Path,
    log: Path,
    pr: int | None,
    validate_args: Sequence[str],
    wait: int,
    hold: int,
    child_deadline: int,
    environment: Mapping[str, str],
    make_validate: bool = False,
    allow_owner_validate_symlink: bool = False,
    cargo_home: Path | None = None,
    runtime_root: Path | None = None,
    ci_dag_jobs: int | None = None,
    repo: str = "rrnewton/hermit",
    frozen_validate: bool = False,
    frozen_result: Path | None = None,
) -> list[str]:
    home = environment.get("HOME", "")
    path = environment.get("PATH", "")
    if not home or not path:
        raise ValueError("HOME and PATH must be set so cargo/rustup resolve inside the user unit")
    build_budget_env: list[str] = []
    for key in ("CARGO_BUILD_JOBS", "THIRD_PARTY_BUILD_JOBS"):
        value = environment.get(key)
        if value:
            if not value.isdigit() or int(value) <= 0:
                raise ValueError(f"{key} must be a positive integer, got {value!r}")
            build_budget_env.extend(["--setenv", f"{key}={value}"])
    # Scheduler width is a per-run experimental input, not ambient caller
    # state.  Requiring the typed option keeps ordinary validation byte-for-byte
    # on VALIDATE's existing default and makes every non-default run attributable.
    if ci_dag_jobs is not None:
        if ci_dag_jobs <= 0:
            raise ValueError(f"CI_DAG_JOBS must be a positive integer, got {ci_dag_jobs!r}")
        build_budget_env.extend(["--setenv", f"CI_DAG_JOBS={ci_dag_jobs}"])
    if repo == "rrnewton/hermit":
        build_budget_env.extend(
            [
                "--setenv",
                f"HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS={hermit_run_timeout_seconds(child_deadline)}",
            ]
        )
    # Last, so it cannot be shadowed by a caller-supplied CARGO_HOME: the whole
    # point is that this run does not share a package-cache lock with the box.
    if cargo_home is not None:
        build_budget_env.extend(["--setenv", f"CARGO_HOME={cargo_home}"])
    if runtime_root is not None:
        build_budget_env.extend(
            [
                "--setenv",
                f"TMPDIR={runtime_root / 'tmp'}",
                "--setenv",
                f"XDG_CACHE_HOME={runtime_root / 'cache'}",
                "--setenv",
                f"PYTHONPYCACHEPREFIX={runtime_root / 'python-cache'}",
                "--setenv",
                f"HERMIT_DATA_DIR={runtime_root / 'hermit-data'}",
            ]
        )
    if repo == "rrnewton/reverie":
        # This is part of the Reverie producer contract, not ambient caller
        # state.  Without it KVM tests silently skip on a host where /dev/kvm
        # is absent or unusable, and a partial suite can look green.
        build_budget_env.extend(["--setenv", "REVERIE_REQUIRE_KVM=1"])
    else:
        e2e_result_root, runner_log_dir = _validate_output_paths(root, unit)
        build_budget_env.extend(
            [
                "--setenv",
                f"E2E_RESULT_ROOT={e2e_result_root}",
                "--setenv",
                f"E2E_RUN_ID={unit}",
                # BOTH spellings, deliberately. agent-utils renamed the runner's
                # log-dir variable SAFE_CI_DAG_RUNNER_LOG_DIR -> DAGRUN_LOG_DIR,
                # and this parent drives hermit checkouts on BOTH sides of that
                # rename: current main pins the new name, an older commit or an
                # unrebased pull-request head pins the old one, and a bisect
                # walks across the boundary. Sending only one name silently
                # loses the runner logs for every target on the other side --
                # silently, because an unread variable raises nothing and the
                # receipt still prints a RUNNER_LOGS path that stays empty.
                # The unrecognised name is ignored by whichever runner receives
                # it, so setting both is safe in both directions.
                "--setenv",
                f"SAFE_CI_DAG_RUNNER_LOG_DIR={runner_log_dir}",
                "--setenv",
                f"DAGRUN_LOG_DIR={runner_log_dir}",
            ]
        )
    if frozen_validate:
        if repo != "rrnewton/hermit":
            raise ValueError("frozen-validate currently supports only rrnewton/hermit")
        if frozen_result is None:
            raise ValueError("frozen-validate requires a durable non-canonical result path")
        build_budget_env.extend(
            [
                "--setenv",
                "VALIDATE_RUN_ON_DIRTY_TREE=1",
                "--setenv",
                "VALIDATE_IGNORE_CACHE=1",
                "--setenv",
                "VALIDATE_LABEL_PR=0",
                "--setenv",
                f"HERMIT_VALIDATE_LEDGER={frozen_result}",
            ]
        )

    child = ["/usr/bin/env"]
    if pr is not None:
        child.append(f"PR_NUMBER={pr}")
    if allow_owner_validate_symlink:
        # Keep the executed command literally `make validate`. The product
        # driver already has a typed escape hatch whose receipt stays explicitly
        # unanchored; the owner watch interprets that distinct evidence rather
        # than laundering it into a clean canonical receipt.
        child.append("VALIDATE_RUN_ON_DIRTY_TREE=1")
    if repo == "rrnewton/reverie":
        child.extend(
            [
                sys.executable,
                str(root / "ci-hub/validate/reverie_safe_ci.py"),
                "--root",
                str(root),
                "--checkout",
                str(checkout),
                "--target",
                target,
                "--log",
                str(log),
            ]
        )
        if pr is not None:
            child.extend(["--pr", str(pr)])
    else:
        # validate-lock's child deadline begins when this helper starts. Anchor
        # Hermit's monotonic deadline here, BEFORE with-proxy and rust-script can
        # spend part of the enclosing 3600 seconds compiling the driver. Setting
        # the epoch when start_unit builds the command would charge FIFO wait;
        # setting it inside validate.rs would fail to charge its own compilation.
        child.extend(
            [
                sys.executable,
                str(root / "ci-hub/validate/run_with_validate_deadline.py"),
            ]
        )
        if make_validate:
            child.extend(["with-proxy", "make", "validate"])
        else:
            child.extend(
                ["with-proxy", "./scripts/validate.rs", *(validate_args or ["full"])]
            )

    return [
        "systemd-run",
        "--user",
        "--collect",
        "--unit",
        unit,
        "--description",
        f"ci-hub full validation {repo} {target[:12]} ({agent})",
        "--working-directory",
        str(checkout),
        "--setenv",
        f"HOME={home}",
        "--setenv",
        f"PATH={path}",
        "--setenv",
        "CI_HUB_VALIDATE_PRODUCER=systemd-user-v1",
        *build_budget_env,
        # DEV_HERMIT_PARENT remains available to non-ledger helpers. Receipt
        # storage does not use it: validate.rs invokes the fixed parent adapter,
        # which appends the tracked per-machine shard that ci-hub unions.
        "--setenv",
        f"DEV_HERMIT_PARENT={root}",
        "--property",
        f"StandardOutput=append:{log}",
        "--property",
        f"StandardError=append:{log}",
        str(root / "ci-hub/ci-hub"),
        "validate-lock",
        "run",
        "--agent",
        agent,
        "--kind",
        (
            FROZEN_VALIDATE_KIND
            if frozen_validate
            else "reverie-validate" if repo == "rrnewton/reverie"
            else "validate"
        ),
        "--target",
        target,
        "--run-record",
        str(record),
        "--wait",
        str(wait),
        "--hold",
        str(hold),
        "--child-deadline",
        str(child_deadline),
        "--",
        *child,
    ]


def service_properties(
    unit: str, *, run: Runner
) -> dict[str, str] | None:
    result = run(
        [
            "systemctl",
            "--user",
            "show",
            f"{unit}.service",
            "--property=ActiveState",
            "--property=LoadState",
            "--property=SubState",
            "--property=ExecMainCode",
            "--property=ExecMainStatus",
            "--property=Result",
            "--property=InvocationID",
            "--no-pager",
        ],
        check=False,
    )
    if result.returncode != 0:
        return None
    properties = dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )
    # `systemctl show` succeeds even after --collect has unloaded the unit.  Its
    # synthetic not-found object says success/0 and is not execution evidence.
    if properties.get("LoadState") != "loaded" or not properties.get("InvocationID"):
        return None
    return properties


class LogTail:
    """Stream a unit's log to stdout as it is appended.

    The transient unit writes with `StandardOutput=append:<log>`, so without
    this the caller who STARTED a validate cannot watch it -- the output exists
    only in a file they have to go and find. This follows the file from the
    offset it has already shown, so each poll prints only what is new.

    IT NEVER RAISES. A validate must not fail because its own progress
    could not be echoed; an unreadable log simply stops streaming.
    """

    def __init__(self, log: Path, *, out: TextIO | None = None) -> None:
        self.log = log
        self.out = out if out is not None else sys.stdout
        self.offset = 0

    def pump(self) -> None:
        try:
            with self.log.open("rb") as handle:
                handle.seek(self.offset)
                chunk = handle.read()
                self.offset = handle.tell()
        except OSError:
            return
        if chunk:
            self.out.write(chunk.decode("utf-8", "replace"))
            self.out.flush()


def wait_for_unit(
    unit: str,
    record: Path,
    *,
    run: Runner,
    poll_seconds: float,
    sleep: Callable[[float], None] = time.sleep,
    stream: LogTail | None = None,
) -> dict[str, Any]:
    seen = False
    missing = 0
    while True:
        if stream is not None:
            stream.pump()
        properties = service_properties(unit, run=run)
        if properties is None:
            missing += 1
            try:
                durable = run_registry.read_record(record)
            except RuntimeError:
                durable = {}
            if service_result.is_evidenced_terminal(durable):
                return durable
            raw_log = durable.get("log")
            log = Path(raw_log) if isinstance(raw_log, str) else Path("/nonexistent")
            classified = service_result.from_log(log)
            if classified.get("exit_code") is not None or missing >= (5 if seen else 50):
                # DO NOT STAMP AN END TIME ONTO AN ADMISSION OF IGNORANCE.
                #
                # This return fires when `systemctl show` stopped answering. If
                # the log also carries no exit, `classify` yields `unknown` --
                # the writer cannot tell what happened. Stamping `finished_at`
                # there asserted a second thing it did not know: that the run
                # had ENDED. That pairing is what made a live run read as a
                # finished one; on 2026-08-24 a record claiming
                # `finished_at 22:30:32` belonged to a unit that went on
                # executing until 23:2x, and a quiet box was declared on it.
                # Give up waiting, yes -- but give up saying when it stopped.
                if classified.get("state") == "unknown":
                    return classified
                return {
                    **classified,
                    "finished_at": datetime.now(timezone.utc).isoformat(),
                }
            sleep(poll_seconds)
            continue
        seen = True
        if properties.get("ActiveState") in TERMINAL_STATES:
            try:
                durable = run_registry.read_record(record)
            except RuntimeError:
                durable = {}
            raw_log = durable.get("log")
            log = Path(raw_log) if isinstance(raw_log, str) else Path("/nonexistent")
            return {
                **service_result.from_log(log, properties=properties),
                "finished_at": datetime.now(timezone.utc).isoformat(),
            }
        sleep(poll_seconds)


def relay_inner_output(
    log: Path, *, out: TextIO | None = None, limit: int = 65536
) -> bool:
    """Print what the inner run said, verbatim. True if anything was shown.

    ⚠️ THIS RELAYS; IT DOES NOT SUMMARISE. The wrapper used to report
    `CANONICAL-VERDICT-UNAVAILABLE: no receipt for commit` when the inner
    validate had already refused with exit 2, named the gate it hit, given the
    commit distance and printed its bypass flag. Reporting the ABSENCE of a
    receipt where a REASON exists is the same defect this project keeps finding
    elsewhere, committed by our own tooling against its own owner.

    So no parsing, no classification, no reconstruction of the inner message:
    the bytes the run wrote are the answer, and any summary this wrapper
    invented could disagree with them.
    """
    sink = out if out is not None else sys.stderr
    try:
        data = log.read_bytes()
    except OSError:
        return False
    if not data.strip():
        return False
    truncated = len(data) > limit
    if truncated:
        data = data[-limit:]
    print(
        f"validate-run: the run refused. Relaying {log} verbatim"
        + (" (tail)" if truncated else "")
        + ":",
        file=sink,
    )
    print("-" * 72, file=sink)
    sink.write(data.decode("utf-8", "replace"))
    if not data.endswith(b"\n"):
        sink.write("\n")
    print("-" * 72, file=sink)
    sink.flush()
    return True


def retained_path_state(raw: str) -> str:
    """Describe a retained artifact path so a reader can tell the cases apart.

    A receipt that names a path a reader cannot use is worse than one that names
    none, because it reads as complete. Three outcomes mean three different
    things and only the first is ordinary: POPULATED is a normal run, EMPTY says
    the run produced nothing there (the runner never wrote, or its output was
    cleaned), and ABSENT says the directory was never created at all. Before
    this, all three printed identically -- the line was emitted on the
    truthiness of a computed path string, which is always true.

    THIS NEVER RAISES AND NEVER FAILS A RUN. An empty artifact directory is not
    necessarily an error, and turning a reporting gap into a standing red is how
    a newly-enforcing check gets suppressed within a week. It reports the
    distinction and leaves the judgement to the reader.
    """
    try:
        path = Path(raw)
        if not path.exists():
            return "absent"
        if not path.is_dir():
            return "not a directory"
        entries = sum(1 for _ in path.iterdir())
    except OSError as error:
        # Even an unreadable path must not fail the run; name it and move on.
        return f"unreadable: {error.strerror or error}"
    if entries == 0:
        return "empty"
    return f"{entries} entry" if entries == 1 else f"{entries} entries"


def emit_report(report: Mapping[str, Any], *, json_output: bool) -> None:
    if json_output:
        # Additive only: existing keys keep their meaning, and a machine reader
        # gets the same distinction the console line now carries.
        enriched = dict(report)
        for key in ("e2e_result_root", "safe_ci_dag_runner_log_dir"):
            value = enriched.get(key)
            if value:
                enriched[f"{key}_state"] = retained_path_state(str(value))
        print(json.dumps(enriched, sort_keys=True), flush=True)
        return
    event = report.get("event", "state").upper()
    print(
        f"validate-run: {event} {report['unit']} target={report['target']} "
        f"state={report.get('state', 'unknown')}",
        flush=True,
    )
    if report.get("pane_id"):
        print(
            f"PANE workspace={report['workspace_id']} tab={report['tab_id']} "
            f"pane={report['pane_id']}",
            flush=True,
        )
    if report.get("log"):
        print(f"LOG {report['log']}", flush=True)
    if report.get("measured_against_superseded_tip") is True:
        print(
            "MEASURED-AGAINST-SUPERSEDED-TIP "
            f"target={report['target']} "
            f"current_main_before_launch={report.get('current_main_before_launch')} "
            "qualifying_receipt=false",
            flush=True,
        )
    # A PATH THAT WAS NEVER CREATED IS NOT AN ARTIFACT, SO DO NOT OFFER IT.
    # `absent` means the directory does not exist -- for a run that refused
    # before starting, both of these are absent, and printing them sent the
    # owner looking for output that could not be there. `empty` differs and
    # is still printed: the run DID create the directory and wrote nothing,
    # which is a fact worth seeing.
    for key, label in (("e2e_result_root", "E2E_RESULTS"),
                       ("safe_ci_dag_runner_log_dir", "RUNNER_LOGS")):
        raw = report.get(key)
        if not raw:
            continue
        state = retained_path_state(str(raw))
        if state == "absent":
            continue
        print(f"{label} {raw} [{state}]", flush=True)



def bookkeep_record(record_path, /, **fields):
    """Update the run record, but never at the cost of the caller's verdict.

    ⚠️ THIS IS THE PRIORITY INVERSION THAT HELD THE OWNER'S BOX. Both callers run
    AFTER the run's outcome is settled: the first after `wait_for_unit` has
    returned the terminal state, the second after the canonical verdict has been
    read. Neither result depends on the write succeeding -- the record is a
    handle for later readers, not the answer. Yet both took `flock(LOCK_EX)` with
    no bound, so a contended lock stopped the wrapper delivering a verdict it had
    already computed.

    Same shape as the rest of tonight: a value produced and never delivered.

    On contention the fact is REPORTED and the caller continues. The in-memory
    fields are returned so downstream reporting is unaffected, and the record is
    the only thing that goes stale -- which is the correct thing to sacrifice,
    because a reader can re-derive it and the human at the terminal cannot.
    """
    try:
        return run_registry.update_record(record_path, blocking=False, **fields)
    except run_registry.RecordLocked as locked:
        print(
            f"validate-run: RUN-RECORD NOT UPDATED: {locked}. The verdict below is "
            "unaffected -- this is a bookkeeping write, and it is reported rather "
            "than waited on so a held lock cannot withhold a result that is "
            "already known. Remedy: none required for this run; if it persists, "
            f"find the holder with `fuser -v {record_path}.lock` or `lsof`.",
            file=sys.stderr,
        )
        return dict(fields)


def attach(
    raw_unit: str,
    *,
    root: Path,
    run: Runner,
    json_output: bool,
    poll_seconds: float,
    sleep: Callable[[float], None],
) -> int:
    unit = sanitize_unit(raw_unit)
    record_path = run_registry.record_path(root, unit)
    record = run_registry.read_record(record_path)
    emit_report(
        {
            **record,
            "event": "attached",
            "unit": f"{unit}.service",
        },
        json_output=json_output,
    )
    final = wait_for_unit(
        unit,
        record_path,
        run=run,
        poll_seconds=poll_seconds,
        sleep=sleep,
    )
    updated = bookkeep_record(record_path, **final)
    if record.get("validation_kind") == FROZEN_VALIDATE_KIND:
        try:
            target = str(record["target"])
            checkout = Path(str(record["checkout"]))
            run_started_at = str(record["started_at"])
            repo = canonical_repo(str(record.get("repo", "rrnewton/hermit")))
            result_path = Path(str(record["result_record"]))
            current_main = str(record["current_main_before_launch"])
            row, measured_result, measured_exit = mark_and_read_frozen_result(
                result_path,
                target,
                checkout,
                run_started_at,
                current_main,
                repo=repo,
            )
        except (KeyError, RuntimeError, ValueError) as error:
            return could_not_determine(
                f"FROZEN-VALIDATE-RESULT-UNAVAILABLE: {error}",
                f"the unit ran; inspect {record.get('result_record', '<result-record>')} "
                "and the durable log. Do not read this as a current receipt.",
            )
        updated = bookkeep_record(
            record_path,
            measured_result=measured_result,
            wrapper_exit_code=measured_exit,
        )
        if not json_output:
            print(
                "RESULT-RECORDED "
                f"commit={target} cwd={checkout} result={measured_result} "
                f"exit={measured_exit} qualifying_receipt=false"
            )
            print(f"  {row[:160]}")
        emit_report(
            {**updated, "event": "finished", "unit": f"{unit}.service"},
            json_output=json_output,
        )
        return measured_exit
    try:
        target = str(record["target"])
        checkout = Path(str(record["checkout"]))
        run_started_at = str(record["started_at"])
        repo = canonical_repo(str(record.get("repo", "rrnewton/hermit")))
        row, canonical_verdict, canonical_exit = assert_row_readable_from_canonical_ledger(
            root, target, checkout, run_started_at, repo=repo, run=run
        )
    except (KeyError, RuntimeError, ValueError) as error:
        return could_not_determine(
            f"CANONICAL-VERDICT-UNAVAILABLE: {error}",
            "the unit ran; re-read with `ci-hub validate-status --sha "
            f"{record.get('target', '<target>')}` and, if the ledger row is "
            "genuinely absent, re-run the validate. Do NOT read this as a failure.",
        )
    updated = bookkeep_record(
        record_path,
        canonical_verdict=canonical_verdict,
        wrapper_exit_code=canonical_exit,
    )
    if not json_output:
        print(
            f"RECEIPT-CANONICAL commit={target} cwd={checkout} "
            f"verdict={canonical_verdict} exit={canonical_exit}"
        )
        print(f"  {row[:160]}")
        # WHAT RAN, WHERE ITS LOG IS, AND WHICH OF THE THREE OUTCOMES -- without a
        # second command. Previously the caller got a verdict word and an exit
        # number and had to know that 3 means the product failed while 2 meant
        # either "your flag was wrong" or "the result is unreadable".
        if canonical_exit != EXIT_PASSED:
            print(f"  ran:    target={target} checkout={checkout} unit={unit}.service")
            log = updated.get("log") or record.get("log")
            if log:
                print(f"  log:    {log}")
            print(f"  state:  {describe_outcome(canonical_exit)}")
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=json_output,
    )
    return canonical_exit


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=(
            "Launch scripts/validate.rs as a detached systemd user service whose only admission "
            "path is ci-hub validate-lock."
        ),
        epilog=(
            "example, run from the checkout you want validated:\n"
            "  ci-hub validate-run --checkout=\"$PWD\" --agent=human \\\n"
            "                      --target=\"$(git rev-parse HEAD)\" -- full\n"
            "\n"
            "--checkout is the DIRECTORY and --target is the SHA. Two readers guessed\n"
            "the opposite before reading the source, so the pairing is spelled out here.\n"
            "The trailing word after -- is the validate profile (full, only-portable,\n"
            "strict-compat-only, envelope-only); it defaults to full.\n"
            "\n"
            "The run is DETACHED: this returns a validate-* unit handle rather than\n"
            "running in your terminal. Reattach with --attach VALIDATE-UNIT. The result\n"
            "from an ordinary run is appended to ledger/<repo>/<host>/<month>.jsonl ON\n"
            "PARENT MAIN, so read it with `git show origin/main:ledger/...`, not from\n"
            "your working tree. A --frozen-validate result is instead written to the\n"
            "marked non-qualifying result path printed by that run."
        ),
    )
    result.add_argument(
        "--checkout",
        type=Path,
        metavar="DIR",
        help=(
            "DIRECTORY of the checkout to validate, normally \"$PWD\". "
            "This is a path, NOT a commit -- the SHA goes in --target."
        ),
    )
    result.add_argument(
        "--repo",
        default="rrnewton/hermit",
        help="receipt repository: rrnewton/hermit (default) or rrnewton/reverie",
    )
    result.add_argument(
        "--in-place",
        action="store_true",
        help=(
            "OPT-OUT: validate the --checkout working tree itself instead of a fresh "
            "temp-dir checkout of --target. The tree must still be clean and at the "
            "exact target, but its ignored state (build output, caches, materialized "
            "submodules) participates in the run, so the receipt's SHA describes the "
            "commit PLUS that unrecorded local state."
        ),
    )
    result.add_argument(
        "--frozen-validate",
        action="store_true",
        help=(
            "deliberately re-run an exact Hermit SHA that does not contain freshly "
            "fetched origin/main. The run bypasses only the moving-main check, "
            "ignores cached validation, publishes no label, and writes a marked "
            "result outside the canonical receipt ledger"
        ),
    )
    result.add_argument(
        "--agent",
        metavar="NAME",
        help=(
            "who is launching this run; recorded in the ledger row and used "
            "in the generated unit name. Any unit-safe string, e.g. human."
        ),
    )
    result.add_argument(
        "--target",
        metavar="SHA",
        help=(
            "commit SHA to validate, normally \"$(git rev-parse HEAD)\". This is a "
            "commit, NOT a path, and it must be the commit that --checkout DIR "
            "currently has checked out, or admission refuses with "
            "'refusing to validate a stale base'."
        ),
    )
    result.add_argument("--pr", type=int)
    result.add_argument(
        "--attach",
        metavar="VALIDATE-UNIT",
        help="reattach to a durable validate-* handle without launching another run",
    )
    result.add_argument("--unit", help="validate-* unit name; .service suffix is optional")
    result.add_argument("--log", type=Path, help="durable log (default: ignored/validate/<unit>.log)")
    result.add_argument("--wait", type=int, default=7200, help="validate-lock queue wait bound")
    result.add_argument("--hold", type=int, default=1200, help="validate-lock lease seconds")
    result.add_argument(
        "--child-deadline",
        type=int,
        help=(
            "payload wall deadline (default: Hermit 3600s; Reverie 8000s). "
            "Hermit requires more than 61s so its derived run timeout and "
            "cleanup grace both fit inside this bound"
        ),
    )
    result.add_argument(
        "--ci-dag-jobs",
        type=int,
        help=(
            "explicit per-run DAG scheduler width propagated into the detached unit; "
            "when absent, preserve VALIDATE's current default"
        ),
    )
    result.add_argument(
        "--make-validate",
        action="store_true",
        help=(
            "run the checkout's literal `make validate` target inside the admitted "
            "unit instead of invoking scripts/validate.rs directly. No validate "
            "arguments may follow --; this exists for owner-checkout spot checks."
        ),
    )
    result.add_argument(
        "--allow-owner-validate-symlink",
        action="store_true",
        help=(
            "narrow owner-watch exception: permit only an untracked `validate` "
            "symlink whose literal target is scripts/validate.rs"
        ),
    )
    result.add_argument("--dry-run", action="store_true")
    result.add_argument("--json", action="store_true")
    result.add_argument(
        "--caller-poll-seconds",
        type=float,
        default=1.0,
        help="poll cadence while the caller blocks on the detached service",
    )
    result.add_argument(
        "validate_args",
        nargs=argparse.REMAINDER,
        help="scripts/validate.rs arguments after -- (default: full)",
    )
    return result


def main(
    argv: Sequence[str] | None = None,
    *,
    run: Runner = run_command,
    environment: Mapping[str, str] | None = None,
    root: Path = ROOT,
    sleep: Callable[[float], None] = time.sleep,
) -> int:
    args = parser().parse_args(argv)
    try:
        repo = canonical_repo(args.repo)
    except ValueError as error:
        return refuse(str(error), "correct the argument named above and re-run")
    if args.caller_poll_seconds <= 0:
        return refuse(
            "caller poll seconds must be positive",
            "pass --caller-poll-seconds with a value greater than 0, or omit it for the default",
        )
    if args.attach:
        if any(
            (
                args.checkout,
                args.agent,
                args.target,
                args.pr,
                args.unit,
                args.log,
                args.ci_dag_jobs,
                args.frozen_validate,
                args.dry_run,
            )
        ):
            return refuse(
                "--attach cannot be combined with launch arguments",
                "attach to an existing unit with --attach ALONE, or drop --attach to launch a new run",
            )
        try:
            return attach(
                args.attach,
                root=root.resolve(),
                run=run,
                json_output=args.json,
                poll_seconds=args.caller_poll_seconds,
                sleep=sleep,
            )
        except (RuntimeError, ValueError) as error:
            return refuse(
                str(error),
                "list live units with `ci-hub validate-worktrees --runs 10` and attach to one of those",
            )
    if args.checkout is None or not args.agent or not args.target:
        return refuse(
            "launch requires --checkout, --agent, and --target",
            "re-run as `ci-hub validate-run --checkout WORKTREE --agent AGENT --target SHA -- full`",
        )
    validate_args = list(args.validate_args)
    if validate_args[:1] == ["--"]:
        validate_args.pop(0)
    if args.frozen_validate and repo != "rrnewton/hermit":
        return refuse(
            "--frozen-validate currently supports only rrnewton/hermit",
            "use ordinary validate-run for Reverie",
        )
    if args.frozen_validate and args.in_place:
        return refuse(
            "--frozen-validate cannot use --in-place",
            "drop --in-place so the deliberately old target runs in a fresh isolated checkout",
        )
    if args.frozen_validate and "--label-pr" in validate_args:
        return refuse(
            "--frozen-validate cannot publish a receipt-backed label",
            "drop --label-pr; this run measures a target that does not contain current main",
        )
    if args.make_validate and validate_args:
        return refuse(
            "--make-validate is the exact no-argument Make target; validate arguments "
            "were passed after --",
            "drop the arguments after -- to run the Make target, or drop --make-validate "
            "to pass driver arguments",
        )
    if repo == "rrnewton/hermit" and any(
        value == "--run-timeout" or value.startswith("--run-timeout=")
        for value in validate_args
    ):
        return refuse(
            "--run-timeout is owned by validate-run and derived from --child-deadline",
            "drop --run-timeout; change --child-deadline if the enclosing bound must change",
        )
    if repo == "rrnewton/reverie" and (args.make_validate or validate_args):
        return refuse(
            "Reverie always runs its exact full validate.sh under safe-ci-dag-runner; "
            "--make-validate and trailing driver arguments are unsupported",
            "re-run against the Reverie checkout with no --make-validate and no trailing "
            "driver arguments",
        )
    if args.allow_owner_validate_symlink and not (args.in_place and args.make_validate):
        return refuse(
            "--allow-owner-validate-symlink requires --in-place and --make-validate",
            "add --in-place and --make-validate, or drop --allow-owner-validate-symlink",
        )
    if args.pr is not None and args.pr <= 0:
        return refuse(
            "--pr must be positive",
            "pass the pull-request NUMBER, e.g. --pr 2614, or omit --pr entirely",
        )
    child_deadline = args.child_deadline or (
        8000 if repo == "rrnewton/reverie" else 3600
    )
    if min(args.wait, args.hold, child_deadline) <= 0:
        return refuse(
            "wait/hold/child-deadline must be positive",
            "give --wait, --hold and --child-deadline values greater than 0, or omit them for defaults",
        )
    try:
        hermit_run_timeout = (
            hermit_run_timeout_seconds(child_deadline)
            if repo == "rrnewton/hermit"
            else None
        )
    except ValueError as error:
        return refuse(str(error), "raise --child-deadline or use the default")
    if args.ci_dag_jobs is not None and args.ci_dag_jobs <= 0:
        return refuse(
            "--ci-dag-jobs must be positive",
            "pass --ci-dag-jobs with a value greater than 0, or omit it to let the runner choose",
        )

    fresh_checkout: Path | None = None
    source_checkout: Path | None = None
    private_cargo_home: Path | None = None
    runtime_root: Path | None = None
    current_main_before_launch: str | None = None
    frozen_result: Path | None = None
    try:
        source_checkout = validate_checkout(
            args.checkout,
            args.target,
            repo=repo,
            run=run,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
        )
        unit = sanitize_unit(args.unit) if args.unit else default_unit(args.agent, args.target)
        if args.frozen_validate:
            current_main_before_launch = current_main_for_frozen_validate(
                source_checkout, args.target, run=run
            )
            frozen_result = frozen_result_path(root.resolve(), unit)
            if frozen_result.exists():
                raise ValueError(
                    f"frozen validation result already exists at {frozen_result}; "
                    "choose a different --unit so two measurements cannot share one record"
                )
        if args.in_place:
            checkout = source_checkout
        else:
            # DEFAULT: validate the COMMIT, not the tree that claims to be at it.
            fresh_checkout = prepare_fresh_checkout(
                source_checkout,
                args.target,
                run=run,
                parent=(
                    frozen_checkout_parent(root.resolve())
                    if args.frozen_validate
                    else (root / "ignored").resolve()
                ),
                repo=repo,
                independent_refs=args.frozen_validate,
            )
            checkout = fresh_checkout
        preflight(root, checkout, args.target, repo=repo, run=run)
        runtime_root = prepare_runtime_root((root / "ignored").resolve(), unit)
        private_cargo_home = prepare_private_cargo_home(
            (root / "ignored").resolve(),
            run=run,
            shared=Path((environment or os.environ).get("HOME", "")) / ".cargo",
        )
        log = (args.log or root / "ignored/validate" / f"{unit}.log").resolve()
        record_path = run_registry.record_path(root.resolve(), unit)
        e2e_result_root, runner_log_dir = _validate_output_paths(root.resolve(), unit)
        output_fields = {"validate_lock_child_deadline_seconds": child_deadline}
        if repo == "rrnewton/hermit":
            output_fields.update(
                {
                    "e2e_result_root": str(e2e_result_root),
                    "safe_ci_dag_runner_log_dir": str(runner_log_dir),
                    "hermit_run_timeout_seconds": hermit_run_timeout,
                }
            )
        started_at = datetime.now(timezone.utc).isoformat()
        command = build_systemd_command(
            root=root.resolve(),
            checkout=checkout,
            target=args.target,
            agent=args.agent,
            unit=unit,
            record=record_path,
            log=log,
            pr=args.pr,
            validate_args=validate_args,
            wait=args.wait,
            hold=args.hold,
            child_deadline=child_deadline,
            environment=environment or os.environ,
            make_validate=args.make_validate,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
            cargo_home=private_cargo_home,
            runtime_root=runtime_root,
            ci_dag_jobs=args.ci_dag_jobs,
            repo=repo,
            frozen_validate=args.frozen_validate,
            frozen_result=frozen_result,
        )
        if args.dry_run:
            pane = None
        else:
            log.parent.mkdir(parents=True, exist_ok=True)
            if frozen_result is not None:
                frozen_result.parent.mkdir(parents=True, exist_ok=True)
            frozen_fields = (
                {
                    "admission": FROZEN_RESULT_ADMISSION,
                    "validation_kind": FROZEN_VALIDATE_KIND,
                    "result_record": str(frozen_result),
                    "measured_against_superseded_tip": True,
                    "current_main_before_launch": current_main_before_launch,
                    "target_contains_current_main_before_launch": False,
                    "qualifying_receipt": False,
                }
                if args.frozen_validate
                else {}
            )
            run_registry.create_record(
                record_path,
                {
                    "schema_version": 1,
                    "state": "preparing",
                    "unit": f"{unit}.service",
                    "target": args.target,
                    "repo": repo,
                    "checkout": str(checkout),
                    "log": str(log),
                    "agent": args.agent,
                    "pr": args.pr,
                    "started_at": started_at,
                    "producer": "systemd-user-v1",
                    "admission": "ci-hub validate-lock",
                    "pane_role": "observer-only",
                    **output_fields,
                    **frozen_fields,
                    **(
                        {"dag_jobs": args.ci_dag_jobs}
                        if args.ci_dag_jobs is not None
                        else {}
                    ),
                },
            )
            # The pane is OBSERVER-ONLY: it watches the durable log and the
            # systemd unit and never runs the driver. So a full workspace must
            # cost us the window, not the validation -- this run already holds
            # a validate-lock slot, and killing it to protect a
            # cosmetic cap would be the worse trade. Refuse the pane loudly,
            # keep the work.
            try:
                pane = pane_owner.create_pane(
                    root=root.resolve(),
                    checkout=checkout,
                    unit=unit,
                    target=args.target,
                    log=log,
                    record=record_path,
                    pr=args.pr,
                    started_at=started_at,
                    run=run,
                    environment=environment or os.environ,
                    sleep=sleep,
                )
            except pane_lifecycle.WorkspaceFull as exc:
                pane = None
                print(f"PANE-REFUSED {exc}", file=sys.stderr)
                print(
                    "PANE-REFUSED continuing WITHOUT an observer pane: the pane only watches "
                    "the durable log, so the validation itself is unaffected.",
                    file=sys.stderr,
                )
            run_registry.update_record(
                record_path,
                state="launching",
                workspace_id=pane.workspace_id if pane else None,
                tab_id=pane.tab_id if pane else None,
                pane_id=pane.pane_id if pane else None,
                pane_title=pane.title if pane else None,
            )
            run_registry.reserve_log(log)
            result = run(command, cwd=root, check=False)
            if result.returncode != 0:
                detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
                run_registry.update_record(
                    record_path,
                    state="refused",
                    result="systemd-launch-refused",
                    detail=detail,
                    finished_at=datetime.now(timezone.utc).isoformat(),
                )
                raise RuntimeError(f"systemd-run refused service: {detail}")
            run_registry.update_record(record_path, state="running")
    except (RuntimeError, ValueError) as error:
        # Nothing was admitted, so the temp checkout holds no evidence and is
        # removed. Failures AFTER launch are handled below and RETAIN it.
        if fresh_checkout is not None and source_checkout is not None:
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
        remove_private_cargo_home(private_cargo_home, run=run)
        remove_runtime_root(runtime_root, run=run)
        return refuse(
            str(error),
            "the unit did not start; check `ci-hub validate-lock status` for a held box "
            "lock and retry once it is free",
        )

    report = {

        "schema_version": 1,
        "event": "would-start" if args.dry_run else "handle",
        "state": "planned" if args.dry_run else "running",
        "unit": f"{unit}.service",
        "target": args.target,
        "repo": repo,
        "checkout": str(checkout),
        "log": str(log),
        "record": str(record_path),
        "admission": "ci-hub validate-lock",
        "producer": "systemd-user-v1",
        "pane_role": "observer-only",
        **output_fields,
        **(
            {
                "admission": FROZEN_RESULT_ADMISSION,
                "validation_kind": FROZEN_VALIDATE_KIND,
                "result_record": str(frozen_result),
                "measured_against_superseded_tip": True,
                "current_main_before_launch": current_main_before_launch,
                "target_contains_current_main_before_launch": False,
                "qualifying_receipt": False,
            }
            if args.frozen_validate
            else {}
        ),
        "workspace_id": pane.workspace_id if pane else pane_owner.WORKSPACE_LABEL,
        "tab_id": pane.tab_id if pane else None,
        "pane_id": pane.pane_id if pane else None,
        "command": command if args.dry_run else None,
    }
    if args.ci_dag_jobs is not None:
        report["dag_jobs"] = args.ci_dag_jobs
    emit_report(report, json_output=args.json)
    if args.dry_run:
        if not args.json:
            print(f"PANE-PLAN workspace={pane_owner.WORKSPACE_LABEL} role=observer-only")
            print(f"COMMAND {shlex.join(command)}")
        if fresh_checkout is not None and source_checkout is not None:
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
        remove_private_cargo_home(private_cargo_home, run=run)
        remove_runtime_root(runtime_root, run=run)
        return 0

    try:
        # TEE THE RUN. The caller started this validate; they should be able to
        # watch it without going to find the log. Suppressed under --json so a
        # machine reader still gets one parseable object on stdout.
        final = wait_for_unit(
            unit,
            record_path,
            run=run,
            poll_seconds=args.caller_poll_seconds,
            sleep=sleep,
            stream=None if args.json else LogTail(log),
        )
        updated = run_registry.update_record(record_path, **final)
    except RuntimeError as error:
        # The run OUTLIVES this waiter by design, so its temp checkout must too:
        # removing it here would delete the tree out from under a live validate.
        if fresh_checkout is not None:
            print(
                f"validate-run: temp checkout RETAINED at {fresh_checkout} (run continues)",
                file=sys.stderr,
            )
        # The wait was interrupted, NOT the run. Returning REFUSED here said "it
        # never started" about a validate that is still executing.
        return could_not_determine(
            f"WAIT-INTERRUPTED {unit}.service; RUN CONTINUES independently: {error}",
            f"the run is still going; re-attach with `ci-hub validate-run --attach {unit}` "
            "to resume watching it, or read its log directly",
        )

    if args.frozen_validate:
        try:
            if frozen_result is None or current_main_before_launch is None:
                raise RuntimeError("frozen validation metadata was not established before launch")
            row, measured_result, measured_exit = mark_and_read_frozen_result(
                frozen_result,
                args.target,
                checkout,
                started_at,
                current_main_before_launch,
                repo=repo,
            )
        except (RuntimeError, ValueError) as error:
            inner_exit = final.get("exit_code") if isinstance(final, dict) else None
            if (
                isinstance(inner_exit, int)
                and inner_exit != 0
                and relay_inner_output(log)
            ):
                print(
                    f"validate-run: the inner validate exited {inner_exit}; "
                    "that refusal above is the reason. No frozen-validation "
                    "result was written because the run did not get far enough "
                    "to write one.",
                    file=sys.stderr,
                )
                if fresh_checkout is not None and source_checkout is not None:
                    remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
                remove_private_cargo_home(private_cargo_home, run=run)
                remove_runtime_root(runtime_root, run=run)
                return inner_exit
            if fresh_checkout is not None:
                print(
                    f"validate-run: fresh checkout RETAINED at {fresh_checkout} "
                    "because the frozen-validation result is unreadable",
                    file=sys.stderr,
                )
            remove_private_cargo_home(private_cargo_home, run=run)
            remove_runtime_root(runtime_root, run=run)
            return could_not_determine(
                f"FROZEN-VALIDATE-RESULT-UNAVAILABLE: {error}",
                f"inspect {frozen_result} and {log}; the run's outcome cannot be "
                "read as either a current receipt or a product verdict",
            )
        updated = bookkeep_record(
            record_path,
            measured_result=measured_result,
            wrapper_exit_code=measured_exit,
        )
        if not args.json:
            print(
                "RESULT-RECORDED "
                f"commit={args.target} cwd={checkout} result={measured_result} "
                f"exit={measured_exit} qualifying_receipt=false"
            )
            print(f"  {row[:160]}")
        if fresh_checkout is not None and source_checkout is not None:
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
        remove_private_cargo_home(private_cargo_home, run=run)
        remove_runtime_root(runtime_root, run=run)
        emit_report(
            {**updated, "event": "finished", "unit": f"{unit}.service"},
            json_output=args.json,
        )
        return measured_exit

    # REQUIREMENT (a), and the ORDER IS THE WHOLE POINT: re-read this run's exact
    # row from the canonical ledger BEFORE the temp directory is removed. Deleting
    # first and checking afterwards would still find the row when it landed
    # canonically, and would find nothing to say when it did not — which is the
    # invisible-green failure this gate exists to make impossible.
    try:
        row, canonical_verdict, canonical_exit = assert_row_readable_from_canonical_ledger(
            root.resolve(), args.target, checkout, started_at, repo=repo, run=run
        )
    except RuntimeError as error:
        # THE INNER RUN'S REASON OUTRANKS THE MISSING ROW. If validate refused,
        # it said why -- named the gate, gave the distance, printed the bypass
        # flag -- and that refusal is the answer. Reporting "no receipt"
        # instead
        # describes a CONSEQUENCE of the refusal as though it were the cause,
        # which is how a clear message became an opaque one on the way out.
        inner_exit = (
            final.get("exit_code") if isinstance(final, dict) else None
        )
        if (
            isinstance(inner_exit, int)
            and inner_exit != 0
            and relay_inner_output(log)
        ):
            print(
                f"validate-run: the inner validate exited {inner_exit}; "
                "that refusal above is the reason. No canonical receipt "
                "was written because the run did not get far enough to "
                "write one.",
                file=sys.stderr,
            )
            if fresh_checkout is not None and source_checkout is not None:
                remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
            remove_private_cargo_home(private_cargo_home, run=run)
            remove_runtime_root(runtime_root, run=run)
            return inner_exit
        print(f"validate-run: CANONICAL-VERDICT-UNAVAILABLE: {error}", file=sys.stderr)
        if fresh_checkout is not None and source_checkout is not None:
            # Retention is for EVIDENCE, not for every failure. Retaining
            # unconditionally cost 26 MB per failed validate, and because
            # The Rust validation driver is fail-fast; that is most of them.
            try:
                orphans = orphaned_receipt_locations(fresh_checkout, run=run)
            except OSError as scan_error:
                # Could not look. A NO-RESULT must never authorize deletion.
                orphans = [f"<scan failed: {scan_error}>"]
            if orphans:
                # The run HAPPENED and produced evidence; it landed somewhere no
                # reader looks. That is unreadable, not unstarted and not failed.
                return could_not_determine(
                    "ORPHANED-RECEIPT: this run wrote a receipt into "
                    f"{', '.join(orphans)} inside the temp checkout, where no "
                    f"consumer reads it. Temp checkout RETAINED at {fresh_checkout} "
                    "-- it is the only trace that this happened.",
                    f"recover the receipt from {fresh_checkout} before anything reclaims "
                    "it; the run's result exists but no consumer can see it",
                )
            print(
                "validate-run: NO-RECEIPT-PRODUCED: the run stopped before writing a "
                "receipt anywhere, so the temp checkout holds no evidence; "
                "reclaiming it.",
                file=sys.stderr,
            )
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
        remove_private_cargo_home(private_cargo_home, run=run)
        remove_runtime_root(runtime_root, run=run)
        # ⚠️ NOT "failed". No receipt means no verdict was ever recorded, so there
        # is nothing to investigate as a product failure -- the previous wording
        # said "the run failed" and the previous code said REFUSED, and neither
        # was true.
        return could_not_determine(
            "the unit terminated without recording a verdict",
            "re-run the validate; if it stops without a receipt again, the failure is "
            "in the harness rather than in the product under test",
        )
    updated = run_registry.update_record(
        record_path,
        canonical_verdict=canonical_verdict,
        wrapper_exit_code=canonical_exit,
    )
    if not args.json:
        print(
            f"RECEIPT-CANONICAL commit={args.target} cwd={checkout} "
            f"verdict={canonical_verdict} exit={canonical_exit}"
        )
        print(f"  {row[:160]}")
    if fresh_checkout is not None and source_checkout is not None:
        remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
    # The normal completion path, and the one that matters most: without this the
    # per-run cargo home leaks on EVERY successful validate.
    remove_private_cargo_home(private_cargo_home, run=run)
    remove_runtime_root(runtime_root, run=run)
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=args.json,
    )
    return canonical_exit


if __name__ == "__main__":
    raise SystemExit(main())
