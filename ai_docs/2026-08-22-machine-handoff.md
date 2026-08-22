# Machine handoff — 2026-08-22

## Do not prune the rescue branches

**The 15 `rescue/20260822-*` product branches are durability refs for commits
that otherwise existed only on this machine. Do not delete them in a routine
branch-pruning sweep.**

Git branches carry no creation timestamp. Tooling that estimates branch age
from the tip commit will judge these newly created rescue branches by commits
that are already about a week old and may classify them as stale. The commit
messages describe the original tasks, not the later rescue operation. The
`rescue/20260822-*` prefix is the only provenance marker on the refs themselves.
Deleting a rescue branch without first proving that another durable ref
supersedes it would undo the purpose of the recovery.

The 15 product rescue branches are:

| Repository | Branch | Exact commit |
| --- | --- | --- |
| `rrnewton/hermit` | `rescue/20260822-claude-diag2129-5fb1d502` | `5fb1d502c32288fc9e0fcc901083ea8796badec1` |
| `rrnewton/hermit` | `rescue/20260822-claude-diag2213-b6310e44` | `b6310e44577674bd996ebbda0692e19939fa3335` |
| `rrnewton/hermit` | `rescue/20260822-claude-diag2213b-e323cdb2` | `e323cdb244c76630965b46726f7d2bb9d617be39` |
| `rrnewton/hermit` | `rescue/20260822-claude-diag3way-e741b38a` | `e741b38afe7812885e8f9538bf03ab973e420212` |
| `rrnewton/hermit` | `rescue/20260822-claude-diagmain-d9b3274f` | `d9b3274f7f6811f1ee37705604781f0cc11f96ec` |
| `rrnewton/hermit` | `rescue/20260822-claude-exitgate-d857b6b8` | `d857b6b853ad587f2ab9fbd261c49e8ed61463ba` |
| `rrnewton/hermit` | `rescue/20260822-consistency-be651570` | `be651570b6097e1461167bc443f5d60e49a0a77b` |
| `rrnewton/hermit` | `rescue/20260822-deslop-a4a129fa` | `a4a129fa37422e36e89cb7c2dad06629b4748f41` |
| `rrnewton/hermit` | `rescue/20260822-fix-drcache-f2369073` | `f236907319171f25508a1d329bb87b0609c58cb2` |
| `rrnewton/hermit` | `rescue/20260822-fix-futex-630bae1b` | `630bae1b755e5365ccbde938f851f29ba27be517` |
| `rrnewton/hermit` | `rescue/20260822-fix-liteinst-66138d34` | `66138d34910d325db9c4a95d265aa06a0a4f6aab` |
| `rrnewton/hermit` | `rescue/20260822-fix-qemupanic-a7a4bb90` | `a7a4bb90e2ddb8363414faacfa43a8ccfbe50409` |
| `rrnewton/hermit` | `rescue/20260822-pr2263-901-cc9f6955` | `cc9f69552a02c107dc3acf912cd49242a67c6d98` |
| `rrnewton/hermit` | `rescue/20260822-scorecard-e6c4fe99` | `e6c4fe996ae480ee5108d5e8d136839451d00176` |
| `rrnewton/reverie` | `rescue/20260822-consistency-7eefbd82` | `7eefbd82a0ab952faa0db67fa2239537e1de8f9e` |

The separate parent-repository ref
`rrnewton/dev-hermit:rescue/20260822-parent-working-tree-preservation` is not
part of the count above.

## Recovery pull requests and dispositions

These are preservation or continuation records, not seven equivalent landing
candidates. GitHub had no submitted review objects on these threads when this
handoff was written; the dispositions below are the explicit states recorded
in their PR bodies.

