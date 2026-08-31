# What would retire earlier evidence after 20 clean runs

Date: 2026-08-30
Task: `what-would-retire-an-earlier-divergence-record-for-a-cell-that-now-passes-20-of-20`
Current repeated-run Hermit: `5b811757980b9cab599206c047560a7962176a6b`
Current repeated-run Detcore tree: `077a5d847f6541daa5ef145536a7065868e9d3ab`
Repeated-run summary SHA-256: `944bf8af55c1102757a068818e840ee7f8e083f9855f9e302b1eddfd363e4cb4`

This is a read-only evidence audit. It changes no activation or scorecard state.

The predecessor report identified 32 verify cells that passed 20 of 20 repetitions cleanly with zero retries but retained an earlier structured result. One additional DBT cell, `backend-parity-c/dup-shared-offset/verify/dbt`, also passed 20 of 20 cleanly but has a published canonical divergence from the same Detcore tree. This report therefore covers 33 cells.

## Result

| Earlier evidence | Cells | Result of this audit |
| --- | ---: | --- |
| LiteInst `/proc/self/maps` determinism failure | 22 | The recorded cause is superseded by landed commit `f968fd753e7d58f8dcb4d0cc1350da8e0398f80f`; 19 cells lack retained per-cell raw coordinates |
| DBT retained-log infrastructure error | 8 | Superseded by landed producer-name fix `e4eee6aae66bdbe21e3fa63de6a93e1ecb0be9eb` |
| DBT emitted no terminal canonical comparison | 1 | Superseded by landed canonical-verdict path `f0584c1aac9d6413601a7ce73410dec4c2d7a6db` |
| DBT pipe-capacity parity failure | 1 | Still live: the historical commit and raw log are unavailable, and no landed change was found that directly explains the observed initial capacity changing from 65536 to 8192 |
| DBT canonical divergence at the same Detcore tree | 1 | Still live: the earlier divergence is at record 80 / syscall 32 on the same Detcore tree as the 20 clean repetitions |
| **Total** | **33** | **31 have a landed change that supersedes the recorded reason; 2 remain live** |

For the task's original 32-cell population, the split is 31 whose recorded reason is superseded and one whose earlier parity failure remains live. The same-Detcore-tree cell is the additional thirty-third record.

This does not decide whether the 31 cells should become green. It establishes the evidence the owner would need for that decision.

## What retires an earlier record

Twenty matching repetitions alone do not retire a prior divergence. If the underlying divergence rate were 2 in 124, the probability of observing zero divergences in the next 20 independent repetitions would still be about 72.2%: `(122/124)^20`.

An earlier record is superseded only when all of the following are available:

1. The earlier result is bound to a source tree and its recorded cause or missing-evidence path. A divergence should also retain its first divergent record or syscall when that evidence still exists.
2. A landed change between that source tree and the current measurement changes the causal product or evidence path. A test or before/after measurement must distinguish the old and new behavior.
3. The change does not weaken an assertion, tolerance, comparator, label, selection rule, or evidence gate.
4. The current repeated measurement runs after the fix under the same or stronger canonical comparison contract.

If raw evidence is gone, a clean rerun cannot reconstruct it. The recorded cause can be superseded only when the retained description is specific enough for an independently proven landed fix to address it. Otherwise the prior result remains live or unexaminable.

## 22 LiteInst `/proc/self/maps` records

All 22 current cells passed 20 of 20 cleanly at Hermit `5b811757980b9cab599206c047560a7962176a6b`, after the fix below.

### Three individually retained divergences

| Cell | Earlier Hermit and Detcore tree | Earlier coordinate and content | Result |
| --- | --- | --- | --- |
| `backend-parity-c/cwd-roundtrip/verify/liteinst` | Hermit `e73e7ba0bf56fbbfefffa304f7622c67aac6ea88`; Detcore `b87e38bfe460884ef498488625123e4e9fc866e1` | Scheduler turn 10; host-assigned `/proc/self/maps` inode digit width produced a 672-RCB / 6,720ns virtual-time divergence | Recorded cause superseded by `f968fd753e7d58f8dcb4d0cc1350da8e0398f80f` |
| `backend-parity-c/personality-domain/verify/liteinst` | Hermit `17107307a33b6f6383c1b7bc2acb364b162b6c8c`; Detcore `b87e38bfe460884ef498488625123e4e9fc866e1` | Scheduler turn 10; an initial 12-RCB / 120ns drift accumulated to 672 RCB / 6,720ns | Recorded cause superseded by `f968fd753e7d58f8dcb4d0cc1350da8e0398f80f` |
| `backend-parity-c/vectored-io/verify/liteinst` | Hermit `17107307a33b6f6383c1b7bc2acb364b162b6c8c`; Detcore `b87e38bfe460884ef498488625123e4e9fc866e1` | Scheduler turn 10; an initial 12-RCB / 120ns drift accumulated to 672 RCB / 6,720ns | Recorded cause superseded by `f968fd753e7d58f8dcb4d0cc1350da8e0398f80f` |

The structured reasons retain the coordinate and numeric drift. The original raw log pairs are not present in the current checkout, so this report does not infer any additional record content.

