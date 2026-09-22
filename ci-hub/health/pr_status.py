#!/usr/bin/env python3
"""Summarize open-PR CI health, robustly, on the hosts we actually run ops from.

Two engines:

* ``gh`` (DEFAULT) — one proxied ready-non-draft ``gh pr list --json`` call per
  repo, canonical review-surface and actor-permission reads for each labeled
  post-facto review, one dereference of the registered exact-head hosted
  authority per ready head, and a bounded exact-job lookup only for a selected
  failing Actions check.
  ``gh`` returns ``mergeable`` and ``statusCheckRollup`` inline, so there is NO
  per-PR local ``git fetch``. The job lookup is the narrow authority that can
  distinguish a setup-only runner failure from a product failure.

  ``statusCheckRollup`` IS NOT A COMPLETE BOARD, AND ON ITS OWN IT LIES ABOUT
  THIS FLEET. Measured 2026-08-08 over rrnewton/hermit's 12 ready non-draft
  heads, matching every check run by job id: of 694 check runs attached to those
  commits, all 239 produced by ``pull_request``-event workflow runs appeared in
  the rollup (239/239) and NONE of the 455 produced by ``workflow_dispatch``
  runs did (0/455). ``merge-gate-v4`` dispatches ``ci-portable.yml`` and
  ``ci-privileged.yml`` via ``workflow_dispatch``, so on every PR the gate has
  driven, the whole product-CI surface — including the registered authority job
  ``Regular tests (GitHub-managed portable)`` — is structurally invisible here.
  That produced a reported ``green=0 of 12`` that was a blind spot rather than
  an observation, and, in the dangerous direction, three heads whose authority
  job had genuinely FAILED (#1873 #1874 #1890) were reported ``pending``.

  So the rollup is kept ONLY for the checks it does faithfully carry (the
  ``pull_request``-triggered landing-gate/review meta-checks), and the hosted
  product verdict comes from the ONE registered verifier every other consumer
  uses, ``ci-hub hosted-status`` (``ci-hub/remediation/protocol.py``:
  ``verification_policy_for_repo`` → ``github_runs`` → exact-SHA jobs). This
  file holds NO second implementation of authority reading; it dereferences the
  shipped command exactly as ``ci-hub/landing/exact-head-validation-authority.sh``
  does. A head is ``red`` if EITHER source is red, ``green`` if neither is red
  and at least one reports green, and ``pending`` otherwise.

* ``planner`` (opt-in, ``--engine planner``) — adapts the pinned
  agent-utils/pr-landing-planner and runs REAL ``git merge-tree`` conflict
  detection over the open set (``--conflict-detector merge-tree``). The thing
  that once made this look expensive was never the analysis: one merge-tree
  base-conflict probe is ~36.5ms local (measured 2026-08-04, 40 probes over
  rrnewton/hermit's 107 open PRs; whole open set vs main ~4s). The real cost
  was a per-PR ``git fetch`` fan-out (21.5s over 25 heads), which agent-utils
  PR #14 collapses to ONE batched fetch (0.85s, ~25x). ``gh`` stays the DEFAULT
  for the every-tick path because it needs no local ``git fetch`` at all and is
  the only path that works under 3pai BpfJailer (where the planner's network
  fetch is intermittently blocked with FILE_OPEN denials or hangs). Reach for
  ``--engine planner`` on a planning run, where real conflict data is worth the
  one fetch.

Why the ``gh`` default and the loudness discipline below matter: this is the
sanctioned ops tool, dispatched every PR-status tick. Its previous per-PR git
fan-out could hang or be silently blocked on 3pai, and *no output plus a zero
exit is indistinguishable from "there are no PRs"* — a monitoring tool that
reports emptiness when it is actually blind will eventually convince someone the
backlog is clear. So: a failed/blocked query is ALWAYS reported as UNAVAILABLE
with an actionable reason and a NON-ZERO exit, never as an empty-but-green
report. The report heading always prints, so stdout is never empty.

Boxing discipline (unchanged intent): per-repo subprocess timeout, an overall
command deadline, bounded retries with backoff on transient network errors, and
partial results plus an explicit statement of what could not be fetched, so the
tool ALWAYS terminates with a report instead of hanging.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable, Mapping, Sequence

ROOT = Path(__file__).resolve().parents[2]
STATE_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", ROOT)).resolve()
sys.path.insert(0, str(ROOT / "ci-hub"))
sys.path.insert(0, str(ROOT / "ci-hub" / "ledger"))
import validate_rows as canonical_ledger  # noqa: E402
import approval_binding as approval_authority  # noqa: E402
import review_contract  # noqa: E402

from check_outcome import CheckOutcome, classify_check, select_latest_checks
from actions_job_outcome import (
    GitHubActionsJobAuthority,
    SetupOnlyVerification,
)
from validate.flake_class import failure_tier

AGENT_TOOL = Path(os.environ.get("CI_HUB_AGENT_TOOL", ROOT / "ci-hub/bin/agent-tool"))
CI_HUB_BIN = Path(os.environ.get("CI_HUB_BIN", ROOT / "ci-hub/ci-hub"))
DEFAULT_REPOS = ("rrnewton/hermit", "rrnewton/reverie")
DEFAULT_WARN_THRESHOLD = 10
MAX_FETCH_ATTEMPTS = 3
# One successful all-open census is authoritative only through the next health
# cadence.  A longer transport TTL used to let a six-hour-old population bypass
# the consumer's 15-minute freshness verdict, so newly-created PRs were absent
# while the count still looked current.  An operator may shorten caching, but
# may never extend it past the evidence bound.
CENSUS_FRESHNESS_BOUND_SECS = 15 * 60
DEFAULT_CENSUS_TTL_SECS = min(
    float(
        os.environ.get(
            "CI_HUB_PR_CENSUS_TTL_SECS", str(CENSUS_FRESHNESS_BOUND_SECS)
        )
    ),
    CENSUS_FRESHNESS_BOUND_SECS,
)
DEFAULT_CENSUS_CACHE_DIR = STATE_ROOT / "ignored/ci-hub/pr-census-cache"

# Timeout basis (derived, not a plausible constant):
#   planner engine — the binding cost is the FETCH, not the conflict analysis.
#   With the pre-#14 pinned agent-utils it fetches per PR: measured 2026-08-03
#   on a devserver, the reverie planner completed in 35.17s for 26 open PRs =>
#   ~1.35s/PR of sequential proxied `git fetch` (hermit's 128 open PRs =>
#   ~173s happy-path; combined ~208s), which is what these budgets are sized
#   for. agent-utils PR #14 collapses that fan-out to ONE batched fetch (0.85s
#   over 25 heads, ~25x), after which the whole planning run is dominated by
#   fetch-once + cheap local merge-tree probes (~36.5ms each; open set vs main
#   ~4s). The `merge-tree` conflict flip in planner_command adds only those
#   cheap local probes, not more fetching, so these budgets stay ample.
#   gh engine — one `gh pr list` API call per repo returns in a few seconds
#   regardless of PR count. Selected failed Actions checks can add bounded,
#   cached exact-job calls, all sharing the same outer per-repo deadline.
DEFAULT_PER_REPO_TIMEOUT = float(os.environ.get("CI_HUB_PR_STATUS_TIMEOUT", "300"))
DEFAULT_OVERALL_DEADLINE = float(os.environ.get("CI_HUB_PR_STATUS_DEADLINE", "480"))
# Seconds to wait for the killed planner child to die before moving on.
_TERMINATE_GRACE = 10.0

# Default network wrapper. Bare `gh`/`git` reach GitHub directly, which fails on
# proxied hosts ("network is unreachable"); `with-proxy` is idempotent (wrapping
# an already-proxied command is a no-op), so we prefix it by default when it is
# on PATH. Override with --net-wrapper "" to disable, or --net-wrapper CMD.
DEFAULT_NET_WRAPPER = "with-proxy"


def _age_hours(created_at):
    """Hours since the PR was opened, or ``None`` when GitHub gave no timestamp.

    Returns ``None`` rather than 0 for a missing/unparseable value: 0 would read
    as "brand new" and sort a PR of unknown age to the front of an oldest-first
    drain, which is the opposite of fail-closed.
    """
    if not isinstance(created_at, str) or not created_at.strip():
        return None
    try:
        opened = datetime.datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    except ValueError:
        return None
    if opened.tzinfo is None:
        opened = opened.replace(tzinfo=datetime.timezone.utc)
    delta = datetime.datetime.now(datetime.timezone.utc) - opened
    return round(delta.total_seconds() / 3600.0, 2)


# Fields pulled in the single `gh pr list` call. mergeable + statusCheckRollup
# are what let us classify every open PR without any per-PR local git fetch.
GH_FIELDS = (
    "number",
    "title",
    "isDraft",
    "mergeable",
    "mergeStateStatus",
    "reviewDecision",
    "baseRefName",
    "headRefName",
    "updatedAt",
    "createdAt",
    "labels",
    "comments",
    "statusCheckRollup",
    "headRefOid",
)
MECHANISM_GH_FIELDS = ("number", "title", "isDraft", "labels")

# Transient network/GH errors worth a bounded retry.
_RETRYABLE_MARKERS = (
    "stream error",
    "CANCEL",
    "504",
    "502",
    "timeout",
    "timed out",
    "connection reset",
    "temporarily unavailable",
    "changed during collection",
    # A TRUNCATED API RESPONSE. gh itself exits non-zero and prints this when it
    # cannot parse what the API returned mid-stream -- the same transport class
    # as "stream error"/"connection reset", and just as transient.
    #
    # Measured 2026-08-06/07: this took out the WHOLE hermit repo on 2 of 5
    # consecutive lander polls, and because the marker was absent `retryable`
    # was False, so the loop broke on attempt 1 and MAX_FETCH_ATTEMPTS (3) was
    # never reached. The tool had a retry budget that the one failure actually
    # occurring in practice could not use.
    #
    # Enriched queries are the ones at risk: measured on rrnewton/hermit at 106
    # open PRs, adding `statusCheckRollup` takes the response from 34,051 to
    # 461,854 bytes (13.6x) and 0.8s to 5.0s (6x). A 462 KB proxied response has
    # far more opportunity to arrive truncated. NOTE that is a plausible
    # mechanism, not a proven cause: 4 of 4 controlled A/B trials succeeded, so
    # the failure was never reproduced on demand.
    "unexpected end of json input",
)

# Review activity is durable and numbered; approval is an unsuffixed assertion
# about the current head. The writer and every reader use review_contract, so a
# round cannot be accepted on one path and rejected on another.
_MECHANISM_TAG_PREFIX = "mechanism:"
HEALTH_VERDICT_RULE = (
    "ready non-draft PR GitHub check rollups (pull_request-triggered checks "
    "only) COMBINED WITH the registered exact-head hosted job authority from "
    "`ci-hub hosted-status`, plus bounded exact-job evidence: a head is red if "
    "EITHER source is red; UNHEALTHY iff any available repo has real_reds>0 or "
    "outage_suspected=yes"
)
HEALTH_VERDICT_EXCLUDES = (
    "local validation receipts",
    "draft PRs",
    "main-branch health",
    "queue depth",
    # Named because it is the defect this tool shipped with: `statusCheckRollup`
    # carries 0 of 455 workflow_dispatch-produced check runs (measured
    # 2026-08-08), and merge-gate-v4 dispatches all product CI. The rollup ALONE
    # is therefore never the CI verdict here; `hosted_authority` carries it.
    "statusCheckRollup alone (it omits every workflow_dispatch check run)",
)


class RepoUnavailable(RuntimeError):
    """The status query could not complete within its time budget or was blocked."""


@dataclass(frozen=True)
class ReviewProtocolStatus:
    """Review activity and current-head approval for one post-facto PR."""

    pr: int | None
    title: str
    draft: bool
    critical_required: bool
    codex_rounds: tuple[int, ...]
    claude_rounds: tuple[int, ...]
    review_rounds: str
    current_approvals: str
    complete: bool
    missing: tuple[str, ...]
    invalid_labels: tuple[str, ...]
    review_permission_unavailable: tuple[str, ...]


@dataclass(frozen=True)
class MechanismPr:
    """One open PR carrying a mechanism tag."""

    pr: int | None
    title: str
    draft: bool


@dataclass(frozen=True)
class MechanismOverlap:
    """Two or more open PRs that require joint coordinator inspection."""

    mechanism: str
    prs: tuple[MechanismPr, ...]


@dataclass(frozen=True)
class RollupClassification:
    """One latest-attempt selection and every conclusion derived from it."""

    state: str
    failing_check_names: tuple[str, ...]
    setup_only_no_result_checks: tuple[str, ...]
    setup_only_evidence: tuple[str, ...]
    prerequisite_no_result_checks: tuple[str, ...]
    prerequisite_evidence: tuple[str, ...]
    actions_job_verification_errors: tuple[str, ...]


SetupOnlyVerifier = Callable[[str, Mapping[str, object], str], SetupOnlyVerification]


@dataclass(frozen=True)
class RepoStatus:
    repo: str
    open: int
    green: int
    red: int
    pending: int
    real_reds: int
    outage_suspected: bool
    prs: tuple[dict[str, object], ...]
    # `open` is intentionally the CI-actionable population: READY NON-DRAFT
    # heads only.  Keep the full GitHub open-PR population beside it so a
    # consumer cannot relabel the smaller number as "all open PRs".  `None`
    # marks engines that do not carry this population; it is never inferred
    # from the ready count.
    all_open: int | None = None
    drafts: int | None = None
    population_error: str = ""
    population_age_secs: float | None = None
    review_protocol: tuple[ReviewProtocolStatus, ...] = ()
    mechanism_overlaps: tuple[MechanismOverlap, ...] = ()
    undetermined_reds: int = 0
    # Split of real_reds by what actually failed at head. A product-red has at
    # least one failing check that is a genuine product test/build; a gate-red
    # fails ONLY landing-gate/review meta-checks (merge-gate*, review-protocol),
    # i.e. the PR merely lacks a valid receipt/review — main is not broken.
    # product_reds + gate_reds == real_reds on the gh engine (0 on the planner,
    # which does not enumerate per-check names).
    product_reds: int = 0
    gate_reds: int = 0
    # Open-PR heads whose EXACT SHA carries a full-green receipt in the LOCAL
    # validate ledger — the second authority the GitHub ``green`` count is blind
    # to. A ledger-green head that GitHub shows red/pending is counted here and
    # kept out of the red buckets; this is what reconciles a GitHub ``green=0``
    # with banked local greens (they measure different authorities).
    green_local: int = 0
    # Open-PR heads whose EXACT SHA carries only a NON-durable failure in the LOCAL
    # validate ledger — a red the ledger-side named-gate predicate already demotes
    # (validate_status::failure_disposition / flake_class.failure_tier).
    # ``ledger_no_result`` = the local run had no named failing gate and no failure
    # count (a NO-RESULT wearing a red badge, or an environment fault);
    # ``ledger_needs_rerun`` = the run did not complete its gate contract, or
    # recorded a failure with no named gate to attribute it to. Both are demoted
    # OUT of real_reds: the GitHub product red is not
    # corroborated by any complete local run, so it must not mark the fleet
    # unhealthy or block landing as a genuine break. This is the SYMMETRIC peer of
    # green_local — green_local rescues a GitHub red the ledger proved GREEN;
    # these demote a GitHub product red the ledger proved was a no-result/partial.
    # A ledger row that ran the full suite and genuinely failed (tier "ok") is NOT
    # counted here — it stays a real product red.
    ledger_no_result: int = 0
    ledger_needs_rerun: int = 0
    # Count of exact Actions check attempts whose dereferenced job proved that
    # only ``Set up job`` ran and failed. These become typed NO_RESULT (pending),
    # never green, and remain visible in each PR's evidence fields.
    setup_only_no_result_checks: int = 0
    # Narrow downstream consequences whose exact registered merge-gate-v4 job
    # failed only because a same-run prerequisite above was setup-only. These
    # are independently visible and, like their source, remain pending.
    prerequisite_no_result_checks: int = 0
    # Ready heads by what the ONE registered exact-head hosted verifier
    # (`ci-hub hosted-status`) said, reported separately from `green`/`red` so
    # the two sources never silently stand in for each other. `hosted_unknown`
    # counts heads whose authority we did NOT manage to dereference — coverage,
    # not a verdict. These three plus the pending/no_result authority states sum
    # to `open`.
    hosted_green: int = 0
    hosted_red: int = 0
    hosted_unknown: int = 0
    available: bool = True
    reason: str = ""

    @property
    def unhealthy(self) -> bool:
        # An unavailable repo is UNKNOWN, not unhealthy: we must not synthesize a
        # red from a query we never completed.
        return self.available and (self.real_reds > 0 or self.outage_suspected)


def _unavailable(
    repo: str,
    reason: str,
    mechanism_overlaps: tuple[MechanismOverlap, ...] = (),
    *,
    open_count: int = 0,
    all_open: int | None = None,
    drafts: int | None = None,
    population_error: str = "",
    population_age_secs: float | None = None,
) -> RepoStatus:
    return RepoStatus(
        repo=repo,
        open=open_count,
        green=0,
        red=0,
        pending=0,
        real_reds=0,
        outage_suspected=False,
        prs=(),
        all_open=all_open,
        drafts=drafts,
        population_error=population_error,
        population_age_secs=population_age_secs,
        mechanism_overlaps=mechanism_overlaps,
        available=False,
        reason=reason,
    )


def _checkout_for(repo: str) -> Path:
    name = repo.rsplit("/", 1)[-1]
    if name == "dev-hermit":
        return ROOT
    checkout = ROOT / name
    if not checkout.is_dir():
        raise RuntimeError(f"{repo}: local checkout is missing: {checkout}")
    return checkout


def resolve_net_wrapper(spec: str | None) -> list[str]:
    """Turn a --net-wrapper spec into a command prefix.

    ``None`` selects the default (``with-proxy`` if on PATH, else nothing, with a
    warning). An explicit empty string disables wrapping. A non-empty string is
    split shell-style. A named wrapper that is not on PATH is an error rather
    than a silent bare call that would fail with "network is unreachable".
    """
    if spec is None:
        if shutil.which(DEFAULT_NET_WRAPPER):
            return [DEFAULT_NET_WRAPPER]
        print(
            f"pr_status: note: default net wrapper {DEFAULT_NET_WRAPPER!r} not on "
            "PATH; issuing bare gh/git (may fail on proxied hosts). Pass "
            "--net-wrapper to override.",
            file=sys.stderr,
        )
        return []
    spec = spec.strip()
    if not spec:
        return []
    import shlex

    parts = shlex.split(spec)
    if parts and not shutil.which(parts[0]):
        raise RuntimeError(
            f"--net-wrapper {parts[0]!r} is not on PATH; refusing to run a bare "
            "network call that would fail on a proxied host"
        )
    return parts


# --------------------------------------------------------------------------- #
# gh engine (default)
# --------------------------------------------------------------------------- #


def _classify_rollup(
    repo: str,
    rollup: object,
    *,
    head_sha: str = "",
    setup_only_verifier: SetupOnlyVerifier | None = None,
) -> RollupClassification:
    """Classify one selected latest attempt per check context exactly once.

    Empty rollup (no checks yet) is treated as pending, not green: a PR with no
    checks has not demonstrated health. A failed Actions check is demoted only
    when the one semantic exact-job verifier proves the setup-only shape. Any
    verifier failure leaves the original failure intact and visible.
    """
    selected = select_latest_checks(rollup, head_sha=head_sha)
    if not selected:
        return RollupClassification("pending", (), (), (), (), (), ())
    any_fail = False
    any_pending = False
    any_ok = False
    failing_names: list[str] = []
    setup_only_names: list[str] = []
    setup_only_evidence: list[str] = []
    prerequisite_names: list[str] = []
    prerequisite_evidence: list[str] = []
    verification_errors: list[str] = []

    failed: list[tuple[int, dict[str, object]]] = []
    for index, check in enumerate(selected):
        if not isinstance(check, dict):
            continue
        status = str(check.get("status") or "").upper()
        outcome = str(check.get("conclusion") or check.get("state") or "").upper()
        if classify_check(status, outcome) is CheckOutcome.FAILED:
            failed.append((index, check))

    verifications: dict[int, SetupOnlyVerification] = {}
    verifier_errors: dict[int, str] = {}
    if setup_only_verifier is not None and failed:
        batch_verifier = getattr(setup_only_verifier, "verify_failures", None)
        if callable(batch_verifier):
            try:
                batch_results = tuple(
                    batch_verifier(
                        repo,
                        tuple(check for _, check in failed),
                        head_sha,
                    )
                )
                if len(batch_results) != len(failed):
                    raise ValueError(
                        "batch verifier result count mismatch: "
                        f"{len(batch_results)} != {len(failed)}"
                    )
                for (index, _), verification in zip(failed, batch_results, strict=True):
                    if not isinstance(verification, SetupOnlyVerification):
                        raise TypeError(
                            "batch verifier returned a non-verification result"
                        )
                    verifications[index] = verification
            except Exception as error:  # fail closed on a verifier defect
                message = f"verifier error: {type(error).__name__}: {error}"
                for index, _ in failed:
                    verifier_errors[index] = message
        else:
            for index, check in failed:
                try:
                    verifications[index] = setup_only_verifier(repo, check, head_sha)
                except Exception as error:  # fail closed on a verifier defect
                    verifier_errors[index] = (
                        f"verifier error: {type(error).__name__}: {error}"
                    )

    for index, check in enumerate(selected):
        if not isinstance(check, dict):
            continue
        name = str(check.get("name") or check.get("context") or "")
        status = str(check.get("status") or "").upper()
        # CheckRun => conclusion; StatusContext => state.
        outcome = str(check.get("conclusion") or check.get("state") or "").upper()
        classified = classify_check(status, outcome)
        if classified is CheckOutcome.FAILED:
            verification = verifications.get(index)
            if index in verifier_errors:
                verification_errors.append(
                    f"{name or '<unnamed>'}: {verifier_errors[index]}"
                )
            if verification is not None and verification.accepted:
                # Both the direct source and its narrowly proven registered
                # consequence are typed NO_RESULT. They block green even if
                # every other check passed.
                any_pending = True
                if verification.kind == "prerequisite-no-result":
                    prerequisite_names.append(name)
                    prerequisite_evidence.append(
                        f"{name or '<unnamed>'}: run={verification.run_id} "
                        f"job={verification.job_id} "
                        f"source_job={verification.source_job_id}"
                    )
                else:
                    setup_only_names.append(name)
                    setup_only_evidence.append(
                        f"{name or '<unnamed>'}: run={verification.run_id} "
                        f"job={verification.job_id}"
                    )
            else:
                any_fail = True
                failing_names.append(name)
                if verification is not None and not verification.accepted:
                    verification_errors.append(
                        f"{name or '<unnamed>'}: {verification.reason}"
                    )
        elif classified is CheckOutcome.PASSED:
            any_ok = True
        else:
            # A hole in the record blocks admission but never reports a red.
            any_pending = True
    if any_fail:
        state = "red"
    elif any_pending:
        state = "pending"
    elif any_ok:
        state = "green"
    else:
        state = "pending"
    return RollupClassification(
        state=state,
        failing_check_names=tuple(failing_names),
        setup_only_no_result_checks=tuple(setup_only_names),
        setup_only_evidence=tuple(setup_only_evidence),
        prerequisite_no_result_checks=tuple(prerequisite_names),
        prerequisite_evidence=tuple(prerequisite_evidence),
        actions_job_verification_errors=tuple(verification_errors),
    )


def _rollup_ci_state(rollup: object, *, head_sha: str = "") -> str:
    """Compatibility wrapper for pure rollup tests and downstream imports."""
    return _classify_rollup("", rollup, head_sha=head_sha).state


# Landing-gate / review META-checks. A red on one of these means the PR lacks a
# valid receipt (merge-gate) or a completed review (core-review-protocol) at its
# current head — a landing blocker, NOT a product-test failure. Kept in sync with
# Hermit's `merge-gate-v2` / `core-review-protocol` and Reverie's `merge-gate`
# check names. Matched tolerantly so a version bump (`merge-gate-v3`) or the
# planner's spaced form ("Merge Gate") still classifies as a gate check.
#
# REVERIE PIN POLICY IS A GATE, AND MISCLASSIFYING IT IS EXPENSIVE BECAUSE IT IS
# FLEET-WIDE. Measured 2026-08-07: hermit #1711 failed the historical
# `reverie-pin-is-latest-main`
# + `merge-gate-v4` at unchanged head 2d4866a0 and was reported as
# `real_reds=1 (product=1)` -- "a genuine break at a ready PR head". It was not.
# #1711 pinned reverie dd3c178e, IDENTICAL to hermit main; the check asserts the
# pin equalled REVERIE's latest main, which had advanced to 0ae0c01b. That was
# the policy then. The owner changed the rule on 2026-08-08 to ancestry plus
# monotonicity: the candidate pin must be on reverie/main history and must not
# regress from the landing-base pin; tip equality is explicitly not required.
# The final identity `reverie-pin-ancestor-and-monotonic` names both clauses:
# the pin is on Reverie's linear main history and advances only forward.  The
# ancestry-and-monotonicity identity remains an exact transition name only.
# A single unrecognised check name defeated the
# `all(...)` test below and turned shared pin drift into a phantom product break.
#
# BUT "one bump clears them all" HOLDS ONLY FOR THE SPECIAL CASE ABOVE, where the
# PR's pin already EQUALS main's. Measured 2026-08-07 on the next red set: #1665,
# #1762 and #1769 carried THREE DIFFERENT stale pins (dd3c178e / 79517704 /
# d973a85b), each a genuine ancestor of reverie main -- i.e. heads stale at
# different DEPTHS. Bumping main does not retroactively change a PR head's pinned
# SHA, so each of those heads additionally needed its own REBASE. The emitted
# actionability text said "one bump clears them all" and was wrong for that
# population; it now states the per-head rebase requirement instead.
_GATE_META_CHECK_NAMES = frozenset(
    {
        "merge-gate",
        "merge-gate-v2",
        "merge gate",
        "core-review-protocol",
        "reverie-pin-is-latest-main",
        "reverie-pin-ancestry-and-monotonicity",
        "reverie-pin-ancestor-and-monotonic",
    }
)
_LEGACY_PIN_GATE_NAME = re.compile(
    r"(?:(?:[a-z0-9][a-z0-9._-]*)-)?pin-is-latest-main"
    r"|(?:(?:[a-z0-9][a-z0-9._ -]*) )?pin is latest main"
)


def _is_gate_meta_check(name: str) -> bool:
    normalized = name.strip().lower()
    if normalized in _GATE_META_CHECK_NAMES:
        return True
    if normalized.startswith(("merge-gate", "merge gate")):
        return True
    if "review-protocol" in normalized:
        return True
    # Deliberately supported historical pin-gate grammar.  Full-match is
    # load-bearing: a product summary that merely contains one of these tokens
    # is still a product failure, never a gate-meta check.
    return _LEGACY_PIN_GATE_NAME.fullmatch(normalized) is not None


def _label_names(entry: dict[str, object]) -> set[str]:
    """Normalize ``gh pr list --json labels`` without accepting bad schemas."""
    labels = entry.get("labels")
    if not isinstance(labels, list):
        return set()
    names: set[str] = set()
    for label in labels:
        if isinstance(label, str):
            names.add(label)
        elif isinstance(label, dict) and isinstance(label.get("name"), str):
            names.add(label["name"])
    return names


def _review_protocol_status(entry: dict[str, object]) -> ReviewProtocolStatus | None:
    """Classify one PR through the canonical exact-head approval verifier."""
    labels = _label_names(entry)
    if review_contract.POST_FACTO_LABEL not in labels:
        return None

    rounds: dict[str, set[int]] = {
        family: set() for family in review_contract.REVIEW_FAMILIES
    }
    invalid_labels: list[str] = []
    for label in labels:
        parsed_round = review_contract.parse_round_label(label)
        if parsed_round is not None:
            reviewer, round_number = parsed_round
            rounds[reviewer].add(round_number)
        elif label.startswith(
            tuple(
                review_contract.round_label_prefix(family)
                for family in review_contract.REVIEW_FAMILIES
            )
        ):
            invalid_labels.append(label)
        elif (
            label.startswith(tuple(review_contract.PASSED_REVIEW_LABELS.values()))
            and label not in review_contract.PASSED_REVIEW_LABELS.values()
        ):
            invalid_labels.append(label)

    has_round = {reviewer: bool(values) for reviewer, values in rounds.items()}
    live_records = entry.get("_canonical_review_records")
    live_read_attempted = any(
        key in entry
        for key in (
            "_canonical_review_records",
            "_review_permission_unavailable",
            "_review_authority_unavailable",
        )
    )
    if isinstance(live_records, list):
        comments = [record for record in live_records if isinstance(record, dict)]
    elif live_read_attempted:
        # Never fall back to the unannotated `gh pr list` comments after a live
        # authority read failed. That would turn an unavailable permission check
        # into syntax-only approval.
        comments = []
    else:
        # Pure/offline callers retain the historical fixture interface. The live
        # command always attaches the permission-annotated shared records above.
        raw_comments = entry.get("comments")
        comments = (
            [
                {**comment, "_review_surface": "issue-comment"}
                for comment in raw_comments
                if isinstance(comment, dict)
            ]
            if isinstance(raw_comments, list)
            else []
        )
    head = str(entry.get("headRefOid") or "").lower()
    malformed: list[tuple[str, str]] = []
    review_order_error: str | None = None
    try:
        authority_rows = approval_authority.canonical_review_lane_rows(
            comments,
            head,
            pr_number=(
                entry.get("number")
                if isinstance(entry.get("number"), int)
                and not isinstance(entry.get("number"), bool)
                else None
            ),
            malformed=malformed,
        )
    except approval_authority.ReviewRecordError as error:
        # Missing event/version metadata cannot be repaired by trusting an
        # endpoint's incidental array order. Empty evidence makes every cache
        # label fail closed while this diagnostic says why authority is absent.
        authority_rows = []
        review_order_error = str(error)
    rows_by_lane = {str(row.get("lane")): row for row in authority_rows}
    has_approval: dict[str, bool] = {}
    unlabeled_bindings: list[str] = []
    permission_unavailable: list[str] = []
    for reviewer, expected in review_contract.PASSED_REVIEW_LABELS.items():
        row = rows_by_lane.get(reviewer, {})
        binding = str(row.get("state") or approval_authority.UNDETERMINED)
        refused_at = str(row.get("refused_at") or "")
        for detail in row.get("review_authority_unavailable") or []:
            if not isinstance(detail, dict):
                continue
            actor = str(detail.get("github_actor") or "unknown")
            permission = str(detail.get("permission") or "unavailable")
            permission_unavailable.append(f"{reviewer}:{actor}@{permission}")
        has_approval[reviewer] = binding == approval_authority.BOUND_CURRENT
        if expected not in labels:
            # Comment evidence decides authority. The missing label remains
            # visible as a cache-reconciliation diagnostic, but cannot remove
            # a canonical exact-head approval or turn a refusal into absence.
            if has_approval[reviewer]:
                unlabeled_bindings.append(f"unlabeled-binding:{reviewer}")
            elif binding in (
                approval_authority.REFUSED,
                approval_authority.REFUSED_STALE,
            ):
                refused_at = refused_at[:12] if refused_at else "unknown"
                unlabeled_bindings.append(
                    f"unlabeled-refusal:{reviewer}@{refused_at}"
                )
        elif not has_approval[reviewer]:
            # Keep the cache visible as a diagnostic, but give it zero current
            # authority. Missing APPROVED-AT is ABSENT approval, never a softer
            # present-but-unverified state.
            invalid_labels.append(f"{expected}:{binding}")
    invalid_labels.extend(f"malformed-binding:{line}" for _, line in malformed)
    if review_order_error is not None:
        invalid_labels.append(f"review-event-order:{review_order_error}")
    permission_error = entry.get("_review_permission_unavailable")
    if isinstance(permission_error, str) and permission_error:
        permission_unavailable.append(f"all:{permission_error}")
    authority_error = entry.get("_review_authority_unavailable")
    if isinstance(authority_error, str) and authority_error:
        invalid_labels.append(f"review-authority-unavailable:{authority_error}")
    critical_required = bool(
        approval_authority.critical_attestation_evidence(comments)
    )
    review_policy = approval_authority.review_lane_policy(
        authority_rows, critical_required
    )
    current_lanes = set(review_policy["current_lanes"])
    objection_lanes = {
        str(row.get("lane")) for row in review_policy["objection_rows"]
    }
    if not critical_required and current_lanes:
        optional_lanes = set(review_contract.REVIEW_FAMILIES) - current_lanes
        invalid_labels = [
            detail
            for detail in invalid_labels
            if not any(
                detail.startswith(
                    review_contract.PASSED_REVIEW_LABELS[lane] + ":"
                )
                for lane in optional_lanes
            )
        ]
        permission_unavailable = [
            detail
            for detail in permission_unavailable
            if not any(detail.startswith(lane + ":") for lane in optional_lanes)
        ]
    # Reported alongside the label diagnostics, and deliberately not folded into
    # them: an unlabeled binding is a MISSING CACHE over a real review, which is a
    # different condition from a cache that no longer holds.
    invalid_labels.extend(unlabeled_bindings)

    def paired_state(values: dict[str, bool]) -> str:
        present = sum(values.values())
        if present == 2:
            return "complete"
        return "partial" if present == 1 else "missing"

    missing: list[str] = []
    if critical_required:
        for reviewer in review_contract.REVIEW_FAMILIES:
            if not has_round[reviewer]:
                missing.append(review_contract.missing_round_requirement(reviewer))
            if not has_approval[reviewer]:
                missing.append(f"current-approval-{reviewer}")
        review_rounds = paired_state(has_round)
        current_approvals = paired_state(has_approval)
    else:
        required_lanes = current_lanes or set(review_contract.REVIEW_FAMILIES)
        if not current_lanes:
            missing.append("one current approval from codex or claude")
        if not any(has_round[lane] for lane in required_lanes):
            missing.append("one numbered review activity label from codex or claude")
        missing.extend(
            f"unresolved review objection in {lane}"
            for lane in sorted(objection_lanes)
        )
        review_rounds = (
            "complete"
            if any(has_round[lane] for lane in required_lanes)
            else "missing"
        )
        current_approvals = (
            "complete"
            if review_policy["satisfied"]
            else ("partial" if current_lanes else "missing")
        )

    number = entry.get("number")
    return ReviewProtocolStatus(
        pr=(
            number
            if isinstance(number, int) and not isinstance(number, bool)
            else None
        ),
        title=str(entry.get("title") or ""),
        draft=entry.get("isDraft") is True,
        critical_required=critical_required,
        codex_rounds=tuple(sorted(rounds["codex"])),
        claude_rounds=tuple(sorted(rounds["claude"])),
        review_rounds=review_rounds,
        current_approvals=current_approvals,
        complete=not missing,
        missing=tuple(missing),
        invalid_labels=tuple(sorted(invalid_labels)),
        review_permission_unavailable=tuple(sorted(permission_unavailable)),
    )


def _attach_canonical_review_authority(
    repo: str,
    entries: Sequence[object],
    *,
    net_wrapper: Sequence[str] = ("with-proxy",),
    gh_cmd: str = "gh",
    deadline: float | None = None,
) -> None:
    """Attach shared, permission-annotated review records to labeled PRs.

    A failure is kept on that PR for reporting. It does not erase the rest of the
    repository health result, and it never falls back to unannotated comment text.
    """
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        if review_contract.POST_FACTO_LABEL not in _label_names(entry):
            continue
        number = entry.get("number")
        if deadline is not None and deadline - time.monotonic() <= 0:
            raise approval_authority.GitHubDeadlineExceeded(
                f"{repo}: time budget exhausted before canonical review "
                f"authority for PR #{number if number is not None else '?'}"
            )
        if not isinstance(number, int) or isinstance(number, bool):
            entry["_review_authority_unavailable"] = "pull request number unavailable"
            continue
        try:
            records = approval_authority.github_review_records(
                repo,
                number,
                net_wrapper=net_wrapper,
                gh_cmd=gh_cmd,
                deadline=deadline,
            )
            if deadline is not None and deadline - time.monotonic() <= 0:
                raise approval_authority.GitHubDeadlineExceeded(
                    f"{repo}: time budget exhausted while reading canonical "
                    f"review authority for PR #{number}"
                )
            entry["_canonical_review_records"] = records
        except approval_authority.GitHubDeadlineExceeded as error:
            raise approval_authority.GitHubDeadlineExceeded(
                f"{repo}: time budget exhausted while reading canonical "
                f"review authority for PR #{number}: {error}"
            ) from error
        except approval_authority.ReviewPermissionUnavailable as error:
            entry["_review_permission_unavailable"] = str(error)
        except approval_authority.GitHubUnavailable as error:
            entry["_review_authority_unavailable"] = str(error)


def _mechanism_overlaps(raw: Sequence[object]) -> tuple[MechanismOverlap, ...]:
    """Group open PRs by exact ``mechanism:<slug>`` GitHub labels.

    The query already contains only open PRs. Drafts stay in this audit because
    a draft and a ready PR can still implement contradictory policies and later
    merge cleanly. This deliberately detects overlap only; intent remains a
    coordinator decision.
    """
    grouped: dict[str, list[MechanismPr]] = {}
    for entry in raw:
        if not isinstance(entry, dict):
            continue
        number = entry.get("number")
        pr = MechanismPr(
            pr=(
                number
                if isinstance(number, int) and not isinstance(number, bool)
                else None
            ),
            title=str(entry.get("title") or ""),
            draft=entry.get("isDraft") is True,
        )
        for label in _label_names(entry):
            if (
                label.startswith(_MECHANISM_TAG_PREFIX)
                and label != _MECHANISM_TAG_PREFIX
            ):
                grouped.setdefault(label, []).append(pr)

    overlaps: list[MechanismOverlap] = []
    for mechanism, prs in sorted(grouped.items()):
        if len(prs) < 2:
            continue
        ordered = tuple(
            sorted(prs, key=lambda item: (item.pr is None, item.pr or 0))
        )
        overlaps.append(MechanismOverlap(mechanism=mechanism, prs=ordered))
    return tuple(overlaps)


def _banked_status_credit(repo: str, sha: str, result: subprocess.CompletedProcess) -> bool:
    """Check the canonical reader's response identity, not its row predicate."""
    def refused(detail: str) -> RepoUnavailable:
        return RepoUnavailable(f"{repo}: local validation ledger unavailable: validate-status {sha}: {detail}")

    try:
        report = json.loads(result.stdout)
    except (TypeError, json.JSONDecodeError) as error:
        raise refused(f"malformed JSON (exit {result.returncode})") from error
    if not isinstance(report, dict) or type(report.get("schema_version")) is not int or report["schema_version"] != 3:
        raise refused("unsupported status report")
    if report.get("repo") != repo or report.get("sha") != sha:
        raise refused("status identity mismatch")
    exits = {"VALIDATED": 0, "FAILED": 3, "TRUNCATED": 4, "NEEDS-RERUN": 4,
             "NO-RESULT": 4, "NOT-VALIDATED": 4, "TERMINATED": 4}
    verdict = report.get("verdict")
    expected_exit = exits.get(verdict) if isinstance(verdict, str) else None
    if expected_exit is None:
        raise refused(f"unavailable or unknown verdict {verdict!r}")
    if type(report.get("exit_code")) is not int or report["exit_code"] != expected_exit or result.returncode != expected_exit:
        raise refused("command/report/verdict exits disagree")
    if "ledger_freshness" not in report:
        raise refused("status report omits ledger freshness")
    freshness = report["ledger_freshness"]
    if freshness is not None and (
        not isinstance(freshness, dict) or freshness.get("state") != "current"
        or freshness.get("local_absence_is_authoritative") is not True
    ):
        raise refused("local ledger absence is not authoritative")
    count, receipts = report.get("qualifying_count"), report.get("qualifying_receipts")
    newest, obligations = report.get("newest_qualifying"), report.get("unresolved_failure_obligations")
    if type(count) is not int or count < 0 or not isinstance(receipts, list) or count != len(receipts) or not isinstance(obligations, list):
        raise refused("incoherent receipt count or obligation list")
    for receipt in receipts:
        if not isinstance(receipt, dict) or any(receipt.get(k) != v for k, v in (("repo", repo), ("sha", sha), ("commit", sha))):
            raise refused("qualifying receipt identity mismatch")
        tree = receipt.get("tree")
        identity = receipt.get("receipt_identity")
        if not isinstance(tree, str) or re.fullmatch(r"[0-9a-f]{40}", tree) is None or not isinstance(identity, dict):
            raise refused("qualifying receipt has no tree or identity")
        digest, canonicalization = identity.get("digest"), identity.get("canonicalization")
        if identity.get("digest_algorithm") != "sha256" or not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None or canonicalization != "serde_json::to_vec(HistoryRow)-v1":
            raise refused("malformed qualifying receipt digest identity")
        binding = identity.get("tuple")
        if not isinstance(binding, dict) or any(binding.get(k) != receipt.get(k) for k in ("repo", "sha", "tree", "finished_at", "host", "slot", "log_file")):
            raise refused("qualifying receipt tuple disagrees")
        if any(not isinstance(receipt.get(k), str) or not receipt[k].strip() for k in ("finished_at", "host", "slot", "log_file")):
            raise refused("qualifying receipt tuple is incomplete")
    if count:
        if newest not in receipts or newest["finished_at"] != max(r["finished_at"] for r in receipts):
            raise refused("newest receipt is not a maximal member")
    elif newest is not None:
        raise refused("empty receipt list has a newest receipt")
    if verdict != "VALIDATED":
        # A genuine failure or unresolved obligation can override positive rows.
        return False
    if not count or obligations or type(report.get("failed_record_count")) is not int or report["failed_record_count"] != 0:
        raise refused("validated verdict contradicts its receipt/failure evidence")
    # Disqualified, withheld, and no-result history may coexist with VALIDATED.
    return True


