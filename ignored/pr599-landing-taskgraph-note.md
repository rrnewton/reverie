KVM lane landing evidence for https://github.com/rrnewton/reverie/pull/599:

- Landed by guarded rebase merge at 2026-09-20T06:51:35Z.
- Exact reviewed PR head: ce442fce6b1ee366e0c3a9cff7fa981ff8893ae4 (tree a83ef7f9e0645b28d3dbe83ed02a32e461674cc7).
- Landed Reverie main SHA: f7bd85e11dd258112148ed2cba6531501a1a00d9.
- gh-merge-verified returned 0 and independently reported 5/5 changed files ARRIVED; a post-merge blob comparison also matched all five PR files exactly.
- Both required exact-head critical reverie-api approval attestations were present: Codex comment https://github.com/rrnewton/reverie/pull/599#issuecomment-5748224064 and Claude comment https://github.com/rrnewton/reverie/pull/599#issuecomment-5748224464.
- Qualification at the exact reviewed head: focused 10/10, full 746/746 at 64 threads, full 746/746 serially, formatting and strict Clippy/check configurations passed.
- Disclosure remains: the host-default 316-thread suite is not green. The retained unbounded matrix was base 3/5 and head 1/5, with failures in unchanged pipe/SIGPIPE and nonblocking EOF/EAGAIN controls. This scoped component landing does not establish Hermit consumer or backend parity.

The KVM lane and kvm_backend_emits_no tasks remain IN_PROGRESS. Next step is the designated Hermit consumer/pin update to landed Reverie f7bd85e11dd258112148ed2cba6531501a1a00d9 followed by the strict no-retry KVM qualification matrix.
