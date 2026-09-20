#!/usr/bin/env python3
"""Deterministic global union and green/timeline/bisect queries over the
per-team/machine validation ledger.

Schema: `ai_docs/validate-ledger-team-machine-schema_20260807.md`; the section
references below (§2, §5, §7 …) point at it, and
`ci-hub/ledger/tests/test_ledger_invariants.py` is the executable spec.

THE SHARDS ARE THE AUTHORITY; THIS MODULE IS A VIEW.
Every function here reads append-only shard files and derives something. Nothing
rewrites a shard, and nothing here is a place to store a fact — if a derived
answer and a shard disagree, the shard is right and the derivation is wrong.
That is why `fold` never mutates its inputs and why `run.correct` keeps the
superseded event rather than replacing it.

TWO SEPARATE CONCERNS, DELIBERATELY NOT FUSED:

* `fold` computes the materialized view. It applies every enrichment it is
  given, last-writer-wins per field, because the view must reflect the shard
  contents exactly as they are.
* `validate_event` judges whether an event SHOULD have been written. It is the
  thing that refuses an enrich which overwrites a set value.

Fusing them would mean a policy-violating line silently changes the view (if
fold enforced nothing) or vanishes from it (if fold dropped violations) — and a
line that exists in the shard but not in the view is exactly the data loss the
append-only design exists to prevent. So a bad line is VISIBLE in the view and
FLAGGED by the linter.
"""

from __future__ import annotations

from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path
from typing import Any, Callable, Iterable, Sequence

ENVELOPE_VERSION = "validate-ledger/v1"

#: Envelope keys every event must carry (§2).
ENVELOPE_KEYS = (
    "schema", "event_id", "event_type", "emitted_at", "team", "host", "run_id",
    "producer",
)

#: Outcomes that are not a verdict about the commit (§10). A run that did not
#: produce a result must never be read as a failure — that is how a flaky
#: infrastructure hiccup gets recorded as a product regression.
NON_VERDICT_OUTCOMES = frozenset({"no_result", "timeout", "incomplete"})

#: Provenance that cannot establish green (§6). Reconstructed rows were inferred
#: after the fact rather than observed, so they may assert a pass nobody saw.
NON_GREEN_SOURCES = frozenset({"reconstructed"})

_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
# Built rather than written literally so this module does not itself contain the
# owner-path needle it rejects (check-portability greps tracked files).
_OWNER_PATH_NEEDLE = "/" + "home" + "/"


class LedgerError(Exception):
    """Base for every ledger fault."""


class MalformedEvent(LedgerError):
    """A line is not a complete, parseable event.

    Raised rather than skipped: a silently dropped line is indistinguishable
    from a line that was never written, which turns data loss into a clean bill
    of health (§7 lint 4).
    """


class ConflictingEventId(LedgerError):
    """One event_id carries two different bodies.

    Never resolved by last-writer-wins: the ids are supposed to be unique, so a
    conflict means two producers disagree about history and a human must say
    which is right (§5.2).
    """


class SharedTypeCheckerUnavailable(LedgerError):
    """The exact Rust event checker could not be materialized safely."""


_SHARED_TYPE_CHECKER_RELATIVE = Path("ci-hub/ledger/check-ledger-events.rs")
_SHARED_TYPE_SOURCE_RELATIVES = (
    _SHARED_TYPE_CHECKER_RELATIVE,
    Path("ci-hub/lib/ledger_event.rs"),
)
_SHARED_TYPE_CACHE_MODES = {
    _SHARED_TYPE_CHECKER_RELATIVE: (b"100755", 0o555),
    Path("ci-hub/lib/ledger_event.rs"): (b"100644", 0o444),
}
_SHARED_TYPE_CACHE_VERSION = "v1"
_SHARED_TYPE_CACHE_KEEP = 2


def _stable_checker_cache_root() -> Path:
    configured = os.environ.get("CI_HUB_LEDGER_EVENT_CHECKER_CACHE")
    if configured:
        if os.environ.get("DEV_HERMIT_TOOL_AUTHORITY"):
            raise SharedTypeCheckerUnavailable(
                "fixture checker cache override is forbidden in immutable tool runs"
            )
        root = Path(configured)
    else:
        tool_cache = os.environ.get("DEV_HERMIT_TOOL_CACHE")
        configured_base = os.environ.get("XDG_CACHE_HOME")
        if tool_cache:
            root = Path(tool_cache) / "checker-sources"
        elif configured_base:
            root = Path(configured_base) / "dev-hermit" / "ledger-event-checker"
        else:
            home = os.environ.get("HOME")
            if not home:
                raise SharedTypeCheckerUnavailable(
                    "HOME and XDG_CACHE_HOME are both absent"
                )
            root = Path(home) / ".cache" / "dev-hermit" / "ledger-event-checker"
    if not root.is_absolute():
        raise SharedTypeCheckerUnavailable(
            f"checker cache root must be absolute: {root}"
        )
    return root


