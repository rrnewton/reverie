"""Read dev-hermit's gitignored per-host operations state."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

import yaml


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PATH = ROOT / ".ops-state.yaml"
PATH_ENV = "DEV_HERMIT_OPS_STATE"
PARENT_ENV = "DEV_HERMIT_PARENT"


class OpsStateError(ValueError):
    """The local operations state exists but cannot support a safe decision."""


def path_from_environment() -> Path:
    override = os.environ.get(PATH_ENV)
    if override:
        return Path(override)
    parent = os.environ.get(PARENT_ENV)
    return Path(parent) / ".ops-state.yaml" if parent else DEFAULT_PATH


def github_actions_policy(path: Path | None = None) -> tuple[bool, str | None]:
    """Return the configured Actions policy and its disabling authority.

    Missing state or a missing flag preserves the historical Actions-on
    behaviour. Invalid state refuses instead of silently disabling a warning.
    """
    state_path = path or path_from_environment()
    try:
        raw: Any = yaml.safe_load(state_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return True, None
    except (OSError, yaml.YAMLError) as error:
        raise OpsStateError(f"cannot read {state_path}: {error}") from error

    if not isinstance(raw, dict):
        raise OpsStateError(f"{state_path}: top level must be a mapping")
    flags = raw.get("flags", {})
    if not isinstance(flags, dict):
        raise OpsStateError(f"{state_path}: flags must be a mapping")
    enabled = flags.get("github_actions_enabled", True)
    if type(enabled) is not bool:
        raise OpsStateError(
            f"{state_path}: flags.github_actions_enabled must be a boolean"
        )
    directive = (
        f"{state_path} flags.github_actions_enabled=false" if not enabled else None
    )
    return enabled, directive
