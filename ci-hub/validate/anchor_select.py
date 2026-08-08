#!/usr/bin/env python3
"""ANCHOR-SELECT -- the one verifier for the GREEN-INHERITANCE ANCHOR authority.

WHAT THIS ANSWERS. An *incremental* (selective) validate run does not test the
whole suite; it tests only what the change can affect and INHERITS the rest of
its green from an earlier run. That earlier run is the ANCHOR. This tool decides
which commit may serve as the anchor for a given target, what diff drives the
selection, and whether the anchor still buys anything.

THE THREE RULES IT ENFORCES (owner directive, task
`green-inheritance-test-selection-anchored-on-full-main-validates`, 2026-08-04):

  1. ONE HOP ONLY. An incremental run anchors on a FULL green, NEVER on another
     incremental. Chaining COMPOUNDS the risk that the footprint map is wrong:
     each hop multiplies the chance that a test which should have run was
     skipped, and after N hops nothing in the chain was ever fully validated --
     the green becomes a claim about a claim. One hop bounds the error to a
     SINGLE selection decision against a known-full baseline.

     Enforced structurally: the anchor predicate requires
     `selection_mode == "full"`, and validate.sh stamps `selection_mode=selective`
     on every selective run (validate.sh `VALIDATION_SELECTION_MODE`). A
     selective receipt therefore can never be an anchor -- there is no second
     hop to take. `hop=1` is emitted in the record so the property is OBSERVED,
     not assumed. See `test_anchor_select.py::test_selective_receipt_refused`.

  2. THE ANCHOR PREDICATE IS THE SHARED QUALIFYING-RECEIPT PREDICATE, NOT
     `result == "pass"`. It is loaded from `qualifying-receipt.json` and NOT
     restated here. Keying on `result` alone anchors on the compat-only receipt
     population: measured on the live ledger (585 rows, 2026-08-05), 346 rows
     satisfy `result == "pass"`, of which only 107 qualify -- 239 (69%) would be
     accepted as anchors by a bare-`result` selector, including 164
     `portable-strict-compat-only` rows and 44 rows that are not commit-anchored.

  3. THE ANCHOR MUST BE AN ANCESTOR OF THE TARGET. `select-tests.rs` computes its
     LOCAL delta as `git diff --name-only <baseline>...HEAD` -- THREE dots. For a
     NON-ancestor baseline that silently relocates the effective anchor to
     `merge-base(baseline, HEAD)`, a commit that carries NO receipt at all: the
     run would inherit green from a commit nothing ever validated. Measured on
     the live ledger: of 105 distinct qualifying commits only 11 are ancestors of
     the current hermit HEAD; 86 are not. This tool requires ancestry and then
     uses TWO-dot `<anchor>..<target>`, so the property is load-bearing and
     visible instead of being papered over by merge-base.

WHAT THE DIFF DRIVES. Selection base is `TIP vs ANCHOR`, **not** `tip vs tip-1`.
Every change since the anchor is in scope, so the selected set grows -- and the
saving DECAYS -- as the tip drifts from the anchor.

WHEN TO REFRESH (DERIVED, never a picked commit count):

  * HARD, exact, available today: `selector decision == full` => the selected set
    IS the full set => the saving is exactly ZERO => RE-ANCHOR NOW. Because
    `force_full` is MONOTONIC over the union of the window, once ANY commit since
    the anchor touches a force_full-class path (`Cargo.*`, `ci/**`, `validate.sh`,
    `rust-toolchain.toml`, `.cargo/**`, gated workflows) the cumulative diff
    forces full at EVERY greater distance. The decay is therefore a CLIFF, not a
    smooth curve; this tool reports the FIRST force_full-class path in the window
    as `reanchor_cause` so the trigger names its own reason.

  * SOFT, correct, BLOCKED: `selected_wall / full_wall >= theta`. This is the
    signal that matches the cost model, and it is NOT available: no per-node wall
    durations are recorded anywhere (ci/test-selection.md "power-to-weight has no
    duration data yet"; the ledger records whole-run `real_seconds` only). The
    node/cell FRACTIONS reported below are an OPTIMISTIC proxy for it -- they
    weight every node equally while full-run wall is dominated by a few heavy e2e
    cells -- so they are REPORTED and never gated on. Unblocking this needs
    per-node duration emission in the receipt.

USAGE
  anchor_select.py --target <sha-or-ref> [--checkout <path>] [--ledger <path>]
                   [--selector <path/to/select-tests.rs>] [--include-dirty]
                   [--max-scan N] [--no-floor] [--json]

EXIT CODES
  0  INHERIT-SELECTIVE / INHERIT-CLEAN -- an anchor exists and selection saves.
  3  RE-ANCHOR-NOW -- an anchor exists but the selected set is the full set.
  4  NO-ANCHOR -- no qualifying full green ancestor; the target must run FULL.
  2  REFUSED -- bad input (unresolvable target, unreadable ledger/predicate).
  5  ERROR -- the selector or git failed; the caller must fall back to FULL.

Every non-zero exit means "run the full lane". There is no path on which a
failure of this tool produces a smaller test set.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

# The canonical coverage predicate is SHARED, not re-implemented here: a private
# copy is exactly how this file came to disagree with it. Mirrors the sys.path
# pattern used by aggregate.py / finalize_receipt.py for the hyphenated dir.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

HERE = Path(os.path.abspath(__file__)).parent
CI_HUB = HERE.parent
PARENT = CI_HUB.parent
sys.path.insert(0, str(CI_HUB))
import qualifying_receipt  # noqa: E402

DEFAULT_CHECKOUT = PARENT / "hermit"
DEFAULT_LEDGER = PARENT / "ignored" / "validate-run-ledger.jsonl"
DEFAULT_PREDICATE = HERE / "qualifying-receipt.json"
DEFAULT_SELECTOR_REL = Path("ci") / "select-tests.rs"
GATE_FLOORS = HERE / "gate_floors.py"

LOCAL_TIMEOUT = 60.0
SELECTOR_TIMEOUT = 300.0

EXIT_OK = 0
EXIT_REFUSED = 2
EXIT_REANCHOR = 3
EXIT_NO_ANCHOR = 4
EXIT_ERROR = 5

VERDICT_SELECTIVE = "INHERIT-SELECTIVE"
VERDICT_CLEAN = "INHERIT-CLEAN"
VERDICT_REANCHOR = "RE-ANCHOR-NOW"
VERDICT_NO_ANCHOR = "NO-ANCHOR"
VERDICT_ERROR = "ERROR"

# NOTE ON WHY THERE IS NO LOCAL COPY OF THE force_full LIST HERE.
# An earlier draft named the re-anchor cause by matching the changed paths against
# a mirrored copy of hermit `ci/test-footprints-policy.json` force_full globs. On
# the first live run that copy named `.github/workflows/ci-portable-autoretry.yml`
# as the cause -- a path the selector does NOT force (only three workflow files
# are in force_full; the rest fall through `.github/**` to ci_irrelevant). The
# mirrored list was a PROXY for the selector's decision and it was wrong on its
# first use. The cause is now read from the selector's own emitted `reasons`, so
# the reported cause and the decision it explains come from the same evaluation.


# ---------------------------------------------------------------------------
# The shared qualifying-receipt predicate (NOT restated -- loaded)
# ---------------------------------------------------------------------------


def load_predicate(path: Path) -> dict:
    with open(path) as handle:
        return json.load(handle)


def _coverage_satisfied(row: dict) -> bool:
    """Delegate the anchor parity probe to the canonical coverage authority.

    This compatibility seam is exercised directly by the recurrence test; it
    must never grow a local restatement of the coverage predicate.
    """
    return qualifying_receipt.coverage_satisfied(row.get("coverage"))


def row_qualifies(row: dict, predicate: dict) -> tuple[bool, str]:
    """Delegate anchor eligibility to the canonical semantic authority.

    The shared authority also returns the first failing clause, so anchor
    diagnostics cannot drift into a second certifier.
    """
    sha = row.get("commit")
    return qualifying_receipt.row_qualification(
        row, sha if isinstance(sha, str) else "", predicate
    )


def load_ledger(path: Path) -> tuple[list[dict], int]:
    rows: list[dict] = []
    malformed = 0
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                malformed += 1
    return rows, malformed


# ---------------------------------------------------------------------------
# git
# ---------------------------------------------------------------------------


def _git(checkout: Path, args: list[str], timeout: float = LOCAL_TIMEOUT) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(checkout), *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def resolve(checkout: Path, ref: str) -> str | None:
    res = _git(checkout, ["rev-parse", "--verify", f"{ref}^{{commit}}"])
    return res.stdout.strip() if res.returncode == 0 else None


def commit_present(checkout: Path, sha: str) -> bool:
    return _git(checkout, ["cat-file", "-e", f"{sha}^{{commit}}"]).returncode == 0


def is_ancestor(checkout: Path, ancestor: str, head: str) -> bool:
    return _git(checkout, ["merge-base", "--is-ancestor", ancestor, head]).returncode == 0


def first_parent_distance(checkout: Path, ancestor: str, head: str) -> int | None:
    res = _git(checkout, ["rev-list", "--count", "--first-parent", f"{ancestor}..{head}"])
    if res.returncode != 0:
        return None
    text = res.stdout.strip()
    return int(text) if text.isdigit() else None


def changed_files(checkout: Path, anchor: str, target: str, include_dirty: bool) -> list[str] | None:
    """TWO-dot diff. Requires (and is only called after) a verified ancestor.

    Two-dot is deliberate: with a verified ancestor it equals the three-dot form,
    but it does not SILENTLY succeed against a non-ancestor by relocating to the
    merge-base. If ancestry is ever lost, this returns the honest larger set
    rather than a quietly narrowed one.
    """
    res = _git(checkout, ["diff", "--name-only", f"{anchor}..{target}"])
    if res.returncode != 0:
        return None
    files = [line.strip() for line in res.stdout.splitlines() if line.strip()]
    if include_dirty:
        dirty = _git(checkout, ["diff", "--name-only", "HEAD"])
        untracked = _git(checkout, ["ls-files", "--others", "--exclude-standard"])
        if dirty.returncode != 0 or untracked.returncode != 0:
            return None
        files.extend(line.strip() for line in dirty.stdout.splitlines() if line.strip())
        files.extend(line.strip() for line in untracked.stdout.splitlines() if line.strip())
    return sorted(set(files))


def clears_floors(checkout: Path, sha: str) -> bool:
    """Delegate to the floor registry -- do not reimplement floor logic here."""
    if not GATE_FLOORS.exists():
        return True
    res = subprocess.run(
        [sys.executable, str(GATE_FLOORS), "--head", sha, "--repo-checkout", str(checkout),
         "--no-fetch", "--json"],
        capture_output=True,
        text=True,
        timeout=LOCAL_TIMEOUT,
        check=False,
    )
    return res.returncode == 0


# ---------------------------------------------------------------------------
# selector
# ---------------------------------------------------------------------------


def run_selector(selector: Path, checkout: Path, files: list[str]) -> dict | None:
    if not files:
        # An empty diff is the degenerate zero-change case: nothing to test.
        return {"decision": "skip", "node_count": 0, "cell_count": 0,
                "nodes": [], "reasons": ["empty diff against anchor"]}
    res = subprocess.run(
        [str(selector), "--files", "-", "--format", "json"],
        input="\n".join(files),
        capture_output=True,
        text=True,
        cwd=str(checkout),
        timeout=SELECTOR_TIMEOUT,
        check=False,
    )
    if res.returncode != 0:
        return None
    try:
        return json.loads(res.stdout)
    except json.JSONDecodeError:
        return None


def universe(selector: Path, checkout: Path) -> dict:
    """Full-suite sizes, obtained from the selector itself so the denominator is
    the selector's own universe rather than a second, driftable count."""
    out = run_selector(selector, checkout, ["Cargo.lock"])  # a force_full path => full
    if not out or out.get("decision") != "full":
        return {}
    return {"nodes": out.get("node_count"), "cells": out.get("cell_count"),
            "shards": len(out.get("shards") or [])}


