#!/usr/bin/env python3
"""Run a validation's `git` as the binary the host's telemetry wrapper execs.

Why this exists: on devbig014 `git` on PATH is /usr/local/bin/git, an ELF
telemetry wrapper (package fb-git-wrapper) that execs
/usr/local/bin/git.meta.real (package fb-git-meta). Both report
`git version 2.53.0-Meta`. The wrapper also spawns further git processes and a
detached logger on every call. One complete `gate.manifest` step runs about
4011 git commands (Hermit ci/dag/validate.json, gate.manifest description).
Local validate run 1961 (target 9d16823f) was killed at 901.5 CPU-seconds of
that step's 900-second CPU budget. Measured at a load average near 200, a
wrapped call cost about 164 ms against 19 ms for git.meta.real, so 4011 calls
carry roughly 580 CPU-seconds of wrapper overhead. This module removes that
overhead by running the same git without the wrapper; it does not raise the
budget, and no cap, timeout, audit or verdict reads anything here.

It is a mitigation, not a fix. In one paired measurement at Hermit 98a621f4
and load average 191-213, gate.manifest used 2292.6 CPU-seconds wrapped and
1559.7 unwrapped: 32 % less, and both still over the 900 CPU-second budget.
https://github.com/rrnewton/hermit/issues/3381 stays open until a local
validate of main finishes gate.manifest's six audits inside 900 seconds of
wall time and 900 CPU-seconds; cutting the number of git calls is
https://github.com/rrnewton/hermit/issues/3382.

What it does, at launch, before the validation's DAG starts:

1. Resolve `git` on the PATH the validation would inherit.
2. Require the unwrapped binary (`REAL_GIT`, /usr/local/bin/git.meta.real) to
   be an executable regular file.
3. Require PATH's git to be the wrapper, decided by file identity
   (`os.path.samefile`) against the `git` next to the unwrapped binary and
   never by a name or package string. PATH's git already being the unwrapped
   binary, or being some other git (for example /usr/bin/git), is left alone.
4. Require `<unwrapped> --version` to equal `<PATH's git> --version`.

When all four hold, a run-private directory (the caller names it, under the
run's own runtime or state directory) is created with exactly one entry, a
`git` symlink to the unwrapped binary, and that directory is put first on the
PATH handed to the run. When any check fails PATH is returned unchanged. Either
way the decision is summarised in one line that names the git the DAG will run,
its version, whether the substitution was applied, and why.

What the substitution gives up: the wrapper's telemetry and its retry of HTTP
429 responses on network operations. It changes nothing else a git command
does: the unwrapped binary is the program the wrapper would have run.

The version probes write to regular files, not pipes: the wrapper leaves a
detached logger holding its inherited output, and reading a pipe to EOF would
wait for that logger instead of for git (the same trap
ci-hub/validate/start_unit.py `run_command` documents).
"""
from __future__ import annotations

import argparse
import json
import os
import select
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Mapping, Sequence

REAL_GIT = Path("/usr/local/bin/git.meta.real")
PROBE_TIMEOUT_SECONDS = 60.0

APPLIED = "applied"
NO_GIT_ON_PATH = "no-git-on-path"
RELATIVE_PATH_ENTRY = "relative-path-entry"
UNUSABLE_DIRECTORY = "unusable-directory"
REAL_GIT_UNAVAILABLE = "real-git-unavailable"
ALREADY_UNWRAPPED = "already-unwrapped"
NOT_THE_WRAPPER = "not-the-wrapper"
VERSION_PROBE_FAILED = "version-probe-failed"
VERSION_MISMATCH = "version-mismatch"
MATERIALIZE_FAILED = "materialize-failed"
CODES = frozenset(
    {
        APPLIED,
        NO_GIT_ON_PATH,
        RELATIVE_PATH_ENTRY,
        UNUSABLE_DIRECTORY,
        REAL_GIT_UNAVAILABLE,
        ALREADY_UNWRAPPED,
        NOT_THE_WRAPPER,
        VERSION_PROBE_FAILED,
        VERSION_MISMATCH,
        MATERIALIZE_FAILED,
    }
)


