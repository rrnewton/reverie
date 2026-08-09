#!/usr/bin/env python3
"""SOFT green vs HARD green — the class of a ledger row, DERIVED not declared.

THE DISTINCTION (owner, 2026-08-04):
  HARD green  = validation actually executed at THIS exact head.
  SOFT green  = validation executed at an ANCESTOR head and is being
                SPECULATIVELY TRUSTED here (the head was rebased, nothing on the
                branch changed). "We DO speculatively trust rebased PRs that were
                green just before."

WHY THIS EXISTS, STATED ACCURATELY
----------------------------------
It is tempting to say "a soft green currently reads identical to a hard one, so
receipts are being laundered today." Measured at ci-hub HEAD, that is NOT what is
happening, and the real situation is worth stating precisely because it changes
what the fix has to be:

  * The validate ledger has ONE SHA field, `commit` (ci-hub/lib/records.rs:117),
    which doubles as "the head this row describes" AND "the SHA validation ran
    on". There is no `validated_head_sha`, no `inherited_from`, no soft/hard flag.
  * `qualifying-receipt.json`'s `require{}` has no class clause.
  * BUT there is also NO PRODUCER that writes an inherited green into the ledger.
    `validate.sh` and `scripts/validate.rs` both stamp the head they actually ran
    on. `ci-hub/landing/rebase_wrapper.py` does model soft-green as a confidence
    LEVEL — and it keeps its records in a SEPARATE store
    (`ignored/rebase-records.jsonl`), and its landability conjunction is
    `soft_green AND base_clears_floor AND receipt_present`, where
    `receipt_present` means `ci-hub validate-status --sha Z` reads VALIDATED at
    the PUSHED head. So its soft-green never SUBSTITUTES for a hard green; it
    gates on one.

So today's safety comes from an ABSENCE (no soft producer exists), not from a
GUARD. That is the fragile kind of safety: it holds until the first soft producer
lands, and three in-flight workstreams are each designed to be exactly that
producer — the test-selection green-inheritance anchor work, the clean-rebase
soft-inherit work, and any relaxation of the rebase wrapper's `receipt_present`
clause. The moment one lands, an inherited green enters the ledger BYTE-IDENTICAL
to a hard one, and no consumer can tell.

The schema-transition rule in this repo (version-aware acceptance) makes the
timing load-bearing in one direction: a consumer that starts REQUIRING a new
field breaks every producer that predates it — that is the incident that once
rejected 254 of 255 rows fleet-wide. Therefore the class field must be introduced
with a DEFINED DEFAULT FOR EXISTING ROWS, and it must be introduced BEFORE the
first soft producer, not after. Adding it now costs one derivable default; adding
it later costs a fleet-wide flag day.

THE DESIGN — THE CLASS IS DERIVED FROM PROVENANCE, NEVER FROM A LABEL
---------------------------------------------------------------------
A `soft: bool` (or a `green_class: "hard"` string) that a writer sets is a PROXY:
a carry-forward writer can stamp `soft=false` just as easily as a real one, and
the row is byte-identical to a hard green again — the same defect one level up.
So the authority here is PROVENANCE, and the class is a function of it:

  validated_head_sha : the SHA validation ACTUALLY executed on.  <-- load-bearing
  inherited_from     : present IFF validated_head_sha != commit; describes the
                       delta between that ancestor and this head.
  green_class        : a CACHE of the derived class. A verifier recomputes it and
                       REFUSES the row when the label and the provenance
                       disagree; it is never read as the truth.

`inherited_from` deliberately does NOT repeat the ancestor SHA — that is
`validated_head_sha`. Two copies of one fact is a drift source.

DEFAULT FOR ROWS WRITTEN BEFORE THIS FIELD EXISTS
-------------------------------------------------
`validated_head_sha` absent  =>  derive it as `commit`  =>  HARD.

This is a DERIVATION, not a guess: every producer that exists today stamps the
head it ran on, so "absent" means exactly "ran here". All existing rows classify
HARD with zero fleet breakage, which is what the version-aware acceptance
contract requires of any new field.

It is also the safe direction to default only because the OTHER half of the rule
is fail-closed: a row that DOES claim inheritance without carrying the provenance
to justify it is REFUSED, not accepted. A future soft producer that forgets to
stamp `inherited_from` also has to forget `validated_head_sha` to pass as hard —
and if it stamps neither, it has written a row asserting it ran here, which is a
producer defect a lint can catch, not an ambiguity in the reader.

THE DECAY RULE (owner item 3: name the boundary, do not treat all soft alike)
-----------------------------------------------------------------------------
Soft green is a bet, and the bet is not uniform:

  rebase-only          the branch's own patches are unchanged and the rebase
                       pulled in NO new upstream commits. Strongest: the tree
                       differs only in parent pointers.
  rebase-plus-upstream the rebase pulled in N new upstream commits. Weaker with
                       N — the ancestor's green says nothing about interactions
                       with code that did not exist when it ran.
  new-branch-commits   the branch itself gained commits. NOT soft green at all.
                       (Owner: "Add a commit -> it becomes NEITHER.")

Within `rebase-plus-upstream` the boundary is DERIVED, not a picked N: if any
pulled-in commit touches a force_full-class path (`Cargo.toml`/`Cargo.lock`,
`ci/**`, `validate.sh`, `rust-toolchain.toml`, `.cargo/**`, gated workflows), the
blast radius is the whole suite and the ancestor's green covers none of it. That
is the same monotonic force_full boundary the test-selection decay measurement
found — where it fires, selection saves exactly zero and inheritance is worth
exactly as little. Such a row is classified `soft-force-full-touched`, which the
landing predicate should not accept even if it accepts other soft classes.

WHAT THE LANDING PREDICATE MUST DO (owner item 2)
-------------------------------------------------
Say which classes it accepts, out loud, in `qualifying-receipt.json`:

    "accepts_green_class": ["hard"]

Defaulting to `["hard"]` keeps today's behavior EXACTLY as it is, and converts it
from an accident (nothing soft exists) into a STATED POLICY. Widening it later is
then one reviewed edit in the one shared predicate file, and every consumer that
reads that file inherits the decision — instead of each consumer quietly deciding
for itself what a soft row means.

USAGE
  green_class.py --ledger PATH [--sha SHA] [--json]     # classify rows
  from green_class import classify_row, classify_delta  # library

EXIT CODES
  0 rows classified   2 refusal(s) found   3 error
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(os.path.abspath(__file__)).parent
DEFAULT_PREDICATE = HERE / "qualifying-receipt.json"

EXIT_OK = 0
EXIT_REFUSED = 2
EXIT_ERROR = 3

# --- the derived classes ---------------------------------------------------
HARD = "hard"
SOFT_REBASE_ONLY = "soft-rebase-only"
SOFT_UPSTREAM_DELTA = "soft-upstream-delta"
SOFT_FORCE_FULL = "soft-force-full-touched"
NOT_GREEN = "not-green"
REFUSED = "refused"

SOFT_CLASSES = (SOFT_REBASE_ONLY, SOFT_UPSTREAM_DELTA, SOFT_FORCE_FULL)

# --- the delta kinds a producer may record ---------------------------------
DELTA_REBASE_ONLY = "rebase-only"
DELTA_REBASE_PLUS_UPSTREAM = "rebase-plus-upstream"
DELTA_NEW_BRANCH_COMMITS = "new-branch-commits"
VALID_DELTA_KINDS = {DELTA_REBASE_ONLY, DELTA_REBASE_PLUS_UPSTREAM, DELTA_NEW_BRANCH_COMMITS}

# force_full-class prefixes. Mirrored from hermit ci/test-footprints-policy.json
# ONLY to describe the delta at RECORD time, in the producer. The consumer never
# re-derives it: it reads the recorded `force_full_paths` list, because by then
# the checkout may be gone. Recording the list (not a bare bool) is what lets a
# reviewer see WHICH path forced the downgrade.
FORCE_FULL_PREFIXES = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/",
    "ci/",
    "validate.sh",
    "scripts/validate.rs",
    "scripts/lib/",
    ".github/workflows/",
)


class Refusal(Exception):
    """The row's class cannot be derived, and guessing would be the defect."""