def name_reanchor_cause(reasons: list[str]) -> str | None:
    """Name WHY the window forces full, using the selector's OWN reasons.

    `select-tests.rs` emits one reason per force_full hit (`"<path> -> force_full"`)
    and one aggregate reason for unmapped paths. Reading them here means the cause
    is bound to the evaluation that produced the decision, not to a second copy of
    the policy that can disagree with it.
    """
    for reason in reasons or []:
        if "force_full" in reason:
            return reason
    for reason in reasons or []:
        if "unmapped" in reason:
            return reason
    return None


# ---------------------------------------------------------------------------
# the algorithm
# ---------------------------------------------------------------------------


def select_anchor(
    *,
    checkout: Path,
    ledger_path: Path,
    predicate_path: Path,
    target_ref: str,
    apply_floor: bool = True,
    max_scan: int = 0,
) -> dict:
    """Steps 1-5: pick the anchor. Returns a report dict (no selector run yet)."""
    predicate = load_predicate(predicate_path)
    target = resolve(checkout, target_ref)
    if target is None:
        return {"verdict": VERDICT_ERROR, "error": f"cannot resolve target {target_ref!r}"}

    rows, malformed = load_ledger(ledger_path)
    qualifying: dict[str, dict] = {}
    refusals: dict[str, int] = {}
    for row in rows:
        sha = row.get("commit")
        ok, reason = qualifying_receipt.authoritative_row_qualification(
            row,
            sha if isinstance(sha, str) else "",
            predicate,
            repositories=[checkout],
        )
        if not ok:
            refusals[reason.split("=")[0]] = refusals.get(reason.split("=")[0], 0) + 1
            continue
        sha = row["commit"]
        prev = qualifying.get(sha)
        # Keep the newest qualifying receipt per commit so the record cites the
        # receipt actually relied on.
        if prev is None or (row.get("finished_at") or "") > (prev.get("finished_at") or ""):
            qualifying[sha] = row

    # Step 3: ancestry bound.
    candidates = []
    not_present = 0
    non_ancestor = 0
    for sha, row in qualifying.items():
        if not commit_present(checkout, sha):
            not_present += 1
            continue
        if not is_ancestor(checkout, sha, target):
            non_ancestor += 1
            continue
        distance = first_parent_distance(checkout, sha, target)
        if distance is None:
            continue
        if max_scan and distance > max_scan:
            continue
        candidates.append((distance, sha, row))

    # Step 4: gate-schema floor.
    floor_refused = 0
    cleared = []
    for distance, sha, row in candidates:
        if apply_floor and not clears_floors(checkout, sha):
            floor_refused += 1
            continue
        cleared.append((distance, sha, row))

    # Step 5: NEAREST ancestor, not newest-by-time. The selection diff is
    # TIP vs ANCHOR, so the nearest qualifying ancestor minimises the diff and
    # therefore maximises the saving. Tie-break on the newer receipt (stable
    # sort: finished_at descending first, then distance ascending).
    cleared.sort(key=lambda item: item[2].get("finished_at") or "", reverse=True)
    cleared.sort(key=lambda item: item[0])

    base = {
        "target": target,
        "ledger_rows": len(rows),
        "ledger_malformed": malformed,
        "qualifying_receipts": len(qualifying),
        "refusal_reasons": refusals,
        "candidates_not_present_locally": not_present,
        "candidates_non_ancestor": non_ancestor,
        "candidates_below_floor": floor_refused,
        "eligible_anchors": len(cleared),
    }
    if not cleared:
        base["verdict"] = VERDICT_NO_ANCHOR
        return base

    distance, sha, row = cleared[0]
    base["anchor"] = {
        "sha": sha,
        "distance_commits": distance,
        "hop": 1,
        "receipt": {
            "finished_at": row.get("finished_at"),
            "profile": row.get("profile"),
            "selection_mode": row.get("selection_mode"),
            "schema_version": row.get("schema_version"),
            "executed_tests": row.get("executed_tests"),
            "real_seconds": row.get("real_seconds"),
            "host": row.get("host"),
        },
    }
    base["runner_up_anchors"] = [
        {"sha": s, "distance_commits": d} for d, s, _ in cleared[1:4]
    ]
    return base


