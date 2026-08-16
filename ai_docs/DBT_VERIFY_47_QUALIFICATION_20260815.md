# Qualifying the 47 `DBT + verify` rows of the qualifiable-fixture queue

Task: `qualify-dbt-verify-47`. Date: 2026-08-15. Host: devbig030.
Source population: the 47 `dbt`/`verify` rows of
`ai_docs/QUALIFIABLE_FIXTURE_QUEUE_20260815.tsv` — the last unallocated subset of that
file's 380 never-evaluated cells.

## Scope statement, adopted verbatim from hermit-revents and true of every number here

> Qualification is EXIT-STATUS REFUSABILITY UNDER THE STRIPPED COMPARATOR — `parity=false`
> on every accept arm — NOT PARITY AND NOT L2.

This is not boilerplate. The ptrace control on `backend-parity-c/set-tid-address` under bare
`--verify` reports `strictness="stripped"`, `verdict="matched"`, `verified=true`, and
`bitwise_parity=false`, on a cell that passes.

## Invocation, stated so it can be compared to other slices

Reproduces `ci/test_harness.sh` literally rather than calling it:

| harness element | source line | what I ran |
| --- | --- | --- |
| compile | `:3254` | `cc -std=c11 -O2 -g -Wall -Wextra -Werror <build.cflags> <src> -o program` |
| cell dirs | `:2365-2371` | `home/ xdg-config/ tmp/ fixtures/`, `tests/e2e/xdg-config` copied in |
| guest env | `:3390-3400` | `LC_ALL=C TZ=UTC HOME=… XDG_CONFIG_HOME=… E2E_TMPDIR=… E2E_FIXTURE_DIR=…` |
| verify tmpdir | `:3384-3388` | `E2E_TMPDIR=/tmp/hermit-e2e` |
| guest args | `:3409-3412` | `modes.verify.guest_args[<backend>]` |
| verify command | `:3465-3468` | `hermit --log=info run --backend dbt --strict --verify --verify-json <report>` |
| portable profile | `:3431-3434` | `--no-virtualize-cpuid --max-timeslice=disabled` |
| capture | `:2387-2402` | `timeout --verbose --kill-after=10s <cell timeout>`, stdin `</dev/null>` |

No `validate.sh`. Portable profile only; no full and no privileged profile.
**Nothing here is a receipt.** Binary: `hermit/target/release/hermit`,
`hermit 0.2.0 (2026-08-14, ge2f9a36b1ad8)`, checkout `main` @ `302a1a9c0`.
Host load 38–63 across the sweeps on 316 cores.

## Gate 1 — the DBT backend runs on this host

Established before any cell was touched, because the dispatch required it and because a
sibling backend on this host cannot mint receipts at all.

```
hermit run --backend dbt -- /bin/true
  hermit: [dbt backend] Detcore Tool active; running "/bin/true" under DynamoRIO
    (hermit/target/install_pkg/rsrcs/dynamorio/bin64/drrun)
  reverie-dbt: tool=Detcore branches=47533 syscalls=32 rewritten=31 stdin_reads=0
               memory_hash=cbf29ce484222325
  rc=0
```

DynamoRIO resolves from the packaged tree, not from an environment override.
`--backend dbt -- /bin/echo HELLO` prints `HELLO` and exits 0 with `tool=Detcore`.

## Stale-fixture check — a graded negative, not a skipped step

Checkout is `main` @ `302a1a9c0`; `origin/main` fetched and identical (0 ahead, 0 behind);
working tree clean. All 47 programs compared against `origin/main:<path>` **by Git blob**:
47/47 byte-identical, 0 mismatches. The stale-checkout inversion cannot apply to this slice.

## Two corrections applied before anything was scored

Both moved cells, so the uncorrected numbers are reported alongside.

1. **`guest_args` is a per-backend dict and `dbt` has no entry in any of the 47.** Run with
   no arguments, seven fixtures die in argument parsing, print a usage line to **stderr**,
   and exit 2 — a non-zero exit that is not a refusal. Re-run with the arguments the cell
   intends (its `ptrace` column), six of the seven pass.
