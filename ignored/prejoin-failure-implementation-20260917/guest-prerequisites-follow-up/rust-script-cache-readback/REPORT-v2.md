The supplied generator context omits a separate rust-script dependency resolution. The existing cache contains two generated projects: `validate_b81ceab65609123303f94190` for the original `scripts/validate.rs`, and `check-nested-lockfiles_be776c6e0dc0044fb82ec5a8` for the original `scripts/check-nested-lockfiles.rs`. Neither generated Cargo.toml/Cargo.lock pair nor the rust-script executable is declared in that context. Of validate’s 122 locked registry package versions, 23 have no source-file record in its recursive input manifest. Exact added source inputs are now available in `SOURCE-INPUTS-FOR-REUSE.json`, SHA256 `ba0192387cd85a8bfe4f401ef0a6de92c08a8c066effe53f341602b704b23790`.

This comparison is bound to `/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/ignored/prejoin-main-callers-v8/post-compose-generator-context.json`, SHA256 `a6d8f064ba36fe08509168f4bc692d9d5accda716c94b34859b691c0f08d47a3`, which records source head `9d983d5ff9591696c7361300cabaf5ce8a2cfe13` / tree `50dfb11cb9cc9e3cc8fcb3e026e508b56b05cf21`. Its tracked-source manifest and 26,423-entry recursive manifest were authenticated by their declared hashes. The direct input list has 265 records. The final capture window was 2026-09-17T19:42:51.127411+00:00 through 2026-09-17T19:42:51.652190+00:00. The owner reported generation terminal before this capture. This report observes the resulting files; the owner’s separate service/accounting evidence establishes termination. Initial exploratory reads while generation was active are not relabelled as terminal bindings.

The real producer is `ci/manifest-plan/src/validation_dag.rs:251`: `generated_plan` invokes the repository’s `scripts/validate.rs --write-generated-plan PATH`. That source has `#!/usr/bin/env -S rust-script --force` and an embedded Cargo manifest with path dependencies on dagrun and hermit-manifest-plan. The generated cache manifest preserves those original paths. The check-nested-lockfiles project was generated for the ordinary commit hook, not as an additional guest control. Both original script hashes match the existing source manifest.

| Generated project directory under `/tmp/hermit-prejoin-commit-cache-20260917/rust-script/projects` | Original source | Locked package population |
| --- | --- | --- |
| `b81ceab65609123303f94190` | `/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/scripts/validate.rs` | 128 |
| `be776c6e0dc0044fb82ec5a8` | `/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/scripts/check-nested-lockfiles.rs` | 1 |

Validate’s 128 packages comprise 122 registry packages, two Reverie Git packages at the already-landed 7d863ab3f02639731713a01467b2548c41e3dbfb revision, and four local packages including the generated script itself. The other project has one package and no dependencies. The local dagrun, hermit-manifest-plan and detcore-model manifests and both Reverie manifests are already source-bound and match their recorded hashes. This is a declared lock population, not a claim that all target-specific packages were compiled on Linux. No Cargo metadata/build command was run to infer a larger execution claim.

The 23 missing registry versions are recorded below. Every source file under each exact version directory was read and hashed, including hidden extraction metadata, without following directory symlinks. Every retained `.crate` archive SHA256 matches that package’s Cargo.lock checksum; all regular archive members match the extracted source files. The modern cache does not contain `.cargo-checksum.json` for the initially inspected version, so that optional-file lookup was unavailable; the archive comparison supplies the explicit source check without inventing a checksum file or substituting another version.

