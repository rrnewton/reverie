#!/usr/bin/env python3
"""Qualifying compat scorecard sweep at an exact Hermit SHA.

Differences from compat-envelope/collect-fullcorpus.sh (the parent collector):

  1. DETERMINISM VERDICT IS CANONICAL/BITWISE, NOT STRIPPED.
     The parent collector runs `--strict --verify` and hardcodes
     verify_compare=stripped / comparison_tier=unqualified-stdout-only into
     every row.  Plain `--verify` normalises numbers, addresses and paths
     before comparing, so a "pass" asserts stripped parity only.  This sweep
     runs `--verify --verify-strict --verify-json <path>` and reads the
     product's own machine-readable verdict, requiring
     `bitwise_parity == true` (the `canonical`/BitwiseInfoV1 policy) for a
     PASS.  The verdict is never inferred from the exit code alone.

  2. THREE OUTCOMES, NEVER TWO.  PASS / FAIL / NO-RESULT.  A cell whose
     source is absent, whose backend is unstaged, that timed out, or that
     produced no verdict JSON is a NO-RESULT and is reported with an
     explicit denominator -- it is never silently dropped and never counted
     red.

  3. CORPUS REPAIRS ARE EXPLICIT.  The parent corpus TSVs are stale relative
     to hermit main; see REPAIRS below.  Each repair is recorded per row in
     the `population_note` column.

  4. RED CELLS GET A SECOND, WEAKER MEASUREMENT.  Every FAIL under the
     canonical policy is re-run under plain (stripped) `--verify` so the
     report can separate "diverges even under the weak comparison"
     (red-stripped, severe) from "only the bitwise comparison catches it"
     (red-canonical-only, subtler).
"""

from __future__ import annotations

import argparse
import concurrent.futures
import csv
import hashlib
import json
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

# --------------------------------------------------------------------------
# Corpus repairs, each tied to the hermit commit that made the parent TSV stale.
# --------------------------------------------------------------------------
# e565b1ab "Rename the DBI backend to DBT" renamed tests/c/dbi_*.c -> dbt_*.c;
#   the parent corpus still names the pre-rename paths.  Recoverable.
# 0c6cda68 "Launch timed progress app directly in backend tests" deleted
#   tests/e2e/applications/timed-progress-bar.sh; the manifest now runs
#   ./examples/timed-progress-bar.py directly.  Recoverable.
# The 30 performance/* sources have NEVER been on hermit main -- they exist
#   only on unmerged branches (origin/codex/long-running-multibackend-perf-tests,
#   origin/tests/restore-fullcorpus-performance-fixtures).  NOT recoverable:
#   recorded as NO-RESULT source-absent-from-main.

TIMED_PROGRESS_REPAIR = ("applications/timed-progress-bar", "examples/timed-progress-bar.py")

LANE_FLAGS = {"portable": ["--no-virtualize-cpuid", "--max-timeslice=disabled"]}


def _children(pid: int) -> list[int]:
    out = []
    try:
        for t in os.listdir(f"/proc/{pid}/task"):
            try:
                out += [int(x) for x in
                        open(f"/proc/{pid}/task/{t}/children").read().split()]
            except OSError:
                pass
    except OSError:
        pass
    return out


def _tree(pid: int) -> list[int]:
    """Full descendant list, deepest last.  Walk it BEFORE killing anything:
    hermit re-sessions its sandbox container, so the descendants leave the
    process group and `killpg` alone leaves them alive (measured: 41 orphaned
    `hermit` roots survived a killpg-only timeout path).  While the parent is
    still alive the ppid chain is intact, so the tree walk reaches them."""
    seen, stack = [], [pid]
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.append(p)
        stack += _children(p)
    return seen