def banked_green_commits(
    repo: str, ready_heads: Sequence[str], *, ci_hub_bin: Path = CI_HUB_BIN,
    timeout: float = 30.0,
) -> frozenset[str]:
    """Canonical local validation credit for the already-fetched ready heads.

    The timing/concurrency population from ledger qualified-rows is deliberately
    narrower in purpose. Only validate-status owns canonical receipt and
    same-commit failure/obligation precedence, including repository policy.
    One deadline covers all distinct heads. Unavailable is never an empty set.
    """
    if repo not in DEFAULT_REPOS or any(not isinstance(sha, str) or re.fullmatch(r"[0-9a-f]{40}", sha) is None for sha in ready_heads):
        raise RepoUnavailable(f"{repo}: local validation ledger unavailable: expected supported repo and full ready-head SHAs")
    heads = tuple(dict.fromkeys(ready_heads))
    if not heads:
        return frozenset()
    if not Path(ci_hub_bin).exists():
        raise RepoUnavailable(f"{repo}: local validation ledger unavailable: missing reader {ci_hub_bin}")
    deadline = time.monotonic() + max(0.0, timeout)
    commits: set[str] = set()
    for sha in heads:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RepoUnavailable(f"{repo}: local validation ledger unavailable: shared validate-status deadline exhausted")
        try:
            result = subprocess.run(
                [str(ci_hub_bin), "validate-status", "--sha", sha, "--repo", repo, "--json"],
                capture_output=True, text=True, check=False, timeout=remaining,
            )
        except subprocess.TimeoutExpired as error:
            raise RepoUnavailable(f"{repo}: local validation ledger unavailable: validate-status exceeded shared deadline") from error
        except OSError as error:
            raise RepoUnavailable(f"{repo}: local validation ledger unavailable: validate-status could not run: {error}") from error
        if _banked_status_credit(repo, sha, result):
            commits.add(sha)
        if time.monotonic() >= deadline:
            raise RepoUnavailable(f"{repo}: local validation ledger unavailable: shared validate-status deadline exhausted")
    return frozenset(commits)


