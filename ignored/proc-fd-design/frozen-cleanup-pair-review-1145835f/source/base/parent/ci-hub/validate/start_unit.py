#!/usr/bin/env python3
"""Launch one detached, admitted Hermit or Reverie validation through ci-hub."""

from __future__ import annotations

import argparse
import filecmp
import fcntl
import hashlib
import json
import math
import os
import re
import secrets
import shlex
import shutil
import stat
import subprocess
import tempfile
import sys
import time
from contextlib import contextmanager, suppress
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any, TextIO

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import git_env  # noqa: E402
import pane_owner
import run_registry
import tree_disposition
import service_result
from immutable_tool_authority import (
    FD_TOOL_ROOT_RE,
    TOOL_AGENT_UTILS_SHA_ENV,
    TOOL_AUTHORITY_ENV,
    TOOL_AUTHORITY_FIELDS,
    TOOL_AUTHORITY_SCHEMA,
    TOOL_BOOTSTRAP_SHA256_ENV,
    TOOL_CONTENT_SHA256_ENV,
    TOOL_HERMIT_SHA_ENV,
    TOOL_PARENT_SHA_ENV,
    ImmutableToolAuthority,
    authority_int,
    authority_string,
    digest_tool_entry,
    immutable_tool_content_sha256,
    proc_fd_identity,
    read_immutable_tool_authority,
    update_digest_length,
)


ROOT = Path(__file__).resolve().parents[2]
HOST_TMP_ROOT = Path("/tmp").resolve()
# Linux permits 107 pathname bytes in an AF_UNIX address. Reverie's longest
# current temporary socket fixture appends 77 bytes including its separator.
MAX_RUNTIME_ROOT_BYTES = 30
VALIDATE_CLEANUP_BATCH_LIMIT = 8


