"""Explicit GitHub executable selection for validation and its callbacks."""

from __future__ import annotations

import os
from collections.abc import Mapping
from pathlib import Path


ENVIRONMENT_KEY = "FLEET_REAL_GH"
RECORD_KEY = "github_executable"


def executable(environment: Mapping[str, str]) -> str:
    value = environment.get(ENVIRONMENT_KEY)
    if (
        not isinstance(value, str)
        or not value
        or not Path(value).is_absolute()
        or not Path(value).is_file()
        or not os.access(value, os.X_OK)
    ):
        raise ValueError(
            "FLEET_REAL_GH must name an existing absolute executable; "
            "GitHub commands never fall back to gh on PATH"
        )
    return value


def command(*arguments: str) -> list[str]:
    return ["with-proxy", executable(os.environ), *arguments]


def recorded_environment(
    record: Mapping[str, object], environment: Mapping[str, str]
) -> dict[str, str]:
    """Restore launch selection for attach/retire; never adopt a changed route.

    Historical records without this field still require an explicit selection
    from their caller. That does not invent a historical executable identity.
    """
    result = dict(environment)
    if RECORD_KEY in record:
        recorded = record[RECORD_KEY]
        if not isinstance(recorded, str):
            raise ValueError("recorded github_executable must be a string")
        current = result.get(ENVIRONMENT_KEY)
        if current is not None and current != recorded:
            raise ValueError("FLEET_REAL_GH contradicts the recorded github_executable")
        result[ENVIRONMENT_KEY] = recorded
    executable(result)
    return result
