#!/usr/bin/env bash
# Install the durable disk-snapshot producer independently of ORC.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/install-disk-snapshot.sh [--check] [--start|--no-start]

--check     Validate dependencies, state root, and installed bootstrap/helper bytes.
--start     Install, enable, and restart the timer.
--no-start  Install with the timer disabled and inactive (the default).
EOF
}

mode=install
start=0
for argument in "$@"; do
    case $argument in
        --check) mode=check ;;
        --start) start=1 ;;
        --no-start) start=0 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'install-disk-snapshot: unknown argument: %s\n' "$argument" >&2; exit 64 ;;
    esac
done
if [[ $mode == check && $start -eq 1 ]]; then
    printf 'install-disk-snapshot: --check and --start are mutually exclusive\n' >&2
    exit 64
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
source_root=$(cd -- "$script_dir/.." && pwd -P)
state_root=${DEV_HERMIT_PARENT:-"${HOME:?HOME must be set}/work/dev-hermit"}
state_root=$(realpath -e -- "$state_root") || {
    printf 'install-disk-snapshot: state root is unavailable: %s\n' "$state_root" >&2
    exit 2
}
unit_dir=${HERMIT_USER_UNIT_DIR:-"${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"}
systemctl_bin=${SYSTEMCTL_BIN:-systemctl}
libexec_dir=$HOME/.local/libexec/dev-hermit
bootstrap_source=$source_root/scripts/hermit-health-tick.sh
bootstrap=$libexec_dir/operational-tool
github_helper=$libexec_dir/gh
install_label=disk-snapshot
producer=$source_root/scripts/publish-disk-snapshots.py
service=hermit-disk-snapshot.service
timer=hermit-disk-snapshot.timer

unit_property() {
    "$systemctl_bin" --user show --no-pager \
        --property="$2" --value "$1"
}

# ⚠️ A SINGLE FIXED PATH UNDER $HOME WAS THE FAILURE, AND RERUNNING THIS COULD
# NOT REPAIR IT. The installers read $HOME/bin/gh. Measured 2026-09-05 on
# devbig014: that path is a symlink into /home/newton/opt, /home/newton/opt DOES
# NOT EXIST, so the link dangles, ~/.local/libexec/dev-hermit/gh is absent, and
# hermit-health-tick.sh refuses every networked git call -- the tick reported no
# signal for GitHub rather than a verdict. Recreating the link, or rerunning
# any installer, reproduces exactly that state.
#
# So resolve over an ordered list and take the first entry that opens as an
# executable regular file. /usr/bin/gh comes FIRST because the host package
# manager maintains it (rpm gh-2.97.0-2.el9.x86_64 on devbig014), whereas
# $HOME/bin/gh is maintained by nobody in particular. $HOME/bin/gh is kept LAST
# rather than dropped, so a host that deliberately supplies its own is still
# honoured -- and a dangling one is now skipped instead of fatal, which is the
# whole defect.
#
# ⚠️ DEV_HERMIT_GH_SOURCE REPLACES THE LIST, it does not extend it. An explicit
# choice that turns out to be broken must fail loudly rather than quietly
# installing something else; that also makes every branch here reachable from a
# test, including the refusal.
#
# This block is byte-identical in the other installers; the only per-installer
# text is $install_label. scripts/test_install_github_helper_source.py asserts
# that identity and exercises every file, because copies that drift are the
# shape this repository keeps finding.
if [[ -n ${DEV_HERMIT_GH_SOURCE:-} ]]; then
    github_helper_candidates=("$DEV_HERMIT_GH_SOURCE")
else
    github_helper_candidates=(/usr/bin/gh /usr/local/bin/gh "$HOME/bin/gh")
fi

github_helper_source=
github_helper_fd=
for github_helper_candidate in "${github_helper_candidates[@]}"; do
    if ! exec {github_helper_fd}<"$github_helper_candidate"; then
        continue
    fi
    # Everything downstream reads the DESCRIPTOR, never the path again. That is
    # what stops the source being swapped between this check and the copy below,
    # and it is also what dereferences a symlink so the installed helper is a
    # regular file rather than a link that can later be repointed.
    github_helper_source_fd=/proc/self/fd/$github_helper_fd
    if [[ -f $github_helper_source_fd && -x $github_helper_source_fd ]]; then
        github_helper_source=$github_helper_candidate
        break
    fi
    exec {github_helper_fd}<&-
    github_helper_fd=
done
if [[ -z $github_helper_source ]]; then
    printf '%s install: required GitHub credential helper is unavailable; no candidate opened as an executable regular file. Tried:\n' \
        "$install_label" >&2
    printf '  %s\n' "${github_helper_candidates[@]}" >&2
    printf '%s install: set DEV_HERMIT_GH_SOURCE to a real gh binary, or install one at /usr/bin/gh\n' \
        "$install_label" >&2
    exit 1
