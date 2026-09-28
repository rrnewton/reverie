from pathlib import Path
import datetime
import hashlib
import json
import os
import platform
import subprocess
import time


ROOT = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent
BASE = "60f2d369b49e6ffbc2b2d9d0f0e55fead0ba6b09"
EXPECTED_HEAD = "ce442fce6b1ee366e0c3a9cff7fa981ff8893ae4"
EXPECTED_TREE = "a83ef7f9e0645b28d3dbe83ed02a32e461674cc7"


def git(*args):
    result = subprocess.run(
        ["git", *args], cwd=ROOT, text=True, capture_output=True, check=True
    )
    return result.stdout.strip()


def capture(*command):
    result = subprocess.run(
        command, cwd=ROOT, text=True, capture_output=True, check=True
    )
    return result.stdout.strip() or result.stderr.strip()


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_identity():
    paths = git("diff", "--name-only", f"{BASE}..HEAD").splitlines()
    return {
        "head": git("rev-parse", "HEAD"),
        "tree": git("rev-parse", "HEAD^{tree}"),
        "tracked_status": git("status", "--porcelain", "--untracked-files=no"),
        "paths": {path: sha256(ROOT / path) for path in paths},
    }


phases = [
    ("clean-package", ["cargo", "clean", "-p", "reverie-kvm"], {}),
    (
        "focused",
        [
            "timeout",
            "--signal=TERM",
            "--kill-after=10s",
            "600s",
            "cargo",
            "test",
            "--offline",
            "--locked",
            "-j",
            "2",
            "-p",
            "reverie-kvm",
            "--lib",
            "runtime::signal_cleanup_completion_tests",
            "--",
            "--nocapture",
        ],
        {"REVERIE_REQUIRE_KVM": "1"},
    ),
    (
        "full-parallel",
        [
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
        ],
        {"REVERIE_REQUIRE_KVM": "1"},
    ),
    (
        "full-serial",
        [
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
            "--test-threads=1",
            "--nocapture",
        ],
        {"REVERIE_REQUIRE_KVM": "1"},
    ),
    ("format", ["cargo", "fmt", "--all", "--", "--check"], {}),
    (
        "check-default",
        [
            "cargo",
            "check",
            "--offline",
            "--locked",
            "-j",
            "2",
            "-p",
            "reverie-kvm",
            "--lib",
        ],
        {},
    ),
    (
        "clippy-default",
        [
            "cargo",
            "clippy",
            "--offline",
            "--locked",
            "-j",
            "2",
            "-p",
            "reverie-kvm",
            "--lib",
            "--",
            "-D",
            "warnings",
        ],
        {},
    ),
    (
        "check-native-test-support",
        [
            "cargo",
            "check",
            "--offline",
            "--locked",
            "-j",
            "2",
            "-p",
            "reverie-kvm",
            "--lib",
            "--features",
            "native-test-support",
        ],
        {},
    ),
    (
        "clippy-native-test-support",
        [
            "cargo",
            "clippy",
            "--offline",
            "--locked",
            "-j",
            "2",
            "-p",
            "reverie-kvm",
            "--lib",
            "--features",
            "native-test-support",
            "--",
            "-D",
            "warnings",
        ],
        {},
    ),
    (
        "clippy-all-targets-all-features",
        [
            "cargo",
            "clippy",
            "--offline",
            "--locked",
            "-j",
            "2",
            "-p",
            "reverie-kvm",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        {},
    ),
]


before = source_identity()
if before["head"] != EXPECTED_HEAD or before["tree"] != EXPECTED_TREE:
    raise SystemExit(f"unexpected source before qualification: {before}")
if before["tracked_status"]:
    raise SystemExit(f"tracked source is dirty before qualification: {before['tracked_status']}")

results = []
for name, command, additions in phases:
    environment = os.environ.copy()
    environment.update(additions)
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    begin = time.monotonic()
    with (OUT / f"{name}.stdout").open("xb") as stdout, (
        OUT / f"{name}.stderr"
    ).open("xb") as stderr:
        result = subprocess.run(
            command,
            cwd=ROOT,
            env=environment,
            stdout=stdout,
            stderr=stderr,
        )
    results.append(
        {
            "name": name,
            "command": command,
            "environment_additions": additions,
            "started_at": started,
            "elapsed_seconds": time.monotonic() - begin,
            "exit_code": result.returncode,
            "stdout_sha256": sha256(OUT / f"{name}.stdout"),
            "stderr_sha256": sha256(OUT / f"{name}.stderr"),
        }
    )
    if result.returncode != 0:
        break

after = source_identity()
receipt = {
    "base": BASE,
    "expected_head": EXPECTED_HEAD,
    "expected_tree": EXPECTED_TREE,
    "runner_sha256": sha256(Path(__file__)),
    "environment": {
        "platform": platform.platform(),
        "rustc": capture("rustc", "-Vv"),
        "cargo": capture("cargo", "-Vv"),
    },
    "before": before,
    "after": after,
    "source_unchanged": before == after,
    "phases": results,
    "all_passed": len(results) == len(phases)
    and all(result["exit_code"] == 0 for result in results),
    "completed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
}
with (OUT / "receipt.json").open("x") as output:
    json.dump(receipt, output, indent=2)
    output.write("\n")
print(json.dumps(receipt))
if not receipt["source_unchanged"] or not receipt["all_passed"]:
    raise SystemExit(1)