# Strength order for a per-SHA failure tier: a genuine named-gate failure ("ok",
# i.e. a durable red) dominates an incomplete/unattributed run ("needs-rerun"),
# which dominates a no-evidence no-result. When a head has several fail rows, the
# STRONGEST wins — one complete failing run at a SHA makes it a real red even if
# other runs there had no named failing gate (a named-gate failure => ok).
_FAILURE_TIER_STRENGTH = {"no-result": 0, "needs-rerun": 1, "ok": 2}


def banked_failure_tier_commits(repo: str) -> dict[str, str]:
    """Map each ``repo`` head SHA that FAILED locally to its strongest failure tier.

    The symmetric peer of :func:`banked_green_commits`. That function dereferences
    the ledger's canonical GREEN verifier (``qualified-rows``); there is no
    equivalent binary command for reds, so we read the same machine-local ledger
    directly and apply the ONE shared named-gate predicate,
    :func:`failure_tier` (mirrored in the Rust ``failure_disposition``),
    to each fail/timeout row. The returned tier is ``"no-result"`` (no named
    failing gate and no failure count, or an environment fault), ``"needs-rerun"``
    (an incomplete gate contract, or a failure with no named gate), or ``"ok"`` (a
    genuine named-gate failure). Binding is by
    EXACT full commit SHA — an identity link, not a proxy.

    A successful read with no failure rows returns an empty map. An
    absent/unreadable ledger raises ``RepoUnavailable``, matching
    :func:`banked_green_commits`: neither side of the local authority may turn a
    reader failure into evidence that no exact-head record exists.

    LIMITATION (stated by design): the ledger is machine-local — most PR heads have
    NO local row here, so this can only demote a red where a fail/timeout row
    exists at the EXACT head SHA. A head red only on GH Actions keeps its GitHub
    verdict untouched.
    """
    name = repo.rsplit("/", 1)[-1]
    try:
        rows = canonical_ledger.rows(root=ROOT)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        raise RepoUnavailable(
            f"{repo}: local validation failure ledger unavailable: {error}"
        ) from error
    tiers: dict[str, str] = {}
    for row in rows:
        if not isinstance(row, dict) or row.get("result") not in ("fail", "timeout"):
            continue
        cwd = str(row.get("cwd") or "")
        row_repo = row.get("repo") or ("reverie" if "/reverie" in cwd else "hermit")
        if row_repo != name:
            continue
        sha = row.get("commit")
        if not isinstance(sha, str) or not sha:
            continue
        tier = failure_tier(row)
        prev = tiers.get(sha)
        if prev is None or _FAILURE_TIER_STRENGTH[tier] > _FAILURE_TIER_STRENGTH[prev]:
            tiers[sha] = tier
    return tiers


