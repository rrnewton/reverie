# Storage fd low-word validation

- Recorded: 2026-09-22T23:33:13-07:00
- Repository: rrnewton/reverie
- Base: c444c4ff15b6f5985082317e7c93b370e67571c1
- Exact head: fb5698fb210f6549c9256ae80ef91cf6e682cfd2
- Frozen artifact: storage-fd-lowword-frozen-v1.diff
- Artifact SHA-256: 8a641d5594bff37bae9da8ff79379eefb7713c3e529fd09198f38cc348b3adad
- Artifact identity: byte-identical to `git diff --binary origin/main..HEAD`

## Exact-head green gates

- `cargo fmt --all -- --check`: pass
- `git diff --check origin/main..HEAD`: pass
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: pass
- Focused unit: 1 passed, 0 failed
- Required real-KVM integration: 1 passed, 0 failed with `REVERIE_REQUIRE_KVM=1`
  - native once
  - direct KVM twice
  - Tool KVM twice
  - exact stdout, stderr, and exit status comparison only; no L2/log/replay-parity claim
- Full library default run 3: 815 passed, 0 failed
- Full library serial: 815 passed, 0 failed
- Post-review full library serial: 815 passed, 0 failed
- Post-review full library default: 815 passed, 0 failed

## Preserved exact-head red runs

- Full library default run 1: 814 passed, 1 failed. The only failure was the pre-existing SIGPIPE-sensitive `executor::tests::positioned_vectored_io_handles_pipes_partial_writes_and_sigpipe`, observed `4` versus expected `-32`.
- Full library default run 2: 814 passed, 1 failed. The only failure was `clock::entry_interrupt_tests::running_reserved_kick_retains_exact_finite_program_branch_total`, which saw `Kvm(Error(4))` while unwrapping.
- An independent reviewer's fresh full-library run was 814 passed, 1 failed. The unchanged `executor::tests::descriptor_retirement_accept_cleanup_releases_both_guards` hit `EAGAIN`; its source is byte-identical between base and head, it then passed 10/10 isolated reruns, and the reviewer independently obtained serial 815/815. No base reproduction was performed.
- A second independent reviewer obtained two serial 814/815 runs, failing different unchanged cells: `vm::tests::fatal_worker_rw_delayed_waiter_qualifies` and `executor::entry_host_wait_tests::queued_host_futex_retains_operands_while_unchanged_mapping_fence_completes`. Each then passed 5/5 in isolation; the same reviewer obtained a default-thread 815/815 run. No base reproduction was performed.
- None of these failed tests overlaps the two changed files semantically; all results remain red evidence. Fresh post-review serial and default exact-head runs both passed 815/815, satisfying the clean-run gate without erasing the earlier observations.

## Causal mutation evidence

Before the final rebase, the source patch had the same SHA-256 as the frozen artifact. Each production decoder was individually restored to its old `i32::try_from` behavior while tests were left unchanged. Both the focused unit and required KVM test rejected every mutant:

- `fallocate`: unit failed with `fallocate returned -9`; KVM guest exited 7.
- shared `fsync`/`fdatasync`: unit observed `-9` instead of 0; KVM guest exited 3.
- `readahead`: unit observed `-9` instead of 0; KVM guest exited 5.
- `sync_file_range`: unit observed `-9` instead of 0; KVM guest exited 6.

After mutation testing, both source files were restored to their pre-mutation SHA-256 values and the focused unit plus required KVM cell passed again. The subsequent rebase changed only the parent commit; the frozen source diff retained SHA-256 `8a641d5594bff37bae9da8ff79379eefb7713c3e529fd09198f38cc348b3adad`.

## Exact-artifact review

Three independent adversarial reviews approved exact head `fb5698fb210f6549c9256ae80ef91cf6e682cfd2` and artifact SHA-256 `8a641d5594bff37bae9da8ff79379eefb7713c3e529fd09198f38cc348b3adad` for frozen/open-PR state. All explicitly found no goalpost moving. None authorizes merge; a coordinator-relayed exact-head Claude-family verdict remains mandatory.
