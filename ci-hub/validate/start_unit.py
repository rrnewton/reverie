#!/usr/bin/env python3
"""Launch one detached, admitted Hermit validation through ci-hub."""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import pane_owner
import run_registry
import service_result


ROOT = Path(__file__).resolve().parents[2]
HOST_TMP_ROOT = Path("/tmp").resolve()
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
UNIT_RE = re.compile(r"^validate-[A-Za-z0-9_.@:-]+$")
Runner = Callable[..., subprocess.CompletedProcess[str]]
TERMINAL_STATES = frozenset(("failed", "inactive"))


def require_guest_visible_root(path: Path, *, role: str) -> Path:
    """Refuse a program root hidden by Hermit's isolated guest ``/tmp``.

    Hermit deliberately replaces guest ``/tmp`` with an isolated directory.
    A fresh validation checkout below host ``/tmp`` therefore builds valid
    programs that Hermit then refuses to execute.  Resolve first so an
    apparently safe symlink cannot bypass the placement check.
    """
    resolved = path.resolve()
    try:
        resolved.relative_to(HOST_TMP_ROOT)
    except ValueError:
        return resolved
    raise ValueError(
        f"{role} resolves beneath host /tmp ({resolved}); Hermit isolates guest /tmp, "
        "so programs built there are not guest-visible. Use the canonical non-/tmp "
        "dev-hermit parent under your workspace root, or another non-/tmp checkout."
    )