def _stable_checker_source_bytes(source_root: Path) -> dict[Path, bytes]:
    sources: dict[Path, bytes] = {}
    for relative in _SHARED_TYPE_SOURCE_RELATIVES:
        path = source_root / relative
        try:
            before = path.lstat()
            if not stat.S_ISREG(before.st_mode):
                raise SharedTypeCheckerUnavailable(
                    f"checker source is not a regular file: {path}"
                )
            executable = bool(stat.S_IMODE(before.st_mode) & 0o111)
            expected_executable = relative == _SHARED_TYPE_CHECKER_RELATIVE
            if executable != expected_executable:
                raise SharedTypeCheckerUnavailable(
                    f"checker source mode disagrees with tracked semantics: {path}"
                )
            if before.st_nlink != 1:
                raise SharedTypeCheckerUnavailable(
                    f"checker source link count is not one: {path}"
                )
            data = path.read_bytes()
            after = path.lstat()
        except SharedTypeCheckerUnavailable:
            raise
        except OSError as error:
            raise SharedTypeCheckerUnavailable(
                f"cannot read checker source {path}: {error}"
            ) from error
        identity_before = (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_mode,
            before.st_nlink,
        )
        identity_after = (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_mode,
            after.st_nlink,
        )
        if identity_before != identity_after or len(data) != after.st_size:
            raise SharedTypeCheckerUnavailable(
                f"checker source changed while it was read: {path}"
            )
        sources[relative] = data
    if not sources[_SHARED_TYPE_CHECKER_RELATIVE].startswith(
        b"#!/usr/bin/env -S rust-script --force\n"
    ):
        raise SharedTypeCheckerUnavailable(
            "checker source no longer carries its required rust-script --force boundary"
        )
    return sources


def _stable_checker_digest(sources: dict[Path, bytes]) -> str:
    digest = hashlib.sha256()
    digest.update(b"dev-hermit-ledger-event-checker-v1\0")
    for relative in _SHARED_TYPE_SOURCE_RELATIVES:
        name = relative.as_posix().encode("utf-8")
        data = sources[relative]
        tracked_mode, _cache_mode = _SHARED_TYPE_CACHE_MODES[relative]
        digest.update(len(name).to_bytes(4, "big"))
        digest.update(name)
        digest.update(tracked_mode)
        digest.update(len(data).to_bytes(8, "big"))
        digest.update(data)
    return digest.hexdigest()


def _checker_expected_directories() -> dict[Path, set[str]]:
    expected: dict[Path, set[str]] = {Path(): set()}
    for relative in _SHARED_TYPE_SOURCE_RELATIVES:
        parent = Path()
        for part in relative.parts[:-1]:
            expected.setdefault(parent, set()).add(part)
            parent /= part
            expected.setdefault(parent, set())
        expected[parent].add(relative.name)
    return expected


def _verify_stable_checker_tree(
    target: Path, sources: dict[Path, bytes]
) -> Path:
    owner = os.geteuid()
    for relative, expected_children in _checker_expected_directories().items():
        directory = target / relative
        try:
            metadata = directory.lstat()
            children = {entry.name for entry in os.scandir(directory)}
        except OSError as error:
            raise SharedTypeCheckerUnavailable(
                f"cannot inspect cached checker directory {directory}: {error}"
            ) from error
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != owner
            or stat.S_IMODE(metadata.st_mode) != 0o555
        ):
            raise SharedTypeCheckerUnavailable(
                f"cached checker directory mode or owner is invalid: {directory}"
            )
        if children != expected_children:
            raise SharedTypeCheckerUnavailable(
                f"cached checker directory has unexpected entries: {directory}"
            )
    for relative, expected in sources.items():
        path = target / relative
        _tracked_mode, expected_mode = _SHARED_TYPE_CACHE_MODES[relative]
        try:
            before = path.lstat()
            if (
                not stat.S_ISREG(before.st_mode)
                or before.st_uid != owner
                or before.st_nlink != 1
                or stat.S_IMODE(before.st_mode) != expected_mode
            ):
                raise SharedTypeCheckerUnavailable(
                    f"cached checker source mode, owner, or link count is invalid: {path}"
                )
            actual = path.read_bytes()
            after = path.lstat()
        except SharedTypeCheckerUnavailable:
            raise
        except OSError as error:
            raise SharedTypeCheckerUnavailable(
                f"cannot inspect cached checker source {path}: {error}"
            ) from error
        if (
            (
                before.st_dev,
                before.st_ino,
                before.st_size,
                before.st_mtime_ns,
                before.st_mode,
                before.st_nlink,
            )
            != (
                after.st_dev,
                after.st_ino,
                after.st_size,
                after.st_mtime_ns,
                after.st_mode,
                after.st_nlink,
            )
            or len(actual) != after.st_size
        ):
            raise SharedTypeCheckerUnavailable(
                f"cached checker source changed while it was read: {path}"
            )
        if actual != expected:
            raise SharedTypeCheckerUnavailable(
                f"cached checker source differs from its content address: {path}"
            )
    return target / _SHARED_TYPE_CHECKER_RELATIVE


