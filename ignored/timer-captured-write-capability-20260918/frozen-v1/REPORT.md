# Captured-write callback capability

Author implementation and component qualification report; independent reviews and critical-trigger-2/manuallint remain pending. Base: Reverie `2c75f5b46e737ca1cbd5d407803e14572f873c0f`, tree `2d2777dfb0dcc891328ac864a87452c11831d5da`. No SCM mutation was performed. Exact eight-path patch: `d89a2b1c73cb724fb2969580bf6c9f1454028fbd541160cff07dca1399a4f8fa` (30,795 bytes).

## Behavior and proof

The new public `Guest::captured_write_signal_site(Write) -> Option<CallbackSignalSite>` defaults to None and forwards through IntoGuest. It is a read-only admission query for the existing CapturedOutput scalar-write path; it does not perform the write, publish/remove signals, execute hooks, read a clock, check the buffer or promise success. The documented caller contract requires the identical call and full returned site to be rechecked immediately before publication without an intervening guest operation or injection.

KVM stores the original SyscallRequest only when constructing the actual subscribed syscall callback (`runtime.rs:3327`). Initial exec, lifecycle, fault and signal callbacks get None. The adapter query (`runtime.rs:692`) compares the complete syscall number and all six raw arguments, requires an Ordinary signal guard and no previous injected execution, then requires the existing original/unconsumed sole-live-leader parked boundary. The descriptor conversion matches scalar write's checked i32 conversion; a malformed high ABI word cannot become fd 1 by truncation. KvmGuest additionally refuses during dequeue notification, nested observation or checked-out stack ownership (`runtime.rs:1137`).

The executor query (`executor.rs:3449`) validates every process/task/callback/boundary identity field, requires actual enabled capture and a currently open output alias, and reads the authoritative shared FileTableState. Reading the executor's cached LoadedStaticElf aliases would be insufficient: a sibling can change the shared table and exit before the leader's next execute/install. A control creates a real executor sibling, closes its shared fd 1, retires it, and proves that the now-sole leader's stale cached stdout entry is refused while stderr remains accepted. Foreign identities and prior callback nonces are refused without publication, output or nonce changes.

The alias decision is shared with the unchanged scalar-write routing through `output_alias_from_sets` (`executor.rs:6579`). Genuine stdout/stderr dup aliases are admitted; closed/reused aliases and ordinary replacements of fd 1 are refused. Current process clones have separate file tables; CLONE_FILES is supported through thread clones, which are excluded while live by the retained sole-receiver guard. Given the coordinated single-original-request transport and no intervening injection/guest operation, immediate requery suffices here without a new opaque alias token. This is not a general concurrent descriptor capability.

The production/API paths are `reverie/src/guest.rs`, `reverie-kvm/src/runtime.rs`, `reverie-kvm/src/executor.rs` and two adapter initializer updates in `parked_signal_runtime.rs`. The remaining paths register three actual VM controls, their C fixture and a lower-level adapter/default control. No prepared-restart hunk, existing syscall result policy, public signal receipt, clock, scheduling or resource-limit behavior changes.

## Actual controls

Ten unique selected test declarations passed, with zero ignored: five new declarations and five existing neighbors. Seven are VM test declarations and three are library declarations; repeated self-exec children and internal mode loops are not counted as additional declarations. REVERIE_REQUIRE_KVM=1 prevents unavailable-KVM skipping from qualifying.

The three new VM controls execute 18 modes:

| Route/boundary | Expected and observed result |
| --- | --- |
| Captured stdout, stderr, stdout dup, stderr dup | Same current capability on requery; actual three-byte write precedes normal signal hook and handler output |
| Capture disabled; fd 1 replaced; alias closed/reused; closed fd 1 reused | Query refused; unchanged actual host/file write or EBADF, with no signal publication |
| Original getpid with Write-shaped arguments; prior injected getpid; consumed write | Query refused; no repeated original effect |
| Each of six wrong raw arguments; checked-out stack | Query refused without changing the valid original identity or publishing/writing |
| Invalid buffer on a captured descriptor | Capability admitted; actual EFAULT preserved, then the actual signal handler |
| Existing 16 MiB short-write limit with a 16 MiB + 1 request | Exact positive short count and bytes preserved, followed by one handler marker |
| Blocked accepted alarm | Original write completes first; unblocking later delivers one real handler |
| Active observation/prepared signal; dequeue and structured hooks | Capability refused within these nested/consumed boundaries; reserved frame still delivered |
| Malformed raw descriptor with fd-1 low bits | Query refused and actual EBADF preserved |

For publication-only cases, recorded Tool events require exactly one original write result before the structured signal hook, and the complete captured byte vectors require `abc!` (or the corresponding stderr bytes and stdout handler marker). The active-observation negative mode is explicitly separate and does not assert hook-after-write ordering. Refused queries do not create publication/dequeue/hook events or extra captured bytes. The fault and short-count assertions are exact.

The other two new controls test authoritative-table/full-identity rejection and default/internal-adapter/IntoGuest/nested guards. Existing controls retained their full assertions: standard descriptor vectored routing, the three prepared-read EINTR/restart/partial controls, and the parked callback/frame/dequeue multi-mode contract.

Compilation passed in 17.255 seconds payload wall / 38.635 aggregate CPU seconds. All ten test payloads finished within 0.004–0.659 seconds (individual complete-service CPU roughly 2.1–2.5 seconds, including the bound wrapper). rustfmt passed; Clippy with -D warnings passed in 4.372 seconds payload / 6.752 CPU seconds. The actual reverie-core plus reverie-ptrace Cargo check passed in 18.208 seconds payload / 32.674 CPU seconds.

The first extra core-check command mistakenly named package `reverie` (the library name) and failed raw 101 before compilation in 0.093 seconds. Its receipt remains accepted=false. A fresh continuation corrected only that selector to the actual package `reverie-core`; source, checks and limits did not change. It is not a product failure or a qualifying original receipt. The copied compile/list receipts in the continuation are provenance reuse, not additional executions.

## Evidence, limits and review triggers

QUALIFICATION.json binds actual phases, raw statuses, exact test-name union, compiler events, retained ELF copies and costs. SOURCE_MANIFEST.json binds all 2,620 copied entries: 2,610 regular files, seven links and three explicit non-expanded gitlinks. The complete frozen source is under source/. Source hashes were rechecked against both that copy and the working source after qualification; the full reverse patch check passed. The earlier SOURCE_INPUTS qualification note records checks still pending at the time of source freeze; this final report and QUALIFICATION contain their actual completed outcomes.

The pinned nightly-2026-07-29 compiler, offline/locked resolution, two Cargo/native jobs and existing private target/lease were retained. Compile/check budgets remain 600 aggregate CPU seconds / 900 wall; tests remain 30 CPU / 60 wall; 16 GiB memory, zero swap, 16 MiB stderr, 64 MiB sampled stdout and 100 GiB free floor remain unchanged. Observer SHA remains 137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179. Exact toolchain, loader, workspace/dependency closure and terminal accounting are in the phase records. No network or source dependency update was required.

This is a new public API and retains the owner's two-independent-review and critical-trigger-2/manuallint landing requirements. The author does not provide independent approval. Hermit's publication transport, request restoration, COMMIT ordering, actual timer/ordinary-write outcomes and full integration are separately owned and are not qualified here. Existing ordinary-write failures, FIFO timeout, pipe-polling failure and broader multiple-recipient limitations are not deleted, relabelled or discharged by these component controls. No assertion, tolerance, comparator, failure label, test skip or existing gate was weakened; no check was removed instead of satisfied.
