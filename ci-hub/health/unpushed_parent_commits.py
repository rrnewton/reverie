#!/usr/bin/env python3
"""Standing check: parent commits that exist ONLY locally.

An unpushed commit emits no error, no failing check and no wakeup. On
2026-08-06 a local-main rewrite orphaned 45 of them; they were recovered only
because someone happened to run an fsck sweep. Three were noticed by their own
authors. The other 42 had nobody looking.

TWO DESIGN RULES, both learned that day:

1.  NEVER REPORT A BOOLEAN. ``scm: dirty`` was useless because it cannot
    distinguish 1 from 45. This prints the COUNT and the SUBJECTS.

2.  SCOPE TO ALL REFS, NOT HEAD. The obvious probe is
    ``rev-list --count HEAD --not --remotes``, but HEAD-scoped counting only
    sees the branch that happens to be checked out. Measured on this repo at
    the time of writing: HEAD-scoped said **1**, all-refs said **25**. Both are
    reported, and the all-refs number is the exposure.

Neither number covers commits that are unreachable from ANY ref -- that is what
the 45 were, and only ``git fsck --lost-found`` finds those. This check exists
to stop commits BECOMING unreachable, by publishing them while a ref still
points at them.

DETECT AND PUBLISH ONLY. This never merges, resets, rewrites, gcs, prunes,
expires a reflog, repacks or cleans. Its only mutation is creating new remote
refs under ``rescue/``, which cannot destroy anything.

OBSERVATION AND RESCUE ARE SEPARATE, AND THE ORDER IS LOAD-BEARING (2026-08-08).
Measured on this repo: the scan takes **0.40s**; a single ``herdr-run`` rescue
round-trip took **65.8s and 74.8s** (both failing, rc=69), and rescue makes TWO
per commit. Under the tick's 30s budget the gate had therefore NEVER emitted a
measurement -- not because it could not measure, but because ``main`` computed
the correct answer in 0.4s, then handed control to rescue, and printed only
afterwards. The timeout killed a process that was already holding a complete
result. **So the report is emitted BEFORE rescue is attempted.** A rescue that
hangs, fails, or is killed can no longer discard a measurement that succeeded.

Corollaries, each measured rather than assumed:

*   Cost does NOT scale with worktree count. 47 worktrees: ``rev-list --all``
    0.13s versus ``--single-worktree`` 0.12s. Narrowing scope would buy 10ms and
    lose the coverage that is the whole point -- head-scoped read 0 while
    all-refs read 1.
*   The rescue transport can be broken while egress is fine: the same ls-remote
    took 0.32s through ``with-proxy`` directly and timed out twice through
    ``herdr-run``. Rescue therefore bounds itself and reports what it skipped.
"""

from __future__ import annotations

import argparse
from collections import Counter
import json
import subprocess
import sys
import time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

PARENT = str(Path(__file__).resolve().parents[2])
HERDR = f"{PARENT}/agent-utils/bin/herdr-run"

# Per-call ceilings for the two remote legs. The old 600s/300s could not be
# reached by any caller with a budget, and one stuck call ate more than the
# whole tick. Measured: a healthy round-trip is sub-second (0.32s direct); a
# broken one runs ~70s. 45s is far above healthy and well below two-stuck-calls.
PUSH_TIMEOUT_SECS = 45
VERIFY_TIMEOUT_SECS = 45
# Total wall ceiling for the whole rescue phase. Rescue is remote mutation, not
# observation, so it gets its OWN budget and must never borrow the scan's.
RESCUE_DEADLINE_SECS = 120


def git(*args: str, cwd: str = PARENT) -> str:
    return bounded_capture.git(args, cwd=cwd).stdout.strip()


