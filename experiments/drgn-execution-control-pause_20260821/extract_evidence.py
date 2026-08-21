#!/usr/bin/env python3
"""Extract the kernel's complaint text from the raw serial logs.

The logs total 271 MB, nearly all of it identical boot output and heartbeat
lines, so committing them is out of the question. What a reader actually needs
is the exact wording of each named complaint and the run it came from. That
is what this writes: the complaint line plus a few lines of surrounding
context, once per distinct (run, mechanism).
"""
import glob, re
from pathlib import Path

MECHANISMS = [
    ("soft-lockup detector", re.compile(r"watchdog: BUG: soft lockup")),
    ("RCU stall detector", re.compile(r"rcu: INFO: rcu_\w+ .*stall")),
    ("workqueue watchdog", re.compile(r"BUG: workqueue lockup")),
    ("hrtimer overrun report", re.compile(r"hrtimer: interrupt took")),
    ("clocksource watchdog", re.compile(r"clocksource: .*(?:Long readout interval|as unstable)")),
]

out = []
for log in sorted(glob.glob("runs/*/*/*/serial.log")):
    text = Path(log).read_bytes().decode("utf-8", "replace")
    lines = text.splitlines()
    seen = set()
    for name, pattern in MECHANISMS:
        for index, line in enumerate(lines):
            if not pattern.search(line):
                continue
            if name in seen:
                break
            seen.add(name)
            out.append("### %s\n%s\n\n%s\n" % (
                name, log, "\n".join(lines[index:index + 6])))
            break
Path("EVIDENCE.md").write_text(
    "# Kernel complaint text, extracted from the raw serial logs\n\n"
    "One excerpt per (run, mechanism). The logs themselves are 271 MB of\n"
    "mostly-identical boot output and are not committed; these are the lines\n"
    "that carry the result. Regenerate with `extract_evidence.py` after a run.\n\n"
    + "\n".join(out))
print("excerpts: %d, bytes: %d" % (len(out), len(Path("EVIDENCE.md").read_text())))
