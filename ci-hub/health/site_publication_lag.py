#!/usr/bin/env python3
"""Measure how far the PUBLISHED compatibility website trails the evidence available for it.

WHY THIS EXISTS. On 2026-09-15 the site at
https://rrnewton.github.io/hermit/compatibility/latest/ was found serving a
2026-09-06 snapshot. It had been nine days stale and nothing reported it. The
deploy was not broken: `docs.yml` had last succeeded on 2026-09-10 and would have
succeeded on any night in between. It republished the SAME pinned snapshot every
time, because the pin still named the 09-06 identity.

That is the whole reason this is two measurements and not one. A nightly job that
just redeploys is not a freshness mechanism -- it would have run green through the
entire nine-day window and changed nothing. The two states need different remedies
and only one of them is cheap, so collapsing them into a single "is the site
stale" boolean produces a warning nobody can act on.

    PIN LAG     a newer retained full-path validate run exists, but no release
                pins it. Remedy: build an artifact from that run, open a pin PR,
                get it reviewed, land it, then deploy. Expensive: the validate run
                behind it measured 4413 CPU-seconds (record devbig014-1789163515-1449760).
    DEPLOY LAG  `origin/main` already pins an identity that gh-pages is not
                serving. Remedy: dispatch docs.yml on main. Cheap: four runs
                measured 174-224 seconds wall, of which the compatibility work is
                8-14 seconds and rustdoc is the rest.

DETECT ONLY, AND DELIBERATELY SO. This reports; it never publishes. Three separate
constraints make an auto-publishing nightly the wrong tool, and each one of them
would have to be relaxed by the owner, not by this file:
  1. Owner directive 2026-08-21, quoted verbatim in five workflow files: GitHub CI
     "does not run automatically". Every workflow in both repositories is
     workflow_dispatch only; there is no `schedule:` trigger anywhere. The
     repository Actions switch and those triggers are ONE mechanism -- adding a
     schedule here would break the directive repository-wide and silently.
  2. tick-hub's own admission rule: "Prefer DETECT-ONLY: a gate that reports a
     fact leaves the decision where the authority is." The neighbouring
     merge_gate_refire_due gate has its action ready and still does not auto-fire.
  3. The pin update is a reviewed change. `gh-merge-verified` exits 6 when the
     required exact-head review lane is absent, so an unattended job cannot land
     the pin without bypassing the admission checks -- which is not automation,
     it is removing the gate.
It passes tick-hub's admission test (CAN ORC OBSERVE THIS DIRECTLY? No -- this is
gh-pages content and GitHub release state, exactly the external surface the
coordinator cannot see).

FAIL CLOSED. Any side that cannot be read is `unknown` and exits 2. "I could not
measure the lag" must never read as "the lag is zero" -- the absence-reads-as-
success shape this repo has removed elsewhere. A nine-day stale site that reports
CURRENT because a fetch failed is worse than no gate at all.

NO SUPPRESSION, NO SNOOZE. The only input that quiets this tool is a fresher
published site.

    site_publication_lag.py                       # report, exit 0
    site_publication_lag.py --json                # machine readable
    site_publication_lag.py --gate --max-age-days 2   # warn past two days
"""

from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import os
import re
import subprocess
import sys
from pathlib import Path

TOOL_ROOT = Path(__file__).resolve().parents[2]
ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", TOOL_ROOT)).resolve()

CURRENT = 0
LAGGING = 1
UNKNOWN = 2

SITE_URL = "https://rrnewton.github.io/hermit/compatibility/latest/"
DEFAULT_REPO = "rrnewton/hermit"


def _run(cmd: list[str], timeout: int = 120) -> tuple[int, str, str]:
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except (OSError, subprocess.SubprocessError) as exc:  # fail closed
        return 1, "", str(exc)


def _with_proxy(cmd: list[str]) -> list[str]:
    """Egress on this host goes through fwdproxy; without it a fetch dies in ~2ms.

    A 2-millisecond failure is indistinguishable from a fast clean answer unless
    the wrapper is applied deliberately, which is why it is not left to the caller.
    """
    return ["with-proxy", *cmd]


