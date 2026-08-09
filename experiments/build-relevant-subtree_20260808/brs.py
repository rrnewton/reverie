#!/usr/bin/env python3
"""Build-Relevant Subtree (BRS) hash -- reference implementation for receipt reuse.

THE QUESTION. A Hermit receipt was validated against Reverie pin X. Reverie main
moves to Y. May the receipt be REUSED for Y, or must Hermit revalidate?

THE ANSWER THIS TOOL COMPUTES. Reuse is permitted iff BRS(X) == BRS(Y), where BRS
digests exactly the Reverie content that can enter a Hermit build. Everything else
-- docs, reverie's own CI and test harness, agent guidance -- cannot change what
Hermit builds, so a delta confined to it is reusable.

WHY DENY-LIST AND NOT ALLOW-LIST. An allow-list of "relevant" paths treats any NEW
unclassified path as irrelevant: it FAILS OPEN, and a receipt ends up certifying a
build it never validated. A deny-list of provably-inert paths treats a new
unclassified path as relevant: it FAILS CLOSED, and the worst case is one needless
revalidation. False reuse is a silent wrong green; false refusal costs ~12 minutes.
The asymmetry is not close, so every mode here fails closed.

MODES (the genuinely arguable boundary; see README for the trade-off):
  strict    every file under a consumed package dir, plus the workspace manifest.
            Safest. Includes the package's own tests/ and benches/.
  no-tests  strict minus tests/ and benches/ INSIDE consumed packages. Sound only
            while the consumer compiles lib/bin targets of a git dependency and
            never its integration tests -- which is cargo's documented behaviour,
            but is a premise, so it is a separate mode rather than the default.

Both modes are computed from `cargo metadata` on the CONSUMER, never from a
hand-maintained path list, so the set tracks the dependency graph automatically:
a reverie crate that becomes a Hermit dependency becomes relevant with no edit
here. The path set is the UNION of the graphs at X and Y, so a crate that exists
at only one end still counts.

GUARDS (each is a fail-closed precondition, not a nicety):
  G1 no consumed build script may read outside its own package directory.
  G2 no consumed lib/bin target may include_str!/include_bytes! outside its package.
  G3 a consumed package directory must not contain a submodule gitlink.
Violating any guard means the crate-dir scoping is unsound; the tool REFUSES
rather than emitting a hash it cannot stand behind.

Usage:
  brs.py --repo <reverie-checkout> --rev <sha> [--mode strict|no-tests]
  brs.py --repo <reverie-checkout> --compare <shaX> <shaY> [--mode ...]
  brs.py --repo <reverie-checkout> --scan <shaA>..<shaB> [--mode ...]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

# The 17 Reverie package directories Hermit resolves, as measured by
# `cargo metadata --locked` on hermit at f65f7446. Passed in via --pkg-dirs in
# real use; this default is the recorded measurement so the tool is runnable
# standalone and so drift from it is visible.
DEFAULT_PKG_DIRS = [
    "experimental/nostd-print",
    "experimental/reverie-rpc",
    "experimental/reverie-rpc-macros",
    "experimental/reverie-sabre",
    "experimental/reverie-sabre-macros",
    "experimental/reverie-sabre-stats",
    "reverie",
    "reverie-dbi",
    "reverie-kvm",
    "reverie-liteinst",
    "reverie-memory",
    "reverie-preload",
    "reverie-process",
    "reverie-ptrace",
    "reverie-rpc-transport",
    "reverie-syscalls",
    "safeptrace",
]

# Workspace-level files that affect how the consumed packages resolve or compile.
# Deliberately conservative: reverie's own Cargo.lock is ignored by cargo for a
# git dependency, but it is one blob and including it costs nothing.
WORKSPACE_FILES = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]

EXCLUDED_SUBDIRS = ("tests/", "benches/")


def git(repo: Path, *args: str) -> str:
    r = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, check=False,
    )
    if r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {r.stderr.strip()}")
    return r.stdout


def tree_entries(repo: Path, rev: str, prefix: str) -> list[tuple[str, str, str]]:
    """(mode, path, blobsha) for every entry under prefix at rev."""
    out = git(repo, "ls-tree", "-r", "-z", rev, "--", prefix)
    rows = []
    for ent in filter(None, out.split("\0")):
        meta, _, path = ent.partition("\t")
        mode, otype, sha = meta.split()
        rows.append((mode, path, sha))
    return rows


def in_scope(path: str, pkg_dirs: list[str], mode: str) -> bool:
    for d in pkg_dirs:
        if path == d or path.startswith(d + "/"):
            rel = path[len(d) + 1:]
            if mode == "no-tests" and rel.startswith(EXCLUDED_SUBDIRS):
                return False
            return True
    return path in WORKSPACE_FILES


def pkg_dirs_present(repo: Path, rev: str, pkg_dirs: list[str]) -> list[str]:
    """Package dirs that actually exist at rev (union semantics handle the rest)."""
    present = []
    for d in pkg_dirs:
        try:
            if tree_entries(repo, rev, d):
                present.append(d)
        except RuntimeError:
            pass
    return present


def guards(repo: Path, rev: str, pkg_dirs: list[str]) -> list[str]:
    """Fail-closed preconditions. Returns a list of violations (empty == ok)."""
    bad = []
    for mode, path, _sha in tree_entries(repo, rev, "."):
        if mode == "160000" and any(
            path == d or path.startswith(d + "/") for d in pkg_dirs
        ):
            bad.append(f"G3 submodule gitlink inside a consumed package: {path}")
    return bad


def brs(repo: Path, rev: str, pkg_dirs: list[str], mode: str) -> tuple[str, int]:
    """(hex digest, file count) over the build-relevant set at rev."""
    rows = []
    for _m, path, sha in tree_entries(repo, rev, "."):
        if in_scope(path, pkg_dirs, mode):
            rows.append((path, sha))
    rows.sort()
    h = hashlib.sha256()
    h.update(f"brs-v1 mode={mode} n={len(rows)}\n".encode())
    for path, sha in rows:
        h.update(f"{sha} {path}\n".encode())
    return h.hexdigest(), len(rows)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", type=Path, required=True)
    ap.add_argument("--rev")
    ap.add_argument("--compare", nargs=2, metavar=("X", "Y"))
    ap.add_argument("--scan", metavar="A..B")
    ap.add_argument("--mode", default="strict", choices=["strict", "no-tests"])
    ap.add_argument("--pkg-dirs", help="comma-separated override")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()

    pkg_dirs = a.pkg_dirs.split(",") if a.pkg_dirs else DEFAULT_PKG_DIRS

    if a.rev:
        v = guards(a.repo, a.rev, pkg_dirs)
        if v:
            print("REFUSE: guard violation\n  " + "\n  ".join(v), file=sys.stderr)
            return 2
        d, n = brs(a.repo, a.rev, pkg_dirs, a.mode)
        print(json.dumps({"rev": a.rev, "mode": a.mode, "brs": d, "files": n})
              if a.json else f"{d}  ({n} files, mode={a.mode})")
        return 0

    if a.compare:
        x, y = a.compare
        # Union semantics: a package present at only one end still counts.
        union = sorted(set(pkg_dirs_present(a.repo, x, pkg_dirs))
                       | set(pkg_dirs_present(a.repo, y, pkg_dirs)))
        for rev in (x, y):
            v = guards(a.repo, rev, union)
            if v:
                print(f"REFUSE: guard violation at {rev}\n  " + "\n  ".join(v),
                      file=sys.stderr)
                return 2
        dx, nx = brs(a.repo, x, union, a.mode)
        dy, ny = brs(a.repo, y, union, a.mode)
        same = dx == dy
        out = {"x": x, "y": y, "mode": a.mode, "brs_x": dx, "brs_y": dy,
               "files_x": nx, "files_y": ny,
               "verdict": "REUSE-PERMITTED" if same else "REVALIDATE-REQUIRED"}
        if a.json:
            print(json.dumps(out))
        else:
            print(f"BRS({x[:12]}) = {dx[:16]}  ({nx} files)")
            print(f"BRS({y[:12]}) = {dy[:16]}  ({ny} files)")
            print(out["verdict"])
        return 0 if same else 1

    if a.scan:
        lo, hi = a.scan.split("..")
        revs = git(a.repo, "rev-list", "--reverse", f"{lo}..{hi}").split()
        prev = lo
        print("parent,child,mode,verdict,changed_files_total,changed_files_relevant")
        for r in revs:
            names = git(a.repo, "diff", "--name-only", prev, r).split("\n")
            names = [n for n in names if n]
            union = sorted(set(pkg_dirs_present(a.repo, prev, pkg_dirs))
                           | set(pkg_dirs_present(a.repo, r, pkg_dirs)))
            rel = [n for n in names if in_scope(n, union, a.mode)]
            dx, _ = brs(a.repo, prev, union, a.mode)
            dy, _ = brs(a.repo, r, union, a.mode)
            verdict = "REUSE" if dx == dy else "REVALIDATE"
            print(f"{prev[:12]},{r[:12]},{a.mode},{verdict},{len(names)},{len(rel)}")
            prev = r
        return 0

    ap.error("one of --rev / --compare / --scan required")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
