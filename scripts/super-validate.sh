#!/usr/bin/env bash
# Scheduled superset: Hermit's super profile followed by parent demos 1-8.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scope=all
case "${1:-}" in
  "") ;;
  --demos-only) scope=demos ;;
  --product-only) scope=product ;;
  -h|--help)
    echo "usage: $0 [--demos-only|--product-only]"
    exit 0
    ;;
  *) echo "usage: $0 [--demos-only|--product-only]" >&2; exit 2 ;;
esac

run_with_optional_proxy() {
  if command -v with-proxy >/dev/null 2>&1; then
    with-proxy "$@"
  else
    "$@"
  fi
}

if [ "$scope" != demos ]; then
  if [ ! -x "$ROOT/hermit/scripts/validate.rs" ]; then
    echo "Hermit submodule is missing; initializing it..."
    run_with_optional_proxy git -C "$ROOT" submodule update --init --checkout -- hermit
  fi
  echo "=== Super validation: Hermit product stress profile ==="
  run_with_optional_proxy "$ROOT/hermit/scripts/validate.rs" super --no-label-pr
fi

if [ "$scope" != product ]; then
  echo "=== Super validation: prepare required Demo 8 fixtures ==="
  "$ROOT/scripts/prepare-demo08-assets.sh"
  echo "=== Super validation: parent demos 1-8 ==="
  DEMO08_REQUIRE_ASSETS=1 "$ROOT/demos/run-all.sh" --all
fi

echo "=== Super validation: SUCCESS ==="
