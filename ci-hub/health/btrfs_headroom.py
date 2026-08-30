#!/usr/bin/env python3
"""Report btrfs data/metadata headroom from btrfs's own allocator accounting.

WHY THIS EXISTS AND WHY IT DOES NOT CALL du
===========================================
On /data (btrfs, compress=zstd:3, on a RAID0 md0) there are three INDEPENDENT
ways a directory-walking measurement misleads, and they do not cancel:

  1. INVISIBLE.  Files written by a process inside its own mount namespace, and
     files unlinked while still held open, are not reachable by du/ncdu/find at
     all.  During the 2026-08-17 out-of-disk incident ~9 TB was unfindable this
     way.  ``stat -L /proc/PID/fd/N`` is what sees them (see ``--writers``).
  2. OVERSTATES.  ``st_blocks`` reports LOGICAL size under compression, so du
     sums pre-compression bytes: ``du -x /data`` reported 6.2T on a 3.42 TiB
     device.  Measured ratio on this box: a 1.9 GiB log occupied 62 MiB.
  3. UNDERSTATES.  Every btrfs subvolume carries its own ``st_dev``, so ``du -x``
     / ``du --one-file-system`` prunes at each one.  One path measured 142G with
     ``du -sh`` and 0 with ``du -shx`` at the same instant: it contained 47
     subvolumes.

Because 2 and 3 push in opposite directions there is no correction factor to
apply.  So this module never walks the tree.  It reads the allocator's own
counters, which are immune to all three by construction: block-group accounting
is filesystem-wide (subvolume-agnostic), post-compression, and namespace-blind
because a byte allocated is a byte allocated no matter who wrote it.

WHAT "HEADROOM" MEANS HERE, AND WHY df IS THE WRONG NUMBER
==========================================================
btrfs allocates the device into chunks that are typed data or metadata.  A pool
can exhaust while the other pool still holds terabytes, and ``df`` reports the
sum.  On 2026-08-19 /data had 1018 GiB "free" by df and 42.6 GiB of real
metadata headroom.  Headroom for a pool is therefore TWO terms:

    headroom = (already-allocated space still unused in that pool)
             + (device space not yet allocated to any pool) / redundancy

The divisor matters and is the part people drop.  Metadata is DUP, so 2 GiB of
unallocated device buys only 1 GiB of usable metadata.  The redundancy factor is
DERIVED per pool (disk_total / total_bytes), never assumed, so this stays correct
if the profile is ever changed.

Unallocated space is the shared reserve BOTH pools draw from, so it is also
reported and alarmed on in its own right.
"""

from __future__ import annotations

import argparse
import json
import os
import time
from dataclasses import dataclass
from pathlib import Path

GiB = 1 << 30

SYSFS_BTRFS = Path("/sys/fs/btrfs")

# Defaults derived from measurements on /data taken 2026-08-19; see
# ci-hub/health/README.md and the task notes on btrfs-metadata-near-exhaustion.
#
# Metadata floor 20 GiB: metadata grew ~2.3 GiB/day over an 11.5 min window, but
# the sample swing was 3x the trend, so the rate is indicative only.  20 GiB is
# roughly a week at that rate and several days even if the true rate is 3x what
# was observed.  It is an absolute floor precisely BECAUSE the rate is not
# trustworthy enough to gate on.
DEFAULT_METADATA_FLOOR = 20 * GiB
# Data floor 400 GiB: data grew +142 GiB/day (firm; the signal is large relative
# to noise).  400 GiB leaves ~3 days to act after the alert fires.
DEFAULT_DATA_FLOOR = 400 * GiB
# Unallocated floor 20 GiB: once this is gone neither pool can grow, whatever
# each pool's own headroom says.
DEFAULT_UNALLOC_FLOOR = 20 * GiB

# A pool is reported "warn" at this multiple of its floor.  Warn does NOT fail
# the gate; it exists so a reader watching output sees the approach rather than
# only the arrival.
WARN_FACTOR = 2.0


