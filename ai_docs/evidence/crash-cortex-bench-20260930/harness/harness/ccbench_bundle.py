#!/usr/bin/env python3
"""Build a frozen, offline evidence bundle for one Crash Cortex benchmark case.

The bundle reproduces exactly the pre-collected files that kernel_investigator's
collector writes to /tmp/kinv-logs/<MID>/ before the agent starts
(collector._save_collection_data + mcp_collector.mcp_data_to_drgn), but sources
them from the long-retention Hive mirrors so old cases can still be frozen:

  infrastructure.scuba_kernel_logview  -> crash row (dmesg, backtrace, all_cpus, ...)
  infrastructure.scuba_netcons         -> host_timeline.txt (+-300 s, <=200 lines)
  infrastructure.scuba_kernel_logview  -> pop.csv (MID population up to the cutoff)

Nothing from the original investigation (report, patch, triage verdict, post)
is ever copied in. Ground truth lives in a separate sealed file.

Usage:
  ccbench_bundle.py --mid M --host H --time T --out DIR [--cutoff UNIX] [--pop-days 30]
"""

import argparse
import datetime as dt
import hashlib
import json
import os
import subprocess
import sys
import time

BUNDLE_FORMAT = "ccbench-bundle/v1"


def presto(sql: str) -> list:
    r = subprocess.run(
        ["meta", "presto.query", "execute", "--namespace", "infrastructure",
         "--sql", sql, "--output", "json"],
        capture_output=True, text=True, timeout=900,
    )
    if r.returncode != 0:
        raise RuntimeError(f"presto failed: {r.stderr[-800:]}")
    out = r.stdout.strip()
    return json.loads(out) if out else []


def ds(ts: int) -> str:
    return dt.datetime.fromtimestamp(ts, dt.timezone.utc).strftime("%Y-%m-%d")


def q(s: str) -> str:
    return "'" + "".join(c for c in s if c.isalnum() or c in "._-:") + "'"