2. **`E2E_TMPDIR`.** The harness sets `/tmp/hermit-e2e` for every non-naked mode; a
   host-backed directory leaks run-one state into run two.

Accept-arm green went **33/47 uncorrected → 39/47 corrected**.

Per-cell `build.cflags` were read and used: five distinct sets across the 47, nine of which
need `-pthread` or `-O0`.

## Method — both arms, with a ptrace control on both

* **Accept arm:** the unmutated fixture must exit 0 under `--backend dbt`.
* **Refuse arm:** a semantic mutation of the same fixture must make `--backend dbt` exit
  non-zero.

A refuse-only bracket cannot tell perfect detection from a harness that never ran, so both
arms were run for every cell, and every arm carries a `--backend ptrace` control on the
identical binary, arguments and environment.

**Mutation families**, applied in a fixed order decided before any DBT answer was read:

* `m2` guard-inversion — rewrite the first `if (COND)` whose error path lies within eight
  non-blank lines to `if (!(COND))`, with comments stripped first so prose cannot anchor the
  edit. This makes correct behaviour take the fixture's own error path, which is what a real
  backend defect would do.
* `m3` ok-init — the queue's own family name for the
  `return ok == EXPECTED_CHECKS ? EXIT_SUCCESS : EXIT_FAILURE` shape: seed the counter one
  high so a fully correct run cannot meet the gated count.
* `m1` terminal-exit — rewrite the final `return 0;` of `main` to `return 23;`. Mechanical;
  proves the exit channel is live end to end and nothing more.

**Mutation-selection rule, blind to the DBT result.** For the eight cells whose first mutant
did not qualify, the guard index was iterated and the first mutant **the ptrace control
refused** was kept; only then was the DBT arm read.

**The mutation is proven to bite.** `.text` was compared, not the whole binary, because an
ELF build-ID difference defeats a whole-binary diff. 46 of 47 mutants compiled and all 46
changed `.text`.

## Result

Counted by **exact match** on one verdict field in `meta/FINAL.json`, one verdict per cell.
The three tokens are chosen so that none is a substring of any other.

| verdict | n | meaning |
| --- | --- | --- |
| `REFUSABLE` | 37 | accept arm exits 0 under dbt and a semantic mutation makes dbt exit non-zero |
| `REFUSEARM-UNPROVEN` | 1 | accept arm green, but no valid refuse arm was established |
| `PARITYASSERT-BLOCKED` | 1 | both exit arms hold, but the cell's own extra gate cannot pass under dbt |
| `ACCEPTARM-RED` | 8 | the unmutated fixture already fails under dbt; nothing to qualify |
| **total** | **47** | |

### Amendment: what each refuse arm actually exercised

An initial pass reported `REFUSABLE 38`. Two corrections, both against my own result:

**One cell is demoted to `REFUSEARM-UNPROVEN`.** `c-programs/dbt-wait-lifecycle`'s first mutation was
inert, and the retry inverted `argc != 1` — so the mutant printed
`usage: … [--accounting-only]` to **stderr** and exited **64**. That is a usage line from argument
parsing, which is misclassification mode 5: a non-zero exit that is not a refusal. The
ptrace-gated selector accepted it because it only asked whether ptrace refused, never *why*. No valid
refuse arm exists for that cell.

**The remaining 37 are not equally strong, and the difference is worth more than the total.** The
guard-inversion family inverts the first guard whose error path is nearby, and on many of these
fixtures the first such guard is *setup plumbing* rather than the parity assertion. Classifying each
refuse arm by the error path it actually reaches:

| what the refuse arm exercised | n | meaning |
| --- | --- | --- |
| semantic | 13 | the inverted guard, or the seeded counter, routes through the property the cell exists to test — e.g. `io_setup populated ctx=%lu on refusal`, `semget rc=%d errno=%d (want -1/ENOSYS)`, `madvise(%d) expected errno %d, got %d` |
| plumbing | 22 | the inverted guard was a setup or I/O check — e.g. `mkstemp`, `pipe`, `socketpair`, `sysconf`, `pthread_barrier_wait`. Proves the fixture's `fail()` path reaches the exit gate under DBT; does **not** show the cell would catch a defect in the property it tests |
| mechanical | 2 | terminal-exit rewrite; proves exit propagation and nothing else |

