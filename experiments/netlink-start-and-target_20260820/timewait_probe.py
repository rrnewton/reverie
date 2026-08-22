import socket, time
# A TIME_WAIT / listening mix. The kernel assigns no inode to TIME_WAIT sockets,
# so if 0 appears in the real kernel's own output, 0 is an in-range value for
# this field rather than an illegal one.
srv = socket.socket(); srv.bind(("127.0.0.1", 0)); srv.listen(1)
cli = socket.socket(); cli.connect(srv.getsockname())
conn, _ = srv.accept()
cli.close(); conn.close()   # provokes TIME_WAIT
time.sleep(0.2)
zero = tot = 0
for line in open("/proc/net/tcp").read().splitlines()[1:]:
    f = line.split()
    if len(f) > 9:
        tot += 1
        if int(f[9]) == 0: zero += 1
print("rows=%d inode==0 rows=%d" % (tot, zero))
for line in open("/proc/net/tcp").read().splitlines()[1:]:
    f = line.split()
    if len(f) > 9 and int(f[9]) == 0:
        print("  st=%s inode=%s  <- kernel's own zero" % (f[3], f[9])); break