class NotBtrfs(Exception):
    """The path is not on a btrfs filesystem this module can account for."""


def _read_int(path: Path) -> int:
    return int(path.read_text().strip())


def mount_source(path: str) -> tuple[str, str]:
    """Return (mount_point, source_device) for the mount backing ``path``.

    Uses /proc/self/mountinfo rather than ``os.stat().st_dev`` on purpose: a
    btrfs subvolume is handed an ANONYMOUS st_dev, so a st_dev-based lookup finds
    no block device at all for /tmp (which is subvol=/tmpvol here).  This is the
    same st_dev quirk that makes ``du -x`` prune subvolumes.
    """
    target = os.path.realpath(path)
    best_point = ""
    best_source = ""
    best_fstype = ""
    with open("/proc/self/mountinfo", encoding="utf-8") as handle:
        for line in handle:
            fields = line.split()
            try:
                sep = fields.index("-")
            except ValueError:
                continue
            point = fields[4]
            fstype = fields[sep + 1]
            source = fields[sep + 2]
            # Longest matching mount point wins; a bind mount of a subvolume is
            # a distinct, longer mount point than the parent filesystem.
            if target == point or target.startswith(point.rstrip("/") + "/"):
                if len(point) >= len(best_point):
                    best_point, best_source, best_fstype = point, source, fstype
    if not best_point:
        raise NotBtrfs(f"no mount found for {path}")
    if best_fstype != "btrfs":
        raise NotBtrfs(f"{path} is on {best_fstype!r}, not btrfs")
    return best_point, best_source


def sysfs_dir(source: str) -> Path:
    """Locate /sys/fs/btrfs/<uuid> for the filesystem whose device is ``source``."""
    name = os.path.basename(os.path.realpath(source))
    if not SYSFS_BTRFS.is_dir():
        raise NotBtrfs("/sys/fs/btrfs is absent; btrfs module not loaded")
    for entry in SYSFS_BTRFS.iterdir():
        if not entry.is_dir() or entry.name in {"features"}:
            continue
        if (entry / "devices" / name).exists():
            return entry
    raise NotBtrfs(f"no /sys/fs/btrfs entry owns device {name!r}")


@dataclass(frozen=True)
class Pool:
    """One btrfs space_info (data, metadata, or system)."""

    name: str
    total: int  # logical bytes allocated to this pool, across its chunks
    used: int  # logical bytes actually in use within those chunks
    disk_total: int  # DEVICE bytes those chunks consume (total * redundancy)

    @property
    def redundancy(self) -> float:
        """DEVICE bytes consumed per logical byte: 1.0 for single, 2.0 for DUP.

        Derived, never assumed, so a profile change cannot silently invalidate
        every number below it.
        """
        if self.total <= 0:
            return 1.0
        return self.disk_total / self.total

    @property
    def free_in_allocated(self) -> int:
        return max(0, self.total - self.used)

    @property
    def pct_used(self) -> float:
        return 100.0 * self.used / self.total if self.total else 0.0


@dataclass(frozen=True)
class Headroom:
    mount: str
    source: str
    uuid: str
    device_total: int
    unallocated: int
    pools: dict[str, Pool]

    def pool_headroom(self, name: str) -> int:
        """Usable bytes this pool can still consume before it exhausts.

        Both terms, with the unallocated term divided by the pool's redundancy —
        the divisor that makes 32 GiB of unallocated worth only 16 GiB of DUP
        metadata.
        """
        pool = self.pools[name]
        return pool.free_in_allocated + int(self.unallocated / pool.redundancy)