`backend-parity-c/sendfile-copy` is the worked example and it converges with three independent
routes that call the fixture scaffolding-only: its `checksum`, `pos` and `own_offset_kept` are
**printed and never gated**, so `own_offset_kept == 0` still exits 0. The refuse arm I obtained came
from inverting its `mkstemp` guard.

**Reading the strongest subset off these two axes: 11 cells are `verify.ci = true`, semantic, and
stable.** Of the 19 immediately-greenable `REFUSABLE` cells, 11 are semantic, 6 plumbing, 2
mechanical.

### The unstable cell

**The classification of one cell depends on the draw, and it is reported that way deliberately.** `backend-parity-c/sendfile-copy` has an unstable accept arm (26 of 27 observed runs
exit 0; the 27th exits 1). On a run where its accept arm exits 0 it classifies `REFUSABLE`; on the
observed refusal it classifies `ACCEPTARM-RED`. A flat `38` would assert a stability that was
measured not to hold. This disagreement was found by re-deriving the counts from the raw per-cell
capture files rather than from the results JSON — the two routes returned 38/8 and 37/9, and that one
cell was the whole difference.

`INERT`, `BUILD-FAILED` and `TIMEOUT` are tokens in the same vocabulary and their final
counts are **zero** — measurements, not absences. Three first-attempt mutants were inert or
uncompilable and were replaced rather than scored:

* `determinism-stress/order-violation` — inverting `if (!observed)` gave gcc a provable null
  argument (`-Werror=nonnull`); replaced by `m1`, which refuses at 23.
* `c-programs/dbt-wait-lifecycle` — the mutation moved **stdout** (`sigchld=observed`
  vanished) while the exit status stayed 0. This is exactly the stdout-versus-exit
  distinction the queue header warns about. The next guard refuses at 64.
* `c-programs/thread-sync-determinism` — first two guards inert; the third refuses at 1.

## The one cell no fixture work can reach

`backend-parity-c/set-tid-address` is the only one of the 47 that opts into
`assert.bitwise_parity`, so `ci/test_harness.sh:3910-3921` makes it additionally satisfy
`assert_bitwise_parity_verdict` (`:3309-3330`), which requires `verified == true` **and**
`bitwise_parity == true`. Bracketed on the exact gate, both directions, with `--verify-strict`
as the harness adds it for that cell:

| backend | rc | verdict JSON | gate |
| --- | --- | --- | --- |
| ptrace | 0 | `verified=true, bitwise_parity=true, verdict=matched, strictness=canonical` | **true** |
| dbt | 0 | `verified=false, bitwise_parity=false, verdict=no_result, strictness=null` | **false** |

Its exit arms both hold (0 accept, 1 refuse). It still cannot pass, because `run_dbt`
(`hermit-cli/src/bin/hermit/backends.rs:450-579`) implements its own two-run verify and never
reaches the shared verification machinery, so it **never writes `--verify-json`** — it accepts
`--verify-strict` silently and ignores it. This is present-tense and re-measurable: it falls
the moment the DBT path emits a verdict. It is not a fixture defect, and the remedy is not to
remove the assertion.

A first pass that omitted `--verify-strict` made the gate read `false` for **ptrace** as well.
A one-sided read would have called ptrace blocked too.

## DBT capability gaps found by the accept arm

Seven of the eight `ACCEPTARM-RED` cells fail under dbt while ptrace passes the identical
binary, arguments and environment, so they are backend capability gaps rather than fixture
defects. Five distinct signatures, quoted:

