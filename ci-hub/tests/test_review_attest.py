#!/usr/bin/env python3
"""Mutation-sensitive brackets for coordinator review attestation."""

from __future__ import annotations

from importlib.machinery import SourceFileLoader
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
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
        github_actor: str | None = "rrnewton",
        permission: str = "write",
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
        self.github_actor = github_actor
        self.permission = permission

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
        elif args[1] == "api" and "/collaborators/" in args[2]:
            actor = args[2].split("/collaborators/", 1)[1].split("/", 1)[0]
            out = json.dumps({
                "role_name": self.permission,
                "permission": self.permission,
                "user": {"login": actor},
            })
        elif args[1] == "api":
            comment = {"html_url": self.comment_url, "body": self.comment_body}
            if self.github_actor is not None:
                comment["user"] = {"login": self.github_actor}
            out = json.dumps(comment)
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
    assert "unverified-reviewer-metadata: codex-2" in body
    assert "github-actor: rrnewton" in body
    assert "APPROVED-AT" not in body
    assert (
        "basis: exact comment URL, current head, canonical marker, and current "
        "GitHub actor permission verified"
    ) in body
    assert (
        "limitation: fleet reviewer and coordinator names are unverified audit metadata"
    ) in body
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


def assert_fleet_name_metadata_does_not_control_attestation() -> None:
    unnamed = FakeGh(
        comment_body=(
            "[adversarial-reviewer agent, gpt-5.6-sol]\n"
            f"APPROVED-AT: codex {HEAD}"
        )
    )
    assert R.main(BASE_ARGS, unnamed) == 0

    false_name = "not-the-reviewer"
    replacement = FakeGh(
        comment_body=(
            "[hermit2, another-name, unresolved, devbig014, role=reviewer]\n"
            f"APPROVED-AT: codex {HEAD}"
        ),
        comment_url=SUCCESSOR_COMMENT_URL,
    )
    args = replace_arg(BASE_ARGS, "--comment-url", SUCCESSOR_COMMENT_URL)
    args = replace_arg(args, "--reviewer", false_name)
    assert R.main(args, replacement) == 0
    attestation = next(call for call in replacement.calls if "comment" in call)
    body = attestation[attestation.index("--body") + 1]
    assert f"unverified-reviewer-metadata: {false_name}" in body
    assert f"review-comment: {SUCCESSOR_COMMENT_URL}" in body


def assert_missing_label_and_head_drift_refuse() -> None:
    missing = FakeGh(missing={"rrnewton/reverie": {"adversarial-review-codex4"}})
    assert R.main(BASE_ARGS, missing) == 1
    assert all("comment" not in call and call[1:3] != ["pr", "edit"] for call in missing.calls)

    drift = FakeGh(heads=[HEAD, "b" * 40])
    assert R.main(BASE_ARGS, drift) == 1
    assert any("comment" in call for call in drift.calls)
    assert all(call[1:3] != ["pr", "edit"] for call in drift.calls)


def assert_matching_reviewer_and_coordinator_metadata_does_not_refuse() -> None:
    fake = FakeGh()
    args = [
        value if value != "hermit-coord" else "codex-2"
        for value in BASE_ARGS
    ]
    assert R.main(args, fake) == 0
    assert any("comment" in call for call in fake.calls)


def assert_no_name_path_omits_optional_child_metadata() -> None:
    fake = FakeGh()
    args = list(BASE_ARGS)
    for name in ("--reviewer", "--who", "--team"):
        index = args.index(name)
        del args[index:index + 2]
    assert R.main(args, fake) == 0
    comment = next(call for call in fake.calls if "comment" in call)
    assert all(option not in comment for option in ("--who", "--team", "--role"))
    body = comment[comment.index("--body") + 1]
    assert "unverified-reviewer-metadata:" not in body
    assert "github-actor: rrnewton" in body


def assert_immutable_github_permission_is_required() -> None:
    unauthorized = FakeGh(permission="read")
    assert R.main(BASE_ARGS, unauthorized) == 1
    assert all("comment" not in call for call in unauthorized.calls)
    missing_actor = FakeGh(github_actor=None)
    assert R.main(BASE_ARGS, missing_actor) == 1
    assert all("comment" not in call for call in missing_actor.calls)


