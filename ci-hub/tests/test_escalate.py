"""Brackets for ci-hub/bin/escalate, the escalation route that does not need Orc.

Every case runs the real tool against a fake `gh` that records what it was asked
to do and answers from a small in-memory repository on disk. Nothing reaches
GitHub, and nothing reaches a person.

WHAT THESE ARE FOR. The tool exists because a check that reaches a correct
verdict and cannot deliver it is indistinguishable from a check that found
nothing wrong. So the cases that matter most are the ones where delivery FAILS or
is DECLINED, and each asserts that the outcome is reported by name rather than
collapsing into a bare exit status.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
ESCALATE = ROOT / "ci-hub/bin/escalate"

# A fake `gh` covering exactly the four calls escalate makes. Anything else exits
# 64 so a future call cannot silently read as "nothing there".
FAKE_GH = r'''#!/usr/bin/env python3
import json, os, re, sys
from pathlib import Path

store = Path(os.environ["FAKE_GH_STORE"])
state = json.loads(store.read_text()) if store.exists() else {"issues": [], "calls": []}
argv = [a for a in sys.argv[1:]]
state["calls"].append(argv)

if os.environ.get("FAKE_GH_FAIL"):
    store.write_text(json.dumps(state))
    print(os.environ["FAKE_GH_FAIL"], file=sys.stderr)
    raise SystemExit(1)

# The attributed boundary's own flags are consumed before the gh subcommand.
positional = list(argv)
for flag in ("--who", "--team", "--role"):
    if flag in positional:
        i = positional.index(flag)
        state.setdefault("attribution", {})[flag] = positional[i + 1]
        del positional[i : i + 2]


def option(name, args):
    if name in args:
        return args[args.index(name) + 1]
    return None


def body_from(args):
    source = option("--body-file", args)
    if source == "-":
        return sys.stdin.read()
    if source:
        return Path(source).read_text()
    return option("--body", args) or ""


def save_and_exit(text=""):
    store.write_text(json.dumps(state))
    if text:
        print(text)
    raise SystemExit(0)


if positional[:2] == ["issue", "list"]:
    open_issues = [
        {"number": i["number"], "title": i["title"], "body": i["body"]}
        for i in state["issues"]
        if i["state"] == "OPEN"
    ]
    save_and_exit(json.dumps(open_issues))

if positional[:2] == ["issue", "view"]:
    number = int(positional[2])
    issue = next(i for i in state["issues"] if i["number"] == number)
    save_and_exit(
        json.dumps(
            {
                "body": issue["body"],
                "createdAt": issue["createdAt"],
                "comments": issue["comments"],
            }
        )
    )

if positional[:2] == ["issue", "create"]:
    number = max((i["number"] for i in state["issues"]), default=0) + 1
    state["issues"].append(
        {
            "number": number,
            "title": option("--title", positional) or "",
            "body": body_from(positional),
            "state": "OPEN",
            "createdAt": os.environ.get("FAKE_GH_NOW", "2026-09-03T00:00:00Z"),
            "comments": [],
        }
    )
    save_and_exit(f"https://github.com/owner/name/issues/{number}")

if positional[:2] == ["issue", "comment"]:
    number = int(positional[2])
    issue = next(i for i in state["issues"] if i["number"] == number)
    issue["comments"].append(
        {
            "body": body_from(positional),
            "createdAt": os.environ.get("FAKE_GH_NOW", "2026-09-03T00:00:00Z"),
        }
    )
    save_and_exit()

if positional[:2] == ["issue", "close"]:
    number = int(positional[2])
    next(i for i in state["issues"] if i["number"] == number)["state"] = "CLOSED"
    save_and_exit()

store.write_text(json.dumps(state))
print(f"fake gh: unhandled call: {argv}", file=sys.stderr)
raise SystemExit(64)
'''


@pytest.fixture()
def bench(tmp_path: Path):
    gh = tmp_path / "gh"
    gh.write_text(FAKE_GH)
    gh.chmod(0o755)
    store = tmp_path / "store.json"
    log = tmp_path / "escalation.log"

    def run(*args: str, message: str | None = None, now: str | None = None, fail: str | None = None):
        environment = dict(os.environ)
        environment["FAKE_GH_STORE"] = str(store)
        environment["DG_AGENT_NAME"] = "health-tick"
        if now:
            environment["FAKE_GH_NOW"] = now
        if fail:
            environment["FAKE_GH_FAIL"] = fail
        else:
            environment.pop("FAKE_GH_FAIL", None)
        return subprocess.run(
            [
                sys.executable,
                str(ESCALATE),
                "--gh",
                str(gh),
                "--log-file",
                str(log),
                *args,
            ],
            input=message,
            capture_output=True,
            text=True,
            env=environment,
            timeout=120,
        )

    def repository():
        return json.loads(store.read_text()) if store.exists() else {"issues": [], "calls": []}

    def records():
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text().splitlines() if line.strip()]

    return run, repository, records


def test_self_test_needs_no_other_argument_and_no_network() -> None:
    completed = subprocess.run(
        [sys.executable, str(ESCALATE), "--self-test"],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert completed.returncode == 0, completed.stderr
    assert "checks passed" in completed.stdout


def test_help_is_a_pure_probe() -> None:
    completed = subprocess.run(
        [sys.executable, str(ESCALATE), "--help"], capture_output=True, text=True, timeout=60
    )
    assert completed.returncode == 0
    assert "--key" in completed.stdout


def test_first_alarm_opens_one_issue_carrying_the_finding(bench) -> None:
    run, repository, records = bench
    completed = run("--key", "tick-findings", "--title", "t", message="ACTION: something broke")
    assert completed.returncode == 0, completed.stderr
    assert "outcome=published" in completed.stdout
    issues = repository()["issues"]
    assert len(issues) == 1
    assert "ACTION: something broke" in issues[0]["body"]
    assert "escalation-key: tick-findings" in issues[0]["body"]
    assert records()[-1]["outcome"] == "published"


def test_the_write_is_attributed(bench) -> None:
    """An unattributed issue is indistinguishable from one the owner wrote, which
    is why the boundary requires these and why the tool must actually pass them."""
    run, repository, _ = bench
    run("--key", "tick-findings", "--title", "t", message="ACTION: x")
    attribution = repository()["attribution"]
    assert attribution["--who"] == "health-tick"
    assert attribution["--role"] == "relay"


def test_an_unchanged_alarm_stays_quiet(bench) -> None:
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t", message="ACTION: same")
    completed = run("--key", "one-alarm", "--title", "t", message="ACTION: same")
    assert completed.returncode == 0
    assert "outcome=unchanged" in completed.stdout
    assert repository()["issues"][0]["comments"] == []


def test_changed_content_comments_on_the_same_issue(bench) -> None:
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t", message="ACTION: first")
    completed = run("--key", "one-alarm", "--title", "t", message="ACTION: second")
    assert "outcome=published" in completed.stdout
    issues = repository()["issues"]
    assert len(issues) == 1, "a second alarm must not open a second issue"
    assert len(issues[0]["comments"]) == 1
    assert "ACTION: second" in issues[0]["comments"][0]["body"]


def test_a_persistent_condition_says_so_again_after_the_repeat_window(bench) -> None:
    """Quiet is right for a minute and wrong for a week: an open issue nobody has
    touched in a month reads as history rather than as a live condition."""
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t", message="ACTION: same", now="2026-01-01T00:00:00Z")
    completed = run(
        "--key", "one-alarm", "--title", "t", "--repeat-after", "60",
        message="ACTION: same", now="2026-06-01T00:00:00Z",
    )
    assert "outcome=published" in completed.stdout
    assert len(repository()["issues"][0]["comments"]) == 1


def test_repeat_after_zero_never_repeats(bench) -> None:
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t", message="ACTION: same", now="2026-01-01T00:00:00Z")
    completed = run(
        "--key", "one-alarm", "--title", "t", "--repeat-after", "0",
        message="ACTION: same", now="2026-06-01T00:00:00Z",
    )
    assert "outcome=unchanged" in completed.stdout
    assert repository()["issues"][0]["comments"] == []


def test_resolving_closes_the_issue_and_says_why(bench) -> None:
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t", message="ACTION: x")
    completed = run("--key", "one-alarm", "--resolve")
    assert completed.returncode == 0
    assert "outcome=resolved" in completed.stdout
    issue = repository()["issues"][0]
    assert issue["state"] == "CLOSED"
    assert "No longer reported" in issue["comments"][-1]["body"]


def test_resolving_with_nothing_open_is_not_an_error(bench) -> None:
    run, _repository, _ = bench
    completed = run("--key", "one-alarm", "--resolve")
    assert completed.returncode == 0
    assert "outcome=nothing-open" in completed.stdout


def test_a_resolved_condition_that_returns_opens_a_new_issue(bench) -> None:
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t", message="ACTION: x")
    run("--key", "one-alarm", "--resolve")
    completed = run("--key", "one-alarm", "--title", "t", message="ACTION: x is back")
    assert "outcome=published" in completed.stdout
    issues = repository()["issues"]
    assert len(issues) == 2
    assert issues[1]["state"] == "OPEN"


def test_an_unreachable_fork_is_never_reported_as_delivered(bench) -> None:
    """The whole point. A caller that cannot separate 'published' from 'written
    down here and nobody told' has rebuilt the defect one layer along."""
    run, _repository, records = bench
    completed = run(
        "--key", "one-alarm", "--title", "t", message="ACTION: x", fail="could not resolve host"
    )
    assert completed.returncode == 1
    assert "outcome=recorded-locally-only" in completed.stderr
    assert "NOWHERE ELSE" in completed.stderr
    assert "outcome=published" not in completed.stdout
    outcomes = [r["outcome"] for r in records()]
    assert "recorded" in outcomes, "the alarm reaches disk before anything can fail"
    assert outcomes[-1] == "recorded-locally-only"


