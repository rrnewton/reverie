# `slot10`: refused, and this one is a designed permanent dead end

**Attempted:** 2026-08-20T19:41:18Z, on `devbig030`, by `hermit-902` (`--actor hermit-902`).
**Outcome: REFUSED, rc=1. Stopped. Nothing removed. 3.5 GB intact.**
**State change caused: `status` `active` → `release-requested`, `lease.state` `active` →
`draining`.** Not quarantined — see below.

Companion to `claude-exitgate-refused-rebase-merge-breaks-durability.md`; the two refusals
are the same family.

## The bytes — the cleanest of the four slots

| | |
| --- | --- |
| contents | `hermit/` only |
| total disk | **3.5 GB** |
| `target/` | **3.4 GB, 8030 files** — build output, reproducible |
| `ignored/` | **does not exist** |
| working tree | clean, 0 paths |
| live processes | 0 |

**A reclaim here destroys 3.4 GB of build output and nothing else.** There is not one
non-reproducible byte in the slot — no `ignored/`, no untracked files, no stash. This is a
stronger case than `pollrevents` or `revents-measure`, both of which were released.

Recovery SHA, re-confirmed: `27526f68f4d7ce526a6f5f7749d28c2252aee629` on
`origin refs/heads/fix/bump-reverie-pin-ee6716a` (and `refs/pull/2281/head`). PR #2281 is
`state: closed, merged: false` — closed unmerged, consistent with the pin having advanced
independently to `af82f1b9`. The change did not land, but the **commit is preserved as a
normal branch on origin**, which is the durable form.

## The half I predicted, and it was right

Before running, I replicated `remote_durable()` against origin's 1254 `refs/heads/*` tips
and predicted `origin_durable: true`. The recorded handoff confirms it:

```json
{"branch": "fix/bump-reverie-pin-ee6716a", "head": "27526f68f4d7ce52…",
 "clean": true, "origin_durable": true}
…
"complete": true
```

**`prepare_stale_clear` succeeded.** The durability proof passed, the handoff completed,
and the lease advanced to `draining` with `status=release-requested`. That is a legitimate
intermediate state, not a failure, and — unlike `claude-exitgate` — **it does not
quarantine and does not block a retry.**

## The half I missed

```
release-worktree: live Codex coordinator lease refused cleanup:
  Codex slot sentinel verify refused: codex-slot-sentinel: sentinel InvocationID changed
  for dev-hermit-slot-slot10-e9f22a6a4bc5aa2a.service:
  expected 1f8c8d708f1342358df595b8f563c894, got
```

**`slot10` is the codex-sentinel slot.** Its `owner_sidecar.source` is
`codex-systemd-sentinel-v1`, not `orc-tmux-pane-supervisor-v1` like the other three, so it
routes through an entirely different release authority — the sentinel branch of
`verify_release_authority_absent`.

State on the row:

```
coordinator_lease            unit          dev-hermit-slot-slot10-e9f22a6a4bc5aa2a.service
                             invocation_id 1f8c8d708f1342358df595b8f563c894
                             main_pid      1750257   main_pid_starttime 39103318
coordinator_lease_revocation (none)
```

and the unit itself:

```
LoadState=not-found   ActiveState=inactive   InvocationID=   (empty)
```

The unit died with the 2026-08-16 reboot and was never recreated. The verifier compares the
recorded `InvocationID` against the live one, gets nothing, and refuses.

**I should have predicted this.** `owner_sidecar.source` was `codex-systemd-sentinel-v1` in
the very first registry dump I took today, and I wrote up this exact branch this morning in
`decision-codex-sentinel-alive-stale-clear-asymmetry_20260820.md`, where I noted *"nothing
currently exercises it … its behaviour is inferred from the source rather than observed."*
I checked the durability predicate and did not check the source. The branch is now
observed, which is worth something — but the prediction I gave was incomplete.

## Why this is permanent, by design

`scripts/codex-slot-sentinel.rs --help`, verbatim:

> `revoke` stops only the exact recorded systemd incarnation. `--recover` resumes an
> already-journaled revocation; **it never turns an absent pre-journal unit into release
> authority.**

`slot10` is exactly *absent* and *pre-journal*: the unit is gone and
`coordinator_lease_revocation` does not exist. **So there is no path through this tooling
that grants release authority for this slot.** That is deliberate and the reasoning is
sound — a unit that vanished without a journaled revocation cannot be proven stopped rather
than still running elsewhere, so authority must be withheld.

But after a host reboot, *every* codex-sentinel slot is in precisely that state. This is a
third standing accumulation mechanism, alongside the unassessable-owner one and the
rebase-merge one, and it points the same way: rows enter the pool and cannot leave it.

## Options, none taken

1. **Leave it.** Costs 3.4 GB of rebuildable output. The lease sits at `draining` /
   `release-requested`, which is honest — a release *was* requested and is pending.
2. **Revert the lease to `active`.** Belongs to the lease lifecycle owner; I caused the
   transition and did not undo it.
3. **Remove the directory outside `release-worktree.rs`.** Explicitly *not* recommended and
   not done: bypassing the allocator is the failure mode every gate here exists to prevent,
   and 3.4 GB is not worth it.
4. **Teach the sentinel path to distinguish "unit absent because the host rebooted" from
   "unit absent, might be live elsewhere"** — e.g. by comparing the recorded `boot_id`, the
   same evidence `slot-owner-authority.py` already uses to answer
   `owner-boot-id-mismatch` for this very slot. A pre-reboot sentinel cannot be running
   now. This is a behaviour change to a refusal and belongs with the owner, like the
   codex-sentinel `alive` asymmetry it sits next to.

**Not done:** no bypass, no un-revoke, no `--force`, nothing removed. The 19 unassessable
slots untouched.