@dataclass(frozen=True)
class GitSubstitution:
    """One launch's decision. `path` is the PATH to hand the run."""

    applied: bool
    code: str
    reason: str
    resolved_git: str | None
    resolved_git_realpath: str | None
    real_git: str
    effective_git: str | None
    effective_version: str | None
    directory: str | None
    path: str
    inherited_path: str

    def record(self) -> dict[str, object]:
        return {
            "applied": self.applied,
            "code": self.code,
            "reason": self.reason,
            "resolved_git": self.resolved_git,
            "resolved_git_realpath": self.resolved_git_realpath,
            "real_git": self.real_git,
            "effective_git": self.effective_git,
            "effective_version": self.effective_version,
            "directory": self.directory,
        }

    def summary(self) -> str:
        """One log line: the decision, then the record as JSON."""
        state = "APPLIED" if self.applied else "NOT-APPLIED"
        return f"git substitution {state}: {json.dumps(self.record(), sort_keys=True)}"


def find_on_path(name: str, path: str) -> str | None:
    """The first PATH entry holding an executable regular file called `name`.

    This is execvp's search. shutil.which is avoided on purpose: callers'
    tests replace it with name-keyed fakes.
    """
    for entry in path.split(os.pathsep):
        candidate = os.path.join(entry or ".", name)
        try:
            mode = os.stat(candidate).st_mode
        except OSError:
            continue
        if stat.S_ISREG(mode) and os.access(candidate, os.X_OK):
            return candidate
    return None


def _wait_bounded(process: subprocess.Popen, timeout: float) -> int | None:
    """The process's exit status, or None if it runs past `timeout` seconds.

    Waits on a pidfd rather than calling Popen.wait(timeout=...), which polls
    with time.sleep: validate-run's tests patch time.sleep to prove the
    launcher itself never backs off, and a probe must not look like one.
    """
    try:
        pidfd = os.pidfd_open(process.pid)
    except (AttributeError, OSError):
        try:
            return process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return None
    try:
        poller = select.poll()
        poller.register(pidfd, select.POLLIN)
        if not poller.poll(max(0, int(timeout * 1000))):
            return None
    finally:
        os.close(pidfd)
    return process.wait()


def probe_version(
    executable: str,
    *,
    env: Mapping[str, str] | None = None,
    timeout: float = PROBE_TIMEOUT_SECONDS,
) -> tuple[str | None, str]:
    """Return (first line of `executable --version`, failure detail).

    Never raises OSError: a probe that cannot create its capture files, start,
    be waited for, or be read back is a failed probe, and the caller leaves
    PATH unchanged.
    """
    process: subprocess.Popen | None = None
    try:
        with tempfile.TemporaryFile("w+b") as out, tempfile.TemporaryFile(
            "w+b"
        ) as err:
            try:
                process = subprocess.Popen(
                    [executable, "--version"],
                    stdin=subprocess.DEVNULL,
                    stdout=out,
                    stderr=err,
                    env=None if env is None else dict(env),
                )
            except OSError as error:
                return None, f"{executable} --version could not start: {error}"
            code = _wait_bounded(process, timeout)
            if code is None:
                process.kill()
                process.wait()
                return None, f"{executable} --version did not finish in {timeout:g}s"
            out.seek(0)
            err.seek(0)
            stdout = out.read().decode("utf-8", "replace")
            stderr = err.read().decode("utf-8", "replace")
    except OSError as error:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait()
        return None, f"{executable} --version could not be probed: {error}"
    lines = stdout.strip().splitlines()
    if code != 0 or not lines:
        detail = stderr.strip()[:200] or "no output"
        return None, f"{executable} --version exited {code}: {detail}"
    return lines[0].strip(), ""


def _same_file(left: str | os.PathLike[str], right: str | os.PathLike[str]) -> bool:
    try:
        return os.path.samefile(left, right)
    except OSError:
        return False


