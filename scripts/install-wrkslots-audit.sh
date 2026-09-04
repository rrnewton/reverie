#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
libexec_dir=$HOME/.local/libexec/dev-hermit
bootstrap=$libexec_dir/operational-tool
service=hermit-wrkslots-audit.service
timer=hermit-wrkslots-audit.timer
start=0

if [[ ${1:-} == --start ]]; then
    start=1
    shift
fi
if [[ $# -ne 0 ]]; then
    printf 'usage: %s [--start]\n' "$0" >&2
    exit 2
fi

if [[ -L /usr/bin/gh || ! -f /usr/bin/gh || ! -x /usr/bin/gh ]]; then
    printf 'wrkslots-audit install: required GitHub credential helper is unavailable: /usr/bin/gh\n' >&2
    exit 1
fi

install -d -m 0755 "$unit_dir" "$libexec_dir"
install -m 0555 "$root/scripts/hermit-health-tick.sh" "$bootstrap"
install -m 0644 "$root/scripts/systemd/$service" "$unit_dir/"
install -m 0644 "$root/scripts/systemd/$timer" "$unit_dir/"

cmp --silent "$root/scripts/hermit-health-tick.sh" "$bootstrap" || {
    printf 'wrkslots-audit install: installed operational bootstrap differs from source\n' >&2
    exit 1
}
grep -Fq \
    'ExecStart=%h/.local/libexec/dev-hermit/operational-tool --run-current-tool' \
    "$unit_dir/$service" || {
    printf 'wrkslots-audit install: service does not execute the installed bootstrap\n' >&2
    exit 1
}
grep -Fxq 'OnCalendar=hourly' "$unit_dir/$timer" || {
    printf 'wrkslots-audit install: timer lacks the hourly calendar schedule\n' >&2
    exit 1
}
grep -Fxq 'Persistent=true' "$unit_dir/$timer" || {
    printf 'wrkslots-audit install: timer lacks persistent catch-up\n' >&2
    exit 1
}

systemctl --user daemon-reload
systemctl --user enable "$timer"

source_head=$(git -C "$root" rev-parse --verify 'HEAD^{commit}')
bootstrap_sha=$(sha256sum -- "$bootstrap" | awk '{print $1}')
printf 'wrkslots-audit install: source_parent_sha=%s bootstrap_sha256=%s bootstrap=%s\n' \
    "$source_head" "$bootstrap_sha" "$bootstrap"

if [[ $start -eq 1 ]]; then
    systemctl --user start "$timer"
    systemctl --user status --no-pager "$timer"
else
    printf 'wrkslots-audit install: timer enabled but not started; rerun with --start after verifying the installed unit\n'
fi
