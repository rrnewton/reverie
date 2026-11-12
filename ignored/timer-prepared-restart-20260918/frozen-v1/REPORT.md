# Prepared caught signal syscall restart correction

Author implementation report; independent review is pending. Base is landed Reverie `d4fdd4d7ae47753154cdb9ffec9897b73c29954a`, tree `808511a79828357cfab6cf0dfb98e76df1b9f452`. No commit, push or public API change was made. Exact three-path patch: `230fd70d1d63addece68e5863c27b48897f79a8979f07fdd7bc1afacdb6d1b1a` (12,141 bytes).

## Defect and correction

In `reverie-kvm/src/runtime.rs`, a Tool returning ERESTARTSYS left the callback loop only when an eligible signal remained pending. The parked observation path has already dequeued and acknowledged a caught signal, run its actual Tool signal hook, and reserved that signal for the existing frame path. It no longer appears in pending. Repeating the callback prematurely increments its nonce; the reserved selection belongs to the previous callback. The later selection validation can reject that stale nonce. The controls below directly observe the earlier unwanted callback, rather than claiming a measured later EINVAL.

The eight-line production hunk admits `executor.has_prepared_signal()` alongside the existing eligible-pending predicate. It returns through the existing EINTR/SA_RESTART frame decision, which consumes and validates the real selected signal. No effects, acknowledgement, selection, callback identity or raw positive result are cleared or synthesized. A handler without SA_RESTART returns EINTR to userspace; with SA_RESTART the handler runs first and the original syscall enters a new callback after rt_sigreturn. Positive syscall results continue through the unchanged `Some(raw)` branch.

Only the existing fixture and Tool support file change alongside that hunk. Modes 17–19 perform actual nonblocking pipe reads. Empty reads must return EAGAIN before the real publication, dequeue notification/acknowledgement, signal hook and posthook RPC/injection. The handler checks actual siginfo, altstack and mask. In restart mode the handler writes one real byte; the next callback must observe handler count one, a newer nonce with unchanged task/process generation, and read exactly that byte. The no-restart mode requires one callback and userspace EINTR.

The partial control preloads exactly `abc`; its marked read must perform one actual successful three-byte read, return that unchanged count, invoke exactly one handler and never repeat the marked callback. A subsequent ordinary empty read must return EAGAIN; it is a zero-effect probe, not a second successful read. These same strict controls are byte-identical before and after the production fix.

## Actual component evidence

All original receipts, including failures, remain immutable in sibling qualification directories. Complete commands, toolchain/source/dependency closure, actual Cargo artifact and loader identities, list output, raw libtest events, service identity, bounds and terminal accounting are bound by `QUALIFICATION.json` and `READBACK.json`.

| Control | Original runtime | Corrected runtime | Corrected payload wall / aggregate CPU |
| --- | --- | --- | --- |
| Prepared read, no SA_RESTART | raw 101: callback repeated when forbidden | pass, 1 selected / 0 ignored | 0.114 s / 2.306 s |
| Prepared read, SA_RESTART | raw 101: repeated callback saw handler count 0 | pass, 1 selected / 0 ignored | 0.112 s / 2.299 s |
| Prepared positive three-byte read | pass | pass, 1 selected / 0 ignored | 0.120 s / 2.332 s |
| Existing pending-signal ERESTARTSYS policy | not repeated before fix | pass | 17.272 s / 2.403 s |
| Existing parked callback/frame/dequeue contract | not repeated before fix | pass | 0.693 s / 2.447 s |
| Existing caught posthook mask/altstack frame | not repeated before fix | pass | 0.108 s / 2.311 s |
| Existing restart-classification unit control | not repeated before fix | pass | 0.002 s / 2.141 s |

Seven unique test names passed after correction: six KVM test declarations and one library declaration. The original multi-mode controls keep every mode. This is a count of actual selected declarations, not additional counts for their self-exec children or repetitions. `REVERIE_REQUIRE_KVM=1` prevents KVM-unavailable skipping from qualifying. Both actual list operations qualified. Corrected compile passed (8.998 s payload, 23.644 s aggregate CPU), rustfmt passed, and Clippy with `-D warnings` passed (2.054 s payload, 4.439 s aggregate CPU).

Two initial test-only compiles failed with missing/incorrect `Addr` import (raw 101, no guest execution). Their sources and compiler output remain in `negative-before-v1` and `negative-before-v2`. The corrected import uses the actual `reverie::syscalls::Addr` reexport. `negative-before-v3` then compiled and recorded the two genuine runtime failures and passing partial control. Neither original failed runtime receipt is relabelled accepted.

Before and corrected static/library ELF copies are separately retained under each successful packet's `retained/`, with byte identity to actual Cargo events. Original receipts still name the now-rebuilt target locations; the immutable retained copies preserve the historical executable provenance. All 2,617 source manifest entries were authenticated for each attempt, reconstructing changed historical files from their retained source copies; gitlinks retain their explicit non-expanded scope.

Pinned nightly-2026-07-29, offline/locked, two Cargo/native jobs and the same owned target/lease were used. Compile/Clippy limits remain 600 aggregate CPU seconds / 900 wall; test limits remain 30 aggregate CPU seconds / 60 wall; 16 GiB memory, zero swap, 16 MiB stderr, 64 MiB sampled stdout and 100 GiB free floor remain unchanged. The approved observer remains SHA `137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179`. All completed phases authenticated terminal accounting and unchanged inputs.

## Scope and goalpost assessment

No assertion, threshold, comparator, skip, failure label or existing gate was weakened or deleted. Existing fixture modes remain admitted exactly as before; the mode whitelist only gains the three added cases. Failure-before remains failure. The fix preserves the existing public API and does not implement a new signal delivery, timer recipient or I/O mechanism.

This establishes a finite initialized KVM callback/frame correction. It does not qualify Hermit parked read continuations, timer integration, multiple recipients, blocking named FIFO operations, poll/pselect temporary-mask policies, or full-DAG parity. FIFO research and broader timer design remain separate. SCM and landing remain with the root owner, following independent review.
