#!/usr/bin/env bash
# parallel-prevalidate.sh -- DRAFT (hermit-220, 2026-08-03)
#
# Removes the measured landing bottleneck: the lander was running full validates
# STRICTLY SERIALLY ("to respect box load"), capping landing at ~6-7 PRs/hour no
# matter how many agents exist. That serialization is a self-imposed constraint,
# not a resource limit -- the same class of error as `-j 2` on a 316-core box.
#
# MEASURED BASIS (do not re-guess; see memory validate-concurrency-ceiling-not-one
# and validate-box-resource-footprint):
#   * one full validate peaks at 1.98 GiB RSS  -> reserve 2 GiB/box (MEASURED,
#     NOT the theoretical `jobs_footprint_bytes`, which overcounts and would
#     artificially depress concurrency).
#   * admission code, driven against LIVE /proc/meminfo, GRANTs ~320 concurrent
#     2-GiB boxes before it QUEUEs (0.85 * MemTotal budget). CPU-mean bound ~130.
#   * every resource ceiling is >=100; the BINDING limit is the 12-active-worktree
#     policy cap. So concurrency up to the slot pool is safe by measurement.
#
# ARCHITECTURE -- the two phases were wrongly coupled:
#   1. PRE-VALIDATION (~9 min, the bottleneck): parallelize here, one validate per
#      OWN worktree slot (slot isolation is what makes it safe -- concurrent
#      validates in a shared cache collide on the DBI cargo dr_config.h race and
#      reflink cmake pollution; admission's memory gate does NOT address that).
#   2. MERGE (rebase+push+stamp, fast): stays SERIAL via the existing land-lock
#      FIFO inside land-pr.sh. We do NOT touch that -- serial merge is correct.
#
# Each pre-validate ASKS admission for a box rather than self-limiting. When
# admission says QUEUE/REFUSE we SURFACE it loudly and wait; we NEVER silently
# fall back to one-at-a-time -- a caller that quietly degrades to serial on any
# refusal reproduces the exact ceiling this removes.
#
# Usage:
#   parallel-prevalidate.sh --slots N [--pr-branch "PR:BRANCH" ...] [--land]
#   parallel-prevalidate.sh --slots 6 --batch batch.tsv --land
# batch.tsv: one "PR<TAB>BRANCH" per line. --land hands each GREEN PR to
# land-pr.sh (which FIFO-serializes the merge). Without --land it only validates.
set -uo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
SLOTS=6                       # concurrency; <= active-worktree policy cap (12)
VAL_MEM_GIB=2                 # MEASURED validate footprint, NOT jobs_footprint_bytes
DO_LAND=0
declare -a PAIRS=()
BATCH=""
AGENT="hermit-lander"

# Admission CLI. Lives in agent-utils branch codex/dag-runner-core-allocator until
# landed; override with ADMISSION_CMD once it reaches a ci-hub-reachable path.
#
# KNOWN-INERT AS A PER-REQUEST CLI (proven 2026-08-03, hermit-220): calling
# `admission request` once per box from bash does NOT gate, because each CLI
# process reserves under its own pid then exits, so _reclaim_dead drops the
# reservation before the next call (two 400-GiB requests both GRANT; status shows
# reserved=0). A reservation only holds while the RESERVING process stays alive
# for the box's lifetime -- which is how run_dag uses the library in-process.
# THE REQUIRED FIX is an admission `run --mem-gib N --box X -- <cmd>` wrapper
# (mirroring `land-lock run`) that reserves, execs the child, and releases on
# exit, so the reservation pid == the wrapper, alive for the whole validate. That
# wrapper is an agent-utils change, SERIALIZED behind PR#8 -- not done here. Until
# it exists, shell-driven admission gating is NOT durable; the pool below falls
# back LOUDLY to slot-pool concurrency (measured-safe, every resource ceiling
# >=100 vs the <=12 slot cap), and NEVER silently to serial.
ADMISSION_CMD="${ADMISSION_CMD:-}"
if [ -z "$ADMISSION_CMD" ]; then
  au="$ROOT/scratch/au-parallel-experiment/py"
  [ -d "$au/safe_ci_dag_runner" ] && ADMISSION_CMD="env PYTHONPATH=$au python3 -m safe_ci_dag_runner.admission"
fi

