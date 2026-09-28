# Worktree slot cleanup record, 2026-09-28

This record answers the owner's Google Chat message `3QSxEZA5oY4`
(`spaces/AAQAWBA0g6A/messages/3QSxEZA5oY4.3QSxEZA5oY4`): "massive slot leakage ... drive down the
worktree slots to exactly match active agents ... Rescue branches as draft PRs IF needed, but drive
towards cleanup either way". Tracking issue: https://github.com/rrnewton/dev-hermit/issues/393.

Eight assessors inspected every checkout under `/home/newton/work/dev-hermit/worktrees/` and the
parent's other top-level checkouts on devbig014. Before any removal they pushed every piece of
unlanded git state (unpushed commits, local branches, dirty tracked files, untracked non-ignored
files) to GitHub as `rescue/slots-20260928/*` branches. This record does not remove anything. It
keeps what the assessors found so that removal can proceed and the evidence outlives this host.

- [`records/`](records/) holds byte-identical copies of the eight assessor outputs
  (`assess-1.json` to `assess-8.json`, 322 records). They are unabridged and include each slot's full
  `handoff_text` (the wrkslots sidecar handoff) and `legacy_handoff_md` (a checkout-local `HANDOFF.md`).
- [`slots.json`](slots.json) is the merged index: one entry per record with its verdict, the
  assessor's reasoning, its rescue branches, the handoff sizes, and `_records_file`, which points to
  the full record. Its `meta` block gives the SHA-256 of each record file. The merged records with
  handoffs come to 3.3 MB even as compact JSON. That is over the 2,048 KiB ceiling in this repository's
  pre-commit hook, which needs a coordinator decision to override. Split this way, every file is under
  the hook's 1,024 KiB soft limit and no override was used.
- Liveness was checked at 2026-09-28T21:36:21Z by scanning `/proc/*/{cwd,root,exe,fd}` and
  `herdr agent list`. No live process or pane was using any of the 322 paths.
- Main heads used for the re-checks below: hermit `fdd8f55a8e7c7a1bfa39704bc20ae2a391ef9218`, reverie `c1ffab7ee6e3f99a2057a70b26fc19ce37c90ec9`, dev-hermit `7424b031dafdc2cc702903981458d754bba9101a`.

## Counts

| Verdict | Records |
|---|---:|
| REMOVE | 319 |
| BLOCKED | 3 |
| **total** | **322** |

| Kind | Records |
|---|---:|
| `slots` | 301 |
| `toplevel` | 17 |
| `validate` | 2 |
| `tool-roots` | 2 |

`REMOVE` is the assessor's verdict that the checkout can be removed: all of its git state is on
GitHub, whether on main, in a merged or open pull request, or on a verified rescue branch, and no
live agent uses the path. `BLOCKED` means the assessor could not make that claim; the three cases
are below.

`REMOVE` does not mean that gitignored content was kept. Build outputs and `ignored/` evidence
directories were not pushed. 304 records have an `ignored_evidence_note` saying what
was left behind and whether a copy survives elsewhere. The two largest are
`worktrees/slots/kvm-stdin-cancel-01a0d5c2` (`target/`, 80.7 GB; the handoff's
`target/stdin-terminal-cancel/RESULT.md` was already gone) and
`worktrees/slots/kvm-rng-gate-recovery-01a0d5c2` (`ignored/`, 47.9 GB).

Rescue branches: 422 distinct names. 420 were read back from GitHub
with `git ls-remote` at the SHA the assessor pushed. By repository: hermit 282, dev-hermit 72, reverie 56, agent-utils 10, liteinst2 1. One name is in two
repositories, so these add up to 421. The 2 that are not on GitHub:

- `rescue/slots-20260928/codex-liteinst-tracing-01a0a13c--wip` (rrnewton/tracing): the push was
  refused with HTTP 403. rrnewton-bots has pull but not push permission on rrnewton/tracing
  (`gh api repos/rrnewton/tracing` reports `push: false`). See the first BLOCKED entry.
- `rescue/slots-20260928/utility-2176--wip` (hermit): deliberately not created. The working tree
  was identical to HEAD, so there was nothing to commit.

`rescue/slots-20260928/dev-hermit-gdb-vfile-pin-20260916--wip` exists in both hermit and dev-hermit
at the same SHA `43dda46ca603`, so it appears twice in the list below.

## BLOCKED (3)

