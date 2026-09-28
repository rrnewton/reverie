"""Read validate's final status and require agreement with its exit code."""

from __future__ import annotations


FINAL_VALIDATE_STATUS_PREFIX = "FINAL_VALIDATE_STATUS: "
FINAL_VALIDATE_STATUS_EXIT_CODES = {
    "PASSED": 0,
    "FAILED": 1,
    "COULD_NOT_RUN": 75,
}


class FinalValidateStatusError(ValueError):
    """The final status is unknown or disagrees with the process exit."""


def read_final_validate_status(output: str) -> str | None:
    """Return the last status occurrence; absence remains a distinct fact.

    Validate itself emits exactly one status line and emits it last.  Output
    from wrappers, fixtures, or quoted documentation can share the channel and
    can contain an earlier lookalike, so readers deliberately take the last
    occurrence rather than trusting the first match.
    """
    values = [
        line.removeprefix(FINAL_VALIDATE_STATUS_PREFIX)
        for line in output.splitlines()
        if line.startswith(FINAL_VALIDATE_STATUS_PREFIX)
    ]
    if not values:
        return None
    status = values[-1]
    if status not in FINAL_VALIDATE_STATUS_EXIT_CODES:
        raise FinalValidateStatusError(f"unknown final validate status {status!r}")
    return status


def read_coupled_final_validate_status(output: str, exit_code: int | None) -> str | None:
    """Return the reported status only when the independent exit agrees."""
    status = read_final_validate_status(output)
    if status is None:
        return None
    require_final_validate_status_exit(status, exit_code)
    return status


def require_final_validate_status_exit(status: str, exit_code: int | None) -> None:
    """Reject a fixed status whose independent process exit disagrees."""
    expected = FINAL_VALIDATE_STATUS_EXIT_CODES[status]
    if exit_code != expected:
        rendered = "absent" if exit_code is None else str(exit_code)
        raise FinalValidateStatusError(
            f"final validate status {status} requires exit {expected}, got {rendered}"
        )
