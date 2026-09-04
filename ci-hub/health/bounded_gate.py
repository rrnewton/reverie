#!/usr/bin/env python3
"""Run one tick gate inside a shorter, typed timeout boundary.

Tick-hub has an outer subprocess timeout.  If that boundary fires first, the
engine can only report that its command did not run; the gate itself emits no
domain outcome.  This adapter deliberately expires first and converts timeout
into ordinary captured gate output plus a nonzero exit:

    state=NO_RESULT
    outcome=timeout
    gate=<name>
    bound_secs=<seconds>
    summary=NO_RESULT/timeout gate=<name> bound_secs=<seconds>

The normal path is transparent: stdout, stderr, and return code are preserved.
The child receives its own process group so a timeout also reaps descendants.
"""

from __future__ import annotations

import argparse
import math
import os
import re
import signal
import subprocess
import sys


_NAME = re.compile(r"^[a-z0-9][a-z0-9_-]*$")


def _positive_seconds(raw: str) -> float:
    try:
        value = float(raw)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be a number") from exc
    if not math.isfinite(value) or value <= 0:
        raise argparse.ArgumentTypeError("must be finite and greater than zero")
    return value


def _bound_text(value: float) -> str:
    return str(int(value)) if value.is_integer() else format(value, "g")


def _terminate_group(proc: subprocess.Popen[bytes]) -> None:
    if os.name == "posix":
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except OSError:
            proc.kill()
    else:
        proc.kill()
    try:
        proc.communicate()
    except OSError:
        pass


def _no_result(name: str, outcome: str, bound: str, detail: str = "") -> int:
    print("state=NO_RESULT")
    print(f"outcome={outcome}")
    print(f"gate={name}")
    print(f"bound_secs={bound}")
    if detail:
        print(f"detail={detail.replace(chr(10), ' ')[:240]}")
    print(f"summary=NO_RESULT/{outcome} gate={name} bound_secs={bound}")
    return 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--name", required=True)
    parser.add_argument("--timeout-secs", required=True, type=_positive_seconds)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if not _NAME.fullmatch(args.name):
        parser.error("--name must contain only lowercase letters, digits, '_' or '-'")
    command = list(args.command)
    if command and command[0] == "--":
        command.pop(0)
    if not command:
        parser.error("a command is required after '--'")

    bound = _bound_text(args.timeout_secs)
    try:
        proc = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=os.name == "posix",
        )
    except OSError as exc:
        return _no_result(args.name, "launch-error", bound, str(exc))

    try:
        stdout, stderr = proc.communicate(timeout=args.timeout_secs)
    except subprocess.TimeoutExpired:
        _terminate_group(proc)
        return _no_result(args.name, "timeout", bound)
    except OSError as exc:
        _terminate_group(proc)
        return _no_result(args.name, "io-error", bound, str(exc))

    sys.stdout.buffer.write(stdout)
    sys.stdout.buffer.flush()
    sys.stderr.buffer.write(stderr)
    sys.stderr.buffer.flush()
    return proc.returncode


if __name__ == "__main__":
    raise SystemExit(main())
