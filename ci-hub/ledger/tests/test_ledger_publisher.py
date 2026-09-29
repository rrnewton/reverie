#!/usr/bin/env python3
"""Publisher tests, and one gap the invariant suite could not see.

`test_ledger_invariants.py` pins the ledger's semantics. This file pins the
PUBLISHING behaviour the schema does not describe — spool durability,
append-aware non-fast-forward retry, and ancestry-gated draining — plus one
invariant test that a mutation sweep proved was passing for the wrong reason.

Run: python3 -m pytest ci-hub/ledger/tests/test_ledger_publisher.py -q
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ledger  # noqa: E402
import publisher  # noqa: E402

TEAM, HOST = "hermit", "hosta"
SHA_1 = "a" * 40


def ev(event_id: str, outcome: str = "pass", **kw) -> dict:
    e = {
        "schema": "validate-ledger/v1",
        "event_id": event_id,
        "event_type": "run.result",
        "emitted_at": "2026-08-07T02:00:00Z",
        "team": TEAM,
        "host": HOST,
        "run_id": event_id,
        "producer": {"source": "observed", "tool": "ci-hub/validate-run", "tool_version": "1"},
        "commit": SHA_1,
        "outcome": outcome,
    }
    e.update(kw)
    return e


def _wait_path(path: Path, process=None) -> None:
    deadline = time.monotonic() + 20
    while not path.exists():
        if process is not None and process.poll() is not None:
            raise AssertionError(f"child exited before {path}: {process.communicate()}")
        if time.monotonic() >= deadline:
            raise AssertionError(f"child never reached {path}")
        time.sleep(0.02)


def _publisher_child(tmp_path, name, clone, spool, *, writer=None, split_source=None,
                     prepare_release=None):
    """A new interpreter: it cannot inherit a parent's flock descriptor."""
    ready = tmp_path / f"{name}-prepared"
    config = dict(module=str(Path(publisher.__file__).parent), repo=str(clone),
                  spool=str(spool), ready=str(ready), writer=str(writer) if writer else None,
                  split_source=str(split_source) if split_source else None,
                  prepare_release=str(prepare_release) if prepare_release else None)
    code = r'''
import json, sys, time
from pathlib import Path
c=json.loads(sys.argv[1]); sys.path.insert(0,c['module'])
import publisher
prepare=publisher.prepare_shared_checker
def observed_prepare():
    prepare()
    Path(c['ready']).touch()
    deadline=time.monotonic()+20
    while c['prepare_release'] and not Path(c['prepare_release']).exists():
        if time.monotonic()>deadline: raise RuntimeError('preparation barrier timed out')
        time.sleep(.02)
publisher.prepare_shared_checker=observed_prepare
try:
    if c['split_source']:
        report=publisher.split_published_to_match_source(
            c['repo'],c['split_source'],['ledger/hermit/hosta/2026-08.jsonl'],
            attribution='publisher split test')
    else:
        report=publisher.publish(c['repo'],c['spool'],attempts=1,
            serialized_writer=c['writer'],attribution='publisher concurrency test')
    print(json.dumps(report))
except Exception as error:
    print(type(error).__name__+': '+str(error),file=sys.stderr)
    sys.exit(2)
'''
    process = subprocess.Popen([sys.executable, "-c", code, json.dumps(config)],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    return process, ready


def _stop_children(*children):
    for child in children:
        if child is not None and child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.communicate(timeout=5)


@pytest.mark.parametrize("same_spool", [False, True])
def test_shared_checkout_publishers_serialize_complete_guarded_transactions(
    tmp_path, remote_and_clone, same_spool
):
    """Hold the first real Git push while the second wants the same index."""
    bare, clone = remote_and_clone
    first_spool, second_spool = tmp_path / "one", tmp_path / "two"
    publisher.spool(first_spool, TEAM, HOST, [ev("01A")])
    if same_spool:
        second_spool = first_spool
    else:
        publisher.spool(second_spool, TEAM, HOST, [ev("01B")])
    entered, release = tmp_path / "writer-entered", tmp_path / "writer-release"
    writer = tmp_path / "guarded-writer"
    writer.write_text(f'''#!{sys.executable}
import subprocess,sys,time
from pathlib import Path
assert sys.argv[1]=='publish'
Path({str(entered)!r}).touch()
deadline=time.monotonic()+20
while not Path({str(release)!r}).exists():
    if time.monotonic()>deadline: sys.exit(2)
    time.sleep(.02)
def git(*args): return subprocess.run(['git',*args],check=True,capture_output=True,text=True).stdout.strip()
git('fetch','origin','main')
git('merge-base','--is-ancestor','origin/main',sys.argv[2])
git('push','origin',sys.argv[2]+':refs/heads/main')
git('fetch','origin','main')
git('merge-base','--is-ancestor',sys.argv[2],'origin/main')
''')
    writer.chmod(0o755)
    first = second = None
    try:
        first, _ = _publisher_child(tmp_path, "first", clone, first_spool, writer=writer)
        _wait_path(entered, first)
        candidate = git(clone, "rev-parse", "HEAD").stdout
        second, ready = _publisher_child(tmp_path, "second", clone, second_spool, writer=writer)
        _wait_path(ready, second)
        with pytest.raises(subprocess.TimeoutExpired):
            second.wait(timeout=0.5)
        assert git(clone, "rev-parse", "HEAD").stdout == candidate
        assert git(clone, "status", "--porcelain").stdout == ""
        release.touch()
        for child in (first, second):
            out, err = child.communicate(timeout=20)
            assert child.returncode == 0, (out, err)
        rel = f"ledger/{TEAM}/{HOST}/2026-08.jsonl"
        rows = [json.loads(line) for line in git(bare, "show", f"main:{rel}").stdout.splitlines()]
        assert [row["event_id"] for row in rows] == (["01A"] if same_spool else ["01A", "01B"])
        assert not publisher.spooled(first_spool) and not publisher.spooled(second_spool)
        assert git(clone, "merge-base", "--is-ancestor", "HEAD", "origin/main").returncode == 0
    finally:
        _stop_children(first, second)


def test_split_waits_for_the_same_checkout_lock(tmp_path, remote_and_clone):
    _, clone = remote_and_clone
    spool = tmp_path / "spool"
    publisher.spool(spool, TEAM, HOST, [ev("01A"), ev("01B")])
    publisher.publish(clone, spool)
    source = tmp_path / "source"
    rel = f"ledger/{TEAM}/{HOST}/2026-08.jsonl"
    (source / rel).parent.mkdir(parents=True)
    (source / rel).write_text(json.dumps(ev("01A"), sort_keys=True) + "\n")
    before = git(clone, "rev-parse", "HEAD").stdout
    child = None
    try:
        with publisher._checkout_transaction(clone):
            child, ready = _publisher_child(tmp_path, "split", clone, spool, split_source=source)
            _wait_path(ready, child)
            with pytest.raises(subprocess.TimeoutExpired):
                child.wait(timeout=0.5)
            assert git(clone, "rev-parse", "HEAD").stdout == before
        out, err = child.communicate(timeout=20)
        assert child.returncode == 0, (out, err)
        assert json.loads(out)["event_union_preserved"] is True
    finally:
        _stop_children(child)


def test_checkout_timeout_retains_spool_and_exception_releases_lock(
    tmp_path, remote_and_clone, monkeypatch
):
    bare, clone = remote_and_clone
    spool = tmp_path / "spool"
    publisher.spool(spool, TEAM, HOST, [ev("01A")])
    before = git(bare, "rev-parse", "main").stdout
    monkeypatch.setattr(publisher, "CHECKOUT_LOCK_TIMEOUT_SECONDS", 0.1)
    with publisher._checkout_transaction(clone):
        with pytest.raises(publisher.PublishRefused, match="transaction lock timed out"):
            publisher.publish(clone, spool)
    assert git(bare, "rev-parse", "main").stdout == before
    assert git(clone, "status", "--porcelain").stdout == ""
    assert publisher.spooled(spool)
    with pytest.raises(ValueError):
        with publisher._checkout_transaction(clone):
            raise ValueError("test refusal inside transaction")
    publisher.publish(clone, spool)
    assert not publisher.spooled(spool)


def test_cold_preparation_holds_no_checkout_authority(tmp_path, remote_and_clone):
    _, clone = remote_and_clone
    spool, release = tmp_path / "spool", tmp_path / "prepare-release"
    publisher.spool(spool, TEAM, HOST, [ev("01A")])
    child = None
    try:
        child, ready = _publisher_child(tmp_path, "cold", clone, spool, prepare_release=release)
        _wait_path(ready, child)
        # The child is still in preparation, so another real writer may finish.
        publisher.publish(clone, spool)
        release.touch()
        out, err = child.communicate(timeout=20)
        assert child.returncode == 0, (out, err)
        assert json.loads(out)["published"] == 0
    finally:
        _stop_children(child)


def test_preparation_executes_help_but_empty_validation_stays_empty(monkeypatch):
    calls = []
    def run(payload, arguments=()):
        calls.append((payload, arguments))
        return subprocess.CompletedProcess([], 0, "Usage", "")
    monkeypatch.setattr(ledger, "_run_shared_type_checker", run)
    assert ledger.validate_events([]) == [] and calls == []
    ledger.prepare_shared_checker()
    assert calls == [("", ("--help",))]


def test_preparation_refuses_override_under_immutable_authority(monkeypatch):
    monkeypatch.setenv("CI_HUB_LEDGER_EVENT_CHECKER", "/not-an-authorized-checker")
    monkeypatch.setenv("DEV_HERMIT_TOOL_AUTHORITY", "1")
    with pytest.raises(ledger.SharedTypeCheckerUnavailable, match="override is forbidden"):
        ledger.prepare_shared_checker()


def test_checkout_lock_does_not_escape_to_exec_children(tmp_path, remote_and_clone):
    _, clone = remote_and_clone
    lock = Path(git(clone, "rev-parse", "--absolute-git-dir").stdout.strip()) / "ci-hub-ledger-publisher.lock"
    with publisher._checkout_transaction(clone):
        held = [int(p.name) for p in Path("/proc/self/fd").iterdir()
                if p.exists() and p.resolve() == lock]
        assert len(held) == 1 and not os.get_inheritable(held[0])
        code = """import os,sys
from pathlib import Path
assert not any(p.exists() and str(p.resolve())==sys.argv[1] for p in Path('/proc/self/fd').iterdir())
"""
        subprocess.run([sys.executable, "-c", code, str(lock)], close_fds=False, check=True)


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(repo), *args],
                          capture_output=True, text=True, check=check)


