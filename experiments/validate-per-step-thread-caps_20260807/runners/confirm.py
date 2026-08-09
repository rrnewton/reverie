import json, re, sys
from pathlib import Path
SLOT = Path("/home/newton/work/dev-hermit/worktrees/soltiming/hermit")
OUT = Path("/home/newton/work/dev-hermit/ignored/soltiming")
DAG = json.loads((SLOT / "ci/dag/portable.json").read_text())
# knees measured this session (n=3 replicated where marked R, else n=1 sweep)
KNEE = {"test.hermit_unit": 4,          # R 6.9x
        "test.cli": 16,                 # R 5.5x
        "test.liteinst_strict": 8,      # R 6.2x
        "test.detcore_misc": 4,         # 1.6x
        "test.arbitrary_binaries": 2,   # 1.25x
        "test.ignored_syscall_regressions": 4,  # 1.4x
        "test.app_strict_verify": 4,    # 1.45x
        "test.hermit_integration": 2}   # 1.18x
# left at shipped value because the measured curve is FLAT (no knee to move to):
#   test.hermit_modes (R 1.0x), test.command_strict_verify (1.0x),
#   test.sabre_examples (1.18x noisy), test.detcore_parallel (1.0x)
def build(name, hermit_guest_cap, apply_threads):
    d = json.loads(json.dumps(DAG))
    d["resource_caps"] = dict(d["resource_caps"])
    d["resource_caps"]["hermit_guest"] = hermit_guest_cap
    if apply_threads:
        for s in d["steps"]:
            t = s["group"] + "." + s["job"]
            if t in KNEE and "--test-threads=" in s["cmd"]:
                s["cmd"] = re.sub(r"--test-threads=\d+", f"--test-threads={KNEE[t]}", s["cmd"])
    p = OUT / f"dag-{name}.json"
    p.write_text(json.dumps(d, indent=1))
    return p
for name, cap, thr in [("c1-knees-only", 1, True),
                       ("c2-knees-plus-hg4", 4, True),
                       ("c3-hg4-only", 4, False)]:
    print(name, build(name, cap, thr))
