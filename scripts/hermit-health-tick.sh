#!/usr/bin/env bash
# Durable driver for the dev-hermit operational health tick.
#
# ROOT CAUSE this replaces: the operational-health poll was previously driven by
# an ORC-session-bound wf.loop (operationalHealthHeartbeat in
# .orc/plugins/hermit-dev/index.ts). Session-runtime state is NOT a durable
# schedule: when the hosting coordinator session was recycled (~2026-08-03
# 08:53 PDT) the loop stopped and the replacement session restored only the
# stale reminder-only hermit-dev-pr-health spec, which never calls health-tick.
# fired-state froze for ~38h and the silence looked exactly like health.
#
# This script is also invoked by a systemd --user timer (Linger=yes) so health
# continues across logout, reboot, and session recycling. The ORC workflow is
# retained for its direct wakeup path; main() serializes their complete runs.
set -uo pipefail

# $HOME, not a literal: this file is version-controlled and the parent's
# portability gate rejects owner-specific paths. A shell DOES expand $HOME
# (unlike a systemd unit, which needs %h).
ROOT="${HOME:?HOME must be set}/work/dev-hermit"
FIRED_STATE="$ROOT/.tick-hub/fired-state"
INVLOG="$HOME/.local/state/hermit-health-tick-invocations.log"
# ⚠️ THE REPORT MUST EXIST ON DISK BEFORE THE TICK FINISHES, because the tick
# does not always finish. Both drivers run it through hermit-operational-tick's
# 1200s whole-subtree CPU budget and 619s blocked-work backstop, with a 650s
# last-resort outer deadline. Capturing health-tick's output in a command
# substitution meant the entire
# report lived in a shell variable of a process that was about to be killed, so
# every finding it had already made was destroyed. Measured 2026-09-03: two runs
# entered the gate loop, one of them for 508 seconds, and neither left a single
# finding line behind. health-tick writes each gate's verdict as it is decided,
# so redirecting to a file here means an interrupted tick still leaves the
# partial report, and the trap below still records it.
LIVE_REPORT="$HOME/.local/state/hermit-health-tick-last-report.txt"
RUN_LOCK="$HOME/.local/state/hermit-health-tick.lock"
LIVE_REPORT_STAMP=""
LIVE_REPORT_RECORDED=0
LIVE_REPORT_ESCALATED=0
# ⚠️ WHETHER THE REPORT IS COMPLETE, WHICH IS NOT THE SAME AS WHETHER IT IS EMPTY.
# The escalation runs from the exit trap too, so it also runs when an outer bound
# killed the tick partway. A killed tick can have produced NO findings yet and
# that must never be read as "nothing is wrong": measured 2026-09-03T19:49:45Z,
# a tick killed at its 300s bound reported zero findings and the resolve path
# CLOSED a live alarm as "no longer reported", retracting something that was
# true. Absence is not a clean report. This flag is what separates them.
TICK_REPORT_COMPLETE=0
# Publishing the findings off this host. The agent name is what makes the write
# attributable: the fork's GitHub author is a constant, so an unattributed issue
# is indistinguishable from one the owner wrote, and ci-hub/bin/gh refuses
# rather than create one. A scheduled service inherits no identity -- measured
# 2026-09-03, all nine identity signals resolve `unavailable` under systemd --
# so it is named here.
# Use the immutable tool root when the installed bootstrap supplied one. The
# parent checkout can be behind precisely because this writer has not advanced
# it yet; taking the writer from that checkout makes a landed sync repair unable
# to deploy itself. Direct/manual runs have no immutable root and retain the
# parent copy as their fallback.
PARENT_MAIN_WRITE="${DEV_HERMIT_TOOL_ROOT:-$ROOT}/scripts/parent-main-write"
ESCALATE="${HERMIT_HEALTH_ESCALATE:-$ROOT/ci-hub/bin/escalate}"
ESCALATION_AGENT="health-tick"
# BOTH SINKS, because they cover different outages and neither one covers the
# other. The fork is off this host and survives the whole fleet being down, which
# is the coordinator being ABSENT. The work queue gets the alarm DISPATCHED
# rather than needing to be noticed, which is the coordinator being PRESENT AND
# NOT LOOKING -- the failure this project actually had. When Orc is down nobody
# reads the queue either, so the queue sink is an addition and not a replacement.
# run-health-escalation drains one chronological queue and sends each event to
# TaskGraph before GitHub. A slow external request therefore cannot overtake an
# older queue update or prevent the work-queue sink from running first.
# ⚠️ NAMED, NEVER INHERITED. Measured 2026-09-03 from this very unit: with no
# TG_DB_PATH, `tg db current` answers "(no active database - using tasks)", while
# every coordinator and agent reads this one. A row filed into the database a
# scheduled service happens to inherit is an invisible alarm, not a quiet one.
ESCALATION_TG_DB="hermit2"
ESCALATION_PENDING_DIR="$ROOT/ignored/ci-hub/health-escalation-pending"
ESCALATION_LOG="$HOME/.local/state/hermit-health-escalation.log"
TOOL_CACHE="${DEV_HERMIT_TOOL_CACHE:-${HERMIT_HEALTH_TOOL_CACHE:-$HOME/.local/state/hermit-health-tool}}"
CANONICAL_REPOSITORY="rrnewton/dev-hermit"
CANONICAL_REMOTE_URL="https://github.com/rrnewton/dev-hermit.git"
GIT_TRANSPORT_ALLOWLIST="https"
GITHUB_CREDENTIAL_HELPER="${HOME:?HOME must be set}/.local/libexec/dev-hermit/gh"

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# THE OBJECT MIRROR, AND WHY THE 34.7 SECONDS EXISTED WITHOUT IT.
#
# The cache under $TOOL_CACHE/trees keeps one complete checkout per parent
# commit and retires the previous one. That is a TREE cache with no OBJECT
# cache, so every time origin/main moves -- continuously on this repository,
# ledger and series commits land all day -- materialization re-downloaded the
# entire object database it had one commit earlier.
#
# Measured 2026-09-03 against the real remote on this host:
#     git clone --no-checkout <canonical>                        36.9s
#     git clone --bare --no-tags <canonical>   (mirror, once)    33.9s
#     git -C mirror fetch --no-tags origin main                   0.6s
#     git clone --reference-if-able <mirror> --dissociate ...     11.3s
#     per tick: 36.9s -> 11.9s
#
# ⚠️ THE AUTHORITY BOUNDARY IS THE POINT AND IT IS UNTOUCHED. The mirror lives
# under the TOOL CACHE, which is immutable tooling, not under the mutable parent.
# Its only remote is $CANONICAL_REMOTE_URL and nothing else ever writes to it, so
# every object still comes from the canonical remote and the mutable parent is
# still never consulted for a byte. The clone's URL argument is unchanged, so the
# staging tree's `origin` is the canonical remote as before, and
# tool_root_is_complete verifies the result exactly as before.
#
# --dissociate, deliberately, and it is 11.3 of the 11.9: it copies the borrowed
# objects in so the published tool root is SELF-CONTAINED with no
# objects/info/alternates. Dropping it would be much faster and would leave
# immutable tooling depending on an object store outside itself.
#
# --reference-if-able rather than --reference: a missing or damaged mirror
# degrades to exactly the previous full clone. The cache's failure mode is
# slower, never wrong.
TOOL_OBJECT_MIRROR=""
TOOL_OBJECT_MIRROR_NAME="canonical-objects.git"

refresh_canonical_object_mirror() {
    local cache_anchor=$1 mirror
    TOOL_OBJECT_MIRROR=""
    mirror="$cache_anchor/$TOOL_OBJECT_MIRROR_NAME"
    if [[ -d $mirror ]]; then
        # Incremental: only what landed since the last tick crosses the network.
        if ! run_git --network -C "$mirror" fetch --quiet --no-tags origin \
            '+refs/heads/main:refs/heads/main'; then
            printf 'operational-tool: object mirror fetch failed; falling back to a full clone\n' >&2
            return 0
        fi
    else
        if ! run_git --network -C "$cache_anchor" clone --quiet --bare --no-tags \
            "$CANONICAL_REMOTE_URL" "$mirror"; then
            printf 'operational-tool: could not seed the object mirror; falling back to a full clone\n' >&2
            rm -rf "$mirror" 2>/dev/null || true
            return 0
        fi
    fi
    # The mirror is a cache OF the canonical remote and nothing else. Verify that
    # every time rather than trusting the directory's history: a mirror whose
    # origin has been repointed would be a second authority, which is exactly
    # what this whole materialization exists to prevent.
    local mirror_origin
    mirror_origin=$(run_git -C "$mirror" config --get remote.origin.url 2>/dev/null) || return 0
    if [[ $mirror_origin != "$CANONICAL_REMOTE_URL" ]]; then
        printf 'operational-tool: object mirror origin is %s, not the canonical remote; ignoring it\n' \
            "$mirror_origin" >&2
        return 0
    fi
    TOOL_OBJECT_MIRROR=$mirror
}

# Copy whatever the tick has reported so far into the durable invocation log.
#
# Called on the ordinary path AND from the exit trap, which is what makes an
# interrupted tick self-describing: SIGTERM from either outer bound runs the
# trap, so the findings already on disk are recorded with the same start stamp
# as the rest of the invocation. `LIVE_REPORT_RECORDED` keeps the two callers
# from writing the same finding twice; a tick killed with SIGKILL records
# nothing here, but the report itself is still readable at LIVE_REPORT.
record_live_report_findings() {
    [ "$LIVE_REPORT_RECORDED" -eq 1 ] && return 0
    [ -n "$LIVE_REPORT_STAMP" ] || return 0
    [ -s "$LIVE_REPORT" ] || return 0
    LIVE_REPORT_RECORDED=1
    while IFS= read -r finding || [ -n "$finding" ]; do
        case "$finding" in
            ACTION:*|NOTE:*|ERROR:*|NO_RESULT:*|CLEAN:*|SUPPRESSED:*)
                printf '%s finding=%s\n' "$LIVE_REPORT_STAMP" "$finding" >>"$INVLOG"
                ;;
        esac
    done <"$LIVE_REPORT"
}

