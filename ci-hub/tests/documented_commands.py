#!/usr/bin/env python3
"""Extract and exercise every fenced shell command in the ci-hub READMEs."""

from __future__ import annotations

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

ROOT = Path(__file__).resolve().parents[2]
DOCS = (
    ROOT / "ci-hub/README.md",
    ROOT / "ci-hub/directives/README.md",
    ROOT / "ci-hub/landing/README.md",
    ROOT / "ci-hub/containers/README.md",
)
EXPECTED_COMMANDS = 56
FENCE = re.compile(r"^```(?P<language>[A-Za-z0-9_-]*)\s*$")
FATAL_OUTPUT = (
    "gh auth login",
    "Traceback (most recent call last):",
    "ModuleNotFoundError:",
    "No such file or directory",
    "unrecognized subcommand",
    "unexpected argument",
    "Could not execute cargo",
)


class DocsCommandError(RuntimeError):
    """A documented command could not be classified or exercised."""


class DocsUnverifiable(RuntimeError):
    """A probe COULD NOT MEASURE, and nobody declared that it could not.

    Three-valued, matching ``ci-hub/taskgraph/unowned_backlog.py``: rc 0 is
    measured-and-clean, rc 1 is measured-and-failing, rc 2 is could-not-measure.
    Before this existed the third state was spelled ``SKIP`` and was counted in
    ``probes=`` exactly like a pass, so a probe that measured NOTHING reported
    the same headline as one that measured everything.
    """


#: Probes that genuinely cannot be measured somewhere must be DECLARED here,
#: with the reason and with what would make them measurable again. A declared
#: could-not-measure is still PRINTED and still counted under ``unverifiable=``
#: -- it just does not fail the run. An UNDECLARED one is rc 2.
#:
#: The rule this encodes: you may be unable to measure something, but you may
#: not be SILENT about it, and you may not discover it by accident.
DECLARED_UNMEASURABLE: dict[str, str] = {
    "quickstart tg": (
        "`tg quickstart` is not a subcommand of tg and never has been. tg's root "
        "command takes a positional `[TASK]...`, so `tg quickstart` parses as the "
        "default mission-control view focused on a task named 'quickstart' and "
        "opens the database -- which is why this probe read as an impurity. "
        "dev-hermit b3995f3 added `ci-hub quickstart` and a probe for a symmetric "
        "`tg quickstart` in one commit, but the tg half lives in fbcode/orc/tg and "
        "was never written: `grep -rn quickstart --include=*.rs` over that tree "
        "returns zero hits. Measurable again once fbcode/orc/tg grows a "
        "database-free `quickstart` that prints the markers below; at that point "
        "DELETE this entry -- test_declared_unmeasurable_entries_are_still_true "
        "fails if a declared probe starts passing."
    ),
}


def _unverifiable(name: str, detail: str) -> str:
    """Record a could-not-measure. Declared -> a counted report; else rc 2."""
    reason = DECLARED_UNMEASURABLE.get(name)
    if reason is None:
        raise DocsUnverifiable(
            f"{name}: {detail}. This probe measured nothing and no declaration "
            f"in DECLARED_UNMEASURABLE says it could not. Either fix the probe or "
            f"declare -- with a reason -- that it cannot run here."
        )
    return f"UNVERIFIABLE {name}: {detail} -- declared: {reason}"


@dataclass(frozen=True)
class DocumentedCommand:
    path: Path
    line: int
    text: str
    mode: str

    @property
    def label(self) -> str:
        return f"{self.path.relative_to(ROOT)}:{self.line}"


def _logical_commands(lines: list[tuple[int, str]]) -> Iterable[tuple[int, str]]:
    pending: list[str] = []
    start = 0
    for line_number, line in lines:
        stripped = line.strip()
        if not pending and (not stripped or stripped.startswith("#")):
            continue
        if not pending:
            start = line_number
        pending.append(line)
        if line.rstrip().endswith("\\"):
            continue
        command = "\n".join(pending).strip()
        pending = []
        if command:
            yield start, command
    if pending:
        yield start, "\n".join(pending).strip()


