#!/usr/bin/env python3
"""Deterministic, pre-judge facts for grading one run against its sealed ground truth.

  grade_prep.py --run-id ID --case CASE_DIR --sealed SEALED.json --out FACTS.json [--blind-id X]

Facts: did the report/patch cite a ground-truth fix commit (sha prefix >=10 or exact
subject); file/function overlap between kernel_fix.patch and the GT fix; reproducer
evidence (qemu actually booted a kernel and ran something) from the transcript's tool
results; cost/time/exit reason from manifest.json. Also writes a blinded copy of the
report (arm-revealing capability names masked) for the LLM judges.
"""
import argparse
import json
import os
import re
import subprocess

MIRROR = "/data/users/newton/linux-full.git"
ARM_TELLS = [r"/kverify", r"/kreview", r"kverify", r"kreview", r"skills-variant", r"variant-kskills"]


def git(*a):
    return subprocess.run(["git", "--git-dir", MIRROR, *a], capture_output=True, text=True).stdout


def patch_files_funcs(patch):
    files = set(re.findall(r"^\+\+\+ b/(\S+)", patch, re.M))
    funcs = set(m.strip() for m in re.findall(r"^@@[^@]*@@\s*(.*)$", patch, re.M))
    fn = set()
    for f in funcs:
        m = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\s*\(", f)
        if m:
            fn.add(m.group(1))
    return files, fn


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--case", required=True)
    ap.add_argument("--sealed", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--blind-id", default="")
    a = ap.parse_args()
    run = f"/data/users/newton/ccbench/runs/{a.run_id}"
    gt = json.load(open(a.sealed))
    man = json.load(open(os.path.join(run, "manifest.json")))
    rep = open(os.path.join(run, "out", "report.md")).read() if os.path.exists(os.path.join(run, "out", "report.md")) else ""
    pat = open(os.path.join(run, "out", "kernel_fix.patch")).read() if os.path.exists(os.path.join(run, "out", "kernel_fix.patch")) else ""
    text = rep + "\n" + pat

    cited = []
    gt_files, gt_funcs = set(), set()
    for f in gt["fix_commits"]:
        s = f["sha"]
        subj = f["subject"]
        hit_sha = any(s[:n] in text for n in (12, 10)) if len(s) >= 10 else False
        hit_subj = subj.lower() in text.lower()
        cited.append({"sha": s, "subject": subj, "sha_cited": hit_sha, "subject_cited": hit_subj})
        full = git("rev-parse", "--verify", "--quiet", s + "^{commit}").strip()
        if full:
            ff, fn = patch_files_funcs(git("show", "--format=", full))
            gt_files |= ff
            gt_funcs |= fn
    pf, pfn = patch_files_funcs(pat)

    # Reproducer evidence from the transcript: qemu commands and whether their results show a boot.
    q_cmds, boots, tool_ids = 0, 0, {}
    for line in open(os.path.join(run, "stream.jsonl")):
        try:
            ev = json.loads(line)
        except Exception:
            continue
        msg = ev.get("message")
        if not isinstance(msg, dict):
            continue
        for c in msg.get("content") or []:
            if not isinstance(c, dict):
                continue
            if c.get("type") == "tool_use" and c.get("name") == "Bash":
                cmd = (c.get("input") or {}).get("command", "")
                if "qemu-system" in cmd and "--version" not in cmd and "-machine help" not in cmd:
                    q_cmds += 1
                    tool_ids[c["id"]] = True
            if c.get("type") == "tool_result" and c.get("tool_use_id") in tool_ids:
                body = c.get("content")
                body = json.dumps(body) if not isinstance(body, str) else body
                if re.search(r"Linux version \d|Run /init|Kernel panic|BUG:|soft lockup|login:", body):
                    boots += 1
    repro_dir = os.path.join(run, "out", "reproducer")
    repro_files = []
    if os.path.isdir(repro_dir):
        for root, _, fs in os.walk(repro_dir):
            for f in fs:
                repro_files.append(os.path.relpath(os.path.join(root, f), repro_dir))
    boot_logs = []
    for f in repro_files:
        p = os.path.join(repro_dir, f)
        try:
            if os.path.getsize(p) < 50_000_000 and re.search(rb"Linux version \d", open(p, "rb").read()):
                boot_logs.append(f)
        except OSError:
            pass

    facts = {
        "blind_id": a.blind_id or a.run_id,
        "mid": gt["mid"],
        "gt_fix_cited": cited,
        "gt_fix_files": sorted(gt_files), "gt_fix_funcs": sorted(gt_funcs),
        "patch_present": bool(pat), "patch_files": sorted(pf), "patch_funcs": sorted(pfn),
        "patch_file_overlap": sorted(gt_files & pf), "patch_func_overlap": sorted(gt_funcs & pfn),
        "repro_qemu_invocations": q_cmds, "repro_qemu_results_showing_boot": boots,
        "repro_files": repro_files[:60], "repro_boot_logs": boot_logs[:20],
        "cost": {k: man.get(k) for k in ("exit_reason", "wall_secs", "tokens_total_live", "cost_usd_list_live",
                                         "cli_result_cost_usd", "api_calls", "permission_denials")},
        "leak_audit_hits": len(man.get("leak_audit_hits") or []),
        "report_chars": len(rep),
    }
    json.dump(facts, open(a.out, "w"), indent=1)
    blind = rep
    for t in ARM_TELLS:
        blind = re.sub(t, "[TOOL]", blind, flags=re.I)
    open(a.out.replace(".json", ".report.blind.md"), "w").write(blind)
    open(a.out.replace(".json", ".patch.diff"), "w").write(pat)
    print(json.dumps({k: facts[k] for k in ("mid", "patch_present", "patch_file_overlap", "patch_func_overlap",
                                            "repro_qemu_invocations", "repro_qemu_results_showing_boot", "leak_audit_hits")}),
          [c["sha_cited"] or c["subject_cited"] for c in cited])


if __name__ == "__main__":
    main()