# Publish the tick's findings where they can be read when Orc is not running.
#
# ⚠️ THE TICK HAS NEVER PUSHED ITS OWN FINDINGS ANYWHERE. This file contained
# zero references to any relay, and the one channel that was wired to the
# coordinator carried the staleness alarm -- whether the MONITOR IS ALIVE -- not
# what the monitor found. So a cycle that produced fifteen findings put them in
# the invocation log and told nobody. That log is on the same host as the thing
# it is reporting on, which is exactly the wrong place for "this host's
# monitoring is dark".
#
# ci-hub/bin/escalate publishes off this host and reads the publication back
# before calling it delivered; it is quiet while the findings are unchanged, and
# it closes its issue when a later tick reports nothing, so the issue list keeps
# saying what is true now. Every outcome is recorded here, including the one
# where publication failed -- a caller that cannot tell "published" from
# "written down locally and nobody told" is the defect this replaces.
#
# Bounded and never fatal: this runs from the exit trap as well, so it must fit
# inside the stop timeout, and a tick whose findings could not be published is
# still a tick that ran.
#
# ⚠️ THROUGH with-proxy, for the same reason health-tick is. A scheduled service
# inherits no proxy variables and this host has no direct external route.
# Measured 2026-09-03 without it, from the real unit: `dial tcp 140.82.112.5:443:
# connect: network is unreachable`, and the alarm was recorded locally and
# correctly reported as undelivered. That refusal is the design working; needing
# it every hour is not.
escalate_live_report_findings() {
    [ "$LIVE_REPORT_ESCALATED" -eq 1 ] && return 0
    [ -n "$LIVE_REPORT_STAMP" ] || return 0
    LIVE_REPORT_ESCALATED=1
    local temporary event kind count suffix
    mkdir -p -- "$ESCALATION_PENDING_DIR" 2>/dev/null || return 0
    temporary=$(mktemp "$ESCALATION_PENDING_DIR/.event-$(date -u +%Y%m%dT%H%M%S%NZ)-$$-XXXXXX") || return 0
    if [ -s "$LIVE_REPORT" ]; then
        grep -E '^(ACTION|ERROR):' "$LIVE_REPORT" >"$temporary" 2>/dev/null || true
    fi
    # NOT `grep -c ... || echo 0`: grep prints 0 AND exits 1 on no match, so the
    # fallback would append a second 0 and the arithmetic below would then fail
    # with "integer expression expected" -- on exactly the clean-tick path.
    count=$(grep -c . "$temporary" 2>/dev/null)
    [[ $count =~ ^[0-9]+$ ]] || count=0
    if [ "$count" -eq 0 ] && [ "$TICK_REPORT_COMPLETE" -ne 1 ]; then
        # NOT a clean tick -- an unfinished one. Retracting an alarm requires
        # having looked, and this run was stopped before it could. Withdrawing a
        # true warning because the check was killed is worse than saying nothing.
        printf '%s escalation_skipped=incomplete-report findings=0\n' \
            "$LIVE_REPORT_STAMP" >>"$INVLOG"
        rm -f -- "$temporary"
        return 0
    fi
    if [ "$count" -eq 0 ]; then
        kind=resolve
    else
        kind=alarm
    fi
    suffix=${temporary##*-}
    event="$ESCALATION_PENDING_DIR/event-$(date -u +%Y%m%dT%H%M%S%NZ)-$$-$suffix.$kind"
    mv -- "$temporary" "$event" || { rm -f -- "$temporary"; return 0; }
    if launch_health_escalation; then
        printf '%s escalation=queued event=%q kind=%s findings=%s\n' \
            "$LIVE_REPORT_STAMP" "$event" "$kind" "$count" >>"$INVLOG"
    else
        printf '%s escalation=retained event=%q kind=%s findings=%s reason=detached-launch-refused\n' \
            "$LIVE_REPORT_STAMP" "$event" "$kind" "$count" >>"$INVLOG"
    fi
}

launch_health_escalation() {
    local unit launch_error
    local -a delivery_command
    unit="hermit-health-escalation-$(date -u +%Y%m%dT%H%M%S%NZ)-$$-$RANDOM"
    mkdir -p -- "$ESCALATION_PENDING_DIR" 2>/dev/null || return 1
    if [[ -n ${DEV_HERMIT_OPERATIONAL_TOOL:-} && \
          -x ${DEV_HERMIT_OPERATIONAL_TOOL:-} && \
          ${DEV_HERMIT_TOOL_PARENT_SHA:-} =~ ^[0-9a-f]{40}$ ]]; then
        delivery_command=(
            "$DEV_HERMIT_OPERATIONAL_TOOL"
            --run-current-tool
            --state-root "$ROOT"
            --tool-parent-sha "$DEV_HERMIT_TOOL_PARENT_SHA"
            --tool-relative ci-hub/bin/run-health-escalation
            --
            --queue "$ESCALATION_PENDING_DIR"
        )
    else
        delivery_command=(
            "$ROOT/ci-hub/bin/run-health-escalation"
            --queue "$ESCALATION_PENDING_DIR"
        )
    fi
    if launch_error=$(systemd-run --user --quiet --collect \
        --unit="$unit" \
        --description="deliver retained operational health findings" \
        --property=CPUQuota=100% \
        --property=RuntimeMaxSec=180 \
        --property="StandardOutput=append:$ESCALATION_LOG" \
        --property="StandardError=append:$ESCALATION_LOG" \
        --working-directory="$ROOT" \
        --setenv="PATH=$PATH" \
        --setenv="DG_AGENT_NAME=$ESCALATION_AGENT" \
        --setenv="DEV_HERMIT_PARENT=$ROOT" \
        --setenv="HERMIT_HEALTH_ESCALATE=$ESCALATE" \
        --setenv="HERMIT_HEALTH_TG_DB=$ESCALATION_TG_DB" \
        -- "${delivery_command[@]}" \
        2>&1 >/dev/null); then
        return 0
    fi
    printf '%s detached_escalation_refused queue=%q detail=%q\n' \
        "$(ts)" "$ESCALATION_PENDING_DIR" "$launch_error" >>"$INVLOG"
    return 1
}

retry_pending_health_escalations() {
    [[ -d $ESCALATION_PENDING_DIR ]] || return 0
    find "$ESCALATION_PENDING_DIR" -maxdepth 1 -type f \
        \( -name 'event-*.alarm' -o -name 'event-*.resolve' \) -print -quit \
        | grep -q . || return 0
    launch_health_escalation || true
}

tool_root_is_complete() {
    local root=$1 expected=$2
    PYTHONDONTWRITEBYTECODE=1 python3 - "$root" "$expected" <<'PY'
import hashlib
import os
import stat
import subprocess
import sys
from pathlib import Path


class InvalidCheckout(RuntimeError):
    pass


def git(repo: Path, *args: str) -> bytes:
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    parts = repo.parts
    pass_fds: tuple[int, ...] = ()
    if len(parts) >= 5 and parts[:4] == ("/", "proc", "self", "fd"):
        try:
            pass_fds = (int(parts[4]),)
        except ValueError as error:
            raise InvalidCheckout(f"invalid retained-directory path {repo}") from error
    try:
        result = subprocess.run(
            ["git", "--no-replace-objects", "-C", str(repo), *args],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            pass_fds=pass_fds,
        )
    except OSError as error:
        raise InvalidCheckout(f"cannot run git in {repo}: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip() or f"exit {result.returncode}"
        raise InvalidCheckout(f"git {' '.join(args)} failed in {repo}: {detail}")
    return result.stdout


def parse_tree(repo: Path, commit: str) -> dict[str, tuple[str, str, str]]:
    entries: dict[str, tuple[str, str, str]] = {}
    for raw in git(repo, "ls-tree", "-rz", "--full-tree", commit).split(b"\0"):
        if not raw:
            continue
        try:
            metadata, raw_path = raw.split(b"\t", 1)
            mode, kind, oid = metadata.decode("ascii").split()
            relative = os.fsdecode(raw_path)
        except (ValueError, UnicodeDecodeError) as error:
            raise InvalidCheckout(f"malformed tree entry in {repo} at {commit}") from error
        path = Path(relative)
        if path.is_absolute() or ".." in path.parts or relative in entries:
            raise InvalidCheckout(f"unsafe or duplicate tree path {relative!r} in {repo}")
        entries[relative] = (mode, kind, oid)
    return entries


def parse_index(repo: Path) -> dict[str, tuple[str, str]]:
    entries: dict[str, tuple[str, str]] = {}
    for raw in git(repo, "ls-files", "--stage", "-z").split(b"\0"):
        if not raw:
            continue
        try:
            metadata, raw_path = raw.split(b"\t", 1)
            mode, oid, stage = metadata.decode("ascii").split()
            relative = os.fsdecode(raw_path)
        except (ValueError, UnicodeDecodeError) as error:
            raise InvalidCheckout(f"malformed index entry in {repo}") from error
        if stage != "0" or relative in entries:
            raise InvalidCheckout(f"unmerged or duplicate index path {relative!r} in {repo}")
        entries[relative] = (mode, oid)
    return entries


def blob_oid(data: bytes, object_format: str) -> str:
    try:
        digest = hashlib.new(object_format)
    except ValueError as error:
        raise InvalidCheckout(f"unsupported Git object format {object_format!r}") from error
    digest.update(f"blob {len(data)}\0".encode("ascii"))
    digest.update(data)
    return digest.hexdigest()


def verify_repo(
    repo: Path,
    expected: str,
    label: str,
    required_gitlink: str | None,
) -> str | None:
    if not repo.is_dir():
        raise InvalidCheckout(f"{label} checkout is absent at {repo}")
    head = git(repo, "rev-parse", "--verify", "HEAD^{commit}").decode().strip()
    if head != expected:
        raise InvalidCheckout(f"{label} HEAD is {head}, expected {expected}")
    object_format = git(repo, "rev-parse", "--show-object-format").decode().strip()
    tree = parse_tree(repo, expected)
    index = parse_index(repo)
    expected_index = {
        path: (mode, oid) for path, (mode, _kind, oid) in tree.items()
    }
    if index != expected_index:
        raise InvalidCheckout(f"{label} index does not exactly represent {expected}")

    gitlinks = {
        path: oid
        for path, (mode, kind, oid) in tree.items()
        if mode == "160000" and kind == "commit"
    }
    for relative, (mode, kind, oid) in tree.items():
        if relative in gitlinks:
            continue
        path = repo / relative
        try:
            metadata = path.lstat()
        except OSError as error:
            raise InvalidCheckout(f"{label} tracked path {relative!r} is unreadable: {error}") from error
        if mode == "120000":
            if not stat.S_ISLNK(metadata.st_mode):
                raise InvalidCheckout(f"{label} tracked symlink {relative!r} changed type")
            data = os.fsencode(os.readlink(path))
        elif mode in {"100644", "100755"} and kind == "blob":
            if not stat.S_ISREG(metadata.st_mode):
                raise InvalidCheckout(f"{label} tracked file {relative!r} changed type")
            if metadata.st_nlink != 1:
                raise InvalidCheckout(
                    f"{label} tracked file {relative!r} has link count "
                    f"{metadata.st_nlink}, expected 1"
                )
            executable = bool(metadata.st_mode & 0o111)
            if executable != (mode == "100755"):
                raise InvalidCheckout(f"{label} tracked file {relative!r} changed executable mode")
            try:
                data = path.read_bytes()
            except OSError as error:
                raise InvalidCheckout(f"{label} tracked file {relative!r} is unreadable: {error}") from error
        else:
            raise InvalidCheckout(
                f"{label} has unsupported tree entry {mode} {kind} at {relative!r}"
            )
        if blob_oid(data, object_format) != oid:
            raise InvalidCheckout(f"{label} tracked bytes differ at {relative!r}")

    def inspect(directory: Path, prefix: Path = Path()) -> None:
        try:
            children = list(os.scandir(directory))
        except OSError as error:
            raise InvalidCheckout(f"cannot inspect {label} directory {directory}: {error}") from error
        for child in children:
            relative = prefix / child.name
            relative_text = relative.as_posix()
            if not prefix.parts and child.name == ".git":
                continue
            if relative_text in gitlinks:
                if not child.is_dir(follow_symlinks=False):
                    raise InvalidCheckout(f"{label} gitlink {relative_text!r} changed type")
                if relative_text != required_gitlink:
                    try:
                        with os.scandir(child.path) as entries:
                            populated = next(entries, None) is not None
                    except OSError as error:
                        raise InvalidCheckout(
                            f"cannot inspect uninitialized gitlink {child.path}: {error}"
                        ) from error
                    if populated:
                        raise InvalidCheckout(
                            f"{label} unexpected initialized gitlink content at {relative_text!r}"
                        )
                continue
            if child.is_dir(follow_symlinks=False):
                inspect(Path(child.path), relative)
            elif relative_text not in tree:
                raise InvalidCheckout(
                    f"{label} has unexpected untracked or ignored content at {relative_text!r}"
                )

    inspect(repo)
    if required_gitlink is None:
        return None
    if required_gitlink not in gitlinks:
        raise InvalidCheckout(
            f"{label} commit {expected} has no required mode-160000 gitlink at {required_gitlink}"
        )
    return gitlinks[required_gitlink]


try:
    root = Path(sys.argv[1])
    if not root.is_absolute() or not root.is_dir():
        raise InvalidCheckout(f"tool root is not an absolute directory: {root}")
    parent_expected = sys.argv[2]
    hermit_expected = verify_repo(root, parent_expected, "parent", "hermit")
    if hermit_expected is None:
        raise InvalidCheckout("parent Hermit gitlink could not be resolved")
    agent_utils_expected = verify_repo(
        root / "hermit", hermit_expected, "Hermit", "agent-utils"
    )
    if agent_utils_expected is None:
        raise InvalidCheckout("Hermit agent-utils gitlink could not be resolved")
    verify_repo(
        root / "hermit" / "agent-utils",
        agent_utils_expected,
        "agent-utils",
        None,
    )
    if not os.access(root / "ci-hub/bin/health-tick", os.X_OK):
        raise InvalidCheckout("parent health-tick entrypoint is not executable")
except (InvalidCheckout, OSError) as error:
    print(f"health-tick: tool checkout verification failed: {error}", file=sys.stderr)
    raise SystemExit(1)
PY
}

TOOL_STAGING_ROOT=""
TOOL_STAGING_ENTRY_NAME=""
TOOL_STAGING_IDENTITY=""
TOOL_STAGING_HOLDER_PID=""
TOOL_STAGING_HOLDER_WAIT_PID=""
TOOL_STAGING_HOLDER_CONTROL_FD=""
TOOL_STAGING_HOLDER_OUTPUT_FD=""
TOOL_STAGING_MAY_BE_PUBLISHED=0
TOOL_ROOT_RESULT=""
TOOL_CACHE_FD=""
TOOL_TREES_FD=""
TOOL_TARGET_ROOT=""
TOOL_RUN_ROOT=""
TOOL_RUN_ENTRY_NAME=""
TOOL_HOLDER_PID=""
TOOL_HOLDER_WAIT_PID=""
TOOL_HOLDER_CONTROL_FD=""
TOOL_HOLDER_OUTPUT_FD=""
TOOL_AUTHORITY_PATH=""
TOOL_CONTENT_SHA256=""
TOOL_PARENT_SHA=""
TOOL_HERMIT_SHA=""
TOOL_AGENT_UTILS_SHA=""
TOOL_BOOTSTRAP_SHA256=""
TICK_CHILD_PID=""
TICK_OUTPUT_FD=""
TICK_OUTPUT_HOLD_FD=""
TICK_OUTPUT_DIR=""
TICK_OUTPUT_PIPE=""
TICK_OUTPUT_READY=""
TICK_CHILD_SPAWN_PENDING=0
TICK_CANCEL_SIGNAL=""
TICK_CANCEL_STATUS=""

close_tick_output() {
    if [[ -n $TICK_OUTPUT_FD ]]; then
        exec {TICK_OUTPUT_FD}<&-
        TICK_OUTPUT_FD=""
    fi
    if [[ -n $TICK_OUTPUT_HOLD_FD ]]; then
        exec {TICK_OUTPUT_HOLD_FD}>&-
        TICK_OUTPUT_HOLD_FD=""
    fi
    if [[ -n $TICK_OUTPUT_PIPE ]]; then
        rm -f -- "$TICK_OUTPUT_PIPE"
        TICK_OUTPUT_PIPE=""
    fi
    if [[ -n $TICK_OUTPUT_READY ]]; then
        rm -f -- "$TICK_OUTPUT_READY"
        TICK_OUTPUT_READY=""
    fi
    if [[ -n $TICK_OUTPUT_DIR ]]; then
        rmdir -- "$TICK_OUTPUT_DIR" 2>/dev/null || true
        TICK_OUTPUT_DIR=""
    fi
}

finish_tick_cancellation() {
    local state="" _
    trap - INT TERM HUP
    if [[ -n $TICK_CHILD_PID ]]; then
        kill -s "$TICK_CANCEL_SIGNAL" -- "-$TICK_CHILD_PID" 2>/dev/null || true
        # Hand the already-streamed report to an independent service before
        # waiting for the child. Delivery then runs concurrently with teardown
        # and survives the wrapper's two-second hard-kill window.
        record_live_report_findings
        escalate_live_report_findings
        # ORC gives this wrapper two seconds before SIGKILL. Give the measured
        # child one second to finish its signal handler, then reap the complete
        # process group before the wrapper releases its private tool tree.
        for _ in {1..50}; do
            if [[ ! -r /proc/$TICK_CHILD_PID/stat ]] || \
               ! read -r _ _ state _ < "/proc/$TICK_CHILD_PID/stat" || \
               [[ $state == Z ]]; then
                break
            fi
            sleep 0.02
        done
        kill -KILL -- "-$TICK_CHILD_PID" 2>/dev/null || true
        wait "$TICK_CHILD_PID" 2>/dev/null || true
        TICK_CHILD_PID=""
    fi
    close_tick_output
    exit "$TICK_CANCEL_STATUS"
}

record_tick_cancellation() {
    TICK_CANCEL_SIGNAL=$1
    TICK_CANCEL_STATUS=$2
    if [[ -n $TICK_CHILD_PID ]]; then
        finish_tick_cancellation
    fi
    # Between declaring spawn intent and assigning $!, returning is the only
    # safe action. The launcher checks this latch immediately after assignment
    # and forwards the signal to the now-observable child session.
    if [[ $TICK_CHILD_SPAWN_PENDING -eq 1 ]]; then
        return 0
    fi
    exit "$TICK_CANCEL_STATUS"
}

cleanup_tool_roots() {
    local rc=${1:-$?}
    trap - EXIT INT TERM HUP
    close_tick_output
    if [[ -n $TOOL_HOLDER_PID ]]; then
        local entry_matched_before=0 holder_status="" holder_ready_to_release=0
        if [[ -n $TOOL_RUN_ENTRY_NAME ]] && held_directory_entry_matches \
            "$TOOL_CACHE_FD" "$TOOL_RUN_ENTRY_NAME" "$TOOL_RUN_ROOT"; then
            entry_matched_before=1
        fi
        if [[ -n $TOOL_HOLDER_CONTROL_FD ]]; then
            if ! printf 'cleanup\n' >&"$TOOL_HOLDER_CONTROL_FD"; then
                [[ $rc -ne 0 ]] || rc=1
            fi
        fi
        if [[ -n $TOOL_HOLDER_OUTPUT_FD ]] && \
           read -r holder_status <&"$TOOL_HOLDER_OUTPUT_FD" && \
           [[ $holder_status == cleaned ]]; then
            holder_ready_to_release=1
        else
            printf 'operational-tool: could not clear retained private tool root\n' >&2
            [[ $rc -ne 0 ]] || rc=1
        fi
        # There is no Linux rmdir-by-directory-fd operation. Deleting this
        # pathname after an inode comparison would recreate a compare/delete
        # race and could recursively remove a same-name replacement. Leave the
        # now-empty retained directory in the cache; the next invocation reuses
        # it under an exclusive flock. A changed entry is preserved verbatim.
        if [[ $entry_matched_before -ne 1 ]] || \
           ! held_directory_entry_matches \
               "$TOOL_CACHE_FD" "$TOOL_RUN_ENTRY_NAME" "$TOOL_RUN_ROOT"; then
            printf 'operational-tool: private tool root entry changed; preserving it and the held directory\n' >&2
            [[ $rc -ne 0 ]] || rc=1
        fi
        if [[ $holder_ready_to_release -eq 1 && -n $TOOL_HOLDER_CONTROL_FD ]]; then
            if ! printf 'release\n' >&"$TOOL_HOLDER_CONTROL_FD"; then
                [[ $rc -ne 0 ]] || rc=1
            fi
            exec {TOOL_HOLDER_CONTROL_FD}>&-
            TOOL_HOLDER_CONTROL_FD=""
        fi
        if [[ -n $TOOL_HOLDER_CONTROL_FD ]]; then
            exec {TOOL_HOLDER_CONTROL_FD}>&-
            TOOL_HOLDER_CONTROL_FD=""
        fi
        if [[ -n $TOOL_HOLDER_WAIT_PID ]] && ! wait "$TOOL_HOLDER_WAIT_PID"; then
            [[ $rc -ne 0 ]] || rc=1
        fi
        [[ -z $TOOL_HOLDER_OUTPUT_FD ]] || exec {TOOL_HOLDER_OUTPUT_FD}<&-
    fi
    if [[ -n $TOOL_STAGING_HOLDER_PID ]]; then
        local staging_action=cleanup
        [[ $TOOL_STAGING_MAY_BE_PUBLISHED -eq 0 ]] || staging_action=release
        if ! stop_staging_holder "$staging_action"; then
            printf 'operational-tool: could not safely release retained staging root\n' >&2
            [[ $rc -ne 0 ]] || rc=1
        fi
    fi
    [[ -z $TOOL_TREES_FD ]] || exec {TOOL_TREES_FD}<&-
    [[ -z $TOOL_CACHE_FD ]] || exec {TOOL_CACHE_FD}<&-
    exit "$rc"
}

finish_wrapper() {
    local rc=$?
    # Preserve the wrapper's status across the report-delivery calls. Without
    # this handoff, cleanup would see the final delivery helper's status and a
    # terminated tick could be rewritten from 143 to 0.
    trap - EXIT
    record_live_report_findings
    escalate_live_report_findings
    cleanup_tool_roots "$rc"
}

stop_staging_holder() {
    local action=$1 holder_status="" rc=0
    [[ -n $TOOL_STAGING_HOLDER_PID ]] || return 0
    if [[ -n $TOOL_STAGING_HOLDER_CONTROL_FD ]]; then
        if ! printf '%s\n' "$action" >&"$TOOL_STAGING_HOLDER_CONTROL_FD"; then
            rc=1
        fi
    fi
    if [[ $action == cleanup ]]; then
        if [[ -n $TOOL_STAGING_HOLDER_OUTPUT_FD ]] && \
           read -r holder_status <&"$TOOL_STAGING_HOLDER_OUTPUT_FD" && \
           [[ $holder_status == cleaned ]]; then
            if ! printf 'release\n' >&"$TOOL_STAGING_HOLDER_CONTROL_FD"; then
                rc=1
            fi
        else
            rc=1
        fi
    fi
    [[ -z $TOOL_STAGING_HOLDER_CONTROL_FD ]] || \
        exec {TOOL_STAGING_HOLDER_CONTROL_FD}>&-
    if [[ -n $TOOL_STAGING_HOLDER_WAIT_PID ]] && \
       ! wait "$TOOL_STAGING_HOLDER_WAIT_PID"; then
        rc=1
    fi
    [[ -z $TOOL_STAGING_HOLDER_OUTPUT_FD ]] || \
        exec {TOOL_STAGING_HOLDER_OUTPUT_FD}<&-
    TOOL_STAGING_ROOT=""
    TOOL_STAGING_ENTRY_NAME=""
    TOOL_STAGING_IDENTITY=""
    TOOL_STAGING_HOLDER_PID=""
    TOOL_STAGING_HOLDER_WAIT_PID=""
    TOOL_STAGING_HOLDER_CONTROL_FD=""
    TOOL_STAGING_HOLDER_OUTPUT_FD=""
    TOOL_STAGING_MAY_BE_PUBLISHED=0
    return "$rc"
}

retire_old_tool_roots() {
    local current_sha=$1
    PYTHONDONTWRITEBYTECODE=1 python3 - \
        "$TOOL_TREES_FD" "$TOOL_CACHE/trees" "$current_sha" <<'PY'
import ctypes
import fcntl
import os
import platform
import re
import secrets
import stat
import sys


trees_fd = int(sys.argv[1])
trees_path = sys.argv[2]
current_sha = sys.argv[3]
sha_pattern = re.compile(r"[0-9a-f]{40}")
retiring_pattern = re.compile(r"\.retiring\.[0-9a-f]{40}\.[0-9a-f]{16}")

# The immediately previous materialisation remains available for a systemd
# command that was accepted with an exact --tool-parent-sha but has not yet
# opened that cached root. Once a launcher opens a root, its shared flock below
# is the authority that keeps it alive. No code compares against an older tree.
keep_count = 2


def list_directory(fd: int) -> list[str]:
    scan_fd = os.open(
        ".", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd
    )
    try:
        return os.listdir(scan_fd)
    finally:
        os.close(scan_fd)


def open_held_directory(name: str) -> tuple[int, os.stat_result]:
    before = os.stat(name, dir_fd=trees_fd, follow_symlinks=False)
    if not stat.S_ISDIR(before.st_mode):
        raise RuntimeError("entry is not a real directory")
    fd = os.open(
        name,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        dir_fd=trees_fd,
    )
    held = os.fstat(fd)
    current = os.stat(name, dir_fd=trees_fd, follow_symlinks=False)
    if (
        not stat.S_ISDIR(held.st_mode)
        or not stat.S_ISDIR(current.st_mode)
        or held.st_nlink < 1
        or current.st_nlink < 1
        or (held.st_dev, held.st_ino) != (before.st_dev, before.st_ino)
        or (held.st_dev, held.st_ino) != (current.st_dev, current.st_ino)
        or os.readlink(f"/proc/self/fd/{fd}") != os.path.join(trees_path, name)
    ):
        os.close(fd)
        raise RuntimeError("directory changed before its descriptor was retained")
    return fd, held


def verify_tree(fd: int, device: int) -> None:
    for name in list_directory(fd):
        entry = os.stat(name, dir_fd=fd, follow_symlinks=False)
        if entry.st_dev != device:
            raise RuntimeError(f"entry crosses a filesystem boundary: {name}")
        if stat.S_ISDIR(entry.st_mode):
            child_fd = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                dir_fd=fd,
            )
            try:
                held = os.fstat(child_fd)
                if (held.st_dev, held.st_ino) != (entry.st_dev, entry.st_ino):
                    raise RuntimeError(f"directory changed during inspection: {name}")
                verify_tree(child_fd, device)
            finally:
                os.close(child_fd)
        elif not (stat.S_ISREG(entry.st_mode) or stat.S_ISLNK(entry.st_mode)):
            raise RuntimeError(f"unsupported entry type: {name}")


def clear_tree(fd: int, device: int) -> None:
    metadata = os.fstat(fd)
    if metadata.st_dev != device:
        raise RuntimeError("directory crossed a filesystem boundary before cleanup")
    os.fchmod(fd, stat.S_IMODE(metadata.st_mode) | stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    for name in list_directory(fd):
        current = os.stat(name, dir_fd=fd, follow_symlinks=False)
        if current.st_dev != device:
            raise RuntimeError(f"entry crossed a filesystem boundary before cleanup: {name}")
        if stat.S_ISDIR(current.st_mode):
            child_fd = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                dir_fd=fd,
            )
            try:
                held = os.fstat(child_fd)
                if (held.st_dev, held.st_ino) != (current.st_dev, current.st_ino):
                    raise RuntimeError(f"directory changed during cleanup: {name}")
                clear_tree(child_fd, device)
                current = os.stat(name, dir_fd=fd, follow_symlinks=False)
                if (held.st_dev, held.st_ino) != (current.st_dev, current.st_ino):
                    raise RuntimeError(f"directory changed during cleanup: {name}")
                os.rmdir(name, dir_fd=fd)
            finally:
                os.close(child_fd)
        else:
            os.unlink(name, dir_fd=fd)


def rename_no_replace(source: str, destination: str) -> None:
    if platform.machine() not in {"x86_64", "amd64"}:
        raise RuntimeError("x86_64 renameat2 is required for cached tool retirement")
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.syscall(
        316,
        trees_fd,
        os.fsencode(source),
        trees_fd,
        os.fsencode(destination),
        1,
    ) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def retire(name: str, *, already_renamed: bool = False) -> bool:
    fd = -1
    try:
        fd, held = open_held_directory(name)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(
                f"operational-tool: cached tool root {name} is in use; preserving it",
                file=sys.stderr,
            )
            return False
        verify_tree(fd, held.st_dev)
        retiring_name = name
        if not already_renamed:
            retiring_name = f".retiring.{name}.{secrets.token_hex(8)}"
            rename_no_replace(name, retiring_name)
            moved = os.stat(retiring_name, dir_fd=trees_fd, follow_symlinks=False)
            if (moved.st_dev, moved.st_ino) != (held.st_dev, held.st_ino):
                raise RuntimeError("directory changed during retirement rename")
        clear_tree(fd, held.st_dev)
        current = os.stat(retiring_name, dir_fd=trees_fd, follow_symlinks=False)
        if (current.st_dev, current.st_ino) != (held.st_dev, held.st_ino):
            raise RuntimeError("directory changed before final retirement")
        os.rmdir(retiring_name, dir_fd=trees_fd)
        print(f"operational-tool: retired cached tool root {name}", file=sys.stderr)
        return True
    finally:
        if fd >= 0:
            os.close(fd)


entries: list[tuple[int, str]] = []
for name in sorted(list_directory(trees_fd)):
    if retiring_pattern.fullmatch(name):
        try:
            retire(name, already_renamed=True)
        except (OSError, RuntimeError) as error:
            print(
                f"operational-tool: could not finish retiring cached tool root {name}: {error}",
                file=sys.stderr,
            )
            raise SystemExit(1) from error
        continue
    if sha_pattern.fullmatch(name) is None:
        continue
    try:
        metadata = os.stat(name, dir_fd=trees_fd, follow_symlinks=False)
    except OSError as error:
        print(
            f"operational-tool: cannot inspect cached tool root {name}: {error}",
            file=sys.stderr,
        )
        raise SystemExit(1) from error
    if not stat.S_ISDIR(metadata.st_mode):
        print(
            f"operational-tool: cached tool root {name} is not a real directory; preserving it",
            file=sys.stderr,
        )
        continue
    entries.append((metadata.st_mtime_ns, name))

if current_sha not in {name for _mtime, name in entries}:
    print(
        f"operational-tool: current cached tool root {current_sha} disappeared before retirement",
        file=sys.stderr,
    )
    raise SystemExit(1)

keep = {current_sha}
for _mtime, name in sorted(entries, reverse=True):
    if name in keep:
        continue
    keep.add(name)
    if len(keep) >= keep_count:
        break

for _mtime, name in sorted(entries):
    if name in keep:
        continue
    try:
        retire(name)
    except (OSError, RuntimeError) as error:
        print(
            f"operational-tool: could not retire cached tool root {name}: {error}",
            file=sys.stderr,
        )
        raise SystemExit(1) from error
PY
}

held_directory_entry_matches() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$1" "$2" "$3" <<'PY'
import os
import stat
import sys


parent_fd = int(sys.argv[1])
name = sys.argv[2]
held_path = sys.argv[3]
try:
    held = os.stat(held_path)
    current = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
except OSError:
    raise SystemExit(1)
if (
    not stat.S_ISDIR(held.st_mode)
    or not stat.S_ISDIR(current.st_mode)
    or held.st_nlink < 1
    or current.st_nlink < 1
    or (held.st_dev, held.st_ino) != (current.st_dev, current.st_ino)
):
    raise SystemExit(1)
PY
}

