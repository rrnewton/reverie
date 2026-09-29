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


SOURCE_COMMIT = (
    "fbsource f3319051bb78 third-party/codex 0.156.1 "
    "(upstream b412ff32c417f855c2b2d1581b77058eed87c84b)"
)
FIX_COMMITS = (
    "7cec3e110ce6dc5c14704dfa06f6db9c23a530fc",
    "a708fc683934d7d7ea7560354968fedd2be28581",
)
CANDIDATE = Path("/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-evidence/meta-build/target/release/codex")
CANDIDATE_SHA256 = "f9228becc28fe4414ba4026db00818d7af48be83590cb6c9e857e64371abb88d"
CANDIDATE_SIZE = 1_370_056_488
LOCK_SHA256 = "5c0c1494f8e140dbf9b6b8b7f9601491d2ade7519d9c0601125b385edc1beda0"
RECEIPT = Path("/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-evidence/diagnostic/codex-meta0156-v2-diagnostic-receipt.json")
TESTS = (
    "testinfra.tfa.tests.test_production_containment."
    "ProductionContainmentIntegrationTest."
    "test_real_codex_native_workspace_sandbox_and_tfa_cleanup",
    "testinfra.tfa.tests.test_production_containment."
    "ProductionContainmentIntegrationTest."
    "test_real_codex_candidate_handoff_uses_controller_buck",
    "testinfra.tfa.tests.test_production_containment."
    "ProductionContainmentIntegrationTest."
    "test_real_codex_final_message_cannot_be_redirected_by_tool",
    "testinfra.tfa.tests.test_production_containment."
    "ProductionContainmentIntegrationTest."
    "test_real_codex_reviewer_final_message_cannot_be_redirected",
)


def _record_candidate() -> dict[str, object]:
    metadata = CANDIDATE.lstat()
    digest = hashlib.sha256(CANDIDATE.read_bytes()).hexdigest()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_size != CANDIDATE_SIZE
        or digest != CANDIDATE_SHA256
    ):
        raise RuntimeError("diagnostic Codex candidate identity differs")
    return {
        "build_command": [
            "cargo",
            "build",
            "--release",
            "--locked",
            "--offline",
            "-p",
            "codex-cli",
            "--bin",
            "codex",
        ],
        "cargo_lock_sha256": LOCK_SHA256,
        "fix_commits": list(FIX_COMMITS),
        "owner_approved": False,
        "path": str(CANDIDATE),
        "production_eligible": False,
        "sha256": digest,
        "size": metadata.st_size,
        "source_commit": SOURCE_COMMIT,
        "version": "codex-cli 0.156.1",
    }


def _chain_source(original_wrapper: Path) -> bytes:
    source = f'''#!/usr/bin/python3 -I
import hashlib
import os
import stat
import sys
from pathlib import Path

candidate = Path({str(CANDIDATE)!r})
descriptor = os.open(candidate, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0))
metadata = os.fstat(descriptor)
digest = hashlib.sha256()
offset = 0
while chunk := os.pread(descriptor, 1 << 20, offset):
    digest.update(chunk)
    offset += len(chunk)
if not stat.S_ISREG(metadata.st_mode) or metadata.st_size != {CANDIDATE_SIZE} or digest.hexdigest() != {CANDIDATE_SHA256!r}:
    raise SystemExit("diagnostic Codex candidate identity differs")
os.set_inheritable(descriptor, True)
candidate_fd = f"/proc/self/fd/{{descriptor}}"
original = {str(original_wrapper)!r}
os.execve(original, [original, candidate_fd, *sys.argv[2:]], dict(os.environ))
'''
    return source.encode()


def main() -> int:
    candidate_record = _record_candidate()
    original_compatibility = dict(process_module._CODEX_COMPATIBILITY)
    original_preflight = process_module._codex_environment_preflight
    original_pinned_runtime = process_module._pinned_runtime
    original_spawn_contained = process_module._spawn_contained
    wrapper_records: list[dict[str, object]] = []
    launches: dict[str, int] = {"codex_substituted": 0, "non_codex_unchanged": 0}
    spawn_state = threading.local()

    # Only a Codex launch hands its payload path to META_EXEC_WRAPPER; every other
    # executable is argv[1] of the payload wrapper and must run unchanged.
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
        saved = dict(process_module._CODEX_COMPATIBILITY)
        try:
            process_module._CODEX_COMPATIBILITY.update(
                {key: ("eligible", None) for key in saved}
            )
            identity = original_preflight(cwd, artifact_dir, environment)
        finally:
            process_module._CODEX_COMPATIBILITY.clear()
            process_module._CODEX_COMPATIBILITY.update(saved)
        identity_path = Path(str(identity["path"]))
        record = json.loads(identity_path.read_text())
        record["diagnostic_candidate"] = candidate_record
        record["eligibility_scope"] = "unsigned-local-diagnostic-only"
        record["production_eligible"] = False
        write_metadata(identity_path, record)
        return regular_file_record(identity_path)

    def diagnostic_pinned_runtime() -> object:
        runtime = original_pinned_runtime()
        if not getattr(spawn_state, "codex", False):
            launches["non_codex_unchanged"] += 1
            return runtime
        launches["codex_substituted"] += 1
        content = _chain_source(runtime.wrapper)
        descriptor = os.memfd_create(
            "tfa-codex-diagnostic-wrapper", os.MFD_ALLOW_SEALING | os.MFD_CLOEXEC
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
            wrapper_records.append(
                {
                    "mode": 0o500,
                    "sha256": hashlib.sha256(content).hexdigest(),
                    "size": len(content),
                }
            )
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
        patch.object(
            process_module,
            "_codex_environment_preflight",
            diagnostic_preflight,
        ),
        patch.object(process_module, "_pinned_runtime", diagnostic_pinned_runtime),
        patch.object(process_module, "_spawn_contained", diagnostic_spawn_contained),
    ):
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    if process_module._CODEX_COMPATIBILITY != original_compatibility:
        raise RuntimeError("production Codex compatibility map was not restored")
    receipt = {
        "candidate": candidate_record,
        "errors": len(result.errors),
        "failures": len(result.failures),
        "finished_wall_time_ns": time.time_ns(),
        "launches": launches,
        "production_compatibility_map_restored": True,
        "schema": "tfa.codex-local-diagnostic-receipt.v1",
        "started_wall_time_ns": started,
        "successful": result.wasSuccessful(),
        "tests": list(TESTS),
        "tests_run": result.testsRun,
        "wrapper_records": wrapper_records,
    }
    write_metadata(RECEIPT, receipt)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