@pytest.fixture()
def remote_and_clone(tmp_path):
    """A bare remote plus one clone, so push/fetch/ancestry are real, not mocked."""
    bare = tmp_path / "remote.git"
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(bare)], check=True)
    seed = tmp_path / "seed"
    subprocess.run(["git", "init", "-q", "-b", "main", str(seed)], check=True)
    (seed / "README").write_text("ledger\n")
    git(seed, "add", "README")
    git(seed, "-c", "user.name=t", "-c", "user.email=t@invalid", "commit", "-q", "-m", "seed")
    git(seed, "remote", "add", "origin", str(bare))
    git(seed, "push", "-q", "origin", "main")
    clone = tmp_path / "clone"
    subprocess.run(["git", "clone", "-q", str(bare), str(clone)], check=True)
    return bare, clone


# --------------------------------------------------------------- the surviving gap


def test_a_valid_final_line_without_a_newline_is_still_truncated(tmp_path):
    """The case ONLY the newline check can catch.

    A mutation sweep showed `test_truncated_final_line_without_newline_is_detected`
    passes even with the newline check removed, because its fragment is also
    invalid JSON and `parse_line` rejects it first. A final line that is VALID
    JSON but unterminated is indistinguishable from a complete event unless the
    newline itself is checked — and a writer killed between `write` and its
    newline produces exactly that.
    """
    p = tmp_path / "2026-08.jsonl"
    whole = json.dumps(ev("01A"))
    p.write_text(whole + "\n" + whole.replace("01A", "01B"))  # valid JSON, no newline
    with pytest.raises(ledger.MalformedEvent):
        ledger.read_shard(p)
    p.write_text(whole + "\n" + whole.replace("01A", "01B") + "\n")  # terminated
    assert len(ledger.read_shard(p)) == 2