def fetch_published(url: str = SITE_URL) -> dict:
    """Read the live page's own provenance disclosure.

    The page is the authority on what is published because it is literally what a
    reader sees. Deriving "what is published" from the pin instead would answer a
    different question -- the pin is the INTENT, gh-pages is the FACT, and the gap
    between those two is precisely the deploy lag measured here.
    """
    # Cache-busted: a CDN copy would make a just-deployed site read as stale.
    bust = f"?cb={int(dt.datetime.now(dt.timezone.utc).timestamp())}"
    rc, out, err = _run(_with_proxy(["curl", "-sS", "-m", "90", "-L", url + bust]))
    if rc != 0 or not out:
        return {"state": "unknown", "reason": f"could not fetch {url}: rc={rc} {err[:200]}"}

    def field(name: str) -> str | None:
        m = re.search(
            r"<dt>" + re.escape(name) + r"</dt><dd>(.*?)</dd>", out, re.S
        )
        if not m:
            return None
        return html.unescape(re.sub(r"<[^>]+>", "", m.group(1))).strip()

    run_id = field("Source validation run")
    finished = field("Run finished")
    if not run_id or not finished:
        return {
            "state": "unknown",
            "reason": "live page carried no 'Source validation run'/'Run finished' provenance",
        }
    return {
        "state": "read",
        "published_run": run_id,
        "published_run_finished_text": finished,
        "published_record": field("Source validation record"),
        "published_validate_path": field("Validate path"),
        "measured_hermit": field("Measured Hermit commit"),
    }


def newest_retained_full_run(root: Path = ROOT) -> dict:
    """Newest passing full-path validate run recorded in the ledger.

    Scans the ledger rather than asking any single host, because the run that
    should be published is whichever passing full run finished most recently
    anywhere, not whichever one this box happens to remember.
    """
    ledger = root / "ledger" / "hermit"
    if not ledger.is_dir():
        return {"state": "unknown", "reason": f"no ledger directory at {ledger}"}
    best = None
    scanned = 0
    for path in sorted(ledger.glob("*/*.jsonl")):
        try:
            text = path.read_text(errors="replace")
        except OSError:
            continue
        for line in text.splitlines():
            if '"run.result"' not in line:
                continue
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            row = ev.get("legacy_row") or {}
            if row.get("selection_mode") != "full" or row.get("result") != "pass":
                continue
            finished = row.get("finished_at") or ev.get("emitted_at")
            if not finished:
                continue
            scanned += 1
            if best is None or finished > best["finished_at"]:
                best = {
                    "run_id": row.get("run_id"),
                    "record_id": row.get("record_id") or ev.get("event_id"),
                    "finished_at": finished,
                    "host": ev.get("host") or row.get("host"),
                    "commit": row.get("commit"),
                }
    if best is None:
        return {"state": "unknown", "reason": "no passing full-path run.result found in the ledger"}
    best["state"] = "read"
    best["candidates_scanned"] = scanned
    return best


def pin_vs_pages(repo: str) -> dict:
    """Is the identity main pins actually being served by gh-pages?

    This is the DEPLOY lag, and it is the cheap one. Answering it from the pin
    alone is impossible: the pin says what SHOULD be served.
    """
    rc, out, err = _run(
        _with_proxy(
            ["gh", "api", f"repos/{repo}/contents/.github/compatibility-site-releases.json",
             "--jq", ".content"]
        )
    )
    if rc != 0 or not out.strip():
        return {"state": "unknown", "reason": f"could not read the pin on main: {err[:200]}"}
    import base64

    try:
        pins = json.loads(base64.b64decode(out.strip()))
    except (ValueError, TypeError) as exc:
        return {"state": "unknown", "reason": f"pin file did not parse: {exc}"}
    latest = pins.get("latest_identity")
    if not latest:
        return {"state": "unknown", "reason": "pin file carried no latest_identity"}

    rc, out, err = _run(
        _with_proxy(["gh", "api", f"repos/{repo}/contents/compatibility?ref=gh-pages",
                     "--jq", ".[].name"])
    )
    if rc != 0:
        return {"state": "unknown", "reason": f"could not list gh-pages compatibility/: {err[:200]}"}
    served = set(out.split())
    return {
        "state": "read",
        "pinned_identity": latest,
        "pinned_identity_served": latest in served,
        "identities_on_pages": len(served),
    }