#: The exact-head hosted authority states ``ci-hub hosted-status`` can report.
#: ``unknown`` is OURS, not the verifier's: it marks a head whose authority we
#: did not manage to dereference (no binary, timeout, malformed answer). It is
#: deliberately NOT folded into ``no_result`` — "we did not look" and "we looked
#: and GitHub has no producer" are different facts, and only the second is an
#: observation. Neither ever contributes a green.
HOSTED_GREEN = "green"
HOSTED_RED = "red"
HOSTED_UNKNOWN = "unknown"
_HOSTED_VERIFIER_STATES = frozenset(
    {HOSTED_GREEN, HOSTED_RED, "pending", "running", "no_result"}
)
#: Concurrency for the per-head authority dereference. Each head costs ~2.4s
#: wall (measured 2026-08-08 on this host: two `actions/runs?head_sha=` +
#: `runs/<id>/jobs` round trips through the proxy, ~0.5s CPU), so 12 hermit
#: heads serially would add ~29s to a ~13s command. Four in flight keeps the
#: added wall under ~10s without fanning out hard enough to matter to GitHub.
DEFAULT_HOSTED_AUTHORITY_WORKERS = int(
    os.environ.get("CI_HUB_PR_STATUS_AUTHORITY_WORKERS", "4")
)
#: Per-head budget for one authority dereference. Generous against the measured
#: 2.4s so a slow proxy degrades ONE head to `unknown` instead of the report.
DEFAULT_HOSTED_AUTHORITY_TIMEOUT = float(
    os.environ.get("CI_HUB_PR_STATUS_AUTHORITY_TIMEOUT", "60")
)
#: THE VERIFIER, addressed at its Python entry point rather than through the
#: `ci-hub` front door. `ci-hub` is a `rust-script --force` shim whose ONLY job
#: for this subcommand is `run_python(root, "ci-hub/remediation/protocol.py",
#: ["hosted-status", ...])` -- it adds no semantics, but `--force` makes Cargo
#: re-check the crate on every invocation. Measured 2026-08-08 over hermit's 12
#: ready heads: front door 43.5s wall / 38.9s CPU for the whole command, direct
#: entry point 0.32s user per head. Same verifier, same public `hosted-status`
#: CLI, same JSON contract; ~30s of CPU per tick that bought nothing.
HOSTED_STATUS_VERIFIER = Path(
    os.environ.get("CI_HUB_HOSTED_STATUS_VERIFIER", ROOT / "ci-hub/remediation/protocol.py")
)


@dataclass(frozen=True)
class HostedAuthority:
    """One head's registered exact-head hosted job authority, as reported."""

    state: str
    positive_count: int
    required_positive_count: int
    failing_job_names: tuple[str, ...]
    reason: str = ""

    @property
    def observed(self) -> bool:
        return self.state != HOSTED_UNKNOWN


HOSTED_AUTHORITY_UNOBSERVED = HostedAuthority(HOSTED_UNKNOWN, 0, 0, (), "")


def hosted_authority_for_head(
    repo: str,
    sha: str,
    *,
    verifier: Path = HOSTED_STATUS_VERIFIER,
    timeout: float = DEFAULT_HOSTED_AUTHORITY_TIMEOUT,
) -> HostedAuthority:
    """Dereference the ``hosted-status`` authority for one exact head.

    THIS IS A CONSUMER, NOT A SECOND VERIFIER. It invokes the same
    ``hosted-status`` command ``ci-hub/landing/exact-head-validation-authority.sh``
    consumes and reads the verifier's own ``state``; it never re-derives
    green/red from conclusions, and this file contains no copy of the policy
    (which jobs are required, how many positives) that decides it. That is the
    whole repair: the previous code answered the authority question from
    ``statusCheckRollup``, an index that does not contain the authority job on
    any ``workflow_dispatch``-driven head (0 of 455 such check runs, measured
    2026-08-08).

    Fails to ``unknown`` in every ambiguous direction — missing verifier,
    timeout, non-JSON, an answer whose ``authority``/``repo``/``sha`` identity
    does not bind to what we asked, or a state outside the verifier's
    vocabulary. The exit code is deliberately NOT the signal (``hosted-status``
    exits 0/3/4 for green/red/other), so a nonzero exit with a well-formed red is
    still a red.
    """
    sha = (sha or "").strip().lower()
    if len(sha) != 40 or not re.fullmatch(r"[0-9a-f]{40}", sha):
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), "head SHA is not a lowercase 40-hex commit"
        )
    if not Path(verifier).exists():
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), f"hosted-status verifier is absent: {verifier}"
        )
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(verifier),
                "hosted-status",
                "--repo",
                repo,
                "--sha",
                sha,
                "--json",
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), f"hosted-status exceeded {timeout:.0f}s"
        )
    except OSError as error:
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), f"hosted-status could not run: {error}"
        )
    text = (result.stdout or "").strip()
    if not text:
        detail = (result.stderr or "").strip()[:200]
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), f"hosted-status printed nothing: {detail}"
        )
    try:
        report = json.loads(text)
    except json.JSONDecodeError:
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), "hosted-status returned non-JSON"
        )
    if not isinstance(report, dict):
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), "hosted-status returned a non-object"
        )
    # IDENTITY BIND. A report that does not name this authority, this repo and
    # this exact SHA describes something else; accepting it would let a stale or
    # misrouted answer authorize a head it never examined.
    if (
        report.get("authority") != "github-actions-exact-head-jobs"
        or report.get("repo") != repo
        or str(report.get("sha") or "").lower() != sha
    ):
        return HostedAuthority(
            HOSTED_UNKNOWN,
            0,
            0,
            (),
            "hosted-status answer does not bind to the requested authority/repo/sha",
        )
    state = str(report.get("state") or "")
    if state not in _HOSTED_VERIFIER_STATES:
        return HostedAuthority(
            HOSTED_UNKNOWN, 0, 0, (), f"hosted-status reported unknown state {state!r}"
        )
    jobs = report.get("jobs")
    failing = tuple(
        str(job.get("job_name") or "")
        for job in (jobs if isinstance(jobs, list) else [])
        if isinstance(job, dict) and job.get("state") == HOSTED_RED
    )
    positive = report.get("positive_count")
    required = report.get("required_positive_count")
    return HostedAuthority(
        state=state,
        positive_count=positive if isinstance(positive, int) else 0,
        required_positive_count=required if isinstance(required, int) else 0,
        failing_job_names=failing,
        reason=str(report.get("last_poll_error") or ""),
    )