def measure(path: str) -> Headroom:
    point, source = mount_source(path)
    base = sysfs_dir(source)

    pools: dict[str, Pool] = {}
    alloc = base / "allocation"
    for name in ("data", "metadata", "system"):
        pdir = alloc / name
        if not pdir.is_dir():
            continue
        pools[name] = Pool(
            name=name,
            total=_read_int(pdir / "total_bytes"),
            used=_read_int(pdir / "bytes_used"),
            disk_total=_read_int(pdir / "disk_total"),
        )
    if "data" not in pools or "metadata" not in pools:
        raise NotBtrfs(f"incomplete allocation counters under {alloc}")

    # Device total is the sum of every member device's size (512-byte sectors).
    device_total = 0
    for dev in (base / "devices").iterdir():
        size = dev / "size"
        if size.exists():
            device_total += _read_int(size) * 512

    allocated = sum(p.disk_total for p in pools.values())
    unallocated = max(0, device_total - allocated)

    return Headroom(
        mount=point,
        source=source,
        uuid=base.name,
        device_total=device_total,
        unallocated=unallocated,
        pools=pools,
    )


# --------------------------------------------------------------------------
# Attribution: who is writing.  Not part of the threshold, but the thing that
# was missing during the incident -- the alert needs to be actionable, and the
# tree walk that would normally answer "who" is exactly what cannot see the
# culprit (failure mode 1).
# --------------------------------------------------------------------------


# Descriptors that are NOT files on any disk.  A memfd in particular reports a
# huge st_size and costs RAM, not filesystem space: an early version of this
# scan named a 2 TiB `/memfd:doublemapper` as the top consumer of a filesystem
# it does not occupy a single byte of.  Naming the wrong culprit during an
# incident is worse than naming none.
_NON_DISK_PREFIXES = ("/memfd:", "anon_inode:", "socket:", "pipe:", "/dev/", "/proc/", "/sys/")


def device_map(mount_point: str) -> tuple[set[int], set[int]]:
    """Return (devices on this filesystem, st_dev of every mount we can see).

    The second set is what makes "elsewhere" distinguishable from "invisible".
    A file whose st_dev is a mount in OUR namespace but not this filesystem is
    definitively somewhere else and can be hidden.  A file whose st_dev matches
    NO mount we can see is either inside another process's mount namespace or in
    a btrfs subvolume that is not itself a mount point -- exactly the two things
    this monitor exists to catch, so it must never be filed under "elsewhere".
    """
    ours: set[int] = set()
    known: set[int] = set()
    try:
        _, source = mount_source(mount_point)
    except NotBtrfs:
        source = None
    with open("/proc/self/mountinfo", encoding="utf-8") as handle:
        for line in handle:
            fields = line.split()
            try:
                sep = fields.index("-")
            except ValueError:
                continue
            try:
                dev = os.stat(fields[4]).st_dev
            except OSError:
                continue
            known.add(dev)
            if source is not None and fields[sep + 2] == source:
                ours.add(dev)
    return ours, known


def _comm_of(proc: Path) -> str:
    try:
        return (proc / "comm").read_text().strip()
    except OSError:
        return "?"


# O_ACCMODE. An fd is a WRITER only if opened O_WRONLY or O_RDWR.
_O_ACCMODE = 0o3
_O_RDONLY = 0o0


def _fd_is_writable(pid: str, fd: str) -> bool:
    """Is this fd open for writing?

    Read from ``/proc/PID/fdinfo/N``, whose ``flags`` field is OCTAL.

    FAILS OPEN. Every unreadable, malformed or racing case returns True, so an
    fd whose mode we cannot establish is still treated as a potential writer and
    still alarms. The whole value of this monitor is catching a runaway that df
    cannot see; trading a false negative for a quieter report would invert that.
    Only a positively decoded O_RDONLY returns False.
    """
    try:
        for line in Path(f"/proc/{pid}/fdinfo/{fd}").read_text().splitlines():
            if line.startswith("flags:"):
                return (int(line.split()[1], 8) & _O_ACCMODE) != _O_RDONLY
    except (OSError, ValueError, IndexError):
        return True
    return True


