#!/usr/bin/env python3
"""Launch one detached, admitted Hermit or Reverie validation through ci-hub."""

from __future__ import annotations

import argparse
import filecmp
import fcntl
import hashlib
import json
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
from contextlib import contextmanager
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any, TextIO

import pane_owner
import run_registry
import service_result


ROOT = Path(__file__).resolve().parents[2]
HOST_TMP_ROOT = Path("/tmp").resolve()
# Linux permits 107 pathname bytes in an AF_UNIX address. Reverie's longest
# current temporary socket fixture appends 77 bytes including its separator.
MAX_RUNTIME_ROOT_BYTES = 30


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
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    head = completed.stdout.strip()
    return head if SHA_RE.match(head) else None


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
FD_TOOL_ROOT_RE = re.compile(r"^/proc/[1-9][0-9]*/fd/[0-9]+$")
TOOL_AUTHORITY_SCHEMA = "dev-hermit-tool-authority-v1"
TOOL_AUTHORITY_ENV = "DEV_HERMIT_TOOL_AUTHORITY"
TOOL_CONTENT_SHA256_ENV = "DEV_HERMIT_TOOL_CONTENT_SHA256"
TOOL_PARENT_SHA_ENV = "DEV_HERMIT_TOOL_PARENT_SHA"
TOOL_HERMIT_SHA_ENV = "DEV_HERMIT_TOOL_HERMIT_SHA"
TOOL_AGENT_UTILS_SHA_ENV = "DEV_HERMIT_TOOL_AGENT_UTILS_SHA"
TOOL_BOOTSTRAP_SHA256_ENV = "DEV_HERMIT_TOOL_BOOTSTRAP_SHA256"
TOOL_AUTHORITY_FIELDS = frozenset(
    (
        "agent_utils_sha",
        "authority_fd",
        "bootstrap_sha256",
        "content_sha256",
        "hermit_sha",
        "holder_pid",
        "parent_sha",
        "root_dev",
        "root_fd",
        "root_ino",
        "schema",
        "state_dev",
        "state_fd",
        "state_ino",
        "state_root",
        "target_dev",
        "target_fd",
        "target_ino",
        "target_root",
    )
)


@dataclass(frozen=True)
class ImmutableToolAuthority:
    holder_pid: int
    authority_fd: int
    target_fd: int
    target_root: Path
    target_dev: int
    target_ino: int
    root_fd: int
    root_dev: int
    root_ino: int
    state_fd: int
    state_root: Path
    state_dev: int
    state_ino: int
    content_sha256: str
    parent_sha: str
    hermit_sha: str
    agent_utils_sha: str
    bootstrap_sha256: str


def proc_fd_identity(path: Path, *, role: str) -> tuple[int, int]:
    match = re.fullmatch(r"/proc/([1-9][0-9]*)/fd/([0-9]+)", str(path))
    if match is None:
        raise ValueError(f"{role} must be an exact /proc/<pid>/fd/<fd> capability")
    return int(match.group(1)), int(match.group(2))


def authority_int(record: Mapping[str, Any], name: str) -> int:
    value = record.get(name)
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"immutable tool authority has no nonnegative integer {name}")
    return value


def authority_string(record: Mapping[str, Any], name: str) -> str:
    value = record.get(name)
    if not isinstance(value, str):
        raise ValueError(f"immutable tool authority has no string {name}")
    return value


def update_digest_length(digest: Any, value: int) -> None:
    digest.update(value.to_bytes(8, "big"))