mark_held_tree_read_only() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$1" "${2:-}" <<'PY'
import os
import stat
import sys


root_path = sys.argv[1]
expected_identity = sys.argv[2]


def identity(metadata: os.stat_result) -> tuple[int, int]:
    return metadata.st_dev, metadata.st_ino


def list_directory(fd: int) -> list[str]:
    scan_fd = os.open(
        ".",
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        dir_fd=fd,
    )
    try:
        return sorted(os.listdir(scan_fd))
    finally:
        os.close(scan_fd)


def freeze_directory(fd: int, display: str) -> None:
    directory = os.fstat(fd)
    if not stat.S_ISDIR(directory.st_mode) or directory.st_nlink < 1:
        raise RuntimeError(f"held tool directory changed at {display}")

    # Remove write permission from the directory before inspecting children so
    # another process using its pathname cannot replace an entry between the
    # identity check and the descriptor-based chmod below.
    os.fchmod(fd, 0o555)
    for name in list_directory(fd):
        before = os.stat(name, dir_fd=fd, follow_symlinks=False)
        child_display = f"{display}/{name}"
        if stat.S_ISLNK(before.st_mode):
            # chmod must never follow a tracked or internal symlink.
            continue
        flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
        if stat.S_ISDIR(before.st_mode):
            child_fd = os.open(name, flags | os.O_DIRECTORY, dir_fd=fd)
            try:
                held = os.fstat(child_fd)
                if not stat.S_ISDIR(held.st_mode) or identity(held) != identity(before):
                    raise RuntimeError(
                        f"tool directory changed before read-only marking: {child_display}"
                    )
                freeze_directory(child_fd, child_display)
                after = os.stat(name, dir_fd=fd, follow_symlinks=False)
                if identity(after) != identity(held):
                    raise RuntimeError(
                        f"tool directory changed during read-only marking: {child_display}"
                    )
            finally:
                os.close(child_fd)
        elif stat.S_ISREG(before.st_mode):
            child_fd = os.open(name, flags, dir_fd=fd)
            try:
                held = os.fstat(child_fd)
                if not stat.S_ISREG(held.st_mode) or identity(held) != identity(before):
                    raise RuntimeError(
                        f"tool file changed before read-only marking: {child_display}"
                    )
                # Local clones may hard-link already-read-only Git object files.
                # Never chmod a writable multiply-linked inode: doing so would
                # mutate content outside this retained tree through the other link.
                if held.st_nlink != 1 and held.st_mode & 0o222:
                    raise RuntimeError(
                        f"writable tool file has link count {held.st_nlink}: {child_display}"
                    )
                if held.st_mode & 0o222:
                    os.fchmod(child_fd, stat.S_IMODE(held.st_mode) & ~0o222)
                after = os.stat(name, dir_fd=fd, follow_symlinks=False)
                held_after = os.fstat(child_fd)
                if identity(after) != identity(held_after):
                    raise RuntimeError(
                        f"tool file changed during read-only marking: {child_display}"
                    )
            finally:
                os.close(child_fd)
        else:
            raise RuntimeError(f"unsupported tool entry type at {child_display}")


