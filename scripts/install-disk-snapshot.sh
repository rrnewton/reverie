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
operational_tool=${OPERATIONAL_TOOL_BIN:-"$HOME/.local/libexec/dev-hermit/operational-tool"}
producer=$source_root/scripts/publish-disk-snapshots.sh
service=hermit-disk-snapshot.service
timer=hermit-disk-snapshot.timer

[[ -x $operational_tool && ! -L $operational_tool ]] || {
    printf 'install-disk-snapshot: immutable-tool bootstrap is unavailable: %s\n' "$operational_tool" >&2
    exit 2
}
[[ -x $producer && ! -L $producer ]] || {
    printf 'install-disk-snapshot: producer is unavailable: %s\n' "$producer" >&2
    exit 2
}
"$producer" --check --state-root "$state_root" --population-timeout 180
[[ $mode == check ]] && exit 0

install -d -m 0700 "$unit_dir"
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

"$systemctl_bin" --user daemon-reload
"$systemctl_bin" --user enable "$timer"
if ((start)); then
    "$systemctl_bin" --user restart "$timer"
    "$systemctl_bin" --user status --no-pager "$service" "$timer"
else
    printf 'install-disk-snapshot: timer enabled but not started; disable the two legacy ORC disk workflows before starting it\n'
fi
