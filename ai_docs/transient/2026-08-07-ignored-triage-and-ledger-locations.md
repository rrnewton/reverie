# `ignored/` triage and validation-ledger locations

**Date:** 2026-08-07
**Scope:** Publication of the completed read-only census from TaskGraph task
`answer-ledger-location-and-triage-ignored-including-the-213gb-ci-hub`. No
sizes were re-measured for this document, and nothing was deleted.

**2026-08-27 update:** the speculative-land obligation framework and its
on-disk checkout tree were deleted. The figures below remain the historical
measurement that identified the disk cost; they are not current retention
guidance.

## Decision summary

- The version-controlled ledger destination is
  `ledger/hermit/devbig014/2026-08.jsonl`, but the migration is incomplete.
  The live producers still append to `ignored/validate-run-ledger.jsonl`.
- `ignored/validate` began as a quarantine for stale DynamoRIO build trees and
  was later reused for validation logs. `ignored/validate-producer` came from a
  coordinator-run exact-head validation batch.
- The directory described as “213 GB in ci-hub” is `ignored/ci-hub`, not the
  tracked `ci-hub/` source tree. It was **211.88 GiB apparent** but **65.83 GiB
  compressed physical** on Btrfs/zstd. Obligation clones account for 98.58% of
  the compressed usage.
- The complete top-level census was **448 entries: KEEP 9 · PURGE 1 · UNKNOWN
  438**. The later owner directive removed the entire speculative-land checkout
  tree rather than applying the original per-clone retention proposal. Linux
  and DRB assets remain KEEP.

## 1. Ledger location: the migration never finished

There is no single path that is both version-controlled and receiving all live
events.

| Path | Observed state | Writer/provenance |
|---|---|---|
| `ledger/hermit/devbig014/2026-08.jsonl` | Intended canonical version-controlled per-team/per-machine shard. It contained 654 rows and had been published once; the active ledger was already 49 rows ahead. | One-time `ledger-publisher` commit `81dc179725b220cd001bbe1de26fb07ebc2bf4cb`. No live producer was wired to it. |
| `ignored/validate-run-ledger.jsonl` | Operational ledger: 703 rows and 806,197 bytes at the completed census. Current source explicitly calls this singular path canonical. | `hermit/validate.sh`, `reverie/validate.sh`, and the non-default `hermit/scripts/validate.rs` path append here. Each permits an explicit environment override. |
| `ignored/ci-hub/validate-runs.jsonl` | Stale plural-path artifact with one row. | Its production writer existed only in unmerged Hermit PR #1554; current parent code has readers/documentation but no writer. |
| `ci-hub/ignored/validate-runs.jsonl` | Historically cited third name; no such file existed. | No current writer. The citation confused the parent-relative location with a path inside tracked `ci-hub/`. |

The tracked shard is therefore the canonical *destination/design*, while the
ignored singular file remains the active operational ledger. Treating either
one alone as “the ledger” hides the unfinished migration. These observations
identify files and source writers only; they do not treat ci-hub ledger
verdicts as validation authority.

## 2. Who created the two validation directories

### `ignored/validate`

The directory and its first child were born at 2026-08-04 01:53 PDT. The
matching first-person TaskGraph note attributes it to the `hermit-ghdag`
workstream during PR #1580 investigation. That workstream moved 18 stale
Reverie/DBI DynamoRIO build directories—10 debug and 8 release—into
`ignored/validate/1580-stale-dynamorio/` because the stale builds produced a
false validation failure.

The directory was subsequently repurposed as the durable root for validation
logs and waiter run records. The current scripted use did not create the
original directory.

### `ignored/validate-producer`

The directory was born at 2026-08-04 20:30 PDT, 16 ms before its first log,
`validate-1243-17b59fc6.log`. Matching task, unit, and output-path evidence
attributes it to the `hermit-coord` sole-producer workstream for exact-head PR
#1243 validation. It later held worktrees and logs for PRs #1200, #1243,
#1470, #1549, #1618, #1623, #1626, and #1627. PR #1549 was prepared but never
launched.