def evaluate(
    *,
    checkout: Path,
    ledger_path: Path,
    predicate_path: Path,
    selector: Path,
    target_ref: str,
    apply_floor: bool = True,
    include_dirty: bool = False,
    max_scan: int = 0,
) -> tuple[dict, int]:
    report = select_anchor(
        checkout=checkout,
        ledger_path=ledger_path,
        predicate_path=predicate_path,
        target_ref=target_ref,
        apply_floor=apply_floor,
        max_scan=max_scan,
    )
    if report.get("verdict") == VERDICT_ERROR:
        return report, EXIT_REFUSED
    if report.get("verdict") == VERDICT_NO_ANCHOR:
        report["action"] = "run FULL: no qualifying full-green ancestor to inherit from"
        return report, EXIT_NO_ANCHOR

    anchor = report["anchor"]["sha"]
    files = changed_files(checkout, anchor, report["target"], include_dirty)
    if files is None:
        report["verdict"] = VERDICT_ERROR
        report["error"] = "git diff against the anchor failed"
        report["action"] = "run FULL"
        return report, EXIT_ERROR

    report["diff"] = {"file_count": len(files), "files": files[:50],
                      "truncated": len(files) > 50}

    selection = run_selector(selector, checkout, files)
    if selection is None:
        report["verdict"] = VERDICT_ERROR
        report["error"] = "select-tests.rs failed or emitted unparseable JSON"
        report["action"] = "run FULL"
        return report, EXIT_ERROR

    full = universe(selector, checkout)
    decision = selection.get("decision", "full")
    selected_nodes = selection.get("node_count") or 0
    selected_cells = selection.get("cell_count") or 0
    report["selection"] = {
        "decision": decision,
        "selected_nodes": selected_nodes,
        "universe_nodes": full.get("nodes"),
        "selected_cells": selected_cells,
        "universe_cells": full.get("cells"),
        "nodes": selection.get("nodes", []),
        "reasons": selection.get("reasons", []),
    }
    # DECAY METRICS. Reported, never gated on -- see the module docstring: these
    # weight every node equally, while full-run wall is dominated by a few heavy
    # e2e cells, so they OVERSTATE the saving. The wall-based signal is blocked
    # on per-node duration emission.
    report["decay"] = {
        "distance_commits": report["anchor"]["distance_commits"],
        "node_fraction": _fraction(selected_nodes, full.get("nodes")),
        "cell_fraction": _fraction(selected_cells, full.get("cells")),
        "wall_fraction": None,
        "wall_fraction_blocked_on": "per-node duration emission in the receipt "
                                    "(ci/test-selection.md: no measured durations yet)",
        "proxy_bias": "OPTIMISTIC -- node/cell fractions weight all nodes equally",
    }

    if decision == "full":
        report["verdict"] = VERDICT_REANCHOR
        cause = name_reanchor_cause(selection.get("reasons", []))
        report["reanchor_cause"] = cause or "unnamed (selector returned full without a force_full/unmapped reason)"
        report["action"] = ("run FULL now: the selected set IS the full set, so the anchor "
                            "buys nothing; this run becomes the new anchor")
        return report, EXIT_REANCHOR

    if decision == "skip":
        report["verdict"] = VERDICT_CLEAN
        report["action"] = "inherit the anchor's green wholesale: no CI-relevant change since the anchor"
    else:
        report["verdict"] = VERDICT_SELECTIVE
        report["action"] = f"run the {selected_nodes}-node selected subset; inherit the rest from the anchor"

    # The obligation a consumer MUST record so the inherited green is auditable
    # and re-derivable rather than a bare label.
    report["receipt_obligation"] = {
        "inherited_green": {
            "hop": 1,
            "anchor_sha": anchor,
            "anchor_receipt_finished_at": report["anchor"]["receipt"]["finished_at"],
            "anchor_profile": report["anchor"]["receipt"]["profile"],
            "anchor_selection_mode": report["anchor"]["receipt"]["selection_mode"],
            "distance_commits": report["anchor"]["distance_commits"],
            "diff_files": len(files),
            "selector_decision": decision,
            "selected_nodes": selected_nodes,
            "universe_nodes": full.get("nodes"),
        }
    }
    return report, EXIT_OK