def run_boxed(argv, stdout, stderr, timeout, env):
    """Run in its OWN process group and, on timeout, kill the WHOLE group.

    subprocess.run(timeout=) kills only the direct child.  Hermit spawns a
    supervisor plus guests (and drrun/QEMU under DBT), so a timed-out cell
    left 36 wedged `hermit` roots plus 16 descendants alive on this box during
    the first attempt at this sweep -- they then burned CPU and caused further
    timeouts in later cells, corrupting the measurement into false reds.
    Signalling the negative PGID kills only processes this call started
    (dev-hermit Hard Invariant 15: never a broad or pattern-matched kill).
    """
    proc = subprocess.Popen(argv, stdout=stdout, stderr=stderr, env=env,
                            start_new_session=True)
    try:
        return proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        # Enumerate the descendants FIRST (see _tree), then signal the whole
        # tree deepest-first, then the process group as a backstop.  Only
        # processes this call started are ever signalled.
        tree = _tree(proc.pid)
        for sig in (15, 9):
            for p in reversed(tree):
                try:
                    os.kill(p, sig)
                except OSError:
                    pass
            try:
                os.killpg(proc.pid, sig)
            except OSError:
                pass
            try:
                proc.wait(timeout=10)
                break
            except subprocess.TimeoutExpired:
                continue
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        return 124


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    try:
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 16), b""):
                h.update(chunk)
    except FileNotFoundError:
        return ""
    return h.hexdigest()


def cell_env_pairs(cell_dir: Path) -> list[str]:
    """The product's own per-cell guest environment contract.

    scripts/manifest-to-commands.rs pins
        env LC_ALL=C TZ=UTC HOME=$cell/home XDG_CONFIG_HOME=$cell/xdg-config
            E2E_TMPDIR=$cell/tmp E2E_FIXTURE_DIR=$cell/fixtures
    around every generated Hermit command, and relies on the default
    --base-env host to pass them through.  The parent full-corpus collector
    instead uses `--base-env minimal -e LC_ALL=C -e TZ=UTC`, which keeps the
    guest stack reproducible but DROPS the four per-cell variables.  Measured
    at 2b38d8e6: that omission turned four shell cells into false reds on
    EVERY backend ("E2E_TMPDIR: unbound variable",
    "E2E_FIXTURE_DIR: unbound variable").  Keep the deterministic minimal base
    and pin the four variables explicitly.
    """
    return ["-e", f"HOME={cell_dir}/home",
            "-e", f"XDG_CONFIG_HOME={cell_dir}/xdg-config",
            "-e", f"E2E_TMPDIR={cell_dir}/tmp",
            "-e", f"E2E_FIXTURE_DIR={cell_dir}/fixtures"]


def build_argv(binary: str, backend: str, lane: str, verify: int, strict_verify: bool,
               json_path: Path | None, cell_dir: Path) -> list[str]:
    """The single place every measured Hermit command line is constructed."""
    argv = [binary]
    if backend != "ptrace":
        argv += ["--backend", backend]
    argv += ["run", "--strict"]
    if verify:
        argv += ["--verify"]
        if strict_verify:
            argv += ["--verify-strict"]
        if json_path is not None:
            argv += ["--verify-json", str(json_path)]
    argv += LANE_FLAGS.get(lane, [])
    argv += ["--base-env", "minimal", "-e", "LC_ALL=C", "-e", "TZ=UTC"]
    argv += cell_env_pairs(cell_dir)
    argv += ["--"]
    return argv


class Cell:
    __slots__ = ("cid", "lane", "argv", "workdir", "note", "absent", "kind")

    def __init__(self, cid, lane, argv, workdir, note="", absent="", kind="c"):
        self.cid = cid
        self.lane = lane
        self.argv = argv          # guest argv (program + args), absolute
        self.workdir = workdir    # per-cell scratch dir
        self.note = note
        self.absent = absent      # non-empty => NO-RESULT reason
        self.kind = kind


def prepare_cell_dirs(cell_dir: Path) -> None:
    for sub in ("home", "xdg-config", "tmp", "fixtures", "captures"):
        (cell_dir / sub).mkdir(parents=True, exist_ok=True)


def run_prepare_step(cell_dir: Path, argv: list[str]) -> str:
    """Run a shell cell's `--prepare` phase on the HOST, exactly as
    scripts/manifest-to-commands.rs does before the `--run` phase.  Several
    guests build their fixtures there; without it they fail inside Hermit and
    look like backend reds."""
    if not argv or not argv[0].endswith(".sh") or "--run" not in argv:
        return ""
    env = dict(os.environ, LC_ALL="C", TZ="UTC",
               HOME=f"{cell_dir}/home", XDG_CONFIG_HOME=f"{cell_dir}/xdg-config",
               E2E_TMPDIR=f"{cell_dir}/tmp", E2E_FIXTURE_DIR=f"{cell_dir}/fixtures")
    res = subprocess.run([argv[0], "--prepare"], env=env, capture_output=True,
                         text=True, timeout=600)
    (cell_dir / "prepare.log").write_text(
        f"rc={res.returncode}\n--stdout--\n{res.stdout}\n--stderr--\n{res.stderr}")
    return "" if res.returncode == 0 else f"prepare-failed-rc{res.returncode}"


