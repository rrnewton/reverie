#!/usr/bin/env python3
"""Classify every kernel71 HERMIT_EXECUTION event by the verifier verdict printed in its tracked STDERR artifact.
Read-only. Output: per-root and per-target counts, and the first divergent syscall for each nondeterministic verdict."""
import json, glob, os, re, collections
REPO = "/home/newton/work/dev-hermit"
cnt = collections.Counter()
by_target = collections.defaultdict(collections.Counter)
firstdiv = collections.Counter()
rows = []
for p in sorted(glob.glob(os.path.join(REPO, "experiments/tfa-kernel71-*_20260921/services/*/*/experiment.json"))):
    d = json.load(open(p))
    root = p.split("/services/")[0]
    rootname = os.path.basename(root)
    t = d["identity"]["canonical_target"]
    for e in d["events"]:
        if e["kind"] != "HERMIT_EXECUTION":
            continue
        r = e["result"]
        se = [a for a in e["artifacts"] if a["kind"] == "STDERR"]
        txt = open(os.path.join(root, se[0]["locator"]), errors="replace").read() if se else ""
        if ":: Failure: nondeterministic" in txt:
            v = "VERIFY_NONDETERMINISTIC"
        elif "Success: deterministic" in txt or "Determinism verified" in txt:
            v = "VERIFY_DETERMINISTIC"
        elif "HERMIT_POLICY_REFUSAL" in txt:
            v = "HERMIT_POLICY_REFUSAL:" + (re.search(r"cause=(\S+)", txt).group(1) if re.search(r"cause=(\S+)", txt) else "?")
        elif r.get("timed_out"):
            v = "OUTER_TIMEOUT"
        else:
            v = "OTHER"
        key = (v, r.get("guest_started"), "BitwiseInfoV1=matched" in (r.get("diagnostic") or ""))
        cnt[key] += 1
        by_target[t][v] += 1
        if v == "VERIFY_NONDETERMINISTIC":
            m = re.search(r"Divergent syscall context:\s*\n\s*run 1, log message \d+: INFO detcore: DETLOG \[syscall\]\[detcore, dtid \d+\] finish syscall #\d+: (\w+)\(([^,)]*)", txt)
            m2 = re.search(r"Mismatch in (\w+) between run 1 and run 2", txt)
            fd = (m.group(1) + "(" + m.group(2) + ")") if m else ("no-syscall-context; first-mismatch=" + (m2.group(1) if m2 else "?"))
            firstdiv[fd] += 1
            rows.append((rootname, e["event_id"], r.get("guest_started"), r.get("exit_code"), fd, se[0]["locator"]))
print("== (verifier verdict from stderr, result.guest_started, diagnostic says BitwiseInfoV1=matched) -> count")
for k, v in sorted(cnt.items(), key=lambda x: -x[1]):
    print(k, v)
print("total HERMIT_EXECUTION events:", sum(cnt.values()))
print("\n== per target verdict counts")
for t, c in sorted(by_target.items()):
    print(t, dict(c))
print("\n== first divergent syscall (run 1) for VERIFY_NONDETERMINISTIC events")
for k, v in firstdiv.most_common():
    print(v, k)
print("\n== VERIFY_NONDETERMINISTIC events")
for r in rows:
    print(*r)