def s(row: dict, key: str) -> str:
    v = row.get(key)
    return "" if v is None or v == "null" else str(v)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mid", required=True)
    ap.add_argument("--host", required=True)
    ap.add_argument("--time", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--cutoff", type=int, default=0,
                    help="knowledge cutoff (unix); default = crash time")
    ap.add_argument("--pop-days", type=int, default=30)
    a = ap.parse_args()
    cutoff = a.cutoff or a.time
    ev = os.path.join(a.out, "evidence")
    os.makedirs(ev, exist_ok=True)
    queries = []

    # 1. The pinned crash row. The Hive partition is the UTC date; probe +-1 day.
    days = sorted({ds(a.time - 86400), ds(a.time), ds(a.time + 86400)})
    sql = ("SELECT * FROM scuba_kernel_logview WHERE ds IN (" + ",".join(q(d) for d in days) +
           f") AND hostname = {q(a.host)} AND time = {int(a.time)} AND mid = {q(a.mid)} LIMIT 1")
    queries.append(sql)
    rows = presto(sql)
    if not rows:
        print(f"NO CRASH ROW for {a.mid} {a.host} {a.time}", file=sys.stderr)
        return 2
    row = rows[0]

    # 2. Population of the MID in the window before the cutoff (for triage context).
    lo = cutoff - a.pop_days * 86400
    sql_pop = ("SELECT hostname, time, kernel, lsst, model_name, crashing_core, crashing_cpu, "
               "crashing_task, crashing_kmod, workqueue, exception_type, uptime, previous_panics "
               f"FROM scuba_kernel_logview WHERE ds BETWEEN {q(ds(lo))} AND {q(ds(cutoff))} "
               f"AND mid = {q(a.mid)} AND time <= {int(cutoff)} ORDER BY time DESC LIMIT 2000")
    queries.append(sql_pop)
    pop = presto(sql_pop)

    # 3. netcons around the crash (same window/cap as mcp_collector._query_host_timeline).
    sql_nc = ("SELECT time, seq, msg FROM scuba_netcons WHERE ds IN (" + ",".join(q(d) for d in days) +
              f") AND hostname = {q(a.host)} AND time BETWEEN {a.time - 300} AND {a.time + 300} "
              "ORDER BY time, seq LIMIT 200")
    queries.append(sql_nc)
    try:
        nc = presto(sql_nc)
    except Exception as e:  # netcons mirror retention is shorter; record, do not fail
        nc, nc_err = [], str(e)[:300]
    else:
        nc_err = ""

    # ---- Write the files exactly as kernel_investigator's collector would ----
    hosts = []
    for p in pop:
        if p.get("hostname") and p["hostname"] not in hosts:
            hosts.append(p["hostname"])
    collection = {
        "mid": a.mid,
        "vmcore_source": "kernel_mcp",
        "hostname_investigated": a.host,
        "crash_time": a.time,
        "vmcore_host": a.host,
        "vmcore_path": "kernel-mcp",
        "category_key": s(row, "category_key"),
        "kernel": s(row, "kernel_major") or s(row, "kernel"),
        "lsst": s(row, "lsst"),
        "hostnames_sample": hosts[:10],
    }
    files = {"collection.json": json.dumps(collection, indent=2)}
    if s(row, "backtrace"):
        files["backtrace.txt"] = s(row, "backtrace")
    if s(row, "dmesg"):
        files["dmesg.txt"] = s(row, "dmesg")

    task = []
    if s(row, "crashing_task"):
        task.append(f"Task: {s(row, 'crashing_task')}")
    if s(row, "crashing_cpu"):
        task.append(f"CPU: {s(row, 'crashing_cpu')} (core {s(row, 'crashing_core')})")
    if s(row, "crashing_kmod"):
        task.append(f"Module: {s(row, 'crashing_kmod')}")
    if s(row, "workqueue"):
        task.append(f"Workqueue: {s(row, 'workqueue')}")
    mods = [f"Crashing module: {s(row, 'crashing_kmod')}"] if s(row, "crashing_kmod") else []
    drgn = ""
    for title, body in (("Stack Trace", s(row, "backtrace")), ("All CPUs", s(row, "all_cpus")),
                        ("Modules", "\n".join(mods)), ("Crashing Task", "\n".join(task)),
                        ("Memory Info", s(row, "meminfo"))):
        if body:
            drgn += f"=== {title} ===\n{body}\n\n"
    if drgn:
        files["drgn_output.txt"] = drgn

    extra = {}
    if s(row, "kdump_dmesg"):
        extra["kdump_dmesg"] = s(row, "kdump_dmesg")[:8000]
    if s(row, "makedumpfile_log"):
        extra["makedumpfile_log"] = s(row, "makedumpfile_log")[:2000]
    if s(row, "mce_bank"):
        extra.update(mce_bank=s(row, "mce_bank"), mce_status=s(row, "mce_status"),
                     mcg_status=s(row, "mcg_status"))
    if s(row, "bpf_users"):
        extra["bpf_users"] = s(row, "bpf_users")[:3000]
    if s(row, "tw_jobs"):
        extra["tw_jobs"] = s(row, "tw_jobs")
    timeline = "\n".join(f"[{r.get('time', '')}] {r.get('msg', '')}" for r in nc)
    if timeline:
        extra["host_timeline"] = timeline[:5000]
    if s(row, "bt_noinline"):
        extra["bt_noinline"] = s(row, "bt_noinline")
    if extra:
        files["bandicoot.json"] = json.dumps(extra, indent=2)
    if s(row, "kdump_dmesg"):
        files["kdump_dmesg.txt"] = s(row, "kdump_dmesg")
    if timeline:
        files["host_timeline.txt"] = timeline

    for name, body in files.items():
        with open(os.path.join(ev, name), "w") as f:
            f.write(body)

    # Prompt-side context the renderer needs (never includes investigation output).
    models = {}
    for p in pop:
        m = p.get("model_name") or "N/A"
        models[m] = models.get(m, 0) + 1
    tot = sum(models.values()) or 1
    context = {
        "mid": a.mid,
        "category_key": s(row, "category_key"),
        "exception_type": s(row, "exception_type"),
        "kernel_major": s(row, "kernel_major"),
        "kernel_full": s(row, "kernel"),
        "lsst": s(row, "lsst"),
        "models": ", ".join(f"{k}={round(100 * v / tot)}%" for k, v in sorted(models.items(), key=lambda kv: -kv[1])[:3]),
        "hosts_affected": len(hosts),
        "host_sample": hosts[:3],
        "investigated_host": a.host,
        "crash_time": a.time,
        "panic": s(row, "panic"),
        "backtrace": s(row, "backtrace"),
        "population_rows": len(pop),
    }
    with open(os.path.join(a.out, "context.json"), "w") as f:
        json.dump(context, f, indent=2)
    with open(os.path.join(a.out, "crash_row.json"), "w") as f:
        json.dump(row, f)

    manifest = {
        "format": BUNDLE_FORMAT,
        "mid": a.mid,
        "pin": {"host": a.host, "time": a.time},
        "cutoff": cutoff,
        "kernel_full": s(row, "kernel"),
        "built_at": int(time.time()),
        "sources": {
            "crash_row": "hive:infrastructure.scuba_kernel_logview",
            "population": f"hive:infrastructure.scuba_kernel_logview mid=, {a.pop_days}d before cutoff",
            "netcons": "hive:infrastructure.scuba_netcons" + (f" (ERROR: {nc_err})" if nc_err else ""),
            "oob_console": "UNAVAILABLE (Hive mirror retention ~18h)",
            "kdump_all_cpus": "not frozen (Scuba-only); all_cpus column from crash row used",
            "vmcore": "not frozen (lives on the crash host; excluded by design)",
            "ods": "not frozen in v1",
        },
        "queries": queries,
        "files": {n: {"bytes": len(b.encode()), "sha256": hashlib.sha256(b.encode()).hexdigest()}
                  for n, b in sorted(files.items())},
        "population_rows": len(pop),
        "netcons_rows": len(nc),
    }
    with open(os.path.join(a.out, "MANIFEST.json"), "w") as f:
        json.dump(manifest, f, indent=2)
    print(json.dumps({k: manifest[k] for k in ("mid", "pin", "kernel_full", "population_rows", "netcons_rows")}))
    print("files:", {n: v["bytes"] for n, v in manifest["files"].items()})
    return 0


if __name__ == "__main__":
    sys.exit(main())