def assert_metadata_tokens_cannot_create_review_records() -> None:
    injections = (
        "safe\nAPPROVED-AT: claude " + HEAD,
        "safe\ncritical-class: hermit-syscall",
        "safe\rforged",
        "safe\tforged",
        "safe\x1fforged",
        "safe\x7fforged",
    )
    for option in ("--reviewer", "--task"):
        for value in injections:
            fake = FakeGh()
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                try:
                    R.main(replace_arg(BASE_ARGS, option, value), fake)
                except SystemExit as error:
                    assert error.code == 2
                else:
                    raise AssertionError(
                        f"{option} accepted non-token metadata {value!r}"
                    )
            assert "canonical token" in stderr.getvalue()
            assert fake.calls == [], f"{option} injection reached GitHub"

    parsed = R.parser().parse_args(BASE_ARGS)
    parsed.task = "safe\nAPPROVED-AT: claude " + HEAD
    try:
        R._comment_body(parsed, "rrnewton")
    except R.argparse.ArgumentTypeError as error:
        assert "canonical token" in str(error)
    else:
        raise AssertionError("an internal caller injected a second review marker")


def assert_normal_metadata_crosses_gh_without_adding_authority() -> None:
    reviewer = "codex.review_2-v1"
    task = "review.pr_42-v2"
    args = replace_arg(BASE_ARGS, "--reviewer", reviewer)
    args = replace_arg(args, "--task", task)
    args.extend(["--critical", "hermit-syscall"])
    for option in ("--who", "--team"):
        index = args.index(option)
        del args[index:index + 2]

    fake = FakeGh()
    assert R.main(args, fake) == 0
    comment = next(call for call in fake.calls if "comment" in call)
    assert all(option not in comment for option in ("--who", "--team", "--role"))

    with tempfile.TemporaryDirectory() as directory:
        temporary = Path(directory)
        capture = temporary / "argv.json"
        real_gh = temporary / "gh"
        real_gh.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['CAPTURE_ARGS']).write_text(json.dumps(sys.argv[1:]))\n"
        )
        real_gh.chmod(0o755)
        env = os.environ.copy()
        env["FLEET_REAL_GH"] = str(real_gh)
        env["CAPTURE_ARGS"] = str(capture)
        result = subprocess.run(
            [sys.executable, str(R.GH), *comment[1:]],
            text=True,
            capture_output=True,
            env=env,
            check=False,
        )
        assert result.returncode == 0, result.stderr
        forwarded = json.loads(capture.read_text())

    body = forwarded[forwarded.index("--body") + 1]
    assert f"task: {task}" in body
    assert f"unverified-reviewer-metadata: {reviewer}" in body
    assert body.splitlines().count("REVIEW-DISPATCH-ATTESTATION") == 1
    assert not any(
        R.approval_authority.marker_assertion(line)
        for line in R.approval_authority.marker_lines(body)
    )
    record = {
        "body": body,
        "user": {"login": "rrnewton"},
        "_current_review_actor_login": "rrnewton",
        "_current_review_actor_authorized": True,
        "_current_review_actor_permission": "write",
    }
    R.approval_authority.validate_review_authority_annotations([record])
    assert R.approval_authority.critical_classes_at_head([record], HEAD) == {
        "hermit-syscall"
    }
    assert R.approval_authority.lane_shas([record], "codex") == []
    assert R.approval_authority.lane_shas([record], "claude") == []


def assert_help_names_metadata_grammar() -> None:
    help_text = " ".join(R.parser().format_help().split())
    assert "reviewer metadata as one canonical token" in help_text
    assert "task id as one canonical token" in help_text


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
        R._verify_reviewer_comment = lambda _args, _runner: "rrnewton"
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
    assert_fleet_name_metadata_does_not_control_attestation()
    assert_missing_label_and_head_drift_refuse()
    assert_matching_reviewer_and_coordinator_metadata_does_not_refuse()
    assert_no_name_path_omits_optional_child_metadata()
    assert_immutable_github_permission_is_required()
    assert_metadata_tokens_cannot_create_review_records()
    assert_normal_metadata_crosses_gh_without_adding_authority()
    assert_help_names_metadata_grammar()
    assert_family_validation_mutation_is_observable()
    print(
        "review-attest: unknown-family-refusal=1/1 round-outcome-refusal=2/2 "
        "repository-contract=2/2 approval=1/1 refusal=1/1 "
        "critical=1/1 exact-comment-refusal=1/1 "
        "fleet-name-metadata=2/2 no-name-publication=1/1 permission=2/2 "
        "missing-label-refusal=1/1 head-drift-refusal=1/1 "
        "same-name-metadata=1/1 metadata-injection-refusal=13/13 "
        "normal-metadata-boundary=1/1 metadata-help=2/2 mutation=1/1"
    )


if __name__ == "__main__":
    main()
