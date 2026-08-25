#!/usr/bin/env python3
"""Launch one detached, admitted Hermit or Reverie validation through ci-hub."""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import time
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
    row: Mapping[str, Any], newest_qualifying: object
) -> bool:
    """Bind a canonical VALIDATED verdict to the exact qualifying row."""
    if not isinstance(newest_qualifying, dict):
        return False
    identity = newest_qualifying.get("receipt_identity")
    if not isinstance(identity, dict):
        return False
    selected = identity.get("tuple")
    if not isinstance(selected, dict):
        return False
    fields = {
        "sha": "commit",
        "tree": "tree",
        "finished_at": "finished_at",
        "host": "host",
        "slot": "slot",
        "log_file": "log_file",
    }
    return all(
        selected_key in selected and selected[selected_key] == row.get(row_key)
        for selected_key, row_key in fields.items()
    )


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
    return subprocess.run(command, text=True, capture_output=True, **kwargs)


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
    return sanitize_unit(f"validate-{safe_agent}-{target[:12]}-{int(time.time())}")


def _validate_output_paths(root: Path, unit: str) -> tuple[Path, Path]:
    base = root / "ignored/validate/artifacts" / unit
    return base / "e2e", base / "safe-ci-dag-runner"


# The row lookup below goes through the canonical union command. This module
# never opens a ledger shard itself.


