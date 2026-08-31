#!/usr/bin/env python3
"""Build, validate, and atomically publish the Stage 1 compatibility website data tree."""

from __future__ import annotations

import argparse
import ctypes
import functools
import gzip
import hashlib
import importlib.util
import json
import os
import pwd
import stat
import sys
from collections import Counter
from collections.abc import Mapping, Sequence
from io import BytesIO
from pathlib import Path
from typing import Any


BUILD_DATA_PATH = Path(__file__).resolve().with_name("build_data.py")
SCHEMA_VERSION = 1
_RENAME_NOREPLACE, _RENAME_EXCHANGE = 1, 2
_DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
_FILE_FLAGS = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC
_DATA_PATHS = (
    "data/cells.json.gz", "data/incomplete-data.json.gz",
    "data/runs.json.gz", "data/tests.json.gz",
)
_DATA_NAMES = frozenset(Path(path).name for path in _DATA_PATHS)
_ROOT_NAMES = frozenset({"build.json", "data"})
_BUILD_FIELDS = frozenset({
    "artifacts", "artifacts_sha256", "build_data_sha256", "counts",
    "generator", "incomplete_data_categories", "provenance", "schema_version",
})
_GENERATOR_FIELDS = frozenset({
    "build_data_schema_version", "name", "write_data_schema_version",
})
_ARTIFACT_FIELDS = frozenset({
    "compressed_bytes", "compressed_sha256", "content_encoding",
    "content_type", "path", "uncompressed_bytes", "uncompressed_sha256",
})
_CHECKOUT_MARKERS = frozenset({".git", ".hg", ".sl", ".svn", ".eden"})
_SYSTEM_ROOTS = frozenset(
    Path("/") / name for name in
    (
        "bin", "boot", "dev", "etc", "home", "lib", "lib64", "opt",
        "proc", "root", "run", "sbin", "srv", "sys", "tmp", "usr", "var",
    )
)


class WriteRefused(Exception):
    """The complete artifact tree cannot be published safely."""


class RollbackRefused(WriteRefused):
    """The prior complete tree remains beside a failed replacement."""