def _fraction(num, den):
    if not den:
        return None
    return round(num / den, 4)


def decay_curve(*, checkout: Path, selector: Path, anchor: str, target: str) -> dict:
    """The three numbers the owner asked for, at every distance from the anchor:
    {commits-from-anchor, selected-vs-full counts, selected-vs-full wall}.

    Wall is reported as None with its blocker named -- see the module docstring.
    Walking the first-parent chain and re-running the selector per step is cheap
    (git diff + one selector invocation each); nothing is built and nothing runs.
    """
    res = _git(checkout, ["rev-list", "--first-parent", "--reverse", f"{anchor}..{target}"])
    if res.returncode != 0:
        return {"error": "rev-list failed"}
    chain = res.stdout.split()
    full = universe(selector, checkout)
    points = []
    first_full = None
    for distance, sha in enumerate(chain, start=1):
        files = changed_files(checkout, anchor, sha, False)
        out = run_selector(selector, checkout, files) if files is not None else None
        if out is None:
            points.append({"distance_commits": distance, "commit": sha, "error": "selector failed"})
            continue
        decision = out.get("decision")
        cause = name_reanchor_cause(out.get("reasons", []))
        if decision == "full" and first_full is None:
            first_full = {"distance_commits": distance, "commit": sha, "cause": cause}
        points.append({
            "distance_commits": distance,
            "commit": sha,
            "diff_files": len(files),
            "decision": decision,
            "selected_nodes": out.get("node_count"),
            "universe_nodes": full.get("nodes"),
            "selected_cells": out.get("cell_count"),
            "universe_cells": full.get("cells"),
            "node_fraction": _fraction(out.get("node_count") or 0, full.get("nodes")),
            "cell_fraction": _fraction(out.get("cell_count") or 0, full.get("cells")),
            "wall_fraction": None,
            "cause": cause,
        })
    return {
        "anchor": anchor,
        "target": target,
        "commits": len(chain),
        "universe": full,
        "points": points,
        # The CLIFF. force_full is monotonic over the union of the window, so the
        # first distance that forces full forces it at every greater distance too:
        # the saving is zero from here on and the anchor must be refreshed.
        "first_full_distance": first_full,
        "wall_fraction_blocked_on": "per-node duration emission in the receipt",
    }


