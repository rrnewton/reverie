#!/usr/bin/env python3
"""Mirror the canonical cell series and its generated scorecard repository.

The writers keep appending to the established dev-hermit series and spool. This
reader captures their canonical union, appends every new event to
``rrnewton/hermit_test_ledger``, and builds the scorecard from the exact series
commit it just made. The final mechanical commit and ``latest.json`` bind both
scorecard files to that series commit and retain the upstream dev-hermit commit,
series tree, live-source identities, row count, and row digest.

This command reports failure with a nonzero exit. ``validate-run`` invokes it
only after the canonical verdict exists, records the full outcome in the run
handle, and deliberately returns the pre-existing validate result regardless of
this command's outcome. A failed mirror is therefore visible without becoming
a validation admission condition.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from collections import defaultdict
from pathlib import Path
from typing import Any, Mapping, Sequence


TOOL_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "lib"))
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "ledger"))
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "series"))

import git_env  # noqa: E402
import publisher  # noqa: E402
import series  # noqa: E402


REMOTE_URL = "https://github.com/rrnewton/hermit_test_ledger.git"
REMOTE_BRANCH = "main"
SOURCE_URL = "https://github.com/rrnewton/dev-hermit.git"
METADATA_SCHEMA = "hermit-test-ledger/v1"
SCORECARD_PATHS = (Path("SCORECARD.md"), Path("ci/compat-envelope/cells.json"))
MIRRORED_SCORECARD_PATHS = (
    Path("scorecard/SCORECARD.md"),
    Path("scorecard/cells.json"),
)
DEFAULT_TIMEOUT_SECONDS = 900
DEFAULT_ATTEMPTS = 3


class MirrorRefused(RuntimeError):
    """The mirror could not prove that its remote contains this snapshot."""


class MirrorPushRejected(MirrorRefused):
    """The remote moved while a mechanical mirror commit was being pushed."""


def _environment() -> dict[str, str]:
    environment = git_env.sanitized_git_env()
    environment.setdefault("GIT_AUTHOR_NAME", "Hermit ledger mirror")
    environment.setdefault("GIT_AUTHOR_EMAIL", "hermit-ledger-mirror@example.invalid")
    environment.setdefault("GIT_COMMITTER_NAME", "Hermit ledger mirror")
    environment.setdefault(
        "GIT_COMMITTER_EMAIL", "hermit-ledger-mirror@example.invalid"
    )
    return environment


def _run(
    command: Sequence[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
    timeout: int = DEFAULT_TIMEOUT_SECONDS,
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            list(command),
            cwd=cwd,
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
            env=_environment(),
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise MirrorRefused(f"cannot run {command[0]}: {error}") from error
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout).strip() or f"exit {result.returncode}"
        raise MirrorRefused(f"{command[0]} failed: {detail}")
    return result


def _network(
    command: Sequence[str], *, cwd: Path | None, wrapper: str
) -> subprocess.CompletedProcess[str]:
    return _run([wrapper, *command], cwd=cwd)


def capture_snapshot(parent: Path) -> dict[str, Any]:
    return series.scorecard_projection_snapshot(parent)


def _row_shard(row: Mapping[str, Any]) -> str:
    emitted_at = row.get("emitted_at")
    team = row.get("team")
    host = row.get("host")
    if not isinstance(emitted_at, str) or len(emitted_at) < 7:
        raise MirrorRefused("series row has no YYYY-MM emitted_at prefix")
    if not isinstance(team, str) or not isinstance(host, str):
        raise MirrorRefused("series row has no team or host")
    try:
        return series.shard_path(team, host, emitted_at[:7])
    except series.SeriesError as error:
        raise MirrorRefused(f"cannot place series row: {error}") from error


def write_series(repo: Path, rows: Sequence[Mapping[str, Any]]) -> list[str]:
    """Write the canonical rows while preserving every already-published line."""
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        grouped[_row_shard(row)].append(dict(row))

    existing = sorted((repo / "series").glob("*/*/*.jsonl"))
    unexpected = [
        path.relative_to(repo).as_posix()
        for path in existing
        if path.relative_to(repo).as_posix() not in grouped
    ]
    if unexpected:
        raise MirrorRefused(
            "remote mirror has series shards absent from the canonical source: "
            + ", ".join(unexpected)
        )

    changed: list[str] = []
    for relative, events in sorted(grouped.items()):
        path = repo / relative
        previous = path.read_text() if path.exists() else ""
        try:
            merged = publisher.merge_shard(
                previous,
                events,
                store="series",
                read_events=True,
            )
        except (publisher.PublishRefused, series.SeriesError) as error:
            raise MirrorRefused(f"cannot mirror {relative}: {error}") from error
        if merged == previous:
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(merged)
        changed.append(relative)
    return changed


def _scorecard_stamp(snapshot: Mapping[str, Any]) -> str:
    rows = snapshot.get("rows")
    if not isinstance(rows, list):
        raise MirrorRefused("series snapshot rows are not an array")
    stamps = sorted(
        row.get("emitted_at")
        for row in rows
        if isinstance(row, dict) and isinstance(row.get("emitted_at"), str)
    )
    return stamps[-1] if stamps else str(snapshot["source"]["commit"])


def _clear_scorecard_history(worktree: Path) -> None:
    """Make the generated baseline describe only evidence in the mirrored series."""
    path = worktree / "ci/compat-envelope/cells.json"
    try:
        value = json.loads(path.read_text())
        cells = value["cells"]
        if not isinstance(cells, list):
            raise TypeError("cells is not an array")
        for cell in cells:
            if not isinstance(cell, dict):
                raise TypeError("cell is not an object")
            cell["observations"] = []
            cell["last_tested"] = None
        value["projection"] = None
        path.write_text(json.dumps(value, separators=(",", ":")) + "\n")
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        raise MirrorRefused(
            f"cannot prepare scorecard series-only baseline: {error}"
        ) from error


def build_scorecard(
    source_checkout: Path,
    target: str,
    snapshot: Mapping[str, Any],
    series_root: Path,
    output_root: Path,
    *,
    net_wrapper: str,
) -> dict[str, str]:
    """Build the scorecard in a disposable worktree at the validated commit."""
    worktree = output_root / "hermit"
    added = False
    try:
        _run(
            [
                "git",
                "-C",
                str(source_checkout),
                "worktree",
                "add",
                "--detach",
                str(worktree),
                f"{target}^{{commit}}",
            ]
        )
        added = True
        agent_utils = source_checkout / "agent-utils"
        if not (agent_utils / ".git").exists():
            raise MirrorRefused(
                f"source checkout has no initialized agent-utils submodule: {agent_utils}"
            )
        _run(
            [
                "git",
                "-C",
                str(worktree),
                "-c",
                "protocol.file.allow=always",
                "-c",
                f"submodule.agent-utils.url={agent_utils}",
                "submodule",
                "update",
                "--init",
                "--checkout",
                "--",
                "agent-utils",
            ]
        )
        tool = worktree / "ci/compat-envelope/scorecard.rs"
        if not tool.is_file():
            raise MirrorRefused(
                f"validated Hermit commit has no scorecard tool: {tool}"
            )
        _clear_scorecard_history(worktree)
        _network([str(tool), "update"], cwd=worktree, wrapper=net_wrapper)
        _network(
            [
                str(tool),
                "project-observations",
                "--series-root",
                str(series_root),
                "--refreshed-at",
                _scorecard_stamp(snapshot),
            ],
            cwd=worktree,
            wrapper=net_wrapper,
        )
        identities: dict[str, str] = {}
        for source, destination in zip(SCORECARD_PATHS, MIRRORED_SCORECARD_PATHS):
            source_path = worktree / source
            if not source_path.is_file():
                raise MirrorRefused(f"scorecard build did not write {source}")
            target_path = output_root / destination
            target_path.parent.mkdir(parents=True, exist_ok=True)
            payload = source_path.read_bytes()
            target_path.write_bytes(payload)
            identities[destination.as_posix()] = hashlib.sha256(payload).hexdigest()
        return identities
    finally:
        if added:
            _run(
                [
                    "git",
                    "-C",
                    str(source_checkout),
                    "worktree",
                    "remove",
                    "--force",
                    str(worktree),
                ],
                check=False,
            )


def write_metadata(
    repo: Path,
    snapshot: Mapping[str, Any],
    target: str,
    series_commit: str,
    scorecard: Mapping[str, str],
) -> None:
    source = snapshot.get("source")
    if not isinstance(source, dict):
        raise MirrorRefused("series snapshot has no source identity")
    value = {
        "schema": METADATA_SCHEMA,
        "source_repository": SOURCE_URL,
        "source_commit": source.get("commit"),
        "source_tree": source.get("tree"),
        "rows": snapshot.get("rows_read"),
        "rows_sha256": snapshot.get("rows_sha256"),
        "local_sources": source.get("local_sources"),
        "series_commit": series_commit,
        "scorecard_hermit_commit": target,
        "scorecard": dict(sorted(scorecard.items())),
    }
    (repo / "latest.json").write_text(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    )


def _prepare_clone(destination: Path, remote_url: str, *, net_wrapper: str) -> bool:
    _network(
        ["git", "clone", "--quiet", remote_url, str(destination)],
        cwd=None,
        wrapper=net_wrapper,
    )
    remote_main = _run(
        [
            "git",
            "-C",
            str(destination),
            "show-ref",
            "--verify",
            "refs/remotes/origin/main",
        ],
        check=False,
    )
    if remote_main.returncode == 0:
        _run(["git", "-C", str(destination), "checkout", "--detach", "origin/main"])
        return True
    return False


def submodule_remote(parent: Path) -> str:
    configured = _run(
        [
            "git",
            "config",
            "--file",
            str(parent / ".gitmodules"),
            "--get",
            "submodule.hermit_test_ledger.url",
        ]
    ).stdout.strip()
    if configured != REMOTE_URL:
        raise MirrorRefused(
            "hermit_test_ledger submodule URL is absent or unexpected: "
            f"{configured!r}"
        )
    return configured


def sync_once(
    parent: Path,
    source_checkout: Path,
    target: str,
    remote_url: str,
    *,
    net_wrapper: str,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="hermit-test-ledger-") as temporary:
        temp = Path(temporary)
        snapshot = capture_snapshot(parent)
        destination = temp / "repository"
        had_main = _prepare_clone(destination, remote_url, net_wrapper=net_wrapper)
        previous_series_commit: str | None = None
        if had_main:
            metadata_path = destination / "latest.json"
            cells_path = destination / "scorecard/cells.json"
            try:
                metadata = json.loads(metadata_path.read_text())
                previous_series_commit = metadata.get("series_commit")
                if previous_series_commit is None:
                    cells = json.loads(cells_path.read_text())
                    previous_series_commit = cells["projection"]["source_commit"]
            except (OSError, KeyError, TypeError, json.JSONDecodeError) as error:
                raise MirrorRefused(
                    f"cannot read prior mirror series commit: {error}"
                ) from error
            if not isinstance(previous_series_commit, str):
                raise MirrorRefused("prior mirror has no series commit")
        changed = write_series(destination, snapshot["rows"])
        if not (destination / "README.md").exists():
            (destination / "README.md").write_text(
                "# Hermit test ledger\n\n"
                "This repository mirrors Hermit's canonical cell series and the scorecard "
                "built from that data after validation completes.\n"
            )
        source_status = _run(["git", "-C", str(destination), "status", "--porcelain"])
        if source_status.stdout.strip():
            _run(
                [
                    "git",
                    "-C",
                    str(destination),
                    "add",
                    "--",
                    "README.md",
                    "series",
                ]
            )
            _run(
                [
                    "git",
                    "-C",
                    str(destination),
                    "commit",
                    "--quiet",
                    "-m",
                    f"Mirror {snapshot['rows_read']} series row(s)",
                ]
            )
        series_commit = _run(
            [
                "git",
                "-C",
                str(destination),
                "rev-parse",
                (
                    "HEAD^{commit}"
                    if source_status.stdout.strip()
                    else f"{previous_series_commit}^{{commit}}"
                ),
            ]
        ).stdout.strip()
        series_source = temp / "series-source"
        _run(
            [
                "git",
                "-C",
                str(destination),
                "worktree",
                "add",
                "--detach",
                str(series_source),
                series_commit,
            ]
        )
        try:
            scorecard = build_scorecard(
                source_checkout,
                target,
                snapshot,
                series_source / "series",
                destination,
                net_wrapper=net_wrapper,
            )
        finally:
            _run(
                [
                    "git",
                    "-C",
                    str(destination),
                    "worktree",
                    "remove",
                    "--force",
                    str(series_source),
                ],
                check=False,
            )
        write_metadata(destination, snapshot, target, series_commit, scorecard)

        status = _run(["git", "-C", str(destination), "status", "--porcelain"])
        if not status.stdout.strip():
            remote_commit = _run(
                ["git", "-C", str(destination), "rev-parse", "HEAD^{commit}"]
            ).stdout.strip()
            return {
                "state": "unchanged",
                "commit": remote_commit,
                "source_commit": snapshot["source"]["commit"],
                "source_tree": snapshot["source"]["tree"],
                "rows": snapshot["rows_read"],
                "series_shards_changed": 0,
                "series_commit": series_commit,
            }

        _run(
            [
                "git",
                "-C",
                str(destination),
                "add",
                "--",
                "latest.json",
                "scorecard",
            ]
        )
        _run(
            [
                "git",
                "-C",
                str(destination),
                "commit",
                "--quiet",
                "-m",
                "Build scorecard from latest series",
            ]
        )
        commit = _run(
            ["git", "-C", str(destination), "rev-parse", "HEAD^{commit}"]
        ).stdout.strip()
        try:
            _network(
                ["git", "push", "origin", f"HEAD:refs/heads/{REMOTE_BRANCH}"],
                cwd=destination,
                wrapper=net_wrapper,
            )
        except MirrorRefused as error:
            raise MirrorPushRejected(str(error)) from error
        _network(
            ["git", "fetch", "--quiet", "origin", REMOTE_BRANCH],
            cwd=destination,
            wrapper=net_wrapper,
        )
        ancestry = _run(
            [
                "git",
                "-C",
                str(destination),
                "merge-base",
                "--is-ancestor",
                commit,
                "origin/main",
            ],
            check=False,
        )
        if ancestry.returncode != 0:
            raise MirrorRefused(
                "push returned success but the mechanical commit is absent from "
                "freshly fetched remote main"
            )
        return {
            "state": "published",
            "commit": commit,
            "source_commit": snapshot["source"]["commit"],
            "source_tree": snapshot["source"]["tree"],
            "rows": snapshot["rows_read"],
            "series_shards_changed": len(changed),
            "series_commit": series_commit,
            "remote_was_empty": not had_main,
        }


def mirror(
    parent: Path,
    source_checkout: Path,
    target: str,
    remote_url: str,
    *,
    net_wrapper: str,
    attempts: int = DEFAULT_ATTEMPTS,
) -> dict[str, Any]:
    last_error = ""
    for attempt in range(1, attempts + 1):
        try:
            report = sync_once(
                parent,
                source_checkout,
                target,
                remote_url,
                net_wrapper=net_wrapper,
            )
            report["attempts"] = attempt
            return report
        except MirrorPushRejected as error:
            last_error = str(error)
    raise MirrorRefused(f"gave up after {attempts} attempt(s): {last_error}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parent", type=Path, required=True)
    parser.add_argument("--source-checkout", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument(
        "--net-wrapper", default=os.environ.get("WITH_PROXY", "with-proxy")
    )
    parser.add_argument("--attempts", type=int, default=DEFAULT_ATTEMPTS)
    args = parser.parse_args(argv)
    if args.attempts <= 0:
        print("ledger mirror: FAILED: attempts must be positive", file=sys.stderr)
        return 2
    try:
        report = mirror(
            args.parent.resolve(),
            args.source_checkout.resolve(),
            args.target,
            submodule_remote(args.parent.resolve()),
            net_wrapper=args.net_wrapper,
            attempts=args.attempts,
        )
    except (MirrorRefused, OSError, KeyError, TypeError, series.SeriesError) as error:
        print(f"ledger mirror: FAILED: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
