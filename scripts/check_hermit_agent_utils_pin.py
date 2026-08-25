#!/usr/bin/env python3
"""Measure how far HERMIT's recorded agent-utils gitlink trails agent-utils main.

THE GAP THIS FILLS. Two checks already watch agent-utils pins and neither watches
this one:

  * `scripts/check-agent-utils-pin.rs` compares the PARENT's agent-utils gitlink,
    the parent's agent-utils checkout, and agent-utils `origin/main`. It never
    reads hermit's own tree.
  * `scripts/check_reverie_pin_invariant.py` legs C and D cover currency for the
    parent's reverie and hermit gitlinks. There is no leg for the edge INSIDE
    hermit.

So hermit's own agent-utils gitlink had nothing looking at it. Measured
2026-08-22: it had not moved since 2026-08-10 and sat 212 commits behind
agent-utils main, twice the parent's own lag, with every gate green.

WHY THIS EDGE IS WORTH ITS OWN CHECK. agent-utils is not an ordinary dependency
of hermit -- it IS hermit's validation harness. `hermit/scripts/validate.rs` is a
rust-script whose embedded manifest declares
`safe-ci-dag-runner = { path = "../agent-utils/rs/safe-ci-dag-runner" }`, and
every DAG gate executes as a safe-ci-dag-runner node. An unwatched pin here means
the thing enforcing the merge gates can rot silently, and the bill arrives all at
once. Clearing the 212-commit lag took 11 compile errors, one self-test bracket
that had been pinned to an upstream bug, and a scheduler refusal that stopped all
60 nodes before one started -- see
https://github.com/rrnewton/hermit/pull/2362.

WHAT IT READS, AND WHY THAT AND NOT A CHECKOUT. The chain is
parent -> recorded hermit gitlink -> that hermit tree's recorded agent-utils
gitlink. Both hops read RECORDED gitlinks, never a checkout and never a grep of
a working tree: a colleague who clones the parent and runs
`git submodule update --init` receives the gitlinks, so an invariant about what
others receive has to be about the gitlinks. A working-tree grep is specifically
wrong here -- run against this repo on 2026-08-22 one returned five different
shas for a pin where main carried one.

IT COMPOSES WITH LEG D RATHER THAN DUPLICATING IT. This answers "for the hermit
commit the parent records, is ITS agent-utils pin current". Whether the parent
records a current hermit commit is leg D's question and is deliberately left
there. Hold both and hermit main's agent-utils pin is current; there is no third
place to look.

A LAG BUDGET, NOT EQUALITY, AND THE NUMBER IS MEASURED. An equality leg is the
right shape for reverie, which moves slowly. It is the wrong shape here.
Measured 2026-08-22, agent-utils main took 265 commits in 14 days (18.9/day) and
70 of those touched `rs/safe-ci-dag-runner/src` (5.0/day), so an equality check
would go red within about an hour of any bump. A gate that is red every hour is
furniture within a week, and then a real 212-commit drift looks exactly like the
noise -- the same way `check-agent-utils-pin.rs` reasons about in-flight versus
stranded commits.

WHY THE DEFAULT IS 30 COMMITS. Two independent measurements agree on it.

  * The changes that actually broke hermit across those 212 commits arrived in
    four episodes, at positions 9, 67, 158 and 196 counting from the old pin
    (per-platform CPU-budget multiplier and the write-domain fields; the
    max-cpus split; --keep-going; run_id). The gaps between episode starts are
    58, 91 and 38 commits, so 38 is the smallest observed interval between
    hermit-breaking changes. Staying under it means the gate fires while at most
    one breaking change has accumulated, when the bump is usually mechanical.
  * Of the last twelve commits on hermit main that moved this gitlink, ten
    advanced it 30 commits or fewer (0, 1, 1, 2, 4, 6, 8, 13, 16, 29, 30, 126).
    So 30 also sits at the cadence hermit actually ran at, and would not have
    fired during normal operation.

At 18.9 commits/day that is about 1.6 days of upstream movement. Retune it with
`--max-lag` and evidence, not by padding: raising it past 38 buys silence at the
price of letting a second breaking change land before anyone is told.

MEASURED ALONG LINEAR ANCESTRY, AND NON-ANCESTOR IS A STATE, NOT A NUMBER.
`rev-list --count PIN..main` only means anything when the pin is an ancestor of
main, so that is checked first. If agent-utils main is ever rewritten the pin
stops being an ancestor, and reporting "lag N" there would invite a bump that
moves the pin off its own history while satisfying a naive equality check. That
case is reported loudly and never given an N. This mirrors
`ci-hub/health/reverie_pin_lag.py`, which exists for the same reason.

FAIL CLOSED, AND FAIL WITHOUT NEEDING A FLAG. An unreadable gitlink, an
unreachable remote, or a missing object is `unknown` and exits 2; "I could not
measure the lag" must never read as "the lag is zero". And exceeding the budget
exits 1 on its own, with no `--strict` to remember: the neighbouring
`gate pin-invariant` line in the Makefile invokes
`check_reverie_pin_invariant.py` WITHOUT `--strict`, so that gate prints
"pin invariant VIOLATED on 2 leg(s)" and still reports OK. A gate whose teeth
depend on the caller passing a flag is a gate that will eventually be invoked
without it.

    check_hermit_agent_utils_pin.py                  # fetch, report, exit 0/1/2
    check_hermit_agent_utils_pin.py --max-lag 10     # a tighter budget
    check_hermit_agent_utils_pin.py --no-fetch       # trust local refs as they are
    check_hermit_agent_utils_pin.py --offline        # skip entirely, exit 0
    check_hermit_agent_utils_pin.py --json           # the measurement as JSON
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import TextIO

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

# ONE definition of "what is the recorded gitlink", "where is a product's main",
# and "how do we shell out to git". Re-deriving any of them here would create a
# second answer to a question this repo already answers.
from check_reverie_pin_invariant import (  # noqa: E402
    FULL_SHA,
    git,
    recorded_gitlink,
    remote_main,
)

CURRENT = 0
OVER_BUDGET = 1
UNKNOWN = 2

MAIN_REF = "refs/heads/main"

# See "WHY THE DEFAULT IS 30 COMMITS" above. Do not raise this without redoing
# both measurements; the docstring records how.
DEFAULT_MAX_LAG = 30


def bound_to_itself(repo: Path) -> str | None:
    """Reason `repo` cannot be trusted to answer about itself, or None if it can.

    An UNINITIALISED submodule is an empty directory inside the parent repo, so
    `git -C agent-utils ...` silently walks up and answers about the PARENT --
    it would resolve a parent sha and then report the agent-utils pin missing,
    and every number after that would describe the wrong repository. A fresh
    parent worktree has exactly this shape, so this is not a hypothetical.
    """
    if not repo.is_dir():
        return f"no checkout at {repo}"
    top = git(repo, "rev-parse", "--show-toplevel")
    if top.returncode != 0:
        return f"{repo} is not a git checkout"
    if Path(top.stdout.strip() or ".").resolve() != repo.resolve():
        return (
            f"{repo} is not its own git checkout (submodule uninitialised?); "
            "refusing to measure, because git would answer about the parent repo"
        )
    return None


def hermit_agent_utils_gitlink(hermit: Path, hermit_commit: str) -> str | None:
    """The agent-utils commit hermit RECORDS at `hermit_commit`."""
    found = git(hermit, "rev-parse", f"{hermit_commit}:agent-utils")
    sha = found.stdout.strip()
    return sha if found.returncode == 0 and FULL_SHA.match(sha) else None


def measure(root: Path, *, fetch: bool = True, max_lag: int = DEFAULT_MAX_LAG) -> dict:
    """Report hermit's recorded agent-utils pin, agent-utils main, and the lag."""
    hermit = root / "hermit"
    agent_utils = root / "agent-utils"

    def unknown(reason: str) -> dict:
        return {"state": "unknown", "reason": reason, "max_lag": max_lag}

    hermit_pin = recorded_gitlink(root, "hermit")
    if not hermit_pin:
        return unknown("no recorded hermit gitlink at the parent")
    problem = bound_to_itself(hermit)
    if problem:
        return unknown(problem)
    problem = bound_to_itself(agent_utils)
    if problem:
        return unknown(problem)

    if git(hermit, "cat-file", "-e", f"{hermit_pin}^{{commit}}").returncode != 0:
        return unknown(
            f"the parent records hermit {hermit_pin[:12]}, which is not present in "
            f"{hermit}; cannot read the agent-utils gitlink it carries"
        )

    pin = hermit_agent_utils_gitlink(hermit, hermit_pin)
    if not pin:
        return unknown(
            f"hermit {hermit_pin[:12]} records no agent-utils gitlink at path 'agent-utils'"
        )

    main = remote_main(agent_utils)
    if not main:
        return unknown("could not resolve agent-utils origin/main")

    # ls-remote gave us the sha; counting needs the objects locally.
    if fetch:
        fetched = git(agent_utils, "fetch", "--quiet", "origin", MAIN_REF, network=True)
        if fetched.returncode != 0:
            return unknown(f"could not fetch agent-utils main: {fetched.stderr.strip()[:80]}")

    common = {
        "hermit_commit": hermit_pin,
        "pin": pin,
        "main": main,
        "max_lag": max_lag,
    }
    for commit in (pin, main):
        if git(agent_utils, "cat-file", "-e", f"{commit}^{{commit}}").returncode != 0:
            return unknown(f"agent-utils commit {commit[:12]} is not present in {agent_utils}")

    if pin == main:
        return {"state": "current", "lag": 0, **common}

    # Ancestry FIRST. A lag over a pin that is not an ancestor is a number with
    # no meaning, and printing one would invite a bump that is not an advance.
    if git(agent_utils, "merge-base", "--is-ancestor", pin, main).returncode != 0:
        return {
            "state": "non-ancestor",
            "reason": (
                "hermit's recorded agent-utils pin is NOT an ancestor of agent-utils main: "
                "that history was rewritten, or the pin is off-branch. There is no monotonic "
                "advance to make. Do NOT bump to main -- that would move the pin off its own "
                "ancestry while satisfying an equality check."
            ),
            **common,
        }

    counted = git(agent_utils, "rev-list", "--count", f"{pin}..{main}")
    if counted.returncode != 0 or not counted.stdout.strip().isdigit():
        return unknown("could not count commits between hermit's agent-utils pin and main")
    return {"state": "lagging", "lag": int(counted.stdout.strip()), **common}