| Locked registry version | Newly recorded source files |
| --- | ---: |
| `android_system_properties-0.1.6` | 11 |
| `bitflags-2.13.2` | 65 |
| `bytesize-2.7.0` | 28 |
| `cc-1.4.5` | 29 |
| `cfg-if-1.0.5` | 15 |
| `clap-4.6.7` | 148 |
| `clap_builder-4.6.7` | 65 |
| `clap_derive-4.6.7` | 24 |
| `clap_lex-1.1.1` | 10 |
| `find-msvc-tools-0.1.12` | 19 |
| `futures-core-0.3.34` | 15 |
| `futures-task-0.3.34` | 15 |
| `futures-util-0.3.34` | 194 |
| `indexmap-2.14.2` | 44 |
| `js-sys-0.3.105` | 19 |
| `log-0.4.34` | 24 |
| `rustix-1.1.5` | 332 |
| `syn-3.0.6` | 105 |
| `unicode-ident-1.0.26` | 27 |
| `wasm-bindgen-0.2.128` | 27 |
| `wasm-bindgen-macro-0.2.128` | 10 |
| `wasm-bindgen-macro-support-0.2.128` | 15 |
| `wasm-bindgen-shared-0.2.128` | 12 |

There are 1,253 source files across those 23 versions. The 99 other locked registry versions are represented in the supplied recursive manifest; this task compares their membership and does not claim a fresh full-file authentication of all 99 trees. `LOCK-PACKAGES.json` retains every package/source/version/checksum/dependency edge and its representation count, and `MISSING-REGISTRY-PACKAGES.json` retains each exact source-directory and archive identity.

`SOURCE-INPUTS-FOR-REUSE.json` contains 1,292 records: the 1,253 missing registry files, 23 matching archives, four generated Cargo files, the two original scripts, rust-script and its three loader files, three existing local manifests, two existing pinned Git manifests and the producer source. Each record has an exact path, resolved path, bytes, mode, SHA256 and observed identity. This is the intended source/input addition. Do not blindly use the broader diagnostic `ADDITIONAL-INPUTS.json` as an immutable input set: it also records generated Cargo outputs and timestamps that may change during another build.

The rust-script loader closure is explicit:

| Path | Bytes | SHA256 |
| --- | ---: | --- |
| `/home/newton/.cargo/bin/rust-script` | 3589808 | `90a6cc5197f2d35f9d9a7c3b30a1a8ca7dcfcfa645024fa4b7a1d565461d7a00` |
| `/lib64/ld-linux-x86-64.so.2` | 938752 | `008e34384d4bc082a053ba086588efa1546086ecb863c9464b7269f8928b7577` |
| `/lib64/libgcc_s.so.1` | 116408 | `3d144b557008c93e4e67567d5dc98cb81cf55187a698d1b4872ce84ba8a26bed` |
| `/lib64/libc.so.6` | 2549352 | `d932cb6bc88da10cc709d5b7ecda57a1387b6571331689dba88023b4c0517776` |

Those four files match the prior prerequisite packet’s complete static ELF dependency records. The current loader configuration/cache files were also rechecked in `RUNTIME-LOOKUP-INPUTS.json`, with `/etc/ld.so.preload` explicitly absent. The fixed service PATH selects `/home/newton/.cargo/bin/rust-script`; `/usr/bin/env` and the Cargo/Rust toolchain are already declared by the supplied context. No runtime loader was executed.

The cache also contains two generated script executables, 18 canonical Cargo build-script executables, and seven proc-macro shared objects. `GENERATED-COMMAND-SOURCES.json` binds the exact script/build-script paths, hashes, compiler-emitted source dependencies and same-inode build-script aliases. The two direct script commands are:

| Existing command | Bytes | SHA256 |
| --- | ---: | --- |
| `/tmp/hermit-prejoin-commit-cache-20260917/rust-script/binaries/release/validate_b81ceab65609123303f94190` | 7476408 | `cf909a2c90ddbc060cf423c58ceda7db2b1320fef02a33c64546f2bf2186a1cb` |
| `/tmp/hermit-prejoin-commit-cache-20260917/rust-script/binaries/release/check-nested-lockfiles_be776c6e0dc0044fb82ec5a8` | 405568 | `cbab828a392202f41872f655f867dfa30e87d5b152e734af6df87aaa74e6cfb4` |

