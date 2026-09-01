#!/usr/bin/env python3
"""Bound the shared rust-script cache without racing a process that uses it.

rust-script keys generated packages by the script's absolute source path. Test
fixtures and short-lived worktrees therefore leave a new package and compiled
artifacts in the user's shared cache even after their source path disappears.

Cleanup is deliberately whole-cache: rust-script 0.36 exposes only
``--clear-cache``, and deleting selected package artifacts would reproduce
Cargo's ownership logic. The cache contains generated files only. Before an
apply, this tool checks process cwd/exe/root/fd/maps plus rust-script's cache
environment, atomically renames the cache, creates a fresh replacement, checks
the renamed tree again, and removes only the renamed tree. A process found at
either check retains the tree and makes the gate fail.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import math
import os
import shutil
import sys
import uuid
from dataclasses import dataclass
from pathlib import Path


GIB = 1 << 30
DEFAULT_MAX_GIB = 64.0
MARKER = ".hermit-rust-script-cache-cleanup.json"
FENCE_PREFIX = "rust-script.cleanup-"


class CleanupUnavailable(RuntimeError):
    """The cache could not be measured or changed safely."""


@dataclass(frozen=True)
class ProcessUse:
    pid: int
    signal: str
    detail: str


def default_cache_root() -> Path:
    xdg = os.environ.get("XDG_CACHE_HOME")
    return Path(xdg) / "rust-script" if xdg else Path.home() / ".cache/rust-script"


def _inside(path: str, root: Path) -> bool:
    clean = path.removesuffix(" (deleted)")
    root_text = str(root)
    return clean == root_text or clean.startswith(root_text.rstrip("/") + "/")


def _readlink(path: Path) -> str | None:
    try:
        return os.readlink(path)
    except OSError:
        return None


def _text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def _bytes(path: Path) -> bytes | None:
    try:
        return path.read_bytes()
    except OSError:
        return None


def process_uses(root: Path, proc_root: Path = Path("/proc")) -> list[ProcessUse]:
    """Return positive process references to root; absence is not agent liveness."""
    uses: list[ProcessUse] = []
    self_pid = os.getpid()
    ancestors: set[int] = set()
    parent = os.getppid()
    while parent > 1 and parent not in ancestors:
        ancestors.add(parent)
        stat = _text(proc_root / str(parent) / "stat")
        if not stat:
            break
        fields = stat.split()
        if len(fields) < 4:
            break
        try:
            parent = int(fields[3])
        except ValueError:
            break

    try:
        processes = sorted(
            (entry for entry in proc_root.iterdir() if entry.name.isdigit()),
            key=lambda entry: int(entry.name),
        )
    except OSError as error:
        raise CleanupUnavailable(f"cannot enumerate {proc_root}: {error}") from error

    for process in processes:
        pid = int(process.name)
        if pid == self_pid:
            continue
        for name in ("cwd", "exe", "root"):
            target = _readlink(process / name)
            if target is not None and _inside(target, root):
                uses.append(ProcessUse(pid, name, target))

        fd_dir = process / "fd"
        try:
            descriptors = list(fd_dir.iterdir())
        except OSError:
            descriptors = []
        for descriptor in descriptors:
            target = _readlink(descriptor)
            if target is not None and _inside(target, root):
                uses.append(ProcessUse(pid, "fd", f"{descriptor.name}:{target}"))

        maps = _text(process / "maps")
        if maps is not None:
            for line in maps.splitlines():
                fields = line.split(maxsplit=5)
                if len(fields) == 6 and _inside(fields[5], root):
                    uses.append(ProcessUse(pid, "maps", fields[5]))
                    break

        command = _bytes(process / "cmdline")
        # A parent bash owns the literal cleanup command, including --cache-root,
        # but that text is not a reference to cache contents. Its cwd/fds/maps
        # remain checked above; only this command-line signal is suppressed.
        if command is not None and pid not in ancestors:
            rendered = command.replace(b"\0", b" ").decode("utf-8", "replace")
            if _inside(rendered.strip(), root) or str(root) in rendered:
                uses.append(ProcessUse(pid, "cmdline", rendered.strip()))

        comm = _text(process / "comm")
        if comm is not None and comm.strip() == "rust-script":
            environment = _bytes(process / "environ")
            if environment is None:
                uses.append(ProcessUse(pid, "rust-script-env", "unreadable"))
            else:
                values = environment.split(b"\0")
                raw_home = next(
                    (
                        value.removeprefix(b"HOME=")
                        for value in values
                        if value.startswith(b"HOME=")
                    ),
                    None,
                )
                raw_xdg = next(
                    (
                        value.removeprefix(b"XDG_CACHE_HOME=")
                        for value in values
                        if value.startswith(b"XDG_CACHE_HOME=")
                    ),
                    None,
                )
                process_cache = (
                    Path(os.fsdecode(raw_xdg)) / "rust-script"
                    if raw_xdg is not None
                    else Path(os.fsdecode(raw_home)) / ".cache/rust-script"
                    if raw_home is not None
                    else Path.home() / ".cache/rust-script"
                )
                if process_cache == root:
                    uses.append(ProcessUse(pid, "rust-script-env", str(process_cache)))

    return sorted(set(uses), key=lambda item: (item.pid, item.signal, item.detail))


def allocated_bytes(root: Path) -> int:
    """Return a hard-link-deduplicated st_blocks upper bound."""
    seen: set[tuple[int, int]] = set()
    total = 0
    try:
        for directory, names, files in os.walk(root, followlinks=False):
            for name in [*names, *files]:
                path = Path(directory) / name
                stat = path.lstat()
                key = (stat.st_dev, stat.st_ino)
                if key not in seen:
                    seen.add(key)
                    total += stat.st_blocks * 512
    except OSError as error:
        raise CleanupUnavailable(f"cannot measure {root}: {error}") from error
    return total


def _valid_cache_root(path: Path) -> None:
    if not path.is_absolute():
        raise CleanupUnavailable(f"cache root must be absolute: {path}")
    if path.name != "rust-script":
        raise CleanupUnavailable(f"cache root basename must be rust-script: {path}")
    if path.is_symlink() or (path.exists() and not path.is_dir()):
        raise CleanupUnavailable(f"cache root must be a real directory: {path}")
    if not path.parent.is_dir() or path.parent.is_symlink():
        raise CleanupUnavailable(f"cache parent must be a real directory: {path.parent}")


def _marker_payload(source: Path, token: str) -> dict[str, object]:
    return {"schema": 1, "source": str(source), "token": token}


def _marked_for(fence: Path, source: Path) -> bool:
    try:
        payload = json.loads((fence / MARKER).read_text(encoding="utf-8"))
    except (OSError, ValueError, TypeError):
        return False
    return (
        payload.get("schema") == 1
        and payload.get("source") == str(source)
        and payload.get("token") == fence.name.removeprefix(FENCE_PREFIX)
    )


def cleanup(cache: Path, maximum: int, apply: bool, proc_root: Path) -> tuple[int, str]:
    _valid_cache_root(cache)
    if not apply:
        if not cache.exists():
            return 0, f"cache is absent: {cache}"
        size = allocated_bytes(cache)
        if size <= maximum:
            return 0, f"cache is {size} bytes (limit {maximum})"
        return 1, f"cache is {size} bytes, above {maximum}; report-only"

    cache.mkdir(mode=0o755, exist_ok=True)
    lock_path = cache.parent / ".rust-script-cleanup.lock"
    with lock_path.open("a+", encoding="utf-8") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)

        retained_fences = []
        removed_fences = 0
        for fence in sorted(cache.parent.glob(f"{FENCE_PREFIX}*")):
            if not fence.is_dir() or fence.is_symlink() or not _marked_for(fence, cache):
                raise CleanupUnavailable(f"unrecognized rust-script cleanup fence: {fence}")
            users = process_uses(fence, proc_root)
            if users:
                retained_fences.append((fence, users))
            elif apply:
                shutil.rmtree(fence)
                removed_fences += 1

        size = allocated_bytes(cache)
        if size <= maximum:
            if retained_fences:
                names = ", ".join(fence.name for fence, _users in retained_fences)
                return 1, f"cache is {size} bytes; retained active cleanup fence(s): {names}"
            return (
                0,
                f"cache is {size} bytes (limit {maximum}); "
                f"removed {removed_fences} prior fence(s)",
            )
        users = process_uses(cache, proc_root)
        if users:
            detail = "; ".join(f"pid {use.pid} {use.signal}={use.detail}" for use in users[:3])
            return 1, f"cache is {size} bytes, above {maximum}; active use: {detail}"

        token = f"{os.getpid()}-{uuid.uuid4().hex[:12]}"
        marker = cache / MARKER
        marker.write_text(
            json.dumps(_marker_payload(cache, token), sort_keys=True) + "\n",
            encoding="utf-8",
        )
        fence = cache.parent / f"{FENCE_PREFIX}{token}"
        os.rename(cache, fence)
        cache.mkdir(mode=0o755)

        users = process_uses(fence, proc_root)
        if users:
            detail = "; ".join(f"pid {use.pid} {use.signal}={use.detail}" for use in users[:3])
            return 1, f"fenced {size} bytes but retained active {fence.name}: {detail}"

        shutil.rmtree(fence)
        return (
            0,
            f"removed generated rust-script cache measured at {size} bytes; "
            f"fresh cache is {cache}",
        )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--cache-root", type=Path, default=default_cache_root())
    result.add_argument("--max-gib", type=float, default=DEFAULT_MAX_GIB)
    result.add_argument("--apply", action="store_true")
    result.add_argument("--proc-root", type=Path, default=Path("/proc"), help=argparse.SUPPRESS)
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if not math.isfinite(args.max_gib) or args.max_gib < 0:
        print("state=could-not-determine")
        print("summary=--max-gib must be non-negative")
        return 2
    try:
        code, summary = cleanup(
            args.cache_root.absolute(), int(args.max_gib * GIB), args.apply, args.proc_root
        )
    except (CleanupUnavailable, OSError, OverflowError) as error:
        print("state=could-not-determine")
        print(f"summary={error}")
        return 2
    print(f"state={'ok' if code == 0 else 'action-required'}")
    print(f"summary={summary}")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
