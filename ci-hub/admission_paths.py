"""Local v2 artifact access through an explicitly selected, held state root.

The root argument is supplied by the caller. Holding and checking it prevents
path redirection; it does not independently authenticate admission authority.
The driver owns that proof and compares the finalizer's complete evidence echo.
"""

from contextlib import contextmanager
import os
from pathlib import Path
import stat

import qualifying_receipt


class AdmissionPaths:
    def __init__(self, root):
        self.root = Path(root)
        if not self.root.is_absolute() or self.root.resolve(strict=True) != self.root:
            raise ValueError("admission state root must be an existing canonical absolute directory")
        self.fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.identity = self._identity(os.fstat(self.fd))

    @staticmethod
    def _identity(info):
        return info.st_dev, info.st_ino

    def __enter__(self):
        try:
            self.check_root()
        except Exception:
            os.close(self.fd)
            raise
        return self

    def __exit__(self, *_exc):
        os.close(self.fd)

    def check_root(self):
        current = os.stat(self.root, follow_symlinks=False)
        if (not stat.S_ISDIR(current.st_mode)
                or self._identity(current) != self.identity
                or self._identity(os.fstat(self.fd)) != self.identity):
            raise ValueError("admission state root changed after selection")

    def path(self, locator):
        relative = qualifying_receipt.admission_workspace_locator(locator)
        parts = relative.split("/")
        if len(parts) < 3 or parts[:2] != ["ignored", "validate"]:
            raise ValueError("admission artifact is outside ignored/validate retention")
        self.check_root()
        return self.root.joinpath(*parts)

    def locator(self, path):
        # No resolve(): accepting a normalized symlink/traversal spelling would
        # detach the received path from its identity before the no-follow walk.
        if not isinstance(path, (str, os.PathLike)):
            raise ValueError("admission path must be an absolute path")
        text = os.fspath(path)
        prefix = str(self.root) + "/"
        if not isinstance(text, str) or not text.startswith(prefix):
            raise ValueError("admission path is outside the selected state root")
        locator = {"scope": "workspace", "path": text[len(prefix):]}
        self.path(locator)
        return locator

    @contextmanager
    def open(self, locator):
        self.path(locator)
        parts = locator["path"].split("/")
        directory = os.dup(self.fd)
        try:
            for part in parts[:-1]:
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                dir_fd=directory)
                os.close(directory)
                directory = child
            descriptor = os.open(parts[-1], os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW,
                                 dir_fd=directory)
            with os.fdopen(descriptor, "rb") as handle:
                if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                    raise ValueError("admission artifact is not a regular file")
                self.check_root()
                yield handle
                self.check_root()
        finally:
            os.close(directory)

    def read(self, locator, *, maximum=None):
        with self.open(locator) as handle:
            before = os.fstat(handle.fileno())
            raw = handle.read() if maximum is None else handle.read(maximum + 1)
            after = os.fstat(handle.fileno())
        if maximum is not None:
            if len(raw) > maximum:
                raise ValueError("admission context exceeds reader byte capacity")
            if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
                    after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
                raise ValueError("admission context changed while reading")
        return raw

    def require_log(self, context, path):
        if self.locator(path) != context["log_identity"]:
            raise ValueError("admission log path differs from the bound logical identity")
        with self.open(context["log_identity"]):
            pass


def read_admission_log(row, state_root):
    """Read a valid v2 row's logical log; never fall back to its display path."""
    qualifying_receipt.validate_admission_floor_evidence(row["admission_floor_evidence"], row)
    if not qualifying_receipt.admission_is_v2(row):
        raise ValueError("portable log resolution requires v2")
    with AdmissionPaths(state_root) as paths:
        return paths.read(row["log_identity"])
