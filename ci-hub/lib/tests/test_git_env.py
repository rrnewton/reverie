#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest


MODULE_PATH = Path(__file__).resolve().parents[1] / "git_env.py"
SPEC = importlib.util.spec_from_file_location("git_env", MODULE_PATH)
assert SPEC and SPEC.loader
git_env = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(git_env)


def test_all_repository_selectors_are_removed_and_unrelated_values_survive() -> None:
    source = {name: f"wrong-{name}" for name in git_env.GIT_REPOSITORY_ENV}
    source["PATH"] = "/bin"

    child = git_env.sanitized_git_env(source)

    assert child == {"PATH": "/bin"}


def test_in_flight_index_requires_an_explicit_narrow_opt_in() -> None:
    source = {
        "GIT_INDEX_FILE": "/repo/.git/in-flight-index",
        "GIT_DIR": "/wrong/.git",
        "PATH": "/bin",
    }

    child = git_env.sanitized_git_env(source, inherit=("GIT_INDEX_FILE",))

    assert child["GIT_INDEX_FILE"] == source["GIT_INDEX_FILE"]
    assert "GIT_DIR" not in child
    assert child["PATH"] == "/bin"


def test_opt_in_refuses_names_outside_the_repository_selector_set() -> None:
    with pytest.raises(ValueError, match="non-repository"):
        git_env.sanitized_git_env({}, inherit=("PATH",))