### Nineteen population-quarantined cells

These cells cite Hermit `8f6d8a57d6a8e4ef7fa0ae14c16e2a494a2c0ab5`, Detcore tree `93a7767b4336f25cd677332cc4152e0562857634`, and TaskGraph task `ov-backend-ratchet-fanout`:

- `backend-parity-c/aio-refusal/verify/liteinst`
- `backend-parity-c/event-delivery-ordering/verify/liteinst`
- `backend-parity-c/eventfd-semantics/verify/liteinst`
- `backend-parity-c/fcntl-owner/verify/liteinst`
- `backend-parity-c/file-io-roundtrip/verify/liteinst`
- `backend-parity-c/membarrier-query/verify/liteinst`
- `backend-parity-c/mkdir-rmdir/verify/liteinst`
- `backend-parity-c/o-tmpfile-anon/verify/liteinst`
- `backend-parity-c/pipe-capacity/verify/liteinst`
- `backend-parity-c/record-lock/verify/liteinst`
- `backend-parity-c/sendfile-copy/verify/liteinst`
- `backend-parity-c/set-tid-address/verify/liteinst`
- `backend-parity-c/signal-delivery-sequence/verify/liteinst`
- `backend-parity-c/symlink-ops/verify/liteinst`
- `backend-parity-c/umask-mode/verify/liteinst`
- `c-programs/ioctl-fioclex/verify/liteinst`
- `c-programs/pause-alarm-interrupt/verify/liteinst`
- `system-utils/clock-determinism/verify/liteinst`
- `system-utils/record-getpid/verify/liteinst`

The durable population evidence says 20 of 26 LiteInst candidates diverged at the shared startup `/proc/self/maps` path; the representative `pipe-capacity` comparison first differed at a `/proc/self/maps` COMMIT at scheduler turn 10. Two of the six apparent three-pass survivors later failed. The removed investigation worktree held the per-cell raw summaries, so an individual old divergence coordinate cannot now be established for any of these 19 cells. Their structured reasons are population quarantine records, not 19 independently retained raw divergence records.

### Landed causal fix

Commit `f968fd753e7d58f8dcb4d0cc1350da8e0398f80f` changes `/proc/*/maps` rewriting so the determinized inode preserves the pathname column rather than copying the host-dependent whitespace count. Before the fix, LiteInst `/memfd:liteinst2-trampoline` mappings produced two outputs in 20 runs, split 14/20 and 6/20, differing by one space as the host inode crossed a power of ten. A strict verify fixture passed 10/20. After the fix, the direct maps probe was identical 20/20 and the strict verify fixture passed 20/20.

The regression test applies the same mapping on either side of an inode digit-width boundary and requires byte-identical output. Reverting the implementation restores the one-space failure. The comparator, virtual-time fields, record selection, and acceptance gate were not relaxed. The fix is a descendant of all three historical LiteInst evidence trees and an ancestor of the current 20-repetition campaign.

For the three individually retained divergences, the fix directly addresses the named cause and coordinate. For the other 19, it supersedes the recorded population reason, but the missing raw per-cell coordinates mean it does not prove that no unrelated rare divergence ever occurred in those cells.

## Eight DBT retained-log infrastructure errors

The eight cells are:

- `c-programs/add-key-enosys/verify/dbt`
- `c-programs/cachestat-enosys/verify/dbt`
- `c-programs/futex-waitv-enosys/verify/dbt`
- `c-programs/get-robust-list-self/verify/dbt`
- `c-programs/ioctl-fioclex/verify/dbt`
- `c-programs/kcmp-eperm/verify/dbt`
- `c-programs/keyctl-enosys/verify/dbt`
- `c-programs/listmount-enosys/verify/dbt`

The earlier evidence is not a divergence. Three retained runs per cell at Hermit `d77c8d0b145b9ef1ca51f9a29b54214838f922dc` and `90e46cba9ad83550205f3c9cd232ae222c6b6173`, both with Detcore tree `284e94bf4ddd166180b384c212dba8ac39c3494e`, reached canonical `matched` verdicts with bitwise parity. A focused probe at Hermit `94dd9f445ab81607cad10279f3fa5f7f0ada88a0`, Detcore tree `d6627566f5b5f4a7cc649168dd37c9f086a314ee`, showed that DBT retained two nonempty 47,507-byte logs as `dbt-run1_log_*` and `dbt-run2_log_*`, while both evidence readers required exactly one nonempty `run1_log_*` and `run2_log_*`. The evidence gate therefore classified the matched runs as `infrastructure-error`. There is no divergence coordinate because the comparisons matched.

Commit `e4eee6aae66bdbe21e3fa63de6a93e1ecb0be9eb` changes the producer to the names the readers already require. It does not broaden the readers: missing, empty, and duplicate captures remain rejected, and the regression test checks both the accepted names and absence of the old names. The current campaign's 20 clean repetitions per cell are after this fix and passed the unchanged retained-evidence gate.

The earlier infrastructure reasons are therefore superseded by a landed fix to the exact evidence-production defect.

## `backend-parity-c/host-identity/verify/dbt`

