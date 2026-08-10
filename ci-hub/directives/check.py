#!/usr/bin/env python3
"""Verify owner tooling directives against TaskGraph and fresh target ancestry."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from collections import Counter, defaultdict
from dataclasses import asdict, dataclass, replace
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Callable, Mapping, Sequence


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_LEDGER = Path(__file__).resolve().with_name("ledger.json")
DEFAULT_REPORT = ROOT / "ignored/ci-hub/directives/latest.json"
TASK_RE = re.compile(r"[A-Za-z0-9_.-]+")
# Marks a status that came from an ARCHIVED database rather than the live one, so
# "resolved" never silently means "resolved somewhere we no longer operate".
ARCHIVED_SUFFIX = "@archived"
SHA_RE = re.compile(r"[0-9a-f]{40}")
REPO_RE = re.compile(r"[^/\s]+/[^/\s]+")
# States that are genuine drift — an unmet obligation nobody is demonstrably
# advancing. `open` (owned, tasked, in progress) and `gated` (deferred on a
# named blocking condition) are NOT drift: surfacing them as drift cries wolf
# and gets the whole signal discounted.
DRIFT_STATES = (
    "needs_owner",
    "missing_task",
    # Named a task that exists in NO known task database. Distinct from
    # `missing_task` (named none at all): same symptom, different remedy --
    # `missing_task` needs a task filed, `task_not_found` needs the pointer or
    # the database set corrected. Collapsing the two is how a database problem
    # disguises itself as a bookkeeping problem.
    "task_not_found",
    # A row that is not satisfied while its accountable task is CLOSED. This IS drift by the
    # definition above -- nobody is demonstrably advancing it, because the record that would
    # have surfaced it says the work is finished.
    "unaccountable",
    "not_landed",
    "invalid",
)
# `unverifiable` and `fetch_failed` are deliberately NOT drift. They must never
# read as green -- they do not: `_evaluate` maps either to overall `unknown`
# with exit 2 -- but "the checker could not look" and "the work regressed" need
# OPPOSITE responses (retry or raise the deadline, versus re-land), so they must
# not share a bucket. Measured at d6450b82 with `--deadline-secs 1`: 19
# satisfied, landed directives rendered as `drift=19`, and three such rows were
# relayed to the owner as real drift on 2026-08-09. The count scaled with the
# deadline (1s -> 19, the shipped 24s default -> 4, 300s -> 0), which is the tell
# that it measured how far the run got rather than the state of the work.
# `_print_fields` reports them by name in the summary so removing them from
# `drift` cannot turn a false alarm into a silent pass.
NO_RESULT_STATES = (
    "unverifiable",
    "fetch_failed",
)
# A fetch/network failure while verifying a claim is NOT the same as a claim
# that was never made (`not_checked`/`open`) nor a genuinely absent commit
# (`not_landed`): the checker itself could not reach the evidence. Conflating
# the two lets a broken checker read as a clean "nothing to see here", which is
# worse than an error, so it gets its own `fetch_failed` state (never green).
FETCH_FAILURE_MARKERS = (
    "not our ref",
    "upload-pack",
    "could not resolve host",
    "unable to access",
    "could not read from remote",
    "fatal: remote error",
    "connection timed out",
    "connection refused",
    "operation timed out",
    "rpc failed",
    "early eof",
    "proxy",
)


def _looks_like_fetch_failure(*texts: str | None) -> bool:
    blob = " ".join(text for text in texts if text).lower()
    if any(marker in blob for marker in FETCH_FAILURE_MARKERS):
        return True
    # A failing fetch subcommand reported by the verifier, e.g.
    # "command failed: with-proxy git -C <checkout> fetch ... origin <sha>".
    return "fetch" in blob and ("command failed" in blob or "fatal" in blob)
Run = Callable[..., subprocess.CompletedProcess[str]]


class LedgerError(RuntimeError):
    """The versioned directive ledger is structurally invalid."""


@dataclass(frozen=True)
class Implementation:
    kind: str
    identity: str


@dataclass(frozen=True)
class Directive:
    id: str
    summary: str
    requested_at: str
    repository: str
    checkout: str
    target: str
    task: str
    owner: str
    source_row: str | None
    parent_id: str | None
    implementation: Implementation | None
    gate: str | None


@dataclass(frozen=True)
class DirectiveResult:
    id: str
    summary: str
    requested_at: str
    repository: str
    target: str
    task: str
    owner: str
    source_row: str | None
    parent_id: str | None
    implementation_kind: str | None
    implementation_identity: str | None
    gate: str | None
    state: str
    landing_state: str
    ancestry: str
    resolved_sha: str | None
    target_tip: str | None
    issues: tuple[str, ...]
    reason: str


@dataclass(frozen=True)
class Report:
    schema_version: int
    checked_at: str
    ledger: str
    source_rows: int
    records: int
    overall_state: str
    exit_code: int
    counts: dict[str, int]
    issue_counts: dict[str, int]
    directives: tuple[DirectiveResult, ...]

    def to_dict(self) -> dict[str, object]:
        payload = asdict(self)
        payload["directives"] = [asdict(item) for item in self.directives]
        return payload


def _run(
    command: Sequence[str],
    *,
    cwd: Path = ROOT,
    timeout: float = 24,
    env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(command),
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=None if env is None else dict(env),
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return subprocess.CompletedProcess(list(command), 2, "", str(error))


def _text(value: object) -> str:
    return value.strip() if isinstance(value, str) else ""


def _parse_directive(payload: Mapping[str, object]) -> Directive:
    raw_implementation = payload.get("implementation")
    implementation = None
    if isinstance(raw_implementation, Mapping):
        implementation = Implementation(
            kind=_text(raw_implementation.get("kind")),
            identity=_text(raw_implementation.get("identity")).lower(),
        )
    elif raw_implementation is not None:
        raise LedgerError("implementation must be an object or null")
    raw_gate = payload.get("gate")
    if raw_gate is None:
        gate = None
    elif isinstance(raw_gate, str):
        # Preserve "" (present but unnamed) distinct from absent (None) so the
        # metadata check can reject a gate that names no blocking condition.
        gate = raw_gate.strip()
    else:
        raise LedgerError("gate must be a string or null")
    return Directive(
        id=_text(payload.get("id")),
        summary=_text(payload.get("summary")),
        requested_at=_text(payload.get("requested_at")),
        repository=_text(payload.get("repository")),
        checkout=_text(payload.get("checkout")),
        target=_text(payload.get("target")),
        task=_text(payload.get("task")),
        owner=_text(payload.get("owner")),
        source_row=_text(payload.get("source_row")) or None,
        parent_id=_text(payload.get("parent_id")) or None,
        implementation=implementation,
        gate=gate,
    )


def load_ledger(path: Path) -> tuple[int, tuple[Directive, ...]]:
    try:
        payload = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise LedgerError(f"cannot read ledger {path}: {error}") from error
    if not isinstance(payload, Mapping) or payload.get("schema_version") != 1:
        raise LedgerError("ledger schema_version must be 1")
    raw_directives = payload.get("directives")
    if not isinstance(raw_directives, list):
        raise LedgerError("ledger directives must be a list")
    directives = tuple(
        _parse_directive(item)
        for item in raw_directives
        if isinstance(item, Mapping)
    )
    if len(directives) != len(raw_directives):
        raise LedgerError("every directive must be an object")
    ids = [item.id for item in directives]
    if any(not item for item in ids):
        raise LedgerError("every directive requires a nonempty id")
    duplicates = sorted(item for item, count in Counter(ids).items() if count > 1)
    if duplicates:
        raise LedgerError(f"duplicate directive ids: {','.join(duplicates)}")
    known = set(ids)
    for directive in directives:
        if directive.parent_id and directive.parent_id not in known:
            raise LedgerError(
                f"directive {directive.id} has unknown parent {directive.parent_id}"
            )
        if directive.parent_id == directive.id:
            raise LedgerError(f"directive {directive.id} cannot parent itself")
    _assert_acyclic(directives)
    return 1, directives


def _assert_acyclic(directives: Sequence[Directive]) -> None:
    parents = {item.id: item.parent_id for item in directives}
    for start in parents:
        seen: set[str] = set()
        current: str | None = start
        while current:
            if current in seen:
                raise LedgerError(f"directive parent cycle includes {current}")
            seen.add(current)
            current = parents[current]


def _query_known_tasks(
    directives: Sequence[Directive], run: Run, timeout: float
) -> tuple[dict[str, str], str | None]:
    """Accountable task id -> its TaskGraph status.

    Selecting `status` as well as `local_id` is load-bearing, not cosmetic. Existence alone
    answers "is there a task", which a CLOSED task satisfies exactly as well as a live one --
    so a directive could sit unlanded while the only record a human would look at said the work
    was done. Measured 2026-08-08 on the live ledger: 3 of 21 rows were non-satisfied with a
    CLOSED accountable task, including the `green-time-automatic-log` row that has neither an
    implementation nor a gate. See `closed_task` in `_metadata_issues`.
    """
    tasks = sorted({item.task for item in directives if item.task})
    if not tasks:
        return {}, None
    if any(not TASK_RE.fullmatch(task) for task in tasks):
        return {}, "task ids may contain only letters, digits, dot, underscore, hyphen"
    quoted = ",".join("'" + task.replace("'", "''") + "'" for task in tasks)
    query = f"SELECT local_id || '\t' || status FROM tasks WHERE local_id IN ({quoted});"

    known: dict[str, str] = {}
    first_error: str | None = None
    for index, db in enumerate(task_databases()):
        env = dict(os.environ)
        if db is not None:
            env["TG_DB_PATH"] = str(db)
        result = run(("tg", "sql", query), cwd=ROOT, timeout=timeout, env=env)
        if result.returncode != 0:
            # Only the LIVE database failing is a lookup outage. An archive that
            # cannot be opened is a missing archive, not an unknown fleet state.
            if index == 0:
                first_error = (
                    result.stderr or result.stdout or "TaskGraph lookup failed"
                ).strip()
            continue
        for line in result.stdout.splitlines():
            local_id, _, status = line.strip().partition("\t")
            local_id = local_id.strip()
            if not TASK_RE.fullmatch(local_id) or local_id == "local_id":
                continue
            status = status.strip()
            # First database to answer wins, and the live one is queried first, so
            # a task that still exists live is never reported from an archive.
            if local_id in known:
                continue
            known[local_id] = status if index == 0 else f"{status}{ARCHIVED_SUFFIX}"
        if len(known) == len(tasks):
            break
    if first_error is not None and not known:
        return {}, first_error
    return known, None


def task_databases() -> list[Path | None]:
    """The live TaskGraph database first, then same-lineage archives.

    A fleet cutover RENAMES the database (2026-08-07: `hermit.db` became the
    archive and `hermit2.db` the live fleet), so every directive filed before the
    cutover names a task that is perfectly real and simply lives in the previous
    file. Resolving against the live database alone reported `task_not_found` on
    21 of 21 records -- a 100% failure that is a partition, never a fact.

    Lineage is the ALPHABETIC STEM of the live database, so `hermit2.db` admits
    `hermit.db` and `hermit1.db` but not `hermit-policy-audit.db`; matching a bare
    `hermit*` prefix would resolve task ids out of unrelated projects' databases
    and manufacture a false positive, which is worse than the miss it fixes.
    Override the whole set with `DIRECTIVE_TASK_DBS` (os.pathsep-separated).
    """
    override = os.environ.get("DIRECTIVE_TASK_DBS")
    if override is not None:
        return [Path(part) for part in override.split(os.pathsep) if part]
    live_raw = os.environ.get("TG_DB_PATH")
    # `None` means "let `tg` pick its own default"; we must not guess it here.
    if not live_raw:
        return [None]
    live = Path(live_raw)
    databases: list[Path | None] = [live]
    stem_base = re.match(r"^([A-Za-z_.-]+?)\d*$", live.stem)
    if stem_base is None:
        return databases
    pattern = re.compile(rf"^{re.escape(stem_base.group(1))}\d*$")
    siblings = []
    try:
        for candidate in live.parent.glob("*.db"):
            if candidate == live or not pattern.match(candidate.stem):
                continue
            siblings.append(candidate)
    except OSError:
        return databases
    # Newest archive first: the most recent predecessor is the likeliest owner.
    siblings.sort(key=lambda path: path.stat().st_mtime if path.exists() else 0, reverse=True)
    databases.extend(siblings)
    return databases


def _metadata_issues(
    directive: Directive, known_tasks: set[str], task_lookup_error: str | None
) -> list[str]:
    issues: list[str] = []
    try:
        date.fromisoformat(directive.requested_at)
    except ValueError:
        issues.append("missing_or_invalid_date")
    if not directive.summary:
        issues.append("missing_summary")
    if not REPO_RE.fullmatch(directive.repository):
        issues.append("missing_or_invalid_repository")
    checkout = (ROOT / directive.checkout).resolve() if directive.checkout else None
    if checkout is None or not checkout.is_relative_to(ROOT) or not checkout.is_dir():
        issues.append("missing_or_invalid_checkout")
    if not directive.target:
        issues.append("missing_target")
    if not directive.task:
        issues.append("missing_task")
    elif task_lookup_error:
        issues.append("task_lookup_unavailable")
    elif directive.task not in known_tasks:
        issues.append("task_not_found")
    elif known_tasks[directive.task].endswith(ARCHIVED_SUFFIX):
        # Real and accountable, but the pointer is into a database this fleet no
        # longer writes. Reported so the pointer can be migrated; deliberately NOT
        # in the state chain below, because a satisfied directive whose task was
        # closed before a cutover is a correct end state, not drift.
        issues.append("task_in_archived_db")
    if (
        directive.task
        and directive.task in known_tasks
        and known_tasks[directive.task]
        .removesuffix(ARCHIVED_SUFFIX)
        .strip()
        .upper()
        .startswith("CLOSED")
    ):
        # Recorded unconditionally, including on satisfied rows, where a closed task is the
        # CORRECT end state. Only `evaluate` decides it is a contradiction, and only for a row
        # that is not satisfied -- the point is the pairing, not the closure.
        issues.append("closed_task")
    if not directive.owner:
        issues.append("missing_owner")
    if directive.implementation is None:
        issues.append("no_implementation")
    else:
        kind = directive.implementation.kind
        identity = directive.implementation.identity
        if kind == "commit" and not SHA_RE.fullmatch(identity):
            issues.append("invalid_commit_identity")
        elif kind == "pr" and (not identity.isdecimal() or int(identity) <= 0):
            issues.append("invalid_pr_identity")
        elif kind not in {"commit", "pr"}:
            issues.append("invalid_implementation_kind")
    if directive.gate is not None:
        # A gate must NAME the blocking condition; a bare "gated" is a quieter
        # form of unknown, so an empty gate is rejected as invalid metadata.
        if not directive.gate:
            issues.append("unnamed_gate")
        elif directive.implementation is not None:
            # An already-implemented directive cannot also be waiting on a gate.
            issues.append("gate_on_implemented")
    return issues


def _verify_landing(
    directive: Directive, run: Run, timeout: float
) -> tuple[str, str, str | None, str | None, str]:
    assert directive.implementation is not None
    source = (ROOT / directive.checkout).resolve()
    command = (
        sys.executable,
        str(ROOT / "ci-hub/remediation/protocol.py"),
        "verify-landing",
        directive.implementation.identity,
        "--repo",
        directive.repository,
        "--source",
        str(source),
        "--target",
        directive.target,
        "--json",
    )
    result = run(command, cwd=ROOT, timeout=timeout)
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError:
        detail = (result.stderr or result.stdout or "verifier emitted no JSON").strip()
        if _looks_like_fetch_failure(result.stderr, result.stdout):
            return "fetch_failed", "fetch_failed", None, None, f"fetch failed: {detail}"
        return "unverifiable", "unverifiable", None, None, detail
    if not isinstance(payload, Mapping):
        return "unverifiable", "unverifiable", None, None, "invalid verifier JSON"
    landing_state = _text(payload.get("state")) or "unverifiable"
    ancestry = _text(payload.get("ancestry")) or "unverifiable"
    resolved = _text(payload.get("resolved_sha")) or None
    reason = _text(payload.get("reason")) or landing_state
    tip_result = run(
        ("git", "-C", str(source), "rev-parse", f"origin/{directive.target}"),
        cwd=ROOT,
        timeout=max(1.0, min(timeout, 5.0)),
    )
    target_tip = tip_result.stdout.strip() if tip_result.returncode == 0 else None
    if result.returncode == 0 and landing_state == "landed" and ancestry == "ancestor":
        return "satisfied", landing_state, resolved, target_tip, "fresh-main ancestor"
    if result.returncode == 1 and landing_state == "not-landed":
        return "not_landed", landing_state, resolved, target_tip, reason
    # The verifier ran but could not confirm landing. A fetch/network failure is
    # distinct from an ambiguous verdict: the checker never reached the evidence,
    # so it must not be conflated with a genuinely missing commit or a clean pass.
    if _looks_like_fetch_failure(reason, result.stderr, result.stdout):
        return "fetch_failed", "fetch_failed", resolved, target_tip, f"fetch failed: {reason}"
    return "unverifiable", landing_state, resolved, target_tip, reason


def evaluate(
    *,
    ledger_path: Path,
    run: Run = _run,
    deadline_seconds: float = 180,
    checked_at: str | None = None,
) -> Report:
    schema_version, directives = load_ledger(ledger_path)
    started = time.monotonic()
    known_tasks, task_lookup_error = _query_known_tasks(
        directives, run, max(1.0, min(deadline_seconds, 5.0))
    )
    base: dict[str, DirectiveResult] = {}
    for directive in directives:
        issues = _metadata_issues(directive, known_tasks, task_lookup_error)
        implementation = directive.implementation
        landing_state = "not_checked"
        ancestry = "not_checked"
        resolved = None
        target_tip = None
        reason = "no claimed implementation identity"
        state = "open"
        identity_valid = implementation is not None and not any(
            issue.startswith("invalid_") for issue in issues
        )
        checkout_valid = not any(
            issue in {"missing_or_invalid_checkout", "missing_or_invalid_repository", "missing_target"}
            for issue in issues
        )
        if identity_valid and checkout_valid:
            remaining = deadline_seconds - (time.monotonic() - started)
            if remaining <= 0:
                state = "unverifiable"
                reason = "directive-check deadline exhausted"
            else:
                state, landing_state, resolved, target_tip, reason = _verify_landing(
                    directive, run, max(1.0, remaining)
                )
                ancestry = {
                    "satisfied": "ancestor",
                    "not_landed": "not-ancestor",
                    "fetch_failed": "fetch_failed",
                }.get(state, "unverifiable")
        # Two facts, two states, two remedies. Merging them made a database
        # cutover -- every accountable task alive in the previous file -- read as
        # 21 directives nobody had ever filed a task for.
        if "missing_task" in issues:
            state = "missing_task"
            reason = "directive names no accountable task"
        elif "task_not_found" in issues:
            state = "task_not_found"
            reason = (
                f"directive names task {directive.task!r}, which exists in none of "
                f"the {len(task_databases())} known task database(s)"
            )
        elif "task_lookup_unavailable" in issues:
            state = "unverifiable"
            reason = task_lookup_error or "TaskGraph lookup unavailable"
        elif "missing_owner" in issues:
            state = "needs_owner"
            reason = "directive has no accountable owner"
        elif any(
            issue
            not in {
                "no_implementation",
                "missing_task",
                "task_not_found",
                # Not invalid metadata: the pointer is well-formed and resolves,
                # just into a pre-cutover database. It is reported as its own
                # issue and left out of the state chain so it cannot demote a row
                # that is otherwise satisfied.
                "task_in_archived_db",
                "missing_owner",
                # A closed task is not INVALID metadata -- on a satisfied row it is the correct
                # end state. Whether it is a contradiction is decided below, against the row's
                # settled state, not here.
                "closed_task",
            }
            for issue in issues
        ):
            state = "invalid"
            reason = "invalid directive metadata"
        elif implementation is None and directive.gate:
            state = "gated"
            reason = f"gated on: {directive.gate}"
        elif implementation is None:
            state = "open"
        # UNACCOUNTABLE: an ungated row is not satisfied, yet its accountable task is CLOSED.
        # Nothing else catches that pairing, because task lookup tests existence and a closed
        # task exists. A named external gate is different: the versioned gate condition is the
        # durable obligation, and the hourly checker continues to surface it after the finite
        # recording task closes. Demoting that explicit deferral to `unaccountable` makes it
        # impossible to record a completed decision without filing an artificial forever-open
        # task. Keep only explicitly named gates exempt; a closed task plus no implementation
        # and no gate remains genuine drift.
        # A NO-RESULT IS NOT A NEGATIVE. `unverifiable` and `fetch_failed` mean the
        # checker could not look -- a deadline expired, or the landing verifier
        # errored -- not that the obligation is unmet. Demoting them here asserts
        # "nothing will surface this unmet obligation" about a row that was never
        # evaluated, and the summary then renders it as drift. Measured at
        # d6450b82 with `--deadline-secs 1`: 19 satisfied, landed directives
        # reported as `drift=19` with `unverifiable=0` -- the state describing what
        # actually happened was erased before it reached the report. The count
        # scales with the deadline (1s -> 19, the shipped 24s default -> 4, 300s ->
        # 0), which is the tell that it measures how far the run got, not the work.
        # They stay NON-GREEN and still fail the run; they simply stop sharing a
        # bucket with drift, because the two need opposite responses: retry or
        # raise the deadline, versus re-land the work.
        if (
            state not in {"satisfied", "gated", "unverifiable", "fetch_failed"}
            and "closed_task" in issues
        ):
            reason = (
                f"{reason}; accountable task {directive.task!r} is CLOSED, so nothing will "
                f"surface this unmet obligation"
            )
            state = "unaccountable"
        base[directive.id] = DirectiveResult(
            id=directive.id,
            summary=directive.summary,
            requested_at=directive.requested_at,
            repository=directive.repository,
            target=directive.target,
            task=directive.task,
            owner=directive.owner,
            source_row=directive.source_row,
            parent_id=directive.parent_id,
            implementation_kind=implementation.kind if implementation else None,
            implementation_identity=implementation.identity if implementation else None,
            gate=directive.gate or None,
            state=state,
            landing_state=landing_state,
            ancestry=ancestry,
            resolved_sha=resolved,
            target_tip=target_tip,
            issues=tuple(issues),
            reason=reason,
        )

    children: dict[str, list[str]] = defaultdict(list)
    for directive in directives:
        if directive.parent_id:
            children[directive.parent_id].append(directive.id)
    effective: dict[str, DirectiveResult] = {}

    def resolve(item_id: str) -> DirectiveResult:
        if item_id in effective:
            return effective[item_id]
        item = base[item_id]
        child_results = [resolve(child) for child in children[item_id]]
        if item.state == "satisfied":
            incomplete = [child.id for child in child_results if child.state != "satisfied"]
            if incomplete:
                item = replace(
                    item,
                    state="partial",
                    reason="incomplete child obligations: " + ",".join(incomplete),
                )
        effective[item_id] = item
        return item

    results = tuple(resolve(item.id) for item in directives)
    counts = dict(sorted(Counter(item.state for item in results).items()))
    issue_counts = dict(
        sorted(Counter(issue for item in results for issue in item.issues).items())
    )
    if any(item.state in {"invalid", "unverifiable", "fetch_failed"} for item in results):
        # A fetch failure means the checker could not reach the evidence, so the
        # verdict is genuinely unknown (exit 2), never a clean pass and never a
        # confirmed "not landed" red — the same severity as `unverifiable`.
        overall_state, exit_code = "unknown", 2
    elif all(item.state == "satisfied" for item in results):
        overall_state, exit_code = "green", 0
    else:
        overall_state, exit_code = "red", 1
    return Report(
        schema_version=schema_version,
        checked_at=checked_at
        or datetime.now(timezone.utc).isoformat(timespec="seconds"),
        ledger=str(ledger_path),
        source_rows=len({item.source_row for item in directives if item.source_row}),
        records=len(results),
        overall_state=overall_state,
        exit_code=exit_code,
        counts=counts,
        issue_counts=issue_counts,
        directives=results,
    )


def _field(value: object) -> str:
    return " ".join(str(value).split()) or "none"


def _print_fields(report: Report) -> None:
    drift = [item.id for item in report.directives if item.state in DRIFT_STATES]
    no_result = [item.id for item in report.directives if item.state in NO_RESULT_STATES]
    gated = [item.id for item in report.directives if item.state == "gated"]
    unaccountable = [item.id for item in report.directives if item.state == "unaccountable"]
    archived = report.issue_counts.get("task_in_archived_db", 0)
    in_progress = report.counts.get("open", 0)
    if drift:
        summary = "drift=" + ",".join(drift[:8])
    elif no_result:
        # NOT "no directive drift": the run did not finish, so it has no opinion
        # about drift to report. Saying so is what keeps this from becoming the
        # silent pass that removing these states from `drift` could otherwise buy.
        summary = (
            f"INCOMPLETE: {len(no_result)} record(s) unverified"
            f" ({','.join(no_result[:5])}); no drift verdict"
        )
    else:
        summary = (
            "no directive drift"
            f"; in_progress={in_progress} gated={len(gated)}"
            f" task_in_archived_db={archived}"
        )
    fields = {
        "state": report.overall_state,
        "source_rows": report.source_rows,
        "records": report.records,
        "satisfied": report.counts.get("satisfied", 0),
        "partial": report.counts.get("partial", 0),
        "open": in_progress,
        "gated": len(gated),
        "needs_owner": report.counts.get("needs_owner", 0),
        "unaccountable": len(unaccountable),
        "missing_task": report.counts.get("missing_task", 0),
        "task_not_found": report.counts.get("task_not_found", 0),
        "task_in_archived_db": report.issue_counts.get("task_in_archived_db", 0),
        "not_landed": report.counts.get("not_landed", 0),
        "unverifiable": report.counts.get("unverifiable", 0),
        "no_result": len(no_result),
        "drift": len(drift),
        "summary": summary,
    }
    for key, value in fields.items():
        print(f"{key}={_field(value)}")


def quickstart() -> None:
    print("Owner directive tracker quickstart")
    print("1. Add the directive to ci-hub/directives/ledger.json when it is asked.")
    print("2. Record date, repo, TaskGraph task, owner, and commit/PR identity.")
    print("3. Add child records for every cross-repo or incomplete-scope remainder.")
    print("4. Run ./ci-hub/directives/check.py; only fresh-main ancestry is satisfied.")
    print("5. Inspect ignored/ci-hub/directives/latest.json for the durable verdict.")


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Fail closed unless owner tooling directives are ancestry-confirmed"
    )
    parser.add_argument("--ledger", type=Path, default=DEFAULT_LEDGER)
    parser.add_argument("--output", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--deadline-secs", type=float, default=180)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--no-write", action="store_true")
    parser.add_argument("--quickstart", action="store_true")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.quickstart:
        quickstart()
        return 0
    if args.deadline_secs <= 0:
        print("deadline must be positive", file=sys.stderr)
        return 2
    try:
        report = evaluate(
            ledger_path=args.ledger.resolve(), deadline_seconds=args.deadline_secs
        )
    except LedgerError as error:
        print(f"state=invalid\nsummary={_field(error)}")
        return 2
    if not args.no_write:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report.to_dict(), indent=2, sort_keys=True) + "\n")
    if args.json:
        print(json.dumps(report.to_dict(), sort_keys=True))
    else:
        _print_fields(report)
    return report.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
