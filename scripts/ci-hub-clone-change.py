#!/usr/bin/env python3
"""Prepare, seal, publish, and activate one isolated ci-hub change."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Sequence


SCHEMA = "ci-hub-change/v1"
MANIFEST = "ci-hub-change-manifest.json"
SEAL = "ci-hub-change-seal.json"
TASK_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")


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
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise Refusal(f"{shlex.join(argv)}: {detail}")
    return result


def git(repo: Path, *args: str, check: bool = True) -> str:
    return run(("git", "-C", str(repo), *args), check=check).stdout.strip()


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
    manifest, seal = verified_seal(repo, require_unpublished_base=True)
    run((str(repo / "scripts/parent-main-write"), "publish", "HEAD"), cwd=repo, capture=False)
    published = fetch_main(repo)
    if published != seal["candidate"]:
        raise Refusal(
            "remote main moved after publication: "
            f"expected {seal['candidate']}, observed {published}"
        )
    print(f"CI_HUB_CHANGE published=1 task={manifest['task']} candidate={published}")


def dirty_candidate_paths(primary: Path, paths: list[str]) -> list[str]:
    result = git(primary, "status", "--porcelain=v1", "--untracked-files=all", "--", *paths)
    return [line for line in result.splitlines() if line]


def is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    result = run(
        ("git", "-C", str(repo), "merge-base", "--is-ancestor", ancestor, descendant),
        check=False,
    )
    return result.returncode == 0


def command_activate(args: argparse.Namespace) -> None:
    clone = repository_root(Path(args.clone).expanduser())
    primary = repository_root(Path(args.primary).expanduser())
    manifest, seal = verified_seal(clone, require_unpublished_base=False)
    candidate = str(seal["candidate"])
    published = fetch_main(primary)
    if not is_ancestor(primary, candidate, published):
        raise Refusal(
            f"activation requires current main to contain candidate; candidate={candidate} "
            f"origin/main={published}"
        )
    paths = [str(path) for path in seal["changed_paths"]]
    dirty = dirty_candidate_paths(primary, paths)
    if dirty:
        raise Refusal(
            "primary candidate paths are dirty; refusing to sweep them into activation: "
            + " | ".join(dirty)
        )
    run(
        (str(primary / "scripts/parent-main-write"), "sync", "--automatic"),
        cwd=primary,
        capture=False,
    )
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
    print(
        f"CI_HUB_CHANGE activated=1 task={manifest['task']} candidate={candidate} "
        f"activated_head={head} paths={len(paths)}"
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


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create", help="create one fresh per-change clone")
    create.add_argument("--task", required=True)
    create.add_argument("--path", action="append", default=[])
    create.add_argument("--clone", default="~/temp/dev-hermit")
    create.add_argument("--remote", default="https://github.com/rrnewton/dev-hermit.git")
    create.set_defaults(func=command_create)
    seal = commands.add_parser("seal", help="test and seal one exact candidate commit")
    seal.add_argument("--clone")
    seal.add_argument("--test", action="append", default=[])
    seal.set_defaults(func=command_seal)
    publish = commands.add_parser("publish", help="publish a still-current sealed commit")
    publish.add_argument("--clone")
    publish.set_defaults(func=command_publish)
    activate = commands.add_parser(
        "activate", help="sync the published commit into a clean primary"
    )
    activate.add_argument("--clone", required=True)
    activate.add_argument("--primary", default="/home/newton/work/dev-hermit")
    activate.set_defaults(func=command_activate)
    status = commands.add_parser("status", help="show manifest and seal state")
    status.add_argument("--clone")
    status.set_defaults(func=command_status)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        args.func(args)
    except Refusal as error:
        print(f"ci-hub-clone-change: REFUSED: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
