#!/usr/bin/env python3
"""Mutation-sensitive brackets for coordinator review attestation."""

from __future__ import annotations

from importlib.machinery import SourceFileLoader
import importlib.util
import io
import json
from pathlib import Path
import subprocess
from contextlib import redirect_stderr


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "ci-hub/bin/review-attest"
SPEC = importlib.util.spec_from_loader(
    "review_attest", SourceFileLoader("review_attest", str(SCRIPT))
)
assert SPEC is not None and SPEC.loader is not None
R = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(R)

HEAD = "a" * 40
COMMENT_ID = "5432109876"
COMMENT_URL = f"https://github.com/rrnewton/hermit/pull/42#issuecomment-{COMMENT_ID}"
SUCCESSOR_COMMENT_ID = "6543210987"
SUCCESSOR_COMMENT_URL = (
    f"https://github.com/rrnewton/hermit/pull/42#issuecomment-{SUCCESSOR_COMMENT_ID}"
)
BASE_ARGS = [
    "--pr", "42",
    "--head", HEAD,
    "--family", "codex",
    "--outcome", "approval",
    "--comment-url", COMMENT_URL,
    "--reviewer", "codex-2",
    "--task", "review-pr-42",
    "--who", "hermit-coord",
    "--team", "hermit2",
]


class FakeGh:
    def __init__(
        self,
        *,
        heads: list[str] | None = None,
        missing: dict[str, set[str]] | None = None,
        comment_body: str | None = None,
        comment_url: str = COMMENT_URL,
        pr_labels: set[str] | None = None,
    ) -> None:
        self.heads = iter(heads or [HEAD, HEAD, HEAD])
        self.calls: list[list[str]] = []
        self.labels = {
            repo: set(R.review_contract.required_repository_labels())
            - (missing or {}).get(repo, set())
            for repo in R.review_contract.SUPPORTED_REPOS
        }
        self.comment_body = comment_body or (
            f"[hermit2, codex-2, unresolved, devbig014, role=reviewer]\n"
            f"APPROVED-AT: codex {HEAD}"
        )
        self.comment_url = comment_url
        self.pr_labels = set(pr_labels or ())

    def __call__(self, args: list[str], **_: object) -> subprocess.CompletedProcess[str]:
        self.calls.append(args)
        if args[1:3] == ["label", "list"]:
            repo = args[args.index("--repo") + 1]
            out = json.dumps([{"name": name} for name in sorted(self.labels[repo])])
        elif args[1:3] == ["pr", "view"]:
            out = json.dumps(
                {
                    "headRefOid": next(self.heads),
                    "state": "OPEN",
                    "labels": [{"name": name} for name in sorted(self.pr_labels)],
                }
            )
        elif args[1] == "api":
            out = json.dumps({"html_url": self.comment_url, "body": self.comment_body})
        elif args[1:3] == ["pr", "edit"]:
            for index, arg in enumerate(args):
                if arg == "--add-label":
                    self.pr_labels.add(args[index + 1])
                elif arg == "--remove-label":
                    self.pr_labels.discard(args[index + 1])
            out = ""
        else:
            out = ""
        return subprocess.CompletedProcess(args, 0, out, "")


def replace_arg(args: list[str], name: str, value: str) -> list[str]:
    changed = list(args)
    if name not in changed:
        return [*changed, name, value]
    changed[changed.index(name) + 1] = value
    return changed


def assert_unknown_family_refuses_before_mutation() -> None:
    fake = FakeGh()
    args = replace_arg(BASE_ARGS, "--family", "codxe")
    stderr = io.StringIO()
    with redirect_stderr(stderr):
        try:
            R.main(args, fake)
        except SystemExit as error:
            assert error.code == 2
        else:
            raise AssertionError("unknown family parsed successfully")
    assert "invalid choice" in stderr.getvalue()
    assert fake.calls == [], "unknown family reached GitHub"


