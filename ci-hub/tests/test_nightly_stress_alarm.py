#!/usr/bin/env python3
"""End-to-end bracket for the nightly-stress P0 alarm path.

WHY THIS EXISTS. The P0 escalation in `ci-hub/stress/nightly.sh` was a silent
no-op for as long as it existed: it called `status-log.rs --append`, a flag that
script has never accepted, wrapped in `>/dev/null 2>&1 || true`, so the refusal
and the non-zero exit were both discarded. The alarm path *reported success
while emitting nothing*. That was corrected by hand, but nothing committed
proved it, so the next refactor could reintroduce the same shape unnoticed.
These tests are that proof, and they are the regression guard.

INERT BY CONSTRUCTION — no user-facing message is ever sent:
  * the harness runs against a THROWAWAY ROOT built in tmp_path, never the real
    repository, so markers land in a temp `ignored/` and the real durable store
    is untouched;
  * `tg` is replaced on PATH by a stub that records its argv to a file instead
    of posting a TaskGraph note, so escalation is *observed* without delivering;
  * the burst primitive is a stub emitting the documented CSV contract, so no
    Hermit workload is built or executed.
Nothing here reaches GChat, TaskGraph, GitHub, or the network.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
STRESS_DIR = REPO_ROOT / "ci-hub" / "stress"

# Burst CSV contract, per ci-hub/stress/stress_store.py:
#   sha,short,build_s,burst_N,hangs,passes,other,hang_rate,STATUS
# CLEAN  = every OK burst had passes == N (0 hangs, 0 others)   -> exit 0
# FLAKY  = some OK burst had 0 < passes < N                     -> exit 2, P0
CSV_CLEAN = "{sha},short,1,4,0,4,0,0.0,OK"
CSV_FLAKY = "{sha},short,1,4,1,3,0,0.25,OK"
CSV_INCOMPLETE = "{sha},short,1,4,0,3,0,0.0,OK"

FAKE_SHA = "0123456789abcdef0123456789abcdef01234567"


def _harness(tmp_path: Path, burst_row: str, *, detail_suffix: str = "") -> dict:
    """Run nightly.sh in an isolated ROOT and return what it produced."""
    root = tmp_path / "root"
    (root / "ci-hub" / "stress").mkdir(parents=True)
    for name in ("nightly.sh", "stress_rows.py", "stress_store.py"):
        shutil.copy2(STRESS_DIR / name, root / "ci-hub" / "stress" / name)

    # The driver reads SHA from `git -C $ROOT/hermit rev-parse`; an empty SHA
    # makes the recorder refuse before the alarm path is ever reached. A local
    # throwaway repo keeps the test hermetic (no clone, no network).
    hermit = root / "hermit"
    hermit.mkdir()
    _git = lambda *a: subprocess.run(
        ["git", "-C", str(hermit), *a], capture_output=True, text=True, check=True
    )
    _git("init", "-q")
    _git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q",
         "--allow-empty", "-m", "fixture")
    _git("update-ref", "refs/remotes/origin/main", "HEAD")
    sha = _git("rev-parse", "HEAD").stdout.strip()

    # Stub burst primitive: emits the CSV contract, builds and runs nothing.
    burst = tmp_path / "burst.sh"
    burst.write_text(
        "#!/usr/bin/env bash\n"
        f'printf "%s\\n" "{burst_row.format(sha="$1")}{detail_suffix}"\n'
    )
    burst.chmod(0o755)

    # Stub `tg`: RECORDS the escalation instead of delivering it. This is what
    # makes the escalation assertion possible without posting a real note.
    stub_bin = tmp_path / "bin"
    stub_bin.mkdir()
    tg_log = tmp_path / "tg-calls.txt"
    tg_stub = stub_bin / "tg"
    tg_stub.write_text(
        "#!/usr/bin/env bash\n"
        f'printf "%s\\n" "$*" >> {tg_log}\n'
        "exit 0\n"
    )
    tg_stub.chmod(0o755)

    env = dict(os.environ)
    env.update(
        {
            "PATH": f"{stub_bin}:{env.get('PATH', '')}",
            "STRESS_BURST_CMD": str(burst),
            "STRESS_WORKLOADS": "probe-workload",
            "STRESS_REPO": "rrnewton/hermit",
            "STRESS_WIDTH": "4",
            "STRESS_TIMEOUT": "1",
            "STRESS_REPS": "1",
            "CI_HUB_STRESS_STORE": str(root / "ignored" / "ci-hub" / "stress-runs.jsonl"),
        }
    )
    proc = subprocess.run(
        ["bash", str(root / "ci-hub" / "stress" / "nightly.sh")],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(root),
        timeout=180,
    )
    markers = sorted((root / "ignored" / "ci-hub").glob("stress-alarm-*.json"))
    return {
        "rc": proc.returncode,
        "out": proc.stdout + proc.stderr,
        "markers": markers,
        "tg_calls": tg_log.read_text().splitlines() if tg_log.exists() else [],
        "sha": sha,
    }


def test_shellcheck_free_syntax() -> None:
    """The driver must at least parse; a syntax error would fail every case
    below for the wrong reason."""
    proc = subprocess.run(
        ["bash", "-n", str(STRESS_DIR / "nightly.sh")],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr


def test_real_red_emits_one_typed_durable_p0_record(tmp_path: Path) -> None:
    """POSITIVE: a genuine nightly RED must leave a typed, parseable record."""
    result = _harness(tmp_path, CSV_FLAKY)

    assert len(result["markers"]) == 1, (
        f"expected exactly one durable P0 record, got {result['markers']}\n"
        f"{result['out']}"
    )
    record = json.loads(result["markers"][0].read_text())

    # TYPED: the fields a triager needs, present and populated.
    for key in ("ts", "repo", "sha", "workload", "verdict", "detail", "severity"):
        assert key in record, f"record is missing {key}: {record}"
        assert str(record[key]).strip() != "", f"record has empty {key}: {record}"
    assert record["severity"] == "P0"
    assert record["repo"] == "rrnewton/hermit"
    assert record["sha"] == result["sha"]
    assert record["workload"] == "probe-workload"
    assert record["verdict"] in {"FLAKY", "FAILING", "ERROR"}, record["verdict"]

    # The run must actually report P0 rather than exiting green.
    assert result["rc"] != 0, f"a RED nightly must not exit 0\n{result['out']}"
    assert "P0 alarm record persisted" in result["out"]


def test_clean_control_emits_no_record_at_all(tmp_path: Path) -> None:
    """NEGATIVE CONTROL: a green nightly must emit NO P0 record.

    Without this, a harness that wrote a marker unconditionally would pass the
    positive test above while making every night look red.
    """
    result = _harness(tmp_path, CSV_CLEAN)

    assert result["markers"] == [], (
        f"a CLEAN nightly must leave no P0 record, found {result['markers']}\n"
        f"{result['out']}"
    )
    assert result["tg_calls"] == [], (
        f"a CLEAN nightly must not escalate, got {result['tg_calls']}"
    )
    assert result["rc"] == 0, f"a CLEAN nightly must exit 0\n{result['out']}"


def test_incomplete_ok_row_is_a_named_error_not_a_false_green(tmp_path: Path) -> None:
    """An OK row must account for every promised burst instance."""
    result = _harness(tmp_path, CSV_INCOMPLETE)

    assert result["rc"] != 0, result["out"]
    assert len(result["markers"]) == 1, result["out"]
    record = json.loads(result["markers"][0].read_text())
    assert record["verdict"] == "ERROR"
    assert "stress_store refused the burst row" in record["detail"]
    assert "🟢" not in result["out"]


def test_escalation_is_delivered_and_acknowledged(tmp_path: Path) -> None:
    """The escalation must be OBSERVED, not fire-and-forget.

    The original defect was indistinguishable from success; this asserts the
    call actually happens and that the driver says so.
    """
    result = _harness(tmp_path, CSV_FLAKY)
    assert len(result["tg_calls"]) == 1, (
        f"expected exactly one escalation, got {result['tg_calls']}\n{result['out']}"
    )
    assert "note" in result["tg_calls"][0]
    assert "P0 NIGHTLY-STRESS RED" in result["tg_calls"][0]
    assert "P0 alarm escalated" in result["out"]


def test_record_stays_valid_json_when_detail_contains_quotes(tmp_path: Path) -> None:
    """A quote in the stress detail must not corrupt the durable record.

    The record used to be printf-interpolated, so a `"` in a workload or detail
    string produced a marker no consumer could parse — the alarm's own evidence
    was corrupt exactly when the detail was most interesting.
    """
    result = _harness(tmp_path, CSV_FLAKY, detail_suffix=' say \\"boom\\" \\\\ end')

    assert len(result["markers"]) == 1, result["out"]
    # Must PARSE — this is the assertion that fails on the printf version.
    record = json.loads(result["markers"][0].read_text())
    assert record["severity"] == "P0"
    assert "NOT VALID JSON" not in result["out"], result["out"]


def test_status_log_is_not_called_from_the_alarm_path() -> None:
    """The removed sink must stay removed.

    `status-log.rs` appends COORDINATOR HOURLY STATUS rows and requires a
    validated workstream mapping plus hourly denominators. A stress alarm has
    none of those, and the old call used a flag that never existed. Any live
    invocation here is a regression; the explanatory comment is expected.
    """
    source = (STRESS_DIR / "nightly.sh").read_text()
    live = [
        line
        for line in source.splitlines()
        if "status-log" in line and not line.lstrip().startswith("#")
    ]
    assert live == [], f"status-log.rs must not be invoked from the alarm path: {live}"
    live_append = [
        line
        for line in source.splitlines()
        if "--append" in line and not line.lstrip().startswith("#")
    ]
    assert live_append == [], f"the never-existent --append flag reappeared: {live_append}"


def test_alarm_refuses_an_undescribable_invocation(tmp_path: Path) -> None:
    """raise_alarm must refuse loudly rather than raise a contentless P0."""
    root = tmp_path / "root"
    (root / "ci-hub" / "stress").mkdir(parents=True)
    shutil.copy2(STRESS_DIR / "nightly.sh", root / "ci-hub" / "stress" / "nightly.sh")
    script = root / "ci-hub" / "stress" / "nightly.sh"

    # Extract raise_alarm alone and supply the few globals it reads. Sourcing
    # the driver is not an option: it logs and runs the probe loop at load, so
    # a unit-level refusal check has to isolate the function.
    text = script.read_text()
    start = text.index("raise_alarm() {")
    end = text.index("\n}\n", start) + len("\n}\n")
    harness = tmp_path / "harness.sh"
    harness.write_text(
        "set -uo pipefail\n"
        f'ROOT="{root}"\nREPO=r\nSHA=s\nALARM_TASK=t\noverall_alarm=0\n'
        'ts() { echo 2026-01-01T00:00:00Z; }\n'
        'log() { echo "$*"; }\n'
        'attribute_capture() { echo ""; }\n'
        + text[start:end]
        + '\nraise_alarm "only-one-arg"\necho "rc=$?"\n'
    )

    proc = subprocess.run(
        ["bash", str(harness)], capture_output=True, text=True, timeout=60
    )
    assert "INVOCATION ERROR" in proc.stdout + proc.stderr, proc.stdout + proc.stderr
    assert "rc=2" in proc.stdout, proc.stdout


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(pytest.main([__file__]))
