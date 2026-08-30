#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
libexec_dir=$HOME/.local/libexec/dev-hermit
bootstrap=$libexec_dir/operational-tool
start=0

if [[ ${1:-} == --start ]]; then
    start=1
    shift
fi
if [[ $# -ne 0 ]]; then
    printf 'usage: %s [--start]\n' "$0" >&2
    exit 2
fi

install -d -m 0755 "$unit_dir" "$libexec_dir"
install -m 0555 "$root/scripts/hermit-health-tick.sh" "$bootstrap"
install -m 0644 "$root/scripts/systemd/hermit-owner-checkout-validate.service" "$unit_dir/"
install -m 0644 "$root/scripts/systemd/hermit-owner-checkout-validate.timer" "$unit_dir/"

cmp --silent "$root/scripts/hermit-health-tick.sh" "$bootstrap" || {
    printf 'owner-watch install: installed operational bootstrap differs from source\n' >&2
    exit 1
}
grep -Fq "ExecStart=%h/.local/libexec/dev-hermit/operational-tool --run-current-tool" \
    "$unit_dir/hermit-owner-checkout-validate.service" || {
    printf 'owner-watch install: service does not execute the installed bootstrap\n' >&2
    exit 1
}

systemctl --user daemon-reload
systemctl --user enable hermit-owner-checkout-validate.timer

source_head=$(git -C "$root" rev-parse --verify 'HEAD^{commit}')
bootstrap_sha=$(sha256sum -- "$bootstrap" | awk '{print $1}')
printf 'owner-watch install: source_parent_sha=%s bootstrap_sha256=%s bootstrap=%s\n' \
    "$source_head" "$bootstrap_sha" "$bootstrap"

if [[ $start -eq 1 ]]; then
    systemctl --user start hermit-owner-checkout-validate.timer
    systemctl --user status --no-pager hermit-owner-checkout-validate.timer
else
    printf 'owner-watch install: timer enabled but not started; rerun with --start after verifying the installed unit\n'
fi
