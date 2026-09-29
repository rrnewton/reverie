# HOLD record: liteinst2-sigtrap-origin-01a0d099

Tracking issue: https://github.com/rrnewton/dev-hermit/issues/393. This file answers the owner's Google Chat
message `3QSxEZA5oY4` ("drive down the worktree slots to exactly match active agents").

It preserves the retention HOLD and every untracked handoff of `/home/newton/work/dev-hermit/worktrees/slots/liteinst2-sigtrap-origin-01a0d099` on devbig014,
written at 2026-09-29T17:52:19Z before the slot was removed. The HOLD file itself stays in `worktrees/`.

## Registry row and liveness

- Registry row: generation 1, agent `codex-liteinst2-sigtrap-01a0d099`, owner PID 3956172, boot `b597126a-0485-436b-b809-4c28c423193d`, start_ticks 236506258.
- Current boot is `47cceadc-9970-4ca3-87b0-90121ca7e88f` (the host booted at 2026-09-26T17:55:31Z), so the owner is not live.
- HOLD issuer: The HOLD was written at 2026-09-24T07:09:28Z by the slot's own agent codex-liteinst2-sigtrap-01a0d099 under coordinator PID 3956172 on boot b597126a. That boot is over: the host rebooted at 2026-09-26T17:55Z, and PID 3956172 does not exist now.
- `herdr agent list` at 2026-09-29T17:52:19Z: coord wJ:p1, kvm wJ:p38, (unnamed) wJ:p3A, liteinst wJ:p3B, gchat-iris wJ:p3X, parity-green wJ:p5B, scorecard wJ:p5C, kvm-readzero-main-claude-final wJ:p5F. None of these lanes is named in the HOLD text.

## Why the HOLD condition is met

The HOLD says to retain the PR 29 artifacts "until explicitly archived". The evidence root the handoff names, target/sigtrap-evidence/, was already gone when the slot was assessed on 2026-09-28 (the directory mtime was 2026-09-26 17:00). The slot has no target/ directory now. The checkout is clean at e216ba0c31177b916081cf526a0dca7201215d74, which is on GitHub, and https://github.com/rrnewton/liteinst2/pull/29 is merged. The separate byte-for-byte PR 29 archive at /home/newton/work/dev-hermit/ignored/liteinst2-pr29-archive-01a0d099 is outside the slot and is not touched by this removal. The CI runs the handoff cites are on GitHub: https://github.com/rrnewton/liteinst2/actions/runs/35967703825 and https://github.com/rrnewton/liteinst2/actions/runs/35968144110.

## HOLD file, verbatim

`/home/newton/work/dev-hermit/worktrees/HOLD.9.devbig014.liteinst2-sigtrap-origin-01a0d099.json` (329 bytes, sha256 `53bce0b0edd39f24b639df510eeb9bb2be58442fef4b1b13e100fe99eb5480c2`):

````json
{
  "held_at": "2026-09-24T07:09:28+00:00",
  "machine": "devbig014",
  "reason": "Retain PR29 bounded reproduction and independent local-review original artifacts, packet and landing receipts until explicitly archived; task preserve_application_sigtrap_delivery.",
  "schema": 1,
  "slot": "liteinst2-sigtrap-origin-01a0d099"
}
````

## Handoff sidecar, verbatim

`/home/newton/work/dev-hermit/worktrees/HANDOFF.9.devbig014.33.liteinst2-sigtrap-origin-01a0d099.1.json` (3415 bytes, sha256 `d1784d8750c2adfa8ed049f2521b0a8ffc5fc07ab68aee13394d099ca1d6c87c`). It stays in `worktrees/`.

````json
{
  "contents_utf8": "LiteInst2 product task COMPLETE and LANDED.\n\nTaskGraph hermit2 preserve_application_sigtrap_delivery is CLOSED. Verified durable completion note 29911 is 6520 bytes, SHA256 098f4669504f81d58bdfb68be70ac14d7c98d9c26e028053cd5550ee53b56b92.\n\nhttps://github.com/rrnewton/liteinst2/pull/29 was bot-authored, independently reviewed by Codex and actual Claude, and landed through ci-hub land-lock plus gh-merge-verified. Exact reviewed head e216ba0c31177b916081cf526a0dca7201215d74, merge/current main 2b200f221eae14bf3a7cdd28bb846ec98a796ca7, complete shared tree 42180852cfc736bc64e32366e6193a825a4b6df2. Both changed files and complete tree were independently verified on fetched main; merged state and bot actor read back from GitHub.\n\nThe fix preserves ordinary application SIGTRAP at inactive registered split sites while retaining genuine delayed temporary-guard retry. Two bounded native-versus-bound tests fail on the exact untouched base and pass with the implementation. Existing real-guard tests and all validation gates remain strict. The host runtime contract now documents native signal metadata preservation.\n\nFinal-head hosted CI passed https://github.com/rrnewton/liteinst2/actions/runs/35967703825. Postlanding push CI passed https://github.com/rrnewton/liteinst2/actions/runs/35968144110 on exact main, all seven required steps. Original local preflight refusal and full-attempt package-consumer FAIL remain preserved; a distinct correct external-package build passed. Nothing was relabelled.\n\nRegistered generation-1 slot liteinst2-sigtrap-origin-01a0d099, agent codex-liteinst2-sigtrap-01a0d099, owner/coordinator PID 3956172. Branch codex/liteinst2-sigtrap-origin-01a0d099 remains clean at reviewed head. The remote branch was retained. A supported wrkslots retention hold protects original local-review evidence; do not retire/remove this slot until that evidence is explicitly archived and the hold is released through supported authority.\n\nEvidence root: /home/newton/work/dev-hermit/worktrees/slots/liteinst2-sigtrap-origin-01a0d099/target/sigtrap-evidence/. Start at coordinator/LANDING.md and coordinator/landing-note-readback.json. Preserve worker/, validation/, validation-final/, validation-main/, native-review/, native-review-final/, claude-review/, claude-review-final/, and coordinator/ local-review packet, admission, terminal receipt, and independent landing proof.\n\nDo not resume this completed task or rerun its passing checks. The earlier https://github.com/rrnewton/reverie/pull/634 and repair_liteinst_optimized_instruction lane remain preserved separately behind the shared KVM prerequisite. No Reverie/Hermit pins, KVM source, devbig030 work, old slots, or superseded pending artifacts were changed. The global active-work snapshot had unrelated stale/registry inconsistencies; no fleet repair or invented landing gate was applied.\n",
  "generation": 1,
  "machine": "devbig014",
  "recorded_at": "2026-09-24T07:13:03+00:00",
  "schema": 2,
  "sha256": "10ae1843c929b6b8f1c93bfc3efea5e3ac185172b19417f6f7523ca29aa8c108",
  "slot": "liteinst2-sigtrap-origin-01a0d099",
  "source": "write-handoff",
  "source_identity": {
    "identity": [
      49,
      540056332
    ],
    "sha256": "10ae1843c929b6b8f1c93bfc3efea5e3ac185172b19417f6f7523ca29aa8c108",
    "size": 2871
  },
  "source_path": "/tmp/liteinst2-sigtrap-handoff-01a0d099.md"
}
````
