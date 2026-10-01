#!/usr/bin/env python3
"""Deterministically verify every fix commit the investigations name, against the
Meta kernel tree (partial clone is enough: commits + trees).

For each MID: resolve crash-kernel commits (the _g<sha12> in the uname strings seen in
Hive), then for each claimed fix: does the commit exist, who wrote it and when, which
release/upstream branches contain it (or a same-subject cherry-pick), and is it already
an ancestor of the crash kernel (then it cannot be the fix for that crash).
"""
import collections
import json
import re
import subprocess

REPO = "/data/users/newton/linux-full.git"
BR = ["v6.9-fb", "v6.11-fb", "v6.13-fb", "v6.16-fb", "v6.19-fb", "v7.1-fb",
      "linus-upstream", "net-next-upstream", "main"]


def git(*a, check=False):
    r = subprocess.run(["git", "--git-dir", REPO, *a], capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError(r.stderr)
    return r.returncode, r.stdout.strip()


def resolve(sha):
    rc, out = git("rev-parse", "--verify", "--quiet", f"{sha}^{{commit}}")
    return out if rc == 0 else ""


def info(full):
    _, out = git("log", "-1", "--format=%an%x09%ae%x09%at%x09%ct%x09%s", full)
    an, ae, at, ct, s = (out.split("\t") + [""] * 5)[:5]
    return dict(author=an, email=ae, author_time=int(at or 0), commit_time=int(ct or 0), subject=s)


def is_anc(a, b):
    rc, _ = git("merge-base", "--is-ancestor", a, b)
    return rc == 0


def main():
    posts = json.load(open("/home/newton/local/ccbench/data/extracted.json"))
    agg = json.load(open("/home/newton/local/ccbench/data/hive_mid_kernel_agg.json"))
    kern = collections.defaultdict(set)
    for r in agg:
        m = re.search(r"_g([0-9a-f]{12})", r["kernel"] or "")
        if m:
            kern[r["mid"]].add(m.group(1))
    tips = {b: resolve(f"refs/heads/{b}") for b in BR}

    # Subject index of release/upstream branches since 2025-01 for cherry-pick matching.
    subj = collections.defaultdict(list)
    for b in BR:
        _, out = git("log", "--since=2025-01-01", "--no-merges", "--format=%H%x09%ct%x09%s", f"refs/heads/{b}")
        for line in out.splitlines():
            h, ct, s = (line.split("\t") + ["", ""])[:3]
            subj[s.strip()].append((h, int(ct or 0), b))

    crash_cache = {}
    out = {}
    for p in posts:
        mid = p["mid"]
        if not mid:
            continue
        rec = out.setdefault(mid, {"crash_kernels": {}, "fixes": {}})
        for k in kern.get(mid, []):
            if k not in crash_cache:
                full = resolve(k)
                crash_cache[k] = dict(full=full, **(info(full) if full else {}))
            rec["crash_kernels"][k] = crash_cache[k]
        refs = [(f["sha"], f["title"], f["kind"]) for f in p["claimed_fixes"]]
        fr = p["human_signal"].get("fix_ref") or ""
        for h in re.findall(r"\b[0-9a-f]{10,40}\b", fr):
            refs.append((h, "", "human_named_fix"))
        for sha, title, kind in refs:
            key = sha or ("title:" + title)
            if not sha and not title:
                continue
            e = rec["fixes"].setdefault(key, {"sha": sha, "title": title, "kinds": []})
            if kind not in e["kinds"]:
                e["kinds"].append(kind)
            if "exists" in e:
                continue
            full = resolve(sha) if sha else ""
            e["exists"] = bool(full)
            if full:
                e.update(full=full, **info(full))
                e["on_branches"] = [b for b, t in tips.items() if t and is_anc(full, t)]
                s = e["subject"]
            else:
                s = title.strip()
            picks = subj.get(s, []) if s else []
            e["same_subject_on"] = sorted({b for _, _, b in picks})
            e["same_subject_first_commit_time"] = min((ct for _, ct, _ in picks), default=0)
            e["in_crash_kernel"] = {}
            for k, ck in rec["crash_kernels"].items():
                if not ck.get("full"):
                    continue
                inck = bool(full) and is_anc(full, ck["full"])
                if not inck and picks:
                    # a same-subject cherry-pick already in the crash kernel also counts
                    inck = any(is_anc(h, ck["full"]) for h, _, _ in picks[:6])
                e["in_crash_kernel"][k] = inck
    json.dump(out, open("/home/newton/local/ccbench/data/fix_verification.json", "w"), indent=1)
    n = sum(len(r["fixes"]) for r in out.values())
    ex = sum(1 for r in out.values() for f in r["fixes"].values() if f.get("exists"))
    print("mids", len(out), "fix refs", n, "exist", ex,
          "crash kernels resolved", sum(1 for v in crash_cache.values() if v["full"]), "/", len(crash_cache))


if __name__ == "__main__":
    main()
