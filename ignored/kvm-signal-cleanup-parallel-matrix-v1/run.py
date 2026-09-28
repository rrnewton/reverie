from pathlib import Path
import datetime
import hashlib
import json
import os
import subprocess
import time


SOURCE = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent
PLAN = OUT / "PLAN.md"
REVISIONS = {
    "base": "60f2d369b49e6ffbc2b2d9d0f0e55fead0ba6b09",
    "head": "ce442fce6b1ee366e0c3a9cff7fa981ff8893ae4",
}
RUNS_PER_REVISION = 5
COMMAND = [
    "timeout",
    "--signal=TERM",
    "--kill-after=10s",
    "900s",
    "cargo",
    "test",
    "--offline",
    "--locked",
    "-j",
    "2",
    "-p",
    "reverie-kvm",
    "--lib",
    "--",
    "--nocapture",
]


def capture(command, cwd):
    result = subprocess.run(
        command, cwd=cwd, text=True, capture_output=True, check=True
    )
    return result.stdout.strip() or result.stderr.strip()


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


if RUNS_PER_REVISION != 5 or not PLAN.exists():
    raise SystemExit("matrix no longer matches its predeclared plan")

results = []
for label, revision in REVISIONS.items():
    checkout = OUT / f"{label}-checkout"
    if checkout.exists():
        raise SystemExit(f"refusing to reuse matrix checkout: {checkout}")
    subprocess.run(
        ["git", "clone", "--shared", "--no-checkout", str(SOURCE), str(checkout)],
        cwd=SOURCE,
        check=True,
    )
    subprocess.run(
        ["git", "checkout", "--detach", revision], cwd=checkout, check=True
    )
    identity = {
        "head": capture(["git", "rev-parse", "HEAD"], checkout),
        "tree": capture(["git", "rev-parse", "HEAD^{tree}"], checkout),
        "tracked_status": capture(
            ["git", "status", "--porcelain", "--untracked-files=no"], checkout
        ),
    }
    if identity["head"] != revision or identity["tracked_status"]:
        raise SystemExit(f"unexpected {label} checkout identity: {identity}")
    clean = subprocess.run(
        ["cargo", "clean", "-p", "reverie-kvm"],
        cwd=checkout,
        text=True,
        capture_output=True,
    )
    if clean.returncode != 0:
        raise SystemExit(f"failed to clean {label}: {clean.stderr}")
    for attempt in range(1, RUNS_PER_REVISION + 1):
        name = f"{label}-{attempt}"
        environment = os.environ.copy()
        environment["REVERIE_REQUIRE_KVM"] = "1"
        started = datetime.datetime.now(datetime.timezone.utc).isoformat()
        begin = time.monotonic()
        with (OUT / f"{name}.stdout").open("xb") as stdout, (
            OUT / f"{name}.stderr"
        ).open("xb") as stderr:
            result = subprocess.run(
                COMMAND,
                cwd=checkout,
                env=environment,
                stdout=stdout,
                stderr=stderr,
            )
        results.append(
            {
                "revision": label,
                "attempt": attempt,
                "source": identity,
                "started_at": started,
                "elapsed_seconds": time.monotonic() - begin,
                "exit_code": result.returncode,
                "stdout_sha256": sha256(OUT / f"{name}.stdout"),
                "stderr_sha256": sha256(OUT / f"{name}.stderr"),
            }
        )

receipt = {
    "plan_sha256": sha256(PLAN),
    "runner_sha256": sha256(Path(__file__)),
    "logical_cpus": os.cpu_count(),
    "command": COMMAND,
    "environment_additions": {"REVERIE_REQUIRE_KVM": "1"},
    "runs_per_revision": RUNS_PER_REVISION,
    "results": results,
    "completed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
}
with (OUT / "receipt.json").open("x") as output:
    json.dump(receipt, output, indent=2)
    output.write("\n")
print(json.dumps(receipt))