def _remove_checker_stage(stage: Path) -> None:
    if not stage.exists():
        return
    for directory, names, files in os.walk(stage):
        os.chmod(directory, 0o700)
        for name in (*names, *files):
            path = Path(directory) / name
            if not path.is_symlink():
                os.chmod(path, 0o700 if path.is_dir() else 0o600)
    shutil.rmtree(stage)


def _private_real_cache_directory(path: Path) -> None:
    if not path.is_absolute():
        raise SharedTypeCheckerUnavailable(
            f"checker cache directory must be absolute: {path}"
        )
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            try:
                current.mkdir(mode=0o700)
            except FileExistsError:
                pass
            try:
                metadata = current.lstat()
            except OSError as error:
                raise SharedTypeCheckerUnavailable(
                    f"cannot create checker cache directory {current}: {error}"
                ) from error
        except OSError as error:
            raise SharedTypeCheckerUnavailable(
                f"cannot inspect checker cache path {current}: {error}"
            ) from error
        if not stat.S_ISDIR(metadata.st_mode):
            raise SharedTypeCheckerUnavailable(
                f"checker cache path component is not a real directory: {current}"
            )
    metadata = path.lstat()
    if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) & 0o022:
        raise SharedTypeCheckerUnavailable(
            f"checker cache directory is not private and owned: {path}"
        )


@contextmanager
def _checker_cache_lock(version_root: Path):
    lock_path = version_root / ".materialize.lock"
    flags = os.O_RDWR | os.O_CREAT
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(lock_path, flags, 0o600)
        metadata = os.fstat(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or metadata.st_nlink != 1
            or stat.S_IMODE(metadata.st_mode) != 0o600
        ):
            raise SharedTypeCheckerUnavailable(
                f"checker cache lock mode, owner, or link count is invalid: {lock_path}"
            )
        fcntl.flock(descriptor, fcntl.LOCK_EX)
    except SharedTypeCheckerUnavailable:
        if "descriptor" in locals():
            os.close(descriptor)
        raise
    except OSError as error:
        if "descriptor" in locals():
            os.close(descriptor)
        raise SharedTypeCheckerUnavailable(
            f"cannot lock checker cache {lock_path}: {error}"
        ) from error
    try:
        yield
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


def _cleanup_abandoned_checker_stages(version_root: Path) -> None:
    pattern = re.compile(r"\.[0-9a-f]{64}\.stage\.[A-Za-z0-9_-]+")
    for entry in os.scandir(version_root):
        if pattern.fullmatch(entry.name) is None:
            continue
        metadata = entry.stat(follow_symlinks=False)
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid():
            raise SharedTypeCheckerUnavailable(
                f"abandoned checker stage is not an owned real directory: {entry.path}"
            )
        _remove_checker_stage(Path(entry.path))


def _retire_old_checker_targets(version_root: Path, current: Path) -> None:
    """Keep the active source identity plus one rollout predecessor."""
    target_pattern = re.compile(r"[0-9a-f]{64}")
    targets: list[tuple[int, str, Path]] = []
    for entry in os.scandir(version_root):
        if target_pattern.fullmatch(entry.name) is None:
            continue
        metadata = entry.stat(follow_symlinks=False)
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid():
            raise SharedTypeCheckerUnavailable(
                f"cached checker target is not an owned real directory: {entry.path}"
            )
        targets.append((metadata.st_mtime_ns, entry.name, Path(entry.path)))
    if current.name not in {name for _mtime, name, _path in targets}:
        raise SharedTypeCheckerUnavailable(
            f"current cached checker target disappeared: {current}"
        )
    keep = {current.name}
    for _mtime, name, _path in sorted(targets, reverse=True):
        keep.add(name)
        if len(keep) >= _SHARED_TYPE_CACHE_KEEP:
            break
    for _mtime, name, path in targets:
        if name not in keep:
            _remove_checker_stage(path)