# ---------------------------------------------------------------------------
# Layer 1 — pure, row-only classification. This is what CONSUMERS call.
# No git, no network, no checkout: by the time a consumer reads a receipt the
# checkout that produced it may not exist, so the row must carry its own class.
# ---------------------------------------------------------------------------


def derive_class(row: dict) -> tuple[str, str]:
    """Derive (class, reason) from one ledger row's provenance fields.

    Raises nothing: a row that cannot be classified returns REFUSED with the
    reason, so a caller can count and report refusals rather than crash on one
    malformed row in a 600-row ledger.
    """
    commit = row.get("commit")
    if not isinstance(commit, str) or not commit or commit == "unknown":
        return REFUSED, "no commit on the row: nothing to bind a class to"

    # Absence is the only version-aware default. An explicit JSON null is a
    # producer statement with no usable value, not an older-schema omission;
    # refusing it keeps Python aligned with Rust's present-Value checks.
    for field in ("validated_head_sha", "inherited_from", "green_class"):
        if field in row and row[field] is None:
            return REFUSED, f"{field} is explicitly null; omit legacy fields or carry a value"

    validated = row.get("validated_head_sha")
    inherited = row.get("inherited_from")
    label = row.get("green_class")

    # Version-aware default: a producer that predates the field stamped the head
    # it ran on, so absent means "ran here".
    if validated is None:
        validated = commit
        default_applied = True
    else:
        default_applied = False
        if not isinstance(validated, str):
            return REFUSED, "validated_head_sha must be a string"

    if validated == commit:
        if inherited is not None:
            derived, reason = (
                REFUSED,
                "contradiction: validated_head_sha == commit (claims exact-head "
                "validation) yet inherited_from is present (claims inheritance)",
            )
        else:
            derived, reason = (
                HARD,
                "validated at this exact head"
                + (" (validated_head_sha absent; defaulted to commit)" if default_applied else ""),
            )
    else:
        derived, reason = _classify_inherited(validated, inherited)

    # The label is a CACHE. Recompute and refuse on disagreement — otherwise the
    # label is exactly the unbacked proxy this module exists to prevent.
    if label is not None and derived != REFUSED and label != derived:
        return (
            REFUSED,
            f"green_class label {label!r} disagrees with the class derived from "
            f"provenance ({derived!r}); the label is a cache, never the authority",
        )
    return derived, reason


