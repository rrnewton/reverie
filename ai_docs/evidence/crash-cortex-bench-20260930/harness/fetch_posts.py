import json, subprocess, sys
out = "/home/newton/local/ccbench/data/wp_posts_all.json"
allp = {}
before = None
for i in range(60):
    cmd = ["meta", "workplace.post", "list", "--group-id", "4508446156040829", "--limit", "50", "--output", "json"]
    if before:
        cmd += ["--before", str(before)]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    try:
        d = json.loads(r.stdout)
    except Exception:
        print("parse fail", r.stderr[:500]); break
    ps = d.get("posts", [])
    new = 0
    for p in ps:
        if p["id"] not in allp:
            allp[p["id"]] = p; new += 1
    print(i, len(ps), new, d.get("has_more"), d.get("next_before"), ps[-1]["time"] if ps else None, flush=True)
    if not ps or not d.get("has_more") or new == 0:
        break
    before = d.get("next_before") or ps[-1]["time_epoch"]
json.dump(list(allp.values()), open(out, "w"))
print("total", len(allp))
