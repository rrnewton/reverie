#!/usr/bin/env python3
"""Fail-closed liveness authority for registered slot owner identities.

The allocator records a schema-versioned ``owner_sidecar`` inside the slot row
under the same registry lock as the ownership mutation.  This verifier reads
that exact row under a shared lock and answers only from the recorded
``(boot_id, supervisor_pid, start_ticks)`` identity.

For ``--slot``, exit codes deliberately match ``agent_liveness_probe.py``:

* 0: the recorded owner incarnation is proven dead;
* 1: the recorded owner incarnation is alive (adoption/release refused);
* 2: indeterminate (adoption/release refused).

Missing, malformed, unsupported, or mismatched evidence is indeterminate,
never dead.  There are no production path overrides: caller-selected evidence
would let a caller manufacture authority.

``--all`` is a transport optimization for census consumers: it reads one
guarded registry snapshot and returns every typed per-slot verdict plus the
snapshot digest. Exit 0 means that complete envelope was produced; it does not
mean any owner is dead or alive.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Mapping, Sequence, cast

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ci-hub" / "lib"))
from worktree_registry import (  # noqa: E402
    RegistrySchemaError,
    ValidatedRegistry,
    parse_registry_bytes,
)


SCHEMA_VERSION = 1
NOT_RUNNING_STATES = frozenset({"Z", "X"})
# residue_sweep's existing outer authority call is capped at 10 seconds. Leave
# its final two seconds for process teardown and the typed indeterminate report.
REGISTRY_LOCK_WAIT_SECONDS = 8.0
REGISTRY_LOCK_POLL_SECONDS = 0.05


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
    slot: str, state: Mapping[str, object]
) -> tuple[Mapping[str, object], Mapping[str, object]]:
    slots = state.get("slots")
    if not isinstance(slots, Mapping) or not isinstance(slots.get(slot), Mapping):
        raise ValueError("slot is absent from registry")
    slot_row = slots[slot]
    if slot_row.get("status") not in {
        "active",
        "held",
        "release-requested",
        "owner-lease-revoked",
        "lease-quarantined",
    }:
        raise ValueError(
            f"slot status is {slot_row.get('status')!r}, expected an owner-lease lifecycle state"
        )
    if not isinstance(state, ValidatedRegistry) or state.owner_sidecar_errors is None:
        raise ValueError("slot registry has no shared owner_sidecar validation")
    if slot not in state.owner_sidecar_errors:
        raise ValueError("shared owner_sidecar validation has no result for slot")
    sidecar_error = state.owner_sidecar_errors[slot]
    if sidecar_error is not None:
        raise ValueError(sidecar_error)
    owner = _mutating_owner(slot_row)
    sidecar = slot_row.get("owner_sidecar")
    if not isinstance(sidecar, Mapping):
        raise ValueError("slot has no versioned owner_sidecar")
    return owner, sidecar


def _missing_sidecar_reason(
    slot_row: Mapping[str, object], detail: str
) -> tuple[str, str]:
    """Separate "binding was tried and failed" from "no binding was ever tried".

    THE VERDICT IS THE SAME EITHER WAY -- indeterminate, and release refuses.
    What differs is what a reader can DO. A row that predates the sidecar
    carries no process identity and, per the sidecar migration contract, cannot
    be given one retroactively; it is permanently unprovable. A row whose
    binding failed at a named moment for a named reason usually becomes provable
    the instant adoption is re-run, because the reason is something like a
    stalled snapshot producer. Reporting one code for both is what let 21 rows
    accumulate (measured 2026-08-27, 21 of 85 owner-lease-lifecycle rows) with
    nothing able to say which of them were repairable.

    This reads the allocator's own record. It is not evidence of ownership and
    is never consulted for aliveness -- only to explain an indeterminate result.
    """

    failure = slot_row.get("owner_binding_failure")
    if (
        isinstance(failure, Mapping)
        and failure.get("schema_version") == 1
        and isinstance(failure.get("reason"), str)
        and isinstance(failure.get("recorded_at"), str)
    ):
        return (
            "owner-binding-failed",
            f"the allocator could not bind owner evidence at "
            f"{failure['recorded_at']}: {failure['reason']}. Re-run the exact "
            f"allocation from the live owner to make this row provable.",
        )
    return "sidecar-invalid", detail


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
        owner, sidecar = _validated_sidecar(slot, state)
    except (TypeError, ValueError) as error:
        return verdict(
            "indeterminate", *_missing_sidecar_reason(slot_row, str(error)), slot
        )

    agent = str(owner["name"])
    identity = {
        "source": sidecar["source"],
        "generation": sidecar["generation"],
        "supervisor_pid": sidecar["supervisor_pid"],
        "boot_id": sidecar["boot_id"],
        "start_ticks": sidecar["start_ticks"],
        "lease": sidecar.get("lease"),
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

    pid = cast(int, sidecar["supervisor_pid"])
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
    state, error = read_guarded_state(root)
    if error is not None:
        return verdict(
            "indeterminate",
            "registry-unreadable",
            error,
            slot,
        )
    return assess_state(
        slot,
        state,
        proc_root=proc_root,
        boot_id_path=boot_id_path,
        cgroup_root=cgroup_root,
    )


def read_guarded_state(
    root: Path,
    *,
    lock_wait_seconds: float = REGISTRY_LOCK_WAIT_SECONDS,
) -> tuple[Mapping[str, object], str | None]:
    """Read one exact registry snapshot under the canonical shared lock."""

    lock_path = root / "worktree-state.lock"
    state_path = root / "worktree-state.json"
    try:
        with lock_path.open("a+", encoding="utf-8") as lock:
            deadline = time.monotonic() + max(0.0, lock_wait_seconds)
            while True:
                try:
                    fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
                    break
                except BlockingIOError as error:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError(
                            "registry lock remained busy for "
                            f"{lock_wait_seconds:.1f}s; owner authority is indeterminate"
                        ) from error
                    time.sleep(min(REGISTRY_LOCK_POLL_SECONDS, remaining))
            try:
                raw_state = state_path.read_bytes()
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)
        state = parse_registry_bytes(
            raw_state,
            checker=Path(__file__).resolve().with_name(
                "check-worktree-registry.rs"
            ),
            include_owner_sidecars=True,
        )
    except (OSError, RegistrySchemaError) as error:
        return {}, f"cannot read guarded slot registry: {error}"
    return state, None


def registry_digest(state: Mapping[str, object]) -> str:
    encoded = json.dumps(
        state, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def assess_all(
    root: Path,
    *,
    proc_root: Path = Path("/proc"),
    boot_id_path: Path = Path("/proc/sys/kernel/random/boot_id"),
    cgroup_root: Path = Path("/sys/fs/cgroup"),
) -> tuple[dict[str, object] | None, str | None]:
    """Assess every active/held row from one guarded registry snapshot.

    Per-slot verdicts retain the exact typed authority. Bulk mode changes only
    transport: one process and one lock acquisition replace N serial processes
    and N independently contended lock acquisitions.
    """

    state, error = read_guarded_state(root)
    if error is not None:
        return None, error
    slots = state.get("slots")
    if not isinstance(slots, Mapping):
        return None, "slot registry slots is not an object"
    selected = sorted(
        str(slot)
        for slot, row in slots.items()
        if isinstance(row, Mapping)
        and row.get("status")
        in {
            "active",
            "held",
            "release-requested",
            "owner-lease-revoked",
            "lease-quarantined",
        }
    )
    authorities = {
        slot: assess_state(
            slot,
            state,
            proc_root=proc_root,
            boot_id_path=boot_id_path,
            cgroup_root=cgroup_root,
        ).render()
        for slot in selected
    }
    return {
        "schema_version": SCHEMA_VERSION,
        "registry_sha256": registry_digest(state),
        "authorities": authorities,
    }, None


def repository_root() -> Path:
    return Path(__file__).resolve().parents[1]


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Report whether registered slot owners are alive, dead, or indeterminate."
    )
    subject = parser.add_mutually_exclusive_group(required=True)
    subject.add_argument("--slot")
    subject.add_argument("--all", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.all:
        payload, error = assess_all(repository_root())
        if error is not None:
            if args.json:
                print(json.dumps({
                    "schema_version": SCHEMA_VERSION,
                    "state": "indeterminate",
                    "reason_code": "registry-unreadable",
                    "detail": error,
                }, sort_keys=True))
            else:
                print(f"state=indeterminate reason=registry-unreadable detail={error}")
            return 2
        assert payload is not None
        if args.json:
            print(json.dumps(payload, sort_keys=True))
        else:
            authorities = cast(dict[str, dict[str, object]], payload["authorities"])
            counts = {state: 0 for state in ("alive", "dead", "indeterminate")}
            for authority in authorities.values():
                counts[cast(str, authority["state"])] += 1
            print(
                f"authorities={len(authorities)} alive={counts['alive']} "
                f"dead={counts['dead']} indeterminate={counts['indeterminate']}"
            )
        return 0
    assert args.slot is not None
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
