#!/usr/bin/env bash
set -euo pipefail
D=/home/newton/work/dev-hermit/experiments/window-law_20260820
SRC=/home/newton/work/dev-hermit/experiments/cc-repro_20260818
bb=$(command -v busybox)
root=$(mktemp -d "$D/root.XXXXXX"); trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root"/{bin,dev,etc,proc,root,sbin,sys,tmp}
install -m 0755 "$bb" "$root/bin/busybox"
install -m 0755 "$SRC/guest/order" "$root/bin/order"
for p in 0 5 15; do install -m 0755 "$D/guest/racer_p$p" "$root/bin/racer_p$p"; done
install -m 0755 "$SRC/guest/init" "$root/init"
(cd "$root" && find . | cpio -o -H newc --quiet | gzip -9) > "$D/initramfs-win.cpio.gz"
echo "built $(du -h "$D/initramfs-win.cpio.gz" | cut -f1)"