def digest_tool_entry(path: bytes, relative: bytes, digest: Any) -> None:
    metadata = os.lstat(path)
    mode = stat.S_IMODE(metadata.st_mode)
    if stat.S_ISDIR(metadata.st_mode):
        if mode & 0o222 or metadata.st_nlink < 1:
            raise ValueError(
                f"immutable tool directory is writable or unlinked: {os.fsdecode(path)}"
            )
        held_fd = os.open(
            path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        )
        try:
            held = os.fstat(held_fd)
            if not stat.S_ISDIR(held.st_mode) or (
                held.st_dev,
                held.st_ino,
            ) != (metadata.st_dev, metadata.st_ino):
                raise ValueError(
                    f"immutable tool directory changed before hashing: {os.fsdecode(path)}"
                )
            digest.update(b"d")
            update_digest_length(digest, len(relative))
            digest.update(relative)
            digest.update(mode.to_bytes(4, "big"))
            for name in sorted(os.listdir(path), key=os.fsencode):
                name_bytes = os.fsencode(name)
                child_relative = relative + (b"/" if relative else b"") + name_bytes
                if child_relative in {
                    b".git",
                    b"hermit/.git",
                    b"hermit/agent-utils/.git",
                }:
                    continue
                digest_tool_entry(
                    os.path.join(path, name_bytes), child_relative, digest
                )
            after = os.lstat(path)
            if (after.st_dev, after.st_ino) != (held.st_dev, held.st_ino):
                raise ValueError(
                    f"immutable tool directory changed while hashing: {os.fsdecode(path)}"
                )
        finally:
            os.close(held_fd)
    elif stat.S_ISREG(metadata.st_mode):
        if mode & 0o222 or metadata.st_nlink != 1:
            raise ValueError(
                f"immutable tool file is writable or multiply linked: {os.fsdecode(path)}"
            )
        held_fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            held = os.fstat(held_fd)
            if not stat.S_ISREG(held.st_mode) or (
                held.st_dev,
                held.st_ino,
                held.st_size,
            ) != (metadata.st_dev, metadata.st_ino, metadata.st_size):
                raise ValueError(
                    f"immutable tool file changed before hashing: {os.fsdecode(path)}"
                )
            digest.update(b"f")
            update_digest_length(digest, len(relative))
            digest.update(relative)
            digest.update(mode.to_bytes(4, "big"))
            digest.update(metadata.st_size.to_bytes(8, "big"))
            observed = 0
            while True:
                chunk = os.read(held_fd, 64 * 1024)
                if not chunk:
                    break
                observed += len(chunk)
                digest.update(chunk)
            after = os.fstat(held_fd)
            if observed != metadata.st_size or (
                after.st_dev,
                after.st_ino,
                after.st_size,
            ) != (held.st_dev, held.st_ino, held.st_size):
                raise ValueError(
                    f"immutable tool file changed while hashing: {os.fsdecode(path)}"
                )
        finally:
            os.close(held_fd)
    elif stat.S_ISLNK(metadata.st_mode):
        target = os.fsencode(os.readlink(path))
        digest.update(b"l")
        update_digest_length(digest, len(relative))
        digest.update(relative)
        digest.update(mode.to_bytes(4, "big"))
        update_digest_length(digest, len(target))
        digest.update(target)
    else:
        raise ValueError(
            f"immutable tool has unsupported entry type at {os.fsdecode(path)}"
        )


def immutable_tool_content_sha256(tool_root: Path) -> str:
    root = os.fsencode(tool_root)
    metadata = os.stat(root)
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) & 0o222:
        raise ValueError("immutable tool root must be one read-only directory")
    digest = hashlib.sha256(b"dev-hermit-tool-content-v1\0")
    for name in sorted(os.listdir(root), key=os.fsencode):
        name_bytes = os.fsencode(name)
        if name_bytes == b".git":
            continue
        digest_tool_entry(os.path.join(root, name_bytes), name_bytes, digest)
    return digest.hexdigest()