def load_corpus(hroot: Path, corpus_dir: Path, build_dir: Path, gargs: dict) -> list[Cell]:
    cells: list[Cell] = []

    # ---- compiled C guests -------------------------------------------------
    with open(corpus_dir / "corpus-c.tsv") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            parts = line.split("|")
            cid, prog, cflags, extra, lane = parts[0], parts[1], parts[2], parts[3], parts[4]
            note = ""
            src = hroot / prog
            if not src.exists():
                base = os.path.basename(prog)
                if base.startswith("dbi_"):
                    alt = hroot / os.path.dirname(prog) / ("dbt_" + base[4:])
                    if alt.exists():
                        src = alt
                        note = "corpus-repair:dbi->dbt-rename(e565b1ab)"
            key = cid.replace("/", "_")
            cell_dir = build_dir / key
            cell_dir.mkdir(parents=True, exist_ok=True)
            prepare_cell_dirs(cell_dir)
            guest = cell_dir / "guest"
            if not src.exists():
                cells.append(Cell(cid, lane, [], cell_dir,
                                  note="source-absent-from-main",
                                  absent=f"source-absent-from-main:{prog}", kind="c"))
                continue
            if not guest.exists():
                cc = ["cc", "-std=c11", "-O2", "-g", "-Wall", "-Wextra", "-Werror"]
                cc += shlex.split(cflags)
                cc += [str(src)]
                cc += [str(hroot / e) for e in shlex.split(extra)]
                cc += ["-o", str(guest)]
                res = subprocess.run(cc, capture_output=True, text=True)
                if res.returncode != 0:
                    (cell_dir / "cc.err").write_text(res.stderr)
                    cells.append(Cell(cid, lane, [], cell_dir, note=note,
                                      absent=f"compile-failed:{src.name}", kind="c"))
                    continue
            argv = [str(guest)] + gargs.get((cid, "__reference"), [])
            cells.append(Cell(cid, lane, argv, cell_dir, note=note, kind="c"))

    # ---- shell / interpreter guests ---------------------------------------
    with open(corpus_dir / "corpus-nonc.tsv") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            cid, lane, cmd = line.split("|", 2)
            note = ""
            if cid == TIMED_PROGRESS_REPAIR[0]:
                cmd = "HERMITROOT/" + TIMED_PROGRESS_REPAIR[1]
                note = "corpus-repair:wrapper-script-deleted(0c6cda68);manifest runs the .py directly"
            cmd = cmd.replace("HERMITROOT", str(hroot))
            argv = shlex.split(cmd)
            key = cid.replace("/", "_")
            cell_dir = build_dir / ("nonc_" + key)
            cell_dir.mkdir(parents=True, exist_ok=True)
            prepare_cell_dirs(cell_dir)
            if not Path(argv[0]).exists():
                cells.append(Cell(cid, lane, [], cell_dir, note=note,
                                  absent=f"source-absent-from-main:{argv[0]}", kind="nonc"))
                continue
            prep = run_prepare_step(cell_dir, argv)
            if prep:
                cells.append(Cell(cid, lane, [], cell_dir, note=note,
                                  absent=prep, kind="nonc"))
                continue
            cells.append(Cell(cid, lane, argv, cell_dir, note=note, kind="nonc"))
    return cells