def _materialize_checker_under_lock(
    version_root: Path, sources: dict[Path, bytes]
) -> Path:
    content_digest = _stable_checker_digest(sources)
    target = version_root / content_digest
    stage: Path | None = None
    try:
        try:
            target.lstat()
        except FileNotFoundError:
            stage = Path(
                tempfile.mkdtemp(prefix=f".{content_digest}.stage.", dir=version_root)
            )
            for relative, data in sources.items():
                destination = stage / relative
                destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                with destination.open("xb") as handle:
                    handle.write(data)
                    handle.flush()
                    os.fsync(handle.fileno())
                _tracked_mode, cache_mode = _SHARED_TYPE_CACHE_MODES[relative]
                destination.chmod(cache_mode)
            directories = {stage}
            for relative in _SHARED_TYPE_SOURCE_RELATIVES:
                current = (stage / relative).parent
                while current != stage:
                    directories.add(current)
                    current = current.parent
            for directory in sorted(
                directories, key=lambda item: len(item.parts), reverse=True
            ):
                directory.chmod(0o555)
            try:
                os.rename(stage, target)
                stage = None
            except OSError:
                # A concurrent creator can win the no-replacement directory
                # rename. Its bytes still have to pass the same exact check.
                target.lstat()
        return _verify_stable_checker_tree(target, sources)
    except SharedTypeCheckerUnavailable:
        raise
    except OSError as error:
        raise SharedTypeCheckerUnavailable(
            f"cannot materialize content-addressed checker {target}: {error}"
        ) from error
    finally:
        if stage is not None:
            _remove_checker_stage(stage)


@contextmanager
def _locked_materialized_shared_type_checker(
    *, source_root: Path | None = None, cache_root: Path | None = None
):
    """Yield the exact checker under a source-content-addressed cache lock.

    Health ticks execute from short-lived ``.run.*`` roots. rust-script 0.36
    keys its generated package by the script's absolute path, and ``--force``
    then refreshes its path dependency. Measured on devbig014 (2026-09-20), a
    fresh random path cost 1.24s wall / 1.82s CPU while the unchanged path cost
    0.03s wall / 0.03s CPU. The two producer-owned Rust sources instead live at
    a stable path keyed by their bytes and normalized tracked modes.

    ``--force`` remains load-bearing. The cache is checked before and after
    execution, and a mismatch is refused rather than repaired or hidden behind
    a compiled binary. The lock covers cooperating writers. As with the shared
    Cargo cache, a deliberately hostile same-uid process can still perform an
    ABA replacement; same-uid processes are inside this host's cache boundary.
    """
    source_root = (source_root or Path(__file__).resolve().parents[2]).resolve()
    cache_root = cache_root or _stable_checker_cache_root()
    if not cache_root.is_absolute():
        raise SharedTypeCheckerUnavailable(
            f"checker cache root must be absolute: {cache_root}"
        )
    _private_real_cache_directory(cache_root)
    version_root = cache_root / _SHARED_TYPE_CACHE_VERSION
    _private_real_cache_directory(version_root)
    with _checker_cache_lock(version_root):
        _cleanup_abandoned_checker_stages(version_root)
        sources = _stable_checker_source_bytes(source_root)
        checker = _materialize_checker_under_lock(version_root, sources)
        # The operational tool cache retains current+previous parent trees for
        # the same rollout race. Mirror that bound for tiny source aliases;
        # rust-script's generated packages have their separate 64-GiB cleanup.
        _retire_old_checker_targets(version_root, checker.parents[2])
        try:
            yield checker
        finally:
            if _stable_checker_source_bytes(source_root) != sources:
                raise SharedTypeCheckerUnavailable(
                    "checker source changed across cached execution"
                )
            if _verify_stable_checker_tree(checker.parents[2], sources) != checker:
                raise SharedTypeCheckerUnavailable(
                    "cached checker identity changed across execution"
                )


def _materialized_shared_type_checker(
    *, source_root: Path | None = None, cache_root: Path | None = None
) -> Path:
    with _locked_materialized_shared_type_checker(
        source_root=source_root, cache_root=cache_root
    ) as checker:
        return checker


# --------------------------------------------------------------------- paths


def shard_path(team: str, host: str, when: Any) -> str:
    """`ledger/<team>/<host>/<YYYY>-<MM>.jsonl` (§1)."""
    return f"ledger/{team}/{host}/{_month_of(when)}.jsonl"


def _month_of(when: Any) -> str:
    """Accept a `YYYY-MM`, an ISO timestamp, or anything with year/month."""
    if hasattr(when, "year") and hasattr(when, "month"):
        return f"{when.year:04d}-{when.month:02d}"
    text = str(when)
    if len(text) >= 7 and text[4] == "-":
        return text[:7]
    raise ValueError(f"cannot derive a YYYY-MM shard month from {when!r}")


