#!/usr/bin/env python3
"""A tree's removability must come from evidence, never from its name."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import tree_disposition as td  # noqa: E402


def real_run(args, check=False):
    return subprocess.run(args, capture_output=True, text=True)


def git(tree: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-C", str(tree), *args],
        check=True,
        capture_output=True,
        text=True,
    )


def make_repo(path: Path) -> None:
    """A tree whose head IS on a remote, which is the ordinary clean state.

    The remote is not decoration. A repository with no remote at all has every
    commit on no remote ref, so the module correctly calls it protected -- work
    that exists in exactly one place is the thing this check exists to save. A
    fixture without an origin therefore cannot express "clean", which is what
    the first version of these tests got wrong.
    """

    origin = path.parent / f"{path.name}.origin.git"
    subprocess.run(
        ["git", "init", "-q", "--bare", "-b", "main", str(origin)],
        check=True, capture_output=True, text=True,
    )
    path.mkdir(parents=True, exist_ok=True)
    git(path, "init", "-q", "-b", "main")
    git(path, "config", "user.email", "t@example.invalid")
    git(path, "config", "user.name", "t")
    (path / "seed").write_text("seed\n")
    git(path, "add", "seed")
    git(path, "commit", "-qm", "seed")
    git(path, "remote", "add", "origin", str(origin))
    git(path, "push", "-q", "origin", "main")


def register(root: Path, slot: str, kind: str, tree: Path) -> None:
    """Write one registry row, the way wrkslots records a created slot."""

    control = root / "worktrees"
    control.mkdir(parents=True, exist_ok=True)
    path = control / "ACTIVE.testmachine.json"
    document = json.loads(path.read_text()) if path.exists() else {
        "machine": "testmachine",
        "schema": 2,
        "slots": [],
    }
    document["slots"].append(
        {
            "slot": slot,
            "slot_type": kind,
            "checkouts": [{"name": "checkout", "path": str(tree)}],
        }
    )
    path.write_text(json.dumps(document))


def test_recorded_validate_with_no_authored_work_is_disposable(tmp_path):
    tree = tmp_path / "trees" / "anything"
    make_repo(tree)
    register(tmp_path, "s", td.KIND_VALIDATE, tree)
    verdict, reason = td.disposition(tmp_path, tree, run=real_run)
    assert verdict == td.DISPOSABLE, reason


def test_recorded_agent_is_protected_however_clean(tmp_path):
    tree = tmp_path / "trees" / "anything"
    make_repo(tree)
    register(tmp_path, "s", td.KIND_AGENT, tree)
    verdict, reason = td.disposition(tmp_path, tree, run=real_run)
    assert verdict == td.PROTECTED
    assert "agent" in reason


def test_the_two_kinds_are_distinguishable_with_identical_names(tmp_path):
    """THE POINT OF THE WHOLE MODULE: the verdict cannot come from the path.

    Both trees are named `checkout` and sit at the same depth under sibling
    parents, so every path-derived rule -- leaf name, prefix, parent directory --
    gives them the same answer. Only the recorded kind separates them.
    """

    disposable = tmp_path / "one" / "checkout"
    owned = tmp_path / "two" / "checkout"
    make_repo(disposable)
    make_repo(owned)
    register(tmp_path, "a", td.KIND_VALIDATE, disposable)
    register(tmp_path, "b", td.KIND_AGENT, owned)

    assert disposable.name == owned.name
    assert td.disposition(tmp_path, disposable, run=real_run)[0] == td.DISPOSABLE
    assert td.disposition(tmp_path, owned, run=real_run)[0] == td.PROTECTED


def test_a_validate_named_tree_is_not_disposable_without_a_record(tmp_path):
    """The name that used to authorise removal now authorises nothing."""

    tree = tmp_path / "trees" / "validate-fresh-deadbeef-1-2"
    make_repo(tree)
    verdict, reason = td.disposition(tmp_path, tree, run=real_run)
    assert verdict == td.PROTECTED
    assert "no registry record" in reason


def test_unregistered_tree_is_unknown_and_therefore_protected(tmp_path):
    tree = tmp_path / "trees" / "anything"
    make_repo(tree)
    verdict, reason = td.disposition(tmp_path, tree, run=real_run)
    assert verdict == td.PROTECTED
    assert "unregistered" in reason or "no registry record" in reason


def test_a_commit_off_every_remote_ref_asks_for_rescue_not_refusal(tmp_path):
    """The NORMAL state of a validate checkout, so it must not read as a refusal.

    A tree made to validate a pull-request head sits at a commit that is not on
    main; after the squash landing it is on no remote ref forever. Measured
    across the real population, three trees were in exactly this state with zero
    `commit:` reflog entries -- nobody had authored anything in them. Refusing
    here would stall on the ordinary case. The commit is addressable, so the
    answer is "preserve it first", not "never".
    """

    tree = tmp_path / "trees" / "anything"
    make_repo(tree)
    (tree / "authored").write_text("work\n")
    git(tree, "add", "authored")
    git(tree, "commit", "-qm", "a head that is on no remote ref")
    register(tmp_path, "s", td.KIND_VALIDATE, tree)
    verdict, reason = td.disposition(tmp_path, tree, run=real_run)
    assert verdict == td.DISPOSABLE_AFTER_RESCUE, reason
    assert "rescue ref" in reason


def test_uncommitted_work_refuses_even_though_a_commit_would_not(tmp_path):
    """The asymmetry is the point: only an uncommitted path is unrecoverable."""

    committed = tmp_path / "trees" / "committed"
    uncommitted = tmp_path / "trees" / "uncommitted"
    for tree in (committed, uncommitted):
        make_repo(tree)
    git(committed, "commit", "-qm", "off-remote head", "--allow-empty")
    (uncommitted / "scratch").write_text("exists nowhere else\n")
    register(tmp_path, "a", td.KIND_VALIDATE, committed)
    register(tmp_path, "b", td.KIND_VALIDATE, uncommitted)

    assert td.disposition(tmp_path, committed, run=real_run)[0] == td.DISPOSABLE_AFTER_RESCUE
    assert td.disposition(tmp_path, uncommitted, run=real_run)[0] == td.PROTECTED


def test_recorded_validate_with_a_dirty_tree_is_protected(tmp_path):
    tree = tmp_path / "trees" / "anything"
    make_repo(tree)
    (tree / "scratch").write_text("uncommitted\n")
    register(tmp_path, "s", td.KIND_VALIDATE, tree)
    verdict, reason = td.disposition(tmp_path, tree, run=real_run)
    assert verdict == td.PROTECTED
    assert "uncommitted" in reason


def test_unreadable_tree_state_protects_rather_than_permits(tmp_path):
    """A failure to measure must never read as "no authored work"."""

    tree = tmp_path / "trees" / "anything"
    make_repo(tree)
    register(tmp_path, "s", td.KIND_VALIDATE, tree)

    def refusing_run(args, check=False):
        return subprocess.CompletedProcess(args, 128, "", "fatal: not a git repository")

    verdict, reason = td.disposition(tmp_path, tree, run=refusing_run)
    assert verdict == td.PROTECTED
    assert "could not be read" in reason


def test_an_unreadable_registry_does_not_become_a_permit(tmp_path):
    tree = tmp_path / "trees" / "anything"
    make_repo(tree)
    control = tmp_path / "worktrees"
    control.mkdir(parents=True, exist_ok=True)
    (control / "ACTIVE.broken.json").write_text("{not json")
    verdict, _reason = td.disposition(tmp_path, tree, run=real_run)
    assert verdict == td.PROTECTED


def test_a_renamed_tree_keeps_its_verdict(tmp_path):
    """Renaming the leaf cannot change the answer, which is the original defect."""

    tree = tmp_path / "trees" / "validate-fresh-aaaa-1-2"
    make_repo(tree)
    register(tmp_path, "s", td.KIND_AGENT, tree)
    before = td.disposition(tmp_path, tree, run=real_run)[0]
    renamed = tree.parent / "kept-work"
    tree.rename(renamed)
    # The registry still points at the old path, so the renamed tree is now
    # unregistered -- and unregistered protects. Either way it is not deletable.
    after = td.disposition(tmp_path, renamed, run=real_run)[0]
    assert before == td.PROTECTED
    assert after == td.PROTECTED


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