def run_command(command: Sequence[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, text=True, capture_output=True, **kwargs)


def checked_output(
    command: Sequence[str], *, run: Runner, purpose: str
) -> str:
    result = run(list(command), check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"{purpose}: {detail}")
    return result.stdout.strip()


def sanitize_unit(raw: str) -> str:
    unit = raw.removesuffix(".service")
    if not UNIT_RE.fullmatch(unit):
        raise ValueError(
            f"invalid unit {raw!r}; expected validate- followed by letters, digits, or ._@:-"
        )
    return unit


def default_unit(agent: str, target: str) -> str:
    safe_agent = re.sub(r"[^A-Za-z0-9_.@:-]+", "-", agent).strip("-.")
    if not safe_agent:
        raise ValueError("--agent must contain at least one unit-safe character")
    return sanitize_unit(f"validate-{safe_agent}-{target[:12]}-{int(time.time())}")


# The row lookup below goes through the canonical union command. This module
# never opens a ledger shard itself.


def prepare_fresh_checkout(
    source: Path, target: str, *, run: Runner, parent: Path
) -> Path:
    """Materialize `target` into a private temp worktree and PROVE it is usable.

    WHY THIS IS THE DEFAULT. `validate_checkout` below refuses a dirty tree, so
    "no uncommitted changes" is already guaranteed — but `git status
    --porcelain=v1` says NOTHING ABOUT IGNORED FILES, and that is where the
    divergence lives. Measured 2026-08-08 on a slot at 393c6a765: `git status
    --porcelain=v1` reported 0 paths while the tree carried 5.1 GB of ignored
    build output, caches and materialized dependencies. A validate of that tree
    is a validate of the commit PLUS 5.1 GB of unrecorded local history, so the
    40-hex SHA on the receipt does not describe what actually ran.

    Two measured cases from the same day where ignored state DECIDED a verdict:
    an empty (gitignored) `hermit/agent-utils` made `scripts/validate.rs` die in
    0.045s in a way that reads like a fast pass; and a stale (gitignored)
    rust-script binary cache made a tree whose `git status` was empty fail
    `--self-test` on a mutation that had already been reverted.

    THE SECOND CASE IS ALSO THIS FUNCTION'S OWN FAILURE MODE. A fresh worktree
    starts with EMPTY submodules, `agent-utils` among them — so materializing
    the tree and launching without initializing them reproduces the 0.045s
    fake-pass BY CONSTRUCTION. That is why this returns only after proving the
    tree is usable, and raises otherwise: an unusable temp checkout must abort
    the launch, never quietly become a fast green.
    """
    parent = require_guest_visible_root(parent, role="fresh-checkout parent")
    fresh = require_guest_visible_root(
        Path(
            checked_output(
                ["mktemp", "-d", str(parent / "validate-fresh-XXXXXXXX")],
                run=run,
                purpose="cannot create temp checkout directory",
            )
        ),
        role="fresh checkout",
    )
    # A worktree, not a clone: it shares the source object store, so this costs
    # no object copy. It does register in the source repo, which is why the
    # caller removes it explicitly rather than just unlinking the directory.
    run(
        ["git", "-C", str(source), "worktree", "add", "--detach", str(fresh), target],
        check=True,
    )
    head = checked_output(
        ["git", "-C", str(fresh), "rev-parse", "HEAD^{commit}"],
        run=run,
        purpose="cannot resolve fresh checkout HEAD",
    )
    if head != target:
        raise RuntimeError(f"fresh checkout resolved to {head}, not requested target {target}")
    run(
        ["git", "-C", str(fresh), "submodule", "update", "--init", "--recursive"],
        check=False,
    )
    # PROVE usable, do not assume. Each of these is a thing whose absence has
    # produced a misleading fast exit rather than an honest failure.
    missing = [
        rel
        for rel in ("scripts/validate.rs", "agent-utils/rs/safe-ci-dag-runner/Cargo.toml")
        if not (fresh / rel).exists()
    ]
    if missing:
        raise RuntimeError(
            f"fresh checkout {fresh} is missing {', '.join(missing)}; refusing to launch a run "
            "that would exit fast for an environmental reason and read like a pass "
            "(retained for inspection)"
        )
    return fresh


def remove_fresh_checkout(source: Path, fresh: Path, *, run: Runner) -> None:
    """Remove the temp worktree AND deregister it. Best-effort, never fatal."""
    run(["git", "-C", str(source), "worktree", "remove", "--force", str(fresh)], check=False)
    run(["git", "-C", str(source), "worktree", "prune"], check=False)


def assert_row_readable_from_canonical_ledger(
    root: Path, target: str, cwd: Path, *, run: Runner
) -> str:
    """REQUIREMENT (a). Fail unless this run's exact row is readable canonically.

    A temp-dir validate that writes its receipt into the temp checkout's own
    ledger and then deletes the directory MANUFACTURES INVISIBLE GREENS, and
    would be strictly worse than validating the slot in place. This is not a
    hypothetical failure mode — it is the measured one. The admission audit
    (`ci-hub-admission-control-audit`) reproduced exactly 111 `validate.rs`
    fallback rows sitting in two per-checkout ledgers that default consumers
    discover ZERO of, and a full green at PR #1635 head 291a2fd6 (862 executed,
    0 failed) read as NOT-VALIDATED because its record went to a per-checkout
    shard. A field recorded only where it does not survive the checkout is not
    recorded at all.

    So the row is re-read THROUGH THE CANONICAL READER, BEFORE the temp
    directory is removed, and the binding is by IDENTITY rather than by a
    correlated proxy: the row must carry this exact 40-hex commit AND the `cwd`
    of this exact temp checkout. A row matching on commit alone could be some
    other run of the same SHA.
    """
    result = run(
        [str(root / "ci-hub/ci-hub"), "ledger", "qualified-rows"],
        cwd=root,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"canonical ledger unreadable ({detail})")
    matched = [
        line
        for line in result.stdout.splitlines()
        if target in line and str(cwd) in line
    ]
    if not matched:
        raise RuntimeError(
            f"no row for commit {target} with cwd {cwd} is readable from the canonical "
            "ledger; the receipt exists nowhere a consumer will look. Temp checkout "
            "RETAINED for inspection"
        )
    return matched[0]


# Directories a receipt is never written into and that are expensive to walk.
RECEIPT_SCAN_PRUNE = {".git", "target", "node_modules"}


def orphaned_receipt_locations(fresh: Path, *, run: Runner) -> list[str]:
    """Receipt files THIS RUN left inside the temp checkout, if any.

    The discriminator between two states the retention path used to conflate:

      * a receipt was produced but written where no consumer reads it -- the
        exact hazard requirement (a) exists to catch. The temp tree is then the
        ONLY trace the hazard occurred, so reclaiming it destroys the evidence.
        RETAIN.
      * the run failed before producing any receipt at all -- ordinary, and the
        common case, because the Rust validation driver is fail-fast and its first gate can
        abort in seconds. Nothing is preserved by keeping 26 MB of build tree.
        RECLAIM.

    THE TEST IS SHAPE, NOT NAME, and that choice is the safety-critical one. A
    whitelist of known ledger filenames fails in the DANGEROUS direction: a
    receipt written under a name nobody enumerated would read as "no evidence"
    and be deleted -- a disk-hygiene fix eating the evidence path. So this looks
    for any `*.jsonl` in the tree that git does not track. Measured before
    relying on it: hermit tracks ZERO `.jsonl` files, so anything found here was
    produced by this run. The tracked-ness check is kept regardless, so the
    property survives that changing.

    Reads no ledger CONTENT -- existence and tracked-ness are the whole test --
    so this module stays a non-reader under the ledger-reader allowlist.
    """
    candidates: list[str] = []
    for current, dirs, files in os.walk(fresh):
        dirs[:] = [d for d in dirs if d not in RECEIPT_SCAN_PRUNE]
        for name in files:
            if name.endswith(".jsonl"):
                candidates.append(str(Path(current, name).relative_to(fresh)))
    if not candidates:
        return []
    tracked = run(["git", "-C", str(fresh), "ls-files", "--", *candidates], check=False)
    known = set(tracked.stdout.split()) if tracked.returncode == 0 else set()
    return sorted(c for c in candidates if c not in known)


def owner_validate_symlink_is_exact(checkout: Path, dirty: str) -> bool:
    """The one owner-checkout exception; every property is deliberately fixed."""
    link = checkout / "validate"
    return (
        dirty.splitlines() == ["?? validate"]
        and link.is_symlink()
        and os.readlink(link) == "scripts/validate.rs"
    )


def validate_checkout(
    checkout: Path,
    target: str,
    *,
    run: Runner,
    allow_owner_validate_symlink: bool = False,
) -> Path:
    checkout = require_guest_visible_root(checkout, role="source checkout")
    if not SHA_RE.fullmatch(target):
        raise ValueError("--target must be an exact lowercase 40-hex commit SHA")
    if not (checkout / "scripts/validate.rs").is_file():
        raise ValueError(f"missing scripts/validate.rs in checkout {checkout}")

    top = Path(
        checked_output(
            ["git", "-C", str(checkout), "rev-parse", "--show-toplevel"],
            run=run,
            purpose="cannot resolve checkout root",
        )
    ).resolve()
    if top != checkout:
        raise ValueError(f"--checkout must name the repository root ({top}), not {checkout}")

    head = checked_output(
        ["git", "-C", str(checkout), "rev-parse", "HEAD^{commit}"],
        run=run,
        purpose="cannot resolve checkout HEAD",
    )
    if head != target:
        raise ValueError(f"checkout HEAD is {head}, not requested exact target {target}")

    dirty = checked_output(
        ["git", "-C", str(checkout), "status", "--porcelain=v1"],
        run=run,
        purpose="cannot inspect checkout cleanliness",
    )
    if dirty and not (
        allow_owner_validate_symlink
        and owner_validate_symlink_is_exact(checkout, dirty)
    ):
        first = dirty.splitlines()[0]
        raise ValueError(f"checkout is dirty ({first}); refusing unrepeatable validation")
    return checkout


def preflight(root: Path, checkout: Path, target: str, *, run: Runner) -> None:
    command = [
        str(root / "ci-hub/validate/preflight_validate.py"),
        "--head",
        target,
        "--repo-checkout",
        str(checkout),
    ]
    result = run(command, cwd=root, check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"validation admission refused target: {detail}")



def _prepend_path(existing: str, entry: str) -> str:
    """Put `entry` first in a colon-separated search path, without duplicating it."""
    parts = [p for p in existing.split(":") if p and p != entry]
    return ":".join([entry, *parts])



# Directories that may hold libunwind's SHARED objects at run time, best first.
# `ignored/lu-parity/usr/lib64` is deliberately last: it has the .pc and the
# static `libunwind-ptrace.a`, but no `libunwind-ptrace.so*`.
RUNTIME_CANDIDATES = (
    "~/fbsource/fbcode/third-party-buck/platform010/build/libunwind/lib",
    "~/.local/hermit-deps/lu/usr/lib64",
)


def _libunwind_runtime_dir(root: Path, link_dir: Path) -> Path:
    """Pick the directory that can actually satisfy the loader.

    `libunwind-ptrace.so.0` is THE file that matters, because pointing
    LIBRARY_PATH at a tree carrying a shared `libunwind-ptrace.so` is what makes
    the link depend on it in the first place (see build_systemd_command). A
    directory that has `libunwind.so.8` but NOT the ptrace variant cannot satisfy
    that dependency, so it must never be preferred over one that can.

    `link_dir` competes on equal terms rather than being a last resort. It used
    to be excluded on the belief that it ships only a static
    `libunwind-ptrace.a`; that is no longer true of `ignored/lu-parity`, which
    now carries the shared objects too and can satisfy build, link and run on its
    own. Excluding it meant that with the first candidate absent the probe
    selected `~/.local/hermit-deps/lu/usr/lib64` -- which has only the static
    `.a` -- on a `libunwind.so.8` tiebreak, producing exactly the
    `libunwind-ptrace.so.0: cannot open shared object file` failure this probe
    exists to prevent.

    Order is still candidates-then-link_dir, so on a host where a candidate has
    the ptrace object the selection is unchanged.
    """
    candidates = [Path(c).expanduser() for c in RUNTIME_CANDIDATES] + [link_dir]
    # First pass: only a directory that carries the object the link needs.
    for candidate in candidates:
        if (candidate / "libunwind-ptrace.so.0").exists():
            return candidate
    # Second pass: no ptrace object anywhere. A tree with the base library is
    # still the best available guess for a statically-linked-ptrace build.
    for candidate in candidates:
        if (candidate / "libunwind.so.8").exists():
            return candidate
    return link_dir


def build_systemd_command(
    *,
    root: Path,
    checkout: Path,
    target: str,
    agent: str,
    unit: str,
    log: Path,
    pr: int | None,
    validate_args: Sequence[str],
    wait: int,
    hold: int,
    child_deadline: int,
    environment: Mapping[str, str],
    make_validate: bool = False,
    allow_owner_validate_symlink: bool = False,
) -> list[str]:
    home = environment.get("HOME", "")
    path = environment.get("PATH", "")
    if not home or not path:
        raise ValueError("HOME and PATH must be set so cargo/rustup resolve inside the user unit")

    child = ["/usr/bin/env"]
    if pr is not None:
        child.append(f"PR_NUMBER={pr}")
    if allow_owner_validate_symlink:
        # Keep the executed command literally `make validate`. The product
        # driver already has a typed escape hatch whose receipt stays explicitly
        # unanchored; the owner watch interprets that distinct evidence rather
        # than laundering it into a clean canonical receipt.
        child.append("VALIDATE_RUN_ON_DIRTY_TREE=1")
    if make_validate:
        child.extend(["with-proxy", "make", "validate"])
    else:
        child.extend(["with-proxy", "./scripts/validate.rs", *(validate_args or ["full"])])

    # libunwind is not installed system-wide on this host class, so
    # `unwind-sys`'s build.rs panics on
    #   pkg-config --libs --cflags libunwind-ptrace
    # and every DAG lane that builds the workspace fails with an environment
    # fault that looks exactly like a product red. The .pc and .so live in the
    # repo; point the unit at them when they are present. Nothing is
    # substituted or weakened if they are absent -- the unit simply runs as it
    # did before and the build fails loudly, as it should.
    #
    # THREE variables are required, and they are NOT interchangeable -- getting
    # this wrong yields a DIFFERENT failure at a LATER node, which is why
    # propagating only the first and third still looked broken:
    #   PKG_CONFIG_PATH  build time. Without it unwind-sys's build.rs panics on
    #                    `pkg-config --libs --cflags libunwind-ptrace`.
    #   LIBRARY_PATH     LINK time. pkg-config emits `-lunwind-ptrace
    #                    -lunwind-generic -lunwind`, and the linker still has to
    #                    FIND those .so files. Without it the build gets past
    #                    build.rs and then dies with
    #                    `rust-lld: error: unable to find library -lunwind`
    #                    while linking reverie-liteinst.
    #   LD_LIBRARY_PATH  RUN time, for the produced binaries. It does NOT help
    #                    the link -- the loader is not the linker, and it must
    #                    NOT be assumed equal to LIBRARY_PATH. See below.
    #
    # THE LINK DIR AND THE RUNTIME DIR NEED NOT BE THE SAME DIRECTORY, which has
    # bitten twice, so the runtime directory is PROBED rather than assumed --
    # see `_libunwind_runtime_dir`. A future relocation is picked up by adding a
    # path to RUNTIME_CANDIDATES, not by rediscovering the failure.
    #
    # NOTE ON WHY LD_LIBRARY_PATH IS LOAD-BEARING AT ALL, since it is easy to
    # conclude it is redundant and delete it. Measured on this host class
    # (libunwind-devel installed system-wide, three units, one variable changed
    # per run):
    #   no LIBRARY_PATH, no LD_LIBRARY_PATH -> `-lunwind-ptrace` resolves to the
    #     system STATIC /usr/lib64/libunwind-ptrace.a, ldd shows no ptrace
    #     dependency, and the binary runs.
    #   LIBRARY_PATH only -> the link now prefers the SHARED
    #     `libunwind-ptrace.so` in the pointed-at tree, and the binary dies with
    #     `libunwind-ptrace.so.0: cannot open shared object file` because
    #     /usr/lib64 ships that library only as a static `.a`.
    # In other words LIBRARY_PATH is what CREATES the runtime dependency that
    # LD_LIBRARY_PATH then satisfies. The three are a set: propagate all of them
    # or none. This also means the probe must key on `libunwind-ptrace.so.0`
    # specifically, not on libunwind generally.
    lu_env: list[str] = []
    lu_root = root / "ignored/lu-parity/usr/lib64"
    if (lu_root / "pkgconfig/libunwind-ptrace.pc").exists():
        runtime = _libunwind_runtime_dir(root, lu_root)
        pkg_config = _prepend_path(
            environment.get("PKG_CONFIG_PATH", ""), str(lu_root / "pkgconfig")
        )
        library = _prepend_path(environment.get("LIBRARY_PATH", ""), str(lu_root))
        ld_library = _prepend_path(environment.get("LD_LIBRARY_PATH", ""), str(runtime))
        lu_env = [
            "--setenv",
            f"PKG_CONFIG_PATH={pkg_config}",
            "--setenv",
            f"LIBRARY_PATH={library}",
            "--setenv",
            f"LD_LIBRARY_PATH={ld_library}",
        ]

    return [
        "systemd-run",
        "--user",
        "--collect",
        "--unit",
        unit,
        "--description",
        f"ci-hub full validation {target[:12]} ({agent})",
        "--working-directory",
        str(checkout),
        "--setenv",
        f"HOME={home}",
        "--setenv",
        f"PATH={path}",
        "--setenv",
        "CI_HUB_VALIDATE_PRODUCER=systemd-user-v1",
        # DEV_HERMIT_PARENT remains available to non-ledger helpers. Receipt
        # storage does not use it: validate.rs invokes the fixed parent adapter,
        # which appends the tracked per-machine shard that ci-hub unions.
        "--setenv",
        f"DEV_HERMIT_PARENT={root}",
        *lu_env,
        "--property",
        f"StandardOutput=append:{log}",
        "--property",
        f"StandardError=append:{log}",
        str(root / "ci-hub/ci-hub"),
        "validate-lock",
        "run",
        "--agent",
        agent,
        "--kind",
        "validate",
        "--target",
        target,
        "--wait",
        str(wait),
        "--hold",
        str(hold),
        "--child-deadline",
        str(child_deadline),
        "--",
        *child,
    ]


def service_properties(
    unit: str, *, run: Runner
) -> dict[str, str] | None:
    result = run(
        [
            "systemctl",
            "--user",
            "show",
            f"{unit}.service",
            "--property=ActiveState",
            "--property=LoadState",
            "--property=SubState",
            "--property=ExecMainCode",
            "--property=ExecMainStatus",
            "--property=Result",
            "--property=InvocationID",
            "--no-pager",
        ],
        check=False,
    )
    if result.returncode != 0:
        return None
    properties = dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )
    # `systemctl show` succeeds even after --collect has unloaded the unit.  Its
    # synthetic not-found object says success/0 and is not execution evidence.
    if properties.get("LoadState") != "loaded" or not properties.get("InvocationID"):
        return None
    return properties


