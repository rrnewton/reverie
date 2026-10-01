import json
W = "/home/newton/.claude/projects/-tmp-agentcloud-18498-workspace-b647859a-ba8f-42ad-b6cc-811954871a34/b647859a-ba8f-42ad-b6cc-811954871a34/subagents/workflows"
D = "/home/newton/local/ccbench/data"
def res(wf):
    started, out = {}, []
    for l in open(f"{W}/{wf}/journal.jsonl"):
        e = json.loads(l)
        if e["type"] == "started":
            started[e["key"]] = e.get("label", "")
        elif e["type"] == "result" and isinstance(e.get("result"), dict):
            out.append((started.get(e.get("key"), ""), e["result"]))
    return out
adj, sk, mined, skm = {}, {}, [], {}
for lab, r in res("wf_35ea35f5-588") + res("wf_9c71e80a-e1e"):
    if lab.startswith("adj:"):
        adj[r["mid"]] = r
    elif lab.startswith("skeptic:"):
        sk[lab.split(":")[1]] = r
    elif lab.startswith("mine:") or lab.startswith("crashfirst:"):
        for c in r.get("cases", []):
            c["miner"] = lab
            mined.append(c)
    elif lab.startswith("skeptic-mined:") or lab.startswith("skeptic-cf:"):
        skm.setdefault(lab.split(":")[1], []).append(r)
for mid, a in adj.items():
    s = sk.get(mid[:8])
    if s:
        a["skeptic"] = s
    a["final_tier"] = (s["corrected_tier"] if (s and not s["upheld"]) else a["gt_tier"])
# attach skeptic verdicts to mined cases by MID prefix (take the first unused)
used = {}
for c in mined:
    k = (c.get("mid") or "")[:8]
    lst = skm.get(k, [])
    i = used.get(k, 0)
    c["skeptic"] = lst[i] if i < len(lst) else None
    used[k] = i + 1
json.dump(list(adj.values()), open(f"{D}/adjudicated.json", "w"), indent=1)
json.dump(mined, open(f"{D}/mined_all.json", "w"), indent=1)
print("adj", len(adj), "mined", len(mined), "mined w/ skeptic", sum(1 for c in mined if c["skeptic"]),
      "upheld", sum(1 for c in mined if c["skeptic"] and c["skeptic"].get("upheld")))