def verdict(result: dict) -> int:
    """Exit status for a measurement. Unmeasurable and non-ancestor fail closed."""
    state = result["state"]
    if state == "unknown":
        return UNKNOWN
    if state == "non-ancestor":
        # No honest lag exists, so no budget can excuse it.
        return OVER_BUDGET
    if state == "current":
        return CURRENT
    return OVER_BUDGET if result["lag"] > result["max_lag"] else CURRENT


def render(result: dict) -> str:
    state = result["state"]
    if state == "unknown":
        return f"hermit's agent-utils pin lag UNMEASURABLE: {result['reason']}"
    pin = result["pin"][:12]
    main = result["main"][:12]
    hermit_commit = result["hermit_commit"][:12]
    if state == "non-ancestor":
        return (
            f"hermit {hermit_commit} records agent-utils {pin}, which is NOT AN ANCESTOR "
            f"of agent-utils main {main} -- {result['reason']}"
        )
    lag = result["lag"]
    budget = result["max_lag"]
    if state == "current":
        return (
            f"hermit {hermit_commit} records agent-utils {pin}, which is agent-utils main "
            f"(0 commits behind, budget {budget})"
        )
    if lag <= budget:
        return (
            f"hermit {hermit_commit} records agent-utils {pin}, {lag} commit(s) behind "
            f"agent-utils main {main} along linear ancestry; within the {budget}-commit budget"
        )
    return (
        f"hermit {hermit_commit} records agent-utils {pin}, {lag} commit(s) behind agent-utils "
        f"main {main} along linear ancestry, over the {budget}-commit budget. agent-utils is "
        f"hermit's validation harness -- scripts/validate.rs compiles against it -- so this is "
        f"the gate that rots. Bump hermit's agent-utils gitlink and land it; the budget exists "
        f"so the bump is still small enough to be mechanical."
    )


