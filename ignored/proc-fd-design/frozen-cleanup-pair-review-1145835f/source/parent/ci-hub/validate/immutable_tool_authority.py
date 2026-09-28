"""Verify the sealed descriptor authority for immutable operational tooling.

This module uses only the standard library so a caller can authenticate and
load these exact pinned bytes without importing the validation launcher.
"""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import re
import stat
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any


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