def read_guest_args(hroot: Path, out_dir: Path) -> dict:
    """Declared per-cell guest arguments, from the product's own manifest reader."""
    out_dir.mkdir(parents=True, exist_ok=True)
    res = subprocess.run(["./scripts/manifest-to-commands.rs", "--guest-args"],
                         cwd=str(hroot), capture_output=True, text=True)
    (out_dir / "guest-args.tsv").write_text(res.stdout)
    (out_dir / "guest-args.err").write_text(res.stderr)
    if res.returncode != 0:
        print("FATAL: cannot read declared guest arguments; refusing to sweep "
              "(argument-taking guests would print usage banners and look red).",
              file=sys.stderr)
        print(res.stderr[-3000:], file=sys.stderr)
        sys.exit(2)
    table: dict = {}
    for line in res.stdout.splitlines():
        if not line.strip():
            continue
        fields = line.split("\t")
        if len(fields) < 3:
            continue
        cid, mode, backend, rest = fields[0], fields[1], fields[2], fields[3:]
        if mode != "verify":
            continue
        table[(cid, backend)] = [f for f in rest if f != ""]
        if backend == "ptrace":
            table[(cid, "__reference")] = [f for f in rest if f != ""]
    return table


def classify(backend, verify_rc, verdict, run_out, run_err, verify_err, tmo):
    """-> (verdict_class, outcome, reason, assurance)

    `assurance` names the level the cell ACTUALLY reached, per hermit
    AGENTS.md "Determinism And Assurance Levels":
      L2-canonical    verified AND bitwise_parity (BitwiseInfoV1 INFO policy)
      L1-output-only  verified but bitwise_parity false.  This is KVM's
                      declared structural fallback -- it compares only exit
                      status/stdout/stderr and has no internal log comparison,
                      so it CANNOT reach L2 today.  A pass here is a real pass
                      of the check that ran and must never be reported as L2.
    """
    blob = (run_out + "\n" + run_err).lower()
    if verify_rc == 124:
        return ("NO-RESULT", "timeout", f"{backend}-verify-timeout-{tmo}s", "")
    for ln in (run_out + "\n" + run_err).splitlines():
        if ln.strip().lower().startswith("usage:"):
            return ("NO-RESULT", "usage",
                    f"{backend}-harness-error-usage-banner-exit{verify_rc}", "")
    low_verr = verify_err.lower()
    if "invalid value" in low_verr and "--backend" in low_verr:
        return ("NO-RESULT", "backend-unavailable",
                f"{backend}-not-compiled-in-exit{verify_rc}", "")
    if verdict is None:
        return ("NO-RESULT", "no-verdict-json",
                f"{backend}-no-verify-json-exit{verify_rc}", "")

    # MEASURED at 2b38d8e6: the DBT backend runs its OWN double-run comparison
    # in hermit-cli/src/bin/hermit/backends.rs (exit status + stdout + Detcore
    # observable summary + guest-memory hash) and never populates
    # --verify-json, which therefore always reports verdict="no_result".
    # Reading `verified` here would report every DBT cell as a divergence;
    # reading rc alone would launder no_result into a green.  Use DBT's own
    # channel and label the weaker assurance -- DBT compares no INFO log, so
    # it cannot reach L2 either.
    if verdict.get("verdict") == "no_result":
        if backend == "dbt":
            if verify_rc == 0 and "success: deterministic" in verify_err.lower():
                return ("PASS", "pass-dbt-selfreport",
                        "dbt--verify-json-always-no_result(product-gap);"
                        "verdict from DBT exit-status+stdout+summary+memhash path",
                        "L1-dbt-summary+memhash")
            if "enotsupp" in low_verr or "operation is not supported" in low_verr:
                return ("FAIL", "unsupported",
                        f"dbt-refused-ENOTSUPP-exit{verify_rc}", "")
            if verify_rc == 0:
                return ("NO-RESULT", "no-verdict-json",
                        "dbt-exit0-without-determinism-marker", "")
            return ("FAIL", "diverge", f"dbt-verify-failed-exit{verify_rc}", "")
        # Non-DBT `no_result` with a NON-ZERO exit is a real red, not a
        # no-result: hermit refuses the second run when the FIRST run does not
        # satisfy --verify-allow, so the guest failed under this backend.
        # MEASURED examples at 2b38d8e6: sabre backend-parity-c/cpuid-probe
        # ("unexpected CPUID identity"), liteinst backend-parity-c/
        # pthread-lifecycle ("-524 ENOTSUPP").
        if verify_rc != 0:
            if "enotsupp" in low_verr or "operation is not supported" in low_verr:
                return ("FAIL", "unsupported",
                        f"{backend}-refused-ENOTSUPP-first-run-exit{verify_rc}", "")
            if "first run" in low_verr:
                return ("FAIL", "guest-fail-first-run",
                        f"{backend}-first-run-errored-exit{verify_rc}", "")
            return ("FAIL", "error", f"{backend}-verify-errored-exit{verify_rc}", "")
        return ("NO-RESULT", "no-verdict-json",
                f"{backend}-exit0-without-verdict", "")

    verified = bool(verdict.get("verified"))
    bitwise = bool(verdict.get("bitwise_parity"))
    strictness = (verdict.get("comparison", {}) or {}).get("strictness")
    gexit = verdict.get("guest_exit_code")
    gsig = verdict.get("guest_signal")
    if verify_rc == 0 and verified and bitwise:
        return ("PASS", "pass", "", "L2-canonical")
    if verify_rc == 0 and verified and not bitwise:
        return ("PASS", "pass-output-only",
                f"{backend}-verified-without-bitwise_parity(policy={strictness})",
                "L1-output-only")
    if "enotsupp" in low_verr or "operation is not supported" in low_verr \
            or "enotsupp" in blob:
        return ("FAIL", "unsupported", f"{backend}-refused-ENOTSUPP-exit{verify_rc}", "")
    if not verified:
        return ("FAIL", "diverge", f"{backend}-diverged-exit{verify_rc}", "")
    # verified true but the process still failed: the guest itself failed
    # (deterministically) under this backend.
    return ("FAIL", "guest-fail",
            f"{backend}-guest-exit{gexit}-signal{gsig}-exit{verify_rc}", "")


