#!/usr/bin/env python3
"""Enforce the main-only-monorepo pin invariant, at the parent, on RECORDED pins.

THE INVARIANT (owner, 2026-08-02). At any dev-hermit `main` commit:

    A. every tracked hermit Cargo.toml pins ONE reverie rev            (internal)
    B. that rev == the parent's RECORDED reverie gitlink               (coherence)
    C. the parent's reverie gitlink == reverie origin/main             (currency)
    D. the parent's hermit  gitlink == hermit  origin/main             (currency)

WHY A NEW SCRIPT RATHER THAN A NEW FLAG ON primary_checkout.py. Three parent
checks already touch this area and none of them binds it:

  * `primary_checkout.py check-pins` is BLOCKING but covers leg A only. Its
    docstring deliberately excludes currency as a networked dimension, which is
    correct for a pre-commit hook and leaves B/C/D uncovered.
  * `primary_checkout.py check` prints "parent gitlink X differs from
    origin/main Y" and then returns `1 if strict and warnings else 0`. The
    Makefile invokes it WITHOUT --strict, so the violation is printed and the
    gate passes. It also compares the manifests to the reverie CHECKOUT HEAD,
    not to the recorded gitlink -- a different quantity.
  * `primary_checkout.py freshness` asserts not-bare / on-main / not-detached /
    equal-to-origin / clean per checkout. Also about CHECKOUTS, not gitlinks.

So the gap is not "no check exists". Measured 2026-08-07, the invariant was
violated on 4 of 4 legs while `make lint` reported its primary-fresh gate OK.
That is a fake green, and the fix is a check that FAILS.

CHECKOUTS ARE NOT PINS, and that distinction is the whole point of this file. A
parent commit records gitlinks; a colleague who clones and runs `git submodule
update --init` gets the GITLINKS, not whatever happens to be checked out on this
box. Any invariant about what others receive must read the recorded pin.

FAIL CLOSED, AND FAIL WITHOUT NEEDING A FLAG. This script used to end with
`return 1 if args.strict else 0`, and the Makefile's `gate pin-invariant` line
invoked it without `--strict`. So it printed "pin invariant VIOLATED on 2
leg(s)" and exited 0, and `make lint` recorded the gate OK -- measured
2026-08-22 at origin/main eea651d9. That is precisely the defect this file was
written to replace in `primary_checkout.py`, described a few paragraphs above,
reproduced here. A gate whose teeth depend on the caller remembering a flag is a
gate that will eventually be invoked without it, so the verdict no longer
depends on one: violations exit 1 on their own. `--strict` is still accepted and
now does nothing. `--report-only` is the way to ask for the prose without a
verdict.

UNMEASURABLE IS ITS OWN ANSWER, exit 2. An uninitialised hermit submodule used
to read as "no tracked Cargo.toml pins reverie", a violation, because "I could
not list the tree" and "I listed it and found nothing" both arrived as an empty
set. They are different claims and only the second is a pin defect. Exit 2 says
the leg was not checked; it is not a pass. This matches
`ci-hub/health/reverie_pin_lag.py` (CURRENT 0 / LAGGING 1 / UNKNOWN 2) and
`scripts/check_hermit_agent_utils_pin.py`, which fail closed the same way.

    check_reverie_pin_invariant.py                # report; 0 holds, 1 violated, 2 unmeasurable
    check_reverie_pin_invariant.py --report-only  # print the legs, exit 0 regardless
    check_reverie_pin_invariant.py --offline      # skip C/D (no network)
"""

from __future__ import annotations

import argparse
from collections.abc import Mapping
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib
from typing import TextIO


REVERIE_GIT_URL = "https://github.com/rrnewton/reverie.git"
MAIN_REF = "refs/heads/main"
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")

# `git -C <repo>` is overridden by an inherited GIT_INDEX_FILE/GIT_DIR, and git
# exports those into hook children -- the 2026-08-06 fleet-wide false block.
# Scrub them unless a caller explicitly wants the in-flight parent index.
GIT_REPO_SCOPED_ENV = (
    "GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE", "GIT_PREFIX", "GIT_INDEX_VERSION",
)


def proxy_disabled(variable: str = "PIN_INVARIANT_DISABLE_PROXY") -> bool:
    """Has the caller explicitly disabled the proxy?

    Intended semantics, stated here because the name has already misled once:
    the exact string ``"1"`` disables the proxy; ``"0"``, empty, and unset all
    leave it ENABLED.

    This is a VALUE check, not a presence check, and the distinction is load
    bearing. ``os.environ.get(name)`` used as a boolean is true for ``"0"`` --
    ``"0"`` is a non-empty string, and non-empty strings are truthy in Python.
    So the presence form fired for ``DISABLE_PROXY=0``, which is the natural way
    to write "do not disable", and silently turned the proxy OFF. Matches the
    ``env_flag(name, "1")`` contract used elsewhere in the tree; any other value,
    ``"true"`` included, leaves the proxy enabled rather than guessing.
    """
    return os.environ.get(variable) == "1"