1. **`worktrees/slots/codex-liteinst-tracing-01a0a13c`** (agent `dev-hermit-15`, purpose "Preserve
   Registry and EnvFilter state through late guest logging"). Pushing the WIP commit `1dd60ba4` to
   rrnewton/tracing failed with HTTP 403. The only unrescued content is the untracked 2,394-byte
   `HANDOFF.md`, a stale 2026-09-16 note. Its full text is in `records/assess-3.json`. HEAD `975950f9` is the
   head of merged https://github.com/rrnewton/tracing/pull/3, and the local branches `4784dc2b` and
   `3e47e172` are the heads of merged https://github.com/rrnewton/tracing/pull/2 and
   https://github.com/rrnewton/tracing/pull/1. **To unblock:** someone with push access to
   rrnewton/tracing pushes `1dd60ba4`, or the coordinator accepts that the HANDOFF.md text preserved
   in `records/assess-3.json` is enough and approves removal.
2. **`worktrees/slots/kvm-setitimer-reverie-20260918`** (agent `kvm_alarm_pending_impl`, purpose "Add
   the reviewed KVM process-pending alarm operation"). The product work is merged
   (https://github.com/rrnewton/reverie/pull/580 through https://github.com/rrnewton/reverie/pull/584).
   The checkout also holds 111,972 untracked files (12.1 GB) under `ignored/`, which reverie does
   not gitignore. That is more than GitHub's 2 GB push limit, so the assessor pushed a curated WIP,
   `8045573d` (7,149 files, 74.6 MB), to
   https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-setitimer-reverie-20260918--wip.
   Not rescued: about 1.43 GB of source-tree snapshots, 689 MB of other logs (mostly `.json` and
   `.jsonl`), and 9.9 GB of retained ELF binaries (75 files over 20 MB). The per-file listing, with a
   flag for which files are in the commit, is at
   `/var/tmp/coord-slotclean/assess-4-kvm-setitimer-untracked-listing.tsv.gz` on devbig014 (991 KB).
   That listing is on this host only. **To unblock:** the coordinator decides whether to discard
   the remainder. The assessor recommends approving removal, because the product work is merged and
   the handoff's "do not remove" (2026-09-18) predates the merges.
3. **`worktrees/slots/liteinst2-sigtrap-origin-01a0d099`** (agent `codex-liteinst2-sigtrap-01a0d099`,
   purpose "Preserve application SIGTRAP provenance at LiteInst sites"). The git content is safe:
   liteinst2 HEAD `e216ba0c` is clean and https://github.com/rrnewton/liteinst2/pull/29 has landed.
   But an active HOLD, placed 2026-09-24T07:09:28Z, says to retain the PR 29 reproduction, review
   artifacts and landing receipts until they are explicitly archived. The protected root
   `target/sigtrap-evidence` is already absent. **To unblock:** release the hold through supported
   authority.

## Draft pull-request candidates

The assessors flagged 19 records with a `draft_pr_candidate`, and one record named an
additional candidate, for 20 candidate branches. Each one was re-checked against the
main heads above using `git cherry` (patch equivalence), a file-presence check of its key new files
and symbols on main, and `gh api repos/rrnewton/<repo>/commits/<sha>/pulls` for every commit on the
branch. That endpoint returns open and merged pull requests only; 90 commits were checked and none
belongs to one. Closed pull requests were looked up by hand. "Behind" is the number of commits on
main that are not in the branch; every one of these branches needs a rebase before it can be
reviewed as current work.

| Slot | Repo | Rescue branch | Decision | Draft PR | Why |
|---|---|---|---|---|---|
| `codex-liteinst-runtime-resume-01a0a13c` | reverie | [rescue/slots-20260928/codex-liteinst-runtime-resume-01a0a13c--wip](https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/codex-liteinst-runtime-resume-01a0a13c--wip) | OPEN | https://github.com/rrnewton/reverie/pull/737 | 1 rescue commit on a 2026-09-17 base, 176 behind; 42 files, +9818/-60. 22 of its 24 new files (protected_fd.rs, guest_log.rs, installed_log.rs, rpc/mapped_setup.rs, ...) are absent from main. https://github.com/rrnewton/reverie/pull/705 overlaps only in Cargo.toml and lib.rs. |
| `dev-hermit-stdio-flags` | hermit | [rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-repair](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-repair) | OPEN | https://github.com/rrnewton/hermit/pull/3314 | 30 commits not on main, 405 behind; 22 files, +4315/-180, 11 new tests absent from main. It is the head of closed https://github.com/rrnewton/hermit/pull/2694 (46670790) plus one fix (046139e3). A successor must answer 2694's two refusals. |
| `demo08review` | hermit | [rescue/slots-20260928/demo08review--branch-demo08review-pr2694-rebase](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/demo08review--branch-demo08review-pr2694-rebase) | SKIP |  | Subset: an older 6-commit rebase of the same https://github.com/rrnewton/hermit/pull/2694 series (911 behind). Its 7 test files are a subset of the stdio-flags branch's 11; it is cited in that draft. |
| `kvm-finalize-profile` | hermit | [rescue/slots-20260928/kvm-finalize-profile](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-finalize-profile) | OPEN | https://github.com/rrnewton/hermit/pull/3315 | 1 commit, 554 behind; +2054/-202 in run.rs, container.rs and tracing.rs. Overlaps KVM VM release with the verification comparison (DeferredContainerRun, absent from main). Main's run_with_deferred_drop_owned is a different design. Alternative to kvm-teardown-opt. |
| `kvm-teardown-opt` | hermit | [rescue/slots-20260928/kvm-teardown-opt](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-teardown-opt) | OPEN | https://github.com/rrnewton/hermit/pull/3316 | 4 commits, 554 behind; +601/-79. Overlaps KVM teardown across verify executions; the assessor measured the median KVM cell wall time falling from 354 ms to 216 ms. Includes argument plumbing for harness and verify-cell Hermit runs. Its reverie half is already on main. |
| `liteinst-owner` | hermit | [rescue/slots-20260928/liteinst-owner--wip](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-owner--wip) | SKIP |  | The owner's own experiment (registry agent `owner`, 114 files, +24129/-432, 563 behind). Preserved on the rescue branch. Whether to open a PR is the owner's decision. |
| `liteinst-session-recovery` | hermit | [rescue/slots-20260928/liteinst-session-recovery--wip](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-session-recovery--wip) | OPEN | https://github.com/rrnewton/hermit/pull/3317 | Preservation only. 1 rescue commit, 563 behind; 79 files, +17709/-19761, on the same base as liteinst-owner but differing from it in 140 files. |
| `ci-signal-ptrace-fatal-cleanup-20260920` | reverie | [rescue/slots-20260928/ci-signal-ptrace-fatal-cleanup-20260920--reverie--wip](https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/ci-signal-ptrace-fatal-cleanup-20260920--reverie--wip) | OPEN | https://github.com/rrnewton/reverie/pull/738 | 2 commits, 126 behind; 13 files, +5334/-256. f61e4b0b's content landed in https://github.com/rrnewton/reverie/pull/646; the uncommitted WIP (InitialStopOwner, TracerStartupOwner) is absent from main. The slot was unregistered. |
| `dev-hermit-15-liteinst-rcb-01a0a13c` | reverie | [rescue/slots-20260928/dev-hermit-15-liteinst-rcb-01a0a13c--wip](https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dev-hermit-15-liteinst-rcb-01a0a13c--wip) | OPEN | https://github.com/rrnewton/reverie/pull/739 | 1 rescue commit, 161 behind; 42 files, +14700/-786, 17 new files absent from main. The purpose was to materialize and qualify the LiteInst root counter. |
| `dev-hermit-ci-provenance-20260915` | dev-hermit | [rescue/slots-20260928/dev-hermit-ci-provenance-20260915](https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/dev-hermit-ci-provenance-20260915) | OPEN | https://github.com/rrnewton/dev-hermit/pull/396 | 1 commit, 878 behind; 15 files, +739/-59, all of which still exist on main. Companion rescue branches exist in hermit (`--hermit`) and agent-utils (`--hermit-agent-utils`). |
| `dev-hermit-structured-results-20260918` | hermit | [rescue/slots-20260928/dev-hermit-structured-results-20260918](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-structured-results-20260918) | OPEN | https://github.com/rrnewton/hermit/pull/3318 | 1 commit, 186 behind; command_strict_verify.rs +427/-8. Main's copy of that file has no retained-log artifact root. |
| `dev-hermit-deferred-capture-api-20260917` | reverie | [salvage/dev-hermit-deferred-capture-api-20260917-root](https://github.com/rrnewton/reverie/tree/salvage/dev-hermit-deferred-capture-api-20260917-root) | OPEN | https://github.com/rrnewton/reverie/pull/740 | 2 commits, 161 behind; +1827/-145. The two-stage prepare_capture_unstarted/start_workers design and destination recovery are absent from main. Main landed a different plan/split design, so this may be superseded. |
| `dev-hermit-verify-two-runs-20260915` | hermit | [salvage/dev-hermit-verify-two-runs-20260915-root](https://github.com/rrnewton/hermit/tree/salvage/dev-hermit-verify-two-runs-20260915-root) | OPEN | https://github.com/rrnewton/hermit/pull/3319 | 4 commits, 367 behind; +1647/-341. owned_cgroup.rs and runner/retained_verify_log.rs are absent from main. Neither an ancestor of nor patch-equivalent to verify-two-runs-v4. |
| `verify-two-runs-v4` | hermit | [rescue/slots-20260928/verify-two-runs-v4--branch-feature-verify-two-harness-managed-runs-v4](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/verify-two-runs-v4--branch-feature-verify-two-harness-managed-runs-v4) | OPEN | https://github.com/rrnewton/hermit/pull/3320 | 7 commits, 558 behind; 24 files, +13255/-1744. Its precursors https://github.com/rrnewton/hermit/pull/2958 and https://github.com/rrnewton/hermit/pull/2969 were closed as superseded, but the obligation is still open. |
| `kvm-signal-abi` | hermit | [rescue/slots-20260928/kvm-signal-abi--wip](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-signal-abi--wip) | OPEN | https://github.com/rrnewton/hermit/pull/3321 | 2 commits, 531 behind. fcd7d5c0's patch is already on main; the WIP replaces the rt_sigprocmask how=-1 probe that is still on main. It edits detcore/src/syscalls/signal.rs and helpers.rs, which open https://github.com/rrnewton/hermit/pull/3224 also edits. |
| `astra-hermit-drafts` | hermit | [salvage/astra-hermit-drafts-root](https://github.com/rrnewton/hermit/tree/salvage/astra-hermit-drafts-root) | OPEN | https://github.com/rrnewton/hermit/pull/3322 | 4 commits, 379 behind; +309/-64. A current-main port of closed https://github.com/rrnewton/hermit/pull/2836, which still needs one. drain_reported_physical_process_exits is absent from main. |
| `astra-hermit-drafts-pr2984` | hermit | [rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2984](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2984) | OPEN | https://github.com/rrnewton/hermit/pull/3323 | 3 commits, 462 behind. 17d4faae was superseded by merged https://github.com/rrnewton/hermit/pull/2984. d298e31f adds a wait on pending run-queue admissions to are_all_quiesced, and main lacks it. It is extracted from astra-hermit-ready without that branch's SIGCHLD dependencies. |
| `astra-hermit-ready` | hermit | [rescue/slots-20260928/astra-hermit-ready--branch-astra-hermit-ready](https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-ready--branch-astra-hermit-ready) | OPEN | https://github.com/rrnewton/hermit/pull/3324 | 17 commits, 487 behind; 33 files, +7585/-349, 9 new files absent from main. Its sibling branches merged as https://github.com/rrnewton/hermit/pull/2973 and https://github.com/rrnewton/hermit/pull/2979. |
| `ledger-fix-design` | dev-hermit | [rescue/slots-20260928/ledger-fix-design](https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ledger-fix-design) | OPEN | https://github.com/rrnewton/dev-hermit/pull/397 | 1 commit, 978 behind; +2328/-203. process_path_census.py is absent from main. |
| `slots-recon` | dev-hermit | [rescue/slots-20260928/slots-recon--wip](https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/slots-recon--wip) | OPEN | https://github.com/rrnewton/dev-hermit/pull/398 | 1 rescue commit, 893 behind; 10 files, +803/-43. A stress-series/v4 producer; the string stress-series/v4 does not appear on main. Related: closed https://github.com/rrnewton/hermit/pull/3009. |

dev-hermit lands on main only. Its three drafts preserve the work so it can be triaged and are
not meant to be merged as pull requests.

## Every rescue branch on GitHub

Grouped by repository. Each entry gives the branch URL, the SHA it was verified at, and the
checkouts it came from, relative to `/home/newton/work/dev-hermit/`.

### hermit (282)

- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/147--scorecard-probe--wip `b1061c8b25a2` (`worktrees/147`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-2425-census `c4c979939632` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-landed-2970-712f `712f57f468bf` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2797 `2a76c3fc0219` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2833 `ec3a6fc3816c` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2836-before-2985 `2f352c631b0b` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2880 `20e3ed8f1cb2` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2927 `edcfed1bad70` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2966 `408443705b37` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2975 `794d6b6ad012` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2982-current `865c11592e9f` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2984 `096df21aec23` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2984-baseline-17d4 `17d4faaee154` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2985 `02eec6fc68f7` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2986 `c3312814da4a` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2987 `dcf6406d4272` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--branch-astra-hermit-drafts-pr2994 `8252530f4fc1` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-drafts--wip `5521a61a36fd` (`worktrees/slots/astra-hermit-drafts`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-ready--branch-astra-hermit-ready `49ef6c9ce79d` (`worktrees/slots/astra-hermit-ready`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-ready--branch-astra-hermit-ready-pr2973 `042a2cbf79f4` (`worktrees/slots/astra-hermit-ready`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-ready--branch-astra-hermit-ready-pr2979 `fd4e6eb8723b` (`worktrees/slots/astra-hermit-ready`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/astra-hermit-ready--ignored-pressure-fresh-Oe0SipCP-pressure-clone-self-test `cefb2a8eca24` (`worktrees/slots/astra-hermit-ready`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/bisect-si--branch-rescue-disk-cleanup-20260921-bisect-si `ebc2a64b4f18` (`worktrees/bisect-si`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/bisect-si--wip `208972d58a72` (`worktrees/bisect-si`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/cell-activation--hermit `f1e1f7bd83c4` (`worktrees/slots/cell-activation`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ci-parallel--hermit `e5922163b586` (`worktrees/slots/ci-parallel`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ci-signal-nextest-cpu-20260920--hermit `c0d965c03bbe` (`worktrees/slots/ci-signal-nextest-cpu-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ci-signal-proccpu-emission-20260920--hermit `63e081c2fb51` (`worktrees/slots/ci-signal-proccpu-emission-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ci-signal-proccpu-emission-20260920--hermit--branch-codex-ci-signal-proccpu-emission-20260920 `fa09d7fb76dd` (`worktrees/slots/ci-signal-proccpu-emission-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ci-signal-record-artifact-20260920--hermit `ba6c2814ca2e` (`worktrees/slots/ci-signal-record-artifact-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ci-signal-version-provenance-20260920--branch-codex-ci-signal-version-provenance-20260920 `be19af6a1e17` (`worktrees/slots/ci-signal-version-provenance-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ci-signal-version-provenance-20260920--hermit `b2e31f852673` (`worktrees/slots/ci-signal-version-provenance-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/claude-family-review-ten-retained--wip `c959091dac1c` (`worktrees/slots/claude-family-review-ten-retained`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/cml-hermit-3233 `4a8b17971f8f` (`worktrees/slots/cml-hermit-3233`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/cml-hermit-3233--branch-cml-sigint-3233 `a9cf326342f9` (`worktrees/slots/cml-hermit-3233`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/codex-liteinst-01a0a13c `944138ef2b30` (`worktrees/slots/codex-liteinst-01a0a13c`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/codex-liteinst-01a0a13c--wip `f8e4e6e3be97` (`worktrees/slots/codex-liteinst-01a0a13c`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/codex-liteinst-resume-01a0a13c `73a60dfa361f` (`worktrees/slots/codex-liteinst-resume-01a0a13c`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/codex-liteinst-resume-01a0a13c--branch-codex-liteinst-resume-01a0a13c `525cfb35a5c2` (`worktrees/slots/codex-liteinst-resume-01a0a13c`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/codex-liteinst-resume-01a0a13c--wip `10e2e1cc5e2c` (`worktrees/slots/codex-liteinst-resume-01a0a13c`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/demo08review--branch-demo08review-pr2694-rebase `11663ac6e72b` (`worktrees/slots/demo08review`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/demo08review--wip `0e2b56597d4c` (`worktrees/slots/demo08review`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/demos--branch-codex-dont-break-demos-principle `10723bd6d939` (`worktrees/slots/demos`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-au-offline-fetch-20260916--wip `71e92ed13437` (`worktrees/slots/dev-hermit-au-offline-fetch-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-ci-cardinality-fixtures-20260916 `84269bc61043` (`worktrees/slots/dev-hermit-ci-cardinality-fixtures-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-ci-gate-repairs-20260916 `7fc1b1200ebb` (`worktrees/slots/dev-hermit-ci-gate-repairs-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-ci-gate-repairs-20260916--wip `de1afcd334fc` (`worktrees/slots/dev-hermit-ci-gate-repairs-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-ci-manifest-20260915 `631131d42972` (`worktrees/slots/dev-hermit-ci-manifest-20260915`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-ci-manifest-20260915--branch-dev-hermit-ci-manifest-20260915 `bda9cbc9c40f` (`worktrees/slots/dev-hermit-ci-manifest-20260915`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-ci-manifest-20260915--wip `9753e650570b` (`worktrees/slots/dev-hermit-ci-manifest-20260915`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-ci-provenance-20260915--hermit `493aab8bc026` (`worktrees/slots/dev-hermit-ci-provenance-20260915`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-dbt-error-cleanup-20260916 `946bb11ba23f` (`worktrees/slots/dev-hermit-dbt-error-cleanup-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-doc-provenance-3093-20260918 `0e35f90cd8f0` (`worktrees/slots/dev-hermit-doc-provenance-3093-20260918`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-doc-provenance-3093-20260918--branch-codex-pr3093-doc-provenance-20260918 `7eef0effc093` (`worktrees/slots/dev-hermit-doc-provenance-3093-20260918`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-gdb-replay-diagnostic-20260916--ignored-hermetic-app-strict-verify-git-clone `2c64e5f45587` (`worktrees/slots/dev-hermit-gdb-replay-diagnostic-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-gdb-replay-diagnostic-20260916--wip `c9c954803030` (`worktrees/slots/dev-hermit-gdb-replay-diagnostic-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-gdb-vfile-pin-20260916 `e208b86786ea` (`worktrees/slots/dev-hermit-gdb-vfile-pin-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-gdb-vfile-pin-20260916--wip `43dda46ca603` (`worktrees/slots/dev-hermit-gdb-vfile-pin-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-harness-journal-20260916 `eca475ef9084` (`worktrees/slots/dev-hermit-harness-journal-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-harness-journal-20260916--wip `ca3d96aaa3e3` (`worktrees/slots/dev-hermit-harness-journal-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-hermetic-v3-20260916--wip `97cec344a55b` (`worktrees/slots/dev-hermit-hermetic-v3-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-host-fetch-proxy-20260917 `ff810b514d6a` (`worktrees/slots/dev-hermit-host-fetch-proxy-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-kvm-native-feature-pin-20260919 `dc15d94e66fb` (`worktrees/slots/dev-hermit-kvm-native-feature-pin-20260919`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-manifest-fixture-ledger-20260916 `abd9d6b16bf8` (`worktrees/slots/dev-hermit-manifest-fixture-ledger-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-pinned-command-fixtures-20260916 `96565ac8b1aa` (`worktrees/slots/dev-hermit-pinned-command-fixtures-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-pinned-command-fixtures-20260916--wip `32b3c39322fb` (`worktrees/slots/dev-hermit-pinned-command-fixtures-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-pinned-guest-paths-20260916 `f844f8534da4` (`worktrees/slots/dev-hermit-pinned-guest-paths-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-pinned-guest-paths-20260916--wip `70f269327628` (`worktrees/slots/dev-hermit-pinned-guest-paths-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-prepare-full-fixture-20260918--branch-dev-hermit-prepare-full-fixture-20260918 `b1046c32a542` (`worktrees/slots/dev-hermit-prepare-full-fixture-20260918`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-proc-locks-runtime-20260916 `505e0641c656` (`worktrees/slots/dev-hermit-proc-locks-runtime-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-proc-locks-runtime-20260916--wip `f6e43f1ff8f7` (`worktrees/slots/dev-hermit-proc-locks-runtime-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-regular-census-20260918 `6908c8f05bb9` (`worktrees/slots/dev-hermit-regular-census-20260918`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-retry-fetch-fixtures-20260918--branch-codex-retain-retry-helper-test-fixtures-20260918 `0d2e74ecd14c` (`worktrees/slots/dev-hermit-retry-fetch-fixtures-20260918`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit `ad0d40be32f1` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-archive-astra-hermit-pr2945-qualified-97b78cb5 `97b78cb5663f` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-core-review-binding-recovery `eb7eb4438087` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-harness-managed-verify-recovery `92cadebb9fca` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-kvm-select-21-recovery `9d65ed672062` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-parity-report-cause `4e5f851730b5` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-parity-report-count `11f850267312` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-parity-scorecard-recovery `24ceb1cb2cd3` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-refusal-classification-recovery `ccbd4ee90065` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-run-evidence-before-main-b6ff3253 `b6ff32538794` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-run-evidence-recovery `91ffc8d1e4f5` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-scorecard-snapshot-repair `6e11ebf94a85` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-dev-hermit-validation-reasons-recovery `80cb202e2d2c` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-pressure-prerequisite-log-isolation `bb7331c50878` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--hermit-branch-scorecard-clippy-borrow `825586e16d70` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-robust-exit-clocks-20260916 `f6c7bf197875` (`worktrees/slots/dev-hermit-robust-exit-clocks-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-rust-script-clippy-20260917--branch-dev-hermit-rust-script-clippy-20260917 `3fa020d4a065` (`worktrees/slots/dev-hermit-rust-script-clippy-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-sabre-getrandom-diagnostic-20260917--wip `51ea078fdc08` (`worktrees/slots/dev-hermit-sabre-getrandom-diagnostic-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-scorecard-timeout-20260919--branch-codex-scorecard-snapshot-timeout-diagnostic-20260919 `85a849abf6ea` (`worktrees/slots/dev-hermit-scorecard-timeout-20260919`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags `df5ac0d282fe` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-b85-first-22 `1a607312d017` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-2973-4d7b25f1 `4d7b25f17f2a` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-canonical-json-4370e25c `4370e25c4395` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-dagrun-main-db48976c `db48976cda64` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-dagrun-prep-c5f973 `c5f973bcad9b` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-fixture-main-2a14ccbe `2a14ccbe1bca` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-footprint-main-913e5fe89 `913e5fe89e47` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-generated-main-08ca65cc `08ca65cc3ded` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-before-submodule-run-state-2a14ccbe `2a14ccbe1bca` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-original-on-main `6d3a9c916f89` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-rebase-f6bc4dd9 `f6bc4dd9beda` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-stdio-flags--branch-dev-hermit-stdio-flags-repair `046139e3affc` (`worktrees/slots/dev-hermit-stdio-flags`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-structured-results-20260918 `9d46215754c4` (`worktrees/slots/dev-hermit-structured-results-20260918`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-submodule-fixture-producer-reuse-20260917 `6247ff5e316d` (`worktrees/slots/dev-hermit-submodule-fixture-producer-reuse-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-submodule-fixture-producer-reuse-20260917--branch-dev-hermit-submodule-fixture-producer-reuse-20260917 `aa2052ce518f` (`worktrees/slots/dev-hermit-submodule-fixture-producer-reuse-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/dev-hermit-verify-two-runs-20260915--wip `0d631df51cbc` (`worktrees/slots/dev-hermit-verify-two-runs-20260915`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2 `ba93752896fe` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-drain-2-pr2765-rebase `9fb9cb8a5a3f` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-drain-2-pr2934-fix `4eecd4e6d48e` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-drain-2-pr3022-review `df6a1775b29f` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-drain-2-receipt-rule-carriers `55d72d2eee04` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-drain-2-test-2980 `db33afbd3c6c` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-hermit007-drain-2552 `58087cff9b93` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-hermit007-drain-2555 `871b49aec3b1` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-hermit007-drain-2556 `75b6bffad3b5` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-hermit007-drain-2558 `55547ee56069` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-hermit007-drain-2570 `629a952f3323` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/drain-2--branch-hermit007-drain-2579 `40e228ec42b8` (`worktrees/slots/drain-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/exact-test-artifact-verifier--branch-exact-test-artifact-verifier-schema9-artifact-byte-verifier `86902d9a6ffb` (`worktrees/slots/exact-test-artifact-verifier`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/fbsource-import-pr94-20260926--wip `376613dc506a` (`worktrees/slots/fbsource-import-pr94-20260926`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--adoptions-pr2880 `416e5c39d36d` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--adoptions-pr2976--ref-source-pr-2976-original `e2d495bd8117` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--hermit--branch-fix-preserve-pressure-series-rows `9da1197f1e8c` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--reconstructions-network-gate-refusal `de1b1de0b3b5` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--reconstructions-pr2694--ref-audit-pr2668 `6a839d935210` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--reconstructions-pr2745--branch-preserved-pr-2745-original `22a0058bbe05` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--reconstructions-pr2747 `d7f01b95a472` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--reconstructions-pr2925--branch-preserved-pr-2925-original `d63465ef6ec4` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--reconstructions-pr2969 `3eb6cc214450` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-002--reconstructions-pr2970--branch-preserve-pr-2970-b0ea4580 `b0ea4580a11d` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-ecx-complete--wip `95527af2d0c8` (`worktrees/slots/hermit-ecx-complete`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/hermit-ecx-findings--wip `105d495e74c3` (`worktrees/slots/hermit-ecx-findings`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/integration-pr529-pr538-diagnostic--wip `65ca987d4f3d` (`worktrees/slots/integration-pr529-pr538-diagnostic`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/integration-pr529-pr538-hermit--wip `9c339f581de1` (`worktrees/slots/integration-pr529-pr538-hermit`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-detcore-rng-readv-01a0d5c2--wip `a4f9d6a34ef5` (`worktrees/slots/kvm-detcore-rng-readv-01a0d5c2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-detcore-vectored--wip `f24b667eafc6` (`worktrees/slots/kvm-detcore-vectored`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-finalize-profile `88164340deca` (`worktrees/slots/kvm-finalize-profile`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-frozen-cleanup-20260917--hermit--wip `ea4b0875832c` (`worktrees/slots/kvm-frozen-cleanup-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-gdb-watcher-01a0d5c2--wip `a737cf52453a` (`worktrees/slots/kvm-gdb-watcher-01a0d5c2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-guest-args--ignored-pressure-clone-self-test-2355021-1788935391902682902 `b555949a3205` (`worktrees/slots/kvm-guest-args`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-next40-final `40a7bab8e941` (`worktrees/slots/kvm-next40-final`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-parity-20260915-codex `e340d7f9160f` (`worktrees/slots/kvm-parity-20260915-codex`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-parity-recovery-20260916 `32033aac4805` (`worktrees/slots/kvm-parity-recovery-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-parity-recovery-20260916--wip `faecda8e9b74` (`worktrees/slots/kvm-parity-recovery-20260916`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-prejoin-hermit-20260917--wip `f9d0f44b6c81` (`worktrees/slots/kvm-prejoin-hermit-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-ratchet--branch-kvm-ratchet11 `2bf5685482eb` (`worktrees/slots/kvm-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-ratchet--branch-kvm-ratchet12 `bbbba70ceccc` (`worktrees/slots/kvm-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-ratchet--branch-kvm-ratchet14 `600a19a8b587` (`worktrees/slots/kvm-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-ratchet--branch-kvm-ratchet16-linux-blockers `dcca60b0043d` (`worktrees/slots/kvm-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-ratchet--branch-kvm-ratchet8 `15e0bf3bbc9f` (`worktrees/slots/kvm-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-ratchet-90-fix6 `db33afbd3c6c` (`worktrees/slots/kvm-ratchet-90-fix6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-raw-stderr-20260917 `c01b0e11b212` (`worktrees/slots/kvm-raw-stderr-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-raw-stderr-20260917--wip `f91de2ecc85b` (`worktrees/slots/kvm-raw-stderr-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-rng-gate-recovery-01a0d5c2 `c8f974c8cfb2` (`worktrees/slots/kvm-rng-gate-recovery-01a0d5c2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-rng-user-access-01a0d5c2 `c8f974c8cfb2` (`worktrees/slots/kvm-rng-user-access-01a0d5c2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-signal-abi `fcd7d5c0a7e1` (`worktrees/slots/kvm-signal-abi`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-signal-abi--wip `1ca96a7e208b` (`worktrees/slots/kvm-signal-abi`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-summary-main-20260917 `b4a442ef1055` (`worktrees/slots/kvm-summary-main-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-summary-main-20260917--wip `e0df94f14965` (`worktrees/slots/kvm-summary-main-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-summary-publication-20260917--wip `17d3688f790d` (`worktrees/slots/kvm-summary-publication-20260917`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-sysinfo `5e6f00542bfc` (`worktrees/slots/kvm-sysinfo`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-teardown-opt `20d7ea6e10f6` (`worktrees/slots/kvm-teardown-opt`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/kvm-thread-exit-order-hermit--branch-codex-kvm-clear-child-tid-detcore `9682a0ed6f05` (`worktrees/slots/kvm-thread-exit-order-hermit`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/lander-2--branch-degraded-lander-2-pr2798-repair `83bfce40974e` (`worktrees/slots/lander-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/lander-2--branch-degraded-lander-2-pr2802-fix `c54ee95f577b` (`worktrees/slots/lander-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/lander-2--branch-degraded-lander-2-pr2802-post2811-fixture-fix `ec0d7a8283d6` (`worktrees/slots/lander-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/lander-2--branch-degraded-lander-2-pr2802-residual `d9e195a4cb51` (`worktrees/slots/lander-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/lander-2--branch-degraded-lander-2-pr2824-landing `13216b5f98cf` (`worktrees/slots/lander-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/lander-2--branch-degraded-lander-2-pr2838-retry-accounting `90ee48acce40` (`worktrees/slots/lander-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/lander-2--branch-rescue-reclaim-20260901T233832Z-lander-2-pr2799-final `ff1d2498121a` (`worktrees/slots/lander-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-owner--wip `d867697fe735` (`worktrees/slots/liteinst-owner`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-19 `4881fa42a269` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-20 `4e7a74f4519f` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-21 `312afc4546ec` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-22 `6c57f3a8803e` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-23 `b34b79a9bb8b` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-24 `4c81b2c39f8f` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-25 `fba26e07d938` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-26 `993f2d270a2b` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-27 `615594dedebc` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-28 `0535e71c80c2` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-29 `188d8989138b` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-30 `7e0f8f8d3530` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-31 `6e4112c55b14` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-32 `de996b818aba` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-33 `fa6a42b42998` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-34 `af1c4910a4d7` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-35 `27c7e0f1f295` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-36 `9183417cc90b` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-37 `e70bcedc1bcb` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-38 `51820ed74da3` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-39 `21fffcb798bf` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-40 `b0ee126ee572` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-41 `c63fd404cc9b` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-42 `fd5a8a3a6dd2` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-codex-liteinst-ratchet-7-hermit `75eb5ab657fe` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-impl-liteinst-ratchet-4-hermit-slot342 `57a9c45d6faa` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet10 `8575509165c0` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet11 `44ba62b8c065` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet12 `b6cd9feba151` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet13 `fd312b6c18a7` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet14 `593a4792aaa1` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet15 `83d190c054f1` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet16 `7a9de483da60` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet8 `b7aad836a7ad` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-ratchet--branch-liteinst-ratchet9 `9555a8e3fb07` (`worktrees/slots/liteinst-ratchet`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/liteinst-session-recovery--wip `7ff27a263ea4` (`worktrees/slots/liteinst-session-recovery`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-ci-passed-count-activation-20260915--wip `b94a39841d18` (`worktrees/slots/mega-ci-passed-count-activation-20260915`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-hermit-2970--wip `01de715ba2ea` (`worktrees/slots/mega-lander-hermit-2970`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-main-census-repair-20260925-r1 `2a8f4098773a` (`worktrees/slots/mega-lander-main-census-repair-20260925-r1`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-20260924-r4 `015dfbece805` (`worktrees/slots/mega-lander-scorecard-20260924-r4`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-20260924-r4--ignored-review-015dfbec-claude-mirror--wip `cf5d5b078343` (`worktrees/slots/mega-lander-scorecard-20260924-r4`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-diagnosis-20260925-r5 `6f7830612396` (`worktrees/slots/mega-lander-scorecard-gate-diagnosis-20260925-r5`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-diagnosis-20260925-r5--ignored-scorecard-gate-diagnosis-patched-checkout--wip `409344e18bac` (`worktrees/slots/mega-lander-scorecard-gate-diagnosis-20260925-r5`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-diagnosis-20260925-r5--ignored-scorecard-gate-diagnosis-patched-final-two-core-checkout--wip `dec6d5e7b3b7` (`worktrees/slots/mega-lander-scorecard-gate-diagnosis-20260925-r5`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-diagnosis-20260925-r5--ignored-scorecard-gate-diagnosis-patched-two-core-checkout--wip `1c666d17e2d5` (`worktrees/slots/mega-lander-scorecard-gate-diagnosis-20260925-r5`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6 `b89ba1757d9a` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-functional-b6c6-checkout `b6c6db017f1d` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-functional-b6c6-pressure-clone-self-test `b04a72768b87` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-02-source `dcaa10749d68` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-02-source--wip `0bcd86bed01c` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-03-source `c667ec835ccf` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-07-source `608de78365b3` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-09-source `608de78365b3` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-11-source `608de78365b3` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-selftest-outer-confusion `d18497fc17e5` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/mega-lander-scorecard-gate-integration-20260925-r6--ignored-pinned-image-recovery-selftest-tampered-source--wip `10eeabe49085` (`worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/parent-receipt-snapshot-repair--hermit `9a0b6b782c95` (`worktrees/slots/parent-receipt-snapshot-repair`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/parent-receipt-snapshot-repair--hermit--branch-demo-review-banner-ordering-repair `c87d1b8e077e` (`worktrees/slots/parent-receipt-snapshot-repair`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/parent-receipt-snapshot-repair--hermit--wip `7dfa96de33bd` (`worktrees/slots/parent-receipt-snapshot-repair`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/pin-validate-tool-root--hermit--branch-_land_2938 `a20dbb13e2f9` (`worktrees/slots/pin-validate-tool-root`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/pr-drain-1-hermit `41af2f0115bc` (`worktrees/slots/pr-drain-1-hermit`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/pr-drain-ci-signal-hermit-20260920 `0e35f90cd8f0` (`worktrees/slots/pr-drain-ci-signal-hermit-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/pr-drain-ci-signal-hermit-admission-fixtures-20260920 `e9486f3b74e8` (`worktrees/slots/pr-drain-ci-signal-hermit-admission-fixtures-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/pr-drain-ci-signal-hermit-portable-20260920 `09e80d81c26f` (`worktrees/slots/pr-drain-ci-signal-hermit-portable-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/pr-drain-ci-signal-hermit-portable-20260920--branch-codex-pr-drain-portable-20260920 `425531a3b82c` (`worktrees/slots/pr-drain-ci-signal-hermit-portable-20260920`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/pr2802-residual-evidence `d9e195a4cb51` (`worktrees/pr2802-residual-evidence`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/propagate-reason-hermit--branch-propagate-computed-reason-hermit `2dcbbf61c040` (`worktrees/slots/propagate-reason-hermit`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ptrace-red-opus-20260924--branch-agent-ptrace-red-opus-20260924 `cf35877efb8d` (`worktrees/slots/ptrace-red-opus-20260924`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/ptrace-red-watermelon-20260924--wip `009cbd79f155` (`worktrees/slots/ptrace-red-watermelon-20260924`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/review-520--hermit-ab `6cf5872e83b6` (`worktrees/slots/review-520`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/review-520--hermit-ab--wip `73ae6fce4fd3` (`worktrees/slots/review-520`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/review-cpuid--branch-review-cpuid-authority-unreachable-is-not-red `5c6fa13b451b` (`worktrees/slots/review-cpuid`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/review-cpuid--branch-review-cpuid-pre-push-names-unpopulated-submodule `8e45aec34dfc` (`worktrees/slots/review-cpuid`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/review-cpuid--wip `6409030b6bbc` (`worktrees/slots/review-cpuid`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/review-pr2802-codex `d9e195a4cb51` (`worktrees/review-pr2802-codex`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/review-pr2980-codex `db33afbd3c6c` (`worktrees/review-pr2980-codex`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/reviewer-3-hermit `a11796fdb813` (`worktrees/slots/reviewer-3-hermit`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/reviewer-3-hermit--wip `df1b54626313` (`worktrees/slots/reviewer-3-hermit`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/run1750-fix-source-00ea6c9b `00ea6c9b51fd` (`worktrees/slots/run1750-fix-source-00ea6c9b`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-pages--branch-scorecard-pages-publication `443d5952406a` (`worktrees/slots/scorecard-pages`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-pin-publish-20260924 `97589095977d` (`worktrees/slots/scorecard-pin-publish-20260924`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-publish-sept14--hermit--branch-scorecard-publish-republish-sept14 `3c54333d7fd6` (`worktrees/slots/scorecard-publish-sept14`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-publish-validation-21bfbfee `21bfbfee29ba` (`worktrees/scorecard-publish-validation-21bfbfee`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-publish-validation-bad483 `bad4832126f5` (`worktrees/scorecard-publish-validation-bad483`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-website `58ffdaff87fd` (`worktrees/slots/scorecard-website`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-website--wip `343970df9fe6` (`worktrees/slots/scorecard-website`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/service-result-passed-tests--branch-feature-service-result-passed-tests `45c98f921047` (`worktrees/slots/service-result-passed-tests`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/service-result-passed-tests--wip `4ee23c6da7b9` (`worktrees/slots/service-result-passed-tests`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/slots-recon--hermit `086ec9b3c423` (`worktrees/slots/slots-recon`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/tick-2--hermit `48a9b148f1b7` (`worktrees/slots/tick-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/tick-2--hermit--branch-tick-2-health-tick-agent-utils-pin `cde82cc64d69` (`worktrees/slots/tick-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/tick-2--hermit--branch-tick-2-tick-hub-parallel-pre-pr2944 `54b4757e6ca4` (`worktrees/slots/tick-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/tick-2--hermit--wip `c8b510cf0e20` (`worktrees/slots/tick-2`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/tick-2-cli-skill `41af2f0115bc` (`worktrees/slots/tick-2-cli-skill`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/timerfd-family-identity-20260925--wip `f886c4a28e5a` (`worktrees/slots/timerfd-family-identity-20260925`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/timerfd-family-identity-opus-r2-20260925--wip `23909b86ae1e` (`worktrees/slots/timerfd-family-identity-opus-r2-20260925`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/utility-2176 `27be9dc9b2a6` (`worktrees/utility-2176`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/utility-2223 `2f57dadc235a` (`worktrees/utility-2223`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/utility-2680 `7c83bb8d2294` (`worktrees/utility-2680`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/validate-current-tip--hermit--wip `52aae2920579` (`worktrees/slots/validate-current-tip`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/validate-status-history--hermit--branch-mega-lander-fp-reduction-timeout `f424c4d31faf` (`worktrees/slots/validate-status-history`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/validate-status-history--hermit--branch-mega-lander-repeat-red-cells `8b8e4f83c22c` (`worktrees/slots/validate-status-history`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/validate-status-history--hermit--branch-mega-lander-scorecard-source-tree `43d54f6ba641` (`worktrees/slots/validate-status-history`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/validate-status-history--hermit--branch-mega-lander-serialize-shared-test-binaries `08420f3e840b` (`worktrees/slots/validate-status-history`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/validate-status-history--hermit--branch-project-scorecard-series-fdcec `737c663a015b` (`worktrees/slots/validate-status-history`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/verify-two-runs-v4--branch-feature-verify-two-harness-managed-runs-v4 `a6e70b4db2c2` (`worktrees/slots/verify-two-runs-v4`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/wrkslots-create-demo-20260914 `37a12da4e66d` (`worktrees/slots/wrkslots-create-demo-20260914`)
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/wrkslots-create-demo-20260914--wip `2b50f9584802` (`worktrees/slots/wrkslots-create-demo-20260914`)

### dev-hermit (72)

- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/cell-activation `f0f63176f351` (`worktrees/slots/cell-activation`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/cell-activation--wip `afbc669b6f4f` (`worktrees/slots/cell-activation`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ci-parallel--wip `25583106e81d` (`worktrees/slots/ci-parallel`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ci-signal-nextest-cpu-20260920 `c2251375d35f` (`worktrees/slots/ci-signal-nextest-cpu-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ci-signal-nextest-cpu-20260920--wip `da60faa3fdfe` (`worktrees/slots/ci-signal-nextest-cpu-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ci-signal-proccpu-emission-20260920 `e0c13dd8090d` (`worktrees/slots/ci-signal-proccpu-emission-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ci-signal-proccpu-emission-20260920--wip `c67bab21124f` (`worktrees/slots/ci-signal-proccpu-emission-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ci-signal-ptrace-fatal-cleanup-20260920--wip `b4a6f09d820e` (`worktrees/slots/ci-signal-ptrace-fatal-cleanup-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ci-signal-reverie-run-identity-20260920--wip `c5fc355e3283` (`worktrees/slots/ci-signal-reverie-run-identity-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/codex-liteinst-receipts-resume-01a0a13c--wip `df83e455e76f` (`worktrees/slots/codex-liteinst-receipts-resume-01a0a13c`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/crosscheck-unadmitted--branch-codex-crosscheck-unadmitted `698518587ada` (`worktrees/slots/crosscheck-unadmitted`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/demos `6a55b3b3a8c9` (`worktrees/slots/demos`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/demos--branch-demos-external-presentability-pass `956ee5b9580f` (`worktrees/slots/demos`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/demos--branch-demos-pinned-hermit-binary `8a9d9924424f` (`worktrees/slots/demos`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/demos--branch-rescue-dagrun-move-demos-parent-20260831 `7d20507cfa52` (`worktrees/slots/demos`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/demos--wip `6a34b32fbd6c` (`worktrees/slots/demos`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/dev-hermit-ci-provenance-20260915 `c2944524e1b8` (`worktrees/slots/dev-hermit-ci-provenance-20260915`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/dev-hermit-ci-provenance-20260915--wip `e3cd463868c9` (`worktrees/slots/dev-hermit-ci-provenance-20260915`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/dev-hermit-gdb-vfile-pin-20260916--wip `43dda46ca603` (`worktrees/slots/dev-hermit-gdb-vfile-pin-20260916`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/dev-hermit-hermit-8515-pin `ea5b239a3d17` (`worktrees/slots/dev-hermit-hermit-8515-pin`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--wip `12b946730f59` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/hermit-002 `06986220ef33` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/hermit-002--wip `e8b9b3496c5e` (`worktrees/slots/hermit-002`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/hermit-003--wip `f1cb2cde62f4` (`worktrees/slots/hermit-003`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/hermit-003-anchored-selection `72f16fe22a0f` (`worktrees/slots/hermit-003-anchored-selection`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/hermit-003-anchored-selection--wip `16ef0cb84931` (`worktrees/slots/hermit-003-anchored-selection`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/hermit-2971-parent-landing `e86abd6674eb` (`worktrees/slots/hermit-2971-parent-landing`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/inventory-ledger-mirror--branch-inventory-1-ledger-mirror `9fe3d7913c54` (`worktrees/slots/inventory-ledger-mirror`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/kvm-frozen-cleanup-20260917 `fba5d50c9675` (`worktrees/slots/kvm-frozen-cleanup-20260917`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/kvm-frozen-cleanup-20260917--wip `59f54c95190a` (`worktrees/slots/kvm-frozen-cleanup-20260917`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/kvm-guest-args-parent `1fea789a3d95` (`worktrees/slots/kvm-guest-args-parent`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/kvm-parity-ledger-parent-20260916--wip `b713944e1bd6` (`worktrees/slots/kvm-parity-ledger-parent-20260916`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/lander-2-seven-gate-receipts--branch-lander-2-seven-gate-receipts `6d9db0dfe03b` (`worktrees/slots/lander-2-seven-gate-receipts`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/ledger-fix-design `fd038f9f1bd3` (`worktrees/slots/ledger-fix-design`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/mega-ci-successor-20260915 `3a06161732bb` (`worktrees/slots/mega-ci-successor-20260915`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/mega-ci-successor-20260915--wip `7def034fe7a7` (`worktrees/slots/mega-ci-successor-20260915`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/mega-lander-anchored-selection-land--branch-mega-lander-anchored-selection-constructed-plan `d8f1fc823786` (`worktrees/slots/mega-lander-anchored-selection-land`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/parent-finalizer-canonical-dag `874fcf30f8e4` (`worktrees/slots/parent-finalizer-canonical-dag`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/parent-receipt-snapshot-repair--wip `bed17e8722c2` (`worktrees/slots/parent-receipt-snapshot-repair`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/passed-tests-receipts `94f2245989ff` (`worktrees/slots/passed-tests-receipts`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/passed-tests-receipts--branch-require-passed-tests-current-receipts `140167af64da` (`worktrees/slots/passed-tests-receipts`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/pr-drain-ci-signal-cell-history-authority-20260920--wip `ac5d12185bdb` (`worktrees/slots/pr-drain-ci-signal-cell-history-authority-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/pr-drain-ci-signal-parent-scorecard-20260920--wip `c74e2420034c` (`worktrees/slots/pr-drain-ci-signal-parent-scorecard-20260920`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/pressure-writeback-path `000c7a1d90f1` (`worktrees/slots/pressure-writeback-path`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/pressure-writeback-path--branch-pressure-writeback-path `75eb614d0f48` (`worktrees/slots/pressure-writeback-path`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/propagate-reason-parent `7e45c83dd1b5` (`worktrees/slots/propagate-reason-parent`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/propagate-reason-parent--branch-propagate-computed-reason-parent `1617ca444afb` (`worktrees/slots/propagate-reason-parent`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/recon-2-plugin-workflow-fix `be0b5453bf6e` (`worktrees/slots/recon-2-plugin-workflow-fix`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/recon-2-plugin-workflow-fix--branch-recon-2-hermit-dev-plugin-workflow-fix `4f6d04d7bac7` (`worktrees/slots/recon-2-plugin-workflow-fix`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/recon-2-plugin-workflow-fix--wip `d689fab97c94` (`worktrees/slots/recon-2-plugin-workflow-fix`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-cold-context `393f6e5f9731` (`worktrees/slots/scorecard-cold-context`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-generator-sept14 `393f6e5f9731` (`worktrees/slots/scorecard-generator-sept14`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-generator-sept14--wip `835514c4fdd6` (`worktrees/slots/scorecard-generator-sept14`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-publish-sept14 `b0c4e086a792` (`worktrees/slots/scorecard-publish-sept14`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-publish-sept14--wip `f596984fc06d` (`worktrees/slots/scorecard-publish-sept14`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-website--.keep-validate-status `08e477171a77` (`worktrees/slots/scorecard-website`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-website--.keep-validate-status--wip `e5946da4f76a` (`worktrees/slots/scorecard-website`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-website--.keep-validate-status-final--wip `dbba6e52ced4` (`worktrees/slots/scorecard-website`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-website--ignored-ci-hub-inflight `ae2d965ebc91` (`worktrees/slots/scorecard-website`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/series-rows `8da370a3b3f1` (`worktrees/slots/series-rows`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/site-plan `122a3f64547e` (`worktrees/slots/site-plan`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/slots-recon--wip `8bbe1bd73105` (`worktrees/slots/slots-recon`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/status-publisher-schema--branch-codex-status-publisher-schema `b714b2c43a9e` (`worktrees/slots/status-publisher-schema`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/tfa-poc-20260919 `68165004d8d6` (`worktrees/slots/tfa-poc-20260919`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/tick-2 `3f00f12632f5` (`worktrees/slots/tick-2`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/tick-2--wip `811626b88a2e` (`worktrees/slots/tick-2`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/tree-aware-signal-crosscheck--branch-tree-aware-signal-crosscheck `8c77482628a5` (`worktrees/slots/tree-aware-signal-crosscheck`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/validate-status-history `a2f54ce089e3` (`worktrees/slots/validate-status-history`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/validate-status-isolation `5258851cff5c` (`worktrees/slots/validate-status-isolation`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/validate-status-isolation--branch-mega-lander-validate-status-isolation `14db36e0b02f` (`worktrees/slots/validate-status-isolation`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/validate-status-isolation--ignored-pr258-shallow-review-6mZg9k-workspace `c662a289b805` (`worktrees/slots/validate-status-isolation`)
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/validate-status-isolation--ignored-pr258-shallow-review-6mZg9k-workspace--wip `a2c96e0089d9` (`worktrees/slots/validate-status-isolation`)

### reverie (56)

- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/astra-reverie--branch-astra-reverie `3a822b4af3a7` (`worktrees/slots/astra-reverie`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/astra-reverie--wip `5416220b0c95` (`worktrees/slots/astra-reverie`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/ci-signal-main-health-20260919--reverie--wip `cfaa9ff13175` (`worktrees/slots/ci-signal-main-health-20260919`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/ci-signal-ptrace-fatal-cleanup-20260920--reverie `f61e4b0bc46e` (`worktrees/slots/ci-signal-ptrace-fatal-cleanup-20260920`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/ci-signal-ptrace-fatal-cleanup-20260920--reverie--wip `15d2bc00cbf5` (`worktrees/slots/ci-signal-ptrace-fatal-cleanup-20260920`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/codex-liteinst-exec-01a0a13c--wip `92fceefc56b1` (`worktrees/slots/codex-liteinst-exec-01a0a13c`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/codex-liteinst-runtime-resume-01a0a13c--branch-codex-liteinst-runtime-resume-01a0a13c `99276ea22b2b` (`worktrees/slots/codex-liteinst-runtime-resume-01a0a13c`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/codex-liteinst-runtime-resume-01a0a13c--wip `679d6b019ad6` (`worktrees/slots/codex-liteinst-runtime-resume-01a0a13c`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dbt-diagnostic-stderr--wip `50973476d86c` (`worktrees/slots/dbt-diagnostic-stderr`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dev-hermit-15-liteinst-rcb-01a0a13c--wip `bc82cf242198` (`worktrees/slots/dev-hermit-15-liteinst-rcb-01a0a13c`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dev-hermit-15-vdso-getrandom-01a0a13c--wip `d23327ad7849` (`worktrees/slots/dev-hermit-15-vdso-getrandom-01a0a13c`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dev-hermit-gdb-vfile-20260916--wip `30c79a0738b7` (`worktrees/slots/dev-hermit-gdb-vfile-20260916`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dev-hermit-reverie-maturity--reverie-branch-dev-hermit-reverie-passed-counts-recovery `39b1a1d0c687` (`worktrees/slots/dev-hermit-reverie-maturity`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dev-hermit-sabre-capture-plan-20260918--wip `f96201a5bc6d` (`worktrees/slots/dev-hermit-sabre-capture-plan-20260918`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/dev-hermit-sabre-random-bootstrap-20260917--wip `ab4b6b623683` (`worktrees/slots/dev-hermit-sabre-random-bootstrap-20260917`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/integration-pr529-pr538-reverie--branch-archive-integration-pr529-pr538-old-04afeec5 `04afeec5f92a` (`worktrees/slots/integration-pr529-pr538-reverie`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/integration-pr529-pr538-reverie--wip `54a0fe04cd85` (`worktrees/slots/integration-pr529-pr538-reverie`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-bootstrap-log-20260918--wip `7124428fa0b4` (`worktrees/slots/kvm-bootstrap-log-20260918`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-child-future-box-01a0d5c2--wip `b22749a912ed` (`worktrees/slots/kvm-child-future-box-01a0d5c2`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-close-lowword-20260922--wip `b196f4dbcb2d` (`worktrees/slots/kvm-close-lowword-20260922`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-exec-replace--wip `606624de1928` (`worktrees/slots/kvm-exec-replace`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-fchmodat2 `fd91952195b3` (`worktrees/slots/kvm-fchmodat2`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-fchmodat2--wip `02bb070d22da` (`worktrees/slots/kvm-fchmodat2`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-fionread `03d13cc17b83` (`worktrees/slots/kvm-fionread`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-fstat-lowword-20260923 `0bca1c9eb4f6` (`worktrees/slots/kvm-fstat-lowword-20260923`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-mutation-trailing-slash `103f95458252` (`worktrees/slots/kvm-mutation-trailing-slash`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-pr611-recovery-20260922--wip `b23f51c0db65` (`worktrees/slots/kvm-pr611-recovery-20260922`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-proc-carrier-auth-20260921--wip `c59727ab6ff0` (`worktrees/slots/kvm-proc-carrier-auth-20260921`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-proc-fd-identity-20260917--bundle-v22-before-forward `9dfffd7ef114` (`worktrees/slots/kvm-proc-fd-identity-20260917`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-proc-fd-identity-20260917--wip `36f606504346` (`worktrees/slots/kvm-proc-fd-identity-20260917`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-random-access-01a0d5c2--wip `b721fe7cb8d6` (`worktrees/slots/kvm-random-access-01a0d5c2`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-read-pread-lowword-20260923--wip `bac893d290ff` (`worktrees/slots/kvm-read-pread-lowword-20260923`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-reverie-landing-20260918--branch-codex-kvm-parity-land-20260918 `0f09630c3d19` (`worktrees/slots/kvm-reverie-landing-20260918`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-reverie-landing-20260918--ignored-kvm-pselect6-sig64-base-negative--wip `c1c837b9c157` (`worktrees/slots/kvm-reverie-landing-20260918`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-reverie-landing-20260918--ignored-kvm-signal-cleanup-parallel-matrix-v1-head-checkout `ce442fce6b1e` (`worktrees/slots/kvm-reverie-landing-20260918`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-reverie-landing-20260918--wip `0c6d192dc6e7` (`worktrees/slots/kvm-reverie-landing-20260918`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-runtime-20260915-codex `137c82f8c4a0` (`worktrees/slots/kvm-runtime-20260915-codex`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-sendfile-lseek-lowword-20260923--wip `bd991fe3d2ab` (`worktrees/slots/kvm-sendfile-lseek-lowword-20260923`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-setitimer-reverie-20260918--wip `8045573d0230` (`worktrees/slots/kvm-setitimer-reverie-20260918`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-split-join-01a0d5c2--wip `f164897f5265` (`worktrees/slots/kvm-split-join-01a0d5c2`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-storage-fd-lowword-20260923--wip `5c735c6197f8` (`worktrees/slots/kvm-storage-fd-lowword-20260923`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-syncfs-identity-20260920--wip `ae2da2e04cae` (`worktrees/slots/kvm-syncfs-identity-20260920`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-teardown-opt--branch-codex-kvm-teardown-opt `210077770ae6` (`worktrees/slots/kvm-teardown-opt`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-teardown-reverie--branch-codex-kvm-teardown-opt `210077770ae6` (`worktrees/slots/kvm-teardown-reverie`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-thread-exit-order--branch-codex-kvm-thread-exit-order `d4b79b5d61de` (`worktrees/slots/kvm-thread-exit-order`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-user-access-read `48a80839bc29` (`worktrees/slots/kvm-user-access-read`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-vectored-io--wip `43c04f055d06` (`worktrees/slots/kvm-vectored-io`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/kvm-vfork-optimization--wip `8720a5af6740` (`worktrees/slots/kvm-vfork-optimization`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/liteinst-reverie-replay-20260922--wip `f31cc66dd4dd` (`worktrees/slots/liteinst-reverie-replay-20260922`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/liteinst-runtime-init-20260923--handoff-58a75be1 `58a75be1af8a` (`worktrees/slots/liteinst-runtime-init-20260923`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/liteinst-upstream-adoption-20260924 `4e9e11a9c7bf` (`worktrees/slots/liteinst-upstream-adoption-20260924`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/pr-drain-ci-signal-reverie-20260920 `b344a90bc0db` (`worktrees/slots/pr-drain-ci-signal-reverie-20260920`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/pr-drain-ci-signal-reverie-20260920--wip `f834e1d96604` (`worktrees/slots/pr-drain-ci-signal-reverie-20260920`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/review-520b--wip `c3a3e05506cd` (`worktrees/slots/review-520b`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/review-521-codex--wip `6ff825be83d0` (`worktrees/slots/review-521-codex`)
- https://github.com/rrnewton/reverie/tree/rescue/slots-20260928/validate-fresh-9e2bb9a4e74c-888428-dc7ef180 `9e2bb9a4e74c` (`worktrees/validate/validate-fresh-9e2bb9a4e74c-888428-dc7ef180`)

### agent-utils (10)

- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/ci-signal-au-history-20260919 `2a1cb9001801` (`worktrees/slots/ci-signal-au-history-20260919`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/ci-signal-au-history-integration-20260920--branch-codex-ci-signal-au-history-20260919 `2a1cb9001801` (`worktrees/slots/ci-signal-au-history-integration-20260920`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/ci-signal-au-history-integration-20260920--branch-codex-ci-signal-au-history-validated-0009-20260920 `0009dc153d53` (`worktrees/slots/ci-signal-au-history-integration-20260920`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/dev-hermit-ci-provenance-20260915--hermit-agent-utils `a640925d4ae4` (`worktrees/slots/dev-hermit-ci-provenance-20260915`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/dev-hermit-ops-authority-20260915 `7b8e342b06d6` (`worktrees/slots/dev-hermit-ops-authority-20260915`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/dev-hermit-ops-authority-20260915--wip `a1378300830a` (`worktrees/slots/dev-hermit-ops-authority-20260915`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/dev-hermit-verify-results-20260915 `4e11fa6fa1c6` (`worktrees/slots/dev-hermit-verify-results-20260915`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/dev-hermit-verify-results-20260915--wip `e43901f25e21` (`worktrees/slots/dev-hermit-verify-results-20260915`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/parent-receipt-snapshot-repair--hermit-agent-utils `e1aadb82dd98` (`worktrees/slots/parent-receipt-snapshot-repair`)
- https://github.com/rrnewton/agent-utils/tree/rescue/slots-20260928/tick-2--hermit-agent-utils--branch-tick-2-tick-hub-drop-non-drop `242b5c06ef63` (`worktrees/slots/tick-2`)

### liteinst2 (1)

- https://github.com/rrnewton/liteinst2/tree/rescue/slots-20260928/codex-liteinst-tail-01a0a13c--wip `8b2ed9c15703` (`worktrees/slots/codex-liteinst-tail-01a0a13c`)

## All 322 assessed paths

Paths are relative to `/home/newton/work/dev-hermit/`. The `refs` column counts the rescue branches
recorded for the path. Each assessor's reasoning is in `slots.json`, field `reason`.

| Path | Kind | Registry agent | Verdict | refs |
|---|---|---|---|---:|
| `worktrees/140` | toplevel |  | REMOVE | 0 |
| `worktrees/147` | toplevel |  | REMOVE | 1 |
| `worktrees/bisect-si` | toplevel |  | REMOVE | 2 |
| `worktrees/ignored` | toplevel |  | REMOVE | 0 |
| `worktrees/investigate-cells-repeat-6938` | toplevel |  | REMOVE | 0 |
| `worktrees/pr2802-residual-evidence` | toplevel |  | REMOVE | 1 |
| `worktrees/review-pr2802-codex` | toplevel |  | REMOVE | 1 |
| `worktrees/review-pr2838-codex` | toplevel |  | REMOVE | 0 |
| `worktrees/review-pr2980-codex` | toplevel |  | REMOVE | 1 |
| `worktrees/scorecard-publish-validation-21bfbfee` | toplevel |  | REMOVE | 1 |
| `worktrees/scorecard-publish-validation-bad483` | toplevel |  | REMOVE | 1 |
| `worktrees/scorecard-publish-validation-base-a7d266d6` | toplevel |  | REMOVE | 0 |
| `worktrees/scorecard-publish-validation-base-ac2d` | toplevel |  | REMOVE | 0 |
| `worktrees/slots/astra-hermit-drafts` | slots | astra-hermit-drafts | REMOVE | 17 |
| `worktrees/slots/astra-hermit-ready` | slots | astra-hermit-ready | REMOVE | 4 |
| `worktrees/slots/astra-reverie` | slots | astra-reverie | REMOVE | 2 |
| `worktrees/slots/cell-activation` | slots | cell-activation | REMOVE | 3 |
| `worktrees/slots/chat-pin-d81` | slots |  | REMOVE | 0 |
| `worktrees/slots/ci-parallel` | slots | orc-d0f81544-dec0-4b41-9f04-d580882f1b99 | REMOVE | 2 |
| `worktrees/slots/ci-signal-au-history-20260919` | slots |  | REMOVE | 1 |
| `worktrees/slots/ci-signal-au-history-integration-20260920` | slots |  | REMOVE | 2 |
| `worktrees/slots/ci-signal-au-history-pin-20260920` | slots |  | REMOVE | 0 |
| `worktrees/slots/ci-signal-main-health-20260919` | slots |  | REMOVE | 1 |
| `worktrees/slots/ci-signal-nextest-cpu-20260920` | slots |  | REMOVE | 3 |
| `worktrees/slots/ci-signal-proccpu-emission-20260920` | slots |  | REMOVE | 4 |
| `worktrees/slots/ci-signal-ptrace-fatal-cleanup-20260920` | slots |  | REMOVE | 3 |
| `worktrees/slots/ci-signal-record-artifact-20260920` | slots |  | REMOVE | 1 |
| `worktrees/slots/ci-signal-reverie-run-identity-20260920` | slots |  | REMOVE | 1 |
| `worktrees/slots/ci-signal-rng-integration-20260920` | slots |  | REMOVE | 0 |
| `worktrees/slots/ci-signal-version-provenance-20260920` | slots |  | REMOVE | 2 |
| `worktrees/slots/claude-family-review-ten-retained` | slots | codex-auth | REMOVE | 1 |
| `worktrees/slots/claude-review-1` | slots | claude-review-1 | REMOVE | 0 |
| `worktrees/slots/cml-653r3` | slots | claude-coord-mega-lander | REMOVE | 0 |
| `worktrees/slots/cml-hermit-3233` | slots | claude-cml-w-3233 | REMOVE | 2 |
| `worktrees/slots/cml-hermit-3235` | slots | claude-cml-w-3235 | REMOVE | 0 |
| `worktrees/slots/cml-liteinst` | slots | claude-cml-w-liteinst | REMOVE | 0 |
| `worktrees/slots/codex-liteinst-01a0a13c` | slots | codex-liteinst-01a0a13c | REMOVE | 2 |
| `worktrees/slots/codex-liteinst-exec-01a0a13c` | slots | codex-liteinst-population-01a0a13c | REMOVE | 1 |
| `worktrees/slots/codex-liteinst-receipts-resume-01a0a13c` | slots | resume_slot_diagnosis | REMOVE | 1 |
| `worktrees/slots/codex-liteinst-resume-01a0a13c` | slots | codex-liteinst-resume-01a0a13c | REMOVE | 3 |
| `worktrees/slots/codex-liteinst-runtime-resume-01a0a13c` | slots | liteinst_population | REMOVE | 2 |
| `worktrees/slots/codex-liteinst-tail-01a0a13c` | slots | codex-liteinst-tail-01a0a13c | REMOVE | 1 |
| `worktrees/slots/codex-liteinst-tracing-01a0a13c` | slots | dev-hermit-15 | BLOCKED | 1 |
| `worktrees/slots/crosscheck-unadmitted` | slots | root_crosscheck_unadmitted | REMOVE | 1 |
| `worktrees/slots/dbt-diagnostic-stderr` | slots | dbt-diagnostic-stderr-impl | REMOVE | 1 |
| `worktrees/slots/demo08review` | slots | demo08review | REMOVE | 2 |
| `worktrees/slots/demos` | slots | demos | REMOVE | 6 |
| `worktrees/slots/demos-fix` | slots | demos-fix | REMOVE | 0 |
| `worktrees/slots/dev-hermit-15-liteinst-hermit-integration-01a0a13c` | slots | liteinst-hermit-integration-01a0a13c | REMOVE | 0 |
| `worktrees/slots/dev-hermit-15-liteinst-rcb-01a0a13c` | slots | dev-hermit-15-rcb | REMOVE | 1 |
| `worktrees/slots/dev-hermit-15-vdso-getrandom-01a0a13c` | slots | vdso_getrandom_design | REMOVE | 1 |
| `worktrees/slots/dev-hermit-au-offline-fetch-20260916` | slots | dev-hermit-au-offline-fetch | REMOVE | 1 |
| `worktrees/slots/dev-hermit-capture-integrity-20260919` | slots | capture_terminal_integrity | REMOVE | 0 |
| `worktrees/slots/dev-hermit-census-consumer-20260917-2` | slots | capture_api_review | REMOVE | 0 |
| `worktrees/slots/dev-hermit-ci-cardinality-fixtures-20260916` | slots | dev-hermit-ci-cardinality-fixtures | REMOVE | 1 |
| `worktrees/slots/dev-hermit-ci-gate-repairs-20260916` | slots | dev-hermit-ci-gate-repairs | REMOVE | 2 |
| `worktrees/slots/dev-hermit-ci-manifest-20260915` | slots | dev-hermit-ci-manifest | REMOVE | 3 |
| `worktrees/slots/dev-hermit-ci-provenance-20260915` | slots | dev-hermit-ci-provenance | REMOVE | 4 |
| `worktrees/slots/dev-hermit-dbt-error-cleanup-20260916` | slots | dev-hermit-dbt-error-cleanup | REMOVE | 1 |
| `worktrees/slots/dev-hermit-dbt-loader-observer-20260916` | slots | dev-hermit-dbt-loader-observer | REMOVE | 0 |
| `worktrees/slots/dev-hermit-deferred-capture-api-20260917` | slots | capture_deferred_api | REMOVE | 0 |
| `worktrees/slots/dev-hermit-doc-provenance-3093-20260918` | slots | root.make7_boxed_failure.parent_fixture_audit | REMOVE | 2 |
| `worktrees/slots/dev-hermit-gdb-replay-diagnostic-20260916` | slots | dev-hermit-gdb-replay-diagnostic | REMOVE | 4 |
| `worktrees/slots/dev-hermit-gdb-vfile-20260916` | slots | dev-hermit-gdb-vfile | REMOVE | 1 |
| `worktrees/slots/dev-hermit-gdb-vfile-pin-20260916` | slots | dev-hermit-gdb-vfile-pin | REMOVE | 2 |
| `worktrees/slots/dev-hermit-harness-journal-20260916` | slots | dev-hermit-harness-journal | REMOVE | 2 |
| `worktrees/slots/dev-hermit-hermetic-v3-20260916` | slots | dev-hermit-hermetic-v3 | REMOVE | 1 |
| `worktrees/slots/dev-hermit-hermit-8515-pin` | slots | codex-slot-builder-8515 | REMOVE | 1 |
| `worktrees/slots/dev-hermit-host-fetch-proxy-20260917` | slots | fetch_proxy_fix | REMOVE | 1 |
| `worktrees/slots/dev-hermit-kvm-native-feature-pin-20260919` | slots | dev-hermit-kvm-native-feature-pin | REMOVE | 1 |
| `worktrees/slots/dev-hermit-manifest-fixture-ledger-20260916` | slots | dev-hermit-manifest-fixture-ledger | REMOVE | 1 |
| `worktrees/slots/dev-hermit-ops-authority-20260915` | slots | dev-hermit-ops-authority | REMOVE | 2 |
| `worktrees/slots/dev-hermit-pinned-command-fixtures-20260916` | slots | dev-hermit-pinned-command-fixtures | REMOVE | 2 |
| `worktrees/slots/dev-hermit-pinned-guest-paths-20260916` | slots | dev-hermit-pinned-guest-paths | REMOVE | 2 |
| `worktrees/slots/dev-hermit-prepare-full-fixture-20260918` | slots | root | REMOVE | 1 |
| `worktrees/slots/dev-hermit-proc-locks-runtime-20260916` | slots | dev-hermit-proc-locks-runtime | REMOVE | 2 |
| `worktrees/slots/dev-hermit-regular-census-20260918` | slots | official_kvm_node_command | REMOVE | 1 |
| `worktrees/slots/dev-hermit-retry-fetch-fixtures-20260918` | slots | root.make7_boxed_failure.aborted_publication_control_design | REMOVE | 1 |
| `worktrees/slots/dev-hermit-reverie-maturity` | slots | dev-hermit | REMOVE | 17 |
| `worktrees/slots/dev-hermit-robust-exit-clocks-20260916` | slots | dev-hermit-robust-exit-clocks | REMOVE | 1 |
| `worktrees/slots/dev-hermit-rust-script-clippy-20260917` | slots | dev-hermit-rust-script-clippy | REMOVE | 1 |
| `worktrees/slots/dev-hermit-sabre-capture-plan-20260918` | slots | dev-hermit-sabre-capture-prereq | REMOVE | 1 |
| `worktrees/slots/dev-hermit-sabre-exec-bootstrap-continuation-20260917` | slots | dev-hermit-sabre-exec-bootstrap-continuation | REMOVE | 0 |
| `worktrees/slots/dev-hermit-sabre-getrandom-diagnostic-20260917` | slots | dev-hermit-sabre-getrandom-diagnostic | REMOVE | 1 |
| `worktrees/slots/dev-hermit-sabre-random-bootstrap-20260917` | slots | dev-hermit-sabre-random-bootstrap | REMOVE | 1 |
| `worktrees/slots/dev-hermit-sabre-random-bootstrap-pin-20260917` | slots | dev-hermit-sabre-random-bootstrap-pin | REMOVE | 0 |
| `worktrees/slots/dev-hermit-sabre-zlib-runtime-20260917` | slots | dev-hermit-sabre-zlib-runtime | REMOVE | 0 |
| `worktrees/slots/dev-hermit-scorecard-timeout-20260919` | slots | dev-hermit-scorecard-diagnostic | REMOVE | 1 |
| `worktrees/slots/dev-hermit-stdio-flags` | slots | astra_reverie | REMOVE | 13 |
| `worktrees/slots/dev-hermit-structured-results-20260918` | slots | make7_boxed_failure | REMOVE | 1 |
| `worktrees/slots/dev-hermit-submodule-fixture-producer-reuse-20260917` | slots | dev-hermit-submodule-fixture-producer-reuse | REMOVE | 2 |
| `worktrees/slots/dev-hermit-verify-results-20260915` | slots | dev-hermit-verify-results | REMOVE | 2 |
| `worktrees/slots/dev-hermit-verify-two-runs-20260915` | slots | dev-hermit-verify-two-runs | REMOVE | 1 |
| `worktrees/slots/drain-2` | slots | drain-2 | REMOVE | 12 |
| `worktrees/slots/drain-verify` | slots | drain-verify | REMOVE | 0 |
| `worktrees/slots/exact-test-artifact-verifier` | slots | exact-test-artifact-verifier | REMOVE | 1 |
| `worktrees/slots/fbsource-import-pr94-20260926` | slots | fbsource-import-opus | REMOVE | 1 |
| `worktrees/slots/fix-2953` | slots |  | REMOVE | 0 |
| `worktrees/slots/hard-green-candidate` | slots | release-worktree-submodule-fix | REMOVE | 0 |
| `worktrees/slots/hermetic-validate-v3-backlog-1` | slots | backlog-1 | REMOVE | 0 |
| `worktrees/slots/hermit-002` | slots | hermit-002 | REMOVE | 12 |
| `worktrees/slots/hermit-003` | slots | hermit-003 | REMOVE | 1 |
| `worktrees/slots/hermit-003-anchored-selection` | slots | hermit-003-anchored-selection | REMOVE | 2 |
| `worktrees/slots/hermit-2971-parent-landing` | slots | mega-lander-hermit-2971 | REMOVE | 1 |
| `worktrees/slots/hermit-agent-utils-pin-20260925` | slots | wrkslots-land-opus | REMOVE | 0 |
| `worktrees/slots/hermit-ecx-complete` | slots | kvm-ratchet-cpuid | REMOVE | 1 |
| `worktrees/slots/hermit-ecx-findings` | slots | kvm-ratchet-hermit | REMOVE | 1 |
| `worktrees/slots/hermit-record-clock-routing-20260923` | slots | mega-lander-recovery | REMOVE | 0 |
| `worktrees/slots/hermit-record-clock-routing-r2-20260924` | slots | hermit-core-megalander-r2 | REMOVE | 0 |
| `worktrees/slots/hermit-recorder-routing-resume-20260923` | slots | mega-lander-recovery-2 | REMOVE | 0 |
| `worktrees/slots/hermit-scorecard-env-identity-review-20260923` | slots | mega-lander-env-identity-review | REMOVE | 0 |
| `worktrees/slots/hermit-scorecard-gate-cost-20260923` | slots | mega-lander-scorecard-gate | REMOVE | 0 |
| `worktrees/slots/hermit-scorecard-gate-cost-v2-20260923` | slots | mega-lander-scorecard-gate-v2 | REMOVE | 0 |
| `worktrees/slots/integration-pr529-pr538-diagnostic` | slots | codex-measure-pr529-pr538-vectored | REMOVE | 1 |
| `worktrees/slots/integration-pr529-pr538-hermit` | slots | codex-pr529-pr538-hermit | REMOVE | 1 |
| `worktrees/slots/integration-pr529-pr538-reverie` | slots | codex-pr529-pr538-vectored | REMOVE | 2 |
| `worktrees/slots/inventory-ledger-mirror` | slots | inventory-1 | REMOVE | 1 |
| `worktrees/slots/kvm-bootstrap-log-20260918` | slots | signal-correction-review | REMOVE | 1 |
| `worktrees/slots/kvm-child-future-box-01a0d5c2` | slots | native-kvm-child-future-box-01a0d5c2 | REMOVE | 1 |
| `worktrees/slots/kvm-close-lowword-20260922` | slots | kvm_close_lowword | REMOVE | 1 |
| `worktrees/slots/kvm-closed-stdin-cdf828dd-source-20260917` | slots | kvm_cli_source | REMOVE | 0 |
| `worktrees/slots/kvm-container-segv-01a0d5c2` | slots | native-kvm-container-segv-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-detcore-rng-readv-01a0d5c2` | slots | kvm-detcore-rng-readv | REMOVE | 1 |
| `worktrees/slots/kvm-detcore-vectored` | slots | implement_detcore_vectored | REMOVE | 1 |
| `worktrees/slots/kvm-e9patch-tempguard-disarm-01a0d5c2` | slots | kvm-e9patch-tempguard-disarm-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-exec-replace` | slots | kvm-exec-replace | REMOVE | 1 |
| `worktrees/slots/kvm-fchdir-lowword-20260923` | slots | kvm-managed-fchdir | REMOVE | 0 |
| `worktrees/slots/kvm-fchmodat2` | slots | kvm-fchmodat2 | REMOVE | 2 |
| `worktrees/slots/kvm-finalize-profile` | slots | kvm-finalize-profile | REMOVE | 1 |
| `worktrees/slots/kvm-fionread` | slots | kvm-fionread | REMOVE | 1 |
| `worktrees/slots/kvm-frozen-cleanup-20260917` | slots | kvm_frozen_cleanup_impl | REMOVE | 3 |
| `worktrees/slots/kvm-fstat-lowword-20260923` | slots | kvm-managed | REMOVE | 1 |
| `worktrees/slots/kvm-gdb-watcher-01a0d5c2` | slots | kvm-gdb-watcher | REMOVE | 1 |
| `worktrees/slots/kvm-getdents64-lowword-20260923` | slots | kvm-managed-getdents64 | REMOVE | 0 |
| `worktrees/slots/kvm-guest-args` | slots | kvm-guest-args | REMOVE | 1 |
| `worktrees/slots/kvm-guest-args-parent` | slots | kvm-guest-args-parent | REMOVE | 1 |
| `worktrees/slots/kvm-hermetic-image-recovery-01a0d5c2` | slots | kvm-hermetic-image-recovery-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-injected-tool-error-01a0d5c2` | slots | kvm-injected-tool-error | REMOVE | 0 |
| `worktrees/slots/kvm-integration-20260915-codex` | slots | ci_budget_review | REMOVE | 0 |
| `worktrees/slots/kvm-main-examples-01a0d5c2` | slots | kvm-main-examples | REMOVE | 0 |
| `worktrees/slots/kvm-mutation-trailing-slash` | slots | kvm-mutation-trailing-slash | REMOVE | 1 |
| `worktrees/slots/kvm-next40-final` | slots | kvm-ratchet-next40-final | REMOVE | 1 |
| `worktrees/slots/kvm-next40-selection` | slots | kvm-ratchet-next40 | REMOVE | 0 |
| `worktrees/slots/kvm-owned-container-01a0d5c2` | slots | kvm-owned-container | REMOVE | 0 |
| `worktrees/slots/kvm-parallel-bound-01a0d5c2` | slots | kvm-parallel-bound | REMOVE | 0 |
| `worktrees/slots/kvm-parity-20260915-codex` | slots | codex-kvm-domain-hermit | REMOVE | 1 |
| `worktrees/slots/kvm-parity-ledger-parent-20260916` | slots | kvm_parity_ledger_parent | REMOVE | 1 |
| `worktrees/slots/kvm-parity-recovery-20260916` | slots | kvm_parity_recovery | REMOVE | 2 |
| `worktrees/slots/kvm-pr611-recovery-20260922` | slots | kvm_pr611_recovery | REMOVE | 1 |
| `worktrees/slots/kvm-pr628-publish-01a0d5c2` | slots | kvm | REMOVE | 0 |
| `worktrees/slots/kvm-pr628-resume-20260924` | slots | codex-kvm-pr628-resume-20260924 | REMOVE | 0 |
| `worktrees/slots/kvm-prctl-state` | slots | fix-kvm-prctl-state | REMOVE | 0 |
| `worktrees/slots/kvm-prejoin-hermit-20260917` | slots | kvm_prejoin_failure_impl | REMOVE | 1 |
| `worktrees/slots/kvm-proc-carrier-auth-20260921` | slots | kvm_proc_carrier_impl | REMOVE | 1 |
| `worktrees/slots/kvm-proc-fd-identity-20260917` | slots | kvm_reverie_queue | REMOVE | 2 |
| `worktrees/slots/kvm-ptrace-fatal-01a0d5c2` | slots | kvm-ptrace-fatal | REMOVE | 0 |
| `worktrees/slots/kvm-ptrace-fatal-impl-01a0d5c2` | slots | kvm-ptrace-fatal-impl | REMOVE | 0 |
| `worktrees/slots/kvm-ptrace-retirement-01a0d5c2` | slots | kvm-ptrace-retirement | REMOVE | 0 |
| `worktrees/slots/kvm-random-access-01a0d5c2` | slots | kvm-random-access | REMOVE | 1 |
| `worktrees/slots/kvm-random-carrier-01a0d5c2` | slots | kvm-random-carrier | REMOVE | 0 |
| `worktrees/slots/kvm-random-paths-01a0d5c2` | slots | kvm-random-paths | REMOVE | 0 |
| `worktrees/slots/kvm-random-stream-01a0d5c2` | slots | next-kvm-defect | REMOVE | 0 |
| `worktrees/slots/kvm-ratchet` | slots | kvm-ratchet | REMOVE | 5 |
| `worktrees/slots/kvm-ratchet-90-fix6` | slots | kvm-ratchet-90-fix6 | REMOVE | 1 |
| `worktrees/slots/kvm-ratchet-90-run2` | slots | kvm-ratchet-90-run2 | REMOVE | 0 |
| `worktrees/slots/kvm-raw-stderr-20260917` | slots | kvm_queue_delta | REMOVE | 2 |
| `worktrees/slots/kvm-read-pread-lowword-20260923` | slots | kvm_read_pread_lowword | REMOVE | 1 |
| `worktrees/slots/kvm-read-zero-count-01a0d5c2` | slots | kvm-read-zero-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-reverie-landing-20260918` | slots | dev-hermit-14 | REMOVE | 4 |
| `worktrees/slots/kvm-reviewed-landing-tools-01a0d5c2` | slots | kvm-reviewed-landing-tools-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-rng-gate-recovery-01a0d5c2` | slots | kvm-rng-gate-01a0d5c2 | REMOVE | 1 |
| `worktrees/slots/kvm-rng-landed-pin-01a0d5c2` | slots | native-kvm-rng-landed-pin-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-rng-singleton-01a0d5c2` | slots | kvm-rng-singleton-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-rng-user-access-01a0d5c2` | slots | kvm-rng-user-access | REMOVE | 1 |
| `worktrees/slots/kvm-runtime-20260915-codex` | slots | codex-kvm-domain-runtime | REMOVE | 1 |
| `worktrees/slots/kvm-scalar-read-user-access-01a0d5c2` | slots | kvm-scalar-read-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-script-isolation-01a0d5c2` | slots | kvm-script-isolation-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-select-21` | slots | kvm-select-21 | REMOVE | 0 |
| `worktrees/slots/kvm-sendfile-lseek-lowword-20260923` | slots | kvm_sendfile_lseek_lowword | REMOVE | 1 |
| `worktrees/slots/kvm-setitimer-reverie-20260918` | slots | kvm_alarm_pending_impl | BLOCKED | 1 |
| `worktrees/slots/kvm-signal-abi` | slots | kvm-signal-abi | REMOVE | 2 |
| `worktrees/slots/kvm-split-join-01a0d5c2` | slots | kvm-next-kvm-defect-01a0d5c2 | REMOVE | 1 |
| `worktrees/slots/kvm-stack-checkout-release` | slots | kvm-stack-checkout-release | REMOVE | 0 |
| `worktrees/slots/kvm-stack-token` | slots | kvm-stack-token | REMOVE | 0 |
| `worktrees/slots/kvm-stdin-cancel-01a0d5c2` | slots | kvm-stdin-cancel-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-stdin-cet-01a0d5c2` | slots | native-kvm-stdin-cet-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-stdin-ubuntu-01a0d5c2` | slots | native-kvm-stdin-ubuntu-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/kvm-storage-fd-lowword-20260923` | slots | kvm_storage_fd_lowword | REMOVE | 1 |
| `worktrees/slots/kvm-summary-main-20260917` | slots | kvm-summary-main | REMOVE | 2 |
| `worktrees/slots/kvm-summary-publication-20260917` | slots | kvm_summary_publication_impl | REMOVE | 1 |
| `worktrees/slots/kvm-syncfs-auth-20260921` | slots | syncfs_auth_impl | REMOVE | 0 |
| `worktrees/slots/kvm-syncfs-identity-20260920` | slots | kvm_syncfs_impl | REMOVE | 1 |
| `worktrees/slots/kvm-syscall-benchmark` | slots | kvm-ratchet-bench | REMOVE | 0 |
| `worktrees/slots/kvm-sysinfo` | slots | kvm-sysinfo | REMOVE | 1 |
| `worktrees/slots/kvm-sysinfo-tool-2979` | slots |  | REMOVE | 0 |
| `worktrees/slots/kvm-teardown-opt` | slots | kvm-teardown-opt | REMOVE | 2 |
| `worktrees/slots/kvm-teardown-reverie` | slots | kvm-teardown-opt-reverie | REMOVE | 1 |
| `worktrees/slots/kvm-thread-exit-order` | slots | kvm-ratchet-thread-exit | REMOVE | 1 |
| `worktrees/slots/kvm-thread-exit-order-hermit` | slots | kvm-ratchet-thread-exit-hermit | REMOVE | 1 |
| `worktrees/slots/kvm-user-access-read` | slots | kvm-user-access-read | REMOVE | 1 |
| `worktrees/slots/kvm-usercopy-write-01a0d5c2` | slots | kvm-usercopy-write | REMOVE | 0 |
| `worktrees/slots/kvm-validation-tool-root` | slots | kvm_15_preflight | REMOVE | 0 |
| `worktrees/slots/kvm-vectored-io` | slots | implement_kvm_vectored_io | REMOVE | 1 |
| `worktrees/slots/kvm-vfork-optimization` | slots | kvm-vfork-optimization | REMOVE | 1 |
| `worktrees/slots/lander-2` | slots | lander-2 | REMOVE | 7 |
| `worktrees/slots/lander-2-seven-gate-receipts` | slots | lander-2-seven-gate-receipts | REMOVE | 1 |
| `worktrees/slots/ledger-fix-design` | slots | ledger_fix_design | REMOVE | 3 |
| `worktrees/slots/liteinst-late-timer-signal-20260925` | slots | liteinst-lane-claude | REMOVE | 0 |
| `worktrees/slots/liteinst-owner` | slots | owner | REMOVE | 1 |
| `worktrees/slots/liteinst-ratchet` | slots |  | REMOVE | 35 |
| `worktrees/slots/liteinst-reverie-replay-20260922` | slots | liteinst-replay | REMOVE | 1 |
| `worktrees/slots/liteinst-runtime-init-20260923` | slots | liteinst-managed | REMOVE | 1 |
| `worktrees/slots/liteinst-session-recovery` | slots | liteinst-session-recovery | REMOVE | 1 |
| `worktrees/slots/liteinst-target-loader-ordinary-20260923` | slots | codex-liteinst-ordinary-01a0cc92 | REMOVE | 0 |
| `worktrees/slots/liteinst-target-loader-resolver-20260922` | slots | codex-liteinst-target-loader-01a0cc92 | REMOVE | 0 |
| `worktrees/slots/liteinst-tools-01a0d5c2` | slots | liteinst-tools-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/liteinst-upstream-adoption-20260924` | slots | liteinst-recovery-r3 | REMOVE | 1 |
| `worktrees/slots/liteinst2-concurrent-overlap-20260923` | slots | liteinst-managed-upstream | REMOVE | 0 |
| `worktrees/slots/liteinst2-entry-backedge-01a0d5c2` | slots | liteinst-backedge-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/liteinst2-external-cet-01a0d5c2` | slots | liteinst-external-cet-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/liteinst2-pc-relative-01a0d099` | slots | codex-liteinst2-pcrel-01a0d099 | REMOVE | 0 |
| `worktrees/slots/liteinst2-replace-first-cet-01a0d5c2` | slots | liteinst-replace-cet-01a0d5c2 | REMOVE | 0 |
| `worktrees/slots/liteinst2-sigtrap-origin-01a0d099` | slots | codex-liteinst2-sigtrap-01a0d099 | BLOCKED | 0 |
| `worktrees/slots/liteinst2-upstream-20260922` | slots | liteinst | REMOVE | 0 |
| `worktrees/slots/liveness-agent-utils` | slots | liveness_agent_utils | REMOVE | 0 |
| `worktrees/slots/measure-intra-step-20260905` | slots | measure-intra-step-codex | REMOVE | 0 |
| `worktrees/slots/mega-ci-passed-count-activation-20260915` | slots | mega-ci-passed-count | REMOVE | 1 |
| `worktrees/slots/mega-ci-successor-20260915` | slots | mega-ci-successor | REMOVE | 2 |
| `worktrees/slots/mega-lander-admission-20260924-r4` | slots | mega-lander-admission-r4 | REMOVE | 0 |
| `worktrees/slots/mega-lander-agent-utils` | slots | liveness_agent_utils_successor | REMOVE | 0 |
| `worktrees/slots/mega-lander-agent-utils-git-boundary` | slots | mega-lander-agent-utils-git-boundary | REMOVE | 0 |
| `worktrees/slots/mega-lander-anchored-selection-land` | slots | mega-lander-anchored-selection | REMOVE | 1 |
| `worktrees/slots/mega-lander-hermit-2970` | slots | mega-lander-hermit-2970 | REMOVE | 1 |
| `worktrees/slots/mega-lander-ledger-migration-20260925-r4` | slots | mega-lander-ledger-migration-r4 | REMOVE | 0 |
| `worktrees/slots/mega-lander-main-census-repair-20260925-r1` | slots | codex-mega-lander-main-health | REMOVE | 1 |
| `worktrees/slots/mega-lander-parent-hermit-tool-pin-20260925-r1` | slots | codex-mega-lander-tool-pin-r1 | REMOVE | 0 |
| `worktrees/slots/mega-lander-proc-locks-20260924-r4` | slots | mega-lander-proc-locks-r4 | REMOVE | 0 |
| `worktrees/slots/mega-lander-producer-authority-20260925-r7` | slots | codex-mega-lander-producer-r7 | REMOVE | 0 |
| `worktrees/slots/mega-lander-producer-pins-20260925-r4` | slots | mega-lander-producer-pins-r4 | REMOVE | 0 |
| `worktrees/slots/mega-lander-recorder-20260924-r4` | slots | mega-lander-recorder-r4 | REMOVE | 0 |
| `worktrees/slots/mega-lander-recorder-integration-20260925-r4` | slots | mega-lander-recorder-integration-r4 | REMOVE | 0 |
| `worktrees/slots/mega-lander-recorder-integration-20260925-r5` | slots | mega-lander-recorder-integration-r5 | REMOVE | 0 |
| `worktrees/slots/mega-lander-reverie-main-restore-20260925-r1` | slots | mega-lander-reverie-restore-r1 | REMOVE | 0 |
| `worktrees/slots/mega-lander-sabre-health-20260925-r4` | slots | mega-lander-sabre-health-r4 | REMOVE | 0 |
| `worktrees/slots/mega-lander-scorecard-20260924-r4` | slots | mega-lander-scorecard-r4 | REMOVE | 2 |
| `worktrees/slots/mega-lander-scorecard-gate-diagnosis-20260925-r5` | slots | mega-lander-scorecard-gate-diagnosis-r5 | REMOVE | 4 |
| `worktrees/slots/mega-lander-scorecard-gate-integration-20260925-r6` | slots | codex-mega-lander | REMOVE | 55 |
| `worktrees/slots/parent-finalizer-canonical-dag` | slots | mega-lander-finalizer-canonical-dag | REMOVE | 1 |
| `worktrees/slots/parent-receipt-snapshot-repair` | slots | astra_parent_repair | REMOVE | 5 |
| `worktrees/slots/parity-scorecard` | slots | codex-parity-scorecard | REMOVE | 0 |
| `worktrees/slots/passed-tests-receipts` | slots | implement-required-passed-tests | REMOVE | 2 |
| `worktrees/slots/pin-validate-tool-root` | slots | agent_utils_pin_followup | REMOVE | 1 |
| `worktrees/slots/pr-drain-1-hermit` | slots | pr-drain-1 | REMOVE | 1 |
| `worktrees/slots/pr-drain-ci-signal-cell-history-authority-20260920` | slots | pr-drain-cell-history-authority | REMOVE | 1 |
| `worktrees/slots/pr-drain-ci-signal-hermit-20260920` | slots | codex-pr-drain | REMOVE | 1 |
| `worktrees/slots/pr-drain-ci-signal-hermit-admission-fixtures-20260920` | slots | codex-pr-drain-admission-fixtures | REMOVE | 1 |
| `worktrees/slots/pr-drain-ci-signal-hermit-portable-20260920` | slots | codex-pr-drain-portable | REMOVE | 2 |
| `worktrees/slots/pr-drain-ci-signal-parent-scorecard-20260920` | slots |  | REMOVE | 1 |
| `worktrees/slots/pr-drain-ci-signal-reverie-20260920` | slots | codex-pr-drain-kvm | REMOVE | 2 |
| `worktrees/slots/pressure-writeback-path` | slots | pressure-writeback-path | REMOVE | 2 |
| `worktrees/slots/procfs-mountinfo-determinism-20260925` | slots |  | REMOVE | 0 |
| `worktrees/slots/propagate-reason-hermit` | slots | propagate-reason-hermit | REMOVE | 1 |
| `worktrees/slots/propagate-reason-parent` | slots | propagate-reason-parent | REMOVE | 2 |
| `worktrees/slots/ptrace-red-opus-20260924` | slots | ptrace-red-opus-20260924 | REMOVE | 1 |
| `worktrees/slots/ptrace-red-watermelon-20260924` | slots | ptrace-red-watermelon-20260924 | REMOVE | 1 |
| `worktrees/slots/recon-2-plugin-workflow-fix` | slots | recon-2 | REMOVE | 3 |
| `worktrees/slots/reverie-520-fix` | slots | reverie-520-fix | REMOVE | 0 |
| `worktrees/slots/reverie-bot-selfhosted-dispatch-20260923` | slots | kvm-managed-selfhosted | REMOVE | 0 |
| `worktrees/slots/reverie-bot-selfhosted-dispatch-v2-20260923` | slots | kvm-managed-selfhosted-v2 | REMOVE | 0 |
| `worktrees/slots/reverie-cpuid` | slots | reverie-cpuid | REMOVE | 0 |
| `worktrees/slots/reverie-exact-count-pin` | slots | reverie-exact-count-pin | REMOVE | 0 |
| `worktrees/slots/review-285` | slots | review-285 | REMOVE | 0 |
| `worktrees/slots/review-2956` | slots | review-2956 | REMOVE | 0 |
| `worktrees/slots/review-520` | slots | review-520 | REMOVE | 2 |
| `worktrees/slots/review-520b` | slots | review-520b | REMOVE | 1 |
| `worktrees/slots/review-521-codex` | slots | review-521-codex | REMOVE | 1 |
| `worktrees/slots/review-524-second` | slots | review-524-second | REMOVE | 0 |
| `worktrees/slots/review-cpuid` | slots | review-cpuid | REMOVE | 3 |
| `worktrees/slots/review-reverie-525-exact` | slots | review_reverie_525_exact | REMOVE | 0 |
| `worktrees/slots/review-reverie-526-exact` | slots | review_reverie_526_exact | REMOVE | 0 |
| `worktrees/slots/reviewer-3` | slots | reviewer-3 | REMOVE | 0 |
| `worktrees/slots/reviewer-3-hermit` | slots | reviewer-3-hermit | REMOVE | 2 |
| `worktrees/slots/run1750-fix-source-00ea6c9b` | slots | validate-run1750-fix | REMOVE | 1 |
| `worktrees/slots/sabre-parity-events-20260925` | slots | scorecard-review-opus | REMOVE | 0 |
| `worktrees/slots/scorecard-cold-context` | slots | scorecard-web | REMOVE | 1 |
| `worktrees/slots/scorecard-generator-sept14` | slots | scorecard-publish-generator | REMOVE | 2 |
| `worktrees/slots/scorecard-pages` | slots | degraded-scorecard-website | REMOVE | 1 |
| `worktrees/slots/scorecard-pin-publish-20260924` | slots | codex-scorecard-pin-publish | REMOVE | 1 |
| `worktrees/slots/scorecard-publish-sept14` | slots | scorecard-publish | REMOVE | 3 |
| `worktrees/slots/scorecard-site-repair-20260924` | slots | scorecard-site-repair | REMOVE | 0 |
| `worktrees/slots/scorecard-website` | slots | scorecard-website | REMOVE | 6 |
| `worktrees/slots/series-rows` | slots |  | REMOVE | 1 |
| `worktrees/slots/service-result-passed-tests` | slots | service-result-passed-tests-impl | REMOVE | 2 |
| `worktrees/slots/site-plan` | slots | site-plan | REMOVE | 1 |
| `worktrees/slots/slots-recon` | slots | slots-recon | REMOVE | 2 |
| `worktrees/slots/status-publisher-schema` | slots | dag-source-review | REMOVE | 1 |
| `worktrees/slots/test-harness-help` | slots | herdr-cross-base-fix | REMOVE | 0 |
| `worktrees/slots/tfa-poc-20260919` | slots | codex-root-tfa-poc | REMOVE | 1 |
| `worktrees/slots/tick-2` | slots | tick-2 | REMOVE | 7 |
| `worktrees/slots/tick-2-cli-skill` | slots | tick-2-cli-skill | REMOVE | 1 |
| `worktrees/slots/tick-repair` | slots | tick-repair | REMOVE | 0 |
| `worktrees/slots/timerfd-family-identity-20260925` | slots | alpine-quasar | REMOVE | 1 |
| `worktrees/slots/timerfd-family-identity-opus-r2-20260925` | slots | timerfd-recovery-opus-v2 | REMOVE | 1 |
| `worktrees/slots/timerfd-family-identity-r2-20260925` | slots | ptrace-watermelon-r2 | REMOVE | 0 |
| `worktrees/slots/tree-aware-signal-crosscheck` | slots | tree-aware-signal-crosscheck | REMOVE | 1 |
| `worktrees/slots/validate-cleanup-consumer` | slots | agent_utils_followup_review | REMOVE | 0 |
| `worktrees/slots/validate-current-tip` | slots | validate-current-tip | REMOVE | 1 |
| `worktrees/slots/validate-retained-cleanup` | slots | kvm-ratchet-cleanup | REMOVE | 0 |
| `worktrees/slots/validate-status-history` | slots | mega-lander | REMOVE | 6 |
| `worktrees/slots/validate-status-isolation` | slots | mega-lander-handle-isolation | REMOVE | 4 |
| `worktrees/slots/validation-removal-proof` | slots | removal-proof-api | REMOVE | 0 |
| `worktrees/slots/verify-two-runs-v4` | slots | verify-two-runs-v4-impl | REMOVE | 1 |
| `worktrees/slots/wrkslots-create-demo-20260914` | slots | wrkslots-create-demo-20260914 | REMOVE | 2 |
| `worktrees/slots/wrkslots-registry-repair-parent` | slots | root_implement_registry_repair_parent | REMOVE | 0 |
| `worktrees/tool-roots/pr2971-validation-e8e5e81d` | tool-roots |  | REMOVE | 0 |
| `worktrees/tool-roots/scorecard-publish-bad483` | tool-roots |  | REMOVE | 0 |
| `worktrees/toolroot-cleanup-3` | toplevel |  | REMOVE | 0 |
| `worktrees/utility-2176` | toplevel |  | REMOVE | 2 |
| `worktrees/utility-2223` | toplevel |  | REMOVE | 1 |
| `worktrees/utility-2680` | toplevel |  | REMOVE | 1 |
| `worktrees/validate/validate-fresh-9e2bb9a4e74c-888428-dc7ef180` | validate | validate-validate-fresh-9e2bb9a4e74c-888428-dc7ef180 | REMOVE | 1 |
| `worktrees/validate/validate-fresh-fdd8f55a8e7c-4065575-6c273f68` | validate | validate-validate-fresh-fdd8f55a8e7c-4065575-6c273f68 | REMOVE | 0 |
