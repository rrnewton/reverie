#!/usr/bin/env bash
# Publish the two disk-health artifacts from one serialized, bounded caller.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/publish-disk-snapshots.sh [--check] [--state-root PATH] [--population-timeout SECONDS]

--check                       Validate the producer and its dependencies only.
--state-root PATH             Mutable dev-hermit state root.
--population-timeout SECONDS Per-population compsize bound (positive integer).
EOF
}

mode=run
state_root=${DEV_HERMIT_PARENT:-"${HOME:?HOME must be set}/work/dev-hermit"}
population_timeout=180
while (($#)); do
    case $1 in
        --check)
            mode=check
            shift
            ;;
        --state-root)
            (($# >= 2)) || { printf 'publish-disk-snapshots: --state-root requires a path\n' >&2; exit 64; }
            state_root=$2
            shift 2
            ;;
        --population-timeout)
            (($# >= 2)) || { printf 'publish-disk-snapshots: --population-timeout requires seconds\n' >&2; exit 64; }
            population_timeout=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'publish-disk-snapshots: unknown argument: %s\n' "$1" >&2
            exit 64
            ;;
    esac
done

[[ $population_timeout =~ ^[1-9][0-9]*$ ]] || {
    printf 'publish-disk-snapshots: population timeout must be a positive integer\n' >&2
    exit 64
}

for dependency in realpath python3 btrfs compsize nice ionice flock; do
    command -v "$dependency" >/dev/null 2>&1 || {
        printf 'publish-disk-snapshots: required command is unavailable: %s\n' "$dependency" >&2
        exit 2
    }
done

state_root=$(realpath -e -- "$state_root") || {
    printf 'publish-disk-snapshots: state root is unavailable: %s\n' "$state_root" >&2
    exit 2
}
[[ -d $state_root ]] || {
    printf 'publish-disk-snapshots: state root is not a directory: %s\n' "$state_root" >&2
    exit 2
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
source_root=$(cd -- "$script_dir/.." && pwd -P)
producer=$source_root/ci-hub/health/operational_health.py
[[ -x $producer && ! -L $producer ]] || {
    printf 'publish-disk-snapshots: operational-health producer is unavailable: %s\n' "$producer" >&2
    exit 2
}

if [[ $mode == check ]]; then
    printf 'publish-disk-snapshots: producer and dependencies are available\n'
    exit 0
fi

state_dir=$state_root/ignored/ops-state
mkdir -p -- "$state_dir"
lock=$state_dir/disk-snapshot.lock
exec {lock_fd}>"$lock"
if ! flock -n "$lock_fd"; then
    printf 'publish-disk-snapshots: another disk snapshot publication is active\n' >&2
    exit 75
fi

# Keep the two publications in one critical section and one systemd cgroup.
# Each operational-health command publishes by atomic rename. `set -e` means a
# failed or unusable path scan prevents the later free-space publication and
# makes the oneshot fail instead of presenting the run as complete.
PYTHONDONTWRITEBYTECODE=1 python3 "$producer" cache-disk-paths \
    --state-root "$state_root" \
    --state-file "$state_dir/disk-paths.json" \
    --previous-state-file "$state_dir/disk-paths.previous.json" \
    --population-timeout "$population_timeout"
PYTHONDONTWRITEBYTECODE=1 python3 "$producer" cache-disk \
    --filesystem-path "$state_root" \
    --state-file "$state_dir/disk.json"