def git(repo: Path, *args: str, network: bool = False, inherit_repo_env: bool = False):
    env = dict(os.environ)
    if not inherit_repo_env:
        for name in GIT_REPO_SCOPED_ENV:
            env.pop(name, None)
    command: list[str] = []
    if network and not proxy_disabled():
        proxy = shutil.which(os.environ.get("WITH_PROXY", "with-proxy"))
        if proxy:
            command.append(proxy)
    command.extend(("git", "-C", str(repo), *args))
    return subprocess.run(command, text=True, capture_output=True, check=False, env=env)


def recorded_gitlink(root: Path, product: str) -> str | None:
    """The commit the parent RECORDS for <product> -- not what is checked out.

    Reads the index (so a staged bump is seen, which is what a pre-commit
    consumer needs) and falls back to HEAD.
    """
    staged = git(root, "ls-files", "--stage", "-z", "--", product, inherit_repo_env=True)
    if staged.returncode == 0:
        for entry in filter(None, staged.stdout.split("\0")):
            meta, _, name = entry.partition("\t")
            fields = meta.split()
            if name == product and len(fields) >= 2 and fields[0] == "160000":
                return fields[1]
    head = git(root, "rev-parse", f"HEAD:{product}")
    sha = head.stdout.strip()
    return sha if head.returncode == 0 and FULL_SHA.match(sha) else None


def _walk(value: object) -> list[str]:
    pins: list[str] = []
    if isinstance(value, Mapping):
        if value.get("git") == REVERIE_GIT_URL and isinstance(value.get("rev"), str):
            pins.append(value["rev"])
        for child in value.values():
            pins.extend(_walk(child))
    elif isinstance(value, list):
        for child in value:
            pins.extend(_walk(child))
    return pins


def manifest_pins(hermit: Path, commit: str | None) -> tuple[set[str], int, bool]:
    """Reverie revs in tracked hermit Cargo.toml, read AT THE RECORDED COMMIT.

    Reading the recorded tree rather than the working tree is deliberate: a
    colleague receives the gitlink, so the invariant must be about the gitlink's
    contents, not about another agent's uncommitted edits.

    Returns (pins, entries, readable). `readable` is False when the recorded
    tree could not be listed at all -- an uninitialised submodule, say -- and
    that is reported separately from "listed fine, pins nothing". Both used to
    arrive here as an empty set, so "I could not look" was indistinguishable
    from "I looked and found none", and only the second is a pin defect.
    """
    if commit is None:
        return set(), 0, False
    listed = git(hermit, "ls-tree", "-r", "-z", "--name-only", commit)
    if listed.returncode != 0:
        return set(), 0, False
    pins: list[str] = []
    for name in filter(None, listed.stdout.split("\0")):
        if not name.endswith("Cargo.toml"):
            continue
        blob = git(hermit, "cat-file", "blob", f"{commit}:{name}")
        if blob.returncode != 0:
            continue
        try:
            pins.extend(_walk(tomllib.loads(blob.stdout)))
        except tomllib.TOMLDecodeError:
            continue
    return set(pins), len(pins), True


def is_own_repo(path: Path) -> bool:
    """Is `path` the root of its own git repository?

    An uninitialised submodule is an EMPTY DIRECTORY inside the parent, and
    `git -C` there walks up and answers about the PARENT. So
    `remote_main(root / "reverie")` returned dev-hermit's origin/main and it was
    compared against reverie's gitlink -- two unrelated shas, an automatic and
    entirely bogus "currency violation". Confirming the directory is its own
    repository root is what stops a missing checkout from being answered for by
    the repository that contains it.
    """
    result = git(path, "rev-parse", "--show-toplevel")
    if result.returncode != 0:
        return False
    try:
        return Path(result.stdout.strip()).resolve() == path.resolve()
    except OSError:
        return False


def remote_main(repo: Path) -> str | None:
    result = git(repo, "ls-remote", "--exit-code", "origin", MAIN_REF, network=True)
    if result.returncode != 0 or not result.stdout.strip():
        return None
    sha = result.stdout.split(maxsplit=1)[0]
    return sha if FULL_SHA.match(sha) else None


