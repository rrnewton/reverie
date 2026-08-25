#!/usr/bin/env bash
# Purpose-built initramfs: busybox + the interleaving detector.
# Modelled on demos/qemu-busybox/build-initramfs.sh; the demos are NOT modified.
set -euo pipefail
D=/home/newton/work/dev-hermit/experiments/cc-repro_20260818
out=${1:-$D/initramfs-cc.cpio.gz}
bb=$(command -v busybox)
file "$bb" | grep -q 'statically linked' || { echo "busybox must be static"; exit 1; }
root=$(mktemp -d "$D/root.XXXXXX"); trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root"/{bin,dev,etc,proc,root,sbin,sys,tmp}
install -m 0755 "$bb" "$root/bin/busybox"
install -m 0755 "$D/guest/racer" "$root/bin/racer"
install -m 0755 "$D/guest/order" "$root/bin/order"
install -m 0755 "$D/guest/order_long" "$root/bin/order_long"
install -m 0755 "$D/guest/order_cpu" "$root/bin/order_cpu"
install -m 0755 "$D/guest/order_same" "$root/bin/order_same"
install -m 0755 "$D/guest/init" "$root/init"
printf 'root:x:0:0:root:/:/bin/sh\n' >"$root/etc/passwd"
while IFS= read -r a; do [[ $a == bin/busybox ]] && continue
  mkdir -p "$root/$(dirname -- "$a")"; ln -s /bin/busybox "$root/$a"; done < <("$bb" --list-full)
find "$root" -exec touch -h -d @0 {} +
( cd "$root"; find . -print0 | sort -z | cpio --quiet --null --create --format=newc --owner=0:0 --reproducible ) \
  | gzip -n -9 >"$out"
printf 'initramfs=%s bytes=%s sha256=%s\n' "$out" "$(stat -c %s "$out")" "$(sha256sum "$out" | cut -c1-16)"
