#!/usr/bin/env python3
"""Brackets for the read-only Chat bridge resource gate."""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import threading
import time
from types import SimpleNamespace

import pytest


HEALTH = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "chat_bridge_health", HEALTH / "chat_bridge_health.py"
)
assert SPEC is not None and SPEC.loader is not None
probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = probe
SPEC.loader.exec_module(probe)

NOW_NS = 100_000 * probe.NSEC_PER_SEC
CHAT = "agentctl-chat-example.service"
EVENTS = "agentctl-gchat-events-example.service"
INVOCATION_A = "a" * 32
INVOCATION_B = "b" * 32


def unit_properties(name: str, **changes: str) -> dict[str, str]:
    values = {
        "Id": name,
        "LoadState": "loaded",
        "UnitFileState": "enabled",
        "ActiveState": "active",
        "SubState": "running",
        "MemoryMax": str(768 * 1024 * 1024),
        "MemorySwapMax": str(128 * 1024 * 1024),
        "TasksMax": "256",
        "LimitNOFILE": "1024",
        "LimitCORE": "0",
        "KillMode": "control-group",
        "OOMPolicy": "kill",
        "AllowedCPUs": "0-3",
        "Restart": (
            "always" if name.startswith(probe.EVENT_UNIT_PREFIX) else "on-failure"
        ),
        "StartLimitBurst": "5",
        "StartLimitIntervalUSec": "10min",
        "TimeoutStopUSec": (
            "30s" if name.startswith(probe.EVENT_UNIT_PREFIX) else "2min 30s"
        ),
        "MemoryAccounting": "yes",
        "TasksAccounting": "yes",
        "IOAccounting": "yes",
        "IOWriteBytes": "1000",
        "CPUUsageNSec": "2000",
        "InvocationID": INVOCATION_A,
    }
    values.update(changes)
    return values


class FakeSystemd:
    def __init__(
        self,
        units: dict[str, dict[str, str]] | None = None,
        *,
        installed: tuple[str, ...] | None = None,
        loaded: tuple[str, ...] | None = None,
        barrier: threading.Barrier | None = None,
    ) -> None:
        self.units = units or {}
        names = tuple(self.units)
        self.installed = names if installed is None else installed
        self.loaded = names if loaded is None else loaded
        self.barrier = barrier
        self.barrier_used = False
        self.calls: list[tuple[str, ...]] = []

    def __call__(self, args: tuple[str, ...]) -> str:
        self.calls.append(args)
        if args[0] == "list-unit-files":
            return "".join(f"{name} enabled enabled\n" for name in self.installed)
        if args[0] == "list-units":
            if self.barrier is not None and not self.barrier_used:
                self.barrier_used = True
                self.barrier.wait(timeout=5)
            return "".join(
                f"{name} loaded active running fixture\n" for name in self.loaded
            )
        if args[0] == "show":
            requested = [token for token in args if token in self.units]
            blocks = []
            for name in requested:
                values = self.units[name]
                blocks.append(
                    "\n".join(f"{key}={values[key]}" for key in probe.SHOW_PROPERTIES)
                )
            return "\n\n".join(blocks) + ("\n" if blocks else "")
        raise AssertionError(args)


def state_directory(root: Path) -> Path:
    return root / probe.STATE_DIRECTORY


def state_path(root: Path) -> Path:
    return state_directory(root) / probe.STATE_NAME


def lock_path(root: Path) -> Path:
    return state_directory(root) / probe.LOCK_NAME


def run(
    root: Path,
    systemd: FakeSystemd,
    *,
    now_ns: int = NOW_NS,
) -> probe.Report:
    return probe.evaluate(root=root, run=systemd, now_ns=now_ns)


def saved_state(root: Path) -> dict[str, object]:
    return json.loads(state_path(root).read_text(encoding="utf-8"))


def seed_state(
    root: Path,
    *,
    observed_ns: int,
    rows: dict[str, tuple[int, int, str]],
) -> None:
    state_directory(root).mkdir(mode=0o700, parents=True, exist_ok=True)
    state_path(root).write_text(
        json.dumps(
            {
                "version": probe.STATE_VERSION,
                "observed_at_unix_ns": observed_ns,
                "units": {
                    name: {
                        "io_write_bytes": counters[0],
                        "cpu_usage_nsec": counters[1],
                        "invocation_id": counters[2],
                    }
                    for name, counters in rows.items()
                },
            }
        ),
        encoding="utf-8",
    )
    state_path(root).chmod(0o600)


