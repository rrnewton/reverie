# Reproducing the OSS Buck2 build

The OSS Buck2 project is generated from Reverie's authoritative root
`Cargo.toml` and tracked `Cargo.lock`. The generated Rust `BUCK` file and
vendored crate sources remain ignored because they can be regenerated.

From a fresh checkout, run:

```sh
./bootstrap/regenerate-rust-deps
./bootstrap/buck2 build //:reverie-ptrace
```

Prerequisites are Git, rustup, and the open-source
[DotSlash](https://dotslash-cli.com) launcher.

The wrappers use immutable versions rather than live branch tips:

- Buck2 release `2026-08-01`, through Buck2's upstream DotSlash descriptor
  with a BLAKE3 digest and size for each supported platform
- Reindeer `e3d72748131d3a70378055f091e0647c1edad85e`
- Rust `nightly-2026-05-22`

The first Reindeer invocation downloads the pinned source revision, installs
the pinned Rust toolchain if needed, and compiles Reindeer into the user cache.
Set `REVERIE_BUCK2_TOOL_CACHE` to place that cache elsewhere. DotSlash downloads
and verifies the platform-specific Buck2 release binary.

`regenerate-rust-deps` starts without generated dependency output, vendors the
versions in `Cargo.lock`, generates `shim/third-party/rust/BUCK` twice, and
refuses the result if the two consecutive outputs differ. It also refuses any
change to the tracked lockfile. The repository-root `.gitignore` excludes the
generated paths. Those patterns must not move into `shim/.gitignore`: pinned
Reindeer reads ignore files through the shim cell root and would otherwise
generate empty crates.

When Reverie is consumed as a cell by Hermit, the historical build stops later
because the two projects currently compile separate copies of third-party Rust
crates. Types crossing the cell boundary consequently have distinct trait
identities. This bootstrap does not choose between changing Hermit's hermetic
Reverie pin and changing the shared third-party graph.