# --------------------------------------------------------------- append-aware merge


def test_merge_keeps_remote_lines_in_place_and_appends_ours():
    remote = json.dumps(ev("01R"), sort_keys=True) + "\n"
    merged = publisher.merge_shard(remote, [ev("01L")])
    lines = merged.splitlines()
    assert lines[0] == remote.strip(), "a published line must not move or change"
    assert json.loads(lines[1])["event_id"] == "01L"
    assert ledger.verify_append_only(remote, merged) == []


def test_merge_is_idempotent_so_a_retry_cannot_duplicate():
    remote = json.dumps(ev("01R"), sort_keys=True) + "\n"
    once = publisher.merge_shard(remote, [ev("01L")])
    twice = publisher.merge_shard(once, [ev("01L")])
    assert once == twice, "re-publishing the same batch must not append it again"


def test_merge_refuses_the_same_event_id_with_a_different_body():
    remote = json.dumps(ev("01R"), sort_keys=True) + "\n"
    with pytest.raises(publisher.PublishRefused, match="different body"):
        publisher.merge_shard(remote, [ev("01R", outcome="fail")])


def test_merge_refuses_a_truncated_remote_rather_than_appending_after_it():
    with pytest.raises(ledger.MalformedEvent):
        publisher.merge_shard(json.dumps(ev("01R")), [ev("01L")])


# --------------------------------------------------------------- spool durability


def test_events_are_spooled_before_any_git_work(tmp_path):
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A"), ev("01B")])
    pending = publisher.spooled(spool_dir)
    assert list(pending) == [(TEAM, HOST, "2026-08")]
    assert [e["event_id"] for e in pending[(TEAM, HOST, "2026-08")]] == ["01A", "01B"]


def test_stage_tracked_copies_events_without_changing_source(tmp_path):
    source = tmp_path / "source"
    shard = source / f"ledger/{TEAM}/{HOST}/2026-08.jsonl"
    shard.parent.mkdir(parents=True)
    original = json.dumps(ev("01A"), sort_keys=True) + "\n"
    shard.write_text(original)
    report = publisher.stage_tracked(source, tmp_path / "spool")

    assert report["staged"] == 1
    assert report["source_unchanged"] is True
    assert shard.read_text() == original
    assert publisher.spooled(tmp_path / "spool")[(TEAM, HOST, "2026-08")][0][
        "event_id"
    ] == "01A"


def test_operational_publish_refuses_the_shared_producer_parent(
    tmp_path, remote_and_clone
):
    _, clone = remote_and_clone
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])

    with pytest.raises(publisher.PublishRefused, match="shared producer parent"):
        publisher.publish(clone, spool_dir, source_root=clone)


def test_publishing_into_a_foreign_repository_is_refused_before_anything_is_written(
    tmp_path, remote_and_clone
):
    """`--repo` naming the wrong checkout must fail closed, not commit into it.

    Measured 2026-08-09: `--repo hermit` resolved, relative to the parent, to
    the Hermit PRODUCT submodule rather than a dev-hermit clone. The publisher
    accepted it, detached that primary checkout at its own `origin/main`, and
    wrote five `ledger: append 1 event(s)` commits into a product repository --
    one per retry -- before the serialized writer refused for an unrelated
    reason. A directory name is a LABEL; shared history is IDENTITY, and only
    the second was ever checked.
    """
    _, clone = remote_and_clone
    foreign = tmp_path / "foreign"  # a wholly separate repository
    subprocess.run(["git", "init", "-q", "-b", "main", str(foreign)], check=True)
    (foreign / "PRODUCT").write_text("not the ledger repository\n")
    git(foreign, "add", "PRODUCT")
    git(foreign, "-c", "user.name=t", "-c", "user.email=t@invalid",
        "commit", "-q", "-m", "product seed")
    before = git(foreign, "rev-parse", "HEAD").stdout.strip()

    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])
    with pytest.raises(publisher.PublishRefused, match="DIFFERENT repository"):
        publisher.publish(foreign, spool_dir, source_root=clone)

    assert git(foreign, "rev-parse", "HEAD").stdout.strip() == before, (
        "the foreign repository must not gain a commit"
    )
    assert not (foreign / "ledger").exists(), "no shard may be written there"
    assert git(foreign, "rev-parse", "--abbrev-ref", "HEAD").stdout.strip() == "main", (
        "the foreign checkout must not be detached"
    )
    assert publisher.spooled(spool_dir), "a refused batch stays spooled"


def test_a_genuine_clone_of_the_source_is_not_refused_by_the_identity_check(
    tmp_path, remote_and_clone
):
    """The positive side: the guard must not be inert, nor fire on the real path.

    A refusal test alone cannot distinguish "refuses foreign checkouts" from
    "refuses everything". This publishes through a second clone of the same
    repository the source came from, and counts what landed.
    """
    bare, clone = remote_and_clone
    source = tmp_path / "source-checkout"
    subprocess.run(["git", "clone", "-q", str(bare), str(source)], check=True)
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])

    out = publisher.publish(clone, spool_dir, source_root=source)

    assert out["published"] == 1
    assert out["shards"] == [f"ledger/{TEAM}/{HOST}/2026-08.jsonl"]
    assert not publisher.spooled(spool_dir)


