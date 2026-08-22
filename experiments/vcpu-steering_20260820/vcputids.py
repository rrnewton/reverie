#!/usr/bin/env python3
"""Ask QEMU which host thread is which vCPU, rather than guessing from thread names.

This build does not name its threads (all four report comm `qemu-system-x86`), and
identifying them by CPU busyness would mean measuring the workload in order to set up
the experiment that measures the workload. `query-cpus-fast` is QEMU's own answer.
"""
import json, socket, sys, time

sock_path = sys.argv[1]
deadline = time.time() + 20
while time.time() < deadline:
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(sock_path)
        break
    except OSError:
        time.sleep(0.05)
else:
    print("FATAL no qmp socket", file=sys.stderr); sys.exit(1)

f = s.makefile("rw")
f.readline()                                  # greeting
f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n"); f.flush(); f.readline()
f.write(json.dumps({"execute": "query-cpus-fast"}) + "\n"); f.flush()
while True:
    reply = json.loads(f.readline())
    if "return" in reply:
        break
for cpu in reply["return"]:
    print(f"{cpu['cpu-index']} {cpu['thread-id']}")
