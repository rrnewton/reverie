#!/usr/bin/env python3
"""Count unanswered refusal records and the retirement route available to each."""

from __future__ import annotations

import argparse
import collections
import datetime
import importlib.machinery
import importlib.util
import json
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[3]
REPOSITORIES = (
    "rrnewton/hermit",
    "rrnewton/reverie",
    "rrnewton/dev-hermit",
    "rrnewton/agent-utils",
    "rrnewton/liteinst2",
)


def load_module(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


gate = load_module("refusal_gate_for_census", ROOT / "ci-hub/bin/changes-requested-gate")
binding = load_module("approval_binding_for_census", ROOT / "ci-hub/health/approval_binding.py")
liveness = load_module("agent_liveness_for_census", ROOT / "ci-hub/health/agent_liveness_probe.py")


QUERY = r"""
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: ASC}) {
      totalCount
      pageInfo { hasNextPage }
      nodes {
        number
        title
        url
        headRefOid
        isDraft
        mergeable
        comments(first: 100) {
          totalCount
          pageInfo { hasNextPage }
          nodes { databaseId url createdAt body }
        }
        reviews(first: 100) {
          totalCount
          pageInfo { hasNextPage }
          nodes { databaseId url submittedAt body }
        }
        reviewThreads(first: 50) {
          totalCount
          pageInfo { hasNextPage }
          nodes {
            comments(first: 50) {
              totalCount
              pageInfo { hasNextPage }
              nodes { databaseId url createdAt body }
            }
          }
        }
      }
    }
  }
}
"""


def github(repository: str) -> list[dict]:
    owner, name = repository.split("/", 1)
    result = subprocess.run(
        [
            "with-proxy",
            "gh",
            "api",
            "graphql",
            "-f",
            f"query={QUERY}",
            "-F",
            f"owner={owner}",
            "-F",
            f"name={name}",
        ],
        capture_output=True,
        text=True,
        timeout=180,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"GitHub query for {repository} failed rc={result.returncode}: "
            f"{(result.stderr or result.stdout).strip()[:300]}"
        )
    payload = json.loads(result.stdout)
    if payload.get("errors"):
        raise RuntimeError(f"GitHub query for {repository} returned {payload['errors']}")
    connection = payload["data"]["repository"]["pullRequests"]
    nodes = connection["nodes"]
    if connection["pageInfo"]["hasNextPage"] or connection["totalCount"] != len(nodes):
        raise RuntimeError(
            f"{repository} open pull requests were truncated: "
            f"read {len(nodes)} of {connection['totalCount']}"
        )
    return nodes


def require_complete(connection: dict, subject: str) -> list[dict]:
    nodes = connection["nodes"]
    total = connection.get("totalCount", len(nodes))
    if connection["pageInfo"]["hasNextPage"] or total != len(nodes):
        raise RuntimeError(
            f"{subject} was truncated: read {len(nodes)} of {total}"
        )
    return nodes


def comment_records(repository: str, pr: dict) -> tuple[list[dict], list[dict]]:
    issue = []
    for node in require_complete(pr["comments"], f"{repository} pull request {pr['number']} comments"):
        issue.append(
            {
                "id": node.get("databaseId"),
                "body": node.get("body") or "",
                "url": node.get("url"),
                "html_url": node.get("url"),
                "created_at": node.get("createdAt"),
                "container": "issue-comment",
            }
        )

    other = []
    empty = {"nodes": [], "totalCount": 0, "pageInfo": {"hasNextPage": False}}
    for node in require_complete(
        pr.get("reviews", empty), f"{repository} pull request {pr['number']} reviews"
    ):
        other.append(
            {
                "id": node.get("databaseId"),
                "body": node.get("body") or "",
                "url": node.get("url"),
                "html_url": node.get("url"),
                "created_at": node.get("submittedAt"),
                "container": "review",
            }
        )
    threads = require_complete(
        pr.get("reviewThreads", empty),
        f"{repository} pull request {pr['number']} review threads",
    )
    for thread_index, thread in enumerate(threads):
        for node in require_complete(
            thread["comments"],
            f"{repository} pull request {pr['number']} review thread {thread_index}",
        ):
            other.append(
                {
                    "id": node.get("databaseId"),
                    "body": node.get("body") or "",
                    "url": node.get("url"),
                    "html_url": node.get("url"),
                    "created_at": node.get("createdAt"),
                    "container": "review-comment",
                }
            )
    other.sort(key=lambda row: row.get("created_at") or "")
    return issue, other


def exact_retirements(records: list[dict]) -> tuple[dict[str, list[str | None]], dict[object, str]]:
    by_id = {record.get("id"): record.get("body") or "" for record in records}
    items = [(record.get("id"), record.get("body") or "") for record in records]
    claims: dict[str, list[str | None]] = {}
    for kind, _lane, _sha, cid in gate.markers_with_ids(items):
        if kind != gate.WITHDRAWN:
            continue
        body = by_id.get(cid, "")
        claimant = gate._issuer_of_comment(body)
        for target in binding.retirement_targets(body):
            claims.setdefault(target, []).append(claimant)
    return claims, by_id


def refusal_candidates(records: list[dict], lane: str) -> list[dict]:
    by_id = {record.get("id"): record for record in records}
    claims, bodies = exact_retirements(records)
    candidates = []
    # Parse one comment at a time. approval_binding deliberately counts a later
    # grammar-correction comment as another refusal record, while the landing gate
    # suppresses a byte-identical later verdict by the same issuer as a citation.
    # The census records both comments, then reports the reader difference instead
    # of silently choosing one count.
    for source in records:
        cid = source.get("id")
        body = source.get("body") or ""
        for kind, marker_lane, sha, _cid in gate.markers_with_ids([(cid, body)]):
            if kind != gate.REFUSAL or marker_lane not in (None, lane) or sha is None:
                continue
            issuer = gate._issuer_of_comment(body)
            retired = cid is not None and binding.retirement_allowed(
                issuer, claims.get(str(cid), [])
            )
            record = by_id.get(cid, {})
            candidates.append(
                {
                    "comment_id": cid,
                    "comment_url": record.get("url"),
                    "container": record.get("container"),
                    "created_at": record.get("created_at"),
                    "lane": marker_lane,
                    "refused_sha": sha,
                    "issuer": issuer,
                    "retired_by_exact_id": retired,
                }
            )
    return candidates


def outstanding_refusals(records: list[dict]) -> list[dict]:
    found: list[dict] = []
    used: set[tuple[str, object, str, str | None]] = set()
    for lane in ("claude", "codex"):
        unanswered: list[tuple[str, str | None]] = []
        binding.lane_shas(records, lane, unanswered=unanswered)
        wanted = collections.Counter(unanswered)
        for candidate in refusal_candidates(records, lane):
            key = (candidate["refused_sha"], candidate["issuer"])
            identity = (
                lane,
                candidate["comment_id"],
                candidate["refused_sha"],
                candidate["issuer"],
            )
            if candidate["retired_by_exact_id"] or wanted[key] <= 0 or identity in used:
                continue
            wanted[key] -= 1
            used.add(identity)
            found.append(candidate)
        if any(wanted.values()):
            raise RuntimeError(
                f"could not bind {lane} unanswered rows back to comment ids: {dict(wanted)}"
            )
    return found


def cached_hermit(path: pathlib.Path) -> list[dict]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(payload, list) and len(payload) == 1:
        payload = payload[0]
    return payload["data"]["repository"]["pullRequests"]["nodes"]


def write_result(result: dict, output: pathlib.Path | None) -> None:
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if output:
        output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")


def retirement_availability(
    comment_id: int | None, issuer: str | None, live: set[str]
) -> str:
    if comment_id is None:
        return "INDETERMINATE_MISSING_COMMENT_ID"
    if issuer is None:
        return "RETIRABLE_BY_EXACT_COMMENT_ID"
    if issuer in live:
        return "RETIRABLE_BY_LIVE_ISSUER"
    return "NO_LIVE_ISSUER"


def self_test() -> None:
    live = {"hermit-121"}
    assert retirement_availability(None, None, live) == "INDETERMINATE_MISSING_COMMENT_ID"
    assert retirement_availability(1, None, live) == "RETIRABLE_BY_EXACT_COMMENT_ID"
    assert retirement_availability(1, "hermit-121", live) == "RETIRABLE_BY_LIVE_ISSUER"
    assert retirement_availability(1, "hermit-016", live) == "NO_LIVE_ISSUER"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cached-hermit",
        type=pathlib.Path,
        help="an earlier complete Hermit GraphQL response; makes no network calls",
    )
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        help="write the complete JSON result here instead of stdout",
    )
    parser.add_argument(
        "--live-agents-from",
        type=pathlib.Path,
        help="reuse the live-agent list preserved in an earlier census result",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="test retirement classification without reading GitHub or /proc",
    )
    args = parser.parse_args(argv)
    if args.self_test:
        self_test()
        print("self-test passed")
        return 0
    captured_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
    basis = {
        "analysis_captured_at": captured_at,
        "reader_commit": subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip(),
        "repositories": ["rrnewton/hermit"] if args.cached_hermit else list(REPOSITORIES),
    }
    if args.live_agents_from:
        try:
            prior = json.loads(args.live_agents_from.read_text(encoding="utf-8"))
            prior_basis = prior["basis"]
            live = set(prior_basis["live_agents"])
            if not live:
                raise ValueError("the preserved live-agent list is empty")
            basis |= {
                "liveness_input": "preserved",
                "liveness_input_path": str(args.live_agents_from.resolve()),
                "liveness_input_captured_at": prior_basis["analysis_captured_at"],
                "process_view": prior_basis["process_view"],
                "process_view_reason": prior_basis["process_view_reason"],
                "own_uid_processes": prior_basis["own_uid_processes"],
                "unreadable_agent_capable_processes": prior_basis[
                    "unreadable_agent_capable_processes"
                ],
                "live_agents": sorted(live),
            }
        except Exception as exc:  # noqa: BLE001 -- a partial liveness input is unusable
            write_result(
                {
                    "state": "INDETERMINATE",
                    "reason": f"the preserved live-agent input could not be read: {type(exc).__name__}: {exc}",
                    "basis": basis,
                },
                args.output,
            )
            return 2
    else:
        scan = liveness.scan_proc("__refusal_census__")
        live = set(scan.distinct_agents_seen)
        basis |= {
            "liveness_input": "live",
            "process_view": scan.process_view,
            "process_view_reason": scan.process_view_reason,
            "own_uid_processes": scan.own_uid_total,
            "unreadable_agent_capable_processes": scan.blind_spot,
            "live_agents": sorted(live),
        }
        if scan.process_view != "full" or scan.blind_spot or not live:
            write_result(
                {
                    "state": "INDETERMINATE",
                    "reason": "the current live-agent population could not be established",
                    "basis": basis,
                },
                args.output,
            )
            return 2
    if args.cached_hermit:
        basis["github_input"] = "cached"
        basis["github_input_path"] = str(args.cached_hermit.resolve())
        basis["github_input_modified_at"] = datetime.datetime.fromtimestamp(
            args.cached_hermit.stat().st_mtime,
            tz=datetime.timezone.utc,
        ).isoformat()
    else:
        basis["github_input"] = "live"
        basis["github_input_captured_at"] = captured_at
    rows = []
    open_counts = {}
    non_issue_marker_records = []
    try:
        sources = (
            [("rrnewton/hermit", cached_hermit(args.cached_hermit))]
            if args.cached_hermit
            else [(repository, github(repository)) for repository in REPOSITORIES]
        )
        for repository, prs in sources:
            open_counts[repository] = len(prs)
            for pr in prs:
                issue, other = comment_records(repository, pr)
                for record in other:
                    items = [(record.get("id"), record.get("body") or "")]
                    if gate.markers_with_ids(items) or gate.approvals_in_order(items):
                        non_issue_marker_records.append(
                            {
                                "repository": repository,
                                "pr": pr["number"],
                                "container": record["container"],
                                "comment_id": record["id"],
                                "comment_url": record["url"],
                            }
                        )
                records = issue + other
                for refusal in outstanding_refusals(records):
                    cid = refusal["comment_id"]
                    issuer = refusal["issuer"]
                    availability = retirement_availability(cid, issuer, live)
                    rows.append(
                        {
                            "repository": repository,
                            "pr": pr["number"],
                            "pr_url": pr["url"],
                            "title": pr["title"],
                            "head": pr["headRefOid"],
                            "is_draft": pr["isDraft"],
                            "mergeable": pr["mergeable"],
                            **refusal,
                            "at_current_head": bool(
                                pr["headRefOid"].startswith(refusal["refused_sha"])
                                or refusal["refused_sha"].startswith(pr["headRefOid"])
                            ),
                            "retirement_availability": availability,
                        }
                    )
    except Exception as exc:  # noqa: BLE001 -- an incomplete census is never a count
        write_result(
            {
                "state": "INDETERMINATE",
                "reason": f"the pull-request census could not be completed: {type(exc).__name__}: {exc}",
                "basis": basis,
            },
            args.output,
        )
        return 2

    counts = collections.Counter(row["retirement_availability"] for row in rows)
    affected = collections.defaultdict(set)
    for row in rows:
        affected[row["retirement_availability"]].add(
            (row["repository"], row["pr"])
        )
    missing_id = counts.get("INDETERMINATE_MISSING_COMMENT_ID", 0)
    result = {
        "state": "INDETERMINATE" if missing_id else "complete",
        "reason": (
            f"{missing_id} unanswered refusal record(s) lacked a GitHub comment id"
            if missing_id
            else "every open pull request and nested comment container was complete"
        ),
        "basis": basis | {"open_pull_requests": open_counts},
        "counts": {
            "open_pull_requests": sum(open_counts.values()),
            "unanswered_refusal_records": len(rows),
            "affected_pull_requests": len({(row["repository"], row["pr"]) for row in rows}),
            **dict(sorted(counts.items())),
            "affected_pull_requests_by_availability": {
                key: len(value) for key, value in sorted(affected.items())
            },
            "non_issue_comment_marker_records": len(non_issue_marker_records),
        },
        "non_issue_comment_marker_records": non_issue_marker_records,
        "rows": sorted(
            rows,
            key=lambda row: (
                row["retirement_availability"],
                row["repository"],
                row["pr"],
                row["comment_id"] or -1,
            ),
        ),
    }
    write_result(result, args.output)
    if args.output:
        print(
            f"wrote {args.output} with {len(rows)} unanswered refusal record(s) "
            f"across {result['counts']['affected_pull_requests']} pull request(s)"
        )
    return 2 if missing_id else 0


if __name__ == "__main__":
    raise SystemExit(main())
