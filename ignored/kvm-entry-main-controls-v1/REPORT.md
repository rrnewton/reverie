# Closed main-entry controls: author release

Source-only implementation in the authorized Reverie slot, based on the two retained files in `baseline/` and BASELINE.json. Writes are confined to `reverie-kvm/src/clock.rs`, `reverie-kvm/src/entry.rs`, and new `reverie-kvm/src/vm/entry_main_tests.rs`. Root owns the include in `vm::tests`. SOURCE.json binds the released three-file snapshot; AUTHOR.patch is the isolated author delta. No compiler, formatter, test, guest, network, model or SCM command was run. This report is implementation evidence, not independent approval or a test pass.

The controls implement the first main-entry slice requested by `S/ignored/kvm-entry-gate-design-successor-v1/CONTROLS.md` section A and `S/ignored/kvm-entry-remaining-integration-v1/REPORT.md` item 3, where S is the `kvm-parent-reader-support-20260916` slot. They retain the unchanged-Mapping scope. No production mapping close caller or mapping mutation is added.

## Observations and exact boundary

All added seams in clock.rs and entry.rs are cfg(test). `CountedVcpu::set_run_probe(Arc<RunProbe>)` binds the probe to that vCPU's Participant and GuestClock. `RunProbe::before_run(&self, hook: impl FnOnce(&RunProbe) + Send + 'static)` installs one hook; `run_hook` removes it under the mutex and invokes it outside the lock. A retained Arc can rearm the hook for a later entry. The closure receives a borrowed probe, so this fixture does not store a self-Arc cycle. `new_guest` retains the observer on its fresh test clock.

The hook runs at actual CountedVcpu::run entry, after caller setup and any ordinary stop check but before Participant::prepare. It arms counters, completes a real unchanged-Mapping close, and retains the Closed token. The actual prepare sees closed admission and returns None. The test observes this branch, not an invented guest result.

Closing readies the subscription taken before run, so first None/first Ready is insufficient evidence of waiting. cfg(test) ObservedChange delegates Clone/Future/FusedFuture to the real Shared oneshot receiver. Only an actual delegated Pending after a closed admission increments closed_waits and sends the notice. Probe notification occurs outside the gate state mutex. Host controllers wait on that bounded channel; Tool controllers require both the public future's Pending and the actual receiver notice. The Closed token is still retained at the observation. The mask observer performs only an atomic increment inside prepare's existing serialized activation section.

Counters observe each shared CountedVcpu Deref, both private fd.run sites separately, the attempt immediately before Mask::install(fd), actual GuestClock::begin, actual CountInterval construction, and the existing exit collector. Counters are armed after setup/initial perf validation. All stop cases require every counter and exit total to remain exactly zero before terminal choice, through consuming cleanup and after backend destruction.

`shared_fd_accesses` is a descriptor-access witness, not a numeric ioctl-count bound: a bootstrap helper can borrow once and issue multiple get/set ioctls. Zero plus the scoped source audit establishes no shared-descriptor ioctl in this phase. The actual VcpuFd is private to CountedVcpu; its two run sites and prepare are separately covered. Audited bootstrap and cpuid_instruction helpers take temporary borrowed VcpuFd references and retain data, not a descriptor/reference. Entry signal's raw fd use is the observed mask install. InitializedKvmResources owns a raw VcpuFd only before wrapping. No saved vCPU raw fd/reference escape was found in these paths or the vm/runtime callers. Perf-event raw fds in clock.rs are distinct. Positive numeric assertions apply to the explicit RUN/MASK/begin/interval sites, not Deref totals.

## New declarations and required outcomes

All selectors start `vm::tests::entry_main_tests::`:

1. `host_raw_main_closed_wait_poison_stops_without_ioctl`: actual public raw run; poison the closed gate with a uniquely shared typed cause; require that exact Arc in the returned error and zero observed dispatch.
2. `host_elf_main_closed_wait_group_exit_stops_without_ioctl`: actual public ELF run and real group request_exit_group(23); require status 23 and zero dispatch.
3. `host_main_fresh_reopen_executes_finite_raw_and_elf_once`: two fresh healthy lifetimes, raw HLT and ELF exit_group(37).
4. `tool_direct_main_closed_wait_global_failure_stops_without_ioctl`: actual direct Tool run and that concrete GlobalTool's failure future; require RunAborted, terminal status 255, one direct failure report and exact lifecycle consumption.
5. `tool_elf_main_closed_wait_group_exit_stops_without_ioctl`: actual ELF Tool run, real group exit 23, zero dispatch and exact lifecycle consumption.
6. `tool_elf_main_closed_wait_global_failure_stops_without_ioctl`: actual ELF Tool run and scoped GlobalTool failure, retaining RunAborted/status 255 and exact lifecycle consumption.
7. `tool_main_fresh_reopen_executes_finite_direct_and_elf_once`: fresh healthy direct HLT and ELF exit_group(37).
8. `closed_tool_main_wait_uses_its_independent_global_scope`: two actual independent ELF Tool invocations. Failure of the first reaches its consuming hook while the second remains parked without terminal consumption; the second then reopens and executes its real exit_group(37).

These are eight declarations and eleven backend lifetimes: six stopped and five healthy. All require actual KVM setup without a silent skip. The five healthy lifetimes are intended to execute actual KVM_RUN, each exactly once: two raw HLT and three ELF Hypercall exits. Each requires one mask-install attempt; Host cases require untracked RUN=1 and tracked RUN/begin/interval=0; Tool cases require tracked RUN/begin/interval=1 and untracked RUN=0. ELF status 37, one Hypercall and the finite exit_group-only guest establish the expected terminating effect. The stopped lifetimes require zero actual RUN. These are assertions awaiting execution, not measured counts.

Host stop results must reach the controller before reopening. Tool stop cases must enter the real Pending on_exit_thread RPC with the selected terminal status while still closed. Only then does the controller reopen and release the consuming hook, allowing dependent cleanup. Exact start/thread-exit/process-exit event lists, thread-state identity, one GlobalState destruction and no owned worker handles are required. The independent scope fixture also verifies both worker registries empty. Host rescue retains its physical thread, reopens and requests stop on failure. Tool registration Drop releases its pending consumer; Closed-control Drop reopens. Controller channel/finish bounds are five seconds; sleeps are not ordering evidence.

The five `tool_case` invocations also send a real unchanged-state notification while closed. Each must perform exactly one further closed admission and one real Pending observation, with every dispatch counter still zero. The initial close-after-subscription path consumes its ready notification and rechecks before the parked observation.

## Preservation and limits

PRESERVATION.json verifies byte-identical existing test suffixes: nine clock declarations and twelve entry declarations. Existing assertions, tolerances, comparators, skips, labels and failure classifications were not edited. No existing check was deleted or failure relabelled as a pass. With cfg(test) removed, the production expressions/behavior are unchanged; prepare's existing mask-install expression is merely wrapped around the test-only atomic observation. The full host signal/mask contract and entry/signal.rs are unchanged.

This slice does not establish all stop-before-registration, final-predicate races or second-close/reopen interleavings. It does not cover the four child/action parking sites, outstanding-hypercall response ownership, foreign EINTR, actual failed spawn, real owner-to-peer publication, sender/final-drain races, or the complete perf/error/permission matrix. Those remain separate controls and existing obligations, not exemptions. In particular, the rearmable hook is available for root's later private-response control but no such case is claimed here. No broad scheduler, determinism, Linux/POSIX, parity or concurrent mapping-publication conclusion follows.
