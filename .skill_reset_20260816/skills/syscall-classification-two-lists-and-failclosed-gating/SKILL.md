---
name: syscall-classification-two-lists-and-failclosed-gating
description: "Syscall policy lives in TWO synced lists; --strict IS fail-closed (aborts on UNSUPPORTED syscalls), as does HERMIT_FAIL_CLOSED / --panic-on-unsupported-syscalls"
---

Reclassifying a Hermit syscall requires editing **two** places that must stay
in sync (editing only the classifier is silently insufficient):

1. `detcore/src/syscall_classification.rs` — `classify_syscall(sysno)` returns
   `Determinized | PassThrough | Unclassified`. Has a compile-time count guard
   `assert_eq!(counts, [D, P, U])` and representative-policy asserts. Update both.
2. `detcore/src/lib.rs` dispatch (`let res = match classify_syscall(...)`, ~1240–1685):
   each class arm re-matches the **typed `Syscall` variant**. To make a syscall
   PassThrough effective you must ALSO add `| Syscall::X(_) => self.passthrough(...)`
   in the PassThrough arm — UNLESS the arm's `unexpected =>` fallback forwards.
   Determinized syscalls each need their own handler arm.

Gotcha: obscure syscalls arrive as `Syscall::Other(sysno,args)`, NOT typed
variants (e.g. the reserved/removed ENOSYS group, sched_get_priority_*). They
hit the `unexpected =>` fallback. Historically that fallback called
`handle_unclassified_syscall` (fail-closed), so a PassThrough classification did
nothing for them. PR #721 changed the PassThrough arm's fallback to
`self.passthrough(guest, unexpected).await` so `classify_syscall` is authoritative.

The fail-closed handling ("unsupported syscall: ...", `handle_unsupported_syscall`
in detcore/src/lib.rs) is gated by `config.panic_on_unsupported_syscalls`. As of
2026-07 (run.rs:1509 `if self.strict { config.panic_on_unsupported_syscalls =
true }`, PR-644) that flag is set by ANY of: `--strict`,
`--panic-on-unsupported-syscalls`, or env `HERMIT_FAIL_CLOSED=1`. `--strict` ALSO
sets `shutdown_on_unsupported_syscall`, so an UNSUPPORTED syscall aborts the run
with "Sandbox container exited unexpectedly / Exited(1)" on first use — NOT a
passthrough. (Earlier note that plain `--strict` won't panic is stale/wrong.)
So `hermit run --strict -- prog` is enough to reproduce an UNSUPPORTED-syscall
abort; grep the DEBUG log for `ERROR detcore: [detcore, dtid N] inbound syscall:
<name>` to name the offending syscall. Fixing = reclassify (Determinized/
PassThrough) + add the dispatch arm; untyped ones (Syscall::Other, e.g.
close_range/epoll_pwait2) use guarded `Determinized if call.number()==Sysno::X`
arms like process_madvise. Never copy a dated classification count: derive all
guard values from the exact current source and run its structural tests.

This is a Hermit product fact. Confirm it against current Hermit main and move
its maintained successor into Hermit's own skill tree rather than expanding the
parent copy.
