#!/usr/bin/env python3
"""Reject Rust control flow that classifies typed errors by display strings."""

from __future__ import annotations

import argparse
import bisect
import dataclasses
import pathlib
import re
import subprocess
import sys
from collections.abc import Iterable, Sequence


VERSION = "1.0"
EXCLUDED_PARTS = frozenset({".git", "target", "third-party", "vendor"})
TOKEN_RE = re.compile(
    r"[A-Za-z_][A-Za-z0-9_]*|==|!=|=>|&&|\|\||::|[{}()\[\].,;:?=<>!+*/&|-]"
)
RAW_STRING_START_RE = re.compile(r"(?:br|r)(?P<hashes>#{0,255})\"")
RECEIVER_BOUNDARIES = frozenset(
    {";", "{", "}", ",", "=>", "==", "!=", "&&", "||", "="}
)
COMPARISON_BOUNDARIES = frozenset({";", "{", "}", ",", "=>", "&&", "||", "="})


@dataclasses.dataclass(frozen=True)
class Token:
    value: str
    start: int
    end: int


@dataclasses.dataclass(frozen=True)
class Finding:
    path: pathlib.Path
    line: int
    column: int
    kind: str


def _mask_non_code(source: str) -> str:
    """Preserve code offsets while blanking comments and string literals."""

    chars = list(source)
    out = list(source)
    i = 0
    block_depth = 0
    state = "code"
    raw_hashes = 0

    def blank(index: int) -> None:
        if out[index] != "\n":
            out[index] = " "

    while i < len(chars):
        if state == "line-comment":
            blank(i)
            if chars[i] == "\n":
                state = "code"
            i += 1
            continue

        if state == "block-comment":
            if source.startswith("/*", i):
                blank(i)
                blank(i + 1)
                block_depth += 1
                i += 2
            elif source.startswith("*/", i):
                blank(i)
                blank(i + 1)
                block_depth -= 1
                i += 2
                if block_depth == 0:
                    state = "code"
            else:
                blank(i)
                i += 1
            continue

        if state == "string":
            blank(i)
            if chars[i] == "\\" and i + 1 < len(chars):
                blank(i + 1)
                i += 2
            elif chars[i] == '"':
                state = "code"
                i += 1
            else:
                i += 1
            continue

        if state == "raw-string":
            terminator = '"' + ("#" * raw_hashes)
            if source.startswith(terminator, i):
                for offset in range(len(terminator)):
                    blank(i + offset)
                i += len(terminator)
                state = "code"
            else:
                blank(i)
                i += 1
            continue

        if source.startswith("//", i):
            blank(i)
            blank(i + 1)
            state = "line-comment"
            i += 2
        elif source.startswith("/*", i):
            blank(i)
            blank(i + 1)
            state = "block-comment"
            block_depth = 1
            i += 2
        else:
            raw = RAW_STRING_START_RE.match(source, i)
            if raw:
                raw_hashes = len(raw.group("hashes"))
                for offset in range(len(raw.group(0))):
                    blank(i + offset)
                i += len(raw.group(0))
                state = "raw-string"
            elif source.startswith('b"', i):
                blank(i)
                blank(i + 1)
                i += 2
                state = "string"
            elif chars[i] == '"':
                blank(i)
                i += 1
                state = "string"
            else:
                i += 1

    return "".join(out)


def _tokens(source: str) -> list[Token]:
    masked = _mask_non_code(source)
    return [Token(match.group(0), match.start(), match.end()) for match in TOKEN_RE.finditer(masked)]


def _is_error_name(value: str) -> bool:
    lowered = value.lower()
    return lowered in {"err", "error", "unwrap_err", "expect_err"} or lowered.endswith(
        ("_err", "_error")
    )


def _error_string_calls(tokens: Sequence[Token]) -> list[tuple[int, int]]:
    calls: list[tuple[int, int]] = []
    for index in range(len(tokens) - 3):
        if [token.value for token in tokens[index : index + 4]] != [".", "to_string", "(", ")"]:
            continue
        lower_bound = max(-1, index - 40)
        cursor = index - 1
        while cursor > lower_bound and tokens[cursor].value not in RECEIVER_BOUNDARIES:
            cursor -= 1
        receiver = tokens[cursor + 1 : index]
        if any(_is_error_name(token.value) for token in receiver):
            calls.append((index, index + 3))
    return calls


def _condition_spans(tokens: Sequence[Token]) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    for index, token in enumerate(tokens):
        if token.value not in {"if", "while", "match"}:
            continue
        paren = bracket = 0
        for cursor in range(index + 1, len(tokens)):
            value = tokens[cursor].value
            if value == "(":
                paren += 1
            elif value == ")":
                paren = max(0, paren - 1)
            elif value == "[":
                bracket += 1
            elif value == "]":
                bracket = max(0, bracket - 1)
            elif paren == 0 and bracket == 0 and value in {"{", "=>"}:
                spans.append((index + 1, cursor))
                break
    return spans


def _is_direct_comparison(tokens: Sequence[Token], call_start: int, call_end: int) -> bool:
    left = call_start - 1
    while left >= 0 and tokens[left].value not in COMPARISON_BOUNDARIES:
        if tokens[left].value in {"==", "!="}:
            return True
        left -= 1

    right = call_end + 1
    while right < len(tokens) and tokens[right].value not in COMPARISON_BOUNDARIES:
        if tokens[right].value in {"==", "!="}:
            return True
        right += 1
    return False


