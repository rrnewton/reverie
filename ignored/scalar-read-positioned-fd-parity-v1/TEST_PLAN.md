All phases below are planned and unexecuted. Independent source review and explicit implementation handoff precede live application. Root owns SCM. Do not start qualification while the higher-priority timer work is being sequenced.

Use the existing approved observer/lease machinery (observer137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179) with fresh own source/plan/context bindings and the owned target. Do not copy an active foreign target, change the observer, or silently relax its guards. Bind pinned nightly-2026-07-29, offline/locked, two Cargo jobs, the actual generated lock/dependency graph and external tools. Each source/caller remains immutable during its phase.

Original limits are per phase: metadata/compile/check/Clippy600 aggregate CPU seconds/900 wall; format/list/tests30 CPU/60 wall; all16GiB memory, zero swap,16MiB stderr,64MiB maintained stdout and100GiB free-space floor. The existing integration helper's inner30s self-exec remains unchanged. Any genuine instrumentation change needs its concrete delta/control review; no observation from this proposal alters the harness allowance.

1. Make a complete owned baseline snapshot of the base, then apply only `baseline-test-overlay`. Authenticate all overlay bytes against ORACLE_CONTINUITY. Use `cargo test --offline --locked -p reverie-kvm --lib --test static_elf --no-run --message-format=json`; retain both actual emitted test ELFs separately from mutable Cargo paths, loader identities and complete exact lists. Record the original source/SCM identity; the overlay must not be mistaken for live product or a complete repository on its own.

2. On that unchanged-production snapshot run these exact five declarations separately. Use `--exact --test-threads=1 --nocapture -Z unstable-options --format=json`, bound actual test events/counts and full terminal accounting. Require REVERIE_REQUIRE_KVM=1 for static tests, plus a fresh owned REVERIE_SCALAR_FD_ARTIFACT_DIR for each phase (the fixture ELF uses create_new and must not overwrite any historical executable).

- static: scalar_read_full_width_descriptor_transport
- static: scalar_pread_full_width_descriptor_transport
- static: scalar_pwrite_full_width_descriptor_transport
- lib: executor::tests::scalar_pwrite_descriptor_width_preserves_signalfd_order_and_carrier
- lib: executor::tests::vectored_high_descriptor_signalfd_guard_is_already_effective

The first three must reach the high-bit raw operation after canonical success; source predicts result -EBADF instead of3, fixture exit21 at case1, with its complete raw record retained. An earlier namespace/KVM/compiler/fixture failure is not this observation. The signalfd selector must reach native ESPIPE versus executor's old EBADF, not merely any failed assertion. The vector selector is expected to pass BEFORE and is never credited as a repaired failure. Actual results may refute these source predictions; preserve them and diagnose without changing assertions. No native failure has occurred yet.

3. After implementation authorization, apply the reviewed full candidate on its exact base and compile the same two targets under the original compile allowance. Retain corrected binaries and full inventories separately. The complete ten new declarations below must each run with one selected/zero ignored, under unchanged30/60 bounds. Reuse exactly the same fixture/assertions as baseline; preserve its independently retained C ELF and full native/KVM raw records. Inspect the emitted native/raw-call and C fixture syscall instructions to bind full-width register transport to the actual compiled artifacts.

Library (all prefixed executor::tests::):
- scalar_read_descriptor_width_preserves_bytes_and_shared_position
- scalar_pread_descriptor_width_preserves_bytes_and_shared_position
- scalar_pwrite_descriptor_width_preserves_bytes_and_shared_position
- scalar_read_descriptor_width_routes_stdin_and_fdinfo
- scalar_read_descriptor_width_keeps_virtual_signalfd_records
- scalar_pwrite_descriptor_width_preserves_signalfd_order_and_carrier
- vectored_high_descriptor_signalfd_guard_is_already_effective

Static:
- scalar_read_full_width_descriptor_transport
- scalar_pread_full_width_descriptor_transport
- scalar_pwrite_full_width_descriptor_transport

These are10 new declarations, not executed inventory counts. Complete successful source loops contain45 regular native raw calls +12 scalar signalfd calls +8 vector calls in the library controls, and15 additional native plus15 KVM operations in the three transport controls. Early failures execute smaller populations; derive actual attempts from evidence rather than reporting the planned totals as measured.

4. Run these eight unchanged nearest declarations; do not rerun a broad suite merely for counts:

Library (prefix executor::tests::):
- read_limits_host_consumption_to_the_accessible_guest_prefix
- inherited_special_stdin_matches_linux_read_precedence
- fdinfo_dispatch_observes_current_owned_offset_flags_and_descriptor_cloexec
- fdinfo_dispatch_pread_and_guest_faults_preserve_linux_sequence_positions
- signalfd_backing_is_not_writable_seekable_or_pollout_visible
- signalfd_vectored_io_uses_virtual_records_and_linux_error_ordering
- positioned_write_seek_truncate_and_sync_round_trip

Static:
- captured_write_signals::captured_write_signal_capability_routes_and_refusals

The last selection retains full raw callback mismatch refusals, including same-low/different-upper descriptor words; do not normalize that identity. The other unchanged controls retain partial memory-prefix consumption, special-stdin errno precedence, sequence positions, original signalfd refusals, real eventfd behavior and positional shared-offset checks. The corrected unique selection is18 declarations; repeated baseline executions are counted separately, and historical modes/old passes are not new measurements.

5. Format the changed Rust files under30/60. Run the existing scoped `cargo clippy --offline --locked -p reverie-kvm --lib --test static_elf --message-format=json -- -D warnings` under600/900. Compilation, actual native/KVM controls, formatting and Clippy all remain pending now. No full-DAG or whole-Hermit runtime claim follows from finite component evidence. No Hermit invocation is required.