def render_decay(curve: dict) -> str:
    if "error" in curve:
        return f"decay-curve error: {curve['error']}"
    u = curve["universe"]
    lines = [
        f"anchor={curve['anchor'][:12]} target={curve['target'][:12]} "
        f"commits={curve['commits']} universe nodes={u.get('nodes')} cells={u.get('cells')} "
        f"shards={u.get('shards')}",
        f"{'d':>3} {'commit':>12} {'files':>6} {'decision':>10} {'nodes':>9} {'cells':>9}  cause",
    ]
    for p in curve["points"]:
        if "error" in p:
            lines.append(f"{p['distance_commits']:>3} {p['commit'][:12]} {p['error']}")
            continue
        lines.append(
            f"{p['distance_commits']:>3} {p['commit'][:12]} {p['diff_files']:>6} "
            f"{p['decision']:>10} {p['selected_nodes']:>3}/{p['universe_nodes']:<5} "
            f"{p['selected_cells']:>3}/{p['universe_cells']:<5}  {(p['cause'] or '')[:64]}"
        )
    ff = curve["first_full_distance"]
    if ff:
        lines.append(
            f"\nFIRST FULL at d={ff['distance_commits']} ({ff['commit'][:12]}): {ff['cause']}"
            f"\n=> saving is ZERO for every d >= {ff['distance_commits']} "
            f"(force_full is monotonic in the union) => RE-ANCHOR"
        )
    else:
        lines.append("\nNo distance in this window forces full; the anchor still buys something.")
    lines.append("wall column: UNAVAILABLE -- " + curve["wall_fraction_blocked_on"])
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# rendering / CLI
# ---------------------------------------------------------------------------


