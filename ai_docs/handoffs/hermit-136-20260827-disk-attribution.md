# HANDOFF — hermit-136 — written 2026-08-27 before an imminent reboot

**Slot:** `/home/newton/work/dev-hermit/worktrees/136`
**My actual working directory during this session was the PARENT
`/home/newton/work/dev-hermit`, not this slot.** Anything I measured with a
relative path was measured against the parent tree.

## Shas

| repo | sha |
|---|---|
| dev-hermit parent | `2c4eec3a8193fbddd0c0434c26bb7cf1c544851c` (branch `main`) |
| hermit submodule HEAD | `618dcd82aa3c846aae128f7ba4933d89a639357c` |
| hermit `origin/main` | `26f5bc2de3d328b64c41c42ef093ecb9d89e98ab` |
| reverie submodule HEAD | `063fa37b05e562760f3d27c80ed8a6482b97b44a` |
| merged PR branch | `fix/dbt-verify-capture-names-the-harness-scans-for` @ `53ecdac33868376c384dad19bf1de84a1d2faaa5` |

## Running validate: LOST, handle recorded

Unit `validate-hermit-136-6172478eae28-1787831741880091578-1009495-b0024242.service`
on `6172478eae288c9f5005545d4f68c00c780f41c0`. **Already absent from
`systemctl --user list-units --all` at handoff time; no log path was captured
and there is no verdict.** Recorded as lost, not pending. The standing
revalidate obligation is NOT discharged by me.

## What I was doing

The standing owner directive
`standing-what-is-consuming-three-and-a-half-terabytes`: attribute disk
consumption to a named producer. Complete; the full write-up with every number
is in the TaskGraph notes on that task and in the addendum below.

## What I would do next, in order

1. Find why `remove_fresh_checkout` (`ci-hub/validate/start_unit.py:898`, seven
   call sites) does not reach the 68 surviving `ignored/validate-fresh-*`
   checkouts. That is the one producer that is unambiguously ours and
   unambiguously regenerable.
2. Run `compsize -x /tmp` — the only large region never measured with a real
   extent-counting instrument. `du` says 923.6 GiB; the estimate of ~330 GiB on
   disk is derived from a ratio, not measured.
3. Report to the tick's owner that `slot_disk_residue.py` prints cached sizes
   from `.tick-hub/slot-disk-residue-state.json` with no staleness marker.
4. Re-run the revalidate obligation, capturing the log path this time.


---

# Addendum, 2026-08-27 afternoon (hermit-136)

## Exact coordinates for everything below

| repo | sha |
|---|---|
| dev-hermit parent | `2c4eec3a8193fbddd0c0434c26bb7cf1c544851c` (branch `main`) |
| hermit submodule HEAD | `618dcd82aa3c846aae128f7ba4933d89a639357c` |
| hermit `origin/main` at write time | `26f5bc2de3d328b64c41c42ef093ecb9d89e98ab` |
| reverie submodule HEAD | `063fa37b05e562760f3d27c80ed8a6482b97b44a` |
| my PR branch (merged, worktree kept) | `fix/dbt-verify-capture-names-the-harness-scans-for` @ `53ecdac33868376c384dad19bf1de84a1d2faaa5` |

## 1. hermit PR 2724 is LANDED

https://github.com/rrnewton/hermit/pull/2724 — `state: MERGED`,
`mergedAt 2026-08-27T13:06:36Z`, method **rebase**, merge commit
`e4eee6aae66bdbe21e3fa63de6a93e1ecb0be9eb`.

Verified by content, not by the merge's report: `origin/main`'s
`hermit-cli/src/bin/hermit/backends.rs:821` reads
`temp_log_files_in("run1", "run2", verify_log_dir)`. The test file arrived
byte-identical.

`ci-hub/bin/gh-merge-verified 2724 --repo rrnewton/hermit -- --rebase` exited
**3 (UNDECIDABLE)**: `backends.rs` matched neither the PR head blob nor the
merge base, because main moved other lines in that file during the rebase. That
is the case the tool's own text says to read by hand. The single changed line is
present, so nothing was lost. Do not read exit 3 here as a failed landing.