def read_immutable_tool_authority(
    tool_root: Path,
    state_root: Path,
    environment: Mapping[str, str],
) -> ImmutableToolAuthority:
    root_pid, root_fd_from_path = proc_fd_identity(
        tool_root, role="DEV_HERMIT_TOOL_ROOT"
    )
    raw_authority = environment.get(TOOL_AUTHORITY_ENV, "")
    if not raw_authority:
        raise ValueError(
            f"fd-backed DEV_HERMIT_TOOL_ROOT lacks {TOOL_AUTHORITY_ENV}"
        )
    authority_path = Path(raw_authority)
    authority_pid, authority_fd_from_path = proc_fd_identity(
        authority_path, role=TOOL_AUTHORITY_ENV
    )
    if authority_pid != root_pid:
        raise ValueError(
            "immutable tool root and authority are owned by different holders"
        )

    authority_file = os.open(authority_path, os.O_RDONLY | os.O_CLOEXEC)
    try:
        metadata = os.fstat(authority_file)
        required_seals = (
            fcntl.F_SEAL_SEAL
            | fcntl.F_SEAL_SHRINK
            | fcntl.F_SEAL_GROW
            | fcntl.F_SEAL_WRITE
        )
        seals = fcntl.fcntl(authority_file, fcntl.F_GET_SEALS)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 0
            or metadata.st_mode & 0o222
            or seals & required_seals != required_seals
        ):
            raise ValueError(
                "immutable tool authority must be one anonymous, read-only, completely sealed regular file"
            )
        encoded = bytearray()
        while len(encoded) <= 4096:
            chunk = os.read(authority_file, min(4097 - len(encoded), 4096))
            if not chunk:
                break
            encoded.extend(chunk)
        if len(encoded) > 4096:
            raise ValueError("immutable tool authority exceeds 4096 bytes")
    finally:
        os.close(authority_file)

    try:
        value = json.loads(encoded)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"immutable tool authority is not valid JSON: {error}") from error
    if not isinstance(value, dict):
        raise ValueError("immutable tool authority must be one JSON object")
    if frozenset(value) != TOOL_AUTHORITY_FIELDS:
        raise ValueError("immutable tool authority fields are incomplete or unknown")
    if authority_string(value, "schema") != TOOL_AUTHORITY_SCHEMA:
        raise ValueError("immutable tool authority has an unsupported schema")

    authority = ImmutableToolAuthority(
        holder_pid=authority_int(value, "holder_pid"),
        authority_fd=authority_int(value, "authority_fd"),
        target_fd=authority_int(value, "target_fd"),
        target_root=Path(authority_string(value, "target_root")),
        target_dev=authority_int(value, "target_dev"),
        target_ino=authority_int(value, "target_ino"),
        root_fd=authority_int(value, "root_fd"),
        root_dev=authority_int(value, "root_dev"),
        root_ino=authority_int(value, "root_ino"),
        state_fd=authority_int(value, "state_fd"),
        state_root=Path(authority_string(value, "state_root")),
        state_dev=authority_int(value, "state_dev"),
        state_ino=authority_int(value, "state_ino"),
        content_sha256=authority_string(value, "content_sha256"),
        parent_sha=authority_string(value, "parent_sha"),
        hermit_sha=authority_string(value, "hermit_sha"),
        agent_utils_sha=authority_string(value, "agent_utils_sha"),
        bootstrap_sha256=authority_string(value, "bootstrap_sha256"),
    )
    if (
        authority.holder_pid != root_pid
        or authority.authority_fd != authority_fd_from_path
        or authority.root_fd != root_fd_from_path
    ):
        raise ValueError(
            "immutable tool authority does not name the supplied descriptors"
        )
    if len(
        {
            authority.authority_fd,
            authority.target_fd,
            authority.root_fd,
            authority.state_fd,
        }
    ) != 4:
        raise ValueError(
            "immutable tool authority conflates target, executable, state, or record descriptors"
        )
    for label, value, digits in (
        ("content digest", authority.content_sha256, 64),
        ("parent SHA", authority.parent_sha, 40),
        ("Hermit SHA", authority.hermit_sha, 40),
        ("agent-utils SHA", authority.agent_utils_sha, 40),
        ("bootstrap digest", authority.bootstrap_sha256, 64),
    ):
        if re.fullmatch(rf"[0-9a-f]{{{digits}}}", value) is None:
            raise ValueError(f"immutable tool authority has an invalid {label}")

    target_path = Path(
        f"/proc/{authority.holder_pid}/fd/{authority.target_fd}"
    )
    target_metadata = os.stat(target_path)
    if (
        not stat.S_ISDIR(target_metadata.st_mode)
        or target_metadata.st_mode & 0o222
        or target_metadata.st_nlink < 1
        or (target_metadata.st_dev, target_metadata.st_ino)
        != (authority.target_dev, authority.target_ino)
    ):
        raise ValueError(
            "cached target descriptor identity does not match its authority"
        )
    resolved_target = authority.target_root.resolve(strict=True)
    if (
        not authority.target_root.is_absolute()
        or resolved_target != authority.target_root
        or authority.target_root.name != authority.parent_sha
        or authority.target_root.parent.name != "trees"
    ):
        raise ValueError(
            "cached target authority does not name canonical trees/<parent-sha>"
        )
    live_target = os.stat(resolved_target, follow_symlinks=False)
    if (live_target.st_dev, live_target.st_ino) != (
        authority.target_dev,
        authority.target_ino,
    ):
        raise ValueError(
            "cached target pathname no longer names the retained authority"
        )
    if os.readlink(target_path) != str(authority.target_root):
        raise ValueError(
            "cached target descriptor no longer names its canonical path"
        )

    root_metadata = os.stat(tool_root)
    if (
        not stat.S_ISDIR(root_metadata.st_mode)
        or root_metadata.st_mode & 0o222
        or root_metadata.st_nlink < 1
        or (root_metadata.st_dev, root_metadata.st_ino)
        != (authority.root_dev, authority.root_ino)
    ):
        raise ValueError(
            "immutable tool root descriptor identity does not match its authority"
        )
    state_path = Path(
        f"/proc/{authority.holder_pid}/fd/{authority.state_fd}"
    )
    state_metadata = os.stat(state_path)
    if (
        not stat.S_ISDIR(state_metadata.st_mode)
        or state_metadata.st_nlink < 1
        or (state_metadata.st_dev, state_metadata.st_ino)
        != (authority.state_dev, authority.state_ino)
    ):
        raise ValueError(
            "retained state-root descriptor identity does not match its authority"
        )
    resolved_state = state_root.resolve(strict=True)
    if not authority.state_root.is_absolute() or resolved_state != authority.state_root:
        raise ValueError(
            "canonical DEV_HERMIT_PARENT does not match immutable tool authority state root"
        )
    live_state = os.stat(resolved_state, follow_symlinks=False)
    if (live_state.st_dev, live_state.st_ino) != (
        authority.state_dev,
        authority.state_ino,
    ):
        raise ValueError(
            "canonical state-root pathname no longer names the retained authority"
        )
    if os.readlink(state_path) != str(authority.state_root):
        raise ValueError(
            "retained state-root descriptor no longer names its canonical path"
        )
    directory_identities = {
        (authority.target_dev, authority.target_ino),
        (authority.root_dev, authority.root_ino),
        (authority.state_dev, authority.state_ino),
    }
    if len(directory_identities) != 3:
        raise ValueError(
            "immutable tool authority conflates target, executable, or state identity"
        )

    required_environment = (
        (TOOL_CONTENT_SHA256_ENV, authority.content_sha256),
        (TOOL_PARENT_SHA_ENV, authority.parent_sha),
        (TOOL_HERMIT_SHA_ENV, authority.hermit_sha),
        (TOOL_AGENT_UTILS_SHA_ENV, authority.agent_utils_sha),
        (TOOL_BOOTSTRAP_SHA256_ENV, authority.bootstrap_sha256),
    )
    for name, expected in required_environment:
        observed = environment.get(name)
        if observed is None:
            raise ValueError(f"immutable tool authority is incomplete: {name} is absent")
        if observed != expected:
            raise ValueError(f"{name} does not match immutable tool authority")

    observed_digest = immutable_tool_content_sha256(tool_root)
    if observed_digest != authority.content_sha256:
        raise ValueError(
            "immutable tool content digest is "
            f"{observed_digest}, expected {authority.content_sha256}"
        )
    return authority


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
SCORECARD_PATHS = ("SCORECARD.md", "ci/compat-envelope/cells.json")


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
    """Return the parent for current-main checkouts and per-run caches."""
    candidate = (root / "worktrees" / "validate").resolve()
    candidate.mkdir(parents=True, exist_ok=True)
    return require_guest_visible_root(candidate, role="validation checkout parent")


