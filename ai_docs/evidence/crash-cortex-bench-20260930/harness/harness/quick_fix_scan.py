import json, subprocess
G = ["git", "--git-dir=/data/users/newton/linux-full.git"]
posts = json.load(open("/home/newton/local/ccbench/data/extracted.json"))
seen = set()
rows = []
for p in posts:
    for f in p["claimed_fixes"]:
        if f["kind"] not in ("exact_root_cause_fix", "backport_of_fix") or not f["sha"]:
            continue
        k = (p["mid"], f["sha"])
        if k in seen:
            continue
        seen.add(k)
        r = subprocess.run(G + ["log", "-1", "--no-walk", "--format=%H%x09%an%x09%ae%x09%at%x09%ct%x09%s", f["sha"]], capture_output=True, text=True)
        if r.returncode:
            rows.append((p["mid"], f["sha"], "MISSING", "", "", "", f["title"][:60])); continue
        parts = r.stdout.strip().split("\t")
        if len(parts) < 6:
            rows.append((p["mid"], f["sha"], "MISSING", "", "", "", f["title"][:60])); continue
        h, an, ae, at, ct, s = parts[:6]
        rows.append((p["mid"], f["sha"], an, ae, at, ct, s[:80]))
json.dump(rows, open("/home/newton/local/ccbench/data/quick_fix_scan.json", "w"))
import collections
print(len(rows), collections.Counter(r[2] == "MISSING" for r in rows))
print(collections.Counter(r[3].split("@")[0] for r in rows if r[2] != "MISSING").most_common(30))