def wait_for_unit(
    unit: str,
    record: Path,
    *,
    run: Runner,
    poll_seconds: float,
    sleep: Callable[[float], None] = time.sleep,
) -> dict[str, Any]:
    seen = False
    missing = 0
    while True:
        properties = service_properties(unit, run=run)
        if properties is None:
            missing += 1
            try:
                durable = run_registry.read_record(record)
            except RuntimeError:
                durable = {}
            if service_result.is_evidenced_terminal(durable):
                return durable
            raw_log = durable.get("log")
            log = Path(raw_log) if isinstance(raw_log, str) else Path("/nonexistent")
            classified = service_result.from_log(log)
            if classified.get("exit_code") is not None or missing >= (5 if seen else 50):
                return {
                    **classified,
                    "finished_at": datetime.now(timezone.utc).isoformat(),
                }
            sleep(poll_seconds)
            continue
        seen = True
        if properties.get("ActiveState") in TERMINAL_STATES:
            try:
                durable = run_registry.read_record(record)
            except RuntimeError:
                durable = {}
            raw_log = durable.get("log")
            log = Path(raw_log) if isinstance(raw_log, str) else Path("/nonexistent")
            return {
                **service_result.from_log(log, properties=properties),
                "finished_at": datetime.now(timezone.utc).isoformat(),
            }
        sleep(poll_seconds)


