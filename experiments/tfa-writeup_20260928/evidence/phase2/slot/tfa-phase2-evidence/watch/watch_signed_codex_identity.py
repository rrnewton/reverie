#!/usr/bin/python3 -I
"""Read-only, bounded watcher for the owner-managed signed Codex identity.

Every INTERVAL seconds it lstat()s the entrypoint symlink, launcher and both channel
manifests, and hashes a file only when its (inode, size, mtime_ns, ctime_ns) changes.
When any sha256 or the symlink target differs from the sealed baseline, it writes
change-<time>.json and exits 3. Once DEADLINE passes it exits 0 with no change.
It never writes outside this directory and never runs Codex.
"""
import hashlib, json, os, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
INTERVAL = int(os.environ.get("WATCH_INTERVAL", "1800"))
DEADLINE = time.time() + int(os.environ.get("WATCH_SECONDS", "86400"))
BASELINE = {
    "/usr/local/bin/codex_cli/codex": "71b8f5bde99ce8b4cff6ce7c7cbe79a0fe8d6bd36a4427e5d308bda4d065ebf0",
    "/usr/local/bin/codex_cli/codex.real": "66494455a90b6f45f1fb93c73847d610337a12252a8f10a8a393f20ca213ae72",
    "/usr/local/bin/codex_cli/codex_latest.real": "4b776a3e1d72706ab648db6fb24f5baa1a1ceaec2d4e3f07bd408db4dc0ecab5",
}
ENTRYPOINT, ENTRY_TARGET = "/usr/local/bin/codex", "/usr/local/bin/codex_cli/codex"


def sha(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while chunk := handle.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def key(path):
    try:
        s = os.lstat(path)
        return (s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    except OSError as error:
        return ("missing", error.errno)


def observe(cache):
    changes = {}
    for path, expected in BASELINE.items():
        k = key(path)
        if cache.get(path, (None,))[0] != k:
            try:
                cache[path] = (k, sha(path))
            except OSError as error:
                cache[path] = (k, f"unreadable errno={error.errno}")
        if cache[path][1] != expected:
            changes[path] = {"expected": expected, "observed": cache[path][1], "stat": list(k)}
    try:
        target = os.readlink(ENTRYPOINT)
    except OSError as error:
        target = f"unreadable errno={error.errno}"
    if os.path.realpath(ENTRYPOINT) != ENTRY_TARGET:
        changes[ENTRYPOINT] = {"expected_realpath": ENTRY_TARGET, "readlink": target}
    return changes


cache = {}
checks = 0
while True:
    checks += 1
    changes = observe(cache)
    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    with open(HERE / "heartbeat.log", "a") as log:
        log.write(f"{stamp} check={checks} changed={sorted(changes)}\n")
    if changes:
        (HERE / f"change-{stamp}.json").write_text(json.dumps({"at": stamp, "changes": changes}, indent=2) + "\n")
        print(f"IDENTITY CHANGE at {stamp}: {sorted(changes)}")
        sys.exit(3)
    if time.time() + INTERVAL > DEADLINE:
        print(f"no identity change after {checks} checks; deadline reached at {stamp}")
        sys.exit(0)
    time.sleep(INTERVAL)
