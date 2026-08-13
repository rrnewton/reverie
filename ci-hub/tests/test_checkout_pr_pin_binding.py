#!/usr/bin/env python3
"""Regression checks for target-repository binding in PR pin preparation."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKOUT_HELPER = ROOT / "scripts/checkout-hermit-pr-latest-reverie.sh"
HERMIT = ROOT / "hermit"
CHECKER = HERMIT / "scripts/check-reverie-pin.rs"


def _print_pin_invocations(source: str) -> list[str]:
    return [line.strip() for line in source.splitlines() if "--print-pin" in line]


def _invocation_binds_target_repo(invocation: str) -> bool:
    return '--repo "$repo"' in invocation


def _has_complete_task_plumbing(source: str) -> bool:
    return (
        "--task <stable-task-id>" in source
        and '--task "$task"' in source
        and source.count('-m "Task: $task"') == 1
    )


def _validator_uses_requested_agent(source: str) -> bool:
    validate_block = source.split('"$ci_hub" validate-run', 1)[-1]
    return '--agent "$agent"' in validate_block and "--agent hermit-ptw" not in source


def _preparation_calls_read_only_pin_check(source: str) -> bool:
    return source.count('selected_pin=$(check_selected_pin "$repo" "$target")') == 1


def _write_executable(path: Path, source: str) -> None:
    path.write_text(source)
    path.chmod(0o755)


def _git(repo: Path, *args: str, capture: bool = False) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=capture,
        text=True,
    )
    return result.stdout.strip() if capture else ""


def _create_pr_preparation_fixture(
    tmp_path: Path, selected_pin: str
) -> tuple[Path, Path, Path, dict[str, str]]:
    lagging_pin = "1" * 40
    advanced_pin = "2" * 40
    assert selected_pin in {lagging_pin, advanced_pin}

    agent_utils = tmp_path / "agent-utils-source"
    (agent_utils / "rs/safe-ci-dag-runner").mkdir(parents=True)
    (agent_utils / "rs/safe-ci-dag-runner/Cargo.toml").write_text(
        '[package]\nname = "fixture"\nversion = "0.0.0"\n'
    )
    subprocess.run(["git", "init", "-q", "-b", "main", str(agent_utils)], check=True)
    _git(agent_utils, "config", "user.name", "Test")
    _git(agent_utils, "config", "user.email", "test@example.com")
    _git(agent_utils, "add", "rs/safe-ci-dag-runner/Cargo.toml")
    _git(agent_utils, "commit", "-qm", "fixture agent-utils")

    seed = tmp_path / "hermit-seed"
    scripts = seed / "scripts"
    scripts.mkdir(parents=True)
    checker = scripts / "check-reverie-pin.rs"
    _write_executable(
        checker,
        """#!/usr/bin/env bash