fi

[[ -x $bootstrap_source && ! -L $bootstrap_source ]] || {
    printf 'install-disk-snapshot: reviewed bootstrap source is unavailable: %s\n' "$bootstrap_source" >&2
    exit 2
}
[[ -x $producer && ! -L $producer ]] || {
    printf 'install-disk-snapshot: producer is unavailable: %s\n' "$producer" >&2
    exit 2
}
"$producer" --check --state-root "$state_root" --population-timeout 180
if [[ $mode == check ]]; then
    [[ -x $bootstrap && -f $bootstrap && ! -L $bootstrap ]] && \
        cmp --silent "$bootstrap_source" "$bootstrap" || {
        printf 'install-disk-snapshot: installed bootstrap is absent or differs from reviewed source: %s\n' "$bootstrap" >&2
        exit 2
    }
    [[ -x $github_helper && -f $github_helper && ! -L $github_helper ]] && \
        cmp --silent "$github_helper_source_fd" "$github_helper" || {
        printf 'install-disk-snapshot: installed GitHub credential helper is absent or differs from selected source: %s\n' \
            "$github_helper" >&2
        exit 2
    }
    exec {github_helper_fd}<&-
    printf 'install-disk-snapshot: installed bootstrap and GitHub credential helper match reviewed sources\n'
    exit 0
fi

# `--no-start` is the cold-takeover path. An already-active timer means this is
# not a cold install at all; replacing its launcher while it can fire would
# defeat the retirement proof this mode exists to establish. Refuse before any
# installed byte changes. A loaded but inactive old unit is disabled first so a
# user-manager restart cannot activate it during the copy below.
if ((!start)); then
    existing_timer_load=$(unit_property "$timer" LoadState)
    existing_timer_active=$(unit_property "$timer" ActiveState)
    existing_service_load=$(unit_property "$service" LoadState)
    existing_service_active=$(unit_property "$service" ActiveState)
    existing_units_supported=1
    case "$existing_timer_load:$existing_service_load" in
        not-found:not-found|not-found:loaded|loaded:not-found|loaded:loaded) ;;
        *) existing_units_supported=0 ;;
    esac
    if [[ $existing_units_supported -ne 1 || \
        $existing_timer_active != inactive || $existing_service_active != inactive ]]; then
        printf 'install-disk-snapshot: refusing --no-start while existing timer/service is %s/%s + %s/%s; stop and disable it explicitly, then retry\n' \
            "$existing_timer_load" "$existing_timer_active" \
            "$existing_service_load" "$existing_service_active" >&2
        exit 1
    fi
    if [[ $existing_timer_load == loaded ]]; then
        "$systemctl_bin" --user disable --now "$timer"
    fi
fi

