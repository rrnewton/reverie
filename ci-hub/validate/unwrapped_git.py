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

It is a mitigation, not a fix. One paired measurement (n=1) at Hermit
98a621f4, load average 191-213, ran gate.manifest's command twice at the same
time, each in its own cgroup scope, and read each scope's cpu.stat
usage_usec: 2292.6 CPU-seconds and 2262.5 s of wall time wrapped, 1559.7
CPU-seconds and 1827.5 s unwrapped. That is 32 % less CPU, and both are over
the 900 CPU-second budget that step had at 98a621f4. cpu.stat usage_usec is
the counter dagrun compares with a step's cpu_timeout when the run is boxed
(agent-utils rs/dagrun/src/scheduler.rs, cpu_seconds_from_stats); an
unboxed run falls back to procfs process-group accounting, which misses the
wrapper's detached logger. Hermit d7138e26 has since moved the tool
self-tests out of gate.manifest into their own leaf nodes and set its
cpu_timeout to 120 (ci/dag/validate.json at Hermit 84da7b81), so neither
number is a measurement of main's step.
https://github.com/rrnewton/hermit/issues/3381 stays open until the first
local validate of main after this change records gate.manifest's CPU-seconds
and wall time and shows its six audits finishing within its budgets; cutting
the number of git calls is https://github.com/rrnewton/hermit/issues/3382.

What it does, at launch, before the validation's DAG starts:

1. Resolve `git` on the PATH the validation would inherit.
2. Require the unwrapped binary (`REAL_GIT`, /usr/local/bin/git.meta.real) to
   be an executable regular file.
3. Require PATH's git to be the wrapper, decided by file identity
   (`os.path.samefile`) against the `git` next to the unwrapped binary and
   never by a name or package string. PATH's git already being the unwrapped
   binary, or being some other git (for example /usr/bin/git), is left alone.
4. Require the wrapper's configuration to name the unwrapped binary. The
   wrapper reads `$SCM_TELEMETRY_CONFIG` when that variable is set and
   `WRAPPER_CONFIG` (/etc/scm-telemetry.toml) otherwise, and execs the
   `[binaries] git` path it finds there. Measured on devbig014 with
   fb-git-wrapper-20260928-001642: a missing file or an empty variable makes
   it exec /usr/bin/git.real (git 2.52.0) instead. The file is parsed as
   TOML and `binaries.git` must be an absolute path that is the same file as
   the unwrapped binary.
5. Require `<unwrapped> --version` to equal `<PATH's git> --version`.

