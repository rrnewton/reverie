#!/usr/bin/env python3
"""`_capture` must honour `check`, because it advertises `run`'s return type.

⚠️ THE REGRESSION THIS PINS WAS A HARD, DETERMINISTIC MAIN-RED THAT READ AS A
FLAKE. `_capture` replaced `subprocess.run` to fix a capture hang, kept run's
SHAPE -- it returns a `CompletedProcess` -- and forwarded `**kwargs` straight to
`Popen`. `check` is a `run` keyword that `Popen` rejects, so all three existing
callers, every one of which passes `check=False`, died with

    TypeError: Popen.__init__() got an unexpected keyword argument 'check'

Measured 2026-08-25 at dev-hermit main 283f0d95:
`tests::python_publisher_and_rust_verifier_share_host_bound_run_identity` failed
5 runs out of 5 in isolation, and had been counted inside a twelve-test "flake
cluster" whose other members genuinely do vary. A deterministic failure sitting
in a flake bucket is the expensive part: the bucket's shifting membership
explains away the one member that never moves.

The fix implements the keyword rather than deleting it from the callers, because
a helper that returns a `CompletedProcess` is one a caller may reasonably drive
like `run`.
"""
import importlib.util
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "qr_capture", ROOT / "ci-hub" / "qualifying_receipt.py"
)
qr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qr)

TRUE = [sys.executable, "-c", "import sys; sys.exit(0)"]
FALSE = [sys.executable, "-c", "import sys; sys.exit(3)"]


def test_check_false_is_accepted_and_does_not_raise():
    """The exact call shape that was raising TypeError on every invocation."""
    result = qr._capture(FALSE, check=False)
    assert result.returncode == 3


def test_check_is_accepted_on_a_successful_command():
    assert qr._capture(TRUE, check=True).returncode == 0


def test_check_true_raises_on_failure_like_subprocess_run():
    """Honouring the keyword means honouring what it MEANS, not just tolerating it.

    Swallowing `check=True` would be worse than the TypeError: the caller asked
    to be told about a nonzero exit and would silently not be.
    """
    try:
        qr._capture(FALSE, check=True)
    except subprocess.CalledProcessError as exc:
        assert exc.returncode == 3
    else:
        raise AssertionError("check=True did not raise on a nonzero exit")


def test_omitting_check_still_defaults_to_not_raising():
    assert qr._capture(FALSE).returncode == 3


def test_output_is_still_captured_and_decoded():
    """The hang fix must survive: stdout/stderr still come back as text."""
    out = qr._capture(
        [sys.executable, "-c", "import sys; print('o'); print('e', file=sys.stderr)"]
    )
    assert out.stdout.strip() == "o"
    assert out.stderr.strip() == "e"


if __name__ == "__main__":  # pragma: no cover
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"PASS {name}")
            except AssertionError as exc:
                failures += 1
                print(f"FAIL {name}: {exc}")
    sys.exit(1 if failures else 0)
