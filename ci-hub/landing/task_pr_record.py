"""The exact task/PR ownership record written when a pull request is created.

The record is a whole TaskGraph note.  A pull-request URL mentioned in prose is
only a mention; this record is the producing operation saying which task owns
the pull request it just created.  Historical tasks without a record remain
readable through the task/PR join's conservative fallback.
"""

from __future__ import annotations

import re
from typing import NamedTuple


RECORD_VERSION = "v1"
HEADER = f"TASK-PR-OWNERSHIP {RECORD_VERSION}"

_FIELD = re.compile(r"^(?P<key>[a-z_]+):[ \t]*(?P<value>.*)$")
_REPO = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
_PR_URL = re.compile(
    r"^https://github\.com/(?P<repo>[^/]+/[^/]+)/pull/(?P<pr>\d+)$"
)
_EXPECTED_FIELDS = frozenset({"task", "repo", "pr", "url", "actor"})


class InvalidTaskPrRecord(ValueError):
    """A note claimed to be a task/PR record but was not the complete shape."""


class TaskPrOwnership(NamedTuple):
    task: str
    repo: str
    pr: int
    url: str
    actor: str


def render(task: str, repo: str, pr: int, url: str, actor: str) -> str:
    """Render one complete record from the URL returned by GitHub."""
    record = _validated(
        {"task": task, "repo": repo, "pr": str(pr), "url": url, "actor": actor},
        expected_task=task,
    )
    return "\n".join(
        [
            HEADER,
            f"task: {record.task}",
            f"repo: {record.repo}",
            f"pr: {record.pr}",
            f"url: {record.url}",
            f"actor: {record.actor}",
        ]
    )


def parse(body: str, *, expected_task: str) -> TaskPrOwnership | None:
    """Read a whole-note record, or return None for ordinary prose.

    Once the header is present, malformed, missing, duplicate, or future fields
    fail by name.  A broken record must not disappear into the prose fallback.
    """
    lines = [line.rstrip() for line in (body or "").strip().splitlines()]
    if not lines or lines[0].strip() != HEADER:
        return None
    fields: dict[str, str] = {}
    for line in lines[1:]:
        if not line.strip():
            raise InvalidTaskPrRecord("blank line in task/PR ownership record")
        match = _FIELD.fullmatch(line)
        if match is None:
            raise InvalidTaskPrRecord(
                f"malformed task/PR ownership field: {line!r}"
            )
        key = match.group("key")
        if key in fields:
            raise InvalidTaskPrRecord(
                f"duplicate task/PR ownership field: {key}"
            )
        fields[key] = match.group("value").strip()
    missing = sorted(_EXPECTED_FIELDS - fields.keys())
    unknown = sorted(fields.keys() - _EXPECTED_FIELDS)
    if missing:
        raise InvalidTaskPrRecord(
            "missing task/PR ownership field(s): " + ",".join(missing)
        )
    if unknown:
        raise InvalidTaskPrRecord(
            "unexpected task/PR ownership field(s): " + ",".join(unknown)
        )
    return _validated(fields, expected_task=expected_task)


def _validated(fields: dict[str, str], *, expected_task: str) -> TaskPrOwnership:
    task = fields["task"]
    repo = fields["repo"]
    url = fields["url"]
    actor = fields["actor"]
    if not task or "\n" in task:
        raise InvalidTaskPrRecord("task/PR ownership task is empty or multiline")
    if task != expected_task:
        raise InvalidTaskPrRecord(
            f"task/PR ownership task mismatch: note={expected_task} record={task}"
        )
    if not _REPO.fullmatch(repo):
        raise InvalidTaskPrRecord(f"malformed task/PR ownership repo: {repo!r}")
    try:
        pr = int(fields["pr"])
    except ValueError as error:
        raise InvalidTaskPrRecord(
            f"malformed task/PR ownership pr: {fields['pr']!r}"
        ) from error
    if pr <= 0:
        raise InvalidTaskPrRecord(f"task/PR ownership pr is not positive: {pr}")
    match = _PR_URL.fullmatch(url)
    if match is None:
        raise InvalidTaskPrRecord(f"malformed task/PR ownership url: {url!r}")
    if match.group("repo").lower() != repo.lower() or int(match.group("pr")) != pr:
        raise InvalidTaskPrRecord(
            "task/PR ownership repo or pr disagrees with url"
        )
    if not actor or "\n" in actor:
        raise InvalidTaskPrRecord("task/PR ownership actor is empty or multiline")
    return TaskPrOwnership(task, repo.lower(), pr, url, actor)
