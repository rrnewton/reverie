# Index of finding-bearing content under `ignored/`

Built 2026-08-15 by hermit-coord (devbig030). **This is an INDEX, not a copy.**

## WHY AN INDEX RATHER THAN PROMOTION

`ignored/` is gitignored, so everything below survives agent context loss but **not host
loss**, and nothing in the system will ever promote any of it — it happens only when
someone asks. A sweep found **445 finding-bearing files**. Almost none of them are mine.

The large groups are per-investigation working directories belonging to other agents' lanes.
**Promoting another agent's working files into a landing I own would be absorption, not
durability** — and it would assert on their behalf that the content is finished and correct,
which I cannot know from the file. So this records WHERE the content is and leaves the
promotion decision with each owner.

**An index is durable against host loss for the KNOWLEDGE THAT THE CONTENT EXISTED AND
WHERE. It is not durable for the content.** If this host is lost, these files are lost and
this index becomes a list of what was lost. That is strictly better than silence and it is
not a substitute for promotion.

## METHOD, SO ITS NEGATIVES CAN BE CHALLENGED

Enumerate `.md`/`.tsv`/`.csv`/`.txt` at depth 1 of `ignored/`, plus depth<=3 inside
subdirectories that are NOT checkouts (no `.git`, no `scripts/`, no `hermit/`); exclude
`target/`, `node_modules/`, `btrfs-progs/`; exclude any file whose basename matches one of
the 3037 basenames tracked on `origin/main` (this drops clone copies of `AGENTS.md`,
`README.md` and friends); keep files of >=20 lines.

**KNOWN-POSITIVE CHECK RUN BEFORE ANY NEGATIVE WAS BELIEVED:** the method finds
`enotconn-env-gate.md`, `liteinst-mechanical-cells.tsv` and `liteinst-exit-gating-screen.tsv`
— three files promoted earlier and known present — so it is not silently dropping content.

**KNOWN LIMITS:** depth 3, four extensions, and the >=20-line floor. Content deeper, in
another format, or shorter is NOT indexed here and is not claimed absent.

## THE VOLUME IS THE FINDING

445 files across every lane, and **no tracked index of any of it existed before this one** —
confirmed by `git grep` for `ignored/evidence` on `origin/main`, which returns nothing.
`ignored/evidence/` alone holds **68 deliberate capture directories** with their own
`MANIFEST.md` files, i.e. content someone took the trouble to structure as evidence, none of
it discoverable from the repository. **Promoting a handful would not change the mechanism.**

## INDEX

Indexed 445 finding-bearing files across 29 groups.

## DURABILITY UPDATE — 2026-09-22

Three dirty product checkouts named by this index were preserved on rescue
branches without rebasing their historical bases. The commit URLs below are the
content-addressed authority; the branch URLs are the human-readable entry
points. The old `ignored/` checkout paths are no longer the durable reference.

### SaBRe stack-ownership diagnostic

