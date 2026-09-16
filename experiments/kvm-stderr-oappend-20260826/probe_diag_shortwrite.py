#!/usr/bin/env python3
"""Does hermit's own verdict survive when a guest makes hermit's stderr nonblocking?

hermit's stderr is a PIPE, shrunk to one page and drained slowly, so it is under
real back-pressure. Run A's guest sets O_NONBLOCK on fd 2; run B's does not.
If the forwarded bit costs hermit its own diagnostics, A loses output B keeps.
"""
import fcntl, os, subprocess, sys, threading, time

HERMIT = sys.argv[1]
BASE = "/home/newton/work/dev-hermit/experiments/kvm-stderr-oappend-20260826"
F_SETPIPE_SZ = 1031

def run(backend, guest, label):
    r, w = os.pipe()
    try:
        fcntl.fcntl(w, F_SETPIPE_SZ, 4096)      # one page: real back-pressure
    except OSError:
        pass
    # PRE-FILL to leave EXACTLY 400 bytes of headroom. hermit's verify
    # diagnostics measured 703-751 bytes, so 400 sits between "one write fits"
    # and "the whole verdict fits" -- the only window where the two arms can
    # differ. Chunked filling overshot to headroom=0 and made both arms block on
    # hermit's first byte, which discriminated nothing.
    HEADROOM = 600
    os.write(w, b"x" * (4096 - HEADROOM))
    prefilled = 4096 - HEADROOM
    captured = bytearray()
    stop = threading.Event()
    def drain():                                 # NO drain during the run
        stop.wait()
        os.set_blocking(r, False)
        while True:
            try:
                chunk = os.read(r, 65536)
                if not chunk: break
                captured.extend(chunk)
            except BlockingIOError:
                break
    t = threading.Thread(target=drain, daemon=True); t.start()
    env = dict(os.environ, FLAG_REPORT=f"{BASE}/diagrep.txt")
    try:
        p = subprocess.run([HERMIT, "--log", "info", "run", "--backend", backend,
                            "--strict", "--verify", "--", guest],
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=w, env=env, timeout=45)
        rc = str(p.returncode)
    except subprocess.TimeoutExpired:
        rc = "TIMEOUT"
    os.close(w); time.sleep(1.0); stop.set(); t.join(timeout=5)
    os.close(r)
    text = bytes(captured).decode(errors="replace")
    verdict = [l for l in text.splitlines() if l.startswith(":: ") and ("Success" in l or "Failure" in l)]
    print(f"  {label:34s} rc={rc:8s} stderr_bytes={len(captured):7d} "
          f"headroom={4096-prefilled:4d} verdict={'PRESENT' if verdict else 'MISSING <<<'}  {verdict[0][:34] if verdict else ''}")

H = HERMIT
for backend in ("kvm",):
    print(f"--- backend={backend}, hermit stderr = 4096-byte pipe, slow drain ---")
    run(backend, f"{BASE}/bin/stdio_flag_report", "guest SETS O_NONBLOCK on fd 2")
    run(backend, "/bin/echo", "control: guest touches nothing")
