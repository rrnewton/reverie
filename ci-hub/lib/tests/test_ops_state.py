import os
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "ci-hub" / "lib"))
sys.path.insert(0, str(ROOT / "ci-hub" / "health"))

import approval_binding  # noqa: E402
import ops_state  # noqa: E402
import unevaluated_head  # noqa: E402


def test_actions_policy_is_bracketed_by_the_local_flag(tmp_path: Path) -> None:
    state = tmp_path / ".ops-state.yaml"
    state.write_text("flags:\n  github_actions_enabled: false\n", encoding="utf-8")
    enabled, directive = ops_state.github_actions_policy(state)
    assert enabled is False
    assert directive == f"{state} flags.github_actions_enabled=false"

    state.write_text("flags:\n  github_actions_enabled: true\n", encoding="utf-8")
    assert ops_state.github_actions_policy(state) == (True, None)


def test_missing_state_preserves_the_fail_visible_default(tmp_path: Path) -> None:
    assert ops_state.github_actions_policy(tmp_path / "absent.yaml") == (True, None)


def test_invalid_flag_refuses_instead_of_disabling_the_check(tmp_path: Path) -> None:
    state = tmp_path / ".ops-state.yaml"
    state.write_text("flags:\n  github_actions_enabled: disabled\n", encoding="utf-8")
    try:
        ops_state.github_actions_policy(state)
    except ops_state.OpsStateError as error:
        assert "must be a boolean" in str(error)
    else:
        raise AssertionError("invalid flag was accepted")


def test_environment_selects_the_live_state_file(tmp_path: Path) -> None:
    state = tmp_path / ".ops-state.yaml"
    state.write_text("flags:\n  github_actions_enabled: false\n", encoding="utf-8")
    previous = os.environ.get(ops_state.PATH_ENV)
    try:
        os.environ[ops_state.PATH_ENV] = str(state)
        assert ops_state.github_actions_policy()[0] is False
    finally:
        if previous is None:
            os.environ.pop(ops_state.PATH_ENV, None)
        else:
            os.environ[ops_state.PATH_ENV] = previous


def test_parent_environment_selects_ops_state_when_direct_override_is_absent(
    tmp_path: Path,
) -> None:
    state_root = tmp_path / "state-root"
    state_root.mkdir()
    state = state_root / ".ops-state.yaml"
    state.write_text("flags:\n  github_actions_enabled: false\n", encoding="utf-8")
    previous_path = os.environ.pop(ops_state.PATH_ENV, None)
    previous_parent = os.environ.get(ops_state.PARENT_ENV)
    try:
        os.environ[ops_state.PARENT_ENV] = str(state_root)
        enabled, directive = ops_state.github_actions_policy()
        assert enabled is False
        assert directive == f"{state} flags.github_actions_enabled=false"
        assert approval_binding.github_actions_enabled("rrnewton/hermit") is False
        assert unevaluated_head.actions_policy("rrnewton/hermit") == (
            False,
            directive,
        )
    finally:
        if previous_path is not None:
            os.environ[ops_state.PATH_ENV] = previous_path
        if previous_parent is None:
            os.environ.pop(ops_state.PARENT_ENV, None)
        else:
            os.environ[ops_state.PARENT_ENV] = previous_parent


def test_both_tick_checkers_consult_the_same_local_flag(tmp_path: Path) -> None:
    state = tmp_path / ".ops-state.yaml"
    previous = os.environ.get(ops_state.PATH_ENV)
    try:
        os.environ[ops_state.PATH_ENV] = str(state)
        state.write_text("flags:\n  github_actions_enabled: false\n", encoding="utf-8")
        assert approval_binding.github_actions_enabled("rrnewton/hermit") is False
        enabled, directive = unevaluated_head.actions_policy("rrnewton/hermit")
        assert enabled is False
        assert "github_actions_enabled=false" in (directive or "")

        state.write_text("flags:\n  github_actions_enabled: true\n", encoding="utf-8")
        assert approval_binding.github_actions_enabled("rrnewton/hermit") is True
        assert unevaluated_head.actions_policy("rrnewton/hermit") == (True, None)
    finally:
        if previous is None:
            os.environ.pop(ops_state.PATH_ENV, None)
        else:
            os.environ[ops_state.PATH_ENV] = previous