def plan(
    path: str,
    directory: Path,
    *,
    real_git: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> GitSubstitution:
    """Decide the substitution for a run that would inherit `path`.

    Creates nothing: `materialize` creates `directory`. A dry run can print the
    planned PATH without touching the filesystem.
    """
    real = Path(REAL_GIT if real_git is None else real_git)
    resolved = find_on_path("git", path)

    def keep(code: str, reason: str, version: str | None = None) -> GitSubstitution:
        return GitSubstitution(
            applied=False,
            code=code,
            reason=reason,
            resolved_git=resolved,
            resolved_git_realpath=(
                os.path.realpath(resolved) if resolved is not None else None
            ),
            real_git=str(real),
            effective_git=resolved,
            effective_version=version,
            directory=None,
            path=path,
            inherited_path=path,
        )

    def keep_with_version(code: str, reason: str) -> GitSubstitution:
        assert resolved is not None
        version, failure = probe_version(resolved, env=env)
        return keep(code, reason if version else f"{reason}; {failure}", version)

    if resolved is None:
        return keep(NO_GIT_ON_PATH, "PATH has no executable git; PATH unchanged")
    if not os.path.isabs(resolved):
        # Not probed: it names a file relative to this launcher's directory,
        # which is neither what the run would execute nor known to be git.
        return keep(
            RELATIVE_PATH_ENTRY,
            f"PATH resolves git through the relative entry {resolved}, which "
            "names a different file in the run's working directory; PATH unchanged",
        )
    if not directory.is_absolute() or os.pathsep in str(directory):
        return keep_with_version(
            UNUSABLE_DIRECTORY,
            f"{directory} cannot be a PATH entry (it must be absolute and "
            f"contain no {os.pathsep!r}); PATH unchanged",
        )
    try:
        real_mode = os.stat(real).st_mode
    except OSError as error:
        return keep_with_version(
            REAL_GIT_UNAVAILABLE,
            f"{real} is unavailable ({error.strerror or error}); PATH unchanged",
        )
    if not stat.S_ISREG(real_mode) or not os.access(real, os.X_OK):
        return keep_with_version(
            REAL_GIT_UNAVAILABLE,
            f"{real} is not an executable regular file; PATH unchanged",
        )
    if _same_file(resolved, real):
        return keep_with_version(
            ALREADY_UNWRAPPED,
            f"PATH's git {resolved} is already {real}; PATH unchanged",
        )
    wrapper = real.parent / "git"
    if not _same_file(resolved, wrapper):
        return keep_with_version(
            NOT_THE_WRAPPER,
            f"PATH's git {resolved} is not the wrapper {wrapper} that execs "
            f"{real}; PATH unchanged",
        )
    wrapper_version, wrapper_failure = probe_version(resolved, env=env)
    if wrapper_version is None:
        return keep(VERSION_PROBE_FAILED, f"{wrapper_failure}; PATH unchanged")
    real_version, real_failure = probe_version(str(real), env=env)
    if real_version is None:
        return keep(
            VERSION_PROBE_FAILED, f"{real_failure}; PATH unchanged", wrapper_version
        )
    if real_version != wrapper_version:
        return keep(
            VERSION_MISMATCH,
            f"{resolved} reports {wrapper_version!r} but {real} reports "
            f"{real_version!r}; PATH unchanged",
            wrapper_version,
        )
    return GitSubstitution(
        applied=True,
        code=APPLIED,
        reason=(
            f"PATH's git {resolved} is the wrapper for {real} and both report "
            f"{real_version!r}; PATH starts with a run-private directory whose "
            f"only entry is a git symlink to {real}"
        ),
        resolved_git=resolved,
        resolved_git_realpath=os.path.realpath(resolved),
        real_git=str(real),
        effective_git=str(real),
        effective_version=real_version,
        directory=str(directory),
        path=f"{directory}{os.pathsep}{path}" if path else str(directory),
        inherited_path=path,
    )


def materialize(substitution: GitSubstitution) -> GitSubstitution:
    """Create the planned directory; on any failure return PATH unchanged.

    The directory must not exist yet: it belongs to exactly one run. Its only
    entry is `git`, a symlink to the unwrapped binary. If the result is not
    exactly that, what this call created is removed again (by `remove`, so
    only the exact shape goes) and the reason says whether anything was left.
    """
    if not substitution.applied:
        return substitution
    assert substitution.directory is not None
    directory = Path(substitution.directory)
    link = directory / "git"
    created = False
    try:
        directory.parent.mkdir(parents=True, exist_ok=True)
        directory.mkdir()
        created = True
        link.symlink_to(substitution.real_git)
        entries = sorted(entry.name for entry in directory.iterdir())
        if entries != ["git"] or not _same_file(link, substitution.real_git):
            raise OSError(f"{directory} holds {entries}, not one git symlink")
    except OSError as error:
        problem = _undo_materialize(directory, substitution.real_git) if created else None
        leftover = "" if problem is None else f"; {problem}"
        return replace(
            substitution,
            applied=False,
            code=MATERIALIZE_FAILED,
            reason=(
                f"could not create {directory} with one git symlink to "
                f"{substitution.real_git} ({error}){leftover}; PATH unchanged"
            ),
            effective_git=substitution.resolved_git,
            directory=None,
            path=substitution.inherited_path,
        )
    return substitution


def _undo_materialize(directory: Path, real_git: str) -> str | None:
    """Remove the directory a failed `materialize` created; None means gone.

    An empty directory is removed. So is one whose only entry is a `git`
    symlink, whatever it points at, because this call made that link. Any
    other entry is not this call's, so the directory is left in place.
    """
    link = directory / "git"
    try:
        if not os.listdir(directory):
            directory.rmdir()
            return None
        target = os.readlink(link) if link.is_symlink() else real_git
    except OSError as error:
        return f"could not remove {directory}: {error}"
    return remove(directory, target)


def remove(directory: Path, real_git: str | os.PathLike[str]) -> str | None:
    """Remove a directory `materialize` created. None means it is gone.

    Only the exact shape `materialize` makes is removed: a directory whose
    only entry is `git`, a symlink whose target is `real_git`. Anything else
    is left in place and the returned text says what was found. An absent
    directory is not an error.
    """
    try:
        metadata = os.lstat(directory)
    except FileNotFoundError:
        return None
    except OSError as error:
        return f"cannot inspect {directory}: {error}"
    if not stat.S_ISDIR(metadata.st_mode):
        return f"{directory} is not a directory; left in place"
    link = directory / "git"
    try:
        entries = sorted(os.listdir(directory))
        if (
            entries != ["git"]
            or not link.is_symlink()
            or os.readlink(link) != os.fspath(real_git)
        ):
            return (
                f"{directory} holds {entries}, not one git symlink to "
                f"{os.fspath(real_git)}; left in place"
            )
        link.unlink()
        directory.rmdir()
    except OSError as error:
        return f"could not remove {directory}: {error}"
    return None


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__.split("\n\n", 1)[0],
        epilog="Prints the decision; with --directory it also creates it.",
    )
    parser.add_argument("--path", default=os.environ.get("PATH", ""))
    parser.add_argument(
        "--directory",
        type=Path,
        help="create this run-private directory when the substitution applies",
    )
    parser.add_argument("--print-path", action="store_true",
                        help="print only the PATH to use (requires --directory)")
    args = parser.parse_args(argv)
    if args.print_path and args.directory is None:
        parser.error("--print-path requires --directory")
    if args.directory is None:
        # Decide only. The placeholder directory is never created, so the
        # record shows no directory and no PATH.
        substitution = plan(args.path, Path(tempfile.gettempdir()) / "unwrapped-git")
        print(json.dumps({**substitution.record(), "directory": None},
                         indent=2, sort_keys=True))
        return 0
    substitution = materialize(plan(args.path, args.directory))
    if args.print_path:
        print(substitution.path)
    else:
        print(json.dumps({**substitution.record(), "path": substitution.path},
                         indent=2, sort_keys=True))
    print(substitution.summary(), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
