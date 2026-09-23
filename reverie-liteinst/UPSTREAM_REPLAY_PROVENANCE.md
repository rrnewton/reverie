# LiteInst upstream replay provenance

This change is a clean replay of the generic LiteInst integration that was
developed in an uncommitted Reverie worktree. It does not commit or reconstruct
that worktree, vendor LiteInst2, or make Hermit consume an unlanded Reverie
revision.

## Fixed inputs

- Reverie destination base: `5f5cc66039de38bc07301705753fc6e6de4d1928`
  (`origin/main` when the replay slot was created).
- Preserved source base: `6e3915b70a71657a08f0028b3f70d6074116206a`.
- Preserved source final tracked diff: 100 paths, 126,568 insertions, 7,215
  deletions, binary-diff SHA-256
  `fc462e99a7d0459f51ad21a44651d9ae3f64254903d35252726af03b799c827f`.
- LiteInst2 upstream pull request:
  <https://github.com/rrnewton/liteinst2/pull/27>.
- Exact landed LiteInst2 revision:
  `9d8ab600159c9be040e0adff4d12c686ad35aee8`.
- The landed LiteInst2 tree is
  `1ca56bb0d1199ac5c091815832da84686f1d91b5`, identical to reviewed source
  revision `751d0227a6ea4612305022dff8f6fe0f6e7f791f`.

The replay selects every tracked source path in that preserved diff except the
complete exclusion set below. This is a path-identity claim, not a claim that
every selected file was copied byte-for-byte: the rule selected 67 tracked
paths, with path-set SHA-256
`a2132f7327d26dcc053c04cfaa5844ad305032864ab201fa0c57220e69e06eb2`.
The source worktree's untracked inventory script was adapted for the
upstream-first graph, giving 68 source-derived paths with path-set SHA-256
`341c85bd004ee8cfd99529697c7f680208bffb2e13009e6af388a123de892229`.
Ten destination-only compatibility and negative-control paths have path-set
SHA-256
`c7dcd5422e5991e1f5b6975b2441b9c6b5605ae14ddea85572568998d1619d52`:

- `reverie-liteinst/tests/fixtures/hybrid_active_footprint.c`
- `reverie-liteinst/tests/fixtures/hybrid_cpuid_policy.c`
- `reverie-liteinst/tests/fixtures/hybrid_hot_site.c`
- `reverie-liteinst/tests/fixtures/hybrid_mapping_churn.c`
- `reverie-liteinst/tests/fixtures/hybrid_rt_sigreturn_site.c`
- `reverie-liteinst/tests/fixtures/hybrid_tsc_policy.c`
- `reverie-preload/tests/smoke.rs`
- `reverie-ptrace/src/injection_stop_tests.rs`
- `reverie/src/tool.rs`
- `safeptrace/tests/pending_cleanup_compat.rs`

The resulting 78 product paths have path-set SHA-256
`242caaa78ce1c3f1730b303c494205872076056cf5bb7d0361c4ccc681c235e3`;
including this provenance document gives 79 paths and SHA-256
`8ca919940c470f7c6c9fd2a212f4ad5c170d256675bedaddb17ef638e20d4429`.
Twenty-seven selected files remain byte-identical to the preserved source; the
51 reconciled, destination-only, or subsequently advanced paths are listed
completely below. The current product-content fingerprint over sorted
`<mode> <Git blob><TAB><path>` records is
`2937a53bd62f6feb12ebf7ee0064dbecf5f21501ac325f4f8dfe9b11f641841e`
(77 mode `100644`, one mode `100755`).

## Complete tracked exclusion set

Repository policy and unrelated integrations:

- `.gitignore`
- `Cargo.toml`
- `validate.sh`
- `reverie-e9patch/src/aot.rs`
- `reverie-kvm/src/executor.rs`
- `reverie-process/src/clone.rs`
- `reverie-process/src/error.rs`

Backward-incompatible replacements that were not replayed:

- `reverie-liteinst/README.md`
- `reverie-liteinst/tests/lifecycle.rs`

Private schema-4 staging code that had no production binder or consumer and
produced 281 dead-code warnings:

- `reverie-ptrace/src/after_loader/inode_policy.rs`
- `reverie-ptrace/src/after_loader/manifest_v4.rs`
- `reverie-ptrace/src/after_loader/stable_cover.rs`
- `reverie-ptrace/src/after_loader/stable_store.rs`

The complete vendored LiteInst2 subtree was excluded in favor of the exact
landed upstream revision:

