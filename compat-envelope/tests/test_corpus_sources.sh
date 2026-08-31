#!/usr/bin/env bash
# The full-corpus gate must reject missing inputs before it spends hours running
# a smaller population that cannot be compared with the committed floors.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
"$ROOT/compat-envelope/collect-fullcorpus.sh" --self-check-corpus-sources