def test_publish_uses_the_repository_author_identity(tmp_path, remote_and_clone):
    _, clone = remote_and_clone
    git(clone, "config", "user.name", "Configured Publisher")
    git(clone, "config", "user.email", "configured@example.com")
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])

    publisher.publish(clone, spool_dir)

    author = git(clone, "show", "-s", "--format=%an <%ae>", "HEAD").stdout.strip()
    assert author == "Configured Publisher <configured@example.com>"


def test_a_writer_refusal_reaches_the_caller_whole_not_as_its_last_line(
    tmp_path, remote_and_clone
):
    """The serialized writer's REASON must survive, not only its final line.

    `scripts/parent-main-write` prints `REFUSED: <reason>` first and may follow
    it with a multi-line generic banner. Keeping `stderr.splitlines()[-1]` --
    which this module did -- reported the banner's tail, "nothing was published
    and nothing was modified", and threw the cause away. Measured 2026-08-09 on
    the real tool: the discarded first line WAS the entire root cause, and the
    reported line named nothing at all.

    The fake writer below reproduces that exact four-line shape; the reason is
    the FIRST line, so a last-line capture cannot pass this test.
    """
    _, clone = remote_and_clone
    writer = tmp_path / "refusing-writer"
    writer.write_text(
        "#!/bin/sh\n"
        "echo 'parent-main-write: REFUSED: machine-attribution checker is missing' >&2\n"
        "echo 'parent-main-write: ABORTED at line 124 with status 1, and no reason' >&2\n"
        "echo '  was printed. That is a defect in this tool, not a verdict on your' >&2\n"
        "echo '  change: nothing was published and nothing was modified.' >&2\n"
        "exit 1\n"
    )
    writer.chmod(0o755)
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])

    with pytest.raises(publisher.PublishRefused) as exc:
        publisher.publish(
            clone,
            spool_dir,
            attempts=1,
            serialized_writer=writer,
            attribution="[impl agent, OPUS] [hermit-receipt, devbig030]",
        )

    message = str(exc.value)
    assert "machine-attribution checker is missing" in message, (
        "the REFUSED reason must survive; a last-line capture loses exactly this"
    )
    assert "nothing was published and nothing was modified" in message, (
        "the rest of the writer's output must survive too, not be traded for the reason"
    )
    assert len(message.splitlines()) >= 4, "all four stderr lines must reach the caller"
    assert publisher.spooled(spool_dir), "a refused publish keeps its events"


def test_split_preserves_remote_bytes_and_adopts_untouched_source_order(
    tmp_path, remote_and_clone
):
    bare, clone = remote_and_clone
    rel = f"ledger/{TEAM}/{HOST}/2026-08.jsonl"
    remote_events = [ev("01A"), ev("01B"), ev("01C")]
    target = clone / rel
    target.parent.mkdir(parents=True)
    remote_text = "".join(json.dumps(event, sort_keys=True) + "\n" for event in remote_events)
    target.write_text(remote_text)
    git(clone, "add", "--", rel)
    git(
        clone,
        "-c", "user.name=t", "-c", "user.email=t@invalid",
        "commit", "-q", "-m", "remote order",
    )
    git(clone, "push", "-q", "origin", "main")

    source = tmp_path / "source"
    source_shard = source / rel
    source_shard.parent.mkdir(parents=True)
    source_text = "".join(
        json.dumps(event, sort_keys=True) + "\n"
        for event in (remote_events[0], remote_events[2])
    )
    source_shard.write_text(source_text)
    report = publisher.split_published_to_match_source(
        clone,
        source,
        [rel],
        attribution="ledger split test attribution",
    )

    assert report["split"] == 1
    assert report["source_unchanged"] is True
    assert report["event_union_preserved"] is True
    assert source_shard.read_text() == source_text
    primary = subprocess.run(
        ["git", "-C", str(bare), "show", f"main:{rel}"],
        capture_output=True, text=True, check=True,
    ).stdout
    recovery_rel = report["shards"][0]["recovery"]
    recovery = subprocess.run(
        ["git", "-C", str(bare), "show", f"main:{recovery_rel}"],
        capture_output=True, text=True, check=True,
    ).stdout
    assert primary == source_text
    assert recovery == remote_text
    union = ledger.union_events(
        [json.loads(line) for line in (primary + recovery).splitlines()]
    )
    assert [event["event_id"] for event in union] == ["01A", "01B", "01C"]


def test_spool_survives_a_failed_publish(tmp_path, remote_and_clone):
    """A publish that cannot reach the remote must leave the events recoverable."""
    _, clone = remote_and_clone
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])
    git(clone, "remote", "set-url", "origin", str(tmp_path / "does-not-exist.git"))
    with pytest.raises(publisher.PublishRefused):
        publisher.publish(clone, spool_dir, attempts=2)
    assert publisher.spooled(spool_dir), "spool must be retained when publication fails"


def test_invalid_event_is_refused_before_it_reaches_shared_history(tmp_path, remote_and_clone):
    _, clone = remote_and_clone
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A", host="hosta.corp")])
    with pytest.raises(publisher.PublishRefused) as exc:
        publisher.publish(clone, spool_dir)
    assert "fqdn-host" in str(exc.value)
    assert publisher.spooled(spool_dir), "a refused batch stays spooled, not silently dropped"


# --------------------------------------------------------------- publish + ancestry


def test_publish_appends_commits_and_drains_only_after_ancestry(tmp_path, remote_and_clone):
    bare, clone = remote_and_clone
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A"), ev("01B")])
    out = publisher.publish(clone, spool_dir)
    assert out["published"] == 2
    assert out["shards"] == [f"ledger/{TEAM}/{HOST}/2026-08.jsonl"]
    assert "is an ancestor of" in out["ancestry"]
    published = subprocess.run(
        ["git", "-C", str(bare), "show", f"main:ledger/{TEAM}/{HOST}/2026-08.jsonl"],
        capture_output=True, text=True, check=True).stdout
    assert [json.loads(l)["event_id"] for l in published.splitlines()] == ["01A", "01B"]
    assert not publisher.spooled(spool_dir), "spool drains only after ancestry confirms it"


