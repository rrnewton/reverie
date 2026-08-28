#!/usr/bin/env python3
"""Typed rows for the nightly stress CSV and JSONL store."""

from __future__ import annotations

import re
import uuid
from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from typing import Any, Iterable, Mapping


SCHEMA_VERSION = 1
SHA_RE = re.compile(r"^[0-9a-f]{7,40}$")
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
RUN_ID_RE = re.compile(r"^[0-9a-f]{32}$")

CSV_COLUMNS = (
    "sha",
    "short",
    "build_s",
    "burst_N",
    "hangs",
    "passes",
    "other",
    "hang_rate",
    "status",
)


class StoreError(RuntimeError):
    """The stress-run store or one of its producer rows is malformed."""


class StressVerdict(str, Enum):
    CLEAN = "CLEAN"
    FLAKY = "FLAKY"
    FAILING = "FAILING"
    ERROR = "ERROR"


class BurstStatus(str, Enum):
    OK = "OK"
    WT_FAIL = "WT_FAIL"
    BUILD_FAIL = "BUILD_FAIL"
    NOBIN = "NOBIN"
    NOTEST = "NOTEST"
    MATCHED_MISSING = "MATCHED_MISSING"
    CALIB_MISSING = "CALIB_MISSING"
    PROBE_FAIL = "PROBE_FAIL"
    CALIB_UNDERPOWERED = "CALIB_UNDERPOWERED"
    NOWIRE = "NOWIRE"
    NOOUTPUT = "NOOUTPUT"


