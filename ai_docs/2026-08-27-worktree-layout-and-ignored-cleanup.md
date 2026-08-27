# Worktree layout and `ignored/` checkout cleanup — 2026-08-27

Task: `restructure-worktrees-into-slots-and-validate-and-empty-ignored`

## Result

- Agent slots now live under `worktrees/slots/`.
- Disposable validation checkouts and their private Cargo homes now live under
  `worktrees/validate/`. Validation logs, run records, and recovered JSONL
  receipts remain under `ignored/validate/`.
- The machine-local registry was migrated atomically: 528 exact legacy path
  cells were rewritten, and zero `worktrees/<slot>/<product>` cells remain.
- No agent-created Git checkout remains under `ignored/`. A full filesystem
  scan found one checkout there after cleanup:
  `ignored/ci-hub/ledger-publisher-clone`, the clean detached checkout created
  by `ci-hub/ledger/publish_backlog.py` for ledger publication. It was retained
  because it is infrastructure-managed rather than agent-created.
- `ignored/ci-hub/obligations/` was removed after the obligation workflow and
  its writers were deleted from `main` at
  `d5673dfb362bc3530d6095799fc8ca46b34b14e0`.
- Durable logs, ledgers, and non-checkout artifacts remain under `ignored/` by
  design. This cleanup removed agent-created checkouts, not their external
  evidence.

## Slots moved

No slot root was deleted. These 24 existing roots were moved into
`worktrees/slots/`:

| Previous path | Current path |
| --- | --- |
| `worktrees/106` | `worktrees/slots/106` |
| `slot120` | `worktrees/slots/120` |
| `worktrees/121` | `worktrees/slots/121` |
| `worktrees/122-goal5` | `worktrees/slots/122` |
| `worktrees/hermit-123-storage-live` | `worktrees/slots/123` |
| `worktrees/hermit-124-pmu-skid-correction` | `worktrees/slots/124` |
| `w125-classify` | `worktrees/slots/125` |
| `worktrees/hermit-126-sabre-divergences` | `worktrees/slots/126` |
| `worktrees/hermit-127-cpu-timeout` | `worktrees/slots/127` |
| `worktrees/hermit-128` | `worktrees/slots/128` |
| `worktrees/hermit-129-retry-divergence` | `worktrees/slots/129` |
| `worktrees/130` | `worktrees/slots/130` |
| `worktrees/hermit-131-series-collapse-land` | `worktrees/slots/131` |
| `worktrees/slot132-kvm-stdin-hermit` | `worktrees/slots/132` |
| `worktrees/hermit-133-pr2694` | `worktrees/slots/133` |
| `/home/newton/temp/dev-hermit/hermit-134-safe-wrapper-slot` | `worktrees/slots/134` |
| `worktrees/136` | `worktrees/slots/136` |
| `worktrees/h137retry` | `worktrees/slots/137` |
| `worktrees/138` | `worktrees/slots/138` |
| `worktrees/139` | `worktrees/slots/139` |
| `worktrees/140` | `worktrees/slots/140` |
| `worktrees/141` | `worktrees/slots/141` |
| `worktrees/142` | `worktrees/slots/142` |
| `worktrees/review411` | `worktrees/slots/144` |

After moving a live slot was found to invalidate its session hook paths, all
further slot moves stopped. Compatibility paths were restored for active or
explicitly protected sessions. Slots 140 and 141 retained their unread handoff
files byte-for-byte; their SHA-256 digests are respectively
`597cc665aa84c53e29e5109224f2080aa5ad0c377d293b9dc29296a987a37ae8` and
`78a05330c0ce09f4918dffad09906a3b6611d99053e4c88769473aac7b71fef7`.

## Salvage before removal

The cleanup removed 232 registered worktrees under `ignored/` and eight
standalone Git checkout roots only after preserving their local state. The
salvage pull requests are:

- dev-hermit: https://github.com/rrnewton/dev-hermit/pull/199 through
  https://github.com/rrnewton/dev-hermit/pull/204
- Hermit: https://github.com/rrnewton/hermit/pull/2756 through
  https://github.com/rrnewton/hermit/pull/2784
- agent-utils: https://github.com/rrnewton/agent-utils/pull/99

Each is a draft with the `salvage` label. Linux commits
`f6a6c280059c4ddc23e12e3de1b01098e240036f` and
`9786531399a679fc2f4630d2c0a186205282ab2f` were verified through the GitHub API
as already present in `rrnewton/linux`; their local checkout had broken
alternates and could not push.

The standalone rescue is
https://github.com/rrnewton/dev-hermit/pull/204 at commit `2a7be82a8`. It stores
the exact heads, remotes, status, compressed binary diffs, untracked archives,
and full-index patches for the remaining independent checkouts before their
removal.

That same draft pull request was extended through commit
`c0da87aae919587eb2bd82bde295f1ef618fcbb7` to preserve the legacy flat
worktrees. The scan covered 356 Git checkouts: 30 had working-tree changes, 39
had a `HEAD` not contained by a local remote-tracking ref, and 64 had at least
one condition. The rescue includes exact status and head records, binary
working-tree patches, untracked archives, Git bundles or full-index patch
fallbacks, checksums, and an archive of 150 `HANDOFF.md` files.

After the salvage branch was verified remotely, 351 checkouts in 180 legacy
flat roots were removed, reclaiming 221,792,712,940 bytes (about 206.6 GiB).
The associated stale Git worktree administration was pruned; a dry run then
reported zero stale entries for dev-hermit, Hermit, Reverie, LiteInst2, and
agent-utils. Three flat roots were deliberately retained:

- `worktrees/140`, owned by the live `hermit-145` session;
- `worktrees/147`, associated with the live `hermit-147` session; and
- `worktrees/hermit-147-parent-current`, associated with that same session.

The second state scan proved this exclusion was necessary:
`worktrees/147/hermit` advanced from
`4cb13dd8cfeaa67e303d758bf42dd711d6c8959a` to
`146ee68fdd2a079882e90e9a5a6d78b844e7f3b9` while the salvage was uploading.
No other one of the 356 checkouts changed, and a six-surface `/proc` scan found
no process-held reference to any deletion candidate immediately before
removal.

## Verification

- `ci-hub/validate/tests/test_start_unit.py`: 53 passed, 4 parameterized cases.
- Aggregate and stranded-work tests: 52 passed.
- Agent identity, slot parity, and disk-residue tests: 79 passed.
- Sentinel, GitHub attribution, liveness, and stranded-work tests: 109 passed,
  28 parameterized cases.
- Allocator, registry, release-scope, and layout-migration tests passed.
- Full release test: 70 fixtures passed.
- Orphan-release review exposed and fixed a pre-existing ordering bug: ordinary
  build-output cleanup could mutate a fenced orphan-residue transaction before
  its manifest was verified. The unchanged four-file/three-copied assertion now
  passes, including the unknown-file refusal and crash-recovery cases.
