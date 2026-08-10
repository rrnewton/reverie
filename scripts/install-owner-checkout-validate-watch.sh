#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user

install -d -m 0755 "$unit_dir"
install -m 0644 "$root/scripts/systemd/hermit-owner-checkout-validate.service" "$unit_dir/"
install -m 0644 "$root/scripts/systemd/hermit-owner-checkout-validate.timer" "$unit_dir/"
systemctl --user daemon-reload
systemctl --user enable --now hermit-owner-checkout-validate.timer
systemctl --user status --no-pager hermit-owner-checkout-validate.timer
