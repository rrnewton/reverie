#!/usr/bin/env python3
"""The REAL green gate for the Reverie auto-safe-bump.

`auto_bump.auto_safe_bump` takes `validate` as a callable so the atomicity
property could be tested without a validator. This module is the production
implementation of that callable, and it exists because getting "is it green"
right is harder than it looks here.

WHY THIS DOES NOT TRUST `validate-run`
--------------------------------------
`ci-hub validate-run` prints a FINISHED state and exits 0 in cases where it ran
NOTHING — a refusal shows up only in the durable log, roughly two seconds in,
with the run never having started. So an exit code from the launcher is a claim
about the launcher, not evidence about the tree.

The authority is the LEDGER, dereferenced at an exact SHA:

    ci-hub validate-status <40-hex> --json

and a green requires ALL of:

    newest_qualifying is not null
    newest_qualifying.commit == the exact SHA we asked about   (identity, not "latest")
    newest_qualifying.failures == 0
    newest_qualifying.executed_tests > 0                       (a run that executed
                                                                nothing is a no-result,
                                                                not a pass)
    verdict == "VALIDATED"                                     (the canonical judgement)
    unresolved_failure_obligations and unverifiable_evidence are empty lists

A qualifying receipt at the exact SHA is necessary but not sufficient. The
canonical verdict can still be FAILED-ON-RECORD or NEEDS-RERUN when an earlier
failure obligation on an ancestor is unresolved, or when a relevant ledger row
names evidence that is readable only on the host that produced it
(https://github.com/rrnewton/dev-hermit/issues/395). The receipt fields alone
cannot see either, so the verdict is required as well.

The `commit` equality check is the load-bearing one. `newest_qualifying` is the
newest qualifying record in the ledger, and asking about a SHA that has no
record can still surface a record for a DIFFERENT commit; accepting that would
green-light a bump on the strength of some other tree's validation.

WHAT A BUMPED WORKING TREE IS, FOR VALIDATION PURPOSES
-------------------------------------------------------
The bump edits manifests in place; it does not make a commit. The ledger is
keyed by commit SHA. So a bumped tree cannot be validated until it IS a commit —
`commit_bumped_tree` makes that commit inside the isolated checkout, and the
resulting SHA is what everything downstream binds to. Validating "the working
tree" without pinning it to a SHA would produce a receipt nothing could later be
matched against.
"""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Callable
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

SHA40_LEN = 40


@dataclass(frozen=True)
class GreenVerdict:
    """Why the gate said what it said. Never a bare bool."""

    sha: str
    green: bool
    reason: str
    executed_tests: int | None = None
    failures: int | None = None
    record_commit: str | None = None

    def line(self) -> str:
        state = "GREEN" if self.green else "NOT-GREEN"
        return (f"{state} sha={self.sha[:12]} executed={self.executed_tests} "
                f"failures={self.failures} :: {self.reason}")


def assess_ledger_verdict(payload: dict, sha: str) -> GreenVerdict:
    """Pure predicate over a `validate-status --json` payload.

    Split out from the subprocess call so the exact-SHA and nonzero-execution
    rules can be tested against real captured payloads without a ledger.
    """
    record = payload.get("newest_qualifying")
    if not record:
        return GreenVerdict(sha, False,
                            "no qualifying receipt in the ledger for this commit "
                            "(absence of evidence is NOT green)")

    record_commit = str(record.get("commit", ""))
    if record_commit != sha:
        return GreenVerdict(
            sha, False,
            f"receipt is for a DIFFERENT commit ({record_commit[:12]}); a green "
            f"must be bound to the exact SHA under test, never to the newest "
            f"record that happens to exist",
            record.get("executed_tests"), record.get("failures"), record_commit)

    failures = record.get("failures")
    executed = record.get("executed_tests")

    if failures is None or int(failures) != 0:
        return GreenVerdict(sha, False, f"receipt records {failures} failure(s)",
                            executed, failures, record_commit)

    if not executed or int(executed) <= 0:
        return GreenVerdict(
            sha, False,
            "receipt executed ZERO tests — `test result: ok` with nothing "
            "executed is a no-result, not a pass",
            executed, failures, record_commit)

    # The receipt checks above keep their specific reasons; the canonical
    # verdict then has the final word, because it also weighs failure
    # obligations and rows whose evidence this host cannot read.
    verdict = payload.get("verdict")
    if verdict != "VALIDATED":
        return GreenVerdict(
            sha, False,
            f"canonical verdict is {verdict!r}, not 'VALIDATED': a qualifying "
            f"receipt at the exact SHA does not outweigh an unresolved failure "
            f"obligation or relevant evidence this host cannot read",
            executed, failures, record_commit)
    for field in ("unresolved_failure_obligations", "unverifiable_evidence"):
        entries = payload.get(field)
        if not isinstance(entries, list):
            return GreenVerdict(
                sha, False,
                f"validate-status payload has no list in {field!r} "
                f"(got {type(entries).__name__}); a green needs it present and empty",
                executed, failures, record_commit)
        if entries:
            return GreenVerdict(
                sha, False,
                f"canonical verdict is VALIDATED but {field} lists "
                f"{len(entries)} entr{'y' if len(entries) == 1 else 'ies'}; "
                f"refusing a contradictory payload",
                executed, failures, record_commit)

    return GreenVerdict(sha, True, "canonical VALIDATED verdict with a qualifying receipt at the exact SHA",
                        executed, failures, record_commit)