def local_only(scope: str) -> list[dict[str, str]]:
    """Commits reachable from `scope` but from no remote-tracking ref.

    Describes every commit in ONE `git log` rather than one per commit. The
    per-commit form cost ~14.5 ms each, so its runtime scaled linearly with the
    number of unpushed commits -- **this gate got slower exactly as the
    condition it watches got worse**. At the 25 commits recorded when it was
    written that was ~0.4 s and invisible; at 1316 it was ~19 s and the gate
    exceeded its 30 s bound and went dark. Measured on the same 1316 commits,
    batching is ~19 s -> ~0.14 s. The scan itself was never the cost: the
    `rev-list` walk above measures 0.11 s, matching the 0.13 s recorded in this
    module's own header, so narrowing scope would not have helped and would have
    cost the coverage that header defends.

    `--no-walk=unsorted` preserves `rev-list` order, and `%H` is read back from
    the output so each row is paired with its own commit rather than by
    position. `%s` is a subject line and cannot contain a newline, so one line
    per commit parses unambiguously; a subject containing tabs still survives
    because the split is bounded.
    """
    # `--all` reaches TAGS, and tags here point into a submodule's history: it
    # reported 1316 commits while zero branches were ahead of a remote, a
    # constant no push could ever reduce. Scoping to `--branches` reports 0 but
    # EXCLUDES A DETACHED HEAD, which is the case this gate exists for -- the
    # shared parent checkout was accidentally detached earlier and a
    # branch-scoped gate would have been silent through it. `--exclude` drops
    # only tags from what the following `--all` considers, so branches, HEAD,
    # every worktree's HEAD, and the rescue and pr-landing namespaces are all
    # still enumerated. Bracketed both ways in a scratch repository: a detached
    # HEAD in a SECONDARY WORKTREE carrying an unpushed commit is reported, and
    # a commit reachable only from a tag is not.
    spec = ["--exclude=refs/tags/*", "--all"] if scope == "all" else ["HEAD"]
    shas = [s for s in git("rev-list", *spec, "--not", "--remotes").splitlines() if s]
    if not shas:
        return []
    listed = bounded_capture.git(
        ["log", "--no-walk=unsorted", "--stdin",
         "--format=%H\t%h\t%an\t%ad\t%s", "--date=short"],
        cwd=PARENT, input="\n".join(shas) + "\n")
    rows = []
    for line in listed.stdout.splitlines():
        if not line:
            continue
        sha, h, an, ad, subj = (line.split("\t", 4) + ["", "", "", "", ""])[:5]
        rows.append({"sha": sha, "short": h, "author": an, "date": ad, "subject": subj})
    return rows


def raw_blob(rev: str, path: str) -> tuple[str, bytes]:
    """Return one exact Git blob as present/absent/error without decoding it."""

    listed = bounded_capture.git(
        ["ls-tree", "-z", rev, "--", path], cwd=PARENT, text=False
    )
    if listed.returncode != 0:
        return "error", b""
    if not listed.stdout:
        return "absent", b""
    content = bounded_capture.git(
        ["show", f"{rev}:{path}"], cwd=PARENT, text=False
    )
    if content.returncode != 0:
        return "error", b""
    return "present", content.stdout


def complete_raw_rows(data: bytes) -> Counter[bytes] | None:
    """Count byte-exact newline-terminated rows, refusing a partial last row."""

    if data and not data.endswith(b"\n"):
        return None
    return Counter(data.splitlines(keepends=True))


