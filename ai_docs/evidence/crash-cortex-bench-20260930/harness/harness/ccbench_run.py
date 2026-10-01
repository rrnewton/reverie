#!/usr/bin/env python3
"""Run one agent-under-test on one frozen case, offline, with hard caps.

  ccbench_run.py --case CASE_DIR --arm ARM_JSON --run-id ID [--wall 3600] [--token-cap 40000000]

What it does (and why):
  * stages ONLY the bundle's evidence/ files into /tmp/kinv-logs/<MID>/ (the path the
    production prompt and the /kernel-investigation skill hard-code);
  * checks out the crash-kernel commit from the case's time-cut repo (no remote,
    no post-cutoff refs) as the agent's cwd, like kernel_investigator (cwd=kernel tree);
  * renders the production prompt text (prompts.build_investigation_prompt, offline:
    no ODS section, no live-vmcore section) + a fixed offline-environment section that
    is byte-identical across arms + the arm's capability section;
  * launches `claude -p` in --safe-mode with --strict-mcp-config (no user/plugin MCP
    servers, no plugin skills, no hooks, no CLAUDE.md), a fresh CLAUDE_CONFIG_DIR,
    --permission-mode dontAsk and an explicit allowlist (everything else is denied);
  * enforces the wall cap and a live token cap (sum over API calls of
    input + cache_creation + cache_read + output, deduped by message id) by killing
    the process group; no resume;
  * writes manifest.json: wall, tokens, $ at list price, tool-call histogram,
    permission denials, leak-audit hits, exit reason, prompt/skill/arm hashes.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import uuid

HARNESS = os.path.dirname(os.path.abspath(__file__))
RUNS = "/data/users/newton/ccbench/runs"
KINV = "/tmp/kinv-logs"
QEMU_DIR = "/data/users/newton/ccbench/tools/bin"

# List price per token (USD). Verified against the CLI's own costUSD for
# claude-opus-4-8 (in 5, out 25, cache-read 0.5, 1h cache-write 10 per MTok).
PRICE = {"input": 5e-6, "output": 25e-6, "cache_read": 0.5e-6, "cache_write_1h": 10e-6, "cache_write_5m": 6.25e-6}

# Offline allowlist = production AGENT_ALLOWED_TOOLS minus everything that reaches
# fleet, Scuba, Phabricator or the network (sush2 oobit serf scuba ping repair
# nvidia-* netrca buck2 arc pastry jf rpm fbrpm svnyum mkosi), plus the local build/VM
# tools the offline reproducer needs (gcc, qemu, cpio, sed, awk, timeout...).
BASE_TOOLS = ["Read", "Grep", "Glob", "Write", "Edit", "Task", "Agent", "TodoWrite"]
BASE_BASH = ["git", "grep", "find", "ls", "cat", "head", "tail", "wc", "sort", "diff",
             "echo", "mkdir", "cp", "mv", "rm", "chmod", "base64", "sha256sum", "tar",
             "zip", "zcat", "gunzip", "drgn", "make", "python3", "gcc", "cpio", "sed",
             "awk", "timeout", "file", "objdump", "nm", "readelf", "addr2line", "strings",
             "uniq", "cut", "tr", "xz", "zstd", "gzip", "ld", "cd", "pwd", "touch", "tee",
             "qemu-system-x86_64", "qemu-img", "scripts/config", "./scripts/config",
             "ccb-qemu-run", "patch", "bc", "date", "sleep", "kill", "ps", "nproc", "du", "df",
             "stat", "realpath", "dirname", "basename", "test", "true", "printf", "xxd"]
# v1.1 (after the pilot): guest-image tools the btrfs pilot run was denied (mkfs/dd/truncate/fallocate)
BASH_V11 = ["mkfs.btrfs", "mkfs.ext4", "mkfs.xfs", "dd", "truncate", "fallocate"]
HARNESS_VERSION = "ccbench-harness/1.1"  # pilot ran 1.0 (no mkfs/dd/truncate/fallocate)

LEAK_PATTERNS = [
    ("network_client", r"\b(curl|wget|nc|ncat|telnet|ssh|scp|rsync|sush2)\b"),
    ("url", r"https?://"),
    ("meta_internal_host", r"internalfb|facebook\.com|fbinfra|tfbnw|fburl|workplace"),
    ("fleet_data_cli", r"\b(scuba|presto|meta |jf |arc |pastry|serf|oobit|repair |netrca)\b"),
    ("python_network", r"urllib|requests\.|http\.client|socket\.|aiohttp|httpx"),
    ("git_remote", r"git\s+(-C\s+\S+\s+)?(fetch|pull|clone|ls-remote|remote|submodule)"),
    ("object_store_enumeration", r"--batch-all-objects|fsck|--unreachable|rev-list\s+--all\s+--objects|--reflog"),
    ("other_case_dirs", r"/tmp/kinv-logs/(?!{MID})[0-9a-f]{8}|/data/users/newton/ccbench/(cases|runs)/(?!{RUN})"),
    ("original_bot_outputs", r"kinv-bundle|P2[0-9]{9}|report\.md\.orig"),
]


def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest() if os.path.exists(path) else ""


def render_prompt(ctx, mid, log_dir, arm, linux_dir):
    """Production text of prompts.build_investigation_prompt (live_vmcore=False, no ODS),
    then the fixed offline section, then the arm capability section."""
    files = []
    for fname, desc in [("backtrace.txt", "Kernel backtrace"), ("dmesg.txt", "Full dmesg from crash host"),
                        ("drgn_output.txt", "Crash analysis (stack traces, modules, meminfo)"),
                        ("bandicoot.json", "Additional analysis data (MCP-exclusive fields)"),
                        ("kdump_dmesg.txt", "Kdump kernel log at crash time"),
                        ("host_timeline.txt", "Netcons messages around crash"),
                        ("oob_console.txt", "BMC serial console output"),
                        ("collection.json", "Collection metadata")]:
        p = os.path.join(log_dir, fname)
        if os.path.exists(p) and os.path.getsize(p) > 0:
            files.append(f"- `{fname}` ({os.path.getsize(p):,} bytes) — {desc}")
    pr = (
        "Investigate this kernel crash using the /kernel-investigation skill.\n\n"
        "## Crash Context\n"
        f"**MID:** {mid}\n"
        f"**Category:** {ctx['category_key']}\n"
        f"**Exception Type:** {ctx['exception_type']}\n"
        f"**Kernel:** {ctx['kernel_major']}\n"
        f"**Hardware (LSST):** {ctx['lsst'] or 'N/A'}\n"
        f"**Models:** {ctx['models'] or 'N/A'}\n"
        f"**Hosts Affected:** {ctx['hosts_affected']}\n"
        f"**Host Sample:** {', '.join(ctx['host_sample']) or 'N/A'}\n"
        f"**Investigated Host:** {ctx['investigated_host'] or 'N/A'}\n"
        "**Data Source:** kernel_mcp\n"
        "\n## Pre-collected Data\n"
        f"All crash data has been pre-collected and saved to `{log_dir}/`.\n"
        "Read the files there instead of re-collecting via kernel-mcp.\n\n"
        "Available files:\n" + ("\n".join(files) if files else "- (none)\n")
    )
    if ctx.get("backtrace"):
        pr += f"\n\n## Backtrace (for quick reference)\n```\n{ctx['backtrace'][:4000]}\n```\n"
    if ctx.get("panic"):
        pr += f"\n**Panic:** {ctx['panic']}\n"
    pr += (
        "\n## Instructions\n"
        f"1. Read the pre-collected data files from `{log_dir}/`\n"
        "2. Follow the /kernel-investigation skill procedure (all phases)\n"
        f"3. If you develop a patch, save it as `{log_dir}/kernel_fix.patch` (the file MUST exist separately from the report)\n"
        "4. Attempt to reproduce the crash in a mkosi-kernel QEMU VM. Use your analysis to identify the environmental "
        "conditions that triggered it (resource pressure, workload patterns, timing, hardware state) and replicate those "
        "conditions in the VM. If you have a patch, verify the fix by reproducing with and without it. Document the "
        "reproduction attempt in ### REPRODUCER and ### AB_VERIFICATION even if it fails.\n"
        "5. After completing analysis, run /kverify on your findings to verify they are not false positives. Apply the full verification checklist.\n"
        f"6. Write the report to `{log_dir}/report.md`\n"
        "7. Also output the report in your response text\n"
    )
    offline = open(os.path.join(HARNESS, "offline_env.md")).read()
    pr += "\n" + offline.format(LOG_DIR=log_dir, LINUX=linux_dir, QEMU_DIR=QEMU_DIR,
                                CRASH_KERNEL=ctx["kernel_full"], SKILL_DIR=os.path.expandvars(arm["skill_dir"]))
    pr += "\n" + arm["capability_section"]
    return pr


def usage_total(u):
    return (u.get("input_tokens", 0) + u.get("output_tokens", 0) +
            u.get("cache_read_input_tokens", 0) + u.get("cache_creation_input_tokens", 0))


def cost_of(u):
    cc = u.get("cache_creation") or {}
    w1 = cc.get("ephemeral_1h_input_tokens", 0)
    w5 = cc.get("ephemeral_5m_input_tokens", 0)
    if not cc:
        w1 = u.get("cache_creation_input_tokens", 0)
    return (u.get("input_tokens", 0) * PRICE["input"] + u.get("output_tokens", 0) * PRICE["output"] +
            u.get("cache_read_input_tokens", 0) * PRICE["cache_read"] + w1 * PRICE["cache_write_1h"] +
            w5 * PRICE["cache_write_5m"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", required=True)
    ap.add_argument("--arm", required=True)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--wall", type=int, default=3600)
    ap.add_argument("--token-cap", type=int, default=40_000_000)
    ap.add_argument("--model", default="claude-opus-4-8[1m]")
    ap.add_argument("--effort", default="max")
    ap.add_argument("--dry", action="store_true", help="stage + render only")
    ap.add_argument("--allowlist", default="1.1", choices=["1.0", "1.1"], help="1.0 = exactly what the find-mode pilot ran")
    ap.add_argument("--finalize", action="store_true", help="collect/audit an existing run (supervisor died)")
    ap.add_argument("--reason", default="exited")
    a = ap.parse_args()

    case = os.path.abspath(a.case)
    arm = json.load(open(a.arm))
    bm = json.load(open(os.path.join(case, "MANIFEST.json")))
    ctx = json.load(open(os.path.join(case, "context.json")))
    tree = json.load(open(os.path.join(case, "tree.json")))
    mid = bm["mid"]
    run = os.path.join(RUNS, a.run_id)
    if os.path.exists(run):
        sys.exit(f"run dir exists: {run}")
    os.makedirs(run)
    log_dir = os.path.join(KINV, mid)
    if os.path.exists(log_dir):
        sys.exit(f"{log_dir} exists: another run of this MID is active (one run per MID at a time)")

    # 1. Stage evidence (only), verify hashes against the bundle manifest.
    os.makedirs(log_dir, mode=0o700)
    for n, meta in bm["files"].items():
        src = os.path.join(case, "evidence", n)
        if sha(src) != meta["sha256"]:
            sys.exit(f"bundle hash mismatch: {n}")
        shutil.copy(src, os.path.join(log_dir, n))

    # 2. Kernel tree: crash commit from the time-cut per-case repo.
    linux = os.path.join(run, "linux")
    subprocess.run(["git", "--git-dir", os.path.join(case, "linux.git"), "worktree", "add", "-q",
                    "--detach", linux, tree["crash_full"]], check=True)

    # 3. Prompt + Claude config.
    prompt = render_prompt(ctx, mid, log_dir, arm, linux)
    ppath = os.path.join(run, "prompt.txt")
    open(ppath, "w").write(prompt)
    cfg = os.path.join(run, "claude-config")
    os.makedirs(cfg)
    bash = [f"Bash({c}:*)" for c in BASE_BASH + (BASH_V11 if a.allowlist == "1.1" else []) + arm.get("extra_bash", [])]
    tools = BASE_TOOLS + arm.get("extra_tools", []) + bash
    add_dirs = [log_dir] + [os.path.expandvars(d) for d in arm.get("add_dirs", [])]
    session = str(uuid.uuid4())
    cmd = ["claude", "-p", prompt, "--model", a.model, "--effort", a.effort,
           "--safe-mode", "--strict-mcp-config", "--permission-mode", "dontAsk",
           "--allowedTools", *tools, "--output-format", "stream-json", "--verbose",
           "--session-id", session]
    if arm.get("mcp_config"):
        cmd += ["--mcp-config", arm["mcp_config"]]
    for d in add_dirs:
        cmd += ["--add-dir", d]
    env = {k: v for k, v in os.environ.items() if k not in ("CLAUDECODE",)}
    env["CLAUDE_CONFIG_DIR"] = cfg
    env["PATH"] = QEMU_DIR + ":" + env.get("PATH", "")
    json.dump({"cmd": cmd[:2] + ["<prompt.txt>"] + cmd[3:], "cwd": linux, "env_overrides":
               {"CLAUDE_CONFIG_DIR": cfg, "PATH_prefix": QEMU_DIR}}, open(os.path.join(run, "cmd.json"), "w"), indent=1)
    if a.dry:
        print(prompt)
        return

    # 4. Launch + supervise.
    out_path = os.path.join(run, "stream.jsonl")
    err_path = os.path.join(run, "stderr.txt")
    t0 = time.time()
    with open(out_path, "w") as out, open(err_path, "w") as err:
        proc = subprocess.Popen(cmd, cwd=linux, env=env, stdout=out, stderr=err, start_new_session=True)
    per_msg = {}
    reason = "exited"
    result_evt = None
    pos = 0
    buf = ""
    while True:
        rc = proc.poll()
        with open(out_path) as f:
            f.seek(pos)
            chunk = f.read()
            pos = f.tell()
        buf += chunk
        lines = buf.split("\n")
        buf = lines.pop()
        for line in lines:
            try:
                ev = json.loads(line)
            except Exception:
                continue
            if ev.get("type") == "assistant":
                m = ev.get("message") or {}
                if m.get("id") and m.get("usage"):
                    per_msg[m["id"]] = m["usage"]
            elif ev.get("type") == "result":
                result_evt = ev
        tok = sum(usage_total(u) for u in per_msg.values())
        el = time.time() - t0
        if rc is not None:
            break
        if el > a.wall:
            reason = "wall_cap"
        elif tok > a.token_cap:
            reason = "token_cap"
        if reason != "exited":
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(30)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
            break
        time.sleep(5)
    wall = time.time() - t0

    # 5. Collect outputs and audit.
    outd = os.path.join(run, "out")
    shutil.copytree(log_dir, outd, dirs_exist_ok=True, ignore=shutil.ignore_patterns("build", "*.img", "*.qcow2", "vmlinux", "*.o", "*.ko.debug"))
    tool_hist, denials, hits = {}, [], []
    pats = [(k, re.compile(p.replace("{MID}", mid).replace("{RUN}", a.run_id))) for k, p in LEAK_PATTERNS]
    for line in open(out_path):
        try:
            ev = json.loads(line)
        except Exception:
            continue
        msg = ev.get("message")
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
    if result_evt:
        denials = result_evt.get("permission_denials") or []
    tok_live = sum(usage_total(u) for u in per_msg.values())
    cost_live = sum(cost_of(u) for u in per_msg.values())
    manifest = {
        "harness_version": HARNESS_VERSION, "allowlist": a.allowlist, "run_id": a.run_id, "mid": mid, "arm": arm["name"], "model": a.model, "effort": a.effort,
        "session_id": session, "exit_reason": reason, "exit_code": proc.returncode,
        "wall_secs": round(wall, 1), "wall_cap": a.wall, "token_cap": a.token_cap,
        "tokens_total_live": tok_live, "api_calls": len(per_msg),
        "tokens_breakdown_live": {k: sum(u.get(k, 0) for u in per_msg.values()) for k in
                                  ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")},
        "cost_usd_list_live": round(cost_live, 2),
        "cli_result_cost_usd": (result_evt or {}).get("total_cost_usd"),
        "cli_result_usage": (result_evt or {}).get("usage"),
        "cli_model_usage": (result_evt or {}).get("modelUsage"),
        "tokens_total_cli": sum(v.get("inputTokens", 0) + v.get("outputTokens", 0) + v.get("cacheReadInputTokens", 0) +
                                v.get("cacheCreationInputTokens", 0) for v in ((result_evt or {}).get("modelUsage") or {}).values()),
        "cost_usd_cli": round(sum(v.get("costUSD", 0) for v in ((result_evt or {}).get("modelUsage") or {}).values()), 2),
        "tool_calls": tool_hist, "permission_denials": len(denials),
        "permission_denials_sample": [d.get("tool_input") for d in denials[:20]],
        "leak_audit_hits": hits,
        "outputs": sorted(os.listdir(outd)),
        "report_present": os.path.exists(os.path.join(outd, "report.md")),
        "patch_present": os.path.exists(os.path.join(outd, "kernel_fix.patch")),
        "hashes": {"prompt": sha(ppath), "arm": sha(a.arm), "offline_env": sha(os.path.join(HARNESS, "offline_env.md")),
                   "bundle_manifest": sha(os.path.join(case, "MANIFEST.json"))},
        "crash_commit": tree["crash_full"], "cutoff": tree["cutoff"],
    }
    json.dump(manifest, open(os.path.join(run, "manifest.json"), "w"), indent=1)
    wd = subprocess.run(["git", "-C", linux, "diff", "HEAD"], capture_output=True, text=True).stdout
    open(os.path.join(run, "worktree.diff"), "w").write(wd)
    shutil.rmtree(log_dir, ignore_errors=True)
    subprocess.run(["git", "--git-dir", os.path.join(case, "linux.git"), "worktree", "remove", "--force", linux])
    print(json.dumps({k: manifest[k] for k in ("run_id", "mid", "arm", "exit_reason", "wall_secs", "tokens_total_live",
                                               "cost_usd_list_live", "report_present", "patch_present")}))


if __name__ == "__main__":
    main()