def test_two_open_issues_for_one_key_refuse_rather_than_guess(bench, tmp_path: Path) -> None:
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t", message="ACTION: x")
    store = json.loads((tmp_path / "store.json").read_text())
    duplicate = dict(store["issues"][0])
    duplicate["number"] = 99
    store["issues"].append(duplicate)
    (tmp_path / "store.json").write_text(json.dumps(store))
    completed = run("--key", "one-alarm", "--title", "t", message="ACTION: y")
    assert completed.returncode == 1
    assert "refusing to guess" in completed.stderr


def test_an_empty_alarm_is_refused(bench) -> None:
    run, _repository, _ = bench
    completed = run("--key", "one-alarm", "--title", "t", message="   \n")
    assert completed.returncode == 2
    assert "empty alarm" in completed.stderr


def test_an_unnamed_writer_is_refused(bench, tmp_path: Path) -> None:
    """The disclosure prefix is the only thing separating an automated issue from
    one the owner wrote, so a nameless caller must not get one."""
    completed = subprocess.run(
        [
            sys.executable,
            str(ESCALATE),
            "--key",
            "one-alarm",
            "--title",
            "t",
            "--log-file",
            str(tmp_path / "log"),
        ],
        input="ACTION: x",
        capture_output=True,
        text=True,
        env={k: v for k, v in os.environ.items() if k != "DG_AGENT_NAME"},
        timeout=60,
    )
    assert completed.returncode == 2
    assert "--who is required" in completed.stderr