def large_open_writers(
    mount_point: str, min_bytes: int = 1 << 30, limit: int = 10, include_all: bool = False
) -> list[dict]:
    """Largest open files, found via /proc/*/fd, scoped to one filesystem.

    Sees what du cannot: files in another process's mount namespace, and files
    already unlinked but still held open (whose space is NOT freed until the fd
    closes).  ``os.stat`` on /proc/PID/fd/N resolves the magic link to the real
    inode regardless of namespace; the readlink TARGET is a path string in
    someone else's namespace and may not exist here, so it is used only as a
    label and never stat'ed.

    Sizes are ``st_size`` and therefore LOGICAL.  Under compression the on-disk
    cost is smaller -- often much smaller.  This ranks consumers and identifies a
    runaway; it is NOT a reclaim estimate.

    ``on_target`` is:

      "yes"     st_dev is a mount of this filesystem;
      "unknown" st_dev matches NO mount visible to us -- a namespace-private
                mount, or a btrfs subvolume that is not itself a mount point;
      "no"      st_dev is some OTHER filesystem we can see.

    Only "no" is hidden by default.  An earlier version collapsed "unknown" into
    "no", which meant the default view could not show namespace-private files --
    in the one tool built because those files are invisible to du.  Missing the
    real culprit costs far more than one extra row.
    """
    devices, known = device_map(mount_point)
    found: list[dict] = []
    # (st_dev, st_ino) -> index into ``found``: one row per inode. This is a MAP
    # and not a set because several processes hold the same inode and they are
    # not interchangeable -- see the promotion below.
    seen: dict[tuple[int, int], int] = {}
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            entries = list((proc / "fd").iterdir())
        except (PermissionError, FileNotFoundError, ProcessLookupError, NotADirectoryError):
            continue
        for fd in entries:
            try:
                target = os.readlink(fd)
            except OSError:
                continue
            if target.startswith(_NON_DISK_PREFIXES):
                continue
            try:
                st = os.stat(fd)  # follows the magic link to the real inode
            except (OSError, ValueError):
                continue
            if (st.st_mode & 0o170000) != 0o100000:  # regular files only
                continue
            if st.st_size < min_bytes:
                continue
            if st.st_dev in devices:
                on_target = "yes"
            elif st.st_dev in known:
                on_target = "no"
            else:
                on_target = "unknown"
            if on_target == "no" and not include_all:
                continue
            writable = _fd_is_writable(proc.name, fd.name)
            key = (st.st_dev, st.st_ino)
            if key in seen:
                # PROMOTION, not a skip. Several processes hold the same inode
                # and only a WRITABLE one can be growing it. The old code kept
                # whichever /proc entry it met first, so a reader could be named
                # as the culprit -- on 2026-08-19 a read-only holder was reported
                # for a log it could not possibly have been writing, and the pid
                # was dead by the time anyone looked. Worse, the growth gate in
                # writer_alarms would then see "not writable" for a genuinely
                # runaway file and stay silent. Keep the writable holder.
                prior = found[seen[key]]
                if writable and not prior["writable"]:
                    prior.update(
                        pid=int(proc.name),
                        fd=int(fd.name),
                        comm=_comm_of(proc),
                        writable=True,
                    )
                continue
            seen[key] = len(found)
            found.append(
                {
                    "pid": int(proc.name),
                    "comm": _comm_of(proc),
                    "fd": int(fd.name),
                    "logical_bytes": st.st_size,
                    "deleted": target.endswith(" (deleted)"),
                    "on_target_fs": on_target,
                    "path_in_its_namespace": target,
                    "dev": st.st_dev,
                    "ino": st.st_ino,
                    "writable": writable,
                }
            )
    found.sort(key=lambda r: r["logical_bytes"], reverse=True)
    # NO truncation here. The caller must judge every collected row for alarms
    # BEFORE deciding which to print: an early version applied the display limit
    # first, so a small-but-exploding file was dropped before the growth check
    # ever saw it. Trimming is a presentation concern and happens last.
    return found


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def writer_alarms(
    writers: list[dict],
    state_path: Path | None,
    now: float,
    alert_bytes: int,
    growth_bytes_per_hour: int,
) -> tuple[list[dict], list[str]]:
    """Judge each writer on its own terms, independent of filesystem headroom.

    THIS IS THE POINT OF THE WHOLE FILE. An earlier version computed the writer
    list every tick and then threw it away unless headroom was already below
    floor, so detection was coupled to exhaustion: it reported the fire, not the
    smoke. The incident this monitor exists for was three processes writing
    ~4.5 TB each over 28 hours while the filesystem still had room -- the old
    logic would have stayed silent for all 28 hours, and a 767 GiB runaway with
    1 TB free raised nothing.

    A runaway is a runaway whether or not the disk is nearly full. Two
    independent triggers, either sufficient:

      SIZE    one open fd at or above ``alert_bytes``. No legitimate single log
              or recording on this box reaches the default; the largest observed
              from a completing run is 171 MB.
      GROWTH  one open fd gaining at least ``growth_bytes_per_hour``, measured
              against the previous tick via a small (dev, ino) -> (size, time)
              state file, AND still open for write by some process. This catches
              a runaway LONG before it is large, which is the whole difference
              between smoke and fire.

    Growth needs two observations, so the first tick after a fd appears can only
    ever trigger on size. That is stated rather than hidden: a fresh state file
    means growth is genuinely unknown, not zero.

    The ``and still open for write`` clause was added on 2026-08-19 after a false
    positive. The two observations give a correct rate for the interval, but an
    interval is history. A BOUNDED emitter -- one that writes a large file
    quickly and exits, which on this box is routine -- completes inside a single
    inter-tick interval and leaves exactly one interval that looks like a
    runaway. A demo-05 ``hermit-info.log`` wrote 240 MiB and stopped; it was
    escalated as 23.6 GiB/h reaching 1 TB in two days, when every such log on the
    box finishes at 239 MiB and the writing process had already exited.

    Raising the threshold cannot fix that: a bounded 240 MiB write and the first
    interval of an unbounded one are IDENTICAL observations at any threshold. The
    discriminator is not how much was written, it is whether anyone can still be
    writing. Requiring growth in two consecutive intervals would also work but
    would delay a real runaway by a full tick; liveness costs nothing, because a
    genuine runaway always has a live writable holder.

    Two properties of the check matter and are easy to get wrong:

      * It is "does SOME holder of this inode have it open for write", resolved
        across all holders in ``large_open_writers``. It is NOT "is the reported
        pid alive". The reported pid may be a reader -- in the incident above it
        was -- and a live reader proves nothing either way.
      * It fails OPEN. Unknown writability still alarms. Only a positively
        established read-only-everywhere suppresses growth, and even then the row
        is still reported, marked ``growth_completed``.

    SIZE is deliberately NOT gated on writability: an unlinked multi-GiB file
    occupies its space until the last fd closes, whoever holds it and however
    they opened it.
    """
    prior: dict = {}
    if state_path and state_path.exists():
        try:
            loaded = json.loads(state_path.read_text())
        except (OSError, ValueError):
            loaded = None
        if isinstance(loaded, dict) and isinstance(loaded.get("fds"), dict):
            prior = loaded["fds"]

    reasons: list[str] = []
    next_state: dict = {}
    for row in writers:
        key = f"{row['dev']}:{row['ino']}"
        next_state[key] = {"size": row["logical_bytes"], "t": now}
        row["growth_bytes_per_hour"] = None
        row["growth_state"] = "unknown"
        row["alarmed"] = False
        was = prior.get(key)
        if (
            isinstance(was, dict)
            and isinstance(was.get("t"), (int, float))
            and isinstance(was.get("size"), (int, float))
        ):
            dt = now - was["t"]
            if dt > 0:
                rate = (row["logical_bytes"] - was["size"]) / dt * 3600.0
                row["growth_bytes_per_hour"] = int(rate)
                row["growth_state"] = "measured"
                if rate >= growth_bytes_per_hour:
                    # LIVENESS GATE. The rate above is a correct measurement of
                    # the interval, but an interval is history: a bounded
                    # emitter that writes fast and stops leaves exactly one
                    # interval looking like a runaway. On 2026-08-19 a demo-05
                    # hermit-info.log wrote 240 MiB during one interval and
                    # exited; that was escalated as 23.6 GiB/h reaching 1 TB in
                    # two days, when every such log on the box finishes at
                    # 239 MiB. If no process still holds the inode open for
                    # WRITE the growth cannot continue, so it is reported but
                    # not alarmed.
                    #
                    # This costs no sensitivity: a genuine runaway always has a
                    # live writable holder. Note the predicate is "some holder
                    # of this inode is writable" (established across all holders
                    # in large_open_writers), NOT "the named pid is alive" --
                    # the named pid may be a reader, and a reader being alive
                    # says nothing. Unknown writability counts as writable.
                    if row.get("writable", True):
                        row["alarmed"] = True
                        reasons.append(
                            f"pid {row['pid']} ({row['comm']}) fd {row['fd']} is growing at "
                            f"{rate / GiB:.1f} GiB/h (threshold {growth_bytes_per_hour / GiB:.1f}), "
                            f"now {row['logical_bytes'] / GiB:.1f} GiB: "
                            f"{row['path_in_its_namespace']}"
                        )
                    else:
                        row["growth_completed"] = True
        if row["logical_bytes"] >= alert_bytes:
            row["alarmed"] = True
            reasons.append(
                f"pid {row['pid']} ({row['comm']}) fd {row['fd']} holds "
                f"{row['logical_bytes'] / GiB:.1f} GiB (threshold "
                f"{alert_bytes / GiB:.1f})"
                f"{' [unlinked, space not freed until closed]' if row['deleted'] else ''}: "
                f"{row['path_in_its_namespace']}"
            )

    if state_path:
        try:
            state_path.parent.mkdir(parents=True, exist_ok=True)
            tmp = state_path.with_suffix(state_path.suffix + ".tmp")
            tmp.write_text(json.dumps({"fds": next_state}, sort_keys=True))
            os.replace(tmp, state_path)
        except OSError:
            pass  # a monitor that cannot persist must still report this tick

    return writers, reasons


