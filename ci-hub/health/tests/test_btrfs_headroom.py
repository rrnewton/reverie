"""Unit tests for btrfs_headroom.

These cover the ARITHMETIC and the classification, against synthetic sysfs trees
so they run anywhere without root or a btrfs.  The behaviour that cannot be
faked -- that the gate actually fires as a real filesystem fills, and that the
numbers equal btrfs's own accounting -- was verified by driving a loopback btrfs
across the threshold; see the task notes on disk-monitoring-that-tells-truth.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

MODULE = Path(__file__).resolve().parent.parent / "btrfs_headroom.py"
spec = importlib.util.spec_from_file_location("btrfs_headroom", MODULE)
assert spec and spec.loader
bh = importlib.util.module_from_spec(spec)
# Register BEFORE exec: @dataclass resolves annotations via
# sys.modules[cls.__module__], which is None for an unregistered module.
sys.modules["btrfs_headroom"] = bh
spec.loader.exec_module(bh)

GiB = 1 << 30


def make_headroom(
    *,
    meta_total: int,
    meta_used: int,
    meta_disk_total: int,
    data_total: int,
    data_used: int,
    unallocated: int,
) -> "bh.Headroom":
    pools = {
        "metadata": bh.Pool("metadata", meta_total, meta_used, meta_disk_total),
        "data": bh.Pool("data", data_total, data_used, data_total),
    }
    return bh.Headroom(
        mount="/data",
        source="/dev/md0",
        uuid="test",
        device_total=data_total + meta_disk_total + unallocated,
        unallocated=unallocated,
        pools=pools,
    )


def test_dup_metadata_halves_the_unallocated_contribution():
    """The divisor that turns 32 GiB of unallocated into 16 GiB of metadata.

    Reproduces /data as measured 2026-08-19: 26.6 GiB free inside 125 GiB of
    allocated metadata, plus 32.01 GiB unallocated at DUP, is 42.6 GiB -- NOT
    58.6 GiB, which is what dropping the divisor would give.
    """
    hr = make_headroom(
        meta_total=125 * GiB,
        meta_used=int(98.4 * GiB),
        meta_disk_total=250 * GiB,  # DUP
        data_total=3215 * GiB,
        data_used=2235 * GiB,
        unallocated=int(32.01 * GiB),
    )
    assert hr.pools["metadata"].redundancy == pytest.approx(2.0)
    assert hr.pool_headroom("metadata") / GiB == pytest.approx(42.6, abs=0.1)
    # The wrong answer, guarded against explicitly.
    assert hr.pool_headroom("metadata") / GiB != pytest.approx(58.6, abs=0.1)


def test_redundancy_is_derived_not_assumed():
    """A single-profile metadata pool must not be charged the DUP divisor."""
    hr = make_headroom(
        meta_total=100 * GiB,
        meta_used=50 * GiB,
        meta_disk_total=100 * GiB,  # single, not DUP
        data_total=1000 * GiB,
        data_used=500 * GiB,
        unallocated=40 * GiB,
    )
    assert hr.pools["metadata"].redundancy == pytest.approx(1.0)
    assert hr.pool_headroom("metadata") == 50 * GiB + 40 * GiB


def test_metadata_can_be_starved_while_df_shows_terabytes():
    """The failure this gate exists for: df is fine, metadata is nearly gone."""
    hr = make_headroom(
        meta_total=125 * GiB,
        meta_used=124 * GiB,
        meta_disk_total=250 * GiB,
        data_total=3215 * GiB,
        data_used=2235 * GiB,
        unallocated=2 * GiB,
    )
    state, summary, report = bh.render(hr, 20 * GiB, 400 * GiB, 20 * GiB)
    assert state == "low"
    assert report["metadata_state"] == "low"
    # df's view -- what a naive monitor would have reported as healthy.
    assert report["data_headroom_gib"] > 900
    assert "df would report" in summary


def test_classify_boundaries():
    floor = 20 * GiB
    assert bh.classify(floor - 1, floor) == "low"
    assert bh.classify(floor, floor) == "warn"
    assert bh.classify(int(floor * bh.WARN_FACTOR) - 1, floor) == "warn"
    assert bh.classify(int(floor * bh.WARN_FACTOR), floor) == "ok"


def test_free_in_allocated_never_negative():
    """bytes_used can momentarily exceed total_bytes mid-transaction."""
    pool = bh.Pool("metadata", total=10, used=12, disk_total=20)
    assert pool.free_in_allocated == 0


def test_unmeasurable_is_unknown_not_healthy(capsys, monkeypatch):
    """A filesystem we cannot read must exit 2, never 0.

    'Could not determine' and 'determined, and fine' are different states; the
    gate must not launder the first into the second.
    """
    rc = bh.main(["--gate", "--mount", "/proc"])
    out = capsys.readouterr().out
    assert rc == 2
    assert "state=unknown" in out


def test_gate_exit_codes_follow_state():
    hr_ok = make_headroom(
        meta_total=125 * GiB, meta_used=10 * GiB, meta_disk_total=250 * GiB,
        data_total=1000 * GiB, data_used=10 * GiB, unallocated=500 * GiB,
    )
    state, _, _ = bh.render(hr_ok, 20 * GiB, 400 * GiB, 20 * GiB)
    assert state == "ok"

    hr_low = make_headroom(
        meta_total=125 * GiB, meta_used=124 * GiB, meta_disk_total=250 * GiB,
        data_total=1000 * GiB, data_used=999 * GiB, unallocated=1 * GiB,
    )
    state, _, _ = bh.render(hr_low, 20 * GiB, 400 * GiB, 20 * GiB)
    assert state == "low"


def test_memfd_and_anon_fds_are_never_counted():
    """A 2 TiB memfd occupies zero filesystem bytes; an early version ranked one
    first on a filesystem it did not touch."""
    for target in ("/memfd:doublemapper (deleted)", "anon_inode:[eventpoll]",
                   "socket:[12345]", "pipe:[678]", "/dev/zero", "/proc/self/status"):
        assert target.startswith(bh._NON_DISK_PREFIXES), target
    assert not "/home/newton/work/x.log".startswith(bh._NON_DISK_PREFIXES)


# --------------------------------------------------------------------------
# Runaway-writer detection, added after a review found the first version could
# not have caught the incident it was built for.
# --------------------------------------------------------------------------


def _row(pid=1, size=0, comm="x", fd=3, dev=1, ino=1, deleted=False):
    return {
        "pid": pid, "comm": comm, "fd": fd, "logical_bytes": size,
        "deleted": deleted, "on_target_fs": "yes",
        "path_in_its_namespace": "/p", "dev": dev, "ino": ino,
    }


def test_size_trigger_fires_with_no_reference_to_free_space(tmp_path):
    """A runaway is a runaway whether or not the disk is nearly full."""
    rows, reasons = bh.writer_alarms(
        [_row(size=80 * GiB)], tmp_path / "s.json", 1000.0, 50 * GiB, 20 * GiB
    )
    assert reasons and rows[0]["alarmed"]
    assert "80.0 GiB" in reasons[0]


def test_growth_trigger_fires_while_the_file_is_still_small(tmp_path):
    """The whole point: catch it at fractions of a GiB, not at 767.

    100 MiB -> 700 MiB in 60s is 600 MiB/min = 35.2 GiB/h, comfortably over the
    20 GiB/h threshold, and the file is still well under 1 GiB when it trips.
    """
    state = tmp_path / "s.json"
    bh.writer_alarms([_row(size=100 * 1024 * 1024)], state, 0.0, 50 * GiB, 20 * GiB)
    rows, reasons = bh.writer_alarms(
        [_row(size=700 * 1024 * 1024)], state, 60.0, 50 * GiB, 20 * GiB
    )
    assert reasons, "35.2 GiB/h must trip a 20 GiB/h threshold"
    assert rows[0]["growth_bytes_per_hour"] > 34 * GiB
    assert rows[0]["logical_bytes"] < GiB, "must fire while still under 1 GiB"


def test_growth_just_below_threshold_stays_quiet(tmp_path):
    """The other side of the bracket: 17.6 GiB/h must NOT trip 20 GiB/h.

    Written after the first version of the test above asserted that it should --
    my arithmetic, not the code's.
    """
    state = tmp_path / "s.json"
    bh.writer_alarms([_row(size=100 * 1024 * 1024)], state, 0.0, 50 * GiB, 20 * GiB)
    _rows, reasons = bh.writer_alarms(
        [_row(size=400 * 1024 * 1024)], state, 60.0, 50 * GiB, 20 * GiB
    )
    assert not reasons


def test_first_sighting_cannot_trigger_on_growth(tmp_path):
    """Growth needs a baseline; a fresh fd reports unknown, never zero."""
    rows, reasons = bh.writer_alarms(
        [_row(size=10 * GiB)], tmp_path / "s.json", 1000.0, 50 * GiB, 1 * GiB
    )
    assert rows[0]["growth_bytes_per_hour"] is None
    assert not reasons


def test_quiet_when_nothing_is_wrong(tmp_path):
    state = tmp_path / "s.json"
    bh.writer_alarms([_row(size=GiB)], state, 0.0, 50 * GiB, 20 * GiB)
    _rows, reasons = bh.writer_alarms([_row(size=GiB + 1000)], state, 3600.0, 50 * GiB, 20 * GiB)
    assert not reasons


def test_namespace_private_device_is_unknown_not_no():
    """The sharpest defect: 'unknown' is hidden if collapsed into 'no'.

    A device matching NO visible mount is a namespace-private mount or an
    unmounted subvolume -- precisely what this tool exists to see. Filing it
    under 'elsewhere' made the default view blind to it.
    """
    ours, known = {10}, {10, 11, 12}
    def classify(dev):
        return "yes" if dev in ours else ("no" if dev in known else "unknown")
    assert classify(10) == "yes"
    assert classify(11) == "no"          # a real other filesystem: hide it
    assert classify(93) == "unknown"     # namespace-private: MUST be shown
    assert classify(93) != "no"


def test_growth_survives_an_unreadable_state_file(tmp_path):
    bad = tmp_path / "s.json"
    bad.write_text("{ not json")
    rows, reasons = bh.writer_alarms([_row(size=GiB)], bad, 1.0, 50 * GiB, 20 * GiB)
    assert rows[0]["growth_bytes_per_hour"] is None and not reasons