The build-script commands belong to anyhow, generic-array, getrandom, libc, memoffset, nix, num-traits, paste, proc-macro2, quote, radium, rustix, serde, serde_core, serde_json, signal-hook, syscalls and zmij. The proc-macro objects are bincode_derive, clap_derive, derive_more_impl, paste, serde_derive, serde_repr and tracing_attributes. Their existence identifies reusable compiler commands/objects, not an exact historical invocation count. The direct validate script executable is the program the generator’s rust-script launcher is expected to use after its build step; future execution must bind the actual bytes then used.

All 27 existing command/shared-object ELFs were inspected with bounded readelf metadata queries, with no execution of those ELFs. `GENERATED-ELF-DEPENDENCIES.json` records their actual PT_INTERP/DT_NEEDED values; `GENERATED-LOADER-INPUTS.json` binds the three resolved loader/library files. Each readelf query had 5 CPU seconds, 15 wall seconds and 1 MiB per stream, returned zero, and left the inspected artifact hash unchanged. Raw metadata and receipts remain under `elf-queries/`.

The emitted script `.d` files name 83 source inputs. An initial exact-spelling lookup left three `../` spellings and the `embedded_userguide.md` symlink unmatched. `DEPFILE-RESOLVED-COVERAGE.json` resolves them against the existing tracked/recursive records and the context’s declared symlink target; all 83 hashes match. They are not additional missing source dependencies. The earlier exact-spelling observations are retained separately rather than deleted.

Generated binaries, proc macros, Cargo build-script outputs and timestamps remain owned cache artifacts. A later `rust-script --force` invocation may rebuild them. Their recorded current hashes are not a promise that a future rebuild emits or executes identical bytes; after the unchanged generator check, the caller must retain the actual resulting command/artifact identity and terminal accounting. Binding these inputs now does not retroactively repair the original generator write/check’s missing declarations, and it does not require repeating independent direct-Cargo/native phases that never invoke these helpers. Root and the Hermit owner already sequenced the bounded complete-input generator check after the final source commit; this worker ran no such check.

`READBACK.json` records 1,486 successful checks on source/input bytes, existing source coverage, the 27 metadata queries and current command/object hashes. No missing required registry archive/source or loader file was encountered. There were no source/cache/ref edits, Cargo/build/test/guest/network commands, or execution of generated artifacts. Writes were confined to this report directory. The original cache contents, generated project files and earlier limited-context evidence remain intact.

Primary machine-readable records:

- `SOURCE-INPUTS-FOR-REUSE.json`: `ba0192387cd85a8bfe4f401ef0a6de92c08a8c066effe53f341602b704b23790`
- `CACHE-PROJECTS.json`: `d0f7860baf9fc2f30d9915990cf2f37c05703e8262c1ee47fe0811bbfba2448d`
- `LOCK-PACKAGES.json`: `315e5b80724c168d9c5d37806f02d9507d287a7a7a38c25bda2c7e46c533f184`
- `MISSING-REGISTRY-PACKAGES.json`: `35a3a456011dff701b90e77ea146a2f1f94fd603767fc5a9d0307a2106a215fe`
- `RUST-SCRIPT-LOADER.json`: `4cb73b9a64dc56b98021a35e8b28934fe5fe8be6f43eef82191ac9b9e30d03c9`
- `RUNTIME-LOOKUP-INPUTS.json`: `0c3e255e09652fb7544c9f575562e820820df92293136c77ecc9d7db3442f5ec`
- `GENERATED-COMMAND-SOURCES.json`: `c2de29fab52d52a0d8e6d0365f1a6805d55d7b5e1d72a6c82ea10b64bedebabe`
- `GENERATED-ELF-DEPENDENCIES.json`: `0862f634a5b10ee62e50d0c0ce4c86554856924752b5c3825cc0d74f92348b82`
- `READBACK.json`: `a7648c297e251e0fe1bdf652d1a193027107a75fc459d20c35fd6c3862514f77`
