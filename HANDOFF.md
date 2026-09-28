# Current handoff: KVM sibling signals ready for independent review

Registered slot: /home/newton/work/dev-hermit/worktrees/slots/astra-reverie.
Branch astra-reverie-kvm-sibling-signals, exact head 3c3761ee0c6df75a3e7ebc6bd05fa617e949c7de, base 20d7b7610a04e3e46fe656417bdeb3adc4f78693. Separate test synchronization/diagnostic commit 658b1d862a576cd3e666d209e6b199da5b387f99. Tracked tree clean. No remote publication or review approval yet. Root will review; trigger 2 requires independent Claude and Codex. Do not change Hermit capability flag yet.

Full required-KVM suite 605 passed, no ignored; Clippy/fmt passed. All original failures, native oracle corrections, source race control and four signal mutation failures are retained. Historical futex cause remains unknown. Read tracked ai_docs/kvm-sibling-standard-signals.md and verified TaskGraph note astra-reverie (exact text /tmp/astra-reverie-sibling-signal-final-note.md). Full patch/production patch/artifact manifest are /tmp/astra-reverie-sibling-signal-final{,-production}.patch and /tmp/astra-reverie-sibling-signal-final-artifacts.json. Copied test executables target/astra-sibling-signals-3c3761ee/{static_elf,reverie_kvm}.

Previous vector candidate 8b0c5dbe remains preserved on astra-reverie-kvm-vectored; https://github.com/rrnewton/reverie/pull/538 remains draft, full combined Hermit tests failed (sibling tgkill ENOSYS), no qualification inferred. https://github.com/rrnewton/reverie/pull/467 remains waiting cross-family review. Original six draft recovery work is otherwise landed, including new main repair https://github.com/rrnewton/reverie/pull/551 at 20d7b761. No worktree creation needed; only this slot is authorized for product writes.

## Earlier handoff retained below

# astra-reverie handoff

This registered slot belongs to `/root/astra_reverie`, under `/root`'s active
TaskGraph assignment. Do not reclaim or move it while the shared coordinator
process PID 3924610 remains live. Product writes are confined to this slot.
TaskGraph notes use the parent `ci-hub/bin/tg-note-verified` and task
`astra-reverie`. GitHub operations use `with-proxy`. No Google Chat.

Current checkout: `astra-reverie-kvm-vectored`, exact
`8b0c5dbe01a52d888f8d9020ef5735fb3af41853`, based on landed main
`20d7b7610a04e3e46fe656417bdeb3adc4f78693`. Tracked files are clean.
This handoff is deliberately untracked.

## Pending work

- https://github.com/rrnewton/reverie/pull/538 now publishes exact `8b0c5dbe`
  on `codex/kvm-vectored-io`, replacing preserved original `3646ba2c` with an
  explicit expected-tip lease through `git-push-verified`. The full fetched tree
  is `06f39b51c9fef3b4b75474f4c866938c41286553`; API head, complete body,
  three-path file list, draft status and review label are verified. Root approved
  the scoped standalone implementation; drafts approved the test/oracle/README
  cap delta. Required Claude review is pending through the coordinator. Do not
  land before root confirms review completion.
- Worker `/root/astra_hermit_ready` is testing Hermit prerequisite `7b9748c6`
  with an archive of exact Reverie `8b0c5dbe`. The three-pthread control and
  strict descriptor matrix passed. The unchanged full guest FAILED after
  100.812 seconds; separate partial-signal guest FAILED after 100.703 seconds.
  Both remain failed in the public body, fully read back at unchanged 8b0c.
  Minimal native/ptrace sibling signal passes; KVM returns ENOSYS/exit3.
  Root assigned DESIGN ONLY, no source implementation yet: see
  /tmp/astra-reverie-sibling-signal-design.md. It proposes one canonical private
  thread queue with generation-safe publication and distinguishes existing
  restart handling from unqualified broader wakeups. Required primary-source
  grounding is complete and documented there. Wait for root design review
  before production implementation. All tracked source remains unchanged.
- https://github.com/rrnewton/reverie/pull/467 remains draft at
  `d77cf68d3042581d133f998519a0fe2fb971be78`, branch
  `astra-reverie-thread-identity`, with required Claude review pending. Root
  reviewed the repaired first-instruction barrier, including warmed entry
  coverage. Keep source stable unless root directs rebase or a new defect
  requires repair. Current main has advanced since that candidate; final
  review/evidence must bind to any eventual rebase.

## Evidence

For the complete final vectored change use
`/tmp/astra-reverie-pr538-final-candidate.patch`, generated with `--histogram`.
GitHub's default diff for the repeated executor test text is misleadingly large.
The final cap-only delta is `/tmp/astra-reverie-pr538-cap-delta.patch`.

