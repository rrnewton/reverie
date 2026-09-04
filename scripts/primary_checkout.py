#!/usr/bin/env python3
"""Safely refresh and inspect the parent workspace's primary checkouts."""

from __future__ import annotations

import argparse
from collections.abc import Mapping
from contextlib import contextmanager
from contextvars import ContextVar
from dataclasses import dataclass
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import tomllib
from typing import TextIO


PRODUCTS = ("hermit", "reverie", "liteinst2")
MAIN_REF = "refs/heads/main"
REVERIE_GIT_URL = "https://github.com/rrnewton/reverie.git"
SNAPSHOT_COMMIT_MESSAGE = "Advance product submodules as consistent snapshot"
SNAPSHOT_COMMIT_ROLE = "[coordinator, primary-checkout] [hermit-coord, {machine}]"
SNAPSHOT_AUDIT_REASON = "publish coherent product-main gitlinks"
REVERIE_SOURCE = re.compile(
    rf"^git\+{re.escape(REVERIE_GIT_URL)}\?rev=([0-9a-f]{{40}})#([0-9a-f]{{40}})$"
)
REVERIE_LOCKFILES = (Path("Cargo.lock"), Path("liteinst-runtime-build/Cargo.lock"))
# The LiteInst runtime cache key is DERIVED, not stored.
# `scripts/stage-liteinst-runtime.sh` reads the canonical pin
# (`ci/run-reverie-pin-check.sh --print-pin`) and appends its first 8 hex to the
# caller's base path, so callers pass an UNSUFFIXED base and the key can never
# lag a pin bump.
#
# THIS CHECK USED TO ASSERT THE OPPOSITE. It held a hardcoded four-file list and
# REQUIRED each to contain a literal `liteinst-runtime-<8hex>` equal to the pin.
# Those literals were removed when the suffix moved into the staging script, so
# the check reported `cache keys=none` on all four and blocked the snapshot --
# against a tree that was correct. Worse, the obvious way to "fix" the red was to
# paste the literals back, which would have PASSED the check and REINTRODUCED the
# drift it exists to prevent. A green over hardcoded values is worse than a red.
#
# So the polarity is inverted: a hardcoded suffix is now a VIOLATION, and what is
# asserted is the DERIVATION itself.
REVERIE_STAGING_SCRIPT = Path("scripts/stage-liteinst-runtime.sh")
REVERIE_PIN_DERIVATION = re.compile(r"--print-pin")
# A literal 8-hex suffix anywhere in tracked hermit source. `${reverie_pin:0:8}`
# in the staging script does not match, which is the point.
REVERIE_CACHE_KEY = re.compile(r"liteinst-runtime(?:-build)?-([0-9a-f]{8})")
# The checker that legitimately embeds drifted example tokens in its own
# docstring and fixtures; a check must not scan the file that defines it.
REVERIE_CACHE_SCAN_EXCLUDE = ("scripts/check-reverie-pin.rs",)
FULL_SHA = re.compile(r"[0-9a-f]{40}")
KNOWN_OWNER_VALIDATE_PATH = "validate"
KNOWN_OWNER_VALIDATE_TARGET = "scripts/validate.rs"
HERMIT_AGENT_UTILS_PATH = "agent-utils"

# Repo-scoped git environment variables OVERRIDE `git -C <repo>`, and git exports
# them into every hook child. Measured on this box with a probe pre-commit hook:
#
#   git commit -m m -- <path>   =>  GIT_INDEX_FILE=/ABS/.git/next-index-<pid>.lock
#   git commit / --amend        =>  GIT_INDEX_FILE=.git/index      (relative)
#
# So an unscrubbed `git -C hermit ls-files` inside a pre-commit hook enumerates
# the PARENT's index, after which this module resolves those paths under hermit/.
# That is precisely the 2026-08-06 fleet-wide false block: the parent index holds
# 47 `*Cargo.toml` entries (crates-squat-staging/**, experiments/**,
# shmem_exec_obj/** -- none of which exist in the Hermit repo), every open
# ENOENTed, and check-pins concluded "no tracked Cargo manifest pins", forcing
# every agent onto HERMIT_PIN_DRIFT_OVERRIDE=1. `git -C <repo>` must mean <repo>,
# so scrub these unless a caller explicitly wants the in-flight parent index.
GIT_REPO_SCOPED_ENV = (
    "GIT_INDEX_FILE",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_INDEX_VERSION",
)
# Reads of a recorded gitlink's tree must never trigger a promisor round trip:
# a pre-commit hook has no business going to the network, and the Hermit primary
# carries a partial-clone promisor remote for reverie.
NO_LAZY_FETCH = {"GIT_NO_LAZY_FETCH": "1"}
_COMMAND_DEADLINE: ContextVar[float | None] = ContextVar(
    "primary_checkout_command_deadline", default=None
)
#: Name of the phase currently running a child, so a deadline cut can say WHICH
#: step it killed rather than only which argv. An inner cut that cannot name its
#: phase is the outer kill with extra steps.
_COMMAND_PHASE: ContextVar[str | None] = ContextVar(
    "primary_checkout_command_phase", default=None
)

#: The serialized parent-main commit is the LAST step of a run whose deadline is
#: CUMULATIVE, so it inherits only what the preamble left -- with no floor. It is
#: also not a cheap write: its own pre-commit hook re-enters this module
#: (`primary_checkout.py check` and `check-pins`), so the envelope has to cover
#: the freshness+pin verification TWICE while it was sized for once.
#:
#: Measured 2026-08-10 on devbig014: the whole outer pass 2.42s, and
#: `parent-main-write commit -- hermit reverie liteinst2` 7.08s. This floor gives
#: that phase a named 20s of its own instead of a leftover.
#:
#: NOTHING IS RAISED. It is clamped to the existing envelope, so the 24s inner
#: budget and the 30s outer kill are untouched: 2.42 + 20 = 22.4 < 24 < 30. A
#: pathologically slow preamble can still squeeze this phase, and that is
#: deliberate -- widening the outer bound to fit the work is the trade this
#: whole line of work exists to refuse.
PARENT_MAIN_WRITE_PHASE = "serialized parent-main snapshot commit"
PARENT_MAIN_WRITE_PHASE_BUDGET_SECS = 20.0


def _run_process(
    command: list[str] | tuple[str, ...], **kwargs: object
) -> subprocess.CompletedProcess[str]:
    """Run one owned child inside the active checkout-fresh wall deadline."""
    deadline = _COMMAND_DEADLINE.get()
    if deadline is None:
        return subprocess.run(command, **kwargs)  # type: ignore[arg-type]
    remaining = deadline - time.monotonic()
    # BIND THE PHASE TO THE EXCEPTION AT THE RAISE SITE, not at the handler.
    # `_phase` resets its ContextVar in a `finally`, which runs BEFORE the
    # exception reaches the caller -- so a handler reading the ContextVar always
    # saw None and the cut could not name what it killed. Measured while
    # bracketing this change.
    try:
        if remaining <= 0:
            raise subprocess.TimeoutExpired(command, 0)
        explicit = kwargs.get("timeout")
        if isinstance(explicit, (int, float)):
            remaining = min(remaining, float(explicit))
        kwargs["timeout"] = remaining
        return subprocess.run(command, **kwargs)  # type: ignore[arg-type]
    except subprocess.TimeoutExpired as error:
        error.primary_checkout_phase = _COMMAND_PHASE.get()  # type: ignore[attr-defined]
        raise


@contextmanager
def _phase(name: str, budget_secs: float | None = None):
    """Name the active phase, optionally giving it its own bounded allowance.

    The allowance is a FLOOR clamped to the surrounding envelope: a phase never
    extends the run past the deadline it was given, so this cannot widen the
    outer bound. It only stops one step from silently inheriting a remainder.
    """
    phase_token = _COMMAND_PHASE.set(name)
    deadline_token = None
    if budget_secs is not None:
        outer = _COMMAND_DEADLINE.get()
        proposed = time.monotonic() + budget_secs
        if outer is not None:
            proposed = min(proposed, outer)
        deadline_token = _COMMAND_DEADLINE.set(proposed)
    try:
        yield
    finally:
        if deadline_token is not None:
            _COMMAND_DEADLINE.reset(deadline_token)
        _COMMAND_PHASE.reset(phase_token)


def proxy_disabled(variable: str = "PRIMARY_CHECKOUT_DISABLE_PROXY") -> bool:
    """Has the caller explicitly disabled the proxy?

    Intended semantics, stated here because the name has already misled once:
    the exact string ``"1"`` disables the proxy; ``"0"``, empty, and unset all
    leave it ENABLED.

    This is a VALUE check, not a presence check, and the distinction is load
    bearing. ``os.environ.get(name)`` used as a boolean is true for ``"0"`` --
    ``"0"`` is a non-empty string, and non-empty strings are truthy in Python.
    So the presence form fired for ``DISABLE_PROXY=0``, which is the natural way
    to write "do not disable", and silently turned the proxy OFF. Matches the
    ``env_flag(name, "1")`` contract used elsewhere in the tree; any other value,
    ``"true"`` included, leaves the proxy enabled rather than guessing.
    """
    return os.environ.get(variable) == "1"