def check_legs(root: Path, *, offline: bool = False,
               out: TextIO = sys.stdout) -> tuple[list[str], list[str]]:
    """Return (violations, unmeasurable legs).

    A leg is UNMEASURABLE when the inputs it needs cannot be read here -- an
    uninitialised submodule, an unreachable remote. That is not the same claim
    as "the pin disagrees", and collapsing the two in either direction is a
    defect: reporting it as a violation cries wolf in every fresh worktree,
    and reporting it as holding is the fake green this file exists to prevent.
    """
    violations: list[str] = []
    unknowns: list[str] = []
    hermit_link = recorded_gitlink(root, "hermit")
    reverie_link = recorded_gitlink(root, "reverie")

    def report(leg: str, ok: bool, detail: str) -> None:
        print(f"  [{'ok  ' if ok else 'FAIL'}] {leg}: {detail}", file=out)
        if not ok:
            violations.append(leg)

    def unknown(leg: str, detail: str) -> None:
        print(f"  [????] {leg}: {detail}", file=out)
        unknowns.append(leg)

    print("reverie pin invariant (recorded gitlinks, not checkouts)", file=out)

    # A: internal consistency of the recorded hermit tree.
    pins, count, readable = manifest_pins(root / "hermit", hermit_link)
    if hermit_link is None:
        report("A internal", False, "no recorded hermit gitlink to read manifests from")
    elif not readable:
        unknown("A internal", f"cannot read the recorded hermit tree at {hermit_link[:12]} "
                              f"({root / 'hermit'} is not a readable checkout; "
                              "run `git submodule update --init hermit`)")
    elif count == 0:
        report("A internal", False, f"no tracked Cargo.toml pins reverie at {hermit_link[:12]}")
    elif len(pins) != 1:
        report("A internal", False, f"{len(pins)} distinct revs across {count} entries: "
                                    + ",".join(sorted(p[:12] for p in pins)))
    else:
        report("A internal", True, f"one rev {next(iter(pins))[:12]} across {count} entries")

    # B: the manifests' rev must equal the reverie gitlink a colleague receives.
    if len(pins) == 1 and reverie_link:
        pin = next(iter(pins))
        report("B coherence", pin == reverie_link,
               f"manifest {pin[:12]} vs recorded reverie gitlink {reverie_link[:12]}")
    elif not readable or reverie_link is None:
        unknown("B coherence", "cannot compare: "
                               + ("the recorded hermit tree is unreadable"
                                  if not readable else "no recorded reverie gitlink"))
    else:
        report("B coherence", False,
               "cannot compare: the recorded hermit tree does not pin exactly one rev")

    # C/D: currency of the recorded pins against each product's own main.
    if offline:
        print("  [skip] C currency, D currency: --offline", file=out)
        return violations, unknowns
    for leg, product, link in (("C currency", "reverie", reverie_link),
                               ("D currency", "hermit", hermit_link)):
        if link is None:
            report(leg, False, f"{product}: no recorded gitlink")
            continue
        if not is_own_repo(root / product):
            unknown(leg, f"{product}: {root / product} is not its own checkout, so its "
                         "origin/main cannot be read here (an uninitialised submodule "
                         "would otherwise be answered for by the parent repository)")
            continue
        main = remote_main(root / product)
        if main is None:
            unknown(leg, f"{product}: origin/main unavailable (no network, or no such remote)")
        else:
            report(leg, link == main,
                   f"{product} gitlink {link[:12]} vs origin/main {main[:12]}")
    return violations, unknowns


def check(root: Path, *, offline: bool = False, out: TextIO = sys.stdout) -> list[str]:
    """Return a list of violations; empty means no leg was measured as violated.

    Kept for callers that only ask "did any leg disagree". It cannot answer
    "was every leg measurable" -- use `check_legs` for that.
    """
    return check_legs(root, offline=offline, out=out)[0]


def main(argv: list[str] | None = None, out: TextIO = sys.stdout) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--strict", action="store_true",
                        help="accepted and ignored; violations exit 1 with or without it")
    parser.add_argument("--report-only", action="store_true", dest="report_only",
                        help="print the legs and exit 0 whatever they say")
    parser.add_argument("--offline", action="store_true", help="skip the networked currency legs")
    args = parser.parse_args(argv)

    violations, unknowns = check_legs(args.root.resolve(), offline=args.offline, out=out)
    print(file=out)
    if violations:
        print(f"pin invariant VIOLATED on {len(violations)} leg(s): {', '.join(violations)}", file=out)
        print("  a colleague cloning this commit and running `git submodule update --init`"
              " receives the recorded gitlinks, not this box's checkouts.", file=out)
    if unknowns:
        print(f"pin invariant UNMEASURABLE on {len(unknowns)} leg(s): {', '.join(unknowns)}", file=out)
        print("  this is not a clean bill of health: those legs were not checked at all.", file=out)
    if not violations and not unknowns:
        print("pin invariant holds on every leg.", file=out)
    if args.report_only:
        return 0
    if violations:
        return 1
    return 2 if unknowns else 0


if __name__ == "__main__":
    raise SystemExit(main())
