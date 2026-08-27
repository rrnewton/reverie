#!/usr/bin/env bash
# Exercise the Rust-script smoke gate in the three states that matter:
# an empty population refuses, a harmless script passes, and a real type error
# fails while naming the script that did not compile.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/scripts"
cp "$root/scripts/lint-rust-scripts.sh" "$tmp/scripts/"
cp "$root/scripts/lint-exclusions.sh" "$tmp/scripts/"
git -C "$tmp" init -q

run_gate() {
  RUST_SCRIPT_SMOKE_TIMEOUT_SECONDS=120 "$tmp/scripts/lint-rust-scripts.sh" \
    >"$tmp/out" 2>&1
}

if run_gate; then
  echo "test-lint-rust-scripts: empty population passed vacuously" >&2
  exit 1
fi
if ! grep -q 'found NO rust-scripts' "$tmp/out"; then
  echo "test-lint-rust-scripts: empty-population refusal did not name the missing population" >&2
  cat "$tmp/out" >&2
  exit 1
fi

cat >"$tmp/harmless.rs" <<'RS'
#!/usr/bin/env rust-script
fn main() {
    if std::env::args().any(|argument| argument == "--help") {
        println!("Usage: harmless.rs --help");
        return;
    }
    panic!("ordinary work must not run during the smoke test");
}
RS
git -C "$tmp" add harmless.rs
if ! run_gate; then
  echo "test-lint-rust-scripts: harmless --help change did not stay green" >&2
  cat "$tmp/out" >&2
  exit 1
fi
if ! grep -q 'checked 1 script(s) by running --help' "$tmp/out"; then
  echo "test-lint-rust-scripts: harmless run did not assert its one-script population" >&2
  cat "$tmp/out" >&2
  exit 1
fi

rm "$tmp/harmless.rs"
git -C "$tmp" rm --cached -q harmless.rs
cat >"$tmp/broken.rs" <<'RS'
#!/usr/bin/env rust-script
fn main() {
    let _: u32 = "this is a real type error";
}
RS
git -C "$tmp" add broken.rs
if run_gate; then
  echo "test-lint-rust-scripts: real type error passed" >&2
  exit 1
fi
if ! grep -q '=== broken.rs ===' "$tmp/out"; then
  echo "test-lint-rust-scripts: type-error failure did not name broken.rs" >&2
  cat "$tmp/out" >&2
  exit 1
fi
if ! grep -q 'error\[E0308\]' "$tmp/out"; then
  echo "test-lint-rust-scripts: broken.rs failed without the planted type error" >&2
  cat "$tmp/out" >&2
  exit 1
fi

echo "test-lint-rust-scripts: ok -- empty population refused, harmless --help passed, and broken.rs failed by name"