def _shard_coordinates(path: Any) -> tuple[str | None, str | None, str | None]:
    parts = Path(str(path)).parts
    for root in ("ledger-recovery", "ledger"):
        if root not in parts:
            continue
        index = len(parts) - 1 - tuple(reversed(parts)).index(root)
        remaining = parts[index + 1:]
        if root == "ledger" and len(remaining) == 3:
            team, host, filename = remaining
            month = Path(filename).stem
        elif root == "ledger-recovery" and len(remaining) == 4:
            team, host, month, _filename = remaining
        else:
            continue
        return team, host, month if re.fullmatch(r"\d{4}-\d{2}", month) else None
    return None, None, None


def _shard_month(path: Any) -> str | None:
    return _shard_coordinates(path)[2]


def _shard_host(path: Any) -> str | None:
    return _shard_coordinates(path)[1]


def _shard_team(path: Any) -> str | None:
    return _shard_coordinates(path)[0]


# --------------------------------------------------------------------- parse


def parse_line(text: str, *, object_pairs_hook=None) -> dict:
    """Parse one JSONL line into an event, or raise `MalformedEvent`."""
    try:
        event = json.loads(text, object_pairs_hook=object_pairs_hook)
    except (json.JSONDecodeError, ValueError) as exc:
        raise MalformedEvent(f"line is not valid JSON: {exc}") from exc
    if not isinstance(event, dict):
        raise MalformedEvent(f"line is not a JSON object: {type(event).__name__}")
    return event


def read_shard(path: Any, *, object_pairs_hook=None) -> list[dict]:
    """Read one shard. A final line without its newline is TRUNCATED.

    A writer that died mid-append leaves a partial object; JSON would often
    still reject it, but not always (a cut inside a string can stay parseable),
    so the missing terminator is treated as the signal rather than relying on
    the parser to notice.
    """
    text = Path(path).read_text()
    if text and not text.endswith("\n"):
        raise MalformedEvent(
            f"{path}: final line has no newline terminator — the shard is "
            f"truncated (a writer died mid-append); refusing to parse it as whole"
        )
    return [parse_line(line, object_pairs_hook=object_pairs_hook)
            for line in text.splitlines() if line.strip()]


def _shared_type_codes(events: Sequence[dict], *, serializer=None) -> list[list[str]]:
    """Check envelope shape and closed values with the producer-owned type."""
    if not events:
        return []
    # The override is a fixture seam for isolated linked-worktree tests whose
    # deliberately minimal repository copy does not include this checker. An
    # immutable operational run has the complete source tree and must never let
    # inherited health.env content replace the producer-owned type.
    payload = "".join(
        (
            serializer(event)
            if serializer is not None
            else json.dumps(event, sort_keys=True, separators=(",", ":"))
        )
        + "\n"
        for event in events
    )

    def run(checker: Path):
        return subprocess.run(
            [str(checker)],
            input=payload,
            capture_output=True,
            text=True,
            timeout=300,
            check=False,
        )

    try:
        configured_checker = os.environ.get("CI_HUB_LEDGER_EVENT_CHECKER")
        if configured_checker:
            if os.environ.get("DEV_HERMIT_TOOL_AUTHORITY"):
                raise SharedTypeCheckerUnavailable(
                    "fixture checker override is forbidden in immutable tool runs"
                )
            result = run(Path(configured_checker))
        else:
            with _locked_materialized_shared_type_checker() as checker:
                result = run(checker)
    except (OSError, SharedTypeCheckerUnavailable, subprocess.TimeoutExpired) as error:
        return [[f"shared-ledger-event-check-unavailable: {error}"]] + [
            [] for _ in events[1:]
        ]
    if result.returncode == 0:
        return [[] for _ in events]

    detail = result.stderr.strip() or result.stdout.strip() or "no output"
    match = re.search(r"stdin line (\d+): (.*)", detail, re.DOTALL)
    if not match:
        return [[f"shared-ledger-event-check-refused: {detail}"]] + [
            [] for _ in events[1:]
        ]
    index = int(match.group(1)) - 1
    codes = [[] for _ in events]
    if 0 <= index < len(codes):
        codes[index].append(match.group(2).strip())
    else:
        codes[0].append(f"shared-ledger-event-check-refused: {detail}")
    return codes


def read_event_shard(path: Any) -> list[dict]:
    """Read and validate one validation-ledger shard, refusing any bad line."""
    events = read_shard(path)
    for index, codes in enumerate(validate_events(events, path), 1):
        if codes:
            raise MalformedEvent(f"{path}:{index}: event violates {codes}")
    return events


# --------------------------------------------------------------------- union


def _event_key(event: dict) -> tuple[str, str]:
    """The total order (§5.3): emitted_at, then event_id as the tiebreak.

    Event ids are ULIDs, so the tiebreak is itself time-ordered; two events in
    the same second still get a stable, reproducible order.
    """
    return (str(event.get("emitted_at", "")), str(event.get("event_id", "")))