install -d -m 0700 "$unit_dir"
install -d -m 0755 "$libexec_dir"
bootstrap_staging=
github_helper_staging=
timer_start_armed=0
timer_start_committed=0
cleanup_install() {
    local rc=$?
    [[ -z ${bootstrap_staging:-} ]] || rm -f -- "$bootstrap_staging" || true
    [[ -z ${github_helper_staging:-} ]] || rm -f -- "$github_helper_staging" || true
    if ((timer_start_armed && !timer_start_committed)); then
        # A failed `--start` must not become an enabled timer at the next login
        # or leave the oneshot writing after this command reports failure.
        if ! "$systemctl_bin" --user disable --now "$timer"; then
            printf 'install-disk-snapshot: rollback could not disable/stop %s\n' \
                "$timer" >&2
        fi
        if ! "$systemctl_bin" --user stop "$service"; then
            printf 'install-disk-snapshot: rollback could not stop %s\n' \
                "$service" >&2
        fi
    fi
    # Preserve the command or explicit refusal status. A successful cleanup is
    # not evidence that the operation which triggered it succeeded.
    exit "$rc"
}
trap cleanup_install EXIT
bootstrap_staging=$(mktemp "$libexec_dir/.operational-tool.XXXXXX")
github_helper_staging=$(mktemp "$libexec_dir/.gh.XXXXXX")
install -m 0555 "$bootstrap_source" "$bootstrap_staging"
cmp --silent "$bootstrap_source" "$bootstrap_staging" || {
    printf 'install-disk-snapshot: staged bootstrap differs from reviewed source\n' >&2
    exit 1
}
install -m 0555 "$github_helper_source_fd" "$github_helper_staging"
cmp --silent "$github_helper_source_fd" "$github_helper_staging" || {
    printf 'install-disk-snapshot: staged GitHub credential helper differs from selected source\n' >&2
    exit 1
}
mv -fT -- "$bootstrap_staging" "$bootstrap"
bootstrap_staging=
mv -fT -- "$github_helper_staging" "$github_helper"
github_helper_staging=
[[ -x $bootstrap && -f $bootstrap && ! -L $bootstrap ]] && \
    cmp --silent "$bootstrap_source" "$bootstrap" || {
    printf 'install-disk-snapshot: installed bootstrap differs from reviewed source\n' >&2
    exit 1
}
[[ -x $github_helper && -f $github_helper && ! -L $github_helper ]] || {
    printf 'install-disk-snapshot: installed GitHub credential helper is not a regular executable\n' >&2
    exit 1
}
cmp --silent "$github_helper_source_fd" "$github_helper" || {
    printf 'install-disk-snapshot: installed GitHub credential helper differs from selected source\n' >&2
    exit 1
}
exec {github_helper_fd}<&-
install -m 0644 "$source_root/scripts/systemd/$service" "$unit_dir/$service"
install -m 0644 "$source_root/scripts/systemd/$timer" "$unit_dir/$timer"
cmp --silent "$source_root/scripts/systemd/$service" "$unit_dir/$service" || {
    printf 'install-disk-snapshot: installed service differs from source\n' >&2
    exit 1
}
cmp --silent "$source_root/scripts/systemd/$timer" "$unit_dir/$timer" || {
    printf 'install-disk-snapshot: installed timer differs from source\n' >&2
    exit 1
}
grep -Fq \
    'ExecStart=%h/.local/libexec/dev-hermit/operational-tool --run-current-tool' \
    "$unit_dir/$service" || {
    printf 'install-disk-snapshot: service does not execute the installed bootstrap\n' >&2
    exit 1
}

"$systemctl_bin" --user daemon-reload
bootstrap_sha=$(sha256sum -- "$bootstrap" | awk '{print $1}')
github_helper_sha=$(sha256sum -- "$github_helper" | awk '{print $1}')
printf 'install-disk-snapshot: bootstrap_sha256=%s bootstrap=%s github_helper_source=%s github_helper_sha256=%s github_helper=%s\n' \
    "$bootstrap_sha" "$bootstrap" "$github_helper_source" "$github_helper_sha" "$github_helper"
if ((start)); then
    timer_start_armed=1
    "$systemctl_bin" --user enable "$timer"
    "$systemctl_bin" --user restart "$timer"
    service_load=$(unit_property "$service" LoadState)
    service_active=$(unit_property "$service" ActiveState)
    service_sub=$(unit_property "$service" SubState)
    timer_load=$(unit_property "$timer" LoadState)
    timer_active=$(unit_property "$timer" ActiveState)
    timer_sub=$(unit_property "$timer" SubState)
    [[ $service_load == loaded && $timer_load == loaded ]] || {
        printf 'install-disk-snapshot: units are not both loaded: service=%s timer=%s\n' \
            "$service_load" "$timer_load" >&2
        exit 1
    }
    case "$service_active:$service_sub:$timer_active:$timer_sub" in
        inactive:dead:active:waiting|activating:start:active:running) ;;
        *)
            printf 'install-disk-snapshot: incoherent service/timer state: %s/%s + %s/%s\n' \
                "$service_active" "$service_sub" "$timer_active" "$timer_sub" >&2
            exit 1
            ;;
    esac
    timer_start_committed=1
    printf 'install-disk-snapshot: service=%s/%s timer=%s/%s\n' \
        "$service_active" "$service_sub" "$timer_active" "$timer_sub"
else
    timer_start_armed=1
    "$systemctl_bin" --user disable --now "$timer"
    timer_load=$(unit_property "$timer" LoadState)
    timer_active=$(unit_property "$timer" ActiveState)
    timer_sub=$(unit_property "$timer" SubState)
    service_load=$(unit_property "$service" LoadState)
    service_active=$(unit_property "$service" ActiveState)
    service_sub=$(unit_property "$service" SubState)
    [[ $timer_load == loaded && $timer_active == inactive && $timer_sub == dead && \
        $service_load == loaded && $service_active == inactive && $service_sub == dead ]] || {
        printf 'install-disk-snapshot: timer/service did not remain inactive: %s/%s/%s + %s/%s/%s\n' \
            "$timer_load" "$timer_active" "$timer_sub" \
            "$service_load" "$service_active" "$service_sub" >&2
        exit 1
    }
    timer_start_committed=1
    printf 'install-disk-snapshot: timer installed disabled and inactive; prove competing ORC producers absent, run --check, then use --start\n'
fi