def assert_out_of_range_round_and_unknown_outcome_refuse_before_mutation() -> None:
    for flag, value in (("--round", "5"), ("--outcome", "unknown")):
        with io.StringIO() as stderr, redirect_stderr(stderr):
            fake = FakeGh()
            try:
                R.main(replace_arg(BASE_ARGS, flag, value), fake)
            except SystemExit as error:
                assert error.code == 2
            else:
                raise AssertionError(f"{flag}={value} parsed successfully")
            assert stderr.getvalue(), f"{flag}={value} emitted no refusal diagnostic"
            assert fake.calls == [], f"{flag}={value} reached GitHub"


def assert_every_writer_label_is_required_in_both_repositories() -> None:
    fake = FakeGh()
    R._assert_repository_contract(fake)
    label_queries = [call for call in fake.calls if call[1:3] == ["label", "list"]]
    assert {
        call[call.index("--repo") + 1] for call in label_queries
    } == set(R.review_contract.SUPPORTED_REPOS)


def assert_approval_records_activity_plus_approval() -> None:
    fake = FakeGh()
    assert R.main(BASE_ARGS, fake) == 0
    comment = next(call for call in fake.calls if "comment" in call)
    body = comment[comment.index("--body") + 1]
    assert "review-outcome: approval" in body
    assert f"review-comment: {COMMENT_URL}" in body
    assert "APPROVED-AT" not in body
    assert "dispatch attestation does not prove the executing process family" in body
    assert comment[1:7] == ["--who", "hermit-coord", "--team", "hermit2", "--role", "coordinator"]
    edit = next(call for call in fake.calls if call[1:3] == ["pr", "edit"])
    assert "adversarial-review-codex1" in edit and "passed-review-codex" in edit
    assert "post-facto-human-review" not in edit



def assert_refusal_records_activity_without_approval() -> None:
    fake = FakeGh(
        comment_body=(
            f"[hermit2, codex-2, unresolved, devbig014, role=reviewer]\n"
            f"CHANGES-REQUESTED-AT: codex {HEAD}"
        ),
        pr_labels={"passed-review-codex"},
    )
    args = replace_arg(BASE_ARGS, "--outcome", "refusal")
    assert R.main(args, fake) == 0
    edit = next(call for call in fake.calls if call[1:3] == ["pr", "edit"])
    assert edit[edit.index("--add-label") + 1] == "adversarial-review-codex1"
    assert edit[edit.index("--remove-label") + 1] == "passed-review-codex"
    assert "passed-review-codex" not in fake.pr_labels


def assert_critical_path() -> None:
    critical = FakeGh()
    args = [*BASE_ARGS, "--critical", "hermit-syscall"]
    assert R.main(args, critical) == 0
    edit = next(call for call in critical.calls if call[1:3] == ["pr", "edit"])
    assert "post-facto-human-review" in edit


def assert_exact_reviewer_comment_is_required() -> None:
    wrong_head = FakeGh(
        comment_body=(
            f"[hermit2, codex-2, unresolved, devbig014, role=reviewer]\n"
            f"APPROVED-AT: codex {'b' * 40}"
        )
    )
    assert R.main(BASE_ARGS, wrong_head) == 1
    assert all(
        "comment" not in call and call[1:3] != ["pr", "edit"]
        for call in wrong_head.calls
    )


def assert_unattributed_review_refuses_and_independent_successor_attests() -> None:
    invalid = FakeGh(
        comment_body=(
            "[adversarial-reviewer agent, gpt-5.6-sol]\n"
            f"APPROVED-AT: codex {HEAD}"
        )
    )
    stderr = io.StringIO()
    with redirect_stderr(stderr):
        assert R.main(BASE_ARGS, invalid) == 1
    refusal = stderr.getvalue().casefold()
    assert "reviewer comment issuer is missing" in refusal
    assert "do not wait for the original reviewer" in refusal
    assert "another independent reviewer" in refusal
    assert all(
        "comment" not in call and call[1:3] != ["pr", "edit"]
        for call in invalid.calls
    )

    successor = "codex-9"
    replacement = FakeGh(
        comment_body=(
            f"[hermit2, {successor}, unresolved, devbig014, role=reviewer]\n"
            f"APPROVED-AT: codex {HEAD}"
        ),
        comment_url=SUCCESSOR_COMMENT_URL,
    )
    args = replace_arg(BASE_ARGS, "--comment-url", SUCCESSOR_COMMENT_URL)
    args = replace_arg(args, "--reviewer", successor)
    assert R.main(args, replacement) == 0
    attestation = next(call for call in replacement.calls if "comment" in call)
    body = attestation[attestation.index("--body") + 1]
    assert f"reviewer: {successor}" in body
    assert f"review-comment: {SUCCESSOR_COMMENT_URL}" in body


