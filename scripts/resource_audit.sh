#!/bin/bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# resource_audit.sh — periodic system-resource audit for the dev-hermit box.
#
# Intended to run every heartbeat/fleet-monitor cycle as an early-warning
# system against OOMD kills, disk exhaustion, zombie buildup, runaway process
# fan-out, and stale eden/codesync mounts left over from fbsource imports.
#
# It is READ-ONLY: it inspects cgroup v2 accounting, PSI, df, and the process
# table and prints an OK/WARN/CRIT report. It NEVER kills processes or unmounts
# anything. Remediation of stale eden mounts lives in the sibling script
# scripts/cleanup_stale_eden.sh.
#
# Exit status (so the caller can escalate):
#   0  everything OK
#   1  at least one WARN, no CRIT
#   2  at least one CRIT
#
# Usage:
#   scripts/resource_audit.sh            # human-readable report
#   scripts/resource_audit.sh --quiet    # only print WARN/CRIT lines + summary
#   scripts/resource_audit.sh --json     # one machine-readable JSON object
#
# Thresholds are overridable via environment variables (see CONFIG below).

set -uo pipefail

# ---------------------------------------------------------------------------
# CONFIG (percentages unless noted). Override via environment.
# ---------------------------------------------------------------------------
MEM_WARN=${MEM_WARN:-80}          # % of cgroup memory.max (the 80%-of-RAM cap)
MEM_CRIT=${MEM_CRIT:-92}
SWAP_WARN_GB=${SWAP_WARN_GB:-16}  # absolute swap usage, GiB
SWAP_CRIT_GB=${SWAP_CRIT_GB:-48}
MEM_PSI_WARN=${MEM_PSI_WARN:-20}  # memory PSI "some" avg10
MEM_PSI_CRIT=${MEM_PSI_CRIT:-40}
CPU_PSI_WARN=${CPU_PSI_WARN:-40}  # cpu PSI "some" avg10
CPU_PSI_CRIT=${CPU_PSI_CRIT:-70}
LOAD_WARN_RATIO=${LOAD_WARN_RATIO:-100}  # loadavg1 as % of allotted CPUs
LOAD_CRIT_RATIO=${LOAD_CRIT_RATIO:-150}
DISK_WARN=${DISK_WARN:-85}
DISK_CRIT=${DISK_CRIT:-94}
ZOMBIE_WARN=${ZOMBIE_WARN:-15}
ZOMBIE_CRIT=${ZOMBIE_CRIT:-40}
PROC_WARN=${PROC_WARN:-250}       # processes owned by this user
PROC_CRIT=${PROC_CRIT:-500}
VALIDATE_WARN=${VALIDATE_WARN:-3} # concurrent Hermit validation runs
VALIDATE_CRIT=${VALIDATE_CRIT:-6}
STALE_TMP_AGE_HOURS=${STALE_TMP_AGE_HOURS:-6}   # age before a codesync clone is "stale"
DISK_PATHS=${DISK_PATHS:-"/ /tmp /data"}

QUIET=0
JSON=0
for arg in "$@"; do
    case "$arg" in
        --quiet) QUIET=1 ;;
        --json) JSON=1 ;;
        -h | --help)
            sed -n '3,32p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "resource_audit.sh: unknown argument: $arg" >&2; exit 64 ;;
    esac
done

# ---------------------------------------------------------------------------
# Result accumulation. Each finding is "LEVEL\tKEY\tMESSAGE".
# ---------------------------------------------------------------------------
declare -a FINDINGS=()
WORST=0  # 0 OK, 1 WARN, 2 CRIT
declare -a JSON_METRICS=()

# escalate LEVEL KEY MESSAGE   (LEVEL in OK|WARN|CRIT)
function finding {
    local level=$1 key=$2 msg=$3
    FINDINGS+=("$level"$'\t'"$key"$'\t'"$msg")
    case "$level" in
        CRIT) ((WORST < 2)) && WORST=2 ;;
        WARN) ((WORST < 1)) && WORST=1 ;;
    esac
}

# level_for VALUE WARN CRIT  -> prints OK/WARN/CRIT (integer compare, VALUE floored)
function level_for {
    local v=${1%.*} warn=$2 crit=$3
    v=${v:-0}
    if ((v >= crit)); then echo CRIT
    elif ((v >= warn)); then echo WARN
    else echo OK; fi
}

function metric { JSON_METRICS+=("\"$1\":$2"); }

# bytes_h BYTES -> human-readable (GiB with one decimal, or MiB when small)
function bytes_h {
    local b=${1:-0}
    if ((b >= 1073741824)); then
        awk -v b="$b" 'BEGIN{printf "%.1fGiB", b/1073741824}'
    else
        awk -v b="$b" 'BEGIN{printf "%.0fMiB", b/1048576}'
    fi
}

