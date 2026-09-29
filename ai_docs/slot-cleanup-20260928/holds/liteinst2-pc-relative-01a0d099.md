# HOLD record: liteinst2-pc-relative-01a0d099

Tracking issue: https://github.com/rrnewton/dev-hermit/issues/393. This file answers the owner's Google Chat
message `3QSxEZA5oY4` ("drive down the worktree slots to exactly match active agents").

It preserves the retention HOLD and every untracked handoff of `/home/newton/work/dev-hermit/worktrees/slots/liteinst2-pc-relative-01a0d099` on devbig014,
written at 2026-09-29T17:52:19Z before the slot was removed. The HOLD file itself stays in `worktrees/`.

## Registry row and liveness

- Registry row: generation 1, agent `codex-liteinst2-pcrel-01a0d099`, owner PID 3956172, boot `b597126a-0485-436b-b809-4c28c423193d`, start_ticks 236506258.
- Current boot is `47cceadc-9970-4ca3-87b0-90121ca7e88f` (the host booted at 2026-09-26T17:55:31Z), so the owner is not live.
- HOLD issuer: The HOLD was written at 2026-09-24T07:41:04Z by the slot's own agent codex-liteinst2-pcrel-01a0d099 under coordinator PID 3956172 (start_ticks 236506258) on boot b597126a. That boot is over: the host rebooted at 2026-09-26T17:55Z, and PID 3956172 does not exist now.
- `herdr agent list` at 2026-09-29T17:52:19Z: coord wJ:p1, kvm wJ:p38, (unnamed) wJ:p3A, liteinst wJ:p3B, gchat-iris wJ:p3X, parity-green wJ:p5B, scorecard wJ:p5C, kvm-readzero-main-claude-final wJ:p5F. None of these lanes is named in the HOLD text.

## Why the HOLD condition is met

The HOLD protects the PR 30 review and landing evidence that the handoff places under target/pcrel-evidence/. That directory was already gone when the slot was assessed on 2026-09-28, and the slot has no target/ directory at all now. The checkout is clean at daa3de2bc19e35128939122de5f18913133c36b2, which is on GitHub, and https://github.com/rrnewton/liteinst2/pull/30 is merged (squash 2032b49d983f74ec8472761d7704c64c00085a55). Removing the slot deletes nothing that the HOLD still protects. The CI runs the handoff cites are on GitHub: https://github.com/rrnewton/liteinst2/actions/runs/35970713065 and https://github.com/rrnewton/liteinst2/actions/runs/35971754178.

## HOLD file, verbatim

`/home/newton/work/dev-hermit/worktrees/HOLD.9.devbig014.liteinst2-pc-relative-01a0d099.json` (358 bytes, sha256 `76d32ada8af574d64a855652ba48da66ca68cb14070c83de5c8b5154bce10474`):

````json
{
  "held_at": "2026-09-24T07:41:04+00:00",
  "machine": "devbig014",
  "reason": "Preserve exact-head independent review, original completion provenance, bounded product reproduction and landing receipt evidence for https://github.com/rrnewton/liteinst2/pull/30; do not remove while referenced.",
  "schema": 1,
  "slot": "liteinst2-pc-relative-01a0d099"
}
````

## Handoff sidecar, verbatim

`/home/newton/work/dev-hermit/worktrees/HANDOFF.9.devbig014.30.liteinst2-pc-relative-01a0d099.1.json` (5452 bytes, sha256 `ffdca9b6cf909f1aa91afdca430344392428c36da35d0216e53eb5f5c375f63f`). It stays in `worktrees/`.

