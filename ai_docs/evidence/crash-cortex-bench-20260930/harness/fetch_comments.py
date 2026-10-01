import json, subprocess
from concurrent.futures import ThreadPoolExecutor
ps = json.load(open("/home/newton/local/ccbench/data/wp_posts_all.json"))
def get(p):
    if int(p.get("comment_count") or 0) == 0:
        return p["id"], []
    r = subprocess.run(["meta", "workplace.comment", "list", "--post-id", p["id"], "--limit", "100", "--output", "json"], capture_output=True, text=True, timeout=300)
    try:
        return p["id"], json.loads(r.stdout)
    except Exception:
        return p["id"], {"error": r.stderr[:300], "raw": r.stdout[:300]}
with ThreadPoolExecutor(12) as ex:
    res = dict(ex.map(get, ps))
json.dump(res, open("/home/newton/local/ccbench/data/wp_comments.json", "w"))
print(len(res), sum(1 for v in res.values() if isinstance(v, dict) and "error" in v))
