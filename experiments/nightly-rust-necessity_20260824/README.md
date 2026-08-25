# Do we still need nightly Rust? — measurement, 2026-08-24

Task: `do-we-actually-still-need-nightly-rust`. Read-only. Nothing in `hermit/`,
`reverie/` or the PR #2388 flake was modified.

**Answer: yes, nightly is still required today, and `rust-overlay` cannot be
dropped.** It is forced by exactly **4 feature gates** (9 attribute sites) and
**3 nightly-only rustfmt options**. Nothing else — no `-Z` flag, no cargo
unstable feature, no dependency. Two of the four gates are test-only and
mechanical to remove; one is a deliberate design choice; one has no stable
equivalent and no stabilization path.

Separately: the nixpkgs revision currently pinned in PR #2388's `flake.lock`
ships Rust **1.77.2**, which cannot build this codebase at all (30 crates are
`edition = "2024"`, which needs ≥1.85). That is an independent problem from
nightly.

## Measurement anchor

| | |
| --- | --- |
| Date | 2026-08-24 |
| `hermit` | `8ea761cf91` on `main` (4 behind `origin/main`; dirty files are docs-only: `.github/workflows/docs.yml`, `docs/site/*`) |
| `reverie` | `bfbe3b14` on `main`, clean — this is the exact git rev `hermit` pins in `detcore/Cargo.toml:88`, `hermit-cli/Cargo.toml:56` |
| Stable toolchain probed | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Host | devbig, no `nix` installed; nixpkgs facts read from GitHub over `with-proxy` |
| Raw logs | `logs/` in this directory |
| Gate probes | `gate-probes/` (p1–p4.rs, one per feature) |

## 1. What pins the toolchain today (exact strings)

`hermit/rust-toolchain.toml`:
```toml
[toolchain]
# @fb-only: path = "../../third-party-buck/platform010/build/rust/llvm-fb-15"
channel = "nightly" # @oss-only
```

`reverie/rust-toolchain.toml`:
```toml
channel = "nightly-2026-07-29" # @oss-only
components = ["clippy", "rustfmt"] # @oss-only
```
(pinned because nightly-2026-07-30 changed cargo's cdylib artifact uplift under
`cargo test` and broke the reverie-examples liteinst integration tests)

The parent `dev-hermit` repo has **no** `rust-toolchain*` file and no root Cargo
workspace. Its 41 tracked `rust-script` `.rs` files contain **0** feature gates.

Other places the channel is named:
- `reverie/.github/workflows/ci.yml:22` and `:147` — `uses: dtolnay/rust-toolchain@nightly`
- `ci-hub/runners/Containerfile` — installs stable as baseline, then
  `rustup toolchain install nightly` for hermit specifically
- PR #2388 `ci/hermetic/flake.nix:43` — `pkgs.rust-bin.nightly."2026-07-29".default`
  via `github:oxalica/rust-overlay`

## 2. Every nightly-only construct actually used

4 distinct gates, 9 `#![feature(...)]` attribute sites. Each was probed
individually against stable 1.97.1 (`gate-probes/`); **all four still error with
E0658 on stable today** — none has quietly stabilized.

