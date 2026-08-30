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
    assert rows[0]["growth_state"] == "measured"
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
    assert rows[0]["growth_state"] == "unknown"
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
    assert rows[0]["growth_state"] == "unknown"


def test_malformed_prior_sample_is_unknown_not_a_zero_rate(tmp_path):
    state = tmp_path / "s.json"
    state.write_text('{"fds":{"1:1":{"size":"large","t":0}}}')
    rows, reasons = bh.writer_alarms(
        [_row(size=GiB)], state, 3600.0, 50 * GiB, 20 * GiB
    )
    assert not reasons
    assert rows[0]["growth_bytes_per_hour"] is None
    assert rows[0]["growth_state"] == "unknown"


def test_gate_reports_unknown_then_clean_when_growth_becomes_measurable(
    tmp_path, monkeypatch, capsys
):
    hr = make_headroom(
        meta_total=125 * GiB, meta_used=10 * GiB, meta_disk_total=250 * GiB,
        data_total=1000 * GiB, data_used=10 * GiB, unallocated=500 * GiB,
    )
    monkeypatch.setattr(bh, "measure", lambda _mount: hr)
    monkeypatch.setattr(
        bh, "large_open_writers",
        lambda *_args, **_kwargs: [_row(size=100 * 1024 * 1024)],
    )
    state = tmp_path / "writers.json"
    args = ["--gate", "--writers", "--writers-state", str(state), "--mount", "/fake"]
    assert bh.main(args) == 2
    assert "writer growth is UNKNOWN" in capsys.readouterr().out
    assert bh.main(args) == 0
    assert "writer_growth_state=measured" in capsys.readouterr().out


# --------------------------------------------------------------------------
# The liveness gate on GROWTH, added 2026-08-19 after a false positive escalated
# a completed 240 MiB write as "23.6 GiB/h, 1 TB in under two days".
#
# These come in pairs on purpose. Quieting the bounded case is only a fix if the
# unbounded case still fires, so every suppression test below has a twin that
# asserts the alarm survives.
# --------------------------------------------------------------------------


def _grew(state, *, writable, size_from=100 * 1024 * 1024, size_to=700 * 1024 * 1024, **kw):
    """Two ticks 60s apart: 100 MiB -> 700 MiB is 35.2 GiB/h, over the 20 threshold."""
    bh.writer_alarms([_row(size=size_from, **kw)], state, 0.0, 50 * GiB, 20 * GiB)
    row = _row(size=size_to, **kw)
    row["writable"] = writable
    return bh.writer_alarms([row], state, 60.0, 50 * GiB, 20 * GiB)


def test_completed_bounded_write_does_not_alarm_on_growth(tmp_path):
    """THE FALSE POSITIVE. Same 35.2 GiB/h, but nothing holds it open for write.

    A bounded emitter that finishes inside one inter-tick interval leaves exactly
    one interval that looks like a runaway. It cannot keep growing, so it must
    not raise a RUNAWAY WRITER gate failure.
    """
    rows, reasons = _grew(tmp_path / "s.json", writable=False)
    assert reasons == [], "a write that has already finished is not a runaway"
    assert not rows[0]["alarmed"]


def test_live_unbounded_write_still_alarms_on_growth(tmp_path):
    """THE TWIN. Identical rate, identical size, but still open for write."""
    rows, reasons = _grew(tmp_path / "s.json", writable=True)
    assert reasons, "a live writer at 35.2 GiB/h must still trip 20 GiB/h"
    assert rows[0]["alarmed"]
    assert rows[0]["logical_bytes"] < GiB, "must still fire while under 1 GiB"


def test_a_finished_fast_write_is_still_reported_not_silently_dropped(tmp_path):
    """Suppressing the ALARM must not suppress the EVIDENCE.

    "grew at 35 GiB/h and then stopped" is what lets an operator tell a bounded
    emitter from a runaway. The rate is still recorded and the row is flagged.
    """
    rows, _reasons = _grew(tmp_path / "s.json", writable=False)
    assert rows[0]["growth_bytes_per_hour"] > 34 * GiB
    assert rows[0]["growth_completed"] is True


def test_unknown_writability_fails_open_and_alarms(tmp_path):
    """No 'writable' key at all -- the pre-existing row shape. Must still fire.

    Fail-open is the safety posture: the cost of a false negative here is the
    4.5 TB incident this monitor exists for.
    """
    state = tmp_path / "s.json"
    bh.writer_alarms([_row(size=100 * 1024 * 1024)], state, 0.0, 50 * GiB, 20 * GiB)
    rows, reasons = bh.writer_alarms(
        [_row(size=700 * 1024 * 1024)], state, 60.0, 50 * GiB, 20 * GiB
    )
    assert "writable" not in _row(), "guard: _row must stay writability-free for this test"
    assert reasons and rows[0]["alarmed"]


def test_size_trigger_is_not_gated_on_writability(tmp_path):
    """An unlinked 80 GiB file holds its space whoever has it open, and however.

    Gating SIZE on writability would have hidden exactly the class of file this
    tool was built to find.
    """
    row = _row(size=80 * GiB, deleted=True)
    row["writable"] = False
    rows, reasons = bh.writer_alarms([row], tmp_path / "s.json", 1000.0, 50 * GiB, 20 * GiB)
    assert reasons and rows[0]["alarmed"]
    assert "unlinked" in reasons[0]


def test_fd_writability_decodes_octal_flags_and_fails_open(tmp_path, monkeypatch):
    """/proc fdinfo 'flags' is OCTAL. Decoding it as decimal misreads the mode."""
    def fake_read(self, *a, **k):
        return {"w": "pos:\t0\nflags:\t02100002\nmnt_id:\t29\n",     # O_RDWR
                "r": "pos:\t0\nflags:\t02100000\nmnt_id:\t29\n",     # O_RDONLY
                "a": "pos:\t0\nflags:\t02100001\nmnt_id:\t29\n",     # O_WRONLY
                }[self.name]
    monkeypatch.setattr(Path, "read_text", fake_read)
    assert bh._fd_is_writable("1", "w") is True
    assert bh._fd_is_writable("1", "a") is True
    assert bh._fd_is_writable("1", "r") is False, "0o2100000 & 3 == 0 -> read-only"

    def boom(self, *a, **k):
        raise OSError("vanished mid-scan")
    monkeypatch.setattr(Path, "read_text", boom)
    assert bh._fd_is_writable("1", "3") is True, "unreadable fdinfo must fail OPEN"
