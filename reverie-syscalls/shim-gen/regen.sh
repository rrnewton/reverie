#!/usr/bin/env bash
# Regenerates reverie-syscalls/src/libc_shim.rs and src/nix_shim.rs, the
# std-free stand-ins for the libc and nix items reverie-syscalls names, from
# the host's (x86_64-unknown-linux-gnu) libc and nix. Needs a nightly
# toolchain for `-Zunpretty=expanded`. Scratch output goes to
# target/shim-gen/. After regenerating, run `cargo test -p reverie-syscalls`:
# the tests inside both files compare every item with the real crates.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=$root/target/shim-gen
mkdir -p "$out"

dep_version() {
  cargo tree --manifest-path "$root/Cargo.toml" -p reverie-syscalls -e normal \
    --depth 1 --prefix none | awk -v n="$1" '$1 == n { sub(/^v/, "", $2); print $2; exit }'
}
libc_version=$(dep_version libc)
nix_version=$(dep_version nix)
bitflags_version=$(dep_version bitflags)
if ! grep -q "^nix = { version = \"=$nix_version\"" "$here/nixdump/Cargo.toml" ||
   ! grep -q "^bitflags = \"=$bitflags_version\"" "$here/nixdump/Cargo.toml"; then
  echo "regen.sh: pin nixdump/Cargo.toml to nix $nix_version and bitflags $bitflags_version" >&2
  exit 1
fi

cargo rustc --manifest-path "$root/Cargo.toml" -p "libc@$libc_version" --lib \
  -- -Zunpretty=expanded > "$out/libc-expanded.rs"
python3 "$here/find_roots.py" "$here/roots-skip.txt" \
  "$root/reverie-syscalls/src" "$root/reverie/src" > "$out/roots.txt"
python3 "$here/gen_libc_shim.py" "$out/libc-expanded.rs" "$out/roots.txt" \
  "$libc_version" "$out/libc-resolved.txt" > "$root/reverie-syscalls/src/libc_shim.rs"

cargo run --quiet --manifest-path "$here/nixdump/Cargo.toml" \
  > "$root/reverie-syscalls/src/nix_shim.rs"

rustfmt --edition 2024 "$root/reverie-syscalls/src/libc_shim.rs" \
  "$root/reverie-syscalls/src/nix_shim.rs"