Both paths are owned by the shared `newton` Unix account, so the exact process,
agent session, and model identity cannot be recovered. The evidenced creator
roles are `hermit-ghdag` and `hermit-coord`; claiming a more specific identity
would be guesswork.

## 3. What the “213 GB in ci-hub” is

The large path is the ignored runtime tree `ignored/ci-hub`. The tracked
`ci-hub/` source/tooling tree was only 11.19 MiB apparent.

### Apparent, compressed, and reflink figures

These quantities answer different questions and must not be collapsed:

| Tree | Apparent bytes | Apparent GiB | Btrfs compressed physical bytes | Compressed GiB |
|---|---:|---:|---:|---:|
| `ignored/ci-hub` | 227,502,592,953 | 211.88 | 70,680,045,496 | 65.83 |
| `ignored/ci-hub/obligations` | 224,031,889,659 | 208.65 | 69,678,333,964 | 64.89 |
| Everything else under `ignored/ci-hub` | 3,470,703,294 | 3.23 | 1,001,711,532 | 0.93 |

`compsize` processed 508,114 files and reported 202,535,075,243 uncompressed
bytes and 249,531,411,883 referenced bytes for `ignored/ci-hub`; compression
reduced unique physical disk use to 70,680,045,496 bytes. The obligations tree
alone was 98.58% of that compressed physical use.

Btrfs reflink accounting is separate from compression accounting. The whole
tree had 153,620,787,200 exclusive extent bytes and 24,157,601,792 set-shared
extent bytes; obligations had 153,541,971,968 exclusive and 22,627,053,568
set-shared. Those are extent-sharing figures, not physical compressed bytes,
and cannot be quoted as reclaimable disk space.

The live tree grew by 2,259 apparent bytes between the serialized whole-tree
scan and the later top-level partition pass. The table consistently uses the
first serialized-scan value rather than silently mixing snapshots.

### Purpose by subdirectory

- `obligations/` held 13 detached exact-SHA Hermit clones, including their
  `target/` build trees, created by the speculative-land `arm-land` protocol.
  Twelve watcher records were terminal (`satisfied` or `remediated`, with zero
  unresolved work). One remained `remediation_required` with one unresolved
  item: `20260804-004303-d5fcdbe822bd-bbb4b8`.
- `stress-wt/nightly` is a deliberately persistent detached nightly worktree,
  and `stress-calib` is its durable calibrator. They are KEEP assets.
- The remaining roughly 100 MiB of authority/history data included the retired
  event log, per-run logs and cost JSON, GHA CSV/JSON state, directives,
  landing intents, submodule-bump records, and caches.

No production retention path was found for terminal clones. The later owner
directive removed the framework and its checkout tree instead of adding another
cleanup mechanism.

Terminal nested purge candidates are:

1. `20260803-225353-afabb00a8880-3dd324`
2. `20260803-230744-93a82b24267b-620a08`
3. `20260804-002855-b824a34856a3-bb1135`
4. `20260804-005018-e8a0d8d3be3b-a7349b`
5. `20260804-023434-74a5b6b56e76-c7c06f`
6. `20260804-025419-0f891e432a75-7a7725`
7. `20260804-120036-b384187efd72-bd9cfa`
8. `20260804-221543-3801a7dfb9b9-7a545c`
9. `20260804-224816-2a01963e6121-1ab3e2`
10. `20260805-042454-64ffb5147829-7790e3`
11. `20260807-011247-20f21bdbde5f-8118ee`
12. `20260807-011450-851164890bc9-5e9b7c`

This was the original proposal. The 2026-08-27 owner directive superseded it
and authorized deletion of the complete speculative-land checkout tree.

## 4. Complete `ignored/` triage

The denominator is every one of the 448 immediate children present during the
completed census. Names alone are not evidence that a tree is disposable:
`build`, `land`, `validate`, `tmp`, and `logs` paths remain UNKNOWN unless task,
process, registry, and ownership evidence establish otherwise.

### KEEP (9)

