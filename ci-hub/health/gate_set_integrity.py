#!/usr/bin/env python3
"""Assert that the gates which are supposed to be watching are still configured.

WHY THIS EXISTS
===============
A gate that disappears from `tick-hub.yaml` produces silence, and silence reads
as health. Three instances on 2026-08-19 alone, all the same shape:

  * the reverie pin gate emitted correctly for exactly ONE tick and then went
    quiet, because its fix lived only as an uncommitted edit in the shared
    primary and the tick executes the working tree;
  * `btrfs_headroom` was configured on a candidate branch and absent from the
    live config, so nothing watched /data for thirteen hours after the disk
    filled;
  * that same monitor stayed losable afterwards -- revert `tick-hub.yaml`, or
    leave the primary on a branch predating its landing, and the reminder is
    gone with no alarm.

None of those was detected by anything. Each was found by a person looking.

THE TRAP THIS MUST NOT FALL INTO
================================
The obvious implementation -- a hardcoded list of expected gates -- is itself a
config that can be reverted, leaving a checker that expects nothing and
therefore passes. That would be an inert mechanism whose entire purpose is
detecting inert mechanisms. So:

  * `REQUIRED` being empty, missing, or unreadable is a REFUSAL (exit 2), never
    a pass. An expectation list that reads as satisfied because it is empty is
    precisely the defect this file exists to catch.
  * A missing or unparseable tick config is a REFUSAL, not a pass. "I could not
    look" and "I looked and all is well" must never share an exit code.
  * The checker requires ITSELF to be configured, so an operator who prunes
    other reminders but keeps this one still gets an alarm.

TWO INDEPENDENT CHECKS, because a hardcoded list alone is not enough
====================================================================
  REQUIRED  a small in-source set of safety-critical reminders whose absence is
            an incident. Deliberately short: this is the "these must never
            silently vanish" list, not an inventory.

  DRIFT     the live reminder set compared against the same file on
            `origin/main`. This needs no hardcoded list and catches the case a
            list cannot: a gate that was added to main AFTER this checker was
            written and is missing locally because the checkout is stale or was
            reverted. Exactly how the headroom monitor would be lost.

Drift is reported as a finding, not a refusal, when git is available; when git
is NOT available the drift check reports itself as UNAVAILABLE rather than
quietly contributing nothing.

EXIT CODES
  0  every required reminder present, no drift
  1  a required reminder is MISSING, or the live set has drifted from main
  2  REFUSAL -- expectations empty/unreadable, or the config could not be read
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

import yaml

# Safety-critical reminders. Short on purpose: absence of any of these means
# something that is supposed to be watching is not.
#
# Each entry names what goes unwatched if it disappears -- so that a future
# reader deleting a line has to notice what they are switching off.
REQUIRED: dict[str, str] = {
    # The one that motivated this file. Absent for 13h after the 2026-08-17
    # out-of-disk incident; nothing watched the filesystem in that window.
    "btrfs_headroom": "filesystem headroom and runaway writers on /data",
    "orphaned_hermit_reaper": "orphaned Hermit and test processes that no live owner will reap",
    "host_resource_audit": "memory, swap, process, zombie, and disk pressure on the host",
    "chat_bridge_resource_health": (
        "Chat bridge services losing resource bounds or returning to runaway writes"
    ),
    "operational_input_freshness": "agent and disk snapshots whose producers silently stopped",
    "stale_tick_processes": "health-tick invocations that escaped their complete-run bound",
    # Per-slot disk residue; one of only two mechanisms found genuinely live
    # during the 2026-08-19 invocation audit.
    "worktree_disk_residue": "slots accumulating on disk with no live process",
    "rust_script_cache_cleanup": "generated rust-script packages accumulating without a bound",
    # The parity half. The cap check above was firing correctly at 77-against-12
    # and nobody knew; this one names the specific slots with nobody behind
    # them, against a denominator that maintains itself.
    "slot_agent_parity": "active slots outnumbering the agents actually alive",
    # The other genuinely-live one.
    "gate_runner_presence": "whether the gate runner is present at all",
    "integration_tip_validate": "whether the exact Hermit main tip receives a periodic validate attempt",
    # Went silent for a full day and was found by hand.
    "reverie_pin_lag_note": "the reverie pin falling behind and blocking snapshots",
    # A checker that can be removed without complaint is the defect it detects.
    "gate_set_integrity": "THIS CHECK -- that the watchers are still configured",
    # A deleted tool came back UNTRACKED and merged a head before anyone noticed
    # (gh-pr-merge-verified, 2026-08-25). Listed here because the deterrent that
    # failed was a docstring: this entry is what makes the mechanism itself
    # non-removable, which is the same argument as the line above.
    "deleted_tool_absent": "deliberately-deleted tools reappearing on disk",
    # The owner directive of 2026-09-28: the website build always happens and a
    # failed rebuild warns. The two gates that preceded this pair exited 75 on
    # every run for six days while the site went stale
    # (https://github.com/rrnewton/dev-hermit/issues/394).
    "site_rebuild_health": "compatibility website rebuild failures and a stopped builder",
    "site_data_freshness": "stale published website data and silently excluded rows",
}

def _protected_shape(
    name: str,
    cadence_secs: int,
    command: str,
    timeout_secs: int,
    component: str,
    title: str,
) -> dict[str, object]:
    """The exact raw reminder shape, not an incomplete projection of it.

    Tick-hub's canonical Reminder schema currently admits name, cadence_secs,
    cadence_offset_secs, cadence_window_secs, requires_flags, depends_on, gate,
    and emit. Gate admits cmd/when/capture/timeout_secs/parallel; Emit admits
    kind/title/skill/fields. Exact raw equality deliberately protects ABSENCE:
    adding a never-true flag, dependency, phase/window, or parallel execution is
    as visible as changing a value already present. It also rejects any future
    scheduling/jitter key until this executable expectation is reviewed.
    """
    return {
        "name": name,
        "cadence_secs": cadence_secs,
        "gate": {
            "timeout_secs": timeout_secs,
            "cmd": command,
            "when": "failure",
            "capture": True,
        },
        "emit": {
            "kind": "action",
            "skill": "hard-warn-coordinator",
            "fields": {"component": component},
            "title": title,
        },
    }


def _same_typed_value(actual: object, expected: object) -> bool:
    """Recursive equality where bool/int/float and nested variants differ.

    Python deliberately considers ``1800 == 1800.0`` and ``True == 1``. That
    is wrong for executable configuration: tick-hub's schema accepts an integer
    cadence/timeout, and silently accepting a YAML float here would let this
    integrity checker bless a row the canonical parser refuses.
    """
    if type(actual) is not type(expected):
        return False
    if isinstance(expected, dict):
        if not isinstance(actual, dict) or len(actual) != len(expected):
            return False
        unmatched = list(actual.items())
        for expected_key, expected_value in expected.items():
            for index, (actual_key, actual_value) in enumerate(unmatched):
                if _same_typed_value(actual_key, expected_key):
                    if not _same_typed_value(actual_value, expected_value):
                        return False
                    unmatched.pop(index)
                    break
            else:
                return False
        return not unmatched
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) == len(expected) and all(
            _same_typed_value(actual_item, expected_item)
            for actual_item, expected_item in zip(actual, expected, strict=True)
        )
    return actual == expected


# These transition gates are new enough that their names alone are not a
# durable mechanism. Pin their COMPLETE canonical reminder rows. A reminder
# with the right command but a never-true requires_flags, a suppressing
# dependency, or a cadence phase outside the outer tick is inert just as surely
# as one whose command was deleted.
REQUIRED_GATE_SHAPES: dict[str, dict[str, object]] = {
    "chat_bridge_resource_health": _protected_shape(
        "chat_bridge_resource_health",
        1800,
        "python3 ci-hub/health/chat_bridge_health.py "
        '--root "$DEV_HERMIT_PARENT"',
        15,
        "chat-bridge-resource-health",
        "Chat bridge resource health needs attention: {summary}",
    ),
    "host_resource_audit": _protected_shape(
        "host_resource_audit",
        900,
        "scripts/resource_audit.sh --tick-gate",
        30,
        "host-resource-audit",
        (
            "host memory, swap, process, zombie, or disk pressure needs "
            "attention: {summary}"
        ),
    ),
    "operational_input_freshness": _protected_shape(
        "operational_input_freshness",
        900,
        "python3 ci-hub/health/operational_health.py input-freshness "
        '--state-root "$DEV_HERMIT_PARENT"',
        10,
        "operational-input-freshness",
        (
            "agent or disk producer snapshots are stale, missing, or invalid: "
            "{summary}"
        ),
    ),
    "stale_tick_processes": _protected_shape(
        "stale_tick_processes",
        900,
        "python3 ci-hub/health/stale_tick_processes.py --gate "
        '--root "$DEV_HERMIT_PARENT" --max-age-secs 900',
        10,
        "stale-tick-processes",
        (
            "health-tick invocations outlived their complete-run bound: {summary}"
        ),
    ),
    "orphaned_hermit_reaper": _protected_shape(
        "orphaned_hermit_reaper",
        900,
        "python3 scripts/orphaned-hermit-reaper.py --report-orphans",
        60,
        "orphaned-hermit-reaper",
        (
            "orphaned Hermit/test processes are running unowned: {summary}"
        ),
    ),
}

# The two website gates are pinned as literal rows rather than through
# _protected_shape(): site_rebuild_health deliberately carries no timeout_secs
# (tick-hub's 30 s default applies; measured 2.1 s on 2026-09-28), and both
# emit an `outcome` field, which the helper does not model. Cadence 0 is
# tick-hub's EVERY_TICK; see the comment above them in tick-hub.yaml for why a
# 1800 s cadence let a tick skip them and still enqueue kind=resolve.
REQUIRED_GATE_SHAPES["site_rebuild_health"] = {
    "name": "site_rebuild_health",
    "cadence_secs": 0,
    "gate": {
        "cmd": (
            "./ci-hub/health/site_rebuild_health.py --gate --dispatch "
            '--state "$DEV_HERMIT_PARENT/.tick-hub/site-rebuild-health.json"'
        ),
        "when": "failure",
        "capture": True,
    },
    "emit": {
        "kind": "action",
        "skill": "hard-warn-coordinator",
        "fields": {"component": "site-rebuild", "outcome": "{outcome_key}"},
        "title": "compatibility website rebuild needs attention: {summary}",
    },
}
REQUIRED_GATE_SHAPES["site_data_freshness"] = {
    "name": "site_data_freshness",
    "cadence_secs": 0,
    "gate": {
        "timeout_secs": 180,
        "cmd": (
            "./ci-hub/health/site_data_freshness.py --gate "
            '--state "$DEV_HERMIT_PARENT/.tick-hub/site-data-freshness.json"'
        ),
        "when": "failure",
        "capture": True,
    },
    "emit": {
        "kind": "action",
        "skill": "hard-warn-coordinator",
        "fields": {
            "component": "site-data",
            "outcome": "{outcome_key}",
            "excluded_not_in_manifest": "{excluded_not_in_manifest}",
            "excluded_unaccepted": "{excluded_unaccepted}",
        },
        "title": "compatibility website data needs attention: {summary}",
    },
}

DEFAULT_CONFIG = "ci-hub/health/tick-hub.yaml"


def live_reminder_names(path: Path) -> set[str]:
    """Reminder names in the config, or raise. Never returns a silent empty set."""
    text = path.read_text(encoding="utf-8")
    doc = yaml.safe_load(text)
    if not isinstance(doc, dict):
        raise ValueError(f"{path} is not a YAML mapping")
    reminders = doc.get("reminders")
    if not isinstance(reminders, list) or not reminders:
        # A config with no reminders at all is the loudest possible version of
        # this defect, so it must not read as "nothing missing".
        raise ValueError(f"{path} has no reminders list")
    names = {r.get("name") for r in reminders if isinstance(r, dict)}
    names.discard(None)
    if not names:
        raise ValueError(f"{path} reminders carry no names")
    return names  # type: ignore[return-value]


def main_reminder_names(path: Path, ref: str) -> tuple[set[str] | None, str]:
    """The same file as of `ref`. Returns (names, why-unavailable)."""
    try:
        out = subprocess.run(
            ["git", "show", f"{ref}:{path.as_posix()}"],
            capture_output=True, text=True, timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return None, f"git unavailable: {exc}"
    if out.returncode != 0:
        return None, f"git show {ref}:{path} failed: {out.stderr.strip()[:120]}"
    try:
        doc = yaml.safe_load(out.stdout)
        reminders = doc.get("reminders") if isinstance(doc, dict) else None
        if not isinstance(reminders, list):
            return None, f"{ref} copy has no reminders list"
        names = {r.get("name") for r in reminders if isinstance(r, dict)}
        names.discard(None)
        return names, ""  # type: ignore[return-value]
    except yaml.YAMLError as exc:
        return None, f"{ref} copy did not parse: {exc}"


def unrunnable_required_gates(path: Path) -> tuple[list[str], str]:
    """Required reminders whose gate command names a script that is not there.

    WHY THIS EXISTS, and it is the defect that produced it. On 2026-08-19
    btrfs_headroom -- the only safety mechanism proven firing that day -- went
    silent with "refusing emission with unresolved placeholder(s): {summary}".
    The gate had not regressed and its summary construction was fine: its SCRIPT
    HAD BEEN DELETED from the working tree while the reminder stayed in the
    config. A missing script produces zero stdout, tick-hub finds no fields to
    substitute into the title, and refuses. A hard failure becomes silence.

    That is a SHARED failure mode, not a property of one gate: it happens to any
    reminder whose command cannot run, for any reason. Checking that a reminder
    is CONFIGURED is not the same as checking that it CAN RUN, and only the
    second would have caught this.

    Returns (unrunnable, why-unavailable). Inability to inspect is reported, not
    silently treated as fine.
    """
    try:
        doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        reminders = doc.get("reminders") if isinstance(doc, dict) else None
        if not isinstance(reminders, list):
            return [], f"{path} has no reminders list"
    except (OSError, yaml.YAMLError) as exc:
        return [], f"could not re-read {path}: {' '.join(str(exc).split())}"

    root = Path.cwd()
    broken: list[str] = []
    for r in reminders:
        if not isinstance(r, dict) or r.get("name") not in REQUIRED:
            continue
        name = str(r["name"])
        gate = r.get("gate")
        if not isinstance(gate, dict):
            broken.append(f"{name} -> gate definition is missing")
            continue
        cmd = gate.get("cmd")
        if not isinstance(cmd, str) or not cmd.strip():
            broken.append(f"{name} -> gate command is missing")
            continue
        cmd = cmd.strip()
        if gate.get("when") != "failure" or gate.get("capture") is not True:
            broken.append(
                f"{name} -> gate must use when=failure and capture=true"
            )
            continue
        expected = REQUIRED_GATE_SHAPES.get(name)
        if expected is not None and not _same_typed_value(r, expected):
            broken.append(f"{name} -> protected gate execution shape changed")
            continue
        # The script is the first token that looks like a repo-relative path.
        # Deliberately conservative: only a token we can resolve is judged, so an
        # exotic command line is left alone rather than reported as broken.
        for tok in cmd.split():
            if "/" not in tok or tok.startswith("-"):
                continue
            candidate = root / tok
            if candidate.suffix in {".py", ".sh", ".rs"} and not candidate.exists():
                broken.append(f"{r['name']} -> {tok} DOES NOT EXIST")
            break
    return broken, ""


def field(key: str, value: object) -> None:
    # tick-hub substitutes {placeholders} in a reminder title from the gate's
    # captured key=value fields. A gate that prints only prose is refused with
    # "unresolved placeholder(s)" -- which is how the reverie pin gate stayed
    # silent all day. So every gate here prints fields, always.
    print(f"{key}={value}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--config", default=DEFAULT_CONFIG)
    ap.add_argument("--main-ref", default="origin/main")
    ap.add_argument("--no-drift", action="store_true", help="skip the drift check")
    ap.add_argument("--json", dest="as_json", action="store_true")
    args = ap.parse_args(argv)

    # REFUSAL 1: our own expectations. An empty REQUIRED would make every check
    # below vacuously pass, so it is checked before anything else and is fatal.
    if not REQUIRED:
        print("state=unknown")
        field("summary", "REFUSING: this checker's REQUIRED set is EMPTY, so it "
                         "would pass no matter which gates are missing. That is the "
                         "defect it exists to detect, in itself.")
        return 2

    path = Path(args.config)
    try:
        live = live_reminder_names(path)
    except (OSError, ValueError, yaml.YAMLError) as exc:
        print("state=unknown")
        field("summary", f"REFUSING: could not read the gate set from {path}: "
                         f"{' '.join(str(exc).split())}. Could-not-look is not all-is-well.")
        return 2

    missing = sorted(k for k in REQUIRED if k not in live)
    unrunnable, unrunnable_note = unrunnable_required_gates(path)

    drift: list[str] = []
    drift_note = ""
    drift_status = "skipped"          # never report a skip as "performed"
    if not args.no_drift:
        drift_status = "performed"
        main_names, why = main_reminder_names(path, args.main_ref)
        if main_names is None:
            drift_note = f"drift check UNAVAILABLE ({why})"
            drift_status = "unavailable"
        else:
            drift = sorted(main_names - live)

    state = "ok"
    parts: list[str] = []
    if missing:
        state = "actionable"
        parts.append(
            "REQUIRED GATE(S) NOT CONFIGURED: "
            + "; ".join(f"{k} -- nothing is watching {REQUIRED[k]}" for k in missing)
        )
    if unrunnable:
        state = "actionable"
        parts.append(
            "CONFIGURED BUT CANNOT RUN: " + "; ".join(unrunnable)
            + " -- a gate whose script is missing emits nothing, so tick-hub"
              " refuses on an unresolved placeholder and the failure reads as silence"
        )
    if unrunnable_note:
        parts.append(f"runnability check UNAVAILABLE ({unrunnable_note})")
    if drift:
        state = "actionable"
        parts.append(
            f"CONFIGURED ON {args.main_ref} BUT MISSING LOCALLY: {', '.join(drift)}"
            " -- this checkout is stale or the reminder was reverted"
        )
    if drift_note:
        parts.append(drift_note)
    elif args.no_drift:
        parts.append("drift check SKIPPED by --no-drift")
    if not parts:
        parts.append(
            f"all {len(REQUIRED)} required gate(s) configured; "
            f"no drift from {args.main_ref}; {len(live)} reminder(s) live"
        )

    report = {
        "state": state,
        "summary": " | ".join(parts),
        "required_total": len(REQUIRED),
        "required_missing": len(missing),
        "missing": ",".join(missing) or "none",
        "drift_from_main": ",".join(drift) or "none",
        "live_reminders": len(live),
        "drift_check": drift_status,
        "unrunnable_required": ",".join(unrunnable) or "none",
    }
    if args.as_json:
        print(json.dumps(report, sort_keys=True))
    else:
        for key, value in report.items():
            field(key, value) if key != "state" else print(f"state={value}")
    return 1 if state == "actionable" else 0


if __name__ == "__main__":
    raise SystemExit(main())
