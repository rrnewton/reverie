# Plain-main baseline — hermit 1ce198d2, 2026-08-27

The comparison reference for landing measurements: the set of cells and nodes that
fail on plain `main` with no branch content, so a branch can be judged by whether it
adds to this set.

## The three coordinates

A baseline naming only the shas is not enough. Two runs at identical shas on hosts
with different capability sets measure different things and will disagree for reasons
no sha explains. `cpuid_fault` is the clearest case: it gates whether the privileged
lane runs at all.

| coordinate | value |
| --- | --- |
| hermit sha | `1ce198d2f224240ea7e648e92a10e6e8d7f91a93` |
| reverie pin | `86d9003a7a2a8d5399ef94a251e4d991d6c504a5` |
| host | devbig030.atn3.facebook.com |
| kernel | `6.19.2-0_fbk0_hardened_rc25_0_g1d99d2f6053c` |
| cpu | AMD EPYC 9D85, 316 cores |
| capabilities | `cpuid_fault` present on all 316 cores — the privileged lane RAN |
| conditions | load 11–29 throughout; no competing hermit validation at launch |
| run | 66 nodes planned, 66 started, 66 finished; wall 1429s, cpu 4405s |
| retries | every failing node was retried twice and failed both attempts |

Denominators, so any count can be read: **358** test ids, **677** declared cells
(id × mode × enabled backend), **342** with `ci:true`, **66** executed nodes.

## Failing cells — 8 distinct

    applications/timed-progress-bar (verify/ptrace)
    c-programs/fp-reduction-nondeterminism (chaos/ptrace)
    c-programs/record-replay-lseek-seek-cur (verify/ptrace)
    c-programs/record-replay-lseek-seek-cur (verify/sabre)
    language-runtimes/python-dict-hash-iteration (verify/ptrace)
    system-utils/du-tree-summary (verify/ptrace)
    system-utils/example-date (verify/ptrace)
    system-utils/find-tree-metadata (verify/ptrace)

## Failing nodes — 16 distinct

    check.backend_parity_suites
    check.exit_status_class
    doc.rustdoc
    e2e.manifest_applications
    e2e.manifest_c_programs
    e2e.manifest_language_runtimes
    e2e.manifest_system_utils
    privileged-e2e.manifest_applications
    test.applications_e2e
    test.app_strict_verify
    test.cli
    test.command_strict_verify
    test.hermit_integration
    test.hermit_modes
    test.hermit_unit
    test.liteinst_strict

## Why the raw log is not committed here

The run log is 2,699,457 bytes, above this repository's 2 MiB threshold for a text
file, so it is not on `main`. **It is committed, on a ref of its own**, because as a
git blob it costs 203,162 bytes — the threshold is about file size, not storage:

    ref     refs/heads/evidence/hermit-106/baseline-1ce198d2-run-log
    commit  86d998cab554203ce2097597edab05d1b080f7da
    path    validate-hermit-106-1ce198d2f224-run.log
    sha256  b9d9784429b4f9c78cb621382de8d045ad50a3164397c1e2afecccc289a13508

Verified by fetching the ref back and comparing: 2,699,457 bytes, sha256 identical.
Retrieve it with:

    git show refs/heads/evidence/hermit-106/baseline-1ce198d2-run-log:validate-hermit-106-1ce198d2f224-run.log

The original also still exists on devbig030 at
`~/work/dev-hermit/ignored/validate/validate-hermit-106-1ce198d2f224-1787821450812849209-1017041-cdc50011.log`,
but that copy is in an ignored directory on one host and nothing enumerates it; treat
the ref above as the record and that path as a convenience.

⚠️ **A FAILING RUN WRITES NO LEDGER ROW.** `ci-hub validate-status` reports this sha
as `NOT-VALIDATED` with "0 non-qualifying record(s)", because the ledger records
qualifying passes. So the ledger structurally cannot hold a red baseline — the
artifact this project most needs to keep is the one the system is least able to
store. That is why this file exists: without it the reference lived only in an
ignored log on one host, on no branch and in no commit.

## Comparison with the devbig014 measurement, and its limits

A second measurement was taken on devbig014 at hermit `fa76f4e66b`, same reverie pin,
kernel `6.13.2-0_fbk17_hardened`, `cpuid_fault` **absent**, under load 107 with two
duplicate validations running. It was sampled at 64 of its node set and never
completed.

Set-difference language between the two is meaningful **only for cells whose nodes ran
on both**. Outside that intersection the comparison is undefined rather than noisy,
and a merged picture would have looked like a clean subset when it is not one.

* In both (7): `timed-progress-bar`; `fp-reduction-nondeterminism`;
  `record-replay-lseek-seek-cur` on ptrace **and** sabre; `du-tree-summary`;
  `example-date`; `find-tree-metadata`
* Only 6.13 (2): `c-programs/arch-prctl-determinism`,
  `determinism-stress-c/thread-contention` (15-second wall-cap breach)
* Only 6.19 (1): `language-runtimes/python-dict-hash-iteration`

**The tree is ruled out for both divergent cells.** `fa76f4e66b` is an ancestor of
`1ce198d2f224` by exactly one commit — "Remove the scheduled shard checker from the
allowlist", `-7` lines in `scripts/check-checker-scheduling.rs`. A CI allowlist file
cannot affect a C or Python guest determinism test. The kernel and `cpuid_fault`
difference is the best-supported remaining explanation; causation is **not** proven
and is testable by re-running those two cells alone on each host.

## One correction, recorded because acting on the error would have lowered the bar

An earlier reading of the devbig014 set classified seven of its nine cells as load
artifacts, on the grounds that exit 125 "verify exited before producing a terminal
comparison" is a kill signature. **Six of those seven reproduce here at load 11–29
with no competing validation.** Exit 125 is not a load signature.

Only one cell has any demonstrated load or time dependence:
`determinism-stress-c/thread-contention`, the wall-cap breach. **Exclude one cell from
the bar, not seven.** The other eight belong in the reference. Under a one-directional
landing rule — where a cell already failing on the baseline is exempt — a contaminated
baseline does not merely add noise, it silently lowers the bar and nothing reports it.