def render(report: dict) -> str:
    lines = [f"verdict: {report.get('verdict')}"]
    lines.append(f"target:  {report.get('target', '?')}")
    if "error" in report:
        lines.append(f"error:   {report['error']}")
    lines.append(
        f"ledger:  {report.get('ledger_rows', 0)} rows, "
        f"{report.get('qualifying_receipts', 0)} qualifying receipts, "
        f"{report.get('eligible_anchors', 0)} eligible anchors "
        f"(non-ancestor {report.get('candidates_non_ancestor', 0)}, "
        f"absent {report.get('candidates_not_present_locally', 0)}, "
        f"below-floor {report.get('candidates_below_floor', 0)})"
    )
    anchor = report.get("anchor")
    if anchor:
        receipt = anchor["receipt"]
        lines.append(
            f"anchor:  {anchor['sha'][:12]} at first-parent distance "
            f"{anchor['distance_commits']} (hop={anchor['hop']}) "
            f"profile={receipt['profile']} selection_mode={receipt['selection_mode']} "
            f"executed={receipt['executed_tests']} finished_at={receipt['finished_at']}"
        )
    diff = report.get("diff")
    if diff:
        lines.append(f"diff:    {diff['file_count']} files (TIP vs ANCHOR, two-dot)")
    sel = report.get("selection")
    if sel:
        lines.append(
            f"select:  decision={sel['decision']} "
            f"nodes={sel['selected_nodes']}/{sel['universe_nodes']} "
            f"cells={sel['selected_cells']}/{sel['universe_cells']}"
        )
    decay = report.get("decay")
    if decay:
        lines.append(
            f"decay:   d={decay['distance_commits']} "
            f"node_fraction={decay['node_fraction']} "
            f"cell_fraction={decay['cell_fraction']} "
            f"wall_fraction=UNAVAILABLE ({decay['proxy_bias']})"
        )
    if report.get("reanchor_cause"):
        lines.append(f"cause:   {report['reanchor_cause']}")
    if report.get("action"):
        lines.append(f"action:  {report['action']}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Select the green-inheritance anchor for a target commit.")
    parser.add_argument("--target", default="HEAD", help="commit or ref to validate incrementally")
    parser.add_argument("--checkout", default=str(DEFAULT_CHECKOUT))
    parser.add_argument("--ledger", default=str(DEFAULT_LEDGER))
    parser.add_argument("--predicate", default=str(DEFAULT_PREDICATE))
    parser.add_argument("--selector", default=None,
                        help="path to select-tests.rs (default <checkout>/ci/select-tests.rs)")
    parser.add_argument("--include-dirty", action="store_true",
                        help="also feed staged/unstaged/untracked paths to the selector (local runs)")
    parser.add_argument("--max-scan", type=int, default=0,
                        help="ignore anchors farther than N first-parent commits (0 = no bound)")
    parser.add_argument("--no-floor", action="store_true", help="skip the gate-schema floor check")
    parser.add_argument("--decay-curve", action="store_true",
                        help="report distance / selected-vs-full / wall at every distance from the "
                             "anchor, and the distance at which the saving hits zero")
    parser.add_argument("--anchor", default=None,
                        help="with --decay-curve: measure from this anchor instead of the selected one")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    checkout = Path(args.checkout)
    selector = Path(args.selector) if args.selector else checkout / DEFAULT_SELECTOR_REL
    ledger = Path(args.ledger)
    predicate = Path(args.predicate)

    for path, what in ((checkout, "checkout"), (ledger, "ledger"), (predicate, "predicate"),
                       (selector, "selector")):
        if not path.exists():
            report = {"verdict": VERDICT_ERROR, "error": f"missing {what}: {path}"}
            print(json.dumps(report, indent=2) if args.json else render(report))
            return EXIT_REFUSED

    if args.decay_curve:
        target = resolve(checkout, args.target)
        if target is None:
            print(f"cannot resolve target {args.target!r}")
            return EXIT_REFUSED
        anchor = resolve(checkout, args.anchor) if args.anchor else None
        if anchor is None:
            picked = select_anchor(checkout=checkout, ledger_path=ledger,
                                   predicate_path=predicate, target_ref=target,
                                   apply_floor=not args.no_floor, max_scan=args.max_scan)
            if not picked.get("anchor"):
                print(json.dumps(picked, indent=2) if args.json else render(picked))
                return EXIT_NO_ANCHOR
            anchor = picked["anchor"]["sha"]
        curve = decay_curve(checkout=checkout, selector=selector, anchor=anchor, target=target)
        print(json.dumps(curve, indent=2) if args.json else render_decay(curve))
        return EXIT_REANCHOR if curve.get("first_full_distance") else EXIT_OK

    try:
        report, code = evaluate(
            checkout=checkout,
            ledger_path=ledger,
            predicate_path=predicate,
            selector=selector,
            target_ref=args.target,
            apply_floor=not args.no_floor,
            include_dirty=args.include_dirty,
            max_scan=args.max_scan,
        )
    except subprocess.TimeoutExpired as exc:
        report = {"verdict": VERDICT_ERROR, "error": f"timeout: {exc}", "action": "run FULL"}
        code = EXIT_ERROR
    except OSError as exc:
        report = {"verdict": VERDICT_ERROR, "error": str(exc), "action": "run FULL"}
        code = EXIT_ERROR

    print(json.dumps(report, indent=2) if args.json else render(report))
    return code


if __name__ == "__main__":
    sys.exit(main())