| Entry | Apparent bytes | Reason |
|---|---:|---|
| `btrfs-kernel-build` | 3,411,204,685 | Contains a Linux checkout plus kernel build/boot artifacts. |
| `ci-hub` | 227,502,592,953 | Historical pre-deletion size. Preserve the remaining metadata and stress assets; the speculative-land checkout tree was removed on 2026-08-27. |
| `haskell-drb` | 865,030,136 | DRB build/probe assets. |
| `linux` | 1,530,887,364 | Linux Git checkout. Its broken alternate-object reference makes preservation especially important. |
| `logs` | 11,138,360,862 | Mixed log tree containing extensive DRB experiment evidence; conservatively retain the whole tree. |
| `qemu-linux` | 15,338,970,177 | Linux VM/kernel images and boot assets. |
| `qemu-linux-618-vm2` | 1,652,721,776 | Linux 6.18 VM/kernel package and strict-L2 evidence. |
| `qemu-src` | 163,633,549 | QEMU Git checkout supporting Linux VM work. |
| `rb-drb-asplos20-rebuild` | 85,069,397 | DRB/dettrace rebuild assets and evidence. |

`haskell-drb` and `rb-drb-asplos20-rebuild` are experiment asset trees rather
than standalone Git roots; KEEP follows the owner's broader instruction to
preserve DRB work. `logs` is mixed but retained because it contains DRB
evidence.

### PURGE (1)

| Entry | Apparent bytes | Reason |
|---|---:|---|
| `__pycache__` | 7,352 | Regenerable Python bytecode cache. |

### UNKNOWN (438)

Preserve every entry below pending registry/task/process/ownership review.
This is the exhaustive remaining set after subtracting the 9 KEEP and 1 PURGE
entries from the 448-entry denominator.

<details>
<summary>UNKNOWN A–F (176 entries)</summary>