def _classify(text: str) -> str:
    normalized = " ".join(text.replace("\\\n", " ").split())
    if normalized.startswith("cd "):
        return "setup"
    if re.match(
        r"^(?:CI_HUB_DOCS_PARSE_ONLY=1\s+)?(?:\./)?ci-hub/landing/land-pr\.sh\s",
        normalized,
    ):
        return "parse"
    match = re.match(r"^(?:\./)?ci-hub/ci-hub\s+(\S+)", normalized)
    if match:
        command = match.group(1)
        if command in {"fresh", "health", "hosted-status", "runner-health"}:
            return "live-read"
        if command in {
            "help",
            "quickstart",
            "history",
            "local-history",
        }:
            return "local-read"
        if command == "land-lock" and re.search(r"\bland-lock\s+status\b", normalized):
            return "local-read"
        if command in {
            "land-lock",
            "refresh-history",
            "verify-landing",
            "validate-run",
            "compatibility-website",
        }:
            return "parse"
        raise DocsCommandError(f"unclassified ci-hub subcommand: {normalized}")
    if normalized.startswith("./ci-hub/bin/close-task "):
        return "parse"
    if re.match(r"^(?:\./)?ci-hub/bin/reconcile-receipts(?:\s|$)", normalized):
        return "live-read"
    # The landing preflight is illustrated with <placeholder> arguments, so it is
    # parse-only: the snippet documents the three checks, it is not run verbatim.
    if normalized.startswith("python3 ci-hub/landing/preflight.py"):
        return "parse"
    # Same shape as the preflight snippet above: the coalesce-guard example is
    # written with a `<you>` placeholder and a literal `...` standing in for the
    # constituent PR list, so it documents the invocation rather than being a
    # command anyone runs verbatim. Parse-only is the truthful classification --
    # calling it live-read would assert it executes, which it cannot.
    if normalized.startswith("python3 ci-hub/landing/coalesce_guard.py"):
        return "parse"
    if normalized == "./ci-hub/directives/check.py --quickstart":
        return "local-read"
    if normalized.startswith("with-proxy gh "):
        return "live-read"
    if normalized.startswith(
        'git fetch origin "+refs/pull/$PR/head:refs/pr-drain/$PR"'
    ):
        return "parse"
    if normalized.startswith('git cherry origin/main "refs/pr-drain/$PR"'):
        return "parse"
    match = re.match(r"^(?:\./)?scripts/agent-podman\.rs\s+(\S+)", normalized)
    if match:
        return "local-read" if match.group(1) == "quickstart" else "parse"
    if normalized.startswith("systemd-run --user "):
        return "parse"
    if normalized.startswith("systemctl --user status "):
        return "parse"
    if _safe_find_arguments(normalized) is not None:
        return "parse"
    # manual-run STARTS GitHub Actions runs. Every documented form but --list
    # would dispatch real work against a real commit, so the whole family is
    # parse-only. --list is in the family rather than exempted as a live-read
    # because it needs with-proxy, which a GitHub-hosted runner does not have:
    # a probe that refuses there measures the runner, not the tool.
    if re.match(r"^(?:\./)?ci-hub/bin/manual-run(?:\s|$)", normalized):
        return "parse"
    raise DocsCommandError(f"unclassified shell command: {normalized}")


def extract_commands(paths: Iterable[Path] = DOCS) -> list[DocumentedCommand]:
    commands: list[DocumentedCommand] = []
    for path in paths:
        inside = False
        shell = False
        block: list[tuple[int, str]] = []
        for line_number, line in enumerate(path.read_text().splitlines(), 1):
            marker = FENCE.match(line)
            if marker:
                if not inside:
                    inside = True
                    shell = marker.group("language") in {"bash", "sh", "shell"}
                    block = []
                else:
                    if shell:
                        _check_shell_syntax(path, block)
                        for start, text in _logical_commands(block):
                            commands.append(
                                DocumentedCommand(path, start, text, _classify(text))
                            )
                    inside = False
                    shell = False
                    block = []
                continue
            if inside and shell:
                block.append((line_number, line))
        if inside:
            raise DocsCommandError(f"{path}: unterminated fenced block")
    if len(commands) != EXPECTED_COMMANDS:
        raise DocsCommandError(
            f"extracted {len(commands)} commands, expected {EXPECTED_COMMANDS}; "
            "classify and account for every new or removed documented invocation"
        )
    return commands


