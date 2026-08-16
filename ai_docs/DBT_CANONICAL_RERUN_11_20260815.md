# Canonical rerun of 11 DBT cells, at current head — for devbig014

Task: `canonical-rerun-of-11-dbt-cells-for-devbig014`. Date: 2026-08-15. Host: devbig030.

> **Provenance.** Measured on devbig030 under the portable profile only — no `validate.sh`, no full and no privileged profile. **Nothing here is a receipt**, and nothing in it should be read as one now that a qualifying receipt on a red base binds its exact commit as hard green. devbig030 cannot mint full or privileged receipts: `arch_prctl(ARCH_SET_CPUID=0x1012, 0)` returns ENODEV here, re-measured 2026-08-15.


## Answer

**`COULD-NOT-RUN`, 11 of 11. The canonical comparator is not available under `--backend dbt`
at current head.** No Stripped result was substituted.

| outcome | n |
| --- | --- |
| `PASS` | 0 |
| `FAIL` | 0 |
| `HANG` | 0 |
| `COULD-NOT-RUN` | 11 |
| **total** | **11** |

`0 + 0 + 0 + 11 = 11`. No remainder.

## Head, and the second half of "at head" that a rebuild does not cover

Head: **`302a1a9c0fde564db0292d1ea5cabc91343e79bc`**, recorded in every per-cell `meta` file.
Binary rebuilt at that SHA for this task —
`hermit 0.2.0 (2026-08-16, g302a1a9c0fde)`,
sha256 `1ec7968570ff9fc033c23b3ee61b01ff8097d8a795e59e10d1ae68d88436bec8`.
The pre-existing binary was built at `e2f9a36b1ad8`, an ancestor 10 commits behind head, two of
which touch `detcore`; nothing from it is reported here.

**Rebuilding the binary is not sufficient, and this is worth passing on.** The DBT runtime is a
separate cdylib, and `detcore_dbt::runtime_library_path()`
(`detcore-dbt/src/lib.rs:653-676`) prefers the *packaged* copy at
`install_pkg/rsrcs/libdetcore_dbt.so` over the freshly built one. In the primary checkout that
packaged path is a symlink to `target/release/libdetcore_dbt.so`, which `cargo build` did **not**
refresh — it still dated Aug 14 and hashed `21004437…`, while the head build wrote `8a7901af…` to
`target/release/deps/`. A rebuilt binary would silently have loaded a 10-commit-old runtime. A
private `HERMIT_INSTALL_DIR` holding the head-built runtime was staged instead, and
`dbt_runtime_sha256` is recorded per cell. The shared checkout's `install_pkg` was not modified.

## Cell identification

Four of the eleven shorthand names match more than one manifest id (`prctl` matches four;
`rlimit`, `pidfd-open` and `getcpu` match two each). Resolved by the single filter that leaves
exactly one candidate per name — `dbt`/`verify` `enabled=true` **and** `status=red` in
`ci/compat-envelope/cells.json` — which yields 11 for 11, all under `backend-parity-c`, with no
ties and no leftovers. Rejected near-misses, so the choice is auditable:
`backend-parity-c/prctl-pdeathsig`, `c-programs/arch-prctl-determinism`,
`c-programs/prctl-dumpable`, `c-programs/dbt-prlimit-self`, `c-programs/pidfd-open-self`,
`c-programs/getcpu` — each has `dbt`/`verify` `enabled=false`.

## Measurement

Three arms per cell, on the identical head-built binary, head-built runtime, fixture, arguments
and environment. Harness fidelity follows `ci/test_harness.sh` (compile `:3254`, guest env
`:3390-3400`, `E2E_TMPDIR` `:3384-3388`, portable profile `:3431-3434`, capture `:2387-2402`).

| arm | backend | flags | result across all 11 |
| --- | --- | --- | --- |
| canonical | dbt | `--strict --verify --verify-strict` | rc 0, `verdict="no_result"`, `bitwise_parity=false`, `strictness=null` |
| canonical | ptrace | `--strict --verify --verify-strict` | rc 0, `verdict="matched"`, `bitwise_parity=true`, `strictness="canonical"` |
| stripped | dbt | `--strict --verify` | rc 0 — recorded, **not counted** |

