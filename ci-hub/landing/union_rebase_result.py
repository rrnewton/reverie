#!/usr/bin/env python3
"""Strict serialized result shared by union-rebase.sh and land-pr.sh."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
from enum import Enum
import json
import os
from pathlib import Path
import re
import sys
from typing import Any, Mapping


SCHEMA_VERSION = 1
SHA40 = re.compile(r"^[0-9a-f]{40}$")


class UnionRebaseResultError(ValueError):
    pass


class Outcome(str, Enum):
    CLEAN = "clean"
    UNIONED = "unioned"
    HUMAN = "human"
    LINT_FAIL = "lint-fail"
    REBASE_FAIL = "rebase-fail"


_DISPLAY = {
    Outcome.CLEAN: "CLEAN",
    Outcome.UNIONED: "UNIONED",
    Outcome.HUMAN: "HUMAN",
    Outcome.LINT_FAIL: "LINT-FAIL",
    Outcome.REBASE_FAIL: "REBASE-FAIL",
}
_SUCCESS = {Outcome.CLEAN, Outcome.UNIONED}


def require_complete_outcome_mapping() -> None:
    missing = set(Outcome) - set(_DISPLAY)
    extra = set(_DISPLAY) - set(Outcome)
    if missing or extra:
        raise UnionRebaseResultError(
            "union-rebase-result-outcome mapping is incomplete: "
            f"missing={sorted(value.value for value in missing)}, "
            f"extra={sorted(value.value for value in extra)}"
        )


require_complete_outcome_mapping()


@dataclass(frozen=True)
class UnionRebaseResult:
    schema_version: int
    branch: str
    outcome: Outcome
    detail: str | None
    base_sha: str | None
    input_sha: str | None
    fork_sha: str | None
    result_sha: str | None
    pushed: bool
    exit_code: int

    @classmethod
    def from_mapping(cls, raw: Mapping[str, Any]) -> "UnionRebaseResult":
        expected = {
            "schema_version",
            "branch",
            "outcome",
            "detail",
            "base_sha",
            "input_sha",
            "fork_sha",
            "result_sha",
            "pushed",
            "exit_code",
        }
        missing = expected - set(raw)
        unknown = set(raw) - expected
        if missing:
            raise UnionRebaseResultError(
                "union-rebase-result-missing-field: " + ", ".join(sorted(missing))
            )
        if unknown:
            raise UnionRebaseResultError(
                "union-rebase-result-unknown-field: " + ", ".join(sorted(unknown))
            )
        if raw["schema_version"] != SCHEMA_VERSION:
            raise UnionRebaseResultError(
                "union-rebase-result-schema_version: "
                f"expected {SCHEMA_VERSION}, got {raw['schema_version']!r}"
            )
        branch = raw["branch"]
        if not isinstance(branch, str) or not branch or "\n" in branch:
            raise UnionRebaseResultError("union-rebase-result-branch")
        try:
            outcome = Outcome(raw["outcome"])
        except (TypeError, ValueError) as error:
            raise UnionRebaseResultError(
                f"union-rebase-result-outcome: {raw['outcome']!r}"
            ) from error
        detail = raw["detail"]
        if detail is not None and (not isinstance(detail, str) or not detail):
            raise UnionRebaseResultError("union-rebase-result-detail")
        shas = {
            field: _optional_sha(field, raw[field])
            for field in ("base_sha", "input_sha", "fork_sha", "result_sha")
        }
        pushed = raw["pushed"]
        if type(pushed) is not bool:
            raise UnionRebaseResultError("union-rebase-result-pushed")
        exit_code = raw["exit_code"]
        if type(exit_code) is not int or not 0 <= exit_code <= 255:
            raise UnionRebaseResultError("union-rebase-result-exit_code")

        if outcome in _SUCCESS:
            if detail is not None:
                raise UnionRebaseResultError(
                    "union-rebase-result-detail: successful result must not carry failure detail"
                )
            for field, value in shas.items():
                if value is None:
                    raise UnionRebaseResultError(
                        f"union-rebase-result-{field}: successful result requires the SHA"
                    )
            if exit_code != 0:
                raise UnionRebaseResultError(
                    "union-rebase-result-exit_code: successful result requires zero"
                )
        elif detail is None:
            raise UnionRebaseResultError(
                "union-rebase-result-detail: refused result requires a reason"
            )
        if pushed and outcome not in _SUCCESS:
            raise UnionRebaseResultError(
                "union-rebase-result-pushed: refused result cannot claim publication"
            )

        return cls(
            schema_version=SCHEMA_VERSION,
            branch=branch,
            outcome=outcome,
            detail=detail,
            base_sha=shas["base_sha"],
            input_sha=shas["input_sha"],
            fork_sha=shas["fork_sha"],
            result_sha=shas["result_sha"],
            pushed=pushed,
            exit_code=exit_code,
        )

    @property
    def successful(self) -> bool:
        return self.outcome in _SUCCESS

    def display(self) -> str:
        prefix = _DISPLAY[self.outcome]
        return prefix if self.detail is None else f"{prefix}:{self.detail}"

    def to_mapping(self) -> dict[str, Any]:
        raw = asdict(self)
        raw["outcome"] = self.outcome.value
        return raw


def _optional_sha(field: str, value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or not SHA40.fullmatch(value):
        raise UnionRebaseResultError(f"union-rebase-result-{field}")
    return value


def read_result(path: Path) -> UnionRebaseResult:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise UnionRebaseResultError(
            f"union-rebase-result-read: {path}: {error}"
        ) from error
    except json.JSONDecodeError as error:
        raise UnionRebaseResultError(
            f"union-rebase-result-json: {error.msg}"
        ) from error
    if not isinstance(raw, dict):
        raise UnionRebaseResultError("union-rebase-result-shape: expected object")
    return UnionRebaseResult.from_mapping(raw)


def write_result(path: Path, result: UnionRebaseResult) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        with temporary.open("w", encoding="utf-8") as output:
            json.dump(result.to_mapping(), output, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def _none_if_empty(value: str) -> str | None:
    return value or None


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    write = subparsers.add_parser("write")
    write.add_argument("--path", type=Path, required=True)
    write.add_argument("--branch", required=True)
    write.add_argument("--outcome", choices=[value.value for value in Outcome], required=True)
    write.add_argument("--detail", default="")
    write.add_argument("--base-sha", default="")
    write.add_argument("--input-sha", default="")
    write.add_argument("--fork-sha", default="")
    write.add_argument("--result-sha", default="")
    write.add_argument("--pushed", choices=("true", "false"), required=True)
    write.add_argument("--exit-code", type=int, required=True)
    require = subparsers.add_parser("require-success")
    require.add_argument("--path", type=Path, required=True)
    require.add_argument("--expect-branch", required=True)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "write":
            result = UnionRebaseResult.from_mapping(
                {
                    "schema_version": SCHEMA_VERSION,
                    "branch": args.branch,
                    "outcome": args.outcome,
                    "detail": _none_if_empty(args.detail),
                    "base_sha": _none_if_empty(args.base_sha),
                    "input_sha": _none_if_empty(args.input_sha),
                    "fork_sha": _none_if_empty(args.fork_sha),
                    "result_sha": _none_if_empty(args.result_sha),
                    "pushed": args.pushed == "true",
                    "exit_code": args.exit_code,
                }
            )
            write_result(args.path, result)
            return 0

        result = read_result(args.path)
        if result.branch != args.expect_branch:
            raise UnionRebaseResultError(
                "union-rebase-result-branch: "
                f"expected {args.expect_branch!r}, got {result.branch!r}"
            )
        print(result.display())
        return 0 if result.successful else 3
    except UnionRebaseResultError as error:
        print(str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