def run_git(
    repo: Path,
    *args: str,
    network: bool = False,
    use_proxy: bool = True,
    inherit_repo_env: bool = False,
    env_overrides: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run git against ``repo``.

    ``inherit_repo_env=True`` keeps GIT_INDEX_FILE and friends, which is correct
    only when ``repo`` IS the repository whose in-flight commit set them (see
    GIT_REPO_SCOPED_ENV). Every other call must be scrubbed.
    """
    env = dict(os.environ)
    if not inherit_repo_env:
        for name in GIT_REPO_SCOPED_ENV:
            env.pop(name, None)
    if env_overrides:
        env.update(env_overrides)
    command: list[str] = []
    if network and use_proxy and not proxy_disabled():
        proxy = shutil.which(os.environ.get("WITH_PROXY", "with-proxy"))
        if proxy:
            command.append(proxy)
    command.extend(("git", "-C", str(repo), *args))
    return _run_process(command, text=True, capture_output=True, check=False, env=env)


def print_command_output(result: subprocess.CompletedProcess[str], stream: TextIO) -> None:
    for output in (result.stdout, result.stderr):
        if output:
            print(output.rstrip(), file=stream)


def live_main_sha(
    repo: Path, *, use_proxy: bool = True
) -> tuple[str, subprocess.CompletedProcess[str]]:
    """Resolve the live main identity without downloading its objects.

    The primary-snapshot tick used to fetch every product and then run ``pull``,
    which fetched the same branch a second time even when HEAD was already
    current.  ``ls-remote`` is the cheap cache validator: only an identity miss
    needs an object transfer and work-tree operation.
    """
    result = run_git(
        repo,
        "ls-remote",
        "--exit-code",
        "origin",
        MAIN_REF,
        network=True,
        use_proxy=use_proxy,
    )
    sha = result.stdout.split(maxsplit=1)[0] if result.stdout.strip() else ""
    if not FULL_SHA.fullmatch(sha):
        sha = ""
    return sha, result


def _sync_clean_hermit_agent_utils(
    repo: Path,
    dirty_lines: list[str],
    *,
    use_proxy: bool,
    out: TextIO,
    err: TextIO,
) -> tuple[list[str], bool]:
    """Follow Hermit's recorded agent-utils gitlink without losing local work.

    A superproject fast-forward does not update an initialized submodule's
    working tree.  The next checkout-fresh pass therefore sees `` M
    agent-utils`` and correctly preserves it as dirt, even when the nested
    checkout is merely an older, clean ancestor of the newly recorded gitlink.

    Repair only that exact safe shape.  Nested edits, an ahead/diverged nested
    HEAD, another dirty superproject path, or an unreadable expected commit are
    refusals.  In particular, "clean" alone is not permission to move a commit:
    an unpublished clean commit is work too.
    """

    agent_lines = [
        line
        for line in dirty_lines
        if len(line) >= 4 and line[3:].split(" -> ")[-1] == HERMIT_AGENT_UTILS_PATH
    ]
    if not agent_lines:
        return dirty_lines, False

    nested = repo / HERMIT_AGENT_UTILS_PATH
    nested_status = run_git(
        nested, "status", "--porcelain=v1", "--untracked-files=all"
    )
    if nested_status.returncode != 0:
        print(
            "ERROR: hermit/agent-utils gitlink changed, but the nested checkout "
            "could not be inspected; preserving it.",
            file=err,
        )
        print_command_output(nested_status, err)
        return dirty_lines, True
    nested_dirty = nested_status.stdout.rstrip("\n").splitlines()
    if nested_dirty:
        print(
            "ERROR: hermit/agent-utils contains real working-tree changes; "
            "refusing automatic gitlink synchronization and preserving them:",
            file=err,
        )
        for line in nested_dirty[:20]:
            print(f"  {line}", file=err)
        if len(nested_dirty) > 20:
            print(f"  ... {len(nested_dirty) - 20} more path(s)", file=err)
        return dirty_lines, True

    # Anything beyond the one unstaged gitlink mismatch means the enclosing
    # checkout also carries work.  Preserve the whole checkout rather than
    # partially mutating one path and making the remaining state harder to
    # attribute.
    if dirty_lines != [f" M {HERMIT_AGENT_UTILS_PATH}"]:
        print(
            "ERROR: hermit/agent-utils is not the sole clean gitlink mismatch; "
            "preserving the primary checkout unchanged.",
            file=err,
        )
        return dirty_lines, True

    expected_result = run_git(repo, "rev-parse", f"HEAD:{HERMIT_AGENT_UTILS_PATH}")
    actual_result = run_git(nested, "rev-parse", "HEAD")
    expected = expected_result.stdout.strip()
    actual = actual_result.stdout.strip()
    if (
        expected_result.returncode != 0
        or actual_result.returncode != 0
        or not FULL_SHA.fullmatch(expected)
        or not FULL_SHA.fullmatch(actual)
    ):
        print(
            "ERROR: could not bind hermit/agent-utils working HEAD to the "
            "committed gitlink; preserving it.",
            file=err,
        )
        return dirty_lines, True

    expected_exists = run_git(nested, "cat-file", "-e", f"{expected}^{{commit}}")
    if expected_exists.returncode != 0:
        fetch = run_git(
            nested,
            "fetch",
            "origin",
            "main",
            network=True,
            use_proxy=use_proxy,
        )
        print_command_output(fetch, out if fetch.returncode == 0 else err)
        if fetch.returncode != 0:
            print(
                "ERROR: could not fetch the committed agent-utils gitlink; "
                "preserving the nested checkout.",
                file=err,
            )
            return dirty_lines, True
        expected_exists = run_git(
            nested, "cat-file", "-e", f"{expected}^{{commit}}"
        )
    if expected_exists.returncode != 0:
        print(
            f"ERROR: committed agent-utils gitlink {expected} is unavailable "
            "after fetch; preserving the nested checkout.",
            file=err,
        )
        return dirty_lines, True

    ancestry = run_git(nested, "merge-base", "--is-ancestor", actual, expected)
    if ancestry.returncode != 0:
        print(
            "ERROR: hermit/agent-utils has a clean but unpublished, ahead, or "
            f"divergent HEAD {actual}; it is not an ancestor of committed "
            f"gitlink {expected}. Preserving it.",
            file=err,
        )
        return dirty_lines, True

    update = run_git(
        repo,
        "-c",
        "submodule.agent-utils.update=checkout",
        "submodule",
        "update",
        "--init",
        "--checkout",
        "--",
        HERMIT_AGENT_UTILS_PATH,
        network=True,
        use_proxy=use_proxy,
    )
    print_command_output(update, out if update.returncode == 0 else err)
    if update.returncode != 0:
        print(
            "ERROR: safe agent-utils gitlink synchronization failed; the "
            "checkout was not forced or reset.",
            file=err,
        )
        return dirty_lines, True

    final_nested = run_git(nested, "status", "--porcelain=v1", "--untracked-files=all")
    final_head = run_git(nested, "rev-parse", "HEAD").stdout.strip()
    final_super = run_git(repo, "status", "--porcelain=v1", "--untracked-files=all")
    final_lines = final_super.stdout.rstrip("\n").splitlines()
    if (
        final_nested.returncode != 0
        or final_nested.stdout.strip()
        or final_head != expected
        or final_super.returncode != 0
        or final_lines
    ):
        print(
            "ERROR: agent-utils update did not produce the exact clean committed "
            "gitlink; refusing to call the primary fresh.",
            file=err,
        )
        return final_lines or dirty_lines, True

    print(
        f"hermit/agent-utils: advanced clean nested checkout {actual} -> {expected} "
        "to match the committed gitlink",
        file=out,
    )
    return [], False


def run_parent_main_write(
    root: Path, *args: str, use_proxy: bool = True
) -> subprocess.CompletedProcess[str]:
    """Use the single serialized parent-main commit+push authority."""
    env = dict(os.environ)
    for name in GIT_REPO_SCOPED_ENV:
        env.pop(name, None)
    if not use_proxy:
        env["HERMIT_PARENT_MAIN_NO_PROXY"] = "1"
    helper = Path(__file__).with_name("parent-main-write")
    return _run_process(
        (str(helper), *args),
        cwd=root,
        text=True,
        capture_output=True,
        check=False,
        env=env,
    )


def machine_attributed_snapshot_message(root: Path) -> str:
    """Build the automated snapshot message with this writer's exact machine."""

    checker = root / "scripts" / "check_commit_attribution.py"
    detected = _run_process(
        (str(checker), "--detect-machine"),
        cwd=root,
        text=True,
        capture_output=True,
        check=False,
    )
    if detected.returncode != 0 or not detected.stdout.strip():
        detail = detected.stderr.strip() or "no machine token returned"
        raise RuntimeError(f"cannot attribute product snapshot commit: {detail}")
    return (
        f"{SNAPSHOT_COMMIT_MESSAGE}\n\n"
        f"{SNAPSHOT_COMMIT_ROLE.format(machine=detected.stdout.strip())}"
    )


def _walk_reverie_dependencies(value: object) -> list[str]:
    pins: list[str] = []
    if isinstance(value, Mapping):
        if value.get("git") == REVERIE_GIT_URL and isinstance(value.get("rev"), str):
            pins.append(value["rev"])
        for child in value.values():
            pins.extend(_walk_reverie_dependencies(child))
    elif isinstance(value, list):
        for child in value:
            pins.extend(_walk_reverie_dependencies(child))
    return pins


def indexed_submodule_commit(root: Path, product: str) -> tuple[str | None, str]:
    """Return the ``product`` commit this parent commit will record, and its source.

    Deliberately INHERITS the git environment: inside a pre-commit hook,
    GIT_INDEX_FILE names the exact index being committed -- for the mandated
    pathspec form (``git commit -m msg -- <paths>``) a temporary index built from
    HEAD plus the named paths -- so this yields the gitlink the commit will
    actually record, whether or not the caller staged a submodule bump. Outside a
    hook it reads .git/index. Falls back to HEAD, then to ``(None, reason)``.
    """
    staged = run_git(root, "ls-files", "--stage", "-z", "--", product, inherit_repo_env=True)
    if staged.returncode == 0:
        for entry in filter(None, staged.stdout.split("\0")):
            metadata, _, name = entry.partition("\t")
            fields = metadata.split()
            if name == product and len(fields) >= 2 and fields[0] == "160000":
                return fields[1], "parent index"
    head = run_git(root, "rev-parse", f"HEAD:{product}")
    recorded = head.stdout.strip()
    if head.returncode == 0 and FULL_SHA.fullmatch(recorded):
        return recorded, "parent HEAD"
    return None, f"{product} is not recorded as a gitlink in the parent index or HEAD"


def _tracked_manifest_paths(hermit: Path, commit: str | None) -> tuple[list[str], list[str]]:
    """Tracked ``*Cargo.toml`` paths at ``commit``, or in the working tree if None."""
    if commit is None:
        listed = run_git(hermit, "ls-files", "-z", "--", "*Cargo.toml")
        if listed.returncode != 0:
            return [], ["could not list tracked Hermit Cargo.toml files"]
        return list(filter(None, listed.stdout.split("\0"))), []
    listed = run_git(
        hermit, "ls-tree", "-r", "-z", "--name-only", commit, env_overrides=NO_LAZY_FETCH
    )
    if listed.returncode != 0:
        return [], [f"could not list Cargo.toml files at {commit[:12]}"]
    # `ls-files -- '*Cargo.toml'` globs across '/', so a basename suffix test is
    # the equivalent filter over a flat ls-tree listing.
    return [
        name for name in filter(None, listed.stdout.split("\0")) if name.endswith("Cargo.toml")
    ], []


def _read_tracked(hermit: Path, commit: str | None, relative: Path | str) -> tuple[str | None, str]:
    """Contents of a tracked Hermit file at ``commit``, or from the working tree if None."""
    if commit is None:
        try:
            return (hermit / relative).read_text(), ""
        except OSError as error:
            return None, f"{error}"
    blob = run_git(
        hermit, "cat-file", "blob", f"{commit}:{relative}", env_overrides=NO_LAZY_FETCH
    )
    if blob.returncode != 0:
        return None, (blob.stderr.strip() or f"missing at {commit[:12]}")
    return blob.stdout, ""


def reverie_manifest_pins(
    hermit: Path, commit: str | None = None
) -> tuple[set[str], int, list[str]]:
    """Return exact Reverie revisions from tracked Hermit Cargo manifests.

    ``commit`` selects the recorded submodule tree; None reads the working tree.
    """
    names, errors = _tracked_manifest_paths(hermit, commit)
    pins: list[str] = []
    for relative in names:
        text, failure = _read_tracked(hermit, commit, relative)
        if text is None:
            errors.append(f"could not parse {relative}: {failure}")
            continue
        try:
            pins.extend(_walk_reverie_dependencies(tomllib.loads(text)))
        except tomllib.TOMLDecodeError as error:
            errors.append(f"could not parse {relative}: {error}")
    return set(pins), len(pins), errors


def reverie_generated_pin_errors(hermit: Path, expected: str) -> list[str]:
    """Check generated lock sources and revision-keyed build cache paths."""
    errors: list[str] = []
    for relative in REVERIE_LOCKFILES:
        path = hermit / relative
        try:
            with path.open("rb") as source:
                lock = tomllib.load(source)
        except (OSError, tomllib.TOMLDecodeError) as error:
            errors.append(f"could not parse {relative}: {error}")
            continue

        sources = [
            package.get("source", "")
            for package in lock.get("package", [])
            if isinstance(package, Mapping)
            and str(package.get("source", "")).startswith(f"git+{REVERIE_GIT_URL}")
        ]
        if not sources:
            errors.append(f"{relative}: no Reverie git sources found")
            continue
        for source in sources:
            match = REVERIE_SOURCE.fullmatch(str(source))
            if match is None or match.group(1) != expected or match.group(2) != expected:
                errors.append(f"{relative}: stale Reverie source {source}")

    errors.extend(reverie_cache_derivation_errors(hermit))
    return errors


def reverie_cache_derivation_errors(hermit: Path) -> list[str]:
    """Assert the LiteInst cache key is DERIVED from the pin, and never literal.

    Two halves, and neither alone is sufficient:

      * the staging script must still derive its suffix from the canonical pin --
        without this, someone could delete the derivation and the tree would look
        clean because no literal exists either; and
      * no tracked file may hardcode a `liteinst-runtime-<8hex>` suffix -- without
        this, pasting literals back would satisfy any presence-based check while
        recreating the drift.
    """
    errors: list[str] = []

    # The two halves are INDEPENDENT and both always run. An early return here
    # would let a missing staging script mask a hardcoded literal elsewhere --
    # reporting only the first fault is how a second one stays hidden until the
    # first is cleared, which is exactly the serial-blocker pattern this gate has
    # already produced three times.
    script = hermit / REVERIE_STAGING_SCRIPT
    body = ""
    try:
        body = script.read_text()
    except OSError as error:
        errors.append(f"could not read {REVERIE_STAGING_SCRIPT}: {error}")
    if body and not REVERIE_PIN_DERIVATION.search(body):
        errors.append(
            f"{REVERIE_STAGING_SCRIPT}: no `--print-pin` derivation found; the "
            "LiteInst cache key must be derived from the canonical Reverie pin, "
            "not written literally"
        )

    scan = _run_process(
        [
            "git", "-C", str(hermit), "grep", "-I", "-n", "-E",
            r"liteinst-runtime(-build)?-[0-9a-f]{8}",
            "--", ".",
            *(f":(exclude,top){path}" for path in REVERIE_CACHE_SCAN_EXCLUDE),
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    if scan.returncode not in (0, 1):  # 1 = no matches, which is the good case
        errors.append(f"could not scan for hardcoded cache keys: {scan.stderr.strip()}")
        return errors
    for line in scan.stdout.splitlines():
        errors.append(
            f"{line.split(':', 1)[0]}: hardcoded LiteInst cache key -- the suffix "
            "is appended by scripts/stage-liteinst-runtime.sh from the canonical "
            f"pin; remove the literal ({line.strip()})"
        )
    return errors


# Outcome codes for `publish_parent_snapshot`. DEFERRED is the one that matters:
# it separates "this cannot be done AT THIS INSTANT, ask again" from "something is
# wrong". Collapsing those two into a single failure is what made this a gate that
# could not be satisfied.
SNAPSHOT_PUBLISHED = 0
SNAPSHOT_BLOCKED = 1
SNAPSHOT_DEFERRED = 2


def known_owner_validate_exception(
    product: str, repo: Path, status_lines: list[str]
) -> str | None:
    """Return the disclosed exception path, or fail closed with ``None``.

    This deliberately matches the owner-checkout validation watch's predicate:
    the Hermit primary may contain exactly one untracked ``validate`` symlink,
    pointing at ``scripts/validate.rs``, only while ``make validate`` is proven
    to select the phony target's direct ``./scripts/validate.rs`` recipe.  It is
    an allowed *dirty exception*, never a claim that the checkout is clean.
    """
    if product != "hermit" or status_lines != [f"?? {KNOWN_OWNER_VALIDATE_PATH}"]:
        return None
    link = repo / KNOWN_OWNER_VALIDATE_PATH
    if not link.is_symlink() or os.readlink(link) != KNOWN_OWNER_VALIDATE_TARGET:
        return None
    try:
        makefile = (repo / "Makefile").read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    logical_makefile = makefile.replace("\\\n", " ")
    phony = any(
        line.startswith(".PHONY:")
        and KNOWN_OWNER_VALIDATE_PATH in line.split()[1:]
        for line in logical_makefile.splitlines()
    )
    direct_recipe = "./scripts/validate.rs $(ARGS)" in makefile
    return KNOWN_OWNER_VALIDATE_PATH if phony and direct_recipe else None


def report_known_dirty_exception(product: str, path: str, out: TextIO) -> None:
    print(
        f"KNOWN DIRTY EXCEPTION APPLIED: product={product} path={path}; "
        "checkout_state=dirty-with-known-exception (NOT CLEAN); validate is "
        ".PHONY and invokes ./scripts/validate.rs directly; path preserved",
        file=out,
    )


def publish_parent_snapshot(
    root: Path,
    *,
    use_proxy: bool = True,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    """Commit and push exact product-main gitlinks when the snapshot is coherent.

    Returns ``SNAPSHOT_PUBLISHED`` / ``SNAPSHOT_BLOCKED`` / ``SNAPSHOT_DEFERRED``.

    WHY THERE IS A THIRD OUTCOME. This function used to answer "did it publish?"
    with yes/no, and every no was a HARD WARNING. But most nos are not defects,
    they are races: thirteen agents push parent main, so the reference this
    snapshot must sit on top of moves while the snapshot is being prepared.
    Measured 2026-08-08, parent main advanced five times in about twenty minutes
    (fde6376f -> 82a4c328 -> a6d0ed99 -> 08ce5ca0 -> 119aa11b -> a20ca763). An
    agent dispatched to "make the gate stop firing" is chasing a target that
    moves faster than it can be caught, and one nearly looped forever doing it.

    WHAT THE REFUSAL ACTUALLY PROTECTS, which is the thing to bind to. The hazard
    is committing this gitlink snapshot onto a STALE parent base: published, that
    is either rejected non-fast-forward or, reconciled carelessly, reverts
    everything landed between the stale base and the tip. This repository has
    already lost 45 commits exactly that way, so the hazard is real.

    IT IS ALREADY PREVENTED ONE LAYER DOWN, ATOMICALLY. `scripts/parent-main-write`
    takes an exclusive lock and freshly fetches origin/main. When this checkout is
    merely behind, its `sync --automatic` mode advances only if there are zero
    local commits and every dirty regular path is byte-identical to the fetched
    target; ambiguity refuses for human inspection. The later commit still
    requires local main to equal the fetched value, pushes under CAS and verifies
    ancestry. A currency precheck here would be a SECOND, WEAKER COPY run OUTSIDE
    the lock -- precisely where it can race. AGENTS.md is explicit: one verifier
    per authority, called by every consumer. `parent-main-write` remains that
    verifier for both catch-up and publication.

    WHAT STAYS AN IMMEDIATE HARD WARNING: incoherence that no amount of retrying
    fixes -- a dirty primary, a primary off main, a primary not at its own
    origin/main, or an inconsistent Reverie pin. Those are not moving references.
    """
    heads: dict[str, str] = {}
    allowed_dirty: dict[str, str] = {}
    failures: list[str] = []
    for product in PRODUCTS:
        repo = root / product
        status = run_git(repo, "status", "--porcelain=v1", "--untracked-files=all")
        branch = run_git(repo, "branch", "--show-current")
        head = run_git(repo, "rev-parse", "HEAD")
        remote = run_git(repo, "rev-parse", "origin/main")
        status_lines = status.stdout.rstrip("\n").splitlines()
        exception = known_owner_validate_exception(product, repo, status_lines)
        if status.returncode != 0 or (status_lines and exception is None):
            failures.append(f"{product}: primary is dirty; parent snapshot not advanced")
            continue
        if branch.returncode != 0 or branch.stdout.strip() != "main":
            failures.append(f"{product}: primary is not on main")
            continue
        if exception is not None:
            allowed_dirty[product] = exception
            report_known_dirty_exception(product, exception, out)
        head_sha = head.stdout.strip()
        remote_sha = remote.stdout.strip()
        if head.returncode != 0 or remote.returncode != 0 or head_sha != remote_sha:
            failures.append(f"{product}: primary HEAD does not equal fetched origin/main")
            continue
        heads[product] = head_sha

    if not failures and len(heads) == len(PRODUCTS):
        pins, pin_count, pin_errors = reverie_manifest_pins(root / "hermit")
        failures.extend(f"hermit: {message}" for message in pin_errors)
        expected = heads["reverie"]
        if pin_count == 0:
            failures.append("hermit: no tracked Cargo manifest pins rrnewton/reverie")
        elif len(pins) > 1:
            # INTERNAL INCOHERENCE: the manifests contradict each other. No pin
            # value could satisfy them, so this is unfixable by any bump and is
            # never a moving-reference complaint.
            failures.append(
                "Hermit Reverie pins disagree with EACH OTHER across tracked "
                f"Cargo.toml: {', '.join(sorted(pins))}"
            )
        elif pins != {expected}:
            # SNAPSHOT INCOHERENCE: the manifests agree with each other but name a
            # different Reverie than the gitlink this snapshot would record. Say
            # that, rather than "not globally consistent" -- the manifests ARE
            # globally consistent here, and calling them inconsistent sent readers
            # hunting for a disagreement that does not exist.
            #
            # This is EXACT on purpose and must stay exact. `expected` is the
            # gitlink about to be recorded; publishing hermit-pins-X alongside
            # reverie-gitlink-Y hands a colleague who runs `git submodule update
            # --init` a mismatched pair. A small gap is not less broken than a
            # large one, so this is NOT the place for a staleness bound.
            #
            # It nevertheless reads as a treadmill, and the cause is one level up:
            # an earlier loop requires every primary HEAD to equal its own
            # origin/main, so `expected` IS the upstream tip. Coherence-with-the-
            # gitlink therefore composes into currency-with-the-tip. Bound the
            # CURRENCY leg where it lives (that loop, and
            # check_reverie_pin_invariant.py leg C) -- not here.
            behind = run_git(
                root / "reverie", "rev-list", "--count", f"{next(iter(pins))}..{expected}"
            )
            n = behind.stdout.strip() if behind.returncode == 0 else ""
            distance = f"{n} commit{'' if n == '1' else 's'}" if n else "an unknown number of commits"
            failures.append(
                "Hermit Reverie pin lags the reverie gitlink this snapshot would "
                f"record: manifests all agree at {next(iter(pins))}, gitlink/"
                f"reverie-main={expected} ({distance} ahead). The manifests "
                "are consistent with each other; only the recorded gitlink differs."
            )
        failures.extend(
            f"hermit: {message}"
            for message in reverie_generated_pin_errors(root / "hermit", expected)
        )

    if failures:
        print("HARD WARNING: PARENT SUBMODULE SNAPSHOT NOT PUBLISHED", file=err)
        for failure in failures:
            print(f"  {failure}", file=err)
        return SNAPSHOT_BLOCKED

    parent_branch = run_git(root, "branch", "--show-current").stdout.strip()
    parent_head = run_git(root, "rev-parse", "HEAD").stdout.strip()
    parent_remote = run_git(root, "rev-parse", "origin/main").stdout.strip()
    live_parent, remote_query = live_main_sha(root, use_proxy=use_proxy)
    if remote_query.returncode != 0 or not live_parent:
        print_command_output(remote_query, err)
        print("ERROR: parent live origin/main query failed; snapshot not published.", file=err)
        return SNAPSHOT_BLOCKED
    if parent_remote != live_parent:
        fetch = run_git(
            root, "fetch", "origin", "main", network=True, use_proxy=use_proxy
        )
        print_command_output(fetch, out if fetch.returncode == 0 else err)
        if fetch.returncode != 0:
            print("ERROR: parent origin/main fetch failed; snapshot not published.", file=err)
            return SNAPSHOT_BLOCKED
        parent_remote = run_git(root, "rev-parse", "origin/main").stdout.strip()

    # ASK "IS THERE ANYTHING TO PUBLISH?" BEFORE ASKING "MAY I PUBLISH?".
    #
    # This ordering is the fix for a false page with no underlying defect. The
    # currency refusal used to run FIRST, so a parent that was merely a commit or
    # two behind hard-warned even when the published gitlinks were already exactly
    # right and there was nothing whatsoever to commit. That is why the gate could
    # be observed firing while every sample showed HEAD == freshly fetched
    # origin/main: by the time anyone looked, the snapshot was a no-op.
    #
    # The comparison is against `origin/main:<product>` -- what is PUBLISHED --
    # not `HEAD:<product>`, which is only the local tree's view and is itself
    # stale whenever the parent is behind. Asking the stale copy whether the
    # published state is current is the same category of error one level down.
    published_gitlinks = {
        product: run_git(root, "rev-parse", f"origin/main:{product}").stdout.strip()
        for product in PRODUCTS
    }
    if published_gitlinks == heads:
        print(
            "Parent product snapshot already current on origin/main: "
            + ", ".join(f"{name}={heads[name][:12]}" for name in PRODUCTS),
            file=out,
        )
        return SNAPSHOT_PUBLISHED

    # Yield BEFORE automatic sync when a gitlink is already staged.  Index state
    # carries no actor identity, so it is evidence of an in-flight transaction
    # but never evidence that "another coordinator" owns it.  Preserve it and
    # report the attribution honestly as unknown.
    staged = run_git(root, "diff", "--cached", "--quiet", "--", *PRODUCTS)
    if staged.returncode != 0:
        print(
            "DEFERRED: product gitlinks are already staged with unknown ownership; "
            "preserving them and yielding. Index state does not identify an actor.",
            file=out,
        )
        return SNAPSHOT_DEFERRED

    if parent_branch != "main" or not parent_head:
        print(
            "DEFERRED: automatic parent sync requires branch=main and a readable "
            f"HEAD (branch={parent_branch or 'DETACHED'}). Human inspection required; "
            "no raw pull or ref rewrite was attempted.",
            file=out,
        )
        return SNAPSHOT_DEFERRED

    if parent_head != parent_remote:
        behind = run_git(
            root, "rev-list", "--count", f"{parent_head}..{parent_remote}"
        ).stdout.strip() or "?"
        ahead = run_git(
            root, "rev-list", "--count", f"{parent_remote}..{parent_head}"
        ).stdout.strip() or "?"
        if ahead not in ("0", "?"):
            print(
                "DEFERRED: automatic parent sync REFUSED local commits "
                f"(ahead={ahead} behind={behind} HEAD={parent_head[:12]} "
                f"origin/main={parent_remote[:12]}). Human inspection required; "
                "automation never reconciles, rebases, or rewrites them.",
                file=out,
            )
            return SNAPSHOT_DEFERRED

        with _phase("serialized parent-main automatic sync"):
            automatic_sync = run_parent_main_write(
                root, "sync", "--automatic", use_proxy=use_proxy
            )
        print_command_output(automatic_sync, out)
        if automatic_sync.returncode != 0:
            print(
                "DEFERRED: registered automatic parent sync REFUSED. The writer's "
                "exact-target classifier found ambiguity or concurrent ownership; "
                "human inspection is required. No raw pull was attempted.",
                file=out,
            )
            return SNAPSHOT_DEFERRED

        parent_branch = run_git(root, "branch", "--show-current").stdout.strip()
        parent_head = run_git(root, "rev-parse", "HEAD").stdout.strip()
        parent_remote = run_git(root, "rev-parse", "origin/main").stdout.strip()
        if parent_branch != "main" or not parent_head or parent_head != parent_remote:
            print(
                "DEFERRED: registered automatic sync completed but parent currency "
                f"moved again (branch={parent_branch or 'DETACHED'} "
                f"HEAD={parent_head[:12] or 'unknown'} "
                f"origin/main={parent_remote[:12] or 'unknown'}). Retrying next tick.",
                file=out,
            )
            return SNAPSHOT_DEFERRED

        # The writer fetched under its lock. Re-read what is now published: a
        # concurrent snapshot may have completed the work while we caught up.
        published_gitlinks = {
            product: run_git(root, "rev-parse", f"origin/main:{product}").stdout.strip()
            for product in PRODUCTS
        }
        if published_gitlinks == heads:
            print(
                "Parent product snapshot became current during registered auto-sync: "
                + ", ".join(f"{name}={heads[name][:12]}" for name in PRODUCTS),
                file=out,
            )
            return SNAPSHOT_PUBLISHED

    # Recheck after the sync boundary: something may stage a gitlink while the
    # registered writer is active. Yield instead of absorbing it, without
    # inventing an owner that the index cannot prove.
    staged = run_git(root, "diff", "--cached", "--quiet", "--", *PRODUCTS)
    if staged.returncode != 0:
        print(
            "DEFERRED: product gitlinks became staged with unknown ownership; "
            "preserving them and yielding. Index state does not identify an actor.",
            file=out,
        )
        return SNAPSHOT_DEFERRED

    # DO NOT stage product gitlinks in the shared parent index here.  The
    # serialized writer commits an explicit pathspec, for which git builds the
    # commit index from HEAD plus those paths.  Eagerly running `git add` bought
    # no stronger binding: a product could still move before the pathspec commit.
    # It did, however, leave our entries in the shared index whenever the writer
    # lost a race.  The next tick then treated our own residue as foreign work and
    # could never retry.  Keeping the shared index byte-identical makes every
    # DEFERRED return residue-free by construction.
    for product in PRODUCTS:
        current_head = run_git(root / product, "rev-parse", "HEAD").stdout.strip()
        if current_head != heads[product]:
            # The primary advanced after validation. Another moving reference:
            # refusing to publish the mismatch is right, paging is not.
            print(
                f"DEFERRED: validated primary moved before parent publication; "
                f"{product} current={current_head or 'missing'} "
                f"validated={heads[product]}. "
                "Re-validating next tick.",
                file=out,
            )
            return SNAPSHOT_DEFERRED

    try:
        snapshot_message = machine_attributed_snapshot_message(root)
    except RuntimeError as error:
        print(f"ERROR: {error}", file=err)
        return SNAPSHOT_BLOCKED

    with _phase(
        PARENT_MAIN_WRITE_PHASE,
        PARENT_MAIN_WRITE_PHASE_BUDGET_SECS,
    ):
        publish = run_parent_main_write(
            root,
            "commit",
            "-m",
            snapshot_message,
            "--audit-reason",
            SNAPSHOT_AUDIT_REASON,
            "--",
            *PRODUCTS,
            use_proxy=use_proxy,
        )
    print_command_output(publish, out if publish.returncode == 0 else err)
    if publish.returncode != 0:
        # parent-main-write is the single verifier of currency, and it refuses for
        # two very different reasons that its exit code does not separate: lock
        # contention with another writer, and the base moving under it. Both are
        # races and both clear on their own. A durable failure is caught by the
        # tick's persistence tracking rather than by paging on the first loss.
        print(
            "DEFERRED: serialized parent snapshot publication did not complete "
            "(lock contention or the base moved under the write). No local commit "
            "was force-pushed or rewritten; retrying next tick.",
            file=out,
        )
        return SNAPSHOT_DEFERRED
    snapshot = run_git(root, "rev-parse", "HEAD").stdout.strip()
    print(
        f"Published parent snapshot {snapshot}: "
        + ", ".join(f"{name}={heads[name]}" for name in PRODUCTS)
        + (
            "; known_dirty_exceptions="
            + ",".join(f"{name}:{path}" for name, path in allowed_dirty.items())
            if allowed_dirty
            else "; known_dirty_exceptions=none"
        ),
        file=out,
    )
    return SNAPSHOT_PUBLISHED


def _checkout_fresh_impl(
    root: Path,
    *,
    publish_parent: bool = False,
    strict: bool = False,
    use_proxy: bool = True,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    failures = 0
    skipped = 0
    for product in PRODUCTS:
        repo = root / product
        if not (repo / ".git").exists():
            print(f"ERROR: primary checkout is not initialized: {repo}", file=err)
            failures += 1
            continue

        status = run_git(repo, "status", "--porcelain=v1", "--untracked-files=all")
        if status.returncode != 0:
            print(f"ERROR: could not inspect {product}:", file=err)
            print_command_output(status, err)
            failures += 1
            continue
        dirty_lines = status.stdout.rstrip("\n").splitlines()
        if product == "hermit":
            dirty_lines, nested_refused = _sync_clean_hermit_agent_utils(
                repo,
                dirty_lines,
                use_proxy=use_proxy,
                out=out,
                err=err,
            )
            if nested_refused:
                failures += 1
                continue
        exception = known_owner_validate_exception(product, repo, dirty_lines)
        if dirty_lines and exception is None:
            print(
                f"WARNING: {product} is dirty; preserving it and skipping checkout-fresh:",
                file=err,
            )
            for line in dirty_lines[:20]:
                print(f"  {line}", file=err)
            if len(dirty_lines) > 20:
                print(f"  ... {len(dirty_lines) - 20} more path(s)", file=err)
            skipped += 1
            continue

        branch = run_git(repo, "branch", "--show-current").stdout.strip()
        if exception is not None and branch != "main":
            print(
                f"ERROR: {product} known dirty exception was NOT applied because "
                f"branch={branch or 'DETACHED'}, expected main; path preserved",
                file=err,
            )
            failures += 1
            continue
        if exception is not None:
            report_known_dirty_exception(product, exception, out)
        head = run_git(repo, "rev-parse", "HEAD").stdout.strip()
        remote = run_git(repo, "rev-parse", "origin/main").stdout.strip()
        live_remote, remote_query = live_main_sha(repo, use_proxy=use_proxy)
        if remote_query.returncode != 0 or not live_remote:
            print(f"ERROR: {product} live origin/main query failed:", file=err)
            print_command_output(remote_query, err)
            failures += 1
            continue

        if branch == "main" and head and head == remote == live_remote:
            print(
                f"{product}: main is current at {head} (live identity checked)",
                file=out,
            )
            continue

        print(f"Refreshing {product}...", file=out)
        fetch = run_git(repo, "fetch", "origin", "main", network=True, use_proxy=use_proxy)
        print_command_output(fetch, out if fetch.returncode == 0 else err)
        if fetch.returncode != 0:
            print(f"ERROR: {product} fetch failed; checkout left unchanged.", file=err)
            failures += 1
            continue

        local_main = run_git(repo, "show-ref", "--verify", "--quiet", MAIN_REF)
        checkout_args = ("checkout", "main")
        if local_main.returncode != 0:
            checkout_args = ("checkout", "-b", "main", "--track", "origin/main")
        checkout = run_git(repo, *checkout_args)
        print_command_output(checkout, out if checkout.returncode == 0 else err)
        if checkout.returncode != 0:
            print(f"ERROR: {product} could not check out main.", file=err)
            failures += 1
            continue

        # Reuse the tracking ref populated by the fetch above. `git pull` would
        # perform a second fetch of the same branch and was the dominant
        # multiplicative source of SCM telemetry during the 30-minute tick.
        fast_forward = run_git(repo, "merge", "--ff-only", "origin/main")
        print_command_output(
            fast_forward, out if fast_forward.returncode == 0 else err
        )
        if fast_forward.returncode != 0:
            print(f"ERROR: {product} could not fast-forward main.", file=err)
            failures += 1
            continue

        # `git merge --ff-only` advances a superproject gitlink but deliberately
        # does not move an initialized submodule working tree.  Reconcile the
        # one proven-safe nested case before the checkout can be reported clean
        # or used for a parent snapshot.
        post_status = run_git(
            repo, "status", "--porcelain=v1", "--untracked-files=all"
        )
        if post_status.returncode != 0:
            print(f"ERROR: could not inspect {product} after fast-forward:", file=err)
            print_command_output(post_status, err)
            failures += 1
            continue
        post_dirty = post_status.stdout.rstrip("\n").splitlines()
        if product == "hermit":
            post_dirty, nested_refused = _sync_clean_hermit_agent_utils(
                repo,
                post_dirty,
                use_proxy=use_proxy,
                out=out,
                err=err,
            )
            if nested_refused:
                failures += 1
                continue
        post_exception = known_owner_validate_exception(product, repo, post_dirty)
        if post_dirty and post_exception is None:
            print(
                f"ERROR: {product} became dirty during checkout-fresh; preserving it:",
                file=err,
            )
            for line in post_dirty[:20]:
                print(f"  {line}", file=err)
            failures += 1
            continue

        branch = run_git(repo, "branch", "--show-current").stdout.strip()
        head = run_git(repo, "rev-parse", "HEAD").stdout.strip()
        remote = run_git(repo, "rev-parse", "origin/main").stdout.strip()
        if branch != "main" or not head or head != remote:
            print(
                f"ERROR: {product} ended at branch={branch or 'DETACHED'} "
                f"HEAD={head or 'unknown'} origin/main={remote or 'unknown'}; no reset performed.",
                file=err,
            )
            failures += 1
            continue
        print(f"{product}: main is current at {head}", file=out)
    deferred = False
    if publish_parent and failures == 0 and skipped == 0:
        outcome = publish_parent_snapshot(root, use_proxy=use_proxy, out=out, err=err)
        if outcome == SNAPSHOT_DEFERRED:
            deferred = True
        elif outcome != SNAPSHOT_PUBLISHED:
            failures += 1
    elif publish_parent and skipped:
        print(
            "HARD WARNING: parent snapshot not published because a primary checkout "
            "was dirty and preserved.",
            file=err,
        )
    if failures or (strict and skipped):
        return SNAPSHOT_BLOCKED
    # A deferral is surfaced as its own code so the tick can track how long it has
    # persisted. Folding it into either 0 or 1 loses the only thing that separates
    # a lost race from a stuck snapshot.
    return SNAPSHOT_DEFERRED if deferred else SNAPSHOT_PUBLISHED


def checkout_fresh(
    root: Path,
    *,
    publish_parent: bool = False,
    strict: bool = False,
    use_proxy: bool = True,
    deadline_secs: float | None = None,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    """Refresh primary checkouts within one optional wall-clock envelope.

    The operational-health caller sits behind tick-hub's 30-second guillotine.
    Its smaller inner deadline makes a stalled Git/proxy child a named blocked
    result rather than letting the outer runner erase the cause.  Standalone
    callers retain the historical unbounded behavior unless they opt in.
    """
    token = None
    if deadline_secs is not None:
        token = _COMMAND_DEADLINE.set(
            time.monotonic() + max(0.0, deadline_secs)
        )
    try:
        return _checkout_fresh_impl(
            root,
            publish_parent=publish_parent,
            strict=strict,
            use_proxy=use_proxy,
            out=out,
            err=err,
        )
    except subprocess.TimeoutExpired as error:
        command = error.cmd
        rendered = (
            " ".join(str(part) for part in command)
            if isinstance(command, (list, tuple))
            else str(command)
        )
        phase = getattr(error, "primary_checkout_phase", None) or _COMMAND_PHASE.get()
        snapshot_state, snapshot = _snapshot_state_after_timeout(root, phase)
        if snapshot_state == "published":
            print(
                f"SUCCESS: parent snapshot {snapshot} was committed and published "
                "before the wrapper deadline expired; no retry is required.",
                file=out,
            )
            print(
                "WARNING: primary checkout snapshot exceeded its inner deadline "
                "after publication; reporting the child's durable outcome rather "
                "than the wrapper timeout.",
                file=err,
            )
            _print_timed_child_output(error, err)
            return SNAPSHOT_PUBLISHED
        if snapshot_state == "local-unpublished":
            print(
                "ERROR: LOCAL COMMIT EXISTS BUT origin/main DOES NOT CONTAIN IT.",
                file=err,
            )
            print(
                f"Snapshot {snapshot} was committed locally, but publication did not "
                "complete before the serialized parent-main deadline.",
                file=err,
            )
            _print_timed_child_output(error, err)
            return SNAPSHOT_BLOCKED
        print(
            "ERROR: primary checkout snapshot inner deadline exhausted"
            + (f" in phase [{phase}]" if phase else "")
            + f" while running: {rendered}",
            file=err,
        )
        _print_timed_child_output(error, err)
        return SNAPSHOT_BLOCKED
    finally:
        if token is not None:
            _COMMAND_DEADLINE.reset(token)


def _snapshot_state_after_timeout(root: Path, phase: str | None) -> tuple[str, str]:
    """Classify a timed-out snapshot as published, local-only, or unknown.

    This is deliberately local and read-only. A successful push updates the
    remote-tracking ref; requiring the committed local main to be its ancestor
    keeps a preserved success transcript from becoming authority by itself.
    Clearing the expired context only permits these bounded local observations
    -- it does not extend or relaunch the child.
    """

    if phase != PARENT_MAIN_WRITE_PHASE:
        return "unknown", ""
    token = _COMMAND_DEADLINE.set(None)
    try:
        branch = run_git(root, "branch", "--show-current")
        head = run_git(root, "rev-parse", "HEAD")
        published = run_git(root, "rev-parse", "refs/remotes/origin/main")
        subject = run_git(root, "show", "-s", "--format=%s", "HEAD")
    finally:
        _COMMAND_DEADLINE.reset(token)
    observations = (branch, head, published, subject)
    if any(result.returncode != 0 for result in observations):
        return "unknown", ""
    head_sha = head.stdout.strip()
    if (
        branch.stdout.strip() != "main"
        or not FULL_SHA.fullmatch(head_sha)
        or subject.stdout.strip() != SNAPSHOT_COMMIT_MESSAGE
    ):
        return "unknown", ""
    token = _COMMAND_DEADLINE.set(None)
    try:
        contains = run_git(
            root,
            "merge-base",
            "--is-ancestor",
            head_sha,
            "refs/remotes/origin/main",
        )
    finally:
        _COMMAND_DEADLINE.reset(token)
    if contains.returncode == 0:
        return "published", head_sha
    if contains.returncode == 1:
        return "local-unpublished", head_sha
    return "unknown", ""


def _print_timed_child_output(error: subprocess.TimeoutExpired, stream: TextIO) -> None:
    """Preserve child output without interpreting its prose as authority."""

    # `capture_output=True` leaves both streams on the exception. The durable
    # repository boundary above decides success; these bytes remain diagnostic.
    for label, output in (("stdout", error.stdout), ("stderr", error.stderr)):
        if not output:
            continue
        text = output.decode("utf-8", "replace") if isinstance(output, bytes) else output
        text = text.strip()
        if not text:
            continue
        print(f"--- timed child {label} (preserved) ---", file=stream)
        print(text, file=stream)


@dataclass(frozen=True)
class Drift:
    """One way a primary checkout is not in its expected clean state.

    ``kind`` is a stable slug so a caller can act on the class rather than
    parse prose; ``detail`` says what was observed; ``remediation`` is the exact
    command a human should run. ``safe_fix`` marks the *only* class this tool
    will repair on its own (see ``restore_safe`` below).
    """

    primary: str
    kind: str
    detail: str
    remediation: str
    safe_fix: bool = False
    # argv for the repair, when one is safe. Kept as a list so the repair never
    # goes through a shell -- a primary path is attacker-irrelevant but
    # space-and-metacharacter relevant, and `remediation` is prose for humans.
    fix_argv: tuple[str, ...] = ()


# The freshness invariant applies to the PARENT as well as the products. The
# parent is a primary checkout too -- "local main diverged" happened there -- and
# omitting it is why that symptom was never caught by a check.
def primary_paths(root: Path) -> list[tuple[str, Path]]:
    return [("parent", root)] + [(p, root / p) for p in PRODUCTS]


def _rev(repo: Path, *args: str) -> str | None:
    result = run_git(repo, *args)
    return result.stdout.strip() if result.returncode == 0 else None


def inspect_primary(
    root: Path,
    name: str,
    repo: Path,
    *,
    expected_branch: str = "main",
    use_proxy: bool = True,
) -> tuple[list[Drift], str | None]:
    """Evaluate the freshness invariant for one primary.

    FRESH iff: initialized, not bare-flipped, on ``expected_branch`` (not
    detached), exactly equal to that branch on origin, and clean.

    Returns (drifts, head). An empty drift list means fresh.
    """
    drifts: list[Drift] = []
    if not (repo / ".git").exists():
        return (
            [
                Drift(
                    name,
                    "uninitialized",
                    f"{repo} has no .git",
                    "git submodule update --init --recursive",
                )
            ],
            None,
        )

    # BARE FLIP. This must come first, and it must be asked of the RUNNING repo
    # rather than inferred from the presence of .git -- under core.bare=true the
    # directory still has .git, `branch --show-current` still answers, and
    # `rev-parse HEAD` still answers, so a refs-only check sees a perfectly
    # healthy primary while every work-tree operation fails. Measured, not
    # assumed. Note `git -c core.bare=false ...` does NOT override it.
    bare_probe = run_git(repo, "rev-parse", "--is-bare-repository")
    is_bare = bare_probe.stdout.strip() if bare_probe.returncode == 0 else None
    if is_bare not in ("true", "false"):
        return (
            [
                Drift(
                    name,
                    "unknown",
                    "could not determine whether the checkout is bare",
                    f"git -C {repo} rev-parse --is-bare-repository",
                )
            ],
            None,
        )
    if is_bare == "true":
        tracked = run_git(repo, "ls-files")
        has_worktree_files = any(
            (repo / line).exists() for line in tracked.stdout.split("\n")[:50] if line
        )
        detail = "core.bare=true"
        if has_worktree_files:
            detail += " while tracked working-tree files are present (accidental flip)"
        return (
            [
                Drift(
                    name,
                    "bare",
                    detail,
                    f"git -C {repo} config core.bare false",
                    # The only auto-repair: it rewrites one config flag, touches
                    # no ref, no index and no file content, and is exactly
                    # reversible. Nothing else here is unambiguous enough.
                    safe_fix=True,
                    fix_argv=("git", "-C", str(repo), "config", "core.bare", "false"),
                )
            ],
            None,
        )

    head = _rev(repo, "rev-parse", "HEAD")
    branch = _rev(repo, "branch", "--show-current")
    if head is None or branch is None:
        return (
            [Drift(name, "unknown", "could not read HEAD/branch", f"git -C {repo} status")],
            None,
        )

    if not branch:
        drifts.append(
            Drift(
                name,
                "detached",
                f"HEAD detached at {head[:12]}",
                f"git -C {repo} checkout {expected_branch}",
            )
        )
    elif branch != expected_branch:
        drifts.append(
            Drift(
                name,
                "wrong-branch",
                f"on {branch!r}, expected {expected_branch!r}",
                f"git -C {repo} checkout {expected_branch}",
            )
        )

    dirty = run_git(repo, "status", "--porcelain", "--ignore-submodules=all")
    staged = run_git(
        repo, "diff", "--cached", "--quiet", "--ignore-submodules=none", "--"
    )
    if dirty.returncode != 0 or staged.returncode not in (0, 1):
        drifts.append(
            Drift(
                name,
                "unknown",
                "could not inspect working-tree/index status",
                f"git -C {repo} status --short",
            )
        )
    elif dirty.stdout.strip() or staged.returncode == 1:
        drifts.append(
            Drift(
                name,
                "dirty",
                "uncommitted path(s), including any staged gitlink, are present",
                # Never propose discarding: the changes may be another agent's.
                f"attribute the changes first: git -C {repo} status --short",
            )
        )

    remote_ref = f"refs/heads/{expected_branch}"
    ls = run_git(repo, "ls-remote", "--exit-code", "origin", remote_ref, network=True, use_proxy=use_proxy)
    remote = ls.stdout.split(maxsplit=1)[0] if ls.stdout.strip() else ""
    if ls.returncode != 0 or not remote:
        drifts.append(
            Drift(
                name,
                "unknown",
                f"could not query live origin/{expected_branch}",
                f"with-proxy git -C {repo} ls-remote origin {remote_ref}",
            )
        )
        return drifts, head

    if head == remote:
        return drifts, head

    # HEAD differs from origin. Classify it: "differs" is not actionable, and
    # behind / ahead / diverged need three different responses. Classify only if
    # the remote commit is present locally -- otherwise we cannot prove the
    # relationship and must say so rather than guess.
    if run_git(repo, "cat-file", "-e", f"{remote}^{{commit}}").returncode != 0:
        drifts.append(
            Drift(
                name,
                "unclassified-drift",
                f"HEAD {head[:12]} != origin/{expected_branch} {remote[:12]}; "
                "the remote commit is not present locally, so behind/ahead/diverged "
                "cannot be determined",
                f"with-proxy git -C {repo} fetch origin {expected_branch}, then re-run",
            )
        )
        return drifts, head

    relationship = run_git(
        repo, "rev-list", "--left-right", "--count", f"{remote}...{head}"
    )
    parts = relationship.stdout.split() if relationship.returncode == 0 else []
    try:
        behind, ahead = (int(parts[0]), int(parts[1])) if len(parts) == 2 else (0, 0)
    except ValueError:
        behind, ahead = (0, 0)
    if relationship.returncode != 0 or len(parts) != 2 or (behind == 0 and ahead == 0):
        drifts.append(
            Drift(
                name,
                "unknown",
                f"could not classify HEAD relative to origin/{expected_branch}",
                f"git -C {repo} rev-list --left-right --count {remote}...{head}",
            )
        )
        return drifts, head

    if behind and not ahead:
        drifts.append(
            Drift(
                name,
                "behind",
                f"{behind} commit(s) behind origin/{expected_branch} "
                f"({head[:12]} is a strict ancestor of {remote[:12]})",
                # A fast-forward moves the working tree of a shared integration
                # surface with live sibling processes and dependent worktrees, so
                # it is reported, never performed here.
                f"under quiescence: git -C {repo} merge --ff-only origin/{expected_branch}",
            )
        )
    elif ahead and not behind:
        drifts.append(
            Drift(
                name,
                "ahead",
                f"{ahead} unpushed local commit(s) on {expected_branch}",
                f"review then publish: git -C {repo} log origin/{expected_branch}..HEAD",
            )
        )
    else:
        drifts.append(
            Drift(
                name,
                "diverged",
                f"{behind} behind / {ahead} ahead of origin/{expected_branch} "
                "-- histories have forked",
                # Explicitly not a reset: see the no-auto-reset rule.
                f"reconcile by merge in a worktree; never reset/force: "
                f"git -C {repo} log --oneline --left-right origin/{expected_branch}...HEAD",
            )
        )
    return drifts, head


def primary_freshness_report(
    root: Path, *, expected_branch: str = "main", use_proxy: bool = True
) -> tuple[list[Drift], list[str]]:
    """The single freshness invariant, evaluated over every primary checkout."""
    drifts: list[Drift] = []
    fresh: list[str] = []
    for name, repo in primary_paths(root):
        found, head = inspect_primary(
            root, name, repo, expected_branch=expected_branch, use_proxy=use_proxy
        )
        if found:
            drifts.extend(found)
        elif head:
            fresh.append(f"{name}={head[:12]}")
    return drifts, fresh


def check_primary_freshness(
    root: Path,
    *,
    strict: bool = True,
    restore_safe: bool = False,
    expected_branch: str = "main",
    use_proxy: bool = True,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    """Detect (and report) primary-checkout drift.

    Exit codes: 0 every primary fresh; 1 drift detected; 2 at least one primary
    could not be evaluated -- nothing was proven, which is NOT a pass.
    """
    drifts, fresh = primary_freshness_report(
        root, expected_branch=expected_branch, use_proxy=use_proxy
    )

    if restore_safe:
        remaining: list[Drift] = []
        for drift in drifts:
            if not drift.safe_fix or not drift.fix_argv:
                remaining.append(drift)
                continue
            repaired = _run_process(
                list(drift.fix_argv), text=True, capture_output=True, check=False
            )
            if repaired.returncode == 0:
                print(f"RESTORED {drift.primary}: {drift.kind} ({drift.detail})", file=out)
            else:
                remaining.append(drift)
                print(f"RESTORE FAILED {drift.primary}: {drift.kind}", file=err)
        if len(remaining) != len(drifts):
            # Re-evaluate so the reported state is what is true now, not what
            # was true before the repair.
            drifts, fresh = primary_freshness_report(
                root, expected_branch=expected_branch, use_proxy=use_proxy
            )

    if not drifts:
        print(f"PRIMARY FRESHNESS OK ({len(fresh)} primaries): {', '.join(fresh)}", file=out)
        return 0

    print(
        f"PRIMARY CHECKOUT DRIFT: {len(drifts)} finding(s) across "
        f"{len({d.primary for d in drifts})} primary(ies)",
        file=err,
    )
    for drift in drifts:
        print(f"  [{drift.primary}] {drift.kind}: {drift.detail}", file=err)
        print(f"      fix: {drift.remediation}", file=err)
    if fresh:
        print(f"  fresh: {', '.join(fresh)}", file=err)
    if any(d.safe_fix for d in drifts):
        print("  (re-run with --restore-safe to repair the safe_fix classes)", file=err)
    print(
        "  No reset, force or fast-forward is performed automatically: these are shared "
        "integration surfaces with live sibling processes.",
        file=err,
    )

    if not strict:
        return 0
    if any(d.kind in ("unknown", "unclassified-drift") for d in drifts):
        return 2
    return 1


def check_freshness(
    root: Path,
    *,
    strict: bool = False,
    use_proxy: bool = True,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    warnings: list[str] = []
    current: list[str] = []
    # A refs-only inspection cannot see a bare flip or a drifted parent, so pull
    # those two classes in from the shared invariant. Everything below is the
    # original per-product logic, unchanged.
    for name, repo in primary_paths(root):
        if (repo / ".git").exists() and _rev(repo, "rev-parse", "--is-bare-repository") == "true":
            warnings.append(
                f"{name}: core.bare=true -- every work-tree op fails; "
                f"fix: git -C {repo} config core.bare false"
            )
    parent_drifts, _ = inspect_primary(root, "parent", root, use_proxy=use_proxy)
    warnings.extend(
        f"parent: {d.kind}: {d.detail}" for d in parent_drifts if d.kind != "bare"
    )
    for product in PRODUCTS:
        repo = root / product
        if not (repo / ".git").exists():
            warnings.append(f"{product}: primary checkout is not initialized")
            continue

        branch_result = run_git(repo, "branch", "--show-current")
        head_result = run_git(repo, "rev-parse", "HEAD")
        remote_result = run_git(
            repo,
            "ls-remote",
            "--exit-code",
            "origin",
            MAIN_REF,
            network=True,
            use_proxy=use_proxy,
        )
        branch = branch_result.stdout.strip()
        head = head_result.stdout.strip()
        remote = remote_result.stdout.split(maxsplit=1)[0] if remote_result.stdout.strip() else ""

        if branch_result.returncode != 0 or head_result.returncode != 0:
            warnings.append(f"{product}: could not inspect branch/HEAD")
            continue
        if remote_result.returncode != 0 or not remote:
            warnings.append(f"{product}: could not query live origin/main")
            continue
        if branch != "main":
            warnings.append(f"{product}: branch is {branch or 'DETACHED'}, expected main")
        if head != remote:
            warnings.append(f"{product}: HEAD {head} differs from origin/main {remote}")
        if branch == "main" and head == remote:
            current.append(f"{product}={head[:12]}")

        # The parent's own index is exactly what we want here, so keep the
        # in-flight GIT_INDEX_FILE a pre-commit hook hands us.
        gitlink = run_git(root, "rev-parse", f":{product}", inherit_repo_env=True)
        recorded = gitlink.stdout.strip()
        if gitlink.returncode != 0 or not recorded:
            warnings.append(f"{product}: parent index has no gitlink")
        elif remote and recorded != remote:
            warnings.append(
                f"{product}: parent gitlink {recorded} differs from origin/main {remote}"
            )

    reverie_head = run_git(root / "reverie", "rev-parse", "HEAD").stdout.strip()
    pins, pin_count, pin_errors = reverie_manifest_pins(root / "hermit")
    warnings.extend(f"hermit: {message}" for message in pin_errors)
    if pin_count == 0:
        warnings.append("hermit: no tracked Cargo manifest pins rrnewton/reverie")
    elif reverie_head and pins != {reverie_head}:
        warnings.append(
            "Hermit Reverie manifest pin mismatch: "
            f"manifests={','.join(sorted(pins)) or 'none'} reverie={reverie_head}"
        )
    if reverie_head:
        warnings.extend(
            f"hermit: {message}"
            for message in reverie_generated_pin_errors(root / "hermit", reverie_head)
        )

    if warnings:
        print("HARD WARNING: PRIMARY CHECKOUT FRESHNESS", file=err)
        for warning in warnings:
            print(f"  {warning}", file=err)
        print(
            "Run `make checkout-fresh`; dirty primaries are preserved and skipped, "
            "and only a coherent snapshot is published.",
            file=err,
        )
    else:
        print(f"Primary checkouts are current on main: {', '.join(current)}", file=out)
    return 1 if strict and warnings else 0


def check_pins(
    root: Path,
    *,
    out: TextIO = sys.stdout,
    err: TextIO = sys.stderr,
) -> int:
    """Blocking, offline Reverie-pin *consistency* check.

    Unlike ``check`` (which also inspects primary-checkout freshness, parent
    gitlinks, and queries the network), this verifies only that the Reverie
    *pin* is internally consistent across the Hermit checkout: every tracked
    Cargo.toml rev is identical, and every tracked Cargo.lock source (including
    the nested liteinst-runtime-build/Cargo.lock) references that same SHA.

    It performs NO network access and does NOT look at primary freshness or
    parent gitlinks, so it never blocks a commit that merely repairs a stale
    gitlink; it blocks only a *silent pin drift* where one Reverie reference
    was moved without the others. That makes it safe to run in a pre-commit
    hook. Two dimensions are deliberately EXCLUDED from this blocking gate and
    left to the warning-only ``check``:
      * currency (is the pin on reverie main?) -- networked; also enforced by
        hermit's own check-reverie-pin.rs hook + CI;
        * the LiteInst build-cache key -- it is DERIVED at run time by
          scripts/stage-liteinst-runtime.sh from the canonical pin, so there
          is no stored literal to compare and nothing here can drift. What
          is checked instead is that the derivation still exists and that no
          file hardcodes a suffix (reverie_cache_derivation_errors).

    WHAT IT READS (changed 2026-08-06 after a fleet-wide false block): the
    Hermit tree at the gitlink this parent commit will RECORD, never the Hermit
    working tree. A parent commit records a submodule *commit*, so the recorded
    commit is the only pin state it can possibly introduce; another agent's
    in-flight edits under hermit/ are not part of it and must not block an
    unrelated parent commit. Hermit's own pre-commit hook and CI guard the
    Hermit working tree, and the warning-only ``check`` still reports it.

    FAILURE POSTURE. Fail-CLOSED on real drift: the recorded tree is readable
    and its Reverie references disagree -> exit 1. Fail-OPEN, loudly, when the
    gate cannot be evaluated at all (no gitlink, or the recorded commit is
    absent from the local object store) -- an unevaluable gate is not evidence
    of drift, and blocking there is what turned a stale premise into an outage.
    """
    hermit = root / "hermit"
    commit, origin = indexed_submodule_commit(root, "hermit")
    unevaluable = "" if commit is not None else origin
    if commit is not None:
        readable = run_git(
            hermit, "cat-file", "-e", f"{commit}^{{commit}}", env_overrides=NO_LAZY_FETCH
        )
        if readable.returncode != 0:
            unevaluable = (
                f"recorded hermit gitlink {commit[:12]} ({origin}) is not in the local "
                "Hermit object store"
            )
            commit = None
    if commit is None:
        print(
            f"WARNING: {unevaluable}; Reverie pin drift NOT evaluated "
            "(this gate never blocks on being unevaluable).",
            file=err,
        )
        return 0

    errors: list[str] = []
    pins, pin_count, pin_errors = reverie_manifest_pins(hermit, commit)
    errors.extend(f"hermit: {message}" for message in pin_errors)
    expected: str | None = None
    if pin_count == 0:
        errors.append("hermit: no tracked Cargo manifest pins rrnewton/reverie")
    elif len(pins) != 1:
        errors.append(
            "Hermit Reverie manifest pins are not internally consistent: "
            f"manifests={','.join(sorted(pins))}"
        )
    else:
        expected = next(iter(pins))
        for relative in REVERIE_LOCKFILES:
            text, failure = _read_tracked(hermit, commit, relative)
            if text is None:
                errors.append(f"hermit: could not parse {relative}: {failure}")
                continue
            try:
                lock = tomllib.loads(text)
            except tomllib.TOMLDecodeError as error:
                errors.append(f"hermit: could not parse {relative}: {error}")
                continue
            sources = [
                package.get("source", "")
                for package in lock.get("package", [])
                if isinstance(package, Mapping)
                and str(package.get("source", "")).startswith(f"git+{REVERIE_GIT_URL}")
            ]
            if not sources:
                errors.append(f"hermit: {relative}: no Reverie git sources found")
                continue
            for source in sources:
                match = REVERIE_SOURCE.fullmatch(str(source))
                if match is None or match.group(1) != expected or match.group(2) != expected:
                    errors.append(f"hermit: {relative}: stale Reverie source {source}")

    if errors:
        print(
            f"REVERIE PIN DRIFT at recorded hermit gitlink {commit} ({origin}): "
            "tracked Reverie references disagree - BLOCKED",
            file=err,
        )
        for message in errors:
            print(f"  {message}", file=err)
        print(
            "Every Cargo.toml rev and every tracked Cargo.lock source (including the "
            "nested liteinst-runtime-build/Cargo.lock) must reference ONE identical "
            "Reverie SHA. This is the pin state the commit would RECORD -- the Hermit "
            "working tree was not read, so cleaning it will not clear this. Re-run the "
            "Reverie pin update (hermit/docs/updating-reverie.md) so all references move "
            "together and re-point the gitlink, or override a deliberate in-flight state "
            "with HERMIT_PIN_DRIFT_OVERRIDE=1 git commit ...",
            file=err,
        )
        return 1
    print(
        f"Reverie pin is internally consistent at recorded hermit gitlink {commit[:12]} "
        f"({origin}): {expected} ({pin_count} manifest revision entries; tracked "
        "Cargo.lock sources agree).",
        file=out,
    )
    return 0


def default_root() -> Path:
    return Path(__file__).resolve().parent.parent


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=default_root())
    subparsers = parser.add_subparsers(dest="command", required=True)
    fresh = subparsers.add_parser("fresh", description="refresh every clean primary checkout", help="refresh every clean primary checkout")
    fresh.add_argument(
        "--publish-parent",
        action="store_true",
        help="commit and push coherent product gitlinks to parent main",
    )
    fresh.add_argument(
        "--strict",
        action="store_true",
        help="return nonzero when a dirty primary must be skipped",
    )
    check = subparsers.add_parser("check", description="warn about detached or stale primaries", help="warn about detached or stale primaries")
    check.add_argument("--strict", action="store_true", help="return nonzero on warnings")
    freshness = subparsers.add_parser(
        "freshness",
        description="one invariant over every primary (parent included): not bare, on main, "
        "not detached, equal to origin, clean. Fails loudly by default.",
        help="one invariant over every primary (parent included): not bare, on main, "
        "not detached, equal to origin, clean. Fails loudly by default.",
    )
    freshness.add_argument(
        "--restore-safe",
        action="store_true",
        help="repair only the unambiguous classes (core.bare flip); never resets, "
        "forces or fast-forwards",
    )
    freshness.add_argument(
        "--expected-branch",
        default="main",
        help="branch every primary should be on (default: main)",
    )
    freshness.add_argument(
        "--no-strict",
        action="store_true",
        help="report drift but exit 0 (for advisory call sites)",
    )
    subparsers.add_parser(
        "check-pins",
        description="offline: block on Reverie pin drift across manifests and Cargo.lock sources",
        help="offline: block on Reverie pin drift across manifests and Cargo.lock sources",
    )
    args = parser.parse_args(argv)

    root = args.root.resolve()
    if args.command == "fresh":
        return checkout_fresh(
            root, publish_parent=args.publish_parent, strict=args.strict
        )
    if args.command == "check-pins":
        return check_pins(root)
    if args.command == "freshness":
        return check_primary_freshness(
            root,
            strict=not args.no_strict,
            restore_safe=args.restore_safe,
            expected_branch=args.expected_branch,
        )
    return check_freshness(root, strict=args.strict)


if __name__ == "__main__":
    raise SystemExit(main())