def hosted_authority_states(
    repo: str,
    head_shas: Sequence[str],
    *,
    verifier: Path = HOSTED_STATUS_VERIFIER,
    workers: int = DEFAULT_HOSTED_AUTHORITY_WORKERS,
    deadline: float | None = None,
) -> dict[str, HostedAuthority]:
    """Dereference the hosted authority for every ready head, bounded.

    Heads left undone when ``deadline`` (a ``time.monotonic()`` instant) passes
    are simply absent from the map, which the classifier reads as ``unknown``.
    Partial coverage is reported, never silently completed: this tool must always
    produce a report, and a head we did not examine must never read as green.
    """
    wanted = [sha for sha in dict.fromkeys(head_shas) if sha]
    if not wanted:
        return {}

    def remaining() -> float | None:
        if deadline is None:
            return None
        return deadline - time.monotonic()

    results: dict[str, HostedAuthority] = {}

    def dereference(sha: str) -> tuple[str, HostedAuthority] | None:
        left = remaining()
        if left is not None and left <= 0:
            return None
        budget = DEFAULT_HOSTED_AUTHORITY_TIMEOUT
        if left is not None:
            budget = min(budget, left)
        return sha, hosted_authority_for_head(
            repo, sha, verifier=verifier, timeout=budget
        )

    from concurrent.futures import ThreadPoolExecutor

    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        for outcome in pool.map(dereference, wanted):
            if outcome is not None:
                results[outcome[0]] = outcome[1]
    return results