`.ancestry-audit.lock`, `1580-run-jobs.tsv`, `DEVLOG_hermit.md`, `ancestry`,
`ancestry-audit-repository-derived-20260807.log`, `anchor-bracket.py`,
`audit-validate-1786026092.log`, `band-rebase`, `bench-seeds`, `bisect-demo5`,
`bp-buggy`, `bp-fixed`, `breach-table-231b`, `btrfs-b3-validate_20260728`,
`btrfs-f6a6c280-oracle`, `btrfs-f6a6c280-repro_20260729`,
`btrfs-hermit-investigation`, `btrfs-progs-git`, `btrfs-progs-investigation`,
`btrfs-progs-track-b-272`, `btrfs-progs-v7.1`, `btrfs-progs-v7.1-bin`,
`btrfs-progs-v7.1-bin-track-b-272`, `btrfs-progs-v7.1-track-b-272`,
`btrfs-progs-v7.1-track-b-272.tar.xz`, `btrfs-progs-v7.1.tar.xz`,
`btrfs-testing-hermit_20260728`, `build-job-backend-benchmark`,
`busybox-autofetch-verify`, `cancelled-run-jobseconds.tsv`, `cargoprobe`,
`cellrun`, `ch4-logs`, `ch4-probe.out`, `ch4-work`,
`chaosoracle-commit-msg.txt`, `coalesce-det4`, `cold-verify-backends`,
`compat-envelope`, `contamination-355.py`, `coord-demo5-verify`, `cputo-probe`,
`crontab.backup.1785704340`, `d5-optB-235`, `d5r-ae`, `d7.6vAvOj`,
`dbi-l2-logs`, `dbi-l2-logs-rdtsc`, `dbi-l2-sweep-rdtsc.tsv`,
`dbi-l2-sweep.sh`, `dbi-l2-sweep.tsv`, `dbi-parity-logs`,
`dbi-parity-sweep.sh`, `dbi-parity-sweep.tsv`, `dbi-preempt-build.log`,
`dbi-preempt-build2.log`, `dbi-preempt-build3.log`, `dbi-preempt-build4.log`,
`dbi-preempt-final-build.log`, `dbi-tsc-probe-reverie394-20260807`, `dbimaps`,
`dbt-compat-parity-fixed.tsv`, `demo-cycle`, `demo07-drgn`,
`demo07-drgn_20260728`, `demo07-pypi-diagnose`, `demo07-pypi-diagnose2`,
`demo07-pypi-dwarfbridge`, `demo07-pypi-exact`, `demo07-pypi-exact2`,
`demo07-pypi-fixed`, `demo07-pypi-fixed2`, `demo07-pypi-kallsyms`,
`demo07-pypi-modules`, `demo07-pypi-repro`, `demo07-pypi-vmcore`,
`demo07-symbol-pair-test`, `demo08-btrfs`, `demo08-btrfs-nightly-test`,
`demo08-build`, `demo08-repro`, `demo08-run`, `demo08-run-nightly-test`,
`demo5-ab-matched-load`, `demo5-ab-matched-load.sh`,
`demo5-broken-trace.log`, `demo5-good-trace.log`, `demo5-multisect`,
`demo5-plain-main-baseline`, `demo5-plain-main-baseline.sh`,
`demo5-rcb-verify`, `demo5-rcb-verify.sh`,
`demo5-reliability-fix-poller-jump`, `demo5-reliability-post1190-ae2565be`,
`demo5-skid-verify`, `demo5-skid-verify.sh`, `demo5-tagverify-3e4367ec`,
`demo7-origin-main.LeLRXg`, `det4-commit-msg.txt`, `det4-d5-controlled.out`,
`det4-d5-controlled.sh`, `det4-d5-ctl`, `det4-d5-pA`, `det4-d5-pB`,
`det4-d5-qA`, `det4-d5-qB`, `det4-d5-sa`, `det4-d5-selfdet.out`,
`det4-d5-selfdet.sh`, `det4-demo05-parity.out`, `det4-demo05-parity.sh`,
`det4-demo5-batch.out`, `det4-demo5-batch.sh`, `det4-demo5-measure`,
`det4-demo5-measure2`, `det4-demo5-run3`, `det4-demo5-run4`, `det4-demo5-run5`,
`det4-detlogdiff`, `det4-detlogdiff-msg.txt`, `det4-detlogdiff-pr.md`,
`det4-fork3000.out`, `det4-fork3000.sh`, `det4-golden-selfdet.out`,
`det4-golden-selfdet.sh`, `det4-golden-selfdet.tsv`, `det4-gsd`,
`det4-harnessfix-msg.txt`, `det4-hermitdemo5.log`, `det4-linuxboot`,
`det4-linuxboot-msg.txt`, `det4-linuxboot-pr.md`, `det4-parity`,
`det4-parity-build.log`, `det4-parity-depth.out`, `det4-parity-depth.sh`,
`det4-parity-depth.tsv`, `det4-pr2-body.md`, `det4-repro`,
`det4-rung-qualify.out`, `det4-rung-qualify.sh`, `det4-rung-qualify.tsv`,
`det4-rung-sizes.tsv`, `det4-rung-sizing.out`, `det4-rung-sizing.sh`,
`det4-sabre-stage.log`, `det4-validate-1681-run2.log`,
`det4-validate-1681.log`, `det4-xbdiff-fixed.rs`, `detached-verify-cargo-target`,
`detinode-wip-fd.rs`, `detlog-parity`, `detlog-tsc-w22-20260807`, `dettrace`,
`diag-rustbin_exit_group.log`, `diag-rustbin_sched_yield.log`,
`diag-syield-info.log`, `diag-syield-sched.log`, `diag-syield-trace.log`,
`drain-validate-logs`, `drainall-submodule-init.log`, `drainall-validate`,
`drainall-validate-ce9366ae.log`, `drgn-par`, `drgn-par-test`,
`drgn-pypi-0.2.0`, `enumerate_anchor.py`, `envparity`, `f2.log`, `fairness-val`,
`fileio-residue`, `finding-fast-6chk-noresult.txt`,
`finding-verified-deriver-gap.txt`, `flockrev`, `fork-exec-parity`, `ftest.log`.

</details>

<details>
<summary>UNKNOWN G–P (120 entries)</summary>