`/tmp/astra-reverie-pr538-final-published.json` records full source/body readback.
`/tmp/astra-reverie-pr538-final-push.log` records normal formatting and push.
`/tmp/astra-reverie-pr538-final-publication-note.md` was stored byte-for-byte on
TaskGraph. The proposed body is
`/tmp/astra-reverie-pr538-final-proposed-body.md`.

`/tmp/astra-reverie-pr538-final-artifacts.json` binds all source hashes.
`/tmp/astra-reverie-pr538-cap-final-checks.json` records 618 required-KVM tests,
zero failures or ignores, default parallelism, 10.223 seconds including build,
plus passing focused controls, Clippy and formatting. The final metadata-only
rebase preserved the complete tested tree; see
`/tmp/astra-reverie-pr538-cap-rebased.json`.

All original sources remain remotely preserved and tree-verified; refs and
original SHAs are in `/tmp/astra-reverie-pr538-preserved-sources.json`.
All original test/helper names and KVM vector fixtures remain; see
`/tmp/astra-reverie-pr538-source-accounting.json`.

The 16 MiB O_DIRECT limit is explicitly unsupported-operation evidence above
the bound, not native parity. Complete file/buffer/position preservation is
checked for all twelve larger cases; six native/KVM calls at the boundary
still succeed. Removing the guard again fails the live test after those six
supported cases pass. Do not weaken assertions, tolerances or the comparator.

## Landed work and retained limits

Original drafts https://github.com/rrnewton/reverie/pull/463,
https://github.com/rrnewton/reverie/pull/476,
https://github.com/rrnewton/reverie/pull/478 and
https://github.com/rrnewton/reverie/pull/479 are merged.
The discovered ordinary-worker cancellation and per-process Tool ownership
repair https://github.com/rrnewton/reverie/pull/551 also merged at current
main `20d7b761`; root fetched and verified its complete tree.

Existing KVM leader-first and worker-fork limitations are retained in
https://github.com/rrnewton/reverie/issues/549 and
https://github.com/rrnewton/reverie/issues/550. Neither is claimed qualified.
The actual fixture compiler was `/usr/bin/gcc -O2 -pthread`, producing a
dynamic ELF; both issue bodies were corrected and read back exactly.

The DBT coordinator trace capture gap is tracked at
https://github.com/rrnewton/hermit/issues/2993. Strict guest/Tool record parity
does not establish a complete global scheduler trace. The measured barrier
loop overhead remains 1.95x in the retained earlier comparison.

Every ad-hoc Hermit invocation must use the updated parent wrapper
`/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-reverie-maturity/bin/safehermit`.
Do not change skills, protocols, foreign slots or another agent's source.

Latest local experiment: branch astra-reverie-vector-signal-composition at immutable 89a9f0217b2f3ab6106aac12ea2a650eee7972d6 (tree dd76130a579c2f69c588bf0c22d38750496877bd). Combines reviewed vector 8b0c5dbe and signal 3c3761ee on landed 20d7b761. Required-KVM suite passed 626 tests (0 ignored), Clippy and fmt passed. Public heads remain unchanged. Root and ready received the source for the separately authorized Hermit capability experiment. Full verified note: /tmp/astra-reverie-vector-signal-composition-note.md; source/artifact manifest: /tmp/astra-reverie-vector-signal-composition-final.json. No Hermit edit or combined Hermit qualification. Earlier full/partial 100-second failures remain failed.

Independent read-only https://github.com/rrnewton/hermit/pull/2880 review completed; verified report /tmp/astra-reverie-hermit-2880-review.md. Recover the absent producer on current main with CPU timeout policy and exact parsed-input digest binding; no closure/publication was performed.

Latest implementation checkpoint: terminal API is immutable at c53dcab920cad98b011a7ec2453d32ecd372b1de (tree eb484d1b50e34478460ea0d8c5ad5ce1a95808aa), on astra-reverie-vector-signal-composition. Its parent is frozen pipe FIONREAD c8852338edfe71e1282fa9a50d47875d8a256684; root independently approved that FIONREAD component. Both preserve the original 89a9 composition below them. Local rescue refs preserve c885 and c53. Source is clean; public PR heads remain unchanged.

Root authorized the separate terminal API and is reviewing exact c53. It adds async Guest::cancel_current_thread(&mut self) -> Never, explicit IntoGuest forwarding, distinct KVM terminal outcome and consuming lifecycle cleanup. Both injection guards remain byte-identical; the current leader/process boundary stays explicit. Worker errors are retained across joins and returned in guest-TID order after owner hooks. No Hermit predicate is changed in this repository.

652 core/KVM tests passed with KVM required, 0 ignored, in 14.783 seconds. Clippy with warnings denied, fmt and full workspace check passed. Actual 13-mode ELF callback matrix and two direct hypercall modes passed; six production defect restorations each failed the same tests. Earlier actual failures (omitted clone3 fixture accounting, wrong pthread signal context, post-join TID observation) are retained and explained. The final fixture synchronizes before pthread_join so its zero-child-TID assertions observe the consuming hook, and raw clone actually reaches first-instruction signal delivery.