````json
{
  "contents_utf8": "LiteInst PC-relative address preservation is landed.\n\nTaskGraph hermit2 task preserve_liteinst_pc_relative; task closure and complete evidence are recorded in its verified final note. PR https://github.com/rrnewton/liteinst2/pull/30 was authored and merged by rrnewton-bots. Supported landing completed 2026-09-24 07:49:47 UTC, followed by normal main CI success.\n\nRegistered destination: /home/newton/work/dev-hermit/worktrees/slots/liteinst2-pc-relative-01a0d099, generation 1, agent codex-liteinst2-pcrel-01a0d099, owner/coordinator PID 3956172 (start_ticks 236506258). Branch codex/liteinst2-pc-relative-01a0d099 remains clean at daa3de2bc19e35128939122de5f18913133c36b2. Base 2b200f221eae14bf3a7cdd28bb846ec98a796ca7. Squash merge 2032b49d983f74ec8472761d7704c64c00085a55 is on origin/main. Complete tree aa899929d4236bfd504969c9fc18daa368009930 is identical between candidate and landed commit. Exactly src/trampoline.rs and tests/pc_relative.rs changed; both source blobs were independently compared against fetched main and the GitHub PR file set.\n\nProduct defect: observing a self-address LEA changed the returned application pointer to its relocated trampoline address. Bounded untouched-base execution reproduced the exact pointer error; the fix separates iced encoder labels from RIP/EIP memory addresses while preserving genuine direct branches and original logical PCs. The live four-phase test now returns the original address with exactly one callback. Far unrepresentable original addresses fail explicitly; that negative case is not substituted for live success. Existing tests and gates are unchanged. The separate observing-entry-backedge callback-loss defect remains unresolved and outside this change.\n\nOriginal evidence lives under target/pcrel-evidence/:\n- worker/: untouched-base failure, candidate success, exact source/test binaries, raw logs/statuses, 400 emission combinations, far/branch/PC mapping coverage and retained failed compiler/formatting attempts.\n- validation/: one complete unchanged local Rust 1.85 workflow, 132 passed per profile, package/external consumer, full default stress and exact million callbacks; 57.002 seconds, no retry/bound failure, cleanup complete.\n- native-review/: independent Codex exact-head report, triggers and schema-1 completion. Report SHA256 437df4593f5dcba868e0423151b26e5f72833c888d4aecf7e203ef3e64e0ffc8. Completion SHA256 0a8d58f3ee3730576d620bb9a167cd1baf473c1e4cc3913d0b944ae691ab1d80.\n- claude-review/: actual independent Claude UUID 40e2d501-e6d4-4fd8-a500-e8988855dc89, successful original stream/native completion and exact-head approval. Report SHA256 5dbd41ca72c84a36414f1166f28bd1e6496f1584e0023ab2743feef8f661da40. No blocking findings; limitations and non-blocking observations retained.\n- hosted-ci/: successful PR run https://github.com/rrnewton/liteinst2/actions/runs/35970713065 and raw logs.\n- main-ci/: successful normal push run https://github.com/rrnewton/liteinst2/actions/runs/35971754178 at exact merge commit and raw terminal evidence.\n- coordinator/: original observed Codex completion, exact local-review packet, immutable admission/terminal landing receipts, independent landed-content proof, verified TaskGraph notes and identity manifests.\n\nSupported land-lock plus gh-merge-verified returned 0; both changed files arrived, none absent or moved on; lock released with complete cleanup. Terminal landing receipt SHA256 2f8fc48dee47a4e096d14b814a3884872e95581eb66d75e53cbb99e58dc72a62; admission SHA256 f438787393b11f55d5e9614a02f200263ff61389f946b2f5c4e438d2b10228fd. Native/local source review is an ordinary review relay, not a fabricated public GitHub review or signed provider attestation. Claude independently supplied the second review family.\n\nA retention hold protects this slot because original evidence is referenced by absolute path in the admission/receipt. Reading this handoff does not authorize removal or release that hold. Preserve evidence, prove owner liveness, and use the supported lifecycle before any future retirement. Do not reset the feature branch merely to fabricate squash ancestry.\n\nOther lanes remain untouched. Preserve blocked https://github.com/rrnewton/reverie/pull/634, task repair_liteinst_optimized_instruction, original slot codex-liteinst-product-01a0d099, canonical run failures and all KVM comparison evidence. Do not create the superseded alternate slot. No Hermit/KVM invocation or devbig030 work occurred.\n\nCompleted https://github.com/rrnewton/liteinst2/pull/29 evidence is archived byte-for-byte under /home/newton/work/dev-hermit/ignored/liteinst2-pr29-archive-01a0d099, including source.bundle and manifest. Its old SIGTRAP slot remains held, owner-live and retirement-queued. Supported finish refused because ignored target evidence is present; this is recorded in verified note 29912. No unsafe removal, hold release or branch mutation was attempted.\n",
  "generation": 1,
  "machine": "devbig014",
  "recorded_at": "2026-09-24T07:55:28+00:00",
  "schema": 2,
  "sha256": "1ebf8bf3cebb53e18c583e9b515d4f4efb4cfca9387681ecc5f34043741c75bc",
  "slot": "liteinst2-pc-relative-01a0d099",
  "source": "write-handoff",
  "source_identity": {
    "identity": [
      49,
      540474827
    ],
    "sha256": "1ebf8bf3cebb53e18c583e9b515d4f4efb4cfca9387681ecc5f34043741c75bc",
    "size": 4904
  },
  "source_path": "/tmp/liteinst2-pcrel-handoff-01a0d099.md"
}
````
