# One-shot native owned-context positive regression

## Result and scope

At 2026-09-05T05:26:40Z, build05/run02 completed with child exit0, empty stderr,
one authentic signal capture and actual resumed application execution. This is
one completed positive context-transfer case, not a reliability rate. Two
subprocess attempts are retained: run01 aborted before allocation/signal delivery
on an incorrect initial-PKRU-zero precondition. The correction requires the exact
host PKRU0x55555554 and preserves/computes equality of the entire value; it neither
changes permissions nor relaxes any restoration comparison. See the retained
PREFLIGHT-01-FINDING.md. Do not count run01 as a signal-return failure or success.

**Native application fixture only:** no Tool/Detcore/RPC, SUD/seccomp controller,
PMU/clock, arbitrary-program activation, Hermit CLI or L2/340-population credit.
No product source was edited. Existing negative findings are not superseded.

## Exact observed checks

| Check | run02 observation |
|---|---|
| Actual continuation | Witness1 after resumed assembly; captures1 |
| Integer state | 18/18 entries: 16 GPRs, RFLAGS and actual continuation PC |
| Application redzone | 128/128 bytes unchanged |
| Defined FP state | 2228/2228 bytes equal after deliberate ordinary-runtime clobbering |
| Retired source storage | Entire8192-byte signal-stack interior overwritten after first return |
| Owned context storage | Entire4096-byte storage exactly unchanged; retained until subprocess exit |
| Ordinary runtime work | Bookkeeping checksum8976; errno changed then explicitly restored to7 |
| Native controls | Exact signal mask/altstack, FS/GS, CPUID/TSC modes, XCR0, permissions and PKRU preserved |

FP comparison covers x87 defined values/control/status, MXCSR, all XMM/YMM/ZMM
and opmask payloads, and PKRU for this host's standard2440-byte xstate profile
0x2e7. XSAVE reserved/padding/header bytes are not asserted as CPU-register
values; their original captured representation is preserved by the separate
whole-storage comparison. No numerical tolerances. RCX/R11 checks account for
the architectural SYSCALL clobbers, not asynchronous preserve-all semantics.

## Layout, lifetime and return path

Pre-established distinct anonymous guarded interiors: signal stack8192,
ordinary runtime stack8192, fixed application stack4096, owned context4096.
Only the application's initial mmap/mprotect setup is used. One thread, one
SIGUSR1 returning handler, default fatal synchronous-fault dispositions, no
active interval timers, no pending signals, no inherited altstack. All catchable
signals are blocked through handler and runtime work; the copied application
mask is restored only on final return. No SA_NODEFER, no reuse or nesting claim.

The unchanged shared parser copies the authentic kernel prefix/FP representation
and relocates its FP pointer. Only after copying does the handler redirect its
original context to the known ordinary runtime stack/entry, then return normally
through the existing live-frame restorer. Actual runtime execution overwrites
the retired signal storage, performs bounded ordinary work and restores errno.
The external assembly adapter returns the preserved valid copy through the
shared gate's actual syscall label; it does not misuse the original-live-frame
restorer API or construct synthetic signal metadata. Actual application assembly
then captures state before returning to its original host call chain.

Pre-execution linked-code admission: conservative successful-call-chain bound
2944 bytes, declared allowance4096; signal interior8192 exceeds
128+3376+4096=7600. The3376 frame allowance comes from the separately reviewed
matching-kernel analysis, not a single observed frame. Includes redzone/outgoing
arguments and resolved local memory-helper calls. No successful A/R path uses
malloc, output, an IFUNC resolver or recursion. Guard pages are not the proof.
See PLAN-BEFORE-EXECUTION.md and ADMISSION-BUILD05.md for exact assumptions.

## Source and evidence

External evidence root:
`/home/newton/work/dev-hermit/ignored/owned-context-positive-20260904`.

- Shared parser/gate commit: `8bb9f79350955d791f6f08f315ad0e30bb74afc3`.
- Parser: `reverie-preload/src/signal/native_frame.rs`; gate: `reverie-preload/src/trap.rs`.
- Retained dependency blob equality:83/83 files; dependency-bytecheck.txt.
- Fixture: fixture/src/main.rs and fixture/src/fixture.s; final-source.sha256 pins all source/lockfiles.
- Executed binary SHA256: `15875caa8ccf711d9910eb704ee819be72a9f58698f3e6e25882928c75564256`.
- Binary build ID: `7ab06afe96b2fa32b9830978192549922febf822`.
- Kernel: `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, build ID `fa15f90afce5930f146e2138d2ba10eb95e3e5ad`.
- Commands: COMMANDS.md; exact stdout/stderr/status: run-02.*; machine-readable result: result.json.
- Linked instructions/relocations/stack evidence: final-linked05/; original failures and superseded binaries retained.
- Offline build:2 jobs/2GiB/180s. Native execution:cgroup1 core/512MiB/10s; child0, no timeout/OOM.
- Curated freeze: final-positive-evidence.tar.gz and freeze.sha256; bulky Cargo target cache excluded.

The resource CSV's git_sha identifies runner context, **not** the Reverie
dependency or executed fixture. Use the explicit source/binary identities above.

## Remaining obligations

This positive fixture does not establish containment on failed restoration,
nested-window safety, reusable generations, allocator/Tool reentry, Tool/RPC,
clock ownership/accounting or arbitrary-program safety. Those tests and proofs
remain mandatory and unimplemented. Other xstates, permission changes and
general signal/lifecycle support are unqualified. No new run is needed merely
to rephrase this result; independent review should inspect the frozen evidence.

Works? YES for this single admitted native positive case. UX sensible? YES as
an internal fixture: output states layout/admission and explicit completion
checks; raw addresses are diagnostic evidence, not a public CLI interface.
