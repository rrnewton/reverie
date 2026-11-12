# liteinst2

`liteinst2` is a Rust library for **hooking machine code in a running program**.
It lets you intercept a chosen point in already-loaded x86-64 code — redirecting
execution into your own function and then back — by rewriting instructions in
place while the program keeps running, with no recompilation and no need to stop
every thread. This is a form of *dynamic binary instrumentation*: you would reach
for it to trace, profile, intercept, or modify a program's behaviour at runtime.
A common use is hooking a `syscall` instruction to observe or redirect the call.

It is a standalone library: you link it into your own program (or into an
`LD_PRELOAD` shim) and drive it yourself. It has no runtime framework and makes
no policy decisions about processes, signals, or syscalls — those belong to the
client that embeds it.

## Instruction punning

Overwriting live code is dangerous: another thread can fetch an instruction
mid-edit and execute half of the old bytes and half of the new. `liteinst2`
avoids this with *instruction punning* — a technique for x86-64 where a patch is
published as a single atomic store that reuses ("puns") existing bytes so that
every possible concurrent instruction fetch sees either the complete old
instruction or the complete new one, never a torn mix. Split sites that cannot be
covered by one atomic store use the guarded cross-modification protocol
(WordPatch++) instead.

The techniques come from two papers and from the original C++ library that
introduced them:

- Original implementation (C++): [iu-parfunc/liteinst](https://github.com/iu-parfunc/liteinst).
- *Instruction Punning: Lightweight Instrumentation for x86-64* (PLDI 2016) —
  the single-store punning technique.
- *Living on the Edge: Rapid-Toggling Probes with Cross-Modification on x86*
  (PLDI 2017,
  [doi:10.1145/3062341.3062344](https://doi.org/10.1145/3062341.3062344)) — the
  guarded split-site (WordPatch++) protocol.

`liteinst2` is a Rust reimplementation of that work.

## A first hook

`replace_first` installs a hook over a two-byte instruction, mutates the saved
register context, relocates the rest of the five-byte patch window, and toggles
the hook off again:

```console
cargo run --example replace_first
```

The `preload_consumer` cdylib example shows how a consumer can own `LD_PRELOAD`
delivery while depending on the policy-free core:

```console
cargo build --example preload_consumer
```

## How the crate is organised

Each module owns one correctness boundary:

- `cache_line`: overflow-safe patch-span classification.
- `scanner`: fail-closed x86-64 decoding and cache-line crossing discovery.
- `patcher`: multi-instruction jump planning and all-front-byte WordPatch++ publication.
- `planner`: rapid, relocated, and client-trap fallback selection.
- `rapid`: exact instruction-pun planning and atomic opcode toggling.
- `probe`: idempotent probe lifecycle state.
- `trampoline`: near dual-mapped allocation, relocation, hook dispatch, CET-safe
  return, and async-signal-safe reverse-PC lookup.

Clients that keep a ptrace slow path follow the exhaustive
[patch-site decision tree](PATCH_SITE_DECISION_TREE.md): choose quiescent or
concurrent publication, then select a direct pun, a proved relocation, a safe
straddler bailout, or an explicit ptrace fallback.

## Safety invariants

The implementation maintains six invariants:

1. A cross-cache-line patch must use guarded publication unless the caller
   proves that no concurrent instruction fetch or data access is possible.
2. Probe installation publishes state only after planning and allocation pass.
3. Trampoline sizing and emission use the same checked plan.
4. Signal handlers use only async-signal-safe, pre-published data.
5. Trampolines never use one RWX mapping; arena mode instead retains separate
   RW and RX aliases and therefore does not provide strict W^X.
6. Trampoline continuation uses a direct jump when reachable, with a CET NOTRACK
   indirect fallback, so an application continuation need not begin with ENDBR64.

## What clients must provide

These checks do not establish arbitrary-binary or probe-anywhere support.
Generic hooks can displace several complete instructions, replace a short first
instruction such as `syscall`, mutate the saved integer context, and use a
dual-mapped arena prepared before signal-driven registration. Exact one-byte
rapid puns still require the original bytes to encode a free trampoline
destination. Clients must provide a trustworthy code region, writable access to
the live patch word, mapping-lifecycle ownership, proof that no control-flow
entry can target the interior of a relocated window, and a trap fallback when the
planner returns `PunPlan::TrapRequired`.

Installed trampolines publish immutable generated-to-application PC ranges.
Signal handlers and unwind front ends can call
`trampoline::translate_program_counter` without allocation or locks before
reporting or unwinding a relocated fault. This translates the logical PC; it
does not install a signal handler or synthesize DWARF call-frame metadata.
The ordinary `LiveJumpPatch` and `InstalledHook` entrypoints use concurrent-safe
publication. WordPatch++ guards every byte before a cache-line split as
specified by PLDI 2017, but callers must still supply a
machine/topology-qualified `StalenessBudget`. A consumer without that
calibration should route split sites to its ptrace/trap fallback instead of
selecting `GuardedSplit`.

The unsafe `bind_quiescent`, `install_replacing_first_quiescent`, and matching
activation entrypoints retain the same planning and relocation but skip trap
registration and guarded split publication. They are valid only when the caller
can exclude every other thread, signal handler, instruction fetch, and code
writer for the complete publication call. They must not be used as a faster
default for threaded applications.

## Development

```console
cargo test --all-targets --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo package --locked
```

GitHub Actions runs these blocking checks on Linux x86-64:

- `cargo fmt --all -- --check` and Clippy with warnings denied check source
  formatting and every library/test target.
- Debug and release `cargo test --all-targets --all-features` cover decoding,
  cache-line planning, jump publication, trap routing, relocation, context
  preservation, and concurrent toggling using synthetic dual-mapped functions.
- `live_probe_stress_matrix` exercises 128 rapid probes, one million opcode
  stores, 16 guarded jump probes, 10,000 activation cycles, and concurrent
  signal delivery at its default scale.
- `probe_overhead_benchmark` is used only as a functional active-hook loop: CI
  requires one callback per call but does not enforce its host-dependent timing.

The live stress matrix and overhead benchmark are opt-in:

```console
cargo test --release --test stress live_probe_stress_matrix -- --ignored --exact --nocapture
cargo test --release --test stress probe_overhead_benchmark -- --ignored --exact --nocapture
```

`LITEINST_STRESS_RAPID_FUNCTIONS`, `LITEINST_STRESS_RAPID_ITERATIONS`,
`LITEINST_STRESS_GENERAL_FUNCTIONS`, `LITEINST_STRESS_GENERAL_CYCLES`, and
`LITEINST_STRESS_SIGNAL_ROUNDS` scale the matrix without changing source.
