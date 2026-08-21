"""Tests for keeping ledger-referenced git objects reachable.

THE REMOVAL PATH IS TESTED FIRST AND HARDEST, on purpose. Adding refs is the easy
half and the half that obviously works; a keep-alive that only ever adds is its own
defect, because the namespace then grows without bound and nobody notices. The
property that makes this safe to run forever is that the ref set EQUALS the ledger's
present commits -- it subtracts as well as adds -- so that is what these assert.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HEALTH = Path(__file__).resolve().parents[1]
if str(HEALTH) not in sys.path:
    sys.path.insert(0, str(HEALTH))

import ledger_referenced_objects as lro  # noqa: E402


def git(repo: Path, *args: str, stdin: str | None = None) -> str:
    proc = subprocess.run(["git", "-C", str(repo), *args], input=stdin,
                          capture_output=True, text=True, check=True)
    return proc.stdout


def make_repo(path: Path) -> list[str]:
    """A repo with one commit on main and two commits reachable from no ref."""
    path.mkdir(parents=True, exist_ok=True)
    git(path.parent, "init", "-q", path.name)
    cfg = ["-c", "user.email=t@example.invalid", "-c", "user.name=t"]
    subprocess.run(["git", "-C", str(path), *cfg, "commit", "-q", "--allow-empty",
                    "-m", "base"], check=True)
    orphans = []
    for n in (1, 2):
        subprocess.run(["git", "-C", str(path), *cfg, "checkout", "-q", "-b", f"side{n}"],
                       check=True)
        subprocess.run(["git", "-C", str(path), *cfg, "commit", "-q", "--allow-empty",
                        "-m", f"off-main {n}"], check=True)
        orphans.append(git(path, "rev-parse", "HEAD").strip())
        subprocess.run(["git", "-C", str(path), "checkout", "-q", "-"], check=True)
        subprocess.run(["git", "-C", str(path), "branch", "-qD", f"side{n}"], check=True)
    return orphans


def write_ledger(path: Path, commits: list[str], repo: str = "hermit") -> Path:
    path.write_text("".join(
        json.dumps({"commit": c, "repo": repo, "result": "pass"}) + "\n"
        for c in commits))
    return path


@pytest.fixture()
def workspace(tmp_path, monkeypatch):
    root = tmp_path / "root"
    (root).mkdir()
    orphans = make_repo(root / "hermit")
    subprocess.run(["git", "-C", str(root), "init", "-q", "--bare", "reverie"], check=True)
    monkeypatch.setattr(lro, "DEFAULT_CHECKOUTS", {"hermit": "hermit"})
    return root, orphans


def pinned(repo: Path) -> set[str]:
    out = git(repo, "for-each-ref", "--format=%(objectname)", lro.REF_PREFIX)
    return set(out.split())


def test_pins_only_objects_the_ledger_names_and_the_checkout_has(workspace, tmp_path):
    root, orphans = workspace
    absent = "a" * 40
    ledger = write_ledger(tmp_path / "ledger", orphans + [absent])

    rc = lro.main(["--root", str(root), "--ledger", str(ledger)])

    assert rc == 0
    assert pinned(root / "hermit") == set(orphans)
    # The absent one is counted, not invented: you cannot point a ref at nothing.
    assert absent not in pinned(root / "hermit")


def test_a_commit_the_ledger_stops_naming_is_UNPINNED(workspace, tmp_path):
    """The bounded-growth half. Without this the namespace only ever grows."""
    root, orphans = workspace
    ledger = tmp_path / "ledger"

    write_ledger(ledger, orphans)
    lro.main(["--root", str(root), "--ledger", str(ledger)])
    assert pinned(root / "hermit") == set(orphans)

    # Ledger shrinks -- as it would when rows are pruned.
    write_ledger(ledger, orphans[:1])
    lro.main(["--root", str(root), "--ledger", str(ledger)])

    assert pinned(root / "hermit") == {orphans[0]}, "stale pin was not removed"


def test_a_pin_pointing_somewhere_other_than_its_own_name_is_replaced(workspace, tmp_path):
    """A ref whose name and target disagree is not a pin, it is a lie about one."""
    root, orphans = workspace
    ledger = write_ledger(tmp_path / "ledger", orphans)
    hermit = root / "hermit"
    base = git(hermit, "rev-parse", "HEAD").strip()
    git(hermit, "update-ref", f"{lro.REF_PREFIX}/{orphans[0]}", base)

    lro.main(["--root", str(root), "--ledger", str(ledger)])

    target = git(hermit, "rev-parse", f"{lro.REF_PREFIX}/{orphans[0]}").strip()
    assert target == orphans[0]


def test_pinned_objects_survive_gc_and_unpinned_ones_do_not(workspace, tmp_path):
    """The whole point, exercised rather than argued.

    Runs a real `git gc --prune=now` over a repo holding two commits reachable from
    no ref, with exactly one of them pinned. This is the assertion that would have
    caught the original outage.
    """
    root, orphans = workspace
    kept, dropped = orphans
    ledger = write_ledger(tmp_path / "ledger", [kept])

    lro.main(["--root", str(root), "--ledger", str(ledger)])
    hermit = root / "hermit"
    # Reflogs keep unreachable objects alive independently; clear them so the test
    # measures the pin and nothing else.
    subprocess.run(["git", "-C", str(hermit), "reflog", "expire", "--expire=now",
                    "--expire-unreachable=now", "--all"], check=True)
    subprocess.run(["git", "-C", str(hermit), "gc", "--prune=now", "--quiet"], check=True)

    assert git(hermit, "cat-file", "-t", kept).strip() == "commit"
    probe = subprocess.run(["git", "-C", str(hermit), "cat-file", "-t", dropped],
                           capture_output=True, text=True)
    assert probe.returncode != 0, "unpinned object survived; the test proves nothing"


def test_missing_checkout_is_reported_not_silently_zero(tmp_path):
    """Zero pins because a checkout is absent reads exactly like zero pins needed."""
    ledger = write_ledger(tmp_path / "ledger", ["b" * 40])
    rc = lro.main(["--root", str(tmp_path / "nowhere"), "--ledger", str(ledger)])
    assert rc == 1