| cell | dbt rc | observed |
| --- | --- | --- |
| `backend-parity-c/signal-delivery-sequence` | 1 | `invariant pending: observed 0, wanted 1` (ptrace and native report `pending=1`) |
| `c-programs/resource-determinism` | 1 | `prlimit64 child inheritance mismatch: got 1048576:1048576, expected 1048572:1048576` |
| `c-programs/syscall-file-io` | 1 | `symlinkat: File exists` — see the `/tmp` finding below |
| `determinism-stress-c/pipe-chain` | 1 | no stdout at all; ptrace prints all five stages |
| `system-utils/clock-exec-continuity` | 1 | `the clock was reset or rebased across exec (PR #1095 signature)`; dbt emits `gen=0` only, ptrace emits `gen=0,1,2` |
| `c-programs/signal-determinism` | 124 at the declared 90 s | **not a hang**: at a 600 s deadline it fails at 106 s with `SIGALRM was not pending at the delivery point` |
| `determinism-stress-c/mmap-fork-shared` | 124 at the declared 90 s | a 900 s re-probe was still running when this report was written; not characterised |

`bin-c/robust-futex-test` is the eighth and is **not** DBT-specific: it fails under both
(dbt 124, ptrace 1) on this host. Removed from the DBT ledger.

## The DBT backend gives the guest the host `/tmp`

Bracketed in both directions rather than inferred from one symptom.

* **Read.** A host-planted `/tmp/hermit-e2e/probe.txt` is read by a dbt guest (rc 0, contents
  match). The same read under ptrace gives rc 1, `No such file or directory`.
* **Write.** A dbt guest running `sh -c 'echo … > /tmp/hermit-e2e/escaped.txt'` leaves the
  file **on the host**.

(Both probe files were removed.) This is not one fixture's problem. `--verify` runs the guest
twice, and under dbt both runs share one host `/tmp`, so any fixture that creates state there
is not idempotent across the pair. The harness comment at `:3384-3388` — "Hermit's isolated
`/tmp` is fresh for each repeat" — is true for ptrace and false for dbt. Present-tense and
re-measurable.

## What enabling a qualified cell would actually require

Two gates sit beyond fixture qualification, and neither is fixture work.

**1. The `verify` mode itself is off for half the qualified set.** A cell only runs when
`modes.<mode>.ci == true` (`ci/test_harness.sh:2300-2301`). Of the 38 `REFUSABLE` cells,
19 have `verify.ci = true` and 19 have `verify.ci = false`. The evidence that this gate is
decisive comes from the cells' own ptrace column in `ci/compat-envelope/cells.json`:

| subset | ptrace `(enabled, status)` |
| --- | --- |
| the 19 `REFUSABLE` cells with `verify.ci = true` | `(true, green)` × 19 |
| the 19 `REFUSABLE` cells with `verify.ci = false` | `(true, red)` × 19 |

A clean 19/19 split. Adding `dbt` to `backends_enabled` on a `ci = false` cell produces a cell
that never runs, exactly as `ptrace` already is enabled and red there. Flipping `ci` is a
separate decision that affects every backend in that mode, not a DBT decision.

**2. Six qualified cells need a `dbt` entry in `guest_args`.** Mechanical, but omitting it
turns the cell into the usage-line-exit-2 failure described above:
`c-programs/epoll-determinism` (`multi`), `c-programs/ipc-determinism` (`pipe-order`),
`c-programs/liteinst-advanced` (`threads`), `c-programs/mmap-determinism` (`multiple`),
`c-programs/thread-sync-determinism` (`cancellation`),
`determinism-stress-c/thread-contention` (`contention`).

**So the number of cells that qualification alone could turn green is 19, not 38** — the
`REFUSABLE` cells whose mode is already on. The other 19 are qualified and gated elsewhere.

## Weaker evidence, flagged rather than averaged in

Two `REFUSABLE` cells refuse by timeout rather than by a guard, which is still the exit-status
channel but is weaker evidence:

* `c-programs/ipc-determinism` — mutant times out under **both** dbt and ptrace (124/124).
* `c-programs/liteinst-advanced` — mutant times out under dbt (124) where ptrace refuses
  cleanly (1).

## Per-cell results

