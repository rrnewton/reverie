import re, sys
clk = None
rows = []
for line in open(sys.argv[1]):
    m = re.search(r'clock_gettime\(CLOCK_MONOTONIC, 0x[0-9a-f]+ -> \{ tv_sec: (\d+), tv_nsec: (\d+) \}', line)
    if m:
        clk = int(m.group(1)) + int(m.group(2)) / 1e9
        continue
    m = re.search(r'dtid (\d+)\] finish syscall #(\d+): epoll_wait\(\d+, 0x[0-9a-f]+, \d+, (-?\d+)\) = (\S+)', line)
    if m:
        rows.append((int(m.group(2)), m.group(1), int(m.group(3)), m.group(4), clk))
t0 = rows[0][4] if rows else 0
prev = None
for r in rows:
    gap = (r[4] - prev) if (prev is not None and r[4] is not None) else 0
    print(f"#{r[0]} dtid={r[1]} timeout={r[2]} rc={r[3]} clk={r[4]:.6f} gap={gap*1000:.1f}ms")
    prev = r[4]