def measure(repo: str = DEFAULT_REPO, root: Path = ROOT) -> dict:
    pub = fetch_published()
    avail = newest_retained_full_run(root)
    deploy = pin_vs_pages(repo)

    result: dict = {"published": pub, "newest_available_run": avail, "deploy": deploy}

    for part, name in ((pub, "published site"), (avail, "ledger"), (deploy, "pin/gh-pages")):
        if part.get("state") == "unknown":
            result["state"] = "unknown"
            result["reason"] = f"{name}: {part.get('reason')}"
            return result

    pin_lag = pub["published_run"] != avail["run_id"]
    deploy_lag = not deploy["pinned_identity_served"]

    age_days = None
    finished = avail.get("finished_at")
    if finished:
        try:
            t = dt.datetime.fromisoformat(finished.replace("Z", "+00:00"))
            age_days = (dt.datetime.now(dt.timezone.utc) - t).total_seconds() / 86400.0
        except ValueError:
            age_days = None

    result.update(
        {
            "state": "lagging" if (pin_lag or deploy_lag) else "current",
            "pin_lag": pin_lag,
            "deploy_lag": deploy_lag,
            "newest_available_age_days": None if age_days is None else round(age_days, 2),
        }
    )
    return result


def render(m: dict) -> str:
    if m["state"] == "unknown":
        return f"site-publication-lag: UNKNOWN -- {m['reason']}"
    pub = m["published"]
    avail = m["newest_available_run"]
    dep = m["deploy"]
    if m["state"] == "current":
        return (
            "site-publication-lag: CURRENT -- the published site serves the newest "
            f"retained full-path run {avail['run_id']}, and gh-pages serves the pinned "
            f"identity {dep['pinned_identity'][:16]}"
        )
    parts = []
    if m["pin_lag"]:
        parts.append(
            f"PIN LAG: published run is {pub['published_run']} but the newest retained "
            f"full-path run is {avail['run_id']} (finished {avail['finished_at']}, host "
            f"{avail['host']}, age {m['newest_available_age_days']} days). Remedy: build an "
            "artifact from that run, open a pin PR, have it reviewed, land it with "
            "ci-hub/bin/gh-merge-verified, then dispatch docs.yml."
        )
    if m["deploy_lag"]:
        parts.append(
            f"DEPLOY LAG: main pins {dep['pinned_identity'][:16]} but gh-pages is not serving "
            "it. Remedy (cheap, ~224s): gh workflow run docs.yml --repo "
            f"{DEFAULT_REPO} --ref main"
        )
    return "site-publication-lag: LAGGING -- " + " | ".join(parts)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--gate", action="store_true", help="exit nonzero when lagging or unknown")
    ap.add_argument(
        "--max-age-days",
        type=float,
        default=0.0,
        help="only treat PIN LAG as a warning once the newer run is this old; "
        "deploy lag and unknown always warn",
    )
    args = ap.parse_args(argv)

    m = measure(args.repo)
    if args.json:
        print(json.dumps(m, indent=2, sort_keys=True))
    else:
        print(render(m))
        # key=value lines for tick-hub `capture: true`; prose alone captures nothing.
        if m["state"] != "unknown":
            print(f"pin_lag={m['pin_lag']}")
            print(f"deploy_lag={m['deploy_lag']}")
            print(f"newest_available_age_days={m['newest_available_age_days']}")

    if m["state"] == "unknown":
        return UNKNOWN
    if not args.gate:
        return CURRENT
    if m["deploy_lag"]:
        return LAGGING
    if m["pin_lag"]:
        age = m.get("newest_available_age_days")
        if age is None or age >= args.max_age_days:
            return LAGGING
    return CURRENT


if __name__ == "__main__":
    sys.exit(main())
