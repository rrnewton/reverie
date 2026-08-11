#!/usr/bin/env python3
"""Fail-closed liveness authority for one registered slot owner.

The allocator records a schema-versioned ``owner_sidecar`` inside the slot row
under the same registry lock as the ownership mutation.  This verifier reads
that exact row under a shared lock and answers only from the recorded
``(boot_id, supervisor_pid, start_ticks)`` identity.

Exit codes deliberately match ``agent_liveness_probe.py``:

* 0: the recorded owner incarnation is proven dead;
* 1: the recorded owner incarnation is alive (adoption/release refused);
* 2: indeterminate (adoption/release refused).

Missing, malformed, unsupported, or mismatched evidence is indeterminate,
never dead.  There are no production path overrides: caller-selected evidence
would let a caller manufacture authority.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Mapping, Sequence


SCHEMA_VERSION = 1
SIDECAR_SCHEMA_VERSION = 1
SOURCES = frozenset({"orc-tmux-pane-supervisor-v1", "codex-systemd-sentinel-v1"})
EXACT_FIELDS = frozenset(
    {
        "schema_version",
        "source",
        "slot",
        "agent",
        "task",
        "recorded_at",
        "generation",
        "supervisor_pid",
        "boot_id",
        "start_ticks",
        "tmux_pane_id",
        "cgroup_path",
    }
)
NOT_RUNNING_STATES = frozenset({"Z", "X"})


@dataclass(frozen=True)
class Verdict:
    schema_version: int
    state: str
    reason_code: str
    detail: str
    slot: str
    agent: str | None
    owner_identity: dict[str, object] | None

    @property
    def exit_code(self) -> int:
        return {"dead": 0, "alive": 1}.get(self.state, 2)

    @property
    def may_recover(self) -> bool:
        return self.state == "dead"

    def render(self) -> dict[str, object]:
        return {**asdict(self), "may_recover": self.may_recover}


def verdict(
    state: str,
    reason_code: str,
    detail: str,
    slot: str,
    agent: str | None = None,
    owner_identity: dict[str, object] | None = None,
) -> Verdict:
    return Verdict(
        schema_version=SCHEMA_VERSION,
        state=state,
        reason_code=reason_code,
        detail=detail,
        slot=slot,
        agent=agent,
        owner_identity=owner_identity,
    )


def _parse_process_stat(raw: str) -> tuple[str, int]:
    close = raw.rfind(") ")
    if close < 0:
        raise ValueError("process stat has no closing process name")
    fields = raw[close + 2 :].split()
    if len(fields) <= 19:
        raise ValueError("process stat omits field 22 start_ticks")
    return fields[0], int(fields[19])


def _mutating_owner(slot_row: Mapping[str, object]) -> Mapping[str, object]:
    agents = slot_row.get("agents")
    if not isinstance(agents, list):
        raise ValueError("slot agents is not an array")
    mutating = [
        owner
        for owner in agents
        if isinstance(owner, Mapping) and owner.get("read_only") is not True
    ]
    if len(mutating) != 1:
        raise ValueError(f"slot has {len(mutating)} mutating owners, expected exactly one")
    return mutating[0]


def _validated_sidecar(
    slot: str, slot_row: Mapping[str, object]
) -> tuple[Mapping[str, object], Mapping[str, object]]:
    if slot_row.get("status") not in {"active", "held"}:
        raise ValueError(f"slot status is {slot_row.get('status')!r}, expected active|held")
    owner = _mutating_owner(slot_row)
    sidecar = slot_row.get("owner_sidecar")
    if not isinstance(sidecar, Mapping):
        raise ValueError("slot has no versioned owner_sidecar")
    if frozenset(sidecar) != EXACT_FIELDS:
        raise ValueError("owner_sidecar field set is not schema-exact")
    if sidecar.get("schema_version") != SIDECAR_SCHEMA_VERSION:
        raise ValueError("owner_sidecar schema_version is unsupported")
    if sidecar.get("source") not in SOURCES:
        raise ValueError("owner_sidecar source is unsupported")
    if sidecar.get("slot") != slot:
        raise ValueError("owner_sidecar slot binding does not match registry key")
    agent = owner.get("name")
    if not isinstance(agent, str) or not agent or sidecar.get("agent") != agent:
        raise ValueError("owner_sidecar agent binding does not match mutating owner")
    task = owner.get("task", slot_row.get("task", ""))
    if not isinstance(task, str) or sidecar.get("task") != task:
        raise ValueError("owner_sidecar task binding does not match mutating owner")
    pid = sidecar.get("supervisor_pid")
    start_ticks = sidecar.get("start_ticks")
    boot_id = sidecar.get("boot_id")
    if (
        type(pid) is not int
        or pid <= 0
        or type(start_ticks) is not int
        or start_ticks <= 0
        or not isinstance(boot_id, str)
        or not boot_id
    ):
        raise ValueError("owner_sidecar supervisor identity is incomplete")
    if sidecar.get("generation") != f"{boot_id}:{pid}:{start_ticks}":
        raise ValueError("owner_sidecar generation does not bind its exact identity")
    cgroup = sidecar.get("cgroup_path")
    if not isinstance(cgroup, str) or cgroup == "/" or not cgroup.startswith("/"):
        raise ValueError("owner_sidecar cgroup path is not normalized")
    pane = sidecar.get("tmux_pane_id")
    source = sidecar.get("source")
    if source == "orc-tmux-pane-supervisor-v1":
        if not isinstance(pane, str) or not pane.startswith("%"):
            raise ValueError("ORC owner_sidecar has no tmux pane binding")
        if owner.get("tmux_pane_id") != pane or owner.get("cgroup_path") != cgroup:
            raise ValueError("ORC owner_sidecar disagrees with the registered pane/cgroup lease")
    else:
        if pane is not None:
            raise ValueError("Codex sentinel owner_sidecar must not claim a tmux pane")
        lease = slot_row.get("coordinator_lease")
        if (
            not isinstance(lease, Mapping)
            or lease.get("schema_version") != 1
            or lease.get("source") != "codex-systemd-sentinel-v1"
            or lease.get("main_pid") != pid
            or lease.get("main_pid_starttime") != start_ticks
            or lease.get("cgroup_path") != cgroup
        ):
            raise ValueError(
                "Codex owner_sidecar disagrees with the registered coordinator lease"
            )
    return owner, sidecar


def assess_state(
    slot: str,
    state: Mapping[str, object],
    *,
    proc_root: Path = Path("/proc"),
    boot_id_path: Path = Path("/proc/sys/kernel/random/boot_id"),
    cgroup_root: Path = Path("/sys/fs/cgroup"),
) -> Verdict:
    slots = state.get("slots")
    if not isinstance(slots, Mapping) or not isinstance(slots.get(slot), Mapping):
        return verdict("indeterminate", "slot-missing", "slot is absent from registry", slot)
    slot_row = slots[slot]
    try:
        owner, sidecar = _validated_sidecar(slot, slot_row)
    except (TypeError, ValueError) as error:
        return verdict("indeterminate", "sidecar-invalid", str(error), slot)

    agent = str(owner["name"])
    identity = {
        "source": sidecar["source"],
        "generation": sidecar["generation"],
        "supervisor_pid": sidecar["supervisor_pid"],
        "boot_id": sidecar["boot_id"],
        "start_ticks": sidecar["start_ticks"],
    }

    def dead_if_owner_domain_empty(reason_code: str, detail: str) -> Verdict:
        """A missing exact PID is not enough when /proc visibility is partial.

        The recorded cgroup is an independent kernel view of the owner domain.
        Any member, including a namespace-obscured PID rendered as 0, refuses a
        dead verdict. An unreadable domain is likewise indeterminate.
        """

        relative = str(sidecar["cgroup_path"]).lstrip("/")
        procs_path = cgroup_root / relative / "cgroup.procs"
        try:
            members = [line.strip() for line in procs_path.read_text().splitlines() if line.strip()]
        except FileNotFoundError:
            members = []
        except OSError as error:
            return verdict(
                "indeterminate",
                "owner-cgroup-unreadable",
                f"cannot verify recorded owner cgroup is empty: {error}",
                slot,
                agent,
                identity,
            )
        if members:
            return verdict(
                "indeterminate",
                "owner-cgroup-nonempty",
                "exact supervisor appears dead but its recorded owner cgroup "
                f"still has {len(members)} member(s); refusing recovery",
                slot,
                agent,
                identity,
            )
        return verdict("dead", reason_code, detail, slot, agent, identity)
    try:
        current_boot = boot_id_path.read_text(encoding="utf-8").strip()
    except OSError as error:
        return verdict(
            "indeterminate",
            "boot-id-unreadable",
            f"cannot read current boot id: {error}",
            slot,
            agent,
            identity,
        )
    if not current_boot:
        return verdict(
            "indeterminate",
            "boot-id-unreadable",
            "current boot id is empty",
            slot,
            agent,
            identity,
        )
    if current_boot != sidecar["boot_id"]:
        return dead_if_owner_domain_empty(
            "owner-boot-id-mismatch",
            "the recorded supervisor belongs to a previous host boot",
        )

    pid = int(sidecar["supervisor_pid"])
    stat_path = proc_root / str(pid) / "stat"
    try:
        process_state, observed_ticks = _parse_process_stat(
            stat_path.read_text(encoding="utf-8")
        )
    except FileNotFoundError:
        return dead_if_owner_domain_empty(
            "owner-pid-absent",
            "the exact recorded supervisor pid is absent",
        )
    except (OSError, ValueError) as error:
        return verdict(
            "indeterminate",
            "owner-process-unreadable",
            f"cannot verify recorded supervisor identity: {error}",
            slot,
            agent,
            identity,
        )
    if observed_ticks != sidecar["start_ticks"]:
        return dead_if_owner_domain_empty(
            "owner-pid-reused",
            f"pid start_ticks changed from {sidecar['start_ticks']} to {observed_ticks}",
        )
    if process_state in NOT_RUNNING_STATES:
        return dead_if_owner_domain_empty(
            "owner-process-exited",
            f"the exact supervisor is in non-running state {process_state}",
        )
    return verdict(
        "alive",
        "owner-process-alive",
        f"exact supervisor {pid}@{observed_ticks} is alive in state {process_state}",
        slot,
        agent,
        identity,
    )


def assess(
    slot: str,
    root: Path,
    *,
    proc_root: Path = Path("/proc"),
    boot_id_path: Path = Path("/proc/sys/kernel/random/boot_id"),
    cgroup_root: Path = Path("/sys/fs/cgroup"),
) -> Verdict:
    lock_path = root / "worktree-state.lock"
    state_path = root / "worktree-state.json"
    try:
        with lock_path.open("a+", encoding="utf-8") as lock:
            fcntl.flock(lock, fcntl.LOCK_SH)
            try:
                state = json.loads(state_path.read_text(encoding="utf-8"))
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)
    except (OSError, json.JSONDecodeError) as error:
        return verdict(
            "indeterminate",
            "registry-unreadable",
            f"cannot read guarded slot registry: {error}",
            slot,
        )
    if not isinstance(state, Mapping):
        return verdict(
            "indeterminate", "registry-unreadable", "slot registry is not an object", slot
        )
    return assess_state(
        slot,
        state,
        proc_root=proc_root,
        boot_id_path=boot_id_path,
        cgroup_root=cgroup_root,
    )


def repository_root() -> Path:
    return Path(__file__).resolve().parents[1]


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--slot", required=True)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    result = assess(args.slot, repository_root())
    if args.json:
        print(json.dumps(result.render(), sort_keys=True))
    else:
        print(
            f"state={result.state} reason={result.reason_code} "
            f"slot={result.slot} agent={result.agent or '-'} detail={result.detail}"
        )
    return result.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