def _classify_gh_prs(
    repo: str,
    raw: list,
    banked_green: frozenset[str] = frozenset(),
    banked_failure: dict[str, str] | None = None,
    *,
    setup_only_verifier: SetupOnlyVerifier | None = None,
    hosted_authority: Mapping[str, HostedAuthority] | None = None,
    all_open_count: int | None = None,
    draft_count: int | None = None,
    population_known: bool = True,
    population_error: str = "",
    population_age_secs: float | None = None,
) -> RepoStatus:
    if banked_failure is None:
        banked_failure = {}
    if hosted_authority is None:
        hosted_authority = {}
    prs: list[dict[str, object]] = []
    all_open = (
        all_open_count
        if all_open_count is not None
        else sum(1 for entry in raw if isinstance(entry, dict))
    )
    drafts = (
        draft_count
        if draft_count is not None
        else sum(
            1
            for entry in raw
            if isinstance(entry, dict) and entry.get("isDraft") is True
        )
    )
    review_protocol: list[ReviewProtocolStatus] = []
    mechanism_overlaps = _mechanism_overlaps(raw)
    green = red = pending = real_reds = undetermined_reds = 0
    product_reds = gate_reds = 0
    green_local = 0
    ledger_no_result = ledger_needs_rerun = 0
    setup_only_no_result_checks = 0
    prerequisite_no_result_checks = 0
    hosted_green = hosted_red = hosted_unknown = 0
    for entry in raw:
        if not isinstance(entry, dict):
            continue
        review_status = _review_protocol_status(entry)
        if review_status is not None:
            review_protocol.append(review_status)
        if entry.get("isDraft") is True:
            continue
        number = entry.get("number")
        head_sha = str(entry.get("headRefOid") or "")
        rollup = _classify_rollup(
            repo,
            entry.get("statusCheckRollup"),
            head_sha=head_sha,
            setup_only_verifier=setup_only_verifier,
        )
        # TWO SOURCES, NAMED, NEITHER STANDING IN FOR THE OTHER.
        #   rollup    — every `pull_request`-triggered check (landing-gate and
        #               review meta-checks live here, faithfully).
        #   authority — the registered exact-head hosted job, from the ONE
        #               verifier. This is the only source that can see a
        #               `workflow_dispatch` run, i.e. everything merge-gate-v4
        #               drives.
        # Red dominates: a red from either source is a red. Green requires that
        # NEITHER is red and that at least one actually reported green, so an
        # unobserved authority can never manufacture one.
        authority = hosted_authority.get(head_sha, HOSTED_AUTHORITY_UNOBSERVED)
        if authority.state == HOSTED_GREEN:
            hosted_green += 1
        elif authority.state == HOSTED_RED:
            hosted_red += 1
        elif not authority.observed:
            hosted_unknown += 1
        if rollup.state == "red" or authority.state == HOSTED_RED:
            ci = "red"
        elif rollup.state == "green" or authority.state == HOSTED_GREEN:
            ci = "green"
        else:
            ci = "pending"
        # A red authority job is a PRODUCT test, and it must reach the
        # gate-vs-product split as one. Naming it here (rather than special-casing
        # below) keeps a single attribution path: the split reads failing check
        # names, and `Regular tests (GitHub-managed portable)` is not a gate
        # meta-check, so it classifies as product without a second rule.
        failing_checks = tuple(rollup.failing_check_names) + tuple(
            name for name in authority.failing_job_names if name.strip()
        )
        setup_only_no_result_checks += len(rollup.setup_only_no_result_checks)
        prerequisite_no_result_checks += len(rollup.prerequisite_no_result_checks)
        mergeable = str(entry.get("mergeable") or "").upper()
        merge_state = str(entry.get("mergeStateStatus") or "").upper()
        # Stale-base vs real: a CONFLICTING/DIRTY red is red because its merge
        # ref is unbuildable (rebase fixes it), NOT a genuine product failure.
        # BUT GitHub computes mergeability lazily, so a cold query returns
        # mergeable=UNKNOWN / merge_state=UNKNOWN; classifying those reds as
        # "real" would falsely spike real_reds/outage and flap between runs.
        # Treat unknown-base reds as their own "undetermined" class so the health
        # signal is stable and honest — a warm re-run resolves them.
        base_known = mergeable in ("MERGEABLE", "CONFLICTING") or merge_state not in (
            "",
            "UNKNOWN",
        )
        stale_base = mergeable == "CONFLICTING" or merge_state == "DIRTY"
        # Second authority: does the LOCAL validate ledger hold a full-green
        # receipt at THIS exact head SHA? A full-SHA match is an identity bind,
        # not a proxy. An authoritative local green overrides a stale/absent
        # GitHub check (e.g. the blanket self-hosted red) — the head IS validated,
        # so it is not a real red, and green=0 is a GitHub-display artifact.
        ledger_green = bool(head_sha) and head_sha in banked_green
        # Third authority (symmetric with ledger_green): the strongest LOCAL failure
        # tier at this exact head. "" when no local fail row exists.
        failure_tier = banked_failure.get(head_sha, "") if head_sha else ""
        red_class = ""
        real_red_kind = ""
        if ci == "red" and ledger_green:
            # GitHub says red, but LOCAL validate proved full green at this SHA.
            # The local receipt is authoritative and the merge gate honors it, so
            # this is NOT a real/gate/product red — it is a green whose GitHub
            # check is stale/self-hosted-blanket. Count it as green_local, keep it
            # OUT of every red bucket so real_reds (and thus unhealthy) stay honest.
            red += 1
            red_class = "ledger-green"
            green_local += 1
        elif ci == "red":
            red += 1
            if stale_base:
                red_class = "stale-base"
            elif not base_known:
                red_class = "undetermined"
                undetermined_reds += 1
            else:
                # Refine the real-red: gate-only (lacks receipt/review/current
                # pin) vs a genuine product break.
                fails = failing_checks
                # A failing check with no NAME identifies nothing, so it is not
                # evidence of anything -- least of all of a product break.
                named_fails = [name for name in fails if name.strip()]
                if named_fails and all(
                    _is_gate_meta_check(name) for name in named_fails
                ):
                    # A landing-gate/review meta-check red is a genuine blocker
                    # regardless of failure evidence (review is missing, receipt is
                    # missing) — a DIFFERENT authority than the validate ledger, so
                    # the ledger named-gate carve-out does NOT apply. Keep it a real red.
                    red_class = "real-red"
                    real_reds += 1
                    real_red_kind = "gate"
                    gate_reds += 1
                elif failure_tier == "no-result":
                    # A product-looking GitHub red whose EXACT head carries no named
                    # failing gate locally: a NO-RESULT wearing a red badge, not a
                    # corroborated break. Demote OUT of real_reds (merge-gate WAIT, not FAIL).
                    red_class = "ledger-no-result"
                    ledger_no_result += 1
                elif failure_tier == "needs-rerun":
                    # Product red whose only local run was incomplete, or recorded a
                    # failure with no named gate to attribute it to — not durable
                    # evidence of a break; re-run before condemning. Demoted out of real_reds.
                    red_class = "ledger-needs-rerun"
                    ledger_needs_rerun += 1
                elif not named_fails and failure_tier != "ok":
                    # ABSENCE OF EVIDENCE IS NEVER PRODUCT. The rollup named no
                    # failing check at this head AND the exact head has no
                    # corroborating named-gate ledger failure, so nothing
                    # identifies a product break. Still a real red -- the PR is
                    # red and must not be hidden by the split -- but attributed
                    # to the non-product bucket, because "product" is an
                    # instruction to go debug the PR's code and there is no
                    # evidence that would reward it.
                    red_class = "real-red"
                    real_reds += 1
                    real_red_kind = "gate"
                    gate_reds += 1
                else:
                    # A product break with EXACT-HEAD evidence: either a named
                    # failing check at head that is not gate/review/pin meta, or
                    # a full-suite local run at this exact SHA that genuinely
                    # failed a named gate (tier "ok").
                    red_class = "real-red"
                    real_reds += 1
                    real_red_kind = "product"
                    product_reds += 1
        elif ci == "green":
            green += 1
            if ledger_green:
                green_local += 1
        else:
            pending += 1
            if ledger_green:
                # Never dispatched on GitHub, but LOCAL validate banked a green at
                # this SHA — landable via the merge gate; not a no-result.
                green_local += 1
        prs.append(
            {
                "pr": number,
                "ci": ci,
                "red_class": red_class,
                "real_red_kind": real_red_kind,
                "ledger_green": ledger_green,
                "ledger_failure_tier": failure_tier,
                # The verifier's own word about the registered exact-head job,
                # reported beside the rollup rather than merged into it, so a
                # reader can see which source said what.
                "hosted_authority": authority.state,
                "hosted_positive": (
                    f"{authority.positive_count}/{authority.required_positive_count}"
                    if authority.observed
                    else ""
                ),
                "hosted_reason": authority.reason,
                "hosted_failing_jobs": authority.failing_job_names,
                "rollup_ci": rollup.state,
                "failing_checks": failing_checks,
                "setup_only_no_result_checks": (rollup.setup_only_no_result_checks),
                "setup_only_evidence": rollup.setup_only_evidence,
                "prerequisite_no_result_checks": (rollup.prerequisite_no_result_checks),
                "prerequisite_evidence": rollup.prerequisite_evidence,
                "actions_job_verification_errors": (
                    rollup.actions_job_verification_errors
                ),
                "mergeable": mergeable or "UNKNOWN",
                "merge_state": merge_state or "UNKNOWN",
                "title": entry.get("title", ""),
                # STALENESS IS THE BREACH, NOT COUNT. An open-PR total treats a
                # 3-hour-old PR and a 3-week-old one as the same row; the old
                # one is the violation. Do not explain that urgency by claiming
                # that main movement invalidates validation evidence. The
                # validation-authority rule is exact: "Exact-commit evidence does
                # not require the fleet to discard everything it learned whenever
                # `main` advances." Hard/soft-green landability is decided there.
                # `pre.reverie_pin` states the same rule for the Reverie pin: "A
                # lagging ancestor is legitimate; requiring the tip made the
                # verdict a property of WHEN you looked rather than of the tree."
                #
                # `None` when GitHub returned no createdAt: unknown age is
                # reported as unknown rather than silently sorted as brand new.
                "age_hours": _age_hours(entry.get("createdAt")),
                "created_at": entry.get("createdAt"),
            }
        )
    # OLDEST FIRST. The drain order is the report order; anything else buries the
    # breach under whatever happens to be newest. Unknown ages sort last rather
    # than first, so a missing timestamp can never masquerade as the oldest PR
    # and jump the queue.
    prs.sort(key=lambda row: (row.get("age_hours") is None, -(row.get("age_hours") or 0.0)))
    open_count = len(prs)
    # Outage heuristic: a large simultaneous *known* real-red fraction smells
    # like infra, not N independent product breaks. Built only from resolved
    # real reds so a cold mergeability query cannot trigger a false outage.
    outage = open_count >= 4 and real_reds >= max(3, (open_count + 1) // 2)
    return RepoStatus(
        repo=repo,
        open=open_count,
        green=green,
        red=red,
        pending=pending,
        real_reds=real_reds,
        undetermined_reds=undetermined_reds,
        product_reds=product_reds,
        gate_reds=gate_reds,
        green_local=green_local,
        ledger_no_result=ledger_no_result,
        ledger_needs_rerun=ledger_needs_rerun,
        setup_only_no_result_checks=setup_only_no_result_checks,
        prerequisite_no_result_checks=prerequisite_no_result_checks,
        hosted_green=hosted_green,
        hosted_red=hosted_red,
        hosted_unknown=hosted_unknown,
        outage_suspected=outage,
        prs=tuple(prs),
        all_open=all_open if population_known else None,
        drafts=drafts if population_known else None,
        population_error=population_error,
        population_age_secs=population_age_secs,
        review_protocol=tuple(review_protocol),
        mechanism_overlaps=mechanism_overlaps,
    )


def fetch_repo_status(
    repo: str,
    warn_threshold: int = DEFAULT_WARN_THRESHOLD,
    *,
    timeout: float | None = None,
    census_cache_dir: Path = DEFAULT_CENSUS_CACHE_DIR,
) -> RepoStatus:
    """Default single-repo query: the robust gh engine, self-proxied.

    Stable entry point for consumers (e.g. operational_health). ``warn_threshold``
    is accepted for signature compatibility but unused by the gh engine. Raises
    :class:`RepoUnavailable` (a ``RuntimeError``) on any blocked/failed query, so
    a caller catching ``RuntimeError`` still handles it and never mistakes a
    failure for zero open PRs.
    """
    return fetch_repo_status_gh(
        repo,
        net_wrapper=resolve_net_wrapper(None),
        timeout=timeout,
        census_cache_dir=census_cache_dir,
    )


def fetch_repo_status_gh(
    repo: str,
    *,
    net_wrapper: Sequence[str],
    gh_cmd: str = "gh",
    timeout: float | None = None,
    census_cache_dir: Path = DEFAULT_CENSUS_CACHE_DIR,
) -> RepoStatus:
    """Query open-PR health via one rollup call plus bounded exact-job lookups.

    Raises :class:`RepoUnavailable` on timeout, block, transient-exhaustion, or
    an unparseable/malformed response, so the caller records a partial result
    (UNAVAILABLE) instead of silently reporting zero open PRs.
    """
    started = time.monotonic()
    repo_deadline = (
        None if timeout is None else started + max(0.0, timeout)
    )

    def remaining() -> float | None:
        if repo_deadline is None:
            return None
        return max(0.0, repo_deadline - time.monotonic())

    # The expensive GraphQL shape carries statusCheckRollup and mergeability.
    # Drafts are not landing candidates, so ask GitHub for ready heads only.
    ready = _fetch_open_prs_gh(
        repo,
        fields=GH_FIELDS,
        net_wrapper=net_wrapper,
        gh_cmd=gh_cmd,
        timeout=remaining(),
        search="draft:false",
    )
    population_error = ""
    try:
        census, census_age = _cached_open_pr_census(
            repo,
            net_wrapper=net_wrapper,
            gh_cmd=gh_cmd,
            timeout=remaining(),
            cache_dir=census_cache_dir,
        )
    except RepoUnavailable as error:
        # The rare all-open census is not permission to throw away a successful
        # ready-head health read. Carry UNKNOWN population separately.
        census = []
        census_age = None
        population_error = str(error)
    # The rare census is deliberately lightweight and uses REST. Only its draft
    # rows are appended: ready rows above carry the richer exact-head fields.
    raw = ready + [entry for entry in census if entry.get("isDraft") is True]

    def population_only(reason: str) -> RepoStatus:
        """Keep the completed GitHub count while refusing unavailable health."""
        ready_count = sum(
            isinstance(entry, dict) and entry.get("isDraft") is not True
            for entry in raw
        )
        return _unavailable(
            repo,
            reason,
            _mechanism_overlaps(raw),
            open_count=ready_count,
            all_open=len(census) if not population_error else None,
            drafts=(
                sum(entry.get("isDraft") is True for entry in census)
                if not population_error
                else None
            ),
            population_error=population_error,
            population_age_secs=census_age,
        )

    authority_deadline = repo_deadline
    setup_only_authority = GitHubActionsJobAuthority(
        repo,
        net_wrapper=net_wrapper,
        gh_cmd=gh_cmd,
        deadline=authority_deadline,
    )
    # The registered exact-head hosted authority for every READY head. Drafts are
    # excluded here as well as in the classifier: they are not landing candidates
    # and are the bulk of the open set (126 of 138 on hermit, 2026-08-08), so
    # dereferencing them would multiply the cost by ~11 for no consumer.
    ready_heads = [
        str(entry.get("headRefOid") or "")
        for entry in raw
        if isinstance(entry, dict) and entry.get("isDraft") is not True
    ]
    ledger_timeout = remaining()
    if ledger_timeout is not None and ledger_timeout <= 0:
        return population_only(
            f"{repo}: time budget exhausted before local validation ledger"
        )
    try:
        banked_green = (
            banked_green_commits(repo, ready_heads)
            if ledger_timeout is None
            else banked_green_commits(repo, ready_heads, timeout=ledger_timeout)
        )
    except RepoUnavailable as error:
        # GitHub already answered the independent population question. Preserve
        # that evidence while refusing every health classification that depends
        # on the unavailable local authority.
        return population_only(str(error))
    if repo_deadline is not None and remaining() <= 0:
        return population_only(
            f"{repo}: time budget exhausted during local validation ledger"
        )
    try:
        banked_failure = banked_failure_tier_commits(repo)
    except RepoUnavailable as error:
        return population_only(str(error))
    if repo_deadline is not None and remaining() <= 0:
        return population_only(
            f"{repo}: time budget exhausted during local failure ledger"
        )
    try:
        _attach_canonical_review_authority(
            repo,
            raw,
            net_wrapper=net_wrapper,
            gh_cmd=gh_cmd,
            deadline=repo_deadline,
        )
    except approval_authority.GitHubDeadlineExceeded as error:
        return population_only(str(error))
    if repo_deadline is not None and remaining() <= 0:
        return population_only(
            f"{repo}: time budget exhausted during canonical review authority"
        )
    return _classify_gh_prs(
        repo,
        raw,
        banked_green,
        banked_failure,
        setup_only_verifier=setup_only_authority,
        hosted_authority=hosted_authority_states(
            repo, ready_heads, deadline=authority_deadline
        ),
        all_open_count=len(census),
        draft_count=sum(entry.get("isDraft") is True for entry in census),
        population_known=not population_error,
        population_error=population_error,
        population_age_secs=census_age,
    )


def _fetch_open_prs_gh(
    repo: str,
    *,
    fields: Sequence[str],
    net_wrapper: Sequence[str],
    gh_cmd: str = "gh",
    timeout: float | None = None,
    search: str | None = None,
) -> list[object]:
    """Fetch open PRs with a caller-selected bounded GraphQL field set.

    ``timeout`` is one wall-clock deadline for the complete operation,
    including retry backoff.  It is not a fresh subprocess allowance for each
    attempt.
    """
    command = [
        *net_wrapper,
        gh_cmd,
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        "open",
        "--limit",
        "500",
    ]
    if search:
        command.extend(("--search", search))
    command.extend(("--json", ",".join(fields)))
    deadline = None if timeout is None else time.monotonic() + max(0.0, timeout)
    result: subprocess.CompletedProcess[str] | None = None
    for attempt in range(1, MAX_FETCH_ATTEMPTS + 1):
        attempt_timeout = (
            None if deadline is None else max(0.0, deadline - time.monotonic())
        )
        if attempt_timeout is not None and attempt_timeout <= 0:
            raise RepoUnavailable(
                f"{repo}: time budget exhausted before gh attempt {attempt}"
            )
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                check=False,
                timeout=attempt_timeout,
            )
        except subprocess.TimeoutExpired:
            budget = (
                "unbounded"
                if attempt_timeout is None
                else f"{attempt_timeout:.0f}s remaining"
            )
            raise RepoUnavailable(
                f"{repo}: `gh pr list` exceeded {budget} (proxy stall or gh hang)"
            ) from None
        if result.returncode == 0:
            break
        detail = (result.stderr.strip() or result.stdout.strip() or "").strip()
        # BpfJailer FILE_OPEN denial is not retryable by re-running the same call.
        if "security policy" in detail or "bpfjailer" in detail.lower():
            raise RepoUnavailable(
                f"{repo}: gh/git blocked by BpfJailer security policy "
                f"(FILE_OPEN); ensure the call is proxied. detail: {detail[:400]}"
            )
        retryable = any(m.lower() in detail.lower() for m in _RETRYABLE_MARKERS)
        if not retryable or attempt == MAX_FETCH_ATTEMPTS:
            break
        backoff = float(attempt)
        if deadline is not None:
            backoff = min(backoff, max(0.0, deadline - time.monotonic()))
        if backoff > 0:
            time.sleep(backoff)
    assert result is not None
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "unknown error"
        raise RepoUnavailable(f"{repo}: `gh pr list` failed: {detail[:400]}")
    text = result.stdout.strip()
    if not text:
        # gh exited 0 with empty stdout — never treat as "0 open PRs"; that is the
        # blind-but-green failure mode this tool must not produce.
        raise RepoUnavailable(
            f"{repo}: `gh pr list` returned exit 0 but EMPTY output; refusing to "
            "report 0 open PRs from an empty response"
        )
    try:
        raw = json.loads(text)
    except json.JSONDecodeError as error:
        raise RepoUnavailable(f"{repo}: gh returned non-JSON: {text[:200]}") from error
    if not isinstance(raw, list):
        raise RepoUnavailable(f"{repo}: gh returned an unexpected schema (not a list)")
    return raw


def _census_cache_path(repo: str, cache_dir: Path) -> Path:
    return cache_dir / f"{repo.replace('/', '-')}.json"


def _read_census_cache(path: Path, *, now: float, ttl: float) -> tuple[list[dict], float] | None:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    fetched_at = payload.get("fetched_at") if isinstance(payload, dict) else None
    rows = payload.get("rows") if isinstance(payload, dict) else None
    if not isinstance(fetched_at, (int, float)) or not isinstance(rows, list):
        return None
    age = max(0.0, now - float(fetched_at))
    if age > ttl or not all(isinstance(row, dict) for row in rows):
        return None
    return rows, age