try:
    # root_path is a process-owned /proc/<pid>/fd/<n> capability produced by a
    # holder we just started. Following that one constructed procfs link obtains
    # a descriptor for the retained inode; traversal below is entirely dirfd-
    # relative and never follows a directory-entry symlink.
    root_fd = os.open(root_path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        root = os.fstat(root_fd)
        observed_identity = f"{root.st_dev}:{root.st_ino}"
        if (
            not stat.S_ISDIR(root.st_mode)
            or root.st_nlink < 1
            or (expected_identity and observed_identity != expected_identity)
        ):
            raise RuntimeError("retained tool root changed before read-only marking")
        freeze_directory(root_fd, root_path)
        final = os.fstat(root_fd)
        if identity(final) != identity(root):
            raise RuntimeError("retained tool root changed during read-only marking")
    finally:
        os.close(root_fd)
except (OSError, RuntimeError) as error:
    print(f"operational-tool: could not mark retained tool root read-only: {error}", file=sys.stderr)
    raise SystemExit(1) from error
PY
}

ensure_directory_without_symlinks() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$1" <<'PY'
import errno
import os
from pathlib import Path
import sys


path = Path(sys.argv[1])
if not path.is_absolute():
    print(f"operational-tool: cache path is not absolute: {path}", file=sys.stderr)
    raise SystemExit(1)

fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
try:
    for component in path.parts[1:]:
        try:
            os.mkdir(component, mode=0o700, dir_fd=fd)
        except FileExistsError:
            pass
        try:
            next_fd = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=fd,
            )
        except OSError as error:
            if error.errno in (errno.ELOOP, errno.ENOTDIR):
                print(
                    f"operational-tool: cache path component is not a real directory: {path}",
                    file=sys.stderr,
                )
                raise SystemExit(1) from error
            raise
        os.close(fd)
        fd = next_fd
finally:
    os.close(fd)
PY
}

open_cache_layout() {
    ensure_directory_without_symlinks "$TOOL_CACHE" || return 1
    ensure_directory_without_symlinks "$TOOL_CACHE/trees" || return 1

    exec {TOOL_CACHE_FD}<"$TOOL_CACHE" || return 1
    if [[ -L $TOOL_CACHE ]] || \
       [[ $(readlink -f "/proc/self/fd/$TOOL_CACHE_FD") != "$TOOL_CACHE" ]]; then
        printf 'operational-tool: cache root changed during no-follow open\n' >&2
        return 1
    fi
    exec {TOOL_TREES_FD}<"/proc/self/fd/$TOOL_CACHE_FD/trees" || return 1
    if [[ -L /proc/self/fd/$TOOL_CACHE_FD/trees ]] || \
       [[ $(readlink -f "/proc/self/fd/$TOOL_TREES_FD") != "$TOOL_CACHE/trees" ]]; then
        printf 'operational-tool: cache trees directory changed during no-follow open\n' >&2
        return 1
    fi

    ensure_materialize_lock || return 1
    local lock_path="/proc/self/fd/$TOOL_CACHE_FD/materialize.lock"
    exec 8<"$lock_path" || return 1
    if ! verify_materialize_lock_identity opened; then
        exec 8<&-
        return 1
    fi
}

ensure_materialize_lock() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$TOOL_CACHE_FD" <<'PY'
import errno
import os
import stat
import sys


cache_fd = int(sys.argv[1])
flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
try:
    lock_fd = os.open("materialize.lock", flags | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=cache_fd)
except FileExistsError:
    try:
        lock_fd = os.open("materialize.lock", flags, dir_fd=cache_fd)
    except OSError as error:
        if error.errno == errno.ELOOP:
            print("operational-tool: materialize lock must not be a symlink", file=sys.stderr)
        else:
            print(f"operational-tool: materialize lock cannot be opened safely: {error}", file=sys.stderr)
        raise SystemExit(1) from error
except OSError as error:
    print(f"operational-tool: could not safely create materialize lock: {error}", file=sys.stderr)
    raise SystemExit(1) from error

try:
    metadata = os.fstat(lock_fd)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        print("operational-tool: materialize lock must be one regular file", file=sys.stderr)
        raise SystemExit(1)
finally:
    os.close(lock_fd)
PY
}

verify_materialize_lock_identity() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$TOOL_CACHE_FD" 8 "$TOOL_CACHE" "$1" <<'PY'
import os
import stat
import sys


cache_fd = int(sys.argv[1])
locked_fd = int(sys.argv[2])
expected_path = os.path.join(sys.argv[3], "materialize.lock")
phase = sys.argv[4]
flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
try:
    current_fd = os.open("materialize.lock", flags, dir_fd=cache_fd)
except OSError as error:
    print(
        f"operational-tool: materialize lock changed after it was {phase}: {error}",
        file=sys.stderr,
    )
    raise SystemExit(1) from error

try:
    locked = os.fstat(locked_fd)
    current = os.fstat(current_fd)
    path_metadata = os.stat(
        "materialize.lock", dir_fd=cache_fd, follow_symlinks=False
    )
    locked_path = os.readlink(f"/proc/self/fd/{locked_fd}")
    current_path = os.readlink(f"/proc/self/fd/{current_fd}")
    identity = (locked.st_dev, locked.st_ino)
    if (
        not stat.S_ISREG(locked.st_mode)
        or not stat.S_ISREG(current.st_mode)
        or not stat.S_ISREG(path_metadata.st_mode)
        or locked.st_nlink != 1
        or current.st_nlink != 1
        or path_metadata.st_nlink != 1
        or identity != (current.st_dev, current.st_ino)
        or identity != (path_metadata.st_dev, path_metadata.st_ino)
        or locked_path != expected_path
        or current_path != expected_path
    ):
        print(
            f"operational-tool: materialize lock changed after it was {phase}",
            file=sys.stderr,
        )
        raise SystemExit(1)
finally:
    os.close(current_fd)
PY
}

start_staging_root_holder() {
    local holder_pid staging_fd staging_name staging_identity
    coproc TOOL_STAGING_HOLDER {
        trap '' TERM INT HUP
        exec env PYTHONDONTWRITEBYTECODE=1 python3 /dev/fd/3 \
            "$TOOL_TREES_FD" "$TOOL_CACHE/trees" 3<<'PY'
import fcntl
import os
import re
import secrets
import signal
import stat
import sys


trees_fd = int(sys.argv[1])
trees_path = sys.argv[2]

# A systemd user unit uses KillMode=control-group by default.  The wrapper and
# its payload must receive termination, but these descriptor holders are the
# cleanup authority and must live long enough to answer the wrapper's EXIT
# handshake after the payload has stopped.  Pipe EOF still ends a holder if the
# wrapper itself is killed without running its trap.
for termination_signal in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(termination_signal, signal.SIG_IGN)


def list_directory(fd: int) -> list[str]:
    scan_fd = os.open(
        ".",
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        dir_fd=fd,
    )
    try:
        return os.listdir(scan_fd)
    finally:
        os.close(scan_fd)


def directory_is_reusable(fd: int, name: str) -> bool:
    try:
        held = os.fstat(fd)
        current = os.stat(name, dir_fd=trees_fd, follow_symlinks=False)
        return (
            stat.S_ISDIR(held.st_mode)
            and stat.S_ISDIR(current.st_mode)
            and held.st_nlink >= 1
            and current.st_nlink >= 1
            and (held.st_dev, held.st_ino) == (current.st_dev, current.st_ino)
            and os.readlink(f"/proc/self/fd/{fd}") == os.path.join(trees_path, name)
        )
    except OSError:
        return False


def acquire_staging_directory() -> tuple[int, str]:
    for name in sorted(list_directory(trees_fd)):
        if re.fullmatch(r"\.staging\.[0-9a-f]{16}", name) is None:
            continue
        try:
            fd = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=trees_fd,
            )
        except OSError:
            continue
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd)
            continue
        if directory_is_reusable(fd, name):
            try:
                clear_directory(fd)
                if list_directory(fd):
                    raise RuntimeError("retained staging root was not emptied for reuse")
            except (OSError, RuntimeError):
                os.close(fd)
                raise
            return fd, name
        os.close(fd)

    for _attempt in range(100):
        name = f".staging.{secrets.token_hex(8)}"
        try:
            os.mkdir(name, mode=0o700, dir_fd=trees_fd)
        except FileExistsError:
            continue
        created = os.stat(name, dir_fd=trees_fd, follow_symlinks=False)
        fd = os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=trees_fd,
        )
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        held = os.fstat(fd)
        if (
            not stat.S_ISDIR(created.st_mode)
            or (created.st_dev, created.st_ino) != (held.st_dev, held.st_ino)
            or not directory_is_reusable(fd, name)
        ):
            os.close(fd)
            raise RuntimeError("new staging root changed before descriptor retention")
        return fd, name
    raise RuntimeError("could not allocate a staging root")


def clear_directory(fd: int) -> None:
    metadata = os.fstat(fd)
    os.fchmod(fd, metadata.st_mode | stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    for name in list_directory(fd):
        current = os.stat(name, dir_fd=fd, follow_symlinks=False)
        if stat.S_ISDIR(current.st_mode):
            child_fd = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=fd,
            )
            try:
                child = os.fstat(child_fd)
                if (child.st_dev, child.st_ino) != (current.st_dev, current.st_ino):
                    raise RuntimeError(f"staging directory changed during cleanup: {name}")
                clear_directory(child_fd)
                current = os.stat(name, dir_fd=fd, follow_symlinks=False)
                if (child.st_dev, child.st_ino) != (current.st_dev, current.st_ino):
                    raise RuntimeError(f"staging directory changed during cleanup: {name}")
                os.rmdir(name, dir_fd=fd)
            finally:
                os.close(child_fd)
        else:
            os.unlink(name, dir_fd=fd)


try:
    staging_fd, staging_name = acquire_staging_directory()
except (OSError, RuntimeError) as error:
    print(f"operational-tool: {error}", file=sys.stderr)
    raise SystemExit(1) from error

staging = os.fstat(staging_fd)
print(
    os.getpid(),
    staging_fd,
    staging_name,
    f"{staging.st_dev}:{staging.st_ino}",
    flush=True,
)
try:
    command = sys.stdin.buffer.readline()
    if command == b"cleanup\n":
        clear_directory(staging_fd)
        if list_directory(staging_fd):
            raise RuntimeError("retained staging root was not emptied")
        print("cleaned", flush=True)
        if sys.stdin.buffer.readline() != b"release\n":
            raise RuntimeError("staging root holder received no release command")
    elif command not in {b"", b"release\n"}:
        raise RuntimeError("staging root holder received an invalid command")
except (OSError, RuntimeError) as error:
    print(f"operational-tool: {error}", file=sys.stderr)
    raise SystemExit(1) from error
finally:
    os.close(staging_fd)
PY
    }
    TOOL_STAGING_HOLDER_WAIT_PID=$TOOL_STAGING_HOLDER_PID
    TOOL_STAGING_HOLDER_OUTPUT_FD=${TOOL_STAGING_HOLDER[0]}
    TOOL_STAGING_HOLDER_CONTROL_FD=${TOOL_STAGING_HOLDER[1]}
    if ! read -r holder_pid staging_fd staging_name staging_identity \
        <&"$TOOL_STAGING_HOLDER_OUTPUT_FD"; then
        exec {TOOL_STAGING_HOLDER_CONTROL_FD}>&-
        wait "$TOOL_STAGING_HOLDER_WAIT_PID" || true
        exec {TOOL_STAGING_HOLDER_OUTPUT_FD}<&-
        TOOL_STAGING_HOLDER_WAIT_PID=""
        TOOL_STAGING_HOLDER_CONTROL_FD=""
        TOOL_STAGING_HOLDER_OUTPUT_FD=""
        return 1
    fi
    if [[ ! $holder_pid =~ ^[0-9]+$ || ! $staging_fd =~ ^[0-9]+$ || \
          ! $staging_name =~ ^\.staging\.[0-9a-f]{16}$ || \
          ! $staging_identity =~ ^[0-9]+:[0-9]+$ ]]; then
        printf 'operational-tool: staging holder returned malformed descriptor authority\n' >&2
        exec {TOOL_STAGING_HOLDER_CONTROL_FD}>&-
        wait "$TOOL_STAGING_HOLDER_WAIT_PID" || true
        exec {TOOL_STAGING_HOLDER_OUTPUT_FD}<&-
        TOOL_STAGING_HOLDER_WAIT_PID=""
        TOOL_STAGING_HOLDER_CONTROL_FD=""
        TOOL_STAGING_HOLDER_OUTPUT_FD=""
        return 1
    fi
    TOOL_STAGING_HOLDER_PID=$holder_pid
    TOOL_STAGING_ROOT="/proc/$holder_pid/fd/$staging_fd"
    TOOL_STAGING_ENTRY_NAME=$staging_name
    TOOL_STAGING_IDENTITY=$staging_identity
    TOOL_STAGING_MAY_BE_PUBLISHED=0
}

