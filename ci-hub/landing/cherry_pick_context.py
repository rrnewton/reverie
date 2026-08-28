#!/usr/bin/env python3
"""Require cherry-picked units to be disclosed and reviewed in their new PR context.

An identical patch is not identical review evidence after a cherry-pick: the PR
base, neighbouring commits, and stated intent are inputs to semantic review.
This verifier therefore treats an explicit ``(cherry picked from commit ...)``
trailer as provenance, not as inherited approval.

For every such source commit, the PR must carry both:

* in its own ``## Summary`` section (related sources may share one line)::

      Cherry-pick-context: <source-40-hex>[, <source-40-hex>...] - <behavior carried into this PR>

* in every exact-head approval lane relied on by the PR, in the SAME review
  comment as that lane's ``APPROVED-AT`` binding.  The digest binds the reviewer
  to the complete source-to-semantics map, so later PR-body edits cannot inherit
  the old review::

      CHERRY-PICK-CONTEXT-REVIEWED: <lane> <context-sha256> <head-40-hex>

The check says nothing about whether the code is good or bad.  A load-bearing,
correct cherry-pick passes once its behavior is disclosed and reviewed in the
new context.  Missing evidence refuses landing rather than condemning the diff.

Live usage (the final lander calls this)::

    cherry_pick_context.py --repo rrnewton/hermit --pr 2095 --gate

Offline fixtures drive the two-sided bracket::

    cherry_pick_context.py --from-json fixture.json --gate
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


HEALTH = Path(__file__).resolve().parents[1] / "health"
sys.path.insert(0, str(HEALTH))
import approval_binding  # noqa: E402


SHA40 = r"[0-9a-f]{40}"
CHERRY_PICK_RE = re.compile(
    rf"^\s*\(cherry picked from commit (?P<source>{SHA40})\)\s*$",
    re.IGNORECASE | re.MULTILINE,
)
CHERRY_PICK_ATTEMPT_RE = re.compile(
    r"cherry\s+picked\s+from\s+commit", re.IGNORECASE
)
DISCLOSURE_RE = re.compile(
    rf"^\s*(?:[-*]\s+)?Cherry-pick-context:\s*"
    rf"(?P<sources>{SHA40}(?:\s*,\s*{SHA40})*)"
    rf"\s*(?:-|—|:)\s*(?P<detail>\S.*)\s*$",
    re.IGNORECASE,
)
# Approval syntax belongs to `approval_binding`. This gate owns the additional
# rule that the context digest must share a comment with the exact-head
# approval, but it must not independently decide what an approval line says.
CONTEXT_REVIEW_RE = re.compile(
    rf"^\s*CHERRY-PICK-CONTEXT-REVIEWED:\s*(?P<lane>claude|codex)\s+"
    rf"(?P<context>[0-9a-f]{{64}})\s+(?P<head>{SHA40})\s*$",
    re.IGNORECASE,
)
REVIEW_LABEL_RE = re.compile(r"^passed-review-(claude|codex)$")


class Unverifiable(RuntimeError):
    """The observation could not establish a trustworthy verdict."""


@dataclass(frozen=True)
class UnitStatus:
    source: str
    carried_as: tuple[str, ...]
    disclosed: bool
    disclosure_detail: str | None
    reviewed_lanes: tuple[str, ...]
    missing_review_lanes: tuple[str, ...]


@dataclass(frozen=True)
class Verdict:
    pr: int | None
    head: str
    approval_lanes: tuple[str, ...]
    context_sha256: str | None
    units: tuple[UnitStatus, ...]

    @property
    def passes(self) -> bool:
        return all(
            unit.disclosed and not unit.missing_review_lanes for unit in self.units
        )


def _undecorate(line: str) -> str:
    """Allow whole-line Markdown around a binding, but never embedded prose."""
    line = line.strip()
    while True:
        for wrapper in ("`", "**", "__", "*", "_"):
            if (
                line.startswith(wrapper)
                and line.endswith(wrapper)
                and len(line) > 2 * len(wrapper)
            ):
                line = line[len(wrapper) : -len(wrapper)].strip()
                break
        else:
            return line


def _summary_lines(body: str) -> list[str]:
    """Return only the PR's Summary section; a hash elsewhere is not disclosure."""
    lines = body.splitlines()
    start: int | None = None
    for index, line in enumerate(lines):
        if re.match(r"^\s*##\s+Summary\s*$", line, re.IGNORECASE):
            start = index + 1
            break
    if start is None:
        return []
    end = len(lines)
    for index in range(start, len(lines)):
        if re.match(r"^\s*##\s+", lines[index]):
            end = index
            break
    return lines[start:end]