def union_events(events: Iterable[dict], *, canonical=None) -> list[dict]:
    """Deterministically merge already-parsed events (§5).

    Order-independent by construction: the result is sorted by the total order,
    so shard order, file order and line order cannot influence the output.
    """
    canonical = canonical or _canonical
    by_id: dict[str, dict] = {}
    for event in events:
        event_id = str(event.get("event_id", ""))
        seen = by_id.get(event_id)
        if seen is None:
            by_id[event_id] = event
            continue
        if canonical(seen) != canonical(event):
            raise ConflictingEventId(
                f"event_id {event_id!r} appears twice with different bodies; "
                f"refusing last-writer-wins (§5.2)"
            )
        # Identical duplicate: the same event reached us by two paths. Collapse.
    return sorted(by_id.values(), key=_event_key)


def union(paths: Sequence[Any]) -> list[dict]:
    """Read every shard and merge deterministically (§5)."""
    events: list[dict] = []
    for path in paths:
        events.extend(read_event_shard(path))
    return union_events(events)


def _canonical(event: dict) -> str:
    return json.dumps(event, sort_keys=True)


# ---------------------------------------------------------------------- fold


def fold(events: Iterable[dict]) -> dict[str, dict]:
    """Materialize current run facts, keyed by run_id (§3, §9).

    `run_id` repeats by design — it is the join key, not a uniqueness key, so an
    enrichment chain of any depth collapses onto the one run it describes. The
    real ledger contains an eleven-row chain, so depth is never assumed to be 1.

    Unknown keys are carried through untouched (§8): a reader that drops a field
    it does not recognise silently destroys data written by a newer producer.
    """
    runs: dict[str, dict] = {}
    for event in sorted(events, key=_event_key):
        run_id = str(event.get("run_id", event.get("event_id", "")))
        etype = event.get("event_type", "run.result")
        current = runs.setdefault(run_id, {})
        if etype == "run.result":
            for key, value in event.items():
                if key not in ENVELOPE_KEYS or key in ("run_id", "host", "team"):
                    current.setdefault(key, value)
                else:
                    current.setdefault(key, value)
        elif etype == "run.enrich":
            # Apply unconditionally, last-writer-wins. Whether the enrich was
            # ALLOWED to set that field is validate_event's judgement, not the
            # view's; see the module docstring.
            for key, value in _payload(event).items():
                if value is not None:
                    current[key] = value
        elif etype == "run.correct":
            for key, value in _payload(event).items():
                current[key] = value
        else:
            for key, value in _payload(event).items():
                current.setdefault(key, value)
    return runs


def _payload(event: dict) -> dict:
    """The non-envelope, non-reference fields an event carries."""
    skip = set(ENVELOPE_KEYS) | {"enriches", "supersedes", "reason"}
    return {k: v for k, v in event.items() if k not in skip}


def timeline(events: Iterable[dict], run_id: str) -> list[dict]:
    """Every event about one run, in order — including superseded values (§10).

    Returns the events THEMSELVES, not the folded view, because the question the
    timeline answers is "what did we believe, and when". A correction that
    erased the value it replaced would make that unanswerable.
    """
    return [e for e in sorted(events, key=_event_key)
            if str(e.get("run_id", "")) == str(run_id)]


# ------------------------------------------------------------------- queries


def _latest_run_per_commit(runs: dict[str, dict]) -> dict[str, dict]:
    """The LATEST run for each commit (§10).

    Not "any run": an earlier pass followed by a later fail means the commit is
    not green. Taking any-pass would let a stale success outrank fresh evidence.
    """
    latest: dict[str, dict] = {}
    for run in runs.values():
        commit = run.get("commit")
        if not commit:
            continue
        seen = latest.get(commit)
        if seen is None or str(run.get("emitted_at", "")) >= str(seen.get("emitted_at", "")):
            latest[commit] = run
    return latest


def _is_green_run(run: dict, host_pred: Callable[[str], bool] | None) -> bool:
    """A green must carry what it verified: outcome, provenance, host class."""
    if run.get("outcome") != "pass":
        return False
    source = (run.get("producer") or {}).get("source")
    if source in NON_GREEN_SOURCES:
        return False
    if host_pred is not None and not host_pred(str(run.get("host", ""))):
        return False
    return True


def green(runs: dict[str, dict], host_pred: Callable[[str], bool] | None = None) -> str | None:
    """Newest commit whose LATEST run is an observed pass, else None (§10)."""
    candidates = [
        run for run in _latest_run_per_commit(runs).values()
        if _is_green_run(run, host_pred)
    ]
    if not candidates:
        return None
    newest = max(candidates, key=lambda r: str(r.get("emitted_at", "")))
    return newest.get("commit")


