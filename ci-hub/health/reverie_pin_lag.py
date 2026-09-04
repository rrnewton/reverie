#!/usr/bin/env python3
"""Measure how far the parent origin/main Reverie gitlink trails Reverie main.

WHY A LAG COUNT AND NOT AN EQUALITY CHECK. `scripts/check_reverie_pin_invariant.py`
leg C already answers "is the pin equal to reverie origin/main" -- a boolean. That
is the right shape for an invariant and the wrong shape for a warning: it cannot
distinguish one commit behind (routine, someone will bump it) from twenty (the
pin has been forgotten and PRs are silently blocking on it). The owner asked for
the staged version directly:

    < 5 commits behind  -> a small note
    5 or more           -> get louder

MEASURED ALONG LINEAR ANCESTRY, NOT TIP DISTANCE. `rev-list --count PIN..main`
counts commits reachable from main and not from the pin. That number is only
meaningful when the pin is an ANCESTOR of main, which is checked first and
reported as its own state when it fails -- see below.

THE MONOTONICITY TRAP THIS FILE EXISTS TO AVOID. The pin advances MONOTONICALLY
along reverie's linear ancestry; it is never "synced" to whatever reverie main
happens to be. Those are the same operation only while main is forward-only. If
reverie main is ever force-pushed or rewritten, the recorded pin stops being an
ancestor, and a catch-up job implementing "bump to main" literally would jump the
pin off its own history -- passing a naive equality check while destroying the
hermetic link the pin exists to provide. So NON-ANCESTOR is a first-class state
here, reported loudly and never counted as "lag N", because there is no honest N.

NO SUPPRESSION, NO SNOOZE, NO CLEARING FLAG. The only input that quiets this tool
is a smaller lag, and the only way to get one is to bump the pin. A warning that
can be silenced any other way is the warning that rotted over nine hours on
2026-08-11: it fired ~30 times, stayed accurate the whole time, and became
furniture. Staging is not suppression -- the small note is still emitted every
cycle; it is emitted at a severity that does not compete with a live incident.

FAIL CLOSED. An unreadable gitlink, an unreachable remote, or an unresolvable
commit is `unknown` and exits 2. "I could not measure the lag" must never read as
"the lag is zero", which is the same absence-reads-as-success shape the ledger
staleness work removed from validate-status.

    reverie_pin_lag.py                          # report, exit 0
    reverie_pin_lag.py --gate --min-lag 1 --max-lag 4    # the quiet note band
    reverie_pin_lag.py --gate --min-lag 5                # the escalation band
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

TOOL_ROOT = Path(__file__).resolve().parents[2]
ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", TOOL_ROOT)).resolve()
sys.path.insert(0, str(TOOL_ROOT / "scripts"))

# ONE definition of "what is published on parent main" and "where is reverie main".
# Re-deriving either here would create a second answer to a question this repo
# already answers, which is the sprawl class the one-definition audit named.
from check_reverie_pin_invariant import (  # noqa: E402
    git,
    published_gitlink,
    remote_main,
)

CURRENT = 0
LAGGING = 1
UNKNOWN = 2

MAIN_REF = "refs/heads/main"


def measure(root: Path = ROOT) -> dict:
    """Report the pin, reverie main, ancestry, and the linear-ancestry lag."""
    reverie = root / "reverie"
    pin = published_gitlink(root, "reverie")
    if not pin:
        return {
            "state": "unknown",
            "reason": "could not read the Reverie gitlink from freshly fetched parent origin/main",
        }
    if not reverie.is_dir():
        return {"state": "unknown", "reason": f"no reverie checkout at {reverie}"}
    # An UNINITIALISED submodule is an empty directory INSIDE the parent repo, so
    # `git -C reverie ...` silently walks up and answers about the PARENT. Caught
    # live while building this: it fetched the parent's origin/main and resolved a
    # parent sha, then reported the reverie pin "not present locally". Every number
    # after that point would have described the wrong repository. Bind the checkout
    # to itself before trusting a single git answer from it.
    toplevel = git(reverie, "rev-parse", "--show-toplevel")
    if toplevel.returncode != 0 or Path(toplevel.stdout.strip() or ".").resolve() != reverie.resolve():
        return {
            "state": "unknown",
            "reason": (
                f"{reverie} is not its own git checkout (submodule uninitialised?); "
                "refusing to measure, because git would answer about the parent repo"
            ),
        }

    main = remote_main(reverie)
    if not main:
        return {"state": "unknown", "reason": "could not resolve reverie origin/main"}

    # The count needs the objects locally; ls-remote only gave us the sha.
    fetched = git(reverie, "fetch", "--quiet", "origin", MAIN_REF, network=True)
    if fetched.returncode != 0:
        return {
            "state": "unknown",
            "reason": f"could not fetch reverie main: {fetched.stderr.strip()[:80]}",
        }
    for commit in (pin, main):
        if git(reverie, "cat-file", "-e", f"{commit}^{{commit}}").returncode != 0:
            return {"state": "unknown", "reason": f"commit {commit[:12]} not present locally"}

    if pin == main:
        return {"state": "current", "pin": pin, "main": main, "lag": 0}

    # Ancestry FIRST. A lag count over a pin that is not an ancestor is a number
    # with no meaning, and printing one would invite a bump that is not an advance.
    if git(reverie, "merge-base", "--is-ancestor", pin, main).returncode != 0:
        return {
            "state": "non-ancestor",
            "pin": pin,
            "main": main,
            "reason": (
                "the recorded pin is NOT an ancestor of reverie main: reverie history was "
                "rewritten or the pin is off-branch. There is no monotonic advance to make. "
                "Do NOT bump to main -- that would move the pin off its own ancestry."
            ),
        }

    counted = git(reverie, "rev-list", "--count", f"{pin}..{main}")
    if counted.returncode != 0 or not counted.stdout.strip().isdigit():
        return {"state": "unknown", "reason": "could not count commits between pin and main"}
    return {"state": "lagging", "pin": pin, "main": main, "lag": int(counted.stdout.strip())}


def render(result: dict) -> str:
    state = result["state"]
    if state == "current":
        return (
            "parent origin/main Reverie gitlink is current at "
            f"{result['pin'][:12]} (0 commits behind main)"
        )
    if state == "unknown":
        return f"reverie pin lag UNMEASURABLE: {result['reason']}"
    if state == "non-ancestor":
        return (
            f"reverie pin {result['pin'][:12]} is NOT AN ANCESTOR of main "
            f"{result['main'][:12]} -- {result['reason']}"
        )
    lag = result["lag"]
    return (
        f"parent origin/main Reverie gitlink {result['pin'][:12]} is {lag} commit(s) behind "
        f"reverie main {result['main'][:12]} along linear ancestry. Clear it by bumping the "
        f"pin one monotonic step at a time; there is no other way to quiet this."
    )


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--gate", action="store_true", help="exit nonzero inside the lag band")
    ap.add_argument("--min-lag", type=int, default=1, help="band floor, inclusive")
    ap.add_argument(
        "--max-lag", type=int, default=None,
        help="band ceiling, inclusive; omit for unbounded (the escalation band)",
    )
    ap.add_argument("--json", action="store_true", dest="as_json")
    ap.add_argument(
        "--root",
        type=Path,
        default=ROOT,
        help="mutable parent checkout to inspect (default: DEV_HERMIT_PARENT or tool root)",
    )
    args = ap.parse_args(argv)

    result = measure(args.root.resolve())
    if args.as_json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        # EMIT key=value LINES, NOT A BARE STRING.
        #
        # tick-hub substitutes {placeholders} in a reminder's title from the
        # captured `key=value` fields of its gate. Both reverie_pin_lag reminders
        # title on "{summary}", and this producer printed only prose, so every
        # tick all day refused with "refusing emission with unresolved
        # placeholder(s): {summary}". The gate ran, judged correctly, exited with
        # the right code -- and said nothing. Meanwhile the condition it exists to
        # announce became the thing blocking the parent snapshot, and the warning
        # reached the coordinator down a different path entirely.
        #
        # `summary` is the same prose render() always produced, so the human-facing
        # text is unchanged; it is now merely addressable. The extra fields let a
        # reader see the numbers without re-running with --json.
        fields = {"state": result["state"], "summary": render(result)}
        for key in ("pin", "main", "lag", "reason"):
            if key in result:
                fields[key] = result[key]
        for key, value in fields.items():
            print(f"{key}={value}")
    if not args.gate:
        return 0

    # Fail closed, and loudly: an unmeasurable pin and a rewritten reverie history
    # both fire in EVERY band on purpose. Routing them to one band would mean a
    # band could be the only one watching, and whichever band was quiet that cycle
    # would silently own the emergency.
    if result["state"] in {"unknown", "non-ancestor"}:
        return UNKNOWN
    if result["state"] == "current":
        return CURRENT
    lag = result["lag"]
    if lag >= args.min_lag and (args.max_lag is None or lag <= args.max_lag):
        return LAGGING
    return CURRENT


if __name__ == "__main__":
    raise SystemExit(main())