def test_event_spooled_while_publish_is_active_survives_snapshot_drain(
    tmp_path, remote_and_clone, monkeypatch
):
    _, clone = remote_and_clone
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01BEFORE")])
    original_git = publisher._git
    injected = False

    def injecting_git(repo, *args, **kwargs):
        nonlocal injected
        if args and args[0] == "push" and not injected:
            injected = True
            publisher.spool(spool_dir, TEAM, HOST, [ev("01AFTER")])
        return original_git(repo, *args, **kwargs)

    monkeypatch.setattr(publisher, "_git", injecting_git)
    out = publisher.publish(clone, spool_dir)

    assert out["published"] == 1
    assert injected
    remaining = publisher.spooled(spool_dir)[(TEAM, HOST, "2026-08")]
    assert [event["event_id"] for event in remaining] == ["01AFTER"]


def test_concurrent_append_by_another_machine_is_preserved_alongside_ours(tmp_path, remote_and_clone):
    """Another machine appended to OUR shard before we published.

    Our initial fetch already sees their line, so this is the MERGE path, not
    the retry path (a mutation sweep proved that: removing the retry left this
    test green). The retry itself is forced separately, below.
    """
    bare, clone = remote_and_clone
    rel = f"ledger/{TEAM}/{HOST}/2026-08.jsonl"

    other = tmp_path / "other"
    subprocess.run(["git", "clone", "-q", str(bare), str(other)], check=True)
    theirs = other / rel
    theirs.parent.mkdir(parents=True, exist_ok=True)
    theirs.write_text(json.dumps(ev("01OTHER"), sort_keys=True) + "\n")
    git(other, "add", "--", rel)
    git(other, "-c", "user.name=t", "-c", "user.email=t@invalid", "commit", "-q", "-m", "theirs")
    git(other, "push", "-q", "origin", "main")

    # Our clone still has the pre-append origin/main, so the first push is a
    # non-fast-forward exactly as it would be in production.
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01MINE")])
    out = publisher.publish(clone, spool_dir)

    published = subprocess.run(["git", "-C", str(bare), "show", f"main:{rel}"],
                               capture_output=True, text=True, check=True).stdout
    ids = [json.loads(l)["event_id"] for l in published.splitlines()]
    assert ids == ["01OTHER", "01MINE"], "their line stays first; ours is appended once"
    assert len(ids) == len(set(ids)), "no duplicates"
    assert out["published"] == 1
    assert not publisher.spooled(spool_dir)


def test_a_second_publish_of_the_same_batch_adds_nothing(tmp_path, remote_and_clone):
    bare, clone = remote_and_clone
    rel = f"ledger/{TEAM}/{HOST}/2026-08.jsonl"
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])
    publisher.publish(clone, spool_dir)
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])  # same event again
    publisher.publish(clone, spool_dir)
    published = subprocess.run(["git", "-C", str(bare), "show", f"main:{rel}"],
                               capture_output=True, text=True, check=True).stdout
    assert len([l for l in published.splitlines() if l.strip()]) == 1


def test_two_machines_write_disjoint_shards_and_both_survive(tmp_path, remote_and_clone):
    bare, clone = remote_and_clone
    other = tmp_path / "other"
    subprocess.run(["git", "clone", "-q", str(bare), str(other)], check=True)

    s1, s2 = tmp_path / "spool1", tmp_path / "spool2"
    publisher.spool(s1, TEAM, "hosta", [ev("01A", host="hosta")])
    publisher.spool(s2, TEAM, "hostb", [ev("01B", host="hostb")])
    publisher.publish(clone, s1)
    publisher.publish(other, s2)

    for host, event_id in (("hosta", "01A"), ("hostb", "01B")):
        text = subprocess.run(
            ["git", "-C", str(bare), "show", f"main:ledger/{TEAM}/{host}/2026-08.jsonl"],
            capture_output=True, text=True, check=True).stdout
        assert json.loads(text.strip())["event_id"] == event_id


# ------------------------------------------- forced adversarial remote timing
#
# The two tests below use server-side hooks on the bare remote. Both properties
# they check -- retry-on-rejection, and drain-only-after-ancestry -- survived a
# mutation sweep when tested through the ordinary happy path, because nothing
# in it can make a push fail or an ancestry check disagree. Forcing the timing
# on the remote is the only way to exercise them deterministically.


def _hook(bare: Path, name: str, body: str) -> None:
    hook = bare / "hooks" / name
    hook.write_text(body)
    hook.chmod(0o755)


def test_a_rejected_push_is_retried_and_lands_exactly_once(tmp_path, remote_and_clone):
    """An `update` hook rejects the FIRST push only; the retry must succeed.

    This is the real non-fast-forward shape: our push is refused after we have
    already committed. A publisher without retry loses the batch (or leaves it
    spooled forever); one that retries carelessly appends it twice.
    """
    bare, clone = remote_and_clone
    marker = tmp_path / "rejected-once"
    _hook(bare, "update", f"""#!/bin/sh
if [ ! -f {marker} ]; then touch {marker}; echo "rejecting first push" >&2; exit 1; fi
exit 0
""")
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])
    out = publisher.publish(clone, spool_dir, attempts=4)

    assert marker.exists(), "the hook must actually have rejected a push"
    assert out["attempts"] >= 2, f"publish must have retried, took {out['attempts']} attempt(s)"
    rel = f"ledger/{TEAM}/{HOST}/2026-08.jsonl"
    published = subprocess.run(["git", "-C", str(bare), "show", f"main:{rel}"],
                               capture_output=True, text=True, check=True).stdout
    ids = [json.loads(l)["event_id"] for l in published.splitlines() if l.strip()]
    assert ids == ["01A"], f"exactly one copy must land, got {ids}"
    assert not publisher.spooled(spool_dir)


