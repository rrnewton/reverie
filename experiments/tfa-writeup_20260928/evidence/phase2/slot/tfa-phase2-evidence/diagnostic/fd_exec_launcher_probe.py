import os, subprocess, sys
path = "/usr/local/bin/codex_cli/codex"
fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
via = f"/proc/{os.getpid()}/fd/{fd}"
for channel in ("", "latest"):
    env = {k: v for k, v in os.environ.items() if k in {"HOME", "PATH", "USER", "LOGNAME", "LANG"}}
    if channel:
        env["META_CODEX_RELEASE"] = channel
    direct = subprocess.run([path, "--version"], env=env, capture_output=True, text=True, timeout=60)
    pinned = subprocess.run([via, "--version"], executable=via, env=env, capture_output=True, text=True, timeout=60)
    print(repr(channel or "stable"), "direct", direct.returncode, repr(direct.stdout), repr(direct.stderr))
    print(repr(channel or "stable"), "pinned", pinned.returncode, repr(pinned.stdout), repr(pinned.stderr))
    print("identical", (direct.returncode, direct.stdout, direct.stderr) == (pinned.returncode, pinned.stdout, pinned.stderr))