start_tool_root_holder() {
    local target_name=$1 target_identity=$2 expected_state_identity=${3:-}
    local holder_pid target_fd run_fd run_name run_identity state_fd state_identity
    coproc TOOL_ROOT_HOLDER {
        trap '' TERM INT HUP
        exec env PYTHONDONTWRITEBYTECODE=1 python3 /dev/fd/3 \
            "$TOOL_TREES_FD" "$TOOL_CACHE_FD" \
            "$target_name" "$target_identity" \
            "$TOOL_CACHE/trees/$target_name" "$TOOL_CACHE" "$ROOT" \
            "$expected_state_identity" 3<<'PY'
import fcntl
import hashlib
import json
import os
import re
import secrets
import signal
import stat
import sys


trees_fd = int(sys.argv[1])
cache_fd = int(sys.argv[2])
target_name = sys.argv[3]
target_identity = sys.argv[4]
target_path = sys.argv[5]
cache_path = sys.argv[6]
state_path = sys.argv[7]
expected_state_identity = sys.argv[8]

# The service manager terminates the complete unit cgroup.  Keep the two
# retained target, private-root, and state descriptors alive across that broadcast so the wrapper,
# after its foreground payload is terminal, can still clear the exact private
# inode through the control pipe.  This holder remains bounded by that pipe and
# by the unit cgroup; SIGKILL is intentionally not intercepted.
for termination_signal in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(termination_signal, signal.SIG_IGN)


def open_held_directory(
    parent_fd: int,
    name: str,
    expected_identity: str,
    expected_path: str,
    label: str,
) -> int:
    if not name or "/" in name or name in {".", ".."}:
        raise RuntimeError(f"unsafe {label} directory name {name!r}")
    fd = os.open(
        name,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        dir_fd=parent_fd,
    )
    held = os.fstat(fd)
    current = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    identity = f"{held.st_dev}:{held.st_ino}"
    if (
        not stat.S_ISDIR(held.st_mode)
        or not stat.S_ISDIR(current.st_mode)
        or held.st_nlink < 1
        or current.st_nlink < 1
        or (held.st_dev, held.st_ino) != (current.st_dev, current.st_ino)
        or (expected_identity and identity != expected_identity)
        or os.readlink(f"/proc/self/fd/{fd}") != expected_path
    ):
        os.close(fd)
        raise RuntimeError(f"{label} directory changed before its descriptor was retained")
    return fd


def list_directory(fd: int) -> list[str]:
    scan_fd = os.open(
        ".",
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        dir_fd=fd,
    )
    try:
        return os.listdir(scan_fd)
    finally:
        os.close(scan_fd)


def run_directory_is_reusable(fd: int, name: str) -> bool:
    try:
        held = os.fstat(fd)
        current = os.stat(name, dir_fd=cache_fd, follow_symlinks=False)
        return (
            stat.S_ISDIR(held.st_mode)
            and stat.S_ISDIR(current.st_mode)
            and held.st_nlink >= 1
            and current.st_nlink >= 1
            and (held.st_dev, held.st_ino) == (current.st_dev, current.st_ino)
            and os.readlink(f"/proc/self/fd/{fd}") == os.path.join(cache_path, name)
            and not list_directory(fd)
        )
    except OSError:
        return False


def acquire_run_directory() -> tuple[int, str]:
    for name in sorted(list_directory(cache_fd)):
        if re.fullmatch(r"\.run\.[0-9a-f]{16}", name) is None:
            continue
        try:
            fd = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=cache_fd,
            )
        except OSError:
            continue
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd)
            continue
        if run_directory_is_reusable(fd, name):
            os.fchmod(fd, 0o700)
            return fd, name
        os.close(fd)

    for _attempt in range(100):
        name = f".run.{secrets.token_hex(8)}"
        try:
            os.mkdir(name, mode=0o700, dir_fd=cache_fd)
        except FileExistsError:
            continue
        fd = os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=cache_fd,
        )
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if not run_directory_is_reusable(fd, name):
            os.close(fd)
            raise RuntimeError("new private tool root changed before use")
        return fd, name
    raise RuntimeError("could not allocate a private tool root")


def clear_directory(fd: int) -> None:
    metadata = os.fstat(fd)
    os.fchmod(fd, metadata.st_mode | stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    for name in list_directory(fd):
        current = os.stat(name, dir_fd=fd, follow_symlinks=False)
        if stat.S_ISDIR(current.st_mode):
            child_fd = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=fd,
            )
            try:
                child = os.fstat(child_fd)
                if (child.st_dev, child.st_ino) != (current.st_dev, current.st_ino):
                    raise RuntimeError(f"private tool directory changed during cleanup: {name}")
                clear_directory(child_fd)
                current = os.stat(name, dir_fd=fd, follow_symlinks=False)
                if (child.st_dev, child.st_ino) != (current.st_dev, current.st_ino):
                    raise RuntimeError(f"private tool directory changed during cleanup: {name}")
                os.rmdir(name, dir_fd=fd)
            finally:
                os.close(child_fd)
        else:
            os.unlink(name, dir_fd=fd)


def update_length(digest: "hashlib._Hash", value: int) -> None:
    digest.update(value.to_bytes(8, "big"))


def digest_entry(path: bytes, relative: bytes, digest: "hashlib._Hash") -> None:
    metadata = os.lstat(path)
    mode = stat.S_IMODE(metadata.st_mode)
    if stat.S_ISDIR(metadata.st_mode):
        if mode & 0o222 or metadata.st_nlink < 1:
            raise RuntimeError(
                f"immutable tool directory is writable or unlinked: {os.fsdecode(path)}"
            )
        held_fd = os.open(
            path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        )
        try:
            held = os.fstat(held_fd)
            if not stat.S_ISDIR(held.st_mode) or (held.st_dev, held.st_ino) != (
                metadata.st_dev,
                metadata.st_ino,
            ):
                raise RuntimeError(
                    f"immutable tool directory changed before hashing: {os.fsdecode(path)}"
                )
            digest.update(b"d")
            update_length(digest, len(relative))
            digest.update(relative)
            digest.update(mode.to_bytes(4, "big"))
            for name in sorted(os.listdir(path), key=os.fsencode):
                name_bytes = os.fsencode(name)
                child_relative = relative + (b"/" if relative else b"") + name_bytes
                if child_relative in {
                    b".git",
                    b"hermit/.git",
                    b"hermit/agent-utils/.git",
                }:
                    continue
                digest_entry(os.path.join(path, name_bytes), child_relative, digest)
            after = os.lstat(path)
            if (after.st_dev, after.st_ino) != (held.st_dev, held.st_ino):
                raise RuntimeError(
                    f"immutable tool directory changed while hashing: {os.fsdecode(path)}"
                )
        finally:
            os.close(held_fd)
    elif stat.S_ISREG(metadata.st_mode):
        if mode & 0o222 or metadata.st_nlink != 1:
            raise RuntimeError(
                f"immutable tool file is writable or multiply linked: {os.fsdecode(path)}"
            )
        held_fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            held = os.fstat(held_fd)
            if not stat.S_ISREG(held.st_mode) or (
                held.st_dev,
                held.st_ino,
                held.st_size,
            ) != (metadata.st_dev, metadata.st_ino, metadata.st_size):
                raise RuntimeError(
                    f"immutable tool file changed before hashing: {os.fsdecode(path)}"
                )
            digest.update(b"f")
            update_length(digest, len(relative))
            digest.update(relative)
            digest.update(mode.to_bytes(4, "big"))
            digest.update(metadata.st_size.to_bytes(8, "big"))
            observed = 0
            while True:
                chunk = os.read(held_fd, 64 * 1024)
                if not chunk:
                    break
                observed += len(chunk)
                digest.update(chunk)
            after = os.fstat(held_fd)
            if observed != metadata.st_size or (
                after.st_dev,
                after.st_ino,
                after.st_size,
            ) != (held.st_dev, held.st_ino, held.st_size):
                raise RuntimeError(
                    f"immutable tool file changed while hashing: {os.fsdecode(path)}"
                )
        finally:
            os.close(held_fd)
    elif stat.S_ISLNK(metadata.st_mode):
        target = os.fsencode(os.readlink(path))
        digest.update(b"l")
        update_length(digest, len(relative))
        digest.update(relative)
        digest.update(mode.to_bytes(4, "big"))
        update_length(digest, len(target))
        digest.update(target)
    else:
        raise RuntimeError(
            f"immutable tool has unsupported entry type at {os.fsdecode(path)}"
        )


def content_sha256(root_fd: int) -> str:
    root = f"/proc/self/fd/{root_fd}".encode()
    metadata = os.stat(root)
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) & 0o222:
        raise RuntimeError("immutable tool root must be one read-only directory")
    digest = hashlib.sha256(b"dev-hermit-tool-content-v1\0")
    for name in sorted(os.listdir(root), key=os.fsencode):
        name_bytes = os.fsencode(name)
        if name_bytes == b".git":
            continue
        digest_entry(os.path.join(root, name_bytes), name_bytes, digest)
    return digest.hexdigest()


def open_state_root() -> int:
    fd = os.open(
        state_path,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
    )
    metadata = os.fstat(fd)
    current = os.stat(state_path, follow_symlinks=False)
    identity = f"{metadata.st_dev}:{metadata.st_ino}"
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or not stat.S_ISDIR(current.st_mode)
        or metadata.st_nlink < 1
        or current.st_nlink < 1
        or (metadata.st_dev, metadata.st_ino) != (current.st_dev, current.st_ino)
        or os.readlink(f"/proc/self/fd/{fd}") != state_path
        or (expected_state_identity and identity != expected_state_identity)
    ):
        os.close(fd)
        raise RuntimeError(
            "state root changed before its descriptor was retained or did not match "
            "the prior immutable authority"
        )
    return fd


def create_authority(
    state_fd: int,
    parent_sha: str,
    hermit_sha: str,
    agent_utils_sha: str,
    bootstrap_sha256: str,
) -> tuple[int, str]:
    for label, value, pattern in (
        ("parent SHA", parent_sha, r"[0-9a-f]{40}"),
        ("Hermit SHA", hermit_sha, r"[0-9a-f]{40}"),
        ("agent-utils SHA", agent_utils_sha, r"[0-9a-f]{40}"),
        ("bootstrap digest", bootstrap_sha256, r"[0-9a-f]{64}"),
    ):
        if re.fullmatch(pattern, value) is None:
            raise RuntimeError(f"invalid {label} for immutable tool authority")

    target = os.fstat(target_fd)
    current_target = os.stat(target_name, dir_fd=trees_fd, follow_symlinks=False)
    root = os.fstat(run_fd)
    current_root = os.stat(run_name, dir_fd=cache_fd, follow_symlinks=False)
    state = os.fstat(state_fd)
    current_state = os.stat(state_path, follow_symlinks=False)
    if (
        not stat.S_ISDIR(target.st_mode)
        or not stat.S_ISDIR(current_target.st_mode)
        or stat.S_IMODE(target.st_mode) & 0o222
        or target.st_nlink < 1
        or current_target.st_nlink < 1
        or (target.st_dev, target.st_ino)
        != (current_target.st_dev, current_target.st_ino)
        or os.readlink(f"/proc/self/fd/{target_fd}") != target_path
    ):
        raise RuntimeError("cached target changed before authority was sealed")
    if (
        not stat.S_ISDIR(root.st_mode)
        or not stat.S_ISDIR(current_root.st_mode)
        or root.st_nlink < 1
        or current_root.st_nlink < 1
        or (root.st_dev, root.st_ino)
        != (current_root.st_dev, current_root.st_ino)
        or os.readlink(f"/proc/self/fd/{run_fd}")
        != os.path.join(cache_path, run_name)
    ):
        raise RuntimeError("private tool root changed before authority was sealed")
    if (
        not stat.S_ISDIR(state.st_mode)
        or not stat.S_ISDIR(current_state.st_mode)
        or state.st_nlink < 1
        or current_state.st_nlink < 1
        or (state.st_dev, state.st_ino)
        != (current_state.st_dev, current_state.st_ino)
        or os.readlink(f"/proc/self/fd/{state_fd}") != state_path
    ):
        raise RuntimeError("state root changed before authority was sealed")
    if len(
        {
            (target.st_dev, target.st_ino),
            (root.st_dev, root.st_ino),
            (state.st_dev, state.st_ino),
        }
    ) != 3:
        raise RuntimeError(
            "cached target, immutable tool, and mutable state roots must have distinct identities"
        )

    digest = content_sha256(run_fd)
    authority_fd = os.memfd_create(
        "dev-hermit-tool-authority",
        os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING,
    )
    record = {
        "schema": "dev-hermit-tool-authority-v1",
        "holder_pid": os.getpid(),
        "authority_fd": authority_fd,
        "target_fd": target_fd,
        "target_root": target_path,
        "target_dev": target.st_dev,
        "target_ino": target.st_ino,
        "root_fd": run_fd,
        "root_dev": root.st_dev,
        "root_ino": root.st_ino,
        "state_fd": state_fd,
        "state_root": state_path,
        "state_dev": state.st_dev,
        "state_ino": state.st_ino,
        "content_sha256": digest,
        "parent_sha": parent_sha,
        "hermit_sha": hermit_sha,
        "agent_utils_sha": agent_utils_sha,
        "bootstrap_sha256": bootstrap_sha256,
    }
    encoded = json.dumps(record, sort_keys=True, separators=(",", ":")).encode() + b"\n"
    try:
        written = 0
        while written < len(encoded):
            count = os.write(authority_fd, encoded[written:])
            if count <= 0:
                raise RuntimeError("short write while sealing immutable tool authority")
            written += count
        os.fchmod(authority_fd, 0o400)
        fcntl.fcntl(
            authority_fd,
            fcntl.F_ADD_SEALS,
            fcntl.F_SEAL_SEAL
            | fcntl.F_SEAL_SHRINK
            | fcntl.F_SEAL_GROW
            | fcntl.F_SEAL_WRITE,
        )
    except BaseException:
        os.close(authority_fd)
        raise
    return authority_fd, digest