## 2. A LOST VALIDATE RESULT — read this before trusting any green

I launched a detached validate on `6172478eae288c9f5005545d4f68c00c780f41c0`,
unit `validate-hermit-136-6172478eae28-1787831741880091578-1009495-b0024242.service`.
At handoff the unit is **no longer present in `systemctl --user list-units --all`**
and I never captured its log path. **There is no verdict and no artifact.** I am
recording it as lost rather than as pending. The standing revalidate obligation
is therefore NOT discharged by me.

## 3. Disk attribution — the standing owner directive

### The instrument, which inverts an earlier reading including my own

`du` overstates by about **2.8x** on this filesystem and the cause is **zstd
compression, not reflink sharing**. btrfs reports uncompressed blocks in
`st_blocks`. Measured with `compsize`, which reports on-disk extents:

| path | du | compsize on disk | compression |
|---|---|---|---|
| `ignored/land-1586` | 29.0 GiB | 9.9 GiB | 37% |
| `/tmp/hermit-005` | 32.3 GiB | 12.0 GiB | 38% |

Sharing is present but minor (Referenced 31G vs Uncompressed 26G for
land-1586). **Anyone computing a reclaim should divide du by ~2.8 and should not
assume the excess is shared extents.** `btrfs qgroup show` is unavailable here:
it fails with "Could not search B-tree / Input/output error".

### Filesystem ground truth (btrfs filesystem df/usage, no du involved)

```
Data used         2.40 TiB of 2.87 TiB allocated
Metadata used   116.42 GiB of 129.00 GiB allocated (DUP -> 232.8 GiB of device)
Device allocated 3200.02 GiB of 3502.22 GiB; unallocated 302.19 GiB
```

**Metadata at 90% of its allocation is the number nobody has been watching.**

### compsize totals, the numbers to reuse

| subtree | on disk | uncompressed | referenced |
|---|---|---|---|
| `dev-hermit/ignored` | **665 G** | 1.7 T | 2.1 T |
| `dev-hermit/worktrees` | 191 G | 426 G | 497 G |
| `dev-hermit/scratch` | 104 G | 305 G | 494 G |
| `dev-hermit/hermit` | 40 G | 84 G | 91 G |
| `dev-hermit/reverie` | 18 G | 43 G | 49 G |

`/tmp` was not compsize'd before handoff; `du` says 923.6 GiB, so roughly
330 GiB on disk by the same ratio. **Treat that one as an estimate.**

### A region that scans structurally miss

**`/tmp` is a separate mount point on the same `/dev/md0`.** Any
`du -x /home/newton/...` sweep never touches it. It holds 923.6 GiB by du and
**111,532 entries**.

### The named producer

`ignored/` holds 1028 entries, 736 directories, 2052 GiB du / **665 GiB on
disk**. Median entry is 0.014 GiB, so this is not small-file accumulation:
**64 entries over 10 GiB carry 71% of the tree**, each a checkout with a Rust
target directory, and **47 of the 64 untouched for more than two days**.

The largest named group is created by our own tooling:
**`ci-hub/validate/start_unit.py:780` runs
`mktemp -d ignored/validate-fresh-XXXXXXXX` for every validate run.**
**68 survive, 564.2 GiB du, 27% of `ignored/`**, mean 8.3 GiB, max 25.5 GiB.
A removal function exists — `remove_fresh_checkout` at `start_unit.py:898`,
called from seven sites — so the defect is that it does not reach these runs,
not that cleanup was never written. **Two of the 68 are mine from tonight.**
Next groups: 24 `land-*` at 311.2 GiB (all ~22 days old); 23 `hermit-126-*`
target directories at 137.4 GiB.

`/tmp` repeats the pattern with no tooling owner: **208 entries over 1 GiB hold
88.6%** of its 923.6 GiB; the 16 largest are agent checkouts of 10-37 GiB.
Separately, 114,685 measured entries under 1 MiB total only 4.425 GiB — trivial
space, plausible contributor to the metadata pressure above.

