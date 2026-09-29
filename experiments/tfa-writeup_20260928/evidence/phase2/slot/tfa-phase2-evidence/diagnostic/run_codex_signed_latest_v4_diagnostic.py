#!/usr/bin/python3 -I

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import stat
import sys
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from testinfra.tfa import process as process_module
from testinfra.tfa.state import regular_file_record, write_metadata


# Diagnostic only: run the four real-Codex cases against the complete signed
# latest payload selected by the owner-managed launcher. No binary is substituted
# or copied; the only production gate relaxed is the sealed compatibility verdict
# for the exact latest identity, restored after the run.
EVIDENCE = Path(
    "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/"
    "ignored/tfa-phase2-evidence/diagnostic"
)
RECEIPT = EVIDENCE / "codex-signed-latest-v4-diagnostic-receipt.json"
LAUNCH_LOG = EVIDENCE / "codex-signed-latest-v4-payload-launches.jsonl"
LATEST_KEY = (
    "codex-cli 0.156.1",
    "71b8f5bde99ce8b4cff6ce7c7cbe79a0fe8d6bd36a4427e5d308bda4d065ebf0",
    "4b776a3e1d72706ab648db6fb24f5baa1a1ceaec2d4e3f07bd408db4dc0ecab5",
    "latest",
)
PAYLOAD_DIR = Path(
    "/home/newton/.cache/dotslash/obj/cas/b1/ebf433a16e27b9f2eea1f23ba9cfce6a/"
    "extract/tar.zst/linux_x86_64"
)
MANIFEST_DIGEST = (
    "ebf433a16e27b9f2eea1f23ba9cfce6aa4fa68a0154724c5c1ced4ea26616da5:98630430"
)
TESTS = tuple(
    "testinfra.tfa.tests.test_production_containment."
    f"ProductionContainmentIntegrationTest.{name}"
    for name in (
        "test_real_codex_native_workspace_sandbox_and_tfa_cleanup",
        "test_real_codex_candidate_handoff_uses_controller_buck",
        "test_real_codex_final_message_cannot_be_redirected_by_tool",
        "test_real_codex_reviewer_final_message_cannot_be_redirected",
    )
)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def _record_payload() -> dict[str, object]:
    manifest = Path(process_module._CODEX_RELEASE_MANIFESTS["latest"][0])
    text = manifest.read_text()
    if f'"digest": "{MANIFEST_DIGEST}"' not in text:
        raise RuntimeError("signed latest manifest does not name the payload digest")
    files = {}
    for relative in (
        "bin/codex",
        "bin/codex-code-mode-host",
        "codex-path/bwrap",
        "codex-package.json",
    ):
        path = PAYLOAD_DIR / relative
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode):
            raise RuntimeError(f"signed payload member is not a regular file: {path}")
        files[relative] = {"sha256": _sha256(path), "size": metadata.st_size}
    return {
        "manifest": str(manifest),
        "manifest_sha256": _sha256(manifest),
        "manifest_linux_x86_64_digest": MANIFEST_DIGEST,
        "payload_dir": str(PAYLOAD_DIR),
        "files": files,
    }


def _recorder_source(original_wrapper: Path) -> bytes:
    return f'''#!/usr/bin/python3 -I
import hashlib
import json
import os
import sys

payload = os.path.realpath(sys.argv[1])
digest = hashlib.sha256()
with open(payload, "rb") as stream:
    while chunk := stream.read(1 << 20):
        digest.update(chunk)
with open({str(LAUNCH_LOG)!r}, "a") as log:
    log.write(json.dumps({{"argv1": sys.argv[1], "realpath": payload, "sha256": digest.hexdigest(), "release": os.environ.get("META_CODEX_RELEASE")}}) + "\\n")
original = {str(original_wrapper)!r}
os.execve(original, [original, *sys.argv[1:]], dict(os.environ))
'''.encode()


