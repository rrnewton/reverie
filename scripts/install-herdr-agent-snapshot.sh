#!/usr/bin/env bash
# Install the lightweight Herdr fleet-snapshot producer independently of ORC.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/install-herdr-agent-snapshot.sh [--check] [--no-start]

--check     Validate the exact live Herdr bindings without installing units.
--no-start  Install and enable the timer, but do not start it in this invocation.
EOF
}

mode=install
start=1
for argument in "$@"; do
    case "$argument" in
        --check) mode=check ;;
        --no-start) start=0 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'install-herdr-agent-snapshot: unknown argument: %s\n' "$argument" >&2; exit 64 ;;
    esac
done

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
source_root=$(cd -- "$script_dir/.." && pwd -P)
state_root=${DEV_HERMIT_PARENT:-"${HOME:?HOME must be set}/work/dev-hermit"}
state_root=$(cd -- "$state_root" && pwd -P)

herdr_bin=${HERDR_BIN:-"$HOME/bin/herdr"}
agentctl_bin=${AGENTCTL_BIN:-"$HOME/.local/bin/agentctl"}
bindings="$state_root/.tick-hub/herdr-agent-bindings.json"
producer="$source_root/ci-hub/health/herdr_agent_snapshot.py"
unit_dir=${HERMIT_USER_UNIT_DIR:-"${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"}
systemctl_bin=${SYSTEMCTL_BIN:-systemctl}
service=hermit-herdr-agent-snapshot.service
timer=hermit-herdr-agent-snapshot.timer

[[ -x $herdr_bin ]] || { printf 'install-herdr-agent-snapshot: Herdr is not executable at %s\n' "$herdr_bin" >&2; exit 2; }
[[ -x $agentctl_bin ]] || { printf 'install-herdr-agent-snapshot: agentctl is not executable at %s\n' "$agentctl_bin" >&2; exit 2; }
[[ -r $bindings ]] || { printf 'install-herdr-agent-snapshot: binding file is missing at %s\n' "$bindings" >&2; exit 2; }
[[ -x $producer ]] || { printf 'install-herdr-agent-snapshot: producer is not executable at %s\n' "$producer" >&2; exit 2; }

check_status=0
python3 "$producer" --root "$state_root" --bindings "$bindings" \
    --herdr-bin "$herdr_bin" --agentctl-bin "$agentctl_bin" --gate --check \
    || check_status=$?
[[ $mode == check ]] && exit "$check_status"
if ((check_status != 0)); then
    printf 'install-herdr-agent-snapshot: live authority is currently unavailable (rc=%s); installing the retry timer anyway\n' \
        "$check_status" >&2
fi

install -d -m 0700 "$unit_dir"
install -m 0644 "$source_root/scripts/systemd/$service" "$unit_dir/$service"
install -m 0644 "$source_root/scripts/systemd/$timer" "$unit_dir/$timer"
"$systemctl_bin" --user daemon-reload
"$systemctl_bin" --user enable "$timer"
if ((start)); then
    "$systemctl_bin" --user restart "$timer"
    "$systemctl_bin" --user status --no-pager "$timer"
else
    "$systemctl_bin" --user show --no-pager \
        --property=LoadState --property=UnitFileState --property=ActiveState "$timer"
fi
exit "$check_status"
