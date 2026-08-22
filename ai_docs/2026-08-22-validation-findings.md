# Validation findings from 2026-08-22

## Scope and current unresolved state

This document records findings from `rrnewton/hermit` and
`rrnewton/dev-hermit` work on 2026-08-22. Commit names always include their
repository because the validation ledger covers more than one repository.

The central unresolved state is:

- `rrnewton/hermit` main at
  `264c54c7bdeed2194637a9e4329c7826f018b6ac` is red at
  `check.backend_abstraction`.
- [rrnewton/hermit#2359](https://github.com/rrnewton/hermit/pull/2359), exact
  head `4806f6d210763b54433792075e84e34e7b18aa76`, repairs that violation and has
  an independent exact-head approval with no findings or goalpost lowering.
  It has not landed.
- The repair's two exact-head full-profile runs are red. The canonical landing
  predicate requires `result=pass`; after the failure-classification repair in
  `rrnewton/dev-hermit` commit
  `eef5fd792a8ae026d913c68279dafe92838c1e1e`, the solo row correctly classifies
  as `KNOWN_FAILED`, never `VALIDATED`.
- Eight DBT cells cannot produce the canonical INFO comparison required by
  the manifest on the current code path. Deciding the evidence contract is an
  owner decision. The ninth `incomplete-verification-evidence` row is a
  separate ptrace case whose cause remains unresolved.
- The SaBRe physical-exit/reaping race is characterized but not fixed.
- `check.backend_abstraction` has a demonstrated behavior-changing bypass;
  its current text scan is necessary but not sufficient.
- Thirteen validation heads remain unavailable from their recorded upstream
  repositories. Whether any survive on the validating host is unknown.
- Reverie has working validation code but no active driver. The Reverie commit
  pinned by `rrnewton/hermit` main has no Reverie-side validation row.

No assertion, comparison rule, gate, or evidence requirement was weakened to
obtain these results.

## The backend-abstraction repair is measured green but cannot qualify

`rrnewton/hermit` commit
`343ebd3633bbf73bc12ca84ae5dd063e1464e746` introduced the current main
failure. It placed DBT-specific `reverie_dbt::evidence` filtering in
`detcore/src/logdiff.rs`, although the project boundary requires backend
selection and policy to remain outside Detcore core. Its parent
`8f258160cf0e021d01cce708e47bee3c69c9766b` passes the abstraction check;
`343ebd3633bbf73bc12ca84ae5dd063e1464e746` fails it and names six occurrences.

#2359 moves the classification into the always-compiled Hermit CLI layer and
uses generic caller-supplied filtering in Detcore. Record/replay remains
unfiltered, and DBT verification selects the DBT predicate. The exact head was
reviewed independently after a first review found and the author fixed a
`--no-default-features` compatibility hole.

Evidence at exact #2359 head
`4806f6d210763b54433792075e84e34e7b18aa76`:

- `check.backend_abstraction`: pass in 1.66 seconds in the first full run and
  1.61 seconds in the solo run.
- `check.dbt_runtime_abi`: pass in 1.13 seconds and 1.06 seconds.
- Hermit CLI tests: 155 passed with `--no-default-features`; 166 passed with
  `third-party-backends`; zero failed and zero were filtered in either run.
- Hermit Detcore tests: 445 passed. Detcore DBT tests: 17 passed.
- `rustfmt` and Clippy with `-D warnings`: pass in both build configurations.
- Transport-only logs refuse in the one-file, two-file, and follow paths;
  adding one real record admits comparison. These brackets pass in both build
  configurations.
- Independent review: approved at the exact head, no findings, no goalpost
  lowering.

The first full profile planned 60 nodes. Thirty passed, one failed, two were
aborted, two were dependency-skipped, and 25 had no terminal outcome. Its
`e2e.manifest_c_programs` node executed all 138 cells: 127 passed, two failed,
and nine ended in error. The prescribed solo `-j 4` rerun had zero concurrent
validations and reproduced exactly 127 pass, two fail, and nine error across
all 138 cells. It therefore did not produce a qualifying receipt.

The durable logs are:

- `ignored/validate/validate-full-4806f6d21076-20260822T061022Z.log`
- `ignored/validate/validate-full-4806f6d21076-20260822T063120Z.log`

The canonical run IDs are `devbig030-1787379575-1746743` and
`devbig030-1787380802-2946783`.

This creates the current landing deadlock: main's abstraction repair is the
change that clears the failing spine check, but landing requires a passing
full-profile receipt, and the pre-existing DBT evidence contract prevents the
current full profile from passing. The owner must decide how to resolve that
contract. A red ledger event existing is not a qualifying receipt.

## The nine incomplete-evidence rows are two findings

Eight error cells completed both DBT executions successfully:

- `add-key-enosys`
- `cachestat-enosys`
- `futex-waitv-enosys`
- `get-robust-list-self`
- `ioctl-fioclex`
- `kcmp-eperm`
- `keyctl-enosys`
- `listmount-enosys`

The top-level command writes a pending `verdict=no_result` before dispatch.
The DBT arm returns through `run_dbt`, bypassing `RunOpts::verify` and its
`compare_two_runs` / `write_verification_json` result channel. `run_dbt`
performs a separate comparison of exit status, stdout, native counters, and
memory hash, then may print `Determinism verified`, but it never constructs or
writes the canonical INFO verdict. Both runs have stderr in memory; only the
first is emitted, and the second is discarded after summary extraction.

The consumer is therefore not losing a completed canonical verdict: the
required verdict is never produced, and some raw material is also not
retained. This is a producer/consumer contract mismatch, not proof that DBT
cannot provide such evidence. The unlanded #2241 result is not equivalent: it
declares `compare_logs=false`, `guest_stderr_compared=false`,
`internal_log_messages_compared=false`, and `bitwise_parity=false`. Accepting
that as the current canonical INFO comparison would weaken the contract.

The ninth row, `dbt-unsupported-syscall` under ptrace, is separate. It has
`verdict=no_result` and no comparison or guest-exit record after a 64ms sandbox
failure. The retained evidence does not establish whether guest code started.

## The four SaBRe failures are the same shutdown race

Each full-profile run had 52 SaBRe verification cells: 50 matched and two
diverged. The cells changed between runs:

| Cell | INFO records | Final turn | Side with extra line |
| --- | ---: | ---: | --- |
| `getitimer-determinism-probe` | 134 / 133 | 3 | run 1 |
| `mmap-stress-determinism` | 643 / 644 | 3 | run 2 |
| `ioctl-fioclex` | 143 / 142 | 4 | run 1 |
| `syscall-file-metadata` | 195 / 196 | 5 | run 2 |

All four have the same signature:

- Both guests exit 0.
- The scheduler commits, final report, turn count, virtual time, SaBRe syscall
  DETLOG count, and combined DETLOG plus scheduler-COMMIT count agree.
- The final commit has the same resource shape:
  `Exit { group: true, process: DetPid(3), mm: MmId { creator: DetPid(3), generation: 0 } }`.
- `logically_kill` agrees.
- One side alone emits
  `scheduler (step2_process_blocked): zero threads left anywhere, fizzling`.
  Both sides emit `run queue empty, exiting sched_loop`.
- The SaBRe path evidence agrees: guest RPC observed, zero ptrace fallback
  sites, and zero trusted shared-object sites.

The later displayed syscall mismatches and final unmatched `exit_group` are
alignment fallout after the one inserted INFO record. They are not evidence
of syscall-state divergence. This is the same ordering previously retained
for `getsockopt-null`: when the `fizzling` line appears, the SaBRe supervisor
WARN follows scheduler exit; without it, the WARN precedes scheduler exit.
The timing depends on when physical-exit observation clears the barrier.

The four workloads share no substantive syscall: they exercise timers,
memory mapping, pipe/ioctl/fcntl, and file metadata. They are all ordinary
one-process, one-thread C guests that return through `exit_group`. Their
planned SaBRe positions are dispersed at 8, 14, 24, and 46 of 52; failures
moved from positions 8 and 24 to 14 and 46. There is no cell, syscall,
adjacency, direction, or execution-position correlation in these samples.

The observed frequency is four divergences among 104 SaBRe cell executions,
with exactly two in each of two runs. Two runs do not establish that exactly
two always fail. The solo `-j 4` run with zero concurrent validations did not
remove the race, but it does not establish a rate.

Removing or ignoring the conditional INFO line would remove the evidence and
change the comparator outcome. It would not fix the physical-exit/reaping
race.

## The abstraction check has a demonstrated bypass

The current check searches Detcore source text for prohibited backend module
names. Independent review of #2359 planted a live filter in
`detcore/src/logdiff.rs` that reconstructed a prohibited target from pieces:

```rust
const CRATE: &str = "reverie";
const MODULE: &str = "dbt::launcher:";
!line.contains(&format!("{CRATE}_{MODULE}"))
```

The mutation was wired into all three real filter sites. It changed the
shipped tool's observable result for two logs differing only in a launcher
record from a reported mismatch with exit 1 to “no substantive differences”
with exit 0. Despite restoring the original behavior defect:

- the check's grep found zero hits;
- `check-detcore-backend-abstraction.sh` exited 0 and reported the commandment
  intact;
- Clippy exited 0;
- all 445 Hermit Detcore tests passed.

The mutation was reverted and the review worktree was clean afterward. This
does not weaken #2359's approval; #2359 moves behavior in the correct
direction. It does establish that the text check can be bypassed by behavior
that does not spell the target literally. A wider text pattern loses to the
same construction. Detecting this class requires a behavioral oracle.

## Thirteen validation heads cannot be recovered upstream

After repository-aware lookup and exact-SHA fetches, eight initially missing
objects were fetched and pinned. Thirteen ledger-referenced validation heads
still return “No commit found” from their recorded GitHub repository:

```text
rrnewton/hermit  37a4dd6056257a42ee08bde744dbd1d488bd3be5
rrnewton/hermit  55135b25903ce5537c6a24c1fcce3ad40bad7d7e
rrnewton/hermit  5ffc082eb2a84ab4c13d7e6770fdf0f94ff251b8
rrnewton/hermit  625909c6195dec4d192f2c328839d8474eda997f
rrnewton/hermit  6b989e284db6008ade6e1b87c5de9c6f7dcd7633
rrnewton/hermit  6dd193d31491e79f89a8728678faf364abacef11
rrnewton/hermit  a07e0f148f9c92ad42badd37044031378f2fa8e1
rrnewton/hermit  a29316191089e7c64559bf3b623f8486afd041e6
rrnewton/hermit  b40955daf8dfe95f7f8f7a17cb83dfd6c5f31f9d
rrnewton/hermit  b5dc70a4450f81d8298125428a23570f24e4562e
rrnewton/hermit  bd2efd571f76d8763b2bcfa5ce1499506c54c135
rrnewton/reverie d4dbcd4377e4a32d2fa65969fee5ceed25db9603
rrnewton/reverie e33d3b742faf49087ee3845bca9836ce1466ae08
```

Four carry a PASS record: `625909c6195d`, `b40955daf8df`,
`bd2efd571f76`, and `d4dbcd4377e4`. Those receipts cannot now be tied back to
inspectable commit content. The other nine have only failing rows.

All 13 were validated on `devbig014`, but that host wrote about 92% of the
ledger rows in the measured population, and all eight successfully fetched
objects also came from it. The evidence does not support a host-specific
failure rate.

The ledger proves these commits existed on the validating host. It does not
establish whether they were never pushed, were later removed with a branch,
were rebased or squashed, or still survive on that host's disk. “They survive
only on that host” is an upper bound, not a confirmation.

## The third repository spelling was ignored by the Python pin mapper

The validation ledger's non-null `repo` values had three spellings:
`hermit`, `reverie`, and `rrnewton/reverie`. Five rows from
`ci-hub-reverie-validate-run` used the canonical `rrnewton/reverie` spelling,
covering four distinct commits. The Rust reader already aliases `reverie` and
`rrnewton/reverie`; the Python `ledger_referenced_objects.py` mapper knew only
the two short spellings and ignored the third source group.

The initial risk statement was too strong. All four distinct canonical-name
commits also appeared under the short `reverie` spelling and were already
reachable from refs. The mapper defect was latent in the observed ledger; it
had not yet left those objects unpinned. It would become active if the
canonical producer recorded a commit no short-name producer also recorded.

This is now resolved in `rrnewton/dev-hermit` main commit
`a54b0ff2533dbe27f986f054b622b76f376f5acf`. The activated mapper unions both
Reverie spellings before one checkout sync, routes legacy missing-repository
rows to Hermit, reports source counts separately from deduplicated checkout
counts, and refuses unknown repository values before any ref write. Its
focused suite passed 12/12. This resolution does not recover the 13 upstream-
unavailable historical heads.

## Reverie validation has a working script and no active driver

`reverie/validate.sh` still contains the intact `reverie-validate-sh` ledger
producer. No systemd timer, cron entry, or other discovered scheduler invokes
it. The parent-side `ci-hub-reverie-validate-run` path is also on-demand only;
its product-side enabling PR #441 remains open and conflicting.

The five centralized rows were produced during a ten-hour development window
on 2026-08-11--12, not by a production cadence that later stopped. Direct
`reverie-validate-sh` rows continued through 2026-08-18T02:52:29Z, then no
Reverie-side validation of either kind occurred for at least 4.1 days. No
newer Reverie validation logs were found, so this is absence of invocation,
not known failed attempts.

The current Hermit pin,
`rrnewton/reverie` commit
`f2e9839e5316e31c377089ae7f789515ba9d2550`, has no Reverie-side ledger row.
The pin bump itself changed only Reverie workflow YAML and was exercised by
Hermit-side tests before landing as `rrnewton/hermit` commit
`264c54c7bdeed2194637a9e4329c7826f018b6ac`. This document does not grade the
pin unsafe or claim policy requires a Reverie-side receipt. It records that
the Reverie-side validation was not performed.

## Decisions still required

- Decide the DBT verification evidence contract without relabeling the weaker
  DBT comparison as canonical INFO evidence.
- Decide how the validated abstraction repair may land while the qualifying
  receipt predicate requires a pass that the current DBT contract prevents.
- Decide whether and how to fix SaBRe's physical-exit/reaping ordering. Do not
  remove the differing INFO record as a substitute.
- Add a behavioral oracle if `check.backend_abstraction` is expected to detect
  obfuscated but live backend-specific filtering in Detcore.
- Decide what durability rule validation heads must satisfy before their
  ledger records can be treated as inspectable evidence.
- Decide what invokes Reverie's working validation producer and whether a
  Reverie-side row is required for pin updates.
