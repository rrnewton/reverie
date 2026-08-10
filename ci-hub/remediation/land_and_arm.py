#!/usr/bin/env python3
"""Run one land command with write-ahead recovery for exact-SHA verification."""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(ROOT / "ci-hub/history"))

import obligations
import protocol

DEFAULT_COMMAND_TIMEOUT = 20 * 60
DEFAULT_OBSERVE_TIMEOUT = 60
CANNOT_START = 3


class LandError(RuntimeError):
    """The crash-recoverable land-and-arm protocol could not complete."""


def default_intent_dir() -> Path:
    override = os.environ.get("CI_HUB_LAND_INTENT_DIR")
    return (
        Path(override).expanduser()
        if override
        else ROOT / "ignored/ci-hub/land-intents"
    )


def _atomic_json(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def _intent_path(intent_dir: Path, repo: str, pr: int) -> Path:
    return intent_dir / f"{repo.replace('/', '-')}-pr{pr}.json"


def _run_land_command(command: Sequence[str], timeout: int) -> int:
    process = subprocess.Popen(list(command), start_new_session=True)
    try:
        return process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        raise LandError(
            f"land command exceeded its {timeout}s wall-time bound"
        ) from None


def _pr_state(repo: str, pr: int) -> tuple[str, str | None]:
    # This is the FINAL merge/close observation, so one REST-core request is
    # warranted. ``gh pr view`` uses GraphQL and could not start the watcher
    # when four drainers exhausted that separate quota (0/5000 remaining).
    result = protocol._run(
        (
            "with-proxy",
            "gh",
            "api",
            f"repos/{repo}/pulls/{pr}",
        ),
        check=True,
        timeout=protocol.DEFAULT_NETWORK_TIMEOUT,
    )
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise LandError("GitHub PR REST response was invalid JSON") from error
    if not isinstance(payload, Mapping):
        raise LandError("GitHub PR REST response was a non-object")
    merged = payload.get("merged") is True
    oid = payload.get("merge_commit_sha")
    state = "MERGED" if merged else str(payload.get("state") or "").upper()
    return state, str(oid).lower() if merged and oid else None


def _is_rate_limit_error(error: BaseException) -> bool:
    text = str(error).lower()
    return "rate limit" in text or "rate_limit" in text or "http 429" in text


def _record_cannot_start(
    path: Path,
    intent: dict[str, Any],
    *,
    stage: str,
    error: BaseException,
) -> int:
    intent.update(
        state="cannot-start",
        cannot_start_at=obligations.utc_now(),
        cannot_start_reason="github-api-rate-limit-exhausted",
        cannot_start_stage=stage,
        error=str(error),
    )
    _atomic_json(path, intent)
    print("state=CANNOT_START")
    print("reason=github-api-rate-limit-exhausted")
    print(f"stage={stage}")
    print(
        "summary=CANNOT_START: GitHub API rate limit was exhausted before the "
        "post-land watcher could be armed; this is not PASS and the durable "
        f"intent remains recoverable at {path}"
    )
    return CANNOT_START


def observe_merged_sha(
    repo: str,
    pr: int,
    *,
    timeout: int,
    sleep: Any = time.sleep,
) -> str:
    deadline = time.monotonic() + timeout
    while True:
        state, sha = _pr_state(repo, pr)
        if state == "MERGED" and sha and obligations.SHA_RE.fullmatch(sha):
            return sha
        if time.monotonic() >= deadline:
            raise LandError(
                f"PR #{pr} did not expose a merged 40-hex SHA within {timeout}s "
                f"(last state={state or 'unknown'})"
            )
        sleep(min(2.0, max(0.0, deadline - time.monotonic())))


def _existing_obligation(repo: str, sha: str, store: Path) -> dict[str, Any] | None:
    return protocol.recoverable_obligation(repo, sha, store)


def _live_launch_owner(intent: Mapping[str, Any], sha: str) -> dict[str, Any] | None:
    """Return the same-SHA obligation while another live process is arming it."""
    record = _existing_obligation(
        str(intent["repo"]), sha, Path(str(intent["store"]))
    )
    if record is None:
        return None
    launch = record.get("launch")
    if not isinstance(launch, Mapping) or launch.get("state") != "launching":
        return None
    return record if protocol._pid_alive(launch.get("launcher_pid")) else None


def _record_arm_in_progress(
    path: Path, intent: dict[str, Any], record: Mapping[str, Any]
) -> None:
    launch = record.get("launch")
    launch = launch if isinstance(launch, Mapping) else {}
    intent.update(
        state="arm-in-progress",
        obligation_id=str(record["obligation_id"]),
        error=None,
    )
    _atomic_json(path, intent)
    print(
        f"LAND ARM IN PROGRESS: {intent['repo']}#{intent['pr']} "
        f"sha={intent['landed_sha']} obligation={record['obligation_id']} "
        f"owner_pid={launch.get('launcher_pid')}"
    )


def arm_sha(intent: Mapping[str, Any], sha: str) -> tuple[int, str | None]:
    repo = str(intent["repo"])
    persisted_policy = intent.get("verification_policy")
    if persisted_policy is None:
        # Compatibility for write-ahead intents created before policies were
        # recorded. The resulting obligation persists this derived binding in
        # its initial opened event.
        policy = protocol.verification_policy_for_repo(repo)
    elif isinstance(persisted_policy, Mapping):
        policy = protocol.validate_verification_policy(persisted_policy)
    else:
        raise LandError("land intent verification policy is not an object")
    if policy["repo"] != repo:
        raise LandError(
            f"land intent verification policy does not match repository {repo!r}"
        )
    raw_source = intent.get("source")
    source = protocol.resolve_repo_source(
        repo,
        Path(str(raw_source)) if raw_source is not None else None,
    )
    store = Path(str(intent["store"]))
    prior_records = {
        str(record["obligation_id"]): str(record.get("event_id") or "")
        for record in obligations.latest_records(store).values()
        if record.get("repo") == repo and record.get("landed_sha") == sha
    }
    arguments = [
        "arm",
        sha,
        "--repo",
        repo,
        "--verification-policy-json",
        json.dumps(policy, sort_keys=True, separators=(",", ":")),
        "--source",
        str(source),
        "--land-mode",
        str(intent["land_mode"]),
        "--actor",
        str(intent["actor"]),
        "--github-wait-seconds",
        str(intent["github_wait_seconds"]),
        "--poll-seconds",
        str(intent["poll_seconds"]),
        "--store",
        str(store),
    ]
    code = protocol.main(arguments)
    all_matching = [
        record
        for record in obligations.latest_records(store).values()
        if record.get("repo") == repo and record.get("landed_sha") == sha
    ]
    matching = [
        record
        for record in all_matching
        if str(record.get("obligation_id")) not in prior_records
        or str(record.get("event_id") or "")
        != prior_records[str(record.get("obligation_id"))]
    ] or all_matching
    record = (
        max(
            matching,
            key=lambda candidate: (
                str(candidate.get("updated_at") or ""),
                str(candidate.get("opened_at") or ""),
                str(candidate.get("obligation_id") or ""),
            ),
        )
        if matching
        else None
    )
    if record is None or not protocol.obligation_launch_durable(record):
        return code or 2, None
    return code, str(record["obligation_id"])


def _new_intent(args: argparse.Namespace, command: Sequence[str]) -> dict[str, Any]:
    policy = protocol.verification_policy_for_repo(args.repo)
    source = protocol.resolve_repo_source(args.repo, args.source)
    return {
        "schema_version": 1,
        "repo": args.repo,
        "verification_policy": policy,
        "pr": args.pr,
        "source": str(source),
        "land_mode": args.land_mode,
        "actor": args.actor,
        "store": str(args.store.expanduser().resolve()),
        "github_wait_seconds": args.github_wait_seconds,
        "poll_seconds": args.poll_seconds,
        "command": list(command),
        "state": "prepared",
        "prepared_at": obligations.utc_now(),
        "landed_sha": None,
        "obligation_id": None,
        "error": None,
    }


def run(args: argparse.Namespace) -> int:
    command = list(args.command)
    if command and command[0] == "--":
        command.pop(0)
    if not command:
        raise LandError("a bounded land command is required after --")
    path = _intent_path(args.intent_dir, args.repo, args.pr)
    intent = _new_intent(args, command)
    _atomic_json(path, intent)
    code = _run_land_command(command, args.command_timeout)
    if code != 0:
        try:
            state, observed_sha = _pr_state(args.repo, args.pr)
        except (LandError, protocol.ProtocolError) as error:
            if _is_rate_limit_error(error):
                return _record_cannot_start(
                    path, intent, stage="post-land-state-observation", error=error
                )
            # Leave the durable intent recoverable. A wrapper/process failure
            # must never be allowed to prove that the merge did not happen.
            intent.update(
                state="land-result-unknown",
                error=f"land command exited {code}; merged state unavailable",
            )
            _atomic_json(path, intent)
            return code
        if state == "MERGED" and observed_sha is not None:
            sha = observed_sha
        else:
            intent.update(
                state="land-command-failed",
                failed_at=obligations.utc_now(),
                error=f"land command exited {code}",
            )
            _atomic_json(path, intent)
            return code
    else:
        try:
            sha = observe_merged_sha(args.repo, args.pr, timeout=args.observe_timeout)
        except (LandError, protocol.ProtocolError) as error:
            if _is_rate_limit_error(error):
                return _record_cannot_start(
                    path, intent, stage="post-land-state-observation", error=error
                )
            raise
    intent.update(
        state="merged-unarmed", landed_sha=sha, merged_at=obligations.utc_now()
    )
    _atomic_json(path, intent)
    code, obligation_id = arm_sha(intent, sha)
    if obligation_id is None:
        intent.update(
            state="arm-failed",
            error=f"arm-land exited {code} without creating an obligation",
        )
        _atomic_json(path, intent)
        return code or 2
    intent.update(
        state="armed",
        obligation_id=obligation_id,
        armed_at=obligations.utc_now(),
        error=(
            None if code == 0 else f"arm-land exited {code}; obligation owns recovery"
        ),
    )
    _atomic_json(path, intent)
    print(f"LAND ARMED: {args.repo}#{args.pr} sha={sha} obligation={obligation_id}")
    return 0


def prepare(args: argparse.Namespace) -> int:
    """Persist the pre-merge half for a self-wrapped shared lander."""
    path = _intent_path(args.intent_dir, args.repo, args.pr)
    intent = _new_intent(args, ["external-bounded-lander"])
    _atomic_json(path, intent)
    print(f"LAND INTENT PREPARED: {args.repo}#{args.pr} path={path}")
    return 0


def complete(args: argparse.Namespace) -> int:
    """Observe the merged SHA and complete a previously prepared arm."""
    path = _intent_path(args.intent_dir, args.repo, args.pr)
    try:
        intent = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise LandError(f"cannot read prepared land intent {path}: {error}") from error
    if not isinstance(intent, dict) or intent.get("state") == "land-command-failed":
        raise LandError(f"land intent is not completable: {path}")
    try:
        sha = observe_merged_sha(args.repo, args.pr, timeout=args.observe_timeout)
    except (LandError, protocol.ProtocolError) as error:
        if _is_rate_limit_error(error):
            return _record_cannot_start(
                path, intent, stage="prepared-land-completion", error=error
            )
        raise
    intent.update(
        state="merged-unarmed", landed_sha=sha, merged_at=obligations.utc_now()
    )
    _atomic_json(path, intent)
    code, obligation_id = arm_sha(intent, sha)
    if obligation_id is None:
        intent.update(state="arm-failed", error=f"complete arm exited {code}")
        _atomic_json(path, intent)
        return code or 2
    intent.update(
        state="armed",
        obligation_id=obligation_id,
        armed_at=obligations.utc_now(),
        error=None if code == 0 else f"complete arm exited {code}",
    )
    _atomic_json(path, intent)
    print(f"LAND ARMED: {args.repo}#{args.pr} sha={sha} obligation={obligation_id}")
    return 0


def recover_intent(path: Path, *, observe_timeout: int) -> int:
    try:
        intent = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise LandError(f"cannot read recovery intent {path}: {error}") from error
    if not isinstance(intent, dict) or intent.get("schema_version") != 1:
        raise LandError(f"unsupported recovery intent: {path}")
    if intent.get("state") == "land-command-failed":
        return 0
    if intent.get("state") == "armed":
        obligation_id = intent.get("obligation_id")
        if isinstance(obligation_id, str):
            try:
                record = obligations.get_record(
                    obligation_id, Path(str(intent["store"]))
                )
            except obligations.StoreError:
                record = None
            if record is not None and protocol.obligation_launch_durable(record):
                return 0
        # "armed" is a cache, not authority.  A missing/incomplete obligation
        # falls back into the same idempotent resume path as merged-unarmed.
        intent.update(
            state="merged-unarmed",
            error="cached armed state lacked durable verifier/watcher evidence",
        )
        _atomic_json(path, intent)
    sha = intent.get("landed_sha")
    if not isinstance(sha, str) or not obligations.SHA_RE.fullmatch(sha):
        try:
            state, observed = _pr_state(str(intent["repo"]), int(intent["pr"]))
        except (LandError, protocol.ProtocolError) as error:
            if _is_rate_limit_error(error):
                return _record_cannot_start(
                    path, intent, stage="recovery-state-observation", error=error
                )
            raise
        if state != "MERGED" or observed is None:
            if state == "CLOSED":
                intent.update(
                    state="land-command-failed",
                    failed_at=obligations.utc_now(),
                    error="PR is closed without a merge; no post-land watcher is owed",
                )
                _atomic_json(path, intent)
            return 0
        sha = observed
        intent.update(
            state="merged-unarmed",
            landed_sha=sha,
            merged_at=obligations.utc_now(),
        )
        _atomic_json(path, intent)
    # Recovery runs periodically and can overlap the original completer between
    # its durable launch claim and final armed transition. That owner is doing
    # exactly the work recovery exists to preserve. Treat the overlap as
    # in-progress rather than invoking a duplicate arm, whose LaunchBusy exit 2
    # would otherwise overwrite the intent with a false arm-failed state and
    # prevent the watcher poll after `recover && watch-obligations` from running.
    live_owner = _live_launch_owner(intent, sha)
    if live_owner is not None:
        _record_arm_in_progress(path, intent, live_owner)
        return 0
    code, obligation_id = arm_sha(intent, sha)
    if obligation_id is None:
        # Close the check-to-claim race: a completer may claim the obligation
        # after the probe above but before arm_sha tries to claim it.
        live_owner = _live_launch_owner(intent, sha)
        if live_owner is not None:
            _record_arm_in_progress(path, intent, live_owner)
            return 0
        intent.update(state="arm-failed", error=f"recovery arm exited {code}")
        _atomic_json(path, intent)
        return code or 2
    intent.update(
        state="armed",
        obligation_id=obligation_id,
        armed_at=obligations.utc_now(),
        error=None if code == 0 else f"recovery arm exited {code}",
    )
    _atomic_json(path, intent)
    print(
        f"RECOVERED LAND ARM: {intent['repo']}#{intent['pr']} "
        f"sha={sha} obligation={obligation_id}"
    )
    # An obligation may already be in remediation_required because GitHub
    # verification could not be confirmed. Recovery succeeded once ownership
    # moved into that durable obligation; its watcher handles the nonzero state.
    return 0


def recover(args: argparse.Namespace) -> int:
    if not args.intent_dir.exists():
        return 0
    result = 0
    for path in sorted(args.intent_dir.glob("*.json")):
        result = max(result, recover_intent(path, observe_timeout=args.observe_timeout))
    return result


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="operation", required=True)

    run_parser = subparsers.add_parser("run", help="run a land and arm its merged SHA")
    run_parser.add_argument("--repo", default=protocol.DEFAULT_REPO)
    run_parser.add_argument("--pr", type=int, required=True)
    run_parser.add_argument(
        "--source",
        type=Path,
        default=None,
        help="checkout whose origin matches --repo (defaults by supported repo)",
    )
    run_parser.add_argument(
        "--land-mode", choices=("admin", "speculative"), required=True
    )
    run_parser.add_argument(
        "--actor", default=os.environ.get("AGENT", os.environ.get("USER", "unknown"))
    )
    run_parser.add_argument(
        "--command-timeout", type=int, default=DEFAULT_COMMAND_TIMEOUT
    )
    run_parser.add_argument(
        "--observe-timeout", type=int, default=DEFAULT_OBSERVE_TIMEOUT
    )
    run_parser.add_argument(
        "--github-wait-seconds", type=int, default=protocol.DEFAULT_GITHUB_WAIT_SECONDS
    )
    run_parser.add_argument(
        "--poll-seconds", type=int, default=protocol.DEFAULT_POLL_SECONDS
    )
    run_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )
    run_parser.add_argument("--intent-dir", type=Path, default=default_intent_dir())
    run_parser.add_argument("command", nargs=argparse.REMAINDER)

    prepare_parser = subparsers.add_parser(
        "prepare", help="persist intent before a self-wrapped lander can merge"
    )
    prepare_parser.add_argument("--repo", default=protocol.DEFAULT_REPO)
    prepare_parser.add_argument("--pr", type=int, required=True)
    prepare_parser.add_argument(
        "--source",
        type=Path,
        default=None,
        help="checkout whose origin matches --repo (defaults by supported repo)",
    )
    prepare_parser.add_argument(
        "--land-mode", choices=("admin", "speculative"), required=True
    )
    prepare_parser.add_argument(
        "--actor", default=os.environ.get("AGENT", os.environ.get("USER", "unknown"))
    )
    prepare_parser.add_argument(
        "--github-wait-seconds", type=int, default=protocol.DEFAULT_GITHUB_WAIT_SECONDS
    )
    prepare_parser.add_argument(
        "--poll-seconds", type=int, default=protocol.DEFAULT_POLL_SECONDS
    )
    prepare_parser.add_argument(
        "--store", type=Path, default=obligations.default_store_path()
    )
    prepare_parser.add_argument("--intent-dir", type=Path, default=default_intent_dir())
    # Kept in the shared intent schema; the external lander owns the real bound.
    prepare_parser.set_defaults(
        command_timeout=DEFAULT_COMMAND_TIMEOUT,
        observe_timeout=DEFAULT_OBSERVE_TIMEOUT,
    )

    complete_parser = subparsers.add_parser(
        "complete", help="arm the merge recorded by a prepared intent"
    )
    complete_parser.add_argument("--repo", default=protocol.DEFAULT_REPO)
    complete_parser.add_argument("--pr", type=int, required=True)
    complete_parser.add_argument(
        "--intent-dir", type=Path, default=default_intent_dir()
    )
    complete_parser.add_argument(
        "--observe-timeout", type=int, default=DEFAULT_OBSERVE_TIMEOUT
    )

    recover_parser = subparsers.add_parser(
        "recover", help="arm any merged land whose wrapper died before arming"
    )
    recover_parser.add_argument("--intent-dir", type=Path, default=default_intent_dir())
    recover_parser.add_argument(
        "--observe-timeout", type=int, default=DEFAULT_OBSERVE_TIMEOUT
    )

    args = parser.parse_args(argv)
    for name in (
        "command_timeout",
        "observe_timeout",
        "github_wait_seconds",
        "poll_seconds",
    ):
        if hasattr(args, name) and getattr(args, name) <= 0:
            parser.error(f"--{name.replace('_', '-')} must be positive")
    return args