def newest_green(runs: dict[str, dict], host_pred=None) -> str | None:
    """Alias for :func:`green`, named for the CLI query it backs."""
    return green(runs, host_pred)


def bisect_verdict(runs: dict[str, dict], commit: str) -> str:
    """`pass` | `fail` | `unknown` | `nodata` for one commit (§10).

    `nodata` and `unknown` are deliberately distinct: "we never tested this
    commit" and "we tested it and learned nothing" lead a bisect to different
    next moves, and collapsing either into `fail` would blame a commit for an
    infrastructure failure.
    """
    run = _latest_run_per_commit(runs).get(commit)
    if run is None:
        return "nodata"
    outcome = run.get("outcome")
    if outcome in NON_VERDICT_OUTCOMES:
        return "unknown"
    if outcome == "pass":
        return "pass"
    if outcome == "fail":
        return "fail"
    return "unknown"


def commit_gaps(runs: dict[str, dict], commits: Sequence[str]) -> list[str]:
    """Which of `commits` have no run at all — the bisect-fill worklist."""
    have = set(_latest_run_per_commit(runs))
    return [c for c in commits if c not in have]


# ------------------------------------------------------------------ validate


def _contextual_event_codes(
    event: dict,
    shard: Any = None,
    *,
    prior: dict | None = None,
    known_ids: set[str] | None = None,
) -> list[str]:
    codes: list[str] = []

    # Retain the stable lint codes consumed by diagnostics. The shared Rust
    # type is the shape authority; these are names for two of its refusals.
    if event.get("schema") != ENVELOPE_VERSION:
        codes.append("unknown-envelope-version")

    host = str(event.get("host", ""))
    if "." in host:
        codes.append("fqdn-host")

    if shard is not None:
        shard_team = _shard_team(shard)
        team = str(event.get("team", ""))
        if shard_team and team and team != shard_team:
            codes.append("team-path-mismatch")
        shard_host = _shard_host(shard)
        if shard_host and host and host != shard_host:
            codes.append("host-path-mismatch")
        month = _shard_month(shard)
        emitted = str(event.get("emitted_at", ""))
        if month and emitted[:7] and emitted[:7] != month:
            # A past month's shard is frozen; a late line belongs in the shard
            # for the month it was emitted in, or its ordering becomes a lie.
            codes.append("frozen-shard-append")

    if "cwd" in event:
        codes.append("raw-cwd-forbidden")

    if _contains_owner_path(event):
        codes.append("owner-path")

    reference = event.get("enriches") or event.get("supersedes")
    if reference is not None and known_ids is not None and reference not in known_ids:
        codes.append("unresolvable-reference")

    if event.get("event_type") == "run.enrich" and prior is not None:
        for key, value in _payload(event).items():
            if value is None:
                continue
            existing = prior.get(key)
            if existing is not None and existing != value:
                codes.append("enrich-overwrites-set-value")
                break

    return codes


def validate_events(events: Sequence[dict], shard: Any = None, *, serializer=None) -> list[list[str]]:
    """Return every intrinsic and storage-context violation per event.

    The producer-owned Rust type owns the complete envelope, event-type,
    producer, and outcome contract. Python adds only facts that need the
    storage path or another event. Keeping the two responsibilities separate
    prevents a Python key list from becoming a second envelope definition.
    """
    items = list(events)
    shared_codes = _shared_type_codes(items, serializer=serializer)
    codes: list[list[str]] = []
    for index, event in enumerate(items):
        codes.append(_contextual_event_codes(event, shard) + shared_codes[index])
    return codes


def validate_event(
    event: dict,
    shard: Any = None,
    *,
    prior: dict | None = None,
    known_ids: set[str] | None = None,
) -> list[str]:
    """Return the §7 violation codes for one event; empty means clean.

    Returns codes rather than raising so a linter can report every problem with
    a line at once instead of stopping at the first.
    """
    codes = _contextual_event_codes(
        event,
        shard,
        prior=prior,
        known_ids=known_ids,
    )
    codes.extend(_shared_type_codes([event])[0])
    return codes


def _contains_owner_path(value: Any) -> bool:
    """An owner home path ANYWHERE in the event, at any nesting depth (§4.2)."""
    if isinstance(value, str):
        return _OWNER_PATH_NEEDLE in value
    if isinstance(value, dict):
        return any(_contains_owner_path(v) for v in value.values())
    if isinstance(value, (list, tuple)):
        return any(_contains_owner_path(v) for v in value)
    return False


# --------------------------------------------------------------- append-only


