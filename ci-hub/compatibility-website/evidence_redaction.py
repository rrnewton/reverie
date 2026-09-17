#!/usr/bin/env python3
"""Canonicalise host identity out of per-cell evidence before it is published.

The compatibility site is a PUBLIC GitHub Pages artifact. Per-cell stdout,
stderr and INFO logs are produced on a developer workstation and carry that
workstation's absolute workspace path, so publishing them unmodified puts
``/home/<user>`` on a public site for essentially every cell.

WHAT IS REDACTED, AND WHY IT IS EXACTLY ONE CLASS
-------------------------------------------------
Measured 2026-09-17 over retained cells under ``ignored/compat-envelope``:

* the absolute workspace prefix appears in 95.5% of cells' captures and 49.5%
  of cells' INFO logs, and in the sampled population it has exactly ONE
  distinct value;
* no credential-shaped material appeared at all in 264 MB -- no token, bearer,
  secret, password or api-key word, no PRIVATE KEY block, no AKIA key, no
  machine name.

So the exposure is HOST IDENTITY, not credentials, and one deterministic path
canonicalisation covers it.

⚠️ WHAT IS DELIBERATELY NOT REDACTED, BECAUSE OVER-REDACTION DESTROYS THE
EVIDENCE THIS PAGE EXISTS TO SHOW. An earlier pass named ``/tmp/<name>`` paths
and ``HOME=`` assignments as two further leak classes. Measured against the
bytes, they are not host-identity classes at all:

* all nine distinct ``/tmp`` paths in a 300-file sample are GUEST-visible paths
  belonging to the test programs themselves -- ``/tmp/test``,
  ``/tmp/hermit-copy-file-range-source``, ``/tmp/hermit_file_io_roundtrip.bin``
  and so on. None discloses a user, a home or a host. Rewriting them would
  corrupt the syscall traces that are the point of the page;
* the only observed ``HOME=`` value was ``HOME=/``, which discloses nothing.
  Were one ever to carry ``/home/<user>``, the workspace rule below already
  rewrites the value.

A class is only in this module if it was shown to disclose host identity.

ORDERING: REDACT FIRST, THEN DIGEST
-----------------------------------
Published evidence is bound by content digest, and a rebuild from the same
source bytes must be deterministic. Digesting the raw bytes and publishing the
redacted ones would make every published file fail its own verification. That
ordering is easy to get backwards, so :func:`redact_and_bind` does both steps in
the only correct order and returns the digest of what will actually be written;
prefer it to calling :func:`redact_evidence_bytes` and hashing separately.

The transform is a pure function of ``(data, workspace_root)`` and is
idempotent, so the deterministic-rebuild property holds.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass
from typing import Final


WORKSPACE_TOKEN: Final = "{{WORKSPACE_ROOT}}"
HOME_TOKEN: Final = "{{HOME_ROOT}}"
"""The token this repository already uses for a canonicalised workspace root.