- `third_party/liteinst2/Cargo.toml`
- `third_party/liteinst2/LICENSE`
- `third_party/liteinst2/README.md`
- `third_party/liteinst2/examples/preload-consumer/.gitignore`
- `third_party/liteinst2/examples/preload_consumer.rs`
- `third_party/liteinst2/examples/replace_first.rs`
- `third_party/liteinst2/src/cache_line.rs`
- `third_party/liteinst2/src/lib.rs`
- `third_party/liteinst2/src/patcher.rs`
- `third_party/liteinst2/src/planner.rs`
- `third_party/liteinst2/src/probe.rs`
- `third_party/liteinst2/src/rapid.rs`
- `third_party/liteinst2/src/scanner.rs`
- `third_party/liteinst2/src/trampoline.rs`
- `third_party/liteinst2/src/trap.rs`
- `third_party/liteinst2/tests/arena_fork.rs`
- `third_party/liteinst2/tests/stress.rs`
- `third_party/liteinst2/tests/support/arena_fork_fixture.rs`
- `third_party/liteinst2/tests/support/trampoline_tail_fixture.rs`
- `third_party/liteinst2/tests/trampoline_tail.rs`

The source worktree's untracked root `Cargo.lock` (SHA-256
`c2cd2ebd9da120b1d5a74d5dc27005a6645d3ca562f7fda1cb795f6ecb445812`)
was also excluded. The source inventory script (SHA-256
`496fd94caadcfe6e50748dee6b7003efc8d7025b571e30a37754bc506a35426f`)
was instead adapted to remove vendoring and `--locked` assumptions. Its final
mode is `100755`, Git blob
`d1ffff14075c890f27a7e02d0beb922cac0c3dd9`, and SHA-256
`c1a2d7c9f47e0980bf46b6e75b8bf74d819dfd1ce1583e07e374d5ecd6fb986b`.
It asserts the exact target topology, eight after-loader tests, 40-test hybrid
sentinel inventory, and empty ignored inventories; the workflow runs it
directly. All 447 untracked source `ignored/**` paths remain evidence rather
than replay inputs, including eight source-like validation or probe helpers.

## Manual reconciliation

The source and destination bases differed in these relevant files, so their
changes were reconciled rather than copied:

- `reverie-liteinst/tests/hybrid.rs`
- `reverie-process/src/container.rs`
- `reverie-process/src/lib.rs`
- `reverie-ptrace/src/task.rs`
- `reverie-ptrace/src/timer.rs`
- `reverie-ptrace/src/tracer.rs`

`reverie-process/src/clone.rs` also differed but is excluded above. The replay
uses current main's owned clone/pidfd mechanism and split container setup, then
adds only the narrow final pre-seccomp callback required by after-loader
activation. Current main's initial-command timer behavior, early
`PostspawnError::Exited`, and post-spawn error semantics were retained while
the typed stop-ownership and observer lifecycle were integrated.

The complete set of selected or destination-only compatibility paths whose
destination content is not currently byte-identical to the preserved source
has path-set SHA-256
`e4664b6457cc1b5b80f6bae351ec5ed9ff7ec3ca9932b4c87fe8bb842268c15e`:

- `.github/workflows/ci.yml`
- `reverie-liteinst/Cargo.toml`
- `reverie-liteinst/liteinst-helper.ld`
- `reverie-liteinst/src/backend.rs`
- `reverie-liteinst/src/bin/rpc_tool_guest/guard_restorer.rs`
- `reverie-liteinst/src/lib.rs`
- `reverie-liteinst/src/runtime.rs`
- `reverie-liteinst/src/stats.rs`
- `reverie-liteinst/src/tool_host.rs`
- `reverie-liteinst/tests/after_loader.rs`
- `reverie-liteinst/tests/fixtures/host_initializer.c`
- `reverie-liteinst/tests/fixtures/hybrid_active_footprint.c`
- `reverie-liteinst/tests/fixtures/hybrid_cpuid_policy.c`
- `reverie-liteinst/tests/fixtures/hybrid_hot_site.c`
- `reverie-liteinst/tests/fixtures/hybrid_mapping_churn.c`
- `reverie-liteinst/tests/fixtures/hybrid_rt_sigreturn_site.c`
- `reverie-liteinst/tests/fixtures/hybrid_tsc_policy.c`
- `reverie-liteinst/tests/hybrid.rs`
- `reverie-liteinst/tests/rpc_tool.rs`
- `reverie-preload/src/lib.rs`
- `reverie-preload/src/lifecycle.rs`
- `reverie-preload/src/seccomp.rs`
- `reverie-preload/src/signal.rs`
- `reverie-preload/src/trap.rs`
- `reverie-preload/tests/smoke.rs`
- `reverie-process/src/container.rs`
- `reverie-process/src/controller_launch.rs`
- `reverie-process/src/lib.rs`
- `reverie-process/src/spawn.rs`
- `reverie-ptrace/src/after_loader.rs`
- `reverie-ptrace/src/after_loader/manifest.rs`
- `reverie-ptrace/src/after_loader/tests.rs`
- `reverie-ptrace/src/injected_syscall.rs`
- `reverie-ptrace/src/injection_stop_tests.rs`
- `reverie-ptrace/src/lib.rs`
- `reverie-ptrace/src/liteinst_stats.rs`
- `reverie-ptrace/src/target_loader.rs`
- `reverie-ptrace/src/target_loader/environment.rs`
- `reverie-ptrace/src/target_loader/environment/tests.rs`
- `reverie-ptrace/src/task.rs`
- `reverie-ptrace/src/task/after_loader_task.rs`
- `reverie-ptrace/src/timer.rs`
- `reverie-ptrace/src/tracer.rs`
- `reverie-ptrace/src/vdso.rs`
- `reverie/src/backend_stats.rs`
- `reverie/src/tool.rs`
- `safeptrace/src/lib.rs`
- `safeptrace/src/notifier.rs`
- `safeptrace/src/regs.rs`
- `safeptrace/tests/pending_cleanup_compat.rs`
- `scripts/check-liteinst-after-loader-test-inventory.sh`