target_fd = -1
run_fd = -1
state_fd = -1
authority_fd = -1
try:
    target_fd = open_held_directory(
        trees_fd, target_name, target_identity, target_path, "cached tool root"
    )
    fcntl.flock(target_fd, fcntl.LOCK_SH)
    run_fd, run_name = acquire_run_directory()
    state_fd = open_state_root()
    run = os.fstat(run_fd)
    state = os.fstat(state_fd)
    print(
        os.getpid(),
        target_fd,
        run_fd,
        run_name,
        f"{run.st_dev}:{run.st_ino}",
        state_fd,
        f"{state.st_dev}:{state.st_ino}",
        flush=True,
    )

    command = sys.stdin.buffer.readline()
    if command.startswith(b"authorize "):
        try:
            fields = command.decode("ascii").strip().split()
        except UnicodeDecodeError as error:
            raise RuntimeError("private tool root holder received non-ASCII authority") from error
        if len(fields) != 5 or fields[0] != "authorize":
            raise RuntimeError("private tool root holder received malformed authority")
        authority_fd, digest = create_authority(state_fd, *fields[1:])
        print("authorized", authority_fd, digest, flush=True)
        command = sys.stdin.buffer.readline()

    if command not in {b"", b"cleanup\n"}:
        raise RuntimeError("private tool root holder received no cleanup command")
    clear_directory(run_fd)
    if list_directory(run_fd):
        raise RuntimeError("private tool root was not emptied")
    if command == b"cleanup\n":
        print("cleaned", flush=True)
        if sys.stdin.buffer.readline() != b"release\n":
            raise RuntimeError("private tool root holder received no release command")
except (OSError, RuntimeError) as error:
    print(f"operational-tool: {error}", file=sys.stderr)
    raise SystemExit(1) from error
finally:
    if authority_fd >= 0:
        os.close(authority_fd)
    if state_fd >= 0:
        os.close(state_fd)
    if run_fd >= 0:
        os.close(run_fd)
    if target_fd >= 0:
        os.close(target_fd)
PY
    }
    TOOL_HOLDER_WAIT_PID=$TOOL_ROOT_HOLDER_PID
    TOOL_HOLDER_OUTPUT_FD=${TOOL_ROOT_HOLDER[0]}
    TOOL_HOLDER_CONTROL_FD=${TOOL_ROOT_HOLDER[1]}
    if ! read -r holder_pid target_fd run_fd run_name run_identity state_fd state_identity \
        <&"$TOOL_HOLDER_OUTPUT_FD"; then
        exec {TOOL_HOLDER_CONTROL_FD}>&-
        wait "$TOOL_HOLDER_WAIT_PID" || true
        exec {TOOL_HOLDER_OUTPUT_FD}<&-
        TOOL_HOLDER_WAIT_PID=""
        TOOL_HOLDER_CONTROL_FD=""
        TOOL_HOLDER_OUTPUT_FD=""
        return 1
    fi
    if [[ ! $holder_pid =~ ^[0-9]+$ || ! $target_fd =~ ^[0-9]+$ || \
          ! $run_fd =~ ^[0-9]+$ || ! $run_name =~ ^\.run\.[0-9a-f]{16}$ || \
          ! $run_identity =~ ^[0-9]+:[0-9]+$ || ! $state_fd =~ ^[0-9]+$ || \
          ! $state_identity =~ ^[0-9]+:[0-9]+$ ]]; then
        printf 'operational-tool: directory holder returned malformed descriptor authority\n' >&2
        exec {TOOL_HOLDER_CONTROL_FD}>&-
        wait "$TOOL_HOLDER_WAIT_PID" || true
        exec {TOOL_HOLDER_OUTPUT_FD}<&-
        TOOL_HOLDER_WAIT_PID=""
        TOOL_HOLDER_CONTROL_FD=""
        TOOL_HOLDER_OUTPUT_FD=""
        return 1
    fi
    TOOL_HOLDER_PID=$holder_pid
    TOOL_TARGET_ROOT="/proc/$holder_pid/fd/$target_fd"
    TOOL_RUN_ROOT="/proc/$holder_pid/fd/$run_fd"
    TOOL_RUN_ENTRY_NAME=$run_name
}

authorize_tool_root() {
    local parent_sha=$1 hermit_sha=$2 agent_utils_sha=$3 bootstrap_sha256=$4
    local status authority_fd digest
    [[ -n $TOOL_HOLDER_CONTROL_FD && -n $TOOL_HOLDER_OUTPUT_FD ]] || {
        printf 'operational-tool: no retained holder is available for authority creation\n' >&2
        return 1
    }
    if ! printf 'authorize %s %s %s %s\n' \
        "$parent_sha" "$hermit_sha" "$agent_utils_sha" "$bootstrap_sha256" \
        >&"$TOOL_HOLDER_CONTROL_FD"; then
        printf 'operational-tool: could not request immutable tool authority\n' >&2
        return 1
    fi
    if ! read -r status authority_fd digest <&"$TOOL_HOLDER_OUTPUT_FD" || \
       [[ $status != authorized || ! $authority_fd =~ ^[0-9]+$ || \
          ! $digest =~ ^[0-9a-f]{64}$ ]]; then
        printf 'operational-tool: holder returned malformed immutable tool authority\n' >&2
        return 1
    fi
    TOOL_AUTHORITY_PATH="/proc/$TOOL_HOLDER_PID/fd/$authority_fd"
    TOOL_CONTENT_SHA256=$digest
    TOOL_PARENT_SHA=$parent_sha
    TOOL_HERMIT_SHA=$hermit_sha
    TOOL_AGENT_UTILS_SHA=$agent_utils_sha
    TOOL_BOOTSTRAP_SHA256=$bootstrap_sha256
}