def main(argv: list[str] | None = None, out: TextIO = sys.stdout) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument(
        "--max-lag", type=int, default=DEFAULT_MAX_LAG,
        help=f"commits behind agent-utils main that still pass (default {DEFAULT_MAX_LAG};"
             " the docstring records how that number was measured)",
    )
    parser.add_argument("--no-fetch", action="store_true", help="trust local refs as they are")
    parser.add_argument("--offline", action="store_true", help="skip the check entirely, exit 0")
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args(argv)

    if args.offline:
        print("hermit agent-utils pin currency: [skip] --offline", file=out)
        return CURRENT

    result = measure(args.root.resolve(), fetch=not args.no_fetch, max_lag=args.max_lag)
    code = verdict(result)

    if args.as_json:
        print(json.dumps({**result, "exit": code}, indent=2, sort_keys=True), file=out)
        return code

    # key=value lines, matching reverie_pin_lag.py: tick-hub substitutes a
    # reminder's title from a gate's captured fields, and a producer that prints
    # only prose refuses emission with an unresolved placeholder -- the gate then
    # runs, judges correctly, and says nothing.
    fields = {"state": result["state"], "summary": render(result)}
    for key in ("hermit_commit", "pin", "main", "lag", "max_lag", "reason"):
        if key in result:
            fields[key] = result[key]
    for key, value in fields.items():
        print(f"{key}={value}", file=out)

    if code != CURRENT:
        print(f"ERROR: {render(result)}", file=sys.stderr)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