This set includes compatibility adaptations, fixes made during replay review,
and tests/evidence strengthened after import. The six base-reconciliation paths
above are exactly the selected paths whose source and destination base blobs
already differed; the remaining divergence was introduced deliberately by the
upstream-first replay and its review fixes.

The following public compatibility adaptations were made deliberately:

- Existing generic `Backend` run/output/statistics methods remain operational;
  after-loader entry points have separate explicit names.
- Existing `SeccompFilter` constructors retain their signatures; exact
  restorer-aware variants have new names.
- The preload lifecycle discovers libc's restorer only on the exactly
  single-threaded install path. Its blocked `SIGUSR2` probe restores the raw
  disposition and mask on success or failure and calls `_exit(126)` if either
  restoration itself fails. Only the canonical nine-byte x86-64 glibc stub is
  admitted as a second exact `rt_sigreturn` gate; arbitrary/custom sites remain
  trapped. The public legacy `trap::install_handler` performs no discovery.
- `patch_current_vdso` retains its `Vec<VdsoSyscallSite>` result and commits the
  transaction; `patch_current_vdso_transaction` exposes the new rollback-safe
  API.
- Legacy `TerminalCleanup` revocation signatures remain available, backed by a
  distinct fail-closed external-handoff state so typed cleanup cannot repeat a
  raw continuation whose result the legacy API cannot report.
- The ten-value exhaustive `LiteinstDispatchPath` enum and its wire/display
  names are unchanged. Deoptimized hits have a private detail counter and are
  included exactly once in `unpatchable_or_other`.
- The landed LiteInst2 signal API is adapted with exact calling-thread mask
  retention, rollback, ownership, publication ordering, and one-shot restore.
- `Tool::handle_backend_bootstrap_entropy` is a destination-only, fail-closed
  request for deterministic libc bootstrap bytes. It is not represented as a
  guest syscall and defaults to `ENOSYS` for tools that do not opt in.

## Evidence status

Historical compile-only checks completed against the pre-timer-handoff replay
delta before any guest execution:

- `cargo check -p reverie-liteinst --all-targets --offline`
- `cargo check -p reverie-liteinst --all-targets --no-default-features --features liteinst-after-loader-experiment --offline`
- `cargo test -p reverie-liteinst --test after_loader --no-default-features --features liteinst-after-loader-experiment --no-run --offline`
- `cargo check -p safeptrace --tests --offline`

Both Reverie feature configurations compiled without warnings after excluding
the unwired schema-4 modules. These results do not apply to the current delta.
The retained staging and first-guest-attempt receipts bind the obsolete
`ab2d1a90c0a1c6915b9ef3a769f8282073024694fb7681ab82223f4a176cfdeb`
fingerprint and record five failed guest cells; they are historical evidence,
not validation of the current fingerprint.

## Durable archival binding

On 2026-09-23, the 78 product paths were re-read from the replay worktree. The
path set remained
`242caaa78ce1c3f1730b303c494205872076056cf5bb7d0361c4ccc681c235e3`,
while the content fingerprint had advanced to the value recorded above. This
document, those exact product blobs, and five small historical generator/failed-
run evidence files are preserved on
`refs/heads/salvage/devbig014/liteinst-reverie-replay-20260922/reverie-1-2937a53bd62f`.
The archival commit and tree are recorded in the generation-bound wrkslots
handoff for `liteinst-replay-snapshot-20260923`. Regenerable generator targets,
stage binaries, and other run outputs are intentionally excluded.
