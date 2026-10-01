import json, re, collections
ps = json.load(open("/home/newton/local/ccbench/data/wp_posts_all.json"))
cm = json.load(open("/home/newton/local/ccbench/data/wp_comments.json"))
def f(pat, b, g=1):
    m = re.search(pat, b)
    return m.group(g).strip() if m else ""
rows = []
for p in sorted(ps, key=lambda x: int(x["time_epoch"])):
    b = p["body"]
    title = b.split("\n", 1)[0].lstrip("# ").strip()
    r = dict(
        post_id=p["id"], url=p["url"], time=p["time"], time_epoch=int(p["time_epoch"]),
        title=title,
        kernels=f(r"^#?\s*\[([^\]]*)\]", b.strip()),
        mid=f(r"\*\*MID:\*\*\s*`([0-9a-f]+)`", b),
        type=f(r"\*\*Type:\*\*\s*([^\n]+)", b),
        severity=f(r"\*\*Severity:\*\*\s*([^\n]+)", b),
        subsystem=f(r"\*\*Subsystem:\*\*\s*([^\n]+)", b),
        root_cause=f(r"\*\*Root Cause:\*\*\s*([^\n]+)", b),
        confidence=f(r"\*\*Confidence:\*\*\s*([^\n]+)", b),
        hosts=f(r"\*\*Hosts:\*\*\s*(\d+)", b),
        models=f(r"\*\*Models:\*\*\s*([^\n]+)", b),
        report_paste=f(r"\*\*Full Report:\*\*\s*(P\d+)", b),
        patch_paste=f(r"\*\*Kernel Patch:\*\*\s*(P\d+)", b),
        bundle=f(r"jf download (\S+) --file \"(kinv-bundle-[^\"]+)\"", b),
        bundle_file=f(r"jf download (\S+) --file \"(kinv-bundle-[^\"]+)\"", b, 2),
        diff=",".join(sorted(set(re.findall(r"\bD\d{8,9}\b", b)))),
        tasks=",".join(sorted(set(re.findall(r"\bT\d{8,9}\b", b)))),
        shas=",".join(list(dict.fromkeys(re.findall(r"\b[0-9a-f]{12,40}\b", b)))[:40]),
        body_len=len(b),
        n_comments=len(cm.get(p["id"]) or []),
        commenters=";".join(sorted(set(c["author"] for c in (cm.get(p["id"]) or []) if c.get("author") != "CrashCortex"))),
    )
    rows.append(r)
json.dump(rows, open("/home/newton/local/ccbench/data/posts_parsed.json", "w"), indent=1)
print(len(rows))
print(collections.Counter(bool(r["mid"]) for r in rows))
print("no-mid titles:", [r["title"][:90] for r in rows if not r["mid"]][:12])
print(collections.Counter(bool(r["report_paste"]) for r in rows), collections.Counter(bool(r["bundle"]) for r in rows))
print("body_len", sorted(r["body_len"] for r in rows)[::30])
hc = collections.Counter(r["n_comments"] > 0 and bool(r["commenters"]) for r in rows); print("human comments", hc)
print(collections.Counter(c for r in rows for c in r["commenters"].split(";") if c).most_common(25))