| PR | Exact head | Disposition |
| --- | --- | --- |
| [`rrnewton/hermit#2364`](https://github.com/rrnewton/hermit/pull/2364) | `7f3da389906266b043695e0be0516a74fb7daa4b` | Recovered WIP. No product tests were run during preservation. Reconcile with current main, validate, and review before considering landing. |
| [`rrnewton/hermit#2365`](https://github.com/rrnewton/hermit/pull/2365) | `fd0d6b3734ddfe1c526732c7c63b4860e95bf801` | Incomplete WIP; **do not land**. Its missing-population refusal is disabled under `if false` while the bracket expects refusal. |
| [`rrnewton/hermit#2366`](https://github.com/rrnewton/hermit/pull/2366) | `a6df197cf8ce9954c492812b796d577aa99bf6f3` | Coherent recovered fix, but stale-base and not revalidated. Needs rebase or transplant, exact-head validation, and review. |
| [`rrnewton/hermit#2367`](https://github.com/rrnewton/hermit/pull/2367) | `1ab82aa6dcf4fc73a297f0dfca5adbe36777be28` | **Junk for product landing; close unread.** Historical generated safe-CI profile artifacts only; do not merge. |
| [`rrnewton/hermit#2368`](https://github.com/rrnewton/hermit/pull/2368) | `26276e2b266e944bc02ff775b4ceb5cb0956019e` | Buck2 continuation draft. Reverie builds, but Hermit does not link; no Hermit binary or shared-cache measurement exists. It also changes `rust-toolchain.toml`, so it does not meet the owner's build-files-only immediate-landing criterion. |
| [`rrnewton/reverie#466`](https://github.com/rrnewton/reverie/pull/466) | `8b20d70588c0d77b4f832e16fd8ff8f5cbbc1289` | **Junk for landing; close unread.** Superseded preservation snapshot; #430 and stacked #447 contain stronger implementations. |
| [`rrnewton/reverie#467`](https://github.com/rrnewton/reverie/pull/467) | `d2216e8de8c0dfa8c617cb8884f971b844015196` | Unvalidated recovery work. Reconcile with Reverie #453/#455 and Hermit #2259 before deciding what remains applicable; do not merge as-is. |

## Remaining registered slots

The following 15 rows and physical slot directories remained at the final
census. A detector result is not release authority; use the registered
lifecycle tools and preserve every refusal.

| Slot | Current blocker |
| --- | --- |
| `agg2129` | Release refuses the unexpected slot-root directory `target-pr2129b` (5.3 GiB). It is Cargo output produced by `claude-pr2129b` for task `drain_2129_2166` on August 12, 21:10–21:18 PDT: 3.3 GiB dependencies, 1.9 GiB incremental cache, 148 MiB build-script output, no authored source. Some objects came from temporary mutation builds, so the cache is neither reproducible from the current checkout alone nor independent evidence. No deletion was authorized. |
| `claude-1` | Dead pre-reboot Codex owner, quarantined lease, no revocation journal, and Hermit head `27fdb69928f2b51ad26fc2ed1d2afea0ba624054` is not on a current origin ref. Preserve until durability and authority are separately resolved. |
| `claude-exitgate` | Dead pre-reboot ORC owner and quarantined lease. The exact Hermit head is now durable on `rescue/20260822-claude-exitgate-d857b6b8`, but the recorded handoff predates that rescue and remains incomplete; quarantined stale-clear refuses. |
| `codex-hostsweep907` | Invalid ownership: the registry row has zero mutating owners and is stuck in `binding-coordinator-lease`. Do not fabricate an owner. |
| `codex-lsof908` | Clean, durable, registry-correct; Codex sentinel `InvocationID` changed/empty. Lease is quarantined and no revocation journal exists. |
| `codex-robust904` | Clean, durable, registry-correct; Codex sentinel `InvocationID` changed/empty. Lease is quarantined and no revocation journal exists. |
| `coord` | Coordinator-owned workspace; explicitly left untouched. Its Hermit checkout is on the remotely preserved #2367 branch while the registry still names the historical branch. |
| `fixture-align` | Clean, durable, registry-correct; Codex sentinel `InvocationID` changed/empty. Lease is quarantined and no revocation journal exists. |
| `gchat-crash-902` | Owner process is absent and the coordinator lease is already journaled as revoked. The interrupted cleanup remains to be resumed through the registered `--clean --recover-submodule-cleanup` path; no direct deletion. |
| `lander` | No valid versioned `owner_sidecar`, plus an existing dead finalized coordinator sentinel. Legacy-unbound recovery is inapplicable and current tooling exposes no safe clearing command. |
| `review1` | Current authority reports the exact recorded `hermit-review1` supervisor alive. Leave until that owner exits and the ordinary lifecycle can prove absence. |
| `scorecard` | Current recovery sentinel is alive. Both product heads are durable, but the Reverie registry cell still names `fix/dbt-determinism-parity-cell` while the checkout is on `salvage/scorecard-dirty-20260822`; a targeted dry-run reported one branch cell and `unpublished=0`. |
| `shebang` | Current authority reports the exact recorded `hermit-shebang` supervisor alive. Its clean Hermit head `47692a3a459aba7facaf85026070ddd12511728d` is not on a current origin ref; it was superseded during the landed #2230 sequence, but no patch-identity policy was applied to release it. |
| `slot05` | Release was requested and the old coordinator revocation is journaled, but host-level checks still show recorded owner PID 1002072 alive as `DG_AGENT_NAME=hermit-901`, with active `codex.real` child 1003047. Close that owner before retrying the registered recovery path. |
| `slot10` | Clean and durable, but the pre-reboot Codex sentinel has an empty/changed `InvocationID`. Lease is draining, no revocation journal exists, and exact sentinel verification prevents a journal from being created. |

## Required tooling change for dead Codex sentinels

Do not retry `codex-lsof908`, `codex-robust904`, `fixture-align`, `slot10`, or
`lander` expecting a different result. Their refusal is an intentional
fail-closed boundary, not missing command-line knowledge:

- `scripts/codex-slot-sentinel.rs:442` requires the live systemd unit's
  `InvocationID` to equal the recorded lease.
- `scripts/release-worktree.rs:2738` verifies that exact live sentinel before
  it writes a revocation journal.
- `--recover-submodule-cleanup` resumes an existing journal; none of these five
  has one.
- legacy identity recovery requires an existing slot with no coordinator
  lease; these rows do not satisfy that precondition.
- the three quarantined Codex rows are additionally rejected by
  `--stale-clear` before authority evaluation.

Releasing these five requires a reviewed tooling change that derives new
release authority without weakening the existing content, process, cgroup,
cleanliness, and remote-durability checks. Direct registry edits, recreated
units, `--force`, and repeated ordinary release attempts are not substitutes.

## Unidentified `core.bare=true` writer

The Hermit primary has been flipped to `core.bare=true` repeatedly since
August 6. When it happens, refs and `.git` remain readable while every
work-tree operation fails with `fatal: this operation must be run in a work
tree`; gates that inspect the primary therefore go blind together.

The writer remains unidentified after named searches across the parent,
temporary clones, production scripts, and fixtures:

- `release-worktree.rs` and `allocate-worktree.rs` never write `core.bare`.
- No production script found on disk sets it to true.
- `primary_checkout.py` detects the condition and can set it false; it never
  sets it true.
- `test_primary_checkout.py` sets it true only in an unconditional
  `TemporaryDirectory` fixture. The fixture cannot target the real primary.
- Both production `git init --bare` call sites pass explicit separate target
  directories.

The August 22 recurrence first became visible at 07:21:07 PDT. Available file,
reflog, journal, and source evidence did not identify the writer. The next
useful step is observation that records the responsible process when the
config changes; another source grep or another one-line repair will not answer
causation.
