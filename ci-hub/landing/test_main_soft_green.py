from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
from unittest import mock

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import main_soft_green as M  # noqa: E402


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()


def fixture(tmp_path: Path) -> tuple[Path, str, str, str, Path, Path]:
    root = tmp_path / "parent"
    repo = root / "hermit"
    repo.mkdir(parents=True)
    git(repo, "init", "-q")
    git(repo, "config", "user.name", "fixture")
    git(repo, "config", "user.email", "fixture@example.com")
    (repo / "file").write_text("base\n")
    git(repo, "add", "file")
    git(repo, "commit", "-qm", "base")
    source = git(repo, "rev-parse", "HEAD")
    (repo / "file").write_text("candidate\n")
    git(repo, "commit", "-qam", "candidate")
    candidate = git(repo, "rev-parse", "HEAD")
    tree = git(repo, "rev-parse", f"{candidate}^{{tree}}")
    landed = subprocess.run(
        ["git", "-C", str(repo), "commit-tree", tree, "-p", source],
        input="landed\n", check=True, text=True, stdout=subprocess.PIPE,
    ).stdout.strip()
    git(repo, "update-ref", "refs/remotes/origin/main", landed)
    rebase_store = root / "ignored/rebase-records.jsonl"
    rebase_store.parent.mkdir(parents=True)
    rebase_store.write_text(json.dumps({
        "source_rev": source,
        "base": source,
        "result": candidate,
        "soft_green": "soft-green(zero-conflict)",
    }) + "\n")
    store = root / "ignored/ci-hub/main-soft-green.jsonl"
    return root, source, candidate, landed, rebase_store, store


def test_exact_soft_main_launches_before_recording_and_warns_until_hard(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, source, candidate, landed, rebase_store, store = fixture(tmp_path)
    monkeypatch.setenv("CI_HUB_SOFT_MAIN_CHECKOUT", str(root / "hermit"))
    monkeypatch.setattr(M, "canonical_state", lambda _root, _sha: ("NO_RESULT", "absent"))
    observed: list[tuple[str, bool]] = []

    def spawn(_root: Path, **kwargs) -> int:
        # Mutation control: deleting the launch makes this exact assertion fail;
        # a record-only implementation is not the mechanism the owner requested.
        observed.append((kwargs["sha"], store.exists()))
        return 4321

    record = M.record_and_launch(
        root=root, landed_sha=landed, candidate=candidate,
        source_rev=source, base=source, agent="fixture",
        store=store, rebase_store=rebase_store, spawn=spawn,
    )
    assert observed == [(landed, False)]
    assert record["launched"] is True
    state = M.current_main_state(root, store=store, checkout=root / "hermit")
    assert state["state"] == "SOFT_GREEN"
    assert "SOFT_GREEN, not HARD_GREEN" in M.warning_text(state)

    monkeypatch.setattr(M, "canonical_state", lambda _root, _sha: ("HARD_GREEN", "receipt"))
    hard = M.current_main_state(root, store=store, checkout=root / "hermit")
    assert hard["state"] == "HARD_GREEN"
    assert M.warning_text(hard) is None


def test_different_landed_tree_refuses_without_launch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, source, candidate, _landed, rebase_store, store = fixture(tmp_path)
    repo = root / "hermit"
    (repo / "other").write_text("different\n")
    git(repo, "add", "other")
    git(repo, "commit", "-qm", "different landed tree")
    different = git(repo, "rev-parse", "HEAD")
    git(repo, "update-ref", "refs/remotes/origin/main", different)
    monkeypatch.setenv("CI_HUB_SOFT_MAIN_CHECKOUT", str(repo))
    launched: list[str] = []
    with pytest.raises(M.SoftMainError, match="tree differs"):
        M.record_and_launch(
            root=root, landed_sha=different, candidate=candidate,
            source_rev=source, base=source, agent="fixture",
            store=store, rebase_store=rebase_store,
            spawn=lambda _root, **kwargs: launched.append(kwargs["sha"]) or 9,
        )
    assert launched == []
    assert not store.exists()


def test_unknown_never_prints_a_plausible_soft_or_hard_state(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, _source, _candidate, landed, _rebase_store, store = fixture(tmp_path)
    store.parent.mkdir(parents=True, exist_ok=True)
    store.write_text(json.dumps({
        "schema_version": 1, "state": "SOFT_GREEN", "commit": landed,
    }) + "\n")
    monkeypatch.setattr(M, "canonical_state", lambda _root, _sha: ("UNKNOWN", "unreadable"))
    state = M.current_main_state(root, store=store, checkout=root / "hermit")
    assert state["state"] == "UNKNOWN"
    assert M.warning_text(state) is None


def test_ci_hub_contact_warning_is_wired() -> None:
    rust = (M.ROOT / "ci-hub/ci-hub.rs").read_text()
    main_start = rust.index("fn main() -> ExitCode")
    execute = rust.index("match execute(&root, cli.command)", main_start)
    warning = rust.index("warn_if_main_is_soft_green(&root);", main_start)
    cost = rust.index('env::var_os("CI_HUB_TOOL_COST_ACTIVE")', main_start)
    assert cost < warning < execute


@pytest.mark.parametrize("explicit_parent", [False, True])
def test_canonical_state_uses_native_default_artifact_store(tmp_path, monkeypatch, explicit_parent):
    tool = tmp_path / "worktrees/slots/tool"
    tool.mkdir(parents=True)
    state = tmp_path / "operational-state"
    state.mkdir()
    monkeypatch.delenv("CI_HUB_SOFT_MAIN_VALIDATE_STATUS", raising=False)
    if explicit_parent:
        monkeypatch.setenv("DEV_HERMIT_PARENT", str(state))
    else:
        monkeypatch.delenv("DEV_HERMIT_PARENT", raising=False)
    sha = "a" * 40
    row = {"commit": sha, "repo": "hermit"}
    output = subprocess.CompletedProcess([], 0, json.dumps(row) + "\n", "")
    with mock.patch.object(M, "_run", return_value=output), \
         mock.patch.object(M.qualifying_receipt, "validation_evidence_batch") as batch, \
         mock.patch.object(M.qualifying_receipt, "row_qualification", return_value=(True, "qualifies")):
        assert M.canonical_state(tool, sha)[0] == "HARD_GREEN"
    assert batch.call_args.args == ([row],)
    assert "artifact_root" not in batch.call_args.kwargs