def parent_checkout_head(tool_root: Path = ROOT) -> str | None:
    """The commit of the checkout THIS TOOLING CAME FROM, for the run record.

    WHY A RUN MUST RECORD THIS. Validation tooling -- this file, `validate.rs`,
    everything under `ci-hub/` -- can execute from an immutable retained tool
    root while mutable records live in the shared parent. Recording only the
    subject SHA cannot identify which tooling produced the result.

    A long run therefore CAN span a parent update and execute two different
    versions of its own tooling. Recording `target` alone -- the subject being
    validated -- cannot show that: every field of two such runs is identical, so
    a split leaves NO TRACE and answering "did it happen" costs an investigation.
    It cost exactly that tonight, when `ci-hub/validate/start_unit.py` was stale
    and a headline number was briefly in doubt.

    This field makes that otherwise invisible split explicit. It is evidence,
    not a lock.

    None, never a guess, when the commit cannot be established -- an unknown
    origin must not read as a matching one.
    """

    try:
        completed = subprocess.run(
            ["git", "--no-optional-locks", "-C", str(tool_root), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
            env=git_env.sanitized_git_env(),
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    head = completed.stdout.strip()
    return head if SHA_RE.match(head) else None


def source_checkout_branch(
    checkout: Path,
    target: str,
    *,
    run: Runner,
    require_target_at_head: bool = True,
) -> str | None:
    """Return the branch attached to the admitted source checkout, if any.

    This is provenance captured before the private detached validation checkout
    exists.  A detached source has no branch: do not infer one from refs that
    happen to contain the target, because that relationship can change later and
    can be ambiguous even at admission time.
    """

    result = run(
        [
            "git",
            "-C",
            str(checkout),
            "symbolic-ref",
            "--quiet",
            "--short",
            "HEAD",
        ],
        check=False,
    )
    if result.returncode == 1:
        return None
    if result.returncode != 0:
        detail = (
            result.stderr.strip()
            or result.stdout.strip()
            or f"exit {result.returncode}"
        )
        raise ValueError(f"cannot inspect source checkout branch: {detail}")
    branch = result.stdout.strip()
    if not branch or "\n" in branch or "\r" in branch:
        raise ValueError("source checkout symbolic branch is empty or malformed")
    branch_head = checked_output(
        ["git", "-C", str(checkout), "rev-parse", "HEAD^{commit}"],
        run=run,
        purpose="cannot recheck source checkout HEAD after reading its branch",
    )
    if branch_head != target:
        if not require_target_at_head:
            # A repository used only as an immutable object source does not
            # contribute branch provenance for the separately materialized
            # target. Recording its unrelated current branch would falsely
            # bind that branch to a commit it does not contain at HEAD.
            return None
        raise ValueError(
            f"source checkout branch {branch!r} moved to {branch_head}, "
            f"not requested exact target {target}"
        )
    return branch


def canonical_worktree_tool_head(tool_root: Path, state_root: Path) -> str:
    """Bind a pathname tool root to the canonical parent Git repository.

    Descriptor-backed launchers use the sealed authority below and never ask
    Git to identify executable bytes.  This is the compatibility path for a
    normal linked worktree: both roots must be repository tops sharing one Git
    common directory.  An arbitrary directory is never promoted to executable
    authority merely because it happens to contain a ``ci-hub`` subtree.
    """

    environment = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    environment.update(
        {
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
        }
    )

    def git(root: Path, *arguments: str) -> str:
        try:
            completed = subprocess.run(
                [
                    "git",
                    "--no-optional-locks",
                    "--no-replace-objects",
                    "-C",
                    str(root),
                    *arguments,
                ],
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
                env=environment,
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise ValueError(f"cannot inspect tool worktree {root}: {error}") from error
        if completed.returncode != 0:
            detail = completed.stderr.strip() or f"exit {completed.returncode}"
            raise ValueError(f"{root} is not a readable Git worktree: {detail}")
        return completed.stdout.strip()

    tool_top = Path(git(tool_root, "rev-parse", "--show-toplevel")).resolve()
    state_top = Path(git(state_root, "rev-parse", "--show-toplevel")).resolve()
    if tool_top != tool_root.resolve() or state_top != state_root.resolve():
        raise ValueError("tool and state roots must each name a Git worktree top level")
    tool_common = Path(
        git(tool_root, "rev-parse", "--path-format=absolute", "--git-common-dir")
    ).resolve()
    state_common = Path(
        git(state_root, "rev-parse", "--path-format=absolute", "--git-common-dir")
    ).resolve()
    if tool_common != state_common:
        raise ValueError(
            "DEV_HERMIT_TOOL_ROOT is not a worktree of canonical DEV_HERMIT_PARENT"
        )
    head = git(tool_root, "rev-parse", "--verify", "HEAD^{commit}")
    if SHA_RE.fullmatch(head) is None:
        raise ValueError("tool worktree HEAD is not one exact lowercase 40-hex SHA")
    return head


SHA_RE = re.compile(r"^[0-9a-f]{40}$")
UNIT_RE = re.compile(r"^validate-[A-Za-z0-9_.@:-]+$")
PINNED_BOOTSTRAP_EXEC = r"""
import hashlib
import os
import stat
import sys

path = sys.argv[1]
expected = sys.argv[2]
fd = -1
try:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    metadata = os.fstat(fd)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_mode & 0o222
        or not metadata.st_mode & 0o111
    ):
        raise RuntimeError("installed operational-tool is not one read-only executable")
    with os.fdopen(os.dup(fd), "rb") as source:
        observed = hashlib.sha256(source.read()).hexdigest()
    if observed != expected:
        raise RuntimeError("installed operational-tool digest changed before unit exec")
    os.set_inheritable(fd, True)
    os.execv("/bin/bash", ["/bin/bash", f"/proc/self/fd/{fd}", *sys.argv[3:]])
except (OSError, RuntimeError) as error:
    print(f"validate-run: pinned operational-tool exec refused: {error}", file=sys.stderr)
    raise SystemExit(126) from error
finally:
    if fd >= 0:
        os.close(fd)
"""
Runner = Callable[..., subprocess.CompletedProcess[str]]


@dataclass(frozen=True)
class WrkslotsIdentity:
    slot: str
    generation: int


class CleanupPresence(Enum):
    RETAINED = "retained"
    ABSENT = "absent"
    COULD_NOT_DETERMINE = "could-not-determine"


@dataclass(frozen=True)
class CleanupPresenceObservation:
    state: CleanupPresence
    detail: str


@dataclass(frozen=True)
class CreateJournalCheckout:
    """One provider-verified checkout from an interrupted slot creation."""

    name: str
    path: Path
    branch: str
    head: str


@dataclass(frozen=True)
class CreateJournalScope:
    """Read-only provider evidence for one incomplete agent-slot creation."""

    state: str
    slot: str
    agent: str
    slot_path: Path
    checkouts: tuple[CreateJournalCheckout, ...]
    dirty_checkouts: frozenset[str]


@dataclass
class CleanupRow:
    path: Path
    record: dict[str, Any]
    unit: str | None = None
    archived: list[str] = field(default_factory=list)
    scorecard_handoff: str | None = None


@dataclass
class CleanupGroup:
    checkout: Path
    managed: bool
    rows: list[CleanupRow] = field(default_factory=list)
    reasons: list[str] = field(default_factory=list)
    identity: WrkslotsIdentity | None = None


class CleanupUnitState(Enum):
    """Typed service state used to decide whether retained work blocks entry."""

    TERMINAL = "terminal"
    RUNNING = "running"
    INDETERMINATE = "indeterminate"


@dataclass(frozen=True)
class CleanupUnitObservation:
    unit: str | None
    state: CleanupUnitState
    reason: str | None

    @property
    def blocks_entry(self) -> bool:
        # This is the fail-closed default for observations that have not yet
        # been bound to a positively classified cleanup group.  Only a live
        # wrkslots-managed validation is known to be legitimate concurrent
        # work; callers may exempt that case after establishing the group.
        return self.state is not CleanupUnitState.TERMINAL


@dataclass(frozen=True)
class CleanupHold:
    reason: str
    blocks_entry: bool


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
INNER_FRESHNESS_SKIP_ENV = (
    "VALIDATE_SKIP_INNER_DIRTY_WORKING_TREE_AND_REBASE_FRESHNESS_CHECKS"
)
CURRENT_MAIN_FETCH_TIMEOUT_SECONDS = 30
HERMIT_MIN_CLEANUP_GRACE_SECONDS = 60
# A wider validate-lock deadline may reserve more time for teardown or other
# outer work, but it must not turn Hermit's cumulative validation work back into
# an arbitrarily long run.  This ceiling is above the observed 23-36 minute
# healthy range while making a multi-hour validate impossible.
HERMIT_MAX_RUN_TIMEOUT_SECONDS = 4_200
HERMIT_MAX_CHILD_DEADLINE_SECONDS = 4_800

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
SCORECARD_PATHS = run_registry.SCORECARD_WRITEBACK_PATHS
SCORECARD_HANDOFF_SCHEMA_VERSION = 1
# Cap on the scorecard tool's stderr carried into a recorded writeback failure.
# Ample for any single refusal sentence; short enough that the 45-line usage
# block a few argument errors append does not become the durable record.
SCORECARD_REASON_LIMIT = 2000
SCORECARD_INVOCATION_LOCK_RELATIVE = Path("target/validation/validate-invocation.lock")


class ScorecardInvocationLockRefused(RuntimeError):
    """The source checkout could not be made safe for scorecard write-back."""


class ServiceAcceptance(Enum):
    """Whether systemd has accepted responsibility for the validation unit."""

    PRE_ACCEPT = "pre-accept"
    POST_ACCEPT = "post-accept"


def detached_tool_prefix(
    tool_root: Path,
    state_root: Path,
    environment: Mapping[str, str],
    *,
    authority: ImmutableToolAuthority | None = None,
) -> list[str] | None:
    """Return a unit-owned exact-tool launcher for an fd-backed caller.

    A `/proc/<holder>/fd/<n>` tool root is valid only while its holder process
    remains alive.  `validate-run` deliberately permits an accepted systemd
    service to outlive this waiter, so forwarding that pathname would lend the
    unit a lifetime it does not own.  Re-enter through the installed immutable
    bootstrap instead: the unit opens the already-verified cached parent SHA,
    creates its own private root, and keeps that root until validate-lock exits.

    Ordinary checkout-backed callers keep the direct path.  An fd-backed caller
    with incomplete handoff metadata is refused rather than launching a service
    whose executable authority can disappear after acceptance.
    """

    if FD_TOOL_ROOT_RE.fullmatch(str(tool_root)) is None:
        return None

    authority = authority or read_immutable_tool_authority(
        tool_root, state_root, environment
    )

    home = environment.get("HOME", "")
    raw_bootstrap = environment.get("DEV_HERMIT_OPERATIONAL_TOOL", "")
    bootstrap_sha256 = authority.bootstrap_sha256
    raw_cache = environment.get("DEV_HERMIT_TOOL_CACHE", "")
    parent_sha = authority.parent_sha
    if (
        not home
        or not raw_bootstrap
        or not raw_cache
        or not SHA_RE.fullmatch(parent_sha)
    ):
        raise ValueError(
            "fd-backed DEV_HERMIT_TOOL_ROOT lacks the installed-bootstrap, cache, "
            "or exact-parent handoff metadata"
        )
    if re.fullmatch(r"[0-9a-f]{64}", bootstrap_sha256) is None:
        raise ValueError(
            "fd-backed DEV_HERMIT_TOOL_ROOT lacks an exact bootstrap digest"
        )

    bootstrap = Path(raw_bootstrap)
    expected_bootstrap = Path(home) / ".local/libexec/dev-hermit/operational-tool"
    if bootstrap != expected_bootstrap or not bootstrap.is_absolute():
        raise ValueError(
            "DEV_HERMIT_OPERATIONAL_TOOL must name the installed operational-tool "
            f"at {expected_bootstrap}"
        )
    try:
        bootstrap_metadata = bootstrap.lstat()
        bootstrap_resolved = bootstrap.resolve(strict=True)
    except OSError as error:
        raise ValueError(
            f"installed operational-tool is unavailable: {error}"
        ) from error
    if (
        bootstrap_resolved != bootstrap
        or not stat.S_ISREG(bootstrap_metadata.st_mode)
        or bootstrap_metadata.st_nlink != 1
        or bootstrap_metadata.st_mode & 0o222
        or not os.access(bootstrap, os.X_OK)
    ):
        raise ValueError(
            "installed operational-tool must be one non-symlinked, read-only executable"
        )
    try:
        observed_bootstrap_sha256 = hashlib.sha256(bootstrap.read_bytes()).hexdigest()
    except OSError as error:
        raise ValueError(
            f"installed operational-tool is unreadable: {error}"
        ) from error
    if observed_bootstrap_sha256 != bootstrap_sha256:
        raise ValueError(
            "installed operational-tool does not match DEV_HERMIT_TOOL_BOOTSTRAP_SHA256"
        )

    cache_root = Path(raw_cache)
    try:
        cache_resolved = cache_root.resolve(strict=True)
    except OSError as error:
        raise ValueError(f"operational tool cache is unavailable: {error}") from error
    if (
        not cache_root.is_absolute()
        or cache_resolved != cache_root
        or not cache_root.is_dir()
    ):
        raise ValueError(
            "DEV_HERMIT_TOOL_CACHE must name one existing absolute non-symlinked directory"
        )
    expected_target_root = cache_resolved / "trees" / parent_sha
    if expected_target_root != authority.target_root:
        raise ValueError(
            "DEV_HERMIT_TOOL_CACHE does not match the cached target root in "
            "immutable tool authority"
        )

    return [
        sys.executable,
        "-c",
        PINNED_BOOTSTRAP_EXEC,
        str(bootstrap),
        bootstrap_sha256,
        "--run-current-tool",
        "--state-root",
        str(state_root),
        "--state-identity",
        f"{authority.state_dev}:{authority.state_ino}",
        "--target-identity",
        f"{authority.target_dev}:{authority.target_ino}",
        "--cache-root",
        str(cache_root),
        "--tool-parent-sha",
        parent_sha,
        "--tool-relative",
        "ci-hub/ci-hub",
        "--",
    ]


def hermit_run_timeout_seconds(child_deadline: int) -> int:
    """Derive Hermit's inner deadline from validate-lock's child deadline.

    Hermit's checked-in scope ladder reserves ``max(60s, run_timeout / 10)``
    after its in-process deadline so it can stop nodes and flush evidence. The
    canonical launcher used to pass only the outer 3600-second child deadline,
    leaving Hermit unbounded and allowing two attempts of a 2400-second DAG node
    to be killed by validate-lock before Hermit could record the retry result.

    Reserve the same grace from the enclosing deadline, plus one second for a
    strict INNER < OUTER ordering. Cap the result so increasing the outer
    cleanup allowance cannot silently authorize more validation work. Because
    the derived run timeout is smaller than the child deadline, Hermit's own
    cleanup grace cannot exceed the grace reserved here.
    """
    cleanup_grace = max(HERMIT_MIN_CLEANUP_GRACE_SECONDS, child_deadline // 10)
    run_timeout = min(
        child_deadline - cleanup_grace - 1,
        HERMIT_MAX_RUN_TIMEOUT_SECONDS,
    )
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


def effective_child_deadline(repo: str, requested: int) -> int:
    """Keep Hermit's external backstop bounded even when callers ask for more."""
    if repo == "rrnewton/hermit":
        return min(requested, HERMIT_MAX_CHILD_DEADLINE_SECONDS)
    return requested


def canonical_repo(value: str) -> str:
    try:
        return SUPPORTED_REPOS[value]
    except KeyError as error:
        raise ValueError(
            f"unsupported validation repository {value!r}; expected rrnewton/hermit or rrnewton/reverie"
        ) from error


def validation_kind(repo: str, *, frozen_validate: bool) -> str:
    """Return the existing validate-lock kind recorded in the run handle."""
    if frozen_validate:
        return FROZEN_VALIDATE_KIND
    if repo == "rrnewton/reverie":
        return "reverie-validate"
    return "validate"


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


def validate_checkout_parent(root: Path) -> Path:
    """Return the parent reserved for disposable validation checkouts."""
    candidate = (root / "worktrees" / "validate").resolve()
    candidate.mkdir(parents=True, exist_ok=True)
    return require_guest_visible_root(candidate, role="validation checkout parent")


def private_cargo_home_parent(root: Path) -> Path:
    """Return the parent for retained per-run Cargo homes.

    ``worktrees/validate`` is a wrkslots-managed directory: every child there
    must be a registered validation slot.  Cargo homes are retained run state,
    not worktrees, so placing them there manufactures directory-without-row
    findings until cleanup succeeds.
    """
    candidate = (root / "ignored" / "validate" / "cargo-homes").resolve()
    candidate.mkdir(parents=True, exist_ok=True)
    return require_guest_visible_root(candidate, role="private Cargo home parent")


def frozen_checkout_parent(root: Path) -> Path:
    """Return a guest-visible checkout parent outside the dev-hermit boundary.

    A frozen run needs independent refs so updating its historical origin/main
    view cannot mutate another checkout's view. The product front door still
    applies through ``DEV_HERMIT_PARENT`` and verifies the frozen holder.
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


def record_unstarted_refusal(
    record_path: Path, *, detail: str, exit_code: int = EXIT_REFUSED
) -> dict[str, Any]:
    """Atomically make a created handle terminal when its unit never started.

    The observer may already be waiting for the unit to appear.  Publishing the
    existing refused/non-running shape under the record lock gives it a durable
    answer instead of leaving a ``preparing``/``launching`` handle that it can
    only classify as unknown.
    """
    return run_registry.update_record(
        record_path,
        state="refused",
        result="systemd-launch-refused",
        exit_code=exit_code,
        detail=detail,
        finished_at=datetime.now(timezone.utc).isoformat(),
    )


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
    # prepare_fresh_checkout, where a failed `wrkslots create` must raise instead
    # of yielding an empty tree that validates fast and green.
    check = bool(kwargs.pop("check", False))
    if kwargs.get("env") is None:
        kwargs["env"] = git_env.sanitized_git_env()
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


def fresh_validation_slot(target: str) -> str:
    """Choose one unique wrkslots identity before read-only entry checks."""

    return f"validate-fresh-{target[:12]}-{os.getpid()}-{secrets.token_hex(4)}"


def _validate_output_paths(root: Path, unit: str) -> tuple[Path, Path]:
    base = root / "ignored/validate/artifacts" / unit
    return base / "e2e", base / "safe-ci-dag-runner"


def _carries_nosuid_or_nodev(path: Path) -> bool:
    """Does `path` sit on a filesystem mounted `nosuid` or `nodev`?

    Either flag alone breaks every Hermit run. Unreadable counts as unusable, so
    an unstattable candidate is skipped rather than silently trusted.
    """
    try:
        flags = os.statvfs(path).f_flag
    except OSError:
        return True
    return bool(flags & os.ST_NOSUID) or bool(flags & os.ST_NODEV)


def runtime_root_parent(environment: Mapping[str, str]) -> Path:
    """Pick a short host directory whose filesystem Hermit's container can use.

    ⚠️ TMPDIR ON A `nosuid` OR `nodev` FILESYSTEM KILLS EVERY HERMIT RUN, and
    `/run/user/<uid>` -- the previous unconditional choice -- carries both.
    Hermit's container writes its frozen `/etc/group` and empty nscd directory as
    temporary files, i.e. into TMPDIR, then bind-mounts each read-only. A
    read-only bind remount inside a user namespace may not clear flags locked in
    the source mount, so the remount returns EPERM and the container never
    spawns. The guest does not merely fail; it never exists.

    Measured 2026-08-27 on devbig030: one arm of the goal-5 concurrent pair
    produced 612 e2e rows, ALL FAILING, 610 of them from this single cause,
    reading as test results while measuring nothing. Reproduced on devbig014 with
    only the mount flags varying and everything else held constant: no
    nosuid/nodev passes, `nosuid` alone fails, `nodev` alone fails.

    Preference order keeps the previous behaviour wherever it was already
    correct, and the length bound still applies -- `/tmp/vXXXXXX` is shorter than
    the `/run/user/<uid>` path it replaces, so nothing regresses there.
    """
    configured = environment.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    candidates = [Path(configured), Path("/tmp"), Path("/var/tmp")]
    rejected: list[str] = []
    for candidate in candidates:
        if not candidate.is_absolute():
            raise ValueError(f"XDG_RUNTIME_DIR must be absolute, got {candidate}")
        if not candidate.is_dir():
            continue
        if _carries_nosuid_or_nodev(candidate):
            rejected.append(str(candidate))
            continue
        return candidate
    # ⚠️ REFUSE RATHER THAN PROCEED. Running anyway produces a full set of rows
    # that all fail for one reason and say nothing about the code, which is
    # strictly worse than not running: it looks like a measurement.
    raise ValueError(
        "no usable parent for this run's TMPDIR: every candidate is on a "
        "filesystem mounted nosuid or nodev, which makes Hermit's read-only "
        "bind remount fail with EPERM and every guest fail to start. "
        f"Rejected: {', '.join(rejected) or 'none found'}"
    )


def prepare_runtime_root(environment: Mapping[str, str]) -> Path:
    """Create the short host-side temporary/cache tree owned by one validate run."""
    parent = runtime_root_parent(environment)
    runtime = Path(tempfile.mkdtemp(prefix="v", dir=parent))
    if len(os.fsencode(runtime)) > MAX_RUNTIME_ROOT_BYTES:
        shutil.rmtree(runtime)
        raise ValueError(
            "XDG_RUNTIME_DIR is too long for validation's Unix-domain socket paths: "
            f"per-run root {runtime} is {len(os.fsencode(runtime))} bytes, maximum "
            f"is {MAX_RUNTIME_ROOT_BYTES}"
        )
    for name in ("cache", "python-cache", "hermit-data"):
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


def parse_wrkslots_create_result(
    output: str, *, root: Path, path: Path, slot: str, target: str
) -> WrkslotsIdentity:
    try:
        value = json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"wrkslots create emitted invalid JSON: {error}") from error
    fields = {
        "agent", "checkouts", "generation", "owner_process",
        "retained_storage_inconsistencies", "slot", "slot_type",
    }
    if not isinstance(value, dict) or set(value) != fields:
        raise RuntimeError("wrkslots create emitted an unexpected result shape")
    generation = value.get("generation")
    checkouts = value.get("checkouts")
    checkout = checkouts[0] if isinstance(checkouts, list) and len(checkouts) == 1 else None
    valid = (
        value.get("slot") == slot
        and value.get("slot_type") == "validate"
        and value.get("agent") == f"validate-{slot}"
        and value.get("owner_process") == "bound"
        and isinstance(value.get("retained_storage_inconsistencies"), list)
        and isinstance(generation, int)
        and not isinstance(generation, bool)
        and generation > 0
        and isinstance(checkout, dict)
        and set(checkout) == {"head", "name", "path"}
        and checkout.get("name") == "checkout"
        and checkout.get("path") == str(path.resolve())
        and checkout.get("head") == target
    )
    try:
        path.resolve().relative_to(root.resolve())
    except ValueError:
        valid = False
    if not valid:
        raise RuntimeError("wrkslots create result does not identify the created checkout")
    return WrkslotsIdentity(slot, generation)


def prepare_fresh_checkout(
    source: Path,
    target: str,
    *,
    run: Runner,
    parent: Path,
    repo: str,
    tool_root: Path = ROOT,
    independent_refs: bool = False,
    slot: str | None = None,
) -> tuple[Path, WrkslotsIdentity | None]:
    """Materialize `target` into a private temp worktree and PROVE it is usable.

    WHY THIS IS THE DEFAULT. An ordinary source checkout must be clean;
    `--materialize-target` may instead use a dirty or stale checkout only as an
    object store. In both cases this function creates the runnable tree from
    exact committed bytes. That also excludes IGNORED source files, which
    `git status --porcelain=v1` cannot describe and which is where measured
    divergence lived. On 2026-08-08 a slot at 393c6a765 reported 0 status paths
    while carrying 5.1 GB of ignored build output, caches and materialized
    dependencies. Validating that tree would validate the commit PLUS 5.1 GB
    of unrecorded local history, so the 40-hex SHA on the receipt would not
    describe what actually ran.

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
    state_root = parent.parent.parent
    slot = slot or fresh_validation_slot(target)
    fresh = require_guest_visible_root(parent / slot, role="fresh checkout")
    # Ordinary validation uses a worktree: it shares the source object store,
    # so this costs no object copy. Frozen validation needs independent refs.
    # Its target is deliberately behind the freshly observed origin/main, but
    # the target's own pre.reverie_pin gate uses origin/main as its historical
    # monotonicity floor. Leaving the live ref visible makes every old target
    # whose Reverie pin has since advanced fail before the test matrix starts.
    # An ordinary local clone keeps refs independent, so its origin/main can name
    # the commit being re-measured without changing the source repository's ref
    # for concurrent users.
    #
    # DELIBERATELY NOT `--shared`, AND NOT `--dissociate`. `--shared` writes
    # `.git/objects/info/alternates` naming an ABSOLUTE path in the source
    # repository. That is fine on the host and FATAL in the pinned root, which
    # bind-mounts only this checkout, as /src: git inside the container resolves
    # a host path that is not there, and every command dies with
    #
    #     error: unable to normalize alternate object path: <source>/.git/objects
    #     fatal: bad object HEAD
    #
    # `build.manifest_guests_in_pinned_root` and its privileged twin are the
    # first nodes to run git inside the container, so they failed at 2-4s with
    # exit 2 and took 16 of 75 nodes with them as dependency skips -- including
    # three that had FAILED in an earlier run and were then merely absent, which
    # reads as three fixes and is not. Measured across two frozen runs at
    # 7ddb3559a09f: the path in the error tracked whatever --checkout named, so
    # a caller cannot avoid this by supplying a self-contained checkout. The
    # alternate is created HERE.
    #
    # A plain local clone costs no object copy either: git HARDLINKS the object
    # files when source and destination share a filesystem, so this keeps the
    # cheap path the previous comment claimed while producing a self-contained
    # store. Measured on a 3.0 GB source: 6s to clone plus 4s for the recursive
    # submodule update, hardlinked (pack link count 2, same inode as the source),
    # and no alternates file anywhere in the tree -- top level or submodule.
    # `--dissociate` was the obvious alternative and is NOT used: it copies all
    # 3.0 GB and exceeded ten minutes, roughly a 50% increase on a 21-minute
    # frozen run. If the two ever land on different filesystems git falls back to
    # a real copy, which is slower but still correct: it degrades in cost, not in
    # behaviour.
    #
    # Bind-mounting the source alongside /src was also rejected: the pinned
    # root's contract is that only the checkout is visible, and admitting a
    # second host path so git can reach outward weakens the isolation that
    # assert-no-network and the fixed image exist to provide.
    identity: WrkslotsIdentity | None = None
    checkout_error: Exception | None = None
    with git_mutation_lock(source, run=run):
        if independent_refs:
            fresh.mkdir(mode=0o700)
            origin_url = checked_output(
                ["git", "-C", str(source), "remote", "get-url", "origin"],
                run=run,
                purpose="cannot resolve source origin for frozen validation",
            )
            run(
                [
                    "git",
                    "clone",
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
            repository = state_root / repo.rsplit("/", 1)[-1]
            try:
                repository_relative = repository.resolve(strict=True).relative_to(
                    state_root.resolve(strict=True)
                )
            except (OSError, ValueError) as error:
                raise RuntimeError(
                    f"cannot locate the managed source repository for {repo} beneath "
                    f"{state_root}: {error}"
                ) from error
            if source.resolve() != repository.resolve():
                source_remote = checked_output(
                    ["git", "-C", str(source), "remote", "get-url", "origin"],
                    run=run,
                    purpose="cannot resolve validation source remote",
                )
                repository_remote = checked_output(
                    ["git", "-C", str(repository), "remote", "get-url", "origin"],
                    run=run,
                    purpose="cannot resolve managed source remote",
                )
                if source_remote != repository_remote:
                    raise RuntimeError(
                        "validation source and managed source have different origin URLs; "
                        "refusing to copy a commit into the wrong repository"
                    )
                run(
                    [
                        "git",
                        "-C",
                        str(repository),
                        "fetch",
                        "--no-tags",
                        str(source),
                        target,
                    ],
                    check=True,
                )
            created = run(
                [
                    str(tool_root / "ci-hub/bin/wrkslots"),
                    "--project-root",
                    str(state_root),
                    "--allow-existing-unregistered-worktrees",
                    "create",
                    slot,
                    "--slot-type",
                    "validate",
                    "--coordinator-authorized",
                    "--agent",
                    f"validate-{slot}",
                    "--task",
                    f"validate {target}",
                    "--purpose",
                    f"validate commit {target}",
                    "--owner-pid",
                    str(os.getpid()),
                    "--coordinator-pid",
                    str(os.getpid()),
                    "--repo",
                    f"checkout={repository_relative.as_posix()}",
                    "--branch",
                    f"checkout=wrkslots/validate/{slot}/checkout",
                    "--start",
                    f"checkout={target}",
                    "--format",
                    "json",
                ],
                check=False,
            )
            if created.returncode != 0:
                detail = created.stderr.strip() or created.stdout.strip()
                raise RuntimeError(
                    "wrkslots could not create the validation checkout. state: "
                    "REFUSED -- validation never ran and no result exists. remedy: "
                    "follow the wrkslots error, then rerun validate-run"
                    + (f": {detail}" if detail else "")
                )
            identity = parse_wrkslots_create_result(
                created.stdout, root=state_root, path=fresh, slot=slot, target=target
            )
        try:
            head = checked_output(
                ["git", "-C", str(fresh), "rev-parse", "HEAD^{commit}"],
                run=run,
                purpose="cannot resolve fresh checkout HEAD",
            )
            if head != target:
                raise RuntimeError(
                    f"fresh checkout resolved to {head}, not requested target {target}"
                )
            run(
                ["git", "-C", str(fresh), "submodule", "update", "--init", "--recursive"],
                check=False,
            )
        except Exception as error:
            checkout_error = error

    def reject_created_checkout(detail: str) -> RuntimeError:
        removed = remove_fresh_checkout(
            source,
            fresh,
            run=run,
            tool_root=tool_root,
            wrkslots=identity,
        )
        disposition = "removed" if removed else f"RETAINED at {fresh}"
        return RuntimeError(f"{detail}; registered checkout was {disposition}")

    if checkout_error is not None:
        raise reject_created_checkout(str(checkout_error)) from checkout_error
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
        raise reject_created_checkout(
            f"fresh checkout {fresh} is missing {', '.join(missing)}; refusing to launch a run "
            "that would exit fast for an environmental reason and read like a pass "
            "because validation never started and no run evidence exists"
        )
    return fresh, identity


def remove_fresh_checkout(
    source: Path,
    fresh: Path,
    *,
    run: Runner,
    tool_root: Path = ROOT,
    completed_record: Path | None = None,
    wrkslots: WrkslotsIdentity | None = None,
) -> bool:
    """Remove the temp worktree AND deregister it; return observed absence."""
    try:
        validate_parent = fresh.parent
        worktrees = validate_parent.parent
        if validate_parent.name == "validate" and worktrees.name == "worktrees":
            state_root = worktrees.parent
            if wrkslots is None or wrkslots.slot != fresh.name:
                raise RuntimeError("managed validation checkout has no matching identity")
            command = [
                str(tool_root / "ci-hub/bin/wrkslots"),
                "--project-root",
                str(state_root),
                "--allow-existing-unregistered-worktrees",
                "remove",
                fresh.name,
                "--validate-complete",
                "--coordinator-pid",
                str(os.getpid()),
                "--expected-generation",
                str(wrkslots.generation),
            ]
        elif validate_parent.name == "ignored":
            state_root = validate_parent.parent
            if completed_record is None:
                raise RuntimeError(
                    "historical checkout cleanup requires its completed validation record"
                )
            try:
                checkout_relative = fresh.relative_to(state_root)
                record_relative = completed_record.resolve().relative_to(state_root)
            except ValueError as error:
                raise RuntimeError(
                    "historical checkout or completed record is outside the project root"
                ) from error
            repository = state_root / source.name
            try:
                repository_relative = repository.relative_to(state_root)
            except ValueError as error:
                raise RuntimeError(
                    "historical checkout repository is outside the project root"
                ) from error
            command = [
                str(tool_root / "ci-hub/bin/wrkslots"),
                "--project-root",
                str(state_root),
                "--allow-existing-unregistered-worktrees",
                "recover",
                "--coordinator-authorized",
                "--coordinator-pid",
                str(os.getpid()),
                "--legacy-validate-checkout",
                checkout_relative.as_posix(),
                "--completed-record",
                record_relative.as_posix(),
                "--repository",
                repository_relative.as_posix(),
            ]
        elif (
            (fresh / ".git").is_dir()
            and validate_parent == frozen_checkout_parent(source.parent)
        ):
            state_root = source.parent.resolve()
            if completed_record is None:
                raise RuntimeError(
                    "frozen checkout cleanup requires its completed validation record"
                )
            try:
                record_relative = completed_record.resolve().relative_to(state_root)
                repository_relative = source.resolve().relative_to(state_root)
            except ValueError as error:
                raise RuntimeError(
                    "frozen checkout record or source repository is outside the project root"
                ) from error
            command = [
                str(tool_root / "ci-hub/bin/wrkslots"),
                "--project-root",
                str(state_root),
                "--allow-existing-unregistered-worktrees",
                "recover",
                "--coordinator-authorized",
                "--coordinator-pid",
                str(os.getpid()),
                "--frozen-validate-checkout",
                str(fresh.resolve()),
                "--completed-record",
                record_relative.as_posix(),
                "--repository",
                repository_relative.as_posix(),
            ]
        else:
            raise RuntimeError(
                f"checkout is outside the managed or frozen validation roots: {fresh}"
            )
        removal = run(command, check=False)
    except RuntimeError as error:
        print(
            f"validate-run: wrkslots cleanup could not run for {fresh}: {error}; "
            "checkout and row state are unconfirmed. Run "
            "'ci-hub/bin/wrkslots recover --coordinator-pid PID' if an interrupted "
            "operation is reported, otherwise rerun the printed remove command.",
            file=sys.stderr,
        )
        return False
    if removal.returncode != 0:
        detail = removal.stderr.strip() or removal.stdout.strip() or (
            f"exit {removal.returncode}"
        )
        print(
            f"validate-run: wrkslots did not confirm removal of validation checkout "
            f"{fresh}: {detail}",
            file=sys.stderr,
        )
        return False
    if fresh.exists():
        print(
            f"validate-run: wrkslots reported success but checkout still exists at {fresh}; "
            "state: RETAINED -- cleanup is not confirmed. remedy: run "
            "'ci-hub/bin/wrkslots doctor --all-machines' and inspect the slot record.",
            file=sys.stderr,
        )
        return False
    return True


def observe_fresh_checkout_cleanup(
    root: Path,
    fresh: Path,
    wrkslots: WrkslotsIdentity | None,
    *,
    run: Runner,
    tool_root: Path = ROOT,
) -> tuple[CleanupPresenceObservation, CleanupPresenceObservation]:
    """Observe the exact path and registration without inferring either from rc."""

    try:
        os.lstat(fresh)
    except FileNotFoundError:
        path = CleanupPresenceObservation(
            CleanupPresence.ABSENT, f"exact checkout path is absent: {fresh}"
        )
    except OSError as error:
        path = CleanupPresenceObservation(
            CleanupPresence.COULD_NOT_DETERMINE,
            f"cannot inspect exact checkout path {fresh}: {error}",
        )
    else:
        path = CleanupPresenceObservation(
            CleanupPresence.RETAINED, f"exact checkout path exists: {fresh}"
        )

    if wrkslots is None:
        row = CleanupPresenceObservation(
            CleanupPresence.COULD_NOT_DETERMINE,
            "no exact wrkslots slot and generation were available",
        )
        return path, row

    command = [
        str(tool_root / "ci-hub/bin/wrkslots"),
        "--project-root",
        str(root.resolve()),
        "--allow-existing-unregistered-worktrees",
        "status",
        "--slot",
        wrkslots.slot,
        "--format",
        "json",
    ]
    try:
        result = run(command, check=False)
    except (OSError, RuntimeError) as error:
        row = CleanupPresenceObservation(
            CleanupPresence.COULD_NOT_DETERMINE,
            f"cannot inspect wrkslots row for slot {wrkslots.slot} generation "
            f"{wrkslots.generation}: {error}",
        )
        return path, row
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or (
            f"exit {result.returncode}"
        )
        row = CleanupPresenceObservation(
            CleanupPresence.COULD_NOT_DETERMINE,
            f"wrkslots status did not complete for slot {wrkslots.slot} generation "
            f"{wrkslots.generation}: {detail}",
        )
        return path, row
    try:
        status = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        row = CleanupPresenceObservation(
            CleanupPresence.COULD_NOT_DETERMINE,
            f"wrkslots status for slot {wrkslots.slot} generation "
            f"{wrkslots.generation} emitted invalid JSON: {error}",
        )
        return path, row
    active = status.get("active") if isinstance(status, dict) else None
    if (
        not isinstance(status, dict)
        or status.get("schema") != 2
        or status.get("project_root") != str(root.resolve())
        or not isinstance(active, list)
    ):
        row = CleanupPresenceObservation(
            CleanupPresence.COULD_NOT_DETERMINE,
            f"wrkslots status for slot {wrkslots.slot} generation "
            f"{wrkslots.generation} did not identify the exact project and active rows",
        )
        return path, row
    if not active:
        row = CleanupPresenceObservation(
            CleanupPresence.ABSENT,
            f"no active row for slot {wrkslots.slot} generation {wrkslots.generation}",
        )
        return path, row
    record = active[0] if len(active) == 1 else None
    checkouts = record.get("checkouts") if isinstance(record, dict) else None
    checkout = checkouts[0] if isinstance(checkouts, list) and len(checkouts) == 1 else None
    raw_path = checkout.get("path") if isinstance(checkout, dict) else None
    registered_path: Path | None = None
    if isinstance(raw_path, str) and raw_path:
        candidate = Path(raw_path)
        if not candidate.is_absolute() and ".." not in candidate.parts:
            registered_path = (root.resolve() / candidate).resolve()
    exact = (
        isinstance(record, dict)
        and record.get("slot") == wrkslots.slot
        and record.get("slot_type") == "validate"
        and type(record.get("generation")) is int
        and record.get("generation") == wrkslots.generation
        and record.get("storage_inconsistencies") == []
        and isinstance(checkout, dict)
        and checkout.get("name") == "checkout"
        and registered_path == fresh.resolve()
    )
    if not exact:
        row = CleanupPresenceObservation(
            CleanupPresence.COULD_NOT_DETERMINE,
            f"wrkslots status row does not match exact slot {wrkslots.slot} generation "
            f"{wrkslots.generation} and checkout path {fresh}",
        )
        return path, row
    row = CleanupPresenceObservation(
        CleanupPresence.RETAINED,
        f"active row exists for slot {wrkslots.slot} generation {wrkslots.generation}",
    )
    return path, row


def remove_fresh_checkouts_batch(
    root: Path,
    checkouts: Sequence[tuple[Path, WrkslotsIdentity]],
    *,
    run: Runner,
    tool_root: Path = ROOT,
) -> dict[str, str | None]:
    if not checkouts or len(checkouts) > VALIDATE_CLEANUP_BATCH_LIMIT:
        raise ValueError("validation cleanup batch size must be between one and eight")
    parent = validate_checkout_parent(root.resolve())
    requested: dict[tuple[str, int], Path] = {}
    for raw_path, identity in checkouts:
        path = raw_path.resolve()
        generation = identity.generation
        if (
            path.parent != parent
            or not path.name.startswith("validate-fresh-")
            or identity.slot != path.name
            or not isinstance(generation, int)
            or isinstance(generation, bool)
            or generation <= 0
            or any(slot == identity.slot for slot, _generation in requested)
        ):
            raise ValueError(f"invalid validation cleanup identity for {path}")
        requested[(identity.slot, generation)] = path
    command = [
        str(tool_root / "ci-hub/bin/wrkslots"),
        "--project-root", str(root.resolve()),
        "--allow-existing-unregistered-worktrees",
        "remove-validate-batch", "--coordinator-authorized",
        "--coordinator-pid", str(os.getpid()),
    ]
    for slot, generation in requested:
        command.extend(("--slot", f"{slot}={generation}"))
    result = run([*command, "--format", "json"], check=False)
    error = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
    if result.returncode != 0:
        return {str(path): error for path in requested.values()}
    try:
        value = json.loads(result.stdout)
        fields = {
            "batch_limit", "process_censuses", "removed", "requested",
            "retained", "same_uid_process_censuses", "schema",
            "shared_process_censuses",
        }
        if not isinstance(value, dict) or set(value) != fields:
            raise ValueError("unexpected fields")
        counts = [
            value.get(name)
            for name in (
                "process_censuses", "shared_process_censuses",
                "same_uid_process_censuses",
            )
        ]
        metadata = [value.get(name) for name in ("schema", "batch_limit", "requested")]
        if (
            any(type(item) is not int for item in metadata)
            or metadata != [1, VALIDATE_CLEANUP_BATCH_LIMIT, len(requested)]
            or any(not isinstance(item, int) or isinstance(item, bool) or item < 0 for item in counts)
            or counts[0] != counts[1]
            or counts[1] not in (0, 1)
            or counts[2] > len(requested)
        ):
            raise ValueError("invalid batch metadata")
        outcomes: dict[tuple[str, int], str | None] = {}
        for removed, rows in ((True, value.get("removed")), (False, value.get("retained"))):
            if not isinstance(rows, list):
                raise ValueError("outcomes are not lists")
            for row in rows:
                expected = {"generation", "slot"} if removed else {"generation", "reason", "slot"}
                if not isinstance(row, dict) or set(row) != expected:
                    raise ValueError("malformed outcome row")
                slot, generation = row.get("slot"), row.get("generation")
                reason = None if removed else row.get("reason")
                if (
                    not isinstance(slot, str)
                    or not isinstance(generation, int)
                    or isinstance(generation, bool)
                ):
                    raise ValueError("malformed outcome identity")
                key = (slot, generation)
                if key not in requested or key in outcomes or (
                    not removed and (not isinstance(reason, str) or not reason.strip())
                ):
                    raise ValueError("unexpected or duplicate outcome identity")
                outcomes[key] = reason
        removed_count = sum(reason is None for reason in outcomes.values())
        if (
            set(outcomes) != set(requested)
            or (counts[1] == 0 and removed_count)
            or counts[2] < removed_count
            or (counts[2] > 0 and counts[1] != 1)
        ):
            raise ValueError("batch census and outcomes disagree")
    except (json.JSONDecodeError, ValueError) as parse_error:
        error = f"wrkslots returned an invalid batch report: {parse_error}"
        return {str(path): error for path in requested.values()}
    return {
        str(path): (
            "wrkslots reported success but the checkout still exists"
            if outcomes[identity] is None and path.exists()
            else outcomes[identity]
        )
        for identity, path in requested.items()
    }


@dataclass(frozen=True)
class OwnerlessCleanupOutcome:
    reason: str | None
    blocks_entry: bool


def remove_ownerless_checkouts_batch(
    root: Path,
    checkouts: Sequence[tuple[Path, Path, Path]],
    *,
    run: Runner,
    tool_root: Path = ROOT,
    frozen: bool = False,
    classify_only: bool = False,
) -> dict[str, "OwnerlessCleanupOutcome"]:
    """Remove or read-only classify a bounded ownerless validation batch."""
    if not checkouts or len(checkouts) > VALIDATE_CLEANUP_BATCH_LIMIT:
        raise ValueError("ownerless validation cleanup batch size must be between one and eight")
    state_root = root.resolve()
    ignored = state_root / "ignored"
    runs = ignored / "validate" / "runs"
    requested: dict[str, Path] = {}
    records: list[str] = []
    repositories: list[str] = []
    for raw_checkout, raw_record, raw_source in checkouts:
        checkout = raw_checkout.resolve()
        record = raw_record.resolve()
        repository = (
            raw_source if raw_source.is_absolute() else state_root / raw_source
        )
        try:
            record_relative = record.relative_to(state_root).as_posix()
            repository_relative = repository.relative_to(state_root).as_posix()
            checkout_argument = (
                str(checkout)
                if frozen
                else checkout.relative_to(state_root).as_posix()
            )
        except ValueError as error:
            raise ValueError(
                "ownerless cleanup path, record, or repository is outside the project root"
            ) from error
        expected_parent = frozen_checkout_parent(state_root) if frozen else ignored
        if (
            checkout.parent != expected_parent
            or not checkout.name.startswith("validate-fresh-")
            or record.parent != runs
            or record.suffix != ".json"
            or checkout_argument in requested
        ):
            raise ValueError(f"invalid ownerless validation cleanup input for {checkout}")
        requested[checkout_argument] = checkout
        records.append(record_relative)
        repositories.append(repository_relative)
    action = (
        "classify-ownerless-validate-batch"
        if classify_only
        else "recover-ownerless-validate-batch"
    )
    command = [
        str(tool_root / "ci-hub/bin/wrkslots"),
        "--project-root", str(state_root),
        "--allow-existing-unregistered-worktrees",
        action,
    ]
    if not classify_only:
        command.extend(
            ("--coordinator-authorized", "--coordinator-pid", str(os.getpid()))
        )
    checkout_flag = "--frozen-validate-checkout" if frozen else "--checkout"
    for checkout in requested:
        command.extend((checkout_flag, checkout))
    for record in records:
        command.extend(("--completed-record", record))
    for repository in repositories:
        command.extend(("--repository", repository))
    result = run([*command, "--format", "json"], check=False)
    error = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
    if result.returncode != 0:
        return {
            str(path): OwnerlessCleanupOutcome(error, True)
            for path in requested.values()
        }
    try:
        value = json.loads(result.stdout)
        if classify_only:
            expected_fields = {
                "classifications",
                "create_journals",
                "process_censuses",
                "requested",
                "schema",
            }
            if not isinstance(value, dict) or set(value) != expected_fields:
                raise ValueError("unexpected classification fields")
            rows = value.get("classifications")
            schema = value.get("schema")
            requested_count = value.get("requested")
            process_censuses = value.get("process_censuses")
            if (
                type(schema) is not int
                or schema != 1
                or type(requested_count) is not int
                or requested_count != len(requested)
                or type(process_censuses) is not int
                or process_censuses not in (0, 1)
                or not isinstance(value.get("create_journals"), list)
                or not isinstance(rows, list)
            ):
                raise ValueError("invalid classification metadata")
            outcomes: dict[str, OwnerlessCleanupOutcome] = {}
            state_blocks_entry = {
                "could-not-classify": True,
                "historical-retained": False,
                "terminal-retained": False,
            }
            for row in rows:
                if not isinstance(row, dict) or set(row) != {
                    "blocks_entry", "checkout", "reason", "state",
                }:
                    raise ValueError("malformed classification row")
                checkout = row.get("checkout")
                reason = row.get("reason")
                blocks_entry = row.get("blocks_entry")
                state = row.get("state")
                if (
                    not isinstance(checkout, str)
                    or checkout not in requested
                    or checkout in outcomes
                    or not isinstance(reason, str)
                    or not reason.strip()
                    or type(blocks_entry) is not bool
                    or not isinstance(state, str)
                    or state not in state_blocks_entry
                    or blocks_entry is not state_blocks_entry[state]
                ):
                    raise ValueError("invalid classification outcome")
                outcomes[checkout] = OwnerlessCleanupOutcome(reason, blocks_entry)
            if set(outcomes) != set(requested):
                raise ValueError("classification did not cover every requested checkout")
            if any(not outcome.blocks_entry for outcome in outcomes.values()) and (
                process_censuses != 1
            ):
                raise ValueError(
                    "nonblocking classification has no process census"
                )
            return {str(path): outcomes[key] for key, path in requested.items()}
        fields = {
            "batch_limit", "process_censuses", "removed", "requested",
            "retained", "same_uid_process_censuses", "schema",
            "shared_process_censuses",
        }
        if not isinstance(value, dict) or set(value) != fields:
            raise ValueError("unexpected fields")
        counts = [
            value.get(name)
            for name in (
                "process_censuses", "shared_process_censuses",
                "same_uid_process_censuses",
            )
        ]
        metadata = [value.get(name) for name in ("schema", "batch_limit", "requested")]
        if (
            any(type(item) is not int for item in metadata)
            or metadata != [2, VALIDATE_CLEANUP_BATCH_LIMIT, len(requested)]
            or any(not isinstance(item, int) or isinstance(item, bool) or item < 0 for item in counts)
            or counts[0] != counts[1]
            or counts[1] not in (0, 1)
            or counts[2] > len(requested)
        ):
            raise ValueError("invalid batch metadata")
        outcomes: dict[str, OwnerlessCleanupOutcome] = {}
        for removed, rows in ((True, value.get("removed")), (False, value.get("retained"))):
            if not isinstance(rows, list):
                raise ValueError("outcomes are not lists")
            for row in rows:
                expected = (
                    {"checkout"}
                    if removed
                    else {"blocks_entry", "checkout", "reason"}
                )
                if not isinstance(row, dict) or set(row) != expected:
                    raise ValueError("malformed outcome row")
                checkout = row.get("checkout")
                reason = None if removed else row.get("reason")
                blocks_entry = False if removed else row.get("blocks_entry")
                if (
                    not isinstance(checkout, str)
                    or checkout not in requested
                    or checkout in outcomes
                    or (not removed and (not isinstance(reason, str) or not reason.strip()))
                    or type(blocks_entry) is not bool
                ):
                    raise ValueError("unexpected or duplicate outcome identity")
                outcomes[checkout] = OwnerlessCleanupOutcome(reason, blocks_entry)
        removed_count = sum(outcome.reason is None for outcome in outcomes.values())
        nonblocking_count = sum(
            outcome.reason is not None and not outcome.blocks_entry
            for outcome in outcomes.values()
        )
        if (
            set(outcomes) != set(requested)
            or (counts[1] == 0 and removed_count)
            or (nonblocking_count and not frozen)
            or counts[2] < removed_count + nonblocking_count
            or (counts[2] > 0 and counts[1] != 1)
        ):
            raise ValueError("batch census and outcomes disagree")
    except (json.JSONDecodeError, ValueError) as parse_error:
        error = f"wrkslots returned an invalid ownerless batch report: {parse_error}"
        return {
            str(path): OwnerlessCleanupOutcome(error, True)
            for path in requested.values()
        }
    return {
        str(path): OwnerlessCleanupOutcome(
            "wrkslots reported success but the checkout still exists",
            True,
        )
        if outcomes[checkout].reason is None and (path.exists() or path.is_symlink())
        else outcomes[checkout]
        for checkout, path in requested.items()
    }


def archive_orphaned_receipts(
    root: Path,
    fresh: Path,
    locations: Sequence[str],
    *,
    unit: str,
) -> list[str]:
    """Copy checkout-local receipt evidence outside the disposable checkout."""
    destination_root = root / "ignored" / "validate" / "orphaned-receipts" / unit
    fresh_root = fresh.resolve()
    archived: list[str] = []
    for raw in locations:
        relative = Path(raw)
        if relative.is_absolute() or ".." in relative.parts:
            raise RuntimeError(f"unsafe orphaned receipt path {raw!r}")
        source = fresh / relative
        if source.is_symlink() or not source.is_file():
            raise RuntimeError(f"orphaned receipt is not a regular file: {source}")
        try:
            source.resolve().relative_to(fresh_root)
        except ValueError as error:
            raise RuntimeError(f"orphaned receipt escapes its checkout: {source}") from error
        destination = destination_root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        try:
            destination.parent.resolve().relative_to(destination_root.resolve())
        except ValueError as error:
            raise RuntimeError(f"orphaned receipt archive path escapes: {destination}") from error
        if destination.is_symlink():
            raise RuntimeError(f"orphaned receipt archive path is a symlink: {destination}")
        if destination.exists():
            if not destination.is_file() or not filecmp.cmp(
                source, destination, shallow=False
            ):
                raise RuntimeError(
                    f"orphaned receipt archive path has different content: {destination}"
                )
        else:
            shutil.copy2(source, destination, follow_symlinks=False)
        archived.append(str(destination))
    return archived


def recorded_checkout_parents(root: Path, record: Mapping[str, Any]) -> list[Path]:
    """Exact parents in which this record may own a disposable checkout."""
    parents = [
        (root / "worktrees" / "validate").resolve(),
        # Before disposable validation checkouts were separated from retained
        # evidence, ordinary runs put them directly below ignored/.  Those
        # directories are still validate-only checkouts, and a missing
        # temporary_checkout field is the legacy shape rather than evidence
        # that a user works there.
        (root / "ignored").resolve(),
    ]
    if record.get("admission") == FROZEN_RESULT_ADMISSION:
        # Accept the short-lived in-boundary location too, so interrupted
        # frozen runs made between 2ad960917 and this repair remain sweepable.
        frozen = (root.parent / f".{root.name}-frozen-validate").resolve()
        if frozen == root or root in frozen.parents:
            raise ValueError(
                "frozen validation checkout parent must be outside dev-hermit"
            )
        parents.insert(0, frozen)
    return parents


def recorded_wrkslots_identity(
    record: Mapping[str, Any], checkout: Path
) -> WrkslotsIdentity | None:
    has_slot = "wrkslots_slot" in record
    has_generation = "wrkslots_generation" in record
    if not has_slot and not has_generation:
        return None
    slot, generation = record.get("wrkslots_slot"), record.get("wrkslots_generation")
    if has_slot != has_generation:
        raise ValueError("wrkslots slot and generation must be recorded together")
    if not isinstance(slot, str) or slot != checkout.name:
        raise ValueError("recorded wrkslots slot does not match the checkout")
    if (
        not isinstance(generation, int)
        or isinstance(generation, bool)
        or generation <= 0
    ):
        raise ValueError("recorded wrkslots generation is not positive")
    return WrkslotsIdentity(slot, generation)


LEGACY_IN_PLACE_PRODUCER = "systemd-user-v1"


def _resolved_project_path(root: Path, raw: object, *, role: str) -> Path:
    if not isinstance(raw, str) or not raw:
        raise ValueError(f"{role} is not a non-empty path")
    relative = Path(raw)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"{role} is not project-relative")
    resolved = (root / relative).resolve()
    try:
        resolved.relative_to(root.resolve())
    except ValueError as error:
        raise ValueError(f"{role} escapes the project root") from error
    return resolved


def _paths_overlap(first: Path, second: Path) -> bool:
    try:
        first.relative_to(second)
        return True
    except ValueError:
        pass
    try:
        second.relative_to(first)
        return True
    except ValueError:
        return False


def classify_create_journal_scopes(
    root: Path,
    *,
    prospective_slot: str,
    prospective_checkout: Path,
    run: Runner,
    tool_root: Path = ROOT,
) -> tuple[CreateJournalScope, ...]:
    """Read and strictly bind provider evidence for interrupted slot creates."""

    state_root = root.resolve()
    prospective = prospective_checkout.resolve()
    cargo_parent = (state_root / "ignored/validate/cargo-homes").resolve()
    expected_requested_path = (
        state_root / "worktrees" / "validate" / prospective_slot
    ).resolve()
    requested_agent = f"validate-{prospective_slot}"
    result = run(
        [
            str(tool_root / "ci-hub/bin/wrkslots"),
            "--project-root",
            str(state_root),
            "--allow-existing-unregistered-worktrees",
            "classify-create-journals",
            prospective_slot,
            "--slot-type",
            "validate",
            "--agent",
            requested_agent,
            "--format",
            "json",
        ],
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"wrkslots could not classify create journals: {detail}")

    top_fields = {"blocking", "journals", "requested", "schema"}
    journal_fields = {
        "agent",
        "checkouts",
        "coordinator",
        "coordinator_state",
        "created",
        "dirty_checkouts",
        "journal",
        "journal_identity",
        "machine",
        "owner",
        "owner_state",
        "planned",
        "registry_state",
        "slot",
        "slot_path",
        "slot_type",
        "state",
    }
    checkout_fields = {"branch", "dirty", "head", "name", "path"}
    try:
        value = json.loads(result.stdout)
        if not isinstance(value, dict) or set(value) != top_fields:
            raise ValueError("unexpected top-level fields")
        if value.get("schema") != 2 or value.get("blocking") is not False:
            raise ValueError("unexpected classification schema or blocking state")
        requested = value.get("requested")
        if requested != {
            "agent": requested_agent,
            "slot": prospective_slot,
            "slot_path": str(expected_requested_path),
            "slot_type": "validate",
        }:
            raise ValueError("classification does not bind the requested validation slot")
        rows = value.get("journals")
        if not isinstance(rows, list):
            raise ValueError("journal classifications are not a list")
        scopes: list[CreateJournalScope] = []
        seen_slots: set[tuple[str, str]] = set()
        for row in rows:
            if not isinstance(row, dict) or set(row) != journal_fields:
                raise ValueError("malformed journal classification")
            state = row.get("state")
            owner_state = row.get("owner_state")
            coordinator_state = row.get("coordinator_state")
            registry_state = row.get("registry_state")
            expected_process_state = {
                "live-incomplete-create": "live",
                "dead-incomplete-create": "dead",
            }.get(state)
            if state == "completed-create":
                if registry_state != "exact-active":
                    raise ValueError("completed journal has no exact active row")
            elif (
                expected_process_state is None
                or owner_state != expected_process_state
                or coordinator_state != expected_process_state
                or registry_state != "absent"
            ):
                raise ValueError("incomplete journal has uncertain process or registry state")
            for field in ("agent", "machine", "owner", "coordinator", "slot"):
                if not isinstance(row.get(field), str) or not row[field]:
                    raise ValueError(f"journal {field} is not non-empty text")
            slot = str(row["slot"])
            machine = str(row["machine"])
            key = (machine, slot)
            if key in seen_slots:
                raise ValueError("journal classification repeats a slot")
            seen_slots.add(key)
            slot_type = row.get("slot_type")
            if slot_type not in {"agent", "validate"}:
                raise ValueError("journal has an invalid slot type")
            raw_slot_path = row.get("slot_path")
            if not isinstance(raw_slot_path, str) or not raw_slot_path:
                raise ValueError("journal slot path is missing")
            slot_path = Path(raw_slot_path).resolve()
            expected_slot_root = (
                state_root
                / "worktrees"
                / ("slots" if slot_type == "agent" else "validate")
                / slot
            ).resolve()
            if slot_path != expected_slot_root:
                raise ValueError("journal slot path does not match its slot identity")
            if _paths_overlap(prospective, slot_path) or _paths_overlap(
                cargo_parent, slot_path
            ):
                raise ValueError("prospective validation storage overlaps a journal slot")

            journal_path = Path(str(row.get("journal", ""))).resolve()
            if journal_path.parent != (state_root / "worktrees").resolve():
                raise ValueError("journal path is outside the registry control directory")
            identity = row.get("journal_identity")
            if not isinstance(identity, dict) or set(identity) != {
                "device", "inode", "sha256", "size",
            }:
                raise ValueError("journal identity is malformed")
            journal_bytes = journal_path.read_bytes()
            metadata = journal_path.stat(follow_symlinks=False)
            if (
                any(
                    type(identity.get(field)) is not int
                    for field in ("device", "inode", "size")
                )
                or identity["device"] != metadata.st_dev
                or identity["inode"] != metadata.st_ino
                or identity["size"] != metadata.st_size
                or identity.get("sha256") != hashlib.sha256(journal_bytes).hexdigest()
            ):
                raise ValueError("journal changed after provider classification")

            checkout_rows = row.get("checkouts")
            dirty_rows = row.get("dirty_checkouts")
            if (
                type(row.get("planned")) is not int
                or type(row.get("created")) is not int
                or not isinstance(checkout_rows, list)
                or not isinstance(dirty_rows, list)
                or row["planned"] < row["created"]
                or row["created"] != len(checkout_rows)
            ):
                raise ValueError("journal checkout counts are malformed")
            created: list[CreateJournalCheckout] = []
            created_names: set[str] = set()
            for checkout_row in checkout_rows:
                if not isinstance(checkout_row, dict) or set(checkout_row) != checkout_fields:
                    raise ValueError("checkout classification is malformed")
                name = checkout_row.get("name")
                branch = checkout_row.get("branch")
                head = checkout_row.get("head")
                if (
                    not isinstance(name, str)
                    or not name
                    or name in created_names
                    or not isinstance(branch, str)
                    or not branch
                    or not isinstance(head, str)
                    or SHA_RE.fullmatch(head) is None
                    or type(checkout_row.get("dirty")) is not bool
                ):
                    raise ValueError("checkout identity is malformed")
                path = _resolved_project_path(
                    state_root, checkout_row.get("path"), role="checkout path"
                )
                if not path.is_relative_to(slot_path):
                    raise ValueError("checkout is outside its slot")
                created_names.add(name)
                created.append(CreateJournalCheckout(name, path, branch, head))
            if (
                len(dirty_rows) != len(set(dirty_rows))
                or any(
                    not isinstance(name, str) or name not in created_names
                    for name in dirty_rows
                )
            ):
                raise ValueError("dirty checkout identities are malformed")
            if {
                str(item["name"])
                for item in checkout_rows
                if item.get("dirty") is True
            } != set(dirty_rows):
                raise ValueError("dirty checkout facts disagree")
            if (
                state in {"live-incomplete-create", "dead-incomplete-create"}
                and slot_type == "agent"
            ):
                scopes.append(
                    CreateJournalScope(
                        state=str(state),
                        slot=slot,
                        agent=str(row["agent"]),
                        slot_path=slot_path,
                        checkouts=tuple(created),
                        dirty_checkouts=frozenset(str(item) for item in dirty_rows),
                    )
                )
        return tuple(scopes)
    except (json.JSONDecodeError, OSError, ValueError) as error:
        raise RuntimeError(
            f"wrkslots returned an invalid create-journal classification: {error}"
        ) from error


def classify_retained_current_in_place_record(
    root: Path, record: Mapping[str, Any]
) -> tuple[str | None, str | None]:
    """Describe non-disposable storage, without asserting outcome or ownership.

    Call only after classify_recorded_checkout positively identifies storage as
    non-disposable. A create journal can disappear, and the source's owner or
    HEAD can change, without changing the historical record's storage contract.
    This classification authorizes neither removal nor access to a live slot.
    """
    try:
        run_registry.parse_current_record(record)
    except (RuntimeError, ValueError) as error:
        return None, f"current in-place run record is malformed: {error}"
    if record.get("temporary_checkout") is not False:
        return None, "current in-place run record is not explicitly non-temporary"
    raw_checkout = record.get("checkout")
    raw_source = record.get("source_checkout")
    raw_cargo = record.get("cargo_home")
    if not all(
        isinstance(path, str) and Path(path).is_absolute()
        for path in (raw_checkout, raw_source, raw_cargo)
    ):
        return None, "current in-place run record paths must be absolute"
    checkout = Path(str(raw_checkout)).resolve()
    if checkout != Path(str(raw_source)).resolve():
        return None, "in-place run record checkout differs from its source checkout"
    _cargo_home, cargo_error = classify_recorded_cargo_home(root, record)
    if cargo_error is not None:
        return None, cargo_error
    # A legitimate cleanup may already have removed Cargo's directory. Still
    # check its recorded path: absence does not make overlapping storage safe.
    if _paths_overlap(Path(str(raw_cargo)).resolve(), checkout):
        return None, "in-place run record Cargo home overlaps its source checkout"
    return (
        "non-disposable in-place storage remains untouched; "
        "outcome and process state are unchanged",
        None,
    )


def classify_protected_in_place_record(
    root: Path,
    record: Mapping[str, Any],
    scopes: Sequence[CreateJournalScope],
) -> tuple[str | None, str | None]:
    """Bind one in-place record to exactly one provider-verified journal scope."""

    raw_checkout = record.get("checkout")
    raw_source = record.get("source_checkout")
    if not isinstance(raw_checkout, str) or not isinstance(raw_source, str):
        return None, "in-place run record has no checkout and source paths"
    checkout = Path(raw_checkout).resolve()
    source = Path(raw_source).resolve()
    if checkout != source:
        return None, "in-place run record checkout differs from its source checkout"
    producer = record.get("producer")
    if producer == run_registry.PRODUCER:
        try:
            parsed = run_registry.parse_current_record(record)
        except (RuntimeError, ValueError) as error:
            return None, f"current in-place run record is malformed: {error}"
        if record.get("temporary_checkout") is not False:
            return None, "current in-place run record is not explicitly non-temporary"
        record_agent = parsed.agent
    elif producer == LEGACY_IN_PLACE_PRODUCER:
        if "temporary_checkout" in record:
            return None, "legacy in-place run record unexpectedly has temporary_checkout"
        record_agent = record.get("agent")
        if not isinstance(record_agent, str) or not record_agent:
            return None, "legacy in-place run record has no agent identity"
    else:
        return None, "historical in-place run record has no recognized producer identity"
    matches = [
        (scope, created)
        for scope in scopes
        for created in scope.checkouts
        if checkout == created.path or checkout.is_relative_to(created.path)
    ]
    if len(matches) != 1:
        return None, "in-place run record is not under exactly one classified agent slot"
    scope, created = matches[0]
    if record_agent != scope.agent:
        return None, "in-place run record agent differs from its classified agent slot"
    cargo_home, cargo_error = classify_recorded_cargo_home(root, record)
    if cargo_error is not None:
        return None, cargo_error
    if cargo_home is None:
        return None, "in-place run record has no retained Cargo home"
    if _paths_overlap(cargo_home, scope.slot_path):
        return None, "in-place run record Cargo home overlaps its agent slot"
    dirty = "dirty" if created.name in scope.dirty_checkouts else "clean"
    return (
        (
            f"{scope.state} journal preserves {dirty} agent slot {scope.slot}; "
            "the exact in-place run record and Cargo home remain retained"
        ),
        None,
    )


def is_in_place_scope_candidate(record: Mapping[str, Any]) -> bool:
    checkout = record.get("checkout")
    source = record.get("source_checkout")
    if not isinstance(checkout, str) or not isinstance(source, str):
        return False
    if Path(checkout).resolve() != Path(source).resolve():
        return False
    if "temporary_checkout" in record:
        return record.get("temporary_checkout") is False
    return record.get("producer") == LEGACY_IN_PLACE_PRODUCER


def classify_recorded_checkout(
    root: Path, record: Mapping[str, Any]
) -> tuple[Path | None, str | None]:
    """Classify one recorded checkout without changing it.

    A validation run made with ``--in-place`` names its source checkout and
    records ``temporary_checkout=false``.  That is not removable validation
    storage.  Historical run records predate this field, while a record written
    by the current producer is malformed if it omits the field.
    """
    raw_checkout = record.get("checkout")
    if not isinstance(raw_checkout, str):
        return None, "run record has no checkout path"
    checkout = Path(raw_checkout).resolve()
    parents = recorded_checkout_parents(root, record)
    has_temporary = "temporary_checkout" in record
    temporary = record.get("temporary_checkout")
    removable = (
        checkout.parent in parents
        and checkout.name.startswith("validate-fresh-")
    )
    if removable:
        if has_temporary and temporary is not True:
            return None, "temporary_checkout must be true when present"
        if not has_temporary and record.get("producer") == run_registry.PRODUCER:
            return None, "current run record has no temporary_checkout"
        return checkout, None
    if has_temporary and type(temporary) is not bool:
        return None, "temporary_checkout must be a boolean when present"
    if not has_temporary and record.get("producer") == run_registry.PRODUCER:
        return None, "current run record has no temporary_checkout"
    if not has_temporary and record.get("producer") != LEGACY_IN_PLACE_PRODUCER:
        return None, "historical run record has no recognized producer identity"
    if temporary is True:
        expected = " or ".join(str(parent) for parent in parents)
        return None, f"temporary checkout is outside {expected}"
    return None, None


def cleanup_recorded_checkout(
    root: Path,
    record: Mapping[str, Any],
    *,
    record_path: Path,
    unit: str,
    run: Runner,
    tool_root: Path = ROOT,
    wrkslots: WrkslotsIdentity | None = None,
) -> tuple[bool, list[str], str | None]:
    """Archive local receipt evidence and remove one terminal temp checkout."""
    checkout, reason = classify_recorded_checkout(root, record)
    if reason is not None:
        return False, [], reason
    if checkout is None:
        return False, [], None
    if not checkout.exists():
        return False, [], None
    if checkout.parent == validate_checkout_parent(root) and wrkslots is None:
        wrkslots = recorded_wrkslots_identity(record, checkout)
        if wrkslots is None:
            return False, [], "run record has no exact wrkslots identity"
    # Every linked, historical, and frozen checkout is handed to wrkslots,
    # which owns the verified host-process census.  A second parent-side procfs
    # scan can see a restricted namespace and turn an unreadable process into a
    # false live holder.
    locations = orphaned_receipt_locations(checkout, run=run)
    archived = archive_orphaned_receipts(root, checkout, locations, unit=unit)
    raw_source = record.get("source_checkout")
    if isinstance(raw_source, str):
        source = Path(raw_source)
    else:
        repo = canonical_repo(str(record.get("repo", "rrnewton/hermit")))
        source = root / repo.rsplit("/", 1)[-1]
    removed = remove_fresh_checkout(
        source,
        checkout,
        run=run,
        tool_root=tool_root,
        completed_record=record_path,
        wrkslots=wrkslots,
    )
    return (
        removed,
        archived,
        None if removed else "checkout still exists after the removal attempt",
    )


def classify_recorded_cargo_home(
    root: Path, record: Mapping[str, Any]
) -> tuple[Path | None, str | None]:
    """Classify one recorded private Cargo home without changing it."""
    if "cargo_home" not in record:
        return None, None
    raw_private = record.get("cargo_home")
    if not isinstance(raw_private, str):
        return None, "recorded Cargo home must be a path string"
    private = Path(raw_private).resolve()
    parents = (
        (root / "ignored" / "validate" / "cargo-homes").resolve(),
        # Runs launched before private Cargo homes were separated from
        # wrkslots-managed validation checkouts remain sweepable.
        (root / "worktrees" / "validate").resolve(),
    )
    if private.parent not in parents or not private.name.startswith("validate-cargo-"):
        expected = " or ".join(str(parent) for parent in parents)
        return None, f"recorded Cargo home is outside {expected}"
    return (private, None) if private.exists() else (None, None)


def cleanup_recorded_cargo_home(
    root: Path,
    record: Mapping[str, Any],
    *,
    record_path: Path,
    run: Runner,
    tool_root: Path = ROOT,
) -> tuple[bool, str | None]:
    """Recover one recorded per-run Cargo home after its unit is terminal."""
    private, reason = classify_recorded_cargo_home(root, record)
    if reason is not None:
        return False, reason
    if private is None:
        return False, None
    state_root = root.resolve()
    try:
        private_relative = private.relative_to(state_root)
        record_relative = record_path.resolve().relative_to(state_root)
    except ValueError as error:
        raise RuntimeError(
            "recorded Cargo home or completed record is outside the project root"
        ) from error
    removal = run(
        [
            str(tool_root / "ci-hub/bin/wrkslots"),
            "--project-root",
            str(state_root),
            "--allow-existing-unregistered-worktrees",
            "recover",
            "--coordinator-authorized",
            "--coordinator-pid",
            str(os.getpid()),
            "--ownerless-validate-cargo-home",
            private_relative.as_posix(),
            "--completed-record",
            record_relative.as_posix(),
        ],
        check=False,
    )
    if removal.returncode != 0:
        detail = removal.stderr.strip() or removal.stdout.strip() or (
            f"exit {removal.returncode}"
        )
        return False, f"wrkslots retained the Cargo home: {detail}"
    if private.exists():
        return False, "wrkslots reported success but the Cargo home still exists"
    return True, None


def recorded_cleanup_path_exists(root: Path, record: Mapping[str, Any]) -> bool:
    """Whether a run record names material that this sweep could remove."""
    raw_checkout = record.get("checkout")
    if isinstance(raw_checkout, str):
        checkout = Path(raw_checkout).resolve()
        accepted_unmarked_checkout = (
            checkout.parent in recorded_checkout_parents(root, record)
            and checkout.name.startswith("validate-fresh-")
        )
        if checkout.exists() and (
            record.get("temporary_checkout") is True or accepted_unmarked_checkout
        ):
            return True

    raw_cargo_home = record.get("cargo_home")
    return isinstance(raw_cargo_home, str) and Path(raw_cargo_home).resolve().exists()


def read_cleanup_record_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read validation handle {path}: {error}") from error
    if not isinstance(value, dict):
        raise RuntimeError(f"validation handle {path} is not an object")
    return value


def read_cleanup_record(path: Path) -> dict[str, Any]:
    """Read cleanup fields without applying the evolving verdict schema."""
    value = read_cleanup_record_object(path)
    if not run_registry.is_current_schema_version(value.get("schema_version")):
        raise RuntimeError(f"validation handle {path} has an unsupported schema")
    return value


def cleanup_checkout_reference(root: Path, record: Mapping[str, Any]) -> Path | None:
    raw = record.get("checkout")
    if not isinstance(raw, str):
        return None
    checkout = Path(raw).resolve()
    return (
        checkout
        if checkout.exists()
        and checkout.name.startswith("validate-fresh-")
        and checkout.parent in recorded_checkout_parents(root, record)
        else None
    )


def discover_cleanup_groups(
    root: Path, runs: Path
) -> tuple[
    dict[str, CleanupGroup],
    list[CleanupRow],
    list[CleanupRow],
    list[dict[str, str]],
    dict[str, int],
]:
    groups: dict[str, CleanupGroup] = {}
    singles: list[CleanupRow] = []
    rows: list[CleanupRow] = []
    retained: list[dict[str, str]] = []
    unattributable = False
    # Counted so the report can say what discovery LOOKED AT, not only what it
    # acted on. A gate that reports "removed 0" is unreadable without knowing
    # whether it considered 0 candidates or 111.
    discovery_counts: dict[str, int] = {
        "json_files_seen": 0,
        "framework_result_sidecars_skipped": 0,
        "run_handle_candidates": 0,
    }
    for path in sorted(runs.glob("*.json")):
        discovery_counts["json_files_seen"] += 1
        # ⚠️ A FRAMEWORK RESULT SIDECAR IS NOT A RUN HANDLE. Both live here and
        # both end `.json`, so this glob sees them; parsing one as a handle
        # reports it as an unsupported schema and the gate goes red on a file
        # that is exactly what it should be. Measured 2026-09-03: 98 of the 111
        # failures this gate reported were this and nothing else.
        #
        # NARROW ON PURPOSE. This skips the ONE name the producer constructs --
        # service_result.result_path is the other direction of the same rule --
        # and only when the run handle it names is actually present. A sidecar
        # with no handle is orphaned rather than expected, so it stays visible
        # instead of being quietly dropped, and a genuinely malformed or
        # unsupported handle is untouched by any of this.
        handle = service_result.handle_path_for_result(path)
        if handle is not None:
            if handle.exists():
                discovery_counts["framework_result_sidecars_skipped"] += 1
                continue
            retained.append(
                {
                    "checkout": str(path),
                    "reason": (
                        "framework result sidecar has no run handle at "
                        f"{handle}; it is not a candidate and not expected"
                    ),
                }
            )
            continue
        try:
            raw = read_cleanup_record_object(path)
        except RuntimeError as error:
            retained.append({"checkout": str(path), "reason": str(error)})
            unattributable = True
            continue
        discovery_counts["run_handle_candidates"] += 1
        checkout = cleanup_checkout_reference(root, raw)
        if not run_registry.is_current_schema_version(raw.get("schema_version")):
            error = RuntimeError(f"validation handle {path} has an unsupported schema")
            retained.append({"checkout": str(path), "reason": str(error)})
            if checkout is not None:
                managed = checkout.parent == validate_checkout_parent(root)
                groups.setdefault(str(checkout), CleanupGroup(checkout, managed)).reasons.append(str(error))
            continue
        record = raw
        row = CleanupRow(path, record)
        if recorded_cleanup_path_exists(root, record):
            rows.append(row)
        if checkout is not None:
            managed = checkout.parent == validate_checkout_parent(root)
            groups.setdefault(str(checkout), CleanupGroup(checkout, managed)).rows.append(row)
        elif row in rows:
            singles.append(row)
    if unattributable:
        reason = "an unreadable validation handle prevents proving unique checkout ownership"
        for group in groups.values():
            group.reasons.append(reason)
        for row in singles:
            retained.append({"checkout": str(row.record.get("checkout", "")), "reason": reason})
        singles.clear()
        rows.clear()
    return groups, singles, rows, retained, discovery_counts


def terminal_cleanup_unit(
    path: Path, record: Mapping[str, Any], *, run: Runner
) -> CleanupUnitObservation:
    try:
        unit = sanitize_unit(str(record.get("unit", path.stem)))
        properties, unit_absent = service_properties_with_absence(unit, run=run)
    except (OSError, RuntimeError, ValueError) as error:
        return CleanupUnitObservation(
            None, CleanupUnitState.INDETERMINATE, str(error)
        )
    if properties is not None and properties.get("ActiveState") not in TERMINAL_STATES:
        return CleanupUnitObservation(
            None, CleanupUnitState.RUNNING, "unit-running"
        )
    if (
        properties is None
        and not service_result.is_evidenced_terminal(record)
        and not cleanup_record_allows_removal_attempt(
            record, unit_absent=unit_absent
        )
    ):
        return CleanupUnitObservation(
            None,
            CleanupUnitState.INDETERMINATE,
            "unit state unavailable and run record is not terminal",
        )
    return CleanupUnitObservation(unit, CleanupUnitState.TERMINAL, None)


def preflight_cleanup_group(
    root: Path,
    group: CleanupGroup,
    *,
    run: Runner,
    read_only_ownerless: bool = False,
) -> list[CleanupHold]:
    found: list[CleanupHold] = []
    if not group.managed and len(group.rows) != 1:
        hold = CleanupHold(
            "ownerless validation checkout must have exactly one run record; "
            f"found {len(group.rows)}",
            True,
        )
        group.reasons.append(hold.reason)
        return [hold]
    identities: set[WrkslotsIdentity] = set()
    for row in group.rows:
        observation = terminal_cleanup_unit(row.path, row.record, run=run)
        row.unit = observation.unit
        if observation.reason is not None and not (
            read_only_ownerless and not group.managed
        ):
            blocks_entry = observation.blocks_entry
            if observation.state is CleanupUnitState.RUNNING and group.managed:
                blocks_entry = False
            found.append(
                CleanupHold(
                    observation.reason,
                    blocks_entry,
                )
            )
        _checkout, checkout_error = classify_recorded_checkout(root, row.record)
        if checkout_error is not None:
            found.append(CleanupHold(checkout_error, True))
        if group.managed:
            try:
                identity = recorded_wrkslots_identity(row.record, group.checkout)
            except ValueError as error:
                found.append(CleanupHold(str(error), True))
            else:
                if identity is None:
                    found.append(CleanupHold("run record has no exact wrkslots identity", True))
                else:
                    identities.add(identity)
    if group.managed:
        if len(identities) > 1:
            found.append(CleanupHold("run records disagree about the wrkslots generation", True))
        elif len(identities) == 1 and not found and group.rows:
            group.identity = next(iter(identities))
    group.reasons.extend(item.reason for item in found)
    return found


def select_cleanup_groups(
    root: Path,
    groups: Mapping[str, CleanupGroup],
    *,
    persist_cursor: bool = True,
) -> tuple[list[CleanupGroup], list[CleanupGroup]]:
    keys = sorted(groups)
    if not keys:
        return [], []
    if not persist_cursor:
        chosen = keys[:VALIDATE_CLEANUP_BATCH_LIMIT]
        return (
            [groups[key] for key in chosen],
            [groups[key] for key in keys[len(chosen) :]],
        )
    cursor = root / "ignored" / "validate" / "checkout-cleanup-cursor.json"
    cursor.parent.mkdir(parents=True, exist_ok=True)
    with run_registry.exclusive_record(cursor, blocking=False):
        after = ""
        if cursor.is_symlink():
            raise RuntimeError(f"validation cleanup cursor is a symlink: {cursor}")
        if cursor.exists():
            try:
                value = json.loads(cursor.read_text())
            except (OSError, json.JSONDecodeError) as error:
                raise RuntimeError(f"cannot read validation cleanup cursor: {error}") from error
            if not isinstance(value, dict) or set(value) != {"after", "schema"} or type(value.get("schema")) is not int or value["schema"] != 1 or not isinstance(value.get("after"), str) or not value["after"]:
                raise RuntimeError("validation cleanup cursor has an invalid shape")
            after = value["after"]
        split = next((index for index, key in enumerate(keys) if key > after), 0)
        ordered = keys[split:] + keys[:split]
        chosen = ordered[:VALIDATE_CLEANUP_BATCH_LIMIT]
        descriptor, temporary = tempfile.mkstemp(prefix=f".{cursor.name}.", dir=cursor.parent)
        try:
            with os.fdopen(descriptor, "w") as stream:
                json.dump({"after": chosen[-1], "schema": 1}, stream, sort_keys=True)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, cursor)
            directory = os.open(cursor.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            with suppress(FileNotFoundError):
                os.unlink(temporary)
    return [groups[key] for key in chosen], [groups[key] for key in ordered[len(chosen):]]


def sweep_completed_checkouts(
    root: Path,
    *,
    run: Runner,
    tool_root: Path = ROOT,
    classify_ownerless_only: bool = False,
    prospective_slot: str | None = None,
    prospective_checkout: Path | None = None,
) -> dict[str, Any]:
    """Hand off scorecards and remove terminal checkouts left by interrupted waiters."""
    journal_scopes: tuple[CreateJournalScope, ...] = ()
    if classify_ownerless_only:
        if prospective_slot is None or prospective_checkout is None:
            raise ValueError(
                "read-only entry classification requires the prospective validation slot"
            )
        journal_scopes = classify_create_journal_scopes(
            root,
            prospective_slot=prospective_slot,
            prospective_checkout=prospective_checkout,
            run=run,
            tool_root=tool_root,
        )
    runs = root / "ignored" / "validate" / "runs"
    removed: list[str] = []
    removed_cargo_homes: list[str] = []
    scorecard_handoffs: list[str] = []
    retained: list[dict[str, Any]] = []
    deferred: list[dict[str, str]] = []
    bookkeeping_errors: list[dict[str, str]] = []
    if not runs.is_dir():
        # Absent is a real answer and it carries its own zeros, so a reader can
        # tell "there was nothing to look at" from "the counts are missing".
        return {
            "removed": removed,
            "removed_cargo_homes": removed_cargo_homes,
            "scorecard_handoffs": scorecard_handoffs,
            "retained": retained,
            "deferred": deferred,
            "bookkeeping_errors": bookkeeping_errors,
            "discovery": {
                "json_files_seen": 0,
                "framework_result_sidecars_skipped": 0,
                "run_handle_candidates": 0,
            },
        }

    groups, singles, _rows, discovery_errors, discovery = discover_cleanup_groups(
        root, runs
    )
    retained.extend(discovery_errors)

    for row in singles:
        observation = terminal_cleanup_unit(row.path, row.record, run=run)
        row.unit = observation.unit
        if (
            not classify_ownerless_only
            and (observation.reason is not None or row.unit is None)
        ):
            retained.append(
                {
                    "blocks_entry": observation.blocks_entry,
                    "checkout": str(row.record.get("checkout", "")),
                    "reason": observation.reason or "unit state unavailable",
                }
            )
            continue
        fields: dict[str, Any] = {}
        checkout, checkout_error = classify_recorded_checkout(root, row.record)
        protected_reason, protected_error = None, None
        if classify_ownerless_only and checkout is None and checkout_error is None:
            if row.record.get("producer") == run_registry.PRODUCER:
                protected_reason, protected_error = classify_retained_current_in_place_record(
                    root, row.record
                )
            elif is_in_place_scope_candidate(row.record):
                protected_reason, protected_error = classify_protected_in_place_record(
                    root, row.record, journal_scopes
                )
        if classify_ownerless_only and observation.reason is not None:
            # This exemption concerns untouched storage, not legitimate live
            # work under CleanupUnitObservation.blocks_entry's managed-group
            # contract. validate-lock still owns capacity admission. Preserve
            # the live/uncertain observation alongside the storage reason.
            retained.append(
                {
                    "blocks_entry": protected_reason is None,
                    "checkout": str(row.record.get("checkout", "")),
                    "reason": (
                        f"{observation.reason}; {protected_reason}"
                        if protected_reason is not None
                        else protected_error or observation.reason
                    ),
                }
            )
            row.unit = None
            continue
        if checkout_error is not None:
            item: dict[str, Any] = {
                "checkout": str(row.record.get("checkout", "")),
                "reason": checkout_error,
            }
            if classify_ownerless_only:
                item["blocks_entry"] = True
            retained.append(item)
            continue
        if checkout is None:
            if not classify_ownerless_only and observation.state is CleanupUnitState.TERMINAL:
                continue
            if protected_reason is not None:
                retained.append(
                    {
                        "blocks_entry": False,
                        "checkout": str(row.record.get("checkout", "")),
                        "reason": protected_reason,
                    }
                )
                row.unit = None
            else:
                retained.append(
                    {
                        "blocks_entry": True,
                        "checkout": str(row.record.get("checkout", "")),
                        "reason": protected_error
                        or observation.reason
                        or "in-place run record has no verified create-journal scope",
                    }
                )
            continue
        if observation.reason is not None or row.unit is None:
            retained.append(
                {
                    "blocks_entry": observation.blocks_entry,
                    "checkout": str(row.record.get("checkout", "")),
                    "reason": observation.reason or "unit state unavailable",
                }
            )
            continue
        if classify_ownerless_only:
            retained.append(
                {
                    "blocks_entry": True,
                    "checkout": str(checkout),
                    "reason": (
                        "terminal validation checkout remains retained; read-only "
                        "entry classification did not archive or remove it"
                    ),
                }
            )
            continue
        try:
            handoff = publish_recorded_scorecard_handoff(
                root,
                row.path,
                row.record,
                checkout,
                row.unit,
                run=run,
                tool_root=tool_root,
            )
            if handoff is not None:
                row.scorecard_handoff = str(handoff)
                scorecard_handoffs.append(str(handoff))
            cleaned, archived, reason = cleanup_recorded_checkout(
                root, row.record, record_path=row.path, unit=row.unit,
                run=run, tool_root=tool_root,
            )
        except (OSError, RuntimeError, ValueError) as error:
            retained.append({"checkout": str(row.record.get("checkout", "")), "reason": str(error)})
        else:
            if cleaned:
                removed.append(str(row.record.get("checkout")))
                fields.update(checkout_removed_at=datetime.now(timezone.utc).isoformat(), archived_orphaned_receipts=archived)
                if row.scorecard_handoff is not None:
                    fields["scorecard_handoff"] = row.scorecard_handoff
            if reason is not None:
                retained.append({"checkout": str(row.record.get("checkout", "")), "reason": reason})
        if fields:
            try:
                run_registry.update_record(row.path, blocking=False, **fields)
            except (OSError, RuntimeError, ValueError) as error:
                bookkeeping_errors.append({"record": str(row.path), "reason": str(error)})

    managed = {key: group for key, group in groups.items() if group.managed}
    try:
        selected, later = select_cleanup_groups(
            root, managed, persist_cursor=not classify_ownerless_only
        )
    except (OSError, RuntimeError, ValueError) as error:
        selected, later = [], list(managed.values())
        bookkeeping_errors.append({"record": str(root / "ignored/validate/checkout-cleanup-cursor.json"), "reason": str(error)})
    deferred.extend(
        {"checkout": str(group.checkout), "reason": "deferred by the bounded validation cleanup batch"}
        for group in later
    )
    ownerless_selected = [
        group
        for group in groups.values()
        if not group.managed and group.checkout.parent == root.resolve() / "ignored"
    ]
    frozen_selected = [
        group
        for group in groups.values()
        if not group.managed and group.checkout.parent != root.resolve() / "ignored"
    ]
    selected.extend(ownerless_selected[:VALIDATE_CLEANUP_BATCH_LIMIT])
    selected.extend(frozen_selected[:VALIDATE_CLEANUP_BATCH_LIMIT])
    deferred.extend(
        {
            "checkout": str(group.checkout),
            "reason": "deferred by the bounded ownerless validation cleanup batch",
        }
        for group in ownerless_selected[VALIDATE_CLEANUP_BATCH_LIMIT:]
    )
    deferred.extend(
        {
            "checkout": str(group.checkout),
            "reason": "deferred by the bounded frozen validation cleanup batch",
        }
        for group in frozen_selected[VALIDATE_CLEANUP_BATCH_LIMIT:]
    )

    prepared: list[CleanupGroup] = []
    for group in selected:
        if group.reasons:
            continue
        for hold in preflight_cleanup_group(
            root,
            group,
            run=run,
            read_only_ownerless=classify_ownerless_only,
        ):
            retained.append(
                {
                    "blocks_entry": hold.blocks_entry,
                    "checkout": str(group.checkout),
                    "reason": hold.reason,
                }
            )
        if group.reasons:
            continue
        if not classify_ownerless_only:
            for row in group.rows:
                try:
                    handoff = publish_recorded_scorecard_handoff(
                        root,
                        row.path,
                        row.record,
                        group.checkout,
                        row.unit or row.path.stem,
                        run=run,
                        tool_root=tool_root,
                    )
                    if handoff is not None:
                        row.scorecard_handoff = str(handoff)
                        scorecard_handoffs.append(str(handoff))
                    row.archived = archive_orphaned_receipts(
                        root,
                        group.checkout,
                        orphaned_receipt_locations(group.checkout, run=run),
                        unit=row.unit or row.path.stem,
                    )
                except (OSError, RuntimeError, ValueError) as error:
                    group.reasons.append(str(error))
                    retained.append(
                        {"checkout": str(group.checkout), "reason": str(error)}
                    )
        if not group.reasons:
            prepared.append(group)

    ownerless_groups = [
        item
        for item in prepared
        if not item.managed and item.checkout.parent == root.resolve() / "ignored"
    ]
    if ownerless_groups:
        ownerless_inputs: list[tuple[Path, Path, Path]] = []
        for group in ownerless_groups:
            row = group.rows[0]
            source = Path(
                str(
                    row.record.get(
                        "source_checkout",
                        root
                        / canonical_repo(
                            str(row.record.get("repo", "rrnewton/hermit"))
                        ).rsplit("/", 1)[-1],
                    )
                )
            )
            ownerless_inputs.append((group.checkout, row.path, source))
        outcomes = remove_ownerless_checkouts_batch(
            root,
            ownerless_inputs,
            run=run,
            tool_root=tool_root,
            classify_only=classify_ownerless_only,
        )
        for group in ownerless_groups:
            outcome = outcomes.get(str(group.checkout))
            if outcome is None:
                retained.append(
                    {
                        "blocks_entry": True,
                        "checkout": str(group.checkout),
                        "reason": "ownerless cleanup returned no result for this checkout",
                    }
                )
            elif outcome.reason is not None:
                retained.append(
                    {
                        "blocks_entry": outcome.blocks_entry,
                        "checkout": str(group.checkout),
                        "reason": outcome.reason,
                    }
                )
            else:
                removed.append(str(group.checkout))

    frozen_groups = [
        item
        for item in prepared
        if not item.managed and item.checkout.parent != root.resolve() / "ignored"
    ]
    if frozen_groups:
        frozen_inputs: list[tuple[Path, Path, Path]] = []
        for group in frozen_groups:
            row = group.rows[0]
            source = Path(
                str(
                    row.record.get(
                        "source_checkout",
                        root
                        / canonical_repo(
                            str(row.record.get("repo", "rrnewton/hermit"))
                        ).rsplit("/", 1)[-1],
                    )
                )
            )
            frozen_inputs.append((group.checkout, row.path, source))
        outcomes = remove_ownerless_checkouts_batch(
            root,
            frozen_inputs,
            run=run,
            tool_root=tool_root,
            frozen=True,
            classify_only=classify_ownerless_only,
        )
        for group in frozen_groups:
            outcome = outcomes.get(str(group.checkout))
            if outcome is None:
                retained.append(
                    {
                        "blocks_entry": True,
                        "checkout": str(group.checkout),
                        "reason": "frozen cleanup returned no result for this checkout",
                    }
                )
            elif outcome.reason is not None:
                retained.append(
                    {
                        "blocks_entry": outcome.blocks_entry,
                        "checkout": str(group.checkout),
                        "reason": outcome.reason,
                    }
                )
            else:
                removed.append(str(group.checkout))

    batch_groups = [group for group in prepared if group.managed and group.identity is not None]
    # ⚠️ AUTHORISATION, WHICH IS A DIFFERENT QUESTION FROM THE CLASSIFICATION
    # THAT SELECTED THESE. Everything above decides which trees this sweep
    # MANAGES. This decides whether each may actually be REMOVED. A tree can be
    # exactly the kind this sweep handles and still hold the only copy of
    # something, so conflating the two is how a mis-recorded kind becomes a
    # deletion. Measured while placing this: the same check at the five
    # classification sites broke 91 tests; here it breaks 15, and those 15 are
    # fixtures modelling a checkout shape production does not have.
    #
    # A refusal is reported as a retention carrying its reason, the way every
    # other non-removal here already is, rather than aborting -- one protected
    # tree must not stop its peers being cleaned.
    if batch_groups and not classify_ownerless_only:
        # ONLY THE AUTHORED-WORK HALF IS ASKED HERE, deliberately. The other
        # half -- is this tree a disposable validate slot -- is already settled
        # upstream: `batch_groups` is filtered to groups that are `managed` and
        # carry a wrkslots `identity`, which is what makes them validate slots.
        # Re-deriving the kind from the registry at this point would ask a
        # question already answered and would refuse any tree whose row the
        # caller had not written, which is a property of the caller rather than
        # of the tree.
        authorised = []
        for group in batch_groups:
            measured = tree_disposition.authored_work(group.checkout, run=run)
            if measured is None:
                retained.append(
                    {
                        "checkout": str(group.checkout),
                        "reason": "authored-work state could not be read",
                    }
                )
                continue
            dirty, unpushed = measured
            if dirty:
                retained.append(
                    {
                        "checkout": str(group.checkout),
                        "reason": f"holds {dirty} uncommitted path(s)",
                    }
                )
            elif unpushed:
                retained.append(
                    {
                        "checkout": str(group.checkout),
                        "reason": (
                            f"{unpushed} commit(s) reach no remote ref; preserve "
                            "them to a rescue ref before removing"
                        ),
                    }
                )
            else:
                authorised.append(group)
        batch_groups = authorised
    if batch_groups:
        if classify_ownerless_only:
            retained.extend(
                {
                    "blocks_entry": False,
                    "checkout": str(group.checkout),
                    "reason": (
                        "terminal managed validation remains retained; read-only "
                        "entry classification did not archive or remove it"
                    ),
                }
                for group in batch_groups
            )
        else:
            outcomes = remove_fresh_checkouts_batch(
                root,
                [(group.checkout, group.identity) for group in batch_groups],
                run=run,
                tool_root=tool_root,
            )
            for group in batch_groups:
                reason = outcomes.get(str(group.checkout))
                if reason is not None:
                    retained.append(
                        {"checkout": str(group.checkout), "reason": reason}
                    )
                else:
                    removed.append(str(group.checkout))

    for row in singles + [row for group in selected for row in group.rows]:
        if row.unit is None:
            continue
        cargo_home, cargo_error = classify_recorded_cargo_home(root, row.record)
        if cargo_error is not None:
            retained.append(
                {
                    "checkout": str(row.record.get("cargo_home", "")),
                    "reason": cargo_error,
                }
            )
            continue
        if cargo_home is None:
            continue
        if classify_ownerless_only:
            retained.append(
                {
                    "blocks_entry": False,
                    "checkout": str(cargo_home),
                    "reason": (
                        "terminal validation Cargo home remains retained; "
                        "read-only entry classification did not remove it"
                    ),
                }
            )
            continue
        try:
            cleaned, reason = cleanup_recorded_cargo_home(
                root, row.record, record_path=row.path, run=run, tool_root=tool_root
            )
        except (OSError, RuntimeError, ValueError) as error:
            retained.append({"checkout": str(row.record.get("cargo_home", "")), "reason": str(error)})
            continue
        if reason is not None:
            retained.append({"checkout": str(row.record.get("cargo_home", "")), "reason": reason})
        if not cleaned:
            continue
        removed_cargo_homes.append(str(row.record.get("cargo_home")))
        try:
            run_registry.update_record(
                row.path,
                blocking=False,
                cargo_home_removed_at=datetime.now(timezone.utc).isoformat(),
            )
        except (OSError, RuntimeError, ValueError) as error:
            bookkeeping_errors.append({"record": str(row.path), "reason": str(error)})

    removed_set = set(removed)
    for group in prepared:
        if str(group.checkout) not in removed_set:
            continue
        removed_at = datetime.now(timezone.utc).isoformat()
        for row in group.rows:
            try:
                run_registry.update_record(
                    row.path,
                    blocking=False,
                    checkout_removed_at=removed_at,
                    archived_orphaned_receipts=row.archived,
                    **(
                        {"scorecard_handoff": row.scorecard_handoff}
                        if row.scorecard_handoff is not None
                        else {}
                    ),
                )
            except (OSError, RuntimeError, ValueError) as error:
                bookkeeping_errors.append({"record": str(row.path), "reason": str(error)})
    return {
        "removed": removed,
        "removed_cargo_homes": removed_cargo_homes,
        "scorecard_handoffs": sorted(set(scorecard_handoffs)),
        "retained": retained,
        "deferred": deferred,
        "bookkeeping_errors": bookkeeping_errors,
        "discovery": discovery,
    }


class ValidateEntryCleanupRefused(RuntimeError):
    """A new disposable checkout cannot be created while cleanup is incomplete."""


def require_validate_entry_cleanup(
    root: Path,
    *,
    prospective_slot: str,
    prospective_checkout: Path,
    run: Runner,
    tool_root: Path = ROOT,
) -> None:
    """Run one bounded cleanup batch and refuse entry while old work remains.

    An active managed validation is not old work and does not block another
    admitted run.  A provider-classified pre-proof historical checkout also
    remains visible without blocking.  Every retained row without that explicit
    disposition, a bookkeeping error, or a deferred checkout prevents entry.
    """
    started = time.monotonic()
    try:
        report = sweep_completed_checkouts(
            root,
            run=run,
            tool_root=tool_root,
            classify_ownerless_only=True,
            prospective_slot=prospective_slot,
            prospective_checkout=prospective_checkout,
        )
    except (OSError, RuntimeError, ValueError) as error:
        elapsed = time.monotonic() - started
        raise ValidateEntryCleanupRefused(
            "validation checkout cleanup could not complete before entry "
            f"after {elapsed:.3f}s: {error}"
        ) from error
    elapsed = time.monotonic() - started
    retained = list(report["retained"])
    nonblocking = [row for row in retained if row.get("blocks_entry") is False]
    blocked = [row for row in retained if row.get("blocks_entry") is not False]
    deferred = list(report["deferred"])
    bookkeeping_errors = list(report["bookkeeping_errors"])
    removed = list(report["removed"])
    removed_cargo_homes = list(report["removed_cargo_homes"])

    if blocked or deferred or bookkeeping_errors:
        if blocked:
            first_kind = "retained"
            first = blocked[0]
        elif bookkeeping_errors:
            first_kind = "bookkeeping error"
            first = bookkeeping_errors[0]
        else:
            first_kind = "deferred"
            first = deferred[0]
        location = first.get("checkout") or first.get("record") or "(unknown)"
        reason = " ".join(str(first.get("reason", "unknown reason")).split())
        raise ValidateEntryCleanupRefused(
            "validation checkout cleanup is incomplete after "
            f"{elapsed:.3f}s: removed {len(removed)} checkout(s) and "
            f"{len(removed_cargo_homes)} Cargo home(s); waiting on "
            f"{len(deferred)} deferred checkout(s), {len(blocked)} retained "
            f"checkout(s), and {len(bookkeeping_errors)} bookkeeping error(s); "
            f"first {first_kind}: {location}: {reason}"
        )

    print(
        "validate-run: ENTRY-CLEANUP state=ready "
        f"removed={len(removed)} removed-cargo-homes={len(removed_cargo_homes)} "
        f"retained-nonblocking={len(nonblocking)} elapsed={elapsed:.3f}s",
        file=sys.stderr,
    )


# Seeded from the shared cache rather than started empty, and seeded with these
# two subtrees rather than all of it. Both choices are measured, not aesthetic;
# the numbers are in `test_private_cargo_home.py` and in the commit message.
CARGO_SEED_SUBTREES: tuple[tuple[str, str], ...] = (
    ("registry", "registry"),
    ("git/db", "git/db"),
)


def prepare_private_cargo_home(root: Path, *, run: Runner, shared: Path) -> Path:
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
    parent = private_cargo_home_parent(root)
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
    state_root: Path,
    target: str,
    cwd: Path,
    run_started_at: str,
    *,
    repo: str,
    run: Runner,
    tool_root: Path,
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

    Executables come from ``tool_root``. Their working directory remains the
    canonical ``state_root`` so the ledger, live spool, and receipt authority
    stay attached to the shared parent even when its Hermit checkout is absent.
    """
    handle_start = parse_utc_timestamp(run_started_at, role="validate-run handle")
    status = run(
        [
            str(tool_root / "ci-hub/ci-hub"),
            "validate-status",
            "--sha",
            target,
            "--repo",
            repo,
            "--json",
        ],
        cwd=state_root,
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
        [
            sys.executable,
            str(tool_root / "ci-hub/ledger/validate_rows.py"),
            "rows",
        ],
        cwd=state_root,
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
    def raise_walk_error(error: OSError) -> None:
        raise error

    for current, dirs, files in os.walk(fresh, onerror=raise_walk_error):
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


@dataclass(frozen=True)
class FreshCheckoutCleanup:
    locations: tuple[str, ...]
    archived: tuple[str, ...]
    removed: bool
    error: str | None = None


def cleanup_fresh_checkout_preserving_receipts(
    root: Path,
    source: Path,
    fresh: Path,
    *,
    unit: str,
    run: Runner,
    tool_root: Path,
    wrkslots: WrkslotsIdentity | None,
) -> FreshCheckoutCleanup:
    """Archive run-produced receipts before reclaiming a managed checkout."""

    try:
        locations = orphaned_receipt_locations(fresh, run=run)
    except OSError as error:
        return FreshCheckoutCleanup((), (), False, f"receipt scan failed: {error}")
    try:
        archived = archive_orphaned_receipts(root, fresh, locations, unit=unit)
    except (OSError, RuntimeError) as error:
        return FreshCheckoutCleanup(
            tuple(locations), (), False, f"receipt archive failed: {error}"
        )
    removed = remove_fresh_checkout(
        source,
        fresh,
        run=run,
        tool_root=tool_root,
        wrkslots=wrkslots,
    )
    return FreshCheckoutCleanup(
        tuple(locations), tuple(archived), removed, None
    )


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
    materialize_target: bool = False,
) -> Path:
    checkout = require_guest_visible_root(checkout, role="source checkout")
    if not SHA_RE.fullmatch(target):
        raise ValueError("--target must be an exact lowercase 40-hex commit SHA")
    top = Path(
        checked_output(
            ["git", "-C", str(checkout), "rev-parse", "--show-toplevel"],
            run=run,
            purpose="cannot resolve checkout root",
        )
    ).resolve()
    if top != checkout:
        raise ValueError(
            f"--checkout must name the repository root ({top}), not {checkout}"
        )

    if materialize_target:
        resolved_target = checked_output(
            ["git", "-C", str(checkout), "rev-parse", f"{target}^{{commit}}"],
            run=run,
            purpose="cannot resolve requested target in source repository",
        )
        if resolved_target != target:
            raise ValueError(
                f"requested target resolved to {resolved_target}, not exact commit {target}"
            )
        # This checkout supplies Git objects only. The runnable tree is a fresh,
        # registered checkout of `target`, and `preflight` validates that tree
        # before admission. Requiring this unrelated worktree's HEAD, driver
        # bytes, or cleanliness would make a deterministic target depend on
        # whichever edits its owner happens to have in progress.
        return checkout

    driver = "scripts/validate.rs" if repo == "rrnewton/hermit" else "validate.sh"
    if not (checkout / driver).is_file():
        raise ValueError(f"missing {driver} in checkout {checkout}")
    head = checked_output(
        ["git", "-C", str(checkout), "rev-parse", "HEAD^{commit}"],
        run=run,
        purpose="cannot resolve checkout HEAD",
    )
    if head != target:
        raise ValueError(
            f"checkout HEAD is {head}, not requested exact target {target}"
        )

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
        raise ValueError(
            f"checkout is dirty ({first}); refusing unrepeatable validation"
        )
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
    state_root: Path | None = None,
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
    max_validates: int | None = None,
    no_wait: bool = False,
    skip_if_recorded: bool = False,
    repo: str = "rrnewton/hermit",
    frozen_validate: bool = False,
    frozen_result: Path | None = None,
    framework_result: Path | None = None,
    unit_tool_prefix: Sequence[str] | None = None,
) -> list[str]:
    state_root = root if state_root is None else state_root
    child_deadline = effective_child_deadline(repo, child_deadline)
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
        if framework_result is not None:
            build_budget_env.extend(
                ["--setenv", f"VALIDATE_SERVICE_RESULT_PATH={framework_result}"]
            )
    # Last, so it cannot be shadowed by a caller-supplied CARGO_HOME: the whole
    # point is that this run does not share a package-cache lock with the box.
    if cargo_home is not None:
        build_budget_env.extend(["--setenv", f"CARGO_HOME={cargo_home}"])
    if runtime_root is not None:
        build_budget_env.extend(
            [
                "--setenv",
                f"TMPDIR={runtime_root}",
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
        e2e_result_root, runner_log_dir = _validate_output_paths(state_root, unit)
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
                # Retain raw scheduler counters outside the disposable checkout.
                # Older success outcomes erase OOM counts that profiles preserve.
                "--setenv",
                f"RUN_NODE_PERF_DIR={runner_log_dir.parent / 'dagrun-profiles'}",
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
                f"{INNER_FRESHNESS_SKIP_ENV}=1",
                "--setenv",
                "VALIDATE_IGNORE_CACHE=1",
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
        child.append(f"{INNER_FRESHNESS_SKIP_ENV}=1")

    def tool_python(relative: str, *arguments: str) -> list[str]:
        if unit_tool_prefix is None:
            return [sys.executable, str(root / relative), *arguments]
        # The nested operational-tool chooses and retains the unit's private
        # root only after systemd starts it.  Expand that newly-owned root at
        # child execution time; never bake the launcher's expiring /proc path
        # into validate-lock's deferred payload.
        return [
            "/bin/sh",
            "-c",
            f'exec "$0" "$DEV_HERMIT_TOOL_ROOT/{relative}" "$@"',
            sys.executable,
            *arguments,
        ]

    if repo == "rrnewton/reverie":
        if unit_tool_prefix is None:
            child.extend(
                tool_python(
                    "ci-hub/validate/reverie_safe_ci.py",
                    "--root",
                    str(root),
                    "--checkout",
                    str(checkout),
                    "--target",
                    target,
                    "--log",
                    str(log),
                )
            )
        else:
            child.extend(
                [
                    "/bin/sh",
                    "-c",
                    'exec "$0" '
                    '"$DEV_HERMIT_TOOL_ROOT/ci-hub/validate/reverie_safe_ci.py" '
                    '--root "$DEV_HERMIT_TOOL_ROOT" "$@"',
                    sys.executable,
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
            tool_python("ci-hub/validate/run_with_validate_deadline.py")
        )
        if make_validate:
            child.extend(["with-proxy", "make", "validate"])
        elif frozen_validate and unit_tool_prefix is None:
            # The target may predate frozen-holder admission. Run the current
            # validation driver against the historical checkout so the lock
            # boundary is fixed once in current tooling rather than copied into
            # every historical commit that the feature exists to measure.
            child.extend(
                [
                    "with-proxy",
                    str(root / "hermit/scripts/validate.rs"),
                    *(validate_args or ["full"]),
                ]
            )
        elif frozen_validate:
            child.extend(
                [
                    "/bin/sh",
                    "-c",
                    'exec with-proxy "$DEV_HERMIT_TOOL_ROOT/hermit/scripts/validate.rs" "$@"',
                    "frozen-validate",
                    *(validate_args or ["full"]),
                ]
            )
        else:
            child.extend(
                ["with-proxy", "./scripts/validate.rs", *(validate_args or ["full"])]
            )

    tool_command = (
        [*unit_tool_prefix]
        if unit_tool_prefix is not None
        else [str(root / "ci-hub/ci-hub")]
    )
    tool_root_environment = (
        []
        if unit_tool_prefix is not None
        else ["--setenv", f"DEV_HERMIT_TOOL_ROOT={root}"]
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
        # State remains anchored in the canonical project root while every
        # executable helper comes from this exact tooling checkout. Conflating
        # the two made a clean linked launcher execute stale primary code.
        "--setenv",
        f"DEV_HERMIT_PARENT={state_root}",
        *tool_root_environment,
        "--property",
        f"StandardOutput=append:{log}",
        "--property",
        f"StandardError=append:{log}",
        *tool_command,
        "validate-lock",
        "run",
        "--agent",
        agent,
        "--kind",
        validation_kind(repo, frozen_validate=frozen_validate),
        "--target",
        target,
        "--run-record",
        str(record),
        *(["--no-wait"] if no_wait else []),
        *(["--skip-if-recorded"] if skip_if_recorded else []),
        "--wait",
        str(wait),
        "--hold",
        str(hold),
        "--child-deadline",
        str(child_deadline),
        *([] if max_validates is None else ["--max", str(max_validates)]),
        "--",
        *child,
    ]


def service_properties_with_absence(
    unit: str, *, run: Runner
) -> tuple[dict[str, str] | None, bool]:
    """Return loaded properties and whether systemd positively reports no unit."""

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
        return None, False
    properties = dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )
    # `systemctl show` succeeds even after --collect has unloaded the unit.  Its
    # synthetic not-found object says success/0 and is not execution evidence.
    if properties.get("LoadState") != "loaded" or not properties.get("InvocationID"):
        return (
            None,
            properties.get("LoadState") == "not-found"
            and properties.get("ActiveState") == "inactive"
            and not properties.get("InvocationID"),
        )
    return properties, False


def service_properties(
    unit: str, *, run: Runner
) -> dict[str, str] | None:
    properties, _absent = service_properties_with_absence(unit, run=run)
    return properties


def cleanup_record_allows_removal_attempt(
    record: Mapping[str, Any], *, unit_absent: bool
) -> bool:
    """Allow the downstream liveness-checked removal without changing a verdict.

    Refused admission proves that no validation payload started, but the
    validate-lock supervisor may still be returning.  The wrkslots removal
    path remains responsible for proving that no process uses the target.
    """

    try:
        handle = run_registry.parse_current_record(record)
    except RuntimeError:
        return False
    if handle.state is not run_registry.RunState.UNKNOWN:
        return False
    admission = handle.admission_result
    if admission is None:
        return False
    if admission.state is run_registry.AdmissionState.REFUSED:
        return True
    return unit_absent and admission.state is run_registry.AdmissionState.ADMITTED


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
            classified = service_result.from_run_record(record, durable, log)
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
                **service_result.from_run_record(
                    record, durable, log, properties=properties
                ),
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
    if event == "SWEEP-COMPLETED":
        removed = report.get("removed", [])
        removed_cargo_homes = report.get("removed_cargo_homes", [])
        retained = report.get("retained", [])
        deferred = report.get("deferred", [])
        bookkeeping_errors = report.get("bookkeeping_errors", [])
        print(
            "validate-run: SWEEP-COMPLETED "
            f"removed={len(removed)} "
            f"removed-cargo-homes={len(removed_cargo_homes)} "
            f"retained={len(retained)} "
            f"deferred={len(deferred)} "
            f"bookkeeping-errors={len(bookkeeping_errors)}",
            flush=True,
        )
        for row in retained:
            print(
                f"RETAINED {row.get('checkout', '')}: {row.get('reason', '')}",
                flush=True,
            )
        for row in bookkeeping_errors:
            print(
                f"BOOKKEEPING-ERROR {row.get('record', '')}: {row.get('reason', '')}",
                flush=True,
            )
        return
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


@dataclass(frozen=True)
class HeldScorecardInvocationLock:
    """The held validation directory and regular invocation-lock inode."""

    path: Path
    directory_fd: int
    directory_identity: tuple[int, int]
    descriptor: int
    lock_identity: tuple[int, int]

    def verify(self) -> None:
        """Refuse changed names; call immediately before publishing outputs."""
        try:
            held_directory = os.fstat(self.directory_fd)
            named_directory = os.lstat(self.path.parent)
            held_lock = os.fstat(self.descriptor)
            named_lock = os.stat(
                self.path.name, dir_fd=self.directory_fd, follow_symlinks=False
            )
        except OSError as error:
            raise ScorecardInvocationLockRefused(
                f"validation invocation lock identity is unavailable at {self.path}: {error}"
            ) from error
        if any(
            not stat.S_ISDIR(item.st_mode)
            or (item.st_dev, item.st_ino) != self.directory_identity
            for item in (held_directory, named_directory)
        ):
            raise ScorecardInvocationLockRefused(
                f"validation invocation lock directory changed at {self.path.parent}"
            )
        if any(
            not stat.S_ISREG(item.st_mode)
            or (item.st_dev, item.st_ino) != self.lock_identity
            for item in (held_lock, named_lock)
        ):
            raise ScorecardInvocationLockRefused(
                f"validation invocation lock pathname changed at {self.path}"
            )


@contextmanager
def scorecard_invocation_lock(
    source_checkout: Path,
    *,
    timeout_seconds: float,
    poll_seconds: float = 0.05,
):
    """Bound scorecard write-back on Hermit's per-checkout validate lock.

    This is the same kernel ``flock`` domain used by ``scripts/validate.rs``.
    A parent-side writer must take it before reading its snapshot inputs and
    hold it through replacement of both generated files.  Contention is a
    refusal after a finite deadline, never an unbounded wait and never
    permission to write while a qualifying validation is in flight.

    The yielded object binds the directory and regular lock inode. Call its
    ``verify()`` immediately before publishing outputs. Verification at context
    exit also detects changes, but cannot undo an already published output.

    The helper is additive in this slice. The existing writer is deliberately
    not switched until Hermit's combined two-file transaction is available.
    """
    if (
        not isinstance(timeout_seconds, (int, float))
        or isinstance(timeout_seconds, bool)
        or not math.isfinite(timeout_seconds)
        or timeout_seconds < 0
    ):
        raise ValueError("scorecard invocation lock timeout must be finite and nonnegative")
    if (
        not isinstance(poll_seconds, (int, float))
        or isinstance(poll_seconds, bool)
        or not math.isfinite(poll_seconds)
        or poll_seconds <= 0
    ):
        raise ValueError("scorecard invocation lock poll interval must be finite and positive")

    source_checkout = source_checkout.resolve()
    if not source_checkout.is_dir():
        raise ScorecardInvocationLockRefused(
            f"source checkout is unavailable: {source_checkout}"
        )
    lock_path = source_checkout / SCORECARD_INVOCATION_LOCK_RELATIVE
    directory_fd = -1
    descriptor = -1
    acquired = False
    try:
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
        if hasattr(os, "O_NOFOLLOW"):
            directory_flags |= os.O_NOFOLLOW
        directory_fd = os.open(lock_path.parent, directory_flags)
        directory_status = os.fstat(directory_fd)
        flags = os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NONBLOCK
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(lock_path.name, flags, 0o644, dir_fd=directory_fd)
        lock_status = os.fstat(descriptor)
        if not stat.S_ISREG(lock_status.st_mode):
            raise ScorecardInvocationLockRefused(
                f"validation invocation lock is not a regular file: {lock_path}"
            )
        held = HeldScorecardInvocationLock(
            path=lock_path,
            directory_fd=directory_fd,
            directory_identity=(directory_status.st_dev, directory_status.st_ino),
            descriptor=descriptor,
            lock_identity=(lock_status.st_dev, lock_status.st_ino),
        )
        held.verify()
    except OSError as error:
        if descriptor >= 0:
            os.close(descriptor)
        if directory_fd >= 0:
            os.close(directory_fd)
        raise ScorecardInvocationLockRefused(
            f"cannot open source checkout validation invocation lock {lock_path}: {error}"
        ) from error
    except BaseException:
        if descriptor >= 0:
            os.close(descriptor)
        if directory_fd >= 0:
            os.close(directory_fd)
        raise

    started = time.monotonic()
    deadline = started + float(timeout_seconds)
    try:
        while True:
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                acquired = True
                break
            except BlockingIOError:
                now = time.monotonic()
                if now >= deadline:
                    elapsed = now - started
                    raise ScorecardInvocationLockRefused(
                        "source checkout validation invocation lock remained held for "
                        f"{elapsed:.3f}s at {lock_path}; scorecard snapshot and "
                        "two-file write-back did not run"
                    )
                time.sleep(min(float(poll_seconds), deadline - now))
            except OSError as error:
                raise ScorecardInvocationLockRefused(
                    f"cannot lock source checkout validation invocation lock {lock_path}: "
                    f"{error}"
                ) from error
        held.verify()
        yield held
        held.verify()
    finally:
        try:
            if acquired:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            try:
                os.close(descriptor)
            finally:
                os.close(directory_fd)


def write_scorecard_from_results(
    source_checkout: Path,
    target: str,
    results: Path,
    *,
    run: Runner,
    json_output: bool,
) -> list[dict[str, object]] | None:
    """Run the locked writer and capture the exact generated file identities."""
    source_checkout = source_checkout.resolve()
    review_command = shlex.join(
        ["git", "-C", str(source_checkout), "diff", "--", *SCORECARD_PATHS]
    )
    output = sys.stderr if json_output else sys.stdout

    def failed(reason: str) -> None:
        print(
            "validate-run: compatibility scorecard NOT UPDATED: "
            f"{reason}. The established validate verdict is unchanged.",
            file=sys.stderr,
        )
        print(f"validate-run: review with: {review_command}", file=sys.stderr)
        return None

    try:
        head = checked_output(
            ["git", "-C", str(source_checkout), "rev-parse", "HEAD^{commit}"],
            run=run,
            purpose="cannot resolve source checkout HEAD before updating the compatibility scorecard",
        )
        if head != target:
            return failed(
                f"source checkout HEAD moved from validated target {target} to {head}"
            )

        tool = source_checkout / "ci/compat-envelope/scorecard.rs"
        if not tool.is_file():
            return failed(f"missing {tool}")

        result = run(
            [str(tool), "observe-results", "--results", str(results.resolve())],
            cwd=source_checkout,
            check=False,
        )
        if result.stdout:
            print(result.stdout, end="" if result.stdout.endswith("\n") else "\n", file=output)
        if result.stderr:
            print(
                result.stderr,
                end="" if result.stderr.endswith("\n") else "\n",
                file=sys.stderr,
            )
        if result.returncode != 0:
            # CARRY THE REASON, NOT ONLY THE NUMBER. scorecard.rs maps EVERY
            # Err to exit 2, so the status names no condition; the condition is
            # only ever on stderr. Recording the code alone produced nine
            # durable writeback failures all reading "refused with exit status:
            # 2", and diagnosing the actual cause required reading the run log
            # -- which is not a step the cleanup that consumes this record can
            # take.
            #
            # ⚠️ RECORD THE TOOL'S OWN STDERR, WHOLE, RATHER THAN A LINE
            # SELECTED OUT OF IT. An earlier version of this kept only the LAST
            # line, which is wrong for the 12 of scorecard.rs's 201 error
            # strings shaped "unknown <x> option `{arg}`\n\n{USAGE}": the
            # sentence is FIRST and a 45-line usage block follows, so the last
            # line records usage text as the reason -- plausible-looking and
            # wrong, which is worse than the bare number it replaced. No
            # selection is needed anyway: the tool has exactly ONE eprintln!,
            # at scorecard.rs main(), and every progress line goes to stdout,
            # so its stderr IS the refusal and nothing else. Recording it whole
            # also keeps this the SAME sentence the program emits rather than a
            # second description that can drift from it.
            #
            # Bounded because those usage blocks are long, and truncation is
            # stated rather than silent so a reader knows to go to the log.
            reason = (result.stderr or "").strip()
            if len(reason) > SCORECARD_REASON_LIMIT:
                reason = (
                    reason[:SCORECARD_REASON_LIMIT]
                    + f" [truncated at {SCORECARD_REASON_LIMIT} characters; full text on stderr]"
                )
            return failed(
                f"observe-results exited {result.returncode}"
                + (f": {reason}" if reason else "")
            )
        identities, _files = _scorecard_source_files(source_checkout)
        print(f"validate-run: review with: {review_command}", file=output)
        return identities
    except Exception as error:
        # The verdict exists already. Even an unexpected local filesystem or
        # process-launch error must leave that result as the caller's exit code.
        return failed(str(error))


def scorecard_handoff_path(root: Path, unit: str) -> Path:
    """Return the durable, caller-state-root-owned scorecard handoff path."""
    return (
        root.resolve()
        / "ignored"
        / "validate"
        / "scorecard-writebacks"
        / sanitize_unit(unit)
    )


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _scorecard_source_files(
    checkout: Path,
) -> tuple[list[dict[str, object]], dict[str, bytes]]:
    """Read the generated pair once and return its canonical byte identities."""

    checkout = checkout.resolve()
    files: dict[str, bytes] = {}
    identities: list[dict[str, object]] = []
    for relative in SCORECARD_PATHS:
        source = checkout / relative
        if source.is_symlink() or not source.is_file():
            raise RuntimeError(f"scorecard handoff source is not a regular file: {source}")
        try:
            source.resolve().relative_to(checkout)
        except ValueError as error:
            raise RuntimeError(f"scorecard handoff source escapes checkout: {source}") from error
        payload = source.read_bytes()
        files[relative] = payload
        identities.append(
            {
                "path": relative,
                "sha256": hashlib.sha256(payload).hexdigest(),
                "size": len(payload),
            }
        )
    return identities, files


def scorecard_writeback_files(checkout: Path) -> list[dict[str, object]]:
    """Capture the generated pair immediately after the scorecard writer exits."""

    identities, _files = _scorecard_source_files(checkout)
    return identities


def _scorecard_handoff_payload(
    checkout: Path,
    *,
    target: str,
    unit: str,
    writeback_completed: bool,
    expected_files: object,
) -> tuple[dict[str, Any], dict[str, bytes]]:
    recorded = run_registry.read_scorecard_writeback_files(expected_files)
    manifest_files, files = _scorecard_source_files(checkout)
    if manifest_files != recorded:
        raise RuntimeError(
            "scorecard handoff source bytes differ from the writer-recorded identities"
        )
    return (
        {
            "schema_version": SCORECARD_HANDOFF_SCHEMA_VERSION,
            "target": target,
            "unit": sanitize_unit(unit),
            "writeback_completed": writeback_completed,
            "files": manifest_files,
        },
        files,
    )


def require_live_scorecard_checkout(
    root: Path,
    checkout: Path,
    target: str,
    wrkslots: WrkslotsIdentity,
    *,
    run: Runner,
    tool_root: Path,
) -> None:
    """Prove the handoff source is still the exact registered validation checkout."""

    checkout = checkout.resolve()
    head = checked_output(
        ["git", "--no-replace-objects", "-C", str(checkout), "rev-parse", "HEAD^{commit}"],
        run=run,
        purpose="cannot resolve scorecard handoff checkout HEAD",
    )
    if head != target:
        raise RuntimeError(
            f"scorecard handoff checkout HEAD is {head}, expected validated target {target}"
        )
    result = run(
        [
            str(tool_root / "ci-hub/bin/wrkslots"),
            "--project-root",
            str(root.resolve()),
            "--allow-existing-unregistered-worktrees",
            "status",
            "--slot",
            wrkslots.slot,
            "--format",
            "json",
        ],
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"cannot verify live wrkslots registration: {detail}")
    try:
        status = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"wrkslots status emitted invalid JSON: {error}") from error
    active = status.get("active") if isinstance(status, dict) else None
    row = active[0] if isinstance(active, list) and len(active) == 1 else None
    checkouts = row.get("checkouts") if isinstance(row, dict) else None
    registered = checkouts[0] if isinstance(checkouts, list) and len(checkouts) == 1 else None
    raw_path = registered.get("path") if isinstance(registered, dict) else None
    valid_path = False
    if isinstance(raw_path, str) and raw_path:
        candidate = Path(raw_path)
        if not candidate.is_absolute() and ".." not in candidate.parts:
            valid_path = (root.resolve() / candidate).resolve() == checkout
    valid = (
        isinstance(status, dict)
        and status.get("schema") == 2
        and status.get("project_root") == str(root.resolve())
        and isinstance(row, dict)
        and row.get("slot") == wrkslots.slot
        and row.get("slot_type") == "validate"
        and type(row.get("generation")) is int
        and row.get("generation") == wrkslots.generation
        and row.get("storage_inconsistencies") == []
        and isinstance(registered, dict)
        and registered.get("name") == "checkout"
        and registered.get("head") == target
        and valid_path
    )
    if not valid:
        raise RuntimeError(
            "scorecard handoff checkout no longer matches its recorded live wrkslots "
            f"slot {wrkslots.slot} generation {wrkslots.generation}"
        )


def _verify_scorecard_handoff(
    destination: Path,
    manifest: Mapping[str, Any],
    files: Mapping[str, bytes],
) -> None:
    if destination.is_symlink() or not destination.is_dir():
        raise RuntimeError(f"scorecard handoff is not a regular directory: {destination}")
    manifest_path = destination / "handoff.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise RuntimeError(f"scorecard handoff manifest is not a regular file: {manifest_path}")
    try:
        observed = json.loads(manifest_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read scorecard handoff manifest {manifest_path}: {error}") from error
    if observed != dict(manifest):
        raise RuntimeError(f"scorecard handoff manifest conflicts with {destination}")
    expected_paths = {"handoff.json", *files}
    observed_paths = {
        path.relative_to(destination).as_posix()
        for path in destination.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    if observed_paths != expected_paths:
        raise RuntimeError(
            f"scorecard handoff file set conflicts with {destination}: "
            f"expected {sorted(expected_paths)}, got {sorted(observed_paths)}"
        )
    for relative, payload in files.items():
        path = destination / relative
        if path.is_symlink() or not path.is_file() or path.read_bytes() != payload:
            raise RuntimeError(f"scorecard handoff content conflicts with {path}")


def read_scorecard_handoff(
    root: Path,
    recorded: object,
    target: str,
    unit: str,
) -> tuple[Path, bool]:
    """Verify one already-published handoff without its retired source checkout."""
    destination = scorecard_handoff_path(root, unit)
    if not isinstance(recorded, str) or Path(recorded).resolve() != destination:
        raise RuntimeError(
            "recorded scorecard handoff does not match the canonical state-root path"
        )
    if destination.is_symlink() or not destination.is_dir():
        raise RuntimeError(f"scorecard handoff is not a regular directory: {destination}")
    manifest_path = destination / "handoff.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise RuntimeError(f"scorecard handoff manifest is not a regular file: {manifest_path}")
    try:
        manifest = json.loads(manifest_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read scorecard handoff manifest {manifest_path}: {error}") from error
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema_version",
        "target",
        "unit",
        "writeback_completed",
        "files",
    }:
        raise RuntimeError(f"scorecard handoff manifest has an invalid shape: {manifest_path}")
    if (
        manifest.get("schema_version") != SCORECARD_HANDOFF_SCHEMA_VERSION
        or manifest.get("target") != target
        or manifest.get("unit") != sanitize_unit(unit)
        or not isinstance(manifest.get("writeback_completed"), bool)
    ):
        raise RuntimeError(f"scorecard handoff manifest has the wrong identity: {manifest_path}")
    rows = manifest.get("files")
    if not isinstance(rows, list) or len(rows) != len(SCORECARD_PATHS):
        raise RuntimeError(f"scorecard handoff manifest has an invalid file list: {manifest_path}")
    expected_paths = {"handoff.json", *SCORECARD_PATHS}
    observed_paths = {
        path.relative_to(destination).as_posix()
        for path in destination.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    if observed_paths != expected_paths:
        raise RuntimeError(f"scorecard handoff has an invalid file set: {destination}")
    by_path = {
        row.get("path"): row
        for row in rows
        if isinstance(row, dict) and set(row) == {"path", "sha256", "size"}
    }
    if set(by_path) != set(SCORECARD_PATHS):
        raise RuntimeError(f"scorecard handoff manifest has invalid file identities: {manifest_path}")
    for relative in SCORECARD_PATHS:
        row = by_path[relative]
        path = destination / relative
        if path.is_symlink() or not path.is_file():
            raise RuntimeError(f"scorecard handoff output is not a regular file: {path}")
        payload = path.read_bytes()
        if (
            row.get("size") != len(payload)
            or row.get("sha256") != hashlib.sha256(payload).hexdigest()
        ):
            raise RuntimeError(f"scorecard handoff output failed its digest: {path}")
    return destination, bool(manifest["writeback_completed"])


def publish_scorecard_handoff(
    root: Path,
    checkout: Path,
    target: str,
    unit: str,
    *,
    writeback_completed: bool,
    expected_files: object,
    wrkslots: WrkslotsIdentity,
    run: Runner,
    tool_root: Path,
) -> Path:
    """Atomically publish generated scorecard files outside a disposable checkout."""
    if SHA_RE.fullmatch(target) is None:
        raise RuntimeError(f"scorecard handoff target is not an exact commit: {target!r}")
    unit = sanitize_unit(unit)
    destination = scorecard_handoff_path(root, unit)
    parent = destination.parent
    parent.mkdir(parents=True, exist_ok=True)
    if parent.is_symlink() or parent.resolve() != parent:
        raise RuntimeError(f"scorecard handoff parent is not canonical: {parent}")
    require_live_scorecard_checkout(
        root,
        checkout,
        target,
        wrkslots,
        run=run,
        tool_root=tool_root,
    )
    manifest, files = _scorecard_handoff_payload(
        checkout,
        target=target,
        unit=unit,
        writeback_completed=writeback_completed,
        expected_files=expected_files,
    )
    if destination.exists() or destination.is_symlink():
        _verify_scorecard_handoff(destination, manifest, files)
        return destination

    temporary = Path(tempfile.mkdtemp(prefix=f".{unit}.", dir=parent))
    try:
        for relative, payload in files.items():
            path = temporary / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as stream:
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
        manifest_path = temporary / "handoff.json"
        with manifest_path.open("x") as stream:
            json.dump(manifest, stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        directories = [path for path in temporary.rglob("*") if path.is_dir()]
        for directory in sorted(directories, key=lambda path: len(path.parts), reverse=True):
            _fsync_directory(directory)
        _fsync_directory(temporary)
        try:
            os.rename(temporary, destination)
        except FileExistsError:
            _verify_scorecard_handoff(destination, manifest, files)
        _fsync_directory(parent)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)
    _verify_scorecard_handoff(destination, manifest, files)
    return destination


def publish_recorded_scorecard_handoff(
    root: Path,
    record_path: Path,
    record: Mapping[str, Any],
    checkout: Path,
    unit: str,
    *,
    run: Runner,
    tool_root: Path,
) -> Path | None:
    """Preserve or verify a materialized Hermit scorecard before cleanup.

    An inactive service is not enough: the waiter may not yet have copied the
    service result into the durable run handle.  Cleanup owns the checkout only
    after that handle records the writer's typed completion. In particular, an
    absent writeback is pending, not ``False``.

    A historical result deliberately remains ``unknown`` because it lacks
    current validation authority. That is independent from disposal: an exact,
    verified handoff whose manifest and durable record both say writeback
    completed is sufficient to preserve the generated files before cleanup.
    """
    if (
        record.get("materialized_target") is not True
        or canonical_repo(str(record.get("repo", "rrnewton/hermit")))
        != "rrnewton/hermit"
    ):
        return None
    target = record.get("target")
    if not isinstance(target, str):
        raise RuntimeError("materialized scorecard handoff has no target commit")
    destination = scorecard_handoff_path(root, unit)
    recorded = record.get("scorecard_handoff")
    if recorded is not None and (
        not isinstance(recorded, str) or Path(recorded).resolve() != destination
    ):
        raise RuntimeError(
            "recorded scorecard handoff does not match the canonical state-root path"
        )
    state = record.get("state")
    historical_disposal = state == "unknown" and record.get(
        "service_result_schema"
    ) in {
        service_result.WRITEBACK_SCHEMA_VERSION,
        service_result.SELECTION_SCHEMA_VERSION,
    }
    if historical_disposal:
        if recorded is None:
            raise RuntimeError(
                "historical materialized scorecard cleanup requires an "
                "already-published verified scorecard handoff"
            )
        schema_path: Path | None
        if checkout.is_dir():
            # Before retirement, bind the sidecar to the exact producer schema.
            # This is the last point at which that disposable file is expected
            # to exist, and cleanup must not proceed unless it validates.
            schema_path = checkout / service_result.SCHEMA_RELATIVE_PATH
        else:
            removed_at = record.get("checkout_removed_at")
            if not isinstance(removed_at, str) or not removed_at.strip():
                raise RuntimeError(
                    "historical materialized scorecard checkout is absent without "
                    "a durable checkout_removed_at record"
                )
            # The sweep already validated the producer schema before recording
            # removal. Re-attachment now reads the durable sidecar under this
            # consumer's exact versioned parser; it cannot reopen a deleted file.
            schema_path = None
        evidence = service_result.read_framework_result(
            service_result.result_path(record_path),
            expected_commit=target,
            schema_path=schema_path,
        )
        recorded_schema = record.get("service_result_schema")
        if evidence.service_result_schema != recorded_schema:
            raise RuntimeError(
                "historical materialized scorecard service result does not match "
                f"the durable run handle: record declares {recorded_schema!r}, "
                f"result carries {evidence.service_result_schema!r}"
            )
        if evidence.scorecard_writeback != {"status": "completed"}:
            raise RuntimeError(
                "historical materialized scorecard cleanup requires the readable "
                "service result to record completed writeback; got "
                f"{evidence.scorecard_writeback!r}"
            )
    try:
        writeback = service_result.read_scorecard_writeback_value(
            record.get("scorecard_writeback")
        )
    except RuntimeError as error:
        raise RuntimeError(
            "materialized scorecard cleanup requires a typed completed "
            f"scorecard_writeback in the durable run handle: {error}"
        ) from error
    if state != "completed" and not historical_disposal:
        raise RuntimeError(
            "materialized scorecard cleanup requires a completed "
            "scorecard_writeback in a completed durable run handle, or an unknown "
            "historical run with a verified handoff; got "
            f"state={state!r}"
        )
    if historical_disposal:
        handoff, handoff_writeback_completed = read_scorecard_handoff(
            root, recorded, target, unit
        )
        if writeback != {"status": "completed"} or not handoff_writeback_completed:
            raise RuntimeError(
                "materialized scorecard cleanup requires completed writeback in "
                "both the durable run handle and verified scorecard handoff; got "
                f"scorecard_writeback={writeback!r}, "
                f"handoff_writeback_completed={handoff_writeback_completed!r}"
            )
        return handoff
    if writeback != {"status": "completed"}:
        raise RuntimeError(
            # THE REMEDY MUST NAME SOMETHING A LATER READER CAN DO. The
            # previous text said to reattach so the waiter could consume a
            # current service result. That names an actor from a run that has
            # already finished: when the agent that started the run is gone --
            # which is the usual case by the time cleanup runs -- there is
            # nothing to reattach to, and the reader is sent after a process
            # that no longer exists. The writeback does not need the original
            # waiter. Its input is the retained results artifact, which lives
            # OUTSIDE the checkout under ignored/validate/artifacts/, so any
            # later participant can re-drive it once the underlying refusal is
            # fixed.
            # ⚠️ TWO THINGS THE PREVIOUS TWO VERSIONS OF THIS TEXT EACH GOT
            # WRONG, BOTH BY NAMING A REMEDY THE READER CANNOT CARRY OUT.
            #
            # The original said to reattach so the waiter could consume a
            # current service result. That names an actor from a run that has
            # already finished.
            #
            # Its replacement, mine, said to re-drive from the retained results
            # and stopped there. Measured: observe-results binds the results to
            # the commit that produced them, so re-driving from a current
            # checkout refuses with "is not a clean result for HEAD <head>
            # (sha=<producing sha>)". A reader following that instruction
            # against any current tree gets a second refusal and no progress.
            #
            # And NOT EVERY PATH THIS REFUSES ON IS A CHECKOUT AWAITING
            # DISPOSAL. Measured 2026-09-17: of the paths carrying a failed
            # writeback, two were the parent's own `hermit` product checkout and
            # an OCCUPIED AGENT SLOT, each holding a historical failure that
            # nothing will ever complete. Refusing on those is correct. Treating
            # this count as a number to drive to zero points whoever is driving
            # it at a live slot.
            "materialized scorecard cleanup requires a completed scorecard_writeback; "
            f"got state={state!r}, scorecard_writeback={writeback!r}. The writeback "
            "refused and its reason is in that record. Re-driving it needs BOTH the "
            "retained results under ignored/validate/artifacts/ AND a checkout at the "
            "commit that produced them, because observe-results refuses results whose "
            "sha does not match its HEAD; a current checkout is not sufficient. No "
            "reattachment to the original run is needed or possible. If this path is a "
            "primary checkout or an occupied agent slot rather than a disposable "
            "validate checkout, this refusal is correct and permanent -- do not remove "
            "it. Historical results require an already-published verified handoff"
        )
    wrkslots = recorded_wrkslots_identity(record, checkout)
    if wrkslots is None:
        raise RuntimeError("materialized scorecard handoff has no exact wrkslots identity")
    return publish_scorecard_handoff(
        root,
        checkout,
        target,
        unit,
        writeback_completed=True,
        expected_files=record.get("scorecard_writeback_files"),
        wrkslots=wrkslots,
        run=run,
        tool_root=tool_root,
    )


COMMIT_STATUS_PUBLICATION_SUCCEEDED = frozenset({"published", "unchanged"})


def finalize_commit_status_publication(
    state_root: Path,
    tool_root: Path,
    record_path: Path,
    *,
    record: Mapping[str, Any],
    target: str,
    repo: str,
    canonical_verdict: str,
    canonical_exit: int,
    scorecard_updated: bool,
    run: Runner,
) -> dict[str, Any]:
    """Publish an exact-SHA green status without changing the measured verdict.

    The receipt and scorecard are established before this network-facing cache
    is attempted. A missing GitHub status stays visible in the durable run
    handle, but it cannot rewrite a VALIDATED product result into a test failure.
    Re-entry skips an already successful publication; after a crash between the
    remote write and bookkeeping, the publisher's own exact-status comparison
    returns ``unchanged`` instead of writing a duplicate.
    """

    recorded_at = datetime.now(timezone.utc).isoformat()
    try:
        current = run_registry.read_record(record_path)
    except RuntimeError:
        current = {}
    existing = current.get("commit_status_publication")
    if (
        isinstance(existing, dict)
        and existing.get("state") in COMMIT_STATUS_PUBLICATION_SUCCEEDED
        and existing.get("repository") == repo
        and existing.get("sha") == target
    ):
        print(
            "validate-run: commit status publication already recorded; "
            f"not invoking publisher again for {repo}@{target}",
            file=sys.stderr,
        )
        return {**record, "commit_status_publication": existing}

    common: dict[str, str] = {
        "recorded_at": recorded_at,
        "repository": repo,
        "sha": target,
    }
    if canonical_verdict != "VALIDATED" or canonical_exit != EXIT_PASSED:
        outcome: dict[str, str] = {
            **common,
            "state": "not-attempted",
            "reason": f"canonical verdict is {canonical_verdict}",
        }
    elif not scorecard_updated:
        outcome = {
            **common,
            "state": "not-attempted",
            "reason": "scorecard write-back did not complete",
        }
    else:
        environment = git_env.sanitized_git_env()
        environment.update(
            {
                "CI_HUB_SKIP_SOFT_MAIN_WARNING": "1",
                "CI_HUB_TOOL_COST_ACTIVE": "1",
                "DEV_HERMIT_PARENT": str(state_root),
                "DEV_HERMIT_TOOL_ROOT": str(tool_root),
            }
        )
        command = [
            str(tool_root / "ci-hub/ci-hub"),
            "publish-commit-status",
            "--sha",
            target,
            "--repo",
            repo,
            "--json",
        ]
        try:
            published = run(
                command,
                cwd=state_root,
                env=environment,
                check=False,
            )
        except OSError as error:
            outcome = {
                **common,
                "state": "failed",
                "detail": f"cannot launch publisher: {error}",
            }
        else:
            if published.returncode != 0:
                detail = (
                    published.stderr.strip()
                    or published.stdout.strip()
                    or f"publisher exited {published.returncode} without output"
                )[-1000:]
                outcome = {**common, "state": "failed", "detail": detail}
            else:
                outcome = commit_status_publication_outcome(
                    published.stdout,
                    common=common,
                    repository=repo,
                    target=target,
                )

    updated = bookkeep_record(
        record_path,
        base_record=record,
        commit_status_publication=outcome,
    )
    if outcome["state"] in COMMIT_STATUS_PUBLICATION_SUCCEEDED:
        print(
            f"validate-run: commit status {outcome['state']} for {repo}@{target}",
            file=sys.stderr,
        )
    elif outcome["state"] == "failed":
        print(
            "validate-run: COMMIT STATUS NOT PUBLISHED: "
            f"{outcome['detail']}. The canonical validation verdict is unchanged.",
            file=sys.stderr,
        )
    else:
        print(
            "validate-run: commit status publication not attempted: "
            f"{outcome['reason']}",
            file=sys.stderr,
        )
    return updated


def commit_status_publication_outcome(
    output: str,
    *,
    common: Mapping[str, str],
    repository: str,
    target: str,
) -> dict[str, str]:
    try:
        report = json.loads(output)
        action = report["action"]
        if (
            action not in COMMIT_STATUS_PUBLICATION_SUCCEEDED
            or report.get("repository") != repository
            or report.get("sha") != target
        ):
            raise ValueError("publisher report does not name the requested status")
        description = report["description"]
        receipt_commit = report["receipt_commit"]
        receipt_path = report["receipt_path"]
        if not all(
            isinstance(value, str) and value
            for value in (description, receipt_commit, receipt_path)
        ):
            raise ValueError("publisher report omitted receipt identity")
        return {
            **common,
            "state": action,
            "description": description,
            "receipt_commit": receipt_commit,
            "receipt_path": receipt_path,
        }
    except (json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        detail = output.strip()[-1000:]
        return {
            **common,
            "state": "failed",
            "detail": f"unreadable publisher result: {error}; output={detail!r}",
        }


def bookkeep_record(
    record_path: Path,
    /,
    *,
    base_record: Mapping[str, Any] | None = None,
    **fields: Any,
) -> dict[str, Any]:
    """Update the run record, but never at the cost of the caller's verdict.

    ⚠️ THIS IS THE PRIORITY INVERSION THAT HELD THE OWNER'S BOX. Both callers run
    AFTER the run's outcome is settled: the first after `wait_for_unit` has
    returned the terminal state, the second after the canonical verdict has been
    read. Neither result depends on the write succeeding -- the record is a
    handle for later readers, not the answer. Yet both took `flock(LOCK_EX)` with
    no bound, so a contended lock stopped the wrapper delivering a verdict it had
    already computed.

    Same shape as the rest of tonight: a value produced and never delivered.

    On contention the fact is REPORTED and the caller continues. Callers that
    keep using the result supply their complete in-memory snapshot; the attempted
    fields are merged into it whether persistence succeeds or not. The record on
    disk is the only thing that goes stale -- which is the correct thing to
    sacrifice, because a reader can re-derive it and the human at the terminal
    cannot.
    """
    snapshot = {**(base_record or {}), **fields}
    try:
        persisted = run_registry.update_record(record_path, blocking=False, **fields)
        return {**persisted, **snapshot}
    except run_registry.RecordLocked as locked:
        print(
            f"validate-run: RUN-RECORD NOT UPDATED: {locked}. The verdict below is "
            "unaffected -- this is a bookkeeping write, and it is reported rather "
            "than waited on so a held lock cannot withhold a result that is "
            "already known. Remedy: none required for this run; if it persists, "
            f"find the holder with `fuser -v {record_path}.lock` or `lsof`.",
            file=sys.stderr,
        )
        return snapshot


def handoff_scorecard_after_writeback(
    root: Path,
    record_path: Path,
    checkout: Path,
    target: str,
    unit: str,
    *,
    writeback_completed: bool,
    expected_files: object,
    wrkslots: WrkslotsIdentity | None,
    run: Runner,
    tool_root: Path,
) -> Path | None:
    """Publish scorecard output before a materialized checkout is retired."""
    try:
        if wrkslots is None:
            raise RuntimeError("materialized scorecard handoff has no exact wrkslots identity")
        handoff = publish_scorecard_handoff(
            root,
            checkout,
            target,
            unit,
            writeback_completed=writeback_completed,
            expected_files=expected_files,
            wrkslots=wrkslots,
            run=run,
            tool_root=tool_root,
        )
    except (OSError, RuntimeError, ValueError) as error:
        print(
            "validate-run: SCORECARD HANDOFF NOT PUBLISHED: "
            f"{error}. The materialized checkout is retained for recovery.",
            file=sys.stderr,
        )
        return None
    bookkeep_record(record_path, scorecard_handoff=str(handoff))
    print(
        f"validate-run: SCORECARD_HANDOFF {handoff}; the managed checkout may now be retired.",
        file=sys.stderr,
    )
    return handoff


def cleanup_after_attach(
    root: Path,
    record_path: Path,
    record: Mapping[str, Any],
    *,
    unit: str,
    run: Runner,
    tool_root: Path = ROOT,
    retain_checkout_reason: str | None = None,
) -> None:
    """Finish cleanup owned by a caller that previously stopped waiting."""
    if retain_checkout_reason is not None:
        print(
            "validate-run: completed checkout RETAINED at "
            f"{record.get('checkout')}: {retain_checkout_reason}",
            file=sys.stderr,
        )
    else:
        try:
            cleaned, archived, reason = cleanup_recorded_checkout(
                root,
                record,
                record_path=record_path,
                unit=unit,
                run=run,
                tool_root=tool_root,
            )
        except (OSError, RuntimeError, ValueError) as error:
            print(
                "validate-run: completed checkout RETAINED: cleanup could not "
                f"preserve its evidence: {error}",
                file=sys.stderr,
            )
        else:
            if cleaned:
                bookkeep_record(
                    record_path,
                    checkout_removed_at=datetime.now(timezone.utc).isoformat(),
                    archived_orphaned_receipts=archived,
                )
            elif reason is not None:
                print(
                    f"validate-run: completed checkout RETAINED: {reason}",
                    file=sys.stderr,
                )
    try:
        cargo_cleaned, cargo_reason = cleanup_recorded_cargo_home(
            root,
            record,
            record_path=record_path,
            run=run,
            tool_root=tool_root,
        )
    except (OSError, RuntimeError, ValueError) as error:
        print(
            f"validate-run: completed Cargo home RETAINED: {error}",
            file=sys.stderr,
        )
    else:
        if cargo_cleaned:
            bookkeep_record(
                record_path,
                cargo_home_removed_at=datetime.now(timezone.utc).isoformat(),
            )
        elif cargo_reason is not None:
            print(
                f"validate-run: completed Cargo home RETAINED: {cargo_reason}",
                file=sys.stderr,
            )


def attach(
    raw_unit: str,
    *,
    root: Path,
    tool_root: Path = ROOT,
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
    updated = bookkeep_record(record_path, base_record=record, **final)
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
            cleanup_after_attach(
                root, record_path, record, unit=unit, run=run, tool_root=tool_root
            )
            return could_not_determine(
                f"FROZEN-VALIDATE-RESULT-UNAVAILABLE: {error}",
                f"the unit ran; inspect {record.get('result_record', '<result-record>')} "
                "and the durable log. Do not read this as a current receipt.",
            )
        updated = bookkeep_record(
            record_path,
            base_record=updated,
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
        cleanup_after_attach(
            root, record_path, record, unit=unit, run=run, tool_root=tool_root
        )
        return measured_exit
    try:
        target = str(record["target"])
        checkout = Path(str(record["checkout"]))
        run_started_at = str(record["started_at"])
        repo = canonical_repo(str(record.get("repo", "rrnewton/hermit")))
        row, canonical_verdict, canonical_exit = assert_row_readable_from_canonical_ledger(
            root,
            target,
            checkout,
            run_started_at,
            repo=repo,
            run=run,
            tool_root=tool_root,
        )
    except (KeyError, RuntimeError, ValueError) as error:
        cleanup_after_attach(
            root, record_path, record, unit=unit, run=run, tool_root=tool_root
        )
        return could_not_determine(
            f"CANONICAL-VERDICT-UNAVAILABLE: {error}",
            "the unit ran; re-read with `ci-hub validate-status --sha "
            f"{record.get('target', '<target>')}` and, if the ledger row is "
            "genuinely absent, re-run the validate. Do NOT read this as a failure.",
        )
    updated = bookkeep_record(
        record_path,
        base_record=updated,
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
    materialized_target = record.get("materialized_target") is True
    scorecard_handoff: Path | None = None
    scorecard_files: list[dict[str, object]] | None = None
    handoff_required = materialized_target and repo == "rrnewton/hermit"
    scorecard_updated = repo != "rrnewton/hermit"
    recorded_handoff = updated.get("scorecard_handoff")
    historical_result = updated.get("state") == "unknown" and updated.get(
        "service_result_schema"
    ) in {
        service_result.WRITEBACK_SCHEMA_VERSION,
        service_result.SELECTION_SCHEMA_VERSION,
    }
    handoff_recorded = recorded_handoff is not None
    if handoff_required and handoff_recorded:
        try:
            if historical_result:
                scorecard_handoff = publish_recorded_scorecard_handoff(
                    root,
                    record_path,
                    updated,
                    Path(str(updated["checkout"])),
                    unit,
                    run=run,
                    tool_root=tool_root,
                )
                scorecard_updated = scorecard_handoff is not None
            else:
                scorecard_handoff, scorecard_updated = read_scorecard_handoff(
                    root, recorded_handoff, target, unit
                )
        except (OSError, RuntimeError, ValueError) as error:
            print(
                "validate-run: SCORECARD HANDOFF UNREADABLE: "
                f"{error}. The established validate verdict is unchanged.",
                file=sys.stderr,
            )
        else:
            print(
                f"validate-run: SCORECARD_HANDOFF {scorecard_handoff}; using the "
                "already-published output instead of the retired checkout.",
                file=sys.stderr,
            )
    elif handoff_required and historical_result:
        print(
            "validate-run: SCORECARD HANDOFF MISSING: a historical result cannot "
            "publish a new handoff or gain current validation authority.",
            file=sys.stderr,
        )
    else:
        try:
            if repo != "rrnewton/hermit":
                scorecard_updated = True
            else:
                scorecard_files = write_scorecard_from_results(
                    Path(
                        updated["checkout"]
                        if materialized_target
                        else updated["source_checkout"]
                    ),
                    target,
                    Path(updated["e2e_result_root"]),
                    run=run,
                    json_output=json_output,
                )
                scorecard_updated = scorecard_files is not None
                if materialized_target and scorecard_files is not None:
                    updated = bookkeep_record(
                        record_path,
                        base_record=updated,
                        scorecard_writeback_files=scorecard_files,
                    )
        except KeyError:
            print(
                "validate-run: compatibility scorecard NOT UPDATED: the run record is "
                "missing its source checkout or durable result directory. The established "
                "validate verdict is unchanged.",
                file=sys.stderr,
            )
            scorecard_updated = False
    if (
        handoff_required
        and scorecard_handoff is None
        and not handoff_recorded
        and not historical_result
    ):
        wrkslots = recorded_wrkslots_identity(updated, Path(updated["checkout"]))
        scorecard_handoff = handoff_scorecard_after_writeback(
            root,
            record_path,
            Path(updated["checkout"]),
            target,
            unit,
            writeback_completed=scorecard_updated,
            expected_files=scorecard_files,
            wrkslots=wrkslots,
            run=run,
            tool_root=tool_root,
        )
        scorecard_updated = scorecard_updated and scorecard_handoff is not None
    updated = finalize_commit_status_publication(
        root,
        tool_root,
        record_path,
        record=updated,
        target=target,
        repo=repo,
        canonical_verdict=canonical_verdict,
        canonical_exit=canonical_exit,
        scorecard_updated=scorecard_updated,
        run=run,
    )
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=json_output,
    )
    cleanup_after_attach(
        root,
        record_path,
        updated,
        unit=unit,
        run=run,
        tool_root=tool_root,
        retain_checkout_reason=(
            "its scorecard output could not be handed off durably"
            if handoff_required and scorecard_handoff is None
            else None
        ),
    )
    return (
        canonical_exit
        if scorecard_updated or canonical_exit != EXIT_PASSED
        else EXIT_COULD_NOT_DETERMINE
    )


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
            "DIRECTORY of the checkout to validate, normally \"$PWD\". With "
            "--materialize-target it is instead the repository supplying the exact "
            "target object; its HEAD, branch, and worktree bytes are not consumed. "
            "This is a path, NOT a commit -- the SHA goes in --target."
        ),
    )
    result.add_argument(
        "--state-root",
        type=Path,
        help=argparse.SUPPRESS,
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
        "--materialize-target",
        action="store_true",
        help=(
            "use --checkout only as the repository containing the exact --target object, "
            "then build and validate that target in the normal registered fresh checkout. "
            "The source HEAD, branch, index, and worktree are not read or changed. "
            "Generated scorecard files are atomically handed off below the canonical state "
            "root before the managed checkout is retired"
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
            "commit, NOT a path. Ordinarily --checkout DIR must have it checked out; "
            "--materialize-target instead requires that exact object to exist while "
            "leaving the source HEAD untouched."
        ),
    )
    result.add_argument("--pr", type=int)
    result.add_argument(
        "--attach",
        metavar="VALIDATE-UNIT",
        help="reattach to a durable validate-* handle without launching another run",
    )
    result.add_argument(
        "--sweep-completed",
        action="store_true",
        help=(
            "hand off materialized scorecards, archive checkout-local receipt evidence, "
            "and remove completed "
            "worktrees/validate/ checkouts left by interrupted waiters"
        ),
    )
    result.add_argument("--unit", help="validate-* unit name; .service suffix is optional")
    result.add_argument("--log", type=Path, help="durable log (default: ignored/validate/<unit>.log)")
    result.add_argument("--wait", type=int, default=7200, help="validate-lock queue wait bound")
    result.add_argument(
        "--no-wait",
        action="store_true",
        help=(
            "refuse inside the detached service unless validation admission is "
            "immediately available; never remain in either validation queue"
        ),
    )
    result.add_argument(
        "--skip-if-recorded",
        action="store_true",
        help=(
            "after admission acquires a validation slot, refuse before starting "
            "the child if the exact target now has any canonical ledger record"
        ),
    )
    result.add_argument("--hold", type=int, default=1200, help="validate-lock lease seconds")
    result.add_argument(
        "--child-deadline",
        type=int,
        help=(
            "payload wall deadline (default: Hermit 3600s; Reverie 8000s). "
            "Hermit requires more than 61s so its derived run timeout and "
            "cleanup grace both fit inside this bound; Hermit requests are capped "
            f"at {HERMIT_MAX_CHILD_DEADLINE_SECONDS}s and cannot raise the "
            f"{HERMIT_MAX_RUN_TIMEOUT_SECONDS}s whole-run ceiling"
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
        "--max",
        dest="max_validates",
        type=int,
        help=(
            "validate-lock admission policy to request; omit for the measured "
            "concurrent default, or pass 1 for an exclusive solo confirmation"
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
    environment = git_env.sanitized_git_env(
        os.environ if environment is None else environment
    )
    base_run = run

    def run_with_sanitized_environment(
        command: Sequence[str], **kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        if kwargs.get("env") is None:
            kwargs["env"] = environment
        return base_run(command, **kwargs)

    run = run_with_sanitized_environment
    explicit_tool_root = environment.get("DEV_HERMIT_TOOL_ROOT", "")
    if explicit_tool_root:
        tool_root = Path(explicit_tool_root)
        if not tool_root.is_absolute() or not tool_root.is_dir():
            return refuse(
                f"DEV_HERMIT_TOOL_ROOT is not an existing absolute directory: {tool_root}",
                "launch through the immutable operational-tool wrapper",
            )
    else:
        tool_root = root.resolve()
    state_root = (args.state_root or tool_root).resolve()
    tool_authority: ImmutableToolAuthority | None = None
    pathname_tool_head: str | None = None
    try:
        repo = canonical_repo(args.repo)
    except ValueError as error:
        return refuse(str(error), "correct the argument named above and re-run")
    if args.caller_poll_seconds <= 0:
        return refuse(
            "caller poll seconds must be positive",
            "pass --caller-poll-seconds with a value greater than 0, or omit it for the default",
        )
    if args.sweep_completed:
        if any(
            (
                args.attach,
                args.checkout,
                args.agent,
                args.target,
                args.pr,
                args.unit,
                args.log,
                args.ci_dag_jobs,
                args.frozen_validate,
                args.in_place,
                args.materialize_target,
                args.make_validate,
                args.allow_owner_validate_symlink,
                args.dry_run,
                args.validate_args,
            )
        ):
            return refuse(
                "--sweep-completed cannot be combined with launch or attach arguments",
                "run `ci-hub validate-run --sweep-completed` by itself",
            )
        report = sweep_completed_checkouts(state_root, run=run, tool_root=tool_root)
        emit_report(
            {"schema_version": 1, "event": "sweep-completed", **report},
            json_output=args.json,
        )
        return (
            0
            if not report["retained"] and not report["bookkeeping_errors"]
            else EXIT_COULD_NOT_DETERMINE
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
                args.materialize_target,
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
                root=state_root,
                tool_root=tool_root,
                run=run,
                json_output=args.json,
                poll_seconds=args.caller_poll_seconds,
                sleep=sleep,
            )
        except (RuntimeError, ValueError) as error:
            return refuse(
                str(error),
                "attach with the exact validate-* handle printed by the original validate-run",
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
    if args.materialize_target and args.in_place:
        return refuse(
            "--materialize-target cannot use --in-place",
            "drop --in-place so the exact target is built in a registered fresh checkout",
        )
    if args.materialize_target and args.frozen_validate:
        return refuse(
            "--materialize-target cannot be combined with --frozen-validate",
            "use --materialize-target for a current landing candidate or --frozen-validate "
            "for a deliberately historical non-qualifying measurement",
        )
    if args.frozen_validate and "--label-pr" in validate_args:
        return refuse(
            "--frozen-validate cannot publish a receipt-backed label",
            "drop --label-pr; this run measures a target that does not contain current main",
        )
    if args.frozen_validate:
        if not validate_args:
            validate_args.append("full")
        # Use the CLI spelling rather than exporting VALIDATE_LABEL_PR=0. Old
        # validate.rs self-tests intentionally parse a label-capable nested
        # invocation; inheriting the outer environment made that negative case
        # silently non-labeling and stopped the historical run before its matrix.
        validate_args.append("--no-label-pr")
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
    requested_child_deadline = args.child_deadline or (
        8000 if repo == "rrnewton/reverie" else 3600
    )
    child_deadline = effective_child_deadline(repo, requested_child_deadline)
    if child_deadline != requested_child_deadline:
        print(
            "validate-run: limiting Hermit --child-deadline "
            f"from {requested_child_deadline}s to {child_deadline}s; "
            f"validation work remains capped at {HERMIT_MAX_RUN_TIMEOUT_SECONDS}s",
            file=sys.stderr,
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
    fresh_wrkslots: WrkslotsIdentity | None = None
    source_checkout: Path | None = None
    source_branch: str | None = None
    private_cargo_home: Path | None = None
    runtime_root: Path | None = None
    current_main_before_launch: str | None = None
    frozen_result: Path | None = None
    service_tool_prefix: list[str] | None = None
    service_acceptance = ServiceAcceptance.PRE_ACCEPT

    def remove_current_fresh_checkout() -> bool:
        if fresh_checkout is None or source_checkout is None:
            return True
        return remove_fresh_checkout(
            source_checkout,
            fresh_checkout,
            run=run,
            tool_root=tool_root,
            wrkslots=fresh_wrkslots,
        )

    def cleanup_current_fresh_preserving_receipts() -> FreshCheckoutCleanup:
        if fresh_checkout is None or source_checkout is None:
            return FreshCheckoutCleanup((), (), True)
        return cleanup_fresh_checkout_preserving_receipts(
            state_root,
            source_checkout,
            fresh_checkout,
            unit=unit,
            run=run,
            tool_root=tool_root,
            wrkslots=fresh_wrkslots,
        )

    def report_inner_refusal_cleanup(cleanup: FreshCheckoutCleanup) -> None:
        if cleanup.error is not None:
            print(
                "validate-run: ORPHANED-RECEIPT CHECK REFUSED: "
                f"{cleanup.error}. Temp checkout RETAINED at {fresh_checkout}.",
                file=sys.stderr,
            )
        elif cleanup.locations:
            disposition = (
                "removed" if cleanup.removed else f"RETAINED at {fresh_checkout}"
            )
            print(
                "validate-run: ORPHANED-RECEIPT: archived "
                f"{', '.join(cleanup.archived)} before the checkout was {disposition}.",
                file=sys.stderr,
            )

    try:
        if FD_TOOL_ROOT_RE.fullmatch(str(tool_root)) is not None:
            tool_authority = read_immutable_tool_authority(
                tool_root, state_root, environment
            )
        elif explicit_tool_root:
            pathname_tool_head = canonical_worktree_tool_head(tool_root, state_root)
        service_tool_prefix = detached_tool_prefix(
            tool_root, state_root, environment, authority=tool_authority
        )
        source_checkout = validate_checkout(
            args.checkout,
            args.target,
            repo=repo,
            run=run,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
            materialize_target=args.materialize_target,
        )
        source_branch = (
            None
            if args.materialize_target
            else source_checkout_branch(
                source_checkout,
                args.target,
                run=run,
            )
        )
        unit = sanitize_unit(args.unit) if args.unit else default_unit(args.agent, args.target)
        if args.frozen_validate:
            current_main_before_launch = current_main_for_frozen_validate(
                source_checkout, args.target, run=run
            )
            frozen_result = frozen_result_path(state_root, unit)
            if frozen_result.exists():
                raise ValueError(
                    f"frozen validation result already exists at {frozen_result}; "
                    "choose a different --unit so two measurements cannot share one record"
                )
        if args.in_place:
            checkout = source_checkout
        else:
            checkout_parent = (
                frozen_checkout_parent(state_root)
                if args.frozen_validate
                else validate_checkout_parent(state_root)
            )
            fresh_slot = fresh_validation_slot(args.target)
            require_validate_entry_cleanup(
                state_root,
                prospective_slot=fresh_slot,
                prospective_checkout=checkout_parent / fresh_slot,
                run=run,
                tool_root=tool_root,
            )
            # DEFAULT: validate the COMMIT, not the tree that claims to be at it.
            fresh_checkout, fresh_wrkslots = prepare_fresh_checkout(
                source_checkout,
                args.target,
                run=run,
                parent=checkout_parent,
                repo=repo,
                tool_root=tool_root,
                independent_refs=args.frozen_validate,
                slot=fresh_slot,
            )
            if args.materialize_target:
                # The source supplies only Git objects. Prove the tree that will
                # actually execute has the driver, exact HEAD, and no tracked or
                # untracked edits before any preflight or systemd admission.
                validate_checkout(
                    fresh_checkout,
                    args.target,
                    repo=repo,
                    run=run,
                )
            checkout = fresh_checkout
        preflight(tool_root, checkout, args.target, repo=repo, run=run)
        runtime_root = prepare_runtime_root(environment)
        private_cargo_home = prepare_private_cargo_home(
            state_root,
            run=run,
            shared=Path(environment.get("HOME", "")) / ".cargo",
        )
        log = (args.log or state_root / "ignored/validate" / f"{unit}.log").resolve()
        record_path = run_registry.record_path(state_root, unit)
        service_result_schema = (
            service_result.schema_for_checkout(
                checkout, reference_checkout=tool_root / "hermit"
            )
            if repo == "rrnewton/hermit"
            else None
        )
        framework_result = (
            service_result.result_path(record_path)
            if service_result_schema is not None
            else None
        )
        if framework_result is not None and framework_result.exists():
            raise ValueError(
                f"validation service result already exists at {framework_result}; "
                "choose a different --unit so two measurements cannot share one result"
            )
        e2e_result_root, runner_log_dir = _validate_output_paths(state_root, unit)
        output_fields: dict[str, object] = {
            "validate_lock_child_deadline_seconds": child_deadline
        }
        if repo == "rrnewton/hermit":
            output_fields.update(
                {
                    "e2e_result_root": str(e2e_result_root),
                    "safe_ci_dag_runner_log_dir": str(runner_log_dir),
                    "hermit_run_timeout_seconds": hermit_run_timeout,
                    **(
                        {"service_result_schema": service_result_schema}
                        if service_result_schema is not None
                        else {}
                    ),
                }
            )
        started_at = datetime.now(timezone.utc).isoformat()
        command = build_systemd_command(
            root=tool_root,
            state_root=state_root,
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
            environment=environment,
            make_validate=args.make_validate,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
            cargo_home=private_cargo_home,
            runtime_root=runtime_root,
            ci_dag_jobs=args.ci_dag_jobs,
            max_validates=args.max_validates,
            no_wait=args.no_wait,
            skip_if_recorded=args.skip_if_recorded,
            repo=repo,
            frozen_validate=args.frozen_validate,
            frozen_result=frozen_result,
            framework_result=framework_result,
            unit_tool_prefix=service_tool_prefix,
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
            run_registry.create_current_record(
                record_path,
                {
                    "schema_version": run_registry.SCHEMA_VERSION,
                    "kind": validation_kind(repo, frozen_validate=args.frozen_validate),
                    "state": "preparing",
                    "unit": f"{unit}.service",
                    "target": args.target,
                    "repo": repo,
                    "checkout": str(checkout),
                    "source_checkout": str(source_checkout),
                    "materialized_target": args.materialize_target,
                    **(
                        {
                            "scorecard_handoff": str(
                                scorecard_handoff_path(state_root, unit)
                            )
                        }
                        if args.materialize_target and repo == "rrnewton/hermit"
                        else {}
                    ),
                    **({"branch": source_branch} if source_branch is not None else {}),
                    "temporary_checkout": fresh_checkout is not None,
                    **(
                        {
                            "wrkslots_slot": fresh_wrkslots.slot,
                            "wrkslots_generation": fresh_wrkslots.generation,
                        }
                        if fresh_wrkslots is not None
                        else {}
                    ),
                    "cargo_home": str(private_cargo_home),
                    "log": str(log),
                    "agent": args.agent,
                    "pr": args.pr,
                    "started_at": started_at,
                    # The commit this run's TOOLING came from, distinct from
                    # `target`, which is the subject being validated.
                    "parent_checkout_head": (
                        tool_authority.parent_sha
                        if tool_authority is not None
                        else pathname_tool_head or parent_checkout_head(tool_root)
                    ),
                    "producer": run_registry.PRODUCER,
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
            try:
                # Reserve the service's output before creating any observer.
                # A pane for a unit that has not launched can otherwise outlive
                # a reservation refusal and manufacture an UNKNOWN result.
                run_registry.reserve_log(log)
                run_registry.update_record(record_path, state="launching")
            except Exception as error:
                detail = str(error)
                try:
                    record_unstarted_refusal(record_path, detail=detail)
                except Exception as record_error:
                    raise RuntimeError(
                        f"{detail}; additionally could not finalize the unstarted run "
                        f"handle: {record_error}"
                    ) from error
                raise RuntimeError(detail) from error
            try:
                result = run(command, cwd=tool_root, check=False)
            except Exception as error:
                detail = f"systemd-run could not be invoked: {error}"
                try:
                    record_unstarted_refusal(record_path, detail=detail)
                except Exception as record_error:
                    raise RuntimeError(
                        f"{detail}; additionally could not finalize the unstarted run "
                        f"handle: {record_error}"
                    ) from error
                raise RuntimeError(detail) from error
            if result.returncode != 0:
                detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
                record_unstarted_refusal(
                    record_path, detail=detail, exit_code=result.returncode
                )
                raise RuntimeError(f"systemd-run refused service: {detail}")
            # This transition is the cleanup boundary. From this instruction
            # onward systemd owns a live unit, so no bookkeeping or observer
            # failure may claim that it never started or delete its inputs.
            service_acceptance = ServiceAcceptance.POST_ACCEPT
            run_registry.update_record(record_path, state="running")
            # Start the observer only after systemd accepted the unit. It reads
            # the durable log from offset zero, so this loses no output and
            # makes a pane for a never-launched service impossible.
            try:
                pane = pane_owner.create_pane(
                    root=tool_root,
                    checkout=checkout,
                    unit=unit,
                    target=args.target,
                    log=log,
                    record=record_path,
                    pr=args.pr,
                    started_at=started_at,
                    run=run,
                    environment=environment,
                    sleep=sleep,
                )
            except Exception as exc:
                pane = None
                print(f"PANE-REFUSED {exc}", file=sys.stderr)
                print(
                    "PANE-REFUSED continuing WITHOUT an observer pane: the pane only watches "
                    "the durable log, so the validation itself is unaffected.",
                    file=sys.stderr,
                )
            if pane is not None:
                run_registry.update_record(
                    record_path,
                    workspace_id=pane.workspace_id,
                    tab_id=pane.tab_id,
                    pane_id=pane.pane_id,
                    pane_title=pane.title,
                )
    except Exception as error:
        if service_acceptance is ServiceAcceptance.POST_ACCEPT:
            print(
                f"validate-run: ACCEPTED {unit}.service; handle={record_path} "
                f"checkout={checkout} log={log}",
                file=sys.stderr,
            )
            if fresh_checkout is not None:
                print(
                    f"validate-run: temp checkout RETAINED at {fresh_checkout} "
                    "(accepted service may still use it)",
                    file=sys.stderr,
                )
            return could_not_determine(
                f"POST-ACCEPT BOOKKEEPING FAILED for {unit}.service; "
                f"RUN CONTINUES independently: {error}",
                f"do not relaunch; re-attach with `ci-hub validate-run --attach {unit}` "
                f"and reconcile the durable handle at {record_path}",
            )
        # Nothing was admitted, so the temp checkout holds no evidence and is
        # removed. Failures AFTER launch are handled below and RETAIN it.
        remove_current_fresh_checkout()
        remove_private_cargo_home(private_cargo_home, run=run)
        remove_runtime_root(runtime_root, run=run)
        if isinstance(error, ValidateEntryCleanupRefused):
            return refuse(
                str(error),
                "run `ci-hub validate-run --sweep-completed` until it reports "
                "deferred=0, no non-running RETAINED rows, and "
                "bookkeeping-errors=0. If a retained path names a live process, "
                "let it exit or stop it before retrying this validate",
            )
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
        "branch": source_branch,
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
    if args.dry_run:
        checkout_removed = remove_current_fresh_checkout()
        remove_private_cargo_home(private_cargo_home, run=run)
        remove_runtime_root(runtime_root, run=run)
        if not checkout_removed:
            if fresh_checkout is None:
                path_observation = CleanupPresenceObservation(
                    CleanupPresence.COULD_NOT_DETERMINE,
                    "no exact temp checkout path was available",
                )
                row_observation = CleanupPresenceObservation(
                    CleanupPresence.COULD_NOT_DETERMINE,
                    "no exact wrkslots slot and generation were available",
                )
            else:
                path_observation, row_observation = observe_fresh_checkout_cleanup(
                    state_root,
                    fresh_checkout,
                    fresh_wrkslots,
                    run=run,
                    tool_root=tool_root,
                )
            print(
                "validate-run: dry-run checkout path: "
                f"{path_observation.state.value.upper()} -- {path_observation.detail}",
                file=sys.stderr,
            )
            print(
                "validate-run: dry-run wrkslots row: "
                f"{row_observation.state.value.upper()} -- {row_observation.detail}",
                file=sys.stderr,
            )
            return could_not_determine(
                "dry-run checkout cleanup was not confirmed",
                "inspect the wrkslots refusal above and retry only after the "
                "supported checkout cleanup can complete",
            )
    emit_report(report, json_output=args.json)
    if args.dry_run:
        if not args.json:
            print(f"PANE-PLAN workspace={pane_owner.WORKSPACE_LABEL} role=observer-only")
            print(f"COMMAND {shlex.join(command)}")
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
            relayed_inner_refusal = (
                isinstance(inner_exit, int)
                and inner_exit != 0
                and relay_inner_output(log)
            )
            if relayed_inner_refusal:
                print(
                    f"validate-run: the inner validate exited {inner_exit}; "
                    "that refusal above is the reason. No frozen-validation "
                    "result was written because the run did not get far enough "
                    "to write one.",
                    file=sys.stderr,
                )
            cleanup = cleanup_current_fresh_preserving_receipts()
            report_inner_refusal_cleanup(cleanup)
            remove_private_cargo_home(private_cargo_home, run=run)
            remove_runtime_root(runtime_root, run=run)
            if relayed_inner_refusal:
                return inner_exit
            return could_not_determine(
                f"FROZEN-VALIDATE-RESULT-UNAVAILABLE: {error}",
                f"inspect {frozen_result} and {log}; the run's outcome cannot be "
                "read as either a current receipt or a product verdict",
            )
        updated = bookkeep_record(
            record_path,
            base_record=updated,
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
        remove_current_fresh_checkout()
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
            state_root,
            args.target,
            checkout,
            started_at,
            repo=repo,
            run=run,
            tool_root=tool_root,
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
            cleanup = cleanup_current_fresh_preserving_receipts()
            report_inner_refusal_cleanup(cleanup)
            remove_private_cargo_home(private_cargo_home, run=run)
            remove_runtime_root(runtime_root, run=run)
            return inner_exit
        print(f"validate-run: CANONICAL-VERDICT-UNAVAILABLE: {error}", file=sys.stderr)
        if fresh_checkout is not None and source_checkout is not None:
            # Retention is for EVIDENCE, not for every failure. Retaining
            # unconditionally cost 26 MB per failed validate, and because
            # The Rust validation driver is fail-fast; that is most of them.
            cleanup = cleanup_current_fresh_preserving_receipts()
            if cleanup.error is not None:
                remove_private_cargo_home(private_cargo_home, run=run)
                remove_runtime_root(runtime_root, run=run)
                return could_not_determine(
                    "ORPHANED-RECEIPT: checkout-local evidence could not be "
                    f"safely classified or archived: {cleanup.error}. Temp checkout "
                    f"RETAINED at {fresh_checkout}.",
                    "preserve the files manually before reclaiming the checkout",
                )
            if cleanup.locations:
                # The run HAPPENED and produced evidence; preserve it in the
                # durable validation evidence tree before reclaiming the temp
                # checkout. Keeping a whole checkout for one receipt caused the
                # unbounded disk growth this path now prevents.
                disposition = (
                    "removed"
                    if cleanup.removed
                    else f"RETAINED at {fresh_checkout}"
                )
                remove_private_cargo_home(private_cargo_home, run=run)
                remove_runtime_root(runtime_root, run=run)
                return could_not_determine(
                    "ORPHANED-RECEIPT: this run wrote a receipt into "
                    f"{', '.join(cleanup.locations)} inside the temp checkout, where no "
                    "consumer reads it. The receipt was archived outside the temp "
                    f"checkout at {', '.join(cleanup.archived)}, then the checkout was {disposition}.",
                    "inspect the archived receipt; the run's result exists but is not "
                    "a canonical verdict",
                )
            print(
                "validate-run: NO-RECEIPT-PRODUCED: the run stopped before writing a "
                "receipt anywhere, so the temp checkout holds no evidence; "
                "reclaiming it.",
                file=sys.stderr,
            )
            # The helper already reclaimed the evidence-free checkout.
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
    scorecard_files: list[dict[str, object]] | None = None
    if repo != "rrnewton/hermit":
        scorecard_updated = True
    elif source_checkout is not None:
        scorecard_files = write_scorecard_from_results(
            checkout if args.materialize_target else source_checkout,
            args.target,
            e2e_result_root,
            run=run,
            json_output=args.json,
        )
        scorecard_updated = scorecard_files is not None
        if args.materialize_target and scorecard_files is not None:
            updated = bookkeep_record(
                record_path,
                base_record=updated,
                scorecard_writeback_files=scorecard_files,
            )
    else:
        scorecard_updated = False
    scorecard_handoff: Path | None = None
    handoff_required = (
        args.materialize_target
        and repo == "rrnewton/hermit"
        and fresh_checkout is not None
    )
    if handoff_required:
        scorecard_handoff = handoff_scorecard_after_writeback(
            state_root,
            record_path,
            fresh_checkout,
            args.target,
            unit,
            writeback_completed=scorecard_updated,
            expected_files=scorecard_files,
            wrkslots=fresh_wrkslots,
            run=run,
            tool_root=tool_root,
        )
        scorecard_updated = scorecard_updated and scorecard_handoff is not None
    updated = finalize_commit_status_publication(
        state_root,
        tool_root,
        record_path,
        record=updated,
        target=args.target,
        repo=repo,
        canonical_verdict=canonical_verdict,
        canonical_exit=canonical_exit,
        scorecard_updated=scorecard_updated,
        run=run,
    )
    if handoff_required and scorecard_handoff is None:
        print(
            f"validate-run: materialized target checkout RETAINED at {fresh_checkout}; "
            "its scorecard output could not be handed off durably, so the checkout "
            "remains registered with wrkslots for recovery.",
            file=sys.stderr,
        )
    else:
        remove_current_fresh_checkout()
    # The normal completion path, and the one that matters most: without this the
    # per-run cargo home leaks on EVERY successful validate.
    remove_private_cargo_home(private_cargo_home, run=run)
    remove_runtime_root(runtime_root, run=run)
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=args.json,
    )
    return (
        canonical_exit
        if scorecard_updated or canonical_exit != EXIT_PASSED
        else EXIT_COULD_NOT_DETERMINE
    )


if __name__ == "__main__":
    raise SystemExit(main())