`PROMPT_RECAP_20260726.md`, `g45`, `ghdag-val-1612`, `golden-capture`,
`golden-probe`, `gvisor`, `gvisor-boxed-repro-20260804`,
`gvisor-runsc-same-host`, `h`, `helper-210`, `helper-227`, `helper-238`,
`herdr-probe`, `herdr-run`, `herdr-run-probe`, `hermit-235`,
`hermit-320-mutation`, `hermit-ci-evict-poc`, `hermit-primary-e2e-20260802`,
`hermit_run_dbi.sh`, `ignored`, `infoscope-msg.txt`, `javac-task-measure`,
`jvm-verify-investigation`, `kvm-hang-regression-20260807`,
`kvm-ratchet-triage`, `kvm-stack-selfdet`, `land-1213`, `land-1443`,
`land-1468`, `land-1471`, `land-1471b`, `land-1576`, `land-1586`, `land-1595`,
`land-1604`, `land-1613`, `land-1616`, `land-1623`, `land-1624`, `land-1692`,
`land-1719`, `land-fchown-gap`, `land-main-ctl`, `lander-1606`, `lander-1609`,
`lander-preserved`, `lander-revalidate-e8a0d8d3-20260804T012118Z.log`,
`lander-validate-1468-da72794a.log`, `lander-watch.sh`, `landing-plans`, `lfz`,
`liteinst2-ergo`, `load-vtime-repro`, `lock-reconcile.log`,
`lock-reconcile2.log`, `lu-parity`, `m10-multi-process.1qD6Y6`, `m11-pipes`,
`m12-filesystem-ops-294e89bfeeeb`, `m15-network-final.AQPD0Y`,
`m15-network-no-result.wghMu7`, `m15-network-stack.YaSzlL`, `makedet-logs`,
`makedet-repro`, `makedet-work`, `minvtime-measure.sh`, `minvtime-results`,
`mprb-open-prs-1785724152.json`, `mprb2-open-1785726104.json`, `mutsweep`,
`my17.tsv`, `ociprobe`, `offline-build`, `open-prs-anchor-analysis.json`,
`open-prs-fresh-20260802.json`, `open-prs-fresh2-20260803.json`,
`open-prs-rollup.json`, `operandA-1559`, `orc014vt`, `orphan-repro`,
`p1-resume-qemu-linux`, `parent-hygiene-20260729`,
`parent-hygiene-20260801`, `parent-main-write.lock`,
`parent-prepull-gvisor-same-host-20260802`,
`parent-root-handoff-collision-20260802`, `parity-gate`, `pintest.log`,
`pintest2.log`, `portable-ci-1171.watch`, `portable-main-runs.tsv`,
`pr-1147-body.md`, `pr-body-makedet.md`, `pr-infoscope-body.md`,
`pr-list-err.txt`, `pr-list-raw.json`, `pr1147-repro`,
`pr1190-demo5-clock-ci`, `pr1584-slot238b-evidence-cc9d16bf`, `pr1742.body`,
`pr1742.diff`, `pr394_clone_residue_probe`, `pr394_marker_41`,
`pr394_marker_42`, `pr394_marker_under_test`, `pr394_thread_fork_exec_probe`,
`pre-anchor-pr-enumeration.tsv`, `preemption-summary-20260801`, `prefix-build`,
`prefix-build.log`, `prefix_depth.txt`, `prefix_depth2.txt`, `prefix_depth3.txt`,
`primary-recovery-20260806`, `prlist-1785727521.json`,
`prlist2-1785728127.json`, `producer-evidence`, `ptrace-detlog.log`,
`ptrace-selfdet-four-dimensions-20260807`.

</details>

<details>
<summary>UNKNOWN Q–Z (142 entries)</summary>