def _classify_inherited(validated: str, inherited) -> tuple[str, str]:
    """validated_head_sha != commit: the row claims a soft green. Justify it."""
    if inherited is None:
        return (
            REFUSED,
            f"soft green claimed (validated at {validated[:12]}, recorded at a "
            "different head) with NO inherited_from provenance: a label with no "
            "backing is exactly the fake-green shape",
        )
    if not isinstance(inherited, dict):
        return REFUSED, "inherited_from is not an object"

    kind = inherited.get("delta_kind")
    if kind not in VALID_DELTA_KINDS:
        return (
            REFUSED,
            f"inherited_from.delta_kind {kind!r} is not one of "
            f"{sorted(VALID_DELTA_KINDS)}; an unrecognised delta is not a soft green",
        )

    branch_commits = inherited.get("branch_commits")
    if not _is_nonnegative_int(branch_commits):
        return REFUSED, "inherited_from.branch_commits must be a non-negative int"

    if "force_full_paths" not in inherited:
        force_full = []
    else:
        force_full = inherited["force_full_paths"]
    if not isinstance(force_full, list) or not all(
        isinstance(path, str) for path in force_full
    ):
        return REFUSED, "inherited_from.force_full_paths must be a list of strings"

    upstream = inherited.get("upstream_commits", 0)
    if not _is_nonnegative_int(upstream):
        return REFUSED, "inherited_from.upstream_commits must be a non-negative int"

    # OWNER RULE: add a commit and it is NEITHER hard nor soft. Validate every
    # carried provenance field first so a future policy cannot admit a malformed
    # `not-green` row merely by naming that derived class.
    if kind == DELTA_NEW_BRANCH_COMMITS or branch_commits > 0:
        return (
            NOT_GREEN,
            f"the branch itself gained {branch_commits} commit(s): the ancestor's "
            "green does not speak for code it never ran",
        )

    if kind == DELTA_REBASE_ONLY:
        if upstream != 0:
            return (
                REFUSED,
                f"delta_kind=rebase-only contradicts upstream_commits={upstream}",
            )
        return (
            SOFT_REBASE_ONLY,
            f"rebase-only onto {validated[:12]}: the branch's own patches are "
            "unchanged and no new upstream commits came in",
        )

    # rebase-plus-upstream
    if upstream <= 0:
        return (
            REFUSED,
            "delta_kind=rebase-plus-upstream requires a positive upstream_commits",
        )
    if force_full:
        return (
            SOFT_FORCE_FULL,
            f"{upstream} new upstream commit(s) include force_full-class "
            f"path(s) ({', '.join(force_full[:3])}): the blast radius is the whole "
            "suite, so the ancestor's green covers none of it",
        )
    return (
        SOFT_UPSTREAM_DELTA,
        f"{upstream} new upstream commit(s) pulled in; the ancestor's green says "
        "nothing about interactions with code that did not exist when it ran",
    )


