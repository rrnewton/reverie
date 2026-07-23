#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

# Enumerates and counts stub annotations across the tree.
#
# Convention: every intentionally-incomplete code site carries a marker of the
# form `TODO-STUB(#<issue>): <description>` referencing a tracking issue on
# rrnewton/reverie. This script lists every marker and prints a total so the
# stub count can be tracked over time (and gated in CI).
#
# Usage:
#   scripts/count-stubs.sh          # list every TODO-STUB with file:line, then total
#   scripts/count-stubs.sh --count  # print only the total count

set -euo pipefail

# Run from the repository root regardless of the caller's cwd.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

pattern='TODO-STUB'

# Prefer ripgrep when available; fall back to grep -rn. Exclude this script and
# the VCS metadata directory so the marker's own documentation is not counted.
if command -v rg >/dev/null 2>&1; then
    matches="$(rg --no-heading --line-number --glob '!scripts/count-stubs.sh' "$pattern" || true)"
else
    matches="$(grep -rn --exclude=count-stubs.sh --exclude-dir=.git "$pattern" . || true)"
fi

count=0
if [[ -n "$matches" ]]; then
    count="$(printf '%s\n' "$matches" | wc -l | tr -d ' ')"
fi

if [[ "${1:-}" == "--count" ]]; then
    printf '%s\n' "$count"
    exit 0
fi

if [[ -n "$matches" ]]; then
    printf '%s\n' "$matches"
    printf '%s\n' "----------------------------------------"
fi
printf 'TODO-STUB markers: %s\n' "$count"