@functools.cache
def _build_data_module() -> Any:
    spec = importlib.util.spec_from_file_location("compatibility_website_build_data_writer", BUILD_DATA_PATH)
    if spec is None or spec.loader is None:
        raise WriteRefused(f"cannot load {BUILD_DATA_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as error:
        sys.modules.pop(spec.name, None)
        raise WriteRefused(f"cannot load {BUILD_DATA_PATH}: {error}") from error
    return module


def _json_bytes(value: Any) -> bytes:
    try:
        return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    except (TypeError, ValueError, RecursionError) as error:
        raise WriteRefused(f"artifact value is not canonical JSON: {error}") from error


def _gzip_bytes(value: bytes) -> bytes:
    output = BytesIO()
    with gzip.GzipFile(filename="", mode="wb", compresslevel=6, fileobj=output, mtime=0) as stream:
        stream.write(value)
    encoded = output.getvalue()
    if encoded[:4] != b"\x1f\x8b\x08\x00" or encoded[4:8] != b"\0" * 4 or encoded[9] != 255:
        raise WriteRefused("gzip implementation did not produce fixed metadata")
    return encoded


def _gunzip_bytes(value: bytes, limit: int) -> bytes:
    if type(limit) is not int or not 0 <= limit <= 1 << 29:
        raise WriteRefused("existing output artifact size is invalid")
    with gzip.GzipFile(fileobj=BytesIO(value)) as source:
        decoded = source.read(limit + 1)
    if len(decoded) != limit:
        raise WriteRefused("existing output artifact size disagrees with build.json")
    return decoded


def _render_validated_tree(data: Mapping[str, Any]) -> dict[str, bytes]:
    files, artifacts = {}, []
    for path, payload in sorted({
        "data/cells.json.gz": {
            "cells": data["cells"],
            "in_manifest_cell_count": data["counts"]["in_manifest_cell_count"],
            "schema_version": SCHEMA_VERSION,
        },
        "data/incomplete-data.json.gz": {
            "incomplete_data": data["incomplete_data"],
            "incomplete_data_count": data["counts"]["incomplete_data_count"],
            "schema_version": SCHEMA_VERSION,
        },
        "data/runs.json.gz": data["runs"],
        "data/tests.json.gz": {
            "schema_version": SCHEMA_VERSION,
            "selected_by_full_custom_command_count": data["counts"]["selected_by_full_custom_command_count"],
            "selected_by_full_custom_commands": data["selected_by_full_custom_commands"],
            "test_count": data["counts"]["test_count"],
            "tests": data["tests"],
        },
    }.items()):
        uncompressed = _json_bytes(payload) + b"\n"
        compressed = _gzip_bytes(uncompressed)
        files[path] = compressed
        artifacts.append({
            "compressed_bytes": len(compressed),
            "compressed_sha256": hashlib.sha256(compressed).hexdigest(),
            "content_encoding": "gzip",
            "content_type": "application/json",
            "path": path,
            "uncompressed_bytes": len(uncompressed),
            "uncompressed_sha256": hashlib.sha256(uncompressed).hexdigest(),
        })
    categories = Counter(
        (row["input"], row["field"], row["reason"])
        for row in data["incomplete_data"]
    )
    build = {
        "artifacts": artifacts,
        "artifacts_sha256": hashlib.sha256(_json_bytes(artifacts)).hexdigest(),
        "build_data_sha256": hashlib.sha256(_json_bytes(data)).hexdigest(),
        "counts": data["counts"],
        "generator": {
            "build_data_schema_version": data["schema_version"],
            "name": "compatibility-website-stage-1-data",
            "write_data_schema_version": SCHEMA_VERSION,
        },
        "incomplete_data_categories": [
            {"count": count, "field": field, "input": source, "reason": reason}
            for (source, field, reason), count in sorted(categories.items())
        ],
        "provenance": data["provenance"],
        "schema_version": SCHEMA_VERSION,
    }
    files["build.json"] = _json_bytes(build) + b"\n"
    return dict(sorted(files.items()))


def render_tree(value: Any) -> dict[str, bytes]:
    module = _build_data_module()
    try:
        return _render_validated_tree(module.validate_build_data(value))
    except module.BuildRefused as error:
        raise WriteRefused(f"build data refused: {error}") from error


def _destination(value: Path) -> Path:
    raw = value.expanduser()
    if ".." in raw.parts:
        raise WriteRefused("output destination must not contain path traversal")
    destination = Path(os.path.abspath(raw))
    try:
        account_home = Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve()
        environment_home = Path.home().resolve()
    except (KeyError, OSError, RuntimeError) as error:
        raise WriteRefused(f"cannot identify protected home directories: {error}") from error
    protected = {
        Path(destination.anchor), account_home, environment_home,
        Path.cwd().resolve(), *_SYSTEM_ROOTS,
    }
    if destination in protected:
        raise WriteRefused("output destination must not be a filesystem, system, home, or working-directory root")
    try:
        parent = destination.parent.resolve(strict=True)
    except OSError as error:
        raise WriteRefused(f"output parent is unavailable: {error}") from error
    if parent != destination.parent:
        raise WriteRefused("output destination must not traverse a symlink")
    return destination


def _inventory(directory: int) -> frozenset[str]:
    try:
        os.lseek(directory, 0, os.SEEK_SET)
        return frozenset(os.listdir(directory))
    except OSError as error:
        raise WriteRefused(f"cannot inspect output inventory: {error}") from error


def _require_inventory(
    directory: int,
    expected: frozenset[str],
    label: str,
    *,
    allow_missing: bool = False,
) -> frozenset[str]:
    observed = _inventory(directory)
    valid = observed <= expected if allow_missing else observed == expected
    if not valid:
        qualifier = "contains unexpected entries" if allow_missing else "is not exact"
        raise WriteRefused(f"{label} inventory {qualifier}")
    return observed


def _open_regular(directory: int, name: str) -> int:
    before = _entry(directory, name)
    if before is None or not stat.S_ISREG(before.st_mode):
        raise WriteRefused(f"output entry is not a regular file: {name}")
    try:
        descriptor = os.open(name, _FILE_FLAGS, dir_fd=directory)
    except OSError as error:
        raise WriteRefused(f"cannot open output file {name}: {error}") from error
    try:
        metadata = os.fstat(descriptor)
        after = _entry(directory, name)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or not os.path.samestat(metadata, before)
            or after is None
            or not os.path.samestat(metadata, after)
        ):
            raise WriteRefused(f"output entry is not a stable regular file: {name}")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def _read_file(directory: int, name: str, limit: int) -> bytes:
    descriptor = _open_regular(directory, name)
    try:
        metadata = os.fstat(descriptor)
        if metadata.st_size > limit:
            raise WriteRefused(f"prior artifact file is not a bounded regular file: {name}")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            value = source.read(limit + 1)
    finally:
        os.close(descriptor)
    observed = _entry(directory, name)
    if (
        len(value) != metadata.st_size
        or observed is None
        or not os.path.samestat(metadata, observed)
    ):
        raise WriteRefused(f"prior artifact file changed while being read: {name}")
    return value


