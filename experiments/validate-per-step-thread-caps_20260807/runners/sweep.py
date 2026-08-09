#!/usr/bin/env python3
"""Per-step thread-count sweep: measure each DAG step's speedup curve.

For each (step, threads) it writes a ONE-STEP DAG file with the step's
--test-threads rewritten, runs it through scripts/validate.rs --dag-file (same
boxing, same engine, same accounting as a real run), and records elapsed_s,
cpu, effective_cores and PASS/FAIL from the runner's own --perf-dir CSV.
Correctness is recorded at every point: a cap justified by a FAIL at T>1 is a
correctness cap, not a conservative constant.
"""
import csv, json, os, re, subprocess, sys, time
from pathlib import Path

ROOT = Path("/home/newton/work/dev-hermit")
SLOT = ROOT / "worktrees/soltiming/hermit"
OUT = ROOT / "ignored/soltiming/sweep"
OUT.mkdir(parents=True, exist_ok=True)
DAG = json.loads((SLOT / "ci/dag/portable.json").read_text())
STEPS = {s["group"] + "." + s["job"]: s for s in DAG["steps"]}

def retime(cmd, t):
    if "--test-threads=" in cmd:
        return re.sub(r"--test-threads=\d+", f"--test-threads={t}", cmd)
    return None

def run(tag, threads, results):
    step = json.loads(json.dumps(STEPS[tag]))
    cmd = retime(step["cmd"], threads)
    if cmd is None:
        return
    step["cmd"] = cmd
    step.pop("deps", None)
    step["timeout"] = max(step.get("timeout", 600), 1800)
    one = {k: v for k, v in DAG.items() if k != "steps"}
    one["steps"] = [step]
    label = f"{tag}__t{threads}"
    dagf = OUT / f"{label}.json"
    dagf.write_text(json.dumps(one))
    perf = OUT / f"perf-{label}"
    env = dict(os.environ, CI_DAG_JOBS="1")
    t0 = time.time()
    p = subprocess.run(
        ["./scripts/validate.rs", "portable", "--dag-file", str(dagf),
         "--perf-dir", str(perf), "-k"],
        cwd=SLOT, env=env, capture_output=True, text=True, timeout=2400)
    wall = time.time() - t0
    row = {"step": tag, "threads": threads, "harness_wall_s": round(wall, 2),
           "rc": p.returncode}
    csvs = list(perf.glob("*.csv"))
    if csvs:
        r = list(csv.DictReader(open(csvs[0])))[0]
        cpu = float(r["cpu.usage_usec"]) / 1e6 if r.get("cpu.usage_usec") else None
        row.update(elapsed_s=float(r["elapsed_s"]), ok=r["ok"],
                   returncode=r["returncode"], cpu_s=cpu,
                   effective_cores=float(r["effective_cores"]),
                   peak_bytes=int(r["peak_bytes"]),
                   timed_out=r["timed_out"], load1_start=r["load1_start"])
    results.append(row)
    (OUT / "results.jsonl").open("a").write(json.dumps(row) + "\n")
    print(f"{tag:38s} t={threads:<3d} wall={row.get('elapsed_s','?')} "
          f"cores={row.get('effective_cores','?')} ok={row.get('ok','?')} rc={p.returncode}",
          flush=True)

if __name__ == "__main__":
    targets = [t for t, s in STEPS.items() if "--test-threads=" in s["cmd"]]
    threads = [1, 2, 4, 8, 16, 32]
    print(f"sweeping {len(targets)} steps x {len(threads)} thread points", flush=True)
    res = []
    for t in targets:
        for n in threads:
            try:
                run(t, n, res)
            except Exception as e:
                print(f"{t} t={n} EXC {e}", flush=True)