def classify(value: int, floor: int) -> str:
    if value < floor:
        return "low"
    if value < floor * WARN_FACTOR:
        return "warn"
    return "ok"


def render(
    hr: Headroom, meta_floor: int, data_floor: int, unalloc_floor: int
) -> tuple[str, str, dict]:
    meta_hr = hr.pool_headroom("metadata")
    data_hr = hr.pool_headroom("data")
    meta = hr.pools["metadata"]
    data = hr.pools["data"]

    states = {
        "metadata": classify(meta_hr, meta_floor),
        "data": classify(data_hr, data_floor),
        "unallocated": classify(hr.unallocated, unalloc_floor),
    }
    worst = (
        "low"
        if "low" in states.values()
        else "warn" if "warn" in states.values() else "ok"
    )

    def g(n: int) -> str:
        return f"{n / GiB:.1f}GiB"

    breached = [k for k, v in states.items() if v == "low"]
    near = [k for k, v in states.items() if v == "warn"]
    if breached:
        lead = "BELOW FLOOR: " + ", ".join(breached)
    elif near:
        lead = "approaching floor: " + ", ".join(near)
    else:
        lead = "headroom ok"

    summary = (
        f"{lead} on {hr.mount} -- "
        f"metadata headroom {g(meta_hr)} (floor {g(meta_floor)}; "
        f"{g(meta.free_in_allocated)} free in {g(meta.total)} allocated "
        f"+ {g(hr.unallocated)} unallocated / {meta.redundancy:.0f}x), "
        f"data headroom {g(data_hr)} (floor {g(data_floor)}), "
        f"unallocated {g(hr.unallocated)} (floor {g(unalloc_floor)}). "
        f"df would report {g(data.free_in_allocated + hr.unallocated)} free."
    )

    report = {
        "state": worst,
        "summary": summary,
        "mount": hr.mount,
        "source": hr.source,
        "uuid": hr.uuid,
        "metadata_headroom_bytes": meta_hr,
        "metadata_headroom_gib": round(meta_hr / GiB, 2),
        "metadata_state": states["metadata"],
        "metadata_used_bytes": meta.used,
        "metadata_total_bytes": meta.total,
        "metadata_pct_used": round(meta.pct_used, 2),
        "metadata_redundancy": round(meta.redundancy, 3),
        "data_headroom_bytes": data_hr,
        "data_headroom_gib": round(data_hr / GiB, 2),
        "data_state": states["data"],
        "data_used_bytes": data.used,
        "data_total_bytes": data.total,
        "data_pct_used": round(data.pct_used, 2),
        "data_redundancy": round(data.redundancy, 3),
        "unallocated_bytes": hr.unallocated,
        "unallocated_gib": round(hr.unallocated / GiB, 2),
        "unallocated_state": states["unallocated"],
        "device_total_bytes": hr.device_total,
    }
    return worst, summary, report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--gate", action="store_true", help="exit 1 when any headroom is below its floor"
    )
    parser.add_argument("--json", action="store_true", help="emit one JSON report")
    parser.add_argument(
        "--mount", default="/data", help="any path on the btrfs to measure (default: /data)"
    )
    parser.add_argument(
        "--metadata-floor-gib", type=float, default=DEFAULT_METADATA_FLOOR / GiB
    )
    parser.add_argument("--data-floor-gib", type=float, default=DEFAULT_DATA_FLOOR / GiB)
    parser.add_argument(
        "--unallocated-floor-gib", type=float, default=DEFAULT_UNALLOC_FLOOR / GiB
    )
    parser.add_argument(
        "--writers",
        action="store_true",
        help=(
            "also list the largest files held open for writing, via /proc/*/fd; "
            "sees mount-namespace and unlinked-but-held files that du cannot"
        ),
    )
    parser.add_argument(
        "--writers-min-gib",
        type=float,
        default=1.0,
        help="minimum logical size for --writers (default: 1.0)",
    )
    parser.add_argument(
        "--writers-all",
        action="store_true",
        help=(
            "do not restrict --writers to the measured filesystem; shows large "
            "open files anywhere (memfds and other non-disk fds stay excluded)"
        ),
    )
    parser.add_argument(
        "--writers-scan-min-gib", type=float, default=0.05,
        help=(
            "size floor for COLLECTING fds to judge for growth, independent of the "
            "display floor (default: 0.05). Must stay SMALL, and smaller than you "
            "first think: growth needs a BASELINE from the previous tick, so a file "
            "below this floor on tick N is unmeasurable on tick N+1 no matter how "
            "fast it grew. At 0.25 a runaway doing 80 GiB/h went undetected in test "
            "because it was only 0.1 GiB when the first tick sampled it."
        ),
    )
    parser.add_argument(
        "--writers-limit", type=int, default=10,
        help="max writer rows to print; the count suppressed is always reported (default: 10)",
    )
    parser.add_argument(
        "--writer-alert-gib", type=float, default=50.0,
        help=(
            "FAIL if any single open fd reaches this size, REGARDLESS of free space "
            "(default: 50). The largest log from a completing run observed on this box "
            "is 0.16 GiB; the 2026-08-17 runaway reached 767 GiB."
        ),
    )
    parser.add_argument(
        "--writer-growth-gib-per-hour", type=float, default=20.0,
        help=(
            "FAIL if any single open fd grows at least this fast between ticks, "
            "REGARDLESS of free space (default: 20). An uncapped --log=info hermit run "
            "measured ~14 GiB/h; the incident sustained ~160 GiB/h for 28 hours."
        ),
    )
    parser.add_argument(
        "--writers-state", default=None,
        help="JSON file remembering fd sizes between ticks; required for growth detection",
    )
    args = parser.parse_args(argv)

    try:
        hr = measure(args.mount)
    except (NotBtrfs, OSError) as exc:
        # An unmeasurable filesystem is NOT a healthy one.  Exit 2 so the caller
        # can tell "could not determine" from "determined, and fine".
        print("state=unknown")
        print(f"summary=btrfs headroom unavailable for {args.mount}: {exc}")
        return 2

    state, _summary, report = render(
        hr,
        int(args.metadata_floor_gib * GiB),
        int(args.data_floor_gib * GiB),
        int(args.unallocated_floor_gib * GiB),
    )

    writer_reasons: list[str] = []
    if args.writers:
        # Scan WELL BELOW the display floor. Growth is the early signal, and a
        # file growing at 100 GiB/h is only a few hundred MiB in its first
        # minutes -- filtering by size first is how you arrive at the fire
        # having missed the smoke.
        scan_min = min(int(args.writers_min_gib * GiB), int(args.writers_scan_min_gib * GiB))
        collected = large_open_writers(
            hr.mount, min_bytes=scan_min, include_all=args.writers_all
        )
        collected, writer_reasons = writer_alarms(
            collected,
            Path(args.writers_state) if args.writers_state else None,
            time.time(),
            int(args.writer_alert_gib * GiB),
            int(args.writer_growth_gib_per_hour * GiB),
        )
        # Anything that ALARMED is always shown, however small; the rest of the
        # list is the largest fds above the display floor.
        alarming = [w for w in collected if w.get("alarmed")]
        # A fast write that has already FINISHED is shown too, however small, and
        # ranked above the merely-large. It did not alarm because nothing holds
        # the inode open for write any more, but "this grew at 23 GiB/h and then
        # stopped" is exactly the evidence an operator needs to tell a bounded
        # emitter from a runaway -- and suppressing it silently is how the gate
        # would appear to have missed something.
        finished = [
            w for w in collected if not w.get("alarmed") and w.get("growth_completed")
        ]
        rest = [
            w for w in collected
            if not w.get("alarmed")
            and not w.get("growth_completed")
            and w["logical_bytes"] >= int(args.writers_min_gib * GiB)
        ]
        ordered = alarming + finished + rest
        writers = ordered[: args.writers_limit]
        suppressed = max(0, len(ordered) - len(writers))
        report["open_writers"] = writers
        report["open_writers_suppressed"] = suppressed
        report["open_writers_scanned"] = len(collected)
        report["writer_alarms"] = writer_reasons
        growth_unknown = any(w.get("growth_state") == "unknown" for w in collected)
        report["writer_growth_state"] = "unknown" if growth_unknown else "measured"
        if suppressed:
            report["summary"] += (
                f" {suppressed} further writer row(s) SUPPRESSED by "
                f"--writers-limit={args.writers_limit}."
            )
        if writer_reasons:
            # A runaway writer fails the gate on its own, with no reference to
            # how much space is left.
            state = "low"
            report["state"] = "low"
            report["summary"] = (
                "RUNAWAY WRITER: " + "; ".join(writer_reasons) + " -- " + report["summary"]
            )
        elif growth_unknown:
            state = "unknown"
            report["state"] = "unknown"
            report["summary"] = (
                "writer growth is UNKNOWN because no readable prior sample exists; "
                + report["summary"]
            )
        elif writers:
            top = writers[0]
            report["summary"] += (
                f" Largest open fd on this filesystem: pid {top['pid']} ({top['comm']}) "
                f"{top['logical_bytes'] / GiB:.1f}GiB logical"
                f"{' [unlinked, space not freed until closed]' if top['deleted'] else ''}."
            )

    if args.json:
        print(json.dumps(report, sort_keys=True))
    else:
        for key, value in report.items():
            if key == "open_writers":
                for w in value:
                    print(
                        f"writer=pid:{w['pid']} comm:{w['comm']} fd:{w['fd']} "
                        f"logical_bytes:{w['logical_bytes']} deleted:{w['deleted']} "
                        f"on_target_fs:{w['on_target_fs']} "
                        f"writable:{w.get('writable', True)} "
                        f"growth_bytes_per_hour:{w.get('growth_bytes_per_hour')} "
                        f"growth_completed:{bool(w.get('growth_completed'))} "
                        f"path:{w['path_in_its_namespace']}"
                    )
                continue
            print(f"{key}={value}")

    if args.gate and state == "low":
        return 1
    if args.gate and state == "unknown":
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
