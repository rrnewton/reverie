#!/usr/bin/env python3
"""Prepare, seal, publish, and activate one isolated ci-hub change."""

from __future__ import annotations

import argparse
import copy
import fcntl
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence, TextIO


SCHEMA = "ci-hub-change/v1"
MANIFEST = "ci-hub-change-manifest.json"
SEAL = "ci-hub-change-seal.json"
TASK_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
ACTIVATION_SCHEMA = "ci-hub-change-activation-obligation/v1"
ACTIVATION_STORE = "activation-obligations.jsonl"
ACTIVATION_CLOSED_STATES = frozenset(("satisfied", "cancelled"))


class Refusal(RuntimeError):
    pass


def run(
    argv: Sequence[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        list(argv), cwd=cwd, text=True, capture_output=capture, check=False
    )
    if check and result.returncode != 0:
        detail = (
            (result.stderr or "").strip()
            or (result.stdout or "").strip()
            or f"exit {result.returncode}"
        )
        raise Refusal(f"{shlex.join(argv)}: {detail}")
    return result


def git(repo: Path, *args: str, check: bool = True) -> str:
    return run(("git", "-C", str(repo), *args), check=check).stdout.strip()


def emit_completed(result: subprocess.CompletedProcess[str]) -> None:
    if result.stdout:
        print(result.stdout, end="")
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)


def network_git(repo: Path | None, *args: str) -> str:
    command: list[str] = ["git"]
    if repo is not None:
        command.extend(("-C", str(repo)))
    command.extend(args)
    if shutil.which("with-proxy"):
        command.insert(0, "with-proxy")
    return run(command).stdout.strip()


def repository_root(start: Path | None = None) -> Path:
    here = (start or Path.cwd()).resolve()
    result = run(("git", "-C", str(here), "rev-parse", "--show-toplevel"))
    return Path(result.stdout.strip()).resolve()


def git_dir(repo: Path) -> Path:
    raw = Path(git(repo, "rev-parse", "--git-dir"))
    return (repo / raw).resolve() if not raw.is_absolute() else raw.resolve()


def metadata_path(repo: Path, name: str) -> Path:
    return git_dir(repo) / name


def write_json(path: Path, value: dict[str, Any]) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, path)