def oversized_systemd() -> FakeSystemd:
    units = {CHAT: unit_properties(CHAT)}
    units.update(
        {
            f"agentctl-chat-u{index:05d}.service": unit_properties(
                f"agentctl-chat-u{index:05d}.service"
            )
            for index in range(8500)
        }
    )
    counters = {
        name: probe.Counters(1000, 2000, INVOCATION_A) for name in units
    }
    with pytest.raises(probe.EvidenceUnavailable, match="next baseline is"):
        probe.serialize_baseline(NOW_NS, counters)
    return FakeSystemd(units)


def test_no_matching_units_is_healthy_and_never_touches_bad_state(
    tmp_path: Path,
) -> None:
    directory = state_directory(tmp_path)
    directory.mkdir(mode=0o700)
    state_path(tmp_path).write_text("{not-json", encoding="utf-8")
    state_path(tmp_path).chmod(0o400)
    directory.chmod(0o500)
    before = state_path(tmp_path).read_bytes()

    systemd = FakeSystemd()
    report = run(tmp_path, systemd)
    assert report.returncode == 0
    assert report.fields["state"] == "ok"
    assert report.fields["summary"] == (
        "no matching Chat bridge user services are installed or loaded"
    )
    assert report.fields["units_discovered"] == 0
    assert report.fields["units_enabled"] == 0
    assert report.fields["units_active"] == 0
    assert report.fields["units_failed"] == 0
    assert report.fields["policy_issues"] == "none"
    assert report.fields["sample_status"] == "no-units"
    assert report.fields["io_write_limit_bytes_per_hour"] == 5 * probe.GIB
    assert state_path(tmp_path).read_bytes() == before
    assert not lock_path(tmp_path).exists()
    assert [call[0] for call in systemd.calls] == [
        "list-unit-files",
        "list-units",
    ]


def test_first_active_sample_records_private_state_and_lock(tmp_path: Path) -> None:
    report = run(tmp_path, FakeSystemd({CHAT: unit_properties(CHAT)}))
    assert report.returncode == 0
    assert report.fields["state"] == "ok"
    assert report.fields["units_discovered"] == 1
    assert report.fields["units_enabled"] == 1
    assert report.fields["units_active"] == 1
    assert report.fields["units_failed"] == 0
    assert report.fields["policy_issues"] == "none"
    assert report.fields["sample_status"] == "baseline-missing"
    assert report.fields["sample_elapsed_secs"] == 0
    assert report.fields["compared_units"] == 0
    assert report.fields["baseline_units"] == 1
    assert report.fields["invocation_reset_units"] == "none"
    assert report.fields["counter_reset_units"] == "none"
    assert report.fields["counter_decrease_units"] == "none"
    assert report.fields["io_write_delta_bytes"] == 0
    assert report.fields["io_write_rate_bytes_per_hour"] == 0
    assert report.fields["io_write_limit_bytes_per_hour"] == 5 * probe.GIB
    assert report.fields["cpu_delta_nsec"] == 0
    assert report.fields["cpu_millicores"] == 0
    assert report.fields["runaway_units"] == "none"
    assert stat.S_IMODE(state_path(tmp_path).stat().st_mode) == 0o600
    assert stat.S_IMODE(lock_path(tmp_path).stat().st_mode) == 0o600
    assert saved_state(tmp_path)["units"] == {
        CHAT: {
            "cpu_usage_nsec": 2000,
            "io_write_bytes": 1000,
            "invocation_id": INVOCATION_A,
        }
    }


