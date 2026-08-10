# Worktree Artifact Budget Policy

Status: adopted 2026-08-03 (hermit-coord). Task: `worktree-artifact-budget-policy`.

## Problem

`worktrees/` blows the 200 GB cap **structurally**, not transiently. A single
heavy backend lane holds hundreds of GB of Rust build output, so periodic
cleanup passes only buy hours before the next few concurrent lanes refill it.

### Ground truth (2026-08-03, this machine)

- Filesystem: **btrfs** on `/dev/md0`, 3.5T total, **2.4T free (32% used)**. There
  is no disk-full emergency; the 200 GB cap is a **du/governance budget**, and
  `du` overstates real footprint.
- `compsize worktrees/`: **137 GB real on disk** (386 GB uncompressed, 474 GB
  referenced) vs **451 GB `du`** — `du` overstates ~3.3x from zstd compression +
  reflink sharing. Budget accounting must name which number it means.
- Structural dominators (du): `sabre` **302 GB** (hermit/target 249 GB =
  118 debug + 131 release; reverie/target 36 GB), then `landuuid` 60, `ci` 40,
  `247` 38.
- **Disposable `target/*/incremental/` caches ≈ 77 GB du** across slots
  (sabre alone 58 GB) — pure regenerable cargo state.
- Every heavy slot keeps **both** full `debug` and `release` profiles.
- `allocate-worktree.rs` does **not** reflink-seed `target/`; each slot builds a
  fully independent tree, so nothing is shared after allocation.

## Options considered

### (a) Shared/centralized cargo target dir, or sccache — REJECTED / DEFERRED

- **Shared writable `target/` per backend** directly violates Hard Invariant 8
  ("no shared writable build dirs between worktrees"). It is also genuinely
  unsafe: cargo's incremental state and fingerprints are keyed to one source
  tree; two worktrees with different sources sharing one `target/` corrupt each
  other's incremental caches and thrash the `.cargo-lock`. Rejected.
- **Per-backend shared target with locking** serializes otherwise-parallel lanes
  (one global build lock per backend) and still corrupts incremental state across
  divergent sources. Rejected.
- **sccache as a read-through compile cache** (`RUSTC_WRAPPER=sccache`, per-slot
  `target/` retained) does *not* violate #68 — target dirs stay per-slot and
  single-writer; sccache's store is content-addressed and safe for concurrent
  read/write. **But it is the wrong tool for a disk problem:** sccache shrinks
  recompilation *CPU*, not `target/` *size* — final artifacts, linked deps, and
  incremental state still live per-slot. It does nothing for the cap. Deferred as
  an orthogonal build-speed optimization, not part of this fix.

### (b) Auto-reclaim a slot when its agent is recycled/closed — ADOPTED

`release-worktree.rs --clean` already refuses to remove a slot with uncommitted
work and preserves feature branches (`git worktree remove` drops only the
checkout; the branch ref survives in the primary). The one gap: it does not
detect **committed-but-unpushed** commits (e.g. sabre carries 11 unpushed commits
on `codex/sabre-nongated-parity-frontier`). If the local primary is ever pruned,
that is the only copy. The safe protocol is **push-then-remove**: reclaim is fully
recoverable only once every slot branch is on `origin`.

### (c) Hard per-slot artifact budget + stale-target pruning — ADOPTED

A single tool that measures per-slot and total du (plus real via compsize),
reports against a configurable global cap and per-slot budget, and prunes in a
strict safety-ordered escalation, never touching unrecoverable work.

## Recommendation

Adopt **(b) + (c)** as one policy, reject **(a)**. Ship one governance/GC tool
(`scripts/worktree-gc.sh`) plus a safety hardening of `release-worktree.rs`.

### Prune tiers (strict safety order)

1. **incremental (always regenerable):** `target/*/incremental/` in any slot that
   is not actively building. Never tracked content → safe regardless of
   dirty/unpushed. ~77 GB du recoverable now.
2. **idle full target:** entire `target/` of slots that are `released`/parked AND
   clean AND fully pushed AND not building. Cargo rebuilds on next use.
3. **reclaim:** `released` slots that are clean + pushed → `release-worktree
   --clean` (push-then-remove).

### Hard safety rules (never discard unpushed work)

- A slot is **busy** if any `cargo|rustc|cc1|cc1plus|ld|make` process has a cwd
  under it (`/proc/PID/cwd`). Busy slots are skipped by every tier.
- Tiers 2 and 3 require the worktree **clean** (`git status --porcelain` empty)
  **and pushed** (`git rev-list @{upstream}..HEAD` == 0, upstream must exist).
- Tier 1 only ever deletes `incremental/`, which is never source or commit data.
- Default is **dry-run report**; deletion requires explicit action flags.

## Implementation

- `scripts/worktree-gc.sh` — measure + report + tiered prune/reclaim. Cron-safe.
- `scripts/release-worktree.rs` — added `--push` (with-proxy push of unpushed
  slot branches before removal) and an unpushed-commit gate so `--clean` refuses
  to reclaim a slot with unpushed commits unless `--push` or `--force` is given.

### Operating recipe

```bash
# Report only (default): per-slot du, real footprint, cap status.
scripts/worktree-gc.sh

# Always-safe reclaim of regenerable incremental caches:
scripts/worktree-gc.sh --prune-incremental

# Full enforcement toward the cap (escalates tiers, still honors safety gates):
scripts/worktree-gc.sh --enforce --cap-gb 200 --slot-budget-gb 60
```

Suggested cron (hourly report; daily safe incremental prune) once validated.