def measure(binary, backend, cell: Cell, tmo_run, tmo_verify, strict_verify, tag):
    cid = cell.cid
    cdir = cell.workdir
    env = dict(os.environ, LC_ALL="C", TZ="UTC")
    prefix = f"{backend}{tag}"

    run_argv = build_argv(binary, backend, cell.lane, 0, False, None, cdir)
    verify_json = cdir / f"{prefix}.verify.json"
    if verify_json.exists():
        verify_json.unlink()
    verify_argv = build_argv(binary, backend, cell.lane, 1, strict_verify, verify_json, cdir)

    # (1) plain --strict run: the real guest stdout (parity material).
    #     --verify does an internal double-run and emits no guest stdout.
    out_path = cdir / f"{prefix}.out"
    err_path = cdir / f"{prefix}.err"
    with open(out_path, "wb") as so, open(err_path, "wb") as se:
        run_rc = run_boxed(run_argv + cell.argv, so, se, tmo_run, env)

    # (2) --verify [--verify-strict] --verify-json: the determinism verdict.
    t0 = time.time()
    verr_path = cdir / f"{prefix}v.err"
    with open(cdir / f"{prefix}v.out", "wb") as so, open(verr_path, "wb") as se:
        verify_rc = run_boxed(verify_argv + cell.argv, so, se, tmo_verify, env)
    dur_ms = int((time.time() - t0) * 1000)

    verdict = None
    if verify_json.exists():
        try:
            verdict = json.loads(verify_json.read_text().strip().splitlines()[-1])
        except Exception:
            verdict = None

    def head(p, n=6000):
        try:
            return p.read_text(errors="replace")[:n]
        except Exception:
            return ""

    vclass, outcome, reason, assurance = classify(backend, verify_rc, verdict,
                                                  head(out_path), head(err_path),
                                                  head(verr_path), tmo_verify)

    comp = (verdict or {}).get("comparison", {}) or {}
    return {
        "cell_id": cid,
        "backend": backend,
        "lane": cell.lane,
        "verdict": vclass,
        "assurance": assurance,
        "outcome": outcome,
        "reason": reason,
        "verify_exit": verify_rc,
        "run_exit": run_rc,
        "verified": (verdict or {}).get("verified", ""),
        "bitwise_parity": (verdict or {}).get("bitwise_parity", ""),
        "strictness": comp.get("strictness", ""),
        "compare_logs": comp.get("compare_logs", ""),
        "compared_log_messages": (((verdict or {}).get("compared_log_messages") or {})
                                  .get("left", "")),
        "guest_exit_code": (verdict or {}).get("guest_exit_code", ""),
        "guest_signal": (verdict or {}).get("guest_signal", ""),
        "duration_ms": dur_ms,
        "out_sha256": sha256_file(out_path),
        "population_note": cell.note,
        "repro_cmd": " ".join(shlex.quote(a) for a in (verify_argv + cell.argv)),
        "stderr_tail": head(verr_path, 200000).replace("\n", " \\n ")[-500:],
    }


