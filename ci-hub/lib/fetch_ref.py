#!/usr/bin/env python3
"""Fetch a remote branch without racing a shared remote-tracking ref.

Linked Git worktrees share their repository's ref store.  A normal fetch of a
configured remote may therefore spend seconds downloading objects and then lose
Git's compare-and-swap while updating ``refs/remotes/origin/main``.  Fetch the
objects into a unique ref first, then reconcile only the shared tracking ref
with a bounded, fresh-read CAS.  Network transfers remain concurrent.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import uuid
from collections.abc import Callable, Sequence
from dataclasses import asdict, dataclass
from pathlib import Path

ZERO_OID = "0" * 40
OID_RE = re.compile(r"^[0-9a-f]{40}$")
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]*$")


class FetchRefError(RuntimeError):
    """The fetch or the fail-closed tracking-ref reconciliation failed."""


@dataclass(frozen=True)
class FetchResult:
    remote: str
    branch: str
    tracking_ref: str
    previous_sha: str | None
    fetched_sha: str
    tracking_sha: str
    cas_retries: int
    disposition: str


BeforeCas = Callable[[int, str | None, str], None]


def _git(
    repo: Path,
    *args: str,
    proxy: bool = False,
    check: bool = True,
    timeout_s: float | None = None,
) -> subprocess.CompletedProcess[str]:
    argv: list[str] = []
    if proxy:
        argv.append("with-proxy")
    argv.extend(("git", "-C", str(repo), *args))
    try:
        run = subprocess.run(
            argv, capture_output=True, text=True, timeout=timeout_s
        )
    except subprocess.TimeoutExpired as error:
        raise FetchRefError(
            f"{' '.join(argv)} timed out after {timeout_s}s"
        ) from error
    if check and run.returncode != 0:
        detail = (run.stderr or run.stdout).strip()
        raise FetchRefError(
            f"{' '.join(argv)} exited {run.returncode}: {detail or '<no detail>'}"
        )
    return run


def _resolve(repo: Path, ref: str) -> str | None:
    run = _git(repo, "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}", check=False)
    if run.returncode == 1:
        return None
    if run.returncode != 0:
        detail = (run.stderr or run.stdout).strip()
        raise FetchRefError(f"cannot resolve {ref}: {detail or f'exit {run.returncode}'}")
    oid = run.stdout.strip().lower()
    if not OID_RE.fullmatch(oid):
        raise FetchRefError(f"{ref} resolved to invalid commit identity {oid!r}")
    return oid


def _is_ancestor(repo: Path, older: str, newer: str) -> bool:
    run = _git(repo, "merge-base", "--is-ancestor", older, newer, check=False)
    if run.returncode not in (0, 1):
        detail = (run.stderr or run.stdout).strip()
        raise FetchRefError(
            f"cannot compare fetched refs {older}..{newer}: {detail or f'exit {run.returncode}'}"
        )
    return run.returncode == 0


def _remote_branch_tip(
    repo: Path,
    remote_url: str,
    source_ref: str,
    *,
    use_proxy: bool,
    timeout_s: float,
) -> str:
    """Read the current remote authority without updating any local ref."""

    run = _git(
        repo,
        "ls-remote",
        "--exit-code",
        "--refs",
        remote_url,
        source_ref,
        proxy=use_proxy,
        check=False,
        timeout_s=timeout_s,
    )
    if run.returncode != 0:
        detail = (run.stderr or run.stdout).strip()
        raise FetchRefError(
            f"cannot confirm current remote tip {source_ref}: "
            f"{detail or f'exit {run.returncode}'}"
        )
    matches = []
    for line in run.stdout.splitlines():
        fields = line.split()
        if (
            len(fields) == 2
            and fields[1] == source_ref
            and OID_RE.fullmatch(fields[0])
        ):
            matches.append(fields[0].lower())
    if len(matches) != 1:
        raise FetchRefError(
            f"cannot confirm current remote tip {source_ref}: "
            f"expected one exact match, found {len(matches)}"
        )
    return matches[0]


def _validate_name(kind: str, value: str) -> None:
    if not NAME_RE.fullmatch(value) or value.startswith("-") or ".." in value:
        raise FetchRefError(f"invalid {kind} name {value!r}")


def fetch_remote_tracking_branch(
    repo: Path | str,
    *,
    remote: str = "origin",
    branch: str = "main",
    use_proxy: bool = True,
    max_cas_attempts: int = 3,
    network_timeout_s: float = 120.0,
    before_cas: BeforeCas | None = None,
    sleep: Callable[[float], None] = time.sleep,
) -> FetchResult:
    """Fetch ``remote/branch`` and safely advance its shared tracking ref.

    The network fetch writes a process-unique ref, never the shared tracking
    ref.  The latter is changed only by ``git update-ref <new> <old>``.  A CAS
    loser re-reads the ref before deciding whether to retry, accept a newer
    descendant installed by another fetcher, or refuse a divergent update.
    """

    repo = Path(repo).resolve()
    _validate_name("remote", remote)
    _validate_name("branch", branch)
    if max_cas_attempts < 1 or max_cas_attempts > 8:
        raise FetchRefError("max_cas_attempts must be between 1 and 8")

    source_ref = f"refs/heads/{branch}"
    tracking_ref = f"refs/remotes/{remote}/{branch}"
    unique_ref = (
        f"refs/ci-hub/fetch/{os.getpid()}-{uuid.uuid4().hex}/{remote}/{branch}"
    )
    previous = _resolve(repo, tracking_ref)
    fetched: str | None = None

    try:
        remote_url_run = _git(repo, "remote", "get-url", remote)
        remote_url = remote_url_run.stdout.strip()
        if not remote_url:
            raise FetchRefError(f"remote {remote!r} has no fetch URL")
        # An explicit destination prevents Git's opportunistic update of the
        # configured remote-tracking ref ONLY when the fetch source is not the
        # configured remote alias.  Git still applies remote.<name>.fetch as an
        # opportunistic update when given both the alias and an explicit unique
        # destination.  Fetching the exact same URL avoids that hidden second
        # write.  The destination is unique, so only this process can contend on
        # it; the long network transfer remains unlocked.
        _git(
            repo,
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--quiet",
            remote_url,
            f"{source_ref}:{unique_ref}",
            proxy=use_proxy,
            timeout_s=network_timeout_s,
        )
        fetched = _resolve(repo, unique_ref)
        if fetched is None:
            raise FetchRefError(f"fetch succeeded but did not create {unique_ref}")

        def confirm_newer_shared_tip(candidate: str) -> None:
            # fetched < candidate has two indistinguishable local shapes:
            # another fetcher installed a genuinely newer remote tip, or the
            # remote was rewound and our tracking ref is now stale-ahead.  A
            # read-only authority query separates them without locking either
            # network operation.  Only an exact current remote tip is accepted.
            remote_tip = _remote_branch_tip(
                repo,
                remote_url,
                source_ref,
                use_proxy=use_proxy,
                timeout_s=network_timeout_s,
            )
            if remote_tip != candidate:
                raise FetchRefError(
                    f"REFUSED: fetched {remote}/{branch} {fetched} does not "
                    f"fast-forward {tracking_ref} {candidate}; current remote "
                    f"tip is {remote_tip}, not the newer local value"
                )

        expected = _resolve(repo, tracking_ref)
        last_detail = ""
        for attempt in range(max_cas_attempts):
            if expected == fetched:
                return FetchResult(
                    remote,
                    branch,
                    tracking_ref,
                    previous,
                    fetched,
                    fetched,
                    attempt,
                    "already-current",
                )

            # Fetching into a new ref deliberately avoids Git's implicit
            # tracking-ref transaction, so preserve its non-fast-forward
            # safety explicitly.  A remote rewrite is not a contention retry.
            if expected is not None and _is_ancestor(repo, fetched, expected):
                confirm_newer_shared_tip(expected)
                return FetchResult(
                    remote,
                    branch,
                    tracking_ref,
                    previous,
                    fetched,
                    expected,
                    attempt,
                    "newer-shared-ref-kept",
                )
            if expected is not None and not _is_ancestor(repo, expected, fetched):
                raise FetchRefError(
                    f"REFUSED: fetched {remote}/{branch} {fetched} does not "
                    f"fast-forward {tracking_ref} {expected}"
                )

            if before_cas is not None:
                before_cas(attempt, expected, fetched)

            update = _git(
                repo,
                "update-ref",
                "--no-deref",
                tracking_ref,
                fetched,
                expected or ZERO_OID,
                check=False,
            )
            if update.returncode == 0:
                return FetchResult(
                    remote,
                    branch,
                    tracking_ref,
                    previous,
                    fetched,
                    fetched,
                    attempt,
                    "advanced",
                )

            # KEEP THE REFUSAL: the failed CAS is evidence the earlier read is
            # stale.  Re-read before doing anything; never blindly repeat it.
            last_detail = (update.stderr or update.stdout).strip()
            current = _resolve(repo, tracking_ref)
            if current == fetched:
                return FetchResult(
                    remote,
                    branch,
                    tracking_ref,
                    previous,
                    fetched,
                    fetched,
                    attempt + 1,
                    "concurrent-same-tip",
                )
            if current is not None and _is_ancestor(repo, fetched, current):
                confirm_newer_shared_tip(current)
                return FetchResult(
                    remote,
                    branch,
                    tracking_ref,
                    previous,
                    fetched,
                    current,
                    attempt + 1,
                    "newer-shared-ref-kept",
                )
            if current is not None and not _is_ancestor(repo, current, fetched):
                raise FetchRefError(
                    f"REFUSED: {tracking_ref} moved sideways while fetching {fetched}; "
                    f"fresh value is {current}. Original CAS refusal: {last_detail}"
                )

            expected = current
            if attempt + 1 < max_cas_attempts:
                sleep(0.025 * (2**attempt))

        current = _resolve(repo, tracking_ref)
        raise FetchRefError(
            f"REFUSED after {max_cas_attempts} fresh-read CAS attempts: "
            f"{tracking_ref} is {current or '<missing>'}, fetched {fetched}. "
            f"Last CAS refusal: {last_detail or '<no detail>'}"
        )
    finally:
        if fetched is not None:
            cleanup = _git(
                repo,
                "update-ref",
                "--no-deref",
                "-d",
                unique_ref,
                fetched,
                check=False,
            )
            if cleanup.returncode != 0:
                detail = (cleanup.stderr or cleanup.stdout).strip()
                print(
                    f"fetch-ref: WARNING: could not remove unique ref {unique_ref}: "
                    f"{detail or f'exit {cleanup.returncode}'}",
                    file=sys.stderr,
                )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--branch", default="main")
    parser.add_argument("--no-proxy", action="store_true")
    parser.add_argument("--max-cas-attempts", type=int, default=3)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        result = fetch_remote_tracking_branch(
            args.repo,
            remote=args.remote,
            branch=args.branch,
            use_proxy=not args.no_proxy,
            max_cas_attempts=args.max_cas_attempts,
        )
    except FetchRefError as error:
        print(f"fetch-ref: {error}", file=sys.stderr)
        return 2
    print(json.dumps(asdict(result), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
