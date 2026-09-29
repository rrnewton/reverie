#!/usr/bin/env python3
"""Publish cell history directly to the existing hermit_test_ledger repository.

The historical command name is retained for callers. Validation receipts stay
in dev-hermit; cell series batches stay in its ignored spool until a freshly
fetched ledger commit proves publication. One persistent linked worktree owns
ledger writes. A scorecard failure does not undo published rows or change the
validation verdict, and leaves the writer and the spooled rows available for
recovery. The renderer's own partial output is discarded with its copy (below);
it is a function of the published series commit and the measured tree, so it
is regenerated rather than recovered.

The scorecard is rendered by the measured checkout's own scorecard.rs, so a
renderer change travels with its SCORECARD.md. That renderer is code under
validation, so it never receives the writer: it runs against a throwaway copy
of the ledger at the published series commit, without the network wrapper,
proxy variables, GitHub credential variables, or the user's and system's Git
configuration (the fleet's Git proxy and credential helper live there), and
only its exact generated files are copied back (_render_copy). Every ledger commit is then checked to sit directly
on that series commit and to change only the paths being published before it is
pushed (_commit_and_push). This is not an operating-system sandbox: the
renderer runs as the same Unix user, so a hostile renderer could still reach the
writer path, read that user's stored credentials, or rebuild a network route
itself. These checks stop the accidental
and the in-band routes (writing through DEV_HERMIT_TEST_LEDGER_ROOT, committing
in the ledger it was given, planting files beside a carrier), not a deliberate
escape.

A derived portable proof is independent of that scorecard and publishes even
when the scorecard fails or its head order cannot be decided. The public
scorecard head (latest.json and the rendered "Last regenerated" notice) only
moves forward: an older head publishes its rows and proof but not a scorecard.
The head is not required to be on Hermit main (see scorecard_order).
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import re
import shutil
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
SCORECARD_HEAD_TIME_FIELD = "scorecard_hermit_commit_time"
_COMMIT = re.compile(r"[0-9a-f]{40}")
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
# The renderer runs after prepare_scorecard fetched its dependencies, against a
# ledger copy it cannot push from. None of these ambient routes or credentials
# is needed, so none is passed on. Git configuration injected through the
# environment is withheld too; build_scorecard also replaces the user's and the
# system's Git configuration, which on the fleet hosts carry the forward proxy
# and the GitHub credential helper.
RENDERER_WITHHELD_ENV = (
    "http_proxy", "https_proxy", "ftp_proxy", "all_proxy",
    "HTTP_PROXY", "HTTPS_PROXY", "FTP_PROXY", "ALL_PROXY",
    "GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN",
    "GIT_ASKPASS", "SSH_ASKPASS", "SSH_AUTH_SOCK", "FLEET_REAL_GH",
    "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS",
)
RENDER_COPY_PREFIX = "test-ledger-render-"


class MirrorRefused(RuntimeError):
    """Publication is incomplete; local inputs must be retained."""


def _environment(extra: Mapping[str, str] | None = None,
                 withhold: Sequence[str] = ()) -> dict[str, str]:
    environment = git_env.sanitized_git_env()
    for name in withhold:
        environment.pop(name, None)
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
    pass_fds: tuple[int, ...] = (), withhold: Sequence[str] = (),
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            list(command), cwd=cwd, capture_output=True, text=True, check=False,
            timeout=timeout, env=_environment(environment, withhold), pass_fds=pass_fds,
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


def _worktree_changes(repo: Path) -> dict[str, str]:
    """Map every staged, unstaged or untracked path to its two-letter status.

    A rename or copy names two paths; both are reported. Paths are read with
    -z, so quoting cannot hide or disguise a name.
    """
    output = _run(["git", "-C", str(repo), "status", "--porcelain=v1", "-z",
                   "--untracked-files=all"]).stdout
    entries = output.split("\0")
    changes: dict[str, str] = {}
    index = 0
    while index < len(entries):
        entry = entries[index]
        index += 1
        if not entry:
            continue
        if len(entry) < 4 or entry[2] != " ":
            raise MirrorRefused(f"cannot parse ledger writer status entry {entry!r}")
        status = entry[:2]
        changes[entry[3:]] = status
        if "R" in status or "C" in status:
            if index >= len(entries) or not entries[index]:
                raise MirrorRefused(f"ledger writer rename entry has no source path: {entry!r}")
            changes[entries[index]] = status
            index += 1
    return changes


def _commit_and_push(writer: Path, paths: Sequence[str], message: str, *,
                     net_wrapper: str, base: str) -> str | None:
    """Commit exactly PATHS directly on BASE, push, and prove it reached origin/main.

    BASE is the published series commit this publication was computed from.
    The writer must still be at BASE, and the new commit must have BASE as its
    only parent and change exactly the staged PATHS; otherwise nothing is
    pushed and the writer is retained. So a commit that anything else made in
    the writer (an empty one, or one carrying a forged latest.json or carrier)
    can never ride along underneath this publication. Returns None when none
    of PATHS changed. Refuses if the index holds any other path. Hooks are
    disabled: nothing planted in the ledger's Git directory runs here.
    """
    if not paths:
        return None
    head = _git(writer, "rev-parse", "HEAD^{commit}")
    if head != base:
        raise MirrorRefused(f"ledger writer HEAD {head} is not the publication base {base}; "
                            "nothing was pushed and the writer is retained for recovery")
    _git(writer, "add", "--", *paths)
    staged = {path for path in _run([
        "git", "-C", str(writer), "diff", "--cached", "--name-only", "-z",
    ]).stdout.split("\0") if path}
    if not staged <= set(paths):
        raise MirrorRefused(
            "ledger writer index holds paths outside this publication: "
            + ", ".join(sorted(staged - set(paths))))
    if not staged:
        return None
    _git(writer, "-c", "core.hooksPath=/dev/null", "commit", "--quiet", "--no-verify", "-m", message)
    commit = _git(writer, "rev-parse", "HEAD^{commit}")
    parents = _git(writer, "rev-list", "--parents", "-n", "1", commit).split()
    if parents != [commit, base]:
        raise MirrorRefused(f"ledger commit {commit} does not sit directly on the publication base "
                            f"{base} (parents {parents[1:]}); nothing was pushed")
    changed = {path for path in _run([
        "git", "-C", str(writer), "diff-tree", "--no-commit-id", "--name-only", "-r", "-z", base, commit,
    ]).stdout.split("\0") if path}
    if changed != staged:
        raise MirrorRefused(f"ledger commit {commit} changes paths other than this publication: "
                            + ", ".join(sorted(changed ^ staged)) + "; nothing was pushed")
    _network(["git", "-c", "core.hooksPath=/dev/null", "push", "origin", f"{commit}:refs/heads/main"],
             cwd=writer, wrapper=net_wrapper)
    _network(["git", "fetch", "--quiet", "origin", "main"], cwd=writer, wrapper=net_wrapper)
    _git(writer, "merge-base", "--is-ancestor", commit, "refs/remotes/origin/main")
    return commit


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
    def quarantine_shard(path):
        parts = Path(path).parts
        return (len(parts) == 5 and parts[:2] == ("quarantine", "series")
                and publisher._store_shard("series-quarantine", parts[2], parts[3],
                                           Path(parts[4]).stem) == path)
    if not paths or any(not (shard(path) or quarantine_shard(path)) for path in paths):
        raise MirrorRefused(f"ledger writer has unpublished derived or unknown work at {head}; retained for recovery")
    if any(quarantine_shard(path) for path in paths):
        # Same rule for refused inputs: every published record is unchanged and
        # every added one is still pending in the local quarantine spool.
        try:
            with series.series_spool_transition_lock(
                series.quarantine_dir(parent), exclusive=True,
                timeout_seconds=series.QUARANTINE_LOCK_TIMEOUT_SECONDS,
            ):
                pending_quarantine, _ = publisher._store_snapshot(
                    "series-quarantine", series.quarantine_dir(parent))
            _, old_quarantine = series.published_quarantine_at_repo(writer, base)
            _, new_quarantine = series.published_quarantine_at_repo(writer, head)
        except series.LedgerError as error:
            raise MirrorRefused(f"cannot check unpublished quarantine at {head}: {error}") from error
        was = {record["event_id"]: record for record in old_quarantine}
        now = {record["event_id"]: record for record in new_quarantine}
        waiting = {record["event_id"]: record
                   for records in pending_quarantine.values() for record in records}
        if (any(now.get(key) != value for key, value in was.items())
                or any(waiting.get(key) != value for key, value in now.items() if key not in was)):
            raise MirrorRefused(f"unpublished ledger commit {head} is not an unchanged spooled quarantine append; retained")
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


def publish_series(parent: Path, ledger: Path, writer: Path, *, net_wrapper: str, attempts: int,
                   run_id: str | None = None) -> dict[str, Any]:
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
        except (publisher.PublishRefused, subprocess.SubprocessError) as error:
            raise MirrorRefused(str(error)) from error
    report["quarantine"] = publish_quarantine(
        parent, ledger, writer, net_wrapper=net_wrapper, attempts=attempts, run_id=run_id,
    )
    return report


def _quarantine_records(path: Path) -> list[dict]:
    return [record for file in sorted(path.glob("*.jsonl"))
            for record in series._read_quarantine_file(file)]


def publish_quarantine(parent: Path, ledger: Path, writer: Path, *, net_wrapper: str, attempts: int,
                       run_id: str | None = None) -> dict[str, Any]:
    """Publish refused series inputs and count them; never undo the series.

    The result is the machine-readable count that start_unit copies into the
    run's ``ledger_mirror.raw_publication.quarantine``. A failure is recorded
    in ``error`` rather than raised: the series rows are already published, the
    refused inputs stay in the local spool (``pending_records``) for the next
    publication, and ``ledger_spool_backlog --store series-quarantine`` alarms
    on them. The writer is returned to published main so the scorecard never
    binds an unpushed quarantine commit.
    """
    directory = series.quarantine_dir(parent)
    result: dict[str, Any] = {
        "schema": "series-quarantine-publication/v1", "published": 0,
        "already_published": 0, "commit": None, "error": None,
        "pending_records": None, "held_records": None, "run": None,
    }
    try:
        with series.series_spool_transition_lock(
            directory, exclusive=True, timeout_seconds=series.QUARANTINE_LOCK_TIMEOUT_SECONDS,
        ):
            outcome = publisher.publish(
                writer, directory, source_root=ledger, store="series-quarantine",
                attempts=attempts, network_wrapper=net_wrapper,
            )
        result["published"] = outcome.get("published", 0)
        result["already_published"] = outcome.get("already_published", 0)
        result["commit"] = outcome.get("commit")
    except (publisher.PublishRefused, subprocess.SubprocessError, OSError, series.LedgerError) as error:
        result["error"] = f"{type(error).__name__}: {error}"
        try:
            _git(writer, "checkout", "--quiet", "--force", "--detach", "refs/remotes/origin/main")
            _git(writer, "clean", "-fdq", "--", "quarantine")
        except (subprocess.SubprocessError, OSError) as restore_error:
            result["error"] += f"; writer restore FAILED: {restore_error}"
    try:
        pending = _quarantine_records(directory)
        result["pending_records"] = len(pending)
        result["held_records"] = len(_quarantine_records(directory / series.QUARANTINE_HELD_SUBDIR))
        if run_id is not None:
            _, published = series.published_quarantine_at_repo(writer, "refs/remotes/origin/main")
            mine = [record for record in published if record.get("run_id") == run_id]
            waiting = [record for record in pending if record.get("run_id") == run_id]
            result["run"] = {
                "run_id": run_id,
                "published": len(mine),
                "withheld": sum(record.get("withheld") is not None for record in mine),
                "unpublished": len(waiting),
                "records": len({record["event_id"] for record in mine}
                               | {series.quarantine_identity(record) for record in waiting}),
            }
    except (subprocess.SubprocessError, OSError, series.LedgerError) as error:
        count_error = f"cannot count quarantine: {type(error).__name__}: {error}"
        result["error"] = count_error if result["error"] is None else f"{result['error']}; {count_error}"
    return result


def _published_snapshot(writer: Path, ref: str = "HEAD") -> dict[str, Any]:
    commit = _git(writer, "rev-parse", f"{ref}^{{commit}}")
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


def refuse_non_cargo_payload(encoded_row: bytes) -> None:
    """Keep a Buck full run's cells out of the scorecard.

    Such a run executes the release E2E payload, which has no debug assertions
    and no overflow checks. Its cells are not the debug observations the
    scorecard records, so the finalized publication refuses before any proof
    or scorecard work. Raw rows still publish. An absent field predates it and
    was a Cargo run; any present value other than exactly "cargo" refuses.
    """
    row = _private_row(encoded_row)
    if "release_builder" in row and row["release_builder"] != "cargo":
        raise MirrorRefused(
            "finalized run used a non-Cargo release payload "
            f"(release_builder={row['release_builder']!r}); its cells are not scorecard observations"
        )


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
    # A derived census proof depends only on the finalized row and raw results,
    # so it may publish without the scorecard. Schema-10 operands are verified
    # by the scorecard's own native transaction and stay bound to its success.
    return {"source": source, "selection": selected, "artifacts": artifacts,
            "objects": objects, "parent_tool_commit": tool_parent, "derived": derived}


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
            or value.get("scorecard_hermit_commit") != target):
        raise MirrorRefused("published scorecard does not bind this measured source and raw series; run ordinary finalized scorecard publication first")
    source = finalized_proof._hash(value.get("source_commit"), "scorecard source commit", git=True)
    if value.get("series_commit") != source:
        raise MirrorRefused("published scorecard source and series commit differ")
    _git(writer, "merge-base", "--is-ancestor", source, snapshot["source"]["commit"])
    # Raw publication can advance independently of the frozen scorecard. Bind
    # that projection to its own immutable input, then require every original
    # event to survive unchanged in the current proof attachment's input.
    projected = _published_snapshot(writer, source)
    if (value.get("source_tree") != projected["source"]["tree"]
            or value.get("rows_sha256") != projected["rows_sha256"]
            or value.get("rows") != projected["rows_read"]):
        raise MirrorRefused("published scorecard metadata differs from its immutable raw source")
    current_rows = {row["event_id"]: series._canonical_row_json(row) for row in snapshot["rows"]}
    if any(current_rows.get(row["event_id"]) != series._canonical_row_json(row) for row in projected["rows"]):
        raise MirrorRefused("published scorecard source events were changed or removed from current raw series")
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
    # The measured checkout renders its own SCORECARD.md with its own
    # scorecard.rs, so a renderer change and the prose it produces travel in
    # one commit (https://github.com/rrnewton/dev-hermit/issues/392). The
    # parent's hermit gitlink is not consulted. build_scorecard confines the
    # child's writes to the exact generated paths.
    tool_checkout = (tool_checkout or source_checkout).resolve()
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


@contextlib.contextmanager
def _render_copy(writer: Path, base: str) -> Iterator[Path]:
    """Yield a throwaway ledger checkout at BASE for the measured renderer.

    The copy reads the writer's objects through alternates and writes its own,
    so a commit, index, ref, config or hook change the renderer makes through
    DEV_HERMIT_TEST_LEDGER_ROOT stays in the copy and is discarded. Its origin
    URL is the ledger's (scorecard.rs checks it), its push URL is a path that
    does not exist, and its publication lock is a link to the real one, which
    the caller holds. Only the lock holder creates these copies, so one left
    behind by a killed publisher is removed by the next. Each copy has a fresh
    name, so a stale one that cannot be removed is inert and never reused.
    """
    common = Path(_git(writer, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    for stale in writer.parent.glob(RENDER_COPY_PREFIX + "*"):
        if stale.is_dir() and not stale.is_symlink():
            shutil.rmtree(stale, ignore_errors=True)
    with tempfile.TemporaryDirectory(prefix=RENDER_COPY_PREFIX, dir=writer.parent,
                                     ignore_cleanup_errors=True) as temporary:
        copy = Path(temporary) / "hermit_test_ledger"
        _run(["git", "init", "--quiet", "--template=", str(copy)])
        (copy / ".git/objects/info").mkdir(parents=True, exist_ok=True)
        (copy / ".git/objects/info/alternates").write_text(str(common / "objects") + "\n")
        _git(copy, "config", "remote.origin.url", REMOTE_URL)
        _git(copy, "config", "remote.origin.pushurl", str(copy / ".git/push-disabled"))
        _git(copy, "checkout", "--quiet", "--detach", base)
        (copy / ".git" / PUBLICATION_LOCK).symlink_to(common / PUBLICATION_LOCK)
        if _git(copy, "rev-parse", "HEAD^{commit}") != base or _worktree_changes(copy):
            raise MirrorRefused(f"cannot prepare a clean ledger copy at {base} for the scorecard tool")
        yield copy


def _copy_back(copy: Path, writer: Path, base: str, paths: Sequence[Path]) -> None:
    """Move exactly the renderer's generated files from COPY into the writer."""
    if _git(copy, "rev-parse", "HEAD^{commit}") != base:
        raise MirrorRefused("scorecard tool moved the HEAD of its ledger copy; nothing was copied "
                            "into the ledger writer")
    expected = {path.as_posix() for path in paths}
    unexpected = sorted(set(_worktree_changes(copy)) - expected)
    if unexpected:
        raise MirrorRefused(
            "scorecard tool changed ledger paths outside its generated output; nothing was copied "
            "into the ledger writer: " + ", ".join(unexpected))
    # The copy is the only ledger the renderer was given; a change to the
    # writer itself means something reached past it.
    if _git(writer, "rev-parse", "HEAD^{commit}") != base:
        raise MirrorRefused("the ledger writer HEAD moved while the scorecard tool ran; writer retained for recovery")
    touched = _worktree_changes(writer)
    if touched:
        raise MirrorRefused("the ledger writer changed while the scorecard tool ran; writer retained for "
                            "recovery: " + ", ".join(sorted(touched)))
    generated = {}
    for path in paths:
        source = copy / path
        if source.is_symlink() or not source.is_file():
            raise MirrorRefused(f"scorecard tool output {path} is not a regular file")
        generated[path] = source.read_bytes()
    for path, encoded in generated.items():
        destination = writer / path
        if destination.is_symlink():
            raise MirrorRefused(f"ledger writer path {path} is a symbolic link")
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(encoded)