`qemu-snap-research`, `qemu-userspace-verify_20260727`,
`ratchet-executed-count-rescue`, `readmelink-commit-msg.txt`,
`readmelink-pr-body.md`, `rebase-records.jsonl`, `recovery`, `redis-repro`,
`redpop-1785727120.json`, `refile-17`, `regress-kvm-livelock`,
`relink-226-627892-drmemtrace_launcher-20260804.log`,
`relink-226-drmemtrace_launcher-20260804.log`,
`relink-vforkverify-f2ab46-drmemtrace_launcher-20260804.log`, `reload-verify`,
`reverie-check.log`, `reverie-pin-bump`,
`review-reverie403-a6aa8bc-20260807`, `revpin`, `rollup-1785727166.json`,
`routed`, `sabre-build`, `sabre-build.log`, `sabre-detlog.log`, `sabre-loud`,
`sabre-pr2-build-make`, `sabre-pr4-build`, `sabre-pr4-build-make`, `scratch`,
`self-determinism-multiguest-20260807`, `sigtest.log`, `sigtrap`,
`stack-parity-w22`, `staging-batch1`, `staging-batch2`, `staging-batch3`,
`staging-reverie-live`, `sweep-defaultoff-logs`, `sweep-defaultoff.tsv`,
`sweep-q1000-logs`, `sweep-q1000.tsv`, `sweep-q100000-logs`,
`sweep-q100000.tsv`, `tmp`, `truncated-band-findings.txt`,
`unlanded-count-20260807`, `unpushed-parent-rescue.log`, `validate`,
`validate-1580-ghdag.log`, `validate-1609-d6a28771.log`,
`validate-1609-rebased-893991acc-run2.log`,
`validate-1609-rebased-893991acc-run3.log`,
`validate-1609-rebased-893991acc.log`, `validate-local-1785705807.log`,
`validate-local-rerun-1785706233.log`, `validate-logs`, `validate-port`,
`validate-pr1514-f438cab2-r2.log`, `validate-pr1514-f438cab2.log`,
`validate-producer`, `validate-red-attribution.jsonl`,
`validate-run-global.csv`, `validate-run-global.jsonl`,
`validate-run-ledger.jsonl`, `validation-evidence`, `verdict-1638-claude.md`,
`verify-strict-probe`, `w1-kvm-main`, `w1-pin`, `w10-bare-fixture`,
`w10-cells`, `w10-envpin`, `w10-heapdomain`, `w10-heapdomain-repro`,
`w10-strict`, `w14-posttighten`, `w14-ratchet`, `w15-bp`, `w15-bw`,
`w15-demo08-preserve`, `w15-e9`, `w15-prefix`, `w15-prefix-guests`,
`w15-prefix-sweep`, `w15-prefix2`, `w15-procfs-guard`,
`w15-threaded-heap-discriminator`, `w16-acceptance`, `w16-aupin`, `w16-brk`,
`w16-cifix`, `w16-esrch`, `w16-gate`, `w16-green`, `w16-ledger`,
`w16-parity`, `w16-skill`, `w16-tfd`, `w2-demo05`, `w2-ratchet`, `w2-rungs`,
`w2-selfcheck-deepen`, `w24-detlog-stream-analysis.py`, `w28-reconcile`,
`w3-394-wt`, `w3-cold03`, `w3-demo`, `w3-lifecycle-wt`, `w3-mtl-wt`,
`w3-persist-wt`, `w3-ratchet-wt`, `w3-relay-wt`, `w3-reset-wt`,
`w3-sabre-wt`, `w3-sc-wt`, `w3-tax-wt`,
`w3-unauthorized-type-binding.patch`, `w30-probe`, `w30-tsh`, `w6-bitwise`,
`w6-census`, `w6-dbi-stack`, `w6-hermit-primary-rescue`, `w6-kvm`,
`w6-migrate`, `w6-sigalrm`, `w6-status`, `w6-statuslog-rescue`, `w7-1847`,
`w7-apps`, `w7-blocked-patches`, `w7-corpus`, `w7-corpus-run`,
`w7-gchat-shim`, `w7-hooks-handoff`, `w7-port`, `w8-chaos`, `w8-gateprov`,
`w8-parity`, `watch-land-1616.sh`, `xfstests-dev-investigation`,
`zero-byte-cmake-objects-manifest-20260804.txt`.

</details>

## Owner decision update

1. The speculative-land checkout tree and the framework that created it were
   deleted on 2026-08-27.
2. Preserve the remaining KEEP entries, especially Linux, QEMU/Linux, and DRB
   assets.
3. Audit the 438 UNKNOWN entries in descending exclusive-byte order and require
   task/process/ownership evidence before promoting any of them to PURGE.

No deletion was performed during the original census. The later removal was a
separate owner-directed action.