Spelled the same as ``ci-hub/series/series.py`` and ``ci-hub/ledger/validate_rows.py``
so a reader meets one token rather than three. `test_evidence_redaction.py`
asserts the three stay equal rather than trusting the comment.
"""

# The exact delimiter set ci-hub/series/series.py uses to decide where a path
# token ends in recorded argv/env/shell text: Unicode White_Space -- NOT
# Python's broader str.isspace -- plus the punctuation that delimits paths.
# Duplicated as bytes rather than imported because series.py is a 3,000-line
# module on the parent's operational path and this runs inside the site build;
# the test cross-checks the two sets for equality so they cannot drift.
_EVIDENCE_PATH_DELIMITERS: Final = frozenset(
    "\u0009\u000a\u000b\u000c\u000d\u0020\u0085\u00a0\u1680"
    "\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a"
    "\u2028\u2029\u202f\u205f\u3000"
    "=,:;()[]{}<>\"'"
)

# Evidence files are read as bytes: an INFO log is guest output and is not
# guaranteed to be valid UTF-8. Only the ASCII-representable delimiters can
# appear as single bytes, and a multi-byte UTF-8 delimiter's continuation bytes
# are all >= 0x80, so they can never be mistaken for one of these.
_DELIMITER_BYTES: Final = frozenset(
    character.encode("utf-8")
    for character in _EVIDENCE_PATH_DELIMITERS
    if len(character.encode("utf-8")) == 1
)


class RedactionRefused(RuntimeError):
    """The inputs cannot be redacted safely, so nothing is published."""


@dataclass(frozen=True)
class BoundEvidence:
    """Exactly the bytes that will be written, and their binding."""

    data: bytes
    sha256: str
    byte_length: int
    redacted: bool


def _is_delimiter(byte: bytes) -> bool:
    return byte in _DELIMITER_BYTES


def _replace_evidence_root_token(data: bytes, old: bytes, new: bytes) -> bytes:
    """Replace ``old`` only where it is a whole path token.

    The bytes mirror of ``series._replace_evidence_root_token``: a match counts
    when what precedes it is nothing or a delimiter, and what follows is
    nothing, a delimiter, or ``/`` -- the last so a workspace root is rewritten
    when a longer path continues from it, without rewriting a DIFFERENT
    directory whose name merely begins with the same characters.

    ⚠️ NOT ``series._replace_at_path_boundary``, WHICH IS A DIFFERENT RULE AND
    THE EASY ONE TO COPY BY MISTAKE. That one accepts a match only when what
    follows is end-of-string, ``/``, or a closing quote -- deliberately tight,
    because it rewrites structured recorded fields such as argv and cwd where
    the path IS the field. Applied to free-form log text it silently
    under-redacts: `failed to stat /home/<user> for the runner` is followed by a
    space and would be left intact. Evidence is free-form, so this is the rule
    that fits, and it is the one whose name says `evidence`.
    """

    start = 0
    pieces: list[bytes] = []
    while True:
        found = data.find(old, start)
        if found < 0:
            pieces.append(data[start:])
            return b"".join(pieces)
        end = found + len(old)
        preceding = data[found - 1 : found]
        following = data[end : end + 1]
        starts_at_boundary = not preceding or _is_delimiter(preceding)
        ends_at_boundary = (
            not following or following == b"/" or _is_delimiter(following)
        )
        if starts_at_boundary and ends_at_boundary:
            pieces.extend((data[start:found], new))
        else:
            pieces.append(data[start:end])
        start = end


def _validated_root(workspace_root: str) -> bytes:
    """Refuse a root that would make the rewrite unsafe or meaningless."""

    if not workspace_root:
        raise RedactionRefused("workspace root is empty")
    if not workspace_root.startswith("/"):
        raise RedactionRefused(
            f"workspace root must be absolute, got {workspace_root!r}"
        )
    normalised = workspace_root.rstrip("/")
    if not normalised:
        # "/" would rewrite every absolute path in the file to the token.
        raise RedactionRefused("workspace root must not be the filesystem root")
    if normalised.count("/") < 2:
        # "/home" alone would swallow every user's tree and is never the
        # workspace; requiring two separators keeps the rule to a real root.
        raise RedactionRefused(
            f"workspace root is too shallow to be a workspace: {workspace_root!r}"
        )
    if WORKSPACE_TOKEN in workspace_root:
        raise RedactionRefused(
            "workspace root already contains the canonical token; refusing to "
            "redact a value that has been redacted"
        )
    return normalised.encode("utf-8")


_HOME_CONTAINERS: Final = (b"/home", b"/Users", b"/usr/home")
"""Directories whose immediate child is a user's home on the platforms here."""


def _home_prefix(root: bytes) -> bytes | None:
    """`/home/<user>` for a root beneath it, else None.

    Returns None rather than guessing when the workspace is not under a
    recognised home container -- a root such as `/srv/build/tree` discloses no
    username and there is nothing to rewrite.
    """

    for container in _HOME_CONTAINERS:
        prefix = container + b"/"
        if not root.startswith(prefix):
            continue
        remainder = root[len(prefix) :]
        if not remainder:
            return None
        user = remainder.split(b"/", 1)[0]
        if not user:
            return None
        return prefix + user
    return None


def redact_evidence_bytes(data: bytes, *, workspace_root: str) -> bytes:
    """Rewrite the host workspace root to the canonical token.

    Pure and idempotent: the token contains no ``/`` prefix, so a second
    application finds nothing left to rewrite.
    """

    root = _validated_root(workspace_root)
    token = WORKSPACE_TOKEN.encode("ascii")
    redacted = _replace_evidence_root_token(data, root, token)

    # The home directory ABOVE the workspace leaks the SAME username on its own,
    # as a bare `/home/<user>` with no workspace suffix, so the workspace rule
    # alone does not remove it.
    #
    # ⚠️ THE HOME IS NOT THE WORKSPACE'S PARENT. For `/home/u/work/dev-hermit`
    # the parent is `/home/u/work`, which discloses nothing extra; the username
    # lives two levels higher. Deriving the parent instead leaves `/home/u`
    # intact, which is the whole leak.
    home = _home_prefix(root)
    if home is not None:
        redacted = _replace_evidence_root_token(redacted, home, HOME_TOKEN.encode("ascii"))
    return redacted


def redact_and_bind(data: bytes, *, workspace_root: str) -> BoundEvidence:
    """Redact, then digest what will actually be written.

    ⚠️ USE THIS RATHER THAN DIGESTING SEPARATELY. The digest must cover the
    PUBLISHED bytes; a digest of the raw input would make every published file
    fail its own verification, and that mistake is invisible until a verifier
    runs.
    """

    published = redact_evidence_bytes(data, workspace_root=workspace_root)
    return BoundEvidence(
        data=published,
        sha256=hashlib.sha256(published).hexdigest(),
        byte_length=len(published),
        redacted=published != data,
    )
