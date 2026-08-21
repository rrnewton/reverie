#!/usr/bin/env python3
"""The second guard: establish "nothing complained" by exhaustive diff.

Pool every serial log from the SMP runs, normalise numbers away, take the set
of messages common to EVERY run as the boot set, and list every message
outside it with the runs that produced it. A pattern list can miss a symptom
it was not written for; this cannot.
"""
import collections, glob, re
from pathlib import Path

KLINE = re.compile(r"^\[\s*\d+\.\d+\]\s*(.*?)\s*$")
# Only digit runs and 0x-prefixed hex. An earlier version also
# collapsed any run of 2+ hex characters, which ate the letters
# inside ordinary words ("detected" became "#t#t#").
NUM = re.compile(r"0x[0-9a-fA-F]+|\d+")

def klines(path):
    out = []
    for raw in Path(path).read_bytes().decode("utf-8", "replace").splitlines():
        m = KLINE.match(raw.strip())
        if m:
            out.append(NUM.sub("#", m.group(1)))
    return out

runs = {}
for log in sorted(glob.glob("runs/smp-*/*/*/serial.log") + glob.glob("runs/wrap*/*/*/serial.log")):
    runs[log] = klines(log)
print("serial logs scanned: %d" % len(runs))
common = set.intersection(*(set(v) for v in runs.values())) if runs else set()
print("messages present in EVERY run (the boot set): %d" % len(common))

extra = collections.defaultdict(set)
for path, lines in runs.items():
    label = Path(path).parts[1]
    for line in lines:
        if line not in common:
            extra[line].add(label)

# Register-dump and stack-trace lines are the BODY of a report whose header is
# already classified; collapse them so the interesting set stays readable.
NOISE = re.compile(r"^(R\S*:|Code:|FS:|CS:|CR\d|PKRU:|Call Trace:|</?TASK>|"
                   r"\?|[a-z_0-9]+\+#|Modules linked in:|task:|Hardware name:|"
                   r"CPU: |#:|Showing|pwq |pending:|workqueue |[a-z_]+ #:|rcu: \t|"
                   r"[a-z_0-9]+\(\)|Command line:|Kernel command line:)")
interesting = {k: v for k, v in extra.items() if not NOISE.match(k)}
print("\nnon-boot messages, register/stack-dump body lines collapsed:")
for line, labels in sorted(interesting.items(), key=lambda kv: -len(kv[1])):
    print("  [%s] %s" % (",".join(sorted(labels)), line[:120]))
print("\n(%d further lines were register/stack-dump bodies of the reports above)"
      % (len(extra) - len(interesting)))