def assert_missing_label_and_head_drift_refuse() -> None:
    missing = FakeGh(missing={"rrnewton/reverie": {"adversarial-review-codex4"}})
    assert R.main(BASE_ARGS, missing) == 1
    assert all("comment" not in call and call[1:3] != ["pr", "edit"] for call in missing.calls)

    drift = FakeGh(heads=[HEAD, "b" * 40])
    assert R.main(BASE_ARGS, drift) == 1
    assert any("comment" in call for call in drift.calls)
    assert all(call[1:3] != ["pr", "edit"] for call in drift.calls)


def assert_named_reviewer_cannot_self_attest() -> None:
    fake = FakeGh()
    args = [
        value if value != "hermit-coord" else "codex-2"
        for value in BASE_ARGS
    ]
    assert R.main(args, fake) == 1
    assert fake.calls == [], "named reviewer reached GitHub on self-attestation"


def assert_family_validation_mutation_is_observable() -> None:
    """A permissive family mutation reaches the exact GitHub write we guard."""

    mutant = R.parser()
    family = next(action for action in mutant._actions if action.dest == "family")
    assert set(family.choices) == {"codex", "claude"}
    family.choices = None
    R.FAMILY_LABELS["codxe"] = "passed-review-codex"
    original_verify = R._verify_reviewer_comment
    original_round_label = R.review_contract.round_label
    original_contract_check = R._assert_repository_contract
    try:
        fake = FakeGh()
        fake.labels["rrnewton/hermit"].add("adversarial-review-codxe1")
        args = mutant.parse_args(replace_arg(BASE_ARGS, "--family", "codxe"))
        R._verify_reviewer_comment = lambda _args, _runner: None
        R._assert_repository_contract = lambda _runner: None
        R.review_contract.round_label = lambda _family, round_number: f"adversarial-review-codxe{round_number}"
        assert R.execute(args, fake) == 0
        assert any("comment" in call for call in fake.calls), (
            "mutation did not reach the guarded GitHub write"
        )
    finally:
        R._verify_reviewer_comment = original_verify
        R._assert_repository_contract = original_contract_check
        R.review_contract.round_label = original_round_label
        del R.FAMILY_LABELS["codxe"]


def main() -> None:
    assert_unknown_family_refuses_before_mutation()
    assert_out_of_range_round_and_unknown_outcome_refuse_before_mutation()
    assert_every_writer_label_is_required_in_both_repositories()
    assert_approval_records_activity_plus_approval()
    assert_refusal_records_activity_without_approval()
    assert_critical_path()
    assert_exact_reviewer_comment_is_required()
    assert_unattributed_review_refuses_and_independent_successor_attests()
    assert_missing_label_and_head_drift_refuse()
    assert_named_reviewer_cannot_self_attest()
    assert_family_validation_mutation_is_observable()
    print(
        "review-attest: unknown-family-refusal=1/1 round-outcome-refusal=2/2 "
        "repository-contract=2/2 approval=1/1 refusal=1/1 "
        "critical=1/1 exact-comment-refusal=1/1 "
        "unattributed-refusal=1/1 successor-recovery=1/1 "
        "missing-label-refusal=1/1 head-drift-refusal=1/1 "
        "reviewer-self-attestation-refusal=1/1 mutation=1/1"
    )


if __name__ == "__main__":
    main()
