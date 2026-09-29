#!/usr/bin/env python3
"""Choose the CPUs a validation launch may use, from the host's PMU binding.

Why this exists: on devbig014 a ptrace-stop-heavy cell costs 7.6-7.9x the CPU
on host CPU 0, almost all of it kernel time
(https://github.com/rrnewton/hermit/issues/3265). CPU 0 is where the AMD
uncore PMUs (`amd_df`, `amd_umc_*`, `amd_iommu_*`, `power`) are bound, and
`amd_l3` is bound to the first CPU of every L3 complex. A host monitor that
keeps those counters running (dynolog with --enable_mem_pcm_counters=true)
makes every context switch on those CPUs expensive. One cell,
`applications/timed-progress-bar [ptrace/verify]`, sat at 31.0 s of its 32 s
CPU budget and killed an exact-head validate at 32.25 s when it landed there.

The remedy here is placement, not budget: a validation launch runs on the
online CPUs minus every CPU named by a PMU `cpumask` file. Nothing here
changes a budget, a timeout, which cells run, or how a verdict is computed.

Derivation (at launch, from the host; nothing is hard-coded):

* Every `/sys/bus/event_source/devices/<pmu>/cpumask` names the CPUs that
  PMU's events are opened on. The union of those masks is excluded.
* A mask spanning more than a quarter of the online CPUs is a per-core PMU
  (for example `power_core=0-157` on a 316-CPU host), not one bound to a
  few CPUs. It is skipped, and the skip is recorded.
* If the result would leave fewer than half the online CPUs, nothing is
  excluded and the reason is recorded.

Fail-safe, recorded rather than silent:

* No PMU `cpumask` file at all: nothing is excluded, source
  `sysfs-none`.
* A `cpumask` (or the online list) that does not parse as a kernel CPU list:
  nothing is excluded, source `sysfs-malformed`, and the detail names the file
  and its bytes. A partial set derived from a sysfs this code cannot read is
  not trustworthy, and excluding nothing is exactly the pre-change behaviour,
  so validation keeps running and the record says why placement was off.
  Refusing the launch instead would stop every validation on a host over a
  performance measure.

Override (documented in `--help` and ci-hub/validate/README.md):

* `DEV_HERMIT_EXCLUDE_CPUS=none` excludes nothing (source `override-none`).
* `DEV_HERMIT_EXCLUDE_CPUS=<cpu list>`, e.g. `0,16`, excludes exactly that
  list (source `override`). A malformed list, or one that would leave no
  online CPU, is an operator error and REFUSES the launch loudly: the operator
  can fix it, and silently ignoring an explicit instruction would hide it.
* Unset, empty, or `auto` derives from sysfs as above.

The launcher passes the decision down under three names, so the Hermit driver
can apply it to its own cgroup scope and record it in the receipt:

* `HERMIT_CI_ALLOWED_CPUS`: the cpuset to apply; empty means no restriction.
* `HERMIT_CI_EXCLUDED_CPUS`: the excluded list; empty when nothing is.
* `HERMIT_CI_CPU_PLACEMENT_SOURCE`: the source token above.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

OVERRIDE_ENV = "DEV_HERMIT_EXCLUDE_CPUS"
ALLOWED_ENV = "HERMIT_CI_ALLOWED_CPUS"
EXCLUDED_ENV = "HERMIT_CI_EXCLUDED_CPUS"
SOURCE_ENV = "HERMIT_CI_CPU_PLACEMENT_SOURCE"

PMU_ROOT = Path("/sys/bus/event_source/devices")
ONLINE_PATH = Path("/sys/devices/system/cpu/online")

# A PMU whose cpumask covers more than this share of online CPUs is per-core,
# not bound to a few CPUs; excluding it would halve the host for no cause.
MAX_PMU_SHARE_NUMERATOR = 1
MAX_PMU_SHARE_DENOMINATOR = 4
# Never exclude so much that fewer than half the online CPUs remain.
MIN_ALLOWED_SHARE_NUMERATOR = 1
MIN_ALLOWED_SHARE_DENOMINATOR = 2

SOURCES = frozenset(
    {
        "sysfs-pmu-cpumask",
        "sysfs-none",
        "sysfs-too-broad",
        "sysfs-malformed",
        "override",
        "override-none",
    }
)
RECORD_FIELDS = frozenset(
    {
        "source",
        "excluded_cpus",
        "allowed_cpus",
        "online_cpus",
        "pmus",
        "skipped_pmus",
        "detail",
        "override_env",
    }
)

_CPU_LIST_RE = re.compile(r"\A(?:\d+(?:-\d+)?)(?:,\d+(?:-\d+)?)*\Z")
_CPU_NUMBER_RE = re.compile(r"\d+")

# The highest CPU number any Linux kernel can have: CONFIG_NR_CPUS tops out at
# 8192 (x86 MAXSMP), numbered from 0. A list naming a higher CPU is refused
# BEFORE any range is expanded, so an override such as `0-999999999` cannot
# make this process build a billion-element set.
MAX_CPU_NUMBER = 8191


class CpuPlacementError(ValueError):
    """An explicit operator override that cannot be honoured."""


def cpu_number_above_ceiling(text: str) -> str | None:
    """The first CPU number in ``text`` above ``MAX_CPU_NUMBER``, else None.

    Compared by digit count before ``int()``: Python refuses to convert a
    decimal string over 4300 digits, and no such string is a CPU number.
    """
    for number in _CPU_NUMBER_RE.findall(text):
        digits = number.lstrip("0") or "0"
        if len(digits) > len(str(MAX_CPU_NUMBER)) or int(digits) > MAX_CPU_NUMBER:
            return number
    return None


def parse_cpu_list(text: str) -> frozenset[int] | None:
    """Parse a kernel CPU list such as ``0-3,8``; ``None`` when malformed.

    An empty string is a valid empty list (a PMU with no bound CPU). A list
    naming a CPU above ``MAX_CPU_NUMBER`` is malformed, and is refused before
    any range is expanded.
    """
    value = text.strip()
    if value == "":
        return frozenset()
    if _CPU_LIST_RE.fullmatch(value) is None:
        return None
    if cpu_number_above_ceiling(value) is not None:
        return None
    cpus: set[int] = set()
    for part in value.split(","):
        low_text, _, high_text = part.partition("-")
        low = int(low_text)
        high = int(high_text) if high_text else low
        if high < low:
            return None
        cpus.update(range(low, high + 1))
    return frozenset(cpus)


def format_cpu_list(cpus: frozenset[int] | set[int] | tuple[int, ...]) -> str:
    """Format CPUs in the kernel's compressed list form, ``""`` when empty."""
    ordered = sorted(set(cpus))
    parts: list[str] = []
    index = 0
    while index < len(ordered):
        start = ordered[index]
        end = start
        while index + 1 < len(ordered) and ordered[index + 1] == end + 1:
            index += 1
            end = ordered[index]
        parts.append(str(start) if start == end else f"{start}-{end}")
        index += 1
    return ",".join(parts)