def read_json(path: Path, role: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise Refusal(f"cannot read {role} {path}: {error}") from error
    if not isinstance(value, dict) or value.get("schema") != SCHEMA:
        raise Refusal(f"{role} {path} has unsupported schema")
    return value


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace(
        "+00:00", "Z"
    )


def activation_store_path(primary: Path, override: Path | None = None) -> Path:
    if override is not None:
        return override.expanduser().resolve()
    return primary / "ignored" / "ci-hub" / ACTIVATION_STORE


def _activation_latest_from_handle(handle: TextIO) -> dict[str, dict[str, Any]]:
    handle.seek(0)
    latest: dict[str, dict[str, Any]] = {}
    for line_number, line in enumerate(handle, 1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise Refusal(
                f"activation obligation store line {line_number} is invalid JSON: {error}"
            ) from error
        if not isinstance(event, dict) or event.get("schema") != ACTIVATION_SCHEMA:
            raise Refusal(
                f"activation obligation store line {line_number} has unsupported schema"
            )
        obligation_id = event.get("obligation_id")
        if not isinstance(obligation_id, str) or not obligation_id:
            raise Refusal(
                f"activation obligation store line {line_number} has no obligation_id"
            )
        latest[obligation_id] = event
    return latest


def activation_latest(store: Path) -> dict[str, dict[str, Any]]:
    if not store.exists():
        return {}
    with store.open("r", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_SH)
        try:
            return _activation_latest_from_handle(handle)
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _append_activation_event(handle: TextIO, record: dict[str, Any]) -> None:
    handle.seek(0, os.SEEK_END)
    handle.write(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n")
    handle.flush()
    os.fsync(handle.fileno())


def _merge_mapping(target: dict[str, Any], patch: Mapping[str, Any]) -> None:
    for key, value in patch.items():
        if isinstance(value, Mapping) and isinstance(target.get(key), dict):
            _merge_mapping(target[key], value)
        else:
            target[key] = copy.deepcopy(value)


def create_activation_obligation(
    *,
    store: Path,
    task: str,
    candidate: str,
    primary: Path,
    changed_paths: Sequence[str],
    diff_sha256: str,
) -> dict[str, Any]:
    store.parent.mkdir(parents=True, exist_ok=True)
    opened_at = utc_now()
    with store.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            latest = _activation_latest_from_handle(handle)
            for existing in latest.values():
                if (
                    existing.get("candidate") == candidate
                    and existing.get("primary") == str(primary)
                    and existing.get("overall_state")
                    not in ACTIVATION_CLOSED_STATES
                ):
                    expected_identity = {
                        "task": task,
                        "changed_paths": sorted(changed_paths),
                        "diff_sha256": diff_sha256,
                    }
                    if any(
                        existing.get(key) != value
                        for key, value in expected_identity.items()
                    ):
                        raise Refusal(
                            "open activation obligation for this candidate has "
                            "different immutable identity"
                        )
                    return existing
            obligation_id = (
                f"{opened_at.replace('-', '').replace(':', '').replace('T', '-')[:-1]}-"
                f"{candidate[:12]}-{uuid.uuid4().hex[:6]}"
            )
            record = {
                "schema": ACTIVATION_SCHEMA,
                "event_id": uuid.uuid4().hex,
                "event_type": "opened",
                "recorded_at": opened_at,
                "obligation_id": obligation_id,
                "task": task,
                "candidate": candidate,
                "primary": str(primary),
                "changed_paths": sorted(changed_paths),
                "diff_sha256": diff_sha256,
                "opened_at": opened_at,
                "updated_at": opened_at,
                "overall_state": "open",
                "satisfied_at": None,
                "activation": {
                    "state": "awaiting_publication",
                    "attempts": 0,
                    "last_attempt_at": None,
                    "last_reason": None,
                    "activated_head": None,
                },
            }
            _append_activation_event(handle, record)
            return record
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def transition_activation_obligation(
    store: Path,
    obligation_id: str,
    event_type: str,
    patch: Mapping[str, Any],
) -> dict[str, Any]:
    if not re.fullmatch(r"[a-z][a-z0-9_-]*", event_type):
        raise Refusal(f"invalid activation obligation event type {event_type!r}")
    store.parent.mkdir(parents=True, exist_ok=True)
    with store.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            latest = _activation_latest_from_handle(handle)
            try:
                previous = latest[obligation_id]
            except KeyError as error:
                raise Refusal(f"unknown activation obligation {obligation_id}") from error
            immutable = {
                "obligation_id",
                "task",
                "candidate",
                "primary",
                "changed_paths",
                "diff_sha256",
                "opened_at",
            }
            if immutable.intersection(patch):
                raise Refusal("activation transition cannot change immutable identity")
            record = copy.deepcopy(previous)
            _merge_mapping(record, patch)
            now = utc_now()
            record.update(
                {
                    "schema": ACTIVATION_SCHEMA,
                    "event_id": uuid.uuid4().hex,
                    "event_type": event_type,
                    "recorded_at": now,
                    "updated_at": now,
                }
            )
            _append_activation_event(handle, record)
            return record
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def activation_for_candidate(
    store: Path, candidate: str, primary: Path, *, include_closed: bool = False
) -> dict[str, Any] | None:
    matches = [
        record
        for record in activation_latest(store).values()
        if record.get("candidate") == candidate
        and record.get("primary") == str(primary)
        and (
            include_closed
            or record.get("overall_state") not in ACTIVATION_CLOSED_STATES
        )
    ]
    return max(matches, key=lambda item: str(item.get("opened_at", ""))) if matches else None


def normalize_claim(raw: str) -> str:
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or raw.endswith("/"):
        raise Refusal(f"invalid claimed path {raw!r}")
    value = path.as_posix()
    if value != "ci-hub" and not value.startswith("ci-hub/"):
        raise Refusal(f"ci-hub change claims must stay below ci-hub/: {raw!r}")
    return value


def validate_task(task: str) -> str:
    if not TASK_RE.fullmatch(task):
        raise Refusal("--task must be a lowercase hyphenated slug")
    return task


def is_clean(repo: Path) -> bool:
    return not git(repo, "status", "--porcelain=v1", "--untracked-files=all")


def fetch_main(repo: Path) -> str:
    network_git(repo, "fetch", "origin", "main")
    return git(repo, "rev-parse", "refs/remotes/origin/main^{commit}")


def changed_paths(repo: Path, base: str, candidate: str) -> list[str]:
    output = git(repo, "diff", "--name-only", f"{base}..{candidate}")
    return [line for line in output.splitlines() if line]


def claim_covers(claim: str, changed: str) -> bool:
    return changed == claim or changed.startswith(f"{claim}/")


def diff_digest(repo: Path, base: str, candidate: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), "diff", "--binary", f"{base}..{candidate}"],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        raise Refusal("cannot render candidate binary diff")
    return hashlib.sha256(result.stdout).hexdigest()


def initialize_manifest(repo: Path, task: str, claims: list[str], base: str) -> None:
    write_json(
        metadata_path(repo, MANIFEST),
        {
            "schema": SCHEMA,
            "task": validate_task(task),
            "clone": str(repo),
            "created_at": datetime.now(timezone.utc).isoformat(),
            "initial_base": base,
            "claims": sorted(set(claims)),
        },
    )


def command_create(args: argparse.Namespace) -> None:
    destination = Path(args.clone).expanduser().resolve()
    if destination.exists() or destination.is_symlink():
        raise Refusal(
            f"clone destination already exists: {destination}; a coordinator must "
            "disposition the per-change clone before creating another"
        )
    claims = [normalize_claim(item) for item in args.path]
    if not claims:
        raise Refusal("create requires at least one --path claim")
    destination.parent.mkdir(parents=True, exist_ok=True)
    network_git(
        None,
        "clone",
        "--no-recurse-submodules",
        "--single-branch",
        "--branch",
        "main",
        args.remote,
        str(destination),
    )
    base = git(destination, "rev-parse", "HEAD^{commit}")
    remote = git(destination, "rev-parse", "refs/remotes/origin/main^{commit}")
    if base != remote or not is_clean(destination):
        raise Refusal("fresh clone is not clean at exact origin/main")
    setup = destination / "scripts/setup-hooks.sh"
    if setup.is_file():
        run((str(setup),), cwd=destination, capture=False)
    branch = f"ci-hub/{validate_task(args.task)}"
    run(("git", "-C", str(destination), "switch", "-c", branch), capture=False)
    initialize_manifest(destination, args.task, claims, base)
    print(f"CI_HUB_CHANGE created={destination} task={args.task} base={base} branch={branch}")


def load_manifest(repo: Path) -> dict[str, Any]:
    value = read_json(metadata_path(repo, MANIFEST), "change manifest")
    if Path(str(value.get("clone", ""))).resolve() != repo:
        raise Refusal("manifest clone path does not match this checkout")
    claims = value.get("claims")
    if not isinstance(claims, list) or not claims or not all(isinstance(x, str) for x in claims):
        raise Refusal("manifest carries no usable path claims")
    return value


def verify_candidate(repo: Path) -> tuple[dict[str, Any], str, str, list[str], str]:
    manifest = load_manifest(repo)
    if not is_clean(repo):
        raise Refusal("candidate clone is dirty; commit the complete change before sealing")
    candidate = git(repo, "rev-parse", "HEAD^{commit}")
    base = fetch_main(repo)
    parents = git(repo, "rev-list", "--parents", "-n", "1", candidate).split()
    if len(parents) != 2 or parents[1] != base:
        raise Refusal(
            f"candidate must be exactly one commit on current origin/main; candidate={candidate} "
            f"parent={parents[1] if len(parents) > 1 else 'none'} origin/main={base}"
        )
    paths = changed_paths(repo, base, candidate)
    if not paths:
        raise Refusal("candidate commit changes zero paths")
    claims = [str(item) for item in manifest["claims"]]
    outside = [path for path in paths if not any(claim_covers(claim, path) for claim in claims)]
    if outside:
        raise Refusal("candidate changes unclaimed paths: " + ", ".join(outside))
    return manifest, base, candidate, paths, diff_digest(repo, base, candidate)


def command_seal(args: argparse.Namespace) -> None:
    repo = repository_root(Path(args.clone).expanduser() if args.clone else None)
    manifest, base, candidate, paths, digest = verify_candidate(repo)
    if not args.test:
        raise Refusal("seal requires at least one --test command")
    tests: list[dict[str, Any]] = []
    for command in args.test:
        result = run(("bash", "-lc", command), cwd=repo, check=False, capture=False)
        tests.append({"command": command, "exit_code": result.returncode})
        if result.returncode != 0:
            raise Refusal(f"test failed with exit {result.returncode}: {command}")
    write_json(
        metadata_path(repo, SEAL),
        {
            "schema": SCHEMA,
            "task": manifest["task"],
            "base": base,
            "candidate": candidate,
            "changed_paths": paths,
            "diff_sha256": digest,
            "tests": tests,
            "sealed_at": datetime.now(timezone.utc).isoformat(),
        },
    )
    print(
        f"CI_HUB_CHANGE sealed=1 task={manifest['task']} base={base} "
        f"candidate={candidate} paths={len(paths)} tests={len(tests)} diff_sha256={digest}"
    )


def verified_seal(
    repo: Path, *, require_unpublished_base: bool
) -> tuple[dict[str, Any], dict[str, Any]]:
    seal = read_json(metadata_path(repo, SEAL), "change seal")
    manifest = load_manifest(repo)
    if not is_clean(repo):
        raise Refusal("candidate clone changed after sealing")
    base = str(seal.get("base", ""))
    candidate = git(repo, "rev-parse", "HEAD^{commit}")
    parents = git(repo, "rev-list", "--parents", "-n", "1", candidate).split()
    if len(parents) != 2 or parents[1] != base:
        raise Refusal("sealed candidate no longer has its recorded single parent")
    paths = changed_paths(repo, base, candidate)
    digest = diff_digest(repo, base, candidate)
    claims = [str(item) for item in manifest["claims"]]
    outside = [path for path in paths if not any(claim_covers(claim, path) for claim in claims)]
    if outside:
        raise Refusal("sealed candidate now changes unclaimed paths: " + ", ".join(outside))
    expected = {
        "task": manifest["task"],
        "base": base,
        "candidate": candidate,
        "changed_paths": paths,
        "diff_sha256": digest,
    }
    for key, value in expected.items():
        if seal.get(key) != value:
            raise Refusal(f"seal mismatch for {key}: sealed={seal.get(key)!r} current={value!r}")
    tests = seal.get("tests")
    if not isinstance(tests, list) or not tests:
        raise Refusal("seal has no test record")
    for item in tests:
        if not isinstance(item, dict) or item.get("exit_code") != 0:
            raise Refusal("seal contains a missing or unsuccessful test")
    if require_unpublished_base:
        current = fetch_main(repo)
        if current != base:
            raise Refusal(
                f"sealed base is stale; sealed={base} current origin/main={current}"
            )
    return manifest, seal


def command_publish(args: argparse.Namespace) -> None:
    repo = repository_root(Path(args.clone).expanduser() if args.clone else None)
    primary = repository_root(Path(args.primary).expanduser())
    manifest, seal = verified_seal(repo, require_unpublished_base=True)
    store = activation_store_path(primary, args.store)
    obligation = create_activation_obligation(
        store=store,
        task=str(manifest["task"]),
        candidate=str(seal["candidate"]),
        primary=primary,
        changed_paths=[str(path) for path in seal["changed_paths"]],
        diff_sha256=str(seal["diff_sha256"]),
    )
    try:
        published_result = run(
            (str(repo / "scripts/parent-main-write"), "publish", "HEAD"),
            cwd=repo,
        )
        emit_completed(published_result)
    except Refusal as error:
        # A publisher can push successfully and then fail while fetching or
        # verifying the result.  Its nonzero exit proves only that the complete
        # publication operation did not finish; it does not prove that the
        # candidate is absent from main.  Resolve that state from the remote
        # graph.  If even that read is unavailable, retain the obligation open:
        # unknown publication is precisely when evidence must not disappear.
        try:
            observed = fetch_main(repo)
        except Refusal as observation_error:
            transition_activation_obligation(
                store,
                str(obligation["obligation_id"]),
                "publication_unresolved",
                {
                    "activation": {
                        "state": "awaiting_publication",
                        "last_reason": (
                            f"publisher refused: {error}; remote verification "
                            f"unavailable: {observation_error}"
                        ),
                    },
                },
            )
        else:
            contained = is_ancestor(repo, str(seal["candidate"]), observed)
            transition_activation_obligation(
                store,
                str(obligation["obligation_id"]),
                (
                    "publication_confirmed_after_refusal"
                    if contained
                    else "publication_refused"
                ),
                {
                    "overall_state": "open" if contained else "cancelled",
                    "activation": {
                        "state": "pending" if contained else "publication_refused",
                        "last_reason": (
                            f"publisher refused: {error}; observed origin/main "
                            f"{observed} {'contains' if contained else 'does not contain'} "
                            f"candidate {seal['candidate']}"
                        ),
                    },
                },
            )
        raise
    published = fetch_main(repo)
    if published != seal["candidate"]:
        contained = is_ancestor(repo, str(seal["candidate"]), published)
        transition_activation_obligation(
            store,
            str(obligation["obligation_id"]),
            "publication_moved",
            {
                "overall_state": "open" if contained else "cancelled",
                "activation": {
                    "state": "pending" if contained else "publication_refused",
                    "last_reason": (
                        f"remote moved after publication: expected {seal['candidate']}, "
                        f"observed {published}"
                    ),
                },
            },
        )
        raise Refusal(
            "remote main moved after publication: "
            f"expected {seal['candidate']}, observed {published}"
        )
    obligation = transition_activation_obligation(
        store,
        str(obligation["obligation_id"]),
        "publication_confirmed",
        {
            "activation": {
                "state": "pending",
                "last_reason": "published candidate awaits activation in primary",
            }
        },
    )
    print(f"CI_HUB_CHANGE published=1 task={manifest['task']} candidate={published}")
    print(
        f"CI_HUB_CHANGE activation_obligation={obligation['obligation_id']} "
        f"state={obligation['overall_state']} store={store}"
    )


def dirty_candidate_paths(primary: Path, paths: list[str]) -> list[str]:
    result = git(primary, "status", "--porcelain=v1", "--untracked-files=all", "--", *paths)
    return [line for line in result.splitlines() if line]


def is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    result = run(
        ("git", "-C", str(repo), "merge-base", "--is-ancestor", ancestor, descendant),
        check=False,
    )
    return result.returncode == 0


def activation_completion(record: Mapping[str, Any]) -> tuple[bool, str, str | None]:
    primary = Path(str(record.get("primary", "")))
    candidate = str(record.get("candidate", ""))
    paths = record.get("changed_paths")
    if not primary.is_dir() or not (primary / ".git").exists():
        return False, f"primary checkout is unavailable: {primary}", None
    if not isinstance(paths, list) or not paths or not all(
        isinstance(path, str) for path in paths
    ):
        return False, "obligation has no usable changed-path identity", None
    branch = git(primary, "branch", "--show-current")
    if branch != "main":
        return False, f"primary checkout is on {branch or 'detached HEAD'}, not main", None
    head = git(primary, "rev-parse", "HEAD^{commit}")
    if not is_ancestor(primary, candidate, head):
        return False, f"primary HEAD {head} does not contain candidate {candidate}", head
    dirty = dirty_candidate_paths(primary, list(paths))
    if dirty:
        return (
            False,
            "candidate paths differ from primary HEAD: " + " | ".join(dirty),
            head,
        )
    return True, f"primary HEAD {head} contains candidate and candidate paths are clean", head


def reconcile_activation_obligations(
    store: Path, *, primary_filter: Path | None = None
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    latest = activation_latest(store)
    open_records = sorted(
        (
            record
            for record in latest.values()
            if record.get("overall_state") not in ACTIVATION_CLOSED_STATES
            and (
                primary_filter is None
                or record.get("primary") == str(primary_filter)
            )
        ),
        key=lambda item: (str(item.get("opened_at", "")), str(item["obligation_id"])),
    )
    satisfied: list[dict[str, Any]] = []
    remaining: list[dict[str, Any]] = []
    for record in open_records:
        complete, reason, head = activation_completion(record)
        if complete:
            now = utc_now()
            record = transition_activation_obligation(
                store,
                str(record["obligation_id"]),
                "activation_satisfied",
                {
                    "overall_state": "satisfied",
                    "satisfied_at": now,
                    "activation": {
                        "state": "satisfied",
                        "last_reason": reason,
                        "activated_head": head,
                    },
                },
            )
            satisfied.append(record)
        else:
            observed = copy.deepcopy(record)
            observed["_observation_reason"] = reason
            observed["_observation_head"] = head
            remaining.append(observed)
    return satisfied, remaining


def render_activation_obligations(
    store: Path, *, primary_filter: Path | None = None
) -> tuple[int, str]:
    satisfied, remaining = reconcile_activation_obligations(
        store, primary_filter=primary_filter
    )
    lines = [
        f"open={len(remaining)}",
        f"newly_satisfied={len(satisfied)}",
        f"evaluated={len(remaining) + len(satisfied)}",
        f"store={store}",
    ]
    for record in remaining:
        reason = str(record["_observation_reason"])
        head = record["_observation_head"]
        lines.append(
            "  OPEN "
            f"id={record['obligation_id']} task={record['task']} "
            f"candidate={record['candidate']} primary_head={head or 'unresolved'} "
            f"activation_state={(record.get('activation') or {}).get('state')} "
            f"reason={reason}"
        )
    for record in satisfied:
        lines.append(
            "  SATISFIED "
            f"id={record['obligation_id']} candidate={record['candidate']} "
            f"activated_head={(record.get('activation') or {}).get('activated_head')}"
        )
    if remaining:
        subjects = ", ".join(
            f"{record['task']}@{str(record['candidate'])[:12]}"
            for record in remaining[:3]
        )
        extra = f" (+{len(remaining) - 3} more)" if len(remaining) > 3 else ""
        lines.append(
            f"summary={len(remaining)} activation obligation(s) remain OPEN: "
            f"{subjects}{extra}"
        )
    else:
        lines.append("summary=no open activation obligations")
    return (1 if remaining else 0), "\n".join(lines) + "\n"


def command_activate(args: argparse.Namespace) -> None:
    clone = repository_root(Path(args.clone).expanduser())
    primary = repository_root(Path(args.primary).expanduser())
    manifest, seal = verified_seal(clone, require_unpublished_base=False)
    candidate = str(seal["candidate"])
    paths = [str(path) for path in seal["changed_paths"]]
    store = activation_store_path(primary, args.store)
    obligation = activation_for_candidate(store, candidate, primary)
    if obligation is None:
        prior = activation_for_candidate(
            store, candidate, primary, include_closed=True
        )
        if prior is not None and prior.get("overall_state") == "satisfied":
            complete, reason, head = activation_completion(prior)
            if not complete:
                raise Refusal(
                    "a satisfied activation obligation no longer matches the primary: "
                    + reason
                )
            print(
                f"CI_HUB_CHANGE activated=already task={manifest['task']} "
                f"candidate={candidate} activated_head={head} "
                f"activation_obligation={prior['obligation_id']}"
            )
            return
        obligation = create_activation_obligation(
            store=store,
            task=str(manifest["task"]),
            candidate=candidate,
            primary=primary,
            changed_paths=paths,
            diff_sha256=str(seal["diff_sha256"]),
        )
    attempts = int((obligation.get("activation") or {}).get("attempts") or 0) + 1
    attempted_at = utc_now()
    try:
        published = fetch_main(primary)
        if not is_ancestor(primary, candidate, published):
            raise Refusal(
                f"activation requires current main to contain candidate; candidate={candidate} "
                f"origin/main={published}"
            )
        dirty = dirty_candidate_paths(primary, paths)
        if dirty:
            raise Refusal(
                "primary candidate paths are dirty; refusing to sweep them into activation: "
                + " | ".join(dirty)
            )
        sync_result = run(
            (str(primary / "scripts/parent-main-write"), "sync", "--automatic"),
            cwd=primary,
        )
        emit_completed(sync_result)
        head = git(primary, "rev-parse", "HEAD^{commit}")
        if head != published:
            raise Refusal(f"primary activation ended at {head}, not fetched main {published}")
        if not is_ancestor(primary, candidate, head):
            raise Refusal(f"activated primary {head} does not contain candidate {candidate}")
        tree_check = run(
            ("git", "-C", str(primary), "diff", "--quiet", head, "--", *paths),
            check=False,
        )
        if tree_check.returncode != 0:
            raise Refusal("activated primary paths differ from the candidate tree")
    except Refusal as error:
        transition_activation_obligation(
            store,
            str(obligation["obligation_id"]),
            "activation_deferred",
            {
                "activation": {
                    "state": "deferred",
                    "attempts": attempts,
                    "last_attempt_at": attempted_at,
                    "last_reason": str(error),
                }
            },
        )
        raise
    obligation = transition_activation_obligation(
        store,
        str(obligation["obligation_id"]),
        "activation_satisfied",
        {
            "overall_state": "satisfied",
            "satisfied_at": utc_now(),
            "activation": {
                "state": "satisfied",
                "attempts": attempts,
                "last_attempt_at": attempted_at,
                "last_reason": "activation completed and verified",
                "activated_head": head,
            },
        },
    )
    print(
        f"CI_HUB_CHANGE activated=1 task={manifest['task']} candidate={candidate} "
        f"activated_head={head} paths={len(paths)} "
        f"activation_obligation={obligation['obligation_id']}"
    )
    print(
        "CI_HUB_CHANGE LIMIT: git working-tree update is serialized but not a "
        "filesystem-atomic multi-file swap; see ai_docs/ci-hub-change-clone-protocol.md"
    )


def command_status(args: argparse.Namespace) -> None:
    repo = repository_root(Path(args.clone).expanduser() if args.clone else None)
    report: dict[str, Any] = {
        "schema": SCHEMA,
        "manifest": load_manifest(repo),
        "head": git(repo, "rev-parse", "HEAD^{commit}"),
        "clean": is_clean(repo),
        "seal": None,
    }
    seal_path = metadata_path(repo, SEAL)
    if seal_path.is_file():
        report["seal"] = read_json(seal_path, "change seal")
    print(json.dumps(report, indent=2, sort_keys=True))


def unpushed_in_clone(repo: Path) -> list[str]:
    """Commits on this clone's OWN branches that no remote-tracking ref contains.

    The scope is `--branches` plus `HEAD`: every local branch, and also the
    detached HEAD an interrupted change leaves behind, which `--branches` alone
    would drop. Both halves are load-bearing and are bracketed separately.

    The scope is deliberately NOT `--all`. `--all` additionally sweeps
    fetch-destination ref namespaces, whose contents exist on a remote by
    construction: `agent-utils/py/pr_landing_planner/collect.py` fetches
    `refs/pull/<n>/head` into `refs/pr-landing-planner/pr-<n>` and a base
    `refs/heads/<ref>` into `refs/pr-landing-planner/base-<digest>`. Those
    commits are reachable from no *remote-tracking* ref only because they were
    filed outside `refs/remotes/`, so `--all` reported them as at risk when
    nothing was. Measured 2026-08-15 on two real clones:
    `dev-hermit-p1-a-producer-path` 38 under `--all` versus 1 here, and
    `dev-hermit-p1-validate-writes-receipts` 36 versus 0.

    Tags stay out of scope for the reason the parent's unpushed-commits health
    gate excludes them: a tag can point into a submodule's history and report
    commits that no push could ever retire.

    This answers against the clone's remote-tracking refs, which are only a
    snapshot from its last fetch, so `command_dispose` refreshes them first.
    """
    out = git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes")
    return [line for line in out.splitlines() if line]


SCOPED_REF_PREFIXES = ("refs/heads/", "refs/remotes/", "refs/tags/")


def unconsidered_unpushed(repo: Path) -> tuple[int, list[str]]:
    """Commits `unpushed_in_clone` does not consider, and the namespaces present.

    Narrowing the refusal must not make anything invisible, so disposal reports
    what it declined to weigh instead of silently dropping it. The namespaces
    are the out-of-scope ones this clone has, which is where such commits live;
    it is a place to look, not a per-commit attribution.

    The filtering is done here rather than with `for-each-ref --exclude`,
    because `--exclude=refs/heads/*` does not match a branch whose name itself
    contains a slash -- `refs/heads/ci-hub/p1-a-producer-path` survived it -- so
    that spelling reports local branches as out of scope when they are not.
    """
    out = git(
        repo, "rev-list", "--exclude=refs/tags/*", "--all",
        "--not", "--branches", "HEAD", "--remotes",
    )
    commits = [line for line in out.splitlines() if line]
    if not commits:
        return 0, []
    refs = git(repo, "for-each-ref", "--format=%(refname)").splitlines()
    spaces = sorted(
        {
            "/".join(ref.split("/")[:2]) + "/*"
            for ref in refs
            if ref and not ref.startswith(SCOPED_REF_PREFIXES)
        }
    )
    return len(commits), spaces


def summarize_refs(refs: Sequence[str], limit: int = 5) -> str:
    """Name where a disposed commit can be found again, readably.

    Recovery refs exist so a human can retrieve the commit, and fetching every
    head to answer containment can make that list enormous: measured
    2026-08-15, one clone's head sat on 130 refs, mostly `origin/rescue/auto-*`.
    A wall of names is not a recovery instruction, so lead with `main` where it
    qualifies and count the rest.
    """
    preferred = [ref for ref in refs if ref.rsplit("/", 1)[-1] == "main"]
    ordered = preferred + [ref for ref in refs if ref not in preferred]
    shown = ",".join(ordered[:limit])
    return f"{shown},+{len(ordered) - limit}-more" if len(ordered) > limit else shown


def refresh_remote_tracking(repo: Path) -> bool:
    """Bring `refs/remotes/origin/*` up to date with the remote; report success.

    A clone's remote-tracking refs record what the remote looked like when it
    last fetched, and disposal must not decide containment from that snapshot in
    EITHER direction. Stale refs under-report what is pushed: measured
    2026-08-15, clone `dev-hermit-p1-a-producer-path` carried exactly one
    remote-tracking ref, `origin/main` pinned at e8ba1762 (2026-08-12), while
    its own commit 80e50e9e was by then on the remote's `main` and on its own
    remote branch. They can equally over-report it, because a ref the snapshot
    still shows may since have been moved off that commit.

    Fetching every head rather than just `main` asks the accurate question: a
    clone's work may have been pushed to any branch, and guessing the wrong one
    produces exactly the false refusal this is fixing. The cost is that a
    refused clone keeps the remote-tracking refs and objects this brought in;
    they hold no local work and any later fetch would recreate them.
    """
    try:
        network_git(repo, "fetch", "--quiet", "origin", "+refs/heads/*:refs/remotes/origin/*")
    except Refusal:
        return False
    return True


def command_dispose(args: argparse.Namespace) -> None:
    """Retire one per-change clone, refusing whenever anything is at risk.

    THE REFUSAL IS THE POINT. `create` already tells a caller that "a
    coordinator must disposition the per-change clone before creating another",
    and until now no operation did that, so clones accumulated and the
    disposition decision rested on an agent noticing unpushed work by hand. One
    such clone held two commits that existed on no remote ref for two days.
    """
    destination = Path(args.clone).expanduser().resolve()
    if not destination.is_dir():
        raise Refusal(f"clone path does not exist: {destination}")
    # Check for the checkout before asking Git to resolve a root: pointed at a
    # plain directory, root resolution surfaces an unrelated tool's hint rather
    # than a refusal, and a refusal a caller cannot read is not a refusal.
    if not (destination / ".git").exists():
        raise Refusal(f"{destination} is not a Git checkout; dispose only retires change clones")
    repo = repository_root(destination)
    if repo != destination:
        raise Refusal(
            f"{destination} is not the root of a checkout (root is {repo}); "
            "dispose only ever removes a change clone it was pointed at exactly"
        )
    # Refuses unless a change manifest names this exact path, so dispose can
    # never be aimed at the primary or at an unrelated checkout.
    manifest = load_manifest(repo)
    if not is_clean(repo):
        dirty = git(repo, "status", "--porcelain")
        count = len([line for line in dirty.splitlines() if line])
        raise Refusal(
            f"clone {destination} has {count} uncommitted or untracked path(s); "
            "commit or remove them deliberately, never by disposal"
        )
    # Every containment answer below is only as good as this clone's picture of
    # the remote, so refresh it first and refuse outright when that is not
    # possible. Unverifiable is not the same as safe.
    if not refresh_remote_tracking(repo):
        raise Refusal(
            f"cannot reach the remote from clone {destination}, so nothing here "
            "can be shown to exist anywhere else; disposal decides containment "
            "against the remote, never against a snapshot of unknown age"
        )
    unpushed = unpushed_in_clone(repo)
    if unpushed:
        branch = git(repo, "rev-parse", "--abbrev-ref", "HEAD")
        raise Refusal(
            f"clone {destination} holds {len(unpushed)} commit(s) on its own "
            f"branches that no remote contains (branch {branch}, task "
            f"{manifest.get('task')}, tip {unpushed[0]}); PRESERVE THEM FIRST "
            "-- disposal never removes work that exists nowhere else"
        )
    head = git(repo, "rev-parse", "HEAD^{commit}")
    reachable = git(repo, "branch", "--remotes", "--contains", head).split()
    if not reachable:
        raise Refusal(
            f"clone {destination} HEAD {head} is on no remote-tracking branch, "
            "so there would be no recovery commit to record"
        )
    skipped, namespaces = unconsidered_unpushed(repo)
    shutil.rmtree(destination)
    print(
        f"CI_HUB_CHANGE disposed={destination} task={manifest.get('task')} "
        f"recovery_sha={head} recovery_refs={summarize_refs(reachable)}"
    )
    if skipped:
        print(
            f"CI_HUB_CHANGE unconsidered={skipped} namespaces={','.join(namespaces)} "
            "(outside this clone's branches; not weighed in the decision)"
        )


def command_activation_obligations(args: argparse.Namespace) -> int:
    primary = Path(args.primary).expanduser().resolve()
    store = activation_store_path(primary, args.store)
    code, report = render_activation_obligations(store, primary_filter=primary)
    print(report, end="")
    return code


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create", description="create one fresh per-change clone", help="create one fresh per-change clone")
    create.add_argument("--task", required=True)
    create.add_argument("--path", action="append", default=[])
    create.add_argument("--clone", default="~/temp/dev-hermit")
    create.add_argument("--remote", default="https://github.com/rrnewton/dev-hermit.git")
    create.set_defaults(func=command_create)
    seal = commands.add_parser("seal", description="test and seal one exact candidate commit", help="test and seal one exact candidate commit")
    seal.add_argument("--clone")
    seal.add_argument("--test", action="append", default=[])
    seal.set_defaults(func=command_seal)
    publish = commands.add_parser("publish", description="publish a still-current sealed commit", help="publish a still-current sealed commit")
    publish.add_argument("--clone")
    publish.add_argument("--primary", default="/home/newton/work/dev-hermit")
    publish.add_argument("--store", type=Path)
    publish.set_defaults(func=command_publish)
    activate = commands.add_parser(
        "activate", description="sync the published commit into a clean primary", help="sync the published commit into a clean primary"
    )
    activate.add_argument("--clone", required=True)
    activate.add_argument("--primary", default="/home/newton/work/dev-hermit")
    activate.add_argument("--store", type=Path)
    activate.set_defaults(func=command_activate)
    obligations = commands.add_parser(
        "activation-obligations",
        description="close completed activations and report every still-open obligation",
        help="close completed activations and report every still-open obligation",
    )
    obligations.add_argument("--primary", default="/home/newton/work/dev-hermit")
    obligations.add_argument("--store", type=Path)
    obligations.set_defaults(func=command_activation_obligations)
    dispose = commands.add_parser(
        "dispose",
        description="retire one per-change clone, refusing if anything is at risk",
        help="retire one per-change clone, refusing if anything is at risk",
    )
    dispose.add_argument("--clone", required=True)
    dispose.set_defaults(func=command_dispose)
    status = commands.add_parser("status", description="show manifest and seal state", help="show manifest and seal state")
    status.add_argument("--clone")
    status.set_defaults(func=command_status)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        return args.func(args) or 0
    except Refusal as error:
        print(f"ci-hub-clone-change: REFUSED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
