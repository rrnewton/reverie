# Runtime driver ownership activation

Author implementation packet; not an independent approval. No formatter, compiler, test, guest, model, network or SCM operation was run for this increment. Root owns composed qualification and publication.

## Exact source

Repository slot: `/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918`, branch `codex/kvm-parity-land-20260918`, underlying base `91110d249ffd8957267d71fab8c83d9636105efe`. The baseline is the already prepared private v3 source, not that commit alone. `BEFORE.json` and `before/` retain the baseline read context. `SOURCE.json` binds the two authored source snapshots under `after/`; `AUTHOR.patch` is their exact isolated delta. Root may format or compose successors after release; these copies retain this packet's bytes.

- `runtime.rs`: SHA256 `43009b86a390638cffd936c171935b8c1adcb9c9b90c7d75964004fbdd99d100`, 267,562 bytes.
- `runtime/entry_owner_tests.rs`: SHA256 `626581e8457dcd81af93738af1272c56bf7c1ff313aacd5e830016e80739ed55`, 8,535 bytes.

Only those two product files were written. Entry owner/watch/routing, memory origin mechanics and VM/worker ownership are concurrent root or other helper work, not authored or approved here.

## Implementation

Frozen `runtime.rs:1815` introduces the production factory driver with a cloned private entry watch. Compatibility signatures remain available for existing controls. A failed pre-construction check skips the Tool factory and separately catches destruction of the uncalled factory and failure future. The pending typed cause is held outside those destruction catches. The existing constructor-panic path retains the signal outcome, and an additional post-destruction check retains a newly captured cause if no outcome was selected.

At `runtime.rs:1916`, the actual owned callback keeps the earlier explicit polling/destruction catches. The private subscription is polled before the state check; checks occur before and after callback polling, before starting queued children and after actual future destruction. A Ready typed callback value is retained for outer error mapping instead of being discarded on a simultaneous entry failure. No callback is repolled after a terminal choice or panic.

The eight Guest borrow scopes at lines 2149, 2310, 3514, 3621, 4081, 4271, 4564 and 4936 cover nine production callback calls (timestamp and CPUID share one scope). Each scope binds a separately cloned callback memory view. Static executors also rebind their retained address space. Actual callback destruction and the entire Guest/executor borrow block end before the unique callback scope is dropped. The backend and static executor then restore their outside-callback view before scratch cleanup. Retained callback memory clones keep the old immutable origin.

The async finalizer at `runtime.rs:3140` maps typed errors before combining a fresh entry check. Ordinary success becomes a typed fatal result; dropping its value has its own panic catch. Existing nonlocal outcomes retain their settlement path. A routed foreign notification is awaited without publishing another callback's cause; retained entry causes remain sticky for the next ordinary operation and outer completion. The root's route helper, not this finalizer, defines foreign-versus-own identity and origin receipt handling.

Direct and ELF root scopes are created at lines 3499 and 3835. Their final routes and registration closure precede global/context release and intentional panic propagation. ELF signal-installation failure also closes the scope. No separate scope is created inside the shared process loop: VM workers own their complete outer lifetimes. Early direct configuration/global initialization still precedes creation of its driver scope; normal returns from the scoped region use explicit retirement, while unexpected external unwind/cancellation is not recast as successful cleanup.

Outer process routing now precedes synchronous failure publication, cancellation and joins, with additional checks after retained-child cleanup and late clear-TID/files retirement. The free final process helper gains a private `Option<&KvmBackend>` argument so a failure captured by consuming exit hooks can be routed before physical independent-child joins (`runtime.rs:2754`). The existing native compatibility wrapper passes None; the production caller passes Some(self). Runtime restores the outside origin after an unexpected unwind from the caught execution body as well.

## Three new controls (not executed)

All are in `runtime::entry_owner_tests` and construct a KVM backend for setup only; their source contains no KVM_RUN or guest execution.

1. `private_entry_failure_wakes_pending_callback_before_scope_acknowledgement`: manually polls an actual Pending callback, captures an issued gate cause, observes a private wake, forbids repoll, observes actual callback destruction before scope acknowledgement, then uses the production finalizer and retirement path while retaining the typed cause.
2. `existing_entry_failure_skips_factory_and_retains_both_destructor_payloads`: preexisting entry failure prevents construction; both uncalled factory and failure-future destructors panic independently. Exact original payload addresses/order and typed gate cause are asserted.
3. `ready_typed_error_survives_entry_failure_and_outer_mapping`: a callback captures gate failure and returns a uniquely owned HostIo/ENOSPC error in the same poll. The production driver must preserve Returned(Err), and the outer finalizer must retain both original error allocation and gate cause without repoll. A Weak witness verifies the original allocation eventually dies.

These are finite private-driver controls, not complete Tool callback integration, worker lifecycle, signal settlement or parity qualification. Root's separately authored driver/worker controls and retained existing regressions are needed for the composed change.

## Preservation and remaining scope

`PRESERVATION.json` records nine production driver calls, eight callback scopes and two explicit root/direct scopes. All 24 existing in-file test names are unchanged; the existing final test module and external test module declarations are byte-identical apart from appending the new module. Existing scratch/drain source controls remain intact. No assertion, tolerance, comparator, skip or success label was weakened, and no old test was deleted.

The new source has not been compiled or tested. No determinism, parity, full scheduler, whole integration or landing approval follows from this packet. The underlying gate remains a no-op fence preparation, with no mapping publication or whole-memory snapshot guarantee. No arbitrary synchronous host preemption, external future-cancellation recovery, double-panic abort recovery or new public API bound is claimed. Root owns the current dependency implementations, the normal pending-child release check and complete worker/inline Host lifetime composition.
