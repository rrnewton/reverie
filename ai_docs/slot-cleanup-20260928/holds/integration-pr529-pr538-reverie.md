# HOLD record: integration-pr529-pr538-reverie

Tracking issue: https://github.com/rrnewton/dev-hermit/issues/393. This file answers the owner's Google Chat
message `3QSxEZA5oY4` ("drive down the worktree slots to exactly match active agents").

It preserves the retention HOLD and every untracked handoff of `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-reverie` on devbig014,
written at 2026-09-29T17:52:19Z before the slot was removed. The HOLD file itself stays in `worktrees/`.

## Registry row and liveness

- Registry row: generation 1, agent `codex-pr529-pr538-vectored`, owner PID 2152969, boot `b597126a-0485-436b-b809-4c28c423193d`, start_ticks 115349103.
- Current boot is `47cceadc-9970-4ca3-87b0-90121ca7e88f` (the host booted at 2026-09-26T17:55:31Z), so the owner is not live.
- HOLD issuer: The HOLD file names no issuer. It was written at 2026-09-19T00:20:28Z by the same cleanup sweep that held integration-pr529-pr538-hermit and scorecard-publish-sept14, on the previous boot.
- `herdr agent list` at 2026-09-29T17:52:19Z: coord wJ:p1, kvm wJ:p38, (unnamed) wJ:p3A, liteinst wJ:p3B, gchat-iris wJ:p3X, parity-green wJ:p5B, scorecard wJ:p5C, kvm-readzero-main-claude-final wJ:p5F. None of these lanes is named in the HOLD text.

## Why the HOLD condition is met

The HOLD says to release "after these files are preserved elsewhere". All four untracked files are in the rescue WIP commit below and the three HANDOFF documents are reproduced verbatim at the end of this file. The handoff asks to keep the slot until the sibling handoffs are consumed; the sibling slot integration-pr529-pr538-diagnostic was removed on 2026-09-29 after its results were pushed to https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/integration-pr529-pr538-diagnostic--evidence (74067c930985af409f441124113d81c591b3a5b5), and the sibling integration-pr529-pr538-hermit is archived in holds/integration-pr529-pr538-hermit.md. https://github.com/rrnewton/reverie/pull/529 merged on 2026-09-10 and https://github.com/rrnewton/reverie/pull/538 was closed without merging.

## Rescue refs (checked with `git ls-remote` before removal)

- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/integration-pr529-pr538-reverie--wip at `54a0fe04cd85893087be5df5ab1a6f9283855f98`: the 4 untracked files.
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/integration-pr529-pr538-reverie--branch-archive-integration-pr529-pr538-old-04afeec5 at `04afeec5f92a3b56bae1bc6523b1b45a2d87dc8c`: local branch archive/integration-pr529-pr538-old-04afeec5.
- https://github.com/rrnewton/reverie/tree/salvage/integration-pr529-pr538-reverie-root at `d0decf28738521b9ae0a33c4b930c5f3f3d43d27`: the checkout HEAD.

## HOLD file, verbatim

`/home/newton/work/dev-hermit/worktrees/HOLD.9.devbig014.integration-pr529-pr538-reverie.json` (418 bytes, sha256 `d473720b1a53a753c8c5cad2174e1afc73d93dd97693a8777943fbfd08ea7734`):

````json
{
  "held_at": "2026-09-19T00:20:28+00:00",
  "machine": "devbig014",
  "reason": "Holds untracked handoff documents under names the remove guard cannot see (_legacy_handoff_artifact hardcodes HANDOFF.md). Protected pending wrkslots_remove_guards_only. Release only after that guard covers prefixed handoffs, or after these files are preserved elsewhere.",
  "schema": 1,
  "slot": "integration-pr529-pr538-reverie"
}
````

## Untracked handoff files, verbatim

### `HANDOFF.md`

1175 bytes, sha256 `81d1db5ea054cd22f5874305a3d8c545b2bdcd08144cd21fd9140584b0ab81df`.

````markdown
# PR529 + approved PR538 local integration

This worktree is local-only. Nothing was pushed, published, or merged remotely.

- Path: `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-reverie`
- Branch: `codex/integration-pr529-pr538-f8bc-reverie`
- HEAD: `d0decf28738521b9ae0a33c4b930c5f3f3d43d27`
- Tree: `209cf49e90bb83787865e300fb3c56f3add1813d`
- First parent: PR529 `c632c111619cb47922a72235b3e1130b91355603`
- Second parent: approved PR538 repair
  `90ad5b98fa897f03e817d74b1fa66e68f1b758fb`
- PR538 tree: `962f40b6e487e82feb56b0da113f772376cba389`

The merge was conflict-free and preserves both source commits. The superseded
merge with old PR538 head 3646ba2c is preserved on local branch
`archive/integration-pr529-pr538-old-04afeec5` and its handoff is under
`integration-evidence/`.

The exact ignored Hermit fixture and fixed-base 27-cell diagnostic are recorded
in the sibling slots:

- `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/HANDOFF.md`
- `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-diagnostic/HANDOFF.md`

Do not remove this worktree until those handoffs have been consumed.
````

### `integration-evidence/HANDOFF-04afeec5-old-vector-parent.md`

2309 bytes, sha256 `10ef07ee2d5a650bf4fee227c18096a520e7455851c6071de0ab11e645134672`.

````markdown
# Reverie PR529 + PR538 local merge handoff