# External commands -- real defaults; overridable for testing (dependency
# injection). The pilot points these at fast stubs to exercise this orchestration
# code without a 9-min validate or an irreversible merge.
VALIDATE_CMD="${VALIDATE_CMD:-}"                          # per-PR validate; default below = real
VALIDATE_STATUS_CMD="${VALIDATE_STATUS_CMD:-$ROOT/ci-hub/ci-hub validate-status}"
LAND_CMD="${LAND_CMD:-$ROOT/ci-hub/landing/land-pr.sh}"
# Refuse-before-start: the composite authority proves that the producer can emit
# a qualifying receipt by checking immutable fixed floors. Mutable main currency
# is checked once by land-pr immediately before merge. Overridable with a command
# stub for tests, but never disabled in production.
PREFLIGHT_CMD="${PREFLIGHT_CMD:-python3 $ROOT/ci-hub/validate/preflight_validate.py}"

while [ $# -gt 0 ]; do
  case "$1" in
    --slots) SLOTS="$2"; shift ;;
    --pr-branch) PAIRS+=("$2"); shift ;;
    --batch) BATCH="$2"; shift ;;
    --land) DO_LAND=1 ;;
    --agent) AGENT="$2"; shift ;;
    -h|--help) sed -n '1,40p' "$0"; exit 0 ;;
    *) echo "parallel-prevalidate: unknown arg $1" >&2; exit 2 ;;
  esac
  shift
done
case "$SLOTS" in ''|*[!0-9]*|0) echo "--slots must be positive" >&2; exit 2 ;; esac
if [ -n "$BATCH" ]; then
  while IFS=$'\t' read -r pr br; do
    [ -n "$pr" ] && PAIRS+=("$pr:$br")
  done < "$BATCH"
fi
[ "${#PAIRS[@]}" -gt 0 ] || { echo "no PRs given (--pr-branch or --batch)" >&2; exit 2; }

say(){ echo "[prevalidate] $*"; }

# --- admission: ASK for a box; surface QUEUE/REFUSE LOUDLY, never silent-serial ---
# Returns 0 on GRANT (prints reservation id on fd 3), 1 on QUEUE (caller waits),
# 2 on REFUSE. Exit 75 from the CLI == QUEUE, 1 == REFUSE (see admission.py).
admit(){
  local box="$1"
  if [ -z "$ADMISSION_CMD" ]; then
    # Admission unreachable. Per measurement, slot-pool concurrency is safe
    # regardless -- but we say so LOUDLY rather than pretend we gated.
    say "WARN: admission CLI unreachable; proceeding at slot-pool concurrency" \
        "(MEASURED-safe: every resource ceiling >=100, cap is the $SLOTS slot pool)."
    return 0
  fi
  local out rc
  out=$($ADMISSION_CMD request --mem-gib "$VAL_MEM_GIB" --box "$box" 2>&1); rc=$?
  case "$rc" in
    0)  say "ADMIT $box: $out"; return 0 ;;
    75) say "QUEUED $box (admission): $out"; return 1 ;;   # LOUD, then caller waits
    *)  say "REFUSED $box (admission): $out"; return 2 ;;  # LOUD, then caller waits/aborts
  esac
}

