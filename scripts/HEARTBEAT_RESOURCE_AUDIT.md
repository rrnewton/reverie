# Heartbeat Resource Audit

Early-warning system against the OOMD kill / disk-exhaustion / zombie-buildup /
runaway-fan-out / stale-eden-mount failure modes that have taken the dev box
down. Add this to every heartbeat / fleet-monitor cycle.

## Scripts

| Script | Role | Mutating? |
| --- | --- | --- |
| `scripts/resource_audit.sh` | Read-only audit: cgroup memory/swap/PSI, cpu PSI + load, disk, user process count, heavy build fan-out, concurrent Rust validation runs, zombies, stale codesync eden mounts. | No — never kills or unmounts. |
| `scripts/cleanup_stale_eden.sh` | Reclaims stale `/tmp/codesync-*` / `/data/tmpvol/codesync-*` eden clones via `edenfsctl remove`. Protects `~/work/orc-dev/fbsource*`. | Only with `--apply`; dry-run by default. |

Both live in the parent harness (`scripts/`) and are plain bash — no compile
step, so they are cheap to run on a 60s cadence.

## Heartbeat integration contract

Run the audit once per heartbeat cycle. It is the canonical check for the
PROJECT_VISION "Operations and Regulation → Monitor resources" directive.

```bash
# In the heartbeat / fleet-monitor cycle (from ~/work/dev-hermit):
scripts/resource_audit.sh --quiet
rc=$?
# rc: 0 = all OK, 1 = WARN present, 2 = CRIT present.
```

Escalation ladder keyed on the exit code:

- **rc == 0** — log nothing (or one OK line); continue.
- **rc == 1 (WARN)** — surface the printed WARN lines in the heartbeat summary.
  If the `eden` line reports stale codesync mounts, run the cleanup **dry-run**
  and include its plan:
  ```bash
  scripts/cleanup_stale_eden.sh            # dry-run, shows the plan
  ```
- **rc == 2 (CRIT)** — treat as an operator-health incident (this is the OOMD
  precursor). Post it to the coordinator, throttle new local `scripts/validate.rs` /
  experiment launches, and reclaim stale eden clones:
  ```bash
  scripts/cleanup_stale_eden.sh --apply    # remove stale codesync clones
  ```
  Memory CRIT with few stale mounts means live agents/builds are the cause —
  reduce fleet fan-out rather than deleting mounts.

For machine-readable heartbeat state, use `--json` (one object, same exit code):

```bash
scripts/resource_audit.sh --json
# {"timestamp":"…","status":"CRIT","memory_pct":99,"swap_gb":42,"zombies":21,
#  "stale_eden_mounts":4, …}
```

## Thresholds

Defaults are tuned for the safe-dev-limits cap (`user-<uid>.slice` at 80% RAM /
90% CPU). Memory is reported as a percentage **of that cap**, so 100% means the
slice is at its hard limit and the next allocation risks an OOMD kill. Override
any threshold via environment variable — see the `CONFIG` block at the top of
`resource_audit.sh` (e.g. `MEM_CRIT=95 scripts/resource_audit.sh`).

## Notes

- Zombies cannot be reaped without signalling their parents; the audit only
  reports counts + top offenders so the operator can restart the guilty parent.
- `cleanup_stale_eden.sh` only ever touches paths matching `codesync-*` under
  `/tmp` or `/data/tmpvol` and refuses anything under `work/orc-dev`, so it can
  be wired into the CRIT branch without risking the primary fbsource checkouts.
- The heartbeat/fleet-monitor workflow definitions live in the ORC engine
  (`fbcode/orc`), not in this repo; this document is the integration seam the
  coordinator agent follows each cycle.