@dataclass(frozen=True)
class CpuPlacement:
    source: str
    excluded: frozenset[int]
    online: frozenset[int]
    pmus: tuple[str, ...] = ()
    skipped_pmus: tuple[str, ...] = ()
    detail: str = ""

    @property
    def allowed(self) -> frozenset[int]:
        """The CPUs a launch may use; empty means "no restriction"."""
        if not self.excluded:
            return frozenset()
        return self.online - self.excluded

    def environment(self) -> dict[str, str]:
        """The three variables the launcher hands to the Hermit driver."""
        return {
            ALLOWED_ENV: format_cpu_list(self.allowed),
            EXCLUDED_ENV: format_cpu_list(self.excluded),
            SOURCE_ENV: self.source,
        }

    def systemd_property(self) -> str | None:
        """``AllowedCPUs=<list>`` for the launching unit; None when unrestricted."""
        allowed = format_cpu_list(self.allowed)
        return f"AllowedCPUs={allowed}" if allowed else None

    def systemd_properties(self) -> list[str]:
        """``--property AllowedCPUs=<list>`` as argv words, or nothing."""
        value = self.systemd_property()
        return ["--property", value] if value else []

    def record(self) -> dict[str, Any]:
        """The run-record / receipt shape; every list is a kernel CPU list."""
        return {
            "source": self.source,
            "excluded_cpus": format_cpu_list(self.excluded),
            "allowed_cpus": format_cpu_list(self.allowed),
            "online_cpus": format_cpu_list(self.online),
            "pmus": list(self.pmus),
            "skipped_pmus": list(self.skipped_pmus),
            "detail": self.detail,
            "override_env": OVERRIDE_ENV,
        }

    def summary(self) -> str:
        excluded = format_cpu_list(self.excluded) or "none"
        allowed = format_cpu_list(self.allowed) or "all online"
        return (
            f"cpu placement source={self.source} excluded={excluded} "
            f"allowed={allowed}: {self.detail}"
        )


