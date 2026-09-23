#!/usr/bin/env python3
"""Publish cell history directly to the existing hermit_test_ledger repository.

The historical command name is retained for callers. Validation receipts stay
in dev-hermit; cell series batches stay in its ignored spool until a freshly
fetched ledger commit proves publication. One persistent linked worktree owns
ledger writes. A scorecard failure does not undo published rows or change the
validation verdict, and leaves its local work available for recovery.
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Iterator, Mapping, Sequence

TOOL_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "lib"))
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "series"))
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "ledger"))
import git_env  # noqa: E402
import rust_toolchain  # noqa: E402
import series  # noqa: E402
import publisher  # noqa: E402
import finalized_proof  # noqa: E402

REMOTE_URL = series.LEDGER_REPOSITORY
REMOTE_BRANCH = "main"
METADATA_SCHEMA = "hermit-test-ledger/v1"
MIRRORED_SCORECARD_PATHS = (Path("scorecard/SCORECARD.md"), Path("scorecard/cells.json"))
WRITER_RELATIVE = Path("ignored/ci-hub/test-ledger-writer")
PUBLICATION_LOCK = "ci-hub-series-publication.lock"
LOCK_FD_ENV = "DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD"
DEFAULT_TIMEOUT_SECONDS = 900
LIFECYCLE_TIMEOUT_SECONDS = 1800
DEFAULT_ATTEMPTS = 3
SCORECARD_COMMANDS = (
    "observe-results", "import-results", "update-observations",
    "project-observations", "project-and-observe-results", "export-legacy",
)


class MirrorRefused(RuntimeError):
    """Publication is incomplete; local inputs must be retained."""


def _environment(extra: Mapping[str, str] | None = None) -> dict[str, str]:
    environment = git_env.sanitized_git_env()
    environment.setdefault("GIT_AUTHOR_NAME", "Hermit ledger publisher")
    environment.setdefault("GIT_AUTHOR_EMAIL", "hermit-ledger-publisher@example.invalid")
    environment.setdefault("GIT_COMMITTER_NAME", environment["GIT_AUTHOR_NAME"])
    environment.setdefault("GIT_COMMITTER_EMAIL", environment["GIT_AUTHOR_EMAIL"])
    # Only the locked scorecard invocation receives write authority.
    environment.pop(LOCK_FD_ENV, None)
    environment.pop(series.LEDGER_ROOT_ENV, None)
    if extra:
        environment.update(extra)
    return environment


def _run(
    command: Sequence[str], *, cwd: Path | None = None, check: bool = True,
    timeout: int = DEFAULT_TIMEOUT_SECONDS, environment: Mapping[str, str] | None = None,
    pass_fds: tuple[int, ...] = (),
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            list(command), cwd=cwd, capture_output=True, text=True, check=False,
            timeout=timeout, env=_environment(environment), pass_fds=pass_fds,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise MirrorRefused(f"cannot run {command[0]}: {error}") from error
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout).strip() or f"exit {result.returncode}"
        raise MirrorRefused(f"{command[0]} failed: {detail}")
    return result


def _git(repo: Path, *args: str, check: bool = True) -> str:
    return _run(["git", "-C", str(repo), *args], check=check).stdout.strip()


def _network(command: Sequence[str], *, cwd: Path | None, wrapper: str, **kwargs: Any):
    return _run([wrapper, *command], cwd=cwd, **kwargs)


def submodule_remote(parent: Path) -> str:
    state = series.shared_parent_root(parent)
    configured = _run([
        "git", "config", "--file", str(state / ".gitmodules"),
        "--get", "submodule.hermit_test_ledger.url",
    ]).stdout.strip()
    if configured != REMOTE_URL:
        raise MirrorRefused(f"hermit_test_ledger submodule URL is absent or unexpected: {configured!r}")
    return configured


def _require_clean(repo: Path) -> None:
    status = _git(repo, "status", "--porcelain", "--untracked-files=all")
    if status:
        raise MirrorRefused(
            f"ledger writer has unpublished work; preserve and recover it before retrying: {repo}\n{status}"
        )


@contextlib.contextmanager
def publication_writer(
    parent: Path, *, net_wrapper: str, lifecycle_deadline_ns: int | None = None,
) -> Iterator[tuple[Path, Path, int]]:
    if lifecycle_deadline_ns is not None:
        _verify_lifecycle_worker(lifecycle_deadline_ns)
    ledger = series.ledger_root(parent)
    common = Path(_git(ledger, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    writer = series.shared_parent_root(parent) / WRITER_RELATIVE
    with (common / PUBLICATION_LOCK).open("a+b") as lock:
        # Direct library callers retain their existing finite wait. Only the
        # production worker with a verified whole-lifecycle reaper may wait for
        # its remaining shared deadline; this budget never restarts per retry.
        deadline = (lifecycle_deadline_ns / 1e9 if lifecycle_deadline_ns is not None
                    else time.monotonic() + 60)
        while True:
            try:
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise MirrorRefused("another ledger publisher still holds the publication lock")
                time.sleep(0.1)
        _network(["git", "fetch", "--quiet", "origin", "main"], cwd=ledger, wrapper=net_wrapper)
        if not writer.exists():
            writer.parent.mkdir(parents=True, exist_ok=True)
            _git(ledger, "worktree", "add", "--detach", str(writer), "refs/remotes/origin/main")
        actual = Path(_git(writer, "rev-parse", "--path-format=absolute", "--git-common-dir"))
        if actual != common:
            raise MirrorRefused("ledger writer is not a linked worktree of the selected ledger repository")
        if Path(_git(writer, "rev-parse", "--show-toplevel")) != writer.resolve():
            raise MirrorRefused("ledger writer path is not its Git root")
        _require_clean(writer)
        yield ledger, writer, lock.fileno()


def recover_spooled_commit(writer: Path, parent: Path) -> str | None:
    """Retry a failed raw append only when the exact added rows remain spooled."""
    head = _git(writer, "rev-parse", "HEAD")
    remote = _git(writer, "rev-parse", "refs/remotes/origin/main")
    ancestor = _run(["git", "-C", str(writer), "merge-base", "--is-ancestor", head, remote], check=False)
    if ancestor.returncode == 0:
        return None
    if ancestor.returncode != 1:
        raise MirrorRefused(f"cannot reconcile ledger writer ancestry: {ancestor.stderr}")
    base = _git(writer, "merge-base", head, remote)
    paths = _git(writer, "diff", "--name-only", base, head).splitlines()
    def shard(path):
        parts = Path(path).parts
        return (len(parts) == 4 and parts[0] == "series"
                and publisher._store_shard("series", parts[1], parts[2], Path(parts[3]).stem) == path)
    if not paths or any(not shard(path) for path in paths):
        raise MirrorRefused(f"ledger writer has unpublished derived or unknown work at {head}; retained for recovery")
    old = series._published_snapshot_at_repo(writer, base, repository=REMOTE_URL)
    current = series._published_snapshot_at_repo(writer, head, repository=REMOTE_URL)
    before = {row["event_id"]: row for row in old.rows}
    after = {row["event_id"]: row for row in current.rows}
    pending, _ = publisher._store_snapshot("series", series.spool_dir(parent))
    spooled = {row["event_id"]: row for rows in pending.values() for row in rows}
    if (any(after.get(key) != value for key, value in before.items())
            or any(spooled.get(key) != value for key, value in after.items() if key not in before)):
        raise MirrorRefused(f"unpublished ledger commit {head} is not an unchanged spooled append; retained")
    # A durable ref keeps the refused attempt inspectable after the existing
    # publisher re-merges these same rows onto freshly fetched main.
    recovery = f"refs/heads/recovery/cell-series-{head}"
    existing = _run(["git", "-C", str(writer), "rev-parse", "--verify", "--quiet", recovery], check=False)
    if existing.returncode not in {0, 1} or (existing.returncode == 0 and existing.stdout.strip() != head):
        raise MirrorRefused(f"recovery ref {recovery} already names different work")
    _git(writer, "update-ref", recovery, head, head if existing.returncode == 0 else "0" * 40)
    return recovery


def publish_series(parent: Path, ledger: Path, writer: Path, *, net_wrapper: str, attempts: int) -> dict[str, Any]:
    # Immutable batches are moved only while readers and appenders are excluded.
    with series.series_spool_transition_lock(series.spool_dir(parent), exclusive=True):
        try:
            recovery = recover_spooled_commit(writer, parent)
            _git(writer, "checkout", "--detach", "refs/remotes/origin/main")
            report = publisher.publish(
                writer, series.spool_dir(parent), source_root=ledger,
                store="series", attempts=attempts, network_wrapper=net_wrapper,
            )
            if recovery:
                report["recovered_ref"] = recovery
            return report
        except (publisher.PublishRefused, subprocess.SubprocessError) as error:
            raise MirrorRefused(str(error)) from error


def _published_snapshot(writer: Path) -> dict[str, Any]:
    commit = _git(writer, "rev-parse", "HEAD^{commit}")
    published = series._published_snapshot_at_repo(writer, commit, repository=REMOTE_URL)
    rows = published.rows
    return {
        "schema": series.SCORECARD_SNAPSHOT_SCHEMA,
        "source": {"repository": REMOTE_URL, "path": series.SCORECARD_SNAPSHOT_SOURCE,
                   "commit": commit, "tree": series._series_tree_at(writer, commit),
                   "published_rows": len(rows), "local_sources": []},
        "rows_read": len(rows), "rows_sha256": hashlib.sha256(series._canonical_rows_bytes(rows)).hexdigest(),
        "rows": rows,
    }


def _committed_tool_source(tool_checkout: Path) -> str:
    """Bind the scorecard and its local Rust dependencies, not unrelated data."""
    inputs = (
        "ci/compat-envelope/scorecard.rs", "scripts/lib/rust_script_prelude.rs",
        "ci/manifest-plan/Cargo.toml", "ci/manifest-plan/build.rs", "ci/manifest-plan/src",
        "hermit-cli/src/canonical_verdict.rs", "hermit-cli/src/logdiff_report.rs",
        "ci/cargo-guest-binaries.rs",
        "detcore-model/Cargo.toml", "detcore-model/build.rs", "detcore-model/src",
        "Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain", "rust-toolchain.toml",
    )
    status = _git(tool_checkout, "status", "--porcelain", "--untracked-files=all", "--", *inputs)
    if status:
        raise MirrorRefused(f"scorecard tool inputs have uncommitted changes:\n{status}")
    # dagrun is a local path dependency in a separately versioned submodule.
    # Its unrelated Python/docs work does not change this Rust tool's identity.
    recorded = _git(tool_checkout, "ls-tree", "HEAD", "--", "agent-utils")
    if recorded:
        fields = recorded.split()
        dependency = tool_checkout / "agent-utils"
        if fields[:2] != ["160000", "commit"] or _git(dependency, "rev-parse", "HEAD^{commit}") != fields[2]:
            raise MirrorRefused("scorecard tool agent-utils dependency differs from its committed gitlink")
        status = _git(dependency, "status", "--porcelain", "--untracked-files=all", "--",
                      "rs/dagrun", "rs/Cargo.toml", "rs/Cargo.lock", "rs/.cargo",
                      "Cargo.toml", "Cargo.lock", ".cargo",
                      "rust-toolchain", "rust-toolchain.toml")
        if status:
            raise MirrorRefused(f"scorecard tool dependency has uncommitted changes:\n{status}")
    return _git(tool_checkout, "rev-parse", "HEAD^{commit}")


def finalized_producer_row(parent: Path, run_id: str, target: str, checkout: Path) -> bytes:
    """Resolve and recheck one run through the existing canonical ledger reader."""
    import validate_rows

    candidates = [row for row in validate_rows.rows(root=parent, preserve_admission=True)
                  if validate_rows._ledger_team(row.get("repo")) == "hermit"
                  and row.get("run_id") == run_id]
    current = validate_rows.current_validation_run_rows(candidates)
    if len(current) != 1:
        raise MirrorRefused("finalized producer run has no unique canonical terminal")
    candidate = current[0]
    if candidate.get("commit") != target or candidate.get("cwd") != str(checkout.resolve()):
        raise MirrorRefused("finalized producer run differs from the measured source identity")
    # The current-row projection alone is diagnostic: this exact query checks
    # the complete correction chain before accepting its selected terminal.
    selected = validate_rows.canonical_finalized_row(
        root=parent, **{name: candidate.get(name) for name in
                       ("repo", "run_id", "record_id", "commit", "cwd", "started_at")},
    )
    return (validate_rows.qualifying_receipt.admission_json_dumps(
        selected, sort_keys=True) + "\n").encode()


def _row_bytes(row: Mapping[str, Any]) -> bytes:
    import validate_rows
    return (validate_rows.qualifying_receipt.admission_json_dumps(row, sort_keys=True) + "\n").encode()


def _private_row(encoded: bytes) -> dict:
    import validate_rows
    return json.loads(encoded, object_pairs_hook=validate_rows.qualifying_receipt.AdmissionJsonObject)


def _parent_tool_commit() -> str:
    paths = (
        "ci-hub/series", "ci-hub/ledger", "ci-hub/lib",
        "ci-hub/qualifying_receipt.py", "ci-hub/admission_paths.py",
        "ci-hub/ci-hub", "ci-hub/ci-hub.rs",
        "ci-hub/validate/gate_completeness.py", "ci-hub/validate/flake_class.py",
        "ci-hub/validate/flaky_cells.py", "ci-hub/validate/flaky-cells.json",
        "ci-hub/validate/green_class.py",
        "ci-hub/validate/failure_evidence.py", "ci-hub/validate/qualifying-receipt.json",
        "ci-hub/health/ledger_spool_backlog.py",
        "scripts/parent-main-write", "scripts/check_commit_attribution.py",
    )
    if _git(TOOL_ROOT, "status", "--porcelain", "--untracked-files=all", "--", *paths):
        raise MirrorRefused("portable proof producer inputs have uncommitted changes")
    return _git(TOOL_ROOT, "rev-parse", "HEAD^{commit}")


def _select_published_terminal(source: Path, parent: Path, row: Mapping[str, Any], *,
                               net_wrapper: str):
    import validate_rows
    _network(["git", "fetch", "--quiet", "origin", "main"], cwd=source, wrapper=net_wrapper)
    commit = _git(source, "rev-parse", "refs/remotes/origin/main^{commit}")
    namespace = validate_rows.capture_committed_validation_namespace(source, commit, state_root=parent)
    selected = namespace.select(**{key: row.get(key) for key in
                                  ("repo", "run_id", "record_id", "commit", "cwd", "started_at")})
    if _row_bytes(selected["row"]) != _row_bytes(row):
        raise MirrorRefused("published canonical terminal differs from the finalized producer")
    return namespace, selected


def prepare_portable_proof(parent: Path, encoded_row: bytes, *, net_wrapper: str,
                           attempts: int) -> dict | None:
    """Publish owning parent authority before acquiring the cell-ledger lock.

    Legacy rows retain their existing path. A modern terminal must be in the
    complete published parent namespace, not merely in a supplied event subset.
    No writer clone is created here; unavailable preparation preserves raw rows.
    """
    row = _private_row(encoded_row)
    derived = row.get("schema_version") == 5 and "raw_result_input_census_v1" in row
    if row.get("schema_version") != 10 and not derived:
        return None
    if derived:
        finalized_proof.derived_row_binding(row)
    import publish_backlog
    import validate_rows
    tool_parent = _parent_tool_commit()
    source = series.shared_parent_root(parent) / publish_backlog.CLONE_RELATIVE
    if not (source / ".git").exists():
        raise MirrorRefused("existing dedicated parent publisher is unavailable; terminal remains unqualified")
    if _git(source, "remote", "get-url", "origin") not in {
        finalized_proof.PARENT_REPOSITORY, "git@github.com:rrnewton/dev-hermit.git",
    }:
        raise MirrorRefused("parent publisher has another repository identity")
    # This supported publisher owns its serialized parent-main lock and its
    # ordinary durability-proven spool retention. It runs before the cell lock.
    publisher.publish(
        source, validate_rows.live_spool_dir(parent), source_root=parent,
        store="ledger", attempts=attempts, network_wrapper=net_wrapper,
        serialized_writer=TOOL_ROOT / "scripts/parent-main-write",
        attribution=publish_backlog.publishing_attribution(),
    )
    namespace, selected = _select_published_terminal(source, parent, row, net_wrapper=net_wrapper)
    with namespace:
        namespace.verify_origin(selected["parent"], selected["terminal"])
    artifacts, objects = ((None, None) if derived else finalized_proof.capture_artifacts(parent, row))
    return {"source": source, "selection": selected, "artifacts": artifacts,
            "objects": objects, "parent_tool_commit": tool_parent}


def _cargo_target(state_root: Path | None) -> str | None:
    explicit = os.environ.get("CARGO_TARGET_DIR")
    if explicit is None:
        return str(state_root.resolve() / "target") if state_root is not None else None
    target = Path(explicit)
    if not explicit or not target.is_absolute():
        raise MirrorRefused("explicit CARGO_TARGET_DIR must be an existing absolute directory")
    try:
        target = target.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise MirrorRefused(f"explicit CARGO_TARGET_DIR is unavailable: {error}") from error
    if not target.is_dir():
        raise MirrorRefused("explicit CARGO_TARGET_DIR must be an existing absolute directory")
    return str(target)


def emit_derived_portable_proof(prepared: dict, parent: Path, results: Path,
                                tool_checkout: Path | None, *, net_wrapper: str) -> str:
    """Obtain new typed public operands from the complete private raw census.

    A previously published scorecard is not authority for these operands. The
    native writer verifies all original raw bytes again before this capture.
    """
    tool_checkout = (tool_checkout or TOOL_ROOT / "hermit").resolve()
    tool_commit = _committed_tool_source(tool_checkout)
    if _committed_tool_source(TOOL_ROOT / "hermit") != tool_commit:
        raise MirrorRefused("native derived-census provider differs from --tool-checkout; use the parent's committed Hermit provider")
    if _parent_tool_commit() != prepared["parent_tool_commit"]:
        raise MirrorRefused("derived-census producer changed before emission; retry with committed tools")
    compiler = rust_toolchain.selected_toolchain(tool_checkout)
    row = prepared["selection"]["row"]
    with tempfile.TemporaryDirectory(prefix="ledger-derived-census-") as temporary:
        directory = Path(temporary)
        row_path, output = directory / "row.json", directory / "derived.json"
        encoded_row = _row_bytes(row)
        row_path.write_bytes(encoded_row)
        _network([
            "rust-script", "--force", "--toolchain", compiler, str(TOOL_ROOT / "ci-hub/ci-hub.rs"),
            "compatibility-website-derived-census", "--row-json", str(row_path),
            "--results-root", str(results.resolve()), "--output-json", str(output),
        ], cwd=TOOL_ROOT, wrapper=net_wrapper, environment={
            "RUSTUP_TOOLCHAIN": compiler, "RUSTUP_AUTO_INSTALL": "0",
            "CARGO_TARGET_DIR": _cargo_target(parent),
        })
        if row_path.read_bytes() != encoded_row:
            raise MirrorRefused("original finalized row changed during derived emission")
        encoded = finalized_proof._read_local(directory, output.name)
        artifacts, objects = finalized_proof.capture_derived_cells(row, encoded)
    if (_parent_tool_commit() != prepared["parent_tool_commit"]
            or _committed_tool_source(TOOL_ROOT / "hermit") != tool_commit
            or _committed_tool_source(tool_checkout) != tool_commit):
        raise MirrorRefused("derived-census producer changed during emission; retain inputs and retry")
    prepared.update(artifacts=artifacts, objects=objects)
    return tool_commit


def published_scorecard_binding(writer: Path, snapshot: Mapping[str, Any], target: str) -> dict:
    """Require the existing public projection before a later proof attachment."""
    encoded = finalized_proof._read_local(writer, "latest.json")
    value = finalized_proof._json(encoded)
    if (not isinstance(value, dict) or value.get("schema") != METADATA_SCHEMA
            or value.get("source_repository") != REMOTE_URL
            or value.get("scorecard_hermit_commit") != target
            or value.get("source_tree") != snapshot["source"]["tree"]
            or value.get("rows_sha256") != snapshot["rows_sha256"]
            or value.get("rows") != snapshot["rows_read"]):
        raise MirrorRefused("published scorecard does not bind this measured source and raw series; run ordinary finalized scorecard publication first")
    source = finalized_proof._hash(value.get("source_commit"), "scorecard source commit", git=True)
    if value.get("series_commit") != source:
        raise MirrorRefused("published scorecard source and series commit differ")
    _git(writer, "merge-base", "--is-ancestor", source, snapshot["source"]["commit"])
    hashes = value.get("scorecard")
    if not isinstance(hashes, dict) or not all(path.as_posix() in hashes for path in MIRRORED_SCORECARD_PATHS):
        raise MirrorRefused("published scorecard omitted its bound files; run ordinary finalized publication first")
    for path, digest in hashes.items():
        finalized_proof._relative(path)
        finalized_proof._hash(digest, "scorecard file digest")
        if not path.startswith("scorecard/") or hashlib.sha256(finalized_proof._read_local(writer, path)).hexdigest() != digest:
            raise MirrorRefused("published scorecard bytes differ from metadata; preserve the refusal and regenerate normally")
    return {"commit": snapshot["source"]["commit"],
            "latest_sha256": hashlib.sha256(encoded).hexdigest(), "files": hashes}


def finish_portable_proof(prepared: Mapping[str, Any], parent: Path, writer: Path,
                          snapshot: Mapping[str, Any], tool_commit: str, *,
                          net_wrapper: str) -> dict[str, str]:
    """Publish exact transport only after the real H transaction succeeded."""
    selection = prepared["selection"]
    namespace, current = _select_published_terminal(
        prepared["source"], parent, selection["row"], net_wrapper=net_wrapper)
    with namespace:
        if current["terminal"] != selection["terminal"]:
            raise MirrorRefused("canonical terminal changed before proof publication")
        # Read-only requery under the cell lock takes no parent writer lock.
        # A correction without its carrier remains unavailable, never stale credit.
        index = finalized_proof.read_carriers(writer, snapshot["source"]["commit"])
        existing = finalized_proof.carrier_for_row(index, current["row"], current["terminal"])
        if existing is not None:
            namespace.verify_origin(existing["parent"], existing["terminal"])
        if _parent_tool_commit() != prepared["parent_tool_commit"]:
            raise MirrorRefused("portable proof producer source changed during publication")
        manifest = finalized_proof.make_manifest(
            current, prepared["artifacts"], parent_commit=prepared["parent_tool_commit"],
            hermit_tool_commit=tool_commit, snapshot=snapshot)
        return finalized_proof.stage_carrier(writer, manifest, prepared["objects"], existing=existing)


def _scorecard_execution(
    source_checkout: Path, target: str, tool_checkout: Path | None,
    state_root: Path | None,
) -> tuple[Path, str, str, dict[str, str]]:
    if _git(source_checkout, "rev-parse", "HEAD^{commit}") != target:
        raise MirrorRefused("scorecard source HEAD no longer matches the validation target")
    # The measured checkout is input, not a new compiler cache namespace.
    tool_checkout = (tool_checkout or TOOL_ROOT / "hermit").resolve()
    tool_commit = _committed_tool_source(tool_checkout)
    tool = tool_checkout / "ci/compat-envelope/scorecard.rs"
    if not tool.is_file():
        raise MirrorRefused(f"missing scorecard tool: {tool}")
    try:
        compiler = rust_toolchain.selected_toolchain(tool_checkout)
    except rust_toolchain.ToolchainRefused as error:
        raise MirrorRefused(f"cannot select scorecard tool compiler: {error}") from error
    environment = {
        "RUSTUP_TOOLCHAIN": compiler, "RUSTUP_AUTO_INSTALL": "0",
    }
    cargo_target = _cargo_target(state_root)
    if cargo_target is not None:
        # The immutable tool owns code; the existing state target owns mutable
        # Cargo output, shared by recurring publications rather than each run.
        environment["CARGO_TARGET_DIR"] = cargo_target
    return tool, tool_commit, compiler, environment


def prepare_scorecard(
    source_checkout: Path, target: str, *, tool_checkout: Path | None,
    state_root: Path, net_wrapper: str,
) -> tuple[str, str]:
    """Prepare the real compiler before taking the shared publication lock.

    The scorecard's help returns before repository/history access. It receives
    neither the ledger root nor its lock descriptor. The actual transaction
    still uses --force and rechecks these source identities under the lock.
    """
    tool, commit, compiler, environment = _scorecard_execution(
        source_checkout, target, tool_checkout, state_root)
    # derive() invokes this distinct Cargo binary, not the rust-script body.
    # Use its exact package/manifest/target prefix with its read-only help path.
    _network(
        ["cargo", "run", "--quiet", "-p", "hermit-manifest-plan",
         "--manifest-path", str(tool.parents[2] / "Cargo.toml"),
         "--target-dir", environment["CARGO_TARGET_DIR"], "--", "--help"],
        cwd=source_checkout, wrapper=net_wrapper, environment=environment,
    )
    _network(
        ["rust-script", "--force", "--toolchain", compiler, str(tool), "--help"],
        cwd=source_checkout, wrapper=net_wrapper, environment=environment,
    )
    prepared = _scorecard_execution(source_checkout, target, tool_checkout, state_root)
    if prepared[1:3] != (commit, compiler):
        raise MirrorRefused("scorecard tool changed during compiler preparation")
    return commit, compiler


def build_scorecard(
    source_checkout: Path, target: str, snapshot: Mapping[str, Any], writer: Path,
    *, lock_fd: int, net_wrapper: str, results: Path | None = None,
    scorecard_command: str | None = None, scorecard_args: Sequence[str] = (),
    tool_checkout: Path | None = None, finalized_run_id: str | None = None,
    state_root: Path | None = None, prepared_finalized_row: bytes | None = None,
    prepared_tool: tuple[str, str] | None = None,
) -> tuple[dict[str, str], str]:
    tool_checkout = (tool_checkout or TOOL_ROOT / "hermit").resolve()
    tool, tool_commit, compiler, environment = _scorecard_execution(
        source_checkout, target, tool_checkout, state_root)
    if prepared_tool is not None and prepared_tool != (tool_commit, compiler):
        raise MirrorRefused("scorecard tool changed after compiler preparation")
    environment.update({series.LEDGER_ROOT_ENV: str(writer), LOCK_FD_ENV: str(lock_fd)})
    print(
        f"ledger publication: scorecard tool {tool_commit} uses Rust {compiler}; "
        f"measured source {target}", file=sys.stderr,
    )
    commands = []
    finalized_row = None
    with contextlib.ExitStack() as temporary:
        if finalized_run_id is not None:
            if results is None or state_root is None or scorecard_command is not None:
                raise MirrorRefused("finalized run publication requires results and the canonical state root")
            finalized_row = (prepared_finalized_row if prepared_finalized_row is not None else
                             finalized_producer_row(state_root, finalized_run_id, target, source_checkout))
            row = json.loads(finalized_row)
            directory = Path(temporary.enter_context(tempfile.TemporaryDirectory(prefix="ledger-finalized-run-")))
            row_path, snapshot_path = directory / "row.json", directory / "snapshot.json"
            row_path.write_bytes(finalized_row)
            snapshot_bytes = series._snapshot_bytes(snapshot)
            snapshot_path.write_bytes(snapshot_bytes)
            commands.append([
                "project-and-observe-results", "--snapshot", str(snapshot_path),
                "--snapshot-sha256", hashlib.sha256(snapshot_bytes).hexdigest(),
                "--results", str(results.resolve()), "--expected-head", target,
                "--results-head", target, "--refreshed-at", row["started_at"],
                "--finalized-row", str(row_path),
                "--finalized-row-sha256", hashlib.sha256(finalized_row).hexdigest(),
                "--state-root", str(state_root.resolve()),
            ])
        elif scorecard_command:
            commands.append([scorecard_command, *scorecard_args])
        else:
            if results is not None:
                commands.append(["observe-results", "--results", str(results.resolve())])
            stamps = [row["emitted_at"] for row in snapshot["rows"]]
            commands.append([
                "project-observations", "--series-root", str(writer / "series"),
                "--refreshed-at", max(stamps) if stamps else str(snapshot["source"]["commit"]),
            ])
        for command in commands:
            result = _network(
                ["rust-script", "--force", "--toolchain", compiler, str(tool), *command],
                cwd=source_checkout, wrapper=net_wrapper,
                environment=environment, pass_fds=(lock_fd,),
            )
            if result.stdout:
                print(result.stdout, end="", file=sys.stderr)
            if result.stderr:
                print(result.stderr, end="", file=sys.stderr)
    if finalized_row is not None and finalized_producer_row(
        state_root, finalized_run_id, target, source_checkout,
    ) != finalized_row:
        raise MirrorRefused("canonical finalized producer changed during scorecard generation")
    if _git(source_checkout, "rev-parse", "HEAD^{commit}") != target:
        raise MirrorRefused("scorecard source HEAD changed during generation")
    if _committed_tool_source(tool_checkout) != tool_commit:
        raise MirrorRefused("scorecard tool HEAD changed during generation")
    if scorecard_command == "export-legacy":
        blob = _git(source_checkout, "rev-parse", "HEAD:ci/compat-envelope/cells.json")
        paths = (Path(f"scorecard/legacy/{blob}.json"),
                 Path(f"scorecard/legacy/{blob}.identity.json"))
    else:
        paths = MIRRORED_SCORECARD_PATHS
    return ({path.as_posix(): hashlib.sha256((writer / path).read_bytes()).hexdigest()
             for path in paths}, tool_commit)


def write_metadata(repo: Path, snapshot: Mapping[str, Any], target: str,
                   series_commit: str, scorecard: Mapping[str, str], tool_commit: str) -> None:
    source = snapshot["source"]
    value = {
        "schema": METADATA_SCHEMA, "source_repository": source["repository"],
        "source_commit": source["commit"], "source_tree": source["tree"],
        "rows": snapshot["rows_read"], "rows_sha256": snapshot["rows_sha256"],
        "local_sources": source["local_sources"], "series_commit": series_commit,
        "scorecard_hermit_commit": target, "scorecard": dict(sorted(scorecard.items())),
        "scorecard_tool_hermit_commit": tool_commit,
    }
    (repo / "latest.json").write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def mirror(
    parent: Path, source_checkout: Path | None, target: str | None, remote_url: str,
    *, net_wrapper: str, attempts: int = DEFAULT_ATTEMPTS, results: Path | None = None,
    publish_only: bool = False, scorecard_command: str | None = None,
    scorecard_args: Sequence[str] = (),
    tool_checkout: Path | None = None, finalized_run_id: str | None = None,
    lifecycle_deadline_ns: int | None = None,
    portable_proof_only: bool = False,
) -> dict[str, Any]:
    if remote_url != REMOTE_URL:
        raise MirrorRefused("unexpected ledger repository")
    if portable_proof_only and (finalized_run_id is None or results is None or publish_only or scorecard_command):
        raise MirrorRefused("proof-only attachment requires --finalized-run-id/--results and no scorecard override")
    legacy_export = scorecard_command == "export-legacy"
    if not legacy_export:
        # Raw publication and snapshots use a different native package from
        # the scorecard. Prepare it in its own actual caller environment; the
        # real rows are still validated again inside the writer transaction.
        series.prepare_shared_checker()
    finalized_row = None
    portable = None
    preparation_error = None
    prepared_tool = None
    tool_preparation_error = None
    if finalized_run_id is not None and not publish_only:
        try:
            if source_checkout is None or target is None:
                raise MirrorRefused("finalized publication requires measured source identity")
            finalized_row = finalized_producer_row(parent, finalized_run_id, target, source_checkout)
            portable = prepare_portable_proof(parent, finalized_row, net_wrapper=net_wrapper, attempts=attempts)
        except (MirrorRefused, OSError, ValueError, KeyError, series.SeriesError,
                publisher.PublishRefused, subprocess.SubprocessError) as error:
            # Raw cell rows still publish through the normal transaction below.
            preparation_error = error
    if not publish_only and not portable_proof_only and preparation_error is None:
        try:
            if source_checkout is None or target is None:
                raise MirrorRefused("scorecard preparation requires source checkout and target")
            prepared_tool = prepare_scorecard(
                source_checkout, target, tool_checkout=tool_checkout,
                state_root=parent, net_wrapper=net_wrapper,
            )
        except (MirrorRefused, OSError, ValueError) as error:
            # Compilation cannot acquire publication authority or prevent the
            # independent, durable raw-row append below.
            tool_preparation_error = error
    with publication_writer(parent, net_wrapper=net_wrapper, lifecycle_deadline_ns=lifecycle_deadline_ns) as (ledger, writer, lock_fd):
        if legacy_export or portable_proof_only:
            # Preserving legacy bytes must neither drain unrelated pending rows
            # nor claim that the existing scorecard was regenerated.
            _git(writer, "merge-base", "--is-ancestor", "HEAD", "refs/remotes/origin/main")
            _git(writer, "checkout", "--detach", "refs/remotes/origin/main")
            commit = _git(writer, "rev-parse", "HEAD^{commit}")
            publication = {"published": 0, "attempts": 0, "skipped":
                           "portable proof attachment only" if portable_proof_only else "legacy archive only"}
            snapshot = (_published_snapshot(writer) if portable_proof_only else
                        {"source": {"repository": REMOTE_URL, "commit": commit,
                                    "tree": series._series_tree_at(writer, commit)}})
        else:
            publication = publish_series(parent, ledger, writer, net_wrapper=net_wrapper, attempts=attempts)
            snapshot = _published_snapshot(writer)
        series_commit = snapshot["source"]["commit"]
        report = {
            "state": "published" if publication.get("published") else "unchanged",
            "repository": REMOTE_URL, "commit": series_commit,
            "source_repository": REMOTE_URL, "source_commit": series_commit,
            "source_tree": snapshot["source"]["tree"],
            "series_commit": series_commit, "raw_publication": publication,
            "published": publication.get("published", 0),
        }
        if not legacy_export:
            report["rows"] = snapshot["rows_read"]
        if publish_only:
            return report
        operation = "portable_proof" if portable_proof_only else ("legacy_export" if legacy_export else "scorecard")
        try:
            if preparation_error is not None:
                raise MirrorRefused(f"finalized proof preparation failed: {preparation_error}")
            if tool_preparation_error is not None:
                raise MirrorRefused(f"scorecard tool preparation failed: {tool_preparation_error}")
            if source_checkout is None or target is None:
                raise MirrorRefused("scorecard generation requires source checkout and target")
            if portable_proof_only:
                if portable is None or portable["selection"]["row"].get("schema_version") != 5:
                    raise MirrorRefused("proof-only attachment requires an original schema-5 census terminal")
                report["existing_scorecard"] = published_scorecard_binding(writer, snapshot, target)
                hashes = {}
                tool_commit = emit_derived_portable_proof(portable, parent, results, tool_checkout,
                                                          net_wrapper=net_wrapper)
            else:
                hashes, tool_commit = build_scorecard(
                    source_checkout, target, snapshot, writer, lock_fd=lock_fd,
                    net_wrapper=net_wrapper, results=results,
                    scorecard_command=scorecard_command, scorecard_args=scorecard_args,
                    tool_checkout=tool_checkout, finalized_run_id=finalized_run_id,
                    state_root=parent, prepared_finalized_row=finalized_row,
                    prepared_tool=prepared_tool,
                )
                if portable is not None and portable["artifacts"] is None:
                    derived_tool = emit_derived_portable_proof(portable, parent, results, tool_checkout,
                                                              net_wrapper=net_wrapper)
                    if derived_tool != tool_commit:
                        raise MirrorRefused("derived-census and scorecard tools differ")
            if portable is not None:
                hashes.update(finish_portable_proof(
                    portable, parent, writer, snapshot, tool_commit, net_wrapper=net_wrapper))
            if portable_proof_only:
                if published_scorecard_binding(writer, snapshot, target) != report["existing_scorecard"]:
                    raise MirrorRefused("published scorecard changed during proof-only attachment")
            elif not legacy_export:
                write_metadata(writer, snapshot, target, series_commit, hashes, tool_commit)
            # The child has authority only for scorecard output, never code or series.
            changed = _run([
                "git", "-C", str(writer), "status", "--porcelain", "--untracked-files=all",
            ]).stdout
            for line in changed.splitlines():
                path = line[3:]
                allowed = path in hashes if legacy_export or portable_proof_only else (
                    path == "latest.json" or path.startswith("scorecard/"))
                if not allowed:
                    raise MirrorRefused(f"scorecard changed an unexpected path: {path}")
            if changed:
                staged = sorted(hashes) if legacy_export or portable_proof_only else ["latest.json", "scorecard"]
                _git(writer, "add", "--", *staged)
                message = ("Attach verified finalized comparison operands" if portable_proof_only else
                           "Preserve exact legacy Hermit cell history" if legacy_export
                           else "Generate scorecard from published cell history")
                _git(writer, "commit", "--quiet", "-m", message)
                commit = _git(writer, "rev-parse", "HEAD^{commit}")
                _network(["git", "push", "origin", "HEAD:refs/heads/main"], cwd=writer, wrapper=net_wrapper)
                _network(["git", "fetch", "--quiet", "origin", "main"], cwd=writer, wrapper=net_wrapper)
                _git(writer, "merge-base", "--is-ancestor", commit, "refs/remotes/origin/main")
                report["commit"] = commit
                report["state"] = "published"
            report[operation] = {"state": "published", "files": hashes,
                                   "hermit_commit": target, "tool_hermit_commit": tool_commit}
        except (MirrorRefused, OSError, ValueError, KeyError, series.SeriesError) as error:
            report["state"] = "derived-failed"
            report[operation] = {"state": "failed", "error": str(error)}
        return report


def _verify_lifecycle_worker(deadline_ns: int) -> None:
    """Bind the internal worker to the real inherited cleanup domain."""
    try:
        group = Path(os.environ["HERMIT_BOX_LIFECYCLE_CGROUP"])
        monitor = int(os.environ["HERMIT_BOX_LIFECYCLE_MONITOR_PID"])
        declared = int(os.environ["HERMIT_BOX_LIFECYCLE_DEADLINE_NS"])
        actual = Path("/sys/fs/cgroup" + Path("/proc/self/cgroup").read_text().strip().split("::", 1)[1])
        ancestor = Path("/sys/fs/cgroup" + Path(f"/proc/{monitor}/cgroup").read_text().strip().split("::", 1)[1])
        command = Path(f"/proc/{monitor}/cmdline").read_bytes().split(b"\0")
        expected_runner = str(TOOL_ROOT / "scripts/hermit-box-run").encode()
        if (declared != deadline_ns or group != actual or group.parent != ancestor
                or not group.name.startswith(f"hermit-box-run-{monitor}-")
                or expected_runner not in command
                or b"--inherited-deadline-ns" not in command
                or str(deadline_ns).encode() not in command
                or not (group / "cgroup.kill").is_file()):
            raise ValueError("worker does not belong to the declared live cleanup domain")
    except (KeyError, OSError, ValueError, IndexError) as error:
        raise MirrorRefused(f"unverified publication lifecycle: {error}") from error


def main(argv: Sequence[str] | None = None) -> int:
    started_ns = time.monotonic_ns()
    actual_argv = list(sys.argv[1:] if argv is None else argv)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parent", type=Path, required=True)
    parser.add_argument("--source-checkout", type=Path)
    parser.add_argument("--tool-checkout", type=Path, default=TOOL_ROOT / "hermit")
    parser.add_argument("--target")
    parser.add_argument("--results", type=Path)
    parser.add_argument("--finalized-run-id", help="canonical completed validation run; row and proof are read from STATE")
    parser.add_argument("--publish-only", action="store_true")
    parser.add_argument("--portable-proof-only", action="store_true",
                        help="attach newly verified public census operands to an already published scorecard; preserve its files and original validation result")
    parser.add_argument("--scorecard-command", choices=SCORECARD_COMMANDS)
    parser.add_argument("--scorecard-arg", action="append", default=[])
    parser.add_argument("--net-wrapper", default=os.environ.get("WITH_PROXY", "with-proxy"))
    parser.add_argument("--attempts", type=int, default=DEFAULT_ATTEMPTS)
    parser.add_argument("--lifecycle-deadline-ns", type=int,
                        help="absolute monotonic caller deadline; capped at the existing 1800-second lifecycle ceiling")
    parser.add_argument("--bounded-worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args(actual_argv)
    if args.attempts <= 0 or (not args.publish_only and (args.source_checkout is None or args.target is None)):
        parser.error("positive attempts and either --publish-only or --source-checkout/--target are required")
    if args.finalized_run_id is not None and (args.results is None or args.publish_only or args.scorecard_command):
        parser.error("--finalized-run-id requires --results and ordinary scorecard publication")
    if args.portable_proof_only and (args.finalized_run_id is None or args.publish_only or args.scorecard_command):
        parser.error("--portable-proof-only requires --finalized-run-id/--results without --publish-only or --scorecard-command")
    deadline_ns = min(started_ns + LIFECYCLE_TIMEOUT_SECONDS * 1_000_000_000,
                      args.lifecycle_deadline_ns if args.lifecycle_deadline_ns is not None
                      else started_ns + LIFECYCLE_TIMEOUT_SECONDS * 1_000_000_000)
    if deadline_ns <= time.monotonic_ns() + 2_000_000_000:
        print("ledger publication: TIMEOUT: lifecycle deadline exhausted before preparation", file=sys.stderr)
        return 124
    if not args.bounded_worker:
        # The child cgroup is below this caller, so preparation, lock waiting,
        # every held child (including setsid descendants), and cleanup retain
        # the caller's original resource accounting and one absolute deadline.
        command = [str(TOOL_ROOT / "scripts/hermit-box-run"), "--passthrough",
                   "--inherited-deadline-ns", str(deadline_ns), "--diag-fd", "2", "--",
                   sys.executable, str(Path(__file__).resolve()), *actual_argv,
                   "--lifecycle-deadline-ns", str(deadline_ns), "--bounded-worker"]
        os.execv(command[0], command)
    try:
        _verify_lifecycle_worker(deadline_ns)
        report = mirror(
            args.parent.resolve(), args.source_checkout.resolve() if args.source_checkout else None,
            args.target, submodule_remote(args.parent.resolve()), net_wrapper=args.net_wrapper,
            attempts=args.attempts, results=args.results, publish_only=args.publish_only,
            scorecard_command=args.scorecard_command, scorecard_args=args.scorecard_arg,
            tool_checkout=args.tool_checkout, finalized_run_id=args.finalized_run_id,
            lifecycle_deadline_ns=deadline_ns,
            portable_proof_only=args.portable_proof_only,
        )
    except (MirrorRefused, OSError, KeyError, TypeError, series.SeriesError) as error:
        print(f"ledger publication: FAILED: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 1 if report["state"] == "derived-failed" else 0


if __name__ == "__main__":
    raise SystemExit(main())
