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
# This script is invoked by a systemd --user timer (Linger=yes) so it survives
# logout, reboot, and session recycling.
set -uo pipefail

# $HOME, not a literal: this file is version-controlled and the parent's
# portability gate rejects owner-specific paths. A shell DOES expand $HOME
# (unlike a systemd unit, which needs %h).
ROOT="${HOME:?HOME must be set}/work/dev-hermit"
FIRED_STATE="$ROOT/.tick-hub/fired-state"
INVLOG="$HOME/.local/state/hermit-health-tick-invocations.log"
TOOL_CACHE="${HERMIT_HEALTH_TOOL_CACHE:-$HOME/.local/state/hermit-health-tool}"

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

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
    try:
        result = subprocess.run(
            ["git", "--no-replace-objects", "-C", str(repo), *args],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
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
    root = Path(sys.argv[1]).resolve(strict=True)
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

TOOL_CACHE_STAGING=""
TOOL_RUN_ROOT=""
TOOL_ROOT_RESULT=""

cleanup_tool_roots() {
    local rc=$? path
    trap - EXIT INT TERM
    for path in "$TOOL_CACHE_STAGING" "$TOOL_RUN_ROOT"; do
        [[ -n $path ]] || continue
        case $path in
            "$TOOL_CACHE"/.staging.*|"$TOOL_CACHE"/.run.*)
                if ! chmod -R u+w -- "$path" 2>/dev/null; then
                    printf 'health-tick: could not make temporary tool root removable: %s\n' "$path" >&2
                    [[ $rc -ne 0 ]] || rc=1
                    continue
                fi
                if ! rm -rf -- "$path"; then
                    printf 'health-tick: could not remove temporary tool root: %s\n' "$path" >&2
                    [[ $rc -ne 0 ]] || rc=1
                fi
                ;;
            *)
                printf 'health-tick: refusing unsafe temporary cleanup path %s\n' "$path" >&2
                [[ $rc -ne 0 ]] || rc=1
                ;;
        esac
    done
    exit "$rc"
}

