#!/usr/bin/env python3
"""Bind wrkslots identity onto historical validation run records, once.

WHY THIS EXISTS AND WHY IT IS ONE-SHOT. dev-hermit@88cd8c5 gave the validation
cleanup a bounded batch, and it correctly refuses any run handle that does not
carry an exact wrkslots identity -- `wrkslots_slot` and `wrkslots_generation`.
Records written before that change carry neither, so the consumer cannot act on
them and says so. This is the bridge for that pre-change population and nothing
else: it does not change what the recurring cleanup does with a missing identity,
and a record written from now on already carries the fields.

⚠️ ONE IMMUTABLE SNAPSHOT, AND EVERY DECISION COMES FROM IT. The registry is
written continuously by other agents; a migration planned against a moving
population has the wrong denominator, and two of three batch removals on this
host were retained on their first attempt for exactly that reason. The snapshot
is supplied as a file, its SHA-256 is verified before anything is read out of it,
and rows that appear afterwards are simply not in the plan.

⚠️ COMPARE AND BIND, NOT WRITE. Every group carries the SHA-256 of the run
record's exact bytes as planned. At apply time the record is re-read under its
own lock and the hash must still match, or that group is retained. So a record
that changed between plan and apply is never overwritten with a decision made
about a different record.

IDEMPOTENT BY CONSTRUCTION. A record that already carries exactly the identity
this plan would bind is reported `exact-present` and written again by nobody. A
crash mid-batch leaves some groups bound and the rest untouched; re-running the
same plan binds the rest and re-reports the bound ones as `exact-present`. There
is no journal to resume because there is no state to reconstruct -- the record's
own bytes are the state.

WHAT IT WILL NOT DO. It does not remove storage, it does not touch the cleanup
cursor, and it binds nothing to a run that cannot be proven finished: a record
whose state is not terminal is retained, because an identity is what makes a
record eligible for removal and granting one to an unproven run would make the
removal decision on the binding rather than on the evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from pathlib import Path

MAX_BATCH = 8
IDENTITY_KEYS = ("wrkslots_slot", "wrkslots_generation")
TERMINAL_STATES = frozenset({"completed"})

ELIGIBLE = "eligible"
BOUND = "bound"
EXACT_PRESENT = "exact-present"
RETAINED = "retained"


def digest_bytes(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def read_snapshot(path: Path, expected_sha256: str | None) -> dict:
    raw = path.read_bytes()
    actual = digest_bytes(raw)
    if expected_sha256 and actual != expected_sha256:
        raise SystemExit(
            f"migrate-run-identities: REFUSED: snapshot {path} hashes {actual}, "
            f"expected {expected_sha256}; the plan and the snapshot are not the same read"
        )
    document = json.loads(raw)
    if not isinstance(document, dict) or not isinstance(document.get("active"), list):
        raise SystemExit(f"migrate-run-identities: REFUSED: {path} is not a status snapshot")
    return {"document": document, "sha256": actual}


def snapshot_rows_by_checkout(document: dict, root: Path) -> dict[str, list[dict]]:
    by_path: dict[str, list[dict]] = {}
    for row in document["active"]:
        for checkout in row.get("checkouts") or []:
            path = checkout.get("path")
            if not path:
                continue
            resolved = os.path.normpath(os.path.join(str(root), path))
            by_path.setdefault(resolved, []).append(row)
    return by_path


def historical_records(root: Path) -> dict[str, list[tuple[Path, dict, str]]]:
    """Run handles under the runs directory, grouped by canonical checkout.

    Framework result sidecars are excluded by name, the same rule the cleanup
    discovery uses; they are not run handles and never were.
    """
    runs = root / "ignored" / "validate" / "runs"
    grouped: dict[str, list[tuple[Path, dict, str]]] = {}
    if not runs.is_dir():
        return grouped
    for path in sorted(runs.glob("*.json")):
        if path.name.endswith(".service-result.json"):
            continue
        try:
            raw = path.read_bytes()
            record = json.loads(raw)
        except (OSError, UnicodeError, json.JSONDecodeError):
            continue
        if not isinstance(record, dict):
            continue
        checkout = record.get("checkout")
        if not isinstance(checkout, str) or "worktrees/validate/" not in checkout:
            continue
        if not Path(checkout).is_dir():
            continue
        grouped.setdefault(os.path.normpath(checkout), []).append(
            (path, record, digest_bytes(raw))
        )
    return grouped


def plan_group(
    checkout: str, entries: list, rows: list[dict]
) -> dict:
    """One canonical physical checkout, judged against the pinned snapshot."""
    reasons: list[str] = []
    if len(entries) != 1:
        reasons.append(f"{len(entries)} run records name this checkout; expected exactly one")
    if len(rows) != 1:
        reasons.append(f"{len(rows)} snapshot rows map to this checkout; expected exactly one")

    row = rows[0] if len(rows) == 1 else None
    path, record, preimage = entries[0] if len(entries) == 1 else (None, {}, "")

    if row is not None:
        if row.get("slot_type") != "validate":
            reasons.append(f"slot_type is {row.get('slot_type')!r}, not validate")
        if row.get("slot") != os.path.basename(checkout):
            reasons.append("slot does not equal the checkout basename")
        generation = row.get("generation")
        if isinstance(generation, bool) or not isinstance(generation, int) or generation < 1:
            reasons.append("generation is not a positive non-boolean integer")
        if row.get("storage_inconsistencies"):
            reasons.append("snapshot row reports storage inconsistencies")

    if record.get("temporary_checkout") is not True:
        reasons.append("temporary_checkout is not exactly true")
    present = [key for key in IDENTITY_KEYS if key in record]
    if present and len(present) != len(IDENTITY_KEYS):
        reasons.append(f"partial identity already present: {', '.join(present)}")
    state = record.get("state")
    if state not in TERMINAL_STATES:
        reasons.append(f"state is {state!r}, so the run is not proven finished")

    group: dict = {
        "checkout": checkout,
        "record": str(path) if path else None,
        "record_sha256": preimage,
        "slot": row.get("slot") if row else None,
        "generation": row.get("generation") if row else None,
    }
    if reasons:
        group["outcome"] = RETAINED
        group["reasons"] = reasons
        return group
    if len(present) == len(IDENTITY_KEYS):
        if (
            record.get("wrkslots_slot") == group["slot"]
            and record.get("wrkslots_generation") == group["generation"]
        ):
            group["outcome"] = EXACT_PRESENT
        else:
            group["outcome"] = RETAINED
            group["reasons"] = ["identity already present and conflicts with the snapshot"]
        return group
    group["outcome"] = ELIGIBLE
    return group


def build_plan(root: Path, snapshot: dict) -> list[dict]:
    by_path = snapshot_rows_by_checkout(snapshot["document"], root)
    grouped = historical_records(root)
    return [
        plan_group(checkout, entries, by_path.get(checkout, []))
        for checkout, entries in sorted(grouped.items())
    ]


def bind(group: dict, run_registry) -> tuple[str, str | None]:
    """Re-read under the lock, require the planned bytes, then bind."""
    path = Path(group["record"])
    try:
        raw = path.read_bytes()
    except OSError as error:
        return RETAINED, f"record disappeared before binding: {error}"
    if digest_bytes(raw) != group["record_sha256"]:
        return RETAINED, "record changed after the plan was made; not overwriting it"
    current = json.loads(raw)
    if all(key in current for key in IDENTITY_KEYS):
        if (
            current.get("wrkslots_slot") == group["slot"]
            and current.get("wrkslots_generation") == group["generation"]
        ):
            return EXACT_PRESENT, None
        return RETAINED, "identity appeared and conflicts with the snapshot"
    try:
        run_registry.update_record(
            path,
            blocking=True,
            wrkslots_slot=group["slot"],
            wrkslots_generation=group["generation"],
        )
    except (OSError, RuntimeError, ValueError) as error:
        return RETAINED, f"could not bind: {error}"
    after = json.loads(path.read_bytes())
    if (
        after.get("wrkslots_slot") != group["slot"]
        or after.get("wrkslots_generation") != group["generation"]
    ):
        return RETAINED, "read-back after binding does not carry the planned identity"
    return BOUND, None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path("/home/newton/work/dev-hermit"))
    parser.add_argument("--snapshot", type=Path, required=True)
    parser.add_argument("--snapshot-sha256", default=None)
    parser.add_argument("--apply", action="store_true", help="bind; default is a dry run that writes nothing")
    parser.add_argument("--max-batch", type=int, default=MAX_BATCH)
    parser.add_argument(
        "--plan-out",
        type=Path,
        help="write the plan, so it can be reviewed and applied as a separate step",
    )
    parser.add_argument(
        "--plan-in",
        type=Path,
        help=(
            "apply an earlier plan rather than rebuilding one. This is where the "
            "record preimage hashes earn their place: a record that changed since "
            "the plan was written is retained rather than overwritten with a "
            "decision made about different bytes."
        ),
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    # The library comes from where THIS FILE lives, not from --root. --root names
    # the tree being migrated, which in a test is a fixture with no library in
    # it; resolving the writer through it would make the tool untestable and
    # would let a caller point it at someone else's copy.
    here = Path(__file__).resolve().parent
    for candidate in (here, Path("/home/newton/work/dev-hermit/ci-hub/validate")):
        if (candidate / "run_registry.py").is_file():
            sys.path.insert(0, str(candidate))
            break
    import run_registry  # noqa: E402

    snapshot = read_snapshot(args.snapshot, args.snapshot_sha256)
    if args.plan_in:
        stored = json.loads(args.plan_in.read_text())
        if stored.get("snapshot_sha256") != snapshot["sha256"]:
            raise SystemExit(
                "migrate-run-identities: REFUSED: the stored plan was made from a "
                f"different snapshot ({stored.get('snapshot_sha256')})"
            )
        plan = stored["plan"]
    else:
        plan = build_plan(args.root, snapshot)
    if args.plan_out:
        args.plan_out.write_text(
            json.dumps(
                {"snapshot_sha256": snapshot["sha256"], "plan": plan},
                indent=1,
                sort_keys=True,
            )
        )
    eligible = [g for g in plan if g["outcome"] == ELIGIBLE]
    batch = eligible[: max(0, args.max_batch)]

    applied: list[dict] = []
    if args.apply:
        for group in batch:
            outcome, reason = bind(group, run_registry)
            record = dict(group, outcome=outcome)
            if reason:
                record["reasons"] = [reason]
            applied.append(record)

    report = {
        "snapshot": str(args.snapshot),
        "snapshot_sha256": snapshot["sha256"],
        "mode": "apply" if args.apply else "dry-run",
        "max_batch": args.max_batch,
        "groups": len(plan),
        "eligible": len(eligible),
        "batch": len(batch),
        "exact_present": sum(1 for g in plan if g["outcome"] == EXACT_PRESENT),
        "retained": sum(1 for g in plan if g["outcome"] == RETAINED),
        "bound": sum(1 for g in applied if g["outcome"] == BOUND),
        "remaining_after_batch": max(0, len(eligible) - len(batch)),
        "plan": plan,
        "applied": applied,
    }
    if args.json:
        print(json.dumps(report, indent=1, sort_keys=True))
    else:
        for key in (
            "mode", "snapshot_sha256", "groups", "eligible", "batch",
            "bound", "exact_present", "retained", "remaining_after_batch",
        ):
            print(f"{key}={report[key]}")
        for group in plan:
            if group["outcome"] == RETAINED:
                print(f"RETAINED {group['checkout']}: {'; '.join(group.get('reasons', []))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
