import subprocess, sys
H = "/home/newton/work/dev-hermit/hermit/target/release/hermit"
print("%-10s %14s %14s %14s %10s" % ("multiplier","monotonic_d","tsc_d(ns)","rusage_d","mono/tsc"))
for m in sys.argv[1:]:
    cmd = [H, "run"] + ([] if m == "none" else ["--clock-multiplier", m]) + \
          ["--", "./clockprobe", "4", "5000000"]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=600).stdout
    rows = [l.split() for l in out.strip().splitlines() if l and l[0].isdigit()]
    if len(rows) < 2:
        print("%-10s FAILED" % m); continue
    a, b = rows[0], rows[-1]
    mono = float(b[1]) - float(a[1])
    tsc = (int(b[6]) - int(a[6])) / 1e9
    ru = float(b[7]) - float(a[7])
    print("%-10s %14.6f %14.6f %14.6f %10.3f" % (m, mono, tsc, ru, mono/tsc if tsc else 0))
