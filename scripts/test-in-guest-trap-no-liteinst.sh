#!/usr/bin/env bash
# Additive qualification for the in-guest runtime without the site patcher.
# Run after the default LiteInst suites, from a clean source checkout.
set -euo pipefail

if [[ $# != 1 || $1 != /* ]]; then
    echo "usage: scripts/test-in-guest-trap-no-liteinst.sh /absolute/unique-generation" >&2
    exit 2
fi

generation=$1
features=allocator-fixture,rpc-io-fixture,exec-native-injection,loader-private-runtime
graph=$(mktemp)
trap 'rm -f "$graph"' EXIT

# Selecting every consumer root also catches accidental default-feature
# activation through the preload or example CLI. Include build and dev edges.
cargo tree --locked --no-default-features \
    -p reverie-liteinst -p reverie-liteinst-preload -p reverie-examples \
    --features "reverie-liteinst/allocator-fixture,reverie-liteinst/rpc-io-fixture,reverie-liteinst/exec-native-injection,reverie-liteinst/loader-private-runtime" \
    --edges normal,build,dev --prefix none --format '{p}' > "$graph"
if rg -q '^liteinst2 v' "$graph"; then
    echo 'in-guest-trap dependency graph unexpectedly includes liteinst2' >&2
    cat "$graph" >&2
    exit 1
fi
echo 'in-guest-trap graph: liteinst2 absent, including loader/native-exec features'

scripts/build-liteinst-test-runtime.rs --for-ci "$generation" --no-liteinst
# The producer binds this exact feature set and source identity. The test
# consumers reject an ON runtime, a dirty tree, or a mismatched receipt.
source "$generation/bundle/fixture.env"
cargo test --locked -p reverie-liteinst --no-default-features \
    --features "$features" --no-fail-fast -- --test-threads=1
cargo test --locked -p reverie-liteinst-preload -p reverie-loader \
    --no-default-features --no-fail-fast -- --test-threads=1
cargo check --locked -p reverie-inguest --no-default-features \
    --features exec-stop,stage1a-probe,exec-native-injection,loader-private-runtime \
    --all-targets
cargo build --locked -p reverie-examples --no-default-features
cargo test --locked -p reverie-examples --no-default-features \
    --test liteinst -- --test-threads=1
