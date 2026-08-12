#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

MODULE_PATH = Path(__file__).resolve().parents[1] / "fetch_ref.py"
SPEC = importlib.util.spec_from_file_location("fetch_ref", MODULE_PATH)
assert SPEC and SPEC.loader
fetch_ref = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = fetch_ref
SPEC.loader.exec_module(fetch_ref)


def git(repo: Path, *args: str) -> str:
    run = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=True
    )
    return run.stdout.strip()


class Fixture:
    def __init__(self, root: Path) -> None:
        self.remote = root / "remote.git"
        self.publisher = root / "publisher"
        self.consumer = root / "consumer"
        subprocess.run(["git", "init", "--bare", "-q", str(self.remote)], check=True)
        subprocess.run(["git", "init", "-q", str(self.publisher)], check=True)
        git(self.publisher, "config", "user.email", "fetch-ref@example.com")
        git(self.publisher, "config", "user.name", "Fetch Ref Test")
        git(self.publisher, "remote", "add", "origin", str(self.remote))
        self.commits: list[str] = []
        self.commit("base")
        git(self.publisher, "branch", "-M", "main")
        git(self.publisher, "push", "-q", "-u", "origin", "main")

        subprocess.run(["git", "init", "-q", str(self.consumer)], check=True)
        git(self.consumer, "remote", "add", "origin", str(self.remote))
        self.fetch()

    def commit(self, label: str, *, orphan: bool = False) -> str:
        if orphan:
            git(self.publisher, "switch", "--orphan", f"orphan-{label}")
            for path in self.publisher.iterdir():
                if path.name != ".git" and path.is_file():
                    path.unlink()
        path = self.publisher / "value"
        path.write_text(label + "\n")
        git(self.publisher, "add", "value")
        git(self.publisher, "commit", "-qm", label)
        oid = git(self.publisher, "rev-parse", "HEAD")
        self.commits.append(oid)
        return oid

    def push_main(self) -> None:
        git(self.publisher, "push", "-q", "origin", "HEAD:main")

    def fetch(self, **kwargs):
        return fetch_ref.fetch_remote_tracking_branch(
            self.consumer, use_proxy=False, sleep=lambda _: None, **kwargs
        )

    def tracking(self) -> str:
        return git(self.consumer, "rev-parse", "refs/remotes/origin/main")

    def fetch_head_path(self) -> Path:
        path = Path(git(self.consumer, "rev-parse", "--git-path", "FETCH_HEAD"))
        if not path.is_absolute():
            path = self.consumer / path
        return path.resolve()

    def set_tracking(self, oid: str) -> None:
        git(self.consumer, "update-ref", "refs/remotes/origin/main", oid)

    def assert_no_temporary_refs(self) -> None:
        assert git(self.consumer, "for-each-ref", "--format=%(refname)", "refs/ci-hub/fetch") == ""


def test_uncontended_fetch_advances_tracking_ref(tmp_path: Path) -> None:
    fx = Fixture(tmp_path)
    old = fx.tracking()
    fetch_head = fx.fetch_head_path()
    assert fetch_head == (fx.consumer / ".git" / "FETCH_HEAD").resolve()
    fetch_head.write_text("caller-owned sentinel\n")
    new = fx.commit("new")
    fx.push_main()
    result = fx.fetch()
    assert (result.previous_sha, result.fetched_sha, result.tracking_sha) == (old, new, new)
    assert result.disposition == "advanced"
    assert result.cas_retries == 0
    # FETCH_HEAD is repository-global across linked worktrees too.  The helper
    # must not trade the tracking-ref race for a second shared-file race.
    assert fetch_head.read_text() == "caller-owned sentinel\n"
    fx.assert_no_temporary_refs()


