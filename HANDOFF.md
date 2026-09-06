# KVM exec page-discard optimization

Reverie branch `kvm-exec-replace` has no tracked changes at commit
`ab4420efd29d3bb576e9e390eb13bbfe2a67af20`, based directly on
`8c8c0a57649c9ffbf8a7a14291a64320f64b935f`. Nothing was pushed. The
19,106-byte binary base-to-head diff is `/tmp/kvm-exec-madv-ab4420ef.diff`
with SHA-256
`536f27ad5ba997ebffedd484fbbb36a55b28acbc487ae09e31ea4076659fe531`.
It changes only:

- `reverie-kvm/src/memory.rs`
- `reverie-kvm/src/vm.rs`
- `reverie-kvm/tests/static_elf.rs`

The commit replaces the full 1 GiB `write_bytes` exec reset with a page-aligned
`MADV_REMOVE` over `[BOOT_RESERVED_END, guest_end)`. It preserves the same
`MAP_SHARED` allocation, every cloned `GuestMemory` handle, and KVM slot 0. If
the advisory call returns any error, it explicitly zeroes the complete range
under the same `host_access` lock, so the optimization introduces no new fatal
exec outcome and remains correct after any partial discard.

The earlier fresh-mapping/slot-replacement implementation at `a23ff383` was
fully removed from the effective diff. In particular, `runtime.rs` is
byte-identical to base; no Tool memory-refresh or process-lifecycle behavior is
changed.

Tests and mutation checks:

- `cargo fmt --all -- --check`: pass.
- `cargo check -p reverie-kvm --all-targets`: pass.
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: pass.
- `cargo test -p reverie-kvm -- --test-threads=1` with real `/dev/kvm`: 262
  passed, 0 failed (206 unit + 3 counter + 2 ERESTARTSYS + 42 static ELF + 3
  strace + 6 vmcall; doc tests 0).
- `discarded_pages_are_lazy_zeroes_in_every_cloned_handle` checks that both
  handles keep one host mapping, the discarded page reads as zero, and adjacent
  pages retain their bytes. `discard_pages_rejects_unaligned_ranges` checks the
  explicit alignment guard.
- `successful_exec_discards_bytes_outside_replacement_image` loads the target
  at a disjoint PT_LOAD address. The guest directly reads a sentinel that only
  existed in the old image and exits 42 if it survives. It passes in direct and
  Tool modes. With the exec discard temporarily disabled it failed exactly as
  intended: direct arm returned 42 instead of 0.
- The existing real post-exec Tool test now verifies bytes written through the
  post-exec memory handle. The malformed-exec test verifies that guest-visible
  preflight rejection preserves the original sentinel through both the backend
  and a retained clone in direct and Tool modes.
- `post_preflight_interpreter_failure_is_fatal_after_image_reset` removes a
  dynamic interpreter after successful isolated preflight. It proves that the
  later load failure is fatal and the old user image has already been reset,
  matching the base `zero_raw` point-of-no-return behavior. The test passes
  unchanged with `MADV_REMOVE` and with a temporary one-line mutation back to
  `zero_raw`; when it originally asserted preservation, both variants failed
  identically by observing zeroes.

Release mutation A/B used a minimal static root ELF that executes a minimal
static exit ELF in a 1 GiB KVM mapping. The same runner was built twice from the
candidate tree; the control changed only the exec call back to `zero_raw`.
Every invocation used `timeout 5s` and a fixed CPU, and every arm exited 0.

- Direct, CPU 100, n=3: `zero_raw` 162.691--216.675 ms (median 182.264 ms);
  `MADV_REMOVE` 0.967--1.516 ms (median 1.204 ms).
- `StraceTool`, CPU 1, n=3: `zero_raw` 138.496--275.969 ms (median 191.174
  ms); `MADV_REMOVE` 0.554--1.647 ms (median 0.693 ms).

The independent review's multithreaded Tool exec hang is pre-existing, not a
regression from this optimization. The exact 5-second reproducer returns 124
with the same clone/getpid/futex=-4 trace on both base `8c8c0a57` and old head
`a23ff383`. Its preserved evidence is
`/home/newton/work/dev-hermit/ignored/validate/evidence/review-kvm-exec-a23ff-multithread-exec`.
An uncommitted handoff-design sketch was saved before removal at
`/tmp/kvm-multithread-exec-handoff-scaffolding.patch` (8,825 bytes, SHA-256
`fa77a2e082340bc0eb7b9e8fd32bb8b1b50b935343ea5db3551945bfce2afccd`);
that separate semantic defect should not broaden this performance patch.

Hermit Cargo path overrides for the coordinator's canonical A/B:

- `/home/newton/work/dev-hermit/worktrees/slots/kvm-exec-replace/reverie-kvm`
- `/home/newton/work/dev-hermit/worktrees/slots/kvm-exec-replace/reverie`

The concurrent sparse-snapshot prototype changes `GuestMemory` to a shared
memfd. That is conceptually compatible: `MADV_REMOVE` hole-punches tmpfs/memfd
pages while this commit retains mapping identity, though the combined tree
should be remeasured after reconciliation.

Registry/liveness limitation: slot creation from the sandbox with PID 1 was
blocked by read-only shared Git metadata, while escalation with PID 1 was
rejected as identity substitution. Creation succeeded with the real host Codex
PID 2232766, but a sandboxed `wrkslots status` cannot see that host PID and
therefore reports generation 1's owner as dead. Do not infer that the worktree
is abandoned from that namespace-mismatched status; inspect this handoff and
the live agent/session before cleanup.