| verdict | refuse arm exercised | cell | verify.ci | dbt accept | dbt refuse | ptrace accept | ptrace refuse | needs dbt guest_args |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| REFUSABLE | semantic | `backend-parity-c/aio-refusal` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | mechanical | `backend-parity-c/child-subreaper-refusal` | true | 0 | 23 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/cwd-roundtrip` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/fcntl-owner` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `backend-parity-c/file-io-roundtrip` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `backend-parity-c/fork-exec-pipeline` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `backend-parity-c/getrusage-self-accounting` | true | 0 | 3 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/mce-kill-refusal` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/membarrier-query` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/name-to-handle-refusal` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/o-tmpfile-anon` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/pipe-capacity` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/record-lock` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/seccomp-refusal` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing (unstable accept arm) | `backend-parity-c/sendfile-copy` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `backend-parity-c/sysv-ipc-refusal` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/clone` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/dbt-mmap-exec` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/epoll-determinism` | false | 0 | 1 | 0 | 1 | yes: multi |
| REFUSABLE | plumbing | `c-programs/io-uring-fallback` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/ipc-determinism` | false | 0 | 124 | 0 | 124 | yes: pipe-order |
| REFUSABLE | plumbing | `c-programs/liteinst-advanced` | false | 0 | 124 | 0 | 1 | yes: threads |
| REFUSABLE | semantic | `c-programs/madvise-determinism` | false | 0 | 13 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/mmap-determinism` | false | 0 | 1 | 0 | 1 | yes: multiple |
| REFUSABLE | plumbing | `c-programs/ppoll-simulation` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/proc-fd-link-aliases` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/procfs-positioned-probe` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/recvmsg-scm-rights-mmap` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `c-programs/syscall-quick-wins` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | semantic | `c-programs/thread-sync-determinism` | false | 0 | 1 | 0 | 1 | yes: cancellation |
| REFUSABLE | plumbing | `c-programs/wait-on-child` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `determinism-stress-c/fork-tree` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `determinism-stress-c/pipe-prefill` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `determinism-stress-c/producer-consumer` | true | 0 | 1 | 0 | 1 | no |
| REFUSABLE | plumbing | `determinism-stress-c/thread-contention` | false | 0 | 1 | 0 | 1 | yes: contention |
| REFUSABLE | plumbing | `determinism-stress-c/thread-stress` | false | 0 | 1 | 0 | 1 | no |
| REFUSABLE | mechanical | `determinism-stress/order-violation` | true | 0 | 23 | 0 | 1 | no |
| REFUSEARM-UNPROVEN | usage line on stderr, exit 64 -- argument parsing, not a refusal | `c-programs/dbt-wait-lifecycle` | false | 0 | 64 | 0 | 1 | no |
| PARITYASSERT-BLOCKED | - | `backend-parity-c/set-tid-address` | true | 0 | 1 | 0 | 1 | no |
| ACCEPTARM-RED | - | `backend-parity-c/signal-delivery-sequence` | true | 1 | 1 | 0 | 1 | no |
| ACCEPTARM-RED | - | `bin-c/robust-futex-test` | false | 124 | 124 | 1 | 124 | no |
| ACCEPTARM-RED | - | `c-programs/resource-determinism` | false | 1 | 1 | 0 | 0 | no |
| ACCEPTARM-RED | - | `c-programs/signal-determinism` | false | 124 | 1 | 0 | 1 | yes: itimer-delivery |
| ACCEPTARM-RED | - | `c-programs/syscall-file-io` | false | 1 | 1 | 0 | 1 | no |
| ACCEPTARM-RED | - | `determinism-stress-c/mmap-fork-shared` | false | 124 | 1 | 0 | 1 | no |
| ACCEPTARM-RED | - | `determinism-stress-c/pipe-chain` | false | 1 | 1 | 0 | 1 | no |
| ACCEPTARM-RED | - | `system-utils/clock-exec-continuity` | true | 1 | 1 | 0 | 1 | no |

## Reproduction

Artifacts, drivers and raw captures: `ignored/qualify-dbt-verify-47-20260815/` in the
`dev-hermit` parent. That path is **gitignored** — it survives context loss and not host loss,
and devbig014 cannot read it. This report is the durable half.

* `meta/cells47.json` — per-cell manifest projection (program, cflags, guest_args, ci, assert)
* `meta/FINAL.json` — the verdict table above, machine-readable
* `mutate.py`, `arm.sh`, `retry.sh` — the mutators and arm drivers
* `runs/<cell>/` — per-cell captures for both arms and both backends
