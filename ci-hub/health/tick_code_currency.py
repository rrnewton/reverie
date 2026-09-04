#!/usr/bin/env python3
"""Page when the tick is executing code that is not what landed on main.

WHY THIS EXISTS. Production materializes an immutable tool checkout at the
freshly resolved `origin/main` commit, then executes the tick from that checkout.
Every mutable subject remains explicitly rooted at `DEV_HERMIT_PARENT`; code,
configuration, and executables come from the immutable tool root. This split is
necessary because a tool-only linked worktree does not carry the shared parent's
gitignored state or initialized submodules. Measured 2026-08-08, same tool and
same moment:

    slot_disk_residue from the shared parent : 0 reclaimable of 77 on disk (49 occupied)
    slot_disk_residue from a pinned worktree : 0 reclaimable of  0 on disk ( 0 occupied)

0-of-77 becomes 0-OF-0, exit 0, plausible summary, nothing scanned. The explicit
state-root contract avoids that false-success mode while keeping stale or dirty
shared-parent code out of the execution path.

This gate remains defense in depth for direct/manual invocations and for a
damaged tool cache: it proves that the files doing the checking match the
checkout's `origin/main`, rather than trusting provenance metadata alone.

WHAT ACTUALLY FAILED, TWICE, ON THE SAME GATE. The parent is chronically behind
`origin/main` (it cannot fast-forward while other agents hold dirty paths), and
a behind-checkout displays LANDED content as MODIFIED. So `git restore` -- the
most ordinary cleanup command there is -- silently rewinds live tick code to a
pre-fix version. Nobody has to be careless. On 2026-08-08 `unpushed_parent_commits`
was fixed from a 30s timeout to 0.24s, ran clean for several ticks, and was then
reverted by exactly that command; the whole tick went 4s -> 36s and the detector
for unpushed work went blind again.

THE PROPERTY THIS RESTORES IS NOT "the fix cannot be reverted" -- nothing can
give that, and a required human step is a step that eventually does not happen.
It is that **a reverted fix cannot sit silently.** Drift pages on the next tick,
naming the paths, instead of waiting for someone to notice a gate has gone quiet.

FAIL CLOSED, AND PER PATH. If currency cannot be established (no `origin/main`
ref, unreadable file, git failure) that is `unknown`, never `current` -- "I could
not check" must not read as "it matches". Reporting is per path so one drifted
tool names itself rather than casting doubt over all thirteen gates.

DETECT ONLY. Reads git and prints. It never restores, fetches, checks out, or
edits anything -- repairing drift means landing or syncing, both of which are
decisions with owners.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import tempfile
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

def _capture(command, **kwargs):
    """Deprecated local alias for lib/bounded_capture.capture.

    Kept only so this file's existing call sites read unchanged. The
    implementation moved to ci-hub/lib/bounded_capture.py because the same
    helper had been written independently in three files, and three copies of a
    subtle fix drift. Prefer `bounded_capture.capture` directly in new code.

    Capture output while waiting for THE CHILD, not for EOF on the pipe.

    ⚠️ `capture_output=True` hands the child a PIPE and reads to EOF, and EOF
    arrives when the LAST HOLDER closes it -- not when the child exits. A
    background grandchild that inherited the pipe blocks the caller after the
    child is already gone. git's `gc --auto` is the everyday trigger: on by
    default (gc.auto=6700) and detaching by default, so it inherits stderr. Every
    call routed through here shells out to git.

    Reproduced 2026-08-25: a child exiting immediately while leaving one
    background grandchild blocked capture_output indefinitely, while capturing to
    a file returned in 0.0s with identical stdout and exit code. A regular file
    has no EOF-waiting semantics, so wait() on the child is the only gate.

    NOT a timeout (bounds the wait and discards the reason) and NOT disabling
    auto-gc (hides the mechanism for the next background child).
    """
    return bounded_capture.capture(command, **kwargs)


ROOT = Path(__file__).resolve().parents[2]
TICK_CONFIG = ROOT / "ci-hub" / "health" / "tick-hub.yaml"
TARGET_REF = "origin/main"

# Tool paths referenced by a gate command in tick-hub.yaml. The config itself is
# always policed: a reverted tick-hub.yaml changes which command runs at all,
# which is exactly how the 2026-08-08 regression re-armed `--rescue`.
_TOOL_RE = re.compile(
    r"(?:\./)?((?:ci-hub|scripts|agent-utils)/[A-Za-z0-9_./-]+\.(?:py|rs|sh))"
)

CURRENT = 0
DRIFTED = 1
UNKNOWN = 2


@dataclass(frozen=True)
class Verdict:
    path: str
    state: str  # "current" | "stale" | "modified" | "unknown"
    detail: str = ""


# STALE and MODIFIED are both "not what landed", and conflating them would make
# this gate useless. Measured 2026-08-08: all four then-drifting tick tools were
# clean-but-behind, the exact silent-reversion signature; meanwhile agents edit
# gate tools constantly, and paging on that would train everyone to ignore this.
#
#   stale     working bytes != origin/main, with no tracked worktree difference
#             from HEAD -> a landed version is NOT the one executing. This also
#             covers a genuinely different untracked path absent from stale HEAD.
#             The incident. PAGE.
#   modified  differs from HEAD too -> somebody's tracked live work-in-progress.
#             Report, do not page: it is expected, and already visible as dirt.
#
# The discriminator is deliberately content-based, not status-based: after a
# `git restore` the file is CLEAN by `git status`, which is precisely why the
# reversion was invisible the first two times.


def _git(*args: str) -> subprocess.CompletedProcess[str]:
    return _capture(["git", "-C", str(ROOT), *args])


def policed_paths(config: Path = TICK_CONFIG) -> list[str]:
    """Every tool a gate invokes, plus the config that decides the invocation."""
    try:
        text = config.read_text()
    except OSError:
        return []
    found = set(_TOOL_RE.findall(text))
    try:
        found.add(str(config.resolve().relative_to(ROOT.resolve())))
    except ValueError:
        # A config outside ROOT (tests, or an explicit --config elsewhere): police
        # it by the path we were given rather than dropping it silently. Losing
        # the config from the set would be the worst omission here -- it is what
        # decides which command each gate runs.
        found.add(str(config))
    return sorted(found)


#: Directories whose contents EXECUTE or decide authority. Everything else in
#: the parent churns on its own schedule -- `ledger/` gains rows on every
#: publish, `experiments/` and `ai_docs/` accumulate by design -- and including
#: them would page every tick and train the fleet to ignore this gate, which is
#: the one failure mode a currency gate cannot survive.
_EXECUTABLE_PREFIXES = ("ci-hub/", "scripts/", ".orc/")
_EXECUTABLE_SUFFIXES = (".py", ".rs", ".sh", ".yaml", ".yml", ".ts", ".toml")
_GIT_EXECUTABLE_MODE = "100755"


def _changed_path_modes(ref: str) -> list[tuple[str, str, str]]:
    """Return ``(path, old_mode, new_mode)`` for every working-tree difference.

    Raw diff carries the fact the suffix-only scan threw away: Git's executable
    bit. ``--no-renames`` keeps each record single-path, which makes additions,
    removals, and replacements use the same predicate. Both modes matter: a
    newly landed executable has only the old/ref mode here, while an executable
    removed on main can remain live in the stale checkout's new mode.
    """
    out = _git("diff", "--raw", "--no-abbrev", "--no-renames", ref, "--")
    if out.returncode != 0:
        raise RuntimeError(
            f"cannot enumerate paths differing from {ref}: {out.stderr.strip()[:80]}"
        )
    changed: list[tuple[str, str, str]] = []
    for line in out.stdout.splitlines():
        try:
            metadata, path = line.split("\t", 1)
        except ValueError as error:
            raise RuntimeError(f"git diff returned an unparseable row: {line[:80]}") from error
        fields = metadata.split()
        if len(fields) < 5 or not fields[0].startswith(":"):
            raise RuntimeError(f"git diff returned an unparseable row: {line[:80]}")
        changed.append((path, fields[0][1:], fields[1]))
    return changed


def landed_but_unpulled(ref: str = TARGET_REF) -> list[str]:
    """Executable paths whose landed bytes this working tree is not running.

    WHY THIS WIDENS AN EXISTING GATE RATHER THAN ADDING A NEW ONE. `stale`
    already means "the bytes that execute are not the bytes that landed", it is
    already sampled every 900s, and it already pages `hard-warn-coordinator`
    without anyone querying. The only thing missing was the SUBJECT:
    `policed_paths` derives from `tick-hub.yaml`, so the gate watched the tick's
    own 17 tools and nothing else.

    Measured 2026-08-11: three authority fixes landed and sat INACTIVE in the
    shared parent the same day -- the receipt-prefix verifier
    (`ci-hub/validation/verify_receipt.sh`), the snapshot wrapper, and
    `validate-status` (`ci-hub/ci-hub.rs`). Each reported "published, ancestry
    1/1, tests pass" while the fleet ran the old code, and NONE of the three was
    in the policed set, so nothing could have surfaced it. At one sampled moment
    the count was 1 of 1 executable path landed-but-not-deployed and 0 policed.

    LANDED != DEPLOYED is the same shape as CLOSED != LANDED one step later, and
    the reason it went unnoticed three times is that the gap is TRANSIENT: it
    closes the moment someone pulls, so an on-demand query almost never catches
    it. That is precisely why it belongs in a periodic sampler and not in a
    report anyone has to think to run.
    """
    return sorted(
        path
        for path, old_mode, new_mode in _changed_path_modes(ref)
        if path.startswith(_EXECUTABLE_PREFIXES)
        and (
            path.endswith(_EXECUTABLE_SUFFIXES)
            or _GIT_EXECUTABLE_MODE in (old_mode, new_mode)
        )
    )


def subject_paths(config: Path = TICK_CONFIG, ref: str = TARGET_REF) -> list[str]:
    """Everything whose executing bytes this gate is responsible for.

    The union is deliberate: the tick's own tools must stay policed even when
    they match `ref` (that is the reverted-fix case this file was written for),
    and landed-but-unpulled code must be policed even though no gate invokes it.
    """
    return sorted(set(policed_paths(config)) | set(landed_but_unpulled(ref)))


def compare(paths: Sequence[str], ref: str = TARGET_REF) -> list[Verdict]:
    """Working-tree content versus `ref`, per path, fail-closed to unknown."""
    if not paths:
        return [Verdict(str(TICK_CONFIG), "unknown", "no gate tools parsed from tick-hub.yaml")]
    resolved = _git("rev-parse", "--verify", f"{ref}^{{commit}}")
    if resolved.returncode != 0:
        return [
            Verdict(p, "unknown", f"cannot resolve {ref}: {resolved.stderr.strip()[:60]}")
            for p in paths
        ]
    out: list[Verdict] = []
    for path in paths:
        # Compare the bytes that actually execute with the blob that landed.
        # `git diff <ref>` cannot do this for a path absent from stale HEAD's
        # index: Git ignores the untracked working file and reports the target
        # blob as deleted, even when the on-disk bytes are identical.
        target = _git("rev-parse", "--verify", f"{ref}:{path}")
        if target.returncode != 0 or not target.stdout.strip():
            out.append(Verdict(
                path, "unknown",
                (target.stderr or f"cannot resolve {path} at {ref}").strip()[:80],
            ))
            continue
        working = _git("hash-object", "--no-filters", "--", path)
        if working.returncode != 0 or not working.stdout.strip():
            out.append(Verdict(
                path, "unknown",
                (working.stderr or "cannot hash working file").strip()[:80],
            ))
            continue
        if working.stdout.strip() == target.stdout.strip():
            out.append(Verdict(path, "current"))
            continue
        # Working bytes differ from the target. Split the dangerous case from
        # ordinary tracked work-in-progress. For an untracked path absent from
        # stale HEAD, local returns 0; a genuine mismatch must remain STALE.
        local = _git("diff", "--quiet", "HEAD", "--", path)
        if local.returncode == 0:
            out.append(Verdict(
                path, "stale",
                f"working bytes differ from {ref} with no tracked local edit -- "
                f"the landed version is not the one executing",
            ))
        elif local.returncode == 1:
            out.append(Verdict(path, "modified", "uncommitted local edits (work in progress)"))
        else:
            out.append(
                Verdict(path, "unknown", (local.stderr or "git diff failed").strip()[:80])
            )
    return out


def render(verdicts: Sequence[Verdict], ref: str = TARGET_REF) -> tuple[int, str]:
    stale = [v for v in verdicts if v.state == "stale"]
    modified = [v for v in verdicts if v.state == "modified"]
    unknown = [v for v in verdicts if v.state == "unknown"]
    lines = []
    for v in stale:
        lines.append(f"  STALE     {v.path}  ({v.detail})")
    for v in unknown:
        lines.append(f"  UNKNOWN   {v.path}  ({v.detail})")
    for v in modified:
        lines.append(f"  modified  {v.path}  ({v.detail})")

    if stale:
        subjects = ", ".join(v.path for v in stale[:3])
        residue = f" (+{len(stale) - 3} more)" if len(stale) > 3 else ""
        summary = (
            f"{len(stale)} of {len(verdicts)} policed path(s) are STALE against {ref} -- "
            f"this tree is executing bytes that differ from what landed: "
            f"{subjects}{residue}. Reconcile the parent."
        )
        code = DRIFTED
    elif unknown:
        summary = (
            f"could not establish currency for {len(unknown)} of {len(verdicts)} policed "
            f"path(s) against {ref}"
        )
        code = UNKNOWN
    else:
        # Work-in-progress is reported but never paged: it is expected, and it is
        # already visible as dirt to anyone who looks.
        wip = f"; {len(modified)} carrying work-in-progress edits" if modified else ""
        summary = f"all {len(verdicts)} policed path(s) match {ref}{wip}"
        code = CURRENT

    lines.append(f"summary={summary}")
    return code, "\n".join(lines)


def main(argv: Sequence[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--ref", default=TARGET_REF, help="currency target (default origin/main)")
    ap.add_argument("--config", type=Path, default=TICK_CONFIG)
    ap.add_argument("--json", action="store_true", dest="as_json")
    ap.add_argument(
        "--gate", action="store_true",
        help="exit 1 on drift and 2 on unknown; without it the report still prints "
             "but always exits 0",
    )
    args = ap.parse_args(argv)

    try:
        paths = subject_paths(args.config, args.ref)
    except RuntimeError as error:
        if args.as_json:
            print(json.dumps({"state": "unknown", "summary": str(error)}))
        else:
            print("state=unknown")
            print(f"summary={error}")
        return UNKNOWN if args.gate else 0
    verdicts = compare(paths, args.ref)
    code, text = render(verdicts, args.ref)
    if args.as_json:
        print(json.dumps(
            {"ref": args.ref, "code": code,
             "verdicts": [{"path": v.path, "state": v.state, "detail": v.detail}
                          for v in verdicts]},
            indent=2, sort_keys=True))
    else:
        print(text)
    return code if args.gate else 0


if __name__ == "__main__":
    raise SystemExit(main())
