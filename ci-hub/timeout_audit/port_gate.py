#!/usr/bin/env python3
"""PORT-TIME TIMEOUT GATE: audit a node's budgets AT THE MOMENT it is ported to
run genuinely boxed under safe-ci-dag-runner.

WHY THIS IS A GATE AND NOT A SWEEP.  If the timeout audit runs as a separate
pass after a port completes, it fails by construction: the generous wall budget
rides across the boundary, the port is declared done, and nobody re-derives
anything.  That is how a 600s wall budget ends up on an 8-second node -- not
because anyone chose 600s for it, but because nobody re-examined it at the
moment it moved.  So this is a per-port constraint, run as a STEP IN the port.

WHY WALL -> CPU.  A wall budget measures the machine, not the test.  Measured on
this corpus: under load the WALL max inflated 2.80x while the CPU max moved only
1.19x.  Porting to a boxed runner without converting the budget just relocates
an already-wrong number.

THE ONE CHECK EVERYTHING ELSE DEPENDS ON.  A declared budget is worth nothing if
nothing enforces it, and "declared but unenforced" is precisely the state this
work exists to leave.  In safe-ci-dag-runner the CPU-time monitor lives inside
`if let Some(c) = &cg` and reads cgroup `cpu.stat usage_usec`
(scheduler.rs ~600-628).  No cgroup manager => no monitor thread => a declared
`cpu_timeout` is INERT, silently.  So ENFORCEMENT REACHABILITY is check #1, and
a node that fails it cannot pass the gate no matter how well-derived its numbers
are.  UNSET on an unenforced path is honest; a derived-looking constant there is
theatre.

DERIVATION.  cpu_timeout = round(max(cpu_s) * 1.5), anchored on the distribution
MAX (not p95: the tail is what trips a ceiling), with >= 5 samples or the answer
is UNSET.  UNSET is a valid, correct answer.  A fabricated constant is a hard
failure.  The derivation itself is NOT reimplemented here -- it is consumed from
ci-hub/history/query.py:node_cpu_budgets, so there is one derivation authority
rather than two that can drift.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

SCHEMA_VERSION = 1

# Verdicts, worst first.
NOT_ENFORCED = "NOT_ENFORCED"      # path does not box -> any budget here is inert
BLOATED = "BLOATED"                # wall budget wildly exceeds observed need
UNDERIVED = "UNDERIVED"            # cpu_timeout present but not the derived value
MISSING_CPU = "MISSING_CPU"        # enforceable path, samples exist, no cpu_timeout
UNSET_OK = "UNSET_OK"              # too few samples -- UNSET is the correct answer
PASS = "PASS"

# A wall budget this many times the estimated duration is the "carried across the
# port" smell.  Not a hard failure on an unenforced path (nothing is enforced
# there anyway) but always reported.
WALL_BLOAT_RATIO = 10.0

MIN_SAMPLES = 5          # below this the honest answer is UNSET
CPU_MULTIPLIER = 1.5     # round(max_cpu * 1.5)


# --------------------------------------------------------------------------- enforcement


@dataclass
class PathVerdict:
    """A path has TWO independent properties, and conflating them is the whole
    trap this gate exists to avoid.

    ROUTED  the node is executed BY the runner, so the manifest's wall `timeout`
            and `jobs_flag` are honoured.
    BOXED   a cgroup manager exists, so `cpu_timeout` / memory caps are enforced.

    They move separately. The portable lane is currently ROUTED but NOT BOXED:
    the `run --only` port landed (killing the jq+bash second engine), which made
    wall timeouts live for the first time, while `--allow-cgroup-failure` keeps
    cgroups off. A one-boolean model would call that either "ported" or "not"
    and would miss exactly the transition the audit is supposed to catch.
    """

    name: str
    routed: bool
    boxed: bool
    reason: str

    def as_dict(self) -> dict[str, Any]:
        return dict(self.__dict__)


def enforcement_paths(root: Path) -> list[PathVerdict]:
    """Classify each way a DAG node can be executed, by reading the code.

    Deliberately derived from source rather than declared in a table, because a
    table is a proxy that goes stale the moment the code changes -- and this is
    the check the whole gate rests on.
    """
    out: list[PathVerdict] = []

    # 1. The authoritative portable lane: ci-portable.yml -> ci/run-node.sh.
    #    Bind to the actual EXEC LINE, not to the string "safe-ci-dag-runner"
    #    appearing somewhere in the file -- the header comment describes the
    #    retired jq+bash design and mentions the runner nine times, so a
    #    substring match reports "routed" for a file that does not route.
    rn = root / "hermit" / "ci" / "run-node.sh"
    if rn.is_file():
        code = [ln for ln in rn.read_text(encoding="utf-8", errors="replace").splitlines()
                if not ln.lstrip().startswith("#")]
        routed = any(("run" in ln and "--only" in ln and ("exec" in ln or "$runner" in ln))
                     for ln in code)
        opts_out = any("--allow-cgroup-failure" in ln for ln in code)
        out.append(PathVerdict(
            "ci-portable.yml -> ci/run-node.sh  (authoritative Regular-tests lane)",
            routed=routed,
            boxed=routed and not opts_out,
            reason=(
                ("routes each node through `safe-ci-dag-runner run --only`, so the manifest "
                 "wall timeout and jobs_flag ARE honoured" if routed else
                 "does not route through the runner")
                + ("; but adds --allow-cgroup-failure whenever GITHUB_ACTIONS/CI is set "
                   "(ephemeral VM treated as the containment boundary), so cg=None and "
                   "cpu_timeout/memory caps are NOT enforced" if opts_out else "")
            ),
        ))

    # 2. The runner-invoking lanes -- but the runner skips boxing under CI.
    runner_sources = (
        root / "agent-utils" / "rs" / "dagrun" / "src" / "cgroup.rs",
        root / "agent-utils" / "rs" / "safe-ci-dag-runner" / "src" / "cgroup.rs",
    )
    cg = next(
        (candidate for candidate in runner_sources if candidate.is_file()),
        runner_sources[0],
    )
    ci_short_circuit = False
    if cg.is_file():
        src = cg.read_text(encoding="utf-8", errors="replace")
        ci_short_circuit = 'var("CI")' in src and 'var("GITHUB_ACTIONS")' in src
    wf = root / "hermit" / ".github" / "workflows"
    allows = sorted(
        p.name for p in wf.glob("*.yml")
        if "--allow-cgroup-failure" in p.read_text(encoding="utf-8", errors="replace")
    ) if wf.is_dir() else []
    out.append(PathVerdict(
        "ci-dag.yml / validation-levels.yml -> ci/run-dag.sh (runner IS invoked)",
        routed=True,
        boxed=False,
        reason=(
            "reexec_in_scope() short-circuits when CI or GITHUB_ACTIONS is set, and GitHub "
            "sets GITHUB_ACTIONS on hosted AND self-hosted runners, so no systemd scope is "
            "entered; resolve_cgroups then either refuses (exit 3) or, with "
            f"--allow-cgroup-failure (used by {', '.join(allows) or 'none'}), runs UNBOXED "
            "with cg=None -- which disables the cpu_timeout monitor entirely"
        ) if ci_short_circuit else "no CI short-circuit found in cgroup.rs",
    ))

    # 3. Local invocation -- the only place boxing actually engages today.
    out.append(PathVerdict(
        "local safe-ci-dag-runner (developer / validate-run)",
        routed=True,
        boxed=True,
        reason="no CI env var, so the runner re-execs into a systemd --user scope and "
               "per-step cgroup caps engage ('cgroup boxing ACTIVE')",
    ))
    return out


def any_ci_path_boxes(root: Path) -> bool:
    """cpu_timeout/memory enforcement reachable anywhere in CI?"""
    return any(p.boxed for p in enforcement_paths(root) if not p.name.startswith("local"))


def any_ci_path_routed(root: Path) -> bool:
    """Wall timeout / jobs_flag honoured anywhere in CI? Moves independently of
    boxing, and it is the leg that ALREADY ported."""
    return any(p.routed for p in enforcement_paths(root) if not p.name.startswith("local"))


# --------------------------------------------------------------------------- manifests


def node_name(step: dict) -> str:
    """The manifest identifies a node as group + "." + job.

    NOT `name`/`id` -- those keys do not exist in these manifests, and using them
    makes every store lookup miss, so every node reports "no samples" and the
    gate degrades to a uniform UNSET that looks like a clean result. A join that
    silently matches nothing is the failure mode to guard, hence
    test_join_against_the_real_store_is_non_empty.
    """
    g, j = step.get("group"), step.get("job")
    if g and j:
        return f"{g}.{j}"
    return step.get("name") or step.get("id") or "?"


def load_nodes(root: Path) -> list[dict[str, Any]]:
    nodes: list[dict[str, Any]] = []
    for rel in ("hermit/ci/dag/portable.json", "hermit/ci/dag/privileged.json"):
        p = root / rel
        if not p.is_file():
            continue
        d = json.loads(p.read_text(encoding="utf-8"))
        for s in d.get("steps", d.get("nodes", [])):
            s = dict(s)
            s["_manifest"] = rel
            nodes.append(s)
    return nodes


def derive(budgets: dict[str, dict], node: str) -> tuple[Optional[int], int, str]:
    """(derived cpu_timeout, n_samples, note) from the SINGLE derivation authority."""
    b = budgets.get(node)
    if b is None:
        return None, 0, "no store samples for this node"
    n = int(b.get("n_samples") or 0)
    if n < MIN_SAMPLES or b.get("thin"):
        return None, n, f"only {n} samples (< {MIN_SAMPLES}) -- UNSET is the correct answer"
    raw = int(b["suggested_cpu_timeout"])
    # FLOOR AT 1. The scheduler enables the monitor only `if cpu_timeout > 0`
    # (scheduler.rs), so emitting a derived 0 -- which happens for any node whose
    # max_cpu is under ~0.33s -- would SILENTLY DISABLE the very ceiling this
    # audit exists to install, while looking like a derived number.
    derived = max(1, raw)
    note = f"round(max_cpu {b['max_cpu_s']}s * {CPU_MULTIPLIER})"
    if raw != derived:
        note += f" = {raw}, floored to 1 (0 means DISABLED in the scheduler)"
    return derived, n, note


# --------------------------------------------------------------------------- the gate


@dataclass
class NodeAudit:
    node: str
    manifest: str
    verdict: str
    wall_s: Optional[float] = None
    est_s: Optional[float] = None
    wall_bloat_x: Optional[float] = None
    declared_cpu_timeout: Optional[int] = None
    derived_cpu_timeout: Optional[int] = None
    n_samples: int = 0
    enforced: bool = False
    notes: list[str] = field(default_factory=list)

    def as_dict(self) -> dict[str, Any]:
        return dict(self.__dict__)


def audit_node(step: dict, budgets: dict[str, dict], *, enforced: bool) -> NodeAudit:
    name = node_name(step)
    hint = step.get("hint") or {}
    wall = step.get("timeout")
    est = hint.get("est_duration_s")
    declared = step.get("cpu_timeout")
    derived, n, note = derive(budgets, name)

    a = NodeAudit(
        node=name, manifest=step["_manifest"], verdict=PASS,
        wall_s=wall, est_s=est, declared_cpu_timeout=declared,
        derived_cpu_timeout=derived, n_samples=n, enforced=enforced,
    )
    a.notes.append(note)
    if wall and est:
        a.wall_bloat_x = round(float(wall) / float(est), 1) if float(est) else None

    # Check 1 -- enforcement reachability. Everything else is decoration without it.
    if not enforced:
        a.verdict = NOT_ENFORCED
        a.notes.append(
            "this node's path does not engage cgroups, so a cpu_timeout here would be "
            "INERT; port the path first, then derive"
        )
        if a.wall_bloat_x and a.wall_bloat_x >= WALL_BLOAT_RATIO:
            a.notes.append(
                f"wall {wall}s is {a.wall_bloat_x}x est {est}s -- the budget to re-derive "
                "at port time (and note the wall is inert on the run-node.sh path too)"
            )
        return a

    # Check 2 -- derived, not guessed.
    if derived is None:
        a.verdict = UNSET_OK if declared is None else UNDERIVED
        if declared is not None:
            a.notes.append(
                f"declared cpu_timeout={declared} but only {n} samples support a derivation "
                "-- a plausible invented constant is a hard failure; use UNSET"
            )
        return a
    if declared is None:
        a.verdict = MISSING_CPU
        a.notes.append(f"enforceable path with {n} samples: set cpu_timeout={derived}")
        return a
    if int(declared) != int(derived):
        a.verdict = UNDERIVED
        a.notes.append(f"declared {declared} != derived {derived} ({note})")
        return a

    if a.wall_bloat_x and a.wall_bloat_x >= WALL_BLOAT_RATIO:
        a.verdict = BLOATED
        a.notes.append(f"cpu_timeout is correct but wall {wall}s is {a.wall_bloat_x}x est {est}s")
    return a


def run_gate(root: Path, only: Optional[list[str]] = None) -> dict[str, Any]:
    paths = enforcement_paths(root)
    ci_boxes = any(p.boxed for p in paths if not p.name.startswith("local"))
    try:
        sys.path.insert(0, str(root / "ci-hub" / "history"))
        import query  # noqa: E402
        rows = query.node_cpu_budgets(str(root), None, None, MIN_SAMPLES)
        budgets = {r["node"]: r for r in rows}
    except Exception as err:                       # pragma: no cover - store may be absent
        budgets = {}
        paths.append(PathVerdict("budget-store", False, False, f"unavailable: {err}"))

    nodes = load_nodes(root)
    if only:
        nodes = [n for n in nodes if node_name(n) in set(only)]
    audits = [audit_node(s, budgets, enforced=ci_boxes) for s in nodes]

    counts: dict[str, int] = {}
    for a in audits:
        counts[a.verdict] = counts.get(a.verdict, 0) + 1
    return {
        "schema_version": SCHEMA_VERSION,
        "enforcement_paths": [p.as_dict() for p in paths],
        "any_ci_path_boxes": ci_boxes,
        "any_ci_path_routed": any(p.routed for p in paths if not p.name.startswith("local")),
        "nodes_audited": len(audits),
        "verdicts": counts,
        "bloated_wall_nodes": sum(
            1 for a in audits if a.wall_bloat_x and a.wall_bloat_x >= WALL_BLOAT_RATIO
        ),
        "derivable_now": sum(1 for a in audits if a.derived_cpu_timeout is not None),
        "audits": [a.as_dict() for a in audits],
    }


# --------------------------------------------------------------------------- CLI


def _root() -> Path:
    return Path(__file__).resolve().parent.parent.parent


def _cmd_gate(args: argparse.Namespace) -> int:
    res = run_gate(_root(), args.only)
    if args.json:
        print(json.dumps(res, indent=2, sort_keys=True))
        return 0 if res["verdicts"].get(NOT_ENFORCED, 0) == 0 else 1

    print("PORT-TIME TIMEOUT GATE\n")
    print("Check 1 -- ENFORCEMENT REACHABILITY (everything else depends on it):")
    for p in res["enforcement_paths"]:
        print(f"  [{'BOXED    ' if p['boxed'] else 'NOT BOXED'}] {p['name']}")
        print(f"              {p['reason']}")
    print(f"\n  => any CI path boxes today: {res['any_ci_path_boxes']}")
    print(f"  => any CI path is ROUTED through the runner: {res['any_ci_path_routed']}")
    if res["any_ci_path_routed"] and not res["any_ci_path_boxes"]:
        print("     => HALF-PORTED, and this is the moment the audit exists for.")
        print("        ROUTING landed (`run --only` retired the jq+bash second engine), so the")
        print("        manifest WALL timeouts are enforceable for the FIRST time -- carried")
        print("        across unexamined. BOXING did not, so every cpu_timeout stays inert and")
        print("        UNSET remains the honest answer for it.")

    print(f"\nNodes audited: {res['nodes_audited']}")
    for k, v in sorted(res["verdicts"].items(), key=lambda kv: -kv[1]):
        print(f"  {k:<14} {v}")
    print(f"  wall budgets >= {WALL_BLOAT_RATIO:.0f}x est: {res['bloated_wall_nodes']}")
    print(f"  nodes with >= {MIN_SAMPLES} samples (derivable the moment they port): "
          f"{res['derivable_now']}")

    worst = [a for a in res["audits"]
             if a["wall_bloat_x"] and a["wall_bloat_x"] >= WALL_BLOAT_RATIO]
    worst.sort(key=lambda a: -(a["wall_bloat_x"] or 0))
    if worst:
        print("\nWorst carried-wall offenders (re-derive these AT port time):")
        print(f"  {'NODE':<34} {'WALL':>7} {'EST':>7} {'BLOAT':>7} {'DERIVED_CPU':>12}")
        for a in worst[:15]:
            d = a["derived_cpu_timeout"]
            print(f"  {a['node']:<34} {a['wall_s']:>7} {a['est_s']:>7} "
                  f"{a['wall_bloat_x']:>6}x {('UNSET' if d is None else d):>12}")
    return 0 if res["verdicts"].get(NOT_ENFORCED, 0) == 0 else 1


def _cmd_selftest(_a: argparse.Namespace) -> int:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import tests.test_port_gate as t
    return t.run_as_selftest()


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("gate", description="run the port-time checklist over the DAG manifests", help="run the port-time checklist over the DAG manifests")
    g.add_argument("--only", nargs="*", help="restrict to these node names (the porting set)")
    g.add_argument("--json", action="store_true")
    g.set_defaults(func=_cmd_gate)
    s = sub.add_parser("selftest", description="run the decision-table tests", help="run the decision-table tests")
    s.set_defaults(func=_cmd_selftest)
    args = ap.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
