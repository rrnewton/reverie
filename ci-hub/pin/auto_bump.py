#!/usr/bin/env python3
"""AUTO-SAFE-BUMP for the Reverie pin: automatic AND safe, so a pin is
semantically as good as always-main.

THE MODEL (#281). The answer to pin friction is never to float the pin — a
floating ref buys convenience by giving up the ability to say what was built.
It is to make the BUMP automatic and safe. Both halves are required, and the
earlier batch bump was moot because it had neither.

    AUTOMATIC — already exists: `hermit/scripts/check-reverie-pin.rs
                --update-to-latest` rewrites every derived pin site.
    SAFE      — this module. The update step is not the hard part; not landing
                a half-applied or broken bump is.

WHAT "SAFE" MEANS HERE, CONCRETELY

* **Atomic across every entry.** The hazard is real and was measured: 13 of 21
  entries updated, 8 left behind. A partially-applied bump is worse than no
  bump, because the tree then claims two different Reverie revisions at once and
  every consumer picks whichever it happens to read. So the post-condition is
  checked over the WHOLE derived set, and anything short of all-or-nothing is a
  refusal plus a rollback.
* **Rollback restores bytes, not "the previous value".** Every touched file is
  snapshotted verbatim before the first write and restored verbatim on any
  failure, so a refused bump leaves the tree byte-identical to how it was found.
  Re-running the updater "backwards" would not give that — it would recompute a
  value rather than restore a file.
* **Fail-closed.** Every exit that is not a fully verified success rolls back.
  An unexpected exception is treated as failure, not as "probably fine".
* **No floating refs.** The target must be an explicit 40-hex SHA. A branch name
  or short SHA is refused before anything is touched, because the whole point of
  a pin is that it names one immutable commit.

WHAT THIS MODULE DOES NOT DO. It does not decide the entry set itself. The set
is DERIVED, every run, from the canonical tool's own scope (`git ls-files` over
tracked Cargo.toml/Cargo.lock). A hand-maintained list is exactly the thing that
goes stale and produces the partial application it is supposed to prevent.
"""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

REVERIE_REMOTE = "https://github.com/rrnewton/reverie.git"
MAIN_REF = "refs/heads/main"

#: A pin is one immutable commit. Nothing shorter, nothing symbolic.
SHA40 = re.compile(r"^[0-9a-f]{40}$")

#: The two shapes a Reverie revision takes in Cargo metadata. Manifests carry
#: `rev = "<sha>"`; lockfiles carry it inside the source URL as `?rev=<sha>`.
_MANIFEST_REV = re.compile(r'rev\s*=\s*"([0-9a-f]{40})"')
_LOCK_REV = re.compile(r"\?rev=([0-9a-f]{40})")


def _is_reverie_line(line: str) -> bool:
    """The canonical tool's own per-LINE predicate (check-reverie-pin.rs:206).

    Scoping by file is not enough and getting this wrong is expensive: hermit's
    manifests pin liteinst2 by git rev on neighbouring lines in the SAME files.
    A file-level match would have rewritten the liteinst2 pin to the Reverie tip
    — a silent, plausible-looking corruption of a different dependency. Measured
    on the real checkout: 48 rev entries across 10 files carrying TWO distinct
    revisions, only one of which is Reverie's.
    """
    return "github.com/" in line and "/reverie.git" in line

CANONICAL_TOOL = "scripts/check-reverie-pin.rs"


class BumpRefused(Exception):
    """The bump did not happen and the tree was restored.

    Distinct from an ordinary error: raising this is the SUCCESSFUL operation of
    the safety property, so callers should report it as a refusal rather than a
    crash.
    """


@dataclass
class Entry:
    path: Path
    revs: tuple[str, ...]


@dataclass
class BumpReport:
    target: str
    files_scanned: int
    files_with_revs: int
    entries_before: int
    entries_after: int
    distinct_before: tuple[str, ...]
    changed_files: tuple[str, ...] = ()
    validated: bool = False
    refused_reason: str | None = None
    rolled_back: tuple[str, ...] = field(default_factory=tuple)

    def line(self) -> str:
        head = (f"target={self.target} files={self.files_with_revs}/{self.files_scanned} "
                f"entries={self.entries_before}->{self.entries_after} "
                f"distinct_before={','.join(self.distinct_before) or '-'}")
        if self.refused_reason:
            return f"REFUSED {head} rolled_back={len(self.rolled_back)} :: {self.refused_reason}"
        return f"OK {head} changed={len(self.changed_files)} validated={self.validated}"