# ---------------------------------------------------------------------------
# Locate the outer user-<uid>.slice cgroup (the safe-dev-limits cap boundary).
# ---------------------------------------------------------------------------
function user_slice_cgroup {
    local uid rel base
    uid=$(id -u)
    # Derive from our own cgroup so we track the real slice even if nested.
    rel=$(awk -F: '$1=="0"{print $3}' /proc/self/cgroup 2>/dev/null)
    if [[ $rel == *"/user-$uid.slice"* ]]; then
        base="/sys/fs/cgroup${rel%%/user-$uid.slice*}/user-$uid.slice"
        if [[ -r "$base/memory.current" ]]; then
            echo "$base"; return 0
        fi
    fi
    base="/sys/fs/cgroup/user.slice/user-$uid.slice"
    [[ -r "$base/memory.current" ]] && { echo "$base"; return 0; }
    return 1
}

# psi_some FILE -> avg10 for the "some" line (integer floor), or empty
function psi_some {
    [[ -r $1 ]] || return 0
    awk '/^some/{sub("avg10=","",$2); print $2}' "$1" 2>/dev/null | head -1
}

# ---------------------------------------------------------------------------
# 1. Memory + swap (cgroup v2, the enforced cap).
# ---------------------------------------------------------------------------
CG=$(user_slice_cgroup || true)
if [[ -n ${CG:-} ]]; then
    mem_cur=$(cat "$CG/memory.current" 2>/dev/null || echo 0)
    mem_max=$(cat "$CG/memory.max" 2>/dev/null || echo max)
    if [[ $mem_max == max || $mem_max -le 0 ]]; then
        # No cap on the slice: fall back to physical RAM.
        mem_max=$(( $(awk '/MemTotal/{print $2}' /proc/meminfo) * 1024 ))
    fi
    mem_pct=$(( mem_cur * 100 / mem_max ))
    lvl=$(level_for "$mem_pct" "$MEM_WARN" "$MEM_CRIT")
    finding "$lvl" memory \
        "cgroup memory ${mem_pct}% of cap ($(bytes_h "$mem_cur")/$(bytes_h "$mem_max"))"
    metric memory_pct "$mem_pct"

    swap_cur=$(cat "$CG/memory.swap.current" 2>/dev/null || echo 0)
    swap_gb=$(( swap_cur / 1073741824 ))
    lvl=$(level_for "$swap_gb" "$SWAP_WARN_GB" "$SWAP_CRIT_GB")
    finding "$lvl" swap "swap in use ${swap_gb}GiB"
    metric swap_gb "$swap_gb"

    mpsi=$(psi_some "$CG/memory.pressure")
    if [[ -n $mpsi ]]; then
        lvl=$(level_for "$mpsi" "$MEM_PSI_WARN" "$MEM_PSI_CRIT")
        finding "$lvl" mem_pressure "memory PSI some avg10=${mpsi}"
        metric mem_psi_avg10 "${mpsi:-0}"
    fi
else
    finding WARN memory "could not read user-slice cgroup v2 accounting"
fi

# ---------------------------------------------------------------------------
# 2. CPU pressure + load.
# ---------------------------------------------------------------------------
if [[ -n ${CG:-} ]]; then
    cpsi=$(psi_some "$CG/cpu.pressure")
    if [[ -n $cpsi ]]; then
        lvl=$(level_for "$cpsi" "$CPU_PSI_WARN" "$CPU_PSI_CRIT")
        finding "$lvl" cpu_pressure "cpu PSI some avg10=${cpsi}"
        metric cpu_psi_avg10 "${cpsi:-0}"
    fi
    # Allotted CPUs from the cgroup CPU quota if present, else nproc.
    cpus=$(nproc)
    if [[ -r "$CG/cpu.max" ]]; then
        read -r quota period < "$CG/cpu.max"
        if [[ $quota != max && ${period:-0} -gt 0 ]]; then
            cpus=$(( quota / period ))
            ((cpus < 1)) && cpus=1
        fi
    fi
    load1=$(awk '{print $1}' /proc/loadavg)
    load1_int=${load1%.*}
    ratio=$(( load1_int * 100 / cpus ))
    lvl=$(level_for "$ratio" "$LOAD_WARN_RATIO" "$LOAD_CRIT_RATIO")
    finding "$lvl" load "loadavg1 ${load1} = ${ratio}% of ${cpus} allotted CPUs"
    metric load1_pct_of_cpus "$ratio"
fi

# ---------------------------------------------------------------------------
# 3. Disk usage.
# ---------------------------------------------------------------------------
declare -A seen_dev=()
for p in $DISK_PATHS; do
    [[ -e $p ]] || continue
    read -r dev use mnt < <(df -P "$p" 2>/dev/null | awk 'NR==2{gsub("%","",$5); print $1, $5, $6}')
    [[ -z ${dev:-} ]] && continue
    [[ -n ${seen_dev[$dev]:-} ]] && continue   # dedupe by backing device
    seen_dev[$dev]=1
    lvl=$(level_for "$use" "$DISK_WARN" "$DISK_CRIT")
    finding "$lvl" "disk:$mnt" "disk ${use}% used on ${mnt} (${dev})"
    metric "disk_pct_$(echo "$mnt" | tr -c 'A-Za-z0-9' '_')" "$use"