When all five hold, a run-private directory (the caller names it, under the
run's own runtime or state directory) is created with exactly one entry, a
`git` symlink to the unwrapped binary, and that directory is put first on the
PATH handed to the run. When any check fails PATH is returned unchanged. Either
way the decision is summarised in one line that names the git the DAG will run,
its version, the wrapper configuration and the path it names, whether the
substitution was applied, and why.

Who gets the new PATH. validate-run gives it only to validate-lock's child
(`/usr/bin/env PATH=<new> DEV_HERMIT_NETWORK_GIT=<wrapper> ...`), which is the
validation driver and its DAG steps. validate-lock itself and the unit's
ExecStopPost retire path, including the ledger mirror's network push and
fetch, keep the unit's PATH and so the wrapper. The pressure test sets PATH
for its whole unit, and its one network git call, the submodule fetch, runs
DEV_HERMIT_NETWORK_GIT. Network git calls that do run the unwrapped binary,
and so have no 429 retry, are pre.reverie_pin's `ls-remote` and `fetch`
(https://github.com/rrnewton/hermit/issues/3390) and cargo's
git-fetch-with-cli fetches of Reverie.

What the substitution gives up, measured with strace on devbig014
(fb-git-wrapper-20260928-001642, `git -C <repository> rev-parse HEAD`):

- Telemetry. Each wrapped call reads the wrapper configuration, starts
  `scm_telem_log` (found on PATH, /usr/bin/scm_telem_log here) as a detached
  logger that outlives the call and itself runs
  `git.meta.real remote get-url origin`, and runs three more git.meta.real
  commands before the requested one (`rev-parse --show-toplevel`,
  `remote -v`, `version`).
- Two HTTP headers. The requested command runs with
  `-c http.extraHeader=x-fb-git-wrapper: <correlator>` and
  `-c http.extraHeader=X-Client-Info: <JSON with the hostname and
  correlator>`, which git passes to its own children as
  GIT_CONFIG_PARAMETERS.
- A trace2 file. The requested command runs with GIT_TRACE2 naming a new
  0600 file in $TMPDIR (for validate-run, the unit's runtime root) and
  GIT_TRACE2_BRIEF=1. The wrapper reads that file and deletes it when the
  command exits.
- Its HTTP 429 retry. The binary contains `GIT_NO_HTTP_429_RETRY`,
  `HTTP 429 (rate limited)` and `, retrying in `; the retry itself was not
  exercised here.

Nothing else changes. The requested command is the same binary, run with the
same arguments minus those two `-c` options, and with the same environment
minus GIT_TRACE2, GIT_TRACE2_BRIEF and the GIT_CONFIG_PARAMETERS entries
above.

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
import tomllib
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Mapping, Sequence

REAL_GIT = Path("/usr/local/bin/git.meta.real")
# The wrapper's configuration file, and the variable that replaces it.
WRAPPER_CONFIG = Path("/etc/scm-telemetry.toml")
WRAPPER_CONFIG_ENV = "SCM_TELEMETRY_CONFIG"
# Set beside a substituted PATH to PATH's original git (the wrapper), for the
# calls that use the network and so want the wrapper's HTTP 429 retry. The
# pressure campaign's submodule fetch reads it; a validation DAG step reads it
# only if that step's own code does.
NETWORK_GIT_ENV = "DEV_HERMIT_NETWORK_GIT"
PROBE_TIMEOUT_SECONDS = 60.0

APPLIED = "applied"
NO_GIT_ON_PATH = "no-git-on-path"
RELATIVE_PATH_ENTRY = "relative-path-entry"
UNUSABLE_DIRECTORY = "unusable-directory"
REAL_GIT_UNAVAILABLE = "real-git-unavailable"
ALREADY_UNWRAPPED = "already-unwrapped"
NOT_THE_WRAPPER = "not-the-wrapper"
WRAPPER_CONFIG_UNREADABLE = "wrapper-config-unreadable"
WRAPPER_CONFIG_MISMATCH = "wrapper-config-mismatch"
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
        WRAPPER_CONFIG_UNREADABLE,
        WRAPPER_CONFIG_MISMATCH,
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
    # The configuration file the wrapper would read and its `[binaries] git`
    # value. Both are None until PATH's git is known to be the wrapper;
    # `wrapper_config_git` stays None when the file cannot be read.
    wrapper_config: str | None = None
    wrapper_config_git: str | None = None

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
            "wrapper_config": self.wrapper_config,
            "wrapper_config_git": self.wrapper_config_git,
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


def wrapper_config_path(env: Mapping[str, str] | None = None) -> str:
    """The configuration file the wrapper reads for a run with `env`.

    A set `SCM_TELEMETRY_CONFIG` replaces WRAPPER_CONFIG even when it is
    empty; the wrapper then reads no file at all.
    """
    environment = os.environ if env is None else env
    if WRAPPER_CONFIG_ENV in environment:
        return environment[WRAPPER_CONFIG_ENV]
    return str(WRAPPER_CONFIG)


def read_wrapper_config_git(config: str) -> tuple[str | None, str]:
    """Return (`[binaries] git` from the wrapper configuration, failure detail)."""
    if not config:
        return None, (
            f"{WRAPPER_CONFIG_ENV} is set but empty, so the wrapper reads no "
            "configuration and runs its built-in default git"
        )
    if not os.path.isabs(config):
        return None, (
            f"the wrapper configuration {config} is a relative path, which each "
            "git call would resolve against its own working directory"
        )
    try:
        with open(config, "rb") as handle:
            settings = tomllib.load(handle)
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        return None, f"cannot read the wrapper configuration {config}: {error}"
    binaries = settings.get("binaries")
    value = binaries.get("git") if isinstance(binaries, dict) else None
    if not isinstance(value, str) or not value:
        return None, f"the wrapper configuration {config} has no [binaries] git path"
    return value, ""


def plan(
    path: str,
    directory: Path,
    *,
    real_git: Path | None = None,
    wrapper_config: str | os.PathLike[str] | None = None,
    env: Mapping[str, str] | None = None,
) -> GitSubstitution:
    """Decide the substitution for a run that would inherit `path`.

    `env` is the environment the run inherits; it selects the wrapper
    configuration unless `wrapper_config` names one. Creates nothing:
    `materialize` creates `directory`. A dry run can print the planned PATH
    without touching the filesystem.
    """
    real = Path(REAL_GIT if real_git is None else real_git)
    resolved = find_on_path("git", path)
    config: str | None = None
    config_git: str | None = None

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
            wrapper_config=config or None,
            wrapper_config_git=config_git,
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
            f"PATH's git {resolved} is not the wrapper {wrapper} installed "
            f"beside {real}; PATH unchanged",
        )
    config = os.fspath(
        wrapper_config_path(env) if wrapper_config is None else wrapper_config
    )
    config_git, config_failure = read_wrapper_config_git(config)
    if config_git is None:
        return keep_with_version(
            WRAPPER_CONFIG_UNREADABLE, f"{config_failure}; PATH unchanged"
        )
    if not os.path.isabs(config_git) or not _same_file(config_git, real):
        return keep_with_version(
            WRAPPER_CONFIG_MISMATCH,
            f"the wrapper configuration {config} makes {wrapper} exec "
            f"{config_git!r}, not {real}; PATH unchanged",
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
            f"PATH's git {resolved} is the wrapper, {config} makes it exec "
            f"{real}, and both report {real_version!r}; PATH starts with a "
            f"run-private directory whose only entry is a git symlink to {real}"
        ),
        resolved_git=resolved,
        resolved_git_realpath=os.path.realpath(resolved),
        real_git=str(real),
        effective_git=str(real),
        effective_version=real_version,
        directory=str(directory),
        path=f"{directory}{os.pathsep}{path}" if path else str(directory),
        inherited_path=path,
        wrapper_config=config,
        wrapper_config_git=config_git,
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


def decision(substitution: GitSubstitution) -> dict[str, object]:
    """What `main` prints when it only decides: a record without a directory.

    `planned_directory` is where an applied substitution would put its
    directory and `created` is always false, so the output cannot be mistaken
    for (or accepted as) a run handle's `git_substitution`.
    """
    fields = substitution.record()
    planned = fields.pop("directory")
    return {**fields, "planned_directory": planned, "created": False}


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
        # Decide only. This is not a run record: nothing is created, so
        # `directory` is replaced by the placeholder it would have used and
        # `created` is false.
        print(json.dumps(decision(plan(
            args.path, Path(tempfile.gettempdir()) / "unwrapped-git"
        )), indent=2, sort_keys=True))
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
