"""Capture a child's output by waiting for THE CHILD, not for EOF on a pipe.

⚠️ WHAT IS WRONG WITH `capture_output=True`. It hands the child a PIPE and reads
it to EOF. EOF arrives when the LAST HOLDER of the write end closes it, which is
not the same event as the child exiting. Any background grandchild that
inherited the descriptor keeps the pipe open, so the caller blocks forever on a
child that is already reaped and whose exit status is already known.

`git` is the everyday trigger and it needs no unusual configuration: `gc.auto`
defaults to 6700 and `gc.autoDetach` defaults to true, so an ordinary `git
rev-list` can fork a detached `git gc --auto` that inherits stderr and outlives
it. The parent then hangs on a command that finished.

Reproduced 2026-08-25: a child that exited immediately while leaving one
background grandchild blocked `capture_output=True` indefinitely, while
capturing the same command to a regular file returned in 0.0s with byte-identical
stdout and the same exit code. A regular file has no EOF-waiting semantics, so
`wait()` on the child is the only gate.

⚠️ THIS IS DELIBERATELY NOT A TIMEOUT AND NOT `gc.auto=0`.
A timeout bounds the wait and throws away the reason, turning a deterministic
hang into a flaky failure; and it still has to pick a number that is wrong for
either a fast box or a slow one. Disabling auto-gc hides this particular
grandchild and leaves the next one -- a hook, a credential helper, a pager, an
`ssh` multiplexer -- to reintroduce the same hang somewhere else. Reading into a
file removes the mechanism instead of the symptom.

WHY THIS LIVES IN lib/. The same helper was written three times independently
(closure/artifact_triage.py, health/tick_code_currency.py,
qualifying_receipt.py). Four copies of a subtle fix drift; one does not.
"""

from __future__ import annotations

import subprocess
import tempfile
from typing import Sequence


class CapturedProcess(subprocess.CompletedProcess):
    """What `capture` returns: a `CompletedProcess` produced by waiting on the child.

    ⚠️ IT SUBCLASSES `subprocess.CompletedProcess` DELIBERATELY. This replaces
    `subprocess.run(..., capture_output=True)` at call sites all over ci-hub, and
    those sites carry `-> subprocess.CompletedProcess[str]` annotations, are
    stubbed by tests, and are occasionally isinstance-checked. Returning a
    lookalike would have made a behaviour-preserving change into a typing change,
    so the only difference from a `run` result is HOW the output was collected.

    The distinct name is kept so a traceback or repr says which path produced it.
    """

    def __repr__(self) -> str:  # pragma: no cover - diagnostic only
        return f"CapturedProcess(args={self.args!r}, returncode={self.returncode})"


def capture(
    command: Sequence[str],
    *,
    text: bool = True,
    check: bool = False,
    encoding: str | None = None,
    errors: str | None = "replace",
    input: bytes | str | None = None,
    **popen_kwargs,
) -> CapturedProcess:
    """Run `command`, returning its stdout/stderr and exit code.

    Behaves like `subprocess.run(command, capture_output=True)` except that the
    output goes to temporary FILES, so the call returns as soon as the child
    exits regardless of what background processes it left holding descriptors.

    `stdin` defaults to DEVNULL, which is the other half of not hanging: a child
    that decides to prompt gets EOF instead of blocking on a terminal that no
    agent is watching. Pass `input=` to feed it bytes/text instead, or an
    explicit `stdin=` to override.
    """
    if "stdout" in popen_kwargs or "stderr" in popen_kwargs:
        raise TypeError(
            "bounded_capture.capture owns stdout/stderr; passing them defeats the "
            "point of the helper"
        )
    if input is not None and "stdin" in popen_kwargs:
        raise TypeError("pass either input= or stdin=, not both")

    mode = "w+" if text else "w+b"
    stdin_source = popen_kwargs.pop("stdin", subprocess.DEVNULL)
    if input is not None:
        stdin_source = subprocess.PIPE

    with tempfile.TemporaryFile(mode) as out, tempfile.TemporaryFile(mode) as err:
        proc = subprocess.Popen(
            list(command),
            stdout=out,
            stderr=err,
            stdin=stdin_source,
            text=text,
            encoding=encoding,
            errors=errors if text else None,
            **popen_kwargs,
        )
        if input is not None:
            # Writing to stdin CAN block if the child never drains it, but that
            # is a wait on the child's own behaviour, not on an unrelated
            # grandchild holding an inherited descriptor. Close it so the child
            # sees EOF and can exit.
            try:
                proc.stdin.write(input)
            finally:
                proc.stdin.close()
        returncode = proc.wait()
        out.seek(0)
        err.seek(0)
        result = CapturedProcess(list(command), returncode, out.read(), err.read())

    if check:
        result.check_returncode()
    return result


def git(args: Sequence[str], *, cwd=None, **kwargs) -> CapturedProcess:
    """`capture` for a git invocation, which is the case this exists for."""
    command = ["git"]
    if cwd is not None:
        command += ["-C", str(cwd)]
    command += list(args)
    return capture(command, **kwargs)