materialize_current_tool_root() {
    local remote_url remote_listing sha target hermit_pin agent_utils_pin
    mkdir -p "$TOOL_CACHE/trees" || return 1
    exec 8>"$TOOL_CACHE/materialize.lock" || return 1
    flock -w "${HERMIT_HEALTH_TOOL_LOCK_TIMEOUT:-120}" 8 || {
        printf 'health-tick: timed out waiting for current-tool materialization lock\n' >&2
        return 1
    }
    remote_url=$(git -C "$ROOT" remote get-url origin 2>/dev/null) || {
        printf 'health-tick: state root has no readable origin URL\n' >&2
        return 1
    }
    remote_listing=$(with-proxy git ls-remote --heads "$remote_url" refs/heads/main) || {
        printf 'health-tick: cannot resolve current origin/main\n' >&2
        return 1
    }
    sha=$(printf '%s\n' "$remote_listing" | awk '$2 == "refs/heads/main" && $1 ~ /^[0-9a-f]{40}$/ { print $1 }')
    if [[ ! $sha =~ ^[0-9a-f]{40}$ ]] || [[ $(printf '%s\n' "$sha" | wc -l) -ne 1 ]]; then
        printf 'health-tick: origin/main did not resolve to exactly one commit\n' >&2
        return 1
    fi
    target="$TOOL_CACHE/trees/$sha"
    if [[ -e $target ]]; then
        tool_root_is_complete "$target" "$sha" || {
            printf 'health-tick: cached tool root %s is incomplete or dirty; refusing to repair it in place\n' "$target" >&2
            return 1
        }
    else
        TOOL_CACHE_STAGING=$(mktemp -d "$TOOL_CACHE/.staging.XXXXXX") || return 1
        if ! with-proxy git clone --quiet --no-checkout "$remote_url" "$TOOL_CACHE_STAGING"; then
            printf 'health-tick: could not clone current parent tooling\n' >&2
            return 1
        fi
        if ! git -C "$TOOL_CACHE_STAGING" cat-file -e "$sha^{commit}" 2>/dev/null; then
            with-proxy git -C "$TOOL_CACHE_STAGING" fetch --quiet --no-tags origin "$sha" || {
                printf 'health-tick: could not fetch exact origin/main commit %s\n' "$sha" >&2
                return 1
            }
        fi
        git -C "$TOOL_CACHE_STAGING" checkout --quiet --detach "$sha" || return 1
        hermit_pin=$(git --no-replace-objects -C "$TOOL_CACHE_STAGING" \
            ls-tree "$sha" -- hermit \
            | awk '$1 == "160000" && $2 == "commit" { print $3 }')
        if [[ ! $hermit_pin =~ ^[0-9a-f]{40}$ ]]; then
            printf 'health-tick: parent commit %s has no required mode-160000 Hermit gitlink\n' \
                "$sha" >&2
            return 1
        fi
        with-proxy git -C "$TOOL_CACHE_STAGING" submodule update --init --checkout --depth 1 -- hermit \
            || return 1
        agent_utils_pin=$(git --no-replace-objects -C "$TOOL_CACHE_STAGING/hermit" \
            ls-tree "$hermit_pin" -- agent-utils \
            | awk '$1 == "160000" && $2 == "commit" { print $3 }')
        if [[ ! $agent_utils_pin =~ ^[0-9a-f]{40}$ ]]; then
            printf 'health-tick: Hermit commit %s has no required mode-160000 agent-utils gitlink\n' \
                "$hermit_pin" >&2
            return 1
        fi
        with-proxy git -C "$TOOL_CACHE_STAGING/hermit" submodule update --init --checkout --depth 1 -- agent-utils \
            || return 1
        tool_root_is_complete "$TOOL_CACHE_STAGING" "$sha" || {
            printf 'health-tick: materialized tool root failed pin or cleanliness verification\n' >&2
            return 1
        }
        mv -- "$TOOL_CACHE_STAGING" "$target" || return 1
        TOOL_CACHE_STAGING=""
        chmod -R a-w -- "$target" || return 1
    fi

    # Never execute the reusable cache tree itself. A private copy closes the
    # validation-to-exec race: mutation of the shared cache during or after the
    # copy either fails the second byte verification or cannot affect this run.
    TOOL_RUN_ROOT=$(mktemp -d "$TOOL_CACHE/.run.XXXXXX") || return 1
    cp -a --reflink=auto "$target/." "$TOOL_RUN_ROOT/" || return 1
    chmod -R a-w -- "$TOOL_RUN_ROOT" || return 1
    tool_root_is_complete "$TOOL_RUN_ROOT" "$sha" || {
        printf 'health-tick: private tool snapshot failed byte verification\n' >&2
        return 1
    }
    TOOL_ROOT_RESULT=$TOOL_RUN_ROOT
    flock -u 8 || return 1
    exec 8>&-
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
start="$(ts)"
mt_before="$(stat -c %Y "$FIRED_STATE" 2>/dev/null || echo 0)"

if ! cd "$ROOT" 2>/dev/null; then
    printf '%s rc=126 event=cd-failed root=%s\n' "$start" "$ROOT" >>"$INVLOG"
    exit 126
fi

# Pull landed parent changes BEFORE loading the tick runner or its configuration.
# This is the deployment half of a parent-main landing: the writer serializes the
# ref update, freshly fetches origin/main, and permits only a fast-forward whose
# incoming paths do not overlap dirty paths. Overlap or divergent main is preserved
# byte-for-byte and reported with the blocking paths and both observed SHAs. The
# tick still runs so this refusal cannot turn the rest of operational health into
# silence.
parent_before="$(git rev-parse HEAD 2>/dev/null || printf unreadable)"
parent_sync_out="$(./scripts/parent-main-write sync --preserve-dirty 2>&1)"
parent_sync_rc=$?
parent_after="$(git rev-parse HEAD 2>/dev/null || printf unreadable)"

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
fi

# The shared parent is mutable operational state, never executable authority.
# Materialize current origin/main plus the two nested tool pins outside it, then
# keep that exact checkout fixed for the complete tick.
materialize_current_tool_root
tool_root_rc=$?
if [[ $tool_root_rc -ne 0 ]]; then
    out="ERROR: health-tick could not materialize current immutable tooling"
    rc=$tool_root_rc
else
    tool_root=$TOOL_ROOT_RESULT
    tool_root_sha=$(git -C "$tool_root" rev-parse --verify 'HEAD^{commit}')
    printf '%s tool_root=%s tool_root_sha=%s state_root=%s\n' \
        "$start" "$tool_root" "$tool_root_sha" "$ROOT" >>"$INVLOG"
    out="$(DEV_HERMIT_PARENT="$ROOT" \
        DEV_HERMIT_TOOL_ROOT="$tool_root" \
        PYTHONDONTWRITEBYTECODE=1 \
        with-proxy "$tool_root/ci-hub/bin/health-tick" --flush --no-header 2>&1)"
    rc=$?
fi
if [ -n "$parent_sync_finding" ]; then
    out="$(printf '%s\n%s' "$parent_sync_finding" "$out")"
fi
if [[ $emit_output -eq 1 ]]; then
    printf '%s\n' "$out"
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
# ACTION: and NOTE: are the two line kinds tick-hub emits as findings (see
# agent-utils/rs/tick-hub/src/emit.rs). Each is recorded verbatim on its own
# line, tagged with the same start timestamp so a reader can join it to the
# invocation record. A tick that finds nothing writes nothing here, so this
# stays a log of findings rather than a transcript.
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
printf '%s' "$out" | while IFS= read -r finding || [ -n "$finding" ]; do
    case "$finding" in
        ACTION:*|NOTE:*) printf '%s finding=%s\n' "$start" "$finding" >>"$INVLOG" ;;
    esac
done

# Append-only invocation record so the next investigation has a HISTORY, not a
# single mtime. (fired-state itself is gitignored and keeps no per-run history.)
summary="$(printf '%s' "$out" | tr '\n' ' ' | sed 's/  */ /g' | cut -c1-240)"
printf '%s rc=%s advanced=%s fired_state_mtime=%s summary=%q\n' \
    "$start" "$rc" "$advanced" "$mt_after" "$summary" >>"$INVLOG"

exit "$rc"
}

trap cleanup_tool_roots EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
main "$@"
