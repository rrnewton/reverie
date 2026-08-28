#!/usr/bin/env python3
"""Resolve a caller's agent identity from inherited process context.

``DG_AGENT_NAME`` is the primary identity signal.  It is inherited when an
agent is spawned and is already the independent source used by
``agent_liveness_probe.py``.  Slot, pane, cgroup, supervisor, and TaskGraph
owners are corroboration: they can expose a relay or stale registry row, but
none is allowed to manufacture an identity when the process carries none.

The selected model is a separate process-context fact.  It is read only from
an explicit model option on a recognized ``codex`` or ``claude`` process
command line.  It is never caller-supplied and never guessed from an agent
name, client family, configuration file, or default.  When no unique explicit
model is visible the stable value is ``unresolved``.

The resolver deliberately has three outcomes:

* ``AGREEMENT`` (rc 0): a process-derived identity exists and every readable
  identity signal agrees with it.
* ``DISAGREEMENT`` (rc 1): readable signals name different agents.  No winner
  is returned, even when one signal is normally primary.
* ``UNRESOLVED`` (rc 2): no process-derived identity can be established.

Unavailable corroboration is reported, not guessed through.  That makes the
known fleet blind spots -- unreadable/deleted cwd, missing pane metadata,
legacy slots, and an unavailable TaskGraph -- visible without turning them
into either a false identity or a fleet-wide wedge.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Mapping, Sequence

import taskgraph_db
from worktree_registry import RegistrySchemaError, read_registry


AGREEMENT = 0
DISAGREEMENT = 1
UNRESOLVED = 2
SCHEMA_VERSION = 2
IDENTITY_VAR = "DG_AGENT_NAME"
AGENT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/@+-]*$")
MODEL_RE = re.compile(r"^[^,\[\]\r\n]+$")
MODEL_UNRESOLVED = "unresolved"
PANE_RE = re.compile(r"^%[0-9]+$")
ACTIVE_TASK_STATES = frozenset({"IN_PROGRESS", "IN PROGRESS"})


@dataclass(frozen=True)
class Signal:
    source: str
    state: str
    agent: str | None = None
    detail: str = ""
    candidates: tuple[str, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        result = asdict(self)
        result["candidates"] = list(self.candidates)
        return result


@dataclass(frozen=True)
class Resolution:
    state: str
    agent: str | None
    primary_source: str | None
    basis: tuple[str, ...]
    signals: tuple[Signal, ...]
    pid: int
    model: str = MODEL_UNRESOLVED
    model_source: str | None = None
    client_family: str | None = None
    schema: int = SCHEMA_VERSION

    @property
    def exit_code(self) -> int:
        return {
            "AGREEMENT": AGREEMENT,
            "DISAGREEMENT": DISAGREEMENT,
        }.get(self.state, UNRESOLVED)

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema": self.schema,
            "state": self.state,
            "agent": self.agent,
            "primary_source": self.primary_source,
            "basis": list(self.basis),
            "pid": self.pid,
            "model": self.model,
            "model_source": self.model_source,
            "client_family": self.client_family,
            "signals": [signal.to_dict() for signal in self.signals],
        }

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), sort_keys=True)


@dataclass(frozen=True)
class ProcessContext:
    pid: int
    ppid: int | None
    environ: Mapping[str, str] | None
    environ_error: str | None = None
    cmdline: tuple[str, ...] | None = None
    cmdline_error: str | None = None


@dataclass(frozen=True)
class Registry:
    document: Mapping[str, Any] | None
    error: str | None = None


def _canonical(agent: str) -> str:
    return agent.casefold()


def _safe_agent(raw: str | None) -> str | None:
    if raw is None:
        return None
    value = raw.strip()
    return value if AGENT_RE.fullmatch(value) else None


def _read_environ(path: Path) -> tuple[dict[str, str] | None, str | None]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        return None, f"unreadable: {error}"
    result: dict[str, str] = {}
    for item in raw.split(b"\0"):
        if b"=" not in item:
            continue
        key, value = item.split(b"=", 1)
        result[key.decode("utf-8", "replace")] = value.decode("utf-8", "replace")
    return result, None


def _read_ppid(path: Path) -> tuple[int | None, str | None]:
    try:
        raw = path.read_text()
    except OSError as error:
        return None, f"unreadable stat: {error}"
    _, separator, remainder = raw.rpartition(") ")
    fields = remainder.split() if separator else ()
    if len(fields) < 2 or not fields[1].isdigit():
        return None, "malformed stat record"
    return int(fields[1]), None


def _read_cmdline(path: Path) -> tuple[tuple[str, ...] | None, str | None]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        return None, f"unreadable cmdline: {error}"
    argv = tuple(
        item.decode("utf-8", "replace")
        for item in raw.split(b"\0")
        if item
    )
    if not argv:
        return None, "empty cmdline"
    return argv, None


def process_ancestry(pid: int, proc_root: Path) -> tuple[list[ProcessContext], str | None]:
    """Read one ancestry chain without crossing a cycle or guessing a parent."""

    contexts: list[ProcessContext] = []
    seen: set[int] = set()
    current = pid
    for _ in range(256):
        if current <= 0 or current in seen:
            break
        seen.add(current)
        proc = proc_root / str(current)
        if not proc.exists():
            return contexts, f"process {current} does not exist"
        ppid, stat_error = _read_ppid(proc / "stat")
        environ, environ_error = _read_environ(proc / "environ")
        cmdline, cmdline_error = _read_cmdline(proc / "cmdline")
        contexts.append(
            ProcessContext(
                current,
                ppid,
                environ,
                environ_error,
                cmdline,
                cmdline_error,
            )
        )
        if stat_error:
            return contexts, f"pid {current}: {stat_error}"
        if ppid in (None, 0, current):
            break
        current = ppid
    return contexts, None


def _safe_model(raw: str | None) -> str | None:
    if raw is None:
        return None
    # The owner requires the CLI's model slug verbatim. Reject values that
    # cannot occupy one comma-delimited prefix field; never normalize them into
    # a different vocabulary or strip quoting/whitespace into apparent validity.
    return raw if raw == raw.strip() and MODEL_RE.fullmatch(raw) else None


def _recognized_client_index(argv: Sequence[str]) -> int | None:
    """Find the real agent client without treating arbitrary body text as argv."""

    for index, raw in enumerate(argv[:4]):
        name = Path(raw).name.casefold()
        if name in {"codex", "claude"}:
            return index
    return None


def _explicit_models(argv: Sequence[str]) -> tuple[list[str], bool]:
    """Return valid explicit model values and whether malformed input existed."""

    start = _recognized_client_index(argv)
    if start is None:
        return [], False
    args = list(argv[start + 1 :])
    values: list[str] = []
    malformed = False
    index = 0
    while index < len(args):
        arg = args[index]
        raw: str | None = None
        consumed = 1
        if arg == "--model":
            if index + 1 >= len(args):
                malformed = True
            else:
                raw = args[index + 1]
                consumed = 2
        elif arg.startswith("--model="):
            raw = arg.split("=", 1)[1]
        elif arg in {"-c", "--config"}:
            if index + 1 < len(args) and args[index + 1].startswith("model="):
                raw = args[index + 1].split("=", 1)[1]
                consumed = 2
        elif arg.startswith("--config=model="):
            raw = arg.split("=", 2)[2]
        if raw is not None:
            model = _safe_model(raw)
            if model is None:
                malformed = True
            else:
                values.append(model)
        index += consumed
    return values, malformed


CLIENT_FAMILIES = ("claude", "codex")


def client_family_signal(
    contexts: Sequence[ProcessContext],
) -> tuple[Signal, str | None]:
    """Which model FAMILY launched this process, read from /proc, never typed.

    ⚠️ THE FAMILY WAS ALREADY IN HAND AND WAS BEING THROWN AWAY.
    :func:`_recognized_client_index` finds the `claude` or `codex` binary in the
    ancestry and returns only its INDEX, used as an offset for scanning
    ``--model`` flags. The basename it matched on IS the family, and the family
    is the field review independence rests on: ``post-facto-review`` requires one
    Claude-family and one Codex-family exact-head approval, and the only
    machine-readable discriminator between them was a self-reported string.

    ⚠️ THIS IS NOT THE MODEL SLUG AND DOES NOT REPLACE :func:`model_signal`.
    Measured on this board 2026-08-26: every agent reports ``model=unresolved``
    because clients are launched WITHOUT an explicit ``--model``, so the slug is
    genuinely unavailable. The FAMILY is available on exactly the same evidence
    -- the ancestry of agent(hermit-107) carries basenames ``['claude', 'claude']``
    while its model resolves to ``unresolved``. A weaker fact that can be
    established beats a stronger one that cannot.

    Returns ``None`` for the family when it cannot be established, and callers
    must treat that as an ABSENCE OF EVIDENCE rather than as a licence: refuse a
    contradiction, never manufacture one from silence.

    ⚠️ AMBIGUITY IS UNRESOLVED, NOT A GUESS. An ancestry carrying BOTH a claude
    and a codex client -- one agent driving another -- names no single family,
    and picking the nearest would answer a question the evidence does not settle.
    """

    seen: list[tuple[int, str]] = []
    unreadable: list[int] = []
    for context in contexts:
        if context.cmdline is None:
            unreadable.append(context.pid)
            continue
        index = _recognized_client_index(context.cmdline)
        if index is None:
            continue
        name = Path(context.cmdline[index]).name.casefold()
        if name in CLIENT_FAMILIES:
            seen.append((context.pid, name))

    families = tuple(sorted({family for _, family in seen}))
    if len(families) > 1:
        return (
            Signal(
                "client-family",
                "ambiguous",
                detail="ancestry carries more than one recognized client",
                candidates=families,
            ),
            None,
        )
    if len(families) == 1:
        pids = ",".join(str(pid) for pid, family in seen if family == families[0])
        return (
            Signal(
                "client-family",
                "resolved",
                detail=f"client={families[0]} at pid(s) {pids}",
            ),
            families[0],
        )
    detail = "no recognized codex/claude client in the ancestry"
    if unreadable:
        detail += "; unreadable/empty cmdline pid(s) " + ",".join(map(str, unreadable))
    return Signal("client-family", "unavailable", detail=detail), None


def model_signal(contexts: Sequence[ProcessContext]) -> tuple[Signal, str, str | None]:
    """Resolve one explicit launch model, or report the stable unresolved value."""

    found: list[tuple[int, str]] = []
    malformed_pids: list[int] = []
    unreadable_pids: list[int] = []
    for context in contexts:
        if context.cmdline is None:
            unreadable_pids.append(context.pid)
            continue
        models, malformed = _explicit_models(context.cmdline)
        found.extend((context.pid, model) for model in models)
        if malformed:
            malformed_pids.append(context.pid)

    unique = tuple(sorted({model for _, model in found}, key=str.casefold))
    if malformed_pids:
        return (
            Signal(
                "process-model",
                "error",
                detail="malformed explicit model at pid(s) "
                + ",".join(map(str, malformed_pids)),
            ),
            MODEL_UNRESOLVED,
            None,
        )
    if len(unique) > 1:
        return (
            Signal(
                "process-model",
                "ambiguous",
                detail="recognized client cmdlines carry different explicit models",
                candidates=unique,
            ),
            MODEL_UNRESOLVED,
            None,
        )
    if len(unique) == 1:
        pids = [str(pid) for pid, model in found if model == unique[0]]
        return (
            Signal(
                "process-model",
                "resolved",
                detail=f"model={unique[0]} explicit at pid(s) " + ",".join(pids),
            ),
            unique[0],
            "process-cmdline",
        )
    detail = "no recognized codex/claude cmdline carries an explicit model"
    if unreadable_pids:
        detail += "; unreadable/empty cmdline pid(s) " + ",".join(
            map(str, unreadable_pids)
        )
    return Signal("process-model", "unavailable", detail=detail), MODEL_UNRESOLVED, None


def _identity_signals(
    contexts: Sequence[ProcessContext], ancestry_error: str | None
) -> tuple[list[Signal], str | None, str | None, dict[str, str]]:
    signals: list[Signal] = []
    inherited_env: dict[str, str] = {}
    named: list[tuple[int, str]] = []
    invalid: list[int] = []
    for context in contexts:
        if context.environ is None:
            continue
        # Resolve each inherited context field from its nearest readable
        # carrier.  A thin wrapper can preserve DG_AGENT_NAME while omitting
        # TMUX_PANE; the owning shell one generation up still corroborates the
        # pane, without letting a farther ancestor override a nearer value.
        for key in ("TMUX_PANE",):
            if key not in inherited_env and context.environ.get(key):
                inherited_env[key] = context.environ[key]
        raw = context.environ.get(IDENTITY_VAR)
        if raw is None or not raw.strip():
            continue
        agent = _safe_agent(raw)
        if agent is None:
            invalid.append(context.pid)
        else:
            named.append((context.pid, agent))

    caller = contexts[0] if contexts else None
    if caller is None:
        signals.append(Signal("caller-environ", "unavailable", detail=ancestry_error or "no process"))
    elif caller.environ is None:
        signals.append(
            Signal("caller-environ", "unavailable", detail=caller.environ_error or "unreadable")
        )
    else:
        raw = caller.environ.get(IDENTITY_VAR)
        agent = _safe_agent(raw)
        if agent is not None:
            signals.append(Signal("caller-environ", "resolved", agent=agent, detail=f"pid={caller.pid}"))
        elif raw is None or not raw.strip():
            signals.append(
                Signal("caller-environ", "unavailable", detail=f"pid={caller.pid} lacks {IDENTITY_VAR}")
            )
        else:
            signals.append(
                Signal("caller-environ", "error", detail=f"pid={caller.pid} has invalid {IDENTITY_VAR}")
            )

    ancestor_named = named[1:] if caller and named and named[0][0] == caller.pid else named
    ancestor_agents = sorted({_canonical(agent): agent for _, agent in ancestor_named}.values())
    if invalid:
        signals.append(
            Signal(
                "process-ancestry",
                "error",
                detail="invalid DG_AGENT_NAME at pid(s) " + ",".join(map(str, invalid)),
            )
        )
    elif len(ancestor_agents) == 1:
        pids = [str(pid) for pid, agent in ancestor_named if _canonical(agent) == _canonical(ancestor_agents[0])]
        signals.append(
            Signal(
                "process-ancestry",
                "resolved",
                agent=ancestor_agents[0],
                detail="identity-bearing ancestor pid(s)=" + ",".join(pids),
            )
        )
    elif len(ancestor_agents) > 1:
        signals.append(
            Signal(
                "process-ancestry",
                "ambiguous",
                detail="ancestors carry different agent identities",
                candidates=tuple(ancestor_agents),
            )
        )
    else:
        signals.append(
            Signal(
                "process-ancestry",
                "unavailable",
                detail=ancestry_error or "no identity-bearing ancestor",
            )
        )

    primary_agent: str | None = None
    primary_source: str | None = None
    if caller and caller.environ is not None:
        primary_agent = _safe_agent(caller.environ.get(IDENTITY_VAR))
        if primary_agent:
            primary_source = "caller-environ"
    if primary_agent is None and named:
        primary_agent = named[0][1]
        primary_source = "process-ancestry"
    # A malformed identity anywhere on the chain is not permission to skip to
    # a farther, plausible value.  The process binding itself is corrupt, so
    # leave the caller unresolved and preserve the exact bad pid(s) above.
    if invalid:
        primary_agent = None
        primary_source = None
    return signals, primary_agent, primary_source, inherited_env


def _read_cwd(pid: int, proc_root: Path) -> tuple[Path | None, str]:
    try:
        raw = os.readlink(proc_root / str(pid) / "cwd")
    except OSError as error:
        return None, f"unreadable cwd: {error}"
    if raw.endswith(" (deleted)"):
        return None, f"deleted cwd: {raw}"
    path = Path(raw)
    if not path.is_absolute():
        return None, f"non-absolute cwd: {raw}"
    return path, raw


def _read_cgroup(pid: int, proc_root: Path) -> tuple[str | None, str]:
    try:
        lines = (proc_root / str(pid) / "cgroup").read_text().splitlines()
    except OSError as error:
        return None, f"unreadable cgroup: {error}"
    unified = [line.split("::", 1)[1] for line in lines if "::" in line]
    if len(unified) != 1 or not unified[0].startswith("/"):
        return None, "no unique unified cgroup path"
    return unified[0].rstrip("/") or "/", unified[0]


def load_registry(state_path: Path) -> Registry:
    try:
        document = read_registry(state_path)
    except RegistrySchemaError as error:
        return Registry(None, f"cannot read {state_path}: {error}")
    return Registry(document)


def _slot_owners(row: Mapping[str, Any]) -> tuple[str, ...]:
    owners = {
        agent
        for entry in row.get("agents", [])
        if isinstance(entry, dict)
        and (agent := _safe_agent(entry.get("name") if isinstance(entry.get("name"), str) else None))
    }
    return tuple(sorted(owners, key=str.casefold))


def _registered_paths(repo_root: Path, slot: str, row: Mapping[str, Any]) -> tuple[Path, ...]:
    paths = {repo_root / "worktrees" / "slots" / slot}
    for key, value in row.items():
        if not key.endswith("_path") or not isinstance(value, str) or not value.strip():
            continue
        path = Path(value)
        paths.add(path if path.is_absolute() else repo_root / path)
    return tuple(path.resolve(strict=False) for path in paths)


def _within(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def slot_owner_signal(
    cwd: Path | None,
    cwd_detail: str,
    registry: Registry,
    repo_root: Path,
) -> tuple[Signal, tuple[str, ...]]:
    if cwd is None:
        return Signal("cwd-slot-owner", "unavailable", detail=cwd_detail), ()
    if registry.document is None:
        return Signal("cwd-slot-owner", "unavailable", detail=registry.error or "registry unavailable"), ()
    physical = cwd.resolve(strict=False)
    matches: list[tuple[str, Mapping[str, Any]]] = []
    for slot, value in registry.document["slots"].items():
        if not isinstance(slot, str) or not isinstance(value, dict):
            continue
        if any(_within(physical, candidate) for candidate in _registered_paths(repo_root, slot, value)):
            matches.append((slot, value))
    if not matches:
        return Signal("cwd-slot-owner", "unavailable", detail=f"cwd={physical} is outside registered slots"), ()
    if len(matches) != 1:
        return Signal(
            "cwd-slot-owner",
            "ambiguous",
            detail="cwd matches multiple registered slots: " + ",".join(slot for slot, _ in matches),
        ), ()
    slot, row = matches[0]
    owners = _slot_owners(row)
    tasks = {
        task
        for task in [row.get("task"), *[entry.get("task") for entry in row.get("agents", []) if isinstance(entry, dict)]]
        if isinstance(task, str) and task.strip()
    }
    detail = (
        f"cwd={physical} slot={slot} status={row.get('status', 'unknown')} "
        "basis=physical-path+owner (branch metadata is not identity authority)"
    )
    if len(owners) == 1:
        return Signal("cwd-slot-owner", "resolved", agent=owners[0], detail=detail), tuple(sorted(tasks))
    if owners:
        return Signal(
            "cwd-slot-owner", "ambiguous", detail=detail, candidates=owners
        ), tuple(sorted(tasks))
    return Signal("cwd-slot-owner", "unavailable", detail=detail + " has no registered owner"), tuple(sorted(tasks))


def _registry_owner_matches(
    registry: Registry,
    predicate: Any,
    source: str,
    absent_detail: str,
) -> Signal:
    if registry.document is None:
        return Signal(source, "unavailable", detail=registry.error or "registry unavailable")
    matches: set[str] = set()
    for row in registry.document["slots"].values():
        if not isinstance(row, dict):
            continue
        # ⚠️ A RELEASED SLOT OWNS NOTHING, SO ITS RECORDS MUST NOT ANSWER AN
        # OWNERSHIP QUESTION. This corrects WHAT the resolver reads; it does not
        # touch HOW STRICTLY it dissents. A genuine disagreement between two live
        # signals still refuses, and that refusal is the point of this module.
        #
        # Releasing a slot does not clear its `tmux_pane_id`, so a released row
        # kept shadowing a pane that a different, live session had since been
        # given. Measured 2026-08-25: `TMUX_PANE=%3 who-am-i` reported
        # DISAGREEMENT because released slot `codex-cpuid2258` still recorded
        # agent `lane1` holding `%3`, while `%3` was a live claude session. That
        # one stale row blocked every guarded write, including posting a finished
        # review on hermit#2376.
        #
        # It was not one row: ALL 16 pane-carrying agent records in the registry
        # sat on released slots, 16 of 16, and panes %131 and %135 were each
        # claimed by two different released slots.
        #
        # ONLY `released` IS SKIPPED, DELIBERATELY. The other terminal-looking
        # states are not safe to ignore: `release-requested` may still have a
        # live owner mid-release, and suppressing its dissent would hide a real
        # conflict. `lease-quarantined` and `owner-lease-revoked` are likewise
        # unmeasured here. Skipping only the state that provably owns nothing
        # keeps this fail-closed.
        if row.get("status") == "released":
            continue
        for entry in row.get("agents", []):
            if not isinstance(entry, dict) or not predicate(row, entry):
                continue
            agent = _safe_agent(entry.get("name") if isinstance(entry.get("name"), str) else None)
            if agent:
                matches.add(agent)
    ordered = tuple(sorted(matches, key=str.casefold))
    if len(ordered) == 1:
        return Signal(source, "resolved", agent=ordered[0], detail=absent_detail)
    if ordered:
        return Signal(source, "ambiguous", detail=absent_detail, candidates=ordered)
    return Signal(source, "unavailable", detail=absent_detail)


def pane_owner_signal(pane: str | None, registry: Registry) -> Signal:
    if pane is None or not PANE_RE.fullmatch(pane):
        return Signal("pane-owner", "unavailable", detail="caller has no schema-valid TMUX_PANE")

    def matches(row: Mapping[str, Any], entry: Mapping[str, Any]) -> bool:
        if entry.get("tmux_pane_id") == pane:
            return True
        sidecar = row.get("owner_sidecar")
        return bool(
            isinstance(sidecar, dict)
            and sidecar.get("schema_version") == 1
            and sidecar.get("agent") == entry.get("name")
            and sidecar.get("tmux_pane_id") == pane
        )

    return _registry_owner_matches(
        registry,
        matches,
        "pane-owner",
        f"pane={pane}",
    )


def cgroup_owner_signal(cgroup: str | None, detail: str, registry: Registry) -> Signal:
    if cgroup is None:
        return Signal("cgroup-owner", "unavailable", detail=detail)

    def matches(_row: Mapping[str, Any], entry: Mapping[str, Any]) -> bool:
        candidates = [entry.get("cgroup_path")]
        sidecar = _row.get("owner_sidecar")
        if (
            isinstance(sidecar, dict)
            and sidecar.get("schema_version") == 1
            and sidecar.get("agent") == entry.get("name")
        ):
            candidates.append(sidecar.get("cgroup_path"))
        for raw in candidates:
            if not isinstance(raw, str) or not raw.startswith("/"):
                continue
            owner = raw.rstrip("/") or "/"
            if cgroup == owner or (owner != "/" and cgroup.startswith(owner + "/")):
                return True
        return False

    return _registry_owner_matches(
        registry, matches, "cgroup-owner", f"cgroup={cgroup}"
    )


def supervisor_owner_signal(ancestry_pids: set[int], registry: Registry) -> Signal:
    def matches(row: Mapping[str, Any], entry: Mapping[str, Any]) -> bool:
        sidecar = row.get("owner_sidecar")
        return bool(
            isinstance(sidecar, dict)
            and sidecar.get("schema_version") == 1
            and sidecar.get("supervisor_pid") in ancestry_pids
            and sidecar.get("agent") == entry.get("name")
        )

    return _registry_owner_matches(
        registry,
        matches,
        "supervisor-owner",
        "guarded owner sidecar supervisor appears in process ancestry",
    )


def task_owner_signal(task_ids: Sequence[str], db: Path | None) -> Signal:
    unique = tuple(sorted({task for task in task_ids if task.strip()}))
    if not unique:
        return Signal("taskgraph-owner", "unavailable", detail="no explicit or uniquely slot-derived active task")
    if len(unique) != 1:
        return Signal(
            "taskgraph-owner",
            "unavailable",
            detail="multiple candidate tasks: " + ",".join(unique),
        )
    task = unique[0]
    try:
        resolution = taskgraph_db.resolve(db)
        connection = sqlite3.connect(f"file:{resolution.path}?mode=ro", uri=True)
        try:
            rows = connection.execute(
                "SELECT status, owner FROM tasks WHERE local_id = ? OR fbid = ?",
                (task, task),
            ).fetchall()
        finally:
            connection.close()
    except (taskgraph_db.TaskGraphUnavailable, sqlite3.Error, OSError) as error:
        return Signal("taskgraph-owner", "unavailable", detail=f"task={task}: {error}")
    if len(rows) != 1:
        return Signal("taskgraph-owner", "unavailable", detail=f"task={task} matched {len(rows)} rows")
    status, raw_owner = rows[0]
    owner = _safe_agent(raw_owner if isinstance(raw_owner, str) else None)
    if str(status).upper() not in ACTIVE_TASK_STATES:
        return Signal("taskgraph-owner", "unavailable", detail=f"task={task} is status={status}")
    if owner is None:
        return Signal("taskgraph-owner", "unavailable", detail=f"task={task} has no schema-valid owner")
    return Signal("taskgraph-owner", "resolved", agent=owner, detail=f"task={task} status={status}")


def resolve_identity(
    pid: int,
    *,
    proc_root: Path = Path("/proc"),
    repo_root: Path | None = None,
    state_path: Path | None = None,
    task_id: str | None = None,
    task_db: Path | None = None,
) -> Resolution:
    """Resolve one caller, preserving every readable and unreadable basis."""

    if pid <= 0:
        return Resolution(
            "UNRESOLVED",
            None,
            None,
            (),
            (Signal("caller-environ", "error", detail=f"invalid pid={pid}"),),
            pid,
        )
    root = (repo_root or Path(__file__).resolve().parents[2]).resolve()
    registry = load_registry(state_path or root / "worktree-state.json")
    contexts, ancestry_error = process_ancestry(pid, proc_root)
    signals, primary, primary_source, inherited_env = _identity_signals(contexts, ancestry_error)
    process_model_signal, model, model_source = model_signal(contexts)
    signals.append(process_model_signal)
    process_family_signal, client_family = client_family_signal(contexts)
    signals.append(process_family_signal)

    cwd, cwd_detail = _read_cwd(pid, proc_root)
    slot_signal, derived_tasks = slot_owner_signal(cwd, cwd_detail, registry, root)
    signals.append(slot_signal)
    signals.append(pane_owner_signal(inherited_env.get("TMUX_PANE"), registry))
    cgroup, cgroup_detail = _read_cgroup(pid, proc_root)
    signals.append(cgroup_owner_signal(cgroup, cgroup_detail, registry))
    signals.append(supervisor_owner_signal({context.pid for context in contexts}, registry))
    task_ids = (task_id,) if task_id else derived_tasks
    signals.append(task_owner_signal(task_ids, task_db))

    resolved = [signal for signal in signals if signal.state == "resolved" and signal.agent]
    identities = {_canonical(signal.agent) for signal in resolved if signal.agent}
    # Model and client-family ambiguity are reported alongside identity evidence
    # but are not agent-name disagreement. Keep those authorities separate: a
    # caller can have a proven agent while its model or client family is not.
    ambiguous = [
        signal
        for signal in signals
        if signal.source not in {"process-model", "client-family"}
        and signal.state == "ambiguous"
        and signal.candidates
    ]
    for signal in ambiguous:
        identities.update(_canonical(candidate) for candidate in signal.candidates)
    basis = tuple(signal.source for signal in resolved)

    if len(identities) > 1:
        return Resolution(
            "DISAGREEMENT",
            None,
            primary_source,
            basis,
            tuple(signals),
            pid,
            model,
            model_source,
            client_family,
        )
    if primary is None:
        return Resolution(
            "UNRESOLVED",
            None,
            None,
            basis,
            tuple(signals),
            pid,
            model,
            model_source,
            client_family,
        )
    if identities and _canonical(primary) not in identities:
        return Resolution(
            "DISAGREEMENT",
            None,
            primary_source,
            basis,
            tuple(signals),
            pid,
            model,
            model_source,
            client_family,
        )
    return Resolution(
        "AGREEMENT",
        primary,
        primary_source,
        basis,
        tuple(signals),
        pid,
        model,
        model_source,
        client_family,
    )
