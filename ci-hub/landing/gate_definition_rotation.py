#!/usr/bin/env python3
"""Sanctioned, fail-closed rotation of Hermit's registered merge-gate blob.

The candidate gate cannot authorize its own replacement.  A rotation therefore
requires an owner-authored exact-record, two exact-head review bindings, a
counted local receipt, and the candidate gate's typed definition-only refusal.
The actual merge remains the normal non-admin ``land-pr.sh`` path.  Registration
is deliberately later: only after that lander proves mergeCommit ancestry.

There is intentionally no private mutation flag.  In particular, ``--_inner``
is not an entry point and argparse refuses it. Tests drive this public command
with an inert GitHub/local-repository harness rather than adding a test-only
production mutation path.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any

SHA40 = re.compile(r"^[0-9a-f]{40}$")
MARKER = "GATE-DEFINITION-ROTATION-AUTHORIZED"
REPO = "rrnewton/hermit"
OWNER = "rrnewton"
WORKFLOW_PATH = ".github/workflows/merge-gate.yml"
VARIABLE = "MERGE_GATE_V4_BLOB"
DEFINITION_STEP = "Require the registered v4 gate definition"


@dataclass(frozen=True)
class Verdict:
    eligible: bool
    refusals: tuple[str, ...]


def _sha(value: object) -> bool:
    return isinstance(value, str) and SHA40.fullmatch(value) is not None


def _parse_time(value: object) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed.astimezone(timezone.utc)


def assess(snapshot: dict[str, Any], *, now: datetime | None = None) -> Verdict:
    """Apply all nine independent refusal classes."""
    now = now or datetime.now(timezone.utc)
    auth = snapshot.get("authorization")
    observed = snapshot.get("observed")
    refusals: list[str] = []
    expires = _parse_time(auth.get("expires_at")) if isinstance(auth, dict) else None
    if (
        not isinstance(auth, dict)
        or auth.get("marker") != MARKER
        or auth.get("authorized_by") != OWNER
        or not isinstance(auth.get("reason"), str)
        or not auth.get("reason", "").strip()
        or expires is None
        or expires <= now
    ):
        refusals.append("owner authorization is absent, malformed, unsigned, or expired")
        auth = auth if isinstance(auth, dict) else {}
    observed = observed if isinstance(observed, dict) else {}
    if (
        auth.get("repo") != REPO
        or observed.get("repo") != REPO
        or auth.get("pr") != observed.get("pr")
    ):
        refusals.append("repository or PR identity does not match the authorization")
    post_merge_base = (
        observed.get("post_merge") is True
        and observed.get("remote_main_descends_base") is True
        and observed.get("head_descends_base") is True
    )
    pre_merge_base = (
        observed.get("post_merge") is not True
        and observed.get("remote_main") == auth.get("base")
        and observed.get("pr_base") == auth.get("base")
        and observed.get("head_descends_base") is True
    )
    if not _sha(auth.get("base")) or not (pre_merge_base or post_merge_base):
        refusals.append("main moved, the PR base differs, or the head is not fast-forwardable")
    if (
        not _sha(auth.get("head"))
        or observed.get("pr_head") != auth.get("head")
        or observed.get("expected_head") != auth.get("head")
    ):
        refusals.append("PR head does not match the authorized exact head")
    if (
        not _sha(auth.get("old_blob"))
        or not _sha(auth.get("new_blob"))
        or observed.get("old_blob") != auth.get("old_blob")
        or observed.get("new_blob") != auth.get("new_blob")
    ):
        refusals.append("old or new merge-gate workflow blob does not match")
    allowed = auth.get("allowed_paths")
    changed = observed.get("changed_paths")
    if (
        not isinstance(allowed, list)
        or not allowed
        or any(not isinstance(path, str) or not path for path in allowed)
        or not isinstance(changed, list)
        or sorted(set(allowed)) != sorted(set(changed))
    ):
        refusals.append("changed paths differ from the exact authorized path set")
    approvals = observed.get("approvals")
    head = auth.get("head")
    if (
        not isinstance(approvals, dict)
        or approvals.get("claude") != head
        or approvals.get("codex") != head
        or observed.get("malformed_review_binding") is True
        or observed.get("request_changes_at_head") is True
    ):
        refusals.append(
            "dual exact-head approvals are missing/stale/malformed or a current rejection exists"
        )
    if observed.get("local_ledger_qualified") is not True:
        refusals.append("no qualifying counted full local ledger PASS exists at the exact head")
    if observed.get("definition_only_self_refusal") is not True:
        refusals.append("candidate gate did not fail solely at the registered-definition check")
    return Verdict(not refusals, tuple(refusals))


def _run(args: list[str], *, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, text=True, capture_output=True, timeout=300)


def _gh_json(args: list[str]) -> Any:
    proc = _run(["with-proxy", "gh", "api", *args])
    if proc.returncode != 0:
        raise RuntimeError(f"gh api {' '.join(args)} failed: {proc.stderr.strip()[:400]}")
    return json.loads(proc.stdout or "null")


def _gh_pages(endpoint: str) -> list[dict[str, Any]]:
    pages = _gh_json([endpoint, "--paginate", "--slurp"])
    if not isinstance(pages, list) or any(not isinstance(page, list) for page in pages):
        raise RuntimeError(f"paginated GitHub response for {endpoint} is malformed")
    return [item for page in pages for item in page if isinstance(item, dict)]


def _git(checkout: Path, *args: str, proxy: bool = False) -> str:
    command = ["git", "-C", str(checkout), *args]
    if proxy:
        command.insert(0, "with-proxy")
    proc = _run(command)
    if proc.returncode != 0:
        raise RuntimeError(f"{' '.join(command)} failed: {proc.stderr.strip()[:400]}")
    return proc.stdout.strip()


def _latest_owner_record(comments: list[dict[str, Any]]) -> tuple[dict[str, Any] | None, bool]:
    candidates = [comment for comment in comments if MARKER in str(comment.get("body") or "")]
    if not candidates:
        return None, False
    comment = candidates[-1]
    lines = str(comment.get("body") or "").splitlines()
    try:
        index = next(i for i, line in enumerate(lines) if line.strip() == MARKER)
        payload = json.loads("\n".join(lines[index + 1 :]).strip())
    except (StopIteration, json.JSONDecodeError):
        return None, True
    if not isinstance(payload, dict):
        return None, True
    owner_authored = (
        comment.get("author_association") == "OWNER"
        and (comment.get("user") or {}).get("login") == OWNER
        and payload.get("authorized_by") == OWNER
    )
    payload["marker"] = MARKER
    return payload, not owner_authored


def _review_state(
    comments: list[dict[str, Any]], head: str
) -> tuple[dict[str, str | None], bool, bool]:
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "health"))
    import approval_binding  # type: ignore

    malformed: list[tuple[str, str]] = []
    approvals: dict[str, str | None] = {}
    for lane in ("claude", "codex"):
        shas = approval_binding.lane_shas(comments, lane, malformed)
        approvals[lane] = shas[-1] if shas else None
    reject = re.compile(r"^REQUEST\s+CHANGES\s+AT\s+([0-9a-f]{40})$", re.IGNORECASE)
    rejected = any(
        (match := reject.match(line.strip(" *_`"))) is not None
        and match.group(1).lower() == head
        for comment in comments
        for line in str(comment.get("body") or "").splitlines()
    )
    return approvals, bool(malformed), rejected


def _local_ledger_qualified(head: str) -> bool:
    root = Path(__file__).resolve().parents[2]
    proc = _run(
        [str(root / "ci-hub/ci-hub"), "validate-status", "--repo", REPO, "--sha", head, "--json"]
    )
    if proc.returncode != 0:
        return False
    try:
        report = json.loads(proc.stdout)
        row = report["newest_qualifying"]
        coverage = row["coverage"]
        return bool(
            report.get("repo") == REPO
            and report.get("sha") == head
            and report.get("verdict") == "VALIDATED"
            and report.get("qualifying_count", 0) > 0
            and row.get("sha") == head
            and row.get("profile") == "full"
            and row.get("selection_mode") == "full"
            and row.get("coverage_satisfied") is True
            and row.get("failures") == 0
            and row.get("executed_tests", 0) > 0
            and coverage.get("planned_test_nodes", 0) > 0
            and coverage.get("executed_test_nodes") == coverage.get("planned_test_nodes")
            and coverage.get("absent_nodes") == []
            and coverage.get("zero_executed_nodes") == []
        )
    except (KeyError, TypeError, json.JSONDecodeError):
        return False


def _definition_only_self_refusal(head: str) -> bool:
    checks = _gh_json([f"repos/{REPO}/commits/{head}/check-runs?per_page=100"])
    matching = [
        check
        for check in checks.get("check_runs", [])
        if check.get("name") == "merge-gate-v4"
        and check.get("head_sha") == head
        and check.get("status") == "completed"
    ]
    if not matching:
        return False
    check = sorted(matching, key=lambda item: item.get("completed_at") or "")[-1]
    if check.get("conclusion") != "failure":
        return False
    match = re.search(r"/job/(\d+)(?:\?|$)", str(check.get("details_url") or ""))
    if not match:
        return False
    job = _gh_json([f"repos/{REPO}/actions/jobs/{match.group(1)}"])
    steps = job.get("steps")
    if not isinstance(steps, list) or not steps:
        return False
    failed = [step.get("name") for step in steps if step.get("conclusion") == "failure"]
    return failed == [DEFINITION_STEP]


def collect_live(pr: int, checkout: Path, expected_head: str | None = None) -> dict[str, Any]:
    pull = _gh_json([f"repos/{REPO}/pulls/{pr}"])
    comments = _gh_pages(f"repos/{REPO}/issues/{pr}/comments?per_page=100")
    reviews = _gh_pages(f"repos/{REPO}/pulls/{pr}/reviews?per_page=100")
    auth, malformed_owner = _latest_owner_record(comments)
    auth = auth or {}
    head = str((pull.get("head") or {}).get("sha") or "")
    base = str((pull.get("base") or {}).get("sha") or "")
    approvals, malformed_review, rejected = _review_state(comments, head)
    rejected = rejected or any(
        review.get("state") == "CHANGES_REQUESTED" and review.get("commit_id") == head
        for review in reviews
    )
    remote = _git(checkout, "ls-remote", "origin", "refs/heads/main", proxy=True)
    remote_main = remote.split()[0] if remote.split() else ""
    old_blob = _git(checkout, "rev-parse", f"{base}:{WORKFLOW_PATH}")
    new_blob = _git(checkout, "rev-parse", f"{head}:{WORKFLOW_PATH}")
    changed_paths = _git(checkout, "diff", "--name-only", base, head).splitlines()
    descends = _run(["git", "-C", str(checkout), "merge-base", "--is-ancestor", base, head]).returncode == 0
    return {
        "schema_version": 1,
        "authorization": auth,
        "observed": {
            "repo": REPO,
            "pr": pr,
            "remote_main": remote_main,
            "pr_base": base,
            "pr_head": head,
            "expected_head": expected_head or head,
            "head_descends_base": descends,
            "old_blob": old_blob,
            "new_blob": new_blob,
            "changed_paths": changed_paths,
            "approvals": approvals,
            "malformed_review_binding": malformed_owner or malformed_review,
            "request_changes_at_head": rejected,
            "local_ledger_qualified": _local_ledger_qualified(head),
            "definition_only_self_refusal": _definition_only_self_refusal(head),
        },
    }


def collect_registration_recovery(pr: int, checkout: Path) -> dict[str, Any]:
    """Re-derive immutable authority after merge without pretending main stayed put."""
    pull = _gh_json([f"repos/{REPO}/pulls/{pr}"])
    comments = _gh_pages(f"repos/{REPO}/issues/{pr}/comments?per_page=100")
    reviews = _gh_pages(f"repos/{REPO}/pulls/{pr}/reviews?per_page=100")
    auth, malformed_owner = _latest_owner_record(comments)
    auth = auth or {}
    head, base = str((pull.get("head") or {}).get("sha") or ""), str(auth.get("base") or "")
    approvals, malformed_review, rejected = _review_state(comments, head)
    rejected = rejected or any(
        review.get("state") == "CHANGES_REQUESTED" and review.get("commit_id") == head
        for review in reviews
    )
    _git(checkout, "fetch", "-q", "origin", "main", proxy=True)
    old_blob = _git(checkout, "rev-parse", f"{base}:{WORKFLOW_PATH}")
    new_blob = _git(checkout, "rev-parse", f"{head}:{WORKFLOW_PATH}")
    changed_paths = _git(checkout, "diff", "--name-only", base, head).splitlines()
    head_descends = _run(
        ["git", "-C", str(checkout), "merge-base", "--is-ancestor", base, head]
    ).returncode == 0
    main_descends = _run(
        ["git", "-C", str(checkout), "merge-base", "--is-ancestor", base, "origin/main"]
    ).returncode == 0
    return {
        "schema_version": 1,
        "authorization": auth,
        "observed": {
            "repo": REPO,
            "pr": pr,
            "post_merge": True,
            "remote_main_descends_base": main_descends,
            "pr_head": head,
            "expected_head": head,
            "head_descends_base": head_descends,
            "old_blob": old_blob,
            "new_blob": new_blob,
            "changed_paths": changed_paths,
            "approvals": approvals,
            "malformed_review_binding": malformed_owner or malformed_review,
            "request_changes_at_head": rejected,
            "local_ledger_qualified": _local_ledger_qualified(head),
            "definition_only_self_refusal": _definition_only_self_refusal(head),
        },
    }


def _print_verdict(verdict: Verdict, head: str | None = None) -> None:
    if verdict.eligible:
        print(f"SELF_AUTHORIZATION_REFUSED: definition-only gate refusal verified at {head or 'unknown'}")
        print("GATE-DEFINITION-ROTATION: AUTHORIZED (9/9 independent refusal classes clear)")
        return
    print("SELF_AUTHORIZATION_REFUSED: no independent exact-record authority")
    print(f"GATE-DEFINITION-ROTATION: REFUSED ({len(verdict.refusals)} finding(s))")
    for reason in verdict.refusals:
        print(f"  - {reason}")


def assert_land_lock_descendant(pr: int, agent: str) -> None:
    """Prove this caller descends from the exact live land-lock supervisor.

    A hidden flag is not a capability.  This check instead binds the rotation
    lander's inner process to the kernel identity recorded by ``land-lock run``:
    same boot, exact PID/start-ticks, matching holder PR/agent, and ancestry.
    Caller-controlled lock-path overrides are deliberately ignored.
    """
    root = Path(__file__).resolve().parents[2]

    def fields(path: Path) -> dict[str, str]:
        parsed: dict[str, str] = {}
        for line in path.read_text().splitlines():
            if "=" not in line:
                raise RuntimeError(f"malformed lock identity line in {path}")
            key, value = line.split("=", 1)
            if key in parsed:
                raise RuntimeError(f"duplicate lock identity field {key}")
            parsed[key] = value
        return parsed

    holder = fields(root / ".landing-lock")
    owner = fields(root / ".landing-lock.owner")
    boot_id = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    if holder.get("agent") != agent or holder.get("pr") != str(pr):
        raise RuntimeError("live land-lock holder does not match rotation agent/PR")
    if owner.get("boot_id") != boot_id:
        raise RuntimeError("land-lock owner identity belongs to another boot")
    try:
        owner_pid = int(owner["pid"])
        owner_ticks = int(owner["start_ticks"])
    except (KeyError, ValueError) as error:
        raise RuntimeError("land-lock owner identity is incomplete") from error

    pid = os.getppid()
    seen: set[int] = set()
    while pid > 1 and pid not in seen:
        seen.add(pid)
        stat = Path(f"/proc/{pid}/stat").read_text()
        tail = stat[stat.rfind(")") + 2 :].split()
        if len(tail) < 20:
            raise RuntimeError(f"could not parse /proc/{pid}/stat")
        parent, ticks = int(tail[1]), int(tail[19])
        if pid == owner_pid:
            if ticks != owner_ticks:
                raise RuntimeError("land-lock owner PID was recycled")
            print(f"LAND-LOCK-DESCENDANT: pr={pr} agent={agent} owner={pid}@{ticks}")
            return
        pid = parent
    raise RuntimeError("rotation inner process is not a descendant of land-lock run")


class LiveRuntime:
    def __init__(self, pr: int, branch: str, checkout: Path) -> None:
        self.pr, self.branch, self.checkout = pr, branch, checkout
        self.root = Path(__file__).resolve().parents[2]

    def event(self, value: str) -> None:
        print(f"GATE-ROTATION-EVENT {value}")

    def land(self, _snapshot: dict[str, Any]) -> None:
        proc = subprocess.run(
            [
                str(self.root / "ci-hub/landing/land-pr.sh"),
                str(self.pr),
                self.branch,
                "--gate-rotation",
                "--no-rebase",
                "--foreground",
                "--agent",
                "gate-definition-rotation",
            ]
        )
        if proc.returncode != 0:
            raise RuntimeError(f"normal non-admin lander refused (rc={proc.returncode})")

    def register_after_ancestry(self, snapshot: dict[str, Any]) -> None:
        auth = snapshot["authorization"]
        # Re-read the owner record after landing; it must still name the same
        # immutable identities.  Main may have advanced, so the pre-merge base
        # equality is replaced here by ancestry + current workflow-blob checks.
        pull = _gh_json([f"repos/{REPO}/pulls/{self.pr}"])
        comments = _gh_pages(f"repos/{REPO}/issues/{self.pr}/comments?per_page=100")
        current_auth, malformed = _latest_owner_record(comments)
        if malformed or current_auth != auth:
            raise RuntimeError("owner rotation record changed after merge; registration refused")
        if not pull.get("merged_at") or (pull.get("head") or {}).get("sha") != auth["head"]:
            raise RuntimeError("authorized PR/head is not merged; registration refused")
        merge_commit = str(pull.get("merge_commit_sha") or "")
        if not _sha(merge_commit):
            raise RuntimeError("merged PR has no exact mergeCommit identity")
        self.event("refresh_main")
        _git(self.checkout, "fetch", "-q", "origin", "main", proxy=True)
        if _run(
            ["git", "-C", str(self.checkout), "merge-base", "--is-ancestor", merge_commit, "origin/main"]
        ).returncode != 0:
            raise RuntimeError("mergeCommit is not an ancestor of current main; registration refused")
        self.event("prove_ancestry")
        main_blob = _git(self.checkout, "rev-parse", f"origin/main:{WORKFLOW_PATH}")
        if main_blob != auth["new_blob"]:
            raise RuntimeError("current main does not contain the authorized gate blob")
        self.event("read_main_gate_blob")
        _gh_json(
            [
                "--method",
                "PATCH",
                f"repos/{REPO}/actions/variables/{VARIABLE}",
                "-f",
                f"name={VARIABLE}",
                "-f",
                f"value={auth['new_blob']}",
            ]
        )
        self.event("patch_registration")
        variable = _gh_json([f"repos/{REPO}/actions/variables/{VARIABLE}"])
        main_file = _gh_json([f"repos/{REPO}/contents/{WORKFLOW_PATH}?ref=main"])
        if variable.get("value") != auth["new_blob"] or main_file.get("sha") != auth["new_blob"]:
            raise RuntimeError("post-ancestry gate registration did not verify")
        self.event("verify_registration")


def execute(snapshot: dict[str, Any], runtime: LiveRuntime) -> None:
    runtime.event("collect_outer")
    verdict = assess(snapshot)
    _print_verdict(verdict, str(snapshot.get("authorization", {}).get("head") or ""))
    if not verdict.eligible:
        raise RuntimeError("rotation is not independently authorized")
    runtime.event("verify_outer")
    runtime.land(snapshot)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pr", type=int)
    parser.add_argument("--branch")
    parser.add_argument("--checkout", type=Path)
    parser.add_argument("--expected-head")
    parser.add_argument("--agent", default="gate-definition-rotation")
    parser.add_argument("--verify-live", action="store_true")
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--register-only", action="store_true")
    parser.add_argument("--assert-land-lock", action="store_true")
    args = parser.parse_args()
    actions = sum((args.verify_live, args.execute, args.register_only, args.assert_land_lock))
    if actions != 1:
        parser.error(
            "choose exactly one of --verify-live, --execute, --register-only, or --assert-land-lock"
        )
    try:
        if args.assert_land_lock:
            if not args.pr or any((args.checkout, args.branch)):
                parser.error("--assert-land-lock requires only --pr and optional --agent")
            assert_land_lock_descendant(args.pr, args.agent)
            return 0
        if not args.pr or not args.checkout:
            parser.error("live mode requires --pr and --checkout")
        checkout = args.checkout.resolve()
        snapshot = (
            collect_registration_recovery(args.pr, checkout)
            if args.register_only
            else collect_live(args.pr, checkout, args.expected_head)
        )
        if args.verify_live:
            verdict = assess(snapshot)
            _print_verdict(verdict, str(snapshot.get("authorization", {}).get("head") or ""))
            if not verdict.eligible:
                return 2
            return 0
        if args.execute and not args.branch:
            parser.error("--execute requires --branch")
        runtime = LiveRuntime(args.pr, args.branch or "post-merge-registration", checkout)
        if args.execute:
            execute(snapshot, runtime)
        else:
            verdict = assess(snapshot)
            _print_verdict(verdict, str(snapshot.get("authorization", {}).get("head") or ""))
            if not verdict.eligible:
                return 2
            runtime.register_after_ancestry(snapshot)
        print(
            f"GATE-DEFINITION-ROTATION: REGISTERED-AFTER-ANCESTRY "
            f"head={snapshot['authorization']['head']} blob={snapshot['authorization']['new_blob']}"
        )
        return 0
    except (OSError, RuntimeError, KeyError, json.JSONDecodeError) as error:
        print(f"GATE-DEFINITION-ROTATION: REFUSED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