def ledger_rows_contained(
    parent: str, head: str, target: str, path: str
) -> tuple[bool, int, int, int]:
    """Whether head is append-only from parent and its raw rows exist at target.

    This deliberately compares complete raw-row multisets. Parsing JSON or
    selecting producer fields would make a rewritten row look equivalent, and
    comparing the whole file would reject an append-only target merely because
    main has advanced.
    """

    parent_state, parent_data = raw_blob(parent, path)
    head_state, head_data = raw_blob(head, path)
    target_state, target_data = raw_blob(target, path)
    if (
        parent_state == "error"
        or head_state != "present"
        or target_state != "present"
    ):
        return False, 0, 0, 0
    parent_rows = complete_raw_rows(parent_data)
    head_rows = complete_raw_rows(head_data)
    target_rows = complete_raw_rows(target_data)
    if parent_rows is None or head_rows is None or target_rows is None:
        return False, 0, 0, 0
    # Append-only means the head cannot delete or rewrite any exact parent row.
    if any(head_rows[row] < count for row, count in parent_rows.items()):
        return False, 0, sum(head_rows.values()), sum(target_rows.values())
    contributed = head_rows - parent_rows
    # Target containment covers both the new rows and every unchanged base row;
    # otherwise a deletion or rewrite that happened after the branch point
    # would be mistaken for publication of the complete head ledger.
    if any(target_rows[row] < count for row, count in head_rows.items()):
        return (
            False,
            sum(contributed.values()),
            sum(head_rows.values()),
            sum(target_rows.values()),
        )
    return (
        True,
        sum(contributed.values()),
        sum(head_rows.values()),
        sum(target_rows.values()),
    )


PUBLISH_TARGET = "origin/main"


def classify_publication(rows: list[dict[str, str]], target: str = PUBLISH_TARGET) -> None:
    """Split local-only commits by CONTENT into `unpublished` and `superseded`.

    A commit reachable from no remote ref is NOT the same fact as unpublished
    WORK. Rebase-then-force-update -- the normal landing path on this fleet --
    orphans the pre-rebase object every single time while its content lands
    perfectly well. This gate's first successful run flagged exactly one of
    those (`ef7cd9b`, whose change was already on main as `f37bd1c8`), and it
    was reported upward as a real catch before anyone checked it.

    A gate whose alarms are usually wrong is worse than no gate: it spends the
    attention a real alarm needs, and it will not be believed on the day it is
    right. The failure this gate exists for -- a 20-commit stack in zero origin
    refs -- is unrecoverable; the failure it produced here costs one check.
    Those are not symmetric, so BOTH tests below are biased toward reporting.

      1. `git cherry` -- patch-id equivalence. Survives rebase and reword, which
         plain blob comparison does not.
      2. Blob identity for ordinary touched paths, which catches a change that
         landed folded into some other commit, where no patch id matches.
      3. For ledger/*.jsonl, byte-exact complete-row multiset containment from
         parent to head. Whole-file identity gives a false alarm whenever main
         has appended later rows; selected-field comparison can miss rewrites.

    Anything else stays `unpublished` and still pages. A conflict resolved
    during a rebase changes the patch and can defeat both tests; that direction
    is deliberate -- it re-reports a superseded commit rather than silencing a
    real one.
    """
    for row in rows:
        sha = row["sha"]
        row["disposition"] = "unpublished"
        row["evidence"] = f"no equivalent patch, and content differs at {target}"
        first = (git("cherry", target, sha).split("\n", 1)[0] or "").strip()
        if first.startswith("-"):
            row["disposition"] = "superseded"
            row["evidence"] = f"equivalent patch already on {target} (git cherry)"
            continue
        paths = [q for q in git("diff", "--name-only", f"{sha}^", sha).splitlines() if q]
        if not paths:
            continue

        def blob(rev: str, path: str) -> str:
            return git("rev-parse", "--verify", "--quiet", f"{rev}:{path}") or "absent"

        contained = True
        ledger_added = 0
        ledger_head = 0
        ledger_target = 0
        ordinary_paths = 0
        for path in paths:
            if path.startswith("ledger/") and path.endswith(".jsonl"):
                ok, added, head_rows, target_rows = ledger_rows_contained(
                    f"{sha}^", sha, target, path
                )
                contained = contained and ok
                ledger_added += added
                ledger_head += head_rows
                ledger_target += target_rows
            else:
                ordinary_paths += 1
                contained = contained and blob(sha, path) == blob(target, path)
        if contained:
            row["disposition"] = "superseded"
            evidence = []
            if ledger_added:
                evidence.append(
                    f"{ledger_added}/{ledger_added} contributed raw ledger row(s) "
                    f"and {ledger_head}/{ledger_head} head raw ledger row(s) present "
                    f"among {ledger_target} target row(s)"
                )
            elif ordinary_paths != len(paths):
                evidence.append("no new raw ledger rows")
            if ordinary_paths:
                evidence.append(
                    f"all {ordinary_paths} other touched path(s) byte-identical"
                )
            row["evidence"] = f"{'; '.join(evidence)} at {target}"