def _commit_parts(commit: dict[str, Any]) -> tuple[str, str]:
    sha = str(commit.get("oid") or commit.get("sha") or "").lower()
    if not re.fullmatch(SHA40, sha):
        raise Unverifiable(f"commit has missing or malformed oid: {sha or '<absent>'}")
    message = "\n".join(
        part
        for part in (
            str(commit.get("messageHeadline") or ""),
            str(commit.get("messageBody") or commit.get("message") or ""),
        )
        if part
    )
    return sha, message


def context_digest(sources: dict[str, list[str]], disclosures: dict[str, str]) -> str:
    """Digest the complete semantic map; grouping/line order is not authority."""
    document = [
        {"source": source, "detail": disclosures.get(source)}
        for source in sorted(sources)
    ]
    encoded = json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def classify(payload: dict[str, Any], *, expected_head: str | None = None) -> Verdict:
    head = str(payload.get("headRefOid") or payload.get("head") or "").lower()
    if not re.fullmatch(SHA40, head):
        raise Unverifiable(f"PR head is missing or malformed: {head or '<absent>'}")
    if expected_head is not None and head != expected_head.lower():
        raise Unverifiable(
            f"PR head moved: expected {expected_head.lower()}, observed {head}"
        )

    sources: dict[str, list[str]] = {}
    for commit in payload.get("commits") or []:
        carried_as, message = _commit_parts(commit)
        matches = [m.group("source").lower() for m in CHERRY_PICK_RE.finditer(message)]
        attempted = len(CHERRY_PICK_ATTEMPT_RE.findall(message))
        if attempted != len(matches):
            raise Unverifiable(
                f"commit {carried_as} has malformed cherry-pick provenance"
            )
        for source in matches:
            sources.setdefault(source, []).append(carried_as)

    disclosures: dict[str, str] = {}
    for line in _summary_lines(str(payload.get("body") or "")):
        match = DISCLOSURE_RE.match(_undecorate(line))
        if match:
            detail = match.group("detail").strip()
            for source in re.findall(SHA40, match.group("sources"), re.IGNORECASE):
                source = source.lower()
                if source in disclosures and disclosures[source] != detail:
                    raise Unverifiable(
                        f"source {source} has conflicting Summary disclosures"
                    )
                disclosures[source] = detail
    unknown_disclosures = sorted(set(disclosures) - set(sources))
    if unknown_disclosures:
        raise Unverifiable(
            "Summary discloses source(s) absent from commit provenance: "
            + ",".join(unknown_disclosures)
        )
    digest = context_digest(sources, disclosures) if sources else None

    labels = payload.get("labels") or []
    label_names = [
        str(label.get("name") if isinstance(label, dict) else label)
        for label in labels
    ]
    approval_lanes = tuple(
        sorted(
            match.group(1)
            for name in label_names
            if (match := REVIEW_LABEL_RE.fullmatch(name))
        )
    )

    comments = payload.get("comments") or []
    reviewed_context: dict[str, bool] = {}
    current_approval_lanes: set[str] = set()
    for comment in comments:
        body = str(comment.get("body") if isinstance(comment, dict) else comment)
        lines = approval_binding.prose_lines(body)
        approvals = {
            parsed[1]
            for line in lines
            if (parsed := approval_binding.parse_marker(line))
            and parsed[0] == approval_binding.MARKER_APPROVED
            and parsed[2].lower() == head
        }
        current_approval_lanes.update(approvals)
        for line in lines:
            marker = CONTEXT_REVIEW_RE.fullmatch(_undecorate(line))
            if not marker:
                continue
            lane = marker.group("lane").lower()
            marker_context = marker.group("context").lower()
            marker_head = marker.group("head").lower()
            if lane in approvals and marker_head == head and marker_context == digest:
                reviewed_context[lane] = True

    # The final lander separately verifies the passed-review label cache against
    # APPROVED-AT authority. Here, those labels define WHICH approval lanes the
    # PR relies on. Offline/diagnostic use without labels still requires every
    # current exact-head approval lane, and absence of both remains a refusal for
    # a cherry-picked unit rather than an accidental pass.
    required_lanes = approval_lanes or tuple(sorted(current_approval_lanes))
    if sources and not required_lanes:
        required_lanes = ("<no-current-review-lane>",)

    units = []
    for source, carried_as in sorted(sources.items()):
        reviewed_lanes = tuple(
            lane for lane in required_lanes if reviewed_context.get(lane, False)
        )
        missing = tuple(lane for lane in required_lanes if lane not in reviewed_lanes)
        units.append(
            UnitStatus(
                source=source,
                carried_as=tuple(carried_as),
                disclosed=source in disclosures,
                disclosure_detail=disclosures.get(source),
                reviewed_lanes=reviewed_lanes,
                missing_review_lanes=missing,
            )
        )
    return Verdict(
        pr=payload.get("number"),
        head=head,
        approval_lanes=required_lanes,
        context_sha256=digest,
        units=tuple(units),
    )


