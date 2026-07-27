# LiteInst Guest Trait Handoff

## State

Task: impl-liteinst-guest-trait (closed in tg)
Repository: Reverie
Slot: /home/newton/work/dev-hermit/worktrees_reverie/slot111
Branch: impl-liteinst-guest-trait-slot111
Base/uncommitted HEAD: 4713b3054e8b76fe5e9f03f16c89ffea2e401d91
PR: none for this dirty work
State: intentionally dirty and uncommitted. Do not reset, clean, or discard it.
Paired Hermit slot: /home/newton/work/dev-hermit/worktrees/slot128

## Implemented

- Generic LiteinstBackend with explicit-preload status/output APIs.
- CoordinatorRpc over reverie-rpc-transport.
- Generic LiteinstGuest and install_tool.
- Tool subscriptions, per-thread state, LocalMemory, and LocalStack.
- Full saved HookContext through Guest::regs and set_regs.
- Inject/tail-inject and correct on_exit_thread-before-physical-exit ordering.
- Kernel-visible rt_sigprocmask always reserves SIGSYS.
- Bare-program resolution and guest LD_PRELOAD preservation.
- First-trap evidence: calls=32, traps=1, hooks=32 with real RPC.
- Shared reverie-preload source is untracked in this slot.
- Old reverie-liteinst/src/pun.rs is deleted; punning comes from liteinst2.

## Validation

- cargo test -p reverie-liteinst --all-targets: 3/3 unit, 1/1 RPC, 15/15 integration.
- cargo clippy -p reverie-liteinst --all-targets -- -D warnings: pass.
- cargo fmt --all -- --check and git diff --check: pass.
- Paired Hermit exact CLI and L2 micro-suite pass; see its HANDOFF.md.

If Cargo reports Transport endpoint is not connected, use:
with-proxy env PATH=/home/newton/.cargo/bin:/usr/bin:/bin cargo ...

## Dependencies

reverie-liteinst/Cargo.toml points at:
/home/newton/work/dev-hermit/scratch/liteinst2-clean-separation
RPC PR #98 is landed: https://github.com/rrnewton/reverie/pull/98
Shared preload PR #100 is open: https://github.com/rrnewton/reverie/pull/100
The generic backend/Guest work is not committed or in a PR.
Hermit slot128 temporarily path-depends on this slot.

## Known Gaps

- Thread clone, fork, and vfork fail closed with EOPNOTSUPP.
- Exec is unsupported.
- RCB timers/read_clock and CPUID/RDTSC interception are incomplete.
- Full application signal disposition multiplexing is incomplete.
- ToolHost serializes dispatch behind its state lock; revisit for threads.

## Successor Steps

1. Preserve this dirty slot and paired Hermit slot128.
2. Review backend.rs, runtime.rs, rpc.rs, tool_host.rs, and reverie-preload.
3. Make liteinst2 and reverie-preload fetchable/landed before consumers.
4. Split coherent lower-level commits if required; do not add artifacts.
5. Update Hermit only after exact Reverie SHAs exist.
6. Rebuild the Hermit DSO explicitly and repeat both validation sets.
