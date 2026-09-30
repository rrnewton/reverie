#!/usr/bin/env python3
"""Producers for the two absent comparisons: EXIT CODE and INFO LOG.

Of the seven named comparison fields, stdout is the only real producer,
`bitwise_parity` is a hardcoded blank, and five are absent. This supplies two of
those five. detlog/stack/heap are tracked separately.

MODELLED ON `capture_parity` (collect-envelope.rs:744), deliberately, because that
one demonstrably works: it computes SHA-256 digest equality against the ptrace
reference, and the shipped population contains **96 FALSE values in
scorecard.csv** (348 across all four scorecards) — proof it fires rather than
always passing. The two producers here keep its shape:

  * the REFERENCE is ptrace, run identically to the candidate;
  * the comparison is digest equality, not a hand-rolled predicate;
  * a side that cannot be observed yields **None** (unknown), never False —
    an unmeasured cell must not read as a confirmed mismatch.

WHY THESE TWO DID NOT EXIST. Both are discarded upstream rather than missing:

  * EXIT CODE — `run_and_hash` (collect-envelope.rs:1106) does
    `if !out.status.success() { return None }`. The exit status is right there
    and is thrown away, so a cell that exits 1 under one backend and 0 under
    another is indistinguishable from a cell that could not be measured.
  * INFO LOG — `run_and_hash` never passes `--log=info --log-file`, so no INFO
    log is produced to compare. INFO log is a NAMED COMPONENT of the strict
    standard (stdout + INFO log + stack + heap), so its absence is what makes a
    strict claim dishonest.

NORMALISATION, stated because a digest is only as honest as what it hashes. The
INFO log digest strips exactly one thing: the leading wall-clock timestamp,
which is genuinely irreproducible. Addresses are NOT normalised — an
allocation-order change is a real divergence and hiding it is the fake-green
move. This is the same rule the prefix-parity work uses, so the two agree.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile

TS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s+")
REFERENCE_BACKEND = "ptrace"


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def normalise_info_log(text: str) -> str:
    """Strip only the wall-clock prefix. Keep addresses: they are real signal."""
    return "\n".join(TS.sub("", line) for line in text.splitlines())


def run_cell(hermit: str, guest: list[str], backend: str, lane: str = "portable",
             timeout_s: int = 120, scratch: str | None = None) -> dict:
    """One observation of one cell. Mirrors run_and_hash's PINNED guest env.

    Returns exit_code and the INFO-log digest. Unlike run_and_hash this does NOT
    discard a failing run -- the exit code IS the observable here.
    """
    # NOT /tmp. Measured in this sandbox: hermit PANICS with
    # "Failed to open log file: NotFound" for a --log-file under a /tmp tempdir,
    # exits 1, and writes nothing. The first version of this producer therefore
    # reported exit_code_parity=True on exits "1v1" -- a green built on BOTH
    # SIDES FAILING IDENTICALLY, with info_lines 0/0 as the only tell. A producer
    # that cannot distinguish "the guest exited 1" from "hermit never started" is
    # the exact defect this file exists to close, so the log lands in a
    # caller-supplied workspace directory and an empty log is a REFUSAL below.
    base = scratch or os.path.join(os.getcwd(), "scratch", "cei-runs")
    os.makedirs(base, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="cei-", dir=base) as tmp:
        logp = os.path.join(tmp, "info.log")
        cmd = ["timeout", f"{timeout_s}s", hermit,
               "--log=info", f"--log-file={logp}",
               "--backend", backend, "run", "--strict",
               "--base-env", "minimal", "-e", "LC_ALL=C", "-e", "TZ=UTC"]
        if lane == "portable":
            cmd += ["--no-virtualize-cpuid", "--max-timeslice=disabled"]
        cmd += ["--"] + guest
        env = dict(os.environ, LC_ALL="C", TZ="UTC")
        try:
            out = subprocess.run(cmd, capture_output=True, env=env, timeout=timeout_s + 30)
        except subprocess.TimeoutExpired:
            return {"exit_code": None, "info_digest": None, "reason": "timeout"}
        text = ""
        try:
            with open(logp, errors="replace") as fh:
                text = fh.read()
        except OSError:
            pass
        text += out.stderr.decode("utf8", "replace")
        info = [l for l in text.splitlines() if "DETLOG" in l or " INFO " in l]
        digest = _digest(normalise_info_log("\n".join(info)).encode()) if info else None
        # FAIL CLOSED on a harness failure. hermit panicking is not a guest exit
        # code; emitting it as one manufactures agreement between two broken runs.
        err = out.stderr.decode("utf8", "replace")
        if "Failed to open log file" in err or "panicked at" in err:
            return {"exit_code": None, "info_digest": None, "info_lines": 0,
                    "reason": "hermit did not start (panic); not a guest exit code"}
        return {"exit_code": out.returncode, "info_digest": digest,
                "info_lines": len(info), "reason": ""}


def capture_exit_code_parity(hermit: str, guest: list[str], backend: str,
                             lane: str = "portable") -> tuple:
    """(parity, candidate_exit, reference_exit). None when either side is unobserved."""
    ref = run_cell(hermit, guest, REFERENCE_BACKEND, lane)
    cand = run_cell(hermit, guest, backend, lane)
    r, c = ref["exit_code"], cand["exit_code"]
    parity = None if (r is None or c is None) else (r == c)
    return parity, c, r


def capture_info_log_parity(hermit: str, guest: list[str], backend: str,
                            lane: str = "portable") -> tuple:
    """(parity, candidate_digest, reference_digest). Digest equality, like capture_parity."""
    ref = run_cell(hermit, guest, REFERENCE_BACKEND, lane)
    cand = run_cell(hermit, guest, backend, lane)
    r, c = ref["info_digest"], cand["info_digest"]
    parity = None if (r is None or c is None) else (r == c)
    return parity, c, r


def capture_both(hermit: str, guest: list[str], backend: str, lane: str = "portable") -> dict:
    """Both comparisons from ONE pair of runs -- never two invocations that could
    have behaved differently (the same rule run_and_hash applies to its banner)."""
    ref = run_cell(hermit, guest, REFERENCE_BACKEND, lane)
    cand = run_cell(hermit, guest, backend, lane)
    def par(a, b):
        return None if (a is None or b is None) else (a == b)
    return {
        "backend": backend,
        "exit_code_parity": par(ref["exit_code"], cand["exit_code"]),
        "exit_code": cand["exit_code"], "ref_exit_code": ref["exit_code"],
        "info_log_parity": par(ref["info_digest"], cand["info_digest"]),
        "info_log_digest": cand["info_digest"], "ref_info_log_digest": ref["info_digest"],
        "info_lines": cand.get("info_lines"), "ref_info_lines": ref.get("info_lines"),
    }


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--hermit", required=True)
    ap.add_argument("--backend", required=True)
    ap.add_argument("--lane", default="portable")
    ap.add_argument("guest", nargs="+")
    a = ap.parse_args(argv)
    print(json.dumps(capture_both(a.hermit, a.guest, a.backend, a.lane), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