def frozen_checkout_parent(root: Path) -> Path:
    """Return a guest-visible checkout parent outside the dev-hermit boundary.

    Hermit's product front door accepts canonical current-main validation. A
    frozen run instead holds ``frozen-validate`` authority and writes an
    explicitly non-canonical result. Keeping its disposable checkout outside
    dev-hermit prevents the product front door from mistaking that deliberate
    distinction for an unadmitted direct invocation.
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


def prepare_fresh_checkout(
    source: Path,
    target: str,
    *,
    run: Runner,
    parent: Path,
    repo: str,
    tool_root: Path = ROOT,
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
    state_root = parent.parent.parent
    slot = f"validate-fresh-{target[:12]}-{os.getpid()}-{secrets.token_hex(4)}"
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


def remove_fresh_checkout(
    source: Path,
    fresh: Path,
    *,
    run: Runner,
    tool_root: Path = ROOT,
    completed_record: Path | None = None,
) -> bool:
    """Remove the temp worktree AND deregister it; return observed absence."""
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
        return not fresh.exists()
    try:
        validate_parent = fresh.parent
        worktrees = validate_parent.parent
        if validate_parent.name == "validate" and worktrees.name == "worktrees":
            state_root = worktrees.parent
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
                "1",
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
                "--coordinator-pid",
                str(os.getpid()),
                "--legacy-validate-checkout",
                checkout_relative.as_posix(),
                "--completed-record",
                record_relative.as_posix(),
                "--repository",
                repository_relative.as_posix(),
            ]
        else:
            raise RuntimeError(
                f"checkout is outside worktrees/validate and the historical ignored layout: {fresh}"
            )
        removal = run(command, check=False)
    except RuntimeError as error:
        print(
            f"validate-run: wrkslots cleanup could not run for {fresh}: {error}; "
            "the checkout and its lifecycle record were retained. Run "
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
            f"validate-run: wrkslots retained validation checkout {fresh}: {detail}",
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


def path_may_have_live_process(path: Path, *, proc_root: Path = Path("/proc")) -> bool:
    """Whether a same-uid process uses ``path`` or process state is unreadable."""
    resolved = str(path.resolve())
    prefix = f"{resolved}{os.sep}"

    def matches(target: str) -> bool:
        if target.endswith(" (deleted)"):
            target = target[: -len(" (deleted)")]
        return target == resolved or target.startswith(prefix)

    try:
        entries = os.listdir(proc_root)
    except OSError:
        return True
    uid = os.getuid()
    for entry in entries:
        if not entry.isdigit():
            continue
        proc = proc_root / entry
        try:
            if proc.stat().st_uid != uid:
                continue
        except OSError:
            continue
        readable = False
        for name in ("cwd", "exe", "root"):
            try:
                target = os.readlink(proc / name)
            except OSError:
                continue
            readable = True
            if matches(target):
                return True
        for name in ("fd", "map_files"):
            try:
                links = os.listdir(proc / name)
            except OSError:
                continue
            readable = True
            for link in links:
                try:
                    target = os.readlink(proc / name / link)
                except OSError:
                    continue
                if matches(target):
                    return True
        try:
            with (proc / "maps").open(encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    fields = line.rstrip("\n").split(maxsplit=5)
                    if len(fields) == 6 and matches(fields[5]):
                        return True
        except OSError:
            pass
        if not readable:
            try:
                state = (proc / "stat").read_text().rsplit(")", 1)[1].split()[0]
            except (OSError, IndexError):
                continue
            if state != "Z":
                return True
    return False


def recorded_checkout_parents(root: Path, record: Mapping[str, Any]) -> list[Path]:
    """Exact parents in which this record may own a disposable checkout."""
    parents = [
        validate_checkout_parent(root),
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
        parents.insert(0, frozen_checkout_parent(root))
    return parents


def cleanup_recorded_checkout(
    root: Path,
    record: Mapping[str, Any],
    *,
    record_path: Path,
    unit: str,
    run: Runner,
    tool_root: Path = ROOT,
    check_processes: bool = True,
) -> tuple[bool, list[str], str | None]:
    """Archive local receipt evidence and remove one terminal temp checkout."""
    raw_checkout = record.get("checkout")
    if not isinstance(raw_checkout, str):
        return False, [], "run record has no checkout path"
    checkout = Path(raw_checkout).resolve()
    parents = recorded_checkout_parents(root, record)
    temporary = record.get("temporary_checkout")
    if temporary is False:
        return False, [], None
    if checkout.parent not in parents or not checkout.name.startswith("validate-fresh-"):
        if temporary is True:
            expected = " or ".join(str(parent) for parent in parents)
            return False, [], f"temporary checkout is outside {expected}"
        return False, [], None
    if not checkout.exists():
        return False, [], None
    if check_processes and path_may_have_live_process(checkout):
        return False, [], "a live process may still use the checkout"
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
    )
    return (
        removed,
        archived,
        None if removed else "checkout still exists after the removal attempt",
    )


def cleanup_recorded_cargo_home(
    root: Path,
    record: Mapping[str, Any],
    *,
    run: Runner,
    check_processes: bool = True,
) -> tuple[bool, str | None]:
    """Remove one recorded per-run Cargo home after its unit is terminal."""
    raw_private = record.get("cargo_home")
    if not isinstance(raw_private, str):
        return False, None
    private = Path(raw_private).resolve()
    parent = validate_checkout_parent(root)
    if private.parent != parent or not private.name.startswith("validate-cargo-"):
        return False, "recorded Cargo home is outside worktrees/validate"
    if not private.exists():
        return False, None
    if check_processes and path_may_have_live_process(private):
        return False, "a live process may still use the Cargo home"
    remove_private_cargo_home(private, run=run)
    return (
        not private.exists(),
        None if not private.exists() else "Cargo home still exists after the removal attempt",
    )


def recorded_cleanup_path_exists(root: Path, record: Mapping[str, Any]) -> bool:
    """Whether a run record names material that this sweep could remove."""
    raw_checkout = record.get("checkout")
    if isinstance(raw_checkout, str):
        checkout = Path(raw_checkout).resolve()
        temporary = record.get("temporary_checkout")
        accepted_unmarked_checkout = (
            checkout.parent in recorded_checkout_parents(root, record)
            and checkout.name.startswith("validate-fresh-")
        )
        if checkout.exists() and (
            temporary is True
            or (temporary is not False and accepted_unmarked_checkout)
        ):
            return True

    raw_cargo_home = record.get("cargo_home")
    return isinstance(raw_cargo_home, str) and Path(raw_cargo_home).resolve().exists()


def read_cleanup_record(path: Path) -> dict[str, Any]:
    """Read only the fields needed to remove disposable run material safely.

    Run handles are intentionally strict for verdict readers, but that contract
    has acquired required fields without changing its schema version.  Applying
    today's verdict schema to every historical row prevented cleanup before it
    could discover that most rows had no checkout left, and one old admitted row
    without a run number made the complete sweep return could-not-determine.

    Cleanup does not interpret a verdict.  It independently requires an exact
    disposable parent and name, terminal service or durable completion evidence,
    and no live process.  Reading the versioned JSON object here preserves those
    checks without making disk cleanup depend on every historical reporting field.
    """
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read validation handle {path}: {error}") from error
    if not isinstance(value, dict):
        raise RuntimeError(f"validation handle {path} is not an object")
    if value.get("schema_version") != run_registry.SCHEMA_VERSION:
        raise RuntimeError(f"validation handle {path} has an unsupported schema")
    return value


def sweep_completed_checkouts(
    root: Path, *, run: Runner, tool_root: Path = ROOT
) -> dict[str, Any]:
    """Remove terminal validation checkouts left by interrupted waiters."""
    runs = root / "ignored" / "validate" / "runs"
    removed: list[str] = []
    removed_cargo_homes: list[str] = []
    retained: list[dict[str, str]] = []
    bookkeeping_errors: list[dict[str, str]] = []
    if not runs.is_dir():
        return {
            "removed": removed,
            "removed_cargo_homes": removed_cargo_homes,
            "retained": retained,
            "bookkeeping_errors": bookkeeping_errors,
        }
    for path in sorted(runs.glob("*.json")):
        try:
            record = read_cleanup_record(path)
            # The registry also contains bench handles and historical validation
            # handles whose disposable paths are already gone.  Querying systemd
            # for every one made the sweep scale with all retained history and
            # made irrelevant bench unit names permanent failures.  Cleanup is
            # needed only while a recorded disposable path still exists.
            if not recorded_cleanup_path_exists(root, record):
                continue
            unit = sanitize_unit(str(record.get("unit", path.stem)))
            properties = service_properties(unit, run=run)
        except (OSError, RuntimeError, ValueError) as error:
            retained.append({"checkout": str(path), "reason": str(error)})
            continue
        if properties is None:
            if not service_result.is_evidenced_terminal(record):
                retained.append(
                    {
                        "checkout": str(record.get("checkout", "")),
                        "reason": "unit state unavailable and run record is not terminal",
                    }
                )
                continue
        elif properties.get("ActiveState") not in TERMINAL_STATES:
            retained.append(
                {"checkout": str(record.get("checkout", "")), "reason": "unit-running"}
            )
            continue
        fields: dict[str, Any] = {}
        try:
            cleaned, archived, reason = cleanup_recorded_checkout(
                root,
                record,
                record_path=path,
                unit=unit,
                run=run,
                tool_root=tool_root,
                check_processes=True,
            )
        except (OSError, RuntimeError, ValueError) as error:
            retained.append(
                {"checkout": str(record.get("checkout", "")), "reason": str(error)}
            )
        else:
            if cleaned:
                removed.append(str(record.get("checkout")))
                fields.update(
                    checkout_removed_at=datetime.now(timezone.utc).isoformat(),
                    archived_orphaned_receipts=archived,
                )
            if reason is not None:
                retained.append(
                    {"checkout": str(record.get("checkout", "")), "reason": reason}
                )
        try:
            cargo_cleaned, cargo_reason = cleanup_recorded_cargo_home(
                root,
                record,
                run=run,
                check_processes=True,
            )
        except (OSError, RuntimeError, ValueError) as error:
            retained.append(
                {"checkout": str(record.get("cargo_home", "")), "reason": str(error)}
            )
        else:
            if cargo_cleaned:
                removed_cargo_homes.append(str(record.get("cargo_home")))
                fields["cargo_home_removed_at"] = datetime.now(timezone.utc).isoformat()
            if cargo_reason is not None:
                retained.append(
                    {"checkout": str(record.get("cargo_home", "")), "reason": cargo_reason}
                )
        if fields:
            try:
                run_registry.update_record(path, blocking=False, **fields)
            except (OSError, RuntimeError, ValueError) as error:
                bookkeeping_errors.append(
                    {"record": str(path), "reason": str(error)}
                )
    return {
        "removed": removed,
        "removed_cargo_homes": removed_cargo_homes,
        "retained": retained,
        "bookkeeping_errors": bookkeeping_errors,
    }


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


def write_scorecard_from_results(
    source_checkout: Path,
    target: str,
    results: Path,
    *,
    run: Runner,
    json_output: bool,
) -> bool:
    """Run the locked writer after the validation verdict is established."""
    source_checkout = source_checkout.resolve()
    review_command = shlex.join(
        ["git", "-C", str(source_checkout), "diff", "--", *SCORECARD_PATHS]
    )
    output = sys.stderr if json_output else sys.stdout

    def failed(reason: str) -> bool:
        print(
            "validate-run: compatibility scorecard NOT UPDATED: "
            f"{reason}. The established validate verdict is unchanged.",
            file=sys.stderr,
        )
        print(f"validate-run: review with: {review_command}", file=sys.stderr)
        return False

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
            return failed(f"observe-results exited {result.returncode}")
        print(f"validate-run: review with: {review_command}", file=output)
        return True
    except Exception as error:
        # The verdict exists already. Even an unexpected local filesystem or
        # process-launch error must leave that result as the caller's exit code.
        return failed(str(error))



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


def cleanup_after_attach(
    root: Path,
    record_path: Path,
    record: Mapping[str, Any],
    *,
    unit: str,
    run: Runner,
    tool_root: Path = ROOT,
) -> None:
    """Finish cleanup owned by a caller that previously stopped waiting."""
    try:
        cleaned, archived, reason = cleanup_recorded_checkout(
            root,
            record,
            record_path=record_path,
            unit=unit,
            run=run,
            tool_root=tool_root,
            check_processes=True,
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
            root, record, run=run, check_processes=True
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
    try:
        scorecard_updated = repo != "rrnewton/hermit" or write_scorecard_from_results(
            Path(record["source_checkout"]),
            target,
            Path(record["e2e_result_root"]),
            run=run,
            json_output=json_output,
        )
    except KeyError:
        print(
            "validate-run: compatibility scorecard NOT UPDATED: the run record is "
            "missing its source checkout or durable result directory. The established "
            "validate verdict is unchanged.",
            file=sys.stderr,
        )
        scorecard_updated = False
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=json_output,
    )
    cleanup_after_attach(
        root, record_path, record, unit=unit, run=run, tool_root=tool_root
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
            "DIRECTORY of the checkout to validate, normally \"$PWD\". "
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
    result.add_argument(
        "--sweep-completed",
        action="store_true",
        help=(
            "archive checkout-local receipt evidence and remove completed "
            "worktrees/validate/ checkouts left by interrupted waiters"
        ),
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
    environment = os.environ if environment is None else environment
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
    source_checkout: Path | None = None
    private_cargo_home: Path | None = None
    runtime_root: Path | None = None
    current_main_before_launch: str | None = None
    frozen_result: Path | None = None
    service_tool_prefix: list[str] | None = None
    service_acceptance = ServiceAcceptance.PRE_ACCEPT
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
            # DEFAULT: validate the COMMIT, not the tree that claims to be at it.
            checkout_parent = (
                frozen_checkout_parent(state_root)
                if args.frozen_validate
                else validate_checkout_parent(state_root)
            )
            fresh_checkout = prepare_fresh_checkout(
                source_checkout,
                args.target,
                run=run,
                parent=checkout_parent,
                repo=repo,
                tool_root=tool_root,
                independent_refs=args.frozen_validate,
            )
            checkout = fresh_checkout
        preflight(tool_root, checkout, args.target, repo=repo, run=run)
        runtime_root = prepare_runtime_root(environment or os.environ)
        private_cargo_home = prepare_private_cargo_home(
            validate_checkout_parent(state_root),
            run=run,
            shared=Path((environment or os.environ).get("HOME", "")) / ".cargo",
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
            environment=environment or os.environ,
            make_validate=args.make_validate,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
            cargo_home=private_cargo_home,
            runtime_root=runtime_root,
            ci_dag_jobs=args.ci_dag_jobs,
            max_validates=args.max_validates,
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
                    "temporary_checkout": fresh_checkout is not None,
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
                    environment=environment or os.environ,
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
        if fresh_checkout is not None and source_checkout is not None:
            remove_fresh_checkout(
                source_checkout, fresh_checkout, run=run, tool_root=tool_root
            )
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
            remove_fresh_checkout(
                source_checkout, fresh_checkout, run=run, tool_root=tool_root
            )
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
                    remove_fresh_checkout(
                        source_checkout, fresh_checkout, run=run, tool_root=tool_root
                    )
                remove_private_cargo_home(private_cargo_home, run=run)
                remove_runtime_root(runtime_root, run=run)
                return inner_exit
            if fresh_checkout is not None and source_checkout is not None:
                remove_fresh_checkout(
                    source_checkout, fresh_checkout, run=run, tool_root=tool_root
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
            remove_fresh_checkout(
                source_checkout, fresh_checkout, run=run, tool_root=tool_root
            )
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
            if fresh_checkout is not None and source_checkout is not None:
                remove_fresh_checkout(
                    source_checkout, fresh_checkout, run=run, tool_root=tool_root
                )
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
                # The run HAPPENED and produced evidence; preserve it in the
                # durable validation evidence tree before reclaiming the temp
                # checkout. Keeping a whole checkout for one receipt caused the
                # unbounded disk growth this path now prevents.
                try:
                    archived = archive_orphaned_receipts(
                        state_root, fresh_checkout, orphans, unit=unit
                    )
                except (OSError, RuntimeError) as archive_error:
                    remove_private_cargo_home(private_cargo_home, run=run)
                    remove_runtime_root(runtime_root, run=run)
                    return could_not_determine(
                        "ORPHANED-RECEIPT: the run wrote checkout-local evidence, "
                        f"but it could not be archived: {archive_error}. Temp checkout "
                        f"RETAINED at {fresh_checkout}.",
                        "preserve the files manually before reclaiming the checkout",
                    )
                removed = remove_fresh_checkout(
                    source_checkout, fresh_checkout, run=run, tool_root=tool_root
                )
                disposition = "removed" if removed else f"RETAINED at {fresh_checkout}"
                remove_private_cargo_home(private_cargo_home, run=run)
                remove_runtime_root(runtime_root, run=run)
                return could_not_determine(
                    "ORPHANED-RECEIPT: this run wrote a receipt into "
                    f"{', '.join(orphans)} inside the temp checkout, where no "
                    "consumer reads it. The receipt was archived outside the temp "
                    f"checkout at {', '.join(archived)}, then the checkout was {disposition}.",
                    "inspect the archived receipt; the run's result exists but is not "
                    "a canonical verdict",
                )
            print(
                "validate-run: NO-RECEIPT-PRODUCED: the run stopped before writing a "
                "receipt anywhere, so the temp checkout holds no evidence; "
                "reclaiming it.",
                file=sys.stderr,
            )
            remove_fresh_checkout(
                source_checkout, fresh_checkout, run=run, tool_root=tool_root
            )
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
    scorecard_updated = repo != "rrnewton/hermit" or (
        source_checkout is not None
        and write_scorecard_from_results(
            source_checkout,
            args.target,
            e2e_result_root,
            run=run,
            json_output=args.json,
        )
    )
    if fresh_checkout is not None and source_checkout is not None:
        remove_fresh_checkout(
            source_checkout, fresh_checkout, run=run, tool_root=tool_root
        )
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