def build_scorecard(
    source_checkout: Path, target: str, snapshot: Mapping[str, Any], writer: Path,
    *, lock_fd: int, net_wrapper: str, results: Path | None = None,
    scorecard_command: str | None = None, scorecard_args: Sequence[str] = (),
    tool_checkout: Path | None = None, finalized_run_id: str | None = None,
    state_root: Path | None = None, prepared_finalized_row: bytes | None = None,
    prepared_tool: tuple[str, str] | None = None,
) -> tuple[dict[str, str], str]:
    tool_checkout = (tool_checkout or source_checkout).resolve()
    tool, tool_commit, compiler, environment = _scorecard_execution(
        source_checkout, target, tool_checkout, state_root)
    if prepared_tool is not None and prepared_tool != (tool_commit, compiler):
        raise MirrorRefused("scorecard tool changed after compiler preparation")
    base = snapshot["source"]["commit"]
    print(
        f"ledger publication: scorecard tool {tool_commit} uses Rust {compiler}; "
        f"measured source {target}", file=sys.stderr,
    )
    commands = []
    finalized_row = None
    with contextlib.ExitStack() as temporary:
        ledger = temporary.enter_context(_render_copy(writer, base))
        # prepare_scorecard already fetched every dependency with the network;
        # this run needs none, and gets neither network routes nor credentials.
        environment.update({
            series.LEDGER_ROOT_ENV: str(ledger), LOCK_FD_ENV: str(lock_fd),
            "CARGO_NET_OFFLINE": "true", "GIT_TERMINAL_PROMPT": "0",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
        })
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
                "project-observations", "--series-root", str(ledger / "series"),
                "--refreshed-at", max(stamps) if stamps else str(snapshot["source"]["commit"]),
            ])
        for command in commands:
            result = _run(
                ["rust-script", "--force", "--toolchain", compiler, str(tool), *command],
                cwd=source_checkout, environment=environment, pass_fds=(lock_fd,),
                withhold=RENDERER_WITHHELD_ENV,
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
        # The renderer is the measured tree's code. Its write authority is
        # exactly the generated files, copied last so a failure above leaves
        # the writer untouched.
        _copy_back(ledger, writer, base, paths)
    return ({path.as_posix(): hashlib.sha256((writer / path).read_bytes()).hexdigest()
             for path in paths}, tool_commit)


def published_scorecard_head(writer: Path) -> dict[str, Any] | None:
    """Return the hermit commit (and recorded commit time) latest.json names at HEAD."""
    if not _git(writer, "ls-tree", "HEAD", "--", "latest.json"):
        return None
    value = finalized_proof._json(_run(["git", "-C", str(writer), "show", "HEAD:latest.json"]).stdout.encode())
    if not isinstance(value, dict) or value.get("schema") != METADATA_SCHEMA:
        raise MirrorRefused("published latest.json is not scorecard metadata; the scorecard head cannot be ordered")
    commit = value.get("scorecard_hermit_commit")
    if not isinstance(commit, str) or not _COMMIT.fullmatch(commit):
        raise MirrorRefused("published latest.json names no forty-hex scorecard head")
    recorded = value.get(SCORECARD_HEAD_TIME_FIELD)
    if recorded is not None and (type(recorded) is not int or recorded <= 0):
        raise MirrorRefused(f"published latest.json has an invalid {SCORECARD_HEAD_TIME_FIELD}")
    return {"commit": commit, "time": recorded}


def _commit_time(repo: Path, commit: str) -> int | None:
    """Committer time of COMMIT in REPO, or None when REPO does not have it."""
    found = _run(["git", "-C", str(repo), "rev-parse", "--verify", "--quiet", f"{commit}^{{commit}}"],
                 check=False)
    if found.returncode == 1:
        return None
    if found.returncode != 0 or found.stdout.strip() != commit:
        raise MirrorRefused(f"cannot resolve hermit commit {commit} in {repo}")
    return int(_git(repo, "show", "-s", "--format=%ct", commit))


def _is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    result = _run(["git", "-C", str(repo), "merge-base", "--is-ancestor", ancestor, descendant],
                  check=False)
    if result.returncode not in {0, 1}:
        raise MirrorRefused(f"cannot order hermit commits {ancestor} and {descendant}: "
                            + (result.stderr.strip() or f"exit {result.returncode}"))
    return result.returncode == 0


def scorecard_order(writer: Path, source_checkout: Path, target: str) -> dict[str, Any]:
    """Decide whether TARGET may become the public scorecard head.

    The head only moves forward. A descendant of the published head advances
    and an ancestor is superseded. Divergent heads compare committer time: the
    target advances only when strictly newer, so equal or older is superseded.
    A published head this repository cannot read is compared by the commit time
    latest.json records; without that record the order is unknown and refused.

    Eligibility is deliberately not limited to Hermit main. This mirror runs
    when a validation finishes, which is before a lander pushes the validated
    commit to main, so a main-only rule would withhold every pre-landing
    scorecard; and a lane rebased onto main is a descendant of main anyway, so
    a descendant-of-main rule would not exclude lane commits either. The
    consequence is that a validated lane-branch commit can own latest.json and
    the "Last regenerated" notice (on 2026-09-28 the published head was
    6580cd7386e7f91fcb05142dfa8695dfad563d4b, which is not on Hermit main)
    until a strictly newer commit replaces it. latest.json names that commit
    and its committer time, so a reader can always see which tree it was.
    """
    current = published_scorecard_head(writer)
    target_time = _commit_time(source_checkout, target)
    if target_time is None:
        raise MirrorRefused(f"measured source commit {target} is not readable in {source_checkout}")
    order = {"hermit_commit": target, "hermit_commit_time": target_time,
             "current": current and current["commit"], "current_time": current and current["time"]}
    if current is None:
        return {**order, "state": "first", "basis": "no published scorecard head"}
    head = current["commit"]
    if _git(source_checkout, "rev-parse", "--is-shallow-repository") != "false":
        raise MirrorRefused("measured checkout is shallow; scorecard head ancestry cannot be decided")
    head_time = _commit_time(source_checkout, head)
    if head_time is not None and current["time"] is not None and current["time"] != head_time:
        raise MirrorRefused(f"published scorecard head {head} records commit time "
                            f"{current['time']}, but the commit says {head_time}")
    if head == target:
        return {**order, "state": "same", "current_time": head_time, "basis": "published head is this commit"}
    if head_time is not None:
        order["current_time"] = head_time
        if _is_ancestor(source_checkout, head, target):
            return {**order, "state": "advance", "basis": "published head is an ancestor"}
        if _is_ancestor(source_checkout, target, head):
            return {**order, "state": "superseded", "basis": "this commit is an ancestor of the published head"}
        basis = "divergent history; committer time"
    elif current["time"] is not None:
        # An unreadable head cannot be an ancestor of a readable target in a
        # complete repository, so only the recorded time can order it.
        head_time = current["time"]
        basis = "published head unreadable here; recorded committer time"
    else:
        raise MirrorRefused(
            f"published scorecard head {head} is not in {source_checkout} and latest.json records "
            f"no {SCORECARD_HEAD_TIME_FIELD}; refusing to move the scorecard head without an order")
    if target_time > head_time:
        return {**order, "state": "advance", "basis": basis}
    return {**order, "state": "superseded", "basis": basis}


def write_metadata(repo: Path, snapshot: Mapping[str, Any], target: str,
                   series_commit: str, scorecard: Mapping[str, str], tool_commit: str,
                   order: Mapping[str, Any]) -> None:
    """Move latest.json to TARGET only when scorecard_order proved it is newer."""
    if order.get("state") not in {"first", "same", "advance"} or order.get("hermit_commit") != target:
        raise MirrorRefused(f"latest.json may not move to {target}: scorecard head order is "
                            f"{order.get('state')!r} against {order.get('current')}")
    current = published_scorecard_head(repo)
    if (current and current["commit"]) != order.get("current"):
        raise MirrorRefused("published scorecard head changed after it was ordered")
    source = snapshot["source"]
    value = {
        "schema": METADATA_SCHEMA, "source_repository": source["repository"],
        "source_commit": source["commit"], "source_tree": source["tree"],
        "rows": snapshot["rows_read"], "rows_sha256": snapshot["rows_sha256"],
        "local_sources": source["local_sources"], "series_commit": series_commit,
        "scorecard_hermit_commit": target, "scorecard": dict(sorted(scorecard.items())),
        "scorecard_tool_hermit_commit": tool_commit,
        SCORECARD_HEAD_TIME_FIELD: order["hermit_commit_time"],
    }
    (repo / "latest.json").write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def _new_proof_paths(changes: Mapping[str, str], hashes: Mapping[str, str],
                     ignore: frozenset[str] = frozenset()) -> list[str]:
    """Proof attachment may only add the exact new files stage_carrier wrote."""
    paths = []
    for path, status in changes.items():
        if path in ignore:
            continue
        if status != "??" or path not in hashes or not path.startswith(finalized_proof.PREFIX + "/"):
            raise MirrorRefused(f"proof attachment changed an unexpected path: {path} ({status.strip()})")
        paths.append(path)
    return sorted(paths)


def unbound_proof_binding(writer: Path, snapshot: Mapping[str, Any], portable: Mapping[str, Any],
                          head: Mapping[str, Any] | None) -> dict[str, Any]:
    """Admit a proof-only attachment for a run whose scorecard never published.

    Only a commit-anchored, clean census terminal qualifies. The fetched
    ledger may hold no other carrier for its terminal event or its run. A
    carrier for this exact terminal (same repository, run, record, event and
    event digest) is a retry: it is named in ``retry_of``, and the attachment
    must then reproduce that carrier's path and every file byte for byte
    (stage_carrier refuses changed operands), so it reports unchanged.
    """
    row = portable["selection"]["row"]
    terminal = portable["selection"]["terminal"]
    if row.get("commit_anchored") is not True or row.get("tree_dirty") is not False:
        raise MirrorRefused("unbound proof-only attachment requires a commit-anchored run of a clean tree")
    commit = snapshot["source"]["commit"]
    identity = finalized_proof.carrier_identity({"terminal": terminal})
    retry_of = None
    for key, value in finalized_proof.read_carriers(writer, commit).items():
        if (value["terminal"]["event_id"] != terminal["event_id"]
                and value["terminal"]["run_id"] != terminal["run_id"]):
            continue
        if key != identity:
            raise MirrorRefused(
                f"ledger {commit} already carries a finalized proof for run {value['terminal']['run_id']} "
                f"(event {value['terminal']['event_id']}); refusing a second carrier for run "
                f"{terminal['run_id']} (event {terminal['event_id']})")
        retry_of = finalized_proof.carrier_path(value)
    return {"state": "unbound", "latest_hermit_commit": head and head["commit"], "commit": commit,
            "retry_of": retry_of}


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
            refuse_non_cargo_payload(finalized_row)
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
            publication = publish_series(parent, ledger, writer, net_wrapper=net_wrapper, attempts=attempts,
                                         run_id=finalized_run_id)
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
        if portable_proof_only:
            _attach_proof_only(report, parent=parent, writer=writer, snapshot=snapshot, target=target,
                               portable=portable, preparation_error=preparation_error,
                               results=results, tool_checkout=tool_checkout, net_wrapper=net_wrapper)
            return report
        _publish_scorecard(
            report, parent=parent, writer=writer, snapshot=snapshot, lock_fd=lock_fd,
            source_checkout=source_checkout, target=target, portable=portable,
            preparation_error=preparation_error, tool_preparation_error=tool_preparation_error,
            prepared_tool=prepared_tool, finalized_row=finalized_row, results=results,
            scorecard_command=scorecard_command, scorecard_args=scorecard_args,
            tool_checkout=tool_checkout, finalized_run_id=finalized_run_id, net_wrapper=net_wrapper)
        return report


PUBLICATION_ERRORS = (MirrorRefused, OSError, ValueError, KeyError, series.SeriesError)


def _attach_proof_only(report: dict[str, Any], *, parent: Path, writer: Path,
                       snapshot: Mapping[str, Any], target: str | None, portable: dict | None,
                       preparation_error: BaseException | None, results: Path | None,
                       tool_checkout: Path | None, net_wrapper: str) -> None:
    """Attach a derived census carrier; never touch the scorecard or latest.json.

    A run whose published scorecard names it keeps the strict binding to that
    scorecard. A run whose scorecard never published (it failed, or a newer
    head superseded it) attaches unbound, under unbound_proof_binding's guards.
    """
    try:
        if preparation_error is not None:
            raise MirrorRefused(f"finalized proof preparation failed: {preparation_error}")
        if target is None:
            raise MirrorRefused("proof-only attachment requires the measured target")
        if (portable is None or portable.get("derived") is not True
                or portable["selection"]["row"].get("schema_version") != 5):
            raise MirrorRefused("proof-only attachment requires an original schema-5 census terminal")
        head = published_scorecard_head(writer)
        bound = head is not None and head["commit"] == target
        binding = (published_scorecard_binding(writer, snapshot, target) if bound
                   else unbound_proof_binding(writer, snapshot, portable, head))
        report["existing_scorecard"] = binding
        tool_commit = emit_derived_portable_proof(portable, parent, results, tool_checkout,
                                                  net_wrapper=net_wrapper)
        hashes = finish_portable_proof(portable, parent, writer, snapshot, tool_commit,
                                       net_wrapper=net_wrapper)
        again = (published_scorecard_binding(writer, snapshot, target) if bound
                 else unbound_proof_binding(writer, snapshot, portable, published_scorecard_head(writer)))
        if again != binding:
            raise MirrorRefused("published scorecard changed during proof-only attachment")
        paths = _new_proof_paths(_worktree_changes(writer), hashes)
        if not bound and binding["retry_of"] is not None and (paths or binding["retry_of"] not in hashes):
            raise MirrorRefused(
                f"retry of published carrier {binding['retry_of']} would add "
                + (", ".join(paths) or "a different carrier") + "; the published carrier is incomplete")
        commit = _commit_and_push(writer, paths, "Attach verified finalized comparison operands",
                                  net_wrapper=net_wrapper, base=snapshot["source"]["commit"])
        if commit is not None:
            report["commit"] = commit
            report["state"] = "published"
        report["portable_proof"] = {"state": "published" if commit else "unchanged",
                                    "commit": report["commit"], "files": hashes,
                                    "hermit_commit": target, "tool_hermit_commit": tool_commit}
        if not bound:
            print(f"ledger publication: FINALIZED PROOF {report['portable_proof']['state'].upper()} "
                  f"for {target} without a scorecard: latest.json and SCORECARD.md stay at "
                  f"{binding['latest_hermit_commit']}", file=sys.stderr)
    except PUBLICATION_ERRORS as error:
        report["state"] = "derived-failed"
        report["portable_proof"] = {"state": "failed", "error": str(error)}


def _publish_scorecard(report: dict[str, Any], *, parent: Path, writer: Path,
                       snapshot: Mapping[str, Any], lock_fd: int, source_checkout: Path | None,
                       target: str | None, portable: dict | None,
                       preparation_error: BaseException | None,
                       tool_preparation_error: BaseException | None,
                       prepared_tool: tuple[str, str] | None, finalized_row: bytes | None,
                       results: Path | None, scorecard_command: str | None,
                       scorecard_args: Sequence[str], tool_checkout: Path | None,
                       finalized_run_id: str | None, net_wrapper: str) -> None:
    """Regenerate the scorecard, and publish a derived proof whatever it does.

    Outcomes, all recorded in REPORT:
    - scorecard regenerated: scorecard files, latest.json and any carrier
      publish in one commit (a proof failure withholds all of them, as before);
    - scorecard failed, including a head order that cannot be decided (a
      shallow checkout, an unreadable head with no recorded time, a time
      mismatch): a derived census carrier still publishes alone, and the state
      stays derived-failed with both facts recorded separately;
    - scorecard superseded by a newer published head: rows and any derived
      carrier publish, latest.json and SCORECARD.md do not move.
    A schema-10 carrier depends on the scorecard's own native verification,
    so it publishes only with a regenerated scorecard.
    """
    legacy_export = scorecard_command == "export-legacy"
    operation = "legacy_export" if legacy_export else "scorecard"
    series_commit = snapshot["source"]["commit"]
    derived = portable is not None and portable.get("derived") is True
    # Without a prepared proof or a measured identity there is nothing to
    # attach; every other refusal below still reaches the derived carrier.
    try:
        if preparation_error is not None:
            raise MirrorRefused(f"finalized proof preparation failed: {preparation_error}")
        if source_checkout is None or target is None:
            raise MirrorRefused("scorecard generation requires source checkout and target")
    except PUBLICATION_ERRORS as error:
        report["state"] = "derived-failed"
        report[operation] = {"state": "failed", "error": str(error)}
        return
    order = None
    scorecard_error = None
    if not legacy_export:
        try:
            order = scorecard_order(writer, source_checkout, target)
        except PUBLICATION_ERRORS as error:
            # An unknown order withholds the scorecard, not the proof.
            scorecard_error = MirrorRefused(f"scorecard head order cannot be decided: {error}")
    superseded = order is not None and order["state"] == "superseded"
    hashes: dict[str, str] = {}
    tool_commit = None
    if not superseded and scorecard_error is None:
        try:
            if tool_preparation_error is not None:
                raise MirrorRefused(f"scorecard tool preparation failed: {tool_preparation_error}")
            hashes, tool_commit = build_scorecard(
                source_checkout, target, snapshot, writer, lock_fd=lock_fd,
                net_wrapper=net_wrapper, results=results,
                scorecard_command=scorecard_command, scorecard_args=scorecard_args,
                tool_checkout=tool_checkout, finalized_run_id=finalized_run_id,
                state_root=parent, prepared_finalized_row=finalized_row,
                prepared_tool=prepared_tool,
            )
        except PUBLICATION_ERRORS as error:
            scorecard_error = error
    if scorecard_error is None and not superseded:
        try:
            if portable is not None:
                proof_tool = (emit_derived_portable_proof(portable, parent, results, None,
                                                          net_wrapper=net_wrapper)
                              if derived else tool_commit)
                hashes.update(finish_portable_proof(
                    portable, parent, writer, snapshot, proof_tool, net_wrapper=net_wrapper))
            if not legacy_export:
                write_metadata(writer, snapshot, target, series_commit, hashes, tool_commit, order)
            changes = _worktree_changes(writer)
            for path in changes:
                allowed = path in hashes or (not legacy_export and path == "latest.json")
                if not allowed:
                    raise MirrorRefused(f"scorecard changed an unexpected path: {path}")
            message = ("Preserve exact legacy Hermit cell history" if legacy_export
                       else "Generate scorecard from published cell history")
            commit = _commit_and_push(writer, sorted(changes), message, net_wrapper=net_wrapper,
                                      base=series_commit)
            if commit is not None:
                report["commit"] = commit
                report["state"] = "published"
            report[operation] = {"state": "published", "files": hashes,
                                 "hermit_commit": target, "tool_hermit_commit": tool_commit}
        except PUBLICATION_ERRORS as error:
            report["state"] = "derived-failed"
            report[operation] = {"state": "failed", "error": str(error)}
        return
    # No scorecard publishes from here on: it failed, or a newer head owns it.
    if superseded:
        detail = (f"published scorecard head {order['current']} is newer than {target} "
                  f"({order['basis']}); latest.json and SCORECARD.md were left unchanged")
        report[operation] = {"state": "superseded", "hermit_commit": target,
                             "current_hermit_commit": order["current"], "detail": detail}
        print(f"ledger publication: SCORECARD SUPERSEDED: {detail}", file=sys.stderr)
    else:
        report["state"] = "derived-failed"
        report[operation] = {"state": "failed", "error": str(scorecard_error)}
    if portable is None:
        return
    if not derived:
        # The schema-10 carrier's operands are verified only by the scorecard
        # transaction that did not run to completion here.
        reason = (f"superseded by newer scorecard head {order['current']}" if superseded
                  else f"scorecard failed: {scorecard_error}")
        report["state"] = "derived-failed"
        report[operation] = {"state": "failed", "error":
                             f"schema-10 finalized proof requires the scorecard transaction; {reason}"}
        return
    try:
        # The carrier is computed from the published series commit; a writer
        # that has moved (for instance a renderer that committed in it) is
        # retained and attaches nothing.
        head = _git(writer, "rev-parse", "HEAD^{commit}")
        if head != series_commit:
            raise MirrorRefused(f"ledger writer HEAD {head} is not the published series commit "
                                f"{series_commit}; no finalized proof attached; writer retained for recovery")
        # Whatever a failed renderer left behind stays unstaged and retained;
        # none of it may sit where the carrier is written.
        leftover = frozenset(_worktree_changes(writer))
        if any(path.startswith(finalized_proof.PREFIX + "/") for path in leftover):
            raise MirrorRefused("the failed scorecard left changes under the finalized proof directory: "
                                + ", ".join(sorted(path for path in leftover
                                                   if path.startswith(finalized_proof.PREFIX + "/"))))
        proof_tool = emit_derived_portable_proof(portable, parent, results, None, net_wrapper=net_wrapper)
        files = finish_portable_proof(portable, parent, writer, snapshot, proof_tool,
                                      net_wrapper=net_wrapper)
        paths = _new_proof_paths(_worktree_changes(writer), files, ignore=leftover)
        message = ("Attach verified finalized comparison operands; scorecard superseded" if superseded
                   else "Attach verified finalized comparison operands; scorecard generation failed")
        commit = _commit_and_push(writer, paths, message, net_wrapper=net_wrapper, base=series_commit)
        if commit is not None:
            report["commit"] = commit
            if superseded:
                report["state"] = "published"
        report["portable_proof"] = {"state": "published" if commit else "unchanged",
                                    "commit": report["commit"], "files": files,
                                    "hermit_commit": target, "tool_hermit_commit": proof_tool}
        print(f"ledger publication: FINALIZED PROOF {report['portable_proof']['state'].upper()} "
              f"at {report['commit']}: {', '.join(path for path in sorted(files) if '/runs/' in path)}",
              file=sys.stderr)
    except PUBLICATION_ERRORS as error:
        report["state"] = "derived-failed"
        report["portable_proof"] = {"state": "failed", "error": str(error)}
        if superseded:
            report[operation] = {"state": "failed", "error":
                                 f"finalized proof failed while the scorecard was superseded by "
                                 f"{order['current']}: {error}"}
    if not superseded:
        print(f"ledger publication: SCORECARD FAILED: {scorecard_error}; finalized proof "
              f"{report['portable_proof']['state']}", file=sys.stderr)


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


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parent", type=Path, required=True)
    parser.add_argument("--source-checkout", type=Path)
    parser.add_argument("--tool-checkout", type=Path, default=None,
                        help="scorecard renderer checkout; defaults to --source-checkout")
    parser.add_argument("--target")
    parser.add_argument("--results", type=Path)
    parser.add_argument("--finalized-run-id", help="canonical completed validation run; row and proof are read from STATE")
    parser.add_argument("--publish-only", action="store_true")
    parser.add_argument("--portable-proof-only", action="store_true",
                        help="attach newly verified public census operands without regenerating the scorecard; "
                             "a run the published scorecard names stays bound to it, and any other run must be "
                             "commit-anchored, clean and not yet carried; scorecard files, latest.json and the "
                             "original validation result are preserved")
    parser.add_argument("--scorecard-command", choices=SCORECARD_COMMANDS)
    parser.add_argument("--scorecard-arg", action="append", default=[])
    parser.add_argument("--net-wrapper", default=os.environ.get("WITH_PROXY", "with-proxy"))
    parser.add_argument("--attempts", type=int, default=DEFAULT_ATTEMPTS)
    parser.add_argument("--lifecycle-deadline-ns", type=int,
                        help="absolute monotonic caller deadline; capped at the existing 1800-second lifecycle ceiling")
    parser.add_argument("--bounded-worker", action="store_true", help=argparse.SUPPRESS)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    started_ns = time.monotonic_ns()
    actual_argv = list(sys.argv[1:] if argv is None else argv)
    parser = build_parser()
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