Canonical gate (`verified == true` **and** `bitwise_parity == true`), applied exactly as
`ci/test_harness.sh:3309-3330` applies it: **dbt 0/11 true, ptrace 11/11 true**.

The ptrace control is the point. It is 11/11 canonical on the same command shape, so
`COULD-NOT-RUN` is not a wrong invocation.

## Why — from source at head, and the codebase already says it

`hermit-cli/src/bin/hermit/run.rs:1951-1962` returns into `backends::run_dbt` **before** the shared
run/verify machinery, so the DBT arm never enters `verify()`, which is where the verdict is written
(`run.rs:2906`). The `no_result` observed is the pending stamp written at `main.rs:321` before
dispatch and never overwritten. The comment at `main.rs:309-310` states it outright:

> `* the DBT arm, which returns run_dbt(..) and never reaches verify(), so it produces no verdict at all;`

`run_dbt` (`hermit-cli/src/bin/hermit/backends.rs:450-579`) accepts `--verify-strict` and silently
ignores it.

## Two conflations refused, in both directions

* **`no_result` is not a failure.** Reporting these as `FAIL` would hand over eleven product defects
  that were not observed. An absent measurement is not a negative measurement.
* **DBT did do something, and it is not canonical, and it is not counted.** `run_dbt` ran its own
  two-run comparison on all 11: two runs each, matching memory hashes, `:: Success: deterministic`.
  That comparison covers exit status, stdout, and a Detcore summary / memory hash. It does **not**
  compare INFO logs and it is not the canonical comparator. It contributes zero to the accounting
  above and is recorded below only so nobody rediscovers it and reads it as a pass.

No cell hung; all 33 runs completed in roughly 1.3 s wall in total. The nonblocking-pipe
`EAGAIN` → `poll(…, -1)` diagnosis therefore did not arise here — untested by this task, not refuted.

## Two further gates, before anyone plans a requalification

1. **`verify.ci = false` on all 11.** The mode is off, so the cells never run — which is why `ptrace`
   is also `enabled` and also `red` on every one of them. Their red is "never measured", not
   "measured and failed".
2. **None of the 11 sets `assert.bitwise_parity`.** Even with the mode on, the harness would run them
   under the Stripped comparator; it adds `--verify-strict` only for a cell that opts in
   (`ci/test_harness.sh:3462-3465`). Canonical here was an upgrade applied on request, not the cells'
   configured bar.

## What would make these 11 answerable

A change that lets the DBT arm publish a verification verdict — either routing it through the shared
`verify()` path, or having `run_dbt` write one. Until then, "do these 11 pass at canonical under DBT"
has no measurable answer on any host. That is a property of the product, not of devbig014's board and
not of this box.

## Per-cell results

| cell | verdict | canonical under dbt | canonical under ptrace (control) |
| --- | --- | --- | --- |
| `backend-parity-c/numa-node-identity` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/getpriority-identity` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/prctl-identity` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/rlimit-identity` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/pidfd-open-self` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/cpu-virtualization` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/getcpu-identity` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/sched-getaffinity-identity` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/fd-duplication` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/dup-shared-offset` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |
| `backend-parity-c/lseek-positioning` | COULD-NOT-RUN | rc 0, `no_result`, parity `False`, strictness `None` | rc 0, `matched`, parity `True`, strictness `canonical` |

Supplementary and explicitly **not** part of the accounting — the DBT-native two-run comparison that
`run_dbt` performed instead of the canonical one. All 11: 2 runs, identical guest-memory hashes,
`:: Success: deterministic`.

## Reproduction

`ignored/dbt-canonical-11-20260815/` in the `dev-hermit` parent (gitignored: survives context loss,
not host loss). `cells11.json` (manifest projection), `RESULTS.json` (per-cell verdict JSON),
`run11.sh` (the three-arm driver), `install_pkg/` (the private head-built runtime staging),
`runs/<cell>/` (captures and `meta`).
