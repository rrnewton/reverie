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
BASE_ARGS = [
    "--pr", "42",
    "--head", HEAD,
    "--family", "codex",
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
        missing: set[str] | None = None,
        persist_comment: bool = True,
        task_notes: list[str] | None = None,
    ) -> None:
        self.heads = iter(heads or [HEAD, HEAD, HEAD])
        self.calls: list[list[str]] = []
        self.comments: list[dict[str, str]] = []
        self.persist_comment = persist_comment
        # Deliberately not read by review-attest. A TaskGraph note is durable
        # review output, but it is not the PR-comment authority consumed at
        # landing and therefore cannot authorize task completion.
        self.task_notes = task_notes or []
        self.labels = {
            "adversarial-review-codex1",
            "adversarial-review-claude1",
            "passed-review-codex",
            "passed-review-claude",
            "post-facto-human-review",
        } - (missing or set())

    def __call__(self, args: list[str], **_: object) -> subprocess.CompletedProcess[str]:
        self.calls.append(args)
        if args[:2] == ["tg", "show"] or args[:2] == ["tg", "update"]:
            out = ""
        elif args[1:3] == ["label", "list"]:
            out = json.dumps([{"name": name} for name in sorted(self.labels)])
        elif args[1:3] == ["pr", "view"]:
            out = json.dumps({"headRefOid": next(self.heads), "state": "OPEN", "labels": []})
        elif "comment" in args:
            if self.persist_comment:
                body = args[args.index("--body") + 1]
                self.comments.append({"body": body})
            out = ""
        elif args[1:3] == ["api", "--paginate"]:
            out = json.dumps([self.comments])
        else:
            out = ""
        return subprocess.CompletedProcess(args, 0, out, "")


def task_was_closed(fake: FakeGh) -> bool:
    return ["tg", "update", "review-pr-42", "--status", "closed"] in fake.calls


def assert_unknown_family_refuses_before_mutation() -> None:
    fake = FakeGh()
    stderr = io.StringIO()
    with redirect_stderr(stderr):
        try:
            R.main([*BASE_ARGS[:5], "codxe", *BASE_ARGS[6:]], fake)
        except SystemExit as error:
            assert error.code == 2
        else:
            raise AssertionError("unknown family parsed successfully")
    assert "invalid choice" in stderr.getvalue()
    assert fake.calls == [], "unknown family reached GitHub"


def assert_positive_and_critical_paths() -> None:
    fake = FakeGh()
    assert R.main(BASE_ARGS, fake, {}) == 0
    comment = next(call for call in fake.calls if "comment" in call)
    body = comment[comment.index("--body") + 1]
    assert "APPROVED-AT: codex " + HEAD in body
    assert "dispatch attestation does not prove the executing process family" in body
    assert comment[1:7] == ["--who", "hermit-coord", "--team", "hermit2", "--role", "coordinator"]
    edit = next(call for call in fake.calls if call[1:3] == ["pr", "edit"])
    assert "adversarial-review-codex1" in edit and "passed-review-codex" in edit
    assert "post-facto-human-review" not in edit
    assert task_was_closed(fake), "canonically bound review did not close its task"

    critical = FakeGh()
    args = [*BASE_ARGS, "--critical", "hermit-syscall"]
    assert R.main(args, critical, {}) == 0
    edit = next(call for call in critical.calls if call[1:3] == ["pr", "edit"])
    assert "post-facto-human-review" in edit


def assert_missing_label_and_head_drift_refuse() -> None:
    missing = FakeGh(missing={"passed-review-codex"})
    assert R.main(BASE_ARGS, missing, {}) == 1
    assert all("comment" not in call and call[1:3] != ["pr", "edit"] for call in missing.calls)

    drift = FakeGh(heads=[HEAD, "b" * 40])
    assert R.main(BASE_ARGS, drift, {}) == 1
    assert any("comment" in call for call in drift.calls)
    assert all(call[1:3] != ["pr", "edit"] for call in drift.calls)
    assert not task_was_closed(drift)


def assert_comment_success_without_authority_refuses() -> None:
    fake = FakeGh(persist_comment=False)
    assert R.main(BASE_ARGS, fake, {}) == 1
    assert any("comment" in call for call in fake.calls), "fixture did not reach comment rc=0"
    assert all(call[1:3] != ["pr", "edit"] for call in fake.calls)
    assert not task_was_closed(fake), "missing PR authority closed the review task"


def assert_task_note_only_cannot_complete_lane() -> None:
    fake = FakeGh(
        persist_comment=False,
        task_notes=[f"APPROVED-AT: codex {HEAD}"],
    )
    assert R.main(BASE_ARGS, fake, {}) == 1
    assert fake.task_notes, "fixture lost its TaskGraph-only approval"
    assert fake.comments == [], "fixture unexpectedly persisted PR authority"
    assert not task_was_closed(fake), "TaskGraph note alone closed the review task"


def assert_named_reviewer_cannot_self_attest() -> None:
    fake = FakeGh()
    args = [
        value if value != "hermit-coord" else "codex-2"
        for value in BASE_ARGS
    ]
    assert R.main(args, fake, {}) == 1
    assert fake.calls == [], "named reviewer reached GitHub on self-attestation"


def assert_family_validation_mutation_is_observable() -> None:
    """A permissive family mutation reaches the exact GitHub write we guard."""

    mutant = R.parser()
    family = next(action for action in mutant._actions if action.dest == "family")
    assert set(family.choices) == {"codex", "claude"}
    family.choices = None
    R.FAMILY_LABELS["codxe"] = "passed-review-codex"
    try:
        fake = FakeGh()
        fake.labels.add("adversarial-review-codxe1")
        args = mutant.parse_args([*BASE_ARGS[:5], "codxe", *BASE_ARGS[6:]])
        try:
            R.execute(args, fake, {})
        except RuntimeError as error:
            assert "did not bind the requested family" in str(error)
        else:
            raise AssertionError("unknown family passed canonical authority readback")
        assert any("comment" in call for call in fake.calls), (
            "mutation did not reach the guarded GitHub write"
        )
        assert not task_was_closed(fake)
    finally:
        del R.FAMILY_LABELS["codxe"]


def main() -> None:
    assert_unknown_family_refuses_before_mutation()
    assert_positive_and_critical_paths()
    assert_missing_label_and_head_drift_refuse()
    assert_comment_success_without_authority_refuses()
    assert_task_note_only_cannot_complete_lane()
    assert_named_reviewer_cannot_self_attest()
    assert_family_validation_mutation_is_observable()
    print(
        "review-attest: unknown-family-refusal=1/1 positive=1/1 critical=1/1 "
        "missing-label-refusal=1/1 head-drift-refusal=1/1 "
        "comment-no-readback-refusal=1/1 task-note-only-refusal=1/1 "
        "reviewer-self-attestation-refusal=1/1 mutation=1/1"
    )


if __name__ == "__main__":
    main()