Full diff /tmp/astra-reverie-terminal-controls/final.patch (SHA256 78da9dcf17ea7ecd036361313be99d87274f1d89a9db624d2f1550ad34af2e5e); production.patch adjacent; 125 artifact hashes in final-artifacts.json. Tracked report ai_docs/kvm-terminal-cancellation.md; verified TG note /tmp/astra-reverie-terminal-final-note.md. Copied binaries are target/astra-terminal-cancellation/final/{static_elf,reverie_kvm}, hashes recorded in report/manifest. Missing-worker-notify patch is /tmp/astra-reverie-terminal-controls/mutation-missing-worker-notify.patch, SHA256 ea88f2b200138ec58eb465593cac68804aa22849de958540f5cb4e96e3fe663f.

Ready received c53 for an immutable archive and compilation of the already-prepared narrow Hermit caller. Actual paired runs remain behind root source review. Keep source stable for review. Trigger 2 applies; Codex and Claude independent review is required before public landing. No public writes authorized for these components yet. Full/partial vector failures and the original status-37 missing-hook evidence remain failures until new actual combined evidence qualifies them. Existing leader-first and worker-fork limitations remain https://github.com/rrnewton/reverie/issues/549 and https://github.com/rrnewton/reverie/issues/550.

Checkpoint: private exec-worker teardown recovery frozen at7061a4cc16318417988690f39cadbb049874ed2b on c53dcab9. Full source/report under ai_docs/kvm-exec-teardown-errors.md; verified TG note /tmp/astra-reverie-terminal-exec-final-note.md; complete84-artifact manifest /tmp/astra-reverie-terminal-controls/exec-error-artifacts.json. Required-KVM complete repeat654pass; initial SCM_RIGHTS EOF/EAGAIN failure remains unresolved, original log preserved. All10exec modes and5mutations substantiated; no public changes. Root independently reviewing. Next assigned read-only interface feedback: /tmp/astra-hermit-ready-kvm-sigchld-design.md. Keep current source/head stable.

Current checkpoint: normal-child-exit backend component is frozen at b379698a3387c1e640849cd332c4eb111b5e4b62, tree aba6aebd36b5715efd64d49ddd7dce0229b44340, on private branch astra-reverie-kvm-child-exit-signals. Base a8d87e6a15f527e1c683e99faef2979f7c9ecfcb contains the separately approved SCM_RIGHTS test isolation. Root independently approved that isolation and the earlier 7061 exec cleanup. Named rescue refs preserve a8d, 7061, API checkpoint 6f14b930 and final b379. Public heads remain unchanged; no publication is authorized for this private stack yet.

The final production code is byte-identical to 6f14b930410652f694f9bc01b68ab521f60aa2bf; Ready has that immutable API for archival Hermit protocol compilation. The final added receiver controls cover 14 actual native/Tool cases, full siginfo/signalfd bytes, first-event coalescing, independent wait/reaping assertions, ignore/mask behavior, signal/fault contexts, valid Tool replacement, and EINVAL for malformed returned metadata. All 665 core/KVM tests passed with KVM required, zero ignored, in 15.138 seconds. Clippy, fmt and full workspace compile passed. Seven production defect restorations failed actual unchanged assertions; all source was restored before final qualification. Earlier fixture/compile failures remain retained. In particular, native signalfd-before-fork succeeds while KVM explicitly refuses ENOSYS; the final receiver test creates the descriptor after wait without claiming inheritance support.

Durable tracked report: ai_docs/kvm-child-exit-signals.md. Byte-verified TG note: /tmp/astra-reverie-child-exit-final-note.md (5,877 bytes). Full diff: /tmp/astra-reverie-child-exit-final.patch, SHA256 f2d589a953b714c51fcd727d05f431188ee0aba74ed2b08445c7ce233bf1b380. Complete 538-artifact manifest: /tmp/astra-reverie-child-exit-artifacts.json, SHA256 18ae87ef64fab70d2a57c19befcc4ad48354094498d8569fb43301e364cd503c. Source/copied-binary identities: /tmp/astra-reverie-child-exit-final-identity.json. Tests and mutation drivers use only this registered slot and its target area; no command is currently running.

Root has read the entire API checkpoint and found no source issue, including the nonblocking signalfd/F_SETFL invariant, and is now reviewing b379's final controls, mutations and artifacts. This is not final approval. Keep the head stable. Trigger 2 applies to the core Guest API; independent Codex and Claude review remains required before public landing. No Hermit scheduler or capability predicate was edited here, and no combined qualification or general asynchronous signal support is claimed. Earlier full/partial failures and the later SIGCHLD INFO mismatch remain retained.
