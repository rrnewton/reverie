#!/usr/bin/env python3
"""Machine-wide view of the canonical per-machine validation-ledger union.

Sweeps and aggregates EVERY validate run on this machine into one view,
regardless of which repo (hermit or reverie), worktree, agent, or slot produced
it. Each record carries a `repo` discriminator so the two products are queryable
through one command and comparable over time. Receipt rows come from exactly one
source: ``ci-hub/ledger/validate_rows.py``. Raw logs remain diagnostic evidence;
they are never reconstructed into receipt-like rows.

safe-ci-dag-runner per-node profiling CSVs under
     `<checkout>/.safe-ci-dag-runner/profiles/` (aggregate + per-step). These
     are retained and appended across runs; this tool indexes them and links
     each run to its profiling by (checkout, git_sha, timestamp).

This is the canonical validate ledger's machine-wide aggregation boundary and
lives in the dev-hermit PARENT per the tracking-goes-outer principle.

Usage:
  ci-hub/validate/aggregate.py                 # 10 newest runs, newest first
  ci-hub/validate/aggregate.py --limit 25      # bounded recent view
  ci-hub/validate/aggregate.py --all           # every retained run, newest last
  ci-hub/validate/aggregate.py --all --json    # complete JSON dump
  ci-hub/validate/aggregate.py --all --csv out.csv  # complete flat CSV
  ci-hub/validate/aggregate.py --since 2026-08-03  # filter by date
  ci-hub/validate/aggregate.py --profiling     # profiling coverage view
"""
from __future__ import annotations

import argparse
import csv as csvmod
import datetime as dt
import glob
import json
import os
from pathlib import Path
import re
import sys

# ONE extractor, not a second regex copy that could drift: the executed- and
# filtered-test counting logic lives solely in the remediation `nonzero_result`
# module (also imported by remediation/protocol.py). This aggregator feeds that
# module the candidate banner lines it already reads while streaming; it does not
# re-implement `running N tests` parsing.
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "remediation"))
from nonzero_result import executed_test_count, filtered_test_count  # noqa: E402

# Flake/contention reclassification of reds lives beside this aggregator (same
# validate/ dir). A recorded `fail` that is actually a known-flaky-cell coin-flip
# or a cargo-cache contention artifact gets a `flake_analysis` annotation so the
# false red surfaces for re-measurement instead of permanently condemning a
# healthy commit. Read-side half; the producer (hermit/scripts/validate.rs) records the
# -j width + concurrent-validate count and enforces re-run-before-write.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import flake_class  # noqa: E402
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "ledger"))
import validate_rows as canonical_ledger  # noqa: E402

SHA_RE = re.compile(r"\b[0-9a-f]{40}\b")
DUR_RE = re.compile(r"(\d+)")


def parent_root() -> str:
    """Locate the dev-hermit parent (this script lives in ci-hub/validate)."""
    env = os.environ.get("DEV_HERMIT_PARENT")
    if env and os.path.isdir(env):
        return os.path.abspath(env)
    return os.path.abspath(os.path.join(os.path.dirname(__file__), "../.."))


def tmpdirs() -> list[str]:
    override = os.environ.get("CI_HUB_AGGREGATE_TMPDIRS")
    if override is not None:
        candidates = override.split(os.pathsep)
    else:
        candidates = (os.environ.get("TMPDIR", "").rstrip("/"), "/tmp")
    seen, out = set(), []
    for d in candidates:
        d = d.rstrip("/")
        if d and d not in seen and os.path.isdir(d):
            seen.add(d)
            out.append(d)
    return out