def _is_map_err_conversion(tokens: Sequence[Token], call_start: int) -> bool:
    """Recognize formatting used only to translate a Result error for propagation."""

    window = [token.value for token in tokens[max(0, call_start - 16) : call_start]]
    if "map_err" not in window:
        return False
    map_index = len(window) - 1 - window[::-1].index("map_err")
    suffix = window[map_index + 1 :]
    if suffix.count("|") < 2:
        return False

    # THE CLOSURE'S BINDING IS THE SIGNAL, NOT ITS SPELLING. This used to ask
    # `any(_is_error_name(value) for value in suffix)`, which accepts err,
    # error, and anything ending _err or _error -- but NOT `e`, the most
    # ordinary name in Rust for a bound error. So `map_err(|err| ...)` was
    # exempt and the byte-identical `map_err(|e| ...)` was reported, and a
    # pure rename flipped the verdict with no change in behaviour.
    #
    # Measured 2026-08-25: that produced all 3 findings the gate had against
    # hermit/ci/manifest-plan/src/runner.rs, none of which compares an error
    # string -- two are `if let Some(s) = child.try_wait().map_err(|e| ...)?`
    # and one is `if entry.file_type().map_err(|e| ...)?.is_dir()`. The `?`
    # propagates; the branch is on Some/None and on a bool. They turned the
    # PARENT's lint red for a submodule that had done nothing wrong.
    #
    # It survived because both existing exemption tests spell the binding
    # `|error|`, so the suite only ever exercised the name the rule happened
    # to accept.
    first = suffix.index("|")
    second = suffix.index("|", first + 1)
    inner = first + 1
    binding = [value for value in suffix[inner:second] if value.isidentifier()]
    if binding:
        # Exempt only when the thing rendered IS the bound error, whatever it
        # is called. `map_err(|e| other.to_string())` still reports, because
        # then the rendering is not the propagation conversion this allows for.
        return binding[-1] in suffix[second + 1 :]
    return any(_is_error_name(value) for value in suffix)


def scan_source(source: str, path: pathlib.Path = pathlib.Path("<memory>")) -> list[Finding]:
    tokens = _tokens(source)
    conditions = _condition_spans(tokens)
    newline_offsets = [index for index, char in enumerate(source) if char == "\n"]
    findings: list[Finding] = []

    for call_start, call_end in _error_string_calls(tokens):
        direct = _is_direct_comparison(tokens, call_start, call_end)
        conditional = any(start <= call_start < end for start, end in conditions) and not (
            _is_map_err_conversion(tokens, call_start)
        )
        if not direct and not conditional:
            continue

        offset = tokens[call_start].start
        line_index = bisect.bisect_right(newline_offsets, offset)
        line_start = newline_offsets[line_index - 1] + 1 if line_index else 0
        findings.append(
            Finding(
                path=path,
                line=line_index + 1,
                column=offset - line_start + 1,
                kind=(
                    "error display string compared instead of typed error"
                    if direct
                    else "error display string used as a control-flow condition"
                ),
            )
        )
    return findings


def _git_tracked_rust_files(root: pathlib.Path) -> list[pathlib.Path]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", "--", "*.rs"],
        check=True,
        capture_output=True,
    )
    relative_paths = [pathlib.Path(raw.decode()) for raw in result.stdout.split(b"\0") if raw]
    return [
        root / path
        for path in relative_paths
        if not any(part in EXCLUDED_PARTS for part in path.parts)
    ]


def collect_files(roots: Iterable[pathlib.Path]) -> tuple[list[pathlib.Path], list[tuple[pathlib.Path, int]]]:
    files: list[pathlib.Path] = []
    counts: list[tuple[pathlib.Path, int]] = []
    seen: set[pathlib.Path] = set()

    for root in roots:
        if not root.exists():
            raise ValueError(f"configured scan root does not exist: {root}")
        if root.is_file():
            candidates = [root] if root.suffix == ".rs" else []
        else:
            try:
                git_root = subprocess.run(
                    ["git", "-C", str(root), "rev-parse", "--show-toplevel"],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip()
            except subprocess.CalledProcessError as error:
                raise ValueError(f"configured scan root is not a Git worktree: {root}") from error
            if pathlib.Path(git_root).resolve() != root.resolve():
                raise ValueError(f"configured scan root is not a repository root: {root}")
            candidates = _git_tracked_rust_files(root)

        added = 0
        for candidate in candidates:
            resolved = candidate.resolve()
            if resolved in seen:
                continue
            seen.add(resolved)
            files.append(candidate)
            added += 1
        counts.append((root, added))

    return files, counts


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="reject Rust error classification based on display strings"
    )
    parser.add_argument("paths", nargs="*", default=["."], help="repository roots or Rust files")
    parser.add_argument("--version", action="version", version=f"%(prog)s {VERSION}")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    roots = [pathlib.Path(path) for path in args.paths]
    try:
        files, counts = collect_files(roots)
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"lint-rust-error-string-proxies: configuration error: {error}", file=sys.stderr)
        return 2

    findings: list[Finding] = []
    for path in files:
        try:
            findings.extend(scan_source(path.read_text(encoding="utf-8"), path))
        except (OSError, UnicodeError) as error:
            print(f"lint-rust-error-string-proxies: cannot read {path}: {error}", file=sys.stderr)
            return 2

    for finding in findings:
        print(f"{finding.path}:{finding.line}:{finding.column}: {finding.kind}", file=sys.stderr)
    coverage = ", ".join(f"{root}={count}" for root, count in counts)
    if findings:
        print(
            f"lint-rust-error-string-proxies: FAIL: {len(findings)} finding(s); "
            f"checked {len(files)} tracked Rust files ({coverage})",
            file=sys.stderr,
        )
        return 1

    print(
        f"lint-rust-error-string-proxies: ok: checked {len(files)} tracked Rust files "
        f"({coverage})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