set -euo pipefail
repo=.
print_pin=false
while (($#)); do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --print-pin) print_pin=true; shift ;;
    *) shift ;;
  esac
done
if $print_pin; then
  cat "$repo/reverie-pin"
  exit 0
fi
pin=$(cat "$repo/reverie-pin")
grep -Fx "$pin" "$repo/allowed-pins" >/dev/null || exit 9
if [[ -n ${CHECKER_MUTATE_TO:-} ]]; then
  printf '%s\\n' "$CHECKER_MUTATE_TO" > "$repo/reverie-pin"
fi
""",
    )
    (seed / "reverie-pin").write_text(f"{lagging_pin}\n")
    (seed / "allowed-pins").write_text(f"{lagging_pin}\n{advanced_pin}\n")
    subprocess.run(["git", "init", "-q", "-b", "main", str(seed)], check=True)
    _git(seed, "config", "user.name", "Test")
    _git(seed, "config", "user.email", "test@example.com")
    _git(seed, "add", "scripts", "reverie-pin", "allowed-pins")
    _git(seed, "commit", "-qm", "fixture base")
    subprocess.run(
        [
            "git",
            "-C",
            str(seed),
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            str(agent_utils),
            "agent-utils",
        ],
        check=True,
    )
    _git(seed, "commit", "-qam", "fixture validation dependency")

    _git(seed, "switch", "-qc", "feature")
    if selected_pin == advanced_pin:
        (seed / "reverie-pin").write_text(f"{advanced_pin}\n")
        _git(seed, "add", "reverie-pin")
        _git(seed, "commit", "-qm", "Advance pin for fixture reason")
    (seed / "feature-change").write_text("feature\n")
    _git(seed, "add", "feature-change")
    _git(seed, "commit", "-qm", "fixture feature")
    feature_head = _git(seed, "rev-parse", "HEAD", capture=True)

    _git(seed, "switch", "-q", "main")
    (seed / "main-change").write_text("main\n")
    _git(seed, "add", "main-change")
    _git(seed, "commit", "-qm", "fixture main")

    remote = tmp_path / "rrnewton/hermit.git"
    remote.parent.mkdir()
    subprocess.run(["git", "clone", "-q", "--bare", str(seed), str(remote)], check=True)
    target = tmp_path / "target"
    subprocess.run(["git", "clone", "-q", str(remote), str(target)], check=True)
    _git(target, "config", "user.name", "Test")
    _git(target, "config", "user.email", "test@example.com")

    workspace = tmp_path / "workspace"
    (workspace / "scripts").mkdir(parents=True)
    (workspace / "ci-hub").mkdir()
    helper = workspace / "scripts/checkout-hermit-pr-latest-reverie.sh"
    helper.symlink_to(CHECKOUT_HELPER)
    _write_executable(
        workspace / "scripts/check_commit_attribution.py",
        "#!/usr/bin/env bash\nprintf 'fixture-host\\n'\n",
    )
    validate_record = tmp_path / "validate-arguments"
    _write_executable(
        workspace / "ci-hub/ci-hub",
        f"#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" > {validate_record}\n",
    )

    proxy_bin = tmp_path / "bin"
    proxy_bin.mkdir()
    _write_executable(proxy_bin / "with-proxy", '#!/usr/bin/env bash\nexec "$@"\n')
    _write_executable(
        proxy_bin / "gh",
        "#!/usr/bin/env bash\n"
        f'printf \'%s\\n\' \'{{"state":"OPEN","isDraft":false,'
        f'"baseRefName":"main","headRefName":"feature",'
        f'"headRefOid":"{feature_head}",'
        '"headRepositoryOwner":{"login":"rrnewton"}}\'\n',
    )
    env = os.environ.copy()
    env.pop("TARGET_SHA", None)
    env["PATH"] = f"{proxy_bin}:{env['PATH']}"
    env["GIT_ALLOW_PROTOCOL"] = "file"
    return helper, target, validate_record, env


def _run_pr_preparation(
    helper: Path,
    target: Path,
    env: dict[str, str],
    *,
    expected_pin: str | None = None,
) -> subprocess.CompletedProcess[str]:
    command = [
        str(helper),
        "--foreground",
        "--repo",
        str(target),
        "--model",
        "fixture",
        "--agent",
        "hermit-coord",
        "--task",
        "pr-prep-script-forces-pin-to-reverie-tip",
        "1",
    ]
    if expected_pin is not None:
        command[1:1] = ["--target-sha", expected_pin]
    return subprocess.run(
        command,
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )


def test_checkout_helper_binds_print_pin_to_target_repo() -> None:
    source = CHECKOUT_HELPER.read_text()
    invocations = _print_pin_invocations(source)

    assert len(invocations) == 2
    assert all(_invocation_binds_target_repo(invocation) for invocation in invocations)
    assert not _invocation_binds_target_repo(
        invocations[0].replace('--repo "$repo"', "")
    )


def test_checkout_helper_threads_task_trailers_through_every_commit() -> None:
    source = CHECKOUT_HELPER.read_text()

    assert _has_complete_task_plumbing(source)
    assert not _has_complete_task_plumbing(source.replace('-m "Task: $task"', "", 1))
    assert not _has_complete_task_plumbing(source.replace('--task "$task"', "", 1))


def test_checkout_helper_never_requests_an_automatic_pin_update() -> None:
    source = CHECKOUT_HELPER.read_text()

    assert "--update-to-latest" not in source
    assert "github.com/rrnewton/reverie.git refs/heads/main" not in source


def test_pr_preparation_calls_read_only_pin_check() -> None:
    source = CHECKOUT_HELPER.read_text()
    mutated = source.replace(
        'selected_pin=$(check_selected_pin "$repo" "$target")',
        'selected_pin="$target"',
    )

    assert _preparation_calls_read_only_pin_check(source)
    assert mutated != source
    assert not _preparation_calls_read_only_pin_check(mutated)


def test_pr_preparation_accepts_valid_lagging_pin_unchanged(tmp_path: Path) -> None:
    lagging_pin = "1" * 40
    helper, target, validate_record, env = _create_pr_preparation_fixture(
        tmp_path, lagging_pin
    )

    result = _run_pr_preparation(helper, target, env)

    assert result.returncode == 0, result.stderr
    assert f"PIN_SHA={lagging_pin}" in result.stdout
    assert (target / "reverie-pin").read_text() == f"{lagging_pin}\n"
    assert _git(target, "status", "--porcelain", capture=True) == ""
    assert validate_record.read_text().startswith("validate-run ")


def test_pr_preparation_accepts_deliberate_committed_advance_unchanged(
    tmp_path: Path,
) -> None:
    advanced_pin = "2" * 40
    helper, target, validate_record, env = _create_pr_preparation_fixture(
        tmp_path, advanced_pin
    )

    result = _run_pr_preparation(helper, target, env, expected_pin=advanced_pin)

    assert result.returncode == 0, result.stderr
    assert f"PIN_SHA={advanced_pin}" in result.stdout
    assert (target / "reverie-pin").read_text() == f"{advanced_pin}\n"
    assert _git(target, "status", "--porcelain", capture=True) == ""
    assert validate_record.read_text().startswith("validate-run ")


def test_pin_change_during_preparation_is_refused(tmp_path: Path) -> None:
    selected_pin = "1" * 40
    substituted_pin = "2" * 40
    helper, target, validate_record, env = _create_pr_preparation_fixture(
        tmp_path, selected_pin
    )
    env["CHECKER_MUTATE_TO"] = substituted_pin

    result = _run_pr_preparation(helper, target, env)

    assert result.returncode != 0
    assert "pin check changed the worktree" in result.stderr
    assert not validate_record.exists()


def test_checkout_helper_binds_validation_to_requested_agent() -> None:
    source = CHECKOUT_HELPER.read_text()
    mutated = source.replace(
        '--agent "$agent" \\\n    --target "$candidate"',
        '--agent hermit-ptw \\\n    --target "$candidate"',
    )

    assert _validator_uses_requested_agent(source)
    assert mutated != source
    assert not _validator_uses_requested_agent(mutated)


def test_checker_repo_binding_works_from_parent_cwd() -> None:
    # Exercise the checker's real, network-free API from outside the Hermit
    # checkout.  The explicit --repo binding must expose the tracked pin; an
    # unbound invocation from the parent must not accidentally expose that pin.
    lock = (HERMIT / "Cargo.lock").read_text()
    pins = set(
        re.findall(
            r"github\.com/rrnewton/reverie(?:\.git)?\?rev=([0-9a-f]{40})",
            lock,
        )
    )
    assert len(pins) == 1
    pin = pins.pop()
    bound = subprocess.run(
        [str(CHECKER), "--repo", str(HERMIT), "--print-pin"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    unbound = subprocess.run(
        [str(CHECKER), "--print-pin"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    before_status = _git(HERMIT, "status", "--porcelain", capture=True)
    checked = subprocess.run(
        [str(CHECKOUT_HELPER), "--check-pin", "--repo", str(HERMIT)],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    after_status = _git(HERMIT, "status", "--porcelain", capture=True)

    assert bound.returncode == 0, bound.stderr
    assert bound.stdout.strip() == pin
    assert unbound.stdout.strip() != pin
    assert checked.returncode == 0, checked.stderr
    assert checked.stdout.strip() == pin
    assert before_status == after_status == ""
