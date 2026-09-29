"""Firing policy shared by the compatibility-website health gates.

Used by site_rebuild_health.py (is the nightly Actions rebuild succeeding?) and
site_data_freshness.py (is the data it publishes current?). Both used to page
only when their outcome key changed. That policy fails two ways, and the
round-1 review of https://github.com/rrnewton/dev-hermit/issues/394 named both:

  * A PERSISTING WARNING WARNED ONCE. A failure that nobody fixed on the first
    page was silent from the second tick on, which looks the same as a fixed one.
  * UNKNOWN WAS ONLY THE GENERIC NO-SIGNAL LINE. Exit 75 makes tick-hub print a
    generic "could not determine" action on every run, and the gate's own
    warning never fired. The two gates this replaced exited 75 on every run from
    2026-09-22 to 2026-09-28, and nobody acted on the generic line.

THE POLICY
  warning  every evaluation in a warning state fires (exit 1), so tick-hub emits
           the gate's own action on every evaluation for as long as the
           condition lasts. Both site gates run on every tick (cadence_secs: 0
           in tick-hub.yaml), so that is every scheduled and every manual tick.
           The outcome key and the time it last changed are kept, so every page
           says since when the warning has held.
  quiet    a quiet key (`current`) is recorded and does not fire.
  unknown  the first UNKNOWN_LIMIT - 1 consecutive unknown evaluations exit 75,
           as before, and tick-hub prints its generic no-signal action. The
           count is of evaluations, not of scheduled ticks: a manual tick
           advances it too. The UNKNOWN_LIMIT-th consecutive one, and every one
           after it, FIRES the gate's own warning with state `unknown`. A real
           verdict resets the count. An unknown evaluation never overwrites the
           last real outcome key.

THE PAGE TEXT IS STABLE WHILE THE CONDITION IS. The persistence clause names
the time the condition began, not its age or an evaluation count, so an
unchanged condition renders the same line on every tick. Each tick's findings
reach the escalation sink (ci-hub/bin/escalate), which adds nothing to an open
issue whose latest digest matches and re-states it after --repeat-after (one
day by default). A clause that changed every tick would defeat that and comment
every 30 minutes; firing every tick is what keeps the alarm open, and the sink
decides how often a person is told.

The state file is read, changed and replaced atomically under an exclusive
lock, so two ticks cannot interleave a read and a write.
"""

from __future__ import annotations

import contextlib
import datetime as dt
import fcntl
import json
import os
import tempfile
from pathlib import Path
from typing import Iterator

# Three consecutive unknown evaluations, taken on the 30-minute outer tick
# because exit 75 does not use up the reminder's cadence: about one hour of
# not knowing, then the gate's own warning.
UNKNOWN_LIMIT = 3
UNKNOWN_KEY = "unknown"
PERSISTENT_UNKNOWN_KEY = "unknown-persistent"


def iso(moment: dt.datetime) -> str:
    return moment.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_time(value: object) -> dt.datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed


@contextlib.contextmanager
def locked_state(path: Path) -> Iterator[dict]:
    """Read, yield for mutation, then atomically replace the state file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    lock_path = path.with_name(path.name + ".lock")
    with open(lock_path, "a+") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            state = json.loads(path.read_text())
            if not isinstance(state, dict):
                state = {}
        except (OSError, ValueError):
            state = {}
        yield state
        fd, temporary = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
        try:
            with os.fdopen(fd, "w") as handle:
                json.dump(state, handle, indent=2, sort_keys=True)
                handle.write("\n")
            os.replace(temporary, path)
        except BaseException:
            with contextlib.suppress(OSError):
                os.unlink(temporary)
            raise


def _hours(since: dt.datetime | None, now: dt.datetime) -> float | None:
    if since is None:
        return None
    return round(max(0.0, (now - since).total_seconds()) / 3600.0, 1)


def evaluate(
    state: dict,
    key: str,
    now: dt.datetime,
    *,
    quiet_keys: frozenset[str] = frozenset({"current"}),
    unknown_limit: int = UNKNOWN_LIMIT,
) -> dict:
    """Record one evaluation in `state` and decide whether it fires.

    `key` is the gate's outcome key, or UNKNOWN_KEY when the gate could not
    decide. Returns the fields a gate reports: whether it fires, the key it
    reports, how many consecutive unknowns there have been, and since when the
    reported condition has held.
    """
    if unknown_limit < 1:
        raise ValueError("unknown_limit must be at least 1")
    state["measured_at"] = iso(now)
    if key == UNKNOWN_KEY:
        count = int(state.get("consecutive_unknown") or 0) + 1
        state["consecutive_unknown"] = count
        if count == 1 or parse_time(state.get("unknown_since")) is None:
            state["unknown_since"] = iso(now)
        since = parse_time(state["unknown_since"])
        escalated = count >= unknown_limit
        return {
            "fire": escalated,
            "outcome_key": PERSISTENT_UNKNOWN_KEY if escalated else UNKNOWN_KEY,
            "consecutive_unknown": count,
            "unknown_limit": unknown_limit,
            "since": iso(since) if since else None,
            "since_hours": _hours(since, now),
            "last_known_outcome_key": state.get("outcome_key"),
        }
    state["consecutive_unknown"] = 0
    state.pop("unknown_since", None)
    previous = state.get("outcome_key")
    if key != previous or parse_time(state.get("changed_at")) is None:
        state["outcome_key"] = key
        state["changed_at"] = iso(now)
    since = parse_time(state["changed_at"])
    return {
        "fire": key not in quiet_keys,
        "outcome_key": key,
        "previous_outcome_key": previous,
        "consecutive_unknown": 0,
        "unknown_limit": unknown_limit,
        "since": iso(since) if since else None,
        "since_hours": _hours(since, now),
    }


def persistence_text(verdict: dict) -> str:
    """The clause every page carries about since when the condition has held.

    Deliberately free of anything that changes while the condition does not
    (age, evaluation count); see THE PAGE TEXT IS STABLE above. The verdict
    itself still carries `since_hours` and `consecutive_unknown`.
    """
    if verdict.get("outcome_key") in (UNKNOWN_KEY, PERSISTENT_UNKNOWN_KEY):
        return (
            f"undetermined since {verdict.get('since')}; the gate warns from "
            f"{verdict['unknown_limit']} consecutive undetermined evaluations"
        )
    return f"in this state since {verdict.get('since')}"