def emit_report(report: Mapping[str, Any], *, json_output: bool) -> None:
    if json_output:
        print(json.dumps(dict(report), sort_keys=True), flush=True)
        return
    event = report.get("event", "state").upper()
    print(
        f"validate-run: {event} {report['unit']} target={report['target']} "
        f"state={report.get('state', 'unknown')}",
        flush=True,
    )
    if report.get("pane_id"):
        print(
            f"PANE workspace={report['workspace_id']} tab={report['tab_id']} "
            f"pane={report['pane_id']}",
            flush=True,
        )
    if report.get("log"):
        print(f"LOG {report['log']}", flush=True)


def attach(
    raw_unit: str,
    *,
    root: Path,
    run: Runner,
    json_output: bool,
    poll_seconds: float,
    sleep: Callable[[float], None],
) -> int:
    unit = sanitize_unit(raw_unit)
    record_path = run_registry.record_path(root, unit)
    record = run_registry.read_record(record_path)
    emit_report(
        {
            **record,
            "event": "attached",
            "unit": f"{unit}.service",
        },
        json_output=json_output,
    )
    final = wait_for_unit(
        unit,
        record_path,
        run=run,
        poll_seconds=poll_seconds,
        sleep=sleep,
    )
    updated = run_registry.update_record(record_path, **final)
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=json_output,
    )
    status = updated.get("exit_code")
    return status if isinstance(status, int) and 0 <= status <= 255 else 2


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description=(
            "Launch scripts/validate.rs as a detached systemd user service whose only admission "
            "path is ci-hub validate-lock."
        )
    )
    result.add_argument("--checkout", type=Path)
    result.add_argument(
        "--in-place",
        action="store_true",
        help=(
            "OPT-OUT: validate the --checkout working tree itself instead of a fresh "
            "temp-dir checkout of --target. The tree must still be clean and at the "
            "exact target, but its ignored state (build output, caches, materialized "
            "submodules) participates in the run, so the receipt's SHA describes the "
            "commit PLUS that unrecorded local state."
        ),
    )
    result.add_argument("--agent")
    result.add_argument("--target")
    result.add_argument("--pr", type=int)
    result.add_argument(
        "--attach",
        metavar="VALIDATE-UNIT",
        help="reattach to a durable validate-* handle without launching another run",
    )
    result.add_argument("--unit", help="validate-* unit name; .service suffix is optional")
    result.add_argument("--log", type=Path, help="durable log (default: ignored/validate/<unit>.log)")
    result.add_argument("--wait", type=int, default=7200, help="validate-lock queue wait bound")
    result.add_argument("--hold", type=int, default=1200, help="validate-lock lease seconds")
    result.add_argument("--child-deadline", type=int, default=3600)
    result.add_argument(
        "--make-validate",
        action="store_true",
        help=(
            "run the checkout's literal `make validate` target inside the admitted "
            "unit instead of invoking scripts/validate.rs directly. No validate "
            "arguments may follow --; this exists for owner-checkout spot checks."
        ),
    )
    result.add_argument(
        "--allow-owner-validate-symlink",
        action="store_true",
        help=(
            "narrow owner-watch exception: permit only an untracked `validate` "
            "symlink whose literal target is scripts/validate.rs"
        ),
    )
    result.add_argument("--dry-run", action="store_true")
    result.add_argument("--json", action="store_true")
    result.add_argument(
        "--caller-poll-seconds",
        type=float,
        default=1.0,
        help="poll cadence while the caller blocks on the detached service",
    )
    result.add_argument(
        "validate_args",
        nargs=argparse.REMAINDER,
        help="scripts/validate.rs arguments after -- (default: full)",
    )
    return result


