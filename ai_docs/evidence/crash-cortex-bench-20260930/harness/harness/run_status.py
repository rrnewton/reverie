import json, sys, os, time, glob
for r in sorted(glob.glob("/data/users/newton/ccbench/runs/pilot-*/")):
    per = {}; tools = {}; last = ""; t0 = os.path.getctime(r + "stream.jsonl")
    for line in open(r + "stream.jsonl"):
        try: ev = json.loads(line)
        except Exception: continue
        m = ev.get("message") or {}
        if not isinstance(m, dict): continue
        if ev.get("type") == "assistant" and m.get("id") and m.get("usage"): per[m["id"]] = m["usage"]
        for c in m.get("content") or []:
            if isinstance(c, dict) and c.get("type") == "tool_use":
                tools[c["name"]] = tools.get(c["name"], 0) + 1
                last = (c["name"] + " " + json.dumps(c.get("input"))[:160])
    tok = sum(u.get("input_tokens",0)+u.get("output_tokens",0)+u.get("cache_read_input_tokens",0)+u.get("cache_creation_input_tokens",0) for u in per.values())
    print(os.path.basename(r.rstrip("/")), f"{(time.time()-t0)/60:.1f}min", f"tok={tok/1e6:.2f}M calls={len(per)}", tools, "\n   last:", last, "\n   manifest:", os.path.exists(r+"manifest.json"))
