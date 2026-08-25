#!/usr/bin/env bash
set -euo pipefail
D=/home/newton/work/dev-hermit/experiments/vcpu-steering_20260820
bb=$(command -v busybox)
root=$(mktemp -d "$D/root.XXXXXX"); trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root"/{bin,dev,etc,proc,root,sbin,sys,tmp}
install -m 0755 "$bb" "$root/bin/busybox"
for g in advance preempt pingpong; do install -m 0755 "$D/guest/$g" "$root/bin/$g"; done
install -m 0755 "$D/guest/init-disk" "$root/init"
printf 'root:x:0:0:root:/:/bin/sh\n' >"$root/etc/passwd"
while IFS= read -r a; do [[ $a == bin/busybox ]] && continue
  mkdir -p "$root/$(dirname -- "$a")"; ln -s /bin/busybox "$root/$a"; done < <("$bb" --list-full)
(cd "$root" && find . | cpio -o -H newc --quiet | gzip -9) > "$D/initramfs-disk.cpio.gz"
echo "built $(du -h "$D/initramfs-disk.cpio.gz" | cut -f1)"
