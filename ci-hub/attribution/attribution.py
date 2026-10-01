#!/usr/bin/env python3
"""Flaky-failure ATTRIBUTION: turn "N/10 flaky" into a CAUSE with evidence.

A flake *rate* tells you nothing about the *cause*. Three causes present
identically as "N/10 flaky" and demand OPPOSITE responses:

  INFRASTRUCTURE       runner/load problem (a check wedged 2h44m vs a ~2-min
                       baseline; PMU-sensitive tests on a contended runner; a
                       backend unable to measure anything at ~470 concurrent
                       hermit procs).  => fix the runner/load; DO NOT touch
                       product code.
  HERMIT_NONDETERMINISM a real determinism bug in the product (the detcore_misc
                       vfork-reap race, 16-23%).  => highest-value fix; localize
                       it to a divergence point + stack.
  ENVIRONMENT          the guest read a genuinely-varying host resource (the
                       /sys/module/.../refcnt read on command_strict_verify: a
                       nondeterministic sysfs value that changes under load).
                       Not hermit's nondeterminism, not infra.  => determinize /
                       virtualize that read.

Conflating them means either chasing a product bug that is actually infra, or
dismissing a real determinism bug as "just flaky" -- we have done both.

This module provides two things the harnesses lack:

  1. AN EVIDENCE-CAPTURE PRIMITIVE (`capture_run`) that any harness can call
     INSTEAD of `... >/dev/null 2>&1; echo $?`.  On failure it preserves the
     full stdout/stderr, exit code, wall time, whether it timed out, and the
     HOST CONDITIONS AT THAT MOMENT (load average, concurrent-process count,
     CPU pressure, memory) into a bundle directory.  You cannot attribute a
     failure whose evidence was discarded; this is the prerequisite.

  2. AN ATTRIBUTION CLASSIFIER (`attribute`) that folds the codified signals
     into a verdict + the evidence for it:
       (a) FAILURE SHAPE      -- hang/timeout vs output-mismatch vs crash vs
                                 nonzero-exit vs harness/build error.
       (b) HOST PRESSURE      -- load, concurrent procs, PSI at the failure.
       (c) DIVERGENCE CLASS   -- for --verify mismatches, whether the FIRST
                                 divergence is a COMMIT line (schedule diverged
                                 => HERMIT) or a DETLOG line whose value is a
                                 host reading (=> ENVIRONMENT).  This is the
                                 `hermit log-diff` differential technique.
       (d) EXTERNAL READS     -- did the trace show the guest reading a varying
                                 host resource (sysfs/proc/time/rand/cpuid)?
       (e) LOW-LOAD CONTROL   -- the single most decisive test: does it still
                                 fail when the host is quiet?

The classifier is a pure function of an `Evidence` object so it is unit-tested
without needing hermit or a loaded host.  The CLI builds `Evidence` from a
captured bundle (and, optionally, from a low-load control re-run and a
`hermit log-diff` of two traces) and prints the verdict.

Design mirrors the sibling ci-hub tools (operational_health.py; formerly also ci-hub/stress/stress_store.py, deleted 2026-10-01):
stdlib only, exit code carries the signal, loud + structured on every path.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import re
import resource
import shlex
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

# The cpu/wall kill signature (livelock vs contention) lives in ONE table shared
# with ci-hub/history/query.py so the two consumers of the same physical fact
# cannot drift apart.  ci-hub/lib is a sibling package dir with no __init__, so
# it is added to sys.path explicitly rather than imported as a package.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "lib"))
import kill_signature as ks  # noqa: E402

SCHEMA_VERSION = 2  # 2: run bundles carry cpu_s/oom for the kill signature

# --------------------------------------------------------------------------- verdicts

INFRASTRUCTURE = "INFRASTRUCTURE"
HERMIT_NONDETERMINISM = "HERMIT_NONDETERMINISM"
ENVIRONMENT = "ENVIRONMENT"
HARNESS_ERROR = "HARNESS_ERROR"
INDETERMINATE = "INDETERMINATE"

ALL_VERDICTS = (
    INFRASTRUCTURE,
    HERMIT_NONDETERMINISM,
    ENVIRONMENT,
    HARNESS_ERROR,
    INDETERMINATE,
)

# What to DO for each verdict -- the whole point is that they differ.
NEXT_STEP = {
    INFRASTRUCTURE: (
        "Fix the runner/load (reduce concurrency, isolate the job, add capacity). "
        "Do NOT change product code -- there is no product bug here."
    ),
    HERMIT_NONDETERMINISM: (
        "Real determinism bug -- highest value. Localize the first divergence "
        "with `hermit log-diff --syscall-history N` and grab the guest stack at "
        "that event; file/fix the determinism defect."
    ),
    ENVIRONMENT: (
        "The guest read a varying host resource. Determinize/virtualize that "
        "read (sysfs/proc/time/rand/cpuid) in detcore; it is neither infra nor "
        "a scheduling bug."
    ),
    HARNESS_ERROR: (
        "The harness itself could not run the workload (build/setup/probe "
        "failure). Fix the harness; this is not a flake of the subject."
    ),
    INDETERMINATE: (
        "Insufficient evidence to attribute. Run the named next probe (usually "
        "the low-load control re-run and/or two --log info traces + log-diff)."
    ),
}

# --------------------------------------------------------------------------- failure shape

SHAPE_HANG = "hang"                # timed out / no forward progress
SHAPE_MISMATCH = "mismatch"        # --verify said nondeterministic (outcome differs)
SHAPE_CRASH = "crash"              # panic / fatal signal
SHAPE_NONZERO = "nonzero-exit"     # exited nonzero, not a crash
SHAPE_HARNESS = "harness"          # build/setup/probe error, subject never ran
SHAPE_PASS = "pass"                # did not fail

# Substrings that mark a run as a crash rather than an ordinary nonzero exit.
_CRASH_MARKERS = (
    "panicked at",
    "SIGSEGV",
    "SIGABRT",
    "signal: 11",
    "signal: 6",
    "segmentation fault",
    "core dumped",
    "fatal runtime error",
    "assertion failed",
    "double free",
    "AddressSanitizer",
)

# A hermit/detcore-verify mismatch: two runs disagreed.
_MISMATCH_MARKERS = (
    "nondeterministic",
    "non-deterministic",
    "verify failed",
    "runs diverged",
    "desynced",
    "replay diverged",
)

# Harness/build failure tokens (the former ci-hub/stress burst CSV STATUS vocabulary).
_HARNESS_MARKERS = (
    "BUILD_FAIL",
    "WT_FAIL",
    "NOBIN",
    "NOTEST",
    "MATCHED_MISSING",
    "CALIB_MISSING",
    "PROBE_FAIL",
    "error: could not compile",
    "No such file or directory",
)

# Host resources whose value legitimately varies run-to-run / under load. A guest
# read of one of these that reaches the trace as a passthrough is an ENVIRONMENT
# (unvirtualized external read) signal, not hermit nondeterminism.
_EXTERNAL_READ_PATTERNS = (
    r"/sys/",
    r"/proc/(?!self/maps)",          # /proc reads (meminfo, stat, loadavg, module refcnt)
    r"\bclock_gettime\b",
    r"\bgettimeofday\b",
    r"\bgetrandom\b",
    r"\brdtsc\b",
    r"\bcpuid\b",
    r"\bsysinfo\b",
    r"\buname\b",
    r"meminfo",
    r"loadavg",
    r"refcnt",
)
_EXTERNAL_READ_RE = re.compile("|".join(_EXTERNAL_READ_PATTERNS))


# --------------------------------------------------------------------------- host conditions


def _read_float_list(path: str) -> list[float]:
    try:
        with open(path, encoding="utf-8") as handle:
            return [float(x) for x in handle.read().split()[:3]]
    except (OSError, ValueError):
        return []


def _psi_avg10(path: str) -> Optional[float]:
    """CPU/memory Pressure Stall Information avg10 (percent), if the kernel has PSI."""
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("some"):
                    match = re.search(r"avg10=([0-9.]+)", line)
                    if match:
                        return float(match.group(1))
    except OSError:
        return None
    return None


def _count_procs(pattern: str) -> Optional[int]:
    """Count live processes whose comm/cmdline matches `pattern` (cheap /proc scan)."""
    if not pattern:
        return None
    needle = pattern.encode()
    count = 0
    try:
        for entry in os.scandir("/proc"):
            if not entry.name.isdigit():
                continue
            try:
                with open(f"/proc/{entry.name}/comm", "rb") as handle:
                    if needle in handle.read():
                        count += 1
            except OSError:
                continue
    except OSError:
        return None
    return count


@dataclass
class HostConditions:
    """A snapshot of contention on the host at one instant."""

    load1: Optional[float] = None
    load5: Optional[float] = None
    load15: Optional[float] = None
    nproc: Optional[int] = None
    concurrent_procs: Optional[int] = None       # matching a caller-supplied pattern
    proc_pattern: str = ""
    cpu_pressure_avg10: Optional[float] = None    # PSI %, 0..100
    mem_pressure_avg10: Optional[float] = None
    mem_avail_ratio: Optional[float] = None       # MemAvailable / MemTotal
    captured_at: str = ""

    @staticmethod
    def sample(proc_pattern: str = "hermit") -> "HostConditions":
        load = _read_float_list("/proc/loadavg")
        mem_total = mem_avail = None
        try:
            with open("/proc/meminfo", encoding="utf-8") as handle:
                for line in handle:
                    if line.startswith("MemTotal:"):
                        mem_total = float(line.split()[1])
                    elif line.startswith("MemAvailable:"):
                        mem_avail = float(line.split()[1])
        except OSError:
            pass
        ratio = (mem_avail / mem_total) if (mem_total and mem_avail) else None
        return HostConditions(
            load1=load[0] if len(load) > 0 else None,
            load5=load[1] if len(load) > 1 else None,
            load15=load[2] if len(load) > 2 else None,
            nproc=os.cpu_count(),
            concurrent_procs=_count_procs(proc_pattern),
            proc_pattern=proc_pattern,
            cpu_pressure_avg10=_psi_avg10("/proc/pressure/cpu"),
            mem_pressure_avg10=_psi_avg10("/proc/pressure/memory"),
            mem_avail_ratio=round(ratio, 4) if ratio is not None else None,
            captured_at=_utc_now(),
        )

    def as_dict(self) -> dict[str, Any]:
        return dataclasses.asdict(self)


# Thresholds for "the host was contended when this failed". Tunable via env so a
# 316-core box and a 4-core runner can share the same tool.
def _pressure_thresholds() -> dict[str, float]:
    return {
        # load1 as a MULTIPLE of nproc; >1.5x cores == meaningfully oversubscribed.
        "load_per_core": float(os.environ.get("ATTR_LOAD_PER_CORE", "1.5")),
        # PSI cpu avg10 percent above which CPU is a bottleneck.
        "cpu_psi": float(os.environ.get("ATTR_CPU_PSI", "20")),
        # absolute concurrent-process count that alone indicates a stampede.
        "procs": float(os.environ.get("ATTR_PROC_STAMPEDE", "100")),
        # MemAvailable ratio below which memory is a bottleneck.
        "mem_avail": float(os.environ.get("ATTR_MEM_AVAIL", "0.1")),
    }


def host_under_pressure(host: Optional[HostConditions]) -> tuple[bool, list[str]]:
    """True + the reasons if the host looks contended enough to cause infra flakes."""
    if host is None:
        return False, []
    thr = _pressure_thresholds()
    reasons: list[str] = []
    if host.load1 is not None and host.nproc:
        if host.load1 > thr["load_per_core"] * host.nproc:
            reasons.append(
                f"load1={host.load1:.1f} > {thr['load_per_core']}x{host.nproc} cores"
            )
    if host.cpu_pressure_avg10 is not None and host.cpu_pressure_avg10 > thr["cpu_psi"]:
        reasons.append(f"cpu PSI avg10={host.cpu_pressure_avg10:.0f}% > {thr['cpu_psi']:.0f}%")
    if host.concurrent_procs is not None and host.concurrent_procs > thr["procs"]:
        reasons.append(
            f"{host.concurrent_procs} concurrent '{host.proc_pattern}' procs "
            f"> {int(thr['procs'])}"
        )
    if host.mem_avail_ratio is not None and host.mem_avail_ratio < thr["mem_avail"]:
        reasons.append(f"MemAvailable {host.mem_avail_ratio*100:.0f}% < {thr['mem_avail']*100:.0f}%")
    return (bool(reasons), reasons)


# --------------------------------------------------------------------------- evidence


@dataclass
class Divergence:
    """Result of classifying WHERE two --verify traces first diverged."""

    klass: str                       # "commit" | "detlog" | "none"
    first_line: str = ""
    host_value_shaped: bool = False  # the differing DETLOG value looks like a host reading

    def as_dict(self) -> dict[str, Any]:
        return dataclasses.asdict(self)


@dataclass
class LowLoadControl:
    """The decisive re-run: same command, K times, on a quiet host."""

    runs: int
    failures: int

    @property
    def clean(self) -> bool:
        return self.runs > 0 and self.failures == 0

    def as_dict(self) -> dict[str, Any]:
        return {"runs": self.runs, "failures": self.failures, "clean": self.clean}


@dataclass
class Evidence:
    """Everything known about ONE failing run, fed to `attribute`."""

    shape: str
    host: Optional[HostConditions] = None
    divergence: Optional[Divergence] = None
    low_load: Optional[LowLoadControl] = None
    external_reads: list[str] = field(default_factory=list)
    exit_code: Optional[int] = None
    timed_out: bool = False
    wall_s: Optional[float] = None
    cpu_s: Optional[float] = None         # subject CPU seconds (user+sys), for the kill signature
    oom: bool = False                     # memory ceiling fired -- excludes the ratio test
    baseline_s: Optional[float] = None    # a healthy run's wall time, for hang shape
    note: str = ""

    def kill_kind(self) -> Optional[str]:
        """Which budget fired, or None when this run was not killed at a budget.

        The cpu/wall signature is only defined at a kill, so this gate decides
        whether the ratio may be consulted at all.
        """
        if self.oom:
            return ks.KILL_OOM
        if self.timed_out:
            return ks.KILL_WALL_TIMEOUT
        return None

    def kill_signature(self) -> tuple[Optional[float], str]:
        """(cpu/wall ratio, kill verdict) using the ONE shared threshold table."""
        ratio = ks.cpu_wall_ratio(self.cpu_s, self.wall_s)
        return ratio, ks.classify_kill(self.kill_kind(), ratio)

    def as_dict(self) -> dict[str, Any]:
        out = dataclasses.asdict(self)
        return out


@dataclass
class Attribution:
    verdict: str
    confidence: str                  # "high" | "medium" | "low"
    reasons: list[str]
    next_step: str
    signals: dict[str, Any] = field(default_factory=dict)

    def as_dict(self) -> dict[str, Any]:
        return dataclasses.asdict(self)

    def one_line(self) -> str:
        return (
            f"ATTRIBUTION={self.verdict} confidence={self.confidence} "
            f"reasons=[{'; '.join(self.reasons)}]"
        )


# --------------------------------------------------------------------------- the classifier


def attribute(ev: Evidence) -> Attribution:
    """Fold the codified signals into a verdict. Pure function -- unit-testable.

    Ordering matters: we consult the MOST DECISIVE, least-ambiguous signals
    first (a localizable schedule divergence, then a host-read divergence), and
    fall back to shape + host-pressure + the low-load control. When nothing is
    decisive we return INDETERMINATE with the exact next probe rather than a
    confident guess -- a wrong attribution is worse than an honest "don't know".
    """
    pressure, pressure_reasons = host_under_pressure(ev.host)
    signals: dict[str, Any] = {
        "shape": ev.shape,
        "host_pressure": pressure,
        "pressure_reasons": pressure_reasons,
    }
    if ev.divergence is not None:
        signals["divergence"] = ev.divergence.as_dict()
    if ev.low_load is not None:
        signals["low_load"] = ev.low_load.as_dict()
    if ev.external_reads:
        signals["external_reads"] = ev.external_reads[:10]

    # 0. Harness/build failures are their own bucket -- the subject never ran, so
    #    it is neither a product flake nor infra load; the harness is broken.
    if ev.shape == SHAPE_HARNESS:
        return Attribution(
            HARNESS_ERROR, "high",
            ["harness/build/probe failed before the subject ran"
             + (f": {ev.note}" if ev.note else "")],
            NEXT_STEP[HARNESS_ERROR], signals,
        )

    # 1. A localizable OUTCOME divergence is the strongest product signal, and it
    #    is what separates a real load-dependent HERMIT race from an infra hang:
    #    both need load, but only the product bug produces a divergence you can
    #    point to.  (hermit-debugging skill sec.2: first diff COMMIT => schedule
    #    diverged; DETLOG-only => data/source diverged.)
    if ev.divergence is not None and ev.divergence.klass == "commit":
        return Attribution(
            HERMIT_NONDETERMINISM, "high",
            [f"schedule diverged at a COMMIT line ({_short(ev.divergence.first_line)}) "
             "-- nondeterministic thread interleaving"],
            NEXT_STEP[HERMIT_NONDETERMINISM], signals,
        )
    if ev.divergence is not None and ev.divergence.klass == "detlog":
        if ev.divergence.host_value_shaped or ev.external_reads:
            reason = (
                f"COMMITs identical but a DETLOG value diverged "
                f"({_short(ev.divergence.first_line)}) and it reads like a host "
                "resource -- an unvirtualized external read"
            )
            return Attribution(ENVIRONMENT, "high", [reason], NEXT_STEP[ENVIRONMENT], signals)
        # DETLOG data diverged but not obviously a host read: schedule is stable
        # yet a value differs -- still a product determinism defect, just not an
        # obvious external source.  Medium confidence.
        return Attribution(
            HERMIT_NONDETERMINISM, "medium",
            [f"COMMITs identical but a DETLOG value diverged "
             f"({_short(ev.divergence.first_line)}); no obvious host source -- "
             "unvirtualized data path"],
            NEXT_STEP[HERMIT_NONDETERMINISM], signals,
        )

    # 2. No localizable divergence captured. Reason from shape + pressure + control.
    if ev.shape == SHAPE_HANG:
        # 2a. THE KILL SIGNATURE -- free, and decisive at the extremes.
        #
        # This runs BEFORE the low-load control on purpose.  The control is the
        # better test but it costs K extra runs; the cpu/wall ratio is already
        # measured in the bundle and answers the same infra-vs-product question
        # FROM THE FAILING RUN'S OWN EVIDENCE, which is exactly what the owner
        # asked for.  It only speaks at the extremes: the middle band falls
        # through to the control below rather than forcing a verdict.
        #
        # OOM is handled first inside classify_kill -- an OOM row's ratio can be
        # >100 (parallel build hitting a memory ceiling) and would otherwise be
        # misread as a livelock.
        ratio, kill_verdict = ev.kill_signature()
        if kill_verdict != ks.UNKNOWN:
            signals["cpu_wall_ratio"] = None if ratio is None else round(ratio, 3)
            signals["kill_verdict"] = kill_verdict
            signals["retry_futile"] = ks.retry_futile(kill_verdict)
        if kill_verdict == ks.LIVELOCK:
            return Attribution(
                HERMIT_NONDETERMINISM, "high",
                [ks.explain(kill_verdict, ratio),
                 "a spin to the budget is a PRODUCT defect, not a flake: re-running "
                 "cannot clear it, so this red is REAL"],
                NEXT_STEP[HERMIT_NONDETERMINISM], signals,
            )
        # NOTE the ASYMMETRY -- this is the subtle part.  The signature is only
        # decisive in the SPIN direction.  A high ratio means the subject was
        # definitely computing, and nothing but the product can burn a core for
        # a whole budget.  A LOW ratio only means "not spinning", which does NOT
        # imply infrastructure: a starved process and a futex/deadlock wedge are
        # INDISTINGUISHABLE on cpu/wall -- both sit at cpu ~= 0 -- and they are
        # opposite causes (infra vs product).  So CONTENTION does not decide
        # here; it enriches the signals and falls through to the low-load
        # control, which CAN separate starvation from a wedge.
        if kill_verdict == ks.OOM:
            return Attribution(
                INFRASTRUCTURE, "medium",
                [ks.explain(kill_verdict, ratio),
                 "memory ceiling, not a determinism defect; confirm whether the "
                 "ceiling or the workload is wrong before blaming the commit"],
                NEXT_STEP[INFRASTRUCTURE], signals,
            )

        # 2b. Ratio absent or ambiguous. The low-load control is decisive for a hang.
        if ev.low_load is not None:
            if ev.low_load.clean and pressure:
                return Attribution(
                    INFRASTRUCTURE, "high",
                    [f"hangs under load ({', '.join(pressure_reasons)}) but "
                     f"{ev.low_load.runs}/{ev.low_load.runs} clean at low load; "
                     "no localizable divergence -- starvation/contention, not a "
                     "product race"],
                    NEXT_STEP[INFRASTRUCTURE], signals,
                )
            if ev.low_load.clean and not pressure:
                # Clean when quiet but pressure wasn't recorded high at failure:
                # still load-shaped, but weaker evidence.
                return Attribution(
                    INFRASTRUCTURE, "medium",
                    [f"hang that is {ev.low_load.runs}/{ev.low_load.runs} clean at "
                     "low load; load-dependent but host pressure at failure was "
                     "not captured/high"],
                    NEXT_STEP[INFRASTRUCTURE], signals,
                )
            if ev.low_load.failures > 0:
                # STILL hangs when quiet: not infra. A real wedge.
                return Attribution(
                    HERMIT_NONDETERMINISM, "medium",
                    [f"hangs even at low load ({ev.low_load.failures}/"
                     f"{ev.low_load.runs}) -- not load-dependent; scheduler/futex "
                     "wedge suspect, capture --log info of the hang"],
                    NEXT_STEP[HERMIT_NONDETERMINISM], signals,
                )
        # No control yet: lean by pressure but stay INDETERMINATE (run the test).
        lean = "host was contended" if pressure else "host pressure not high/unknown"
        if kill_verdict == ks.CONTENTION:
            lean += (
                f"; cpu/wall={ratio:.3f} shows it was WAITING, not spinning -- but "
                "starvation and a futex/deadlock wedge look identical at cpu~=0, so "
                "this rules out a livelock WITHOUT choosing infra vs product"
            )
        return Attribution(
            INDETERMINATE, "low",
            [f"hang with no localizable divergence and no low-load control ({lean})"],
            "RUN THE DECISIVE TEST: re-run the SAME command K times at low load "
            "(`attribution.py attribute <bundle> --low-load-control K`). Clean at "
            "low load + pressure at failure => INFRASTRUCTURE; still hangs => "
            "capture `--log info` and look for a scheduler wedge (HERMIT).",
            signals,
        )

    if ev.shape == SHAPE_CRASH:
        if ev.low_load is not None and ev.low_load.runs > 0 and ev.low_load.failures == ev.low_load.runs:
            return Attribution(
                HERMIT_NONDETERMINISM, "high",
                [f"crashes {ev.low_load.failures}/{ev.low_load.runs} even at low "
                 "load -- a deterministic product crash, not a flake"],
                NEXT_STEP[HERMIT_NONDETERMINISM], signals,
            )
        if ev.external_reads:
            return Attribution(
                ENVIRONMENT, "medium",
                ["crash correlated with a varying host read: "
                 f"{', '.join(ev.external_reads[:3])}"],
                NEXT_STEP[ENVIRONMENT], signals,
            )
        return Attribution(
            INDETERMINATE, "low",
            ["crash without a low-load control or external-read signal"],
            "Re-run at low load: always-crashes => product bug (HERMIT); "
            "crashes only under load with no divergence => likely INFRASTRUCTURE "
            "(OOM/kill); scan the trace for external reads.",
            signals,
        )

    if ev.shape == SHAPE_MISMATCH:
        # --verify reported nondeterminism but we don't yet have the divergence
        # class. That single log-diff is the whole game.
        if ev.external_reads:
            return Attribution(
                ENVIRONMENT, "medium",
                ["outcome mismatch and the trace shows a varying host read: "
                 f"{', '.join(ev.external_reads[:3])}"],
                NEXT_STEP[ENVIRONMENT], signals,
            )
        return Attribution(
            INDETERMINATE, "low",
            ["--verify mismatch without a divergence classification"],
            "Capture two `--log info` traces of the SAME input and run "
            "`hermit log-diff` (`attribution.py attribute <bundle> --log-a A "
            "--log-b B`). First diff COMMIT => HERMIT (schedule); DETLOG host "
            "value => ENVIRONMENT.",
            signals,
        )

    if ev.shape == SHAPE_NONZERO:
        if ev.external_reads:
            return Attribution(
                ENVIRONMENT, "low",
                ["nonzero exit with a varying host read in the trace: "
                 f"{', '.join(ev.external_reads[:3])}"],
                NEXT_STEP[ENVIRONMENT], signals,
            )
        if ev.low_load is not None and ev.low_load.clean and pressure:
            return Attribution(
                INFRASTRUCTURE, "medium",
                [f"nonzero exit under load ({', '.join(pressure_reasons)}); "
                 f"{ev.low_load.runs}/{ev.low_load.runs} clean at low load"],
                NEXT_STEP[INFRASTRUCTURE], signals,
            )
        return Attribution(
            INDETERMINATE, "low",
            [f"nonzero exit (code={ev.exit_code}) with no decisive signal"],
            "Re-run at low load and capture a `--log info` trace; check whether "
            "the exit is a host-dependent read or load-only.",
            signals,
        )

    # SHAPE_PASS or anything unexpected.
    return Attribution(
        INDETERMINATE, "low",
        [f"no failure signal to attribute (shape={ev.shape})"],
        NEXT_STEP[INDETERMINATE], signals,
    )


def classify_shape(
    *, exit_code: Optional[int], timed_out: bool, text: str
) -> str:
    """Infer the failure shape from exit code + timeout + combined stdout/stderr."""
    if _contains_any(text, _HARNESS_MARKERS):
        return SHAPE_HARNESS
    if timed_out or exit_code == 124:
        return SHAPE_HANG
    if _contains_any(text, _MISMATCH_MARKERS):
        return SHAPE_MISMATCH
    if _contains_any(text, _CRASH_MARKERS):
        return SHAPE_CRASH
    # Fatal-signal exit codes from a shell: 128 + signum.
    if exit_code is not None and exit_code > 128 and exit_code != 255:
        return SHAPE_CRASH
    if exit_code not in (None, 0):
        return SHAPE_NONZERO
    return SHAPE_PASS


def scan_external_reads(text: str, limit: int = 25) -> list[str]:
    """Lines in a trace that show the guest touching a varying host resource."""
    hits: list[str] = []
    for line in text.splitlines():
        if _EXTERNAL_READ_RE.search(line):
            hits.append(line.strip()[:200])
            if len(hits) >= limit:
                break
    return hits


# --------------------------------------------------------------------------- capture primitive


@dataclass
class RunResult:
    label: str
    cmd: list[str]
    exit_code: Optional[int]
    timed_out: bool
    wall_s: float
    cpu_s: Optional[float]
    failed: bool
    shape: str
    bundle_dir: Optional[str]
    host_before: dict[str, Any]
    host_after: dict[str, Any]

    def as_dict(self) -> dict[str, Any]:
        return dataclasses.asdict(self)


def capture_run(
    cmd: list[str],
    *,
    label: str = "run",
    timeout_s: Optional[float] = None,
    bundle_root: Optional[Path] = None,
    proc_pattern: str = "hermit",
    keep_on_success: bool = False,
    success_exit_codes: tuple[int, ...] = (0,),
    env: Optional[dict[str, str]] = None,
) -> RunResult:
    """Run `cmd` once and PRESERVE the failing run's evidence.

    The drop-in replacement for `... >/dev/null 2>&1; echo $?`: on failure it
    writes a bundle directory with stdout, stderr, and meta.json (exit code,
    timed_out, wall_s, and host conditions sampled just before and just after
    the run) so the failure can be attributed later without re-running until it
    breaks again.
    """
    host_before = HostConditions.sample(proc_pattern)
    # CPU seconds actually burned by the subject, for the cpu/wall kill
    # signature.  RUSAGE_CHILDREN is cumulative over all reaped children of THIS
    # process, so we take a delta around the run.  It only counts children that
    # have been WAITED FOR -- subprocess.run waits, so the subject is included;
    # a detached grandchild that outlives the timeout is not, which is why a
    # timed-out run's cpu is a lower bound (recorded as such in the bundle).
    ru_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    timed_out = False
    try:
        completed = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout_s,
            env=env,
        )
        exit_code: Optional[int] = completed.returncode
        out_bytes = completed.stdout or b""
        err_bytes = completed.stderr or b""
    except subprocess.TimeoutExpired as expired:
        timed_out = True
        exit_code = 124
        out_bytes = expired.stdout or b""
        err_bytes = expired.stderr or b""
    except FileNotFoundError as missing:
        timed_out = False
        exit_code = 127
        out_bytes = b""
        err_bytes = str(missing).encode()
    wall_s = round(time.monotonic() - started, 3)
    ru_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu_s = round(
        (ru_after.ru_utime - ru_before.ru_utime)
        + (ru_after.ru_stime - ru_before.ru_stime),
        3,
    )
    host_after = HostConditions.sample(proc_pattern)

    out_text = out_bytes.decode("utf-8", "replace")
    err_text = err_bytes.decode("utf-8", "replace")
    shape = classify_shape(
        exit_code=exit_code, timed_out=timed_out, text=out_text + "\n" + err_text
    )
    failed = timed_out or (exit_code not in success_exit_codes)

    bundle_dir: Optional[str] = None
    if bundle_root is not None and (failed or keep_on_success):
        bundle = Path(bundle_root) / f"{label}-{_stamp()}-{uuid.uuid4().hex[:8]}"
        bundle.mkdir(parents=True, exist_ok=True)
        (bundle / "stdout").write_text(out_text, encoding="utf-8")
        (bundle / "stderr").write_text(err_text, encoding="utf-8")
        meta = {
            "schema_version": SCHEMA_VERSION,
            "label": label,
            "cmd": cmd,
            "cmd_str": " ".join(shlex.quote(c) for c in cmd),
            "exit_code": exit_code,
            "timed_out": timed_out,
            "wall_s": wall_s,
            # cpu_s enables the cpu/wall kill signature (livelock vs contention).
            # On a timed-out run this is a LOWER BOUND: RUSAGE_CHILDREN only counts
            # reaped children, so a killed subject's unreaped descendants are absent.
            "cpu_s": cpu_s,
            "cpu_s_is_lower_bound": timed_out,
            "failed": failed,
            "shape": shape,
            "captured_at": _utc_now(),
            "host_before": host_before.as_dict(),
            "host_after": host_after.as_dict(),
            "external_reads": scan_external_reads(err_text),
        }
        (bundle / "meta.json").write_text(
            json.dumps(meta, indent=2, sort_keys=True), encoding="utf-8"
        )
        bundle_dir = str(bundle)

    return RunResult(
        label=label,
        cmd=cmd,
        exit_code=exit_code,
        timed_out=timed_out,
        wall_s=wall_s,
        cpu_s=cpu_s,
        failed=failed,
        shape=shape,
        bundle_dir=bundle_dir,
        host_before=host_before.as_dict(),
        host_after=host_after.as_dict(),
    )


# --------------------------------------------------------------------------- divergence via hermit log-diff


def classify_divergence(
    log_a: Path, log_b: Path, *, hermit_bin: Optional[str] = None
) -> Divergence:
    """Use `hermit log-diff` to say WHERE two --verify traces first diverge.

    Runs it twice: COMMIT-only (`--skip-detlog`) tells us whether the SCHEDULE
    diverged; if not, DETLOG-only (`--skip-commit`) tells us a data/source value
    diverged.  A schedule divergence is a hermit interleaving bug; a DETLOG-only
    divergence whose value reads like a host resource is an unvirtualized
    external read (ENVIRONMENT).
    """
    hermit = hermit_bin or _find_hermit()
    if hermit is None:
        return Divergence("none", first_line="(hermit binary not found for log-diff)")

    # `hermit log-diff` exits 1 when the two traces diverge, 0 when identical.
    # COMMIT-only first: a schedule divergence outranks a data one (a different
    # interleaving explains any downstream data diff).
    commit_rc, commit_out = _run_log_diff(hermit, log_a, log_b, extra=["--skip-detlog"])
    if commit_rc == 1:
        return Divergence("commit", first_line=_first_meaningful_line(commit_out))

    detlog_rc, detlog_out = _run_log_diff(hermit, log_a, log_b, extra=["--skip-commit"])
    if detlog_rc == 1:
        return Divergence(
            "detlog",
            first_line=_first_meaningful_line(detlog_out),
            host_value_shaped=bool(_EXTERNAL_READ_RE.search(detlog_out)),
        )

    if commit_rc not in (0, 1) or detlog_rc not in (0, 1):
        return Divergence("none", first_line=f"(log-diff errored rc={commit_rc}/{detlog_rc})")
    return Divergence("none", first_line="(log-diff found no COMMIT/DETLOG divergence)")


def _run_log_diff(hermit: str, a: Path, b: Path, *, extra: list[str]) -> tuple[int, str]:
    try:
        completed = subprocess.run(
            [hermit, "log-diff", *extra, "--limit", "1", str(a), str(b)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=120,
        )
        return completed.returncode, completed.stdout.decode("utf-8", "replace")
    except (subprocess.TimeoutExpired, OSError) as error:
        return 3, f"(log-diff error: {error})"


def _find_hermit() -> Optional[str]:
    for candidate in (
        os.environ.get("HERMIT_BIN"),
        str(_root() / "hermit/target/release/hermit"),
        str(_root() / "hermit/target/debug/hermit"),
    ):
        if candidate and Path(candidate).is_file():
            return candidate
    return None


# --------------------------------------------------------------------------- helpers


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")


def _root() -> Path:
    return Path(__file__).resolve().parents[2]


def _contains_any(text: str, needles: tuple[str, ...]) -> bool:
    return any(n in text for n in needles)


def _short(text: str, width: int = 120) -> str:
    text = " ".join(text.split())
    return text if len(text) <= width else text[: width - 1] + "…"


def _host_fields(meta: dict[str, Any]) -> dict[str, Any]:
    """Keep only keys HostConditions accepts (bash helpers may add extras)."""
    allowed = {f.name for f in dataclasses.fields(HostConditions)}
    return {k: v for k, v in meta.items() if k in allowed}


def _first_meaningful_line(text: str) -> str:
    for line in text.splitlines():
        stripped = line.strip()
        if stripped and not stripped.startswith(("===", "---", "+++")):
            return _short(stripped)
    return _short(text)


# --------------------------------------------------------------------------- CLI


def _evidence_from_bundle(
    bundle: Path,
    *,
    low_load: Optional[LowLoadControl] = None,
    divergence: Optional[Divergence] = None,
    baseline_s: Optional[float] = None,
) -> Evidence:
    meta = json.loads((bundle / "meta.json").read_text(encoding="utf-8"))
    # host_after is preferred (conditions at completion); host_before is the
    # bash helper's single sample. Tolerate either / neither.
    host_meta = meta.get("host_after") or meta.get("host_before")
    host = HostConditions(**_host_fields(host_meta)) if host_meta else None
    err = out = ""
    err_path = bundle / "stderr"
    if err_path.is_file():
        err = err_path.read_text(encoding="utf-8", errors="replace")
    out_path = bundle / "stdout"
    if out_path.is_file():
        out = out_path.read_text(encoding="utf-8", errors="replace")
    external = meta.get("external_reads") or scan_external_reads(err + "\n" + out)
    # A bash-written bundle may omit shape; recompute it from the raw evidence so
    # attribution.py stays the single source of truth for shape classification.
    shape = meta.get("shape") or classify_shape(
        exit_code=meta.get("exit_code"),
        timed_out=meta.get("timed_out", False),
        text=out + "\n" + err,
    )
    return Evidence(
        shape=shape,
        host=host,
        divergence=divergence,
        low_load=low_load,
        external_reads=external,
        exit_code=meta.get("exit_code"),
        timed_out=meta.get("timed_out", False),
        wall_s=meta.get("wall_s"),
        # Absent in schema_version 1 bundles -- stays None, which classify_kill
        # reports as UNKNOWN rather than guessing.  Old bundles degrade to the
        # previous behaviour instead of getting a fabricated ratio.
        cpu_s=meta.get("cpu_s"),
        oom=bool(meta.get("oom", False)),
        baseline_s=baseline_s,
        note=meta.get("cmd_str", ""),
    )


def _cmd_capture(args: argparse.Namespace) -> int:
    result = capture_run(
        args.command,
        label=args.label,
        timeout_s=args.timeout,
        bundle_root=Path(args.bundle_root) if args.bundle_root else None,
        proc_pattern=args.proc_pattern,
        keep_on_success=args.keep_on_success,
    )
    print(json.dumps(result.as_dict(), sort_keys=True))
    if result.failed:
        print(
            f"FAILED shape={result.shape} exit={result.exit_code} "
            f"wall={result.wall_s}s bundle={result.bundle_dir}",
            file=sys.stderr,
        )
    return 1 if result.failed else 0


def _cmd_attribute(args: argparse.Namespace) -> int:
    bundle = Path(args.bundle)
    low_load: Optional[LowLoadControl] = None
    if args.low_load_control:
        low_load = _run_low_load_control(bundle, args.low_load_control, args.timeout)
    divergence: Optional[Divergence] = None
    if args.log_a and args.log_b:
        divergence = classify_divergence(Path(args.log_a), Path(args.log_b))
    ev = _evidence_from_bundle(bundle, low_load=low_load, divergence=divergence)
    result = attribute(ev)
    if args.json:
        print(json.dumps({"attribution": result.as_dict(), "evidence": ev.as_dict()},
                         sort_keys=True, default=str))
    else:
        print(result.one_line())
        print(f"  next: {result.next_step}")
    return 0


def _run_low_load_control(bundle: Path, runs: int, timeout: Optional[float]) -> LowLoadControl:
    """Re-run the SAME command K times (assumed quiet host) -- the decisive test."""
    meta = json.loads((bundle / "meta.json").read_text(encoding="utf-8"))
    cmd = meta["cmd"]
    failures = 0
    for _ in range(runs):
        res = capture_run(cmd, label="lowload", timeout_s=timeout, bundle_root=None)
        if res.failed:
            failures += 1
    return LowLoadControl(runs=runs, failures=failures)


def _cmd_report(args: argparse.Namespace) -> int:
    root = Path(args.bundle_root)
    bundles = sorted(p.parent for p in root.rglob("meta.json"))
    if not bundles:
        print(f"(no failure bundles under {root})")
        return 0
    tally: dict[str, int] = {v: 0 for v in ALL_VERDICTS}
    print(f"{'VERDICT':22} {'conf':6} {'shape':12} bundle")
    for bundle in bundles:
        ev = _evidence_from_bundle(bundle)
        result = attribute(ev)
        tally[result.verdict] = tally.get(result.verdict, 0) + 1
        print(f"{result.verdict:22} {result.confidence:6} {ev.shape:12} {bundle.name}")
        for reason in result.reasons:
            print(f"    - {reason}")
    print("\nATTRIBUTION SUMMARY: " + ", ".join(
        f"{v}={tally[v]}" for v in ALL_VERDICTS if tally.get(v)
    ))
    return 0


def _cmd_selftest(_args: argparse.Namespace) -> int:
    from tests import test_attribution  # noqa: local import for CLI convenience

    return test_attribution.run_as_selftest()


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    cap = sub.add_parser(
        "capture",
        help="run a command once; preserve full evidence on failure (exit 1 if failed)",
        description="run a command once; preserve full evidence on failure (exit 1 if failed)",
    )
    cap.add_argument("--label", default="run")
    cap.add_argument("--timeout", type=float, default=None)
    cap.add_argument("--bundle-root", default=None, help="dir to write failure bundles")
    cap.add_argument("--proc-pattern", default="hermit",
                     help="comm pattern to count for concurrent-process pressure")
    cap.add_argument("--keep-on-success", action="store_true")
    cap.add_argument("command", nargs=argparse.REMAINDER,
                     help="-- <command...> to run")
    cap.set_defaults(func=_cmd_capture)

    att = sub.add_parser("attribute", help="attribute a preserved failure bundle", description="attribute a preserved failure bundle")
    att.add_argument("bundle", help="a bundle directory containing meta.json")
    att.add_argument("--low-load-control", type=int, default=0, metavar="K",
                     help="re-run the same command K times (decisive quiet-host test)")
    att.add_argument("--timeout", type=float, default=None)
    att.add_argument("--log-a", default=None, help="--verify trace A for log-diff")
    att.add_argument("--log-b", default=None, help="--verify trace B for log-diff")
    att.add_argument("--json", action="store_true")
    att.set_defaults(func=_cmd_attribute)

    rep = sub.add_parser("report", help="attribute every bundle under a root dir", description="attribute every bundle under a root dir")
    rep.add_argument("bundle_root")
    rep.set_defaults(func=_cmd_report)

    st = sub.add_parser("selftest", help="run the classifier decision-table tests", description="run the classifier decision-table tests")
    st.set_defaults(func=_cmd_selftest)
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    # argparse.REMAINDER keeps a leading "--"; drop it.
    if getattr(args, "command", None) and args.command and args.command[0] == "--":
        args.command = args.command[1:]
    return args.func(args)


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    raise SystemExit(main())
