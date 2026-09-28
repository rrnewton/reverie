[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Plain Language Summary and Project Impact

Expose the generation-bound KVM child-exit publication primitive needed by Hermit's deterministic scheduler. Exact parent and child lifetimes, terminal status, waitability, and fields for virtual UID and CPU ticks now flow through an at-most-once receipt instead of being reconstructed from reusable numeric PIDs.

The backend also tracks process-family terminal state and retires the exact family child actually consumed by `wait4` or `waitid`. Missing generations, missing family edges, ancestry corruption, duplicate publication, and managed backend/ledger drift fail closed with typed errors.

This continues https://github.com/rrnewton/reverie/pull/599.

## Determinism

KVM arms a per-child publication fence before polling the child-wait callback's synchronous decision prefix. An ordinary still-running child preserves immediate `WNOHANG` behavior; once logical publication is announced, a concurrent parent wait crosses only the bounded Ready-or-Failed bookkeeping transition. The slot mutex is not held while Tool code runs, and success or failure wakes waiters without allowing duplicate publication or resurrection.

Process-family classification uses the exact direct parent's logical terminal state. A terminal transitive root cannot steal a grandchild from its still-live direct parent. Child and direct-parent exit transitions serialize through the direct parent's signal transaction, so their host retirement order cannot change the result. Exact-generation identities and retained receipts prevent PID reuse from selecting a different child event. `WNOWAIT` and error paths consume no family state; consuming waits retire the PID actually removed by the backend.

This is a Reverie prerequisite only. The Hermit consumer must still implement its two-stage Exit/callback fence, publish while holding the scheduler reservation, inject the backend wait before retiring scheduler shadow state, and update its Reverie pin. Exact-main CLI evidence through `bin/safehermit`, record/replay qualification, and full KVM parity remain open.

## Linux Semantics

The bounded terminal-exit path preserves full exit/signal/core status, ordinary waitability, explicit `SIG_IGN` suppression with auto-reap, `SA_NOCLDWAIT` notification with auto-reap, standard-SIGCHLD coalescing with first-siginfo retention, independent signalfd carriers, and `wait4`/`waitid` selection including `P_ALL`, `P_PGID`, `P_PID`, and `WNOWAIT`.

This does not claim a complete Linux process or signal model. Live-descendant reparenting remains fail-closed; stopped/continued child events, later `SIG_IGN` zombie flushing, broader default-SIGCHLD fidelity, and nonzero production UID/CPU accounting remain outside this increment. For a root-child-grandchild family, the middle process exiting while the root is live and the grandchild remains live is refused because reparenting is not implemented. If only the transitive root is terminal while the middle parent remains live, the grandchild remains that middle parent's child and preserves its frozen waitability. Only an exact direct parent that is already logically terminal makes the later child a consuming teardown transition. That guest-causal direct-parent state can change compatibility, although host retirement order cannot; Linux PID-namespace task killing is not implemented here.

Dropping the final provisional task does not manufacture a successful family transition. If the explicit lifecycle-to-family transition is missing, the backend fails closed with the exact production invariant error.

## Relationship to gVisor

The design follows gVisor's high-level platform/policy separation: KVM transports execution events, while the Reverie Tool and `ElfExecutor` own lifecycle and signal policy. No gVisor code is copied. Reverie does not provide gVisor's Sentry, Linux ABI breadth, sandbox boundary, or scheduling semantics, and no gVisor parity claim is made.

## Validation

Exact target: base `f7bd85e11dd258112148ed2cba6531501a1a00d9`, head `6152ee999a41324be36456bb0a9b94a703711068`, tree `1aa237f33bbf42528ae3082e09acc7cfe1d18b75`. The 257,157-byte base-to-head patch has SHA-256 `8eed79a86b435be20433ecf98e2ca4d855cdb2d07b81c98a55dc0e7620b94a57`.

- Focused publication/family tests: 38 passed.
- Forced publication-fence and failed-publication wake tests: 1 passed each. The former drives both `wait4(WNOHANG)` and `waitid(WNOHANG)` through an armed fence; the latter proves failure wakes without resurrection.
- Terminal-fork tests: 23/23 passed with the default parallel harness in 1.03 seconds and serialized in 6.09 seconds. The exact deliberate callback-panic case passed alone in 0.23 seconds.
- The earlier a2e414dc parallel failure was a real publication race, not a load-only timeout: the callback-prefix event was visible before waitability, the parent observed no child, status became 255, and the controller timeout was downstream. Isolated and serialized passes at that head selected a winning interleaving and did not qualify it. The new fence and forced regression address that defect.
- Real-KVM child-wait and grandchild-family tests: 1 passed each, with 335 filtered each. The child test ran with `--nocapture` and no known `/dev/kvm` skip line; the grandchild run emitted post-probe diagnostics. Grandchild modes 6-8 cover waitable, `SIG_IGN`, and `SA_NOCLDWAIT` with a terminal transitive root and live direct parent at virtual root PIDs 1 and 3.
- One first fresh full-library parallel run exposed a pre-existing fixture race after 773 passes: `descriptor_retirement_accept_cleanup_releases_both_guards`, unchanged from the base, unwrapped `EAGAIN` from an immediate nonblocking peer read. The raw failure is retained and is not counted as a pass. Its exact retry passed, the complete default-parallel retry passed 774/774 in 12.52 seconds, and the complete serialized run passed 774/774 in 38.41 seconds. The separate fixture repair is outside this pull request.
- The optimized publication-fence regression and injected backend-failure routing test passed. The latter reached `HandlerOutcome::RuntimeError`, not a guest errno.
- `reverie-core`: 22 passed; one pre-existing doctest remained ignored.
- Workspace all-target check, strict Clippy, formatting, and `git diff --check`: passed.
- The final recorded status check found 448,325,578,752 bytes available, above the 429,496,729,600-byte (400 GiB) floor.

These are Reverie component checks. No Hermit exact-main `safehermit` run, record/replay result, or full backend-parity result is claimed.

## Human Review Required

Trigger 2 applies because this changes core `ProcessSignalControl`, Tool callback, and Guest lifecycle abstractions. Route it for post-facto human review; no pre-land owner hold is inferred. Normal exact-head canonical code, determinism, and evidence reviews remain required before landing. Earlier review labels or comments bound to `0598b5ffbeb737866372f89224d915efaeb29943` do not attest this head.
