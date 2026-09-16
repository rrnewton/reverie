# Parent KVM parity reader handoff

Worker: /root/kvm_parity_ledger_parent. Root owns TaskGraph and the KVM goal.
Worktree: /home/newton/work/dev-hermit/worktrees/slots/kvm-parity-ledger-parent-20260916
Branch: codex/kvm-parity-ledger-parent-20260916
Base: 3c2bf99615cdfd62f63bd4077b1f58be34710099

All implementation is uncommitted; no index, ref, pin, policy or network mutation
has been made. The 31-file reader/test checkpoint must remain intact. Root is
landing the independently extracted schema8 nine-path patch in another slot;
do not reset, stash, or extract it here.

Evidence root:
/home/newton/work/dev-hermit/ignored/kvm-parity-20260914-codex/next-work/queue-drain-20260916/parent-ledger

Current checkpoint:
- schema10-native-source.patch SHA256 0cf6c7f0af9a046c5a99fb6f4e59eba8090f76287e135c5392b5e3a328bfff57
- schema10-native-source.json SHA256 864af586d936cc30a8489652d49a64bf7d3df8818abe0c692bab449a4ce733e6
- SCHEMA10-NATIVE-VERIFY.md has the proposed bounded native commands, not executed.
- Own Hermit is still clean ed99e05133b00058fafed7f3dfaf48ab8fd334e6 with agent-utils
  2781b1054efc3a9c561dbed35584a6fca1ed8676. Native compilation requires the root's
  landed Hermit support dependency direction; do not pin an unlanded intermediate.

Earlier checks passed: schema8 compile, 27 failure-obligation Rust tests, six
schema8 Rust tests, 11 Python tests plus five subtests, format/AST/diff. Later
schema10 boundary checkpoint passed nine mocked boundary/attribution tests and
static checks. Current source adds real artifacts/native tests and four more
boundary cases; these additions have NOT run. Timeout coverage so far mocks the
requested process-group cleanup, not an actual child kill/reap.

Root approved the direct shared Rust helper + one standalone Python batch, with
Rust qualification using the separate coverage/base/admission-only Python mode
before invoking the shared artifact verifier in process. No per-row compiler
recursion, no row-supplied preverified authority, no active max-version bump.
Schema9 stays readable but has no artifact/full qualification without the new
independent plan. Compact schema10 ledger summaries keep raw report/path bytes
in the full local artifact, preserving existing event privacy transport.

Exact producer fixtures were copied from edd48bf2040e7faa9330a8262e4af03555f30ab7
and retain their manifest/provenance under ci-hub/validate/tests/fixtures/schema10.
All raw environment maps are empty and paths deliberately use /home/fixture.

Focused read-only producer review targets immutable Hermit
fc402c2e191b5a08672eacb4396d0901549ec201. See shared-review-fc402c2e/REVIEW.md and
source.json. One high finding was sent to root and recovery: ordinary matched
operands of unavailable cross attempts omit exact self-output validation, so
contradictory compared_outputs can receive ordinary/reference clean-leg credit.
Completed cross reports validate this, but unavailable ones do not reach that
check. A static, unexecuted negative fixture is saved under
shared-review-fc402c2e/unavailable-cross-mismatched-self-output/. Producer worker
/root/kvm_parity_recovery owns the fix; do not edit producer source here.

Next: root coordinates landed dependency materialization, any rebase onto landed
schema8 reader support, source review, and the final fully bound verification
caller. Preserve first failures; no budget increases or retries without authority.
No claim of backend-wide parity, new guest measurements, or task completion.
