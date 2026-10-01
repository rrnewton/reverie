#!/usr/bin/env python3
"""Collect + audit a run whose supervisor exited before writing manifest.json
(also used to re-audit any run). Mirrors step 5 of ccbench_run.py.

  ccbench_finalize.py --case CASE_DIR --arm ARM_JSON --run-id ID [--reason wall_cap|token_cap|exited]
"""
import argparse
import json
import os
import re
import shutil
import subprocess

from ccbench_run import KINV, LEAK_PATTERNS, RUNS, cost_of, sha, usage_total, HARNESS


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", required=True)
    ap.add_argument("--arm", required=True)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--reason", default="")
    ap.add_argument("--keep-live", action="store_true", help="do not remove /tmp/kinv-logs or the worktree")
    a = ap.parse_args()
    case = os.path.abspath(a.case)
    arm = json.load(open(a.arm))
    bm = json.load(open(os.path.join(case, "MANIFEST.json")))
    tree = json.load(open(os.path.join(case, "tree.json")))
    mid = bm["mid"]
    run = os.path.join(RUNS, a.run_id)
    out_path = os.path.join(run, "stream.jsonl")
    log_dir = os.path.join(KINV, mid)
    linux = os.path.join(run, "linux")

    per_msg, result_evt, tool_hist, hits = {}, None, {}, []
    pats = [(k, re.compile(p.replace("{MID}", mid).replace("{RUN}", a.run_id))) for k, p in LEAK_PATTERNS]
    first_ts = last_ts = None
    for line in open(out_path):
        try:
            ev = json.loads(line)
        except Exception:
            continue
        msg = ev.get("message")
        if ev.get("type") == "assistant" and isinstance(msg, dict) and msg.get("id") and msg.get("usage"):
            per_msg[msg["id"]] = msg["usage"]
        if ev.get("type") == "result":
            result_evt = ev
        for c in (msg.get("content") if isinstance(msg, dict) else None) or []:
            if isinstance(c, dict) and c.get("type") == "tool_use":
                tool_hist[c["name"]] = tool_hist.get(c["name"], 0) + 1
                inp = c.get("input") or {}
                if c["name"] in ("Write", "Edit", "NotebookEdit", "TodoWrite", "TaskCreate", "TaskUpdate"):
                    s = json.dumps({k: inp.get(k) for k in ("file_path",)})  # audit paths, not file bodies
                else:
                    s = json.dumps(inp)
                for k, rx in pats:
                    if rx.search(s):
                        hits.append({"pattern": k, "tool": c["name"], "input": s[:400]})
    st = os.stat(out_path)
    wall = st.st_mtime - os.stat(os.path.join(run, "prompt.txt")).st_mtime
    reason = a.reason or ("wall_cap" if wall >= 3590 else ("exited" if result_evt else "unknown"))

    outd = os.path.join(run, "out")
    if os.path.isdir(log_dir):
        shutil.copytree(log_dir, outd, dirs_exist_ok=True,
                        ignore=shutil.ignore_patterns("build", "*.img", "*.qcow2", "vmlinux", "*.o", "*.ko.debug"))
    if os.path.isdir(linux):
        wd = subprocess.run(["git", "-C", linux, "diff", "HEAD"], capture_output=True, text=True).stdout
        open(os.path.join(run, "worktree.diff"), "w").write(wd)
    denials = (result_evt or {}).get("permission_denials") or []
    manifest = {
        "run_id": a.run_id, "mid": mid, "arm": arm["name"], "exit_reason": reason,
        "wall_secs": round(wall, 1), "tokens_total_live": sum(usage_total(u) for u in per_msg.values()),
        "api_calls": len(per_msg),
        "tokens_breakdown_live": {k: sum(u.get(k, 0) for u in per_msg.values()) for k in
                                  ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")},
        "cost_usd_list_live": round(sum(cost_of(u) for u in per_msg.values()), 2),
        "cli_result_cost_usd": (result_evt or {}).get("total_cost_usd"),
        "cli_result_usage": (result_evt or {}).get("usage"),
        "cli_model_usage": (result_evt or {}).get("modelUsage"),
        "tokens_total_cli": sum(v.get("inputTokens", 0) + v.get("outputTokens", 0) + v.get("cacheReadInputTokens", 0) +
                                v.get("cacheCreationInputTokens", 0) for v in ((result_evt or {}).get("modelUsage") or {}).values()),
        "cost_usd_cli": round(sum(v.get("costUSD", 0) for v in ((result_evt or {}).get("modelUsage") or {}).values()), 2),
        "cli_num_turns": (result_evt or {}).get("num_turns"),
        "tool_calls": tool_hist, "permission_denials": len(denials),
        "permission_denials_sample": [d.get("tool_input") for d in denials[:25]],
        "leak_audit_hits": hits,
        "outputs": sorted(os.listdir(outd)) if os.path.isdir(outd) else [],
        "report_present": os.path.exists(os.path.join(outd, "report.md")),
        "patch_present": os.path.exists(os.path.join(outd, "kernel_fix.patch")),
        "hashes": {"prompt": sha(os.path.join(run, "prompt.txt")), "arm": sha(a.arm),
                   "offline_env": sha(os.path.join(HARNESS, "offline_env.md")),
                   "bundle_manifest": sha(os.path.join(case, "MANIFEST.json"))},
        "crash_commit": tree["crash_full"], "cutoff": tree["cutoff"], "finalized_by": "ccbench_finalize.py",
    }
    json.dump(manifest, open(os.path.join(run, "manifest.json"), "w"), indent=1)
    if not a.keep_live:
        shutil.rmtree(log_dir, ignore_errors=True)
        subprocess.run(["git", "--git-dir", os.path.join(case, "linux.git"), "worktree", "remove", "--force", linux])
    print(json.dumps({k: manifest[k] for k in ("run_id", "exit_reason", "wall_secs", "tokens_total_live",
                                               "cost_usd_list_live", "report_present", "patch_present",
                                               "permission_denials")}), "leak_hits", len(hits))


if __name__ == "__main__":
    main()
