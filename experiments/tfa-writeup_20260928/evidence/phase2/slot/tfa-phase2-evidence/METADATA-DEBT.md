# Slot metadata debt: tfa-phase2-cyber-opus

- Host: devbig030 (hostname devbig030). Registry shard: devbig014.
- Cause: tracked parent `.wrkslots.yml` pins `"machine": "devbig014"`; `wrkslots register`
  without `--machine` wrote the row to `ACTIVE.devbig014.json` (event 2229) on 2026-09-24.
- `wrkslots --machine devbig030 import-existing ... --apply` refused, rc=3:
  `REFUSED: slot 'tfa-phase2-cyber-opus' is already active on devbig014` (no state written).
- Coordinator decision (2026-09-24): keep the devbig014 row; do not tear down or duplicate
  only to relabel the shard. Owner/coordinator PID 2748938 is a devbig030 PID recorded in
  the devbig014 shard; liveness probes run from devbig014 cannot observe it.
- Slot git worktree: branch tfa/phase2-cyber-opus-20260924 at 4f7874bc1 (not used for commits).
- Nested Eden fbsource checkout: ignored/fbsource (filteredhg, cloned from the /tmp mount,
  same backing repo /data/users/newton/.eden-backing-repos/fbsource), checked out ef4885b6.
  Removal of this slot must `eden rm` that mount first.
