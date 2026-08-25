#!/usr/bin/env bash
set -euo pipefail
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
bb=$(command -v busybox)
root=$(mktemp -d "$D/root.XXXXXX"); trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root"/{bin,dev,etc,proc,root,sbin,sys,tmp}
install -m 0755 "$bb" "$root/bin/busybox"
install -m 0755 "$D/guest/advance" "$root/bin/advance"
install -m 0755 "$D/guest/preempt" "$root/bin/preempt"
install -m 0755 "$D/guest/pingpong" "$root/bin/pingpong"
install -m 0755 "$D/guest/init-shell" "$root/init"
printf 'root:x:0:0:root:/:/bin/sh\n' >"$root/etc/passwd"
while IFS= read -r a; do [[ $a == bin/busybox ]] && continue
  mkdir -p "$root/$(dirname -- "$a")"; ln -s /bin/busybox "$root/$a"; done < <("$bb" --list-full)
(cd "$root" && find . | cpio -o -H newc --quiet | gzip -9) > "$D/initramfs-shell.cpio.gz"
echo "built $(du -h "$D/initramfs-shell.cpio.gz" | cut -f1)"
