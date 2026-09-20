#!/usr/bin/env bash
# Install the durable disk-snapshot producer independently of ORC.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/install-disk-snapshot.sh [--check] [--start|--no-start]

--check     Validate the producer, dependencies, state root, and bootstrap only.
--start     Install, enable, and restart the timer.
--no-start  Install and enable the timer without starting it (the default).
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
producer=$source_root/scripts/publish-disk-snapshots.py
service=hermit-disk-snapshot.service
timer=hermit-disk-snapshot.timer

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
    printf 'install-disk-snapshot: installed bootstrap matches reviewed source\n'
    exit 0
fi

install -d -m 0700 "$unit_dir"
install -d -m 0755 "$libexec_dir"
bootstrap_staging=$(mktemp "$libexec_dir/.operational-tool.XXXXXX")
trap '[[ -z ${bootstrap_staging:-} ]] || rm -f -- "$bootstrap_staging"' EXIT
install -m 0555 "$bootstrap_source" "$bootstrap_staging"
cmp --silent "$bootstrap_source" "$bootstrap_staging" || {
    printf 'install-disk-snapshot: staged bootstrap differs from reviewed source\n' >&2
    exit 1
}
mv -fT -- "$bootstrap_staging" "$bootstrap"
bootstrap_staging=
[[ -x $bootstrap && -f $bootstrap && ! -L $bootstrap ]] && \
    cmp --silent "$bootstrap_source" "$bootstrap" || {
    printf 'install-disk-snapshot: installed bootstrap differs from reviewed source\n' >&2
    exit 1
}
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
"$systemctl_bin" --user enable "$timer"
if ((start)); then
    "$systemctl_bin" --user restart "$timer"
    unit_property() {
        "$systemctl_bin" --user show --no-pager \
            --property="$2" --value "$1"
    }
    service_load=$(unit_property "$service" LoadState)
    service_active=$(unit_property "$service" ActiveState)
    service_sub=$(unit_property "$service" SubState)
    timer_load=$(unit_property "$timer" LoadState)
    timer_active=$(unit_property "$timer" ActiveState)
    timer_sub=$(unit_property "$timer" SubState)
    [[ $service_load == loaded ]] || {
        printf 'install-disk-snapshot: service is not loaded: %s\n' "$service_load" >&2
        exit 1
    }
    case "$service_active:$service_sub" in
        inactive:dead|activating:start) ;;
        *)
            printf 'install-disk-snapshot: service is not healthy: %s/%s\n' \
                "$service_active" "$service_sub" >&2
            exit 1
            ;;
    esac
    [[ $timer_load == loaded && $timer_active == active && $timer_sub == waiting ]] || {
        printf 'install-disk-snapshot: timer is not loaded and waiting: %s/%s/%s\n' \
            "$timer_load" "$timer_active" "$timer_sub" >&2
        exit 1
    }
    printf 'install-disk-snapshot: service=%s/%s timer=%s/%s\n' \
        "$service_active" "$service_sub" "$timer_active" "$timer_sub"
else
    printf 'install-disk-snapshot: timer enabled but not started; disable the two legacy ORC disk workflows before starting it\n'
fi
