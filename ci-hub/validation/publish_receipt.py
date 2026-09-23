#!/usr/bin/env python3
"""Mechanically publish one verifier-selected local-validation receipt.

This module is deliberately not an authority: it never scans or selects a
ledger row, comments on a PR, or applies a label.  The Rust
Rust ``ci-hub`` consumers select one exact canonical row and pass its digest.
Python verifies byte identity and checks that selected row against the shared
policy predicate; Rust alone authorizes the commit status or PR binding.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
from typing import Any
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

# Stable, greppable marker meaning "this receipt's evidence lives on another
# host", as distinct from "this receipt is malformed".  Consumers match on this
# token rather than on path text; see preserve_log().
FOREIGN_HOST_TOKEN = "RECEIPT-ARTIFACT-FOREIGN-HOST"

# The shared qualifying-receipt predicate lives at the ci-hub root (beside
# check_outcome.py), one directory above this validation/ package.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualifying_receipt  # noqa: E402
from validate import gh_route  # noqa: E402
from admission_paths import read_admission_log  # noqa: E402


RECEIPT_REPO = "rrnewton/dev-hermit"
RECEIPT_BRANCH = "validation-receipts"
RECEIPT_CANONICALIZATION = "serde_json::to_vec(HistoryRow)-v1"


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"publish-receipt: {message}")


def canonical(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def selected_record(
    canonical_record: bytes,
    *,
    sha: str,
    expected_digest: str,
    canonicalization: str,
    repo: str = "rrnewton/hermit",
) -> dict[str, Any]:
    """Verify selected-row identity and shared-policy consistency."""
    if canonicalization != RECEIPT_CANONICALIZATION:
        fail(f"unsupported canonicalization {canonicalization!r}")
    if len(expected_digest) != 64 or any(
        ch not in "0123456789abcdef" for ch in expected_digest
    ):
        fail("selected receipt digest must be exactly 64 lowercase hex characters")
    actual_digest = hashlib.sha256(canonical_record).hexdigest()
    if actual_digest != expected_digest:
        fail(
            "selected receipt digest mismatch: "
            f"expected {expected_digest}, got {actual_digest}"
        )
    try:
        row = json.loads(canonical_record, object_pairs_hook=qualifying_receipt.AdmissionJsonObject)
    except json.JSONDecodeError as error:
        fail(f"selected canonical record is invalid JSON: {error}")
    if not isinstance(row, dict):
        fail("selected canonical record is not an object")
    if row.get("commit") != sha:
        fail("selected canonical record is not bound to --sha")
    if repo not in ("rrnewton/hermit", "rrnewton/reverie"):
        fail(f"unsupported validation repository {repo!r}")
    row_repo = row.get("repo")
    if row_repo not in (repo, repo.rsplit("/", 1)[1]) and not (
        repo == "rrnewton/hermit" and row.get("schema_version") == 4 and row_repo is None
    ):
        fail("selected canonical record is not bound to --repo")
    if repo == "rrnewton/reverie":
        # The existing Rust authority owns Reverie's versioned gates, count
        # accounting, admission and containment contract. Hermit's shared
        # predicate requires different base and producer evidence.
        environment = os.environ.copy()
        environment["CI_HUB_SKIP_SOFT_MAIN_WARNING"] = "1"
        result = bounded_capture.capture(
            [
                str(Path(__file__).resolve().parents[1] / "ci-hub"),
                "receipt-digest", "--sha", sha, "--repository", repo,
                "--require-canonical-qualifying", "--canonical-row",
            ],
            input=canonical_record.decode(),
            env=environment,
        )
        if result.returncode != 0:
            fail(f"selected Reverie record failed canonical qualification: {result.stderr.strip()}")
        if result.stdout.encode() != canonical_record:
            fail("selected Reverie record does not contain the exact canonical HistoryRow bytes")
    elif not qualifying_receipt.row_qualifies(row, sha, qualifying_receipt.active()):
        fail("selected canonical record does not satisfy the shared receipt predicate")
    for field in ("started_at", "log_file", "host"):
        if not isinstance(row.get(field), str) or not row[field]:
            fail(f"selected canonical record has no {field}")
    return row


def short_hostname() -> str:
    """The canonical short host, matching how ledger rows record ``host``."""
    return socket.gethostname().split(".", 1)[0]


def preserve_log(evidence_dir: Path, sha: str, row: dict[str, Any], *, state_root=None) -> Path:
    source = Path(row["log_file"])
    portable_content = None
    if qualifying_receipt.admission_is_v2(row):
        if state_root is None:
            fail("v2 log preservation requires an explicit state root")
        try:
            portable_content = read_admission_log(row, state_root)
        except (OSError, ValueError) as error:
            fail(f"v2 bound log is unavailable: {error}")
    elif not source.is_absolute() or not source.is_file():
        # A receipt's ``log_file`` is recorded through a host-relative
        # ``{{WORKSPACE_ROOT}}`` token, which LOOKS portable but is not: the
        # artifact only exists on the machine that produced it.  Resolving it
        # here on a different host finds nothing, and the bare message below
        # then reads as a malformed receipt when the real condition is "this
        # evidence belongs to another machine".  Say which, and emit a stable
        # token so a caller can distinguish the two without parsing paths.
        #
        # The original sentence is preserved verbatim as a prefix: it is the
        # same-host failure's contract and is pinned by
        # validation/test_apply_local_label_receipt_reuse.sh.
        produced_on = row["host"].split(".", 1)[0]
        here = short_hostname()
        if produced_on != here:
            fail(
                f"ledger log is not a readable absolute file: {source} "
                f"[{FOREIGN_HOST_TOKEN}: receipt produced on {produced_on}, "
                f"this host is {here}; the log artifact is not available here]"
            )
        fail(f"ledger log is not a readable absolute file: {source}")
    evidence_dir.mkdir(parents=True, exist_ok=True)
    started = "".join(ch for ch in row["started_at"] if ch.isalnum())
    content = portable_content if portable_content is not None else source.read_bytes()
    # A run log can legitimately grow after the canonical ledger row is
    # emitted: post-run receipt binding writes its own audit suffix.  SHA and
    # start time therefore do not uniquely identify immutable log bytes.  Key
    # the preserved copy by its content as well, so a later finalization of the
    # same run can mint a second, exact artifact without overwriting or
    # conflicting with the already-published prefix.
    content_digest = hashlib.sha256(content).hexdigest()
    destination = evidence_dir / f"{sha}-{started}-{content_digest}.log"
    if destination.exists() and destination.read_bytes() != content:
        fail(f"durable log path already contains different content: {destination}")
    if not destination.exists():
        destination.write_bytes(content)
    return destination


def producer_verifier_path() -> Path:
    return qualifying_receipt.producer_definition_verifier_path()


def run_producer_verifier(
    *arguments: str, input_body: str | None = None
) -> subprocess.CompletedProcess[str]:
    """Invoke the one semantic producer-definition authority."""
    return bounded_capture.capture(
        [str(producer_verifier_path()), *arguments],
        input=input_body,
        env=os.environ.copy(),
    )


def registered_producer() -> dict[str, str]:
    """Read the primary map through the one semantic verifier."""
    try:
        return qualifying_receipt.registered_producer_definition()
    except RuntimeError as error:
        fail(str(error))


def producer_definition_allowed(
    definition: dict[str, str], *, sha: str | None = None
) -> bool:
    """Ask whether this exact whole map is allowed at an optional commit."""
    try:
        return qualifying_receipt.producer_definition_allowed(definition, sha=sha)
    except RuntimeError as error:
        fail(str(error))


def resolution_repositories(row: dict[str, Any]) -> list[Path]:
    """Repositories that may durably hold the validated commit, most specific first.

    WHAT THE CONSUMER ACTUALLY NEEDS is a git object store containing the SHA --
    the producer blobs are "a property of the commit that was validated", as
    `producer_definition` says. What it USED was `row["cwd"]`, the directory the
    run happened to execute in. That is a correlated handle standing in for the
    real requirement, and it only worked while the run and the work shared a
    directory.

    MEASURED 2026-08-08, which is why this is not fixed on the producer side:
    of 124 qualified rows, 72 (58%) already pointed at a directory that no longer
    exists -- and only 2 of those were temp checkouts. The rest were RECLAIMED
    SLOTS (worktrees/227b/hermit x24, 238b x6, sabre x6, ...). So recording the
    slot instead of the temp dir is not a fix; it is the status quo that already
    fails, on a slower clock. Every path is ephemeral here; only the half-life
    differs.

    `cwd` is also load-bearing elsewhere and must keep meaning what it says:
    `start_unit.assert_row_readable_from_canonical_ledger` binds a receipt by
    commit AND cwd precisely so that "a row matching on commit alone could be
    some other run of the same SHA" cannot pass. Rewriting it to a shared slot
    would make two concurrent runs from one slot indistinguishable.

    So the recorded cwd is still tried FIRST -- it is the most specific answer
    and preserves existing behaviour when the directory survives -- and a durable
    repository is the fallback rather than the replacement.
    """
    return qualifying_receipt.producer_definition_repositories(row)


def producer_definition(row: dict[str, Any], sha: str) -> dict[str, Any]:
    """Identify the check definition that produced this row.

    Derived from the VALIDATED COMMIT (`git rev-parse <sha>:<path>`), not
    self-reported by the producer: the blobs are a property of the commit that
    was validated, so a producer cannot claim a definition it did not run
    without also changing the commit under test.  Failure to resolve is fatal --
    a receipt that cannot name its producer must not be minted at all, because a
    producer-less receipt is exactly what the consumer now refuses.
    """
    try:
        return qualifying_receipt.resolve_producer_definition(row, sha)
    except (qualifying_receipt.ProducerDefinitionError, RuntimeError) as error:
        # REFUSED, never silently skipped. A label step that quietly applies
        # nothing is the fail-open shape wearing a green tick.
        fail(str(error))


def observed_producer_definition(row: dict[str, Any], sha: str) -> dict[str, Any]:
    """Record exact producer blobs without requiring a separately registered hash set."""
    try:
        return qualifying_receipt.observe_producer_definition(row, sha)
    except (qualifying_receipt.ProducerDefinitionError, RuntimeError) as error:
        fail(str(error))


def build_receipt(
    repo: str,
    sha: str,
    row: dict[str, Any],
    durable_log: Path,
    *,
    selected_digest: str,
    canonicalization: str,
    observe_producer: bool = False,
) -> tuple[dict[str, Any], bytes, str]:
    log_digest = hashlib.sha256(durable_log.read_bytes()).hexdigest()
    # Host-in-identity (Req2): the receipt identity carries the producing host,
    # so a receipt cannot be read blind to where it was produced and its host
    # field cannot be swapped without breaking this tamper-evident run_id.
    run_id = f"{sha}@{row['started_at']}@{row['host']}"
    receipt = {
        "schema_version": 1,
        "repository": repo,
        "commit": sha,
        "run_id": run_id,
        "source_log_file": row["log_file"],
        "durable_log_file": str(durable_log),
        "log_sha256": log_digest,
        # Producer binding: WHICH check definition produced this receipt, at
        # which head. PR binding retains the registered-definition check.
        # Commit-status publication records the same exact blobs directly from
        # the already-qualified commit, because the local status authority no
        # longer requires a second repository to register validator edits.
        "producer": (
            observed_producer_definition(row, sha)
            if observe_producer
            else producer_definition(row, sha)
        ),
        "selected_receipt_identity": {
            "digest_algorithm": "sha256",
            "canonicalization": canonicalization,
            "digest": selected_digest,
        },
        "ledger_record": row,
    }
    body = canonical(receipt)
    return receipt, body, hashlib.sha256(body).hexdigest()


def gh_command() -> list[str]:
    try:
        return gh_route.command()
    except ValueError as error:
        fail(str(error))


def gh(
    args: list[str], *, check: bool = True, input_body: str | None = None
) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    if not environment.get("GH_CONFIG_DIR"):
        candidate = Path.home() / ".config" / "gh"
        if (candidate / "hosts.yml").is_file():
            environment["GH_CONFIG_DIR"] = str(candidate)
    result = bounded_capture.capture(
        gh_command() + args, env=environment, input=input_body
    )
    if check and result.returncode:
        fail(f"{' '.join(args)} failed: {result.stderr.strip()}")
    return result


def branch_head(repo: str, branch: str) -> str:
    result = gh(["api", f"repos/{repo}/git/ref/heads/{branch}"], check=False)
    if result.returncode == 0:
        return json.loads(result.stdout)["object"]["sha"]
    main = gh(["api", f"repos/{repo}/git/ref/heads/main"])
    main_sha = json.loads(main.stdout)["object"]["sha"]
    created = gh(
        [
            "api",
            "--method",
            "POST",
            f"repos/{repo}/git/refs",
            "-f",
            f"ref=refs/heads/{branch}",
            "-f",
            f"sha={main_sha}",
        ],
        check=False,
    )
    if created.returncode != 0:
        # A concurrent publisher may have created the branch.
        result = gh(["api", f"repos/{repo}/git/ref/heads/{branch}"])
        return json.loads(result.stdout)["object"]["sha"]
    return json.loads(created.stdout)["object"]["sha"]


def publish(repo: str, branch: str, path: str, body: bytes) -> str:
    head = branch_head(repo, branch)
    endpoint = f"repos/{repo}/contents/{path}"
    # Resolve the path against the exact branch head we will return. Asking for
    # the moving branch here could observe a concurrent publisher's newer commit
    # and then return an older commit that does not contain the receipt.
    existing = gh(["api", f"{endpoint}?ref={head}"], check=False)
    if existing.returncode == 0:
        value = json.loads(existing.stdout)
        found = base64.b64decode(value["content"].replace("\n", ""))
        if found != body:
            fail(
                f"immutable receipt path already exists with different content: {path}"
            )
        return head

    request = json.dumps(
        {
            "message": f"validation receipt: {path.rsplit('/', 1)[-1]}",
            "content": base64.b64encode(body).decode(),
            "branch": branch,
        }
    )
    result = gh(
        [
            "api",
            "--method",
            "PUT",
            endpoint,
            "--input",
            "-",
        ],
        input_body=request,
    )
    return json.loads(result.stdout)["commit"]["sha"]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Publish one validated receipt and its preserved evidence."
    )
    parser.add_argument("--repo", required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--selected-receipt-sha256", required=True)
    parser.add_argument("--canonicalization", required=True)
    parser.add_argument("--receipt-repo", default=RECEIPT_REPO)
    parser.add_argument("--receipt-branch", default=RECEIPT_BRANCH)
    parser.add_argument(
        "--state-root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
    )
    parser.add_argument(
        "--observe-producer-definition",
        action="store_true",
        help="record exact producer blobs without requiring a registered hash set",
    )
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args()


def execute(args: argparse.Namespace, canonical_record: bytes) -> dict[str, Any]:
    if len(args.sha) != 40 or any(ch not in "0123456789abcdef" for ch in args.sha):
        fail("--sha must be exactly 40 lowercase hex characters")
    row = selected_record(
        canonical_record,
        sha=args.sha,
        expected_digest=args.selected_receipt_sha256,
        canonicalization=args.canonicalization,
        repo=args.repo,
    )
    evidence_dir = args.state_root / "ignored" / "validation-evidence"
    durable_log = preserve_log(evidence_dir, args.sha, row, state_root=args.state_root)
    receipt, body, artifact_digest = build_receipt(
        args.repo,
        args.sha,
        row,
        durable_log,
        selected_digest=args.selected_receipt_sha256,
        canonicalization=args.canonicalization,
        observe_producer=args.observe_producer_definition,
    )
    path = f"validation-receipts/{args.repo}/{args.sha}/{artifact_digest}.json"
    if args.dry_run:
        action = "would-publish"
        commit = None
    else:
        action = "published"
        commit = publish(args.receipt_repo, args.receipt_branch, path, body)
    return {
        "schema_version": 1,
        "action": action,
        "receipt_commit": commit,
        "receipt_repository": args.receipt_repo,
        "receipt_branch": args.receipt_branch,
        "path": path,
        "receipt_identity_sha256": args.selected_receipt_sha256,
        "artifact_sha256": artifact_digest,
        "artifact_body": body.decode(),
    }


def main() -> int:
    args = parse_args()
    report = execute(args, sys.stdin.buffer.read())
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
