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
import subprocess
import sys
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
