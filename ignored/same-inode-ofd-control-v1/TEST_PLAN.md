# Held unit-control plan

This plan is unexecuted. Do not apply the test to the live product tree or launch phases until the owner reviews the packet and hands off execution. Parent owns HEAD, index and refs. No correction implementation is authorized by this plan.

1. Create the owned baseline snapshot at `/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918/ignored/same-inode-ofd-control-v1/baseline-source`, using exact tracked blobs/modes/symlinks from `000c15a1161ea2d58749431b5ddaaa97f7aa37d5`. Replace only executor.rs with this packet's complete after file. Include the separately bound local Cargo.lock unchanged. Prove all other tracked content equals the immutable base and retain a complete fresh manifest. Record any needed immutable dependency input separately. Do not copy a foreign target, invent a new product commit, or use a symlink into mutable source.

2. Prepare fresh evidence under this packet's `qualification/`. Place the proposed `caller/after/prepare.py` there with the unchanged `phase.py`, `common.py`, `cache_lease.py` and observer companions from `caller/unchanged/`. The reviewed helper is intended at this exact directory depth; the files under `caller/` themselves are review artifacts, not executable launch locations. Copy the bound SETUP/SELECTORS parameters and create a fresh RUNNER_ORIGINS record tying the original and proposed files. All per-phase plan hashes, source/SCM state, dependency closure, toolchain/loader inputs, target identity and lease identity must be produced freshly. No old plan or terminal receipt becomes a current receipt by changing a label.

3. Reuse only the existing owned target `/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918/target/process-alarm-qualification-v1` and its existing lane lease `/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918/ignored/timer-integration-20260918/lane.lease`, after the ordinary ownership/state checks permit a new phase. Preserve all earlier results and retained binaries. If the lease is active, do not bypass or repair it speculatively. Product source and SCM must remain held unchanged throughout each phase.

4. Use the pinned `/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin` Cargo/Rustc/Rustdoc; offline, locked, two Cargo and third-party jobs, unchanged environment guards. Resolve actual metadata/dependency closure, then compile only this native library harness:

```text
cargo metadata --offline --locked --format-version 1
cargo test --offline --locked -p reverie-kvm --lib --no-run --message-format=json
```

These are payload arguments through the bounded caller, never unobserved shell commands. The invocation form remains:

```text
/usr/bin/python3 -B <qualification>/phase.py launch <exact-plan-path> <exact-plan-sha256>
```

The caller keeps the reviewed observer `137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179`. It retains actual Cargo JSON completion and exactly one `reverie_kvm` library test artifact; bind and retain its executable bytes separately from the mutable compiler path before reuse. Authenticate actual loader inputs. No static ELF fixture or Hermit guest is part of this scope. Existing `REVERIE_REQUIRE_KVM=1` is retained in the environment but these exact executor unit controls do not claim to run a VM.

5. Run the one-file format check through the same caller, with original format limits:

```text
rustfmt --check --edition 2024 --config skip_children=true reverie-kvm/src/executor.rs
```

The proposed helper binds the snapshot file against the reviewed complete after bytes and leaves the phase's full source-manifest check intact. It does not inspect the unrelated clean product diff to decide which snapshot test file changed.

6. List the exact emitted library harness with `--list -Z unstable-options --format=json`, while the target lease holds its bytes stable and the separate immutable ELF copy preserves them. Require each of these three names exactly once. Run each separately with `--exact --test-threads=1 --nocapture -Z unstable-options --format=json`, using the same actual ELF:

- `executor::tests::fdinfo_dispatch_retains_partial_records_and_shared_dup_seek_state`
- `executor::tests::forked_states_share_file_object_identity_namespace`
- `executor::tests::shared_file_table_reopen_same_inode_replaces_description`

The first two are unchanged positive neighbors. They do not replace the new target or dilute its result. The new target's expected Linux behavior is fixed in the patch and must stay unchanged across before/corrected source. On the present production source, preserve whatever real result occurs. A reached failing replacement assertion can establish the negative observation only with the exact started/failed event, raw assertion values, no skip/ignore, actual selected count 1, complete terminal accounting and unchanged source/ELF inputs. Do not call an arbitrary compile/setup/namespace/observer refusal the intended failure. Keep `accepted=false`; any later negative-observation authentication is separately labelled and never rewrites that receipt. A surprise pass refutes or narrows the source hypothesis and must be investigated without changing the oracle.

| Phase | Aggregate CPU | Wall | Memory | Swap |
| --- | --- | --- | --- | --- |
| Metadata, compile | 600 seconds | 900 seconds | 16 GiB | 0 |
| Format, list, each exact test | 30 seconds | 60 seconds | 16 GiB | 0 |

Every phase retains 16 MiB stderr, 64 MiB maintained stdout, 16 MiB bounded readback and 100 GiB free-space floor. These equal the bound original phase plans in CALLER_PLAN.json. No aggregate/runtime allowance is substituted for an individual test allowance. No new observer branch, timeout tolerance, test normalization or special success classification is proposed. No Clippy/core/ptrace/full-suite runtime expansion is needed to obtain this single source-control observation; any later correction gets its own finite qualification plan.

Current counts: one proposed new declaration; three selected declarations; zero attempted compilations, format checks, lists or tests; zero results. Hold after packet review.
