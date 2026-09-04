#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
libexec_dir=$HOME/.local/libexec/dev-hermit
bootstrap=$libexec_dir/operational-tool
github_helper_source=$HOME/bin/gh
github_helper=$libexec_dir/gh
service=hermit-wrkslots-audit.service
timer=hermit-wrkslots-audit.timer
health_service=hermit-health-tick.service
start=0

if [[ ${1:-} == --start ]]; then
    start=1
    shift
fi
if [[ $# -ne 0 ]]; then
    printf 'usage: %s [--start]\n' "$0" >&2
    exit 2
fi

github_helper_fd=
if ! exec {github_helper_fd}<"$github_helper_source"; then
    printf 'wrkslots-audit install: required GitHub credential helper is unavailable: %s\n' \
        "$github_helper_source" >&2
    exit 1
fi
github_helper_source_fd=/proc/self/fd/$github_helper_fd
if [[ ! -f $github_helper_source_fd || ! -x $github_helper_source_fd ]]; then
    printf 'wrkslots-audit install: required GitHub credential helper is unavailable: %s\n' \
        "$github_helper_source" >&2
    exit 1
fi

install -d -m 0755 "$unit_dir" "$libexec_dir"
github_helper_staging=$(mktemp "$libexec_dir/.gh.XXXXXX")
trap '[[ -z ${github_helper_staging:-} ]] || rm -f -- "$github_helper_staging"' EXIT
install -m 0555 "$github_helper_source_fd" "$github_helper_staging"
cmp --silent "$github_helper_source_fd" "$github_helper_staging" || {
    printf 'wrkslots-audit install: staged GitHub credential helper differs from source\n' >&2
    exit 1
}
mv -fT -- "$github_helper_staging" "$github_helper"
github_helper_staging=
install -m 0555 "$root/scripts/hermit-health-tick.sh" "$bootstrap"
install -m 0644 "$root/scripts/systemd/$service" "$unit_dir/"
install -m 0644 "$root/scripts/systemd/$timer" "$unit_dir/"
install -m 0644 "$root/scripts/systemd/$health_service" "$unit_dir/"

cmp --silent "$root/scripts/hermit-health-tick.sh" "$bootstrap" || {
    printf 'wrkslots-audit install: installed operational bootstrap differs from source\n' >&2
    exit 1
}
[[ ! -L $github_helper && -f $github_helper && -x $github_helper ]] || {
    printf 'wrkslots-audit install: installed GitHub credential helper is not a regular executable\n' >&2
    exit 1
}
cmp --silent "$github_helper_source_fd" "$github_helper" || {
    printf 'wrkslots-audit install: installed GitHub credential helper differs from source\n' >&2
    exit 1
}
exec {github_helper_fd}<&-
grep -Fq \
    'ExecStart=%h/.local/libexec/dev-hermit/operational-tool --run-current-tool' \
    "$unit_dir/$service" || {
    printf 'wrkslots-audit install: service does not execute the installed bootstrap\n' >&2
    exit 1
}
grep -Fq \
    'ExecStart=%h/.local/libexec/dev-hermit/operational-tool --run-current-tool --state-root %h/work/dev-hermit --tool-relative scripts/hermit-health-tick.sh' \
    "$unit_dir/$health_service" || {
    printf 'wrkslots-audit install: health service does not execute through the installed bootstrap\n' >&2
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
github_helper_sha=$(sha256sum -- "$github_helper" | awk '{print $1}')
printf 'wrkslots-audit install: source_parent_sha=%s bootstrap_sha256=%s bootstrap=%s github_helper_sha256=%s github_helper=%s\n' \
    "$source_head" "$bootstrap_sha" "$bootstrap" "$github_helper_sha" "$github_helper"

if [[ $start -eq 1 ]]; then
    systemctl --user start "$timer"
    systemctl --user status --no-pager "$timer"
else
    printf 'wrkslots-audit install: timer enabled but not started; rerun with --start after verifying the installed unit\n'
fi