def prepare_fresh_checkout(
    source: Path, target: str, *, run: Runner, parent: Path, repo: str
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
    # A worktree, not a clone: it shares the source object store, so this costs
    # no object copy. It does register in the source repo, which is why the
    # caller removes it explicitly rather than just unlinking the directory.
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
    run(["git", "-C", str(source), "worktree", "remove", "--force", str(fresh)], check=False)
    run(["git", "-C", str(source), "worktree", "prune"], check=False)


# Seeded from the shared cache rather than started empty, and seeded with these
# two subtrees rather than all of it. Both choices are measured, not aesthetic;
# the numbers are in `test_private_cargo_home.py` and in the commit message.
CARGO_SEED_SUBTREES: tuple[tuple[str, str], ...] = (
    ("registry", "registry"),
    ("git/db", "git/db"),
)


def prepare_private_cargo_home(parent: Path, *, run: Runner, shared: Path) -> Path | None:
    """Give this validate its own CARGO_HOME, seeded by reflink from the shared one.

    WHY. Cargo serialises every cache mutation on one lock file,
    `$CARGO_HOME/.package-cache`. `ci-hub validate-lock` is box-exclusive, so no
    two VALIDATES run at once -- but nothing serialises a validate against the
    ~18 agents running ordinary `cargo build` in their own slots, and every one
    of them inherits the same `~/.cargo`. When a validate node loses that race
    it does not run slowly, it sits in `Blocking waiting for file lock on
    package cache` until its node budget kills it. Measured on devbig030:
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

    Returns the new CARGO_HOME, or None if it could not be built -- in which
    case the caller MUST fall back to the shared cache loudly. Degrading in
    silence would reintroduce the contention while looking fixed.
    """
    if not shared.is_dir():
        print(
            f"validate-run: no shared cargo home at {shared}; leaving CARGO_HOME unset "
            "(this run stays exposed to package-cache lock contention)",
            file=sys.stderr,
        )
        return None
    try:
        private = Path(
            checked_output(
                ["mktemp", "-d", str(parent / "validate-cargo-XXXXXXXX")],
                run=run,
                purpose="cannot create private cargo home",
            )
        )
    except (RuntimeError, ValueError) as error:
        print(f"validate-run: private CARGO_HOME unavailable ({error}); using shared cache", file=sys.stderr)
        return None

    for relative, destination in CARGO_SEED_SUBTREES:
        source = shared / relative
        if not source.is_dir():
            continue
        target = private / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        # --reflink=auto, not =always: a non-CoW filesystem must still work,
        # just slower. Failing here would trade a flake for an outage.
        result = run(
            ["cp", "-a", "--reflink=auto", str(source), str(target)],
            check=False,
        )
        if getattr(result, "returncode", 1) != 0:
            print(
                f"validate-run: could not seed {relative} into {private}; using shared cache",
                file=sys.stderr,
            )
            remove_private_cargo_home(private, run=run)
            return None

    # Registry sources and credentials live in config; without it a private home
    # can resolve a different registry than the shared one did.
    for name in ("config.toml", "config"):
        candidate = shared / name
        if candidate.is_file():
            run(["cp", "-a", str(candidate), str(private / name)], check=False)
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
        newest_qualifying = report.get("newest_qualifying")
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
            if row_is_selected_qualifying_receipt(candidate[1], newest_qualifying)
        ]
        if not selected:
            raise RuntimeError(
                "canonical VALIDATED verdict selects no qualifying receipt belonging "
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
    ci_dag_jobs: int | None = None,
    repo: str = "rrnewton/hermit",
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
    # Last, so it cannot be shadowed by a caller-supplied CARGO_HOME: the whole
    # point is that this run does not share a package-cache lock with the box.
    if cargo_home is not None:
        build_budget_env.extend(["--setenv", f"CARGO_HOME={cargo_home}"])
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
    elif make_validate:
        child.extend(["with-proxy", "make", "validate"])
    else:
        child.extend(["with-proxy", "./scripts/validate.rs", *(validate_args or ["full"])])

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
        "reverie-validate" if repo == "rrnewton/reverie" else "validate",
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
    updated = run_registry.update_record(record_path, **final)
    try:
        target = str(record["target"])
        checkout = Path(str(record["checkout"]))
        run_started_at = str(record["started_at"])
        repo = canonical_repo(str(record.get("repo", "rrnewton/hermit")))
        row, canonical_verdict, canonical_exit = assert_row_readable_from_canonical_ledger(
            root, target, checkout, run_started_at, repo=repo, run=run
        )
    except (KeyError, RuntimeError, ValueError) as error:
        print(f"validate-run: CANONICAL-VERDICT-UNAVAILABLE: {error}", file=sys.stderr)
        return 2
    updated = run_registry.update_record(
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
            "is appended to ledger/<repo>/<host>/<month>.jsonl ON PARENT MAIN, so read it\n"
            "with `git show origin/main:ledger/...`, not from your working tree."
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
        help="payload wall deadline (default: Hermit 3600s; Reverie 8000s)",
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
        print(f"validate-run: REFUSED: {error}", file=sys.stderr)
        return 2
    if args.caller_poll_seconds <= 0:
        print("validate-run: REFUSED: caller poll seconds must be positive", file=sys.stderr)
        return 2
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
                args.dry_run,
            )
        ):
            print(
                "validate-run: REFUSED: --attach cannot be combined with launch arguments",
                file=sys.stderr,
            )
            return 2
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
            print(f"validate-run: REFUSED: {error}", file=sys.stderr)
            return 2
    if args.checkout is None or not args.agent or not args.target:
        print(
            "validate-run: REFUSED: launch requires --checkout, --agent, and --target",
            file=sys.stderr,
        )
        return 2
    validate_args = list(args.validate_args)
    if validate_args[:1] == ["--"]:
        validate_args.pop(0)
    if args.make_validate and validate_args:
        print(
            "validate-run: REFUSED: --make-validate is the exact no-argument Make "
            "target; do not pass validate arguments after --",
            file=sys.stderr,
        )
        return 2
    if repo == "rrnewton/reverie" and (args.make_validate or validate_args):
        print(
            "validate-run: REFUSED: Reverie always runs its exact full validate.sh under "
            "safe-ci-dag-runner; --make-validate and trailing driver arguments are unsupported",
            file=sys.stderr,
        )
        return 2
    if args.allow_owner_validate_symlink and not (args.in_place and args.make_validate):
        print(
            "validate-run: REFUSED: --allow-owner-validate-symlink requires "
            "--in-place and --make-validate",
            file=sys.stderr,
        )
        return 2
    if args.pr is not None and args.pr <= 0:
        print("validate-run: REFUSED: --pr must be positive", file=sys.stderr)
        return 2
    child_deadline = args.child_deadline or (
        8000 if repo == "rrnewton/reverie" else 3600
    )
    if min(args.wait, args.hold, child_deadline) <= 0:
        print("validate-run: REFUSED: wait/hold/child-deadline must be positive", file=sys.stderr)
        return 2
    if args.ci_dag_jobs is not None and args.ci_dag_jobs <= 0:
        print("validate-run: REFUSED: --ci-dag-jobs must be positive", file=sys.stderr)
        return 2

    fresh_checkout: Path | None = None
    source_checkout: Path | None = None
    private_cargo_home: Path | None = None
    try:
        source_checkout = validate_checkout(
            args.checkout,
            args.target,
            repo=repo,
            run=run,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
        )
        if args.in_place:
            checkout = source_checkout
        else:
            # DEFAULT: validate the COMMIT, not the tree that claims to be at it.
            fresh_checkout = prepare_fresh_checkout(
                source_checkout,
                args.target,
                run=run,
                parent=(root / "ignored").resolve(),
                repo=repo,
            )
            checkout = fresh_checkout
        preflight(root, checkout, args.target, repo=repo, run=run)
        private_cargo_home = prepare_private_cargo_home(
            (root / "ignored").resolve(),
            run=run,
            shared=Path((environment or os.environ).get("HOME", "")) / ".cargo",
        )
        unit = sanitize_unit(args.unit) if args.unit else default_unit(args.agent, args.target)
        log = (args.log or root / "ignored/validate" / f"{unit}.log").resolve()
        record_path = run_registry.record_path(root.resolve(), unit)
        e2e_result_root, runner_log_dir = _validate_output_paths(root.resolve(), unit)
        output_fields = (
            {
                "e2e_result_root": str(e2e_result_root),
                "safe_ci_dag_runner_log_dir": str(runner_log_dir),
            }
            if repo == "rrnewton/hermit"
            else {}
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
            ci_dag_jobs=args.ci_dag_jobs,
            repo=repo,
        )
        if args.dry_run:
            pane = None
        else:
            log.parent.mkdir(parents=True, exist_ok=True)
            run_registry.write_record(
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
            # the box-exclusive validate-lock, and killing it to protect a
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
        print(f"validate-run: REFUSED: {error}", file=sys.stderr)
        return 2

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
        print(
            f"validate-run: WAIT-INTERRUPTED {unit}.service; RUN CONTINUES independently: {error}",
            file=sys.stderr,
        )
        return 2

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
                print(
                    "validate-run: ORPHANED-RECEIPT: this run wrote a receipt into "
                    f"{', '.join(orphans)} inside the temp checkout, where no "
                    f"consumer reads it. Temp checkout RETAINED at {fresh_checkout} "
                    "-- it is the only trace that this happened.",
                    file=sys.stderr,
                )
                return 2
            print(
                "validate-run: NO-RECEIPT-PRODUCED: the run failed before writing a "
                "receipt anywhere, so the temp checkout holds no evidence; "
                "reclaiming it.",
                file=sys.stderr,
            )
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
        remove_private_cargo_home(private_cargo_home, run=run)
        return 2
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
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=args.json,
    )
    return canonical_exit


if __name__ == "__main__":
    raise SystemExit(main())