def _read_cpu_list(path: Path) -> tuple[frozenset[int] | None, str]:
    try:
        raw = path.read_text(encoding="ascii")
    except (OSError, UnicodeDecodeError) as error:
        return None, f"{path} is unreadable ({error})"
    parsed = parse_cpu_list(raw)
    if parsed is None:
        return None, f"{path} does not parse as a CPU list: {raw!r}"
    return parsed, ""


def _override(text: str, online: frozenset[int]) -> CpuPlacement:
    value = text.strip()
    if value.lower() == "none":
        return CpuPlacement(
            source="override-none",
            excluded=frozenset(),
            online=online,
            detail=f"{OVERRIDE_ENV}=none: no CPU excluded by operator override",
        )
    too_high = (
        cpu_number_above_ceiling(value)
        if _CPU_LIST_RE.fullmatch(value) is not None
        else None
    )
    if too_high is not None:
        shown = too_high if len(too_high) <= 20 else f"{too_high[:20]}... ({len(too_high)} digits)"
        raise CpuPlacementError(
            f"{OVERRIDE_ENV} names CPU {shown}, above {MAX_CPU_NUMBER}, the highest "
            "CPU number a Linux kernel can have; refused before expanding the list"
        )
    requested = parse_cpu_list(value)
    if requested is None or not requested:
        raise CpuPlacementError(
            f"{OVERRIDE_ENV}={text!r} is not a CPU list (use e.g. '0,16', 'none', or unset it)"
        )
    excluded = requested & online
    if not online - excluded:
        raise CpuPlacementError(
            f"{OVERRIDE_ENV}={text!r} would exclude every online CPU "
            f"({format_cpu_list(online)})"
        )
    ignored = requested - online
    detail = f"{OVERRIDE_ENV}={value}: excluded by operator override"
    if ignored:
        detail += f"; not online and ignored: {format_cpu_list(ignored)}"
    return CpuPlacement(
        source="override", excluded=excluded, online=online, detail=detail
    )


def derive(
    *,
    environ: Mapping[str, str] | None = None,
    pmu_root: Path = PMU_ROOT,
    online_path: Path = ONLINE_PATH,
) -> CpuPlacement:
    """Decide the launch's CPU placement; raise only for a bad override."""
    environ = os.environ if environ is None else environ
    override = environ.get(OVERRIDE_ENV, "")
    online, problem = _read_cpu_list(online_path)
    if online is None or not online:
        if override.strip() and override.strip().lower() not in {"auto", "none"}:
            raise CpuPlacementError(
                f"{OVERRIDE_ENV}={override!r} cannot be applied: "
                f"{problem or f'{online_path} lists no CPU'}"
            )
        return CpuPlacement(
            source="override-none"
            if override.strip().lower() == "none"
            else "sysfs-malformed",
            excluded=frozenset(),
            online=frozenset(),
            detail=problem or f"{online_path} lists no CPU; nothing excluded",
        )
    if override.strip() and override.strip().lower() != "auto":
        return _override(override, online)

    if not pmu_root.is_dir():
        return CpuPlacement(
            source="sysfs-none",
            excluded=frozenset(),
            online=online,
            detail=f"{pmu_root} is absent; no PMU cpumask, nothing excluded",
        )
    contributing: list[tuple[str, frozenset[int]]] = []
    skipped: list[str] = []
    for entry in sorted(pmu_root.iterdir(), key=lambda path: path.name):
        mask_path = entry / "cpumask"
        if not mask_path.is_file():
            continue
        mask, mask_problem = _read_cpu_list(mask_path)
        if mask is None:
            return CpuPlacement(
                source="sysfs-malformed",
                excluded=frozenset(),
                online=online,
                detail=f"{mask_problem}; nothing excluded (a partial set from an "
                "unparseable sysfs is not trusted)",
            )
        bound = mask & online
        if not bound:
            continue
        if (
            len(bound) * MAX_PMU_SHARE_DENOMINATOR
            > len(online) * MAX_PMU_SHARE_NUMERATOR
        ):
            skipped.append(
                f"{entry.name}={format_cpu_list(bound)} "
                f"({len(bound)} of {len(online)} online CPUs: per-core, not bound)"
            )
            continue
        contributing.append((entry.name, bound))

    excluded = frozenset().union(*(mask for _, mask in contributing))
    pmus = tuple(f"{name}={format_cpu_list(mask)}" for name, mask in contributing)
    if not excluded:
        return CpuPlacement(
            source="sysfs-none",
            excluded=frozenset(),
            online=online,
            skipped_pmus=tuple(skipped),
            detail="no PMU cpumask binds a subset of CPUs; nothing excluded",
        )
    remaining = online - excluded
    if (
        len(remaining) * MIN_ALLOWED_SHARE_DENOMINATOR
        < len(online) * MIN_ALLOWED_SHARE_NUMERATOR
    ):
        return CpuPlacement(
            source="sysfs-too-broad",
            excluded=frozenset(),
            online=online,
            pmus=pmus,
            skipped_pmus=tuple(skipped),
            detail=(
                f"PMU cpumasks name {len(excluded)} of {len(online)} online CPUs; "
                "excluding them would leave fewer than half, so nothing is excluded"
            ),
        )
    names = ", ".join(name for name, _ in contributing)
    return CpuPlacement(
        source="sysfs-pmu-cpumask",
        excluded=excluded,
        online=online,
        pmus=pmus,
        skipped_pmus=tuple(skipped),
        detail=(
            f"excluded the CPUs bound by PMU cpumask ({names}); "
            "https://github.com/rrnewton/hermit/issues/3265"
        ),
    )