publish_staging_no_replace() {
    PYTHONDONTWRITEBYTECODE=1 python3 - \
        "$TOOL_TREES_FD" "$TOOL_CACHE/trees" "$1" \
        "$2" "$3" "$4" <<'PY'
import ctypes
import errno
import os
import platform
import stat
import sys


trees_fd = int(sys.argv[1])
trees_path = sys.argv[2]
staging_path = sys.argv[3]
staging_name = sys.argv[4]
target_name = sys.argv[5]
expected_identity = sys.argv[6]
if any(not name or "/" in name or name in {".", ".."} for name in (staging_name, target_name)):
    print("operational-tool: unsafe cache publication name", file=sys.stderr)
    raise SystemExit(1)

published = False
source_fd = -1
try:
    # staging_path is the holder-owned /proc/<pid>/fd/<n> capability. Open that
    # exact retained inode locally, then compare the visible trees/ entry before
    # using its name in the same-parent RENAME_NOREPLACE operation.
    source_fd = os.open(
        staging_path,
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
    source = os.fstat(source_fd)
    current = os.stat(staging_name, dir_fd=trees_fd, follow_symlinks=False)
    if (
        not stat.S_ISDIR(source.st_mode)
        or not stat.S_ISDIR(current.st_mode)
        or source.st_nlink < 1
        or current.st_nlink < 1
        or f"{source.st_dev}:{source.st_ino}" != expected_identity
        or (source.st_dev, source.st_ino) != (current.st_dev, current.st_ino)
        or os.readlink(staging_path) != os.path.join(trees_path, staging_name)
    ):
        raise RuntimeError("staging directory changed before publication")

    if platform.machine() not in {"x86_64", "amd64"}:
        raise RuntimeError(
            "x86_64 renameat2 is required for no-replace cache publication"
        )
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.syscall(
        316,  # SYS_renameat2 on x86_64, the only supported Hermit host architecture.
        trees_fd,
        os.fsencode(staging_name),
        trees_fd,
        os.fsencode(target_name),
        1,  # RENAME_NOREPLACE
    ) != 0:
        error = ctypes.get_errno()
        if error == errno.EEXIST:
            raise SystemExit(17)
        raise RuntimeError(
            f"no-replace cache publication failed: {os.strerror(error)}"
        )
    published = True

    try:
        os.stat(staging_name, dir_fd=trees_fd, follow_symlinks=False)
    except FileNotFoundError:
        pass
    else:
        raise RuntimeError("successful cache publication left staging unconsumed")
    target_fd = os.open(
        target_name,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        dir_fd=trees_fd,
    )
    try:
        target = os.fstat(target_fd)
        if (
            (source.st_dev, source.st_ino) != (target.st_dev, target.st_ino)
            or os.readlink(staging_path) != os.path.join(trees_path, target_name)
        ):
            raise RuntimeError("published cache target does not match staging inode")
    finally:
        os.close(target_fd)
except (OSError, RuntimeError) as error:
    print(f"operational-tool: retained staging publication failed: {error}", file=sys.stderr)
    # Once renameat2 succeeds, the retained inode is the published cache root.
    # A caller must never treat it as disposable staging again, even when a
    # later identity/readback check fails.
    raise SystemExit(18 if published else 1) from error
finally:
    if source_fd >= 0:
        os.close(source_fd)
PY
}

run_git() {
    local network=0 name
    local -a git_command=(git --no-replace-objects)
    if [[ ${1:-} == --network ]]; then
        network=1
        shift
    fi
    if [[ ${1:-} != -C || $# -lt 3 ]]; then
        printf 'operational-tool: internal Git call lacks an explicit working directory\n' >&2
        return 2
    fi
    local -a clean_env=(env)
    # Git's environment variables can redirect the index, object database,
    # repository, worktree, hooks, or config. None is executable authority for
    # this bootstrap; the versioned transport allowlist below is the sole
    # exception and is supplied after every inherited GIT_* value is removed.
    while IFS= read -r name; do
        [[ $name == GIT_* ]] && clean_env+=(-u "$name")
    done < <(compgen -e)
    clean_env+=(
        GIT_ALLOW_PROTOCOL="$GIT_TRANSPORT_ALLOWLIST"
        GIT_CONFIG_NOSYSTEM=1
        GIT_CONFIG_GLOBAL=/dev/null
        GIT_CEILING_DIRECTORIES="$TOOL_CACHE"
        GIT_TERMINAL_PROMPT=0
    )
    if [[ $network -eq 1 ]]; then
        if [[ -L $GITHUB_CREDENTIAL_HELPER || \
              ! -f $GITHUB_CREDENTIAL_HELPER || \
              ! -x $GITHUB_CREDENTIAL_HELPER ]]; then
            printf 'operational-tool: required GitHub credential helper is unavailable: %s\n' \
                "$GITHUB_CREDENTIAL_HELPER" >&2
            return 1
        fi
        # Global and system Git configuration remain disabled: either may name
        # an arbitrary executable credential helper or rewrite transport URLs.
        # The service still needs credentials for the private canonical GitHub
        # repository, so install exactly one fixed helper for network calls.
        git_command+=(
            -c credential.https://github.com.helper=
            -c "credential.https://github.com.helper=!$GITHUB_CREDENTIAL_HELPER auth git-credential"
        )
        "${clean_env[@]}" with-proxy "${git_command[@]}" "$@"
    else
        "${clean_env[@]}" "${git_command[@]}" "$@"
    fi
}

normalize_github_remote() {
    local url=$1 path
    case "$url" in
        https://github.com/*) path=${url#https://github.com/} ;;
        git@github.com:*) path=${url#git@github.com:} ;;
        ssh://git@github.com/*) path=${url#ssh://git@github.com/} ;;
        *) return 1 ;;
    esac
    path=${path%.git}
    [[ $path =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || return 1
    printf '%s\n' "$path"
}

materialize_current_tool_root() {
    local requested_sha=${1:-}
    local expected_state_identity=${2:-}
    local expected_target_identity=${3:-}
    local configured_remote configured_repository remote_listing sha target hermit_pin agent_utils_pin
    local bootstrap_sha
    local cache_anchor trees_anchor staging_anchor
    local target_identity="" publish_rc
    open_cache_layout || return 1
    cache_anchor="/proc/self/fd/$TOOL_CACHE_FD"
    trees_anchor="/proc/self/fd/$TOOL_TREES_FD"
    flock -w "${HERMIT_HEALTH_TOOL_LOCK_TIMEOUT:-120}" 8 || {
        printf 'health-tick: timed out waiting for current-tool materialization lock\n' >&2
        return 1
    }
    # The pathname may have been renamed and replaced while flock was blocked.
    # Re-open it no-follow through the held cache dirfd and prove that the path,
    # locked descriptor, inode, device, and link count still name one file.
    verify_materialize_lock_identity locked || return 1
    if [[ -n $requested_sha ]]; then
        if [[ ! $requested_sha =~ ^[0-9a-f]{40}$ ]]; then
            printf 'operational-tool: --tool-parent-sha must be one full lowercase commit SHA\n' >&2
            return 2
        fi
        sha=$requested_sha
    else
        configured_remote=$(run_git -C "$ROOT" config --get remote.origin.url 2>/dev/null) || {
            printf 'health-tick: state root has no readable origin URL\n' >&2
            return 1
        }
        configured_repository=$(normalize_github_remote "$configured_remote" 2>/dev/null || true)
        if [[ $configured_remote != "$CANONICAL_REMOTE_URL" && \
              $configured_repository != "$CANONICAL_REPOSITORY" ]]; then
            printf 'health-tick: state root origin is %s, expected canonical %s; refusing mutable remote authority\n' \
                "$configured_remote" "$CANONICAL_REPOSITORY" >&2
            return 1
        fi
        remote_listing=$(run_git --network -C "$cache_anchor" ls-remote --heads \
            "$CANONICAL_REMOTE_URL" refs/heads/main) || {
            printf 'health-tick: cannot resolve current origin/main\n' >&2
            return 1
        }
        sha=$(printf '%s\n' "$remote_listing" | awk '$2 == "refs/heads/main" && $1 ~ /^[0-9a-f]{40}$/ { print $1 }')
        if [[ ! $sha =~ ^[0-9a-f]{40}$ ]] || [[ $(printf '%s\n' "$sha" | wc -l) -ne 1 ]]; then
            printf 'health-tick: origin/main did not resolve to exactly one commit\n' >&2
            return 1
        fi
    fi
    target="$trees_anchor/$sha"
    if [[ -L $target ]]; then
        printf 'health-tick: cached tool root %s is a symlink; refusing it\n' "$sha" >&2
        return 1
    elif [[ ! -e $target && -n $requested_sha ]]; then
        printf 'operational-tool: exact cached tool root %s is absent; refusing to fetch a different authority\n' \
            "$sha" >&2
        return 1
    elif [[ ! -e $target ]]; then
        start_staging_root_holder || return 1
        staging_anchor=$TOOL_STAGING_ROOT
        refresh_canonical_object_mirror "$cache_anchor"
        mirror_reference=()
        if [[ -n $TOOL_OBJECT_MIRROR ]]; then
            mirror_reference=(--reference-if-able "$TOOL_OBJECT_MIRROR" --dissociate)
        fi
        if ! run_git --network -C "$cache_anchor" clone --quiet --no-checkout \
            "${mirror_reference[@]}" "$CANONICAL_REMOTE_URL" "$staging_anchor"; then
            printf 'health-tick: could not clone current parent tooling\n' >&2
            return 1
        fi
        if ! run_git -C "$staging_anchor" cat-file -e "$sha^{commit}" 2>/dev/null; then
            run_git --network -C "$staging_anchor" fetch --quiet --no-tags \
                "$CANONICAL_REMOTE_URL" "$sha" || {
                printf 'health-tick: could not fetch exact origin/main commit %s\n' "$sha" >&2
                return 1
            }
        fi
        run_git -C "$staging_anchor" checkout --quiet --detach "$sha" || return 1
        hermit_pin=$(run_git -C "$staging_anchor" \
            ls-tree "$sha" -- hermit \
            | awk '$1 == "160000" && $2 == "commit" { print $3 }')
        if [[ ! $hermit_pin =~ ^[0-9a-f]{40}$ ]]; then
            printf 'health-tick: parent commit %s has no required mode-160000 Hermit gitlink\n' \
                "$sha" >&2
            return 1
        fi
        run_git --network -C "$staging_anchor" submodule update \
            --init --checkout --depth 1 -- hermit \
            || return 1
        agent_utils_pin=$(run_git -C "$staging_anchor/hermit" \
            ls-tree "$hermit_pin" -- agent-utils \
            | awk '$1 == "160000" && $2 == "commit" { print $3 }')
        if [[ ! $agent_utils_pin =~ ^[0-9a-f]{40}$ ]]; then
            printf 'health-tick: Hermit commit %s has no required mode-160000 agent-utils gitlink\n' \
                "$hermit_pin" >&2
            return 1
        fi
        run_git --network -C "$staging_anchor/hermit" submodule update \
            --init --checkout --depth 1 -- agent-utils \
            || return 1
        tool_root_is_complete "$staging_anchor" "$sha" || {
            printf 'health-tick: materialized tool root failed pin or cleanliness verification\n' >&2
            return 1
        }
        # The directory becomes read-only before its atomic publication, while
        # the retained staging descriptor still names the exact verified inode.
        # Recheck bytes afterward so publication cannot separate the checked
        # tree from the tree whose permissions were frozen.
        mark_held_tree_read_only "$staging_anchor" "$TOOL_STAGING_IDENTITY" || return 1
        tool_root_is_complete "$staging_anchor" "$sha" || {
            printf 'health-tick: read-only staging root failed final byte verification\n' >&2
            return 1
        }
        # Re-read the retained inode after byte verification. This is both an
        # integrity check and the publication boundary: the visible name is not
        # authority, and publish_staging_no_replace compares the atomically
        # moved target back to this already-verified descriptor.
        [[ $(stat -Lc '%d:%i' "$staging_anchor") == "$TOOL_STAGING_IDENTITY" ]] || {
            printf 'operational-tool: retained staging directory changed during verification\n' >&2
            return 1
        }
        # From this point a signal or process death may leave the inode
        # published. The holder must release rather than clear it until the
        # publication helper proves that RENAME_NOREPLACE did not happen.
        TOOL_STAGING_MAY_BE_PUBLISHED=1
        publish_staging_no_replace \
            "$staging_anchor" "$TOOL_STAGING_ENTRY_NAME" "$sha" "$TOOL_STAGING_IDENTITY"
        publish_rc=$?
        if [[ $publish_rc -eq 17 ]]; then
            TOOL_STAGING_MAY_BE_PUBLISHED=0
            printf 'health-tick: cache target collision left staging unconsumed; refusing replacement\n' >&2
            return 1
        elif [[ $publish_rc -eq 18 ]]; then
            return 1
        elif [[ $publish_rc -ne 0 ]]; then
            TOOL_STAGING_MAY_BE_PUBLISHED=0
            return "$publish_rc"
        fi
        target_identity=$TOOL_STAGING_IDENTITY
        stop_staging_holder release || return 1
    fi

    if [[ -n $expected_target_identity ]]; then
        local observed_target_identity
        observed_target_identity=$(stat -Lc '%d:%i' -- "$target") || {
            printf 'operational-tool: cannot inspect cached exact-parent target %s\n' "$target" >&2
            return 1
        }
        if [[ $observed_target_identity != "$expected_target_identity" ]]; then
            printf 'operational-tool: cached exact-parent target identity changed across detached reentry: observed %s, expected %s\n' \
                "$observed_target_identity" "$expected_target_identity" >&2
            return 1
        fi
        target_identity=$expected_target_identity
    fi

    # Retain all terminal directories by no-follow descriptors before any
    # chmod, copy, verification, authority read, or execution. The /proc paths
    # below name those exact inodes even if their directory entries are moved.
    start_tool_root_holder \
        "$sha" "$target_identity" "$expected_state_identity" || return 1
    # Existing roots may predate the atomic read-only publication protocol.
    # Verify their exact tracked bytes and link counts before changing any mode,
    # then apply the mark through the retained target descriptor rather than by
    # re-resolving the cache pathname.
    tool_root_is_complete "$TOOL_TARGET_ROOT" "$sha" || {
        printf 'health-tick: cached tool root %s is incomplete or dirty; refusing to repair it in place\n' \
            "$sha" >&2
        return 1
    }
    mark_held_tree_read_only "$TOOL_TARGET_ROOT" "$target_identity" || return 1
    tool_root_is_complete "$TOOL_TARGET_ROOT" "$sha" || {
        printf 'health-tick: read-only cached tool root %s failed final byte verification\n' \
            "$sha" >&2
        return 1
    }
    retire_old_tool_roots "$sha" || return 1
    cp -a --reflink=auto "$TOOL_TARGET_ROOT/." "$TOOL_RUN_ROOT/" || return 1
    mark_held_tree_read_only "$TOOL_RUN_ROOT" || return 1
    tool_root_is_complete "$TOOL_RUN_ROOT" "$sha" || {
        printf 'health-tick: private tool snapshot failed byte verification\n' >&2
        return 1
    }
    hermit_pin=$(run_git -C "$TOOL_TARGET_ROOT" \
        ls-tree "$sha" -- hermit \
        | awk '$1 == "160000" && $2 == "commit" { print $3 }')
    agent_utils_pin=$(run_git -C "$TOOL_TARGET_ROOT/hermit" \
        ls-tree "$hermit_pin" -- agent-utils \
        | awk '$1 == "160000" && $2 == "commit" { print $3 }')
    if [[ ! $hermit_pin =~ ^[0-9a-f]{40}$ || \
          ! $agent_utils_pin =~ ^[0-9a-f]{40}$ ]]; then
        printf 'operational-tool: verified tool snapshot has incomplete nested pins\n' >&2
        return 1
    fi
    bootstrap_sha=$(sha256sum -- "${BASH_SOURCE[0]}" | awk '{print $1}') || return 1
    authorize_tool_root \
        "$sha" "$hermit_pin" "$agent_utils_pin" "$bootstrap_sha" || return 1
    TOOL_ROOT_RESULT=$TOOL_RUN_ROOT
    flock -u 8 || return 1
    exec 8>&-
}

# A detached launcher must outlive this tick's private tool snapshot. Establish
# its stable entrypoint from the installation, never from an inherited hint.
verified_installed_operational_tool() {
    local installed_operational_tool="$HOME/.local/libexec/dev-hermit/operational-tool"
    if [[ ! -L $installed_operational_tool && -f $installed_operational_tool && \
          -x $installed_operational_tool ]] && \
       [[ $(stat -Lc '%a' "$installed_operational_tool") =~ ^[0-7]*[0145][0145][0145]$ ]] && \
       cmp --silent -- "${BASH_SOURCE[0]}" "$installed_operational_tool"; then
        printf '%s' "$installed_operational_tool"
    fi
}

run_current_tool() {
    local state_root="${DEV_HERMIT_PARENT:-$ROOT}"
    local cache_root="$TOOL_CACHE"
    local requested_parent_sha=""
    local expected_state_identity=""
    local expected_target_identity=""
    local tool_relative=""
    local component
    local -a tool_components=()
    local -a payload=()

    shift
    while [[ $# -gt 0 ]]; do
        case $1 in
            --state-root)
                [[ $# -ge 2 ]] || {
                    printf 'operational-tool: --state-root requires a path\n' >&2
                    return 2
                }
                state_root=$2
                shift 2
                ;;
            --cache-root)
                [[ $# -ge 2 ]] || {
                    printf 'operational-tool: --cache-root requires a path\n' >&2
                    return 2
                }
                cache_root=$2
                shift 2
                ;;
            --state-identity)
                [[ $# -ge 2 ]] || {
                    printf 'operational-tool: --state-identity requires DEV:INO\n' >&2
                    return 2
                }
                expected_state_identity=$2
                shift 2
                ;;
            --target-identity)
                [[ $# -ge 2 ]] || {
                    printf 'operational-tool: --target-identity requires DEV:INO\n' >&2
                    return 2
                }
                expected_target_identity=$2
                shift 2
                ;;
            --tool-relative)
                [[ $# -ge 2 ]] || {
                    printf 'operational-tool: --tool-relative requires a path\n' >&2
                    return 2
                }
                tool_relative=$2
                shift 2
                ;;
            --tool-parent-sha)
                [[ $# -ge 2 ]] || {
                    printf 'operational-tool: --tool-parent-sha requires a commit SHA\n' >&2
                    return 2
                }
                requested_parent_sha=$2
                shift 2
                ;;
            -h|--help)
                printf '%s\n' \
                    'usage: operational-tool --run-current-tool --state-root PATH [--state-identity DEV:INO] [--target-identity DEV:INO] [--cache-root PATH] [--tool-parent-sha SHA] --tool-relative PATH [-- ARGS...]' \
                    '' \
                    'Materialize current origin/main into a verified private tool root and run one' \
                    'repository-relative executable. Mutable operational state remains under PATH.'
                return 0
                ;;
            --)
                shift
                payload=("$@")
                break
                ;;
            *)
                printf 'operational-tool: unknown argument %s\n' "$1" >&2
                return 2
                ;;
        esac
    done

    [[ -n $tool_relative ]] || {
        printf 'operational-tool: --tool-relative is required\n' >&2
        return 2
    }
    if [[ -n $expected_state_identity && \
          ! $expected_state_identity =~ ^[0-9]+:[0-9]+$ ]]; then
        printf 'operational-tool: --state-identity must be one DEV:INO pair\n' >&2
        return 2
    fi
    if [[ -n $expected_target_identity && \
          ! $expected_target_identity =~ ^[0-9]+:[0-9]+$ ]]; then
        printf 'operational-tool: --target-identity must be one DEV:INO pair\n' >&2
        return 2
    fi
    [[ $tool_relative != /* ]] || {
        printf 'operational-tool: tool path must be relative to the immutable tool root\n' >&2
        return 2
    }
    IFS=/ read -r -a tool_components <<<"$tool_relative"
    for component in "${tool_components[@]}"; do
        [[ -n $component && $component != . && $component != .. ]] || {
            printf 'operational-tool: unsafe tool path %s\n' "$tool_relative" >&2
            return 2
        }
    done

    ROOT=$(realpath -e -- "$state_root") || {
        printf 'operational-tool: state root does not exist: %s\n' "$state_root" >&2
        return 2
    }
    if [[ -n $expected_state_identity ]]; then
        local observed_state_identity
        observed_state_identity=$(stat -Lc '%d:%i' -- "$ROOT") || {
            printf 'operational-tool: cannot inspect canonical state root %s\n' "$ROOT" >&2
            return 1
        }
        if [[ $observed_state_identity != "$expected_state_identity" ]]; then
            printf 'operational-tool: canonical state root identity changed across detached reentry: observed %s, expected %s\n' \
                "$observed_state_identity" "$expected_state_identity" >&2
            return 1
        fi
    fi
    TOOL_CACHE=$(PYTHONDONTWRITEBYTECODE=1 python3 - "$cache_root" <<'PY'
import os
import sys

print(os.path.abspath(sys.argv[1]))
PY
    ) || {
        printf 'operational-tool: cannot resolve cache root: %s\n' "$cache_root" >&2
        return 2
    }
    case "$TOOL_CACHE/" in
        "$ROOT/"|"$ROOT/"*)
            printf 'operational-tool: cache root must be outside mutable state root %s\n' "$ROOT" >&2
            return 2
            ;;
    esac

    materialize_current_tool_root \
        "$requested_parent_sha" "$expected_state_identity" \
        "$expected_target_identity" || return $?
    local tool_root=$TOOL_ROOT_RESULT
    local tool_path="$tool_root/$tool_relative"
    local resolved_tool_root resolved_tool_path
    resolved_tool_root=$(realpath -e -- "$tool_root") || {
        printf 'operational-tool: immutable tool root disappeared\n' >&2
        return 2
    }
    resolved_tool_path=$(realpath -e -- "$tool_path") || {
        printf 'operational-tool: immutable tool is absent: %s\n' "$tool_relative" >&2
        return 2
    }
    case "$resolved_tool_path" in
        "$resolved_tool_root/"*) ;;
        *)
            printf 'operational-tool: tool path escapes immutable tool root: %s\n' "$tool_relative" >&2
            return 2
            ;;
    esac
    [[ ! -L $tool_path && -f $tool_path && -x $tool_path ]] || {
        printf 'operational-tool: immutable tool is not executable: %s\n' "$tool_relative" >&2
        return 2
    }

    local parent_sha=$TOOL_PARENT_SHA
    local hermit_sha=$TOOL_HERMIT_SHA
    local agent_utils_sha=$TOOL_AGENT_UTILS_SHA
    local bootstrap_sha=$TOOL_BOOTSTRAP_SHA256
    if [[ ! $TOOL_AUTHORITY_PATH =~ ^/proc/[1-9][0-9]*/fd/[0-9]+$ || \
          ! $TOOL_CONTENT_SHA256 =~ ^[0-9a-f]{64}$ || \
          ! $parent_sha =~ ^[0-9a-f]{40}$ || ! $hermit_sha =~ ^[0-9a-f]{40}$ || \
          ! $agent_utils_sha =~ ^[0-9a-f]{40}$ || \
          ! $bootstrap_sha =~ ^[0-9a-f]{64}$ ]]; then
        printf 'operational-tool: immutable tool authority is incomplete\n' >&2
        return 1
    fi
    if [[ -n $requested_parent_sha && $parent_sha != "$requested_parent_sha" ]]; then
        printf 'operational-tool: exact cached tool root resolved to %s, expected %s\n' \
            "$parent_sha" "$requested_parent_sha" >&2
        return 1
    fi
    local operational_tool
    operational_tool=$(verified_installed_operational_tool)

    printf 'operational-tool: bootstrap_sha256=%s parent_sha=%s hermit_sha=%s agent_utils_sha=%s tool_root=%s state_root=%s entrypoint=%s\n' \
        "$bootstrap_sha" "$parent_sha" "$hermit_sha" "$agent_utils_sha" \
        "$tool_root" "$ROOT" "$tool_relative" >&2

    DEV_HERMIT_PARENT="$ROOT" \
    DEV_HERMIT_TOOL_ROOT="$tool_root" \
    DEV_HERMIT_TOOL_AUTHORITY="$TOOL_AUTHORITY_PATH" \
    DEV_HERMIT_TOOL_CONTENT_SHA256="$TOOL_CONTENT_SHA256" \
    DEV_HERMIT_TOOL_PARENT_SHA="$parent_sha" \
    DEV_HERMIT_TOOL_HERMIT_SHA="$hermit_sha" \
    DEV_HERMIT_TOOL_AGENT_UTILS_SHA="$agent_utils_sha" \
    DEV_HERMIT_TOOL_BOOTSTRAP_SHA256="$bootstrap_sha" \
    DEV_HERMIT_TOOL_CACHE="$TOOL_CACHE" \
    DEV_HERMIT_OPERATIONAL_TOOL="$operational_tool" \
        "$tool_path" "${payload[@]}"
}

# Parse the complete invocation before it updates this version-controlled file.
# The running tick therefore finishes with one coherent wrapper version even
# when the fetched commit changes scripts/hermit-health-tick.sh itself.
main() {
emit_output=0
if [[ ${1:-} == --emit-output ]]; then
    emit_output=1
    shift
fi

# The durable systemd timer and the ORC workflow are independent schedulers.
# Serialize the complete stateful run before truncating LIVE_REPORT: otherwise
# phase-aligned firings can erase each other's report and duplicate gates or
# escalations. A duplicate is expected redundancy, not a failed health check.
mkdir -p -- "$(dirname -- "$RUN_LOCK")" || {
    printf 'ERROR: health-tick could not create run-lock directory: %s\n' "$RUN_LOCK" >&2
    return 73
}
exec 7>"$RUN_LOCK" || {
    printf 'ERROR: health-tick could not open run lock: %s\n' "$RUN_LOCK" >&2
    return 73
}
if ! flock -n 7; then
    printf 'NOTE: operational health tick already running; concurrent invocation skipped\n'
    return 0
fi

start="$(ts)"
mt_before="$(stat -c %Y "$FIRED_STATE" 2>/dev/null || echo 0)"

# Start this run's report empty and stamped, so the exit trap can tell this
# tick's partial findings from the previous tick's complete ones.
LIVE_REPORT_STAMP="$start"
LIVE_REPORT_RECORDED=0
LIVE_REPORT_ESCALATED=0
TICK_REPORT_COMPLETE=0
: >"$LIVE_REPORT" 2>/dev/null || true

# --emit-output means the caller reads the report from stdout, so the report has
# to reach stdout as it is produced for the same reason it has to reach the file:
# the caller is the one holding the outer bound, and it kills this process.
#
# A DUPLICATED DESCRIPTOR, not the path /dev/stdout. `tee` opens its path
# arguments with O_TRUNC, so teeing to /dev/stdout would truncate whatever this
# script's stdout is redirected to -- a service log, a caller's capture file --
# the moment the first line is written. Duplicating descriptor 1 writes to the
# same open file at its current offset and cannot truncate anything.
if [[ $emit_output -eq 1 ]]; then
    exec 9>&1
else
    exec 9>/dev/null
fi


if ! cd "$ROOT" 2>/dev/null; then
    printf '%s rc=126 event=cd-failed root=%s\n' "$start" "$ROOT" >>"$INVLOG"
    exit 126
fi

# A cancelled earlier run may have retained a payload because its detached
# publisher could not be started or did not complete. Relaunching is safe: the
# escalation key and digest make repeated delivery idempotent, and the payload
# disappears only after both sinks succeed.
retry_pending_health_escalations

# Pull landed parent changes BEFORE loading the tick runner or its configuration.
# This is the deployment half of a parent-main landing: the writer serializes the
# ref update, freshly fetches origin/main, and permits only a fast-forward whose
# incoming paths do not overlap dirty paths. Overlap or divergent main is preserved
# byte-for-byte and reported with the blocking paths and both observed SHAs. The
# tick still runs so this refusal cannot turn the rest of operational health into
# silence.
parent_before="$(run_git -C "$ROOT" rev-parse HEAD 2>/dev/null || printf unreadable)"
parent_sync_out="$("$PARENT_MAIN_WRITE" sync --preserve-dirty 2>&1)"
parent_sync_rc=$?
parent_after="$(run_git -C "$ROOT" rev-parse HEAD 2>/dev/null || printf unreadable)"

# Keep the complete writer report, including every blocking path, in the durable
# invocation history. One compact ACTION line carries the same evidence into the
# coordinator path when the fast-forward was skipped or unavailable.
while IFS= read -r parent_sync_line || [ -n "$parent_sync_line" ]; do
    printf '%s parent_sync_detail=%q\n' "$start" "$parent_sync_line" >>"$INVLOG"
done <<<"$parent_sync_out"
printf '%s parent_sync_rc=%s before=%s after=%s\n' \
    "$start" "$parent_sync_rc" "$parent_before" "$parent_after" >>"$INVLOG"

parent_sync_finding=""
if [ "$parent_sync_rc" -ne 0 ]; then
    # The exact output remains above. Keep the one-line ACTION parseable even if
    # a quoted Git path contains characters meaningful to the finding grammar.
    parent_sync_summary="$(printf '%s' "$parent_sync_out" | tr '\n' ';' | sed 's/["\\]/ /g; s/  */ /g; s/;$//')"
    parent_sync_finding="ACTION: hard-warn-coordinator component=parent-checkout-sync state=blocked before=$parent_before after=$parent_after summary=\"$parent_sync_summary\" title=\"parent checkout fast-forward skipped: before=$parent_before after=$parent_after; $parent_sync_summary\""
    # Into the report file, not only into a variable: this finding is known
    # before the gates start, so a tick killed during the gates must not lose
    # the one thing it already knew.
    printf '%s\n' "$parent_sync_finding" | tee -a "$LIVE_REPORT" >&9
fi

# The shared parent is mutable operational state, never executable authority.
# Materialize current origin/main plus the two nested tool pins outside it, then
# keep that exact checkout fixed for the complete tick.
materialize_current_tool_root
tool_root_rc=$?
if [[ $tool_root_rc -ne 0 ]]; then
    printf '%s\n' "ERROR: health-tick could not materialize current immutable tooling" \
        | tee -a "$LIVE_REPORT" >&9
    TICK_REPORT_COMPLETE=1
    out="$(cat "$LIVE_REPORT" 2>/dev/null)"
    rc=$tool_root_rc
else
    tool_root=$TOOL_ROOT_RESULT
    tool_root_sha=$TOOL_PARENT_SHA
    local operational_tool
    operational_tool=$(verified_installed_operational_tool)
    printf '%s tool_root=%s tool_root_sha=%s state_root=%s\n' \
        "$start" "$tool_root" "$tool_root_sha" "$ROOT" >>"$INVLOG"
    # Keep the measured tick in its own process group so either outer bound can
    # stop every gate descendant without also killing this wrapper before its
    # exit trap records and escalates the report already received. A dedicated
    # read descriptor preserves tick-hub's line-by-line delivery; unlike the old
    # command substitution, it does not hold findings until the final gate exits.
    TICK_OUTPUT_DIR=$(mktemp -d "${TMPDIR:-/tmp}/hermit-health-tick.$$.XXXXXX") || exit 1
    TICK_OUTPUT_PIPE="$TICK_OUTPUT_DIR/report"
    TICK_OUTPUT_READY="$TICK_OUTPUT_DIR/writer-ready"
    mkfifo -- "$TICK_OUTPUT_PIPE" || exit 1
    # The temporary read/write descriptor lets both one-way FIFO opens finish
    # without either side blocking before the child exists. It is closed as
    # soon as the real writer starts, so EOF still means the measured tick and
    # all descendants have closed their output.
    exec {TICK_OUTPUT_HOLD_FD}<>"$TICK_OUTPUT_PIPE"
    exec {TICK_OUTPUT_FD}<"$TICK_OUTPUT_PIPE"
    TICK_CHILD_SPAWN_PENDING=1
    setsid sh -c ': > "$1"; shift; exec "$@"' sh "$TICK_OUTPUT_READY" \
        env DEV_HERMIT_PARENT="$ROOT" \
            DEV_HERMIT_TOOL_ROOT="$tool_root" \
            DEV_HERMIT_TOOL_AUTHORITY="$TOOL_AUTHORITY_PATH" \
            DEV_HERMIT_TOOL_CONTENT_SHA256="$TOOL_CONTENT_SHA256" \
            DEV_HERMIT_TOOL_PARENT_SHA="$TOOL_PARENT_SHA" \
            DEV_HERMIT_TOOL_HERMIT_SHA="$TOOL_HERMIT_SHA" \
            DEV_HERMIT_TOOL_AGENT_UTILS_SHA="$TOOL_AGENT_UTILS_SHA" \
            DEV_HERMIT_TOOL_BOOTSTRAP_SHA256="$TOOL_BOOTSTRAP_SHA256" \
            DEV_HERMIT_TOOL_CACHE="$TOOL_CACHE" \
            DEV_HERMIT_OPERATIONAL_TOOL="$operational_tool" \
            PYTHONDONTWRITEBYTECODE=1 \
            with-proxy "$tool_root/ci-hub/bin/health-tick" --flush --no-header \
            >"$TICK_OUTPUT_PIPE" 2>&1 &
    TICK_CHILD_PID=$!
    TICK_CHILD_SPAWN_PENDING=0
    if [[ -n $TICK_CANCEL_SIGNAL ]]; then
        finish_tick_cancellation
    fi
    # The child creates this marker only after the shell has opened its stdout
    # FIFO. Until then the temporary writer must remain open: closing it first
    # could let the reader observe a false EOF and remove the FIFO before the
    # child reaches its redirection.
    while [[ ! -e $TICK_OUTPUT_READY && -d /proc/$TICK_CHILD_PID ]]; do
        sleep 0.005
    done
    exec {TICK_OUTPUT_HOLD_FD}>&-
    TICK_OUTPUT_HOLD_FD=""
    while IFS= read -r tick_line <&"$TICK_OUTPUT_FD" || [[ -n $tick_line ]]; do
        printf '%s\n' "$tick_line" >>"$LIVE_REPORT"
        printf '%s\n' "$tick_line" >&9
        tick_line=""
    done
    close_tick_output
    rc=0
    wait "$TICK_CHILD_PID" || rc=$?
    TICK_CHILD_PID=""
    if [[ -n $TICK_CANCEL_SIGNAL ]]; then
        exit "$TICK_CANCEL_STATUS"
    fi
    # The runner returned, so whatever it was going to say is now in the file.
    # This is deliberately not conditional on rc: a tick that ran its gates and
    # failed has still REPORTED, and health-tick emits an explicit gate=ALL line
    # when it dies before reporting, which arrives here as a finding.
    TICK_REPORT_COMPLETE=1
    # NOT a command substitution. The report reached the file and the caller
    # line by line as each gate decided, so the copy below is a convenience for
    # the code that follows rather than the only place the findings exist.
    out="$(cat "$LIVE_REPORT" 2>/dev/null)"
fi

mt_after="$(stat -c %Y "$FIRED_STATE" 2>/dev/null || echo 0)"
advanced=no
[ "$mt_after" -gt "$mt_before" ] && advanced=yes

# THE FINDINGS THEMSELVES, IN FULL, UNTRUNCATED.
#
# WHY THIS EXISTS. The `summary=` record below is a 240-character window onto
# the whole tick, and the tick's own boilerplate is wider than the window: the
# tool-cost banner is 159 characters and the "fired-state persisted to <abs
# path>" line is another 85, so 244 characters are spent before the first
# character of any gate's output. Measured 2026-08-24: across 5415 recorded
# invocations, the number containing the string "SLOT CAP BREACH" was ZERO, and
# the number containing the word "worktree" was ZERO -- while the slot gate was
# firing "ACTIVE SLOT CAP BREACH: 77 active slot(s) against cap 12" every 900
# seconds. Nothing else retained the text either: `out` captures stdout AND
# stderr, so the service journal saw none of it. The detector worked; its
# findings were destroyed before any human or agent could read them.
#
# ACTION:, NOTE: and ERROR: are the line kinds tick-hub emits as findings. Each
# is recorded verbatim on its own line, tagged with the same start timestamp so a
# reader can join it to the invocation record. A tick that finds nothing writes
# nothing here, so this stays a log of findings rather than a transcript.
#
# THE SAME RECORDING RUNS FROM THE EXIT TRAP. On the ordinary path this call
# does the work and marks it done; on an interrupted tick the trap does it
# instead, so the findings a killed tick had already made are recorded rather
# than lost with the process.
#
# NOT `%q`. The summary field is shell-quoted, which renders "SLOT CAP BREACH"
# as "SLOT\ CAP\ BREACH" and defeats a plain grep for the phrase -- recorded but
# still unfindable, which is the same defect one layer down. Findings are single
# lines by construction (they are produced by `read -r` over the tick's output),
# so line integrity does not need quoting and greppability is worth more.
#
# FINDINGS ARE WRITTEN BEFORE THE INVOCATION RECORD, DELIBERATELY.
# scripts/hermit-health-staleness.sh reads this file with `tail -1` and expects
# an invocation record there; a findings line last would make its known
# agent-utils-pin-drift branch stop matching and downgrade a correctly
# classified state into "WORK-STALLED (unexpected)". That reader is also
# hardened to select the last invocation record explicitly, but the ordering
# here keeps the file correct for any reader that has not been.
record_live_report_findings
escalate_live_report_findings

# Append-only invocation record so the next investigation has a HISTORY, not a
# single mtime. (fired-state itself is gitignored and keeps no per-run history.)
summary="$(printf '%s' "$out" | tr '\n' ' ' | sed 's/  */ /g' | cut -c1-240)"
printf '%s rc=%s advanced=%s fired_state_mtime=%s summary=%q\n' \
    "$start" "$rc" "$advanced" "$mt_after" "$summary" >>"$INVLOG"

exit "$rc"
}

# The findings first, then the tool roots: a trap that cleans up before it
# records would lose the report of a tick that was interrupted, which is the
# only case this trap exists for.
trap finish_wrapper EXIT
trap 'record_tick_cancellation INT 130' INT
trap 'record_tick_cancellation TERM 143' TERM
trap 'record_tick_cancellation HUP 129' HUP
if [[ ${1:-} == --run-current-tool ]]; then
    run_current_tool "$@"
else
    main "$@"
fi