- Rescue branch: <https://github.com/rrnewton/hermit/tree/rescue/stackowner-52f75645-20260922>
- Exact commit: <https://github.com/rrnewton/hermit/commit/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47>
- Preserved source: [`hermit-cli/src/lib.rs`](https://github.com/rrnewton/hermit/blob/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47/hermit-cli/src/lib.rs), [`hermit-cli/src/sabre_ptrace.rs`](https://github.com/rrnewton/hermit/blob/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47/hermit-cli/src/sabre_ptrace.rs), and [`hermit-cli/src/sabre_stack_capture.rs`](https://github.com/rrnewton/hermit/blob/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47/hermit-cli/src/sabre_stack_capture.rs)
- The uncommitted diagnostic executable at `target-diag/debug/hermit` had SHA-256 `4e7f8b4c658a89c3d18c2d9c4194ac35f1d462a8937f666e74b674214a2da3a7`. The binary is identified here but deliberately not committed.
- Scope boundary: `instr_on/dumps/manifest.tsv` and `probe/dumps/manifest.tsv` are outside the rescued Git checkout. They remain host-local evidence and have no rescue-commit URL.

### Detcore deterministic-hash-seed experiment

- Rescue branch: <https://github.com/rrnewton/hermit/tree/rescue/detcore-hashseed-52f75645-20260922>
- Exact commit (the authoritative 13-path file list and diff): <https://github.com/rrnewton/hermit/commit/13ac7f4ccd43a8de93b202254428804de09fd54e>

### Reverie deterministic-hash-seed experiment

- Rescue branch: <https://github.com/rrnewton/reverie/tree/rescue/reverie-hashseed-c261050c-20260922>
- Exact commit: <https://github.com/rrnewton/reverie/commit/8d561fc6da9210170ee8ea794fcccc4525db1452>
- Preserved source: [`experimental/reverie-sabre/src/reverie_adapter.rs`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/experimental/reverie-sabre/src/reverie_adapter.rs) and [`reverie/src/backtrace/cache.rs`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/reverie/src/backtrace/cache.rs)

### `ignored/scratch/` — 107 files, newest 08-14 22:04
- `scratch/hermit-2265-teardown-arm-3641632a/docs/ARCHITECTURE.md` — 868 lines, 08-14 20:51
- `scratch/hermit-2265-two-gaps-3641632a/docs/ARCHITECTURE.md` — 868 lines, 08-14 21:17
- `scratch/hermit-liteinst-assert-audit-97d06af0a/docs/ARCHITECTURE.md` — 868 lines, 08-14 22:04
- `scratch/hermit-stack-diag-e3900865/docs/ARCHITECTURE.md` — 868 lines, 08-14 20:22
- `scratch/hermit-2265-teardown-arm-3641632a/docs/USER_GUIDE.md` — 693 lines, 08-14 20:51
- `scratch/hermit-2265-two-gaps-3641632a/docs/USER_GUIDE.md` — 693 lines, 08-14 21:17
- …and 101 more in this group

### `ignored/gate-remediation-options-20260815/` — 81 files, newest 08-14 22:55
- `gate-remediation-options-20260815/pressure-hermit/docs/ARCHITECTURE.md` — 868 lines, 08-14 22:54
- `gate-remediation-options-20260815/strict-hermit/docs/ARCHITECTURE.md` — 868 lines, 08-14 22:54
- `gate-remediation-options-20260815/validate-hermit/docs/ARCHITECTURE.md` — 868 lines, 08-14 22:54
- `gate-remediation-options-20260815/pressure-hermit/docs/USER_GUIDE.md` — 693 lines, 08-14 22:54
- `gate-remediation-options-20260815/strict-hermit/docs/USER_GUIDE.md` — 693 lines, 08-14 22:54
- `gate-remediation-options-20260815/validate-hermit/docs/USER_GUIDE.md` — 693 lines, 08-14 22:54
- …and 75 more in this group

### `ignored/rev-fable/` — 52 files, newest 08-13 09:23
- `rev-fable/h2129/docs/ARCHITECTURE.md` — 868 lines, 08-13 09:23
- `rev-fable/h2213/docs/ARCHITECTURE.md` — 868 lines, 08-13 09:23
- `rev-fable/h2129/docs/USER_GUIDE.md` — 677 lines, 08-13 09:23
- `rev-fable/h2213/docs/USER_GUIDE.md` — 677 lines, 08-13 09:23
- `rev-fable/h2129/WHATS_WORKING.md` — 640 lines, 08-13 09:23
- `rev-fable/h2213/WHATS_WORKING.md` — 640 lines, 08-13 09:23
- …and 46 more in this group

### `ignored/pr2069-codex-review/` — 50 files, newest 08-10 06:47
- `pr2069-codex-review/drift-tree-ba360ff3/docs/ARCHITECTURE.md` — 868 lines, 08-10 06:46
- `pr2069-codex-review/schema-tree-ba360ff3/docs/ARCHITECTURE.md` — 868 lines, 08-10 06:47
- `pr2069-codex-review/drift-tree-ba360ff3/docs/USER_GUIDE.md` — 669 lines, 08-10 06:46
- `pr2069-codex-review/schema-tree-ba360ff3/docs/USER_GUIDE.md` — 669 lines, 08-10 06:47
- `pr2069-codex-review/drift-tree-ba360ff3/WHATS_WORKING.md` — 630 lines, 08-10 06:46
- `pr2069-codex-review/schema-tree-ba360ff3/WHATS_WORKING.md` — 630 lines, 08-10 06:47
- …and 44 more in this group

### `ignored/evidence/` — 41 files, newest 08-14 20:41
- `evidence/devbig014-ledger-audit-20260814/REAL-VALIDATE-RUNS.tsv` — 251 lines, 08-14 10:01
- `evidence/devbig014-validate-coverage-distribution-20260814/PER-RUN-COVERAGE.tsv` — 251 lines, 08-14 10:19
- `evidence/writev-determinism-e3900865-20260815/population-results.tsv` — 129 lines, 08-14 18:31
- `evidence/dbt-canonical-verdict-bracket-20260814/MANIFEST.md` — 118 lines, 08-14 20:41
- `evidence/sabre-stack-hash-divergence-52f75645-20260814/MANIFEST.md` — 67 lines, 08-14 14:33
- `evidence/dbt-wait-lifecycle-stack-hash-investigation-e3900865-20260815/lazy-binding-discriminator/pair-results.tsv` — 65 lines, 08-14 20:37
- …and 35 more in this group

### `ignored/stackowner-52f75645/` — 28 files, newest 08-14 15:22
- [`docs/ARCHITECTURE.md`](https://github.com/rrnewton/hermit/blob/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47/docs/ARCHITECTURE.md) — 868 lines, 08-14 15:14
- `stackowner-52f75645/instr_on/dumps/manifest.tsv` — 770 lines, 08-14 15:22
- [`docs/USER_GUIDE.md`](https://github.com/rrnewton/hermit/blob/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47/docs/USER_GUIDE.md) — 693 lines, 08-14 15:14
- [`WHATS_WORKING.md`](https://github.com/rrnewton/hermit/blob/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47/WHATS_WORKING.md) — 640 lines, 08-14 15:14
- [`docs/OCI_RUNTIME_DESIGN.md`](https://github.com/rrnewton/hermit/blob/fc400e293af5a4ad1fa3673c0284ad89f0cf6f47/docs/OCI_RUNTIME_DESIGN.md) — 519 lines, 08-14 15:14
- `stackowner-52f75645/probe/dumps/manifest.tsv` — 365 lines, 08-14 15:19
- …and 22 more in this group

### `ignored/sigsegv-diag/` — 26 files, newest 08-14 19:46
- `sigsegv-diag/hermit-base/docs/ARCHITECTURE.md` — 868 lines, 08-14 19:46
- `sigsegv-diag/hermit-base/docs/USER_GUIDE.md` — 693 lines, 08-14 19:46
- `sigsegv-diag/hermit-base/WHATS_WORKING.md` — 640 lines, 08-14 19:46
- `sigsegv-diag/hermit-base/docs/OCI_RUNTIME_DESIGN.md` — 519 lines, 08-14 19:46
- `sigsegv-diag/hermit-base/docs/TESTING_ENVIRONMENTS.md` — 342 lines, 08-14 19:46
- `sigsegv-diag/hermit-base/docs/agent-debugging-vision.md` — 304 lines, 08-14 19:46
- …and 20 more in this group

### `ignored/(top level)/` — 13 files, newest 08-15 06:16
- `DEVLOG_hermit.md` — 5967 lines, 07-28 10:29
- `overnight-report-2026-08-15.md` — 817 lines, 08-15 06:16
- `gate-manifest-sample2.txt` — 472 lines, 08-14 21:50
- `PROMPT_RECAP_20260726.md` — 295 lines, 07-26 20:04
- `liteinst-exit-gating-screen.tsv` — 272 lines, 08-15 03:51
- `2026-08-13-compat-ratchet-sprint.md` — 247 lines, 08-14 10:31
- …and 7 more in this group

### `ignored/scorecard-detlog-tier.QuaFnn/` — 7 files, newest 08-08 05:55
- `scorecard-detlog-tier.QuaFnn/repo/hermit/WHATS_WORKING.md` — 630 lines, 08-08 05:55
- `scorecard-detlog-tier.QuaFnn/repo/reverie/BACKENDS.md` — 245 lines, 08-08 05:55
- `scorecard-detlog-tier.QuaFnn/repo/liteinst2/PATCH_SITE_DECISION_TREE.md` — 177 lines, 08-08 05:55
- `scorecard-detlog-tier.QuaFnn/repo/hermit/CONTRIBUTING.md` — 98 lines, 08-08 05:55
- `scorecard-detlog-tier.QuaFnn/repo/hermit/CODE_OF_CONDUCT.md` — 80 lines, 08-08 05:55
- `scorecard-detlog-tier.QuaFnn/repo/reverie/CODE_OF_CONDUCT.md` — 80 lines, 08-08 05:55
- …and 1 more in this group

### `ignored/reverie-hashseed-c261050c/` — 6 files, newest 08-14 16:16
- [`BACKENDS.md`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/BACKENDS.md) — 245 lines, 08-14 16:16
- [`docs/BACKEND_SOURCES.md`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/docs/BACKEND_SOURCES.md) — 116 lines, 08-14 16:16
- [`experimental/dbt-strace-vs-ptrace.md`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/experimental/dbt-strace-vs-ptrace.md) — 89 lines, 08-14 16:16
- [`CODE_OF_CONDUCT.md`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/CODE_OF_CONDUCT.md) — 80 lines, 08-14 16:16
- [`docs/MERGE_QUEUE.md`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/docs/MERGE_QUEUE.md) — 52 lines, 08-14 16:16
- [`CONTRIBUTING.md`](https://github.com/rrnewton/reverie/blob/8d561fc6da9210170ee8ea794fcccc4525db1452/CONTRIBUTING.md) — 46 lines, 08-14 16:16

### `ignored/drain-triage-20260812/` — 5 files, newest 08-12 10:44
- `drain-triage-20260812/note-2188.md` — 23 lines, 08-12 10:44
- `drain-triage-20260812/close-2194.md` — 22 lines, 08-12 10:43
- `drain-triage-20260812/note-2164.md` — 22 lines, 08-12 10:44
- `drain-triage-20260812/note-2191.md` — 22 lines, 08-12 10:44
- `drain-triage-20260812/close-2195.md` — 20 lines, 08-12 10:43

### `ignored/recovery/` — 4 files, newest 08-08 08:16
- `recovery/20260806-parent-cleanup/slot-destruction-investigation_20260804.md` — 130 lines, 08-04 20:34
- `recovery/scorecard-pr1976-20260808/MANIFEST.md` — 59 lines, 08-08 05:31
- `recovery/scorecard-pr1976-20260808/appended-rows.csv` — 28 lines, 08-08 05:32
- `recovery/reviewer-2-profiles-20260805/MANIFEST.md` — 27 lines, 08-08 08:16

### `ignored/ci-hub-changes/` — 3 files, newest 08-14 19:53
- `ci-hub-changes/land-pr-soft-land-20260814/hermit/WHATS_WORKING.md` — 640 lines, 08-14 19:53
- `ci-hub-changes/land-pr-soft-land-20260814/hermit/CONTRIBUTING.md` — 98 lines, 08-14 19:53
- `ci-hub-changes/land-pr-soft-land-20260814/hermit/CODE_OF_CONDUCT.md` — 80 lines, 08-14 19:53

### `ignored/current-main-validation.pQM1PCJB/` — 3 files, newest 08-08 06:25
- `current-main-validation.pQM1PCJB/parent/hermit/WHATS_WORKING.md` — 630 lines, 08-08 06:25
- `current-main-validation.pQM1PCJB/parent/hermit/CONTRIBUTING.md` — 98 lines, 08-08 06:25
- `current-main-validation.pQM1PCJB/parent/hermit/CODE_OF_CONDUCT.md` — 80 lines, 08-08 06:25

### `ignored/inert40/` — 3 files, newest 08-15 01:35
- `inert40/binary-split.tsv` — 40 lines, 08-15 01:07
- `inert40/observable.tsv` — 39 lines, 08-15 01:07
- `inert40/expected-derivation.tsv` — 35 lines, 08-15 01:35

### `ignored/coord-fable-salvage/` — 2 files, newest 08-09 23:09
- `coord-fable-salvage/consistency-pass2/2026-08-10-guidance-consistency-audit-pass2.md` — 312 lines, 08-09 23:09
- `coord-fable-salvage/cadence2-scorecard/dbi.csv` — 197 lines, 08-09 18:03

### `ignored/ec38/` — 2 files, newest 08-15 01:38
- `ec38/twostep2.tsv` — 38 lines, 08-15 01:38
- `ec38/twostep.tsv` — 38 lines, 08-15 01:37

### `ignored/btrfs-b3-validate_20260728/` — 1 files, newest 07-28 13:57
- `btrfs-b3-validate_20260728/FINDINGS.md` — 88 lines, 07-28 13:57

### `ignored/btrfs-kernel-build/` — 1 files, newest 07-28 13:21
- `btrfs-kernel-build/artifacts/MANIFEST.md` — 83 lines, 07-28 13:21

### `ignored/btrfs-progs-v7.1/` — 1 files, newest 07-14 15:41
- `btrfs-progs-v7.1/TODO.md` — 105 lines, 07-14 15:41

### `ignored/btrfs-progs-v7.1-track-b-272/` — 1 files, newest 07-14 15:41
- `btrfs-progs-v7.1-track-b-272/TODO.md` — 105 lines, 07-14 15:41

### `ignored/census-manifest-touching-open-prs-20260815/` — 1 files, newest 08-15 12:18
- `census-manifest-touching-open-prs-20260815/dev-hermit/ai_docs/open-hermit-pr-manifest-census-20260815.tsv` — 79 lines, 08-15 12:18

### `ignored/ec26/` — 1 files, newest 08-15 02:34
- `ec26/twostep.tsv` — 26 lines, 08-15 02:34

### `ignored/hermit-replay-host-identity-bd54/` — 1 files, newest 08-14 15:15
- `hermit-replay-host-identity-bd54/PR_BODY.md` — 78 lines, 08-14 15:15

### `ignored/pr-bodies/` — 1 files, newest 08-14 14:55
- `pr-bodies/chaos-fp-reduction-dd72f57de.md` — 73 lines, 08-14 14:55

### `ignored/qemu-linux/` — 1 files, newest 07-23 09:21
- `qemu-linux/scx-logs/FINAL_RESULTS.md` — 48 lines, 07-23 09:21

### `ignored/qemu-snap-research/` — 1 files, newest 07-24 05:53
- `qemu-snap-research/SNAPSHOTS.md` — 169 lines, 07-24 05:53

### `ignored/recheck50/` — 1 files, newest 08-15 00:37
- `recheck50/list.tsv` — 50 lines, 08-15 00:37

### `ignored/revents-measure/` — 1 files, newest 08-14 22:29
- `revents-measure/PR-BODY-verify-labels.md` — 71 lines, 08-14 22:29


## NOT PROMOTED, AND WHY — NAMED RATHER THAN OMITTED

- **hermit-shell's `open-hermit-pr-manifest-census-20260815.tsv`** — pushed to
  `origin/codex/census-manifest-touching-open-prs` at `ebec0e41`, not landed. It
  survives this host. **Not mine to land**; leaving it named as exposure for its owner.
- **hermit-sabre's 228-line plan-absence survey, sha256 `dbc74b3ebae37bab`** — **not
  found by my method**: content-hash match over 228-line `.md`/`.tsv`/`.txt` in
  `ignored/`, `ai_docs/`, `scratch/`, plus a grep for its distinguishing citation
  `scorecard.rs:54`. **Reported as not-found-by-this-method, NOT as lost.** sabre is on a
  P0 and was not interrupted; worth asking when it is free.
- **Every other agent's lane directory** — `scratch/` 107, `gate-remediation-options-20260815/`
  81, `rev-fable/` 52, `pr2069-codex-review/` 50, `stackowner-52f75645/` 28,
  `sigsegv-diag/` 26. Indexed above, not copied.
- **Build byproducts and samples** — e.g. `gate-manifest-sample.txt` and
  `gate-manifest-sample2.txt` are per-second process snapshots (`pid= comm= state= wchan=`),
  which are sampling output rather than a finding. Not durable-worthy.
- **Superseded by name** — `SUPERSEDED-mechanical-78-cells.tsv`, whose successor is
  already tracked at `ai_docs/LITEINST_MECHANICAL_CELLS_20260815.tsv`.
- **Stale historical logs** — `DEVLOG_hermit.md` (5967 lines, last written 2026-07-28)
  and `PROMPT_RECAP_20260726.md`. Outside this session and not re-verified.

## THE NEAR-MISS THAT GOVERNS EVERY PROMOTION IN THIS SWEEP

Earlier today the obvious "re-promote to refresh a stale copy" move would have
**deleted 46 data rows**: my source for the exit-gating screen is 272 lines / 30 data
rows, while `origin/main` already carried a **599-line / 76-row SUPERSET** landed by
another agent's commit `6350677a`. **A seam tells you two things differ, never which one
is newer.** Content and authorship must both be compared before any promotion, every
time. That check is why this sweep promoted nothing by overwrite.

<!-- PROVENANCE: built 2026-08-15 by hermit-coord. INDEX ONLY -- no indexed file was copied.
     Index is a point-in-time listing of a MOVING tree; entries WILL go stale as lanes work.
     It is deliberately not seam-hashed against 445 sources: a seam per file would be noise
     and the index's value is discoverability, not currency.
     candidate list sha256 58e5e2fa3cf9a53c6e2880ff38a5f277e6ac8293f2a8d0e788571df835d699a7 (445 paths)
-->