def test_normal_write_and_cpu_deltas_are_compared(tmp_path: Path) -> None:
    earlier = NOW_NS - 1800 * probe.NSEC_PER_SEC
    seed_state(
        tmp_path,
        observed_ns=earlier,
        rows={CHAT: (1000, 2000, INVOCATION_A)},
    )
    props = unit_properties(
        CHAT,
        IOWriteBytes=str(1000 + 64 * 1024 * 1024),
        CPUUsageNSec=str(2000 + 90 * probe.NSEC_PER_SEC),
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 0
    assert report.fields["sample_status"] == "compared"
    assert report.fields["io_write_delta_bytes"] == 64 * 1024 * 1024
    assert report.fields["io_write_rate_bytes_per_hour"] == 128 * 1024 * 1024
    assert report.fields["io_write_limit_bytes_per_hour"] == 5 * probe.GIB
    assert report.fields["cpu_delta_nsec"] == 90 * probe.NSEC_PER_SEC
    assert report.fields["cpu_millicores"] == 50


def test_write_limit_is_exactly_five_gib_per_hour() -> None:
    assert probe.WRITE_LIMIT_BYTES_PER_HOUR == 5 * probe.GIB


@pytest.mark.parametrize(
    ("delta", "expected_rc"),
    [
        (5 * probe.GIB - 1, 0),
        (5 * probe.GIB, 1),
        (5 * probe.GIB + 1, 1),
    ],
    ids=("one-byte-below", "exact-boundary", "one-byte-above"),
)
def test_write_rate_boundary_is_inclusive_and_emits_exact_limit(
    tmp_path: Path, delta: int, expected_rc: int
) -> None:
    seed_state(
        tmp_path,
        observed_ns=NOW_NS - 3600 * probe.NSEC_PER_SEC,
        rows={CHAT: (1000, 2000, INVOCATION_A)},
    )
    props = unit_properties(
        CHAT,
        IOWriteBytes=str(1000 + delta),
        CPUUsageNSec="2000",
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == expected_rc
    assert report.fields["io_write_delta_bytes"] == delta
    assert report.fields["io_write_rate_bytes_per_hour"] == delta
    assert report.fields["io_write_limit_bytes_per_hour"] == 5 * probe.GIB
    assert report.fields["runaway_units"] == (
        CHAT if delta >= 5 * probe.GIB else "none"
    )


def test_measured_604_3_gib_over_12_hours_can_never_age_into_health(
    tmp_path: Path,
) -> None:
    elapsed = 12 * 3600
    seed_state(
        tmp_path,
        observed_ns=NOW_NS - elapsed * probe.NSEC_PER_SEC,
        rows={CHAT: (0, 0, INVOCATION_A)},
    )
    incident_bytes = 6043 * probe.GIB // 10
    props = unit_properties(
        CHAT,
        IOWriteBytes=str(incident_bytes),
        CPUUsageNSec=str(6000 * probe.NSEC_PER_SEC),
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 1
    assert report.fields["sample_elapsed_secs"] == elapsed
    assert report.fields["sample_status"] == "compared"
    assert report.fields["io_write_delta_bytes"] == incident_bytes
    assert report.fields["io_write_rate_bytes_per_hour"] > 49 * probe.GIB
    assert report.fields["io_write_limit_bytes_per_hour"] == 5 * probe.GIB
    assert report.fields["runaway_units"] == CHAT
    assert "aggregate-write-rate" in str(report.fields["summary"])


def test_long_window_same_invocation_passes_only_when_rate_is_low(
    tmp_path: Path,
) -> None:
    elapsed = 24 * 3600
    seed_state(
        tmp_path,
        observed_ns=NOW_NS - elapsed * probe.NSEC_PER_SEC,
        rows={CHAT: (0, 0, INVOCATION_A)},
    )
    props = unit_properties(
        CHAT,
        IOWriteBytes=str(100 * probe.GIB),
        CPUUsageNSec=str(3600 * probe.NSEC_PER_SEC),
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 0
    assert report.fields["sample_status"] == "compared"
    assert report.fields["io_write_rate_bytes_per_hour"] == 100 * probe.GIB // 24
    assert report.fields["io_write_rate_bytes_per_hour"] < 5 * probe.GIB


def test_aggregate_rate_catches_multiple_subthreshold_writers(tmp_path: Path) -> None:
    earlier = NOW_NS - 1800 * probe.NSEC_PER_SEC
    seed_state(
        tmp_path,
        observed_ns=earlier,
        rows={
            CHAT: (0, 0, INVOCATION_A),
            EVENTS: (0, 0, INVOCATION_A),
        },
    )
    units = {
        CHAT: unit_properties(CHAT, IOWriteBytes=str(3 * probe.GIB // 2)),
        EVENTS: unit_properties(EVENTS, IOWriteBytes=str(3 * probe.GIB // 2)),
    }
    report = run(tmp_path, FakeSystemd(units))
    assert report.returncode == 1
    assert report.fields["io_write_rate_bytes_per_hour"] == 6 * probe.GIB
    assert report.fields["runaway_units"] == "none"


@pytest.mark.parametrize("decreased", ["io", "cpu"])
def test_same_invocation_counter_decrease_refuses_without_state_write(
    tmp_path: Path, decreased: str
) -> None:
    earlier = NOW_NS - 1800 * probe.NSEC_PER_SEC
    seed_state(
        tmp_path,
        observed_ns=earlier,
        rows={CHAT: (10_000, 20_000, INVOCATION_A)},
    )
    before_bytes = state_path(tmp_path).read_bytes()
    before_mtime = state_path(tmp_path).stat().st_mtime_ns
    props = unit_properties(
        CHAT,
        IOWriteBytes="9999" if decreased == "io" else "10000",
        CPUUsageNSec="19999" if decreased == "cpu" else "20000",
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 2
    assert report.fields["state"] == "unknown"
    assert report.fields["sample_status"] == "inconsistent-counter-decrease"
    assert report.fields["counter_reset_units"] == "none"
    assert report.fields["counter_decrease_units"] == CHAT
    assert report.fields["io_write_delta_bytes"] == 0
    assert "monotonic CPU/I/O counter decreased" in str(report.fields["summary"])
    assert state_path(tmp_path).read_bytes() == before_bytes
    assert state_path(tmp_path).stat().st_mtime_ns == before_mtime


def test_changed_invocation_is_a_clean_reset_even_when_counters_grow(
    tmp_path: Path,
) -> None:
    earlier = NOW_NS - 1800 * probe.NSEC_PER_SEC
    seed_state(
        tmp_path,
        observed_ns=earlier,
        rows={CHAT: (1000, 2000, INVOCATION_A)},
    )
    props = unit_properties(
        CHAT,
        InvocationID=INVOCATION_B,
        IOWriteBytes=str(100 * probe.GIB),
        CPUUsageNSec=str(1000 * probe.NSEC_PER_SEC),
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 0
    assert report.fields["sample_status"] == "invocation-reset-baseline"
    assert report.fields["invocation_reset_units"] == CHAT


def test_older_sample_refuses_and_never_overwrites_newer_state(tmp_path: Path) -> None:
    seed_state(
        tmp_path,
        observed_ns=NOW_NS,
        rows={CHAT: (5 * probe.GIB, 10_000, INVOCATION_A)},
    )
    before = state_path(tmp_path).read_bytes()
    report = run(
        tmp_path,
        FakeSystemd({CHAT: unit_properties(CHAT)}),
        now_ns=NOW_NS - probe.NSEC_PER_SEC,
    )
    assert report.returncode == 2
    assert report.fields["sample_status"] == "nonmonotonic-sample-time"
    assert state_path(tmp_path).read_bytes() == before


def test_invalid_private_baseline_refuses_once_and_repairs_it(tmp_path: Path) -> None:
    state_directory(tmp_path).mkdir(mode=0o700)
    state_path(tmp_path).write_text("{not-json", encoding="utf-8")
    state_path(tmp_path).chmod(0o600)
    report = run(tmp_path, FakeSystemd({CHAT: unit_properties(CHAT)}))
    assert report.returncode == 2
    assert report.fields["sample_status"] == "baseline-invalid-replaced"
    assert saved_state(tmp_path)["version"] == probe.STATE_VERSION
    assert stat.S_IMODE(state_path(tmp_path).stat().st_mode) == 0o600


def test_oversize_first_baseline_refuses_without_state_or_temp_residue(
    tmp_path: Path,
) -> None:
    report = run(tmp_path, oversized_systemd())
    assert report.returncode == 2
    assert report.fields["state"] == "unknown"
    assert "next baseline is" in str(report.fields["summary"])
    assert "above 1048576 byte bound" in str(report.fields["summary"])
    assert not state_path(tmp_path).exists()
    assert lock_path(tmp_path).is_file()
    assert list(state_directory(tmp_path).glob(f".{probe.STATE_NAME}.*")) == []


def test_oversize_next_baseline_preserves_existing_state_byte_for_byte(
    tmp_path: Path,
) -> None:
    seed_state(
        tmp_path,
        observed_ns=NOW_NS - probe.NSEC_PER_SEC,
        rows={CHAT: (1000, 2000, INVOCATION_A)},
    )
    before = state_path(tmp_path).read_bytes()
    before_mtime = state_path(tmp_path).stat().st_mtime_ns
    report = run(tmp_path, oversized_systemd())
    assert report.returncode == 2
    assert "next baseline is" in str(report.fields["summary"])
    assert state_path(tmp_path).read_bytes() == before
    assert state_path(tmp_path).stat().st_mtime_ns == before_mtime
    assert list(state_directory(tmp_path).glob(f".{probe.STATE_NAME}.*")) == []


def test_enabled_inactive_unit_is_unhealthy_without_live_counters(
    tmp_path: Path,
) -> None:
    props = unit_properties(
        CHAT,
        ActiveState="inactive",
        SubState="dead",
        IOWriteBytes="[not set]",
        CPUUsageNSec="[not set]",
        InvocationID="",
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 1
    assert f"{CHAT}:enabled-not-active" in str(report.fields["policy_issues"])


def test_disabled_inactive_unit_still_validates_installed_resource_policy(
    tmp_path: Path,
) -> None:
    props = unit_properties(
        CHAT,
        UnitFileState="disabled",
        ActiveState="inactive",
        SubState="dead",
        MemoryMax=str(probe.MAX_MEMORY_BY_PREFIX[probe.CHAT_UNIT_PREFIX] + 1),
        IOWriteBytes="[not set]",
        CPUUsageNSec="[not set]",
        InvocationID="",
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 1
    assert (
        f"{CHAT}:MemoryMax-above-"
        f"{probe.MAX_MEMORY_BY_PREFIX[probe.CHAT_UNIT_PREFIX]}"
    ) in str(report.fields["policy_issues"])


@pytest.mark.parametrize("active_state", ["active", "inactive"])
def test_masked_unit_is_unhealthy_and_live_counters_cannot_hide(
    tmp_path: Path, active_state: str
) -> None:
    live = active_state == "active"
    props = unit_properties(
        CHAT,
        LoadState="masked",
        UnitFileState="masked",
        ActiveState=active_state,
        SubState="running" if live else "dead",
        IOWriteBytes="1000" if live else "[not set]",
        CPUUsageNSec="2000" if live else "[not set]",
        InvocationID=INVOCATION_A if live else "",
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 1
    assert f"{CHAT}:masked" in str(report.fields["policy_issues"])
    assert report.fields["baseline_units"] == (1 if live else 0)


def test_deactivating_unit_preserves_and_compares_live_counters(
    tmp_path: Path,
) -> None:
    seed_state(
        tmp_path,
        observed_ns=NOW_NS - 1800 * probe.NSEC_PER_SEC,
        rows={CHAT: (0, 0, INVOCATION_A)},
    )
    props = unit_properties(
        CHAT,
        UnitFileState="disabled",
        ActiveState="deactivating",
        SubState="stop-sigterm",
        IOWriteBytes=str(3 * probe.GIB),
        CPUUsageNSec=str(probe.NSEC_PER_SEC),
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 1
    assert report.fields["baseline_units"] == 0
    assert report.fields["compared_units"] == 1
    assert report.fields["io_write_delta_bytes"] == 3 * probe.GIB
    assert report.fields["io_write_rate_bytes_per_hour"] == 6 * probe.GIB
    assert CHAT in saved_state(tmp_path)["units"]


def test_failed_unit_alerts_even_when_disabled(tmp_path: Path) -> None:
    props = unit_properties(
        CHAT,
        UnitFileState="disabled",
        ActiveState="failed",
        SubState="failed",
        IOWriteBytes="[not set]",
        CPUUsageNSec="[not set]",
        InvocationID="",
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 1
    assert f"{CHAT}:failed" in str(report.fields["policy_issues"])


def test_legacy_unit_is_outside_generic_discovery_policy(tmp_path: Path) -> None:
    assert probe.UNIT_NAME.fullmatch("herdr-chat-coordinator.service") is None
    report = run(tmp_path, FakeSystemd())
    assert report.returncode == 0
    assert not state_directory(tmp_path).exists()


def test_disabled_but_active_unit_must_have_every_resource_guard(
    tmp_path: Path,
) -> None:
    props = unit_properties(
        CHAT,
        UnitFileState="disabled",
        MemoryMax="infinity",
        MemorySwapMax="infinity",
        TasksMax=str((1 << 64) - 1),
        LimitNOFILE="infinity",
        LimitCORE="infinity",
        KillMode="process",
        OOMPolicy="stop",
        AllowedCPUs="0-7",
        Restart="no",
        StartLimitBurst="6",
        StartLimitIntervalUSec="1s",
        TimeoutStopUSec="infinity",
        MemoryAccounting="no",
        TasksAccounting="no",
        IOAccounting="no",
    )
    report = run(tmp_path, FakeSystemd({CHAT: props}))
    assert report.returncode == 1
    issues = str(report.fields["policy_issues"])
    for fragment in (
        "MemoryMax-infinity",
        "MemorySwapMax-infinity",
        "TasksMax-uint64-max",
        "LimitNOFILE-infinity",
        "LimitCORE-not-zero",
        "KillMode-not-control-group",
        "OOMPolicy-not-kill",
        "AllowedCPUs-above-4",
        "Restart-not-on-failure",
        "StartLimitBurst-above-5",
        "StartLimitIntervalUSec-too-short",
        "TimeoutStopUSec-out-of-range",
        "MemoryAccounting-not-yes",
        "TasksAccounting-not-yes",
        "IOAccounting-not-yes",
    ):
        assert fragment in issues


@pytest.mark.parametrize(
    ("property_name", "bad_value", "fragment"),
    [
        (
            "MemoryMax",
            str(probe.MAX_MEMORY_BY_PREFIX[probe.CHAT_UNIT_PREFIX] + 1),
            "MemoryMax-above-",
        ),
        (
            "MemorySwapMax",
            str(probe.MAX_SWAP_BYTES + 1),
            "MemorySwapMax-above-",
        ),
        ("TasksMax", str(probe.MAX_TASKS + 1), "TasksMax-above-"),
        ("LimitNOFILE", str(probe.MAX_NOFILE + 1), "LimitNOFILE-above-"),
    ],
)
def test_finite_resource_values_above_policy_are_unhealthy(
    tmp_path: Path,
    property_name: str,
    bad_value: str,
    fragment: str,
) -> None:
    report = run(
        tmp_path,
        FakeSystemd({CHAT: unit_properties(CHAT, **{property_name: bad_value})}),
    )
    assert report.returncode == 1
    assert f"{CHAT}:{fragment}" in str(report.fields["policy_issues"])


@pytest.mark.parametrize(
    ("property_name", "bad_value"),
    [
        ("MemoryMax", "many"),
        ("MemorySwapMax", "many"),
        ("TasksMax", "many"),
        ("LimitNOFILE", "many"),
        ("LimitCORE", "many"),
        ("AllowedCPUs", "many"),
        ("StartLimitBurst", "many"),
        ("StartLimitIntervalUSec", "many"),
        ("TimeoutStopUSec", "many"),
        ("MemoryAccounting", "maybe"),
        ("IOWriteBytes", "[not set]"),
        ("CPUUsageNSec", "[not set]"),
        ("InvocationID", ""),
        ("InvocationID", "not-a-generation"),
    ],
)
def test_malformed_active_unit_evidence_fails_closed(
    tmp_path: Path, property_name: str, bad_value: str
) -> None:
    report = run(
        tmp_path,
        FakeSystemd({CHAT: unit_properties(CHAT, **{property_name: bad_value})}),
    )
    assert report.returncode == 2
    assert report.fields["state"] == "unknown"
    assert not state_path(tmp_path).exists()


def test_unavailable_discovery_fails_closed_without_state(tmp_path: Path) -> None:
    def unavailable(_args: tuple[str, ...]) -> str:
        raise probe.EvidenceUnavailable("user manager unavailable")

    report = probe.evaluate(root=tmp_path, run=unavailable, now_ns=NOW_NS)
    assert report.returncode == 2
    assert "user manager unavailable" in str(report.fields["summary"])
    assert not state_directory(tmp_path).exists()


def test_show_omitting_an_installed_unit_fails_closed(tmp_path: Path) -> None:
    systemd = FakeSystemd({CHAT: unit_properties(CHAT)})

    def missing(args: tuple[str, ...]) -> str:
        if args[0] == "show":
            return ""
        return systemd(args)

    report = probe.evaluate(root=tmp_path, run=missing, now_ns=NOW_NS)
    assert report.returncode == 2
    assert "omitted unit" in str(report.fields["summary"])


def test_discovery_unions_installed_and_loaded_generic_families(
    tmp_path: Path,
) -> None:
    units = {
        CHAT: unit_properties(CHAT),
        EVENTS: unit_properties(EVENTS, UnitFileState="disabled"),
    }
    systemd = FakeSystemd(units, installed=(CHAT,), loaded=(EVENTS,))
    report = run(tmp_path, systemd)
    assert report.returncode == 0
    assert report.fields["units_discovered"] == 2


def test_state_replacement_is_same_directory_private_and_atomic(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    seed_state(
        tmp_path,
        observed_ns=NOW_NS - probe.NSEC_PER_SEC,
        rows={CHAT: (7, 11, INVOCATION_A)},
    )
    before = state_path(tmp_path).read_bytes()
    real_replace = os.replace
    observations = 0

    def inspect_replace(
        source: str,
        destination: str,
        *,
        src_dir_fd: int | None = None,
        dst_dir_fd: int | None = None,
    ) -> None:
        nonlocal observations
        assert destination == probe.STATE_NAME
        assert src_dir_fd == dst_dir_fd
        assert src_dir_fd is not None
        assert state_path(tmp_path).read_bytes() == before
        info = os.stat(source, dir_fd=src_dir_fd, follow_symlinks=False)
        assert stat.S_IMODE(info.st_mode) == 0o600
        source_fd = os.open(source, os.O_RDONLY, dir_fd=src_dir_fd)
        try:
            assert os.read(source_fd, probe.MAX_STATE_BYTES + 1) == payload
        finally:
            os.close(source_fd)
        observations += 1
        real_replace(
            source,
            destination,
            src_dir_fd=src_dir_fd,
            dst_dir_fd=dst_dir_fd,
        )

    monkeypatch.setattr(probe.os, "replace", inspect_replace)
    with probe.StateTransaction(tmp_path) as transaction:
        assert transaction.load_baseline().status == "loaded"
        payload = probe.serialize_baseline(
            NOW_NS,
            {CHAT: probe.Counters(13, 17, INVOCATION_A)},
        )
        transaction.write_baseline(payload)
    assert observations == 1
    assert stat.S_IMODE(state_path(tmp_path).stat().st_mode) == 0o600
    assert saved_state(tmp_path)["units"] == {
        CHAT: {
            "io_write_bytes": 13,
            "cpu_usage_nsec": 17,
            "invocation_id": INVOCATION_A,
        }
    }
    assert list(state_directory(tmp_path).glob(f".{probe.STATE_NAME}.*")) == []


def test_concurrent_5_20_gib_per_hour_cannot_lose_the_newer_sample(
    tmp_path: Path,
) -> None:
    base_ns = NOW_NS - 3600 * probe.NSEC_PER_SEC
    seed_state(
        tmp_path,
        observed_ns=base_ns,
        rows={CHAT: (0, 0, INVOCATION_A)},
    )
    barrier = threading.Barrier(2)
    older = FakeSystemd(
        {
            CHAT: unit_properties(
                CHAT,
                IOWriteBytes=str(26 * probe.GIB // 10),
                CPUUsageNSec=str(100 * probe.NSEC_PER_SEC),
            )
        },
        barrier=barrier,
    )
    newer = FakeSystemd(
        {
            CHAT: unit_properties(
                CHAT,
                IOWriteBytes=str(52 * probe.GIB // 10),
                CPUUsageNSec=str(200 * probe.NSEC_PER_SEC),
            )
        },
        barrier=barrier,
    )
    with ThreadPoolExecutor(max_workers=2) as pool:
        older_future = pool.submit(
            probe.evaluate,
            root=tmp_path,
            run=older,
            now_ns=base_ns + 1800 * probe.NSEC_PER_SEC,
        )
        newer_future = pool.submit(
            probe.evaluate,
            root=tmp_path,
            run=newer,
            now_ns=base_ns + 3600 * probe.NSEC_PER_SEC,
        )
        reports = [older_future.result(timeout=5), newer_future.result(timeout=5)]
    assert 1 in {report.returncode for report in reports}
    assert all(report.returncode in {1, 2} for report in reports)
    final = saved_state(tmp_path)
    assert final["observed_at_unix_ns"] == base_ns + 3600 * probe.NSEC_PER_SEC
    assert final["units"][CHAT]["io_write_bytes"] == 52 * probe.GIB // 10


def test_busy_lock_refuses_within_its_internal_bound(tmp_path: Path) -> None:
    systemd = FakeSystemd({CHAT: unit_properties(CHAT)})
    with probe.StateTransaction(tmp_path):
        started = time.monotonic()
        report = run(tmp_path, systemd)
        elapsed = time.monotonic() - started
    assert report.returncode == 2
    assert "lock remained busy" in str(report.fields["summary"])
    assert elapsed < 2


def test_symlinked_state_directory_is_rejected_without_following(
    tmp_path: Path,
) -> None:
    outside = tmp_path / "outside"
    outside.mkdir()
    state_directory(tmp_path).symlink_to(outside, target_is_directory=True)
    report = run(tmp_path, FakeSystemd({CHAT: unit_properties(CHAT)}))
    assert report.returncode == 2
    assert list(outside.iterdir()) == []


@pytest.mark.parametrize("linked_name", [probe.STATE_NAME, probe.LOCK_NAME])
def test_symlinked_state_or_lock_file_is_rejected_without_following(
    tmp_path: Path, linked_name: str
) -> None:
    state_directory(tmp_path).mkdir(mode=0o700)
    outside = tmp_path / "outside"
    outside.write_text("do-not-touch", encoding="utf-8")
    (state_directory(tmp_path) / linked_name).symlink_to(outside)
    report = run(tmp_path, FakeSystemd({CHAT: unit_properties(CHAT)}))
    assert report.returncode == 2
    assert outside.read_text(encoding="utf-8") == "do-not-touch"
    assert (state_directory(tmp_path) / linked_name).is_symlink()


@pytest.mark.parametrize(
    ("target", "mode"),
    [("directory", 0o777), ("state", 0o644), ("lock", 0o644)],
)
def test_untrusted_state_modes_are_rejected(
    tmp_path: Path, target: str, mode: int
) -> None:
    seed_state(
        tmp_path,
        observed_ns=NOW_NS - probe.NSEC_PER_SEC,
        rows={CHAT: (0, 0, INVOCATION_A)},
    )
    if target == "directory":
        state_directory(tmp_path).chmod(mode)
    elif target == "state":
        state_path(tmp_path).chmod(mode)
    else:
        lock_path(tmp_path).write_text("", encoding="utf-8")
        lock_path(tmp_path).chmod(mode)
    before = state_path(tmp_path).read_bytes()
    report = run(tmp_path, FakeSystemd({CHAT: unit_properties(CHAT)}))
    assert report.returncode == 2
    assert state_path(tmp_path).read_bytes() == before


def test_foreign_ownership_is_rejected_by_the_shared_guard() -> None:
    foreign = SimpleNamespace(st_uid=os.geteuid() + 1, st_mode=stat.S_IFREG | 0o600)
    with pytest.raises(probe.EvidenceUnavailable, match="owned by uid"):
        probe._require_owned(foreign, label="fixture", exact_mode=0o600)