def _is_nonnegative_int(value) -> bool:
    """JSON integer predicate shared semantically with serde_json::Value::as_u64.

    Python's ``bool`` subclasses ``int``; accepting it here while the Rust
    receipt verifier refuses it makes the same provenance hard/soft depending
    on which authority reads it.  Keep the contract explicit and fail closed.
    """
    return (
        isinstance(value, int)
        and not isinstance(value, bool)
        and 0 <= value <= (1 << 64) - 1
    )


def accepted_classes(predicate: dict) -> list[str]:
    """Which classes the landing predicate accepts.

    Defaults to `["hard"]` when the key is absent — today's behavior exactly, now
    stated rather than implied by the absence of a soft producer.
    """
    value = predicate.get("accepts_green_class")
    if value is None:
        return [HARD]
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
        raise Refusal("accepts_green_class must be a list of strings")
    return value


def classify_row(row: dict, predicate: dict) -> dict:
    """Full per-row verdict: the derived class plus whether landing accepts it."""
    klass, reason = derive_class(row)
    accepts = accepted_classes(predicate)
    return {
        "commit": row.get("commit"),
        "validated_head_sha": row.get("validated_head_sha") or row.get("commit"),
        "green_class": klass,
        "reason": reason,
        "accepted_for_landing": klass in accepts,
        "accepts_green_class": accepts,
    }


# ---------------------------------------------------------------------------
# Layer 2 — the PRODUCER side. Needs a checkout; run at record time, never at
# read time. Its whole output is the `inherited_from` block Layer 1 consumes.
# ---------------------------------------------------------------------------


def _git(checkout: Path, args: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(checkout), *args],
        capture_output=True, text=True, timeout=60, check=False,
    )


def _patch_ids(checkout: Path, base: str, head: str) -> list[str] | None:
    """Stable identity of the commits `head` adds on top of `base`.

    `git patch-id` is what makes "the branch's own patches are unchanged" a
    CHECKABLE claim rather than an assertion: a rebase preserves patch ids while
    changing every commit SHA, so comparing SHAs would report a total change and
    comparing trees would miss an added commit.
    """
    log = _git(checkout, ["log", "--no-merges", "--format=%H", f"{base}..{head}"])
    if log.returncode != 0:
        return None
    ids = []
    for sha in log.stdout.split():
        show = _git(checkout, ["show", sha])
        if show.returncode != 0:
            return None
        pid = subprocess.run(
            ["git", "-C", str(checkout), "patch-id", "--stable"],
            input=show.stdout, capture_output=True, text=True, timeout=60, check=False,
        )
        if pid.returncode != 0 or not pid.stdout.strip():
            return None
        ids.append(pid.stdout.split()[0])
    return ids