def _gh_json(args: list[str]) -> Any:
    proc = subprocess.run(
        ["with-proxy", "gh", *args],
        capture_output=True,
        text=True,
        timeout=300,
    )
    if proc.returncode != 0:
        raise Unverifiable(
            f"gh {' '.join(args)} failed rc={proc.returncode}: {proc.stderr.strip()[:300]}"
        )
    try:
        return json.loads(proc.stdout or "null")
    except json.JSONDecodeError as exc:
        raise Unverifiable(f"gh returned malformed JSON: {exc}") from exc


def collect_live(repo: str, pr: int) -> dict[str, Any]:
    payload = _gh_json(
        [
            "pr",
            "view",
            str(pr),
            "--repo",
            repo,
            "--json",
            "number,headRefOid,body,labels,commits",
        ]
    )
    if not isinstance(payload, dict):
        raise Unverifiable("GitHub returned no PR object")
    comments = _gh_json(
        ["api", f"repos/{repo}/issues/{pr}/comments?per_page=100", "--paginate"]
    )
    if not isinstance(comments, list):
        raise Unverifiable("GitHub returned a non-list comment response")
    payload["comments"] = comments
    return payload


def _print(verdict: Verdict, *, as_json: bool) -> None:
    disclosed = sum(unit.disclosed for unit in verdict.units)
    context_reviewed = sum(not unit.missing_review_lanes for unit in verdict.units)
    state = "PASS" if verdict.passes else "REFUSED"
    if as_json:
        print(
            json.dumps(
                {
                    "pr": verdict.pr,
                    "head": verdict.head,
                    "approval_lanes": verdict.approval_lanes,
                    "context_sha256": verdict.context_sha256,
                    "unit_count": len(verdict.units),
                    "disclosed_count": disclosed,
                    "context_reviewed_count": context_reviewed,
                    "verdict": state,
                    "units": [asdict(unit) for unit in verdict.units],
                },
                indent=2,
            )
        )
        return
    print(
        "cherry-pick-context: "
        f"units={len(verdict.units)} disclosed={disclosed} "
        f"context-reviewed={context_reviewed} verdict={state}"
    )
    if verdict.context_sha256 is not None:
        print(f"  context-sha256={verdict.context_sha256}")
        if disclosed == len(verdict.units):
            for lane in verdict.approval_lanes:
                print(
                    "  required-review-line="
                    f"CHERRY-PICK-CONTEXT-REVIEWED: {lane} "
                    f"{verdict.context_sha256} {verdict.head}"
                )
        else:
            print("  required-review-line=<complete Summary disclosure first>")
    for unit in verdict.units:
        if unit.disclosed and not unit.missing_review_lanes:
            continue
        missing = []
        if not unit.disclosed:
            missing.append("Summary disclosure")
        if unit.missing_review_lanes:
            missing.append("new-context review by " + ",".join(unit.missing_review_lanes))
        print(
            f"  source={unit.source} carried-as={','.join(unit.carried_as)} "
            f"CONTEXT-CONTRACT-MISSING: {'; '.join(missing)}"
        )
        if not unit.disclosed:
            print(
                "  required-summary-line="
                f"Cherry-pick-context: {unit.source} - <behavior carried into this PR>"
            )
    if not verdict.units:
        print("  no explicit cherry-pick provenance; no context contract required")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--from-json", type=Path)
    source.add_argument("--pr", type=int)
    parser.add_argument("--repo", default="rrnewton/hermit")
    parser.add_argument("--expected-head")
    parser.add_argument("--gate", action="store_true")
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args()

    try:
        if args.from_json:
            payload = json.loads(args.from_json.read_text())
            if not isinstance(payload, dict):
                raise Unverifiable("fixture must contain one PR object")
        else:
            payload = collect_live(args.repo, args.pr)
        verdict = classify(payload, expected_head=args.expected_head)
    except (OSError, json.JSONDecodeError, Unverifiable) as exc:
        print(f"cherry-pick-context: UNVERIFIABLE: {exc}", file=sys.stderr)
        return 2

    _print(verdict, as_json=args.as_json)
    return 1 if args.gate and not verdict.passes else 0


if __name__ == "__main__":
    raise SystemExit(main())