# ----------------------------------------------------------------- discovery


def resolve_reverie_tip(runner: Callable[[list[str]], str] | None = None) -> str:
    """The current Reverie main tip, as an explicit 40-hex SHA."""
    run = runner or _run
    out = run(["git", "ls-remote", REVERIE_REMOTE, MAIN_REF])
    sha = out.split()[0] if out.split() else ""
    if not SHA40.match(sha):
        raise BumpRefused(
            f"reverie tip did not resolve to a 40-hex SHA (got {sha!r}); refusing "
            f"to pin anything that is not one immutable commit"
        )
    return sha


def derive_entries(repo: Path, runner: Callable[[list[str]], str] | None = None) -> list[Entry]:
    """Every tracked Cargo.toml/Cargo.lock carrying a Reverie revision.

    Scope is taken from `git ls-files`, matching the canonical tool exactly, and
    re-derived on every run. This is the anti-stale-list property: a file added
    to the repo tomorrow is in scope tomorrow, with nothing to remember to edit.
    """
    run = runner or _run
    listing = run(["git", "-C", str(repo), "ls-files",
                   "Cargo.toml", "Cargo.lock",
                   ":(glob)**/Cargo.toml", ":(glob)**/Cargo.lock"])
    entries: list[Entry] = []
    for rel in sorted({line.strip() for line in listing.splitlines() if line.strip()}):
        path = repo / rel
        try:
            text = path.read_text()
        except (OSError, UnicodeDecodeError):
            continue
        pattern = _LOCK_REV if path.name == "Cargo.lock" else _MANIFEST_REV
        revs = tuple(
            rev
            for line in text.splitlines() if _is_reverie_line(line)
            for rev in pattern.findall(line)
        )
        if revs:
            entries.append(Entry(path=path, revs=revs))
    return entries


def _rev_count(entries: Iterable[Entry]) -> int:
    return sum(len(e.revs) for e in entries)


# --------------------------------------------------------------------- bump