### Two hypotheses tested and REFUTED, with the numbers that refute them

**(a) "181 slots each with a build tree are most of the filesystem."** No.
`worktrees/` is 191 GiB on disk against 2.40 TiB used, and the reclaim available
inside it is single-digit GiB — see (b).

**(b) The tick's 208.92 GiB reclaim figure is a CACHED number.**
`slot_disk_residue.py` stores `{size_bytes, sized_at}` in
`.tick-hub/slot-disk-residue-state.json` and reprints it; re-running does not
re-measure. Measured fresh with the tick's own flags:

```
slot                          cached     now   sized_at
dap                           102.99    1.61   2026-08-27T05:21:04Z
slot16                         18.79    0.08   2026-08-26T18:01:29Z
impl-2373-pidfd                15.79    0.02   2026-08-27T05:21:04Z
validate-fix                   14.66    0.02   2026-08-26T18:41:44Z
tls-rfc                        12.01    0.06   2026-08-26T18:16:29Z
rework-2359                    11.27    0.02   2026-08-26T17:45:58Z
rework-2246                     8.05    0.03   2026-08-27T05:21:04Z
implement-hygiene-runner        6.27    0.02   2026-08-27T01:08:24Z
rev-triage-b2                   5.32    5.32   2026-08-27T05:21:04Z
fix-main-4b9-pressure-path      2.89    2.89   2026-08-27T05:21:04Z
rework-2280                     2.77    0.04   2026-08-27T05:21:04Z
mainval-f972                    2.26    0.12   2026-08-27T05:21:04Z
006-dbterr                      2.23    0.03   2026-08-26T22:57:36Z
cumulative-six-validation       1.93    0.02   2026-08-27T05:21:04Z
015                             1.70    0.03   2026-08-26T19:41:59Z
TOTAL                         208.92   10.29
```

The only two that did not shrink, `rev-triage-b2` and
`fix-main-4b9-pressure-path`, are **both protected**, and are 8.21 of the
surviving 10.29 GiB. **Reclaiming every unprotected reclaimable slot frees about
2.1 GiB.** The summary line carries no staleness marker, so a value up to 20
hours old reads as present state. That is a real defect in the tick and it is
the reason a refuted hypothesis looked confirmed.

**(c) "952 hermit worktrees are an unmeasured population."** They are not a
separate population. `git worktree list` in hermit reports 952 and all 952 paths
exist, distributed across locations already measured: `/tmp` 340,
`dev-hermit/worktrees` 259, `dev-hermit/ignored` 155, `/home/newton/scratch` 72,
`dev-hermit/hermit` 34, `/home/newton/worktrees` 25, 57 elsewhere. Same bytes,
different label. What IS true: nothing enumerates them for disk purposes, and
340 sit in `/tmp`, the region no sweep covers.

### The answer to the owner's question, in one sentence

Each work unit — validate run, landing, review, ad-hoc agent checkout —
materialises a full source tree with a Rust build directory, in three locations
(`ignored/`, `/tmp`, `worktrees/`), of which only `worktrees/` has a tick that
enumerates it, and that tick reports cached sizes.

### Nothing was deleted and no slot was touched.

The highest-value next step is a cleanup owner for `ignored/validate-fresh-*`
(the one producer that is unambiguously ours and unambiguously regenerable), and
finding why `remove_fresh_checkout` does not reach 68 completed runs.

## 4. Evidence files (untracked, under `ignored/hermit-136/`)

`merge-window-check.sh`, `stability-by-attempt-counts.py`,
`pressure-test-stale-or-reverted-ANSWER.md`, `dbt-logdir-probe/`,
`dbt-logdir-AFTER/`, `ptrace-logdir-control/`.
These are gitignored and a reboot that clears nothing will keep them, but a
`ignored/` sweep will not. They are working notes, not results; every result is
in this document or in the TaskGraph notes on
`standing-what-is-consuming-three-and-a-half-terabytes`.