This registered slot is the local-only dependency checkout for the paired
Hermit integration. Nothing was pushed, published, or merged remotely.

- Slot: `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-reverie`
- Registered name: `integration-pr529-pr538-reverie`
- Branch: `codex/integration-pr529-pr538-f8bc-reverie`
- Common base: `8c8c0a57649c9ffbf8a7a14291a64320f64b935f`
- First parent: https://github.com/rrnewton/reverie/pull/529 head
  `c632c111619cb47922a72235b3e1130b91355603`
- Second parent: https://github.com/rrnewton/reverie/pull/538 head
  `3646ba2c662f65b97e94d39f60852b62610ca5a0`
- Local merge HEAD: `04afeec5f92a3b56bae1bc6523b1b45a2d87dc8c`
- Merge tree: `ea72940de448bb4a0c96da52bb1557b23aeeb6b5`

Neither PR head is an ancestor of the other. PR529 contains two commits after
the common base:

1. `cf60111c` — Release KVM stack checkout after failed commit
2. `c632c111` — Make KVM Tool scratch stacks safe across threads

PR538 contains one commit after the common base:

1. `3646ba2c` — Add KVM positioned vectored I/O

The exact combination command, run from PR529 HEAD, was:

```text
git merge --no-ff 3646ba2c662f65b97e94d39f60852b62610ca5a0 -m 'Local integration: combine Reverie PR 529 and PR 538'
```

Exact result (exit 0):

```text
Auto-merging reverie-kvm/src/executor.rs
Merge made by the 'ort' strategy.
 reverie-kvm/src/executor.rs | 2333 +++++++++++++++++++++++++++++++++++++++++--
 1 file changed, 2227 insertions(+), 106 deletions(-)
```

There were no conflicts. `git show -s --format='%H%n%P%n%T%n%s' HEAD` gives:

```text
04afeec5f92a3b56bae1bc6523b1b45a2d87dc8c
c632c111619cb47922a72235b3e1130b91355603 3646ba2c662f65b97e94d39f60852b62610ca5a0
ea72940de448bb4a0c96da52bb1557b23aeeb6b5
Local integration: combine Reverie PR 529 and PR 538
```

The paired Hermit slot is
`/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit`.
Its `HANDOFF.md` has the build, exact ignored KVM test, ptrace controls,
safehermit run IDs, and failure mechanisms.

Expected untracked files are `.wrkslots-owner-lease.sh` and this `HANDOFF.md`.
The registered owner-lease process was PID 2250816 when this handoff was
written. Do not remove this worktree until the handoff has been consumed.
````

### `integration-evidence/HANDOFF-before-d0decf.md`

2309 bytes, sha256 `10ef07ee2d5a650bf4fee227c18096a520e7455851c6071de0ab11e645134672`.

````markdown
# Reverie PR529 + PR538 local merge handoff

This registered slot is the local-only dependency checkout for the paired
Hermit integration. Nothing was pushed, published, or merged remotely.

- Slot: `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-reverie`
- Registered name: `integration-pr529-pr538-reverie`
- Branch: `codex/integration-pr529-pr538-f8bc-reverie`
- Common base: `8c8c0a57649c9ffbf8a7a14291a64320f64b935f`
- First parent: https://github.com/rrnewton/reverie/pull/529 head
  `c632c111619cb47922a72235b3e1130b91355603`
- Second parent: https://github.com/rrnewton/reverie/pull/538 head
  `3646ba2c662f65b97e94d39f60852b62610ca5a0`
- Local merge HEAD: `04afeec5f92a3b56bae1bc6523b1b45a2d87dc8c`
- Merge tree: `ea72940de448bb4a0c96da52bb1557b23aeeb6b5`

Neither PR head is an ancestor of the other. PR529 contains two commits after
the common base:

1. `cf60111c` — Release KVM stack checkout after failed commit
2. `c632c111` — Make KVM Tool scratch stacks safe across threads

PR538 contains one commit after the common base:

1. `3646ba2c` — Add KVM positioned vectored I/O

The exact combination command, run from PR529 HEAD, was:

```text
git merge --no-ff 3646ba2c662f65b97e94d39f60852b62610ca5a0 -m 'Local integration: combine Reverie PR 529 and PR 538'
```

Exact result (exit 0):

```text
Auto-merging reverie-kvm/src/executor.rs
Merge made by the 'ort' strategy.
 reverie-kvm/src/executor.rs | 2333 +++++++++++++++++++++++++++++++++++++++++--
 1 file changed, 2227 insertions(+), 106 deletions(-)
```

There were no conflicts. `git show -s --format='%H%n%P%n%T%n%s' HEAD` gives:

```text
04afeec5f92a3b56bae1bc6523b1b45a2d87dc8c
c632c111619cb47922a72235b3e1130b91355603 3646ba2c662f65b97e94d39f60852b62610ca5a0
ea72940de448bb4a0c96da52bb1557b23aeeb6b5
Local integration: combine Reverie PR 529 and PR 538
```

The paired Hermit slot is
`/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit`.
Its `HANDOFF.md` has the build, exact ignored KVM test, ptrace controls,
safehermit run IDs, and failure mechanisms.

Expected untracked files are `.wrkslots-owner-lease.sh` and this `HANDOFF.md`.
The registered owner-lease process was PID 2250816 when this handoff was
written. Do not remove this worktree until the handoff has been consumed.
````