def rescue(
    rows: list[dict[str, str]],
    agent: str,
    dry: bool,
    deadline_secs: float = RESCUE_DEADLINE_SECS,
) -> list[dict[str, str]]:
    """Push each local-only commit to its own rescue ref, then VERIFY at the
    remote. A push exit code is not evidence; the ls-remote re-read is.

    Bounded, and every commit gets an explicit disposition. A commit the budget
    never reached is `skipped-deadline` -- NOT absent, and NOT `verified`. The
    caller can then say what it did and did not publish instead of implying
    coverage it never attempted.
    """
    done: list[dict[str, str]] = []
    started = time.monotonic()
    for index, r in enumerate(rows):
        ref = f"rescue/auto-{r['short']}"
        if dry:
            done.append({**r, "ref": ref, "published": "dry-run"})
            continue
        if time.monotonic() - started >= deadline_secs:
            # Do not start work that cannot finish, and do not pretend the
            # remaining rows were examined.
            done.extend(
                {**row, "ref": f"rescue/auto-{row['short']}",
                 "published": "skipped-deadline"}
                for row in rows[index:]
            )
            break
        try:
            subprocess.run(
                [HERDR, "--agent", agent,
                 f"with-proxy git -C {PARENT} push origin {r['sha']}:refs/heads/{ref}"],
                capture_output=True, text=True, timeout=PUSH_TIMEOUT_SECS)
            seen = subprocess.run(
                [HERDR, "--agent", agent,
                 f"with-proxy git -C {PARENT} ls-remote --heads origin refs/heads/{ref}"],
                capture_output=True, text=True, timeout=VERIFY_TIMEOUT_SECS).stdout
        except subprocess.TimeoutExpired:
            # A stuck transport is a FAILED publish, never a silent success.
            done.append({**r, "ref": ref, "published": "FAILED-timeout"})
            continue
        ok = r["sha"] in seen
        done.append({**r, "ref": ref, "published": "verified" if ok else "FAILED"})
    return done


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--scope", choices=["all", "head"], default="all",
                    help="all = every local ref (the real exposure); head = the "
                         "checked-out branch only (under-reports)")
    ap.add_argument("--rescue", action="store_true",
                    help="publish each local-only commit to rescue/auto-<sha> and "
                         "verify at the remote. A report needs a reader; this does not")
    ap.add_argument("--dry-run", action="store_true", help="with --rescue, do not push")
    ap.add_argument("--rescue-deadline", type=float, default=RESCUE_DEADLINE_SECS,
                    help="wall ceiling for the whole rescue phase; commits the "
                         "budget never reaches are reported skipped-deadline, "
                         "never verified")
    ap.add_argument("--agent", default="hermit-det2")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()

    scan_started = time.monotonic()
    rows = local_only(a.scope)
    head_n = len(local_only("head")) if a.scope == "all" else len(rows)
    classify_publication(rows)
    # `rows` stays the FULL census: rescue and --json must still see every
    # local-only object. Only the ALARM narrows to genuinely unpublished work.
    unpublished = [r for r in rows if r["disposition"] == "unpublished"]
    superseded = [r for r in rows if r["disposition"] == "superseded"]
    scan_secs = time.monotonic() - scan_started

    def emit(published: list[dict[str, str]]) -> None:
        if a.json:
            print(json.dumps({"scope": a.scope, "count": len(rows),
                              "unpublished_count": len(unpublished),
                              "superseded_count": len(superseded),
                              "head_scoped_count": head_n,
                              "scan_seconds": round(scan_secs, 3),
                              "rescued": bool(published),
                              "commits": published or rows}, indent=2), flush=True)
            return
        # COUNT AND SUBJECTS, never a bare boolean.
        print(f"unpushed-parent-commits scope={a.scope} count={len(rows)} "
              f"unpublished={len(unpublished)} superseded={len(superseded)} "
              f"head_scoped_count={head_n} scan_seconds={scan_secs:.2f}", flush=True)
        for r in (published or unpublished):
            extra = f"  -> {r['ref']} [{r['published']}]" if "ref" in r else ""
            print(f"  {r['short']}  {r['date']}  {r['author']:<14.14}  "
                  f"{r['subject'][:72]}{extra}", flush=True)
        # Listed, never hidden: a superseded object is prune-able, not lost,
        # and naming it is what stops the next reader re-deriving it by hand.
        for r in superseded:
            print(f"  [superseded, content published] {r['short']}  "
                  f"{r['subject'][:56]}  ({r['evidence']})", flush=True)
        if not rows:
            print("  (none: every local commit is reachable from a remote ref)",
                  flush=True)
        elif not unpublished:
            print("  (no unpublished work: every local-only commit's content is "
                  "already on the publication target)", flush=True)
        if head_n != len(rows) and a.scope == "all":
            print(f"  NOTE: a HEAD-scoped probe would report {head_n}, missing "
                  f"{len(rows) - head_n}. Scope to --all.", flush=True)
        # tick-hub resolves `{summary}` from `key=value` stdout lines
        # (parse_kv_lines). Without this the emitted warning renders the LITERAL
        # string `{summary}` -- the same unactionable alarm the worktree-liveness
        # gate shipped. It stayed invisible here only because the gate never
        # completed, so it never emitted anything at all. Name the subjects: an
        # alarm that omits its own subject cannot be verified.
        subjects = ", ".join(
            f"{r['short']} {r['subject'][:40]}" for r in unpublished[:3])
        residue = f" (+{len(unpublished) - 3} more)" if len(unpublished) > 3 else ""
        if unpublished:
            summary = (f"{len(unpublished)} parent commit(s) hold UNPUBLISHED "
                       f"work: {subjects}{residue}")
        elif superseded:
            summary = (f"no unpublished work; {len(superseded)} superseded local "
                       f"object(s) are prune-able")
        else:
            summary = "no local-only commits"
        print(f"summary={summary}", flush=True)

    # EMIT THE MEASUREMENT FIRST. Rescue is slower than the scan by two orders
    # of magnitude and can hang on a broken transport; printing afterwards is
    # how a completed 0.4s measurement got discarded by a 30s timeout on every
    # tick for a full day. A rescue failure must cost the rescue, not the
    # observation.
    emit([])
    if not a.rescue:
        return 1 if unpublished else 0

    published = rescue(rows, a.agent, a.dry_run, a.rescue_deadline)
    verified = sum(1 for r in published if r.get("published") == "verified")
    skipped = sum(1 for r in published if r.get("published") == "skipped-deadline")
    failed = len(published) - verified - skipped
    if not a.json:
        for r in published:
            print(f"  rescue {r['short']} -> {r['ref']} [{r['published']}]", flush=True)
    print(f"unpushed-parent-commits rescue attempted={len(published) - skipped} "
          f"verified={verified} failed={failed} skipped_deadline={skipped}",
          flush=True)
    # Partial coverage is NOT success. Anything unpublished keeps the nonzero
    # exit, so a deadline or a broken transport can never read as "all clear".
    return 1 if unpublished else 0


if __name__ == "__main__":
    sys.exit(main())