FIELDS = ["cell_id", "backend", "lane", "verdict", "assurance", "outcome", "reason", "tier",
          "verify_exit", "run_exit", "verified", "bitwise_parity", "strictness",
          "compare_logs", "compared_log_messages",
          "guest_exit_code", "guest_signal", "stdout_parity_vs_ptrace",
          "out_sha256", "ref_sha256", "duration_ms", "hermit_sha", "reverie_sha",
          "population_note", "repro_cmd", "stderr_tail"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hroot", required=True)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--corpus", default="/home/newton/work/dev-hermit/compat-envelope/corpus")
    ap.add_argument("--build-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--backends", default="ptrace,kvm,dbt,sabre,liteinst")
    ap.add_argument("--par", type=int, default=24)
    ap.add_argument("--tmo-run", type=int, default=90)
    ap.add_argument("--tmo-verify", type=int, default=120)
    ap.add_argument("--hermit-sha", default="")
    ap.add_argument("--reverie-sha", default="")
    args = ap.parse_args()

    hroot = Path(args.hroot).resolve()
    build_dir = Path(args.build_dir).resolve()
    build_dir.mkdir(parents=True, exist_ok=True)
    out_path = Path(args.out).resolve()
    out_path.parent.mkdir(parents=True, exist_ok=True)

    gargs = read_guest_args(hroot, build_dir / "meta")
    print(f"== declared guest-arg vectors (verify mode): {len(gargs)} ==", flush=True)

    cells = load_corpus(hroot, Path(args.corpus), build_dir, gargs)
    runnable = [c for c in cells if not c.absent]
    absent = [c for c in cells if c.absent]
    print(f"== population: {len(cells)} cells; runnable {len(runnable)}; "
          f"no-result(absent/uncompilable) {len(absent)} ==", flush=True)

    backends = [b for b in args.backends.split(",") if b]
    rows: list[dict] = []

    # ptrace first: it writes the stdout parity reference.
    if "ptrace" in backends:
        backends = ["ptrace"] + [b for b in backends if b != "ptrace"]

    ref_hash: dict[str, str] = {}

    for backend in backends:
        # Backend staging probe.  An unstaged backend "fails" in ~100ms with
        # zero execution; that is a NO-RESULT for every cell, not 205 reds.
        probe = subprocess.run([args.bin] + ([] if backend == "ptrace" else ["--backend", backend])
                               + ["run", "--", "/bin/true"],
                               capture_output=True, text=True, timeout=120)
        staged = probe.returncode == 0
        print(f"== backend {backend}: staging probe rc={probe.returncode} "
              f"({'STAGED' if staged else 'NOT STAGED -> all cells NO-RESULT'}) ==", flush=True)
        if not staged:
            (build_dir / "meta" / f"probe-{backend}.err").write_text(probe.stderr)
        for c in absent:
            rows.append({"cell_id": c.cid, "backend": backend, "lane": c.lane,
                         "verdict": "NO-RESULT", "outcome": "absent", "reason": c.absent,
                         "population_note": c.note, "hermit_sha": args.hermit_sha,
                         "reverie_sha": args.reverie_sha, "repro_cmd": "", "tier": ""})
        if not staged:
            tail = (probe.stderr or probe.stdout)[-300:].replace("\n", " \\n ")
            for c in runnable:
                rows.append({"cell_id": c.cid, "backend": backend, "lane": c.lane,
                             "verdict": "NO-RESULT", "outcome": "backend-unstaged",
                             "reason": f"{backend}-probe-rc{probe.returncode}",
                             "population_note": c.note, "hermit_sha": args.hermit_sha,
                             "reverie_sha": args.reverie_sha, "repro_cmd": "",
                             "tier": "", "stderr_tail": tail})
            continue

        t0 = time.time()
        got: list[dict] = []
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.par) as ex:
            futs = {ex.submit(measure, args.bin, backend, c, args.tmo_run,
                              args.tmo_verify, True, ""): c for c in runnable}
            for i, f in enumerate(concurrent.futures.as_completed(futs), 1):
                got.append(f.result())
                if i % 25 == 0:
                    print(f"   {backend}: {i}/{len(runnable)}", flush=True)
        print(f"== backend {backend}: canonical pass done in {time.time()-t0:.0f}s ==", flush=True)

        by_id = {c.cid: c for c in runnable}

        # Repair pass: a timeout under a parallel storm is a load artifact,
        # not a verdict.  Re-run every timed-out cell serially.
        timed_out = [r for r in got if r["outcome"] == "timeout"]
        if timed_out:
            print(f"   repair: re-running {len(timed_out)} timed-out {backend} cell(s) serially",
                  flush=True)
            fixed = {}
            for r in timed_out:
                fixed[r["cell_id"]] = measure(args.bin, backend, by_id[r["cell_id"]],
                                              args.tmo_run, args.tmo_verify, True, "")
            got = [fixed.get(r["cell_id"], r) if r["outcome"] == "timeout" else r for r in got]
            still = sum(1 for r in got if r["outcome"] == "timeout")
            print(f"   repair: {len(timed_out)-still} recovered, {still} still timing out "
                  f"(recorded NO-RESULT)", flush=True)

        # Tier pass: re-measure every FAIL under plain (stripped) --verify so a
        # red can be attributed to the weak or only the bitwise comparison.
        reds = [r for r in got if r["verdict"] == "FAIL"]
        if reds:
            print(f"   tier: re-running {len(reds)} {backend} red cell(s) under plain --verify",
                  flush=True)
            strip: dict[str, dict] = {}
            with concurrent.futures.ThreadPoolExecutor(max_workers=args.par) as ex:
                futs = {ex.submit(measure, args.bin, backend, by_id[r["cell_id"]],
                                  args.tmo_run, args.tmo_verify, False, "-strip"): r["cell_id"]
                        for r in reds}
                for f in concurrent.futures.as_completed(futs):
                    rr = f.result()
                    strip[rr["cell_id"]] = rr
            for r in got:
                if r["verdict"] != "FAIL":
                    r["tier"] = "canonical-green" if r["verdict"] == "PASS" else ""
                    continue
                s = strip.get(r["cell_id"])
                if s is None:
                    r["tier"] = "red-tier-unknown"
                elif s["verdict"] == "PASS":
                    r["tier"] = "red-canonical-only"
                elif s["verdict"] == "NO-RESULT":
                    r["tier"] = "red-stripped-noresult"
                else:
                    r["tier"] = "red-stripped"
                    r["reason"] += f";stripped-also:{s['outcome']}"
        for r in got:
            r.setdefault("tier", "canonical-green" if r["verdict"] == "PASS" else "")

        # stdout parity vs the ptrace reference
        for r in got:
            if backend == "ptrace":
                ref_hash[r["cell_id"]] = r["out_sha256"] if r["run_exit"] == 0 else ""
            ref = ref_hash.get(r["cell_id"], "")
            r["ref_sha256"] = ref
            if not ref:
                r["stdout_parity_vs_ptrace"] = ""
            else:
                r["stdout_parity_vs_ptrace"] = "1" if r["out_sha256"] == ref else "0"
            r["hermit_sha"] = args.hermit_sha
            r["reverie_sha"] = args.reverie_sha
        rows.extend(got)

        # Write incrementally so a lost agent does not lose measured backends.
        with open(out_path, "w", newline="") as fh:
            w = csv.DictWriter(fh, fieldnames=FIELDS, extrasaction="ignore")
            w.writeheader()
            for r in rows:
                w.writerow(r)
        p = sum(1 for r in rows if r["backend"] == backend and r["verdict"] == "PASS")
        f_ = sum(1 for r in rows if r["backend"] == backend and r["verdict"] == "FAIL")
        n = sum(1 for r in rows if r["backend"] == backend and r["verdict"] == "NO-RESULT")
        print(f"== {backend}: PASS {p} | FAIL {f_} | NO-RESULT {n} | denominator {p+f_+n} ==",
              flush=True)

    print(f"== scorecard written: {out_path} ==", flush=True)


if __name__ == "__main__":
    main()
