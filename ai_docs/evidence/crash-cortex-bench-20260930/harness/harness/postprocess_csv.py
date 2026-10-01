#!/usr/bin/env python3
"""After select_cases.py select: normalize mechanism classes, flag the strict race subset,
and check every pin row is bundle-ready in Hive (dmesg + backtrace present)."""
import csv, datetime as dt, json, re, subprocess
P = "/home/newton/local/ccbench/deliverables/"
ENUM = {"data_race_or_missing_sync", "lock_ordering_deadlock", "lock_hold_too_long_or_starvation", "use_after_free_lifetime",
        "null_deref_or_missing_check", "refcount_or_leak", "accounting_or_invariant_violation", "livelock_or_feedback_loop",
        "resource_exhaustion", "config_or_tunable", "hardware_fault", "firmware_bug", "oot_driver_bug", "bpf_or_sched_ext", "other"}
def norm(m):
    s = m.lower()
    if m in ENUM: return m
    if re.search(r"use-after-free|uaf|double-free|double list_del|freed", s): return "use_after_free_lifetime"
    if re.search(r"race|toctou|memory-ordering", s): return "data_race_or_missing_sync"
    if re.search(r"null", s): return "null_deref_or_missing_check"
    if re.search(r"livelock|infinite loop|printk flood|warn/printk", s): return "livelock_or_feedback_loop"
    if re.search(r"lock contention|long lock hold|resched|soft lockup", s): return "lock_hold_too_long_or_starvation"
    if re.search(r"allocation", s): return "resource_exhaustion"
    if re.search(r"refcount", s): return "refcount_or_leak"
    if re.search(r"sched_class|state corruption|accounting|bug_on", s): return "accounting_or_invariant_violation"
    return "other"
def is_race(r):
    s = r["mechanism_class"].lower()
    return r["timing_dependent"] == "True" and (r["mechanism_class"] in ("data_race_or_missing_sync", "lock_ordering_deadlock", "use_after_free_lifetime")
                                               or bool(re.search(r"race|toctou|memory-ordering", s)))
def hive_sizes(rows):
    conds = []
    for r in rows:
        t = int(r["rep_time"]); d = dt.datetime.fromtimestamp(t, dt.timezone.utc)
        days = sorted({(d + dt.timedelta(days=k)).strftime("%Y-%m-%d") for k in (-1, 0, 1)})
        conds.append(f"(ds IN ({','.join(repr(x) for x in days)}) AND hostname='{r['rep_host']}' AND time={t} AND mid='{r['mid']}')")
    res = []
    for i in range(0, len(conds), 25):
        sql = ("SELECT mid, hostname, time, length(dmesg) dl, length(backtrace) bl, length(all_cpus) al, length(kdump_dmesg) kl "
               "FROM scuba_kernel_logview WHERE " + " OR ".join(conds[i:i + 25]))
        out = subprocess.run(["meta", "presto.query", "execute", "--namespace", "infrastructure", "--sql", sql, "--output", "json"],
                             capture_output=True, text=True, timeout=1800).stdout
        res += json.loads(out) if out.strip() else []
    return {(x["mid"], x["hostname"], int(x["time"])): x for x in res}
for fn in ("cases_100.csv", "cases_spares.csv"):
    rows = list(csv.DictReader(open(P + fn)))
    for r in rows:
        r["mechanism_detail"] = r["mechanism_class"]
        r["race_subset"] = is_race(r)
        r["mechanism_class"] = norm(r["mechanism_class"])
    got = hive_sizes(rows)
    for r in rows:
        x = got.get((r["mid"], r["rep_host"], int(r["rep_time"])))
        r["bundle_ready"] = bool(x and int(x["dl"] or 0) > 1000)
        r["pin_row_sizes"] = f"dmesg={x['dl']},bt={x['bl']},all_cpus={x['al']},kdump_dmesg={x['kl']}" if x else "MISSING"
        if x and int(x["bl"] or 0) < 50:
            r["pin_row_sizes"] += " (backtrace column near-empty; trace present in dmesg)"
    cols = [c for c in rows[0].keys() if c not in ("mechanism_detail", "race_subset")]
    i = cols.index("mechanism_class") + 1
    cols = cols[:i] + ["mechanism_detail"] + cols[i:]
    j = cols.index("timing_dependent") + 1
    cols = cols[:j] + ["race_subset"] + cols[j:]
    w = csv.DictWriter(open(P + fn, "w", newline=""), fieldnames=cols)
    w.writeheader()
    for r in rows:
        w.writerow(r)
    print(fn, len(rows), "race", sum(r["race_subset"] for r in rows), "timing", sum(r["timing_dependent"] == "True" for r in rows),
          "bundle_ready", sum(r["bundle_ready"] for r in rows))