The earlier structured record was introduced at Hermit `88ccebce2e0d3c34465ae7cf86ea197893d2c32e`, Detcore tree `ef2c66773d999685c480172de5c8f85fcef97d4e`. It cited the DBT return at `hermit-cli/src/bin/hermit/run.rs:2163`: DBT completed its two guest executions and returned before the common verifier, leaving `verdict=no_result` and `comparison=null`. No divergence coordinate exists because no canonical comparison was published, and no raw run artifact is retained with that source citation.

Commit `f0584c1aac9d6413601a7ce73410dec4c2d7a6db` is a descendant of that record and an ancestor of the current campaign. It makes DBT materialize the protected INFO records and call the existing canonical `compare_two_runs` path with canonical strictness and `compare_logs=true`; it writes the ordinary typed verification JSON. Empty, malformed, truncated, unframed, zero-record, and zero-INFO evidence fail closed. The comparator and canonicalization policy were not relaxed.

The current 20 clean repetitions therefore exercise the evidence path that the earlier record said did not exist. The earlier `unavailable` reason is superseded by the landed verdict-publication fix.

## `c-programs/pipe2-errno-precedence/verify/dbt`

The durable TaskGraph record binds the earlier run to Hermit `637b0b19d63facb4cc77174334ed325132b83516`: the EFAULT/EINVAL checks passed, then the positive control observed an initial pipe capacity of 65536 instead of the required Detcore capacity 8192. Run 1 exited 1 before comparison. This is a guest-oracle backend-parity failure, so there is no first divergent record or syscall coordinate.

The `637b0b19...` commit object and its raw run log are no longer reachable locally. Its Detcore tree therefore cannot be recovered, and this report does not infer one from the later successor branch. The retained result survives in TaskGraph and in the landed manifest reason at `3c4ee7efc46c3a1776ecbc3449b63691626a5897`.

Commit `0bf402ecaaa009305c50617f71518bbde904684c` was examined because it is the obvious intervening pipe-capacity change. It refuses guest `F_SETPIPE_SZ` growth above 8192 and sanitizes `/proc/sys/fs/pipe-max-size`; its own before-state already reports an initial capacity of 8192. The failing fixture does not request growth: it checks `F_GETPIPE_SZ` immediately after `pipe2`. That commit therefore does not directly explain the historical DBT value changing from 65536 to 8192. No other landed change between the published successor and the current campaign was found that changes the creation-time capacity pin for this DBT cell.

The 20 clean current repetitions show that the current built path reports 8192. They do not establish why the earlier path reported 65536. Without the old source tree or raw artifact, the earlier parity failure remains live and partly unexaminable.

## `backend-parity-c/dup-shared-offset/verify/dbt`

The published parent series at dev-hermit `90a55d8190dd2551179a9394be3504ee89a6a5a9` retains both passes and a canonical determinism failure from run `pressure-e12c472d22d2-red-verify-3x-20260830T151049Z`:

- Hermit tree: `e12c472d22d2725aa1e58b7169c4a44bf82fc890`
- Detcore tree: `077a5d847f6541daa5ef145536a7065868e9d3ab`
- first divergent record: 80
- first divergent syscall: 32
- left message: an added `fstat` to retrieve file metadata on fd 4
- right message: `openat(..., O_CREAT | O_EXCL | O_RDWR, ...) = EEXIST` for the fixture's temporary file

The current 20 clean repetitions use Hermit `5b811757980b9cab599206c047560a7962176a6b` and the identical Detcore tree `077a5d847f6541daa5ef145536a7065868e9d3ab`. The two Hermit commits are sibling harness branches, and no Detcore fix separates them. The published series preserves the coordinate and first messages, but the original retained log pair was not found.

This is the proposed four-state rule's same-coordinate-history case: a later match does not replace or retire a divergence at the same product tree. The earlier divergence remains live until its cause is found and fixed, or stronger retained evidence establishes that the published result came from outside the cell's declared execution contract.

## Goalpost-moving assessment of the three explanatory fixes

- Assertions weakened: no. The LiteInst fix adds a byte-identity regression; the DBT log-name fix adds exact accepted-name checks; the DBT verdict fix adds fail-closed evidence checks.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. The canonical comparator is unchanged; the retained-log readers are unchanged; the DBT path is moved onto the canonical comparator.
- Failure renamed or relabelled as a pass: no. The DBT log-name change exposes two real divergences that were previously hidden under infrastructure errors.
- Check deleted instead of satisfied: no. The product output or producer path is changed to satisfy the existing checks.

## Evidence limitations

- The 19 population-quarantined LiteInst cells do not have retained per-cell raw coordinates. Their durable record names a shared cause and population result only.
- The original raw log pairs for the three individually named LiteInst divergences are not present; their structured reasons retain the coordinate and numeric drift.
- The `637b0b19...` pipe2 evidence commit and raw log are unavailable, so its Detcore tree cannot be established.
- The `dup-shared-offset` raw log pair was not found, although the published series retains the exact record/syscall coordinate and first differing messages.

No missing field above is filled by inference, and no cell is activated by this report.