done

# ---------------------------------------------------------------------------
# 4. Process counts + heavy build fan-out + concurrent validates.
# ---------------------------------------------------------------------------
my_procs=$(ps -u "$(id -u)" --no-headers 2>/dev/null | wc -l)
lvl=$(level_for "$my_procs" "$PROC_WARN" "$PROC_CRIT")
finding "$lvl" procs "$my_procs processes owned by $(id -un)"
metric my_procs "$my_procs"

builders=$(pgrep -u "$(id -u)" -c -x 'cc1|cc1plus|rustc|cargo|ld|lld|as|buck2' 2>/dev/null) || builders=0
metric build_procs "$builders"
finding OK build "${builders} active compiler/build processes"

validates=$(pgrep -u "$(id -u)" -f -c 'scripts/validate\.rs' 2>/dev/null) || validates=0
lvl=$(level_for "$validates" "$VALIDATE_WARN" "$VALIDATE_CRIT")
finding "$lvl" validate "${validates} concurrent scripts/validate.rs runs"
metric validate_runs "$validates"

# ---------------------------------------------------------------------------
# 5. Zombie (defunct) processes.
# ---------------------------------------------------------------------------
zombies=$(ps -eo stat --no-headers 2>/dev/null | grep -c '^Z' || true)
zombies=${zombies:-0}
lvl=$(level_for "$zombies" "$ZOMBIE_WARN" "$ZOMBIE_CRIT")
zmsg="$zombies zombie/defunct processes"
if ((zombies > 0)); then
    top=$(ps -eo stat,comm --no-headers 2>/dev/null | awk '$1 ~ /^Z/{print $2}' \
        | sort | uniq -c | sort -rn | head -3 | awk '{printf "%s(%s) ", $2, $1}')
    zmsg="$zmsg [${top% }]"
fi
finding "$lvl" zombies "$zmsg"
metric zombies "$zombies"

# ---------------------------------------------------------------------------
# 6. Stale eden/codesync mounts from fbsource imports.
#    Codesync clones live under /tmp/codesync-* (== /data/tmpvol/codesync-*).
#    The cleanup helper protects primary mounts below ORC_DEV_ROOT.
# ---------------------------------------------------------------------------
stale_mounts=0
if command -v edenfsctl >/dev/null 2>&1; then
    mapfile -t eden_list < <(timeout 20 edenfsctl list 2>/dev/null || true)
    for m in "${eden_list[@]}"; do
        case "$m" in
            */codesync-*) ((stale_mounts++)) ;;
        esac
    done
fi
stale_tmp=$(find /tmp -maxdepth 1 -name 'codesync-*' -type d \
    -mmin "+$((STALE_TMP_AGE_HOURS * 60))" 2>/dev/null | wc -l)
lvl=$(level_for "$stale_mounts" 1 8)
finding "$lvl" eden \
    "${stale_mounts} stale codesync eden mounts; ${stale_tmp} codesync dirs older than ${STALE_TMP_AGE_HOURS}h"
metric stale_eden_mounts "$stale_mounts"
metric stale_codesync_dirs "$stale_tmp"
if ((stale_mounts > 0)); then
    finding OK eden_hint "run scripts/cleanup_stale_eden.sh (dry-run) then --apply to reclaim"
fi

# ---------------------------------------------------------------------------
# Output.
# ---------------------------------------------------------------------------
if ((JSON == 1)); then
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    worst_s=OK; ((WORST == 1)) && worst_s=WARN; ((WORST == 2)) && worst_s=CRIT
    printf '{"timestamp":"%s","status":"%s",%s}\n' \
        "$ts" "$worst_s" "$(IFS=,; echo "${JSON_METRICS[*]}")"
    exit "$WORST"
fi

icon() { case "$1" in OK) echo "  ok";; WARN) echo "⚠️ WARN";; CRIT) echo "🚨 CRIT";; esac; }

nwarn=0; ncrit=0
for f in "${FINDINGS[@]}"; do
    IFS=$'\t' read -r level key msg <<<"$f"
    [[ $level == WARN ]] && ((nwarn++))
    [[ $level == CRIT ]] && ((ncrit++))
    if ((QUIET == 1)) && [[ $level == OK ]]; then continue; fi
    printf "%s  %-14s %s\n" "$(icon "$level")" "$key" "$msg"
done

summary="resource-audit: ${ncrit} CRIT, ${nwarn} WARN ($(date -u +%H:%M:%SZ))"
if ((WORST == 2)); then
    echo "🚨 ALERT $summary — investigate/cleanup NOW (OOMD risk)"
elif ((WORST == 1)); then
    echo "⚠️  $summary"
else
    echo "✅ $summary — all clear"
fi
exit "$WORST"