def test_ambient_git_dir_cannot_redirect_fetch(tmp_path: Path, monkeypatch) -> None:
    fx = Fixture(tmp_path)
    wrong = tmp_path / "wrong"
    subprocess.run(["git", "init", "-q", str(wrong)], check=True)
    git(wrong, "config", "user.name", "Wrong Object Store")
    git(wrong, "config", "user.email", "wrong@example.invalid")
    (wrong / "wrong.txt").write_text("wrong\n")
    git(wrong, "add", "wrong.txt")
    git(wrong, "commit", "-qm", "wrong")
    wrong_head = git(wrong, "rev-parse", "HEAD")

    ambient = dict(os.environ)
    ambient["GIT_DIR"] = str(wrong / ".git")
    unsafe = subprocess.run(
        ["git", "-C", str(fx.consumer), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
        env=ambient,
    )
    assert unsafe.stdout.strip() == wrong_head

    new = fx.commit("ambient-safe")
    fx.push_main()
    monkeypatch.setenv("GIT_DIR", str(wrong / ".git"))
    result = fx.fetch()
    monkeypatch.delenv("GIT_DIR")

    assert result.fetched_sha == new
    assert result.tracking_sha == new == fx.tracking()
    fx.assert_no_temporary_refs()


def test_cli_reports_the_installed_tracking_tip(tmp_path: Path) -> None:
    fx = Fixture(tmp_path)
    new = fx.commit("cli-new")
    fx.push_main()
    run = subprocess.run(
        [
            sys.executable,
            str(MODULE_PATH),
            "--repo",
            str(fx.consumer),
            "--no-proxy",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    result = json.loads(run.stdout)
    assert result["fetched_sha"] == new
    assert result["tracking_sha"] == new == fx.tracking()
    assert result["disposition"] == "advanced"
    fx.assert_no_temporary_refs()


def test_stale_fetcher_keeps_newer_tip_installed_by_second_fetcher(tmp_path: Path) -> None:
    fx = Fixture(tmp_path)
    first_tip = fx.commit("first")
    fx.push_main()
    observed: list[str] = []

    def second_writer(attempt: int, expected: str | None, fetched: str) -> None:
        assert attempt == 0
        assert fetched == first_tip
        # The unique-ref network fetch has NOT touched the shared tracking ref.
        assert fx.tracking() == expected
        second_tip = fx.commit("second")
        fx.push_main()
        second = fx.fetch()
        observed.append(second.tracking_sha)

    result = fx.fetch(before_cas=second_writer)
    assert observed
    assert result.fetched_sha == first_tip
    assert result.tracking_sha == observed[0] == fx.tracking()
    assert result.disposition == "newer-shared-ref-kept"
    assert result.cas_retries == 1
    fx.assert_no_temporary_refs()


def test_remote_rewind_to_ancestor_refuses_instead_of_keeping_stale_tip(
    tmp_path: Path,
) -> None:
    fx = Fixture(tmp_path)
    ancestor = fx.tracking()
    newer = fx.commit("newer")
    fx.push_main()
    fx.fetch()
    assert fx.tracking() == newer

    # A remote rewind has the same local ancestry shape as a stale fetcher
    # observing a newer concurrent fetch.  The read-only remote confirmation
    # must distinguish them and preserve Git's non-fast-forward refusal.
    git(fx.remote, "update-ref", "refs/heads/main", ancestor)
    with pytest.raises(fetch_ref.FetchRefError, match="does not fast-forward"):
        fx.fetch()
    assert fx.tracking() == newer
    fx.assert_no_temporary_refs()


def test_fresh_read_retries_when_other_writer_installs_older_ancestor(tmp_path: Path) -> None:
    fx = Fixture(tmp_path)
    middle = fx.commit("middle")
    tip = fx.commit("tip")
    fx.push_main()

    def intervening_writer(attempt: int, _expected: str | None, _fetched: str) -> None:
        if attempt == 0:
            fx.set_tracking(middle)

    result = fx.fetch(before_cas=intervening_writer)
    assert result.fetched_sha == tip
    assert result.tracking_sha == tip == fx.tracking()
    assert result.disposition == "advanced"
    assert result.cas_retries == 1
    fx.assert_no_temporary_refs()


def test_sideways_ref_move_refuses_instead_of_overwriting(tmp_path: Path) -> None:
    fx = Fixture(tmp_path)
    fx.commit("wanted")
    fx.push_main()
    side = fx.commit("side", orphan=True)
    # Make the deliberately off-history object locally available without
    # changing origin/main; update-ref correctly refuses unknown objects.
    git(fx.consumer, "fetch", "-q", str(fx.publisher), "HEAD")

    def sideways_writer(attempt: int, _expected: str | None, _fetched: str) -> None:
        if attempt == 0:
            fx.set_tracking(side)

    with pytest.raises(fetch_ref.FetchRefError, match="moved sideways"):
        fx.fetch(before_cas=sideways_writer)
    assert fx.tracking() == side
    fx.assert_no_temporary_refs()


def test_divergent_remote_tip_refuses_before_first_tracking_write(tmp_path: Path) -> None:
    fx = Fixture(tmp_path)
    original = fx.tracking()
    side = fx.commit("rewritten-main", orphan=True)
    # Put the orphaned object in the isolated bare remote before moving main;
    # update-ref correctly refuses an object the remote does not contain.
    git(fx.publisher, "push", "-q", "origin", f"{side}:refs/heads/side")
    git(fx.remote, "update-ref", "refs/heads/main", side)

    with pytest.raises(fetch_ref.FetchRefError, match="does not fast-forward"):
        fx.fetch()
    assert fx.tracking() == original
    fx.assert_no_temporary_refs()


def test_repeated_cas_losses_are_bounded_and_fail_closed(tmp_path: Path) -> None:
    fx = Fixture(tmp_path)
    middle1 = fx.commit("middle-1")
    middle2 = fx.commit("middle-2")
    fx.commit("tip")
    fx.push_main()
    intervening = [middle1, middle2]

    def always_move(attempt: int, _expected: str | None, _fetched: str) -> None:
        fx.set_tracking(intervening[attempt])

    with pytest.raises(fetch_ref.FetchRefError, match="after 2 fresh-read CAS attempts"):
        fx.fetch(max_cas_attempts=2, before_cas=always_move)
    assert fx.tracking() == middle2
    fx.assert_no_temporary_refs()
