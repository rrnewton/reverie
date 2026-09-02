# Disk retention forensics and corrected reclaim accounting

Date: 2026-09-02

## Result first

The filesystem is no longer near the ENOSPC condition that killed validation and
coincided with the coordinator outage. At the measurement point,
`btrfs filesystem usage --raw /home/newton/work/dev-hermit` reported a
3,760,476,651,520-byte device, 1,689,281,609,728 bytes used, and
1,988,094,377,984 bytes estimated free. `Used` here is physical device usage,
including duplicated Btrfs metadata; it is not a sum of logical file sizes.

The owner's 1.5 TB target was therefore 189,281,609,728 bytes away. A single
`compsize -b` invocation over all seven directories awaiting owner disposition
found at most 187,377,395,632 physical bytes in that set. Even if deleting every
directory freed every one of those bytes, the filesystem would still be
1,904,214,096 bytes above the target. Shared extents referenced outside the set
can only make the actual reclaim smaller.

**Correction:** the earlier approximately 442 GB figure for those directories
was not physical reclaimable space. Deleting possibly irreplaceable experiments
on the strength of that figure would incur the irreversible cost and still miss
the stated target.

No directory in this table was removed as part of this measurement.

## The seven held directories

The table is from `compsize -b`, not `du`. `Referenced` is the logical extent
content referenced at the named location; it can count reflink-shared content
more than once. `Disk Usage` is the compression-aware physical extent usage
reported by `compsize`. Decimal GB are used so they compare directly with the
1.5 TB target.

| Directory | Referenced bytes | Referenced GB | Physical bytes (`Disk Usage`) | Physical GB |
|---|---:|---:|---:|---:|
| `/home/newton/agent_logs_archive_bak` | 69,421,529,584 | 69.42 | 24,337,090,793 | 24.34 |
| `/home/newton/work/dev-hermit/scratch` | 236,863,469,433 | 236.86 | 38,889,162,702 | 38.89 |
| `/home/newton/work/dev-hermit/experiments` | 174,241,145,455 | 174.24 | 23,983,302,283 | 23.98 |
| `/home/newton/work/dev-hermit4/worktrees` | 103,837,696,172 | 103.84 | 37,620,492,182 | 37.62 |
| `/home/newton/work/multi_sched-test` | 68,003,730,619 | 68.00 | 14,938,759,187 | 14.94 |
| `/home/newton/work/hatch-dev/jarvis` | 82,683,594,236 | 82.68 | 27,889,243,827 | 27.89 |
| `/home/newton/work/hatch-dev/qemu-vm` | 33,519,276,789 | 33.52 | 19,726,610,962 | 19.73 |
| **Combined, one invocation** | **768,570,442,288** | **768.57** | **187,377,395,632** | **187.38** |

The individual physical rows sum to 187,384,661,936 bytes. The combined
invocation is 7,266,304 bytes lower because it accounts for sharing within the
seven-path set; the combined value is the relevant upper bound. It still cannot
detect whether an extent is also referenced by a path outside the set, so
187.38 GB is not a promise of 187.38 GB reclaim.

## Instrument rule and the two corrected readings

- `btrfs filesystem usage --raw` is the filesystem-level physical accounting.
  Its 1.689 TB `Used` value is the number to compare with the target.
- `btrfs filesystem du --raw` reports logical referenced extents and separates
  shared from exclusive references. It does not reduce those numbers to the
  compressed physical allocation that deletion can release.
- `compsize -b` uses FIEMAP and Btrfs compression information to report both
  `Referenced` and compression-aware `Disk Usage`. It is the per-path instrument
  used for the table.

This distinction corrected two material errors during the incident response:

1. The seven held directories were initially presented as roughly 442 GB of
   reclaim. Their combined compression-aware physical upper bound is 187.38 GB.
   The earlier figure was a Btrfs referenced/exclusive measure, a third metric;
   it is not interchangeable with either column above.
2. The 998 retained verification logs were initially presented as 58.74 GB of
   reclaim from `btrfs filesystem du`. `compsize -b` measured only
   3,834,740,736 physical bytes for 58,740,826,112 referenced bytes: about
   15.3 times less physical space. The logs are highly compressible text.

The approximately fifteen-fold divergence describes that log population, not
all content. Across the seven held directories, referenced bytes are about 4.1
times physical bytes; disk images are less compressible than logs.

## Four retention mechanisms found

