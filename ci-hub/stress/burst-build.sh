#!/usr/bin/env bash
# Adapter: maps nightly.sh's positional contract (<sha> <width> <timeout>
# <workload>) onto a sha-accurate burst WITHOUT reflink-copying target/.
#
# WHY no reflink: `cp -a --reflink=auto hermit/target <wt>/target` is (a) blocked
# by this host's BPFJailer FS policy for the agent (3pai) role — FILE_OPEN on the
# clone — and (b) a known cmake-cache poisoner for native builds (see memory
# "reflink-seed-cmake-cache-cross-worktree-pollution"). So instead of a throwaway
# per-SHA worktree seeded from the primary (what `stress-burst --build-at` does),
# this adapter keeps ONE persistent target directory outside a short-lived
# wrkslots validation checkout. The build is a cold compile the first night and
# incremental thereafter, while every Git worktree remains registered and is
# removed through the same lifecycle interface.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SHA="$1"; WIDTH="${2:-64}"; TIMEOUT="${3:-20}"; WL="$4"
BIN_NAME="${WL%%:*}"
WRKSLOTS="$ROOT/ci-hub/bin/wrkslots"
SLOT="stress-nightly-${SHA:0:12}-$$"
SLOT_CREATED=0
TARGET_DIR="$ROOT/ignored/ci-hub/stress-target/nightly"
WT=""

cleanup() {
  local rc=$?
  trap - EXIT
  if [ "$SLOT_CREATED" -eq 1 ]; then
    cleanup_output=$(
      "$WRKSLOTS" --allow-existing-unregistered-worktrees \
        remove "$SLOT" --validate-complete \
        --coordinator-pid "$$" --expected-generation 1 2>&1
    )
    cleanup_rc=$?
    if [ "$cleanup_rc" -ne 0 ]; then
      printf '%s\n' \
        "burst-build: validation checkout RETAINED after wrkslots cleanup refused." \
        "state: RETAINED -- the stress result remains exit $rc; cleanup did not complete." \
        "remedy: $cleanup_output" >&2
    fi
  fi
  exit "$rc"
}
trap cleanup EXIT

short() { git -C "$WT" rev-parse --short HEAD 2>/dev/null || echo unknown; }
detcore_package_name() {
  grep -m1 -E '^name = "(hermit-)?detcore"$' "$1/detcore/Cargo.toml" |
    cut -d '"' -f 2
}

mkdir -p "$TARGET_DIR"
create_output=$(
  "$WRKSLOTS" --allow-existing-unregistered-worktrees \
    create "$SLOT" --format json --slot-type validate \
    --coordinator-authorized --agent "stress-nightly-$$" \
    --task "nightly-stress-$SHA" \
    --purpose "build the nightly stress subject at its recorded commit" \
    --owner-pid "$$" --coordinator-pid "$$" \
    --repo checkout=hermit --start "checkout=$SHA"
)
create_rc=$?
if [ "$create_rc" -ne 0 ]; then
  printf '%s\n' \
    "burst-build: wrkslots could not create the validation checkout." \
    "state: REFUSED -- no stress workload ran, so this is WT_FAIL rather than a test result." \
    "remedy: follow the wrkslots refusal above, then rerun this command." >&2
  echo "$SHA,,,,,,,,WT_FAIL"
  exit 0
fi
SLOT_CREATED=1
WT=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["checkouts"][0]["path"])' <<<"$create_output")
parse_rc=$?
if [ "$parse_rc" -ne 0 ] || [ ! -d "$WT" ]; then
  printf '%s\n' \
    "burst-build: wrkslots returned no readable checkout path." \
    "state: REFUSED -- no stress workload ran, so this is WT_FAIL rather than a test result." \
    "remedy: run '$WRKSLOTS status --slot $SLOT --format json' and inspect the recorded path." >&2
  echo "$SHA,,,,,,,,WT_FAIL"
  exit 0
fi

# Build the burst binary in-tree (incremental; NO reflink seed of target/).
bstart=$(date +%s)
DETCOR_PACKAGE="$(detcore_package_name "$WT")"
case "$DETCOR_PACKAGE" in
  detcore | hermit-detcore) ;;
  *) echo "$SHA,$(short),$(( $(date +%s)-bstart )),$WIDTH,,,,,BUILD_FAIL"; exit 0 ;;
esac
if ! ( cd "$WT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo test -p "$DETCOR_PACKAGE" --test "$BIN_NAME" --no-run ) >/dev/null 2>&1; then
  echo "$SHA,$(short),$(( $(date +%s)-bstart )),$WIDTH,,,,,BUILD_FAIL"; exit 0
fi

# Burst against the just-built, sha-matched binary.
"$ROOT/ci-hub/stress/stress-burst" --prebuilt "$WT" --target-dir "$TARGET_DIR" \
  --width "$WIDTH" --timeout "$TIMEOUT" --workload "$WL"