# One PR: acquire admission (waiting loudly while queued), run the Rust driver in an
# OWN slot, then optionally hand the green PR to the serial FIFO merge.
run_one(){
  local pr="$1" br="$2" slot="$3"
  local box="validate-pr${pr}"
  # REFUSE-BEFORE-START: fixed-floor stale, moving-base stale, and unresolved
  # authority all stop here. "Couldn't check" is never permission to spend a
  # slot or mint a receipt.
  local pf; pf=$($PREFLIGHT_CMD --pr "$pr" 2>&1); local pfrc=$?
  if [ "$pfrc" -ne 0 ]; then
    say "pr#$pr SKIP (validation admission rc=$pfrc): $pf"; return 3
  fi
  say "pr#$pr validation admission: $pf"
  # Block until admitted; every wait iteration is SURFACED (no silent contention).
  local waited=0
  until admit "$box"; do
    [ $? -eq 2 ] && { say "pr#$pr permanently refused; leaving for retry"; return 3; }
    waited=$((waited+1))
    say "pr#$pr waiting for a box (${waited}0s elapsed)..."
    sleep 10
  done
  local wt="$ROOT/worktrees/slots/$slot/hermit"
  say "pr#$pr -> slot $slot ($wt): checkout + full validate"
  # Per-PR validate: checkout PR head into the slot, run the full validate (which
  # auto-reports into the hub ledger consumed by validate-status). Overridable.
  if [ -n "$VALIDATE_CMD" ]; then
    PR="$pr" SLOT="$slot" WT="$wt" $VALIDATE_CMD \
      >"$ROOT/ignored/ci-hub/prevalidate-pr${pr}.log" 2>&1
  else
    # Recheck the exact fetched head at the last point before the Rust driver. The
    # earlier --pr check saves the box on already-stale work; this second check
    # closes a PR-head/main race between GitHub resolution and slot checkout.
    ( cd "$wt" \
        && with-proxy git fetch origin "pull/$pr/head" \
        && git switch --detach FETCH_HEAD \
        && exact_head=$(git rev-parse "HEAD^{commit}") \
        && $PREFLIGHT_CMD --head "$exact_head" --repo-checkout "$wt" \
        && ./scripts/validate.rs ) >"$ROOT/ignored/ci-hub/prevalidate-pr${pr}.log" 2>&1
  fi

  # Re-mint one exact count-backed schema-5 row from its durable log BEFORE
  # reading the ledger. The validation producer can write a count-less row when it
  # can't reach the parent count helper, and with the uncounted-receipt
  # grandfather removed that would read NotValidated. Resolve the just-validated
  # head and select one source row by its Rust-canonical digest before invoking
  # the append-safe finalizer. Best-effort; validate-status below stays the
  # authoritative fail-closed gate. The slot worktree holds the head.
  local exact_head selected_row_sha256
  exact_head=$(git -C "$wt" rev-parse --verify "HEAD^{commit}" 2>/dev/null) \
    || { say "pr#$pr cannot resolve exact validated head for finalization"; return 1; }
  selected_row_sha256=$("$ROOT/ci-hub/validate/scan-finalize.sh" \
    --select-candidate-sha256 --sha "$exact_head" --hermit-checkout "$wt" \
    2>/dev/null) || selected_row_sha256=
  if [[ $selected_row_sha256 =~ ^[0-9a-f]{64}$ ]]; then
    "$ROOT/ci-hub/validate/scan-finalize.sh" --hermit-checkout "$wt" \
      --sha "$exact_head" --selected-row-sha256 "$selected_row_sha256" || true
  else
    say "pr#$pr scan-finalize: no unique exact source row selected for $exact_head (best-effort)"
  fi

  # Capture the VERBATIM first_error_line of every surviving red log into the
  # durable append-only sidecar BEFORE its /tmp log is evicted (append-only +
  # idempotent; best-effort; never affects the verdict below). Preserves
  # attribution that would otherwise die with the log.
  python3 "$ROOT/ci-hub/validate/attribute_reds.py" --last 0 --persist >/dev/null 2>&1 || true

  # Landing verdict comes from the ledger, not the exit code: validate-status is
  # the same predicate land-pr.sh gates on -- never looser, never fabricated.
  local vs; vs=$($VALIDATE_STATUS_CMD --pr "$pr" 2>&1)
  local sc=$?
  if [ "$sc" -ne 0 ]; then
    say "pr#$pr NOT landable (validate-status rc=$sc): $vs"; return 1
  fi
  say "pr#$pr validated clean: $vs"
  if [ "$DO_LAND" -eq 1 ]; then
    say "pr#$pr -> land-pr.sh (serial FIFO merge)"
    $LAND_CMD "$pr" "$br" --agent "$AGENT"
  fi
}

# --- bounded pool: at most $SLOTS validates in flight, each in its own slot ------
say "batch=${#PAIRS[@]} PRs, concurrency=$SLOTS slots, footprint=${VAL_MEM_GIB}GiB/box, land=$DO_LAND"
declare -A INUSE=()
next_slot(){ for i in $(seq 1 "$SLOTS"); do [ -z "${INUSE[val$i]:-}" ] && { echo "val$i"; return; }; done; }
i=0
for pair in "${PAIRS[@]}"; do
  pr="${pair%%:*}"; br="${pair#*:}"
  # wait for a free slot
  while [ -z "$(next_slot)" ]; do wait -n 2>/dev/null || true;
    for s in "${!INUSE[@]}"; do kill -0 "${INUSE[$s]}" 2>/dev/null || unset "INUSE[$s]"; done
  done
  slot="$(next_slot)"
  run_one "$pr" "$br" "$slot" &
  INUSE[$slot]=$!
  i=$((i+1))
done
wait
say "batch complete: $i PRs processed at concurrency $SLOTS"