def _check_shell_syntax(path: Path, block: list[tuple[int, str]]) -> None:
    source = "\n".join(line for _, line in block) + "\n"
    result = subprocess.run(
        ["bash", "-n"], input=source, text=True, capture_output=True, check=False
    )
    if result.returncode != 0:
        start = block[0][0] if block else 0
        raise DocsCommandError(
            f"{path}:{start}: invalid shell: {result.stderr.strip()}"
        )


def _render(text: str) -> str:
    return (
        text.replace("PARENT_HEAD", "a" * 40)
        .replace("REPAIR_SHA", "a" * 40)
        .replace("PR_NUMBER", os.environ.get("CI_HUB_DOCS_MERGED_PR", "1278"))
    )


def _is_cost_line(line: str) -> bool:
    # Canonical form is `# <tool> tool COST ESTIMATE ...` / `# <tool> tool COST ACTUAL ...`
    # (see ci-hub/TOOL-COST-CONVENTION.md): a `# ` prefix marks it meta-output.
    return line.startswith("# ") and (
        " tool COST ESTIMATE " in line or " tool COST ACTUAL " in line
    )


def _business_output(output: str) -> str:
    return "\n".join(
        line for line in output.splitlines() if line.strip() and not _is_cost_line(line)
    )


def _safe_find_arguments(command: str) -> list[str] | None:
    normalized = " ".join(command.replace("\\\n", " ").split())
    try:
        arguments = shlex.split(normalized)
    except ValueError:
        return None
    if (
        len(arguments) == 5
        and arguments[:3] == ["find", "ignored/ci-hub/landing", "-name"]
        and re.fullmatch(r"[A-Za-z0-9*?._-]+", arguments[3])
        and arguments[4] == "-print"
    ):
        return arguments
    return None


def _external_help(command: str) -> str:
    normalized = " ".join(command.replace("\\\n", " ").split())
    if normalized.startswith("with-proxy gh api "):
        return "gh api --help"
    return "gh pr view --help"


def _parse_probe(command: str, *, find_root: Path | None = None) -> str:
    normalized = " ".join(command.replace("\\\n", " ").split())
    website_prefix = "./ci-hub/ci-hub compatibility-website"
    if normalized.startswith(website_prefix):
        # Unlike other opaque passthrough commands, ci-hub forwards this exact
        # argument list under CI_HUB_DOCS_PARSE_ONLY so the owning Python parser
        # validates the documented mode and options before returning.
        return normalized
    if normalized.startswith("systemd-run --user "):
        return "systemd-run --help"
    if normalized.startswith("systemctl --user status "):
        return "systemctl --help"
    find_arguments = _safe_find_arguments(normalized)
    if find_arguments is not None:
        if find_root is None:
            raise DocsCommandError("safe find parse probe requires an empty directory")
        find_arguments[1] = str(find_root)
        return shlex.join(find_arguments)
    if normalized.startswith("./ci-hub/bin/close-task "):
        return "./ci-hub/bin/close-task --help"
    if re.match(r"^(?:\./)?ci-hub/bin/reconcile-receipts(?:\s|$)", normalized):
        return "./ci-hub/bin/reconcile-receipts --help"
    if normalized.startswith("python3 ci-hub/landing/preflight.py"):
        return "python3 ci-hub/landing/preflight.py --help"
    # Classifying a snippet "parse" is only half the job: parse-mode still RUNS
    # something, and without an entry here that something is the snippet
    # VERBATIM. The coalesce-guard example carries a `<you>` placeholder and a
    # literal `...` standing in for the constituent PR list, so running it
    # verbatim dies in argparse ("invalid int value: '...'", exit 2) and the
    # step reports a documented-command failure for a doc that is correct.
    # That is precisely what happened after 3e9c299 added the _classify entry
    # without this one.
    if normalized.startswith("python3 ci-hub/landing/coalesce_guard.py"):
        return "python3 ci-hub/landing/coalesce_guard.py --help"
    if normalized.startswith(
        'git fetch origin "+refs/pull/$PR/head:refs/pr-drain/$PR"'
    ):
        return "git fetch --help"
    if normalized.startswith('git cherry origin/main "refs/pr-drain/$PR"'):
        return "git cherry --help"
    if re.match(r"^(?:\./)?ci-hub/bin/manual-run(?:\s|$)", normalized):
        return "./ci-hub/bin/manual-run --help"
    return command