def force_full_paths_in(paths) -> list[str]:
    """Which of `paths` are force_full-class (whole-suite blast radius)."""
    hits = []
    for path in paths:
        for prefix in FORCE_FULL_PREFIXES:
            if path == prefix or path.startswith(prefix):
                hits.append(path)
                break
    return sorted(hits)


def classify_delta(checkout: Path, ancestor: str, head: str, old_base: str,
                   new_base: str) -> dict:
    """Build the `inherited_from` block for a green carried from `ancestor` to
    `head`, where the branch moved from `old_base` to `new_base`.

    Raises `Refusal` when the delta cannot be established. Refusing is the point:
    an unclassifiable delta must not silently become the strongest class.
    """
    for ref in (ancestor, head, old_base, new_base):
        if _git(checkout, ["cat-file", "-e", f"{ref}^{{commit}}"]).returncode != 0:
            raise Refusal(f"commit not present in the checkout: {ref}")

    before = _patch_ids(checkout, old_base, ancestor)
    after = _patch_ids(checkout, new_base, head)
    if before is None or after is None:
        raise Refusal("could not compute patch ids for the branch commits")

    # Extra patches on the head that the ancestor did not have = new branch work.
    added = [p for p in after if p not in before]
    branch_commits = len(added)

    upstream = _git(checkout, ["rev-list", "--count", f"{old_base}..{new_base}"])
    if upstream.returncode != 0 or not upstream.stdout.strip().isdigit():
        raise Refusal("could not count the new upstream commits")
    upstream_commits = int(upstream.stdout.strip())

    changed = _git(checkout, ["diff", "--name-only", f"{old_base}..{new_base}"])
    if changed.returncode != 0:
        raise Refusal("could not diff the old base against the new base")
    upstream_paths = [p for p in changed.stdout.splitlines() if p.strip()]

    if branch_commits > 0:
        kind = DELTA_NEW_BRANCH_COMMITS
    elif upstream_commits > 0:
        kind = DELTA_REBASE_PLUS_UPSTREAM
    else:
        kind = DELTA_REBASE_ONLY

    return {
        "delta_kind": kind,
        "upstream_commits": upstream_commits,
        "branch_commits": branch_commits,
        "patch_identical": branch_commits == 0 and len(after) == len(before),
        "force_full_paths": force_full_paths_in(upstream_paths),
        "recorded_by": "green_class.classify_delta",
    }


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Classify ledger rows as hard/soft green.")
    parser.add_argument("--ledger", required=True)
    parser.add_argument("--sha", default=None, help="only rows for this commit")
    parser.add_argument("--predicate", default=str(DEFAULT_PREDICATE))
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    try:
        with open(args.predicate) as handle:
            predicate = json.load(handle)
    except OSError as exc:
        print(f"green_class: cannot read predicate: {exc}", file=sys.stderr)
        return EXIT_ERROR

    verdicts, malformed = [], 0
    try:
        with open(args.ledger) as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    malformed += 1
                    continue
                if args.sha and row.get("commit") != args.sha:
                    continue
                verdicts.append(classify_row(row, predicate))
    except OSError as exc:
        print(f"green_class: cannot read ledger: {exc}", file=sys.stderr)
        return EXIT_ERROR

    counts: dict[str, int] = {}
    for v in verdicts:
        counts[v["green_class"]] = counts.get(v["green_class"], 0) + 1
    report = {
        "rows": len(verdicts),
        "malformed_lines": malformed,
        "counts": counts,
        "accepts_green_class": accepted_classes(predicate),
        "verdicts": verdicts if args.sha else [],
    }
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(f"rows: {len(verdicts)} (malformed lines: {malformed})")
        print(f"accepts for landing: {', '.join(report['accepts_green_class'])}")
        for klass in sorted(counts):
            print(f"  {klass:<26} {counts[klass]}")
        for v in report["verdicts"]:
            mark = "ACCEPT" if v["accepted_for_landing"] else "refuse"
            print(f"  [{mark}] {v['commit'][:12]} {v['green_class']}: {v['reason']}")
    return EXIT_REFUSED if counts.get(REFUSED) else EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