def auto_safe_bump(
    repo: Path,
    *,
    target: str | None = None,
    validate: Callable[[], bool] | None = None,
    apply_fn: Callable[[Path, str, list[Entry]], None] | None = None,
    runner: Callable[[list[str]], str] | None = None,
) -> BumpReport:
    """Bump every Reverie pin entry to `target`, atomically, or refuse.

    `validate` is the green gate: it runs AFTER the bump is applied and BEFORE
    the caller is told it succeeded, because the thing being validated is the
    bumped tree, not the old one. A false return rolls the tree back.

    `apply_fn` is injectable so the atomicity property can be tested against a
    deliberately partial application without needing a way to make the real
    updater fail.
    """
    repo = Path(repo)
    if not (repo / CANONICAL_TOOL).exists():
        raise BumpRefused(
            f"{repo}/{CANONICAL_TOOL} not found — the entry set must be derived "
            f"from the canonical tool's scope, never from a hand-written list"
        )

    target = target or resolve_reverie_tip(runner)
    if not SHA40.match(target or ""):
        raise BumpRefused(
            f"target {target!r} is not a 40-hex SHA. A branch name or short SHA "
            f"is a floating ref, which defeats the purpose of pinning."
        )

    before = derive_entries(repo, runner)
    if not before:
        raise BumpRefused("no Reverie revision entries found; refusing to 'succeed' vacuously")

    distinct_before = tuple(sorted({r for e in before for r in e.revs}))
    report = BumpReport(
        target=target,
        files_scanned=len(before),
        files_with_revs=len(before),
        entries_before=_rev_count(before),
        entries_after=0,
        distinct_before=distinct_before,
    )

    # Snapshot BYTES before the first write. Restoring bytes is what makes a
    # refusal leave no trace; recomputing an "undo" would not.
    snapshot = {e.path: e.path.read_bytes() for e in before}

    def rollback(reason: str) -> BumpReport:
        restored = []
        for path, data in snapshot.items():
            if path.read_bytes() != data:
                path.write_bytes(data)
                restored.append(str(path.relative_to(repo)))
        report.refused_reason = reason
        report.rolled_back = tuple(sorted(restored))
        return report

    try:
        (apply_fn or _apply_bump)(repo, target, before)
    except Exception as exc:  # noqa: BLE001 — any failure is fail-closed
        rollback(f"apply step raised {type(exc).__name__}: {exc}")
        raise BumpRefused(report.refused_reason) from exc

    after = derive_entries(repo, runner)
    report.entries_after = _rev_count(after)

    # ---- the atomicity post-condition, checked over the WHOLE derived set ----
    stragglers = sorted(
        str(e.path.relative_to(repo)) for e in after if any(r != target for r in e.revs)
    )
    if stragglers:
        rollback(
            f"PARTIAL APPLICATION: {len(stragglers)} file(s) still not at {target} "
            f"({', '.join(stragglers[:4])}{'...' if len(stragglers) > 4 else ''}). "
            f"A tree claiming two Reverie revisions at once is worse than an "
            f"un-bumped one, so the whole bump is reverted."
        )
        raise BumpRefused(report.refused_reason)

    if report.entries_after != report.entries_before:
        rollback(
            f"ENTRY COUNT CHANGED {report.entries_before} -> {report.entries_after}; "
            f"the bump must rewrite entries in place, never add or drop one"
        )
        raise BumpRefused(report.refused_reason)

    report.changed_files = tuple(
        sorted(str(p.relative_to(repo)) for p, data in snapshot.items()
               if p.read_bytes() != data)
    )

    if validate is not None:
        try:
            ok = bool(validate())
        except Exception as exc:  # noqa: BLE001
            rollback(f"validate raised {type(exc).__name__}: {exc}")
            raise BumpRefused(report.refused_reason) from exc
        if not ok:
            rollback("validation FAILED on the bumped tree; a bump that cannot "
                     "prove itself green does not land")
            raise BumpRefused(report.refused_reason)
        report.validated = True

    return report


def _apply_bump(repo: Path, target: str, entries: list[Entry]) -> None:
    """Rewrite every derived entry to `target`, in place.

    Deliberately a pure textual substitution of the exact 40-hex tokens found
    during derivation: it cannot touch a line the derivation did not classify as
    an entry, so the applied set and the verified set are the same set.
    """
    for entry in entries:
        lines = entry.path.read_text().splitlines(keepends=True)
        for i, line in enumerate(lines):
            # Rewrite ONLY on lines the derivation classified as Reverie, so a
            # liteinst2 rev sharing a file (or even the same 40-hex value) is
            # never collaterally rewritten.
            if not _is_reverie_line(line):
                continue
            for rev in set(entry.revs):
                if rev != target:
                    lines[i] = lines[i].replace(rev, target)
        entry.path.write_text("".join(lines))


def _run(cmd: list[str]) -> str:
    proc = bounded_capture.capture(cmd)
    if proc.returncode != 0:
        raise BumpRefused(f"{' '.join(cmd[:3])}… failed rc={proc.returncode}: "
                          f"{proc.stderr.strip()[:200]}")
    return proc.stdout


def main(argv: list[str] | None = None) -> int:
    import argparse
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--repo", required=True, help="hermit checkout to bump")
    ap.add_argument("--target", help="explicit 40-hex SHA (default: live reverie main tip)")
    ap.add_argument("--dry-run", action="store_true",
                    help="derive and report the entry set; write nothing")
    args = ap.parse_args(argv)

    repo = Path(args.repo)
    if args.dry_run:
        entries = derive_entries(repo)
        distinct = sorted({r for e in entries for r in e.revs})
        print(f"files_with_revs={len(entries)} entries={_rev_count(entries)} "
              f"distinct={','.join(distinct) or '-'}")
        for e in entries:
            print(f"  {e.path.relative_to(repo)}: {len(e.revs)} entr"
                  f"{'y' if len(e.revs) == 1 else 'ies'}")
        return 0

    try:
        report = auto_safe_bump(repo, target=args.target)
    except BumpRefused as exc:
        print(f"REFUSED: {exc}")
        return 1
    print(report.line())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
