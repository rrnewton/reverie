#!/usr/bin/env python3
"""Build child environments for Git commands bound to an explicit repository."""

from __future__ import annotations

import os
from collections.abc import Iterable, Mapping


# These variables override repository, object-store, work-tree, or index state
# even when a command uses ``git -C <repo>``. Git also exports them to hook
# children, so accepting them implicitly lets a caller redirect a verifier to
# a different repository while its argv still names the intended one.
GIT_REPOSITORY_ENV = frozenset(
    (
        "GIT_INDEX_FILE",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
        "GIT_INDEX_VERSION",
    )
)


def sanitized_git_env(
    env: Mapping[str, str] | None = None,
    *,
    inherit: Iterable[str] = (),
) -> dict[str, str]:
    """Return ``env`` without ambient repository selectors.

    ``inherit`` is deliberately narrow: a caller that really consumes its own
    in-flight index must name that variable explicitly. Unknown names refuse
    rather than turning this into a general environment allowlist.
    """

    inherited = frozenset(inherit)
    unknown = inherited - GIT_REPOSITORY_ENV
    if unknown:
        raise ValueError(
            "inherit contains non-repository Git environment variables: "
            + ", ".join(sorted(unknown))
        )
    child = dict(os.environ if env is None else env)
    for name in GIT_REPOSITORY_ENV - inherited:
        child.pop(name, None)
    return child