| Gate | Sites | Tracking issue | Stable equivalent? |
| --- | --- | --- | --- |
| `thread_local` | `reverie-liteinst/src/lib.rs:1`, `reverie-preload/src/lib.rs:50`; 12 `#[thread_local]` statics (liteinst `runtime.rs` 8, `patch_alloc.rs` 2, preload `trap.rs` 2) | [#29594](https://github.com/rust-lang/rust/issues/29594), open, `S-tracking-impl-incomplete`, last updated 2025-05-14 | `thread_local!` exists but is **not** equivalent here. `reverie-preload/src/trap.rs:77` states the reason: `#[thread_local]` was chosen "so first access performs no lazy initialization / allocation inside the handler" — this is the reentrancy guard on the syscall-interception path. Replacing it would put lazy-init and destructor registration inside a preloaded syscall handler. |
| `internal_output_capture` | `reverie-ptrace/src/lib.rs:31` (unconditional); `reverie-process/src/lib.rs:15` (already `cfg_attr(feature = "nightly", ...)`) | **none** — rustc emits no `see issue` note, only "this function is meant for use in the test crate and may disappear in the future" | **No stable equivalent.** Used at `reverie-ptrace/src/tracer.rs:2492`/`:2527` and `reverie-process/src/container.rs:849`/`:875` to disable libtest's output capture across a `fork`/`clone` (rust-lang/rust#35136). `reverie-process` already ships a stable path: with the `nightly` cargo feature off, the two calls are simply omitted. `reverie-ptrace` has no such gate. |
| `get_mut_unchecked` | `reverie/tests/standalone/parallel_tasks.rs:9`, `hermit/detcore/tests/parallelism/mod.rs:9`, `hermit/tests/rust/mem_race.rs:9` | [#63292](https://github.com/rust-lang/rust/issues/63292), open, `B-unstable` `requires-nightly` | **Test-only, mechanical.** The tests deliberately alias an `Arc` to manufacture a data race, so checked `Arc::get_mut` will not do — but a raw pointer or `UnsafeCell` rewrite would. |
| `thread_id_value` | same 3 test files (`thread::current().id().as_u64().get()`) | [#67939](https://github.com/rust-lang/rust/issues/67939), open, `B-unstable` `Libs-Small` | **Test-only, mechanical.** `gettid(2)`, an atomic counter, or parsing `ThreadId`'s `Debug` all work. |

None of the four is close to stabilization. Two open library gates have sat for
6–7 years; `thread_local` is blocked on incomplete implementation (destructors);
`internal_output_capture` is not on a stabilization path at all.

## 3. Non-feature-gate forcers

Checked, and found:

- **rustfmt: 3 nightly-only options.** Measured by running stable rustfmt against
  `hermit/rustfmt.toml`: `format_code_in_doc_comments`, `imports_granularity`,
  `group_imports`. Present in 3 files — `hermit/rustfmt.toml`,
  `hermit/detcore/rustfmt.toml`, `reverie/rustfmt.toml`. **Trap:** stable rustfmt
  *warns and exits 0*, so `cargo fmt --all -- --check` on stable would not fail
  loudly; it would silently enforce a different format.

Checked, and found nothing:

- **No `.cargo/config.toml`** in either repo (0 tracked files matching `cargo/config`).
- **No `cargo-features =`** and **no `[unstable]`** in any `Cargo.toml`.
- **One `-Z` use**, already stable-safe by construction:
  `hermit/tests/e2e/lib/data-handling/reproducible-build.sh:49-50` sets
  `RUSTC_BOOTSTRAP=1` before `rustc -Zthreads=1 -Ccodegen-units=1 -Cdebuginfo=0`.
- **No dependency requires nightly** — see run B below; every third-party crate in
  both workspaces compiled under stable rustc.
- **No MSRV** (`rust-version`) is declared anywhere in either repo.

## 4. The stable-build attempt

Neither repo was edited. The stable toolchain was selected via the environment
(`RUSTUP_TOOLCHAIN=stable`) and output redirected to a scratch `CARGO_TARGET_DIR`
so the working caches were untouched.

```
RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR=<scratch> \
  cargo check --all-targets --workspace --keep-going
```

### Run A — stable 1.97.1, no bootstrap

| Workspace | Members | Units that failed | Error |
| --- | --- | --- | --- |
| `reverie` | 25 | 4: `reverie-preload` (lib), `reverie-preload` (lib test), `reverie-ptrace` (lib), `reverie-ptrace` (lib test) | all E0554 `#![feature] may not be used on the stable release channel` |
| `hermit` | 13 | 4: `reverie-preload` (lib), `reverie-ptrace` (lib) [git deps], `hermetic_infra_hermit_tests` bin `rustbin_mem_race` and its test | all E0554 |

Logs: `logs/tmp-stable-probe-reverie2.log`, `logs/tmp-stable-probe-hermit2.log`.

Caveat on the denominator: `cargo --keep-going` does **not** check crates that
depend on a failed crate. This is the first-order blocker set, not a full
accounting — which is why run B exists.

### Run B — stable 1.97.1 + `RUSTC_BOOTSTRAP=1`

Same compiler, same std, same crates; only the *stability gate* is lifted. This
isolates the question "besides the four gates, is anything here beyond stable's
compiler and library?"

| Workspace | Errors | Rust warnings |
| --- | --- | --- |
| `reverie` | **0** | **0** |
| `hermit` | **0** | **0** |

Logs: `logs/tmp-stable-probe-reverie-bs.log`, `logs/tmp-stable-probe-hermit-bs.log`.
The 19 `warning:` lines in the reverie log are all C compiler output from the
vendored libelf/SaBRe sources and DynamoRIO build-script chatter, not Rust
diagnostics.

**Reading:** the four gates are the *entire* difference. No language feature, no
std API, no dependency, and no edition requirement in either workspace exceeds
what stable 1.97.1 provides.

Caveats, stated plainly:
- This is `cargo check`, not `cargo build` or `cargo test`. Codegen and linking
  were not exercised and **no test was run**. A stable build could still fail at
  link time or behave differently at runtime; that is not measured here.
- `RUSTC_BOOTSTRAP=1` is a diagnostic, not a proposal. Shipping on stable via
  `RUSTC_BOOTSTRAP` would be nightly-in-disguise with none of the pinning.
- Clippy under `-D warnings` on stable was not run; the stable and nightly lint
  sets differ.

## 5. Which nixpkgs Rust is actually available

No `nix` on this host, so read from nixpkgs sources over the proxy.

| Source | Default `pkgs.rustc` | Enough for this codebase? |
| --- | --- | --- |
| PR #2388 `ci/hermetic/flake.lock` pin `b134951a4c9f3c995fd7be05f3243f8ecd65d798`, committed **2024-12-30** | **1.77.2** (`rust = rust_1_77`, `all-packages.nix:16505`); only alternative is `rust_1_80` = 1.80.0 | **No.** Edition 2024 requires ≥1.85. |
| `nixos-unstable` today | **1.97.1** (`rust = rust_1_97`, `all-packages.nix:4013`) | **Yes** — the exact version run B used clean. |

Edition spread, since it matters as much as the gates:

| Repo | `edition = "2024"` | `edition = "2021"` |
| --- | --- | --- |
| `hermit` | 15 | 3 |
| `reverie` | 15 | 12 |

Note the flake's inputs are 20 months apart: nixpkgs locked 2024-12-30,
`rust-overlay` locked 2026-08-22. Today `rust-overlay` is doing more than
supplying nightly — it is also the only reason a modern Rust exists in that
image at all.

## 6. Answer to the owner's question

**Can `github:oxalica/rust-overlay` be dropped? No — not today.**

Two independent things would each have to be fixed first:

1. **Bump the nixpkgs pin.** At the current lock, nixpkgs' Rust is 1.77.2 and
   cannot compile 30 edition-2024 crates. A bump to something near
   `nixos-unstable` (Rust 1.97.1) fixes this and is cheap.
2. **Remove four feature gates and three rustfmt options.** Ranked by cost:
   - `get_mut_unchecked`, `thread_id_value` — test-only, 3 files, mechanical.
   - the 3 rustfmt options — a formatting-policy decision plus one reformat pass.
   - `internal_output_capture` in `reverie-ptrace` — no stable equivalent, but
     `reverie-process` already demonstrates the shape of the fix
     (`cfg_attr(feature = "nightly", ...)`, calls omitted when off). The cost is
     that libtest output capture is then not suppressed across the fork in
     `tracer.rs`, i.e. rust-lang/rust#35136 comes back for ptrace-backend tests.
   - `#[thread_local]` in `reverie-preload` / `reverie-liteinst` — the real one.
     12 statics on the syscall-interception hot path, chosen specifically to
     avoid lazy init and allocation inside the handler. This is not a
     find-and-replace; it needs a design answer.

So the honest framing for the owner: this is **not** an open-ended dependency on
a moving nightly. It is a tracked list of 4 gates, of which 3 have a known route
off nightly and 1 (`#[thread_local]` in the preload handler) is a deliberate
engineering choice that would have to be revisited. Until then, keeping
`rust-overlay` is the correct call — and the more urgent finding in this
investigation is the stale nixpkgs pin, which is a supply-chain and
buildability problem regardless of which Rust channel is chosen.
