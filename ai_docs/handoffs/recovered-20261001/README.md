# Handoff notes recovered from retired agent slots, 2026-09-30 to 2026-10-01

## Background

- **Slot.** An agent slot is a git worktree under
  `/home/newton/work/dev-hermit/worktrees/slots/<slot>/` on devbig014. One agent owns it, and the
  `wrkslots` tool (`ci-hub/bin/wrkslots`) records it in the host-local registry
  `worktrees/ACTIVE.devbig014.json`.
- **Handoff.** When an agent releases a slot, it writes a short note: which branch and commit hold
  the work, where that work lives on GitHub, and what is left to do. Current agents store the note
  with `wrkslots write-handoff`, which puts it in a generation-bound control sidecar,
  `worktrees/HANDOFF.9.devbig014.<n>.<slot>.<generation>.json` (field `contents_utf8`). Older agents
  wrote a `HANDOFF.md` inside the checkout instead (a "legacy file"); wrkslots copies that into a
  sidecar when a coordinator reads it. A coordinator reads the note with `wrkslots read-handoff`
  before `wrkslots remove`. On removal the sidecar is renamed to
  `worktrees/HANDOFF-RETIRED.<...>.<sha256>.<...>.json`, which exists only on that host.
- **Why these were recovered.** The owner's Google Chat directive of 2026-09-29T21:05Z
  (`spaces/AAQAWBA0g6A/messages/DChXpyYcvrM.DChXpyYcvrM`) asked the coordinator to "KEEP DRIVING TO A
  CLEAN POSITION ... Keep a tight set of worktrees (matching live agents)." The coordinator then
  retired slots whose owning agent was dead, in three rounds on 2026-09-30 and 2026-10-01. Rounds 1
  and 2 removed 18 slots. Round 3 removed `cml-hermit-3221`, `liteinst-lane-20260927-expexit` and
  `liteinst-lane-20260927-promote`, the slots round 2 had left behind. Until this commit the
  handoff texts read during those rounds existed only in `/tmp` and in host-local sidecars. This
  directory keeps them in the private dev-hermit repository so they survive the host.

## What is here

Each `<slot>.md` holds the exact bytes of that slot's handoff. Its SHA-256 equals the `sha256` field
of the slot's sidecar and the hash that `wrkslots read-handoff` printed when the coordinator read it.
For the round 1 and round 2 texts, the coordinator's saved `read-handoff` output wrapped the note in
`--- <source path> ---` / `--- end ... ---` lines and a `recorded HANDOFF.md read` line. Those wrapper
lines were removed, and the remaining text was checked against the recorded hash before it was
written here. Two slots were read twice (`cml-hermit-3221` in rounds 2 and 3,
`liteinst-lane-20260927-expexit` in rounds 1 and 3); both reads produced the same bytes, so each
slot has one file.

`kvm-readzero-main-20260929-01a0d5c2.md` also quotes, in full, the older legacy handoffs of
`kvm-carrier-tests-20260929-01a0d5c2`, `kvm-readzero-cancel-20260929-01a0d5c2` and
`kvm-rescue-carrier-01a0d5c2`.

All 16 came from a sidecar written with `wrkslots write-handoff`. None came from a legacy file or
from the event log alone. "Recorded" is the sidecar's `recorded_at` time.