def iso(ts: float) -> str:
    return dt.datetime.fromtimestamp(ts, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


PRUNE = {"target", ".git", "node_modules", ".cargo", "incremental", "deps",
         "build", ".venv", "__pycache__"}

# The runner's per-checkout profile store, in both spellings. agent-utils renamed
# it .safe-ci-dag-runner/profiles -> .dagrun/profiles, and this parent reads
# checkouts on BOTH sides of that rename (current main pins the new name; an
# older commit or unrebased PR head pins the old one; a bisect walks across the
# boundary). Matching only one name silently yields zero profile rows, which
# reads as "this run recorded no profiling" rather than as a lookup that missed.
PROFILE_STORE_DIRS = (".dagrun", ".safe-ci-dag-runner")


def walk_pruned(root: str, want, max_depth: int = 6):
    """Yield files matching predicate `want(name)`, skipping heavy build dirs.

    Recursive `glob("**")` traverses every worktree's target/ tree (millions of
    files) and never returns; this prunes those and bounds depth instead.
    """
    root = os.path.abspath(root)
    base_depth = root.rstrip("/").count("/")
    for dirpath, dirnames, filenames in os.walk(root):
        depth = dirpath.rstrip("/").count("/") - base_depth
        if depth >= max_depth:
            dirnames[:] = []
        # Prune heavy dirs in place (but keep the runner profile stores).
        dirnames[:] = [d for d in dirnames
                       if d not in PRUNE
                       and not any(d.startswith(s + ".") for s in PROFILE_STORE_DIRS)]
        for fn in filenames:
            if want(fn):
                yield os.path.join(dirpath, fn)


def slot_from_cwd(cwd: str | None) -> str:
    if not cwd:
        return "unknown"
    # Current slots live below worktrees/slots/. Keep reading historical rows
    # from the former worktrees/<slot>/ layout: the ledger is append-only and a
    # directory move must not erase its slot attribution.
    m = re.search(r"/worktrees/(?:slots/)?([^/]+)/", cwd + "/")
    if m:
        return m.group(1)
    if cwd.startswith("/tmp") or "/tmp/" in cwd:
        return "standalone"
    if cwd.rstrip("/").endswith(("/dev-hermit/hermit", "/dev-hermit/reverie")):
        return "primary"
    return os.path.basename(cwd.rstrip("/")) or "unknown"


def repo_from(cwd: str | None, path: str | None, header_repo: str | None) -> str:
    """Attribute a run to hermit or reverie.

    Trust order: the log header's own product line (`Hermit/Reverie validation`),
    then the raw-log filename prefix, then the checkout basename, defaulting to
    hermit so pre-`repo` ledger records (only hermit ever wrote them) stay
    correctly attributed.
    """
    if header_repo in ("hermit", "reverie"):
        return header_repo
    base = os.path.basename(path or "")
    if base.startswith("reverie-validate"):
        return "reverie"
    if base.startswith("hermit-validate"):
        return "hermit"
    tail = (cwd or "").rstrip("/")
    if tail.endswith("/reverie"):
        return "reverie"
    return "hermit"


# ---------------------------------------------------------------------------
# Source 1 + 2: ledgers and raw logs -> unified run records
# ---------------------------------------------------------------------------

def load_ledger_records(parent: str):
    records = {}  # keyed by canonical log_file (fallback: synthetic key)
    for index, source in enumerate(canonical_ledger.rows(root=Path(parent))):
        rec = dict(source)
        rec["_source"] = "ledger"
        rec["_ledger_file"] = "canonical-union"
        rec.setdefault("repo", "hermit")
        key = rec.get("record_id") or rec.get("log_file") or f"canonical:{index}"
        records[key] = rec
    return records


# Faithful per-gate classes. Each is anchored ONLY on markers the Hermit driver
# writes itself, never on workload text (a test named "panic" or a program that
# prints "timeout" must not sway the class). See hermit/scripts/validate.rs.
#   * real gate  -> `=== name ===`, `Command: ...`, [output], then ALWAYS
#     `Exit: <status>` + `Duration: <n>s`
#     (run_check_with_timeout / wait_for_background_checks / *_compatibility_probe).
#   * killed by a bound -> a real gate whose body carries `Gate timed out after`
#     (run_timed_command) and/or Exit 124 (a bare `timeout` in a compat probe).
#     This is the RESOURCE story: timeouts / cgroup caps / contention.
#   * deferred    -> `=== name ===` + `Skipped: ...`, no Exit (not run).
#   * section banner -> `=== name ===` alone, no `Command:`/`Exit:` (the
#     Record/replay, SaBRe, e9patch, and Strict-envelope headers).
# A real gate with a `Command:` but no `Exit:` means the log was cut off mid-gate
# (the run did not finish) -> `incomplete`, which is NOT a product failure.
GATE_KINDS = ("pass", "fail", "timeout", "incomplete", "skipped", "banner")
# Verdict-bearing gates: the ones that count toward the pass/total denominator.
VERDICT_KINDS = ("pass", "fail", "timeout", "incomplete")
# Legacy per-gate `result` (pass/fail/skip/unknown/banner) kept for any external
# reader; the killed-by-a-bound vs product-fail split is carried by `kind`.
_KIND_TO_RESULT = {"pass": "pass", "fail": "fail", "timeout": "fail",
                   "skipped": "skip", "incomplete": "unknown", "banner": "banner"}


def _classify_gate(g: dict) -> str:
    if g.get("_has_exit"):
        if g.get("exit_code") == 0:
            return "pass"
        if g.get("_timed_out") or g.get("exit_code") == 124:
            return "timeout"
        return "fail"
    if g.get("_skipped"):
        return "skipped"
    if g.get("_has_command"):
        return "incomplete"
    return "banner"


def gate_kind(g: dict) -> str:
    """Class of a gate, tolerant of ledger rows that only carry `result`.

    `no_result` is mapped explicitly and must stay that way. The fallback below
    infers a pass from a zero exit status, which is right for a row carrying no
    `result` at all and WRONG for a node that reported it could not determine
    one. Until hermit's classifier gained the never-written-report case, every
    `no_result` row also carried a nonzero or null exit status, so the fallback
    happened to land on `incomplete` and the gap was invisible. That refusal
    fires precisely when the command exited 0, so the same gap now lands on
    `pass` and counts an undetermined node toward the GATES numerator.
    See hermit https://github.com/rrnewton/hermit/pull/3066.
    """
    k = g.get("kind")
    if k in GATE_KINDS:
        return k
    return {"pass": "pass", "fail": "fail", "skip": "skipped",
            "banner": "banner", "unknown": "incomplete",
            "no_result": "incomplete"}.get(
                g.get("result"),
                "pass" if g.get("exit_code") == 0 else "incomplete")


def parse_raw_log(path: str) -> dict:
    root = level = None
    gates = []
    cur = None
    commit = None
    header_repo = None
    test_banner_lines: list[str] = []
    selection = None
    try:
        with open(path, errors="replace") as fh:
            for ln in fh:
                ln = ln.rstrip("\n")
                # Test-runner banners live inside gate output, not on the gate
                # structure lines, so collect the candidate banner lines here
                # (independently of the gate-parsing elif chain below) and hand
                # them to the shared counter after the loop. The `in` pre-filter
                # is a cheap SUPERSET of what nonzero_result's regexes match, so
                # nothing that would count is dropped, and the count logic itself
                # stays single-sourced there.
                if "running " in ln or "test result:" in ln:
                    test_banner_lines.append(ln)
                # Validation producers print `... ; selection: <mode>` on the Commit line.
                if selection is None and "selection:" in ln:
                    selection = ln.rsplit("selection:", 1)[1].strip() or None
                if header_repo is None and ln.startswith("Hermit validation"):
                    header_repo = "hermit"
                elif header_repo is None and ln.startswith("Reverie validation"):
                    header_repo = "reverie"
                if root is None and ln.startswith("Root:"):
                    root = ln.split(":", 1)[1].strip()
                elif level is None and ln.startswith("Level:"):
                    level = ln.split(":", 1)[1].strip()
                elif ln.startswith("=== ") and ln.endswith(" ==="):
                    if cur is not None:  # previous gate never got its Duration footer
                        gates.append(cur)
                    cur = {"name": ln[4:-4].strip()}
                elif cur is not None and ln.startswith("Command:"):
                    cur["_has_command"] = True
                elif cur is not None and ln.startswith("Skipped:"):
                    cur["_skipped"] = True
                elif cur is not None and ln.startswith("Gate timed out after"):
                    # Validation producers write this as their own timed-command line;
                    # requiring the line prefix keeps guest stdout from ever
                    # forging the marker.
                    cur["_timed_out"] = True
                elif cur is not None and ln.startswith("Exit:"):
                    try:
                        cur["exit_code"] = int(ln.split(":", 1)[1].strip())
                    except ValueError:
                        cur["exit_code"] = -1
                    cur["_has_exit"] = True
                elif cur is not None and ln.startswith("Duration:"):
                    m = DUR_RE.search(ln)
                    cur["real_seconds"] = int(m.group(1)) if m else None
                    gates.append(cur)
                    cur = None
                elif commit is None:
                    m = SHA_RE.search(ln)
                    if m:
                        commit = m.group(0)
    except OSError:
        pass
    if cur is not None:  # trailing gate with no Duration footer (run cut off)
        gates.append(cur)
    counts = {k: 0 for k in GATE_KINDS}
    for g in gates:
        g["kind"] = _classify_gate(g)
        g["result"] = _KIND_TO_RESULT[g["kind"]]
        counts[g["kind"]] += 1
        for k in ("_has_command", "_skipped", "_timed_out", "_has_exit"):
            g.pop(k, None)
    # Executed- and filtered-test counts via the shared extractor. `None` means
    # UNKNOWN (no banner seen — a build-only/skipped-gate log), distinct from `0`
    # (banners present and every one executed zero tests). `filtered_tests` tells
    # a zero-executed EMPTY TARGET (filtered==0) apart from a FILTERED-TO-EMPTY
    # run (filtered>0), and exposes the `1 passed; 154 filtered out` narrowed-
    # scope trap on an otherwise-green row. See nonzero_result.
    banner_text = "\n".join(test_banner_lines)
    executed_tests = executed_test_count(banner_text)
    filtered_tests = filtered_test_count(banner_text)
    # A PASS must also carry WHAT it covered. The validation driver overrides the profile
    # name to the "-only" form for every partial run (and to `selective`/`only-X`
    # for subset selections), so `Level:` — parsed into `level` — is `full` iff
    # the run was a full-coverage validate. `full_coverage` lets any reader tell a
    # full green from a partial one WITHOUT knowing which profile names are
    # partial; the verdict below types a partial pass `pass-partial`, never a bare
    # `pass`. (`selection` corroborates but cannot override: a subset selection
    # already forces a non-`full` profile.)
    full_coverage = level == "full"
    # Run verdict, most-severe first. Killed-by-a-bound (`timeout`: a RESOURCE
    # story) is deliberately kept distinct from a product `fail` and from an
    # `incomplete` (cut-off) run, so attribution is never conflated. Skipped
    # gates cannot decide the verdict.
    if counts["fail"]:
        result = "fail"
    elif counts["timeout"]:
        result = "timeout"
    elif counts["incomplete"]:
        result = "incomplete"
    elif executed_tests == 0:
        # A GREEN must carry a NONZERO executed-test count. Every gate exited 0
        # yet the banners prove zero tests ran — a no-result wearing a success
        # badge (the classic `--features`-gated build that compiles the tests
        # out). Downgrade to `no_result` so it is never certified as a pass.
        # `None` (unknown) is NOT `0`, so a banner-less green is untouched.
        result = "no_result"
    elif not full_coverage:
        # A partial-profile run (e.g. portable-strict-compat-only: 2 gates) that
        # passes is a real pass over a NARROWED scope. Typing it `pass-partial`
        # keeps it from reading as a full-coverage green to anyone who does not
        # know the profile taxonomy. (The landing certifier already refuses it
        # via its profile==full predicate; this makes the row self-describing.)
        result = "pass-partial"
    else:
        result = "pass"
    non_banner = sum(counts[k] for k in GATE_KINDS if k != "banner")
    mtime = os.path.getmtime(path)
    return {
        "schema_version": 1,
        "finished_at": iso(mtime),
        "started_at": None,
        "host": os.uname().nodename,
        "slot": slot_from_cwd(root),
        "repo": repo_from(root, path, header_repo),
        "cwd": root,
        "profile": level,
        "commit": commit or "unknown",
        "result": result,
        "executed_tests": executed_tests,
        "filtered_tests": filtered_tests,
        "selection_mode": selection,
        "full_coverage": full_coverage,
        "checks": non_banner,
        "failures": counts["fail"],            # product failures only
        "killed_by_bound": counts["timeout"],  # resource story: timeout/cgroup/contention
        "incomplete_gates": counts["incomplete"],  # run cut off before the gate footer
        "skipped_gates": counts["skipped"],
        "banner_lines": counts["banner"],
        "gate_classification": counts,
        "real_seconds": sum(g.get("real_seconds") or 0 for g in gates),
        "user_seconds": None,
        "sys_seconds": None,
        "log_file": path,
        "gates": gates,
        "_source": "reconstructed",
        "_ledger_file": None,
        "_mtime": mtime,
    }


def load_all_runs(parent: str) -> list[dict]:
    records = load_ledger_records(parent)
    runs = list(records.values())
    for r in runs:
        r.setdefault("_mtime", 0)
        if not r.get("_mtime"):
            lf = r.get("log_file")
            r["_mtime"] = os.path.getmtime(lf) if lf and os.path.isfile(lf) else 0
        r["_sortkey"] = r.get("started_at") or r.get("finished_at") or iso(r["_mtime"])
    runs.sort(key=lambda r: r["_sortkey"])
    return runs


# ---------------------------------------------------------------------------
# Source 3: safe-ci-dag-runner profiling
# ---------------------------------------------------------------------------

def discover_profiles(parent: str) -> list[dict]:
    rows = []

    def is_agg_csv(fn: str) -> bool:
        return (fn.endswith(".csv") and not fn.startswith("step_profiles_"))

    for path in walk_pruned(parent, is_agg_csv):
        store = next((s for s in PROFILE_STORE_DIRS
                      if f"/{s}/profiles/" in path), None)
        if store is None:
            continue
        checkout = path.split(f"/{store}/")[0]
        checkout_repo = "reverie" if checkout.rstrip("/").endswith("/reverie") else "hermit"
        try:
            with open(path, errors="replace") as fh:
                for row in csvmod.DictReader(fh):
                    row["_checkout"] = checkout
                    row["_store"] = store
                    row["_slot"] = slot_from_cwd(checkout + "/")
                    row["_repo"] = checkout_repo
                    row["_file"] = path
                    rows.append(row)
        except OSError:
            continue
    return rows


def _epoch(s: str | None) -> float | None:
    if not s:
        return None
    try:
        return dt.datetime.strptime(s[:19], "%Y-%m-%dT%H:%M:%S").replace(
            tzinfo=dt.timezone.utc).timestamp()
    except ValueError:
        return None


def link_profiling(runs: list[dict], profiles: list[dict], window_s: int = 90) -> int:
    by_sha = {}
    for p in profiles:
        by_sha.setdefault((p.get("git_sha") or "")[:12], []).append(p)
    linked = 0
    for r in runs:
        # Never link across products: a reverie run must not borrow a hermit
        # profile that merely shares a slot+timestamp (reverie has no
        # safe-ci-dag-runner profiling of its own today, so its runs stay
        # unlinked rather than mis-attributed).
        r_repo = r.get("repo") or "hermit"
        sha = (r.get("commit") or "")[:12]
        cands = ([c for c in by_sha.get(sha, []) if c.get("_repo") == r_repo]
                 if sha and sha != "unknown"[:12] else [])
        match_kind = "git_sha"
        if not cands:
            # Fallback: same slot + profiling timestamp within window of the run.
            # (validation drivers and safe-ci-dag-runner record different SHA fields for
            # the same invocation, but the wall-clock timestamps coincide.)
            rt = _epoch(r.get("finished_at") or r.get("started_at"))
            if rt is not None:
                cands = [p for p in profiles
                         if p.get("_slot") == r.get("slot")
                         and p.get("_repo") == r_repo
                         and (pt := _epoch(p.get("timestamp"))) is not None
                         and abs(pt - rt) <= window_s]
                match_kind = "slot+time"
        # Prefer same slot.
        same = [c for c in cands if c.get("_slot") == r.get("slot")]
        chosen = (same or cands)
        if chosen:
            r["_profiling_match"] = match_kind
            r["_profiling"] = {
                "n_profiles": len(chosen),
                "checkouts": sorted({c["_checkout"] for c in chosen}),
                "sample": {k: chosen[0].get(k) for k in
                           ("timestamp", "wall_s", "user_s", "sys_s", "n_steps",
                            "total_busy_pct", "jobs", "nproc")},
            }
            linked += 1
        else:
            r["_profiling"] = None
    return linked


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------

def fmt_secs(v):
    return "-" if v is None else f"{float(v):.0f}"


def render_table(runs: list[dict]) -> str:
    # WALL/USER/SYS carry an explicit `(s)` so a bare number is never unit-less.
    # PROFILE is never truncated: a clipped identifier (`portable-stric`) is not
    # an identifier.
    hdr = ("TIME (UTC)", "REPO", "SLOT", "COMMIT", "PROFILE", "RESULT",
           "GATES", "WALL(s)", "USER(s)", "SYS(s)", "SRC", "PROF")
    lines = []
    rows = []
    for r in runs:
        gates = r.get("gates") or []
        verdict = [g for g in gates if gate_kind(g) in VERDICT_KINDS]
        npass = sum(1 for g in verdict if gate_kind(g) == "pass")
        # GATES denominator counts only verdict-bearing gates, so section banners
        # never inflate it. Fall back to the run's own tallies for ledger rows
        # whose gate list is absent.
        if verdict:
            ngates = f"{npass}/{len(verdict)}"
        else:
            checks = r.get("checks")
            fails = r.get("failures") or 0
            ngates = f"{(checks - fails)}/{checks}" if checks is not None else "-"
        t = (r.get("started_at") or r.get("finished_at") or "?")[:19]
        rows.append((
            t,
            (r.get("repo") or "hermit")[:7],
            (r.get("slot") or "?")[:16],
            (r.get("commit") or "?")[:8],
            (r.get("profile") or "?"),
            r.get("effective_result") or flake_class.effective_result(r) or "?",
            ngates,
            fmt_secs(r.get("real_seconds")),
            fmt_secs(r.get("user_seconds")),
            fmt_secs(r.get("sys_seconds")),
            "L" if r.get("_source") == "ledger" else "R",
            ("y" if r.get("_profiling") else "-")
            if r.get("_profiling_checked")
            else "?",
        ))
    widths = [len(h) for h in hdr]
    for row in rows:
        for i, c in enumerate(row):
            widths[i] = max(widths[i], len(str(c)))
    fmt = "  ".join("{:<%d}" % w for w in widths)
    lines.append(fmt.format(*hdr))
    lines.append(fmt.format(*["-" * w for w in widths]))
    for row in rows:
        lines.append(fmt.format(*[str(c) for c in row]))
    return "\n".join(lines)


def summarize(
    runs,
    profiles,
    linked,
    *,
    total_matching: int | None = None,
    profiles_scanned: bool = True,
) -> str:
    by_result, by_slot, by_src, by_repo = {}, {}, {}, {}
    for r in runs:
        result = flake_class.effective_result(r)
        by_result[result] = by_result.get(result, 0) + 1
        by_slot[r.get("slot")] = by_slot.get(r.get("slot"), 0) + 1
        by_src[r.get("_source")] = by_src.get(r.get("_source"), 0) + 1
        repo = r.get("repo") or "hermit"
        by_repo[repo] = by_repo.get(repo, 0) + 1
    # De-conflate the old catch-all: killed-by-a-bound (resource story) vs
    # product failures vs cut-off runs are separate attributions.
    killed = sum(r.get("killed_by_bound") or 0 for r in runs)
    incomplete = sum(r.get("incomplete_gates") or 0 for r in runs)
    prod_fail = sum(
        r.get("failures") or 0
        for r in runs
        if flake_class.effective_result(r) in ("fail", "timeout")
    )
    out = ["", "=== Machine-wide validate-run summary ==="]
    total_matching = len(runs) if total_matching is None else total_matching
    if len(runs) < total_matching:
        out.append(
            f"  shown runs        : {len(runs)} newest of {total_matching} matching"
        )
    else:
        out.append(f"  total runs        : {len(runs)}")
    out.append(f"  by repo           : " +
               ", ".join(f"{k}={v}" for k, v in sorted(by_repo.items())))
    out.append(f"  by result         : " +
               ", ".join(f"{k}={v}" for k, v in sorted(by_result.items(), key=lambda x: str(x[0]))))
    out.append(f"  gate attribution  : "
               f"product-fail={prod_fail}, killed-by-bound={killed} "
               f"(timeout/cgroup/contention), incomplete={incomplete} (run cut off)"
               "   (RESULT values: pass/pass-partial/no_result/fail/timeout/incomplete/truncated;"
               " pass-partial=non-full profile, no_result=zero tests executed)")
    out.append(f"  by source         : " +
               ", ".join(f"{k}={v}" for k, v in sorted(by_src.items(), key=lambda x: str(x[0]))) +
               "   (L=ledger, R=reconstructed-from-raw-log)")
    out.append(f"  by slot/worktree  : " +
               ", ".join(f"{k}={v}" for k, v in sorted(by_slot.items(), key=lambda x: -x[1])))
    if profiles_scanned:
        out.append(f"  profiling CSVs    : {len(profiles)} rows across "
                   f"{len({p['_checkout'] for p in profiles})} checkouts; "
                   f"{linked}/{len(runs)} runs linked to profiling by git_sha")
    else:
        out.append(
            "  profiling CSVs    : not scanned in the recent view "
            "(use --all or --profiling)"
        )
    return "\n".join(out)


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def recent_runs(runs: list[dict], limit: int) -> list[dict]:
    """Return a bounded newest-first view without mutating the full history."""
    return list(reversed(runs[-limit:]))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    view = ap.add_mutually_exclusive_group()
    view.add_argument("--limit", type=positive_int, default=10,
                      help="show N newest runs, newest first (default: 10)")
    view.add_argument("--all", action="store_true",
                      help="show every retained run (slower; newest last)")
    ap.add_argument("--json", action="store_true", help="emit unified records as JSON")
    ap.add_argument("--csv", metavar="FILE", help="write a flat CSV")
    ap.add_argument("--since", metavar="YYYY-MM-DD", help="only runs on/after this UTC date")
    ap.add_argument("--slot", metavar="NAME", help="filter to one slot/worktree")
    ap.add_argument("--profiling", action="store_true",
                    help="show profiling-source coverage instead of the run table")
    ap.add_argument("--false-reds", action="store_true",
                    help="list recorded reds reclassified as needs-rerun "
                         "(known-flaky cell or contended run) — the false reds "
                         "that permanently condemn a healthy commit until re-run")
    args = ap.parse_args()

    parent = parent_root()
    runs = load_all_runs(parent)
    if args.since:
        runs = [r for r in runs if (r.get("_sortkey") or "") >= args.since]
    if args.slot:
        runs = [r for r in runs if r.get("slot") == args.slot]
    total_matching = len(runs)

    complete_analysis = args.all or args.profiling or args.false_reds
    if not complete_analysis:
        runs = recent_runs(runs, args.limit)

    # The retained profiling inventory walks every checkout. Keep that cost out
    # of the default recent view; '?' in the PROF column means "not inspected",
    # not "no profile exists".
    profiles_scanned = complete_analysis
    profiles = discover_profiles(parent) if profiles_scanned else []
    linked = link_profiling(runs, profiles)
    for r in runs:
        r["_profiling_checked"] = profiles_scanned

    # Resolve the -j width parent-side for profiled runs: the safe-ci-dag-runner
    # profiling CSV records `jobs` even though the ledger row does not yet (that
    # is the schema-4 producer change). A run whose width is knowable is a
    # stronger contention judgement than one whose width is unrecorded.
    for r in runs:
        if r.get("jobs") is None and isinstance(r.get("_profiling"), dict):
            j = (r["_profiling"].get("sample") or {}).get("jobs")
            if j not in (None, ""):
                try:
                    r["jobs"] = int(j)
                except (TypeError, ValueError):
                    pass
    # Annotate every red with defect-vs-flake/contention analysis (additive;
    # never mutates `result`).
    flake_class.annotate(runs)
    for r in runs:
        r["effective_result"] = flake_class.effective_result(r)

    if args.profiling:
        by_ckt = {}
        for p in profiles:
            by_ckt.setdefault(p["_checkout"], []).append(p)
        print("safe-ci-dag-runner profiling (retained per checkout, appended per run):")
        for ckt, rows in sorted(by_ckt.items()):
            ts = sorted({r.get("timestamp") for r in rows})
            print(f"  {slot_from_cwd(ckt + '/'):20} runs={len(ts):3}  "
                  f"first={ts[0]}  last={ts[-1]}")
            stores = sorted({r["_store"] for r in rows})
            print(f"      dir: {ckt}/{{{','.join(stores)}}}/profiles/"
                  if len(stores) > 1
                  else f"      dir: {ckt}/{stores[0]}/profiles/")
        print(f"\n  total profiling rows: {len(profiles)} across {len(by_ckt)} checkouts")
        return 0

    if args.false_reds:
        # A recorded red is NEVER re-run, so any red that classifies as NO-RESULT
        # (exercised nothing / no count) or NEEDS-RERUN (partial suite, contention,
        # or known-flaky) is a false row permanently condemning a healthy commit.
        # Surface both, with a per-verdict count so the reclassification is
        # reportable (deliverable: report the count).
        flagged = [r for r in runs
                   if (r.get("flake_analysis") or {}).get("verdict")
                   in ("needs-rerun", "no-result")]
        if not flagged:
            print("No recorded reds reclassify as needs-rerun/no-result "
                  f"(scanned {len(runs)} runs).")
            return 0
        n_no_result = sum(1 for r in flagged
                          if r["flake_analysis"]["verdict"] == "no-result")
        n_rerun = len(flagged) - n_no_result
        print(f"{len(flagged)} recorded red(s) are FALSE — a single FAILED here "
              "condemns a healthy commit until re-run "
              f"({n_no_result} NO-RESULT: exercised nothing / no count; "
              f"{n_rerun} NEEDS-RERUN: partial suite, contention, or known-flaky):\n")
        for r in flagged:
            fa = r["flake_analysis"]
            t = (r.get("started_at") or r.get("finished_at") or "?")[:19]
            print(f"  {t}  {(r.get('repo') or 'hermit')}  "
                  f"{(r.get('commit') or '?')[:10]}  profile={r.get('profile')}  "
                  f"result={r.get('result')}")
            print(f"      cells={fa.get('failing_cells') or '-'}  "
                  f"concurrent_validates={fa.get('concurrent_validates')}  "
                  f"jobs={fa.get('jobs')}")
            for reason in fa.get("reasons", []):
                print(f"      - {reason}")
        return 0

    if args.csv:
        with open(args.csv, "w", newline="") as fh:
            w = csvmod.writer(fh)
            # Units: real_s/user_s/sys_s are seconds. failures = product
            # failures only; killed_by_bound (timeout/cgroup/contention) and
            # incomplete (run cut off) are separate columns, not folded in.
            w.writerow(["time", "repo", "slot", "commit", "profile", "result",
                        "checks", "failures", "killed_by_bound", "incomplete",
                        "real_s", "user_s", "sys_s",
                        "source", "profiling_linked", "log_file"])
            for r in runs:
                w.writerow([
                    r.get("started_at") or r.get("finished_at"),
                    r.get("repo") or "hermit", r.get("slot"),
                    r.get("commit"), r.get("profile"), r.get("result"),
                    r.get("checks"), r.get("failures"),
                    r.get("killed_by_bound"), r.get("incomplete_gates"),
                    r.get("real_seconds"),
                    r.get("user_seconds"), r.get("sys_seconds"), r.get("_source"),
                    bool(r.get("_profiling")), r.get("log_file")])
        print(f"wrote CSV -> {args.csv}")

    if args.json:
        out = []
        for r in runs:
            clean = {k: v for k, v in r.items() if not k.startswith("_")}
            clean["source"] = r.get("_source")
            clean["profiling"] = r.get("_profiling")
            clean["profiling_checked"] = profiles_scanned
            out.append(clean)
        print(json.dumps(out, indent=2))
        return 0

    if not args.csv:
        print(render_table(runs))
    print(summarize(
        runs,
        profiles,
        linked,
        total_matching=total_matching,
        profiles_scanned=profiles_scanned,
    ))
    return 0


if __name__ == "__main__":
    sys.exit(main())
