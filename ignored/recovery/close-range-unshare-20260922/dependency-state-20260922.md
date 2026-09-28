# KVM unshare recovery dependency state (2026-09-22)

- Current verified Reverie `origin/main`: `cdcefc4b9020b6297dd5f746fae3ab2066791cb0`.
- Frozen unshare head: `22e8c6c4a43fd5db1562b7782cff501874f5d815`.
- Frozen unshare parent: `2330dbbfef142584a911c5d76319a36a8956678c`.
- Frozen diff artifact SHA-256: `0e9026ebcecd7288ff0078b1ae2eba51d5a7799e6187edd624f0e03e3e5d07df`.
- `git cherry origin/main codex/kvm-pr611-recovery-20260922` reports every commit from `16b57331` through `22e8c6c4` as unpublished relative to current main.

PR 611 (`https://github.com/rrnewton/reverie/pull/611`) remains an open draft based on branch `codex/kvm-proc-carrier-auth-20260921`, with remote head `25306805dd5aefa1c73d844a48649c7266d951d3`. Its implementation commit `b33a3df0591a0bb610f583c0974d247e814c6c59` and recovered local commit `46a31789b80eb9e7ad9a34306cb3c1e27580adae` have the same stable patch ID, `027ffc33112a0f10efaf7c2a6765d4cc8d8de9ff`.

The isolated low-word prerequisite landed through `https://github.com/rrnewton/reverie/pull/622` as `cdcefc4b9020b6297dd5f746fae3ab2066791cb0`, from exact reviewed head `dadd0f6f1085a57366f247649dfa9a0d7a78f623`. All three changed files were verified byte-identical on remote main, followed by successful real `safehermit` runs of `echo`, `true`, and exact-input `cat`. It does not carry the PR 610/611 authentication or proc-path dependency.

The production `executor.rs` hunk in local commit `2330dbbf` has stable patch ID `e42c7256c1880e85d54aa5df004aa734733cba7f`, identical to the landed production hunk. The complete commits are intentionally not patch-identical: PR 622 adapts coverage to current main, while `2330dbbf` retains captured-supervisor reuse assertions that depend on the unpublished stack. During the eventual rebase, drop the already-landed production change but preserve/reconcile those stronger stacked tests; do not let an automatic duplicate or conflict discard them.

Hard gate: preserve the frozen unshare stack and do not rebase, publish, or weaken it until the backward-compatible PR 610 prerequisite lands and PR 611 can be rebased without replacing its `loginuid_fds`/authentication semantics. The recovered PR 611 implementation is semantically traceable by exact patch ID; remote human-authored history remains untouched.