def _object(value: Any, context: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise StoreError(f"{context}: expected an object")
    return value


def _require_keys(
    value: Mapping[str, Any], required: set[str], optional: set[str], context: str
) -> None:
    missing = sorted(required - value.keys())
    if missing:
        raise StoreError(f"{context}: missing field {missing[0]!r}")
    unknown = sorted(value.keys() - required - optional)
    if unknown:
        raise StoreError(f"{context}: unknown field {unknown[0]!r}")


def _string(value: Any, field: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value):
        condition = "a string" if allow_empty else "a non-empty string"
        raise StoreError(f"{field}: expected {condition}")
    return value


def _optional_string(value: Any, field: str) -> str | None:
    if value is None:
        return None
    return _string(value, field)


def _integer(value: Any, field: str, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise StoreError(f"{field}: expected an integer >= {minimum}")
    return value


def _optional_integer(value: Any, field: str, *, minimum: int = 0) -> int | None:
    if value is None:
        return None
    return _integer(value, field, minimum=minimum)


def _boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise StoreError(f"{field}: expected a boolean")
    return value


def _timestamp(value: Any, field: str) -> str:
    text = _string(value, field)
    try:
        datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError as error:
        raise StoreError(f"{field}: invalid ISO-8601 timestamp {text!r}") from error
    return text


def _parse_optional_csv_integer(value: str, field: str) -> int | None:
    if value == "":
        return None
    try:
        parsed = int(value)
    except ValueError as error:
        raise StoreError(f"{field}: expected an integer, got {value!r}") from error
    if parsed < 0:
        raise StoreError(f"{field}: expected an integer >= 0, got {parsed}")
    return parsed


def _status(value: Any, field: str) -> BurstStatus:
    text = _string(value, field)
    try:
        return BurstStatus(text)
    except ValueError as error:
        allowed = ", ".join(status.value for status in BurstStatus)
        raise StoreError(f"{field}: unknown status {text!r}; expected one of {allowed}") from error


def _verdict(value: Any, field: str) -> StressVerdict:
    text = _string(value, field)
    try:
        return StressVerdict(text)
    except ValueError as error:
        allowed = ", ".join(verdict.value for verdict in StressVerdict)
        raise StoreError(f"{field}: unknown verdict {text!r}; expected one of {allowed}") from error


@dataclass(frozen=True)
class BurstRow:
    sha: str
    short: str
    build_s: str
    burst_N: int | None
    hangs: int | None
    passes: int | None
    other: int | None
    hang_rate: str
    status: BurstStatus

    @property
    def ok(self) -> bool:
        return self.status is BurstStatus.OK

    @classmethod
    def from_csv_cells(cls, cells: list[str], line_number: int) -> "BurstRow":
        if len(cells) != len(CSV_COLUMNS):
            raise StoreError(
                f"burst CSV row {line_number}: expected {len(CSV_COLUMNS)} columns, "
                f"got {len(cells)}"
            )
        raw = {name: cell.strip() for name, cell in zip(CSV_COLUMNS, cells)}
        try:
            status = BurstStatus(raw["status"])
        except ValueError as error:
            allowed = ", ".join(status.value for status in BurstStatus)
            raise StoreError(
                f"burst CSV row {line_number}.status: unknown status "
                f"{raw['status']!r}; expected one of {allowed}"
            ) from error
        row = cls(
            sha=raw["sha"],
            short=raw["short"],
            build_s=raw["build_s"],
            burst_N=_parse_optional_csv_integer(
                raw["burst_N"], f"burst CSV row {line_number}.burst_N"
            ),
            hangs=_parse_optional_csv_integer(
                raw["hangs"], f"burst CSV row {line_number}.hangs"
            ),
            passes=_parse_optional_csv_integer(
                raw["passes"], f"burst CSV row {line_number}.passes"
            ),
            other=_parse_optional_csv_integer(
                raw["other"], f"burst CSV row {line_number}.other"
            ),
            hang_rate=raw["hang_rate"],
            status=status,
        )
        row._validate(f"burst CSV row {line_number}")
        return row

    @classmethod
    def from_dict(cls, value: Any, context: str) -> "BurstRow":
        obj = _object(value, context)
        required = set(CSV_COLUMNS) | {"ok"}
        _require_keys(obj, required, set(), context)
        row = cls(
            sha=_string(obj["sha"], f"{context}.sha", allow_empty=True),
            short=_string(obj["short"], f"{context}.short", allow_empty=True),
            build_s=_string(obj["build_s"], f"{context}.build_s", allow_empty=True),
            burst_N=_optional_integer(obj["burst_N"], f"{context}.burst_N"),
            hangs=_optional_integer(obj["hangs"], f"{context}.hangs"),
            passes=_optional_integer(obj["passes"], f"{context}.passes"),
            other=_optional_integer(obj["other"], f"{context}.other"),
            hang_rate=_string(
                obj["hang_rate"], f"{context}.hang_rate", allow_empty=True
            ),
            status=_status(obj["status"], f"{context}.status"),
        )
        row._validate(context)
        if _boolean(obj["ok"], f"{context}.ok") != row.ok:
            raise StoreError(
                f"{context}.ok: does not agree with status {row.status.value!r}"
            )
        return row

    def _validate(self, context: str) -> None:
        _string(self.sha, f"{context}.sha")
        _string(self.short, f"{context}.short", allow_empty=True)
        _string(self.build_s, f"{context}.build_s", allow_empty=True)
        _string(self.hang_rate, f"{context}.hang_rate", allow_empty=True)
        if not isinstance(self.status, BurstStatus):
            raise StoreError(f"{context}.status: expected BurstStatus")
        _optional_integer(self.burst_N, f"{context}.burst_N")
        _optional_integer(self.hangs, f"{context}.hangs")
        _optional_integer(self.passes, f"{context}.passes")
        _optional_integer(self.other, f"{context}.other")
        if not SHA_RE.fullmatch(self.sha):
            raise StoreError(f"{context}.sha: invalid git SHA {self.sha!r}")
        if self.build_s:
            _parse_optional_csv_integer(self.build_s, f"{context}.build_s")
        if self.burst_N is not None and self.burst_N == 0:
            raise StoreError(f"{context}.burst_N: expected an integer >= 1")

        counts = (self.hangs, self.passes, self.other)
        if self.ok:
            if self.burst_N is None:
                raise StoreError(f"{context}.burst_N: required when status is OK")
            for name, value in zip(("hangs", "passes", "other"), counts):
                if value is None:
                    raise StoreError(f"{context}.{name}: required when status is OK")
            total = sum(value for value in counts if value is not None)
            if total != self.burst_N:
                raise StoreError(
                    f"{context}: hangs + passes + other = {total}, "
                    f"but burst_N = {self.burst_N}"
                )
            try:
                rate = float(self.hang_rate)
            except ValueError as error:
                raise StoreError(
                    f"{context}.hang_rate: expected a decimal when status is OK"
                ) from error
            if not 0.0 <= rate <= 1.0:
                raise StoreError(f"{context}.hang_rate: expected a value from 0 to 1")
            decimals = len(self.hang_rate.partition(".")[2]) if "." in self.hang_rate else 0
            tolerance = 0.5 * 10 ** (-decimals) + 1e-12
            expected = (self.hangs or 0) / self.burst_N
            if abs(rate - expected) > tolerance:
                raise StoreError(
                    f"{context}.hang_rate: {rate} does not describe "
                    f"{self.hangs}/{self.burst_N} within its printed precision"
                )
        else:
            if any(value is not None for value in counts):
                raise StoreError(
                    f"{context}: non-OK status {self.status.value!r} must not carry counts"
                )
            if self.hang_rate:
                raise StoreError(
                    f"{context}.hang_rate: non-OK status {self.status.value!r} "
                    "must not carry a rate"
                )

    def to_dict(self) -> dict[str, Any]:
        return {
            "sha": self.sha,
            "short": self.short,
            "build_s": self.build_s,
            "burst_N": self.burst_N,
            "hangs": self.hangs,
            "passes": self.passes,
            "other": self.other,
            "hang_rate": self.hang_rate,
            "status": self.status.value,
            "ok": self.ok,
        }


@dataclass(frozen=True)
class StressSummary:
    verdict: StressVerdict
    alarm: bool
    bursts: int
    bursts_ok: int
    bursts_error: int
    error_tokens: tuple[str, ...]
    total_instances: int
    hangs: int
    passes: int
    others: int
    hang_rate: float | None

    @classmethod
    def from_rows(cls, rows: Iterable[BurstRow]) -> "StressSummary":
        rows = tuple(rows)
        ok_rows = tuple(row for row in rows if row.ok)
        bad_rows = tuple(row for row in rows if not row.ok)
        hangs = sum(row.hangs or 0 for row in ok_rows)
        passes = sum(row.passes or 0 for row in ok_rows)
        others = sum(row.other or 0 for row in ok_rows)
        total = hangs + passes + others

        if any(row.passes == 0 and ((row.hangs or 0) or (row.other or 0)) for row in ok_rows):
            verdict = StressVerdict.FAILING
        elif any((row.hangs or 0) or (row.other or 0) for row in ok_rows):
            verdict = StressVerdict.FLAKY
        elif not ok_rows or bad_rows:
            verdict = StressVerdict.ERROR
        else:
            verdict = StressVerdict.CLEAN

        return cls(
            verdict=verdict,
            alarm=verdict is not StressVerdict.CLEAN,
            bursts=len(rows),
            bursts_ok=len(ok_rows),
            bursts_error=len(bad_rows),
            error_tokens=tuple(sorted({row.status.value for row in bad_rows})),
            total_instances=total,
            hangs=hangs,
            passes=passes,
            others=others,
            hang_rate=round(hangs / total, 4) if total else None,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "verdict": self.verdict.value,
            "alarm": self.alarm,
            "bursts": self.bursts,
            "bursts_ok": self.bursts_ok,
            "bursts_error": self.bursts_error,
            "error_tokens": list(self.error_tokens),
            "total_instances": self.total_instances,
            "hangs": self.hangs,
            "passes": self.passes,
            "others": self.others,
            "hang_rate": self.hang_rate,
        }


@dataclass(frozen=True)
class StressRunRecord:
    schema_version: int
    run_id: str
    trigger: str
    repo: str
    git_sha: str
    git_short: str
    workload: str
    backend: str
    width: int | None
    timeout_s: int | None
    host: str
    agent: str
    started_at: str
    finished_at: str
    source_tool: str | None
    summary: StressSummary
    rows: tuple[BurstRow, ...]
    extra: dict[str, Any] | None = None

    @classmethod
    def build(
        cls,
        *,
        rows: Iterable[BurstRow],
        repo: str,
        git_sha: str,
        workload: str,
        width: int | None,
        timeout_s: int | None,
        backend: str,
        host: str,
        agent: str,
        started_at: str,
        finished_at: str,
        source_tool: str | None,
        trigger: str,
        extra: dict[str, Any] | None = None,
    ) -> "StressRunRecord":
        rows = tuple(rows)
        record = cls(
            schema_version=SCHEMA_VERSION,
            run_id=uuid.uuid4().hex,
            trigger=trigger,
            repo=repo,
            git_sha=git_sha,
            git_short=git_sha[:12],
            workload=workload,
            backend=backend,
            width=width,
            timeout_s=timeout_s,
            host=host,
            agent=agent,
            started_at=started_at,
            finished_at=finished_at,
            source_tool=source_tool,
            summary=StressSummary.from_rows(rows),
            rows=rows,
            extra=extra,
        )
        record._validate("stress run record")
        return record

    @classmethod
    def from_dict(cls, value: Any, context: str = "stress run record") -> "StressRunRecord":
        obj = _object(value, context)
        if "schema_version" not in obj:
            raise StoreError(f"{context}: missing field 'schema_version'")
        schema_version = _integer(obj["schema_version"], f"{context}.schema_version")
        if schema_version != SCHEMA_VERSION:
            raise StoreError(
                f"{context}.schema_version: unsupported value {schema_version}; "
                f"expected {SCHEMA_VERSION}"
            )
        summary_fields = set(StressSummary.__dataclass_fields__)
        required = {
            "schema_version",
            "run_id",
            "trigger",
            "repo",
            "git_sha",
            "git_short",
            "workload",
            "backend",
            "width",
            "timeout_s",
            "host",
            "agent",
            "started_at",
            "finished_at",
            "source_tool",
            "rows",
        } | summary_fields
        _require_keys(obj, required, {"extra"}, context)
        raw_rows = obj["rows"]
        if not isinstance(raw_rows, list):
            raise StoreError(f"{context}.rows: expected an array")
        rows = tuple(
            BurstRow.from_dict(row, f"{context}.rows[{index}]")
            for index, row in enumerate(raw_rows)
        )
        summary = StressSummary.from_rows(rows)
        record = cls(
            schema_version=schema_version,
            run_id=_string(obj["run_id"], f"{context}.run_id"),
            trigger=_string(obj["trigger"], f"{context}.trigger"),
            repo=_string(obj["repo"], f"{context}.repo"),
            git_sha=_string(obj["git_sha"], f"{context}.git_sha"),
            git_short=_string(obj["git_short"], f"{context}.git_short"),
            workload=_string(obj["workload"], f"{context}.workload"),
            backend=_string(obj["backend"], f"{context}.backend"),
            width=_optional_integer(obj["width"], f"{context}.width", minimum=1),
            timeout_s=_optional_integer(
                obj["timeout_s"], f"{context}.timeout_s", minimum=1
            ),
            host=_string(obj["host"], f"{context}.host"),
            agent=_string(obj["agent"], f"{context}.agent"),
            started_at=_timestamp(obj["started_at"], f"{context}.started_at"),
            finished_at=_timestamp(obj["finished_at"], f"{context}.finished_at"),
            source_tool=_optional_string(obj["source_tool"], f"{context}.source_tool"),
            summary=summary,
            rows=rows,
            extra=(
                dict(_object(obj["extra"], f"{context}.extra"))
                if "extra" in obj
                else None
            ),
        )
        record._validate(context)
        record._require_summary(obj, context)
        return record

    def _validate(self, context: str) -> None:
        _string(self.run_id, f"{context}.run_id")
        _string(self.trigger, f"{context}.trigger")
        _string(self.repo, f"{context}.repo")
        _string(self.git_sha, f"{context}.git_sha")
        _string(self.git_short, f"{context}.git_short")
        _string(self.workload, f"{context}.workload")
        _string(self.backend, f"{context}.backend")
        _optional_integer(self.width, f"{context}.width", minimum=1)
        _optional_integer(self.timeout_s, f"{context}.timeout_s", minimum=1)
        _string(self.host, f"{context}.host")
        _string(self.agent, f"{context}.agent")
        _timestamp(self.started_at, f"{context}.started_at")
        _timestamp(self.finished_at, f"{context}.finished_at")
        _optional_string(self.source_tool, f"{context}.source_tool")
        if self.extra is not None and not isinstance(self.extra, dict):
            raise StoreError(f"{context}.extra: expected an object")
        if self.schema_version != SCHEMA_VERSION:
            raise StoreError(
                f"{context}.schema_version: unsupported value {self.schema_version}; "
                f"expected {SCHEMA_VERSION}"
            )
        if not RUN_ID_RE.fullmatch(self.run_id):
            raise StoreError(f"{context}.run_id: expected 32 lowercase hex characters")
        if not REPO_RE.fullmatch(self.repo):
            raise StoreError(f"{context}.repo: expected OWNER/REPO")
        if not SHA_RE.fullmatch(self.git_sha):
            raise StoreError(f"{context}.git_sha: invalid git SHA {self.git_sha!r}")
        if self.git_short != self.git_sha[:12]:
            raise StoreError(
                f"{context}.git_short: {self.git_short!r} does not match git_sha"
            )
        for index, row in enumerate(self.rows):
            row._validate(f"{context}.rows[{index}]")
            if row.sha != self.git_sha:
                raise StoreError(
                    f"{context}.rows[{index}].sha: {row.sha!r} does not match git_sha"
                )
            if self.width is not None and row.burst_N is not None and row.burst_N != self.width:
                raise StoreError(
                    f"{context}.rows[{index}].burst_N: {row.burst_N} does not match "
                    f"record width {self.width}"
                )

    def _require_summary(self, obj: Mapping[str, Any], context: str) -> None:
        expected = self.summary.to_dict()
        for field, value in expected.items():
            actual = obj[field]
            if field == "verdict":
                actual = _verdict(actual, f"{context}.{field}").value
            elif field == "alarm":
                actual = _boolean(actual, f"{context}.{field}")
            elif field == "error_tokens":
                if not isinstance(actual, list) or not all(
                    isinstance(token, str) for token in actual
                ):
                    raise StoreError(f"{context}.error_tokens: expected an array of strings")
            elif field == "hang_rate":
                if actual is not None and (
                    isinstance(actual, bool) or not isinstance(actual, (int, float))
                ):
                    raise StoreError(f"{context}.hang_rate: expected a number or null")
                if actual is not None:
                    actual = float(actual)
            else:
                actual = _integer(actual, f"{context}.{field}")
            if actual != value:
                raise StoreError(
                    f"{context}.{field}: stored value {actual!r} does not match "
                    f"rows-derived value {value!r}"
                )

    def to_dict(self) -> dict[str, Any]:
        record = {
            "schema_version": self.schema_version,
            "run_id": self.run_id,
            "trigger": self.trigger,
            "repo": self.repo,
            "git_sha": self.git_sha,
            "git_short": self.git_short,
            "workload": self.workload,
            "backend": self.backend,
            "width": self.width,
            "timeout_s": self.timeout_s,
            "host": self.host,
            "agent": self.agent,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "source_tool": self.source_tool,
            **self.summary.to_dict(),
            "rows": [row.to_dict() for row in self.rows],
        }
        if self.extra is not None:
            record["extra"] = self.extra
        return record