def _write_file(directory: int, name: str, value: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC
    descriptor = os.open(name, flags, 0o644, dir_fd=directory)
    with os.fdopen(descriptor, "wb") as output:
        output.write(value)
        output.flush()
        os.fsync(output.fileno())
    if _read_file(directory, name, len(value)) != value:
        raise WriteRefused(f"generated file changed before publication: {name}")


def _fsync_directory(path_or_fd: Path | int) -> None:
    descriptor = os.dup(path_or_fd) if isinstance(path_or_fd, int) else os.open(path_or_fd, _DIR_FLAGS)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _renameat2(directory: int, left: str, right: str, flags: int) -> None:
    renameat2 = getattr(ctypes.CDLL(None, use_errno=True), "renameat2", None)
    if renameat2 is None:
        raise WriteRefused("atomic directory rename is unavailable")
    renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    renameat2.restype = ctypes.c_int
    if renameat2(directory, os.fsencode(left), directory, os.fsencode(right), flags):
        raise WriteRefused(f"atomic directory rename failed: {os.strerror(ctypes.get_errno())}")


def _entry(directory: int, name: str) -> os.stat_result | None:
    try:
        return os.stat(name, dir_fd=directory, follow_symlinks=False)
    except FileNotFoundError:
        return None


def _entry_matches(directory: int, name: str, descriptor: int) -> bool:
    observed = _entry(directory, name)
    return observed is not None and os.path.samestat(os.fstat(descriptor), observed)


def _require_entry(directory: int, name: str, descriptor: int, message: str) -> None:
    if not _entry_matches(directory, name, descriptor):
        raise WriteRefused(message)


def _require_absent(directory: int, name: str, message: str) -> None:
    if _entry(directory, name) is not None:
        raise WriteRefused(message)


def _open_directory(directory: int, name: str | Path) -> int:
    descriptor = None
    try:
        descriptor = os.open(name, _DIR_FLAGS, dir_fd=directory)
        observed = _entry(directory, os.fspath(name))
        opened = os.fstat(descriptor)
    except OSError as error:
        if descriptor is not None:
            os.close(descriptor)
        raise WriteRefused(f"output directory is unavailable: {error}") from error
    if observed is None or not os.path.samestat(opened, observed):
        os.close(descriptor)
        raise WriteRefused("output directory changed while being opened")
    return descriptor


def _open_parent(path: Path) -> int:
    descriptor = os.open("/", _DIR_FLAGS)
    try:
        for part in path.parts[1:]:
            child = _open_directory(descriptor, part)
            os.close(descriptor)
            descriptor = child
        return descriptor
    except (OSError, WriteRefused):
        os.close(descriptor)
        raise


def _prior_manifest(raw: bytes) -> tuple[dict[str, Any], Mapping[str, Any], list[Any]]:
    build = json.loads(raw)
    generator, artifacts = build["generator"], build["artifacts"]
    if (
        set(build) != _BUILD_FIELDS
        or raw != _json_bytes(build) + b"\n"
        or build["schema_version"] != SCHEMA_VERSION
    ):
        raise WriteRefused("existing output is not a canonical compatibility artifact tree")
    if (
        set(generator) != _GENERATOR_FIELDS
        or generator["name"] != "compatibility-website-stage-1-data"
        or generator["write_data_schema_version"] != SCHEMA_VERSION
    ):
        raise WriteRefused("existing output was not written by this compatibility artifact writer")
    if (
        [row["path"] for row in artifacts] != list(_DATA_PATHS)
        or build["artifacts_sha256"]
        != hashlib.sha256(_json_bytes(artifacts)).hexdigest()
    ):
        raise WriteRefused("existing output artifact inventory is invalid")
    return build, generator, artifacts


def _prior_payloads(
    data: int,
    artifacts: Sequence[Mapping[str, Any]],
) -> tuple[dict[str, Any], dict[str, bytes]]:
    payloads, encoded_files = {}, {}
    for row in artifacts:
        if set(row) != _ARTIFACT_FIELDS:
            raise WriteRefused("existing output artifact metadata is invalid")
        size = row["compressed_bytes"]
        if type(size) is not int or not 0 <= size <= 1 << 28:
            raise WriteRefused("existing output artifact size is invalid")
        encoded = _read_file(data, Path(row["path"]).name, size)
        decoded = _gunzip_bytes(encoded, row["uncompressed_bytes"])
        if (
            row["content_encoding"] != "gzip"
            or row["content_type"] != "application/json"
            or len(encoded) != size
            or hashlib.sha256(encoded).hexdigest() != row["compressed_sha256"]
            or hashlib.sha256(decoded).hexdigest()
            != row["uncompressed_sha256"]
        ):
            raise WriteRefused("existing output artifact bytes disagree with build.json")
        value = json.loads(decoded)
        if decoded != _json_bytes(value) + b"\n":
            raise WriteRefused("existing output artifact is not canonical JSON")
        payloads[row["path"]] = value
        encoded_files[row["path"]] = encoded
    return payloads, encoded_files


def _recognize_prior(directory: int) -> dict[str, str]:
    root_names = _inventory(directory)
    if _CHECKOUT_MARKERS.intersection(root_names):
        raise WriteRefused("output destination is a checkout or workspace")
    _require_inventory(directory, _ROOT_NAMES, "existing output root")
    data = None
    try:
        raw = _read_file(directory, "build.json", 1 << 20)
        build, generator, artifacts = _prior_manifest(raw)
        data = _open_directory(directory, "data")
        _require_inventory(data, _DATA_NAMES, "existing output data")
        payloads, encoded_files = _prior_payloads(data, artifacts)
        source = {
            "cells": payloads[_DATA_PATHS[0]]["cells"], "counts": build["counts"],
            "incomplete_data": payloads[_DATA_PATHS[1]]["incomplete_data"],
            "provenance": build["provenance"], "runs": payloads[_DATA_PATHS[2]],
            "schema_version": generator["build_data_schema_version"],
            "selected_by_full_custom_commands": payloads[_DATA_PATHS[3]]["selected_by_full_custom_commands"],
            "tests": payloads[_DATA_PATHS[3]]["tests"],
        }
        if hashlib.sha256(_json_bytes(source)).hexdigest() != build["build_data_sha256"]:
            raise WriteRefused("existing output artifacts disagree with build-data digest")
        module = _build_data_module()
        try:
            validated = module.validate_build_data(source)
        except module.BuildRefused as error:
            raise WriteRefused(f"existing output build data is invalid: {error}") from error
        files = {"build.json": raw, **encoded_files}
        if _render_validated_tree(validated) != files:
            raise WriteRefused("existing output does not match its validated compatibility artifacts")
        _require_inventory(directory, _ROOT_NAMES, "existing output root")
        _require_inventory(data, _DATA_NAMES, "existing output data")
        return {
            path: hashlib.sha256(contents).hexdigest()
            for path, contents in sorted(files.items())
        }
    except (KeyError, TypeError, ValueError, OSError, EOFError, RecursionError, json.JSONDecodeError) as error:
        raise WriteRefused(f"existing output is not a recognized compatibility artifact tree: {error}") from error
    finally:
        if data is not None:
            os.close(data)


def _endpoint_state(
    temporary: str,
    destination: str,
    prior: int | None,
    new: int,
    *,
    published: bool,
) -> tuple[list[tuple[str, int]], list[str]]:
    if published:
        present = [(destination, new)]
        return (
            [*present, (temporary, prior)] if prior is not None else present,
            [] if prior is not None else [temporary],
        )
    present = [(temporary, new)]
    return (
        [*present, (destination, prior)] if prior is not None else present,
        [] if prior is not None else [destination],
    )


def _require_endpoint_state(
    directory: int,
    present: Sequence[tuple[str, int]],
    absent: Sequence[str],
    context: str,
) -> None:
    for name, descriptor in present:
        _require_entry(
            directory,
            name,
            descriptor,
            f"{context}: {name!r} no longer names its captured tree",
        )
    for name in absent:
        _require_absent(
            directory,
            name,
            f"{context}: unexpected entry at {name!r}",
        )


def _rollback(
    directory: int,
    temporary: str,
    destination: str,
    prior: int | None,
    new: int,
    error: Exception,
) -> None:
    replaced = prior is not None
    published = _endpoint_state(
        temporary, destination, prior, new, published=True
    )
    try:
        _require_endpoint_state(directory, *published, "before rollback")
    except (OSError, WriteRefused) as verify_error:
        raise RollbackRefused(
            "cannot publish compatibility data; rollback was not attempted "
            f"because an endpoint changed; inspect retained entries "
            f"{destination!r} and {temporary!r}: {verify_error}"
        ) from verify_error

    try:
        if replaced:
            _renameat2(directory, temporary, destination, _RENAME_EXCHANGE)
        else:
            _renameat2(directory, destination, temporary, _RENAME_NOREPLACE)
    except (OSError, WriteRefused) as rollback_error:
        detail = (
            f"the prior tree remains at {temporary!r} and the complete new "
            f"tree remains at {destination!r}"
            if replaced
            else f"the complete new tree remains at {destination!r}; no prior tree existed"
        )
        raise RollbackRefused(
            f"cannot publish compatibility data; rollback failed and {detail}: "
            f"{rollback_error}"
        ) from rollback_error

    original = _endpoint_state(
        temporary, destination, prior, new, published=False
    )
    try:
        _require_endpoint_state(
            directory,
            *original,
            "after rollback",
        )
    except (OSError, WriteRefused) as verify_error:
        raise RollbackRefused(
            "cannot publish compatibility data; rollback postcondition is "
            f"unverified for retained entries {destination!r} and "
            f"{temporary!r}: {verify_error}"
        ) from verify_error

    state = "prior tree restored" if replaced else "original absence restored"
    try:
        _fsync_directory(directory)
    except OSError as rollback_sync:
        raise WriteRefused(
            f"cannot publish compatibility data; {state}, but its directory "
            f"sync failed: {rollback_sync}"
        ) from rollback_sync
    raise WriteRefused(f"cannot publish compatibility data; {state}: {error}") from error


def _require_parent(directory: int, parent: Path, message: str) -> None:
    if not os.path.samestat(os.fstat(directory), os.stat(parent, follow_symlinks=False)):
        raise WriteRefused(message)


def _publish_tree(
    directory: int,
    parent: Path,
    temporary: str,
    destination: str,
    prior: int | None,
    new: int,
    prior_digests: Mapping[str, str] | None,
) -> int | None:
    replaced = prior is not None
    exchanged = False
    displaced = None
    try:
        _require_parent(directory, parent, "output parent changed before atomic replacement")
        original = _endpoint_state(
            temporary, destination, prior, new, published=False
        )
        _require_endpoint_state(
            directory, *original, "before atomic replacement"
        )
        _renameat2(
            directory,
            temporary,
            destination,
            _RENAME_EXCHANGE if replaced else _RENAME_NOREPLACE,
        )
        exchanged = True
        published = _endpoint_state(
            temporary, destination, prior, new, published=True
        )
        _require_endpoint_state(
            directory, *published, "after atomic replacement"
        )
        _require_parent(directory, parent, "output parent changed during atomic replacement")
        if replaced:
            if prior_digests is None:
                raise WriteRefused("captured prior digests are unavailable")
            displaced = _open_directory(directory, temporary)
            if not os.path.samestat(os.fstat(displaced), os.fstat(prior)):
                raise WriteRefused("reopened prior tree disagrees with captured root inode")
            if _recognize_prior(displaced) != prior_digests:
                raise WriteRefused("displaced prior artifact bytes changed after recognition")
        _fsync_directory(directory)
        return displaced
    except (OSError, WriteRefused) as publish_error:
        if displaced is not None:
            os.close(displaced)
        if exchanged:
            _rollback(
                directory,
                temporary,
                destination,
                prior,
                new,
                publish_error,
            )
        raise


def _remove_expected_tree(
    parent: int,
    name: str,
    directory: int,
    *,
    complete: bool,
    expected_digests: Mapping[str, str] | None = None,
) -> None:
    _require_entry(parent, name, directory, "retained tree changed before removal")
    if complete:
        if expected_digests is None or _recognize_prior(directory) != expected_digests:
            raise WriteRefused("retained prior tree changed before removal")
    root_names = _require_inventory(
        directory,
        _ROOT_NAMES,
        "retained output root",
        allow_missing=not complete,
    )
    data = None
    files: dict[tuple[int, str], int] = {}
    try:
        if "build.json" in root_names:
            files[(directory, "build.json")] = _open_regular(directory, "build.json")
        data_names = frozenset()
        if "data" in root_names:
            data = _open_directory(directory, "data")
            data_names = _require_inventory(
                data,
                _DATA_NAMES,
                "retained output data",
                allow_missing=not complete,
            )
            for filename in data_names:
                files[(data, filename)] = _open_regular(data, filename)
        _require_entry(parent, name, directory, "retained tree changed during removal")
        if _inventory(directory) != root_names:
            raise WriteRefused("retained output root changed during removal")
        if data is not None and _inventory(data) != data_names:
            raise WriteRefused("retained output data changed during removal")
        for (owner, filename), descriptor in sorted(files.items(), key=lambda item: item[0][1]):
            _require_entry(
                owner,
                filename,
                descriptor,
                f"retained file changed before removal: {filename}",
            )
            os.unlink(filename, dir_fd=owner)
        if data is not None:
            _require_inventory(data, frozenset(), "retained output data")
            _require_entry(
                directory,
                "data",
                data,
                "retained data directory changed before removal",
            )
            os.rmdir("data", dir_fd=directory)
        _require_inventory(directory, frozenset(), "retained output root")
        _require_entry(parent, name, directory, "retained tree changed before removal")
        os.rmdir(name, dir_fd=parent)
    finally:
        for descriptor in files.values():
            os.close(descriptor)
        if data is not None:
            os.close(data)


def _write_tree(files: Mapping[str, bytes], destination_value: Path) -> dict[str, Any]:
    destination = _destination(destination_value)
    parent = _open_parent(destination.parent)
    prior, data, displaced, temporary_fd = None, None, None, None
    prior_digests = None
    temporary = f".{destination.name}.tmp-{os.getpid()}-{os.urandom(8).hex()}"
    published = False
    try:
        current = _entry(parent, destination.name)
        if current is not None:
            if stat.S_ISLNK(current.st_mode):
                raise WriteRefused("output destination must not be a symlink")
            if not stat.S_ISDIR(current.st_mode):
                raise WriteRefused("output destination exists and is not a directory")
            prior = _open_directory(parent, destination.name)
            prior_digests = _recognize_prior(prior)
        os.mkdir(temporary, 0o700, dir_fd=parent)
        temporary_fd = _open_directory(parent, temporary)
        os.fchmod(temporary_fd, 0o755)
        os.mkdir("data", 0o755, dir_fd=temporary_fd)
        data = _open_directory(temporary_fd, "data")
        for relative, contents in files.items():
            _write_file(data if relative.startswith("data/") else temporary_fd, Path(relative).name, contents)
        _fsync_directory(data)
        os.close(data)
        data = None
        _fsync_directory(temporary_fd)
        displaced = _publish_tree(
            parent,
            destination.parent,
            temporary,
            destination.name,
            prior,
            temporary_fd,
            prior_digests,
        )
        published = True
    except RollbackRefused:
        temporary = ""
        raise
    except OSError as error:
        raise WriteRefused(f"cannot publish compatibility data: {error}") from error
    finally:
        if data is not None:
            os.close(data)
        if temporary and _entry(parent, temporary) is not None:
            try:
                cleanup = displaced if published and displaced is not None else temporary_fd
                if cleanup is None:
                    raise WriteRefused("retained tree could not be opened for removal")
                _remove_expected_tree(
                    parent,
                    temporary,
                    cleanup,
                    complete=published and displaced is not None,
                    expected_digests=prior_digests if published else None,
                )
            except (OSError, WriteRefused) as error:
                print(
                    "compatibility-website write data: WARNING: retained tree "
                    f"remains at {destination.parent / temporary}: {error}",
                    file=sys.stderr,
                )
        if displaced is not None:
            os.close(displaced)
        if temporary_fd is not None:
            os.close(temporary_fd)
        if prior is not None:
            os.close(prior)
        os.close(parent)
    return json.loads(files["build.json"])


def write_data(value: Any, destination_value: Path) -> dict[str, Any]:
    return _write_tree(render_tree(value), destination_value)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""Publication:
  The complete data tree is validated before writing, then the whole
  destination tree is replaced atomically. A refusal leaves an existing
  destination unchanged or an absent destination absent and writes nothing to
  stdout. If rollback itself fails, stderr identifies the retained complete
  trees. The output parent directory must already exist. An existing output
  must be a recognized prior compatibility artifact tree. Its root and data
  inventories must be exact; stale or extra entries are refused. Filesystem,
  system, home, checkout, workspace, and working-directory roots, paths
  containing '..', symlinks, and existing non-directory destinations are
  refused.

Example, from a clean dev-hermit checkout:
  python3 ci-hub/compatibility-website/write_data.py --parent . \\
    --parent-commit "$(git rev-parse HEAD)" --hermit hermit \\
    --hermit-commit "$(git -C hermit rev-parse HEAD)" \\
    --hermit-main-commit "$(git -C hermit rev-parse refs/remotes/origin/main)" \\
    --output /tmp/compatibility-website-data""",
    )
    parser.add_argument(
        "--parent", required=True, type=Path,
        help="exact clean dev-hermit checkout whose canonical ledger and series are read",
    )
    parser.add_argument(
        "--parent-commit", required=True,
        help="full dev-hermit commit currently checked out by --parent",
    )
    parser.add_argument(
        "--hermit", required=True, type=Path,
        help="exact clean Hermit checkout used for manifest policy and ancestry",
    )
    parser.add_argument(
        "--hermit-commit", required=True,
        help="full Hermit commit currently checked out by --hermit",
    )
    parser.add_argument(
        "--hermit-main-commit", required=True,
        help=(
            "full Hermit ancestry target; must equal the checkout's stable "
            "refs/remotes/origin/main"
        ),
    )
    parser.add_argument(
        "--output", required=True, type=Path,
        help="destination directory for the complete five-file website data tree",
    )
    arguments = parser.parse_args(argv)
    try:
        module = _build_data_module()
        try:
            data = module.build_data(
                arguments.parent,
                arguments.parent_commit,
                arguments.hermit,
                arguments.hermit_commit,
                arguments.hermit_main_commit,
            )
        except module.BuildRefused as error:
            raise WriteRefused(f"build data refused: {error}") from error
        encoded = _json_bytes(_write_tree(_render_validated_tree(data), arguments.output)) + b"\n"
    except (WriteRefused, OSError, ValueError) as error:
        print(f"compatibility-website write data: REFUSED: {error}", file=sys.stderr)
        return 2
    sys.stdout.buffer.write(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