| Slot | Source | SHA-256 | Bytes | Read in |
|---|---|---|---:|---|
| `cml-hermit-3221` | sidecar, `write-handoff` (recorded 2026-09-29T13:55:20Z) | `07595ec4c683d6b5f07c93def8758e6f91cbb78667ec37aa5c17f792249d1593` | 2109 | round 2, round 3 |
| `kvm-child-future-box-recovery-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T16:32:04Z) | `38c63117355724824f0ecaf8dfb51aa863e5e38dc1cfb6c7e02bb11ed867d98c` | 2798 | round 1 |
| `kvm-child-helper-20260930-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T05:38:24Z) | `bcfc9642eafde83b211ead3a7e58790b88762510f183360e8eb9318d837181af` | 2102 | round 1 |
| `kvm-child-wait-process-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T16:31:53Z) | `072499c626da4d30bd1544f727b18b491623c78f8101c62ab4c7959525c0dd1b` | 2770 | round 1 |
| `kvm-clear-tid-696-20260930-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T16:31:41Z) | `8d271bf8769fcae2d603feb36448d218541be2069ffc0af4f564d1182c1871fb` | 2395 | round 1 |
| `kvm-fstat-lowword-recovery-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T16:31:08Z) | `0a36061cdb75b6b67a4a51696971da1fde9be1949ed4c089c051b39f51d901da` | 2251 | round 1 |
| `kvm-readzero-main-20260929-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T13:24:59Z) | `92fdd67ee62208edfb6770991142962d178bd1357506db3ab4cf4b1afbb552d8` | 8028 | round 1 |
| `kvm-recvmsg-flags-20260930-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T13:24:47Z) | `248d98b414251cd65118512e3a74306179c64e31e973e754f6e51220a22b8479` | 1278 | round 1 |
| `kvm-rescue-carrier-01a0d5c2` | sidecar, `write-handoff` (recorded 2026-09-30T03:21:19Z) | `62a4c622b1f868fb0f4fa22f28cee67f41e328fcded2e34786fd29da7443d693` | 2025 | round 1 |
| `liteinst-lane-20260927-childstack` | sidecar, `write-handoff` (recorded 2026-09-30T00:49:03Z) | `a03f29f4dd857f18b2c475b8dbe3c1da31c7402ad3145fe9ea51bcb7d8ef19c5` | 963 | round 1 |
| `liteinst-lane-20260927-expexit` | sidecar, `write-handoff` (recorded 2026-09-29T09:01:13Z) | `9b7219f498f8b7ef00828f5df4592d6740f46ddcca3477e3d7e721073b55a474` | 1077 | round 1, round 3 |
| `liteinst-lane-20260927-inodes` | sidecar, `write-handoff` (recorded 2026-09-29T13:02:07Z) | `c8224d778455586042bc915d981ad1e8e8ee67333d484600ad277097865c13fa` | 1412 | round 1 |
| `liteinst-lane-20260927-promote` | sidecar, `write-handoff` (recorded 2026-09-29T09:02:57Z) | `d7d352451b18426b84e4e327b0f8cc516763cab7cd0f402112d7e77426985e86` | 908 | round 3 |
| `liteinst-lane-20260929-heaps1` | sidecar, `write-handoff` (recorded 2026-09-29T22:21:13Z) | `ec53ae2a5af954d6c92fe82cec7c797eabce4477cb41d9ada6a7007611c20d08` | 802 | round 2 |
| `liteinst-lane-20260929-restart3` | sidecar, `write-handoff` (recorded 2026-09-29T22:30:51Z) | `245ef4208c22c9e695c61e7ed20f47f783fe032961402b83f1ebe29566ff0f54` | 909 | round 2 |
| `liteinst-lane-20260929-so724` | sidecar, `write-handoff` (recorded 2026-09-29T22:15:58Z) | `d4c19694f8286070dd0ea79dc4a8a777106cb547e5b586cbbbbda096db0c8840` | 932 | round 2 |

## Not copied: already tracked byte-identically

Four texts read during these rounds were left out because files with exactly these bytes (the same
git blob) were already tracked in this repository:

- `dev-hermit-gdb-replay-diagnostic-20260916` (`fec9eeed987f54ba5080537805fb6fba3e1215a946c576532267883c747b84fa`, 1306 bytes): `ai_docs/handoffs/recovered-20260916/dev-hermit-gdb-replay-diagnostic-20260916.md`, `ai_docs/slot-cleanup-20260928/handoffs/dev-hermit-gdb-replay-diagnostic-20260916.md`
- `dev-hermit-proc-locks-runtime-20260916` (`8a96fc69d9a73bc0a8ab10d6a0329a922177a536948ef6720d49cdaee2bf8209`, 554 bytes): `ai_docs/handoffs/recovered-20260916/dev-hermit-proc-locks-runtime-20260916.md`, `ai_docs/slot-cleanup-20260928/handoffs/dev-hermit-proc-locks-runtime-20260916.md`
- `dev-hermit-sabre-getrandom-diagnostic-20260917` (`ef57044115abda404d384f3fa8a53fdca768c2e262a738377fdc7a6f6edd58f6`, 1847 bytes): `ai_docs/handoffs/recovered-20260918/dev-hermit-sabre-getrandom-diagnostic-20260917.md`, `ai_docs/slot-cleanup-20260928/handoffs/dev-hermit-sabre-getrandom-diagnostic-20260917.md`
- `lander-2` (`bd9323dc2bed0130aacc3718787208a14bc65d725dbd325ac0976a51a6975256`, 11192 bytes): `ai_docs/slot-cleanup-20260928/handoffs/lander-2.md`