def _workspace_state(root: Path, *, include_ignored: bool) -> str:
    arguments = ["git", "status", "--porcelain=v1", "--untracked-files=all"]
    if include_ignored:
        arguments.append("--ignored=matching")
    result = subprocess.run(
        arguments,
        cwd=root,
        text=True,
        capture_output=True,
        check=True,
    )
    return result.stdout


def _tracked_mtimes(root: Path) -> dict[str, int]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        capture_output=True,
        check=True,
    )
    mtimes: dict[str, int] = {}
    for raw_path in result.stdout.split(b"\0"):
        if not raw_path:
            continue
        relative = os.fsdecode(raw_path)
        path = root / relative
        if path.is_file() or path.is_symlink():
            mtimes[relative] = path.lstat().st_mtime_ns
    return mtimes


def _changed_mtimes(before: dict[str, int], after: dict[str, int]) -> list[str]:
    return sorted(
        path
        for path in before.keys() | after.keys()
        if before.get(path) != after.get(path)
    )


def _run_one(
    command: DocumentedCommand,
    *,
    root: Path,
    environment: dict[str, str],
    live: bool,
    verify_purity: bool,
) -> list[str]:
    rendered = _render(command.text)
    run_environment = environment.copy()
    executed = rendered
    allowed = {0}
    find_probe: tempfile.TemporaryDirectory[str] | None = None
    if command.mode == "parse" or (command.mode == "live-read" and not live):
        run_environment["CI_HUB_DOCS_PARSE_ONLY"] = "1"
        if command.mode == "parse":
            if _safe_find_arguments(rendered) is not None:
                find_probe = tempfile.TemporaryDirectory(
                    prefix="ci-hub-documented-find-"
                )
                executed = _parse_probe(rendered, find_root=Path(find_probe.name))
            else:
                executed = _parse_probe(rendered)
        elif rendered.startswith("with-proxy gh "):
            executed = _external_help(rendered)
        else:
            executed = _parse_probe(rendered)
    elif command.mode == "live-read":
        # ci-hub read commands report their VERDICT through the exit code, so a
        # nonzero status here is data, not a probe failure: `hosted-status`
        # alone answers 0 GREEN / 3 RED / 4 NO_RESULT, and it is documented with
        # a placeholder SHA that necessarily dereferences to NO_RESULT. The old
        # {0, 1, 2} allowlist therefore failed the step for a command that had
        # worked perfectly. This never showed up on main because this whole
        # live step sits behind the coalesce-guard red and had never once
        # executed. Crashes are still caught: a Rust panic is 101, a missing
        # binary 127, and any traceback trips FATAL_OUTPUT above.
        allowed = (
            {0, 1, 2, 3, 4} if re.match(r"^(?:\./)?ci-hub/ci-hub\s", rendered) else {0}
        )

    timeout = 120 if live else 45
    before = _workspace_state(root, include_ignored=True) if verify_purity else ""
    before_mtimes = _tracked_mtimes(root) if verify_purity else {}
    try:
        result = subprocess.run(
            executed,
            cwd=root,
            env=run_environment,
            shell=True,
            executable="/bin/bash",
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise DocsCommandError(
            f"{command.label}: {command.mode} command exceeded {timeout}s: {executed}"
        ) from error
    finally:
        if find_probe is not None:
            find_probe.cleanup()
    output = result.stdout + result.stderr
    fatal = next((marker for marker in FATAL_OUTPUT if marker in output), None)
    if fatal is not None:
        raise DocsCommandError(
            f"{command.label}: fatal diagnostic {fatal!r}\n"
            f"command: {executed}\noutput:\n{output}"
        )
    if result.returncode not in allowed:
        raise DocsCommandError(
            f"{command.label}: exit {result.returncode}, expected {sorted(allowed)}\n"
            f"command: {executed}\noutput:\n{output}"
        )
    if (
        command.mode != "setup"
        and not _business_output(output)
        and _safe_find_arguments(rendered) is None
    ):
        raise DocsCommandError(
            f"{command.label}: silent success (no domain output)\ncommand: {executed}"
        )
    reports = [
        f"PASS {command.mode:10} {command.label} exit={result.returncode} "
        f"output={len(_business_output(output).splitlines())} line(s)"
    ]

    if verify_purity:
        after = _workspace_state(root, include_ignored=True)
        changed_mtimes = _changed_mtimes(before_mtimes, _tracked_mtimes(root))
        if after != before or changed_mtimes:
            raise DocsCommandError(
                f"{command.label}: command mutated a clean checkout\n"
                f"command: {executed}\nstatus before:\n{before}\nstatus after:\n{after}"
                f"\ntracked mtimes changed: {changed_mtimes or 'none'}"
            )
    return reports


def _tg_exposes_quickstart(binary: str) -> bool:
    """Does this tg actually HAVE a `quickstart` subcommand?

    Read out of the `Commands:` block ONLY. tg's help opens with several
    paragraphs of prose and a worked example, so a substring match over the
    whole text would cheerfully "find" a subcommand that does not exist -- and
    the point of this function is to stop us asserting against a fiction.
    Absent binary or unreadable help both answer False: cannot tell IS no.
    """
    try:
        result = subprocess.run(
            [binary, "--help"],
            text=True,
            capture_output=True,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    in_commands = False
    for line in (result.stdout + result.stderr).splitlines():
        if re.match(r"^\s*(?:Commands|SUBCOMMANDS):\s*$", line):
            in_commands = True
            continue
        if not in_commands:
            continue
        if not line.strip():
            continue
        if not line.startswith((" ", "\t")):
            break  # dedent: the Commands block ended
        if re.match(r"^\s+quickstart\b", line):
            return True
    return False


def _run_tg_quickstart(
    binary: str,
    *,
    root: Path,
    environment: dict[str, str],
    verify_purity: bool,
) -> list[str]:
    """Exercise the source-owned TaskGraph primer without opening its database."""
    before = _workspace_state(root, include_ignored=True) if verify_purity else ""
    before_mtimes = _tracked_mtimes(root) if verify_purity else {}
    with tempfile.TemporaryDirectory(prefix="tg-quickstart-") as temporary:
        home = Path(temporary)
        run_environment = environment | {
            "HOME": str(home),
            "TG_DB_PATH": str(home / "must-not-exist.db"),
        }
        try:
            result = subprocess.run(
                [binary, "quickstart"],
                cwd=root,
                env=run_environment,
                text=True,
                capture_output=True,
                timeout=15,
                check=False,
            )
        except subprocess.TimeoutExpired as error:
            raise DocsCommandError("tg quickstart exceeded 15s") from error
        output = result.stdout + result.stderr
        if result.returncode != 0:
            raise DocsCommandError(
                f"tg quickstart exited {result.returncode}\noutput:\n{output}"
            )
        business = _business_output(output)
        required = (
            "TaskGraph agent quickstart",
            "tg claim TASK_ID",
            "tg note TASK_ID",
            "TG_DB_PATH",
        )
        missing = [marker for marker in required if marker not in business]
        if missing:
            raise DocsCommandError(
                f"tg quickstart is missing agent workflow markers {missing}\noutput:\n{output}"
            )
        created = sorted(str(path.relative_to(home)) for path in home.rglob("*"))
        if created:
            raise DocsCommandError(
                "tg quickstart mutated its isolated HOME: " + ", ".join(created)
            )
    if verify_purity:
        after = _workspace_state(root, include_ignored=True)
        changed_mtimes = _changed_mtimes(before_mtimes, _tracked_mtimes(root))
        if after != before or changed_mtimes:
            raise DocsCommandError(
                "tg quickstart mutated the checkout\n"
                f"status before:\n{before}\nstatus after:\n{after}"
                f"\ntracked mtimes changed: {changed_mtimes or 'none'}"
            )
    return [
        "PASS quickstart tg external-source exit=0 "
        f"output={len(_business_output(output).splitlines())} line(s) purity=verified"
    ]


def _evaluate_closeout(
    *,
    head: str,
    origin_main: str,
    unpushed: int,
    dirty: str,
    dirty_note: str | None,
) -> list[str]:
    if unpushed:
        raise DocsCommandError(
            f"closeout refused: HEAD {head} has {unpushed} commit(s) not on "
            f"origin/main {origin_main}; commit and push before handoff"
        )
    note = (dirty_note or "").strip()
    if dirty and len(note) < 20:
        raise DocsCommandError(
            "closeout refused: parent is dirty; commit/push task-owned work or pass "
            "--dirty-note with an explicit ownership/reason statement\n" + dirty
        )
    if not dirty and note:
        raise DocsCommandError(
            "closeout refused: --dirty-note was supplied but the parent is clean; "
            "remove the stale exception"
        )
    reports = [f"CLOSEOUT PUSHED: head={head} origin_main={origin_main} unpushed=0"]
    if dirty:
        reports.append("CLOSEOUT DIRTY PATHS:\n" + dirty.rstrip())
        reports.append("CLOSEOUT DIRTY NOTE: " + note)
    else:
        reports.append("CLOSEOUT WORKTREE: clean")
    return reports


def closeout_guard(*, root: Path, dirty_note: str | None) -> list[str]:
    fetch = subprocess.run(
        ["with-proxy", "git", "fetch", "--quiet", "origin", "main"],
        cwd=root,
        text=True,
        capture_output=True,
        timeout=90,
        check=False,
    )
    if fetch.returncode != 0:
        raise DocsCommandError(
            "closeout refused: cannot refresh origin/main through with-proxy\n"
            + fetch.stdout
            + fetch.stderr
        )

    def git_output(*arguments: str) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=root,
            text=True,
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            raise DocsCommandError(
                f"closeout refused: git {' '.join(arguments)} failed\n"
                + result.stdout
                + result.stderr
            )
        return result.stdout.strip()

    return _evaluate_closeout(
        head=git_output("rev-parse", "HEAD"),
        origin_main=git_output("rev-parse", "origin/main"),
        unpushed=int(git_output("rev-list", "--count", "origin/main..HEAD")),
        dirty=git_output("status", "--porcelain=v1", "--untracked-files=all"),
        dirty_note=dirty_note,
    )


def run(*, root: Path = ROOT, live: bool = False) -> list[str]:
    commands = extract_commands(
        (
            root / "ci-hub/README.md",
            root / "ci-hub/directives/README.md",
            root / "ci-hub/landing/README.md",
            root / "ci-hub/containers/README.md",
        )
    )
    reports: list[str] = []
    verify_purity = not _workspace_state(root, include_ignored=False).strip()
    with tempfile.TemporaryDirectory(prefix="ci-hub-docs-") as temporary:
        state = Path(temporary)
        home = state / "home"
        checkout = home / "work/dev-hermit"
        checkout.parent.mkdir(parents=True)
        checkout.symlink_to(root, target_is_directory=True)
        environment = os.environ.copy()
        original_home = Path(environment.get("HOME", str(Path.home())))
        environment.update(
            {
                "HOME": str(home),
                "CARGO_HOME": environment.get(
                    "CARGO_HOME", str(original_home / ".cargo")
                ),
                "RUSTUP_HOME": environment.get(
                    "RUSTUP_HOME", str(original_home / ".rustup")
                ),
                "XDG_CACHE_HOME": environment.get(
                    "XDG_CACHE_HOME", str(original_home / ".cache")
                ),
                "GH_CONFIG_DIR": environment.get(
                    "GH_CONFIG_DIR", str(original_home / ".config/gh")
                ),
                "DEV_HERMIT_PARENT": str(root),
                "CI_HUB_LANDING_LOCK": str(state / "landing.lock"),
                "CI_HUB_MAIN_HEALTH_TIMEOUT": "4",
                "CI_HUB_MAIN_HEALTH_DEADLINE": "8",
                "CI_HUB_PR_STATUS_TIMEOUT": "8",
                "CI_HUB_PR_STATUS_DEADLINE": "12",
                "PYTHONDONTWRITEBYTECODE": "1",
            }
        )
        for command in commands:
            reports.extend(
                _run_one(
                    command,
                    root=root,
                    environment=environment,
                    live=live,
                    verify_purity=verify_purity,
                )
            )
        # Three outcomes, never two. Absent tool and missing interface are both
        # "could not measure" -- they are NOT a pass, and the old `SKIP` string
        # made them indistinguishable from one.
        tg_binary = os.environ.get("CI_HUB_TG_BIN") or shutil.which("tg")
        if not tg_binary:
            reports.append(
                _unverifiable(
                    "quickstart tg",
                    "tg is not on PATH (it is a large fbsource binary, absent from "
                    "ubuntu-latest, so GH Actions can never measure this)",
                )
            )
        elif not _tg_exposes_quickstart(tg_binary):
            reports.append(
                _unverifiable(
                    "quickstart tg",
                    f"{tg_binary} exposes no `quickstart` subcommand, so there is no "
                    "primer here whose purity could be measured",
                )
            )
        else:
            reports.extend(
                _run_tg_quickstart(
                    tg_binary,
                    root=root,
                    environment=environment,
                    verify_purity=verify_purity,
                )
            )
    reports.append(
        "PASS checkout-purity "
        + ("verified-no-writes" if verify_purity else "skipped-dirty-input")
    )
    return reports


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--live",
        action="store_true",
        help="execute networked read-only examples instead of their pure help/parse probes",
    )
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--list", action="store_true")
    parser.add_argument(
        "--closeout",
        action="store_true",
        help="require pushed HEAD and clean parent, or an explicit dirty-tree note",
    )
    parser.add_argument(
        "--dirty-note",
        help="explicit ownership/reason for concurrent parent dirt at closeout",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.closeout:
            for report in closeout_guard(
                root=args.root.resolve(), dirty_note=args.dirty_note
            ):
                print(report)
            return 0
        if args.dirty_note:
            raise DocsCommandError("--dirty-note requires --closeout")
        if args.list:
            for command in extract_commands(
                (
                    args.root / "ci-hub/README.md",
                    args.root / "ci-hub/landing/README.md",
                    args.root / "ci-hub/containers/README.md",
                )
            ):
                print(f"{command.mode:10} {command.label} {shlex.join([command.text])}")
            return 0
        reports = run(root=args.root.resolve(), live=args.live)
    except DocsCommandError as error:
        print(f"DOCUMENTED COMMAND FAILURE: {error}", file=sys.stderr)
        return 1
    except DocsUnverifiable as error:
        # rc 2, not 1 and not 0: measuring nothing is neither a pass nor a
        # failure, and collapsing it into either is how a hole stays invisible.
        print("state=unverifiable", file=sys.stderr)
        print(f"summary={error}", file=sys.stderr)
        return 2
    for report in reports:
        print(report)
    # `probes=` counts what was MEASURED. An unverifiable is reported next to
    # it, never inside it -- the old code did `probes=len(reports)` with the
    # SKIP string in that list, so a probe that measured nothing inflated the
    # headline exactly like one that passed.
    unverifiable = [r for r in reports if r.startswith("UNVERIFIABLE ")]
    print(
        f"DOCUMENTED COMMANDS: PASS commands={EXPECTED_COMMANDS} "
        f"probes={len(reports) - len(unverifiable)} "
        f"unverifiable={len(unverifiable)} live={str(args.live).lower()}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