def test_spool_is_retained_when_push_succeeds_but_ancestry_does_not(tmp_path, remote_and_clone):
    """Push exit status is not publication.

    A `post-receive` hook resets the branch immediately after accepting our
    push, so `git push` reports success while the commit is NOT an ancestor of
    the branch anyone will fetch. The events must stay spooled: dropping them
    on the strength of the exit code is how a 'successful' publish silently
    loses data.
    """
    bare, clone = remote_and_clone
    base = subprocess.run(["git", "-C", str(bare), "rev-parse", "main"],
                          capture_output=True, text=True, check=True).stdout.strip()
    _hook(bare, "post-receive", f"""#!/bin/sh
git --git-dir={bare} update-ref refs/heads/main {base}
""")
    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])
    with pytest.raises(publisher.PublishRefused):
        publisher.publish(clone, spool_dir, attempts=2)
    assert publisher.spooled(spool_dir), (
        "a push whose commit does not become an ancestor must NOT drain the spool"
    )


def test_a_leftover_file_from_a_failed_attempt_is_not_mistaken_for_published(tmp_path, remote_and_clone):
    """Published state comes from the remote ref, not from the working tree.

    A previous attempt can leave the shard written but uncommitted. Reading the
    working tree treats that leftover as already-published content, so publish()
    finds nothing to add, reports success, and drains the spool -- losing every
    event. This is the exact failure that migrating the 654-row legacy ledger
    hit: attempt 1 wrote the file then failed at commit, attempt 2 reported
    0 published and dropped the batch.
    """
    bare, clone = remote_and_clone
    rel = f"ledger/{TEAM}/{HOST}/2026-08.jsonl"

    # Simulate the wreckage of a failed attempt: the shard exists locally with
    # exactly the content we are about to publish, but nothing is committed.
    leftover = clone / rel
    leftover.parent.mkdir(parents=True, exist_ok=True)
    leftover.write_text(json.dumps(ev("01A"), sort_keys=True) + "\n")

    spool_dir = tmp_path / "spool"
    publisher.spool(spool_dir, TEAM, HOST, [ev("01A")])
    out = publisher.publish(clone, spool_dir)

    assert out["published"] == 1, f"the event must actually be published, got {out}"
    published = subprocess.run(["git", "-C", str(bare), "show", f"main:{rel}"],
                               capture_output=True, text=True, check=True).stdout
    assert [json.loads(l)["event_id"] for l in published.splitlines() if l.strip()] == ["01A"]
    assert not publisher.spooled(spool_dir)


# --------------------------------------------------------------- parity rides with series
#
# https://github.com/rrnewton/hermit/issues/3301: a validate's parity rows are
# published in the SAME ledger commit as its series rows, under `parity/` and
# `parity-runs/`, never under the `series/` tree every series reader globs.

PARITY_FIXTURE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "parity-run-1953"
PARITY_RUN = "validate-coord-s17-d3a0a4ae3d16-1790649970094317174-3328074-da07a1da"
PARITY_TREE = "d3a0a4ae3d168565595ae4157b25ab8efbb1861a"
PARITY_EMITTED = "2026-09-28T21:40:00Z"


def _series_row(identifier: str) -> dict:
    return {
        "schema": "stress-series/v1", "event_id": identifier,
        "event_type": "series.observation", "emitted_at": "2026-09-21T12:00:00Z",
        "team": TEAM, "host": HOST, "run_id": identifier, "producer": "fixture",
        "series": {"cell": "c-programs/hello/verify/ptrace", "tree": "a" * 40,
                   "run_index": 0, "outcome": "passed"},
    }


@pytest.fixture()
def parity_store(monkeypatch):
    """The publisher's own series module with the Rust row checker stubbed.

    Only the SERIES rows' shared Rust checker is replaced; every parity check
    below is the real one.
    """
    store = publisher._series_store()
    monkeypatch.setattr(store, "_run_shared_checker", lambda *args, **kwargs:
                        subprocess.CompletedProcess(["schema-fixture"], 0, "", ""))
    return store.parity_ledger_module()


def _parity_sources(parity) -> list[dict]:
    text = (PARITY_FIXTURE / "ledger-sources-typed.jsonl").read_text(encoding="utf-8")
    return [parity.loads(line, f"fixture line {n}") for n, line in enumerate(text.splitlines(), 1)]


def _parity_events(parity, sources, **overrides) -> list[dict]:
    options = dict(producer="validate", run_id=PARITY_RUN, hermit_sha=PARITY_TREE,
                   emitted_at=PARITY_EMITTED, source_tree_dirty=False, host=HOST)
    options.update(overrides)
    return parity.build_batch(sources, **options)


def _spool_parity(parity, spool: Path, events: list[dict]) -> Path:
    """Place a batch in the spool exactly as `append_parity` names it."""
    spool.mkdir(parents=True, exist_ok=True)
    path = spool / parity.batch_name(events[0])
    path.write_text(parity.encode_batch(events), encoding="utf-8")
    return path


def _spool_series(spool: Path, *identifiers: str) -> Path:
    spool.mkdir(parents=True, exist_ok=True)
    path = spool / f"{identifiers[0]}.jsonl"
    path.write_text("".join(json.dumps(_series_row(i), sort_keys=True) + "\n" for i in identifiers))
    return path


def _remote_text(bare: Path, rel: str) -> str:
    return git(bare, "show", f"main:{rel}").stdout


