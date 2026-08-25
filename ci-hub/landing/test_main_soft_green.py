from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

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


def test_ci_hub_contact_warning_and_lander_soft_binding_are_wired() -> None:
    rust = (M.ROOT / "ci-hub/ci-hub.rs").read_text()
    main_start = rust.index("fn main() -> ExitCode")
    execute = rust.index("match execute(&root, cli.command)", main_start)
    warning = rust.index("warn_if_main_is_soft_green(&root);", main_start)
    cost = rust.index('env::var_os("CI_HUB_TOOL_COST_ACTIVE")', main_start)
    assert cost < warning < execute

    lander = (M.ROOT / "ci-hub/landing/land-pr.sh").read_text()
    # ⚠️ THIS PREVIOUSLY PINNED `land_and_arm.py" mark-soft`, WHICH WAS REMOVED ON
    # PURPOSE. f317f715 "Remove unrequested landing refusals" (2026-08-11) dropped
    # the pre-merge remediation vetoes, mark-soft among them, "while retaining
    # exact-head review and VALIDATE/LEDGER authority". So the assertion was
    # guarding a decision that had been reversed, and it never went red because
    # `ci-hub/landing` is not in run_python_suites.py's registry -- nothing has
    # run this file since.
    #
    # The removal is NOT reverted here and the guard is NOT deleted. The lander
    # still arms remediation, now through the chained `prepare && complete`, so
    # the ordering invariant is re-pinned against the wiring that exists: the
    # lock result is known before arming begins, and completion follows
    # preparation rather than standing alone.
    prepare = lander.index('land_and_arm.py" prepare')
    complete = lander.index('land_and_arm.py" complete')
    lock_result = lander.index("rc=$?")
    assert lock_result < prepare, "the lander arms remediation before it knows the lock result"
    assert prepare < complete, "completion must follow preparation, not precede it"
    # `mark-soft` itself is still IMPLEMENTED in land_and_arm.py with no caller in
    # the lander. That is a live subcommand with no invoker, which is worth an
    # owner decision (revive or retire) and is deliberately not decided here.