def verify_append_only(old_text: str, new_text: str) -> list[str]:
    """Compare two versions of a shard; empty means a legal pure append (§3).

    Line-wise rather than diff-wise: the only legal transition is "the old lines
    are still there, byte for byte, followed by new ones".
    """
    old_lines = old_text.splitlines()
    new_lines = new_text.splitlines()
    codes: list[str] = []
    if len(new_lines) < len(old_lines):
        codes.append("deleted-line")
    for index, old_line in enumerate(old_lines):
        if index >= len(new_lines):
            if "deleted-line" not in codes:
                codes.append("deleted-line")
            break
        if new_lines[index] != old_line:
            codes.append("rewritten-line")
            break
    return codes


def append_events(path: Any, events: Iterable[dict]) -> int:
    """Append whole lines to a shard, atomically per line.

    Concurrency: each event is serialized first and written with a single
    `os.write` in `O_APPEND` mode, so two producers on one shard interleave
    whole LINES and never tear one. Buffered text writes would not give that —
    a flush can land mid-object.
    """
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    payloads = [(json.dumps(e, sort_keys=True) + "\n").encode() for e in events]
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    try:
        for payload in payloads:
            os.write(fd, payload)
    finally:
        os.close(fd)
    return len(payloads)


# ------------------------------------------------------------------- legacy


_LEGACY_GROUP_KEYS = ("host", "started_at", "finished_at", "commit")


def migrate_legacy(
    rows: Sequence[dict],
    *,
    team: str,
    start_run_index: int = 0,
    start_legacy_index: int = 0,
) -> list[dict]:
    """One event per legacy row — nothing collapsed, nothing invented (§9).

    Rows sharing (host, started_at, finished_at, commit) are one run: the first
    becomes `run.result`, every later one becomes `run.enrich`. Never
    `run.correct`, because the real corpus was measured exhaustively and 0 of 37
    multi-row groups change an already-set value; emitting corrections would
    assert a conflict that does not exist.

    The source row is carried verbatim on the event so the migration is exactly
    reversible (`replay_legacy`). Reversibility is the property that makes the
    migration safe to run before anyone trusts the new format.

    `start_run_index` / `start_legacy_index` let an INCREMENTAL import continue a
    shard's existing numbering instead of restarting it. Both default to 0, so a
    whole-corpus migration is byte-for-byte what it always was. They exist
    because both ids are POSITIONAL: a second import that restarted at zero would
    mint `legacy-000000` again, and `union_events` deduplicates BY `event_id` —
    so the re-minted events would silently annihilate the already-published ones
    rather than collide visibly. `legacy_index` is offset for the same reason on
    the read side: `replay_legacy` orders by it, and duplicated indices would
    make the replayed order ambiguous.
    """
    groups: dict[tuple, list[int]] = {}
    for index, row in enumerate(rows):
        groups.setdefault(tuple(row.get(k) for k in _LEGACY_GROUP_KEYS), []).append(index)

    events: list[dict] = [None] * len(rows)  # type: ignore[list-item]
    for group_index, (_key, members) in enumerate(sorted(groups.items(), key=lambda kv: kv[1][0])):
        run_id = f"legacy-{start_run_index + group_index:06d}"
        previous_id = None
        for position, row_index in enumerate(members):
            row = rows[row_index]
            event_id = f"{run_id}-{position:03d}"
            event = {
                "schema": ENVELOPE_VERSION,
                "event_id": event_id,
                "event_type": "run.result" if position == 0 else "run.enrich",
                "emitted_at": _legacy_emitted_at(row, position),
                "team": team,
                "host": row.get("host"),
                "run_id": run_id,
                "producer": {"source": "imported", "tool": "ci-hub/ledger/migrate_legacy",
                             "tool_version": "1"},
                "commit": row.get("commit"),
                "outcome": row.get("result"),
                "legacy_row": row,
                "legacy_index": start_legacy_index + row_index,
            }
            if position:
                event["enriches"] = previous_id
            previous_id = event_id
            events[row_index] = event
    for index, codes in enumerate(validate_events(events), 1):
        if codes:
            raise MalformedEvent(
                f"migrated legacy event {index} violates {codes}"
            )
    return events


def _legacy_emitted_at(row: dict, position: int) -> str:
    """Order enrichments after the result they enrich.

    Legacy rows carry no emission time, only the run's own start/finish. Reusing
    finished_at for every member would make the chain order ambiguous under the
    (emitted_at, event_id) total order, so the position is folded into the id
    and the timestamp is kept stable.
    """
    return str(row.get("finished_at") or row.get("started_at") or "")


def replay_legacy(events: Sequence[dict]) -> list[dict]:
    """Reconstruct the original legacy rows exactly (§9, reversibility)."""
    ordered = sorted(
        (e for e in events if "legacy_row" in e),
        key=lambda e: e.get("legacy_index", 0),
    )
    return [e["legacy_row"] for e in ordered]