def test_one_commit_carries_the_series_rows_and_the_parity_run(tmp_path, remote_and_clone, parity_store):
    parity = parity_store
    bare, clone = remote_and_clone
    series_spool, parity_spool = tmp_path / "series-spool", tmp_path / "parity-spool"
    _spool_series(series_spool, "run-one")
    events = _parity_events(parity, _parity_sources(parity))
    batch = _spool_parity(parity, parity_spool, events)

    out = publisher.publish(clone, series_spool, store="series", parity_spool=parity_spool)

    assert out["published"] == 1, out
    assert out["parity"]["published"] == 193, out["parity"]  # marker + 192 rows
    assert out["parity"]["refused"] == [] and out["parity"]["error"] is None
    assert out["parity"]["batches"] == [batch.name]
    assert git(bare, "log", "-1", "--format=%s", "main").stdout.strip() == (
        "series: append 1 event(s); parity: append 193 event(s)")
    changed = git(bare, "show", "--name-only", "--format=", "main").stdout.split()
    assert sorted(changed) == [f"parity-runs/{TEAM}/{HOST}/2026-09.jsonl",
                               f"parity/{TEAM}/{HOST}/2026-09.jsonl",
                               f"series/{TEAM}/{HOST}/2026-09.jsonl"]
    # One commit on top of the seed: both stores landed together or not at all.
    assert git(bare, "rev-list", "--count", "main").stdout.strip() == "2"
    rows_text = _remote_text(bare, f"parity/{TEAM}/{HOST}/2026-09.jsonl")
    assert rows_text == parity.encode_batch(events[1:])
    assert _remote_text(bare, f"parity-runs/{TEAM}/{HOST}/2026-09.jsonl") == parity.encode_batch(events[:1])
    # Both batches were proven published, so both moved into retention.
    assert not batch.exists() and (parity_spool / "published" / batch.name).exists()
    assert not list(series_spool.glob("*.jsonl"))


def test_the_series_glob_sees_only_series_rows(tmp_path, remote_and_clone, parity_store):
    parity = parity_store
    bare, clone = remote_and_clone
    _spool_series(tmp_path / "series-spool", "run-one", "run-two")
    _spool_parity(parity, tmp_path / "parity-spool", _parity_events(parity, _parity_sources(parity)))
    publisher.publish(clone, tmp_path / "series-spool", store="series",
                      parity_spool=tmp_path / "parity-spool")
    listed = git(bare, "ls-tree", "-r", "--name-only", "main", "--", "series").stdout.split()
    assert listed == [f"series/{TEAM}/{HOST}/2026-09.jsonl"]
    series_rows = [json.loads(line) for line in _remote_text(bare, listed[0]).splitlines()]
    assert [row["event_id"] for row in series_rows] == ["run-one", "run-two"]
    assert {row["schema"] for row in series_rows} == {"stress-series/v1"}
    # The source-root reader globs `<store>/*/*/YYYY-MM.jsonl`; parity is not in it.
    git(clone, "fetch", "-q", "origin", "main")
    git(clone, "checkout", "-q", "--detach", "origin/main")
    assert publisher._source_root_event_ids(clone, store="series") == {"run-one", "run-two"}


def test_a_changed_parity_replay_is_refused_but_the_series_still_publishes(
        tmp_path, remote_and_clone, parity_store):
    parity = parity_store
    bare, clone = remote_and_clone
    sources = _parity_sources(parity)
    _spool_series(tmp_path / "s1", "run-one")
    _spool_parity(parity, tmp_path / "p1", _parity_events(parity, sources))
    publisher.publish(clone, tmp_path / "s1", store="series", parity_spool=tmp_path / "p1")
    rows_before = _remote_text(bare, f"parity/{TEAM}/{HOST}/2026-09.jsonl")

    # The same run, same event ids, different bytes: now claims a dirty tree.
    changed = _spool_parity(parity, tmp_path / "p2",
                            _parity_events(parity, sources, source_tree_dirty=True))
    _spool_series(tmp_path / "s2", "run-two")
    out = publisher.publish(clone, tmp_path / "s2", store="series", parity_spool=tmp_path / "p2")

    assert out["published"] == 1, out
    assert out["parity"]["published"] == 0
    (refusal,) = out["parity"]["refused"]
    assert refusal["batch"] == changed.name
    assert refusal["error"] == (
        f"run marker of validate run {PARITY_RUN} (192 record(s)) is already published "
        "with a different body")
    assert git(bare, "log", "-1", "--format=%s", "main").stdout.strip() == "series: append 1 event(s)"
    assert _remote_text(bare, f"parity/{TEAM}/{HOST}/2026-09.jsonl") == rows_before
    assert changed.exists(), "a refused parity batch stays spooled and visible"


def test_unusable_published_parity_is_reported_and_never_blocks_a_new_run(
        tmp_path, remote_and_clone, parity_store, capsys):
    """https://github.com/rrnewton/hermit/issues/3301 finding [12]: published
    parity is indexed, not revalidated. Lines the index cannot use are listed,
    and the next run still publishes and its retained copy still reaps."""
    parity = parity_store
    bare, clone = remote_and_clone
    sources = _parity_sources(parity)
    first = _parity_events(parity, sources)
    _spool_parity(parity, tmp_path / "p1", first)
    assert publisher.publish(clone, tmp_path / "p1", store="parity")["parity"]["historic_invalid"] == 0
    # Something outside this tool appends to the published shard: a line that
    # is not JSON, and a second copy of a published row with another body.
    seed = tmp_path / "seed"
    git(seed, "pull", "-q", "origin", "main")
    rows_rel = f"parity/{TEAM}/{HOST}/2026-09.jsonl"
    conflicting = dict(first[1], reason="rewritten after publication")
    with open(seed / rows_rel, "a", encoding="utf-8") as handle:
        handle.write("not json\n" + parity.canonical_line(conflicting) + "\n")
    git(seed, "add", rows_rel)
    git(seed, "-c", "user.name=t", "-c", "user.email=t@invalid", "commit", "-q", "-m", "tamper")
    git(seed, "push", "-q", "origin", "main")
    tampered = _remote_text(bare, rows_rel)

    # Another run over the same records: the pressure-test producer's ids differ.
    second = _parity_events(parity, sources, producer="pressure-test")
    batch = _spool_parity(parity, tmp_path / "p2", second)
    out = publisher.publish(clone, tmp_path / "p2", store="parity")

    assert out["parity"]["published"] == 193 and out["parity"]["refused"] == [], out
    assert out["parity"]["historic_invalid"] == 2
    assert out["parity"]["historic_invalid_sample"] == [
        {"path": rows_rel, "line": 193,
         "error": f"{rows_rel}:193: not JSON: Expecting value: line 1 column 1 (char 0)"},
        {"path": rows_rel, "line": 194,
         "error": f"event_id {first[1]['event_id']} is published twice with different "
                  "bodies; the first copy stands"},
    ]
    after = _remote_text(bare, rows_rel)
    assert after.startswith(tampered) and after[len(tampered):] == parity.encode_batch(second[1:])
    git(clone, "fetch", "-q", "origin", "main")
    git(clone, "checkout", "-q", "--detach", "origin/main")
    assert publisher.reap_retained(tmp_path / "p2", clone, store="parity") == 1
    assert not (tmp_path / "p2" / "published" / batch.name).exists()