def ledger_verdict(
    sha: str,
    *,
    ci_hub: Path,
    runner: Callable[[list[str]], tuple[int, str]] | None = None,
) -> GreenVerdict:
    """Dereference the ledger for `sha` via the canonical `validate-status`."""
    if len(sha) != SHA40_LEN:
        return GreenVerdict(sha, False, "not a 40-hex SHA; refusing to assess a floating ref")
    run = runner or _run
    rc, out = run([str(ci_hub), "validate-status", sha, "--json"])
    try:
        payload = json.loads(out or "{}")
    except json.JSONDecodeError:
        # A non-JSON answer means the query itself failed. Fail closed: an
        # unparseable verdict is not a green one.
        return GreenVerdict(sha, False, f"validate-status returned non-JSON (rc={rc})")
    return assess_ledger_verdict(payload, sha)


def commit_bumped_tree(repo: Path, message: str,
                       runner: Callable[[list[str]], tuple[int, str]] | None = None) -> str:
    """Commit the bumped tree in the ISOLATED checkout and return its SHA.

    Only pin sites are staged, by explicit path: the bump must not sweep up
    whatever else happens to be in the working tree of the checkout it was
    handed.
    """
    run = runner or _run
    rc, _ = run(["git", "-C", str(repo), "commit", "-q", "-m", message, "--",
                 "Cargo.toml", "Cargo.lock", ":(glob)**/Cargo.toml", ":(glob)**/Cargo.lock"])
    if rc != 0:
        raise RuntimeError(f"could not commit the bumped tree (rc={rc})")
    rc, out = run(["git", "-C", str(repo), "rev-parse", "HEAD"])
    return out.strip()


def real_validator(
    repo: Path,
    *,
    ci_hub: Path,
    agent: str = "hermit-w1",
    profile: str = "full",
    launch: bool = True,
    runner: Callable[[list[str]], tuple[int, str]] | None = None,
) -> Callable[[], bool]:
    """Build the `validate` callable `auto_safe_bump` expects.

    Commits the bumped tree, launches validation for that exact SHA through the
    sole admission point (`ci-hub validate-run`), then IGNORES that command's
    exit status and asks the ledger. `launch=False` skips the launch and only
    dereferences an existing receipt, which is the mode a scheduler uses when a
    validation for that SHA has already been produced.
    """
    run = runner or _run
    state: dict[str, object] = {}

    def validate() -> bool:
        # Remember where HEAD was BEFORE we commit, so a refusal can put it back.
        _, pre = run(["git", "-C", str(repo), "rev-parse", "HEAD"])
        pre_head = pre.strip()

        sha = commit_bumped_tree(repo, "reverie pin: auto-safe-bump candidate", runner)
        state["sha"] = sha
        if launch:
            # Launched for its SIDE EFFECT (a durable receipt). Its rc is
            # deliberately unused; see the module docstring.
            run([str(ci_hub), "validate-run", "--checkout", str(repo),
                 "--agent", agent, "--target", sha, "--", profile])
        verdict = ledger_verdict(sha, ci_hub=ci_hub, runner=runner)
        state["verdict"] = verdict

        if not verdict.green:
            # UNDO OUR OWN CANDIDATE COMMIT, and only ever our own.
            #
            # Found by running this for real: auto_bump's rollback restores the
            # FILES, but this gate had already committed them, so a refusal left
            # HEAD on the candidate with the working tree reverted under it --
            # ten dirty paths and a polluted history that the next run would
            # stack another candidate onto.
            #
            # `HEAD~1` is a PROXY for "the commit I just made" and nothing binds
            # the two, so the reset is guarded: it happens only if HEAD is still
            # exactly the SHA we created. If anything else committed in between,
            # the tip is not ours and we leave it alone rather than deleting
            # someone else's work.
            _, now = run(["git", "-C", str(repo), "rev-parse", "HEAD"])
            if now.strip() == sha and pre_head:
                # --mixed, not --soft: a soft reset moves HEAD but leaves the
                # bumped content STAGED, so the index and the restored working
                # tree disagree and the checkout is dirty in both directions.
                # --mixed resets the index too, and auto_bump's file restore
                # then lands on a clean tree. Not --hard: that would discard
                # working-tree state this function does not own.
                run(["git", "-C", str(repo), "reset", "-q", "--mixed", pre_head])
                state["candidate_undone"] = True
            else:
                state["candidate_undone"] = False
        return verdict.green

    validate.state = state  # type: ignore[attr-defined]
    return validate


def _run(cmd: list[str]) -> tuple[int, str]:
    proc = bounded_capture.capture(cmd)
    return proc.returncode, proc.stdout


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description="Query the real green gate for one SHA.")
    ap.add_argument("sha")
    ap.add_argument("--ci-hub",
                    default=str(Path(__file__).resolve().parents[2] / "ci-hub" / "ci-hub"))
    a = ap.parse_args()
    v = ledger_verdict(a.sha, ci_hub=Path(a.ci_hub))
    print(v.line())
    raise SystemExit(0 if v.green else 1)