def main(
    argv: Sequence[str] | None = None,
    *,
    run: Runner = run_command,
    environment: Mapping[str, str] | None = None,
    root: Path = ROOT,
    sleep: Callable[[float], None] = time.sleep,
) -> int:
    args = parser().parse_args(argv)
    if args.caller_poll_seconds <= 0:
        print("validate-run: REFUSED: caller poll seconds must be positive", file=sys.stderr)
        return 2
    if args.attach:
        if any((args.checkout, args.agent, args.target, args.pr, args.unit, args.log, args.dry_run)):
            print(
                "validate-run: REFUSED: --attach cannot be combined with launch arguments",
                file=sys.stderr,
            )
            return 2
        try:
            return attach(
                args.attach,
                root=root.resolve(),
                run=run,
                json_output=args.json,
                poll_seconds=args.caller_poll_seconds,
                sleep=sleep,
            )
        except (RuntimeError, ValueError) as error:
            print(f"validate-run: REFUSED: {error}", file=sys.stderr)
            return 2
    if args.checkout is None or not args.agent or not args.target:
        print(
            "validate-run: REFUSED: launch requires --checkout, --agent, and --target",
            file=sys.stderr,
        )
        return 2
    validate_args = list(args.validate_args)
    if validate_args[:1] == ["--"]:
        validate_args.pop(0)
    if args.make_validate and validate_args:
        print(
            "validate-run: REFUSED: --make-validate is the exact no-argument Make "
            "target; do not pass validate arguments after --",
            file=sys.stderr,
        )
        return 2
    if args.allow_owner_validate_symlink and not (args.in_place and args.make_validate):
        print(
            "validate-run: REFUSED: --allow-owner-validate-symlink requires "
            "--in-place and --make-validate",
            file=sys.stderr,
        )
        return 2
    if args.pr is not None and args.pr <= 0:
        print("validate-run: REFUSED: --pr must be positive", file=sys.stderr)
        return 2
    if min(args.wait, args.hold, args.child_deadline) <= 0:
        print("validate-run: REFUSED: wait/hold/child-deadline must be positive", file=sys.stderr)
        return 2

    fresh_checkout: Path | None = None
    source_checkout: Path | None = None
    try:
        source_checkout = validate_checkout(
            args.checkout,
            args.target,
            run=run,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
        )
        if args.in_place:
            checkout = source_checkout
        else:
            # DEFAULT: validate the COMMIT, not the tree that claims to be at it.
            fresh_checkout = prepare_fresh_checkout(
                source_checkout,
                args.target,
                run=run,
                parent=(root / "ignored").resolve(),
            )
            checkout = fresh_checkout
        preflight(root, checkout, args.target, run=run)
        unit = sanitize_unit(args.unit) if args.unit else default_unit(args.agent, args.target)
        log = (args.log or root / "ignored/validate" / f"{unit}.log").resolve()
        record_path = run_registry.record_path(root.resolve(), unit)
        started_at = datetime.now(timezone.utc).isoformat()
        command = build_systemd_command(
            root=root.resolve(),
            checkout=checkout,
            target=args.target,
            agent=args.agent,
            unit=unit,
            log=log,
            pr=args.pr,
            validate_args=validate_args,
            wait=args.wait,
            hold=args.hold,
            child_deadline=args.child_deadline,
            environment=environment or os.environ,
            make_validate=args.make_validate,
            allow_owner_validate_symlink=args.allow_owner_validate_symlink,
        )
        if args.dry_run:
            pane = None
        else:
            log.parent.mkdir(parents=True, exist_ok=True)
            run_registry.write_record(
                record_path,
                {
                    "schema_version": 1,
                    "state": "preparing",
                    "unit": f"{unit}.service",
                    "target": args.target,
                    "checkout": str(checkout),
                    "log": str(log),
                    "agent": args.agent,
                    "pr": args.pr,
                    "started_at": started_at,
                    "producer": "systemd-user-v1",
                    "admission": "ci-hub validate-lock",
                    "pane_role": "observer-only",
                },
            )
            pane = pane_owner.create_pane(
                root=root.resolve(),
                checkout=checkout,
                unit=unit,
                target=args.target,
                log=log,
                record=record_path,
                pr=args.pr,
                started_at=started_at,
                run=run,
                environment=environment or os.environ,
                sleep=sleep,
            )
            run_registry.update_record(
                record_path,
                state="launching",
                workspace_id=pane.workspace_id,
                tab_id=pane.tab_id,
                pane_id=pane.pane_id,
                pane_title=pane.title,
            )
            result = run(command, cwd=root, check=False)
            if result.returncode != 0:
                detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
                run_registry.update_record(
                    record_path,
                    state="refused",
                    result="systemd-launch-refused",
                    detail=detail,
                    finished_at=datetime.now(timezone.utc).isoformat(),
                )
                raise RuntimeError(f"systemd-run refused service: {detail}")
            run_registry.update_record(record_path, state="running")
    except (RuntimeError, ValueError) as error:
        # Nothing was admitted, so the temp checkout holds no evidence and is
        # removed. Failures AFTER launch are handled below and RETAIN it.
        if fresh_checkout is not None and source_checkout is not None:
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
        print(f"validate-run: REFUSED: {error}", file=sys.stderr)
        return 2

    report = {

        "schema_version": 1,
        "event": "would-start" if args.dry_run else "handle",
        "state": "planned" if args.dry_run else "running",
        "unit": f"{unit}.service",
        "target": args.target,
        "checkout": str(checkout),
        "log": str(log),
        "record": str(record_path),
        "admission": "ci-hub validate-lock",
        "producer": "systemd-user-v1",
        "pane_role": "observer-only",
        "workspace_id": pane.workspace_id if pane else pane_owner.WORKSPACE_LABEL,
        "tab_id": pane.tab_id if pane else None,
        "pane_id": pane.pane_id if pane else None,
        "command": command if args.dry_run else None,
    }
    emit_report(report, json_output=args.json)
    if args.dry_run:
        if not args.json:
            print(f"PANE-PLAN workspace={pane_owner.WORKSPACE_LABEL} role=observer-only")
            print(f"COMMAND {shlex.join(command)}")
        if fresh_checkout is not None and source_checkout is not None:
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
        return 0

    try:
        final = wait_for_unit(
            unit,
            record_path,
            run=run,
            poll_seconds=args.caller_poll_seconds,
            sleep=sleep,
        )
        updated = run_registry.update_record(record_path, **final)
    except RuntimeError as error:
        # The run OUTLIVES this waiter by design, so its temp checkout must too:
        # removing it here would delete the tree out from under a live validate.
        if fresh_checkout is not None:
            print(
                f"validate-run: temp checkout RETAINED at {fresh_checkout} (run continues)",
                file=sys.stderr,
            )
        print(
            f"validate-run: WAIT-INTERRUPTED {unit}.service; RUN CONTINUES independently: {error}",
            file=sys.stderr,
        )
        return 2

    # REQUIREMENT (a), and the ORDER IS THE WHOLE POINT: re-read this run's exact
    # row from the canonical ledger BEFORE the temp directory is removed. Deleting
    # first and checking afterwards would still find the row when it landed
    # canonically, and would find nothing to say when it did not — which is the
    # invisible-green failure this gate exists to make impossible.
    if fresh_checkout is not None and source_checkout is not None:
        try:
            row = assert_row_readable_from_canonical_ledger(
                root.resolve(), args.target, fresh_checkout, run=run
            )
        except RuntimeError as error:
            print(f"validate-run: RECEIPT-NOT-CANONICAL: {error}", file=sys.stderr)
            # Retention is for EVIDENCE, not for every failure. Retaining
            # unconditionally cost 26 MB per failed validate, and because
            # The Rust validation driver is fail-fast; that is most of them.
            try:
                orphans = orphaned_receipt_locations(fresh_checkout, run=run)
            except OSError as scan_error:
                # Could not look. A NO-RESULT must never authorize deletion.
                orphans = [f"<scan failed: {scan_error}>"]
            terminal_status = updated.get("exit_code")
            propagated = (
                terminal_status
                if isinstance(terminal_status, int) and 0 < terminal_status <= 255
                else 2
            )
            if orphans:
                print(
                    "validate-run: ORPHANED-RECEIPT: this run wrote a receipt into "
                    f"{', '.join(orphans)} inside the temp checkout, where no "
                    f"consumer reads it. Temp checkout RETAINED at {fresh_checkout} "
                    "-- it is the only trace that this happened.",
                    file=sys.stderr,
                )
                return propagated
            print(
                "validate-run: NO-RECEIPT-PRODUCED: the run failed before writing a "
                "receipt anywhere, so the temp checkout holds no evidence; "
                "reclaiming it.",
                file=sys.stderr,
            )
            remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
            return propagated
        if not args.json:
            print(f"RECEIPT-CANONICAL commit={args.target} cwd={fresh_checkout}")
            print(f"  {row[:160]}")
        remove_fresh_checkout(source_checkout, fresh_checkout, run=run)
    emit_report(
        {**updated, "event": "finished", "unit": f"{unit}.service"},
        json_output=args.json,
    )
    status = updated.get("exit_code")
    return status if isinstance(status, int) and 0 <= status <= 255 else 2


if __name__ == "__main__":
    raise SystemExit(main())
