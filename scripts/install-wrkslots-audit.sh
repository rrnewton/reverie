#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
libexec_dir=$HOME/.local/libexec/dev-hermit
bootstrap=$libexec_dir/operational-tool
install_label=wrkslots-audit
github_helper=$libexec_dir/gh
service=hermit-wrkslots-audit.service
timer=hermit-wrkslots-audit.timer
health_service=hermit-health-tick.service
health_timer=hermit-health-tick.timer
start=0

if [[ ${1:-} == --start ]]; then
    start=1
    shift
fi
if [[ $# -ne 0 ]]; then
    printf 'usage: %s [--start]\n' "$0" >&2
    exit 2
fi

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
install -m 0644 "$root/scripts/systemd/$health_timer" "$unit_dir/"

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
    'ExecStart=%h/.local/libexec/dev-hermit/operational-tool --run-current-tool --state-root %h/work/dev-hermit --tool-relative scripts/hermit-operational-tick' \
    "$unit_dir/$health_service" || {
    printf 'wrkslots-audit install: health service does not execute through the installed bootstrap\n' >&2
    exit 1
}
grep -Fxq 'OnUnitActiveSec=30min' "$unit_dir/$health_timer" || {
    printf 'wrkslots-audit install: health timer does not carry the 30-minute cadence\n' >&2
    exit 1
}
grep -Fxq 'Persistent=true' "$unit_dir/$health_timer" || {
    printf 'wrkslots-audit install: health timer lacks persistent catch-up\n' >&2
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
systemctl --user enable "$timer" "$health_timer"

source_head=$(git -C "$root" rev-parse --verify 'HEAD^{commit}')
bootstrap_sha=$(sha256sum -- "$bootstrap" | awk '{print $1}')
github_helper_sha=$(sha256sum -- "$github_helper" | awk '{print $1}')
printf 'wrkslots-audit install: source_parent_sha=%s bootstrap_sha256=%s bootstrap=%s github_helper_source=%s github_helper_sha256=%s github_helper=%s\n' \
    "$source_head" "$bootstrap_sha" "$bootstrap" "$github_helper_source" \
    "$github_helper_sha" "$github_helper"

if [[ $start -eq 1 ]]; then
    systemctl --user start "$timer"
    # Restart, rather than start, so an already-active timer recalculates its
    # next firing from the newly installed 30-minute interval immediately.
    systemctl --user restart "$health_timer"
    systemctl --user status --no-pager "$timer" "$health_timer"
else
    printf 'wrkslots-audit install: timers enabled but not started; rerun with --start after verifying the installed units\n'
fi