def _write_census_cache(path: Path, rows: list[dict], *, now: float) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(
        json.dumps({"fetched_at": now, "rows": rows}, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def _fetch_open_pr_census_rest(
    repo: str,
    *,
    net_wrapper: Sequence[str],
    gh_cmd: str,
    timeout: float | None,
) -> list[dict]:
    """Fetch a lightweight all-open census with REST, not GraphQL.

    ``--paginate --slurp`` is one process and at most ceil(open/100) core-REST
    requests. It carries no status rollups; those are reserved for ready heads.
    """
    try:
        result = subprocess.run(
            [
                *net_wrapper,
                gh_cmd,
                "api",
                "--paginate",
                "--slurp",
                f"repos/{repo}/pulls?state=open&per_page=100",
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        raise RepoUnavailable(f"{repo}: lightweight REST census timed out") from None
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "unknown error"
        raise RepoUnavailable(f"{repo}: lightweight REST census failed: {detail[:400]}")
    try:
        pages = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RepoUnavailable(f"{repo}: REST census returned non-JSON") from error
    if not isinstance(pages, list) or not all(isinstance(page, list) for page in pages):
        raise RepoUnavailable(f"{repo}: REST census returned an unexpected schema")
    rows: list[dict] = []
    for entry in (item for page in pages for item in page):
        if not isinstance(entry, dict):
            continue
        rows.append(
            {
                "number": entry.get("number"),
                "title": entry.get("title") or "",
                "isDraft": entry.get("draft") is True,
                "labels": entry.get("labels") if isinstance(entry.get("labels"), list) else [],
                "headRefOid": (
                    entry.get("head", {}).get("sha")
                    if isinstance(entry.get("head"), dict)
                    else ""
                ),
            }
        )
    return rows


def _cached_open_pr_census(
    repo: str,
    *,
    net_wrapper: Sequence[str],
    gh_cmd: str = "gh",
    timeout: float | None = None,
    cache_dir: Path = DEFAULT_CENSUS_CACHE_DIR,
    ttl: float = DEFAULT_CENSUS_TTL_SECS,
    now: float | None = None,
) -> tuple[list[dict], float]:
    moment = time.time() if now is None else now
    path = _census_cache_path(repo, cache_dir)
    cached = _read_census_cache(path, now=moment, ttl=ttl)
    if cached is not None:
        return cached
    rows = _fetch_open_pr_census_rest(
        repo, net_wrapper=net_wrapper, gh_cmd=gh_cmd, timeout=timeout
    )
    _write_census_cache(path, rows, now=moment)
    return rows, 0.0


def fetch_mechanism_overlaps_gh(
    repo: str,
    *,
    net_wrapper: Sequence[str],
    gh_cmd: str = "gh",
    timeout: float | None = None,
) -> tuple[MechanismOverlap, ...]:
    """Fetch only labels needed to preserve semantic warnings during CI 504s."""
    raw = _fetch_open_prs_gh(
        repo,
        fields=MECHANISM_GH_FIELDS,
        net_wrapper=net_wrapper,
        gh_cmd=gh_cmd,
        timeout=timeout,
    )
    return _mechanism_overlaps(raw)


# --------------------------------------------------------------------------- #
# planner engine (opt-in)
# --------------------------------------------------------------------------- #


def planner_command(repo: str, warn_threshold: int) -> list[str]:
    return [
        str(AGENT_TOOL),
        "pr-landing-planner",
        "status",
        "--repo",
        repo,
        "--base",
        "main",
        "--git-dir",
        str(_checkout_for(repo)),
        "--remote",
        "origin",
        "--net-wrapper",
        "with-proxy",
        "--gh-cmd",
        "gh",
        # Real merge-tree conflict detection, ON for planning runs (this is the
        # opt-in `--engine planner` path, NOT the every-tick `gh` default). It
        # was previously pinned to `file-overlap` on the theory that real
        # conflict analysis is an "expensive fan-out" — a conservative default
        # nobody had measured. Measured 2026-08-04 (denominator: 40 probes over
        # rrnewton/hermit's 107 open PRs): one `git merge-tree` base-conflict
        # probe = 36.5 ms local; the whole open set vs main ~4 s. The real cost
        # was never the analysis — it was a per-PR `git fetch` fan-out (21.5 s
        # over 25 hermit heads), which agent-utils PR #14 collapses to ONE
        # batched fetch (0.85 s, ~25x). So real conflict data over the full open
        # set costs seconds. A stated derivation cannot silently rot back into a
        # guess. `gh` stays the default for the every-tick path (no local fetch;
        # the only path that works under 3pai BpfJailer); planning runs opt in.
        "--conflict-detector",
        "merge-tree",
        "--gate-check",
        "merge-gate" if repo == "rrnewton/hermit" else "Merge Gate",
        "--format",
        "json",
        "--warn-threshold",
        str(warn_threshold),
    ]


def _parse_planner_payload(repo: str, stdout: str) -> RepoStatus:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{repo}: planner returned invalid JSON") from error
    if not isinstance(payload, dict) or not isinstance(payload.get("summary"), dict):
        raise RuntimeError(f"{repo}: planner returned an unexpected schema")
    summary = payload["summary"]
    prs = payload.get("prs")
    if not isinstance(prs, list):
        raise RuntimeError(f"{repo}: planner result has no PR list")

    def count(key: str) -> int:
        value = summary.get(key)
        return value if isinstance(value, int) and not isinstance(value, bool) else 0

    return RepoStatus(
        repo=repo,
        open=count("open"),
        green=count("green"),
        red=count("red"),
        pending=count("pending"),
        real_reds=count("real_reds"),
        outage_suspected=summary.get("outage_suspected") is True,
        prs=tuple(pr for pr in prs if isinstance(pr, dict)),
    )


def fetch_repo_status_planner(
    repo: str,
    warn_threshold: int = DEFAULT_WARN_THRESHOLD,
    *,
    timeout: float | None = None,
) -> RepoStatus:
    """Query one repo's PR health via the planner, bounded by ``timeout`` seconds.

    Raises :class:`RepoUnavailable` if the (possibly-retried) planner cannot
    complete within the budget, so the caller can record a partial result and
    keep going instead of hanging.
    """
    command = planner_command(repo, warn_threshold)
    result: subprocess.CompletedProcess[str] | None = None
    for attempt in range(1, MAX_FETCH_ATTEMPTS + 1):
        if timeout is not None and timeout <= 0:
            raise RepoUnavailable(
                f"{repo}: time budget exhausted before planner attempt {attempt}"
            )
        started = time.monotonic()
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                check=False,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            # subprocess.run has already SIGKILLed the direct planner child; a
            # single in-flight `git fetch` grandchild (if any) exits on its own.
            budget = "unbounded" if timeout is None else f"{timeout:.0f}s"
            raise RepoUnavailable(
                f"{repo}: planner exceeded {budget} "
                f"(per-PR proxied git fetch fan-out; ~1.35s/PR measured)"
            ) from None
        if result.returncode == 0:
            break
        detail = result.stderr.strip() or result.stdout.strip()
        if "security policy" in detail or "bpfjailer" in detail.lower():
            raise RepoUnavailable(
                f"{repo}: planner git fetch blocked by BpfJailer (FILE_OPEN); "
                f"use --engine gh on 3pai hosts. detail: {detail[:300]}"
            )
        retryable = "504" in detail or "changed during collection" in detail
        if not retryable or attempt == MAX_FETCH_ATTEMPTS:
            break
        # Bounded backoff, but never sleep past the remaining budget.
        backoff = float(attempt)
        if timeout is not None:
            elapsed = time.monotonic() - started
            timeout = max(0.0, timeout - elapsed)
            backoff = min(backoff, timeout)
        time.sleep(backoff)
    assert result is not None
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "unknown error"
        raise RuntimeError(f"{repo}: agent-utils pr-landing-planner failed: {detail}")
    return _parse_planner_payload(repo, result.stdout)


# --------------------------------------------------------------------------- #
# orchestration
# --------------------------------------------------------------------------- #


def collect_statuses(
    repos: Sequence[str],
    warn_threshold: int,
    *,
    engine: str,
    net_wrapper: Sequence[str],
    per_repo_timeout: float,
    overall_deadline: float,
    gh_cmd: str = "gh",
) -> list[RepoStatus]:
    """Query every repo, always returning one status each (partial on failure)."""
    deadline = time.monotonic() + overall_deadline
    statuses: list[RepoStatus] = []
    for repo in repos:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            statuses.append(
                _unavailable(
                    repo,
                    f"overall deadline {overall_deadline:.0f}s exhausted "
                    f"before querying {repo}",
                )
            )
            continue
        budget = min(per_repo_timeout, remaining)
        try:
            if engine == "planner":
                statuses.append(
                    fetch_repo_status_planner(repo, warn_threshold, timeout=budget)
                )
            else:
                statuses.append(
                    fetch_repo_status_gh(
                        repo,
                        net_wrapper=net_wrapper,
                        gh_cmd=gh_cmd,
                        timeout=budget,
                    )
                )
        except RepoUnavailable as unavailable:
            reason = str(unavailable)
            mechanism_overlaps: tuple[MechanismOverlap, ...] = ()
            if engine == "gh":
                remaining = deadline - time.monotonic()
                if remaining > 0:
                    try:
                        mechanism_overlaps = fetch_mechanism_overlaps_gh(
                            repo,
                            net_wrapper=net_wrapper,
                            gh_cmd=gh_cmd,
                            timeout=min(60.0, remaining),
                        )
                    except RepoUnavailable as fallback_error:
                        reason += f"; mechanism-label fallback failed: {fallback_error}"
            statuses.append(_unavailable(repo, reason, mechanism_overlaps))
        except RuntimeError as error:
            # A hard planner/schema error still yields a report line rather than
            # aborting the whole command with nothing printed.
            statuses.append(_unavailable(repo, f"query failed: {error}"))
    return statuses


def _render_mechanism_overlaps(
    lines: list[str], overlaps: Sequence[MechanismOverlap]
) -> None:
    if not overlaps:
        return
    lines.append(
        "    Mechanism overlaps: "
        f"{len(overlaps)} coordinator review required "
        "(shared tag exposes overlap; it does not prove conflicting intent)"
    )
    for overlap in overlaps:
        lines.append(f"      {overlap.mechanism}")
        for pr in overlap.prs:
            draft = " draft=yes" if pr.draft else ""
            lines.append(
                f"        #{pr.pr if pr.pr is not None else '?':<5}"
                f"{draft} {pr.title}"
            )


def health_verdict(statuses: Sequence[RepoStatus]) -> dict[str, object]:
    """Explain the top-level banner from the exact inputs that determine it."""
    unhealthy = any(status.unhealthy for status in statuses)
    unavailable = [status for status in statuses if not status.available]
    state = "unhealthy" if unhealthy else ("degraded" if unavailable else "healthy")
    inputs: list[dict[str, object]] = []
    for status in statuses:
        stale_base_reds = sum(
            pr.get("red_class") == "stale-base" for pr in status.prs
        )
        inputs.append(
            {
                "repo": status.repo,
                "available": status.available,
                "ready_prs": status.open,
                "green": status.green,
                "green_local": status.green_local,
                "real_reds": status.real_reds,
                "product_reds": status.product_reds,
                "gate_reds": status.gate_reds,
                "stale_base_reds": stale_base_reds,
                "undetermined_reds": status.undetermined_reds,
                "ledger_no_result": status.ledger_no_result,
                "ledger_needs_rerun": status.ledger_needs_rerun,
                "setup_only_no_result_checks": (status.setup_only_no_result_checks),
                "prerequisite_no_result_checks": (status.prerequisite_no_result_checks),
                "hosted_green": status.hosted_green,
                "hosted_red": status.hosted_red,
                "hosted_unknown": status.hosted_unknown,
                "no_result": status.pending,
                "outage_suspected": status.outage_suspected,
                "triggers_unhealthy": status.unhealthy,
                "unavailable_reason": status.reason if not status.available else "",
            }
        )
    return {
        "state": state,
        "rule": HEALTH_VERDICT_RULE,
        "inputs": inputs,
        "not_inputs": list(HEALTH_VERDICT_EXCLUDES),
    }


def render_report(statuses: Sequence[RepoStatus], warn_threshold: int, engine: str) -> str:
    # Ready-head population is independently known for a population-only result;
    # health availability controls verdicts, not whether that count exists.
    total = sum(status.open for status in statuses)
    unavailable = [status for status in statuses if not status.available]
    verdict = health_verdict(statuses)
    if verdict["state"] == "unhealthy":
        heading = "CI health: UNHEALTHY"
    elif unavailable:
        heading = (
            f"CI health: DEGRADED (partial: {len(unavailable)} of "
            f"{len(statuses)} repos unavailable)"
        )
    else:
        heading = "CI health: HEALTHY"
    source = {
        "gh": (
            "gh pr list --json statusCheckRollup (pull_request-triggered checks "
            "ONLY -- it carries no workflow_dispatch check run, so it cannot see "
            "merge-gate-v4-driven product CI) + `ci-hub hosted-status` per ready "
            "head for the registered exact-head job authority + bounded exact "
            "failed-Action-job lookup (labels-only fallback after a full-query "
            "failure)"
        ),
        "planner": "pinned agent-utils/pr-landing-planner status (per-PR git fetch)",
    }.get(engine, engine)
    lines = [
        heading,
        f"Source: {source}",
        f"Verdict rule: {HEALTH_VERDICT_RULE}",
        "Verdict does not read: " + ", ".join(HEALTH_VERDICT_EXCLUDES),
        "Verdict inputs:",
    ]
    for item in verdict["inputs"]:
        assert isinstance(item, dict)
        lines.append(
            "  {repo}: available={available} ready={ready_prs} green={green} "
            "green_local={green_local} "
            "real_reds={real_reds} (product={product_reds} gate={gate_reds}) "
            "stale_base_reds={stale_base_reds} "
            "ledger_no_result={ledger_no_result} "
            "ledger_needs_rerun={ledger_needs_rerun} "
            "setup_only_no_result_checks={setup_only_no_result_checks} "
            "prerequisite_no_result_checks={prerequisite_no_result_checks} "
            "hosted_authority=green:{hosted_green}/red:{hosted_red}/"
            "unobserved:{hosted_unknown} "
            "undetermined_reds={undetermined_reds} no_result={no_result} "
            "outage={outage_suspected} triggers_unhealthy={triggers_unhealthy}".format(
                **item
            )
        )
    # Make the banner actionable: say WHETHER any product test is actually broken,
    # so a gate/review-only UNHEALTHY is not mistaken for product breakage (and,
    # conversely, a real product red is pointed at directly). Only over available
    # repos — an unavailable repo's product state is unknown, not zero.
    if verdict["state"] == "unhealthy":
        available_statuses = [s for s in statuses if s.available]
        total_product = sum(s.product_reds for s in available_statuses)
        if total_product == 0:
            lines.append(
                "  Actionability: 0 product-test reds on any ready PR head — "
                "UNHEALTHY is driven entirely by landing-gate reds (PRs "
                "lacking a valid receipt, a completed review, or a current "
                "dependency pin at head), not product breakage. Fix by "
                "producing receipts, completing review, or landing the shared "
                "pin bump — not by debugging tests. A pin-freshness red "
                "compares THIS HEAD's pin to the dependency's live main, so "
                "heads of different ages carry different stale pins: bumping "
                "main clears a head only after that head is REBASED onto it. "
                "Check each head's pin before assuming one bump suffices."
            )
        else:
            hot = ", ".join(
                f"{s.repo}={s.product_reds}"
                for s in available_statuses
                if s.product_reds
            )
            lines.append(
                f"  Actionability: {total_product} product-test red(s) — a genuine "
                f"break at a ready PR head: {hot}. Remaining real_reds are "
                "landing-gate/review only."
            )
    for status in statuses:
        if not status.available:
            population = ""
            if status.all_open is not None:
                population = (
                    f"; population all_open={status.all_open} "
                    f"ready_non_draft={status.open} drafts={status.drafts}"
                )
            lines.append(
                f"  {status.repo}: UNAVAILABLE — {status.reason}{population}"
            )
            _render_mechanism_overlaps(lines, status.mechanism_overlaps)
            continue
        undet = (
            f" undetermined_reds={status.undetermined_reds}"
            if status.undetermined_reds
            else ""
        )
        # Show the product/gate split only when it fully accounts for real_reds
        # (the gh engine classifies every real-red; the planner does not).
        split = ""
        if status.real_reds and status.product_reds + status.gate_reds == status.real_reds:
            split = f" (product={status.product_reds} gate={status.gate_reds})"
        local = (
            f" green_local={status.green_local}" if status.green_local else ""
        )
        demoted = ""
        if status.ledger_no_result or status.ledger_needs_rerun:
            demoted = (
                f" ledger_no_result={status.ledger_no_result}"
                f" ledger_needs_rerun={status.ledger_needs_rerun}"
            )
        lines.append(
            f"  {status.repo}: open={status.open} green={status.green}{local} "
            f"red={status.red} pending={status.pending} real_reds={status.real_reds}"
            f"{split}{demoted}{undet} outage={'yes' if status.outage_suspected else 'no'}"
        )
        if status.open:
            lines.append(
                f"    hosted authority (ci-hub hosted-status, the same verifier "
                f"landing uses): green={status.hosted_green} "
                f"red={status.hosted_red} unobserved={status.hosted_unknown} of "
                f"{status.open} ready head(s)"
            )
        if status.hosted_unknown:
            lines.append(
                f"    CAUTION: {status.hosted_unknown} ready head(s) had NO hosted "
                "authority answer this run (binary absent, budget exhausted, or a "
                "malformed/unbound reply). Those heads are reported from the "
                "rollup alone, which cannot see workflow_dispatch check runs — "
                "treat them as UNKNOWN, not green (see hosted_authority and "
                "hosted_reason per PR)."
            )
        if status.ledger_no_result or status.ledger_needs_rerun:
            lines.append(
                f"    ledger: {status.ledger_no_result} red PR head(s) had no named "
                f"failing gate locally (NO-RESULT) and {status.ledger_needs_rerun} "
                "were incomplete/unattributed (NEEDS-RERUN) at the exact head — demoted "
                "OUT of real_reds; the GitHub product red is uncorroborated by any "
                "complete local run (named-gate carve-out, see ledger_failure_tier flag)"
            )
        if status.setup_only_no_result_checks:
            lines.append(
                f"    actions jobs: {status.setup_only_no_result_checks} failed "
                "check attempt(s) dereferenced to an exact completed job whose "
                "only step was failed `Set up job`; classified NO_RESULT, never "
                "green (see setup_only_no_result_checks and setup_only_evidence)"
            )
        if status.prerequisite_no_result_checks:
            lines.append(
                f"    actions jobs: {status.prerequisite_no_result_checks} "
                "registered merge-gate-v4 consequence(s) were bound to an exact "
                "same-run setup-only Reverie-pin prerequisite; classified "
                "NO_RESULT, never green (see prerequisite_no_result_checks and "
                "prerequisite_evidence)"
            )
        if status.green_local:
            lines.append(
                f"    ledger: {status.green_local} open head(s) carry an "
                "authoritative LOCAL full-green receipt (landable via merge gate). "
                "A hosted red/pending at those heads is a different authority's "
                "answer, not absence of validated work (see ledger_green flag)"
            )
        if status.undetermined_reds:
            lines.append(
                f"    CAUTION: {status.undetermined_reds} red PR(s) have mergeability "
                "still computing (base state UNDETERMINED this query); real-red vs "
                "stale-base unresolved — re-run to classify."
            )
        for pr in status.prs:
            klass = pr.get("red_class") or "-"
            kind = pr.get("real_red_kind")
            if kind:
                klass = f"{klass}:{kind}"
            hosted = str(pr.get("hosted_authority") or HOSTED_UNKNOWN)
            positive = str(pr.get("hosted_positive") or "")
            hosted_cell = f"{hosted}({positive})" if positive else hosted
            lines.append(
                f"    #{pr.get('pr', '?'):<5} ci={pr.get('ci', 'unknown'):<7} "
                f"rollup={str(pr.get('rollup_ci') or '-'):<7} "
                f"hosted={hosted_cell:<16} "
                f"class={klass:<23} {pr.get('title', '')}"
            )
            hosted_reason = pr.get("hosted_reason")
            if hosted == HOSTED_UNKNOWN and hosted_reason:
                lines.append(f"      hosted authority unobserved: {hosted_reason}")
            setup_only = pr.get("setup_only_no_result_checks")
            if setup_only:
                lines.append(
                    "      setup-only NO_RESULT: "
                    + ", ".join(str(v) for v in setup_only)
                )
            prerequisite = pr.get("prerequisite_no_result_checks")
            if prerequisite:
                lines.append(
                    "      prerequisite consequence NO_RESULT: "
                    + ", ".join(str(v) for v in prerequisite)
                )
            verification_errors = pr.get("actions_job_verification_errors")
            if verification_errors:
                lines.append(
                    "      exact-job verification retained red: "
                    + " | ".join(str(v) for v in verification_errors)
                )
        _render_mechanism_overlaps(lines, status.mechanism_overlaps)
        if status.review_protocol:
            audits = status.review_protocol
            complete = sum(audit.complete for audit in audits)
            review_complete = sum(
                audit.review_rounds == "complete" for audit in audits
            )
            partial_review = sum(audit.review_rounds == "partial" for audit in audits)
            no_review = sum(audit.review_rounds == "missing" for audit in audits)
            approval_complete = sum(
                audit.current_approvals == "complete" for audit in audits
            )
            invalid = sum(bool(audit.invalid_labels) for audit in audits)
            permission_unavailable = sum(
                bool(audit.review_permission_unavailable) for audit in audits
            )
            lines.append(
                "    Review protocol source: canonical issue comments, inline review "
                "comments, native reviews, and current GitHub actor permission from "
                "the shared review authority; public-only reporting, not merge admission. "
                "Explicit local review packets are not discovered by this view."
            )
            lines.append(
                "    Review protocol: "
                f"labeled={len(audits)} complete={complete} "
                f"review_complete={review_complete} partial_review={partial_review} "
                f"no_review_evidence={no_review} "
                f"current_approval_complete={approval_complete} invalid_label_prs={invalid} "
                f"review_permission_unavailable={permission_unavailable}"
            )
            for audit in audits:
                invalid_detail = (
                    f" invalid={','.join(audit.invalid_labels)}"
                    if audit.invalid_labels
                    else ""
                )
                draft = " draft=yes" if audit.draft else ""
                permission_detail = (
                    " review_permission_unavailable="
                    + ",".join(audit.review_permission_unavailable)
                    if audit.review_permission_unavailable
                    else ""
                )
                lines.append(
                    f"      #{audit.pr if audit.pr is not None else '?':<5} "
                    f"critical={'yes' if audit.critical_required else 'no'} "
                    f"review={audit.review_rounds:<8} "
                    f"approval={audit.current_approvals:<8} "
                    f"missing={','.join(audit.missing) or '-'}"
                    f"{invalid_detail}{permission_detail}{draft} {audit.title}"
                )
    if unavailable:
        # DO NOT NAME A CAUSE THIS LINE DOES NOT KNOW. It previously asserted
        # "could not be queried within the time budget" for EVERY unavailable
        # repo, whatever actually happened. Measured 2026-08-06/07: the real
        # failure was gh exiting non-zero with "unexpected end of JSON input"
        # against a 300s budget on a 5.0s query -- nowhere near binding. Anyone
        # acting on the printed message would have raised
        # CI_HUB_PR_STATUS_TIMEOUT and seen no change, because the timeout was
        # never the constraint. The per-repo reason is already reported verbatim
        # above; this line points at it instead of inventing one.
        reasons = ", ".join(sorted(s.repo for s in unavailable))
        lines.append(
            f"PARTIAL RESULT: {len(unavailable)} of {len(statuses)} repo(s) "
            f"could not be queried ({reasons}); see the UNAVAILABLE line above "
            "for each repo's exact reason. Open-PR totals above cover only the "
            "repos that responded (NOT a claim that they have no PRs)."
        )
    if total > warn_threshold:
        lines.append(
            f"WARNING: {total} open PRs exceeds the {warn_threshold} PR threshold."
        )
    return "\n".join(lines)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--repo",
        dest="repos",
        action="append",
        help="GitHub OWNER/REPO to query; repeat to override defaults",
    )
    parser.add_argument(
        "--engine",
        choices=("gh", "planner"),
        default=os.environ.get("CI_HUB_PR_STATUS_ENGINE", "gh"),
        help=(
            "status backend: 'gh' (default; one proxied rollup call, canonical "
            "review reads for labeled post-facto PRs, and bounded failed-job "
            "lookups; no per-PR git fetch) or 'planner' (agent-utils, real merge-tree "
            "conflict detection over the open set; use on planning runs)"
        ),
    )
    parser.add_argument(
        "--net-wrapper",
        dest="net_wrapper",
        default=None,
        help=(
            "command prefix for gh (gh engine); default 'with-proxy' if on PATH "
            "(idempotent). Pass '' to disable."
        ),
    )
    parser.add_argument(
        "--gh-cmd",
        default="gh",
        help="GitHub CLI executable used by the gh engine (default: gh)",
    )
    parser.add_argument("--warn-threshold", type=int, default=DEFAULT_WARN_THRESHOLD)
    parser.add_argument(
        "--per-repo-timeout",
        type=float,
        default=DEFAULT_PER_REPO_TIMEOUT,
        help=(
            "seconds to allow one repo's query before recording it "
            f"unavailable (default: {DEFAULT_PER_REPO_TIMEOUT:.0f}; "
            "env CI_HUB_PR_STATUS_TIMEOUT)"
        ),
    )
    parser.add_argument(
        "--overall-deadline",
        type=float,
        default=DEFAULT_OVERALL_DEADLINE,
        help=(
            "total seconds across all repos before remaining repos are marked "
            f"unavailable (default: {DEFAULT_OVERALL_DEADLINE:.0f}; "
            "env CI_HUB_PR_STATUS_DEADLINE)"
        ),
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.warn_threshold < 0:
        parser.error("--warn-threshold must be non-negative")
    if args.per_repo_timeout <= 0:
        parser.error("--per-repo-timeout must be positive")
    if args.overall_deadline <= 0:
        parser.error("--overall-deadline must be positive")
    if not args.gh_cmd.strip():
        parser.error("--gh-cmd must not be empty")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    repos = tuple(args.repos) if args.repos else DEFAULT_REPOS
    try:
        net_wrapper = resolve_net_wrapper(args.net_wrapper)
    except RuntimeError as error:
        # Configuration error: fail loudly, never silently issue a bare call.
        print(f"CI health: ERROR — {error}", file=sys.stderr)
        return 3
    statuses = collect_statuses(
        repos,
        args.warn_threshold,
        engine=args.engine,
        net_wrapper=net_wrapper,
        gh_cmd=args.gh_cmd,
        per_repo_timeout=args.per_repo_timeout,
        overall_deadline=args.overall_deadline,
    )
    if args.json:
        print(
            json.dumps(
                {
                    "engine": args.engine,
                    "verdict": health_verdict(statuses),
                    "repos": [asdict(s) for s in statuses],
                },
                sort_keys=True,
            )
        )
    else:
        print(render_report(statuses, args.warn_threshold, args.engine))
    if any(status.unhealthy for status in statuses):
        return 1
    if any(not status.available for status in statuses):
        # Degraded/partial: cannot fully verify PR health, but we terminated
        # with a report. Non-zero mirrors github_main_health's "cannot verify".
        return 2
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("CI health: ERROR — interrupted", file=sys.stderr)
        raise SystemExit(130)