def _run_costed(argv: Sequence[str]) -> int:
    command = [
        str(ROOT / "ci-hub/bin/tool-cost"),
        "--tool",
        "ci-hub/land-and-arm",
        "--estimate-unknown",
        "--basis",
        "not measured: bounded merge command plus exact-SHA dual-verifier arming",
        "--",
        sys.executable,
        str(Path(__file__).resolve()),
        *argv,
    ]
    environment = os.environ.copy()
    environment["CI_HUB_TOOL_COST_ACTIVE"] = "1"
    return subprocess.run(command, env=environment, check=False).returncode


def main(argv: Sequence[str] | None = None) -> int:
    raw = list(sys.argv[1:] if argv is None else argv)
    args = parse_args(raw)
    if os.environ.get("CI_HUB_DOCS_PARSE_ONLY") is not None:
        print(f"DOCS PARSE OK: land_and_arm.py {' '.join(raw)}")
        return 0
    if (
        args.operation in {"run", "complete"}
        and os.environ.get("CI_HUB_TOOL_COST_ACTIVE") is None
    ):
        return _run_costed(raw)
    try:
        if args.operation == "run":
            return run(args)
        if args.operation == "prepare":
            return prepare(args)
        if args.operation == "complete":
            return complete(args)
        return recover(args)
    except (LandError, protocol.ProtocolError, obligations.StoreError) as error:
        print(f"ci-hub land-and-arm: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
