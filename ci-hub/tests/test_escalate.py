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
from datetime import datetime, timezone

import pytest

ROOT = Path(__file__).resolve().parents[2]
ESCALATE = ROOT / "ci-hub/bin/escalate"
sys.path.insert(0, str(ROOT / "ci-hub/lib"))
from health_escalation_event import make_event, parse_part, part_documents, write_event  # noqa: E402

# A fake `gh` covering exactly the four calls escalate makes. Anything else exits
# 64 so a future call cannot silently read as "nothing there".
FAKE_GH = r'''#!/usr/bin/env python3
import json, os, re, sys
from datetime import datetime, timezone
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
        {
            "number": i["number"], "title": i["title"], "body": i["body"],
            "author": i.get("author", {"login": "rrnewton"}),
        }
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
                "author": issue.get("author", {"login": "rrnewton"}),
            }
        )
    )

if positional[:1] == ["api"]:
    endpoint = next((arg for arg in positional if arg.startswith("repos/")), "")
    match = re.search(r"/issues/(\d+)/comments", endpoint)
    if not match:
        store.write_text(json.dumps(state))
        print(f"fake gh: unhandled api endpoint: {endpoint}", file=sys.stderr)
        raise SystemExit(64)
    number = int(match.group(1))
    issue = next(i for i in state["issues"] if i["number"] == number)
    comments = [
        {
            "body": comment["body"], "created_at": comment["createdAt"],
            "user": comment.get("author", {"login": "rrnewton"}),
        }
        for comment in issue["comments"]
    ]
    pages = [comments[index : index + 100] for index in range(0, len(comments), 100)]
    save_and_exit(json.dumps(pages or [[]]))

if positional[:2] == ["issue", "create"]:
    number = max((i["number"] for i in state["issues"]), default=0) + 1
    state["issues"].append(
        {
            "number": number,
            "title": option("--title", positional) or "",
            "body": body_from(positional),
            "state": "OPEN",
            "createdAt": os.environ.get("FAKE_GH_NOW", datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")),
            "comments": [],
            "author": {"login": "rrnewton"},
        }
    )
    save_and_exit(f"https://github.com/owner/name/issues/{number}")

if positional[:2] == ["issue", "comment"]:
    number = int(positional[2])
    issue = next(i for i in state["issues"] if i["number"] == number)
    issue["comments"].append(
        {
            "body": body_from(positional),
            "createdAt": os.environ.get("FAKE_GH_NOW", datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")),
            "author": {"login": "rrnewton"},
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


def test_a_key_is_not_matched_as_a_prefix_of_another_key(bench) -> None:
    run, repository, _ = bench
    run(
        "--key", "one-alarm-detailed", "--title", "detailed",
        message="ACTION: detailed",
    )
    completed = run(
        "--key", "one-alarm", "--title", "plain", message="ACTION: plain"
    )
    assert completed.returncode == 0, completed.stderr
    assert len(repository()["issues"]) == 2


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


def test_a_per_run_timestamp_does_not_read_as_a_change(bench) -> None:
    run, repository, _ = bench
    stamped = 'ACTION: x title="y observed_at=%s"'
    run("--key", "one-alarm", "--title", "t",
        "--digest-ignore", r"observed_at=[0-9TZ:+-]+",
        message=stamped % "2026-01-01T00:00:00Z")
    completed = run("--key", "one-alarm", "--title", "t",
                    "--digest-ignore", r"observed_at=[0-9TZ:+-]+",
                    message=stamped % "2026-01-01T01:00:00Z")
    assert "outcome=unchanged" in completed.stdout
    assert repository()["issues"][0]["comments"] == []


def test_ignoring_a_timestamp_does_not_hide_a_real_change(bench) -> None:
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t",
        "--digest-ignore", r"observed_at=[0-9TZ:+-]+",
        message='ACTION: first title="y observed_at=2026-01-01T00:00:00Z"')
    completed = run("--key", "one-alarm", "--title", "t",
                    "--digest-ignore", r"observed_at=[0-9TZ:+-]+",
                    message='ACTION: second title="y observed_at=2026-01-01T01:00:00Z"')
    assert "outcome=published" in completed.stdout
    assert len(repository()["issues"][0]["comments"]) == 1


def test_the_published_text_keeps_what_the_digest_ignores(bench) -> None:
    """Ignoring is for comparison only. A body with the timestamp stripped out
    would be a worse record than one that repeats."""
    run, repository, _ = bench
    run("--key", "one-alarm", "--title", "t",
        "--digest-ignore", r"observed_at=[0-9TZ:+-]+",
        message='ACTION: x title="y observed_at=2026-01-01T00:00:00Z"')
    assert "observed_at=2026-01-01T00:00:00Z" in repository()["issues"][0]["body"]


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


def test_untrusted_issue_author_cannot_capture_a_health_key(bench, tmp_path: Path) -> None:
    run, repository, _ = bench
    (tmp_path / "store.json").write_text(
        json.dumps(
            {
                "calls": [],
                "issues": [
                    {
                        "number": 1,
                        "title": "spoof",
                        "body": "escalation-key: owner-only-key",
                        "state": "OPEN",
                        "createdAt": "2026-09-20T00:00:00Z",
                        "comments": [],
                        "author": {"login": "outside-user"},
                    }
                ],
            }
        )
    )

    completed = run(
        "--key", "owner-only-key", "--title", "trusted", message="ACTION: real"
    )

    assert completed.returncode == 0, completed.stderr
    assert len(repository()["issues"]) == 2
    assert repository()["issues"][1]["author"]["login"] == "rrnewton"


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


def durable_event(
    tmp_path: Path, *, sequence: int, payload: str,
    host: str = "devbig014.example.com", kind: str = "alarm",
) -> tuple[Path, dict]:
    envelope = make_event(
        kind=kind,
        payload=payload,
        origin_host=host,
        findings=1 if kind == "alarm" else 0,
        event_id=f"event-20260920T010203{sequence:06d}Z-123-{sequence:08x}",
        created_at=f"2026-09-20T01:02:{sequence:02d}Z",
    )
    return write_event(tmp_path, envelope), envelope


def test_event_payload_of_227307_bytes_is_chunked_and_read_back_completely(
    bench, tmp_path: Path
) -> None:
    run, repository, _ = bench
    payload = "ACTION: exact-large-payload\n" + "x" * (227_307 - 28)
    assert len(payload.encode()) == 227_307
    path, envelope = durable_event(tmp_path, sequence=1, payload=payload)

    completed = run("--event-file", str(path))

    assert completed.returncode == 0, completed.stderr
    issue = repository()["issues"][0]
    publications = [issue["body"], *(comment["body"] for comment in issue["comments"])]
    parsed = [parse_part(body) for body in publications]
    assert all(part is not None for part in parsed)
    rebuilt = "".join(
        part["payload"] for part in sorted(parsed, key=lambda part: part["number"])
    )
    assert rebuilt == payload
    assert len(publications) == len(part_documents(envelope))


def test_event_retry_resumes_only_missing_parts(bench, tmp_path: Path) -> None:
    run, repository, _ = bench
    payload = "ACTION: partial\n" + "p" * 100_000
    path, envelope = durable_event(tmp_path, sequence=2, payload=payload)
    parts = part_documents(envelope)
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    store = tmp_path / "store.json"
    store.write_text(
        json.dumps(
            {
                "calls": [],
                "issues": [
                    {
                        "number": 1,
                        "title": envelope["title"],
                        "body": parts[0],
                        "state": "OPEN",
                        "createdAt": now,
                        "comments": [
                            {"body": parts[1], "createdAt": now},
                        ],
                    }
                ],
            }
        )
    )

    completed = run("--event-file", str(path))

    assert completed.returncode == 0, completed.stderr
    issue = repository()["issues"][0]
    bodies = [issue["body"], *(comment["body"] for comment in issue["comments"])]
    assert bodies == parts


def test_complete_event_after_more_than_100_comments_is_read_without_duplicates(
    bench, tmp_path: Path
) -> None:
    run, repository, _ = bench
    path, envelope = durable_event(
        tmp_path, sequence=9, payload="ACTION: paginated\n" + "r" * 100_000
    )
    parts = part_documents(envelope)
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    filler = [
        {"body": f"unrelated historical comment {index}", "createdAt": now}
        for index in range(105)
    ]
    (tmp_path / "store.json").write_text(
        json.dumps(
            {
                "calls": [],
                "issues": [
                    {
                        "number": 1,
                        "title": envelope["title"],
                        "body": parts[0],
                        "state": "OPEN",
                        "createdAt": now,
                        "comments": filler
                        + [{"body": part, "createdAt": now} for part in parts[1:]],
                    }
                ],
            }
        )
    )

    completed = run("--event-file", str(path))

    assert completed.returncode == 0, completed.stderr
    issue = repository()["issues"][0]
    assert len(issue["comments"]) == len(filler) + len(parts) - 1
    calls = repository()["calls"]
    assert any("--paginate" in call and "--slurp" in call for call in calls)
    assert not any(call[:2] == ["issue", "comment"] for call in calls)


def test_conflicting_partial_event_is_permanent_and_never_overwritten(
    bench, tmp_path: Path
) -> None:
    run, repository, _ = bench
    path, envelope = durable_event(tmp_path, sequence=8, payload="ACTION: expected\n")
    conflicting = make_event(
        kind="alarm",
        payload="ACTION: conflicting\n",
        origin_host=envelope["origin_host"],
        findings=1,
        event_id=envelope["event_id"],
        created_at=envelope["created_at"],
    )
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    (tmp_path / "store.json").write_text(
        json.dumps(
            {
                "calls": [],
                "issues": [
                    {
                        "number": 1,
                        "title": envelope["title"],
                        "body": part_documents(conflicting)[0],
                        "state": "OPEN",
                        "createdAt": now,
                        "comments": [],
                    }
                ],
            }
        )
    )

    completed = run("--event-file", str(path))

    assert completed.returncode == 78
    assert "classification=permanent" in completed.stderr
    assert repository()["issues"][0]["body"] == part_documents(conflicting)[0]


def test_trusted_human_quoting_the_sentinel_is_not_a_machine_part(
    bench, tmp_path: Path
) -> None:
    run, repository, _ = bench
    first, _ = durable_event(tmp_path, sequence=10, payload="ACTION: first\n")
    second, _ = durable_event(tmp_path, sequence=11, payload="ACTION: changed\n")
    assert run("--event-file", str(first)).returncode == 0
    store = repository()
    store["issues"][0]["comments"].append(
        {
            "body": "> <!-- hermit-health-escalation-part-v1 -->\nquoted in prose",
            "createdAt": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "author": {"login": "rrnewton"},
        }
    )
    (tmp_path / "store.json").write_text(json.dumps(store))

    completed = run("--event-file", str(second))

    assert completed.returncode == 0, completed.stderr
    assert "outcome=published" in completed.stdout


def test_small_unchanged_event_keeps_existing_quiet_behavior(bench, tmp_path: Path) -> None:
    run, repository, _ = bench
    first, _ = durable_event(tmp_path, sequence=3, payload="ACTION: same\n")
    second, _ = durable_event(tmp_path, sequence=4, payload="ACTION: same\n")
    assert run("--event-file", str(first), "--repeat-after", "0").returncode == 0

    repeated = run("--event-file", str(second), "--repeat-after", "0")

    assert repeated.returncode == 0, repeated.stderr
    assert "outcome=unchanged" in repeated.stdout
    assert repository()["issues"][0]["comments"] == []


def test_resolve_from_one_host_cannot_close_another_hosts_issue(
    bench, tmp_path: Path
) -> None:
    run, repository, _ = bench
    alarm, _ = durable_event(
        tmp_path, sequence=5, payload="ACTION: host-scoped\n", host="devbig014"
    )
    other_resolve, _ = durable_event(
        tmp_path, sequence=6, payload="", host="devbig030", kind="resolve"
    )
    own_resolve, _ = durable_event(
        tmp_path, sequence=7, payload="", host="devbig014", kind="resolve"
    )
    assert run("--event-file", str(alarm)).returncode == 0

    unrelated = run("--event-file", str(other_resolve))
    assert unrelated.returncode == 0
    assert "outcome=nothing-open" in unrelated.stdout
    assert repository()["issues"][0]["state"] == "OPEN"

    related = run("--event-file", str(own_resolve))
    assert related.returncode == 0
    assert repository()["issues"][0]["state"] == "CLOSED"