def validate_record(value: Any) -> str | None:
    """Return why ``value`` is not a well-formed placement record, else None."""
    if not isinstance(value, dict):
        return "must be an object"
    if set(value) != RECORD_FIELDS:
        return f"must carry exactly {sorted(RECORD_FIELDS)}"
    if value["source"] not in SOURCES:
        return f"source {value['source']!r} is not one of {sorted(SOURCES)}"
    for field in ("excluded_cpus", "allowed_cpus", "online_cpus"):
        text = value[field]
        if not isinstance(text, str) or parse_cpu_list(text) is None:
            return f"{field} must be a kernel CPU list string"
    for field in ("pmus", "skipped_pmus"):
        items = value[field]
        if not isinstance(items, list) or not all(isinstance(i, str) for i in items):
            return f"{field} must be a list of strings"
    if not isinstance(value["detail"], str) or not value["detail"]:
        return "detail must be a non-empty string"
    if value["override_env"] != OVERRIDE_ENV:
        return f"override_env must be {OVERRIDE_ENV!r}"
    excluded = parse_cpu_list(value["excluded_cpus"]) or frozenset()
    allowed = parse_cpu_list(value["allowed_cpus"]) or frozenset()
    online = parse_cpu_list(value["online_cpus"]) or frozenset()
    if excluded & allowed:
        return "allowed_cpus and excluded_cpus overlap"
    if bool(excluded) != bool(allowed):
        return "allowed_cpus is empty exactly when nothing is excluded"
    if excluded and not excluded <= online:
        return "excluded_cpus must be a subset of online_cpus"
    if excluded and allowed != online - excluded:
        return "allowed_cpus must equal online_cpus minus excluded_cpus"
    return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Print the CPU placement a validation launch would use on this host: "
            "online CPUs minus those bound by a PMU cpumask "
            "(https://github.com/rrnewton/hermit/issues/3265). "
            f"Override with {OVERRIDE_ENV}=none (exclude nothing) or "
            f"{OVERRIDE_ENV}=<cpu list> (exclude exactly that list)."
        )
    )
    parser.add_argument("--json", action="store_true", help="print the record as JSON")
    parser.add_argument(
        "--allowed", action="store_true", help="print only the allowed CPU list"
    )
    args = parser.parse_args(argv)
    try:
        placement = derive()
    except CpuPlacementError as error:
        print(f"cpu_placement: REFUSED: {error}", file=sys.stderr)
        return 2
    if args.allowed:
        print(format_cpu_list(placement.allowed))
    elif args.json:
        print(json.dumps(placement.record(), indent=2, sort_keys=True))
    else:
        print(placement.summary())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
