"""Tests for keeping ledger-referenced git objects reachable.

THE REMOVAL PATH IS TESTED FIRST AND HARDEST, on purpose. Adding refs is the easy
half and the half that obviously works; a keep-alive that only ever adds is its own
defect, because the namespace then grows without bound and nobody notices. The
property that makes this safe to run forever is that the ref set EQUALS the ledger's
present commits -- it subtracts as well as adds -- so that is what these assert.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

HEALTH = Path(__file__).resolve().parents[1]
if str(HEALTH) not in sys.path:
    sys.path.insert(0, str(HEALTH))

import ledger_referenced_objects as lro  # noqa: E402


def git(repo: Path, *args: str, stdin: str | None = None) -> str:
    proc = subprocess.run(["git", "-C", str(repo), *args], input=stdin,
                          capture_output=True, text=True, check=True)
    return proc.stdout


def make_repo(path: Path, count: int = 2) -> list[str]:
    """A repo with one commit on main and `count` commits reachable from no ref."""
    path.mkdir(parents=True, exist_ok=True)
    git(path.parent, "init", "-q", path.name)
    cfg = ["-c", "user.email=t@example.invalid", "-c", "user.name=t"]
    subprocess.run(["git", "-C", str(path), *cfg, "commit", "-q", "--allow-empty",
                    "-m", "base"], check=True)
    orphans = []
    for n in range(1, count + 1):
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


def write_ledger_rows(path: Path, rows: list[tuple[str, str | None]]) -> Path:
    encoded = []
    for commit, repo in rows:
        row = {"commit": commit, "result": "pass"}
        if repo is not None:
            row["repo"] = repo
        encoded.append(json.dumps(row) + "\n")
    path.write_text("".join(encoded))
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


def test_source_counts_are_separate_from_checkout_union():
    legacy_hermit = {f"{number:040x}" for number in range(3000, 3299)}
    hermit = {f"{number:040x}" for number in range(317)}
    reverie = {f"{number:040x}" for number in range(1000, 1031)}
    canonical_reverie = set(sorted(reverie)[:4])

    grouped = {
        None: legacy_hermit,
        "hermit": hermit,
        "reverie": reverie,
        "rrnewton/reverie": canonical_reverie,
    }
    combined, source_counts, unknown = lro.commits_by_checkout(grouped)
    sources = {source["repo"]: source for source in lro.source_repo_results(grouped)}

    assert unknown == []
    assert len(combined["hermit"]) == 299 + 317
    assert len(combined["reverie"]) == 31
    assert canonical_reverie <= combined["reverie"]
    assert source_counts == {"hermit": 299 + 317, "reverie": 31 + 4}
    assert sources[None]["ledger_commits"] == 299
    assert sources["hermit"]["ledger_commits"] == 317
    assert sources["reverie"]["ledger_commits"] == 31
    assert sources["rrnewton/reverie"]["ledger_commits"] == 4
    assert sum(sources[repo]["ledger_commits"] for repo in (
        "hermit", "reverie", "rrnewton/reverie")) == 317 + 31 + 4


def test_overlapping_canonical_reverie_rows_reach_one_checkout_sync(tmp_path, capsys):
    root = tmp_path / "root"
    root.mkdir()
    make_repo(root / "hermit")
    reverie_commits = make_repo(root / "reverie", count=5)
    canonical_reverie = reverie_commits[1:]
    ledger = write_ledger_rows(
        tmp_path / "ledger",
        [(commit, "reverie") for commit in reverie_commits]
        + [(commit, "rrnewton/reverie") for commit in canonical_reverie],
    )

    rc = lro.main(["--root", str(root), "--ledger", str(ledger), "--json"])

    assert rc == 0
    assert len(canonical_reverie) == 4
    assert set(canonical_reverie) <= pinned(root / "reverie")
    assert pinned(root / "reverie") == set(reverie_commits)
    report = json.loads(capsys.readouterr().out)
    sources = {source["repo"]: source for source in report["sources"]}
    reverie = next(repo for repo in report["repos"] if repo["repo"] == "reverie")
    assert sources["reverie"]["ledger_commits"] == 5
    assert sources["rrnewton/reverie"]["ledger_commits"] == 4
    assert reverie["source_commits"] == 5 + 4
    assert reverie["ledger_commits"] == 5


def test_missing_repo_defaults_to_hermit_and_pins_present_objects(workspace, tmp_path, capsys):
    root, orphans = workspace
    absent = "a" * 40
    ledger = write_ledger_rows(
        tmp_path / "ledger",
        [(orphans[0], None), (orphans[1], None), (absent, None)],
    )

    rc = lro.main(["--root", str(root), "--ledger", str(ledger), "--json"])

    assert rc == 0
    assert pinned(root / "hermit") == set(orphans)
    report = json.loads(capsys.readouterr().out)
    missing = next(source for source in report["sources"] if source["repo"] is None)
    hermit = next(repo for repo in report["repos"] if repo["repo"] == "hermit")
    assert missing == {"checkout_repo": "hermit", "ledger_commits": 3, "repo": None}
    assert hermit["ledger_commits"] == 3
    assert hermit["present"] == 2
    assert hermit["absent"] == 1


def test_missing_repo_pin_is_removed_when_ledger_stops_naming_it(workspace, tmp_path):
    root, orphans = workspace
    ledger = write_ledger_rows(tmp_path / "ledger", [(orphans[0], None)])

    assert lro.main(["--root", str(root), "--ledger", str(ledger)]) == 0
    assert pinned(root / "hermit") == {orphans[0]}

    write_ledger_rows(ledger, [])
    assert lro.main(["--root", str(root), "--ledger", str(ledger)]) == 0
    assert pinned(root / "hermit") == set()


def test_unknown_repo_value_refuses_before_writing_refs(tmp_path, capsys):
    root = tmp_path / "root"
    root.mkdir()
    commits = make_repo(root / "hermit")
    make_repo(root / "reverie")
    ledger = write_ledger(tmp_path / "ledger", [commits[0]], repo="unknown/repo")

    rc = lro.main(["--root", str(root), "--ledger", str(ledger), "--json"])

    assert rc == 1
    report = json.loads(capsys.readouterr().out)
    assert report["failures"] == ["unsupported ledger repo value(s): 'unknown/repo'"]
    assert report["repos"] == []
    assert report["sources"] == [{
        "checkout_repo": None,
        "ledger_commits": 1,
        "repo": "unknown/repo",
    }]
    assert pinned(root / "hermit") == set()
    assert pinned(root / "reverie") == set()


def test_unknown_repo_value_with_invalid_commit_is_still_visible(tmp_path, capsys):
    root = tmp_path / "root"
    root.mkdir()
    make_repo(root / "hermit")
    make_repo(root / "reverie")
    ledger = write_ledger(tmp_path / "ledger", ["not-a-commit"], repo="unknown/repo")

    rc = lro.main(["--root", str(root), "--ledger", str(ledger)])

    assert rc == 1
    assert "unsupported ledger repo value(s): 'unknown/repo'" in capsys.readouterr().out


def test_non_string_repo_value_refuses_before_writing_refs(tmp_path, capsys):
    root = tmp_path / "root"
    root.mkdir()
    commits = make_repo(root / "hermit")
    make_repo(root / "reverie")
    ledger = tmp_path / "ledger"
    ledger.write_text(json.dumps({
        "commit": commits[0],
        "repo": {"owner": "unknown"},
        "result": "pass",
    }) + "\n")

    rc = lro.main(["--root", str(root), "--ledger", str(ledger), "--json"])

    assert rc == 1
    report = json.loads(capsys.readouterr().out)
    assert report["failures"] == [
        "ledger repo value is not a string or null: {'owner': 'unknown'}"
    ]
    assert report["repos"] == []
    assert pinned(root / "hermit") == set()
    assert pinned(root / "reverie") == set()


def test_default_ledger_and_checkout_root_follow_dev_hermit_parent(tmp_path):
    state_root = tmp_path / "state-root"
    state_root.mkdir()
    make_repo(state_root / "hermit", count=0)
    make_repo(state_root / "reverie", count=0)
    (state_root / "ledger").write_text("")
    tool_status_before = subprocess.run(
        ["git", "status", "--porcelain=v1"],
        cwd=lro.TOOL_ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    environment = os.environ | {
        "DEV_HERMIT_PARENT": str(state_root),
        "PYTHONDONTWRITEBYTECODE": "1",
    }

    result = subprocess.run(
        [sys.executable, str(Path(lro.__file__)), "--dry-run", "--json"],
        cwd=lro.TOOL_ROOT,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert {Path(item["checkout"]) for item in report["repos"]} == {
        state_root / "hermit",
        state_root / "reverie",
    }
    assert subprocess.run(
        ["git", "status", "--porcelain=v1"],
        cwd=lro.TOOL_ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout == tool_status_before


# The three gates that replay the validation ledger share one wall budget; the
# comment on ledger_referenced_objects in tick-hub.yaml derives it. The longest
# tick in the log it was sized from (591 ticks, 2026-09-18 to 2026-09-29) took
# this long, and the budget counts all of it as other work.
LONGEST_RECORDED_TICK_SECONDS = 541.7
LEDGER_REPLAY_GATES = (
    "ledger_referenced_objects",
    "validate_wall_regression",
    "validate_wall_series_readiness",
)


def test_ledger_replay_gate_bounds_fit_the_tick_backstop_together() -> None:
    root = HEALTH.parents[1]
    config = yaml.safe_load((HEALTH / "tick-hub.yaml").read_text())
    reminders = {item["name"]: item for item in config["reminders"]}
    bounds = {
        name: reminders[name]["gate"].get("timeout_secs")
        for name in LEDGER_REPLAY_GATES
    }
    # Each declares its bound: the 30 s default is not part of the budget.
    assert all(isinstance(bound, int) for bound in bounds.values()), bounds
    match = re.search(
        r"^OPERATIONAL_TICK_WALL_BACKSTOP_SECONDS=(\d+)$",
        (root / "scripts/hermit-operational-tick").read_text(),
        re.MULTILINE,
    )
    assert match, "the operational tick no longer declares its wall backstop"
    # A gate that times out runs again on every tick, so all three can run to
    # their bounds in the same tick.
    total = sum(bounds.values()) + LONGEST_RECORDED_TICK_SECONDS
    assert total <= int(match.group(1)), bounds
