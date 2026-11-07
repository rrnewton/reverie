# Backend: in-guest-trap

The LiteInst runtime with site patching off: **every** syscall
traps. Cell **V4-preload-trap** in
`reverie::selection::implemented_cells` — same activation,
placement, and admitted envelope as the
[patching backend](backends-liteinst.md), with the seccomp trap
as the mechanism instead of in-place patching. Its job in the
family is to be the patching backends' *semantic reference*: a
patcher may change how a syscall reaches the dispatcher, never
what the dispatcher does with it. When the patching backend and
this one disagree, the patcher is wrong by definition.

## Mechanism

* **Hooking:** none. No syscall site is rewritten.
* **Trapping:** the shared `reverie-inguest` seccomp filter turns
  every syscall into `SIGSYS` and the common runtime dispatches
  it — there is no missed-site case for a backstop to cover.
* **Placement:** identical to the patching backend: a tool DSO in
  the guest, global state in the launcher's `RpcServer`.

The switch is one environment variable on the guest command.
`reverie_liteinst::SITE_PATCHING_ENV` is
`REVERIE_LITEINST_SITE_PATCHING`; `0` selects this backend, and
`reverie_liteinst::set_guest_site_patching(&mut command, false)`
spells the same thing for a `std::process::Command`. The
examples' launcher sets exactly this when you pass `--backend
in-guest-trap`.

## Lifecycle, refusals, statistics

Everything is inherited from the in-guest family and nothing is
widened by turning patching off:

* The V4 envelope matches V5 line for line: dynamic,
  non-`AT_SECURE`, single-thread guests; thread creation and
  exec successors refuse by name; fork followed, vfork and
  signals declared-weaker; the declared pre-init window is part
  of the cell (`R-WINDOW` for zero-window tools). See
  [LiteInst in-guest](backends-liteinst.md#lifecycle-coverage).
* There is no `R-CODE` patching question to answer — coverage is
  by trap, for any code shape the preload envelope admits — and
  correspondingly no patched fast path. CPUID/RDTSC still
  require P5/P6; RDRAND/RDSEED and XGETBV remain absent.
* Statistics are the same `LiteinstBackendStatsSnapshot` (same
  collector, same accessors): read `dispatch_paths()` to confirm
  a run really stayed on the trap path instead of assuming the
  switch took effect.

## Run it

```console
$ reverie-examples/e2e/run.sh --backends in-guest-trap
```

On the [scorecard](testing.md) it matches native on **14/23**
programs — the same cells as the patching backend, which is the
point: the two in-guest columns are expected to differ only in
cost, and any cell where they disagree is a patcher bug with a
ready-made reproducer.