def main() -> int:
    payload_record = _record_payload()
    original_compatibility = dict(process_module._CODEX_COMPATIBILITY)
    if original_compatibility.get(LATEST_KEY, ("", None))[0] != "blocked":
        raise RuntimeError("sealed latest compatibility entry differs")
    original_preflight = process_module._codex_environment_preflight
    original_pinned_runtime = process_module._pinned_runtime
    original_spawn_contained = process_module._spawn_contained
    launches: dict[str, int] = {"codex_recorded": 0, "non_codex_unchanged": 0}
    preflights: list[dict[str, object]] = []
    spawn_state = threading.local()

    def diagnostic_spawn_contained(executable: str, *args: object, **kwargs: object) -> object:
        spawn_state.codex = executable == "/usr/local/bin/codex"
        try:
            return original_spawn_contained(executable, *args, **kwargs)
        finally:
            spawn_state.codex = False

    def diagnostic_preflight(
        cwd: Path,
        artifact_dir: Path,
        environment: dict[str, str] | None,
    ) -> dict[str, object]:
        # run_process passes one mutable snapshot to the preflight and then to
        # _spawn_contained, so the launch selects the same channel it verified.
        if environment is None:
            raise RuntimeError("diagnostic requires an explicit environment snapshot")
        environment["META_CODEX_RELEASE"] = "latest"
        saved = dict(process_module._CODEX_COMPATIBILITY)
        try:
            process_module._CODEX_COMPATIBILITY[LATEST_KEY] = ("eligible", None)
            identity = original_preflight(cwd, artifact_dir, environment)
        finally:
            process_module._CODEX_COMPATIBILITY.clear()
            process_module._CODEX_COMPATIBILITY.update(saved)
        identity_path = Path(str(identity["path"]))
        record = json.loads(identity_path.read_text())
        if (
            record.get("release_channel") != "latest"
            or record.get("version") != LATEST_KEY[0]
            or record["resolved_executable"].get("sha256") != LATEST_KEY[1]
            or record["release_manifest"].get("sha256") != LATEST_KEY[2]
        ):
            raise RuntimeError("preflight did not verify the signed latest identity")
        preflights.append(
            {
                "path": str(identity_path),
                "version": record["version"],
                "version_stderr": record.get("version_stderr"),
            }
        )
        record["diagnostic_signed_latest_payload"] = payload_record
        record["eligibility_scope"] = "signed-latest-diagnostic-compatibility-override"
        record["production_eligible"] = False
        record["sealed_state"] = "blocked"
        write_metadata(identity_path, record)
        return regular_file_record(identity_path)

    def diagnostic_pinned_runtime() -> object:
        runtime = original_pinned_runtime()
        if not getattr(spawn_state, "codex", False):
            launches["non_codex_unchanged"] += 1
            return runtime
        launches["codex_recorded"] += 1
        content = _recorder_source(runtime.wrapper)
        descriptor = os.memfd_create(
            "tfa-codex-launch-recorder", os.MFD_ALLOW_SEALING | os.MFD_CLOEXEC
        )
        try:
            os.fchmod(descriptor, 0o500)
            offset = 0
            while offset < len(content):
                written = os.write(descriptor, content[offset:])
                if written <= 0:
                    raise RuntimeError("diagnostic wrapper write made no progress")
                offset += written
            seals = (
                fcntl.F_SEAL_WRITE
                | fcntl.F_SEAL_GROW
                | fcntl.F_SEAL_SHRINK
                | fcntl.F_SEAL_SEAL
            )
            fcntl.fcntl(descriptor, fcntl.F_ADD_SEALS, seals)
            runtime.wrapper = Path(f"/proc/{os.getpid()}/fd/{descriptor}")
            runtime.descriptors += (descriptor,)
            return runtime
        except BaseException:
            os.close(descriptor)
            runtime.close()
            raise

    started = time.time_ns()
    suite = unittest.defaultTestLoader.loadTestsFromNames(TESTS)
    with (
        patch.object(process_module, "_codex_environment_preflight", diagnostic_preflight),
        patch.object(process_module, "_pinned_runtime", diagnostic_pinned_runtime),
        patch.object(process_module, "_spawn_contained", diagnostic_spawn_contained),
    ):
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    if process_module._CODEX_COMPATIBILITY != original_compatibility:
        raise RuntimeError("production Codex compatibility map was not restored")
    payload_launches = [
        json.loads(line) for line in LAUNCH_LOG.read_text().splitlines()
    ] if LAUNCH_LOG.exists() else []
    expected_codex = payload_record["files"]["bin/codex"]["sha256"]
    receipt = {
        "compatibility_override": {"key": list(LATEST_KEY), "sealed": "blocked"},
        "errors": len(result.errors),
        "failures": len(result.failures),
        "finished_wall_time_ns": time.time_ns(),
        "launches": launches,
        "payload": payload_record,
        "payload_launches_all_signed_latest": bool(payload_launches)
        and all(
            row["sha256"] == expected_codex
            and row["realpath"] == str(PAYLOAD_DIR / "bin/codex")
            for row in payload_launches
        ),
        "payload_launches": payload_launches,
        "preflights": preflights,
        "production_compatibility_map_restored": True,
        "production_eligible": False,
        "schema": "tfa.codex-signed-latest-diagnostic-receipt.v1",
        "started_wall_time_ns": started,
        "successful": result.wasSuccessful(),
        "tests": list(TESTS),
        "tests_run": result.testsRun,
    }
    write_metadata(RECEIPT, receipt)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
