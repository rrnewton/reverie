#!/usr/bin/env python3
"""Normalize adjudicated + mined candidates, resolve pins against Hive, re-verify fixes
deterministically, and write the candidate pool for family clustering / selection.

  select_cases.py pool      -> data/pool.json   (all tier A/B candidates, pins resolved)
  select_cases.py select    -> deliverables/cases_100.csv (+ inputs csv) using data/families.json
"""
import collections
import csv
import datetime as dt
import json
import re
import subprocess
import sys

D = "/home/newton/local/ccbench/data"
OUT = "/home/newton/local/ccbench/deliverables"
MIRROR = "/data/users/newton/linux-full.git"


def git(*a):
    r = subprocess.run(["git", "--git-dir", MIRROR, *a], capture_output=True, text=True)
    return r.returncode, r.stdout.strip()


def presto(sql):
    r = subprocess.run(["meta", "presto.query", "execute", "--namespace", "infrastructure", "--sql", sql,
                        "--output", "json"], capture_output=True, text=True, timeout=1800)
    return json.loads(r.stdout) if r.stdout.strip() else []


def iso(t):
    return dt.datetime.fromtimestamp(int(t), dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ") if t else ""


def load_journal_results(path, prefixes):
    started, res = {}, []
    for line in open(path):
        e = json.loads(line)
        if e["type"] == "started":
            started[e["key"]] = e.get("label", "")
        elif e["type"] == "result" and isinstance(e.get("result"), dict):
            lab = started.get(e.get("key"), "")
            if any(lab.startswith(p) for p in prefixes):
                res.append((lab, e["result"]))
    return res


def fix_facts(sha, pin_kernel_sha, pin_time):
    rc, full = git("rev-parse", "--verify", "--quiet", f"{sha}^{{commit}}")
    if rc:
        return None
    _, meta = git("log", "-1", "--format=%an%x09%ae%x09%ct%x09%s", full)
    an, ae, ct, subj = (meta.split("\t") + [""] * 4)[:4]
    on = []
    for b in ("v6.9-fb", "v6.11-fb", "v6.13-fb", "v6.16-fb", "v6.19-fb", "v7.1-fb", "linus-upstream", "net-next-upstream"):
        if git("merge-base", "--is-ancestor", full, f"refs/heads/{b}")[0] == 0:
            on.append(b)
    in_crash = False
    if pin_kernel_sha:
        in_crash = git("merge-base", "--is-ancestor", full, pin_kernel_sha)[0] == 0
    return dict(sha=full[:12], full=full, author=an, email=ae, commit_time=int(ct or 0), subject=subj,
                landed_on=on, in_crash_kernel=in_crash, available_at_pin=bool(int(ct or 0) <= pin_time and on))


def pool():
    adj = json.load(open(f"{D}/adjudicated.json"))
    mined = json.load(open(f"{D}/mined_all.json"))
    cands = []
    for a in adj:
        if a.get("final_tier") not in ("A", "B"):
            continue
        cands.append(dict(source="crashcortex", mid=a["mid"], tier=a["final_tier"], root_cause=a["gt_root_cause"],
                          fatal=a["gt_fatal_mechanism"], fixes=[f["sha"] for f in a["gt_fix_commits"] if f["sha"]],
                          fix_rel={f["sha"]: f["relation"] for f in a["gt_fix_commits"]},
                          family_label=a["family_label"], subsystem=a["subsystem"], mechanism=a["mechanism_class"],
                          timing=a["timing_dependent"], visible=a["answer_visible_in_input"], oot=a["needs_oot_source"],
                          difficulty=a["difficulty"], fair=a["fair_test_rationale"], bot_was_right=a["bot_was_right"],
                          pin_host=a.get("pin_host", ""), pin_time=a.get("pin_time", 0),
                          evidence=a["gt_evidence"][:4], skeptic=(a.get("skeptic") or {}).get("problems", [])[:3]))
    for m in mined:
        sk = m.get("skeptic") or {}
        if not sk.get("upheld"):
            continue
        cands.append(dict(source="tree-mined" + (":crashfirst" if "crashfirst" in m.get("miner", "") else ":fixfirst"),
                          mid=m["mid"], tier="A", root_cause=m["gt_root_cause"], fatal="",
                          fixes=[m["fix"]["sha"]], fix_rel={m["fix"]["sha"]: m["fix"]["relation"]},
                          family_label=m["family_label"], subsystem=m["subsystem"], mechanism=m["mechanism_class"],
                          timing=m["timing_dependent"], visible=m["answer_visible_in_input"], oot=False,
                          difficulty="", fair=m["fair_test_rationale"], bot_was_right="n/a",
                          pin_host=sk.get("corrected_sample_host") or m["sample_host"],
                          pin_time=sk.get("corrected_sample_time") or m["sample_time"],
                          evidence=[m["signature_match_evidence"][:300], m.get("production_validation", "")[:300]],
                          skeptic=sk.get("problems", [])[:3]))
    # de-dup by MID (prefer crashcortex)
    seen, uniq = set(), []
    for c in cands:
        if c["mid"] in seen:
            continue
        seen.add(c["mid"])
        uniq.append(c)
    mids = [c["mid"] for c in uniq]
    rows = []
    for i in range(0, len(mids), 60):
        inl = ",".join(f"'{m}'" for m in mids[i:i + 60])
        rows += presto("SELECT mid, hostname, time, kernel, model_name, category_key FROM scuba_kernel_logview "
                       f"WHERE ds >= '2025-10-06' AND mid IN ({inl})")
    by = collections.defaultdict(list)
    for r in rows:
        r["time"] = int(r["time"])
        by[r["mid"]].append(r)
    out = []
    for c in uniq:
        rs = by.get(c["mid"], [])
        c["hive_rows"] = len(rs)
        c["n_hosts"] = len({r["hostname"] for r in rs})
        pin = None
        h = (c["pin_host"] or "").strip()
        if h and rs:
            hs = [r for r in rs if r["hostname"].startswith(h.replace(".facebook.com", ""))]
            if c["pin_time"]:
                hs.sort(key=lambda r: abs(r["time"] - int(c["pin_time"])))
            else:
                hs.sort(key=lambda r: -r["time"])
            if hs and (not c["pin_time"] or abs(hs[0]["time"] - int(c["pin_time"])) < 6 * 3600):
                pin = hs[0]
        if pin is None and rs:
            kc = collections.Counter(r["kernel"] for r in rs)
            modal = kc.most_common(1)[0][0]
            prod = [r for r in rs if r["kernel"] == modal and not re.match(r"^(devvm|devbig)", r["hostname"])] or \
                   [r for r in rs if r["kernel"] == modal]
            prod.sort(key=lambda r: -r["time"])
            pin = prod[0]
            c["pin_method"] = "modal-kernel latest production row (no investigated host in post)"
        else:
            c["pin_method"] = "investigated host from post, matched in Hive" if pin else "none"
        if not pin:
            c["pin_ok"] = False
            out.append(c)
            continue
        c.update(pin_ok=True, rep_host=pin["hostname"], rep_time=pin["time"], kernel_full=pin["kernel"],
                 model=pin.get("model_name", ""), category_key=pin.get("category_key", ""))
        m = re.search(r"_g([0-9a-f]{12})", pin["kernel"])
        ks = m.group(1) if m else ""
        rc, full = git("rev-parse", "--verify", "--quiet", f"{ks}^{{commit}}") if ks else (1, "")
        c["crash_commit"] = full if rc == 0 else ""
        ff = [fix_facts(s, c["crash_commit"], pin["time"]) for s in c["fixes"]]
        c["fix_facts"] = [f for f in ff if f]
        c["fix_verified"] = bool(c["fix_facts"]) and all(not f["in_crash_kernel"] for f in c["fix_facts"]) and \
            any(f["landed_on"] for f in c["fix_facts"])
        c["fix_available_at_cutoff"] = any(f["available_at_pin"] for f in c["fix_facts"])
        out.append(c)
    json.dump(out, open(f"{D}/pool.json", "w"), indent=1)
    ok = [c for c in out if c.get("pin_ok") and c.get("fix_verified") and c["visible"] != "high" and not c["oot"]]
    print("candidates", len(out), "pinned", sum(bool(c.get("pin_ok")) for c in out),
          "fix_verified", sum(bool(c.get("fix_verified")) for c in out), "eligible", len(ok),
          "timing", sum(c["timing"] for c in ok), collections.Counter(c["source"] for c in ok))


def select():
    pool_ = json.load(open(f"{D}/pool.json"))
    fam = json.load(open(f"{D}/families.json"))  # {mid: {"family_id":..., "family_name":...}}
    elig = [c for c in pool_ if c.get("pin_ok") and c.get("fix_verified") and c["visible"] != "high" and not c["oot"]]
    for c in elig:
        f = fam.get(c["mid"], {})
        c["family_id"], c["family_name"] = f.get("family_id", "F??"), f.get("family_name", c["family_label"])
    posts = collections.defaultdict(list)
    for q in json.load(open(f"{D}/extracted.json")):
        posts[q["mid"]].append(q)
    parsed = {r["post_id"]: r for r in json.load(open(f"{D}/posts_parsed.json"))}
    # subject -> author times over every ref a time-cut repo can expose (release + upstream branches)
    subj_at = collections.defaultdict(list)
    exposed = [f"refs/heads/{b}" for b in ("v6.9-fb", "v6.11-fb", "v6.13-fb", "v6.16-fb", "v6.19-fb", "v7.1-fb",
                                           "linus-upstream", "net-next-upstream", "main")]
    _, out = git("log", "--no-merges", "--since=2024-01-01", "--format=%at%x09%s", *exposed)
    for line in out.splitlines():
        at, _, sj = line.partition("\t")
        if at.isdigit():
            subj_at[sj.strip()].append(int(at))
    for c in elig:
        # derive-mode cutoff: 1 day before the earliest authoring of any GT fix variant (same subject, exposed refs)
        ts = []
        for f in c["fix_facts"]:
            rc, at = git("log", "-1", "--format=%at", f["full"])
            if at:
                ts.append(int(at))
            ts += subj_at.get(f["subject"].strip(), [])
        c["cutoff_derive"] = min([min(ts) - 86400] + [c["rep_time"]]) if ts else c["rep_time"]
        ps = posts.get(c["mid"], [])
        hs = [q["human_signal"] for q in ps if q["human_signal"]["kind"] != "none"]
        c["human_signal"] = "; ".join(f"{h['kind']} by {h['who']}: {h['quote'][:160]}" for h in hs)[:400]
        if ps:
            pr = parsed.get(ps[-1]["post_id"], {})
            c["post_url"], c["report_paste"] = pr.get("url", ""), pr.get("report_paste", "")
    # priority: tier A, then CrashCortex cases where the bot was wrong, then timing, then human-confirmed
    def pri(c):
        return (c["tier"] != "A", c["bot_was_right"] not in ("no", "partial"), not c["timing"], c["source"] != "crashcortex")
    elig.sort(key=pri)
    per = collections.Counter()
    chosen = []
    for c in elig:
        if per[c["family_id"]] >= 5:
            continue
        chosen.append(c)
        per[c["family_id"]] += 1
        if len(chosen) >= 100:
            break
    # ensure >= 20 timing cases
    tcount = sum(c["timing"] for c in chosen)
    print("chosen", len(chosen), "timing", tcount, "families", len(per), "tiers", collections.Counter(c["tier"] for c in chosen),
          "sources", collections.Counter(c["source"] for c in chosen), "avail", collections.Counter(c["fix_available_at_cutoff"] for c in chosen))
    spares = [c for c in elig if c not in chosen]
    cols = ["case_id", "mid", "source", "family_id", "family_name", "subsystem", "mechanism_class", "timing_dependent",
            "category_key", "kernel_full", "crash_commit", "rep_host", "rep_time", "rep_time_utc", "cutoff", "n_hosts",
            "gt_tier", "known_root_cause", "fix_commits", "fix_authors", "fix_landed_on", "fix_available_at_cutoff",
            "bot_was_right", "answer_visible_in_input", "difficulty", "fair_test_rationale", "evidence", "pin_method",
            "cutoff_derive", "cutoff_derive_utc", "human_signal", "original_post", "original_report_paste"]
    def row(i, c):
        ff = c["fix_facts"]
        return dict(case_id=f"CCB-{i:03d}", mid=c["mid"], source=c["source"], family_id=c["family_id"],
                    family_name=c["family_name"], subsystem=c["subsystem"], mechanism_class=c["mechanism"],
                    timing_dependent=c["timing"], category_key=c["category_key"], kernel_full=c["kernel_full"],
                    crash_commit=c["crash_commit"][:12], rep_host=c["rep_host"], rep_time=c["rep_time"],
                    rep_time_utc=iso(c["rep_time"]), cutoff=c["rep_time"], n_hosts=c["n_hosts"], gt_tier=c["tier"],
                    known_root_cause=c["root_cause"],
                    fix_commits="; ".join(f"{f['sha']} {f['subject']}" for f in ff),
                    fix_authors="; ".join(sorted({f["author"] for f in ff})),
                    fix_landed_on="; ".join(f"{f['sha']}:{'/'.join(f['landed_on'])}@{iso(f['commit_time'])[:10]}" for f in ff),
                    fix_available_at_cutoff=c["fix_available_at_cutoff"], bot_was_right=c["bot_was_right"],
                    answer_visible_in_input=c["visible"], difficulty=c.get("difficulty", ""),
                    fair_test_rationale=c["fair"], evidence=" | ".join(e for e in c["evidence"] if e)[:600],
                    pin_method=c["pin_method"], cutoff_derive=c.get("cutoff_derive", ""),
                    cutoff_derive_utc=iso(c.get("cutoff_derive", 0)), human_signal=c.get("human_signal", ""),
                    original_post=c.get("post_url", ""), original_report_paste=c.get("report_paste", ""))
    with open(f"{OUT}/cases_100.csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=cols)
        w.writeheader()
        for i, c in enumerate(chosen, 1):
            w.writerow(row(i, c))
    with open(f"{OUT}/cases_spares.csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=cols)
        w.writeheader()
        for i, c in enumerate(spares, 1):
            w.writerow(row(900 + i, c))
    inp = ["case_id", "mid", "kernel_full", "crash_commit", "rep_host", "rep_time", "cutoff"]
    with open(f"{OUT}/cases_100_inputs.csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=inp)
        w.writeheader()
        for i, c in enumerate(chosen, 1):
            r = row(i, c)
            w.writerow({k: r[k] for k in inp})
    json.dump({"chosen": [c["mid"] for c in chosen], "per_family": per}, open(f"{D}/selection.json", "w"), indent=1)


if __name__ == "__main__":
    {"pool": pool, "select": select}[sys.argv[1]]()