def test_a_node_that_never_ran_publishes_as_record_missing(tmp_path, remote_and_clone, parity_store):
    parity = parity_store
    bare, clone = remote_and_clone
    portable = [s for s in _parity_sources(parity) if s["source"]["lane"] == "portable"]
    missing = {
        "cell": "backend-parity-c/never-ran@kvm", "test_id": "backend-parity-c/never-ran",
        "backend": "kvm", "verdict": "record-missing", "unavailable_class": "record-missing", "operand": None,
        "reason": "the parity post-pass of privileged/manifest_backend_parity_c left no parity.status.json",
        "run_id": None, "hermit_sha": None,
        "source": {"lane": "privileged", "node": "manifest_backend_parity_c",
                   "post_pass_state": "absent", "status_sha256": None, "records_sha256": None,
                   "hermit_bin_sha256": None, "scope_source": "expected-scope"},
        "record": None,
    }
    _spool_parity(parity, tmp_path / "parity-spool", _parity_events(parity, [*portable, missing]))

    out = publisher.publish(clone, tmp_path / "parity-spool", store="parity")

    assert out["published"] == 192 and out["parity"]["published"] == 192, out  # 191 rows + marker
    assert git(bare, "log", "-1", "--format=%s", "main").stdout.strip() == "parity: append 192 event(s)"
    rows = [json.loads(line) for line in
            _remote_text(bare, f"parity/{TEAM}/{HOST}/2026-09.jsonl").splitlines()]
    (published_missing,) = [row for row in rows if row["verdict"] == "record-missing"]
    assert published_missing["record"] is None
    assert (published_missing["unavailable_class"], published_missing["operand"]) == ("record-missing", None)
    assert published_missing["source"]["post_pass_state"] == "absent"
    (marker,) = [json.loads(line) for line in
                 _remote_text(bare, f"parity-runs/{TEAM}/{HOST}/2026-09.jsonl").splitlines()]
    assert marker["records"] == 191 and marker["verdicts"]["record-missing"] == 1
    assert {(n["lane"], n["post_pass_state"], n["records"]) for n in marker["nodes"]} == {
        ("portable", "complete", 190), ("privileged", "absent", 1)}


def test_a_spooled_row_whose_envelope_disagrees_with_its_record_is_refused(
        tmp_path, remote_and_clone, parity_store, capsys):
    parity = parity_store
    bare, clone = remote_and_clone
    events = _parity_events(parity, _parity_sources(parity))
    index = next(i for i, e in enumerate(events) if e.get("verdict") == "diverged")
    events[index]["verdict"] = "matched"
    # Keep the marker consistent so the ROW check is the one that must fire.
    events[0]["verdicts"] = {**events[0]["verdicts"]}
    events[0]["verdicts"]["diverged"] -= 1
    events[0]["verdicts"]["matched"] = events[0]["verdicts"].get("matched", 0) + 1
    spool = tmp_path / "parity-spool"
    spool.mkdir()
    batch = spool / parity.batch_name(events[0])
    batch.write_text(parity.encode_batch(events))
    head_before = git(bare, "rev-parse", "main").stdout

    out = publisher.publish(clone, spool, store="parity")

    assert out["published"] == 0 and out["commit"] is None
    assert out["note"] == "nothing published: every pending parity batch was refused"
    (refusal,) = out["parity"]["refused"]
    assert refusal == {"batch": batch.name, "error": (
        f"{batch.name}: event {index + 1}: parity ledger row {events[index]['cell']} "
        f"(validate run {PARITY_RUN}): row verdict matched disagrees with record verdict diverged")}
    assert git(bare, "rev-parse", "main").stdout == head_before
    assert batch.exists()

    # The CLI says so loudly and exits nonzero rather than reporting success.
    source = tmp_path / "source"
    subprocess.run(["git", "clone", "-q", str(bare), str(source)], check=True)
    rc = publisher.main(["publish", "--source-root", str(source), "--repo", str(clone),
                         "--spool-dir", str(spool), "--store", "parity"])
    err = capsys.readouterr().err
    assert rc == 2
    assert f"ledger-publisher: REFUSED parity batch {batch.name}: " in err
    assert "row verdict matched disagrees with record verdict diverged" in err


def test_a_batch_not_in_canonical_bytes_is_refused(tmp_path, remote_and_clone, parity_store):
    """Published parity bytes are the canonical encoding; a re-encoded batch is not."""
    parity = parity_store
    _bare, clone = remote_and_clone
    events = _parity_events(parity, _parity_sources(parity))
    spool = tmp_path / "parity-spool"
    spool.mkdir()
    batch = spool / parity.batch_name(events[0])
    batch.write_text("".join(json.dumps(e, sort_keys=True) + "\n" for e in events))
    out = publisher.publish(clone, spool, store="parity")
    assert out["parity"]["refused"] == [
        {"batch": batch.name, "error": f"{batch.name}:1: not the canonical compact encoding"}]
    assert batch.exists()


def test_a_parity_spool_rides_only_on_the_series_store(tmp_path, remote_and_clone):
    _bare, clone = remote_and_clone
    with pytest.raises(publisher.PublishRefused, match="rides only on the series store"):
        publisher.publish(clone, tmp_path / "spool", store="ledger", parity_spool=tmp_path / "p")