| Mechanism | Evidence and rate | Durable status as of this report |
|---|---|---|
| Health tool retained every materialised parent-SHA checkout | Eleven immutable parent trees occupied 34,673,336,320 referenced/exclusive bytes. Materialising a previously absent SHA after the fix retired ten roots and left exactly two, reducing the retained referenced amount by 26.33 GiB. | **Prevention code landed** in [`5bfa5d91f56e`](https://github.com/rrnewton/dev-hermit/commit/5bfa5d91f56e380631a549bf8dcf58631c97dabd). At the last deployment check, the periodic service still named the wrapper in a stale shared parent checkout because parent publication was blocked by a dirty `hermit` path. The bound reaches that consumer after the parent sync; this report does not claim that deployment already happened. |
| Temporary Git roots lived outside the managed worktree lifecycle | The sweep began with 767 top-level Git roots under `/tmp`; 450 had appeared in five days. The checked-in `AGENTS.md` explicitly prescribed `git worktree add --detach /tmp/<name> origin/main`, so agents were following documented procedure. Salvage-first cleanup reduced the checkpoint count to 113 and verified 58 rescue refs before removal. | **Prevention remains open and needs the owner's policy wording.** The cleanup reduced the population but cannot hold while the documented landing recipe recreates it. No policy file was edited. |
| Failed verify comparisons retained raw logs without a population bound | 499 failures left 998 `run1_log_*` and `run2_log_*` files; 48 reached the existing 1 GiB per-file cap. The cap bounded each file but nothing bounded the population. The raw logs are evidence and must remain paired and readable while live. | **Implementation exists but is not landed.** [Hermit pull request 2925](https://github.com/rrnewton/hermit/pull/2925) keeps the newest 64 implicit failed comparisons, retires pairs atomically under locks, and leaves explicit artifact destinations unbounded. Its tests materialise another divergence and hold the count at the bound, including refusal while a reader lock is live. At this snapshot the pull request is open and exact-head status is NOT-VALIDATED. |
| `cells.json` committed per-run execution detail | One ordinary validation added 4,773,867 bytes and changed 103,828 lines: 82,654 insertions and 21,174 deletions. It added 350 observed invocations and 505 attempt records, including per-attempt argv and process outcomes. | **Diagnosis is landed; behavior is not fixed.** The field-by-field lifetime report is [`ai_docs/cells-json-and-results-series-lifetimes-20260902.md`](https://github.com/rrnewton/dev-hermit/blob/97fa27f27a64d184f698f6d9ce633da5ded4ad7f/ai_docs/cells-json-and-results-series-lifetimes-20260902.md). Moving per-run data to the cell-level results ledger and keeping `cells.json` sparse needs the owner's design ruling. Sparseness does not mean pruning the manifest, and no history rewrite is proposed. |

This status is deliberately more precise than saying “two mechanisms landed.”
One prevention fix is landed, one prevention implementation is in an open pull
request, one investigation report is landed while the behavior remains open,
and one prevention decision is still with the owner.

## Reclaim outcome and safety boundary

The incident response reduced filesystem use from about 2.06 TB to about 1.689
TB without treating authored work as disposable. The `/tmp` worktree sweep used
process/path censuses, pushed rescue refs individually, and read them back from
the remote before removal. It preserved live or unreadable candidates rather
than forcing them. Build `target/` directories were treated separately as
regenerable output.

The ENOSPC condition is resolved at this snapshot: the filesystem is about 45%
used and reports about 1.988 TB estimated free. That is an operational recovery,
not evidence that the creation rates are fixed.

## What this investigation did not establish

- Extents in the seven-directory set may be shared with paths outside that set.
  `compsize` cannot turn the 187.38 GB physical total into a guaranteed deletion
  yield without a filesystem-wide reverse-reference attribution. Actual reclaim
  could be lower.
- An earlier Btrfs referenced-size census found about 509 GB under
  `/home/newton/work/dev-hermit/ignored`, but no dominant per-event retention
  mechanism was established there and this report does not have a corresponding
  compression-aware physical total. It is unexplained space, not 509 GB of
  established reclaimable space.
- The ad-hoc end-to-end guest proof for the verify-log change could not run on
  this host: ptrace preflight was denied, the LiteInst runtime DSO was absent,
  and KVM mount setup returned `EPERM`. Unit tests exercised real divergent
  comparisons and the reader-lock refusal, but the pull request still requires
  exact-head validation and landing.
- This report does not decide the disposition of any of the seven held
  directories. Their contents may be irreplaceable; the corrected accounting
  only establishes that deleting all seven cannot meet the target.

## Reproducible measurement commands

Filesystem snapshot:

```bash
sudo btrfs filesystem usage --raw /home/newton/work/dev-hermit
```

Seven-path accounting was collected in one invocation so sharing within the
set was not counted twice in the combined physical result:

```bash
sudo compsize -b -x \
  /home/newton/agent_logs_archive_bak \
  /home/newton/work/dev-hermit/scratch \
  /home/newton/work/dev-hermit/experiments \
  /home/newton/work/dev-hermit4/worktrees \
  /home/newton/work/multi_sched-test \
  /home/newton/work/hatch-dev/jarvis \
  /home/newton/work/hatch-dev/qemu-vm
```

The raw combined result was:

```text
Processed 4789430 files, 4083632 regular extents (6413283 refs), 2673184 inline.
Type       Perc     Disk Usage   Uncompressed Referenced
TOTAL       36%     187377395632 509399020080 768570442288
none       100%      88433238330  88433238330 165966774586
zstd        23%      98944157302 420965781750 602603667702
```
