#!/usr/bin/env python3
"""Record a soft-green Hermit main tip and launch its exact-SHA VALIDATE.

The rebase store describes a candidate before it lands.  This module owns the
first point at which that candidate can truthfully be called ``main``: after the
tracked lander has observed the merge commit and proved that its tree is the
candidate tree.  Recording and launching stay one operation; callers never
create an obligation for an agent to remember later.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any, Callable, Mapping, Sequence
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub"))

import qualifying_receipt  # noqa: E402

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
SOFT_STATES = {"soft-green(zero-conflict)", "soft-green(resolver-judged)"}
SKIP_WARNING_ENV = "CI_HUB_SKIP_SOFT_MAIN_WARNING"


class SoftMainError(RuntimeError):
    """The soft-main binding or direct launch could not be proved."""


def default_store(root: Path = ROOT) -> Path:
    override = os.environ.get("CI_HUB_SOFT_MAIN_STORE")
    return Path(override) if override else root / "ignored/ci-hub/main-soft-green.jsonl"


def default_rebase_store(root: Path = ROOT) -> Path:
    override = os.environ.get("CI_HUB_REBASE_STORE")
    return Path(override) if override else root / "ignored/rebase-records.jsonl"


def default_main_checkout(root: Path = ROOT) -> Path:
    override = os.environ.get("CI_HUB_SOFT_MAIN_CHECKOUT")
    return Path(override) if override else root / "hermit"


def _run(
    command: Sequence[str],
    *,
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[str]:
    return bounded_capture.capture(
        list(command), cwd=cwd, env=env, check=check,
    )


def _git(checkout: Path, *arguments: str) -> str:
    result = _run(("git", "-C", str(checkout), *arguments))
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise SoftMainError(f"git {' '.join(arguments)} failed in {checkout}: {detail}")
    return result.stdout.strip()


def _tree(checkout: Path, commit: str) -> str:
    return _git(checkout, "rev-parse", f"{commit}^{{tree}}")


def _load_jsonl(path: Path) -> list[dict[str, Any]]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        return []
    except OSError as error:
        raise SoftMainError(f"cannot read {path}: {error}") from error
    records: list[dict[str, Any]] = []
    for line in lines:
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            records.append(value)
    return records


def _latest(records: Sequence[Mapping[str, Any]], field: str) -> dict[str, dict[str, Any]]:
    latest: dict[str, dict[str, Any]] = {}
    for record in records:
        key = record.get(field)
        if isinstance(key, str) and key:
            latest[key] = dict(record)
    return latest


def _append(path: Path, record: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        with path.open("a", encoding="utf-8") as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
            handle.write(json.dumps(dict(record), sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    except OSError as error:
        raise SoftMainError(f"cannot append soft-main record {path}: {error}") from error


def canonical_state(root: Path, sha: str) -> tuple[str, str]:
    """Return HARD_GREEN, NO_RESULT, or UNKNOWN from the canonical consumer."""
    command = os.environ.get("CI_HUB_SOFT_MAIN_VALIDATE_STATUS")
    if command:
        result = _run([command, "--sha", sha, "--json"], cwd=root)
        detail = (result.stdout or result.stderr).strip()
        if result.returncode == 0:
            return "HARD_GREEN", detail
        if result.returncode == 4:
            return "NO_RESULT", detail
        return "UNKNOWN", detail or f"validate-status exited {result.returncode}"

    # Do not recursively invoke the rust-script ci-hub front door from every
    # ci-hub contact. That doubles compilation/dispatch cost and can contend on
    # the very package lock operational status is meant to diagnose. Read the
    # canonical union and call its one Python predicate directly instead.
    rows = _run(
        (sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"),
        cwd=root,
    )
    if rows.returncode != 0:
        return "UNKNOWN", (rows.stderr or rows.stdout).strip()
    predicate = qualifying_receipt.active()
    candidates = []
    for line in rows.stdout.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(row, dict) or row.get("commit") != sha:
            continue
        if row.get("repo") not in (None, "hermit", "rrnewton/hermit"):
            continue
        candidates.append(row)
    try:
        # The native checker owns operational-state resolution, including main
        # worktrees and DEV_HERMIT_PARENT. This is the tool checkout, not a store.
        with qualifying_receipt.validation_evidence_batch(candidates, predicate=predicate):
            for row in candidates:
                accepted, _reason = qualifying_receipt.row_qualification(row, sha, predicate)
                if accepted:
                    return "HARD_GREEN", "canonical exact-SHA qualifying receipt found"
    except qualifying_receipt.ValidationEvidenceReaderFault as error:
        return "UNKNOWN", str(error)
    return "NO_RESULT", f"canonical ledger has {len(candidates)} row(s) for exact main without qualifying hard green"


def current_main_state(
    root: Path = ROOT,
    *,
    store: Path | None = None,
    checkout: Path | None = None,
) -> dict[str, Any]:
    """Derive current-main hard/soft state without treating uncertainty as red."""
    checkout = checkout or default_main_checkout(root)
    store = store or default_store(root)
    try:
        sha = _git(checkout, "rev-parse", "refs/remotes/origin/main^{commit}")
    except SoftMainError as error:
        return {"state": "UNKNOWN", "reason": str(error), "commit": None}
    if not SHA_RE.fullmatch(sha):
        return {"state": "UNKNOWN", "reason": "origin/main is not exact 40-hex", "commit": sha}
    marker = _latest(_load_jsonl(store), "commit").get(sha)
    if marker is None or marker.get("state") != "SOFT_GREEN":
        # This contact hook is not a general main-health query. Without an exact
        # soft-main marker there is no warning it could legitimately print, so
        # do not charge every ci-hub command a full canonical ledger census.
        return {
            "state": "UNKNOWN",
            "commit": sha,
            "reason": "current main has no exact soft-main record",
        }
    authority, detail = canonical_state(root, sha)
    if authority == "HARD_GREEN":
        return {"state": "HARD_GREEN", "commit": sha, "reason": detail}
    if (
        authority == "NO_RESULT"
    ):
        return {
            "state": "SOFT_GREEN",
            "commit": sha,
            "reason": "exact main tip has recorded soft-green landing provenance but no hard-green receipt",
            "record": marker,
            "authority_state": authority,
            "authority_detail": detail,
        }
    return {
        "state": "UNKNOWN",
        "commit": sha,
        "reason": (
            "canonical exact-SHA authority is unreadable"
            if authority == "UNKNOWN"
            else "current main has no hard-green receipt and no exact soft-main record"
        ),
        "authority_state": authority,
        "authority_detail": detail,
    }


def warning_text(state: Mapping[str, Any]) -> str | None:
    if state.get("state") != "SOFT_GREEN":
        return None
    sha = str(state.get("commit") or "unknown")
    record = state.get("record") if isinstance(state.get("record"), Mapping) else {}
    unit = record.get("unit") or "unknown"
    return (
        f"ci-hub: WARNING: current Hermit main {sha} is SOFT_GREEN, not HARD_GREEN; "
        f"exact-main VALIDATE was launched as {unit}. Until its exact-SHA receipt "
        "qualifies, main remains out of compliance."
    )


def _prepare_source(root: Path, sha: str, workspace: Path) -> Path:
    source = workspace / "source"
    if source.exists():
        actual = _git(source, "rev-parse", "HEAD^{commit}")
        if actual != sha:
            raise SoftMainError(
                f"existing direct-launch source is {actual}, expected {sha}; refusing to overwrite it"
            )
        return source
    workspace.mkdir(parents=True, exist_ok=True)
    result = _run((
        "git", "clone", "--shared", "--no-checkout",
        str(default_main_checkout(root)), str(source),
    ))
    if result.returncode != 0:
        raise SoftMainError(f"cannot clone direct-launch source: {(result.stderr or result.stdout).strip()}")
    _git(source, "checkout", "--detach", sha)
    return source


def _spawn_runner(
    root: Path, *, sha: str, source: Path, unit: str, agent: str, log: Path
) -> int:
    log.parent.mkdir(parents=True, exist_ok=True)
    output = log.open("ab", buffering=0)
    try:
        process = subprocess.Popen(
            [
                "nohup", "setsid", "--fork", "--wait", "--",
                sys.executable, str(Path(__file__).resolve()), "_run",
                "--root", str(root), "--sha", sha, "--source", str(source),
                "--unit", unit, "--agent", agent,
            ],
            stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
            start_new_session=True, close_fds=True,
        )
    finally:
        output.close()
    return process.pid


def record_and_launch(
    *,
    root: Path,
    landed_sha: str,
    candidate: str,
    source_rev: str,
    base: str,
    agent: str,
    store: Path | None = None,
    rebase_store: Path | None = None,
    spawn: Callable[..., int] = _spawn_runner,
) -> dict[str, Any]:
    """Prove exact landed soft state, launch first, then record the launch."""
    for name, value in (("landed_sha", landed_sha), ("candidate", candidate),
                        ("source_rev", source_rev), ("base", base)):
        if not SHA_RE.fullmatch(value):
            raise SoftMainError(f"{name} must be exact lowercase 40-hex")
    checkout = default_main_checkout(root)
    observed_main = _git(checkout, "rev-parse", "refs/remotes/origin/main^{commit}")
    if observed_main != landed_sha:
        raise SoftMainError(
            f"landed SHA {landed_sha} is not current origin/main {observed_main}"
        )
    if _tree(checkout, landed_sha) != _tree(checkout, candidate):
        raise SoftMainError("landed main tree differs from the soft-green candidate tree")

    rebase_store = rebase_store or default_rebase_store(root)
    provenance = _latest(_load_jsonl(rebase_store), "result").get(candidate)
    if provenance is None:
        raise SoftMainError("soft-green candidate has no rebase provenance record")
    if provenance.get("soft_green") not in SOFT_STATES:
        raise SoftMainError("candidate rebase provenance is not soft-green")
    if provenance.get("source_rev") != source_rev or provenance.get("base") != base:
        raise SoftMainError("candidate rebase provenance does not match the land intent")

    authority, detail = canonical_state(root, landed_sha)
    if authority == "HARD_GREEN":
        return {
            "state": "HARD_GREEN", "commit": landed_sha, "launched": False,
            "reason": "exact landed SHA already has qualifying authority",
            "authority_detail": detail,
        }

    store = store or default_store(root)
    prior = _latest(_load_jsonl(store), "commit").get(landed_sha)
    if prior is not None and prior.get("state") == "SOFT_GREEN":
        return {**prior, "launched": False, "reason": "exact soft-main launch already recorded"}

    workspace = root / "ignored/ci-hub/main-soft-green" / landed_sha
    source = _prepare_source(root, landed_sha, workspace)
    unit = f"validate-soft-green-main-{landed_sha[:12]}"
    log = workspace / "validate.log"
    launch_agent = f"soft-green-main-{agent}"
    pid = spawn(
        root, sha=landed_sha, source=source, unit=unit,
        agent=launch_agent, log=log,
    )
    if not isinstance(pid, int) or pid <= 0:
        raise SoftMainError("direct validate launcher returned no positive pid")
    record = {
        "schema_version": 1,
        "state": "SOFT_GREEN",
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "commit": landed_sha,
        "candidate": candidate,
        "source_rev": source_rev,
        "base": base,
        "soft_green": provenance["soft_green"],
        "agent": agent,
        "unit": f"{unit}.service",
        "launcher_pid": pid,
        "workspace": str(workspace),
        "log": str(log),
    }
    _append(store, record)
    return {**record, "launched": True}


def _run_validate(args: argparse.Namespace) -> int:
    environment = os.environ.copy()
    environment[SKIP_WARNING_ENV] = "1"
    result = subprocess.run(
        [
            str(args.root / "ci-hub/ci-hub"), "validate-run",
            "--checkout", str(args.source), "--agent", args.agent,
            "--target", args.sha, "--unit", args.unit, "--", "full",
        ],
        cwd=args.root, env=environment,
    )
    return result.returncode


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    status = commands.add_parser(
        "status",
        help="show whether current main is still soft green",
        description="Show whether current main is still soft green.",
    )
    status.add_argument("--json", action="store_true")
    run = commands.add_parser(
        "_run",
        help="run the exact-main validation worker (internal)",
        description="Run the exact-main validation worker used by the launcher.",
    )
    run.add_argument("--root", type=Path, required=True)
    run.add_argument("--sha", required=True)
    run.add_argument("--source", type=Path, required=True)
    run.add_argument("--unit", required=True)
    run.add_argument("--agent", required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "_run":
        return _run_validate(args)
    state = current_main_state()
    if args.json:
        print(json.dumps(state, sort_keys=True))
    else:
        warning = warning_text(state)
        if warning:
            print(warning, file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SoftMainError as error:
        print(f"main-soft-green: REFUSED: {error}", file=sys.stderr)
        raise SystemExit(2)
